//! On-device screen text backed by PP-OCRv5.
//!
//! [`PaddleScreenText`] implements [`LocalScreenText`]. `available` is true
//! only when the pinned detector and the handwriting recognizer both match
//! their SHA-256. One region is resized on this device and run through the
//! detector, then each line through the recognizer, both on CPU. No method
//! here downloads or opens a socket, and none of them capture the screen.
//!
//! The detector is PaddlePaddle `PP-OCRv5_mobile_det` (Apache-2.0). The
//! recognizer is the same `PP-OCRv5_mobile_rec` the handwriting pad uses.
//! Preprocess follows that export: long side 960, BGR, ImageNet mean and
//! std, DB threshold 0.3, box threshold 0.6, unclip 1.5. CUDA, TensorRT, and
//! CoreML stay off.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ndarray::Array4;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::TensorRef;

use crate::handwriting_engine::{ctc_greedy, orient_logits, ort_environment};
use crate::handwriting_model::{self, DICT_FILE, INK_MODEL_FILE, parse_character_dict};
use crate::screen_model::{self, DET_FILE, screen_text_ready};
use crate::screen_text::{
    LocalScreenText, ScreenImage, ScreenRegion, ScreenSnippet, ScreenTextManifest,
};

const IMG_H: usize = 48;
const IMG_W: usize = 320;
const DET_LONG: f32 = 960.0;
const BIN_THRESH: f32 = 0.3;
const BOX_THRESH: f32 = 0.6;
const UNCLIP: f32 = 1.5;

struct Loaded {
    modified: SystemTime,
    session: Option<Session>,
}

struct RecLoaded {
    modified: SystemTime,
    session: Option<Session>,
    dict: Vec<String>,
}

/// Bundled reader. Without the verified files this stays unavailable.
pub struct PaddleScreenText {
    det_dir: PathBuf,
    rec_dir: PathBuf,
    det: Option<Loaded>,
    rec: Option<RecLoaded>,
}

impl PaddleScreenText {
    pub fn open_default() -> Self {
        Self::new(
            screen_model::default_screen_text_dir(),
            handwriting_model::default_handwriting_dir(),
        )
    }

    pub fn new(det_dir: PathBuf, rec_dir: PathBuf) -> Self {
        Self {
            det_dir,
            rec_dir,
            det: None,
            rec: None,
        }
    }

    fn ready(&self) -> bool {
        screen_text_ready(&self.det_dir, &self.rec_dir)
    }

    fn ensure_det(&mut self) -> bool {
        if !self.ready() {
            self.det = None;
            self.rec = None;
            return false;
        }
        let path = self.det_dir.join(DET_FILE);
        let modified = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if self
            .det
            .as_ref()
            .is_some_and(|loaded| loaded.modified == modified && loaded.session.is_some())
        {
            return true;
        }
        let session = load_session(&path);
        let ok = session.is_some();
        self.det = Some(Loaded { modified, session });
        ok
    }

    fn ensure_rec(&mut self) -> bool {
        if !self.ready() {
            return false;
        }
        let path = self.rec_dir.join(INK_MODEL_FILE);
        let modified = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if self
            .rec
            .as_ref()
            .is_some_and(|loaded| loaded.modified == modified && loaded.session.is_some())
        {
            return true;
        }
        let dict = std::fs::read_to_string(self.rec_dir.join(DICT_FILE))
            .map(|text| parse_character_dict(&text))
            .unwrap_or_default();
        if dict.is_empty() {
            self.rec = Some(RecLoaded {
                modified,
                session: None,
                dict,
            });
            return false;
        }
        let session = load_session(&path);
        let ok = session.is_some();
        self.rec = Some(RecLoaded {
            modified,
            session,
            dict,
        });
        ok
    }
}

impl LocalScreenText for PaddleScreenText {
    fn manifest(&self) -> ScreenTextManifest {
        ScreenTextManifest {
            id: "builtin.pp-ocrv5-mobile-det".into(),
            locales: vec!["zh-Hans".into(), "en".into()],
            network: false,
            available: self.ready(),
        }
    }

    fn recognize(&mut self, image: &ScreenImage, region: ScreenRegion) -> Vec<ScreenSnippet> {
        if !self.ensure_det() || !self.ensure_rec() {
            return Vec::new();
        }
        let Some(crop) = crop_region(image, region) else {
            return Vec::new();
        };
        let boxes = detect_lines(self, &crop);
        let mut snippets = Vec::new();
        for det_box in boxes {
            let Some(text) = read_line(self, &crop, det_box) else {
                continue;
            };
            snippets.push(ScreenSnippet { text, score: 1.0 });
            if snippets.len() == 8 {
                break;
            }
        }
        snippets
    }
}

struct Crop {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

#[derive(Clone, Copy)]
pub(crate) struct DetBox {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

fn load_session(path: &Path) -> Option<Session> {
    let env = ort_environment()?;
    Session::builder(env)
        .ok()?
        .with_optimization_level(GraphOptimizationLevel::Level1)
        .ok()?
        .with_intra_threads(2)
        .ok()?
        .commit_from_file(path)
        .ok()
}

fn crop_region(image: &ScreenImage, region: ScreenRegion) -> Option<Crop> {
    let expected = (image.width as usize)
        .saturating_mul(image.height as usize)
        .saturating_mul(4);
    if image.rgba.len() != expected || region.width < 8 || region.height < 8 {
        return None;
    }
    let x1 = region.x.saturating_add(region.width);
    let y1 = region.y.saturating_add(region.height);
    if x1 > image.width || y1 > image.height {
        return None;
    }
    let width = region.width as usize;
    let height = region.height as usize;
    let mut rgba = vec![0u8; width * height * 4];
    for row in 0..height {
        let src = ((region.y as usize + row) * image.width as usize + region.x as usize) * 4;
        let dst = row * width * 4;
        rgba[dst..dst + width * 4].copy_from_slice(&image.rgba[src..src + width * 4]);
    }
    Some(Crop {
        width,
        height,
        rgba,
    })
}

fn det_size(width: usize, height: usize) -> (usize, usize) {
    let long = (width.max(height).max(1)) as f32;
    let ratio = DET_LONG / long;
    let mut rw = ((width as f32) * ratio).round().max(1.0) as usize;
    let mut rh = ((height as f32) * ratio).round().max(1.0) as usize;
    rw = rw.div_ceil(32) * 32;
    rh = rh.div_ceil(32) * 32;
    (rw.max(32), rh.max(32))
}

fn sample_bgr(crop: &Crop, x: f32, y: f32) -> [f32; 3] {
    let x = x.clamp(0.0, (crop.width.saturating_sub(1)) as f32);
    let y = y.clamp(0.0, (crop.height.saturating_sub(1)) as f32);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(crop.width - 1);
    let y1 = (y0 + 1).min(crop.height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let pixel = |px: usize, py: usize| -> [f32; 3] {
        let i = (py * crop.width + px) * 4;
        let r = crop.rgba[i] as f32 / 255.0;
        let g = crop.rgba[i + 1] as f32 / 255.0;
        let b = crop.rgba[i + 2] as f32 / 255.0;
        [b, g, r]
    };
    let p00 = pixel(x0, y0);
    let p10 = pixel(x1, y0);
    let p01 = pixel(x0, y1);
    let p11 = pixel(x1, y1);
    let mut out = [0.0; 3];
    for channel in 0..3 {
        let top = p00[channel] * (1.0 - tx) + p10[channel] * tx;
        let bottom = p01[channel] * (1.0 - tx) + p11[channel] * tx;
        out[channel] = top * (1.0 - ty) + bottom * ty;
    }
    out
}

fn det_input(crop: &Crop, width: usize, height: usize) -> Array4<f32> {
    let mean = [0.485f32, 0.456, 0.406];
    let std = [0.229f32, 0.224, 0.225];
    let mut data = vec![0.0f32; 3 * height * width];
    for y in 0..height {
        for x in 0..width {
            let sx = (x as f32 + 0.5) * crop.width as f32 / width as f32 - 0.5;
            let sy = (y as f32 + 0.5) * crop.height as f32 / height as f32 - 0.5;
            let bgr = sample_bgr(crop, sx, sy);
            for channel in 0..3 {
                data[channel * height * width + y * width + x] =
                    (bgr[channel] - mean[channel]) / std[channel];
            }
        }
    }
    Array4::from_shape_vec((1, 3, height, width), data)
        .unwrap_or_else(|_| Array4::zeros((1, 3, height, width)))
}

fn detect_lines(engine: &mut PaddleScreenText, crop: &Crop) -> Vec<DetBox> {
    let (rw, rh) = det_size(crop.width, crop.height);
    let array = det_input(crop, rw, rh);
    let Some(loaded) = engine.det.as_mut() else {
        return Vec::new();
    };
    let Some(session) = loaded.session.as_mut() else {
        return Vec::new();
    };
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
    let Some((map_w, map_h, prob)) = prob_map(view.shape(), view.as_slice().unwrap_or(&[])) else {
        return Vec::new();
    };
    let scale_x = crop.width as f32 / map_w as f32;
    let scale_y = crop.height as f32 / map_h as f32;
    boxes_from_map(&prob, map_w, map_h)
        .into_iter()
        .map(|det_box| DetBox {
            x0: det_box.x0 * scale_x,
            y0: det_box.y0 * scale_y,
            x1: det_box.x1 * scale_x,
            y1: det_box.y1 * scale_y,
        })
        .filter(|det_box| det_box.x1 - det_box.x0 >= 2.0 && det_box.y1 - det_box.y0 >= 2.0)
        .take(8)
        .collect()
}

fn prob_map(shape: &[usize], data: &[f32]) -> Option<(usize, usize, Vec<f32>)> {
    let (height, width) = match shape {
        [1, 1, height, width] | [1, height, width] => (*height, *width),
        [height, width] => (*height, *width),
        _ => return None,
    };
    if width == 0 || height == 0 || data.len() < width * height {
        return None;
    }
    Some((width, height, data[..width * height].to_vec()))
}

/// Connected components above the DB thresholds, in reading order.
pub(crate) fn boxes_from_map(prob: &[f32], width: usize, height: usize) -> Vec<DetBox> {
    if width == 0 || height == 0 || prob.len() < width * height {
        return Vec::new();
    }
    let mut seen = vec![false; width * height];
    let mut boxes = Vec::new();
    let mut queue = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let start = y * width + x;
            if seen[start] || prob[start] <= BIN_THRESH {
                continue;
            }
            queue.clear();
            queue.push(start);
            seen[start] = true;
            let mut head = 0usize;
            let mut min_x = x;
            let mut max_x = x;
            let mut min_y = y;
            let mut max_y = y;
            let mut sum = 0.0f32;
            let mut count = 0usize;
            while head < queue.len() {
                let index = queue[head];
                head += 1;
                let cx = index % width;
                let cy = index / width;
                sum += prob[index];
                count += 1;
                min_x = min_x.min(cx);
                max_x = max_x.max(cx);
                min_y = min_y.min(cy);
                max_y = max_y.max(cy);
                let neighbors = [
                    (cx.wrapping_sub(1), cy, cx > 0),
                    (cx + 1, cy, cx + 1 < width),
                    (cx, cy.wrapping_sub(1), cy > 0),
                    (cx, cy + 1, cy + 1 < height),
                ];
                for (nx, ny, ok) in neighbors {
                    if !ok {
                        continue;
                    }
                    let next = ny * width + nx;
                    if seen[next] || prob[next] <= BIN_THRESH {
                        continue;
                    }
                    seen[next] = true;
                    queue.push(next);
                }
            }
            if count < 8 {
                continue;
            }
            let score = sum / count as f32;
            if score < BOX_THRESH {
                continue;
            }
            let bw = (max_x - min_x + 1) as f32;
            let bh = (max_y - min_y + 1) as f32;
            let dist = (bw * bh * UNCLIP) / (2.0 * (bw + bh));
            boxes.push(DetBox {
                x0: (min_x as f32 - dist).max(0.0),
                y0: (min_y as f32 - dist).max(0.0),
                x1: ((max_x + 1) as f32 + dist).min(width as f32),
                y1: ((max_y + 1) as f32 + dist).min(height as f32),
            });
            if boxes.len() == 1000 {
                break;
            }
        }
        if boxes.len() == 1000 {
            break;
        }
    }
    boxes.sort_by(|left, right| {
        left.y0
            .total_cmp(&right.y0)
            .then(left.x0.total_cmp(&right.x0))
    });
    boxes
}

fn read_line(engine: &mut PaddleScreenText, crop: &Crop, det_box: DetBox) -> Option<String> {
    let x0 = det_box.x0.floor().max(0.0) as usize;
    let y0 = det_box.y0.floor().max(0.0) as usize;
    let x1 = det_box.x1.ceil().min(crop.width as f32) as usize;
    let y1 = det_box.y1.ceil().min(crop.height as f32) as usize;
    if x1 <= x0 + 1 || y1 <= y0 + 1 {
        return None;
    }
    let box_w = x1 - x0;
    let box_h = y1 - y0;
    let out_h = IMG_H;
    let out_w = ((out_h as f32) * box_w as f32 / box_h as f32)
        .round()
        .clamp(8.0, IMG_W as f32) as usize;
    let array = rec_input(crop, x0, y0, box_w, box_h, out_w);
    let loaded = engine.rec.as_mut()?;
    let session = loaded.session.as_mut()?;
    let dict = loaded.dict.clone();
    let tensor = TensorRef::from_array_view(&array).ok()?;
    let outputs = session.run(ort::inputs![tensor]).ok()?;
    if outputs.len() == 0 {
        return None;
    }
    let view = outputs[0].try_extract_array::<f32>().ok()?;
    let (time, classes, logits) =
        orient_logits(view.shape(), view.as_slice().unwrap_or(&[]), dict.len())?;
    ctc_greedy(&logits, time, classes, &dict)
}

fn rec_input(
    crop: &Crop,
    x0: usize,
    y0: usize,
    box_w: usize,
    box_h: usize,
    out_w: usize,
) -> Array4<f32> {
    let mut data = vec![0.0f32; 3 * IMG_H * IMG_W];
    for y in 0..IMG_H {
        for x in 0..out_w {
            let sx = x0 as f32 + (x as f32 + 0.5) * box_w as f32 / out_w as f32 - 0.5;
            let sy = y0 as f32 + (y as f32 + 0.5) * box_h as f32 / IMG_H as f32 - 0.5;
            let bgr = sample_bgr(crop, sx, sy);
            for channel in 0..3 {
                let norm = (bgr[channel] - 0.5) / 0.5;
                data[channel * IMG_H * IMG_W + y * IMG_W + x] = norm;
            }
        }
    }
    Array4::from_shape_vec((1, 3, IMG_H, IMG_W), data)
        .unwrap_or_else(|_| Array4::zeros((1, 3, IMG_H, IMG_W)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bright_block_becomes_one_box_and_a_dim_block_does_not() {
        let width = 20;
        let height = 12;
        let mut prob = vec![0.0f32; width * height];
        for y in 2..8 {
            for x in 3..12 {
                prob[y * width + x] = 0.95;
            }
        }
        let boxes = boxes_from_map(&prob, width, height);
        assert_eq!(boxes.len(), 1);
        assert!(boxes[0].x0 < 3.0);
        assert!(boxes[0].x1 > 12.0);
        assert!(boxes[0].y0 < 2.0);

        let mut dim = vec![0.0f32; width * height];
        for y in 2..8 {
            for x in 3..12 {
                dim[y * width + x] = 0.45;
            }
        }
        assert!(boxes_from_map(&dim, width, height).is_empty());
    }

    #[test]
    fn two_blocks_stay_in_reading_order() {
        let width = 16;
        let height = 20;
        let mut prob = vec![0.0f32; width * height];
        for y in 12..18 {
            for x in 2..8 {
                prob[y * width + x] = 0.9;
            }
        }
        for y in 2..7 {
            for x in 9..15 {
                prob[y * width + x] = 0.9;
            }
        }
        let boxes = boxes_from_map(&prob, width, height);
        assert_eq!(boxes.len(), 2);
        assert!(boxes[0].y0 < boxes[1].y0);
    }

    #[test]
    fn long_side_resize_is_a_multiple_of_32() {
        let (width, height) = det_size(200, 40);
        assert_eq!(width % 32, 0);
        assert_eq!(height % 32, 0);
        assert!(width >= 32 && height >= 32);
        assert!(width.max(height) >= 960);
    }

    #[test]
    fn without_a_model_the_engine_stays_off_and_does_not_fetch() {
        let det = std::env::temp_dir().join(format!("w3cos-screen-empty-{}", std::process::id()));
        let rec =
            std::env::temp_dir().join(format!("w3cos-screen-rec-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&det);
        let _ = std::fs::remove_dir_all(&rec);
        std::fs::create_dir_all(&det).unwrap();
        std::fs::create_dir_all(&rec).unwrap();
        let engine = PaddleScreenText::new(det.clone(), rec.clone());
        let manifest = engine.manifest();
        assert_eq!(manifest.id, "builtin.pp-ocrv5-mobile-det");
        assert!(!manifest.network);
        assert!(!manifest.available);
        let mut engine = engine;
        let image = ScreenImage {
            width: 16,
            height: 16,
            rgba: vec![255; 16 * 16 * 4],
        };
        let region = ScreenRegion {
            x: 0,
            y: 0,
            width: 16,
            height: 16,
        };
        assert!(engine.recognize(&image, region).is_empty());
        assert!(std::fs::read_dir(&det).unwrap().next().is_none());
        assert!(std::fs::read_dir(&rec).unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(&det);
        let _ = std::fs::remove_dir_all(&rec);
    }

    #[test]
    fn pinned_models_read_a_blank_region_on_cpu() {
        let Some(det) = std::env::var_os("W3COS_SCREEN_TEXT_MODEL_DIR") else {
            return;
        };
        let Some(rec) = std::env::var_os("W3COS_HANDWRITING_MODEL_DIR") else {
            return;
        };
        let det = PathBuf::from(det);
        let rec = PathBuf::from(rec);
        assert!(
            screen_text_ready(&det, &rec),
            "model dirs are not the pinned files"
        );
        let mut engine = PaddleScreenText::new(det, rec);
        assert!(engine.manifest().available);
        assert!(!engine.manifest().network);
        let image = ScreenImage {
            width: 64,
            height: 32,
            rgba: vec![255; 64 * 32 * 4],
        };
        let region = ScreenRegion {
            x: 0,
            y: 0,
            width: 64,
            height: 32,
        };
        let snippets = engine.recognize(&image, region);
        assert!(
            snippets.len() <= 8,
            "blank region should not explode into text: {snippets:?}"
        );
    }
}
