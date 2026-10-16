//! On-device streaming recognizer backed by whisper.cpp.
//!
//! [`WhisperSpeech`] implements [`LocalSpeech`]. `available` is true only when
//! the pinned model file is on disk and matches its SHA-256. The microphone
//! (or a local WAV, for a fixture) opens inside `begin`. `poll` emits interim
//! text from a growing 16 kHz window. `flush` decodes once more and marks the
//! text final. No method here downloads or opens a socket.
//!
//! whisper.cpp is a windowed encoder-decoder, not a frame transducer. Partials
//! are greedy decodes of the audio held so far, on CPU, with the GPU flag off.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use crate::speech::{LocalSpeech, SpeechChunk, SpeechManifest, SpeechOff, SpeechStart};
use crate::speech_model::{self, MODEL_FILE, model_ready};
use crate::speech_wav::{SAMPLE_RATE, downmix_resample, parse_wav};

const STORE_MAX: usize = SAMPLE_RATE as usize * 20;
const WINDOW: usize = SAMPLE_RATE as usize * 8;
const MIN_PARTIAL: usize = SAMPLE_RATE as usize;
const MIN_FINAL: usize = SAMPLE_RATE as usize / 2;
const PARTIAL_STEP: usize = SAMPLE_RATE as usize * 3 / 4;

enum Backend {
    Whisper,
    #[cfg(test)]
    Script(Box<dyn FnMut(&[f32]) -> String + Send>),
}

enum Source {
    Idle,
    Pcm(PcmCursor),
    Mic(MicCapture),
}

struct PcmCursor {
    samples: Vec<f32>,
    cursor: usize,
    chunk: usize,
}

impl PcmCursor {
    fn new(samples: Vec<f32>, chunk: usize) -> Self {
        Self {
            samples,
            cursor: 0,
            chunk: chunk.max(1),
        }
    }

    fn rewind(&mut self) {
        self.cursor = 0;
    }

    fn pull(&mut self) -> Vec<f32> {
        if self.cursor >= self.samples.len() {
            return Vec::new();
        }
        let end = (self.cursor + self.chunk).min(self.samples.len());
        let out = self.samples[self.cursor..end].to_vec();
        self.cursor = end;
        out
    }
}

/// The cpal stream is not `Send`. It stays on a dedicated thread. This handle
/// only shares the sample buffer, so the recognizer can move between threads.
struct MicCapture {
    stop: Arc<AtomicBool>,
    samples: Arc<Mutex<Vec<f32>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MicCapture {
    fn pull(&self) -> Vec<f32> {
        let mut guard = self
            .samples
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        std::mem::take(&mut *guard)
    }
}

impl Drop for MicCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Bundled recognizer. Without a verified model this stays unavailable.
pub struct WhisperSpeech {
    dir: PathBuf,
    state: Option<WhisperState>,
    loaded_modified: Option<SystemTime>,
    capturing: bool,
    samples: Vec<f32>,
    decoded: usize,
    last: String,
    language: Option<String>,
    source: Source,
    /// Test builds swap in a script. Production always uses whisper.cpp.
    #[cfg_attr(not(test), allow(dead_code))]
    backend: Backend,
    #[cfg(test)]
    forced_available: bool,
    #[cfg(test)]
    mic_attempts: u32,
}

impl WhisperSpeech {
    pub fn open_default() -> Self {
        Self::new(speech_model::default_speech_dir())
    }

    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            state: None,
            loaded_modified: None,
            capturing: false,
            samples: Vec::new(),
            decoded: 0,
            last: String::new(),
            language: None,
            source: Source::Idle,
            backend: Backend::Whisper,
            #[cfg(test)]
            forced_available: false,
            #[cfg(test)]
            mic_attempts: 0,
        }
    }

    #[cfg(test)]
    pub fn with_pcm_script(
        pcm: Vec<f32>,
        script: impl FnMut(&[f32]) -> String + Send + 'static,
    ) -> Self {
        let mut engine = Self::new(PathBuf::new());
        engine.backend = Backend::Script(Box::new(script));
        engine.forced_available = true;
        engine.source = Source::Pcm(PcmCursor::new(pcm, 8_000));
        engine
    }

    #[cfg(test)]
    pub fn use_pcm_for_tests(&mut self, pcm: Vec<f32>) {
        self.source = Source::Pcm(PcmCursor::new(pcm, 8_000));
    }

    #[cfg(test)]
    pub fn mic_attempts(&self) -> u32 {
        self.mic_attempts
    }

    fn ready(&self) -> bool {
        #[cfg(test)]
        if self.forced_available {
            return true;
        }
        model_ready(&self.dir)
    }

    fn arm_source(&mut self) -> bool {
        if let Source::Pcm(cursor) = &mut self.source {
            cursor.rewind();
            return true;
        }
        if let Some(path) = std::env::var_os("W3COS_SPEECH_WAV") {
            return self.arm_wav(Path::new(&path));
        }
        #[cfg(test)]
        {
            self.mic_attempts = self.mic_attempts.saturating_add(1);
        }
        match open_mic() {
            Some(mic) => {
                self.source = Source::Mic(mic);
                true
            }
            None => false,
        }
    }

    fn arm_wav(&mut self, path: &Path) -> bool {
        let Ok(bytes) = std::fs::read(path) else {
            return false;
        };
        let Ok(wav) = parse_wav(&bytes) else {
            return false;
        };
        let samples = if wav.sample_rate == SAMPLE_RATE {
            wav.samples
        } else {
            downmix_resample(&wav.samples, 1, wav.sample_rate)
        };
        if samples.is_empty() {
            return false;
        }
        self.source = Source::Pcm(PcmCursor::new(samples, 1_600));
        true
    }

    fn ensure_model(&mut self) -> bool {
        #[cfg(test)]
        if matches!(self.backend, Backend::Script(_)) {
            return true;
        }
        self.ensure_whisper()
    }

    fn ensure_whisper(&mut self) -> bool {
        let modified = std::fs::metadata(self.dir.join(MODEL_FILE))
            .ok()
            .and_then(|meta| meta.modified().ok());
        if self.state.is_some() && self.loaded_modified == modified {
            return true;
        }
        self.state = None;
        // Without a log feature this drops whisper.cpp's stderr trace.
        whisper_rs::install_logging_hooks();
        let mut parameters = WhisperContextParameters::default();
        parameters.use_gpu(false);
        let ctx = WhisperContext::new_with_params(self.dir.join(MODEL_FILE), parameters).ok();
        let Some(ctx) = ctx else {
            return false;
        };
        self.state = ctx.create_state().ok();
        self.loaded_modified = modified;
        self.state.is_some()
    }

    fn pull(&mut self) -> Vec<f32> {
        match &mut self.source {
            Source::Idle => Vec::new(),
            Source::Pcm(cursor) => cursor.pull(),
            Source::Mic(mic) => mic.pull(),
        }
    }

    fn ingest(&mut self) {
        let fresh = self.pull();
        if fresh.is_empty() {
            return;
        }
        self.samples.extend(fresh);
        if self.samples.len() > STORE_MAX {
            let drop_n = self.samples.len() - STORE_MAX;
            self.samples.drain(..drop_n);
            self.decoded = self.decoded.saturating_sub(drop_n);
        }
    }

    fn hypothesis(&mut self, force: bool) -> Option<SpeechChunk> {
        self.ingest();
        let min = if force { MIN_FINAL } else { MIN_PARTIAL };
        if self.samples.len() < min {
            if force && !self.last.is_empty() {
                return Some(SpeechChunk {
                    text: self.last.clone(),
                    interim: false,
                });
            }
            return None;
        }
        if !force && self.decoded > 0 && self.samples.len() - self.decoded < PARTIAL_STEP {
            return None;
        }
        let start = self.samples.len().saturating_sub(WINDOW);
        let window = self.samples[start..].to_vec();
        let Some(text) = self.transcribe(&window) else {
            self.decoded = self.samples.len();
            if force && !self.last.is_empty() {
                return Some(SpeechChunk {
                    text: self.last.clone(),
                    interim: false,
                });
            }
            return None;
        };
        self.decoded = self.samples.len();
        if text == self.last && !force {
            return None;
        }
        self.last = text.clone();
        Some(SpeechChunk {
            text,
            interim: !force,
        })
    }

    fn transcribe(&mut self, pcm: &[f32]) -> Option<String> {
        #[cfg(test)]
        if let Backend::Script(script) = &mut self.backend {
            let text = clean(&script(pcm));
            return if text.is_empty() { None } else { Some(text) };
        }
        self.transcribe_whisper(pcm)
    }

    fn transcribe_whisper(&mut self, pcm: &[f32]) -> Option<String> {
        let state = self.state.as_mut()?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(2);
        params.set_translate(false);
        params.set_no_context(true);
        params.set_single_segment(true);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_print_special(false);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        params.set_temperature(0.0);
        params.set_temperature_inc(0.0);
        let language = self.language.clone();
        match language.as_deref() {
            Some(code) => {
                params.set_language(Some(code));
                params.set_detect_language(false);
            }
            None => {
                params.set_language(Some("auto"));
                params.set_detect_language(true);
            }
        }
        state.full(params, pcm).ok()?;
        let mut raw = String::new();
        for index in 0..state.full_n_segments() {
            if let Some(segment) = state.get_segment(index)
                && let Ok(text) = segment.to_str_lossy()
            {
                raw.push_str(&text);
            }
        }
        let text = clean(&raw);
        if text.is_empty() { None } else { Some(text) }
    }
}

impl LocalSpeech for WhisperSpeech {
    fn manifest(&self) -> SpeechManifest {
        SpeechManifest {
            id: "builtin.whisper-cpp".into(),
            locales: vec!["zh-Hans".into(), "en".into()],
            network: false,
            available: self.ready(),
        }
    }

    fn begin(&mut self, locales: &[String]) -> SpeechStart {
        self.language = whisper_language(locales);
        if !self.ready() {
            self.capturing = false;
            return SpeechStart::Off(SpeechOff::NoLocalModel);
        }
        if !self.ensure_model() {
            self.capturing = false;
            return SpeechStart::Off(SpeechOff::NoLocalModel);
        }
        if !self.arm_source() {
            self.capturing = false;
            return SpeechStart::Off(SpeechOff::NoMicrophone);
        }
        self.samples.clear();
        self.decoded = 0;
        self.last.clear();
        self.capturing = true;
        SpeechStart::Listening
    }

    fn poll(&mut self) -> Option<SpeechChunk> {
        if !self.capturing {
            return None;
        }
        self.hypothesis(false)
    }

    fn flush(&mut self) -> Option<SpeechChunk> {
        if !self.capturing {
            return None;
        }
        self.hypothesis(true)
    }

    fn end(&mut self) {
        self.capturing = false;
        self.samples.clear();
        self.decoded = 0;
        self.source = Source::Idle;
    }

    fn capturing(&self) -> bool {
        self.capturing
    }
}

/// `zh` or `en` when the session is one language. Mixed or empty stays auto.
pub fn whisper_language(locales: &[String]) -> Option<String> {
    let zh = locales.iter().any(|tag| {
        let tag = tag.to_ascii_lowercase();
        tag == "zh" || tag.starts_with("zh-")
    });
    let en = locales.iter().any(|tag| {
        let tag = tag.to_ascii_lowercase();
        tag == "en" || tag.starts_with("en-")
    });
    match (zh, en) {
        (true, false) => Some("zh".into()),
        (false, true) => Some("en".into()),
        _ => None,
    }
}

fn clean(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '[' {
            for next in chars.by_ref() {
                if next == ']' {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn open_mic() -> Option<MicCapture> {
    let samples = Arc::new(Mutex::new(Vec::<f32>::new()));
    let produced = Arc::clone(&samples);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::Builder::new()
        .name("w3cos-speech-mic".into())
        .spawn(move || {
            let Some(stream) = start_input(produced) else {
                let _ = ready_tx.send(false);
                return;
            };
            if stream.play().is_err() {
                let _ = ready_tx.send(false);
                return;
            }
            let _ = ready_tx.send(true);
            while !stop_flag.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
            }
            drop(stream);
        })
        .ok()?;
    if !ready_rx.recv().unwrap_or(false) {
        let _ = thread.join();
        return None;
    }
    Some(MicCapture {
        stop,
        samples,
        thread: Some(thread),
    })
}

fn start_input(produced: Arc<Mutex<Vec<f32>>>) -> Option<cpal::Stream> {
    let host = cpal::default_host();
    let device = host.default_input_device()?;
    let supported = device.default_input_config().ok()?;
    let channels = supported.channels() as usize;
    let rate = supported.sample_rate().0;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    match format {
        cpal::SampleFormat::F32 => {
            input_stream::<f32, _>(&device, &config, produced, channels, rate, |sample| sample)
        }
        cpal::SampleFormat::I16 => {
            input_stream::<i16, _>(&device, &config, produced, channels, rate, |sample| {
                f32::from(sample) / 32768.0
            })
        }
        cpal::SampleFormat::U16 => {
            input_stream::<u16, _>(&device, &config, produced, channels, rate, |sample| {
                (f32::from(sample) - 32768.0) / 32768.0
            })
        }
        _ => None,
    }
}

fn input_stream<T, F>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    produced: Arc<Mutex<Vec<f32>>>,
    channels: usize,
    rate: u32,
    convert: F,
) -> Option<cpal::Stream>
where
    T: cpal::SizedSample + Send + 'static,
    F: Fn(T) -> f32 + Send + Copy + 'static,
{
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                let floats: Vec<f32> = data.iter().copied().map(convert).collect();
                let mixed = downmix_resample(&floats, channels, rate);
                if let Ok(mut guard) = produced.lock() {
                    guard.extend(mixed);
                    let cap = SAMPLE_RATE as usize * 30;
                    if guard.len() > cap {
                        let drop_n = guard.len() - cap;
                        guard.drain(..drop_n);
                    }
                }
            },
            |_| {},
            None,
        )
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speech_wav::fixture_pcm;
    use crate::{
        ImeHub, Purpose, SpeechOff, SpeechStart, flush_speech, poll_speech, press_speech,
        release_speech,
    };

    #[test]
    fn language_follows_the_session_and_mix_stays_auto() {
        assert_eq!(whisper_language(&["zh-Hans".into()]).as_deref(), Some("zh"));
        assert_eq!(whisper_language(&["en-US".into()]).as_deref(), Some("en"));
        assert_eq!(whisper_language(&["zh-Hans".into(), "en".into()]), None);
        assert_eq!(clean("  [BLANK_AUDIO] and so  "), "and so");
        assert_eq!(clean("你好 世界"), "你好 世界");
    }

    #[test]
    fn without_a_model_begin_does_not_open_the_microphone() {
        let dir = std::env::temp_dir().join(format!("w3cos-speech-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut engine = WhisperSpeech::new(dir.clone());
        let manifest = engine.manifest();
        assert_eq!(manifest.id, "builtin.whisper-cpp");
        assert!(!manifest.network);
        assert!(!manifest.available);
        assert_eq!(
            manifest.locales,
            vec!["zh-Hans".to_string(), "en".to_string()]
        );
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("s", Purpose::Text, vec!["zh-Hans".into(), "en".into()]);
        assert_eq!(
            press_speech(&mut engine, &session),
            SpeechStart::Off(SpeechOff::NoLocalModel)
        );
        assert_eq!(engine.mic_attempts(), 0);
        assert!(!engine.capturing());
        assert!(poll_speech(&mut engine, true).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scripted_audio_streams_partials_then_a_final() {
        let mut engine = WhisperSpeech::with_pcm_script(fixture_pcm(), |samples| {
            let seconds = samples.len() as f32 / SAMPLE_RATE as f32;
            if seconds < 1.0 {
                String::new()
            } else if seconds < 2.5 {
                "and".into()
            } else {
                "and so".into()
            }
        });
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("s", Purpose::Chat, vec!["en".into()]);
        assert!(engine.manifest().available);
        assert!(!engine.manifest().network);
        assert_eq!(press_speech(&mut engine, &session), SpeechStart::Listening);
        assert_eq!(engine.mic_attempts(), 0);
        let mut interim = Vec::new();
        for _ in 0..12 {
            if let Some(chunk) = poll_speech(&mut engine, true) {
                assert!(chunk.interim);
                interim.push(chunk.text);
            }
        }
        assert!(interim.iter().any(|text| text == "and"), "{interim:?}");
        let flushed = flush_speech(&mut engine, true).expect("final");
        assert!(!flushed.interim);
        assert_eq!(flushed.text, "and so");
        release_speech(&mut engine, true);
        assert!(!engine.capturing());
        assert!(engine.samples.is_empty());
        assert!(poll_speech(&mut engine, true).is_none());
    }

    #[test]
    fn pinned_model_transcribes_the_fixture_when_installed() {
        let Some(path) = std::env::var_os("W3COS_SPEECH_MODEL") else {
            return;
        };
        let bytes = std::fs::read(path).expect("model");
        assert_eq!(
            speech_model::sha256_hex(&bytes),
            speech_model::MODEL_SHA256,
            "W3COS_SPEECH_MODEL is not the pinned file"
        );
        let dir = std::env::temp_dir().join(format!("w3cos-speech-model-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        speech_model::install_checked(&dir, &bytes, speech_model::MODEL_SHA256).unwrap();
        assert!(model_ready(&dir));
        let mut engine = WhisperSpeech::new(dir.clone());
        engine.use_pcm_for_tests(fixture_pcm());
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("s", Purpose::Text, vec!["en".into()]);
        assert_eq!(press_speech(&mut engine, &session), SpeechStart::Listening);
        assert_eq!(engine.mic_attempts(), 0);
        let mut saw_interim = false;
        for _ in 0..12 {
            if let Some(chunk) = poll_speech(&mut engine, true) {
                assert!(chunk.interim, "{chunk:?}");
                saw_interim = true;
            }
        }
        let flushed = flush_speech(&mut engine, true).expect("final transcript");
        assert!(!flushed.interim);
        let text = flushed.text.to_ascii_lowercase();
        assert!(
            text.contains("american") || text.contains("ask") || text.contains("country"),
            "transcript: {text}"
        );
        assert!(saw_interim, "no interim before {text}");
        release_speech(&mut engine, true);
        assert!(engine.samples.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
