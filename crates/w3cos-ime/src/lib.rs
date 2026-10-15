//! IME Hub scaffold.
//!
//! Message types and the `ImeContext` / `InputEngine` traits from
//! `docs/ime/smart-ime-protocol.md`. There is no pinyin, speech, or handwriting
//! engine here. L2 stays off unless `W3COS_L2` is `1`, `true`, or `script`.

mod context;
mod engine;
mod hub;
mod intent;

pub use context::{
    BufferContext, CommitId, Edit, ImeContext, Rect, SURROUNDING_CAP, Surrounding, TextRange,
};
pub use engine::{EngineManifest, Feedback, InputEngine};
pub use hub::{ImeHub, l2_from_value};
pub use intent::{
    InputIntent, IntentHint, IntentPayload, Modality, Proposal, Purpose, Scope, Session,
};
