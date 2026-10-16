//! On-device handwriting recognizer backed by PP-OCRv5.
//!
//! [`PaddleHandwriting`] implements [`LocalHandwriting`]. `available` is true
//! only when both pinned files are on disk and match their SHA-256. Strokes
//! are rasterized on this device and run through the ONNX graph on CPU. No
//! method here downloads or opens a socket.
//!
//! Compared with the other open recognizers: Zinnia is BSD-3 and small, but
//! the published simplified-Chinese model (Tegaki / Tomoe) is LGPL-2.1 and
//! has no English model. A full PaddleOCR detector plus ONNX Runtime is the
//! bulk that speech already skipped for sherpa-onnx. This path keeps only the
//! mobile recognition network: one Apache-2.0 file, about 16 MB, Chinese and
//! English together, CPU execution provider, no CUDA / TensorRT / CoreML.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use ndarray::Array4;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::TensorRef;

use crate::handwriting::{InkCandidate, InkStroke, LocalHandwriting};
use crate::handwriting_model::{
    self, DICT_FILE, INK_MODEL_FILE, handwriting_model_ready, parse_character_dict,
};

const IMG_H: usize = 48;
const IMG_W: usize = 320;
const NEG: f32 = -1.0e30;
const STROKE_RADIUS: i32 = 2;

struct Loaded {
    modified: SystemTime,
    session: Option<Session>,
    dict: Vec<String>,
}

/// Bundled recognizer. Without the verified files this stays unavailable.
pub struct PaddleHandwriting {
    dir: PathBuf,
    loaded: Option<Loaded>,
}

impl PaddleHandwriting {
    pub fn open_default() -> Self {
        Self::new(handwriting_model::default_handwriting_dir())
    }

    pub fn new(dir: PathBuf) -> Self {
        Self { dir, loaded: None }
    }

    fn ready(&self) -> bool {
        handwriting_model_ready(&self.dir)
    }

    fn ensure(&mut self) -> bool {
        if !self.ready() {
            self.loaded = None;
            return false;
        }
        let model_path = self.dir.join(INK_MODEL_FILE);
        let modified = std::fs::metadata(&model_path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if self
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.modified == modified && loaded.session.is_some())
        {
            return true;
        }
        let dict = std::fs::read_to_string(self.dir.join(DICT_FILE))
            .map(|text| parse_character_dict(&text))
            .unwrap_or_default();
        if dict.is_empty() {
            self.loaded = Some(Loaded {
                modified,
                session: None,
                dict,
            });
            return false;
        }
        let session = load_session(&model_path);
        let ok = session.is_some();
        self.loaded = Some(Loaded {
            modified,
            session,
            dict,
        });
        ok
    }
}

impl LocalHandwriting for PaddleHandwriting {
    fn manifest(&self) -> crate::handwriting::HandwritingManifest {
        crate::handwriting::HandwritingManifest {
            id: "builtin.pp-ocrv5-mobile-rec".into(),
            locales: vec!["zh-Hans".into(), "en".into()],
            network: false,
            available: self.ready(),
        }
    }

    fn recognize(&mut self, width: u32, height: u32, strokes: &[InkStroke]) -> Vec<InkCandidate> {
        if !self.ensure() {
            return Vec::new();
        }
        let Some(gray) = rasterize(width, height, strokes) else {
            return Vec::new();
        };
        self.decode_gray(&gray.pixels, gray.width)
    }
}

impl PaddleHandwriting {
    fn decode_gray(&mut self, pixels: &[u8], width: usize) -> Vec<InkCandidate> {
        if !self.ensure() {
            return Vec::new();
        }
        let Some(loaded) = self.loaded.as_mut() else {
            return Vec::new();
        };
        let Some(session) = loaded.session.as_mut() else {
            return Vec::new();
        };
        let dict = loaded.dict.clone();
        let array = to_model_input(pixels, width);
        let Ok(tensor) = TensorRef::from_array_view(&array) else {
            return Vec::new();
        };
        let Ok(outputs) = session.run(ort::inputs![tensor]) else {
            return Vec::new();
        };
        if outputs.len() == 0 {
            return Vec::new();
        }
        let Ok(view) = outputs[0].try_extract_array::<f32>() else {
            return Vec::new();
        };
        let Some((time, classes, logits)) =
            orient_logits(view.shape(), view.as_slice().unwrap_or(&[]), dict.len())
        else {
            return Vec::new();
        };
        ctc_nbest(&logits, time, classes, &dict, 8)
            .into_iter()
            .map(|(text, score)| InkCandidate { text, score })
            .collect()
    }
}

struct Gray {
    width: usize,
    pixels: Vec<u8>,
}

fn load_session(path: &Path) -> Option<Session> {
    let env = shared_env()?;
    Session::builder(env)
        .ok()?
        .with_optimization_level(GraphOptimizationLevel::Level1)
        .ok()?
        .with_intra_threads(2)
        .ok()?
        .commit_from_file(path)
        .ok()
}

fn shared_env() -> Option<&'static ort::environment::Environment> {
    static SLOT: Mutex<Option<&'static ort::environment::Environment>> = Mutex::new(None);
    let mut guard = SLOT.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(env) = *guard {
        return Some(env);
    }
    let env = ort::init().build().ok()?;
    let leaked = Box::leak(Box::new(env));
    *guard = Some(leaked);
    Some(leaked)
}

fn to_model_input(pixels: &[u8], width: usize) -> Array4<f32> {
    let mut data = vec![0.0f32; 3 * IMG_H * IMG_W];
    let copy_w = width.min(IMG_W);
    for y in 0..IMG_H.min(pixels.len() / width.max(1)) {
        for x in 0..copy_w {
            let pixel = pixels[y * width + x] as f32;
            let norm = (pixel / 255.0 - 0.5) / 0.5;
            for channel in 0..3 {
                data[channel * IMG_H * IMG_W + y * IMG_W + x] = norm;
            }
        }
    }
    Array4::from_shape_vec((1, 3, IMG_H, IMG_W), data)
        .unwrap_or_else(|_| Array4::zeros((1, 3, IMG_H, IMG_W)))
}

/// Crop the ink to its box, scale the height to 48, and keep the aspect ratio.
fn rasterize(pad_w: u32, pad_h: u32, strokes: &[InkStroke]) -> Option<Gray> {
    if pad_w == 0 || pad_h == 0 {
        return None;
    }
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    let mut count = 0usize;
    for stroke in strokes {
        for point in &stroke.points {
            if !point.x.is_finite() || !point.y.is_finite() {
                continue;
            }
            min_x = min_x.min(point.x);
            min_y = min_y.min(point.y);
            max_x = max_x.max(point.x);
            max_y = max_y.max(point.y);
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    let bw = (max_x - min_x).max(8.0);
    let bh = (max_y - min_y).max(8.0);
    let margin = bw.max(bh) * 0.28;
    min_x -= margin;
    min_y -= margin;
    let bw = bw + margin * 2.0;
    let bh = bh + margin * 2.0;
    let out_h = IMG_H;
    let out_w = ((out_h as f32) * bw / bh).round().clamp(8.0, IMG_W as f32) as usize;
    let mut pixels = vec![255u8; out_w * out_h];
    let scale_x = out_w as f32 / bw;
    let scale_y = out_h as f32 / bh;
    for stroke in strokes {
        let points: Vec<(f32, f32)> = stroke
            .points
            .iter()
            .filter(|point| point.x.is_finite() && point.y.is_finite())
            .map(|point| ((point.x - min_x) * scale_x, (point.y - min_y) * scale_y))
            .collect();
        if points.is_empty() {
            continue;
        }
        if points.len() == 1 {
            stamp(
                &mut pixels,
                out_w,
                out_h,
                points[0].0,
                points[0].1,
                STROKE_RADIUS,
            );
            continue;
        }
        for pair in points.windows(2) {
            stroke_line(&mut pixels, out_w, out_h, pair[0], pair[1], STROKE_RADIUS);
        }
    }
    if pixels.iter().all(|pixel| *pixel > 250) {
        return None;
    }
    Some(Gray {
        width: out_w,
        pixels,
    })
}

fn stroke_line(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    from: (f32, f32),
    to: (f32, f32),
    radius: i32,
) {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let steps = dx.abs().max(dy.abs()).ceil().max(1.0) as usize;
    for step in 0..=steps {
        let t = step as f32 / steps as f32;
        stamp(
            pixels,
            width,
            height,
            from.0 + dx * t,
            from.1 + dy * t,
            radius,
        );
    }
}

fn stamp(pixels: &mut [u8], width: usize, height: usize, x: f32, y: f32, radius: i32) {
    let cx = x.round() as i32;
    let cy = y.round() as i32;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dy * dy > radius * radius + radius {
                continue;
            }
            let px = cx + dx;
            let py = cy + dy;
            if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                continue;
            }
            pixels[py as usize * width + px as usize] = 0;
        }
    }
}

pub(crate) fn ctc_nbest(
    logits: &[f32],
    time: usize,
    classes: usize,
    dict: &[String],
    limit: usize,
) -> Vec<(String, f32)> {
    if time == 0 || classes == 0 || dict.is_empty() || limit == 0 || logits.len() < time * classes {
        return Vec::new();
    }
    let beam_width = limit.clamp(1, 8);
    let mut logp = vec![0.0f32; time * classes];
    let mut tops = Vec::with_capacity(time);
    for t in 0..time {
        let row = &logits[t * classes..(t + 1) * classes];
        log_softmax(row, &mut logp[t * classes..(t + 1) * classes]);
        tops.push(top_indexes(
            &logp[t * classes..(t + 1) * classes],
            beam_width.max(5),
        ));
    }
    let mut beams: HashMap<String, (f32, f32)> = HashMap::new();
    beams.insert(String::new(), (0.0, NEG));
    for t in 0..time {
        let mut next: HashMap<String, (f32, f32)> = HashMap::new();
        for (prefix, (blank, nonblank)) in &beams {
            for &class in &tops[t] {
                let prob = logp[t * classes + class];
                if class == 0 {
                    let entry = next.entry(prefix.clone()).or_insert((NEG, NEG));
                    entry.0 = log_add(entry.0, log_add(*blank, *nonblank) + prob);
                    continue;
                }
                let Some(label) = class_label(class, classes, dict) else {
                    continue;
                };
                if !prefix.is_empty() && prefix.ends_with(label) {
                    let stay = next.entry(prefix.clone()).or_insert((NEG, NEG));
                    stay.1 = log_add(stay.1, nonblank + prob);
                    let doubled = format!("{prefix}{label}");
                    let extra = next.entry(doubled).or_insert((NEG, NEG));
                    extra.1 = log_add(extra.1, blank + prob);
                } else {
                    let extended = format!("{prefix}{label}");
                    let entry = next.entry(extended).or_insert((NEG, NEG));
                    entry.1 = log_add(entry.1, log_add(*blank, *nonblank) + prob);
                }
            }
        }
        beams = prune(next, beam_width);
    }
    let mut ranked: Vec<(String, f32)> = beams
        .into_iter()
        .filter(|(text, _)| !text.is_empty())
        .map(|(text, (blank, nonblank))| (text, log_add(blank, nonblank)))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let greedy = greedy_decode(logits, time, classes, dict);
    if greedy.is_empty() {
        return Vec::new();
    }
    let best = ranked.first().map(|(_, score)| *score).unwrap_or(0.0);
    ranked.retain(|(text, score)| text.chars().count() <= 4 && *score >= best - 2.0);
    if let Some(index) = ranked.iter().position(|(text, _)| text == &greedy) {
        let chosen = ranked.remove(index);
        ranked.insert(0, chosen);
    } else {
        ranked.insert(0, (greedy, best));
    }
    ranked.truncate(limit);
    ranked
}

fn class_label(class: usize, classes: usize, dict: &[String]) -> Option<&str> {
    if class == 0 {
        return None;
    }
    let index = class - 1;
    if index < dict.len() {
        return Some(dict[index].as_str());
    }
    if classes == dict.len() + 2 && index == dict.len() {
        return Some(" ");
    }
    None
}

fn greedy_decode(logits: &[f32], time: usize, classes: usize, dict: &[String]) -> String {
    let mut out = String::new();
    let mut previous = usize::MAX;
    for t in 0..time {
        let row = &logits[t * classes..(t + 1) * classes];
        let mut best = 0usize;
        let mut best_value = row[0];
        for (index, value) in row.iter().enumerate().skip(1) {
            if *value > best_value {
                best = index;
                best_value = *value;
            }
        }
        if best != 0
            && best != previous
            && let Some(label) = class_label(best, classes, dict)
        {
            out.push_str(label);
        }
        previous = best;
    }
    out
}

fn log_softmax(row: &[f32], out: &mut [f32]) {
    let max = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0.0f32;
    for value in row {
        sum += (value - max).exp();
    }
    let log_z = max + sum.ln();
    for (index, value) in row.iter().enumerate() {
        out[index] = value - log_z;
    }
}

fn top_indexes(logp: &[f32], k: usize) -> Vec<usize> {
    let mut indexes: Vec<usize> = (0..logp.len()).collect();
    indexes.sort_by(|&a, &b| {
        logp[b]
            .partial_cmp(&logp[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    indexes.truncate(k.max(1));
    if !indexes.contains(&0) {
        indexes.push(0);
    }
    indexes
}

fn log_add(left: f32, right: f32) -> f32 {
    if left <= NEG / 2.0 {
        return right;
    }
    if right <= NEG / 2.0 {
        return left;
    }
    let (hi, lo) = if left > right {
        (left, right)
    } else {
        (right, left)
    };
    hi + (1.0 + (lo - hi).exp()).ln()
}

fn prune(beams: HashMap<String, (f32, f32)>, width: usize) -> HashMap<String, (f32, f32)> {
    let mut ranked: Vec<(String, (f32, f32))> = beams.into_iter().collect();
    ranked.sort_by(|a, b| {
        log_add(b.1.0, b.1.1)
            .partial_cmp(&log_add(a.1.0, a.1.1))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked.truncate(width);
    ranked.into_iter().collect()
}

pub(crate) fn orient_logits(
    shape: &[usize],
    data: &[f32],
    dict_len: usize,
) -> Option<(usize, usize, Vec<f32>)> {
    let (time, classes) = match shape {
        [1, a, b] | [a, b] => {
            let classes_guess = dict_len + 1;
            if (*b).abs_diff(classes_guess) <= (*a).abs_diff(classes_guess) && *b > 1 {
                (*a, *b)
            } else if *a > 1 {
                (*b, *a)
            } else {
                return None;
            }
        }
        _ => return None,
    };
    if data.len() < time * classes {
        return None;
    }
    let time_major = match shape {
        [1, a, _] | [a, _] => *a == time,
        _ => true,
    };
    if time_major {
        return Some((time, classes, data[..time * classes].to_vec()));
    }
    let mut transposed = vec![0.0f32; time * classes];
    for t in 0..time {
        for c in 0..classes {
            transposed[t * classes + c] = data[c * time + t];
        }
    }
    Some((time, classes, transposed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handwriting::{InkPoint, InkStroke, LocalHandwriting};

    #[test]
    fn a_horizontal_stroke_becomes_dark_ink_on_white() {
        let stroke = InkStroke {
            points: vec![InkPoint { x: 8.0, y: 20.0 }, InkPoint { x: 90.0, y: 20.0 }],
        };
        let gray = rasterize(100, 40, &[stroke]).expect("ink");
        assert_eq!(gray.pixels.len() / gray.width, IMG_H);
        assert!(gray.width > 16);
        assert!(gray.pixels.iter().any(|pixel| *pixel < 32));
        assert!(gray.pixels.iter().any(|pixel| *pixel > 200));
        assert!(rasterize(100, 40, &[]).is_none());
    }

    #[test]
    fn ctc_beam_reads_a_two_character_line() {
        let dict = vec!["你".to_string(), "好".to_string()];
        let classes = 3;
        let mut logits = vec![-6.0f32; 4 * classes];
        let paint = |logits: &mut [f32], time: usize, class: usize| {
            logits[time * classes + class] = 6.0;
        };
        paint(&mut logits, 0, 1);
        paint(&mut logits, 1, 1);
        paint(&mut logits, 2, 0);
        paint(&mut logits, 3, 2);
        let ranked = ctc_nbest(&logits, 4, classes, &dict, 4);
        assert_eq!(ranked.first().map(|(text, _)| text.as_str()), Some("你好"));
        let blank = vec![6.0f32, -6.0, -6.0, 6.0, -6.0, -6.0];
        assert!(ctc_nbest(&blank, 2, 3, &dict, 2).is_empty());
    }

    #[test]
    fn without_a_model_the_engine_stays_off_and_does_not_fetch() {
        let dir = std::env::temp_dir().join(format!("w3cos-ink-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let engine = PaddleHandwriting::new(dir.clone());
        let manifest = engine.manifest();
        assert_eq!(manifest.id, "builtin.pp-ocrv5-mobile-rec");
        assert!(!manifest.network);
        assert!(!manifest.available);
        assert_eq!(
            manifest.locales,
            vec!["zh-Hans".to_string(), "en".to_string()]
        );
        let mut engine = engine;
        let stroke = InkStroke {
            points: vec![InkPoint { x: 1.0, y: 1.0 }, InkPoint { x: 8.0, y: 1.0 }],
        };
        assert!(engine.recognize(40, 20, &[stroke]).is_empty());
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logits_follow_the_class_axis() {
        let data: Vec<f32> = (0..6).map(|i| i as f32).collect();
        let (time, classes, row) = orient_logits(&[1, 2, 3], &data, 2).expect("shape");
        assert_eq!((time, classes), (2, 3));
        assert_eq!(row, data);
        let (time, classes, row) = orient_logits(&[1, 3, 2], &data, 2).expect("transposed");
        assert_eq!((time, classes), (2, 3));
        assert_eq!(row, vec![0.0, 2.0, 4.0, 1.0, 3.0, 5.0]);
    }

    #[test]
    fn pinned_model_reads_chinese_and_english_strokes() {
        let Some(dir) = std::env::var_os("W3COS_HANDWRITING_MODEL_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        assert!(
            handwriting_model_ready(&dir),
            "W3COS_HANDWRITING_MODEL_DIR is not the pinned pair"
        );
        let mut engine = PaddleHandwriting::new(dir);
        let letter = [
            InkStroke {
                points: vec![InkPoint { x: 24.0, y: 6.0 }, InkPoint { x: 8.0, y: 42.0 }],
            },
            InkStroke {
                points: vec![InkPoint { x: 24.0, y: 6.0 }, InkPoint { x: 40.0, y: 42.0 }],
            },
            InkStroke {
                points: vec![InkPoint { x: 12.0, y: 28.0 }, InkPoint { x: 36.0, y: 28.0 }],
            },
        ];
        let latin = engine.recognize(48, 48, &letter);
        assert!(
            latin.iter().any(|item| item.text.contains('A')),
            "latin stroke candidates: {latin:?}"
        );
        let person = [
            InkStroke {
                points: vec![InkPoint { x: 22.0, y: 6.0 }, InkPoint { x: 6.0, y: 42.0 }],
            },
            InkStroke {
                points: vec![InkPoint { x: 18.0, y: 20.0 }, InkPoint { x: 42.0, y: 42.0 }],
            },
        ];
        let han = engine.recognize(48, 48, &person);
        assert!(
            han.iter().any(|item| item.text.contains('人')),
            "han stroke candidates: {han:?}"
        );
    }
}
