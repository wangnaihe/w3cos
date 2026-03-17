//! CSS Transition Animation Engine
//!
//! Interpolates style property values over time using easing functions
//! defined in `w3cos_std::style::Easing`.

use std::time::Instant;
use w3cos_std::color::Color;
use w3cos_std::style::{Easing, Style, Transition, TransitionProperty};
use w3cos_std::Component;

/// Tracks an in-progress animation for a specific component.
struct AnimEntry {
    component_index: usize,
    start_time: Instant,
    duration_ms: u32,
    delay_ms: u32,
    easing: Easing,
    /// Snapshot of the style at animation start (before the change).
    from: AnimatedValues,
    /// Target style values (the new style).
    to: AnimatedValues,
}

/// Subset of Style fields that can be animated.
#[derive(Clone, Copy)]
struct AnimatedValues {
    opacity: f32,
    background: Color,
    color: Color,
    border_radius: f32,
}

impl AnimatedValues {
    fn extract(style: &Style) -> Self {
        Self {
            opacity: style.opacity,
            background: style.background,
            color: style.color,
            border_radius: style.border_radius,
        }
    }

    fn apply(&self, style: &mut Style) {
        style.opacity = self.opacity;
        style.background = self.background;
        style.color = self.color;
        style.border_radius = self.border_radius;
    }
}

fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: lerp_u8(a.r, b.r, t),
        g: lerp_u8(a.g, b.g, t),
        b: lerp_u8(a.b, b.b, t),
        a: lerp_u8(a.a, b.a, t),
    }
}

fn interpolate(a: &AnimatedValues, b: &AnimatedValues, t: f32) -> AnimatedValues {
    AnimatedValues {
        opacity: lerp_f32(a.opacity, b.opacity, t),
        background: lerp_color(a.background, b.background, t),
        color: lerp_color(a.color, b.color, t),
        border_radius: lerp_f32(a.border_radius, b.border_radius, t),
    }
}

/// Manages all active CSS transitions.
pub struct AnimationManager {
    entries: Vec<AnimEntry>,
    /// Stores the "before" snapshot of styles for each component index
    /// so we can detect style changes and start transitions.
    prev_styles: Vec<Option<AnimatedValues>>,
}

impl AnimationManager {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            prev_styles: Vec::new(),
        }
    }

    /// Detect style changes on components and start new transitions.
    /// Should be called before paint on the cloned render root.
    pub fn detect_changes(&mut self, root: &Component) {
        let count = AnimationManager::count_components(root);
        self.ensure_prev_size(count);
        let mut idx = 0usize;
        Self::detect_recursive(root, &mut idx, &mut self.prev_styles, &mut self.entries);
    }

    fn detect_recursive(
        comp: &Component,
        idx: &mut usize,
        prev_styles: &mut Vec<Option<AnimatedValues>>,
        entries: &mut Vec<AnimEntry>,
    ) {
        let my_idx = *idx;
        *idx += 1;

        let current_vals = AnimatedValues::extract(&comp.style);

        if let Some(slot) = prev_styles.get_mut(my_idx) {
            let prev_val = *slot;
            // Check if transition is defined and values actually changed
            if let Some(p) = prev_val {
                if let Some(ref transition) = comp.style.transition {
                    let changed = p.opacity != current_vals.opacity
                        || p.background != current_vals.background
                        || p.color != current_vals.color
                        || p.border_radius != current_vals.border_radius;

                    if changed && Self::property_matches(transition, &p, &current_vals) {
                        entries.retain(|e| e.component_index != my_idx);
                        entries.push(AnimEntry {
                            component_index: my_idx,
                            start_time: Instant::now(),
                            duration_ms: transition.duration_ms,
                            easing: transition.easing,
                            delay_ms: transition.delay_ms,
                            from: p,
                            to: current_vals,
                        });
                    }
                }
            }
            *slot = Some(current_vals);
        }

        for child in &comp.children {
            Self::detect_recursive(child, idx, prev_styles, entries);
        }
    }

    fn property_matches(transition: &Transition, from: &AnimatedValues, to: &AnimatedValues) -> bool {
        match transition.property {
            TransitionProperty::All => true,
            TransitionProperty::Opacity => from.opacity != to.opacity,
            TransitionProperty::Background => from.background != to.background,
            TransitionProperty::Color => from.color != to.color,
            TransitionProperty::Transform => false, // transform not yet animatable
            TransitionProperty::Custom(_) => true,
        }
    }

    /// Ensure `prev_styles` has the correct size for the flattened tree.
    fn ensure_prev_size(&mut self, count: usize) {
        if self.prev_styles.len() < count {
            self.prev_styles.resize_with(count, || None);
        }
        self.prev_styles.truncate(count);
    }

    /// Process all active animations and apply interpolated styles.
    /// Returns `true` if any animation is still running (caller should request another frame).
    pub fn tick_and_apply(&mut self, root: &mut Component) -> bool {
        // First, ensure we have the right size
        let count = AnimationManager::count_components(root);
        self.ensure_prev_size(count);

        // Detect new style changes
        self.detect_changes(root);

        let now = Instant::now();
        let mut any_active = false;

        for entry in &self.entries {
            let elapsed = now.duration_since(entry.start_time).as_millis() as u32;

            // Handle delay
            if elapsed < entry.delay_ms {
                any_active = true;
                continue;
            }

            let anim_elapsed = elapsed - entry.delay_ms;
            let t_raw = if entry.duration_ms > 0 {
                (anim_elapsed as f32) / (entry.duration_ms as f32)
            } else {
                1.0
            };

            if t_raw >= 1.0 {
                // Animation complete — apply final values
                let mut counter = 0usize;
                Self::apply_to_component(root, entry.component_index, &entry.to, &mut counter);
            } else {
                // Interpolate
                let eased_t = entry.easing.interpolate(t_raw);
                let interpolated = interpolate(&entry.from, &entry.to, eased_t);

                let mut counter = 0usize;
                Self::apply_to_component(root, entry.component_index, &interpolated, &mut counter);
                any_active = true;
            }
        }

        // Remove completed animations
        let now = Instant::now();
        self.entries.retain(|e| {
            let elapsed = now.duration_since(e.start_time).as_millis() as u32;
            elapsed < e.delay_ms + e.duration_ms
        });

        any_active
    }

    fn count_components(comp: &Component) -> usize {
        let mut count = 1;
        for child in &comp.children {
            count += Self::count_components(child);
        }
        count
    }

    fn apply_to_component(
        comp: &mut Component,
        target_idx: usize,
        values: &AnimatedValues,
        counter: &mut usize,
    ) {
        let my_idx = *counter;
        *counter += 1;

        if my_idx == target_idx {
            values.apply(&mut comp.style);
        }

        for child in &mut comp.children {
            Self::apply_to_component(child, target_idx, values, counter);
        }
    }
}
