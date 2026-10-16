//! IME Hub.
//!
//! Message types and the `ImeContext` / `InputEngine` traits from
//! `docs/ime/smart-ime-protocol.md`. The bundled pinyin engine is pure Rust
//! (`dict/pinyin.dict`); librime is not linked. L2 stays off unless
//! `W3COS_L2` is `1`, `true`, or `script`.

mod commands;
mod context;
mod engine;
mod hub;
mod intent;
mod lexicon;
mod pinyin;

pub use commands::command_utterance;
pub use context::{
    BufferContext, CommitId, Edit, ImeContext, Rect, SURROUNDING_CAP, Surrounding, TextRange,
};
pub use engine::{EngineManifest, Feedback, InputEngine};
pub use hub::{ImeHub, l2_from_value};
pub use intent::{
    InputIntent, IntentHint, IntentPayload, Modality, Proposal, Purpose, Scope, Session,
};
pub use lexicon::{
    Entry, EntryKind, Lexicon, derive_lexicon_key, learn, open_lexicon, rank_candidates,
    seal_lexicon, seal_with_key,
};
pub use pinyin::{
    PinyinEngine, candidates as pinyin_candidates, preedit_display, segment as pinyin_segment,
};
