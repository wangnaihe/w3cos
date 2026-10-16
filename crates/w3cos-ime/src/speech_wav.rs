//! 16 kHz PCM helpers.
//!
//! The recognizer only consumes mono `f32` at 16 kHz. WAV fixtures and a
//! microphone callback both land here. Nothing in this module opens a socket.

/// Whisper's sample rate.
pub const SAMPLE_RATE: u32 = 16_000;

#[derive(Clone, Debug, PartialEq)]
pub struct PcmWav {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}

/// Parse a PCM WAV into mono `f32` samples in `-1.0..1.0`.
///
/// 16-bit little-endian PCM only. Stereo is averaged. Other codecs are refused.
pub fn parse_wav(bytes: &[u8]) -> Result<PcmWav, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a wav".into());
    }
    let mut offset = 12usize;
    let mut format = None;
    let mut data = None;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let start = offset + 8;
        let end = start.saturating_add(size).min(bytes.len());
        if id == b"fmt " && end - start >= 16 {
            format = Some(Fmt {
                audio_format: u16::from_le_bytes(bytes[start..start + 2].try_into().unwrap()),
                channels: u16::from_le_bytes(bytes[start + 2..start + 4].try_into().unwrap()),
                sample_rate: u32::from_le_bytes(bytes[start + 4..start + 8].try_into().unwrap()),
                bits: u16::from_le_bytes(bytes[start + 14..start + 16].try_into().unwrap()),
            });
        } else if id == b"data" {
            data = Some(&bytes[start..end]);
        }
        offset = end + (size % 2);
    }
    let Some(format) = format else {
        return Err("wav is missing fmt".into());
    };
    let Some(data) = data else {
        return Err("wav is missing data".into());
    };
    if format.audio_format != 1 || format.bits != 16 || format.channels == 0 {
        return Err("wav must be 16-bit PCM".into());
    }
    if format.sample_rate == 0 {
        return Err("wav sample rate is zero".into());
    }
    let channels = format.channels as usize;
    let frame_bytes = channels.saturating_mul(2);
    if frame_bytes == 0 || data.len() < frame_bytes {
        return Err("wav has no frames".into());
    }
    let frames = data.len() / frame_bytes;
    let mut mono = Vec::with_capacity(frames);
    for frame in 0..frames {
        let mut sum = 0.0f32;
        for channel in 0..channels {
            let at = frame * frame_bytes + channel * 2;
            let sample = i16::from_le_bytes([data[at], data[at + 1]]);
            sum += f32::from(sample) / 32768.0;
        }
        mono.push(sum / channels as f32);
    }
    Ok(PcmWav {
        sample_rate: format.sample_rate,
        samples: mono,
    })
}

/// Average channels (already mono) and linearly resample to 16 kHz.
pub fn downmix_resample(interleaved: &[f32], channels: usize, from_rate: u32) -> Vec<f32> {
    let channels = channels.max(1);
    if from_rate == 0 || interleaved.is_empty() {
        return Vec::new();
    }
    let frames = interleaved.len() / channels;
    let mut mono = Vec::with_capacity(frames);
    for frame in 0..frames {
        let mut sum = 0.0f32;
        for channel in 0..channels {
            sum += interleaved[frame * channels + channel];
        }
        mono.push(sum / channels as f32);
    }
    if from_rate == SAMPLE_RATE {
        return mono;
    }
    if mono.is_empty() {
        return mono;
    }
    let ratio = f64::from(SAMPLE_RATE) / f64::from(from_rate);
    let out_len = ((mono.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    let last = mono.len() - 1;
    for index in 0..out_len {
        let src = index as f64 / ratio;
        let left = (src.floor() as usize).min(last);
        let right = (left + 1).min(last);
        let mix = (src - left as f64) as f32;
        out.push(mono[left] * (1.0 - mix) + mono[right] * mix);
    }
    out
}

struct Fmt {
    audio_format: u16,
    channels: u16,
    sample_rate: u32,
    bits: u16,
}

#[cfg(test)]
pub fn fixture_pcm() -> Vec<f32> {
    parse_wav(include_bytes!("../fixtures/jfk-excerpt.wav"))
        .expect("fixture")
        .samples
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../fixtures/jfk-excerpt.wav");

    #[test]
    fn bundled_excerpt_is_five_seconds_of_16k_speech() {
        let wav = parse_wav(FIXTURE).expect("fixture");
        assert_eq!(wav.sample_rate, SAMPLE_RATE);
        assert_eq!(wav.samples.len(), SAMPLE_RATE as usize * 5);
        let peak = wav
            .samples
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak > 0.05, "fixture looks silent: peak {peak}");
    }

    #[test]
    fn stereo_downmix_averages_channels() {
        let interleaved = vec![1.0, -1.0, 0.5, -0.5];
        let mono = downmix_resample(&interleaved, 2, SAMPLE_RATE);
        assert_eq!(mono.len(), 2);
        assert!(mono[0].abs() < 1e-5);
        assert!(mono[1].abs() < 1e-5);
    }
}
