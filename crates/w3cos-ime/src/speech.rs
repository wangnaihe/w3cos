//! Local streaming speech.
//!
//! A [`LocalSpeech`] engine turns a microphone hold into text on this device.
//! [`NoLocalSpeech`] has no model. [`crate::WhisperSpeech`] is the on-device
//! whisper.cpp engine; it stays unavailable until a verified model file is
//! present. [`press_speech`] does not call `begin` when the model is missing,
//! the manifest asks for the network, the field is secure, or the session has
//! ended. Audio bytes are never stored or uploaded.

use crate::{ModalityBlock, ModalityState, Session};

/// How many Unicode scalars one transcript may carry.
pub const VOICE_LIMIT: usize = 1024;

/// Why the microphone stays closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeechOff {
    /// No on-device recognizer. `begin` is not called.
    NoLocalModel,
    /// The model is present, but no microphone or fixture opened.
    NoMicrophone,
    /// The engine asked to upload audio. `begin` is not called.
    NetworkRefused,
    SecureField,
    NoSession,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeechManifest {
    pub id: String,
    pub locales: Vec<String>,
    /// `true` means the engine wants a network. The hub refuses it.
    pub network: bool,
    /// `false` is the honest off state. The hub does not call `begin`.
    pub available: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeechStart {
    Listening,
    Off(SpeechOff),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeechChunk {
    pub text: String,
    /// `true` while the person is still holding the microphone.
    pub interim: bool,
}

/// On-device streaming recognizer. Implementations must not open a socket.
pub trait LocalSpeech: Send {
    fn manifest(&self) -> SpeechManifest;
    /// Open the microphone. Called only for a local, available manifest.
    fn begin(&mut self, locales: &[String]) -> SpeechStart;
    fn poll(&mut self) -> Option<SpeechChunk>;
    /// One last decode before the hold ends. The default is another [`poll`](Self::poll).
    fn flush(&mut self) -> Option<SpeechChunk> {
        self.poll()
    }
    /// Close the microphone. Safe to call when it is already closed.
    fn end(&mut self);
    /// True only while this engine is capturing audio.
    fn capturing(&self) -> bool;
}

/// Bundled engine: no model, no capture.
#[derive(Clone, Debug, Default)]
pub struct NoLocalSpeech {
    pub begins: u32,
    pub ends: u32,
}

impl LocalSpeech for NoLocalSpeech {
    fn manifest(&self) -> SpeechManifest {
        SpeechManifest {
            id: "builtin.no-local-speech".into(),
            locales: Vec::new(),
            network: false,
            available: false,
        }
    }

    fn begin(&mut self, _locales: &[String]) -> SpeechStart {
        self.begins = self.begins.saturating_add(1);
        SpeechStart::Off(SpeechOff::NoLocalModel)
    }

    fn poll(&mut self) -> Option<SpeechChunk> {
        None
    }

    fn end(&mut self) {
        self.ends = self.ends.saturating_add(1);
    }

    fn capturing(&self) -> bool {
        false
    }
}

/// Whether the mic chip can arm. A network manifest stays blocked.
pub fn voice_state(session: &Session, manifest: &SpeechManifest) -> ModalityState {
    if session.id.is_empty() {
        return ModalityState::Blocked(ModalityBlock::NoSession);
    }
    if session.purpose.is_secure() {
        return ModalityState::Blocked(ModalityBlock::SecureField);
    }
    if manifest.network || !manifest.available {
        return ModalityState::Blocked(ModalityBlock::NoLocalModel);
    }
    ModalityState::Ready
}

/// Start a hold. Refused engines are not asked to `begin`.
pub fn press_speech(engine: &mut dyn LocalSpeech, session: &Session) -> SpeechStart {
    if session.id.is_empty() {
        return SpeechStart::Off(SpeechOff::NoSession);
    }
    if session.purpose.is_secure() {
        return SpeechStart::Off(SpeechOff::SecureField);
    }
    let manifest = engine.manifest();
    if manifest.network {
        return SpeechStart::Off(SpeechOff::NetworkRefused);
    }
    if !manifest.available {
        return SpeechStart::Off(SpeechOff::NoLocalModel);
    }
    match engine.begin(&session.locales) {
        SpeechStart::Listening if engine.capturing() => SpeechStart::Listening,
        SpeechStart::Off(reason) => {
            engine.end();
            SpeechStart::Off(reason)
        }
        SpeechStart::Listening => {
            engine.end();
            SpeechStart::Off(SpeechOff::NoLocalModel)
        }
    }
}

/// Next transcript while a hold is recording. Empty and over-long chunks drop.
pub fn poll_speech(engine: &mut dyn LocalSpeech, recording: bool) -> Option<SpeechChunk> {
    if !recording || !engine.capturing() {
        return None;
    }
    let chunk = engine.poll()?;
    let text = chunk.text.trim();
    if text.is_empty() || text.chars().count() > VOICE_LIMIT {
        return None;
    }
    Some(SpeechChunk {
        text: text.to_string(),
        interim: chunk.interim,
    })
}

/// Last transcript before release. Empty and over-long chunks drop.
pub fn flush_speech(engine: &mut dyn LocalSpeech, recording: bool) -> Option<SpeechChunk> {
    if !recording || !engine.capturing() {
        return None;
    }
    let chunk = engine.flush()?;
    let text = chunk.text.trim();
    if text.is_empty() || text.chars().count() > VOICE_LIMIT {
        return None;
    }
    Some(SpeechChunk {
        text: text.to_string(),
        interim: chunk.interim,
    })
}

/// Close the microphone after a hold. A refused press does not call `end`.
pub fn release_speech(engine: &mut dyn LocalSpeech, recording: bool) {
    if recording || engine.capturing() {
        engine.end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ImeHub, Purpose};
    use std::collections::VecDeque;

    struct Probe {
        network: bool,
        available: bool,
        capturing: bool,
        begins: u32,
        ends: u32,
        chunks: VecDeque<SpeechChunk>,
        locales: Vec<String>,
        /// `begin` returns off but leaves the mic flag set, to check `end`.
        leak: bool,
        /// `begin` returns this reason and still sets the capture flag.
        fail: Option<SpeechOff>,
    }

    impl LocalSpeech for Probe {
        fn manifest(&self) -> SpeechManifest {
            SpeechManifest {
                id: "test.local-speech".into(),
                locales: vec!["zh-Hans".into()],
                network: self.network,
                available: self.available,
            }
        }

        fn begin(&mut self, locales: &[String]) -> SpeechStart {
            self.begins = self.begins.saturating_add(1);
            self.locales = locales.to_vec();
            if self.leak {
                self.capturing = true;
                return SpeechStart::Off(SpeechOff::NoLocalModel);
            }
            if let Some(reason) = self.fail {
                self.capturing = true;
                return SpeechStart::Off(reason);
            }
            self.capturing = true;
            SpeechStart::Listening
        }

        fn poll(&mut self) -> Option<SpeechChunk> {
            if !self.capturing {
                return None;
            }
            self.chunks.pop_front()
        }

        fn end(&mut self) {
            self.ends = self.ends.saturating_add(1);
            self.capturing = false;
        }

        fn capturing(&self) -> bool {
            self.capturing
        }
    }

    fn probe(available: bool, network: bool) -> Probe {
        Probe {
            network,
            available,
            capturing: false,
            begins: 0,
            ends: 0,
            chunks: VecDeque::new(),
            locales: Vec::new(),
            leak: false,
            fail: None,
        }
    }

    #[test]
    fn no_model_and_network_never_begin_or_capture() {
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("s", Purpose::Text, vec!["zh-Hans".into()]);
        let mut bundled = NoLocalSpeech::default();
        assert_eq!(
            voice_state(&session, &bundled.manifest()),
            ModalityState::Blocked(ModalityBlock::NoLocalModel)
        );
        assert_eq!(
            press_speech(&mut bundled, &session),
            SpeechStart::Off(SpeechOff::NoLocalModel)
        );
        assert_eq!(bundled.begins, 0);
        assert_eq!(bundled.ends, 0);
        assert!(!bundled.capturing());
        assert!(poll_speech(&mut bundled, false).is_none());
        release_speech(&mut bundled, false);
        assert_eq!(bundled.ends, 0);

        let mut cloud = probe(true, true);
        assert_eq!(
            voice_state(&session, &cloud.manifest()),
            ModalityState::Blocked(ModalityBlock::NoLocalModel)
        );
        assert_eq!(
            press_speech(&mut cloud, &session),
            SpeechStart::Off(SpeechOff::NetworkRefused)
        );
        assert_eq!(cloud.begins, 0);
        assert_eq!(cloud.ends, 0);
        assert!(!cloud.capturing());
    }

    #[test]
    fn secure_and_ended_sessions_do_not_open_the_microphone() {
        let hub = ImeHub::with_l2(false);
        let secure = hub.begin_session("lock", Purpose::Password, vec!["zh-Hans".into()]);
        let mut engine = probe(true, false);
        assert_eq!(
            press_speech(&mut engine, &secure),
            SpeechStart::Off(SpeechOff::SecureField)
        );
        assert_eq!(engine.begins, 0);
        assert!(!engine.capturing());

        let mut ended = hub.begin_session("s", Purpose::Chat, vec!["zh-Hans".into()]);
        ended.end();
        assert_eq!(
            press_speech(&mut engine, &ended),
            SpeechStart::Off(SpeechOff::NoSession)
        );
        assert_eq!(engine.begins, 0);
        assert_eq!(engine.ends, 0);
    }

    #[test]
    fn a_failed_begin_is_closed_even_if_it_set_the_capture_flag() {
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("s", Purpose::Text, vec!["zh-Hans".into()]);
        let mut engine = probe(true, false);
        engine.leak = true;
        assert_eq!(
            press_speech(&mut engine, &session),
            SpeechStart::Off(SpeechOff::NoLocalModel)
        );
        assert_eq!(engine.begins, 1);
        assert!(engine.ends >= 1);
        assert!(!engine.capturing());
        assert!(poll_speech(&mut engine, true).is_none());

        let mut mic = probe(true, false);
        mic.fail = Some(SpeechOff::NoMicrophone);
        assert_eq!(
            press_speech(&mut mic, &session),
            SpeechStart::Off(SpeechOff::NoMicrophone)
        );
        assert!(mic.ends >= 1);
        assert!(!mic.capturing());
    }

    #[test]
    fn a_local_engine_streams_text_and_release_stops_capture() {
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("s", Purpose::Chat, vec!["zh-Hans".into(), "en".into()]);
        let mut engine = probe(true, false);
        engine.chunks.push_back(SpeechChunk {
            text: "  改到  ".into(),
            interim: true,
        });
        engine.chunks.push_back(SpeechChunk {
            text: "   ".into(),
            interim: true,
        });
        engine.chunks.push_back(SpeechChunk {
            text: "字".repeat(VOICE_LIMIT + 1),
            interim: true,
        });
        engine.chunks.push_back(SpeechChunk {
            text: "改到下周一上午".into(),
            interim: false,
        });
        assert_eq!(
            voice_state(&session, &engine.manifest()),
            ModalityState::Ready
        );
        assert_eq!(press_speech(&mut engine, &session), SpeechStart::Listening);
        assert!(engine.capturing());
        assert_eq!(
            engine.locales,
            vec!["zh-Hans".to_string(), "en".to_string()]
        );
        assert_eq!(engine.begins, 1);
        assert!(poll_speech(&mut engine, false).is_none());
        assert_eq!(
            poll_speech(&mut engine, true),
            Some(SpeechChunk {
                text: "改到".into(),
                interim: true,
            })
        );
        assert!(poll_speech(&mut engine, true).is_none());
        assert!(poll_speech(&mut engine, true).is_none());
        assert_eq!(
            poll_speech(&mut engine, true),
            Some(SpeechChunk {
                text: "改到下周一上午".into(),
                interim: false,
            })
        );
        assert!(engine.capturing());
        engine.chunks.push_back(SpeechChunk {
            text: "尾".into(),
            interim: false,
        });
        assert_eq!(
            flush_speech(&mut engine, true),
            Some(SpeechChunk {
                text: "尾".into(),
                interim: false,
            })
        );
        release_speech(&mut engine, true);
        assert_eq!(engine.ends, 1);
        assert!(!engine.capturing());
        assert!(poll_speech(&mut engine, true).is_none());
    }
}
