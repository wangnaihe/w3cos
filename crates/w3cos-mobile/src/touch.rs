//! Touch input → shared jsdom PointerEvent / TouchEvent dispatch.
//!
//! Layout hit-testing uses the same CSSOM boxes as `document.elementFromPoint`.
//! Android MotionEvent and iOS UITouch surface adapters are not wired here.

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
    /// Hit-test the live document and dispatch paired PointerEvent/TouchEvent
    /// lifecycles for each contact.
    ///
    /// Returns whether any dispatched event called `preventDefault()`. A `Start`
    /// that misses every layout box is ignored. Active contacts keep their
    /// target through later move/end/cancel even if the point leaves the box.
    pub fn dispatch(&self) -> bool {
        let phase = match self.phase {
            TouchPhase::Start => "down",
            TouchPhase::Move => "move",
            TouchPhase::End => "up",
            TouchPhase::Cancel => "cancel",
        };
        let pressure = match self.phase {
            TouchPhase::Start | TouchPhase::Move => 0.5,
            TouchPhase::End | TouchPhase::Cancel => 0.0,
        };
        let mut prevented = false;
        for point in &self.points {
            if w3cos_runtime::jsdom::dispatch_hit_tested_touch(
                phase,
                point.x,
                point.y,
                i64::from(point.id),
                pressure,
            ) {
                prevented = true;
            }
        }
        prevented
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use w3cos_core::Value;
    use w3cos_runtime::dom;
    use w3cos_runtime::jsdom::{document_value, reset_bridge};

    fn setup() {
        dom::reset_document();
        reset_bridge();
    }

    fn create_in_body(tag: &str) -> Value {
        let doc = document_value();
        let el = doc.call_method("createElement", vec![Value::string(tag)]);
        doc.get_property("body")
            .call_method("appendChild", vec![el.clone()]);
        el
    }

    fn set_layout_box(element: &Value, left: f32, top: f32, width: f32, height: f32) {
        let style = element.get_property("style");
        for (property, value) in [
            ("position", "absolute".to_string()),
            ("left", format!("{left}px")),
            ("top", format!("{top}px")),
            ("width", format!("{width}px")),
            ("height", format!("{height}px")),
        ] {
            style.set_property(property, Value::string(&value));
        }
    }

    #[test]
    fn dispatch_hit_tests_layout_and_delivers_touch_lifecycle() {
        setup();
        let target = create_in_body("button");
        set_layout_box(&target, 8.0, 16.0, 32.0, 24.0);
        let log = Rc::new(RefCell::new(Vec::<String>::new()));
        for event_type in ["pointerdown", "touchstart", "pointerup", "touchend"] {
            let log = Rc::clone(&log);
            target.call_method(
                "addEventListener",
                vec![
                    Value::string(event_type),
                    Value::function(move |_, args| {
                        log.borrow_mut()
                            .push(args[0].get_property("type").to_js_string());
                        Value::Undefined
                    }),
                ],
            );
        }

        let miss = TouchEvent {
            phase: TouchPhase::Start,
            points: vec![TouchPoint {
                id: 9,
                x: 0.0,
                y: 0.0,
            }],
            timestamp_ms: 1,
        };
        assert!(!miss.dispatch());
        assert!(log.borrow().is_empty());

        let start = TouchEvent {
            phase: TouchPhase::Start,
            points: vec![TouchPoint {
                id: 9,
                x: 12.0,
                y: 20.0,
            }],
            timestamp_ms: 2,
        };
        assert!(!start.dispatch());
        let end = TouchEvent {
            phase: TouchPhase::End,
            points: vec![TouchPoint {
                id: 9,
                x: 0.0,
                y: 0.0,
            }],
            timestamp_ms: 3,
        };
        assert!(!end.dispatch());
        assert_eq!(
            log.borrow().as_slice(),
            &["pointerdown", "touchstart", "pointerup", "touchend"]
        );
    }

    #[test]
    fn prevent_default_on_touchmove_is_reported() {
        setup();
        let target = create_in_body("div");
        set_layout_box(&target, 0.0, 0.0, 50.0, 50.0);
        target.call_method(
            "addEventListener",
            vec![
                Value::string("touchmove"),
                Value::function(|_, args| {
                    args[0].call_method("preventDefault", vec![]);
                    Value::Undefined
                }),
            ],
        );
        assert!(
            !TouchEvent {
                phase: TouchPhase::Start,
                points: vec![TouchPoint {
                    id: 3,
                    x: 10.0,
                    y: 10.0,
                }],
                timestamp_ms: 1,
            }
            .dispatch()
        );
        assert!(
            TouchEvent {
                phase: TouchPhase::Move,
                points: vec![TouchPoint {
                    id: 3,
                    x: 12.0,
                    y: 14.0,
                }],
                timestamp_ms: 2,
            }
            .dispatch()
        );
    }
}
