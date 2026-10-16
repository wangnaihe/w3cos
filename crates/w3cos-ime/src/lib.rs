//! IME Hub.
//!
//! Message types and the `ImeContext` / `InputEngine` traits from
//! `docs/ime/smart-ime-protocol.md`. The bundled pinyin engine is pure Rust
//! (`dict/pinyin.dict`); librime is not linked. L2 stays off unless
//! `W3COS_L2` is `1`, `true`, or `script`.

mod commands;
mod context;
mod engine;
mod handwriting;
mod handwriting_engine;
mod handwriting_model;
mod hub;
mod intent;
mod lexicon;
mod modality;
mod pinyin;
mod speech;
mod speech_engine;
mod speech_model;
mod speech_wav;

pub use commands::command_utterance;
pub use context::{
    BufferContext, CommitId, Edit, ImeContext, Rect, SURROUNDING_CAP, Surrounding, TextRange,
};
pub use engine::{EngineManifest, Feedback, InputEngine};
pub use handwriting::{
    HandwritingManifest, INK_LIMIT, InkCandidate, InkOff, InkPoint, InkResult, InkStroke,
    LocalHandwriting, NoLocalHandwriting, ink_state, recognize_ink,
};
pub use handwriting_engine::PaddleHandwriting;
pub use handwriting_model::{
    DICT_FILE, DICT_LEN, DICT_SHA256, DICT_URL, INK_MODEL_FILE, INK_MODEL_LEN, INK_MODEL_SHA256,
    INK_MODEL_URL, InkModelError, default_handwriting_dir, download_handwriting_model,
    handwriting_model_ready,
};
pub use hub::{ImeHub, l2_from_value};
pub use intent::{
    InputIntent, IntentHint, IntentPayload, Modality, Proposal, Purpose, Scope, Session,
};
pub use lexicon::{
    Entry, EntryKind, Lexicon, derive_lexicon_key, learn, open_lexicon, rank_candidates,
    seal_lexicon, seal_with_key,
};
pub use modality::{
    CLIPBOARD_LIMIT, ClipboardDecision, ModalityBlock, ModalityState, ModalitySurface,
    clipboard_decision, modality_state, p3_surface, route_p3,
};
pub use pinyin::{
    PinyinEngine, candidates as pinyin_candidates, preedit_display, segment as pinyin_segment,
};
pub use speech::{
    LocalSpeech, NoLocalSpeech, SpeechChunk, SpeechManifest, SpeechOff, SpeechStart, VOICE_LIMIT,
    flush_speech, poll_speech, press_speech, release_speech, voice_state,
};
pub use speech_engine::{WhisperSpeech, whisper_language};
pub use speech_model::{
    MODEL_FILE, MODEL_LEN, MODEL_SHA256, MODEL_URL, ModelError, default_speech_dir, download_from,
    download_speech_model, install_checked, model_ready, sha256_hex,
};
