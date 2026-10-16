//! Local handwriting.
//!
//! A [`LocalHandwriting`] engine turns pen strokes into candidates on this
//! device. [`NoLocalHandwriting`] has no model. [`crate::PaddleHandwriting`]
//! stays unavailable until the pinned files are present. [`recognize_ink`]
//! does not call `recognize` when the model is missing, the manifest asks for
//! the network, the field is secure, or the session has ended. Strokes are
//! not stored or uploaded.

use crate::{ModalityBlock, ModalityState, Session};

/// How many Unicode scalars one handwriting candidate may carry.
pub const INK_LIMIT: usize = 32;

/// One sample on the pad, in the pad's own pixel space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InkPoint {
    pub x: f32,
    pub y: f32,
}

/// One pen-down … pen-up trace.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct InkStroke {
    pub points: Vec<InkPoint>,
}

/// Why the pad stays closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InkOff {
    /// No on-device recognizer. `recognize` is not called.
    NoLocalModel,
    /// The engine asked to upload strokes. `recognize` is not called.
    NetworkRefused,
    SecureField,
    NoSession,
    /// The pad has no ink. `recognize` is not called.
    Empty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandwritingManifest {
    pub id: String,
    pub locales: Vec<String>,
    /// `true` means the engine wants a network. The hub refuses it.
    pub network: bool,
    /// `false` is the honest off state. The hub does not call `recognize`.
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InkCandidate {
    pub text: String,
    pub score: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InkResult {
    Candidates(Vec<InkCandidate>),
    Off(InkOff),
}

/// On-device stroke recognizer. Implementations must not open a socket.
pub trait LocalHandwriting: Send {
    fn manifest(&self) -> HandwritingManifest;
    /// Rank candidates for the strokes already on the pad.
    fn recognize(&mut self, width: u32, height: u32, strokes: &[InkStroke]) -> Vec<InkCandidate>;
}

/// Bundled engine: no model, no recognition.
#[derive(Clone, Debug, Default)]
pub struct NoLocalHandwriting {
    pub calls: u32,
}

impl LocalHandwriting for NoLocalHandwriting {
    fn manifest(&self) -> HandwritingManifest {
        HandwritingManifest {
            id: "builtin.no-local-handwriting".into(),
            locales: Vec::new(),
            network: false,
            available: false,
        }
    }

    fn recognize(
        &mut self,
        _width: u32,
        _height: u32,
        _strokes: &[InkStroke],
    ) -> Vec<InkCandidate> {
        self.calls = self.calls.saturating_add(1);
        Vec::new()
    }
}

/// Whether the ink chip can open. A network manifest stays blocked.
pub fn ink_state(session: &Session, manifest: &HandwritingManifest) -> ModalityState {
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

fn strokes_have_ink(strokes: &[InkStroke]) -> bool {
    strokes.iter().any(|stroke| !stroke.points.is_empty())
}

/// Ask a local engine. Refused engines are not asked to `recognize`.
pub fn recognize_ink(
    engine: &mut dyn LocalHandwriting,
    session: &Session,
    width: u32,
    height: u32,
    strokes: &[InkStroke],
) -> InkResult {
    if session.id.is_empty() {
        return InkResult::Off(InkOff::NoSession);
    }
    if session.purpose.is_secure() {
        return InkResult::Off(InkOff::SecureField);
    }
    if width == 0 || height == 0 || !strokes_have_ink(strokes) {
        return InkResult::Off(InkOff::Empty);
    }
    let manifest = engine.manifest();
    if manifest.network {
        return InkResult::Off(InkOff::NetworkRefused);
    }
    if !manifest.available {
        return InkResult::Off(InkOff::NoLocalModel);
    }
    let mut kept = Vec::new();
    for candidate in engine.recognize(width, height, strokes) {
        let text = candidate.text.trim();
        if text.is_empty() || text.chars().count() > INK_LIMIT {
            continue;
        }
        if kept.iter().any(|item: &InkCandidate| item.text == text) {
            continue;
        }
        kept.push(InkCandidate {
            text: text.to_string(),
            score: candidate.score,
        });
        if kept.len() == 8 {
            break;
        }
    }
    InkResult::Candidates(kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ImeHub, Purpose};

    struct Probe {
        network: bool,
        available: bool,
        calls: u32,
        out: Vec<String>,
    }

    impl LocalHandwriting for Probe {
        fn manifest(&self) -> HandwritingManifest {
            HandwritingManifest {
                id: "test.local-handwriting".into(),
                locales: vec!["zh-Hans".into(), "en".into()],
                network: self.network,
                available: self.available,
            }
        }

        fn recognize(
            &mut self,
            _width: u32,
            _height: u32,
            _strokes: &[InkStroke],
        ) -> Vec<InkCandidate> {
            self.calls = self.calls.saturating_add(1);
            self.out
                .iter()
                .map(|text| InkCandidate {
                    text: text.clone(),
                    score: 1.0,
                })
                .collect()
        }
    }

    fn stroke() -> InkStroke {
        InkStroke {
            points: vec![InkPoint { x: 4.0, y: 8.0 }, InkPoint { x: 40.0, y: 8.0 }],
        }
    }

    fn probe(available: bool, network: bool) -> Probe {
        Probe {
            network,
            available,
            calls: 0,
            out: vec![
                "永".into(),
                "  ".into(),
                "字".repeat(INK_LIMIT + 1),
                "永".into(),
            ],
        }
    }

    #[test]
    fn no_model_and_network_never_recognize() {
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("s", Purpose::Text, vec!["zh-Hans".into()]);
        let mut bundled = NoLocalHandwriting::default();
        assert_eq!(
            ink_state(&session, &bundled.manifest()),
            ModalityState::Blocked(ModalityBlock::NoLocalModel)
        );
        assert!(matches!(
            recognize_ink(&mut bundled, &session, 100, 40, &[stroke()]),
            InkResult::Off(InkOff::NoLocalModel)
        ));
        assert_eq!(bundled.calls, 0);

        let mut cloud = probe(true, true);
        assert!(matches!(
            recognize_ink(&mut cloud, &session, 100, 40, &[stroke()]),
            InkResult::Off(InkOff::NetworkRefused)
        ));
        assert_eq!(cloud.calls, 0);
    }

    #[test]
    fn secure_ended_and_empty_pads_do_not_recognize() {
        let hub = ImeHub::with_l2(false);
        let secure = hub.begin_session("lock", Purpose::Password, vec!["en".into()]);
        let mut engine = probe(true, false);
        assert!(matches!(
            recognize_ink(&mut engine, &secure, 100, 40, &[stroke()]),
            InkResult::Off(InkOff::SecureField)
        ));
        let mut ended = hub.begin_session("s", Purpose::Chat, vec!["zh-Hans".into()]);
        ended.end();
        assert!(matches!(
            recognize_ink(&mut engine, &ended, 100, 40, &[stroke()]),
            InkResult::Off(InkOff::NoSession)
        ));
        let session = hub.begin_session("s", Purpose::Text, vec!["zh-Hans".into()]);
        assert!(matches!(
            recognize_ink(&mut engine, &session, 100, 40, &[]),
            InkResult::Off(InkOff::Empty)
        ));
        assert_eq!(engine.calls, 0);
    }

    #[test]
    fn a_local_engine_returns_candidates_and_drops_junk() {
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("s", Purpose::Chat, vec!["zh-Hans".into(), "en".into()]);
        let mut engine = probe(true, false);
        engine.out.insert(1, "  水  ".into());
        assert_eq!(
            ink_state(&session, &engine.manifest()),
            ModalityState::Ready
        );
        match recognize_ink(&mut engine, &session, 120, 60, &[stroke()]) {
            InkResult::Candidates(items) => {
                assert_eq!(
                    items
                        .iter()
                        .map(|item| item.text.as_str())
                        .collect::<Vec<_>>(),
                    vec!["永", "水"]
                );
            }
            other => panic!("expected candidates, got {other:?}"),
        }
        assert_eq!(engine.calls, 1);
    }
}
