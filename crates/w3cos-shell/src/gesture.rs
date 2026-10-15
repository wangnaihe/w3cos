//! Phone gestures, receipt filters, and the coarse-pointer target size.
//!
//! The runtime reports a press origin and a release delta. This module
//! decides what that motion means. Confirm cards and permission sheets are
//! never closed by an edge swipe.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Swipe {
    None,
    Switcher,
    Notifications,
    EdgeBack,
    /// The motion was an edge-back, and a sheet is open, so it is swallowed.
    EdgeIgnored,
    CloseFront,
}

const SLOP: f32 = 48.0;

pub fn classify(
    x0: f32,
    y0: f32,
    dx: f32,
    dy: f32,
    width: f32,
    height: f32,
    phone: bool,
    confirm_open: bool,
    permission_open: bool,
    switcher_open: bool,
) -> Swipe {
    if !phone || width < 1.0 || height < 1.0 {
        return Swipe::None;
    }
    let horizontal = dx.abs() > dy.abs();
    if x0 <= 24.0 && dx >= SLOP && horizontal {
        if confirm_open || permission_open {
            return Swipe::EdgeIgnored;
        }
        return Swipe::EdgeBack;
    }
    if y0 >= height - 72.0 && dy <= -SLOP && !horizontal {
        return if switcher_open {
            Swipe::CloseFront
        } else {
            Swipe::Switcher
        };
    }
    if y0 <= 36.0 && dy >= SLOP && !horizontal {
        return Swipe::Notifications;
    }
    Swipe::None
}

pub fn coarse_target(coarse: bool) -> f32 {
    if coarse { 48.0 } else { 28.0 }
}

/// Filter 0 is everything. 1 allowed, 2 denied, 3 cancelled.
pub fn receipt_matches(filter: i64, line: &str) -> bool {
    let decision = line.split_whitespace().nth(1).unwrap_or("");
    match filter {
        1 => decision == "allowed",
        2 => decision == "denied",
        3 => decision == "cancelled",
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swipe_up_from_the_bottom_opens_the_switcher() {
        assert_eq!(
            classify(
                200.0, 740.0, 4.0, -80.0, 390.0, 760.0, true, false, false, false
            ),
            Swipe::Switcher
        );
    }

    #[test]
    fn swipe_up_while_the_switcher_is_open_closes_the_front_card() {
        assert_eq!(
            classify(
                200.0, 740.0, 0.0, -90.0, 390.0, 760.0, true, false, false, true
            ),
            Swipe::CloseFront
        );
    }

    #[test]
    fn swipe_down_from_the_top_opens_notifications() {
        assert_eq!(
            classify(
                180.0, 12.0, 2.0, 70.0, 390.0, 760.0, true, false, false, false
            ),
            Swipe::Notifications
        );
    }

    #[test]
    fn edge_back_does_not_dismiss_confirm_or_permission() {
        assert_eq!(
            classify(
                8.0, 400.0, 80.0, 4.0, 390.0, 760.0, true, true, false, false
            ),
            Swipe::EdgeIgnored
        );
        assert_eq!(
            classify(
                8.0, 400.0, 80.0, 4.0, 390.0, 760.0, true, false, true, false
            ),
            Swipe::EdgeIgnored
        );
        assert_eq!(
            classify(
                8.0, 400.0, 80.0, 4.0, 390.0, 760.0, true, false, false, false
            ),
            Swipe::EdgeBack
        );
    }

    #[test]
    fn desktop_and_tiny_motion_are_not_gestures() {
        assert_eq!(
            classify(
                8.0, 400.0, 90.0, 0.0, 1280.0, 800.0, false, false, false, false
            ),
            Swipe::None
        );
        assert_eq!(
            classify(
                8.0, 400.0, 12.0, 4.0, 390.0, 760.0, true, false, false, false
            ),
            Swipe::None
        );
    }

    #[test]
    fn receipt_filter_keeps_the_matching_decision() {
        let allowed = "#1 allowed open tier=L0 actor=me confirmed=true — opened";
        let denied = "#2 denied run tier=L0 actor=me confirmed=false — locked";
        assert!(receipt_matches(0, allowed));
        assert!(receipt_matches(1, allowed));
        assert!(!receipt_matches(1, denied));
        assert!(receipt_matches(2, denied));
        assert!(!receipt_matches(3, allowed));
    }

    #[test]
    fn coarse_targets_are_48_and_fine_title_buttons_stay_28() {
        assert_eq!(coarse_target(true), 48.0);
        assert_eq!(coarse_target(false), 28.0);
    }
}
