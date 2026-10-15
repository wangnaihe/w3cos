//! Floating window geometry for the shell compositor.
//!
//! Positions live beside the signal store so the existing signal indices stay
//! stable. A local state file reloads them on the next start.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;

use w3cos_runtime::state;

pub const MIN_W: f32 = 320.0;
pub const MIN_H: f32 = 200.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub z: i32,
    pub maximized: bool,
    pub rx: f32,
    pub ry: f32,
    pub rw: f32,
    pub rh: f32,
}

#[derive(Clone, Copy)]
struct DragOrigin {
    id: i64,
    frame: Frame,
}

thread_local! {
    static FRAMES: RefCell<BTreeMap<i64, Frame>> = const { RefCell::new(BTreeMap::new()) };
    static LOADED: Cell<bool> = const { Cell::new(false) };
    static ORIGIN: Cell<Option<DragOrigin>> = const { Cell::new(None) };
}

pub fn default_frame(id: i64) -> Frame {
    let step = (id - 1).max(0) as f32;
    let x = 24.0 + step * 26.0;
    let y = 16.0 + step * 22.0;
    Frame {
        x,
        y,
        w: 680.0,
        h: 420.0,
        z: id as i32,
        maximized: false,
        rx: x,
        ry: y,
        rw: 680.0,
        rh: 420.0,
    }
}

pub fn frame(id: i64) -> Frame {
    FRAMES.with(|frames| {
        frames
            .borrow()
            .get(&id)
            .copied()
            .unwrap_or_else(|| default_frame(id))
    })
}

pub fn is_maximized(id: i64) -> bool {
    frame(id).maximized
}

fn put(id: i64, frame: Frame) {
    FRAMES.with(|frames| {
        frames.borrow_mut().insert(id, frame);
    });
}

pub fn raise(id: i64) {
    if !(1..=6).contains(&id) {
        return;
    }
    // Compare against other windows only. Including this window made every
    // drag move look tied for the top and climb z without a visual change.
    let top_other = FRAMES.with(|frames| {
        let frames = frames.borrow();
        (1..=6)
            .filter(|other| *other != id)
            .map(|other| {
                frames
                    .get(&other)
                    .map(|frame| frame.z)
                    .unwrap_or(other as i32)
            })
            .max()
            .unwrap_or(0)
    });
    let mut frame = frame(id);
    if frame.z <= top_other {
        frame.z = top_other.saturating_add(1);
        put(id, frame);
        state::mark_dirty();
    }
}

pub fn toggle_maximize(id: i64, stage_w: f32, stage_h: f32) -> bool {
    if !(1..=6).contains(&id) {
        return false;
    }
    let mut frame = frame(id);
    if frame.maximized {
        frame.maximized = false;
        frame.x = frame.rx;
        frame.y = frame.ry;
        frame.w = frame.rw;
        frame.h = frame.rh;
    } else {
        frame.rx = frame.x;
        frame.ry = frame.y;
        frame.rw = frame.w;
        frame.rh = frame.h;
        frame.maximized = true;
        frame.x = 0.0;
        frame.y = 0.0;
        frame.w = stage_w.max(MIN_W);
        frame.h = stage_h.max(MIN_H);
    }
    put(id, frame);
    raise(id);
    true
}

pub fn on_drag(id: i64, mode: u8, dx: f32, dy: f32, phase: u8, stage_w: f32, stage_h: f32) {
    match phase {
        0 => {
            ORIGIN.with(|origin| {
                origin.set(Some(DragOrigin {
                    id,
                    frame: frame(id),
                }))
            });
        }
        1 => {
            let Some(origin) = ORIGIN.with(Cell::get) else {
                return;
            };
            if origin.id != id {
                return;
            }
            let next = apply_drag(&origin.frame, mode, dx, dy, stage_w, stage_h);
            let mut next = next;
            next.z = frame(id).z.max(next.z);
            put(id, next);
            state::mark_dirty();
        }
        2 => {}
        _ => {}
    }
}

pub fn apply_drag(frame: &Frame, mode: u8, dx: f32, dy: f32, stage_w: f32, stage_h: f32) -> Frame {
    let mut next = *frame;
    if next.maximized {
        next.maximized = false;
        next.x = next.rx;
        next.y = next.ry;
        next.w = next.rw;
        next.h = next.rh;
    }
    if mode == 0 {
        let max_x = (stage_w - 64.0).max(0.0);
        let max_y = (stage_h - 40.0).max(0.0);
        next.x = (next.x + dx).clamp(48.0 - next.w, max_x);
        next.y = (next.y + dy).clamp(0.0, max_y);
        return next;
    }
    let right = next.x + next.w;
    let bottom = next.y + next.h;
    if matches!(mode, 1 | 5 | 6) {
        let y = (next.y + dy).clamp(0.0, bottom - MIN_H);
        next.y = y;
        next.h = bottom - y;
    }
    if matches!(mode, 2 | 7 | 8) {
        next.h = (next.h + dy).clamp(MIN_H, stage_h.max(MIN_H));
    }
    if matches!(mode, 4 | 6 | 8) {
        let x = (next.x + dx).clamp(0.0, right - MIN_W);
        next.x = x;
        next.w = right - x;
    }
    if matches!(mode, 3 | 5 | 7) {
        next.w = (next.w + dx).clamp(MIN_W, stage_w.max(MIN_W));
    }
    next
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub open: i64,
    pub minimized: i64,
    pub focused: i64,
    pub frames: Vec<(i64, Frame)>,
}

pub fn format_session(open: i64, minimized: i64, focused: i64, frames: &[(i64, Frame)]) -> String {
    let mut text = format!("w3win1\n{open} {minimized} {focused}\n");
    for (id, frame) in frames {
        text.push_str(&format!(
            "{id} {:.1} {:.1} {:.1} {:.1} {} {} {:.1} {:.1} {:.1} {:.1}\n",
            frame.x,
            frame.y,
            frame.w,
            frame.h,
            frame.z,
            i32::from(frame.maximized),
            frame.rx,
            frame.ry,
            frame.rw,
            frame.rh,
        ));
    }
    text
}

pub fn parse_session(text: &str) -> Option<Session> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    if lines.next()?.trim() != "w3win1" {
        return None;
    }
    let header: Vec<&str> = lines.next()?.split_whitespace().collect();
    if header.len() != 3 {
        return None;
    }
    let open = header[0].parse().ok()?;
    let minimized = header[1].parse().ok()?;
    let focused = header[2].parse().ok()?;
    let mut frames = Vec::new();
    for line in lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 11 {
            return None;
        }
        let id: i64 = parts[0].parse().ok()?;
        if !(1..=6).contains(&id) {
            return None;
        }
        let frame = Frame {
            x: parts[1].parse().ok()?,
            y: parts[2].parse().ok()?,
            w: parts[3].parse().ok()?,
            h: parts[4].parse().ok()?,
            z: parts[5].parse().ok()?,
            maximized: parts[6] != "0",
            rx: parts[7].parse().ok()?,
            ry: parts[8].parse().ok()?,
            rw: parts[9].parse().ok()?,
            rh: parts[10].parse().ok()?,
        };
        frames.push((id, frame));
    }
    Some(Session {
        open,
        minimized,
        focused,
        frames,
    })
}

fn state_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("W3COS_WINDOW_STATE") {
        if path.is_empty() {
            return None;
        }
        return Some(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".w3cos").join("windows.state"))
}

pub fn load_once() -> Option<Session> {
    if LOADED.with(|loaded| loaded.replace(true)) {
        return None;
    }
    let path = state_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let session = parse_session(&text)?;
    FRAMES.with(|frames| {
        let mut frames = frames.borrow_mut();
        for (id, frame) in &session.frames {
            frames.insert(*id, *frame);
        }
    });
    Some(session)
}

pub fn save(open: i64, minimized: i64, focused: i64) {
    let Some(path) = state_path() else {
        return;
    };
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = std::fs::create_dir_all(parent);
    }
    let frames = FRAMES.with(|frames| {
        frames
            .borrow()
            .iter()
            .map(|(id, frame)| (*id, *frame))
            .collect::<Vec<_>>()
    });
    let text = format_session(open, minimized, focused, &frames);
    let _ = std::fs::write(path, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Frame {
        Frame {
            x: 40.0,
            y: 20.0,
            w: 400.0,
            h: 300.0,
            z: 2,
            maximized: false,
            rx: 40.0,
            ry: 20.0,
            rw: 400.0,
            rh: 300.0,
        }
    }

    #[test]
    fn drag_moves_and_keeps_a_titlebar_grip() {
        let next = apply_drag(&sample(), 0, 30.0, 15.0, 900.0, 540.0);
        assert_eq!(next.x, 70.0);
        assert_eq!(next.y, 35.0);
        let shoved = apply_drag(&sample(), 0, 2000.0, 2000.0, 900.0, 540.0);
        assert!(shoved.x <= 900.0 - 64.0);
        assert!(shoved.y <= 540.0 - 40.0);
    }

    #[test]
    fn resize_respects_the_minimum_and_the_east_edge() {
        let next = apply_drag(&sample(), 7, 40.0, 25.0, 900.0, 540.0);
        assert_eq!(next.w, 440.0);
        assert_eq!(next.h, 325.0);
        let tiny = apply_drag(&sample(), 3, -500.0, 0.0, 900.0, 540.0);
        assert_eq!(tiny.w, MIN_W);
    }

    #[test]
    fn dragging_a_maximized_window_restores_it_first() {
        let mut frame = sample();
        frame.maximized = true;
        frame.x = 0.0;
        frame.y = 0.0;
        frame.w = 900.0;
        frame.h = 540.0;
        let next = apply_drag(&frame, 0, 10.0, 8.0, 900.0, 540.0);
        assert!(!next.maximized);
        assert_eq!(next.w, 400.0);
        assert_eq!(next.x, 50.0);
        assert_eq!(next.y, 28.0);
    }

    #[test]
    fn raise_does_not_climb_while_already_in_front() {
        raise(3);
        let once = frame(3).z;
        raise(3);
        assert_eq!(frame(3).z, once);
        assert!(once > frame(1).z || once > default_frame(1).z);
    }

    #[test]
    fn session_text_round_trips() {
        let frames = vec![(1, sample())];
        let text = format_session(2, 4, 1, &frames);
        let session = parse_session(&text).expect("session");
        assert_eq!(session.open, 2);
        assert_eq!(session.minimized, 4);
        assert_eq!(session.focused, 1);
        assert_eq!(session.frames[0].1, sample());
        assert!(parse_session("nope").is_none());
    }
}
