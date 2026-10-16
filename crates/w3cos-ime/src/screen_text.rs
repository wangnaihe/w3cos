//! Local screen text.
//!
//! A [`LocalScreenText`] engine reads one region the caller already captured.
//! It does not capture the screen and it does not open a socket.
//! [`NoLocalScreenText`] has no model. [`crate::PaddleScreenText`] stays
//! unavailable until the pinned detector and the handwriting recognizer are
//! both present. [`recognize_screen`] does not call `recognize` when the
//! model is missing, the manifest asks for the network, the field is secure,
//! the session has ended, the region is empty, or this capture was not
//! confirmed.

use crate::{ModalityBlock, ModalityState, Session};

/// How many Unicode scalars one screen snippet may carry.
pub const SCREEN_LIMIT: usize = 128;

/// Straight RGBA the caller captured. The engine does not take this itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One explicit region, in [`ScreenImage`] pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Why this capture produced no text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenOff {
    /// No on-device recognizer. `recognize` is not called.
    NoLocalModel,
    /// The engine asked to upload the image. `recognize` is not called.
    NetworkRefused,
    SecureField,
    NoSession,
    /// The region is empty. `recognize` is not called.
    Empty,
    /// This capture was not confirmed. `recognize` is not called.
    Unconfirmed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenTextManifest {
    pub id: String,
    pub locales: Vec<String>,
    /// `true` means the engine wants a network. The hub refuses it.
    pub network: bool,
    /// `false` is the honest off state. The hub does not call `recognize`.
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScreenSnippet {
    pub text: String,
    pub score: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScreenResult {
    Snippets(Vec<ScreenSnippet>),
    Off(ScreenOff),
}

/// On-device text reader. Implementations must not open a socket or capture.
pub trait LocalScreenText: Send {
    fn manifest(&self) -> ScreenTextManifest;
    /// Read text in `region` of an image the caller already holds.
    fn recognize(&mut self, image: &ScreenImage, region: ScreenRegion) -> Vec<ScreenSnippet>;
}

/// Bundled engine: no model, no recognition.
#[derive(Clone, Debug, Default)]
pub struct NoLocalScreenText {
    pub calls: u32,
}

impl LocalScreenText for NoLocalScreenText {
    fn manifest(&self) -> ScreenTextManifest {
        ScreenTextManifest {
            id: "builtin.no-local-screen-text".into(),
            locales: Vec::new(),
            network: false,
            available: false,
        }
    }

    fn recognize(&mut self, _image: &ScreenImage, _region: ScreenRegion) -> Vec<ScreenSnippet> {
        self.calls = self.calls.saturating_add(1);
        Vec::new()
    }
}

/// Whether the screen chip can ask. A network manifest stays blocked.
/// Ready still does not capture: the person confirms each region.
pub fn screen_state(session: &Session, manifest: &ScreenTextManifest) -> ModalityState {
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

fn region_holds_pixels(image: &ScreenImage, region: ScreenRegion) -> bool {
    if image.width == 0 || image.height == 0 || region.width < 8 || region.height < 8 {
        return false;
    }
    let expected = (image.width as usize)
        .saturating_mul(image.height as usize)
        .saturating_mul(4);
    if image.rgba.len() != expected {
        return false;
    }
    let x1 = region.x.saturating_add(region.width);
    let y1 = region.y.saturating_add(region.height);
    region.x < image.width && region.y < image.height && x1 <= image.width && y1 <= image.height
}

/// Ask a local engine. Refused captures are not asked to `recognize`.
pub fn recognize_screen(
    engine: &mut dyn LocalScreenText,
    session: &Session,
    image: &ScreenImage,
    region: ScreenRegion,
    permitted: bool,
) -> ScreenResult {
    if session.id.is_empty() {
        return ScreenResult::Off(ScreenOff::NoSession);
    }
    if session.purpose.is_secure() {
        return ScreenResult::Off(ScreenOff::SecureField);
    }
    if !region_holds_pixels(image, region) {
        return ScreenResult::Off(ScreenOff::Empty);
    }
    if !permitted {
        return ScreenResult::Off(ScreenOff::Unconfirmed);
    }
    let manifest = engine.manifest();
    if manifest.network {
        return ScreenResult::Off(ScreenOff::NetworkRefused);
    }
    if !manifest.available {
        return ScreenResult::Off(ScreenOff::NoLocalModel);
    }
    let mut kept = Vec::new();
    for snippet in engine.recognize(image, region) {
        let text = snippet.text.trim();
        if text.is_empty() || text.chars().count() > SCREEN_LIMIT {
            continue;
        }
        if kept.iter().any(|item: &ScreenSnippet| item.text == text) {
            continue;
        }
        kept.push(ScreenSnippet {
            text: text.to_string(),
            score: snippet.score,
        });
        if kept.len() == 8 {
            break;
        }
    }
    ScreenResult::Snippets(kept)
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

    impl LocalScreenText for Probe {
        fn manifest(&self) -> ScreenTextManifest {
            ScreenTextManifest {
                id: "test.local-screen-text".into(),
                locales: vec!["zh-Hans".into(), "en".into()],
                network: self.network,
                available: self.available,
            }
        }

        fn recognize(&mut self, _image: &ScreenImage, _region: ScreenRegion) -> Vec<ScreenSnippet> {
            self.calls = self.calls.saturating_add(1);
            self.out
                .iter()
                .map(|text| ScreenSnippet {
                    text: text.clone(),
                    score: 1.0,
                })
                .collect()
        }
    }

    fn image() -> ScreenImage {
        ScreenImage {
            width: 32,
            height: 16,
            rgba: vec![255; 32 * 16 * 4],
        }
    }

    fn region() -> ScreenRegion {
        ScreenRegion {
            x: 0,
            y: 0,
            width: 32,
            height: 16,
        }
    }

    fn probe(available: bool, network: bool) -> Probe {
        Probe {
            network,
            available,
            calls: 0,
            out: vec![
                "周五".into(),
                "  ".into(),
                "字".repeat(SCREEN_LIMIT + 1),
                "周五".into(),
            ],
        }
    }

    #[test]
    fn no_model_network_and_unconfirmed_never_recognize() {
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("s", Purpose::Text, vec!["zh-Hans".into()]);
        let mut bundled = NoLocalScreenText::default();
        assert_eq!(
            screen_state(&session, &bundled.manifest()),
            ModalityState::Blocked(ModalityBlock::NoLocalModel)
        );
        assert!(matches!(
            recognize_screen(&mut bundled, &session, &image(), region(), true),
            ScreenResult::Off(ScreenOff::NoLocalModel)
        ));
        assert_eq!(bundled.calls, 0);

        let mut cloud = probe(true, true);
        assert!(matches!(
            recognize_screen(&mut cloud, &session, &image(), region(), true),
            ScreenResult::Off(ScreenOff::NetworkRefused)
        ));
        assert_eq!(cloud.calls, 0);

        let mut local = probe(true, false);
        assert!(matches!(
            recognize_screen(&mut local, &session, &image(), region(), false),
            ScreenResult::Off(ScreenOff::Unconfirmed)
        ));
        assert_eq!(local.calls, 0);
    }

    #[test]
    fn secure_ended_and_empty_regions_do_not_recognize() {
        let hub = ImeHub::with_l2(false);
        let secure = hub.begin_session("lock", Purpose::Password, vec!["en".into()]);
        let mut engine = probe(true, false);
        assert!(matches!(
            recognize_screen(&mut engine, &secure, &image(), region(), true),
            ScreenResult::Off(ScreenOff::SecureField)
        ));
        let mut ended = hub.begin_session("s", Purpose::Chat, vec!["zh-Hans".into()]);
        ended.end();
        assert!(matches!(
            recognize_screen(&mut engine, &ended, &image(), region(), true),
            ScreenResult::Off(ScreenOff::NoSession)
        ));
        let session = hub.begin_session("s", Purpose::Text, vec!["zh-Hans".into()]);
        let tiny = ScreenRegion {
            x: 0,
            y: 0,
            width: 4,
            height: 16,
        };
        assert!(matches!(
            recognize_screen(&mut engine, &session, &image(), tiny, true),
            ScreenResult::Off(ScreenOff::Empty)
        ));
        assert_eq!(engine.calls, 0);
    }

    #[test]
    fn a_confirmed_local_engine_returns_snippets_and_drops_junk() {
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("s", Purpose::Chat, vec!["zh-Hans".into(), "en".into()]);
        let mut engine = probe(true, false);
        engine.out.insert(1, "  评审  ".into());
        assert_eq!(
            screen_state(&session, &engine.manifest()),
            ModalityState::Ready
        );
        match recognize_screen(&mut engine, &session, &image(), region(), true) {
            ScreenResult::Snippets(items) => {
                assert_eq!(
                    items
                        .iter()
                        .map(|item| item.text.as_str())
                        .collect::<Vec<_>>(),
                    vec!["周五", "评审"]
                );
            }
            other => panic!("expected snippets, got {other:?}"),
        }
        assert_eq!(engine.calls, 1);
    }
}
