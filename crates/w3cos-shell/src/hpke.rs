//! Sealed handoff bytes. The implementation lives with the transport so a
//! relay and a direct QUIC path use the same envelope.

pub use w3cos_p2p::hpke::{open, seal};
