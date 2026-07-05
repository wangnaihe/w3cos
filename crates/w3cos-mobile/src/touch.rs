//! Touch input → W3C DOM pointer events (M1 stub).
//!
//! Full implementation will map Android MotionEvent / iOS UITouch to
//! `w3cos_dom::events` and feed the runtime hit-test pipeline.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct TouchPoint {
    pub id: u32,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TouchPhase {
    Start,
    Move,
    End,
    Cancel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TouchEvent {
    pub phase: TouchPhase,
    pub points: Vec<TouchPoint>,
    pub timestamp_ms: u64,
}

impl TouchEvent {
    /// Placeholder — wired to DOM dispatch in M1 follow-up.
    pub fn dispatch(&self) {
        let _ = self;
    }
}
