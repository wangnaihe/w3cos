//! Touch input → W3C DOM pointer events (M1 stub).
//!
//! Full implementation will map Android MotionEvent / iOS UITouch to
//! `w3cos_dom::events` and feed the runtime hit-test pipeline.

use serde::{Deserialize, Serialize};
use std::sync::Once;

static DISPATCH_WARNING: Once = Once::new();

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
    /// Compatibility placeholder for platform shells that still submit this
    /// legacy DTO instead of using the runtime's native pointer adapter.
    pub fn dispatch(&self) {
        DISPATCH_WARNING.call_once(|| {
            eprintln!(
                "W3COS warning: w3cos-mobile TouchEvent::dispatch() is not connected to a DOM \
                 surface; use the runtime native touch adapter for PointerEvent/TouchEvent dispatch"
            );
        });
    }
}
