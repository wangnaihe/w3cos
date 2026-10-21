//! Real-time audio output for `AudioContext` through `cpal`.
//!
//! The device callback runs on the operating-system audio thread and only
//! touches `Send` data: the latest [`GraphSnapshot`] published by the
//! JavaScript thread, the [`Renderer`] that owns playback state, and a few
//! atomics. `cpal::Stream` is not `Send`, so it lives on a dedicated thread
//! that is parked until the context closes.
//!
//! Hosts without a desktop audio stack (Android, iOS, OHOS) or builds without
//! the `web-media-advanced` feature get [`LiveOutput::open`] returning an
//! error, and the context keeps its silent clock with a warning.

#[cfg(all(
    feature = "web-media-advanced",
    any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    )
))]
mod imp {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    use crate::audio_render::{GraphSnapshot, Renderer};

    struct Shared {
        graph: Mutex<Arc<GraphSnapshot>>,
        paused: AtomicBool,
        stop: AtomicBool,
        frames: AtomicU64,
        finished: Mutex<Vec<u64>>,
    }

    pub struct LiveOutput {
        shared: Arc<Shared>,
        thread: Option<JoinHandle<()>>,
        sample_rate: u32,
        channels: u16,
        device_name: String,
    }

    struct Opened {
        sample_rate: u32,
        channels: u16,
        device_name: String,
    }

    impl LiveOutput {
        pub fn open(preferred_rate: f64, start_time: f64) -> Result<Self, String> {
            let shared = Arc::new(Shared {
                graph: Mutex::new(Arc::new(GraphSnapshot::empty())),
                paused: AtomicBool::new(false),
                stop: AtomicBool::new(false),
                frames: AtomicU64::new(0),
                finished: Mutex::new(Vec::new()),
            });
            let (ready_tx, ready_rx) = mpsc::channel::<Result<Opened, String>>();
            let thread_shared = Arc::clone(&shared);
            let thread = thread::Builder::new()
                .name("w3cos-audio-output".into())
                .spawn(move || {
                    let stream = match build_stream(&thread_shared, preferred_rate, start_time) {
                        Ok((stream, opened)) => {
                            let _ = ready_tx.send(Ok(opened));
                            stream
                        }
                        Err(error) => {
                            let _ = ready_tx.send(Err(error));
                            return;
                        }
                    };
                    while !thread_shared.stop.load(Ordering::Acquire) {
                        thread::park_timeout(Duration::from_millis(100));
                    }
                    drop(stream);
                })
                .map_err(|error| format!("cannot spawn the audio output thread: {error}"))?;
            let opened = ready_rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| "the audio output device did not start in time".to_string())
                .and_then(|result| result);
            match opened {
                Ok(opened) => Ok(Self {
                    shared,
                    thread: Some(thread),
                    sample_rate: opened.sample_rate,
                    channels: opened.channels,
                    device_name: opened.device_name,
                }),
                Err(error) => {
                    shared.stop.store(true, Ordering::Release);
                    thread.thread().unpark();
                    let _ = thread.join();
                    Err(error)
                }
            }
        }

        pub fn update(&self, graph: GraphSnapshot) {
            if let Ok(mut current) = self.shared.graph.lock() {
                *current = Arc::new(graph);
            }
        }

        pub fn set_paused(&self, paused: bool) {
            self.shared.paused.store(paused, Ordering::Release);
        }

        pub fn take_finished(&self) -> Vec<u64> {
            self.shared
                .finished
                .lock()
                .map(|mut finished| std::mem::take(&mut *finished))
                .unwrap_or_default()
        }

        pub fn frames(&self) -> u64 {
            self.shared.frames.load(Ordering::Acquire)
        }

        pub fn sample_rate(&self) -> u32 {
            self.sample_rate
        }

        pub fn channels(&self) -> u16 {
            self.channels
        }

        pub fn device_name(&self) -> &str {
            &self.device_name
        }
    }

    impl Drop for LiveOutput {
        fn drop(&mut self) {
            self.shared.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                thread.thread().unpark();
                let _ = thread.join();
            }
        }
    }

    struct Callback {
        shared: Arc<Shared>,
        renderer: Renderer,
        graph: Arc<GraphSnapshot>,
        scratch: Vec<f32>,
        reported: HashSet<u64>,
        channels: usize,
    }

    impl Callback {
        fn fill(&mut self, frames_out: usize) -> &[f32] {
            self.scratch.resize(frames_out, 0.0);
            if let Ok(graph) = self.shared.graph.try_lock() {
                if !Arc::ptr_eq(&self.graph, &graph) {
                    self.graph = Arc::clone(&graph);
                }
            }
            if self.shared.paused.load(Ordering::Acquire) {
                self.scratch.fill(0.0);
                return &self.scratch;
            }
            self.renderer
                .render_interleaved(&self.graph, &mut self.scratch, self.channels);
            self.shared
                .frames
                .fetch_add((frames_out / self.channels.max(1)) as u64, Ordering::AcqRel);
            let newly_finished: Vec<u64> = self
                .renderer
                .finished_sources()
                .into_iter()
                .filter(|id| self.reported.insert(*id))
                .collect();
            if !newly_finished.is_empty() {
                if let Ok(mut finished) = self.shared.finished.try_lock() {
                    finished.extend(newly_finished);
                }
            }
            &self.scratch
        }
    }

    fn build_stream(
        shared: &Arc<Shared>,
        preferred_rate: f64,
        start_time: f64,
    ) -> Result<(cpal::Stream, Opened), String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no default audio output device".to_string())?;
        let device_name = device.name().unwrap_or_else(|_| "default".into());
        let supported = device
            .default_output_config()
            .map_err(|error| format!("cannot query the audio output configuration: {error}"))?;
        let sample_format = supported.sample_format();
        let mut config: cpal::StreamConfig = supported.config();
        let wanted = preferred_rate.round() as u32;
        if let Ok(mut ranges) = device.supported_output_configs() {
            if ranges.any(|range| {
                range.channels() == config.channels
                    && range.sample_format() == sample_format
                    && range.min_sample_rate().0 <= wanted
                    && range.max_sample_rate().0 >= wanted
            }) {
                config.sample_rate = cpal::SampleRate(wanted);
            }
        }
        let channels = config.channels as usize;
        let mut callback = Callback {
            shared: Arc::clone(shared),
            renderer: Renderer::new(config.sample_rate.0 as f64, 2, start_time),
            graph: Arc::new(GraphSnapshot::empty()),
            scratch: Vec::new(),
            reported: HashSet::new(),
            channels,
        };
        let on_error = |error: cpal::StreamError| {
            eprintln!("[w3cos] warning: audio output stream error: {error}");
        };
        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let rendered = callback.fill(data.len());
                    data.copy_from_slice(rendered);
                },
                on_error,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    let rendered = callback.fill(data.len());
                    for (out, sample) in data.iter_mut().zip(rendered) {
                        *out = (sample * i16::MAX as f32) as i16;
                    }
                },
                on_error,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_output_stream(
                &config,
                move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                    let rendered = callback.fill(data.len());
                    for (out, sample) in data.iter_mut().zip(rendered) {
                        *out = ((sample * 0.5 + 0.5) * u16::MAX as f32) as u16;
                    }
                },
                on_error,
                None,
            ),
            other => {
                return Err(format!("unsupported audio output sample format {other:?}"));
            }
        }
        .map_err(|error| format!("cannot open the audio output stream: {error}"))?;
        stream
            .play()
            .map_err(|error| format!("cannot start the audio output stream: {error}"))?;
        Ok((
            stream,
            Opened {
                sample_rate: config.sample_rate.0,
                channels: config.channels,
                device_name,
            },
        ))
    }
}

#[cfg(not(all(
    feature = "web-media-advanced",
    any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    )
)))]
mod imp {
    use crate::audio_render::GraphSnapshot;

    pub struct LiveOutput;

    impl LiveOutput {
        pub fn open(_preferred_rate: f64, _start_time: f64) -> Result<Self, String> {
            Err("this build has no native audio output adapter".into())
        }
        pub fn update(&self, _graph: GraphSnapshot) {}
        pub fn set_paused(&self, _paused: bool) {}
        pub fn take_finished(&self) -> Vec<u64> {
            Vec::new()
        }
        pub fn frames(&self) -> u64 {
            0
        }
        pub fn sample_rate(&self) -> u32 {
            0
        }
        pub fn channels(&self) -> u16 {
            0
        }
        pub fn device_name(&self) -> &str {
            ""
        }
    }
}

pub use imp::LiveOutput;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_render::{GraphSnapshot, NodeKind, NodeSnapshot, Schedule, Waveform};

    /// Opens the default output device and plays a quiet tone. This is
    /// opt-in (`W3COS_AUDIO_DEVICE_TESTS=1`) so `cargo test` never makes a
    /// developer machine beep; CI runners have no sound card anyway.
    #[test]
    fn default_output_device_renders_a_tone_when_enabled() {
        if std::env::var_os("W3COS_AUDIO_DEVICE_TESTS").is_none() {
            eprintln!("skipping device output test: set W3COS_AUDIO_DEVICE_TESTS=1");
            return;
        }
        let output = match LiveOutput::open(48_000.0, 0.0) {
            Ok(output) => output,
            Err(error) => {
                eprintln!("skipping device output test: {error}");
                return;
            }
        };
        let mut graph = GraphSnapshot::empty();
        graph.destination = 1;
        graph.nodes.insert(
            1,
            NodeSnapshot {
                kind: NodeKind::Destination,
                inputs: vec![3],
            },
        );
        graph.nodes.insert(
            3,
            NodeSnapshot {
                kind: NodeKind::Gain { gain: 0.05 },
                inputs: vec![2],
            },
        );
        graph.nodes.insert(
            2,
            NodeSnapshot {
                kind: NodeKind::Oscillator {
                    waveform: Some(Waveform::Sine),
                    table: None,
                    frequency: 440.0,
                    detune: 0.0,
                    schedule: Schedule {
                        start: Some(0.0),
                        stop: None,
                    },
                },
                inputs: vec![],
            },
        );
        output.update(graph);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(output.sample_rate() > 0);
        assert!(output.channels() > 0);
        assert!(
            output.frames() > 0,
            "device {} rendered no frames",
            output.device_name()
        );
        output.set_paused(true);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let paused_at = output.frames();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(output.frames(), paused_at);
    }
}
