//! Web Audio rendering engine shared by `OfflineAudioContext` and the
//! real-time `AudioContext` device output.
//!
//! The JavaScript thread turns the live node graph into a [`GraphSnapshot`]
//! (plain, `Send` data). A [`Renderer`] owns per-source playback state
//! (oscillator phase, buffer playhead) and pulls render quanta from the
//! destination. The same renderer runs inside the offline `startRendering()`
//! loop and inside the audio-device callback, so offline tests exercise the
//! exact code that produces audible output.
//!
//! Rendered node types: `OscillatorNode` (sine/square/sawtooth/triangle and
//! `PeriodicWave`), `AudioBufferSourceNode` (playback rate, detune, offset,
//! duration, looping, linear resampling), `ConstantSourceNode`, `GainNode`,
//! `StereoPannerNode`, `MediaStreamAudioSourceNode` (live PCM from a
//! microphone queue, with linear resampling when the capture rate differs)
//! and `AudioDestinationNode`. Every other node renders silence and is listed
//! in [`GraphSnapshot::unsupported`] so the caller can warn. AudioParams are
//! sampled at their current `value`; automation timelines and AudioParam
//! inputs are not rendered yet.

use std::collections::HashMap;
use std::f64::consts::PI;
use std::sync::Arc;

/// Size of a Web Audio render quantum.
pub const RENDER_QUANTUM: usize = 128;
const WAVETABLE_SIZE: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Waveform {
    Sine,
    Square,
    Sawtooth,
    Triangle,
}

#[derive(Clone, Debug)]
pub enum NodeKind {
    Destination,
    Gain {
        gain: f32,
    },
    StereoPanner {
        pan: f32,
    },
    Oscillator {
        waveform: Option<Waveform>,
        /// Normalised single-cycle table for `type = "custom"`.
        table: Option<Arc<Vec<f32>>>,
        frequency: f64,
        detune: f64,
        schedule: Schedule,
    },
    ConstantSource {
        offset: f32,
        schedule: Schedule,
    },
    BufferSource {
        channels: Arc<Vec<Vec<f32>>>,
        buffer_rate: f64,
        playback_rate: f64,
        detune: f64,
        looping: bool,
        loop_start: f64,
        loop_end: f64,
        offset: f64,
        duration: Option<f64>,
        schedule: Schedule,
    },
    /// Live microphone (or other captured) PCM. `audibility` is loaded while
    /// rendering so mute, disable, and `stop()` take effect immediately.
    MediaStream {
        queue: Arc<crate::audio_input::PcmQueue>,
        audibility: Arc<crate::audio_input::Audibility>,
        channels: u16,
        sample_rate: u32,
    },
    /// A node type this renderer does not process yet; renders silence.
    Silent,
}

/// Context-time start/stop of a scheduled source. `start == None` means the
/// source was never started.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Schedule {
    pub start: Option<f64>,
    pub stop: Option<f64>,
}

impl Schedule {
    fn active_at(&self, time: f64) -> bool {
        match self.start {
            Some(start) => time >= start && self.stop.is_none_or(|stop| time < stop),
            None => false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct NodeSnapshot {
    pub kind: NodeKind,
    /// Upstream node ids connected to this node's input.
    pub inputs: Vec<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct GraphSnapshot {
    pub nodes: HashMap<u64, NodeSnapshot>,
    pub destination: u64,
    /// Class names of connected nodes that rendered as silence.
    pub unsupported: Vec<String>,
}

impl GraphSnapshot {
    pub fn empty() -> Self {
        Self::default()
    }
}

#[derive(Default)]
struct SourceState {
    phase: f64,
    position: Option<f64>,
    finished: bool,
    /// Absolute capture-frame position of a media-stream source.
    mic_pos: Option<f64>,
}

/// Pull-model renderer with persistent per-source state.
pub struct Renderer {
    sample_rate: f64,
    channels: usize,
    /// Context time of the next rendered frame.
    time: f64,
    frames: u64,
    state: HashMap<u64, SourceState>,
}

type Bus = Vec<Vec<f32>>;

impl Renderer {
    pub fn new(sample_rate: f64, channels: usize, start_time: f64) -> Self {
        Self {
            sample_rate,
            channels: channels.max(1),
            time: start_time,
            frames: 0,
            state: HashMap::new(),
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn current_time(&self) -> f64 {
        self.time
    }

    pub fn frames_rendered(&self) -> u64 {
        self.frames
    }

    /// Ids of buffer sources that played to their natural end.
    pub fn finished_sources(&self) -> Vec<u64> {
        self.state
            .iter()
            .filter(|(_, state)| state.finished)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Renders `frames` frames into planar channel buffers.
    pub fn render_planar(&mut self, graph: &GraphSnapshot, frames: usize) -> Bus {
        let mut out = vec![vec![0.0f32; frames]; self.channels];
        let mut done = 0;
        while done < frames {
            let block = (frames - done).min(RENDER_QUANTUM);
            let rendered = self.render_quantum(graph, block);
            for (channel, samples) in out.iter_mut().zip(rendered) {
                channel[done..done + block].copy_from_slice(&samples);
            }
            done += block;
        }
        out
    }

    /// Renders interleaved, clamped samples for an audio device with
    /// `device_channels` channels.
    pub fn render_interleaved(
        &mut self,
        graph: &GraphSnapshot,
        out: &mut [f32],
        device_channels: usize,
    ) {
        let device_channels = device_channels.max(1);
        let frames = out.len() / device_channels;
        let planar = self.render_planar(graph, frames);
        let mixed = mix_to(planar, device_channels, frames);
        for frame in 0..frames {
            for channel in 0..device_channels {
                out[frame * device_channels + channel] = mixed[channel][frame].clamp(-1.0, 1.0);
            }
        }
    }

    fn render_quantum(&mut self, graph: &GraphSnapshot, frames: usize) -> Bus {
        let mut memo: HashMap<u64, Bus> = HashMap::new();
        let mut visiting = Vec::new();
        let bus = if graph.nodes.contains_key(&graph.destination) {
            self.pull(graph, graph.destination, frames, &mut memo, &mut visiting)
        } else {
            vec![vec![0.0; frames]]
        };
        self.time += frames as f64 / self.sample_rate;
        self.frames += frames as u64;
        mix_to(bus, self.channels, frames)
    }

    fn pull(
        &mut self,
        graph: &GraphSnapshot,
        id: u64,
        frames: usize,
        memo: &mut HashMap<u64, Bus>,
        visiting: &mut Vec<u64>,
    ) -> Bus {
        if let Some(bus) = memo.get(&id) {
            return bus.clone();
        }
        let Some(node) = graph.nodes.get(&id) else {
            return vec![vec![0.0; frames]];
        };
        // Cycles are only legal through a DelayNode, which is not rendered;
        // break them with silence instead of recursing forever.
        if visiting.contains(&id) {
            return vec![vec![0.0; frames]];
        }
        visiting.push(id);
        let inputs: Vec<Bus> = node
            .inputs
            .iter()
            .map(|input| self.pull(graph, *input, frames, memo, visiting))
            .collect();
        visiting.pop();
        let bus = self.process(id, &node.kind, inputs, frames);
        memo.insert(id, bus.clone());
        bus
    }

    fn process(&mut self, id: u64, kind: &NodeKind, inputs: Vec<Bus>, frames: usize) -> Bus {
        let rate = self.sample_rate;
        let t0 = self.time;
        match kind {
            NodeKind::Destination => sum_inputs(inputs, frames),
            NodeKind::Silent => vec![vec![0.0; frames]],
            NodeKind::Gain { gain } => {
                let mut bus = sum_inputs(inputs, frames);
                for channel in &mut bus {
                    for sample in channel.iter_mut() {
                        *sample *= gain;
                    }
                }
                bus
            }
            NodeKind::StereoPanner { pan } => {
                let bus = sum_inputs(inputs, frames);
                stereo_pan(bus, *pan, frames)
            }
            NodeKind::ConstantSource { offset, schedule } => {
                let mut out = vec![0.0; frames];
                for (index, sample) in out.iter_mut().enumerate() {
                    if schedule.active_at(t0 + index as f64 / rate) {
                        *sample = *offset;
                    }
                }
                vec![out]
            }
            NodeKind::Oscillator {
                waveform,
                table,
                frequency,
                detune,
                schedule,
            } => {
                let hz = frequency * 2f64.powf(detune / 1200.0);
                let state = self.state.entry(id).or_default();
                let mut out = vec![0.0; frames];
                if !hz.is_finite() || hz.abs() >= rate / 2.0 {
                    return vec![out];
                }
                let step = hz / rate;
                for (index, sample) in out.iter_mut().enumerate() {
                    if !schedule.active_at(t0 + index as f64 / rate) {
                        continue;
                    }
                    *sample = match (waveform, table) {
                        (Some(waveform), _) => wave_sample(*waveform, state.phase),
                        (None, Some(table)) => table_sample(table, state.phase),
                        (None, None) => 0.0,
                    };
                    state.phase = (state.phase + step).rem_euclid(1.0);
                }
                vec![out]
            }
            NodeKind::BufferSource {
                channels,
                buffer_rate,
                playback_rate,
                detune,
                looping,
                loop_start,
                loop_end,
                offset,
                duration,
                schedule,
            } => {
                let width = channels.len().max(1);
                let mut out = vec![vec![0.0f32; frames]; width];
                let length = channels.first().map_or(0, Vec::len);
                let state = self.state.entry(id).or_default();
                if length == 0 || state.finished {
                    return out;
                }
                let speed = playback_rate * 2f64.powf(detune / 1200.0) * buffer_rate / rate;
                let len = length as f64;
                let (ls, le) = {
                    let ls = (loop_start * buffer_rate).clamp(0.0, len);
                    let le = loop_end * buffer_rate;
                    if *loop_end > 0.0 && le > ls && le <= len {
                        (ls, le)
                    } else {
                        (0.0, len)
                    }
                };
                for index in 0..frames {
                    let time = t0 + index as f64 / rate;
                    let mut schedule = *schedule;
                    if let (Some(start), Some(duration)) = (schedule.start, duration) {
                        let end = start + duration;
                        schedule.stop = Some(schedule.stop.map_or(end, |stop| stop.min(end)));
                    }
                    if !schedule.active_at(time) {
                        if state.position.is_some()
                            && schedule.stop.is_some_and(|stop| time >= stop)
                        {
                            state.finished = true;
                            break;
                        }
                        continue;
                    }
                    let position = *state
                        .position
                        .get_or_insert_with(|| (offset * buffer_rate).clamp(0.0, len));
                    let position = if *looping && position >= le && le > ls {
                        ls + (position - le).rem_euclid(le - ls)
                    } else {
                        position
                    };
                    if !*looping && (position >= len || position < 0.0) {
                        state.finished = true;
                        break;
                    }
                    let base = position.floor() as usize;
                    let fraction = (position - base as f64) as f32;
                    let next = if base + 1 < length {
                        Some(base + 1)
                    } else if *looping {
                        Some(ls as usize)
                    } else {
                        None
                    };
                    for (channel, data) in channels.iter().enumerate() {
                        let current = data[base.min(length - 1)];
                        let following = next.map_or(0.0, |next| data[next.min(length - 1)]);
                        out[channel][index] = current + (following - current) * fraction;
                    }
                    state.position = Some(position + speed);
                }
                out
            }
            NodeKind::MediaStream {
                queue,
                audibility,
                channels,
                sample_rate,
            } => self.render_media(id, queue, audibility, *channels, *sample_rate, frames),
        }
    }

    fn render_media(
        &mut self,
        id: u64,
        queue: &crate::audio_input::PcmQueue,
        audibility: &crate::audio_input::Audibility,
        channels: u16,
        sample_rate: u32,
        frames: usize,
    ) -> Bus {
        let channels = channels.max(1) as usize;
        let mut out = vec![vec![0.0f32; frames]; channels];
        if frames == 0 {
            return out;
        }
        let in_rate = f64::from(sample_rate.max(1));
        let step = in_rate / self.sample_rate.max(1.0);
        let mut pos = self
            .state
            .get(&id)
            .and_then(|state| state.mic_pos)
            .unwrap_or_else(|| queue.start_cursor() as f64);
        if (step - 1.0).abs() < 1e-9 && (pos - pos.round()).abs() < 1e-6 {
            let mut cursor = pos.round() as u64;
            let raw = queue.read_frames(&mut cursor, frames);
            let got = raw.len() / channels;
            for frame in 0..got {
                if !audibility.audible() {
                    continue;
                }
                for channel in 0..channels {
                    out[channel][frame] = raw[frame * channels + channel];
                }
            }
            pos = cursor as f64;
        } else {
            for frame in 0..frames {
                let left_index = pos.floor() as u64;
                let fraction = (pos - left_index as f64) as f32;
                let Some(left) = queue.copy_frame(left_index) else {
                    break;
                };
                let right = if fraction > 0.0 {
                    let Some(right) = queue.copy_frame(left_index + 1) else {
                        break;
                    };
                    right
                } else {
                    left.clone()
                };
                if audibility.audible() {
                    for channel in 0..channels {
                        let start = left.get(channel).copied().unwrap_or(0.0);
                        let end = right.get(channel).copied().unwrap_or(start);
                        out[channel][frame] = start + (end - start) * fraction;
                    }
                }
                pos += step;
            }
        }
        self.state.entry(id).or_default().mic_pos = Some(pos);
        out
    }
}

fn wave_sample(waveform: Waveform, phase: f64) -> f32 {
    (match waveform {
        Waveform::Sine => (2.0 * PI * phase).sin(),
        Waveform::Square => {
            if phase < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        Waveform::Sawtooth => {
            if phase < 0.5 {
                2.0 * phase
            } else {
                2.0 * phase - 2.0
            }
        }
        Waveform::Triangle => {
            if phase < 0.25 {
                4.0 * phase
            } else if phase < 0.75 {
                2.0 - 4.0 * phase
            } else {
                4.0 * phase - 4.0
            }
        }
    }) as f32
}

fn table_sample(table: &[f32], phase: f64) -> f32 {
    let position = phase * table.len() as f64;
    let base = position.floor() as usize % table.len();
    let next = (base + 1) % table.len();
    let fraction = (position - position.floor()) as f32;
    table[base] + (table[next] - table[base]) * fraction
}

/// Builds the `PeriodicWave` single-cycle table from Fourier coefficients.
pub fn periodic_wave_table(real: &[f64], imag: &[f64], normalize: bool) -> Vec<f32> {
    let harmonics = real.len().max(imag.len());
    let mut table = vec![0.0f64; WAVETABLE_SIZE];
    for (index, sample) in table.iter_mut().enumerate() {
        let t = index as f64 / WAVETABLE_SIZE as f64;
        let mut value = 0.0;
        for k in 1..harmonics {
            let a = real.get(k).copied().unwrap_or(0.0);
            let b = imag.get(k).copied().unwrap_or(0.0);
            let angle = 2.0 * PI * k as f64 * t;
            value += a * angle.cos() + b * angle.sin();
        }
        *sample = value;
    }
    if normalize {
        let peak = table
            .iter()
            .fold(0.0f64, |peak, value| peak.max(value.abs()));
        if peak > 0.0 {
            for sample in &mut table {
                *sample /= peak;
            }
        }
    }
    table.into_iter().map(|sample| sample as f32).collect()
}

fn sum_inputs(inputs: Vec<Bus>, frames: usize) -> Bus {
    let width = inputs.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let mut out = vec![vec![0.0f32; frames]; width];
    for input in inputs {
        let input = mix_to(input, width, frames);
        for (channel, samples) in out.iter_mut().zip(input) {
            for (sum, sample) in channel.iter_mut().zip(samples) {
                *sum += sample;
            }
        }
    }
    out
}

/// Web Audio "speakers" up/down-mixing for mono and stereo, discrete otherwise.
fn mix_to(bus: Bus, channels: usize, frames: usize) -> Bus {
    let have = bus.len();
    if have == channels {
        return bus;
    }
    match (have, channels) {
        (0, _) => vec![vec![0.0; frames]; channels],
        (1, 2) => vec![bus[0].clone(), bus[0].clone()],
        (2, 1) => vec![
            bus[0]
                .iter()
                .zip(&bus[1])
                .map(|(left, right)| 0.5 * (left + right))
                .collect(),
        ],
        _ => (0..channels)
            .map(|channel| {
                bus.get(channel)
                    .cloned()
                    .unwrap_or_else(|| vec![0.0; frames])
            })
            .collect(),
    }
}

/// Equal-power `StereoPannerNode` processing.
fn stereo_pan(bus: Bus, pan: f32, frames: usize) -> Bus {
    let pan = pan.clamp(-1.0, 1.0) as f64;
    let mut left = vec![0.0f32; frames];
    let mut right = vec![0.0f32; frames];
    if bus.len() == 1 {
        let x = (pan + 1.0) / 2.0;
        let (gain_l, gain_r) = ((x * PI / 2.0).cos() as f32, (x * PI / 2.0).sin() as f32);
        for index in 0..frames {
            left[index] = bus[0][index] * gain_l;
            right[index] = bus[0][index] * gain_r;
        }
    } else {
        let stereo = mix_to(bus, 2, frames);
        let x = if pan <= 0.0 { pan + 1.0 } else { pan };
        let (gain_l, gain_r) = ((x * PI / 2.0).cos() as f32, (x * PI / 2.0).sin() as f32);
        for index in 0..frames {
            let (l, r) = (stereo[0][index], stereo[1][index]);
            if pan <= 0.0 {
                left[index] = l + r * gain_l;
                right[index] = r * gain_r;
            } else {
                left[index] = l * gain_l;
                right[index] = r + l * gain_r;
            }
        }
    }
    vec![left, right]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(nodes: Vec<(u64, NodeKind, Vec<u64>)>) -> GraphSnapshot {
        GraphSnapshot {
            nodes: nodes
                .into_iter()
                .map(|(id, kind, inputs)| (id, NodeSnapshot { kind, inputs }))
                .collect(),
            destination: 1,
            unsupported: Vec::new(),
        }
    }

    fn started(start: f64) -> Schedule {
        Schedule {
            start: Some(start),
            stop: None,
        }
    }

    fn oscillator(waveform: Waveform, frequency: f64, schedule: Schedule) -> NodeKind {
        NodeKind::Oscillator {
            waveform: Some(waveform),
            table: None,
            frequency,
            detune: 0.0,
            schedule,
        }
    }

    #[test]
    fn sine_oscillator_through_gain_matches_closed_form() {
        let graph = graph(vec![
            (1, NodeKind::Destination, vec![3]),
            (2, oscillator(Waveform::Sine, 440.0, started(0.0)), vec![]),
            (3, NodeKind::Gain { gain: 0.5 }, vec![2]),
        ]);
        let mut renderer = Renderer::new(48_000.0, 1, 0.0);
        let out = renderer.render_planar(&graph, 1000);
        for (index, sample) in out[0].iter().enumerate() {
            let expected = 0.5 * (2.0 * PI * 440.0 * index as f64 / 48_000.0).sin();
            assert!((*sample as f64 - expected).abs() < 1e-4, "frame {index}");
        }
        assert_eq!(renderer.frames_rendered(), 1000);
    }

    #[test]
    fn mono_source_upmixes_to_stereo_destination_and_detune_shifts_pitch() {
        let graph = graph(vec![
            (1, NodeKind::Destination, vec![2]),
            (
                2,
                NodeKind::Oscillator {
                    waveform: Some(Waveform::Square),
                    table: None,
                    frequency: 1_000.0,
                    detune: 1_200.0,
                    schedule: started(0.0),
                },
                vec![],
            ),
        ]);
        let mut renderer = Renderer::new(8_000.0, 2, 0.0);
        let out = renderer.render_planar(&graph, 8);
        // 2 kHz square at 8 kHz: + + - - repeating, identical on both channels.
        assert_eq!(out[0], vec![1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0]);
        assert_eq!(out[0], out[1]);
    }

    #[test]
    fn scheduled_sources_start_and_stop_on_sample_boundaries() {
        let graph = graph(vec![
            (1, NodeKind::Destination, vec![2]),
            (
                2,
                NodeKind::ConstantSource {
                    offset: 0.25,
                    schedule: Schedule {
                        start: Some(0.5),
                        stop: Some(0.75),
                    },
                },
                vec![],
            ),
        ]);
        let mut renderer = Renderer::new(8.0, 1, 0.0);
        let out = renderer.render_planar(&graph, 8);
        assert_eq!(out[0], vec![0.0, 0.0, 0.0, 0.0, 0.25, 0.25, 0.0, 0.0]);
    }

    #[test]
    fn buffer_source_resamples_loops_and_reports_natural_end() {
        let buffer = Arc::new(vec![vec![0.0, 1.0, 2.0, 3.0]]);
        let source = |looping, playback_rate| NodeKind::BufferSource {
            channels: Arc::clone(&buffer),
            buffer_rate: 4.0,
            playback_rate,
            detune: 0.0,
            looping,
            loop_start: 0.0,
            loop_end: 0.0,
            offset: 0.0,
            duration: None,
            schedule: started(0.0),
        };
        let once = graph(vec![
            (1, NodeKind::Destination, vec![2]),
            (2, source(false, 1.0), vec![]),
        ]);
        let mut renderer = Renderer::new(4.0, 1, 0.0);
        assert_eq!(
            renderer.render_planar(&once, 6)[0],
            vec![0.0, 1.0, 2.0, 3.0, 0.0, 0.0]
        );
        assert_eq!(renderer.finished_sources(), vec![2]);

        let half_speed = graph(vec![
            (1, NodeKind::Destination, vec![2]),
            (2, source(false, 0.5), vec![]),
        ]);
        let mut renderer = Renderer::new(4.0, 1, 0.0);
        assert_eq!(
            renderer.render_planar(&half_speed, 4)[0],
            vec![0.0, 0.5, 1.0, 1.5]
        );

        let looped = graph(vec![
            (1, NodeKind::Destination, vec![2]),
            (2, source(true, 1.0), vec![]),
        ]);
        let mut renderer = Renderer::new(4.0, 1, 0.0);
        assert_eq!(
            renderer.render_planar(&looped, 6)[0],
            vec![0.0, 1.0, 2.0, 3.0, 0.0, 1.0]
        );
        assert!(renderer.finished_sources().is_empty());
    }

    #[test]
    fn fan_in_sums_and_device_output_is_interleaved_and_clamped() {
        let graph = graph(vec![
            (1, NodeKind::Destination, vec![2, 3]),
            (
                2,
                NodeKind::ConstantSource {
                    offset: 0.75,
                    schedule: started(0.0),
                },
                vec![],
            ),
            (
                3,
                NodeKind::ConstantSource {
                    offset: 0.5,
                    schedule: started(0.0),
                },
                vec![],
            ),
        ]);
        let mut renderer = Renderer::new(48_000.0, 2, 0.0);
        assert_eq!(renderer.render_planar(&graph, 1)[0][0], 1.25);
        let mut device = vec![0.0f32; 6];
        renderer.render_interleaved(&graph, &mut device, 2);
        assert_eq!(device, vec![1.0; 6]);
    }

    #[test]
    fn periodic_wave_table_and_stereo_panner_follow_spec_formulas() {
        let table = periodic_wave_table(&[0.0, 0.0], &[0.0, 2.0], true);
        assert!((table[WAVETABLE_SIZE / 4] - 1.0).abs() < 1e-6);
        assert!(table[0].abs() < 1e-6);
        let panned = stereo_pan(vec![vec![1.0]], 1.0, 1);
        assert!(panned[0][0].abs() < 1e-6);
        assert!((panned[1][0] - 1.0).abs() < 1e-6);
        let centred = stereo_pan(vec![vec![1.0]], 0.0, 1);
        assert!((centred[0][0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    }

    #[test]
    fn unsupported_nodes_and_cycles_render_silence() {
        let graph = graph(vec![
            (1, NodeKind::Destination, vec![3]),
            (
                2,
                NodeKind::ConstantSource {
                    offset: 1.0,
                    schedule: started(0.0),
                },
                vec![],
            ),
            (3, NodeKind::Silent, vec![2, 4]),
            (4, NodeKind::Gain { gain: 1.0 }, vec![3]),
        ]);
        let mut renderer = Renderer::new(8.0, 1, 0.0);
        assert_eq!(renderer.render_planar(&graph, 2)[0], vec![0.0, 0.0]);
    }

    fn media_stream(samples: &[f32], capture_rate: u32, audible: bool) -> NodeKind {
        let queue = crate::audio_input::PcmQueue::new(1, capture_rate);
        queue.push_interleaved(samples);
        let audibility = Arc::new(crate::audio_input::Audibility::new());
        audibility.set_enabled(audible);
        NodeKind::MediaStream {
            queue,
            audibility,
            channels: 1,
            sample_rate: capture_rate,
        }
    }

    #[test]
    fn media_stream_copies_matching_rate_and_silence_when_muted() {
        let connected = graph(vec![
            (1, NodeKind::Destination, vec![3]),
            (
                2,
                media_stream(&[0.25, -0.5, 0.125, -0.125], 8_000, true),
                vec![],
            ),
            (3, NodeKind::Gain { gain: 0.5 }, vec![2]),
        ]);
        let mut renderer = Renderer::new(8_000.0, 2, 0.0);
        let out = renderer.render_planar(&connected, 4);
        assert_eq!(out[0], vec![0.125, -0.25, 0.0625, -0.0625]);
        assert_eq!(out[0], out[1]);

        let muted = graph(vec![
            (1, NodeKind::Destination, vec![2]),
            (2, media_stream(&[1.0, 1.0, 1.0, 1.0], 8_000, false), vec![]),
        ]);
        let mut renderer = Renderer::new(8_000.0, 1, 0.0);
        assert_eq!(renderer.render_planar(&muted, 4)[0], vec![0.0; 4]);
        assert_eq!(renderer.render_planar(&muted, 4)[0], vec![0.0; 4]);
    }

    #[test]
    fn media_stream_resamples_linearly_between_capture_and_context_rates() {
        let up = graph(vec![
            (1, NodeKind::Destination, vec![2]),
            (2, media_stream(&[0.0, 1.0, 0.0, -1.0], 8_000, true), vec![]),
        ]);
        let mut renderer = Renderer::new(16_000.0, 1, 0.0);
        let out = renderer.render_planar(&up, 8);
        let expected = [0.0, 0.5, 1.0, 0.5, 0.0, -0.5, -1.0, 0.0];
        for (index, (sample, expected)) in out[0].iter().zip(expected).enumerate() {
            assert!(
                (sample - expected).abs() < 1e-5,
                "upsample frame {index}: {sample} != {expected}"
            );
        }

        let down = graph(vec![
            (1, NodeKind::Destination, vec![2]),
            (2, media_stream(&[1.0, 2.0, 3.0, 4.0], 8_000, true), vec![]),
        ]);
        let mut renderer = Renderer::new(4_000.0, 1, 0.0);
        let out = renderer.render_planar(&down, 2);
        assert!((out[0][0] - 1.0).abs() < 1e-5);
        assert!((out[0][1] - 3.0).abs() < 1e-5);
    }
}
