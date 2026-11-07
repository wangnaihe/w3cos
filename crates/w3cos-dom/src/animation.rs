//! Animated style (Blink `core/animation`, compositor-free).
//!
//! Mirrors Blink's split: a `DocumentTimeline` clock, per-element
//! `ElementAnimations` holding script-created `KeyframeEffect`s, CSS
//! animations created from the cascaded `animation-*` values
//! (`CSSAnimations::CalculateAnimationUpdate`), and the Web Animations timing
//! model (web-animations-1 §4.8) producing an iteration progress that samples
//! the keyframes. The sampled values are applied as the animation cascade
//! origin (`StyleResolver::ApplyAnimatedStyle`): above author normal
//! declarations, below `!important`, so both getComputedStyle and layout see
//! them. Elements whose animated value depends on time are recorded so the
//! frame loop can invalidate them (`DocumentAnimations::
//! UpdateAnimationTimingForAnimationFrame`).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use w3cos_std::style::Easing;

/// One keyframe: computed offset in [0, 1] plus its declarations.
#[derive(Clone, Debug, PartialEq)]
pub struct Keyframe {
    pub offset: f64,
    pub declarations: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillMode {
    None,
    Forwards,
    Backwards,
    Both,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackDirection {
    Normal,
    Reverse,
    Alternate,
    AlternateReverse,
}

/// `EffectTiming` with resolved defaults (ms; `duration` NaN = auto = 0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    pub delay: f64,
    pub end_delay: f64,
    pub duration: f64,
    pub iterations: f64,
    pub iteration_start: f64,
    pub direction: PlaybackDirection,
    pub fill: FillMode,
    pub easing: Easing,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            delay: 0.0,
            end_delay: 0.0,
            duration: 0.0,
            iterations: 1.0,
            iteration_start: 0.0,
            direction: PlaybackDirection::Normal,
            fill: FillMode::Auto,
            easing: Easing::Linear,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Before,
    Active,
    After,
}

impl Timing {
    pub fn active_duration(&self) -> f64 {
        let duration = self.duration.max(0.0);
        if duration == 0.0 || self.iterations == 0.0 {
            0.0
        } else {
            duration * self.iterations
        }
    }

    pub fn end_time(&self) -> f64 {
        (self.delay + self.active_duration() + self.end_delay).max(0.0)
    }

    pub fn phase(&self, local_time: f64) -> Phase {
        let active = self.active_duration();
        let end = self.end_time();
        let before_boundary = self.delay.min(end).max(0.0);
        let after_boundary = (self.delay + active).min(end).max(0.0);
        if local_time < before_boundary {
            Phase::Before
        } else if local_time >= after_boundary {
            Phase::After
        } else {
            Phase::Active
        }
    }

    /// Transformed progress (web-animations-1 §4.8), `None` when the effect
    /// is not in effect at `local_time` (outside its fill).
    pub fn transformed_progress(&self, local_time: f64) -> Option<f64> {
        let phase = self.phase(local_time);
        let active = self.active_duration();
        let fill = self.fill;
        let backwards = matches!(fill, FillMode::Backwards | FillMode::Both);
        let forwards = matches!(fill, FillMode::Forwards | FillMode::Both);
        let active_time = match phase {
            Phase::Before if backwards => (local_time - self.delay).max(0.0),
            Phase::Before => return None,
            Phase::Active => local_time - self.delay,
            Phase::After if forwards => (local_time - self.delay).min(active).max(0.0),
            Phase::After => return None,
        };
        let duration = self.duration.max(0.0);
        let overall = if duration == 0.0 {
            if phase == Phase::Before {
                0.0
            } else {
                self.iterations
            }
        } else {
            active_time / duration
        } + self.iteration_start;
        let mut simple = if overall.is_infinite() {
            self.iteration_start % 1.0
        } else {
            overall % 1.0
        };
        if simple == 0.0
            && phase != Phase::Before
            && active_time == active
            && self.iterations != 0.0
            && overall != 0.0
        {
            simple = 1.0;
        }
        let current_iteration = if phase == Phase::After && self.iterations.is_infinite() {
            f64::INFINITY
        } else if simple == 1.0 {
            overall.floor() - 1.0
        } else {
            overall.floor()
        };
        let forwards_iteration = current_iteration.is_infinite() || current_iteration % 2.0 == 0.0;
        let directed = match self.direction {
            PlaybackDirection::Normal => simple,
            PlaybackDirection::Reverse => 1.0 - simple,
            PlaybackDirection::Alternate if forwards_iteration => simple,
            PlaybackDirection::Alternate => 1.0 - simple,
            PlaybackDirection::AlternateReverse if forwards_iteration => 1.0 - simple,
            PlaybackDirection::AlternateReverse => simple,
        };
        Some(match self.easing {
            Easing::Linear => directed,
            easing => f64::from(easing.interpolate(directed as f32)),
        })
    }
}

/// Animation playback state (`Animation` start time / hold time).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaybackClock {
    pub start_time: Option<f64>,
    pub hold_time: Option<f64>,
    pub playback_rate: f64,
}

impl PlaybackClock {
    pub fn current_time(&self, timeline_now: f64) -> Option<f64> {
        self.hold_time.or_else(|| {
            self.start_time
                .map(|start| (timeline_now - start) * self.playback_rate)
        })
    }
}

/// One script-created effect registered on its target element.
#[derive(Clone, Debug)]
pub struct ElementEffect {
    pub id: u64,
    pub keyframes: Rc<Vec<Keyframe>>,
    pub timing: Timing,
    pub clock: PlaybackClock,
}

struct CssAnimationState {
    name: String,
    clock: PlaybackClock,
}

thread_local! {
    static TIMELINE_ORIGIN: Cell<Option<Instant>> = const { Cell::new(None) };
    static KEYFRAMES: RefCell<Vec<(u64, String, Rc<Vec<Keyframe>>)>> = const { RefCell::new(Vec::new()) };
    static ELEMENT_EFFECTS: RefCell<HashMap<u32, Vec<ElementEffect>>> = RefCell::new(HashMap::new());
    static CSS_ANIMATIONS: RefCell<HashMap<u32, Vec<CssAnimationState>>> = RefCell::new(HashMap::new());
    static TIME_DEPENDENT: RefCell<HashSet<u32>> = RefCell::new(HashSet::new());
    static DISABLED: bool = std::env::var_os("W3COS_ANIMATED_STYLE").is_some_and(|value| value == "0");
}

/// `document.timeline.currentTime` in ms since the timeline origin.
pub fn timeline_now() -> f64 {
    TIMELINE_ORIGIN.with(|origin| {
        let start = origin.get().unwrap_or_else(|| {
            let now = Instant::now();
            origin.set(Some(now));
            now
        });
        start.elapsed().as_secs_f64() * 1000.0
    })
}

/// Register `@keyframes name` from a stylesheet owner (later rules win,
/// CSS Animations 1 §3).
pub fn register_keyframes(owner: u64, name: &str, keyframes: Vec<Keyframe>) {
    KEYFRAMES.with(|registry| {
        registry
            .borrow_mut()
            .push((owner, name.to_string(), Rc::new(keyframes)))
    });
    crate::stylesheet::note_animation_rules_changed();
}

pub(crate) fn clear_keyframes_owner(owner: Option<u64>) -> bool {
    KEYFRAMES
        .try_with(|registry| {
            let mut registry = registry.borrow_mut();
            let before = registry.len();
            match owner {
                Some(owner) => registry.retain(|(entry, _, _)| *entry != owner),
                None => registry.clear(),
            }
            registry.len() != before
        })
        .unwrap_or(false)
}

fn keyframes_named(name: &str) -> Option<Rc<Vec<Keyframe>>> {
    KEYFRAMES.with(|registry| {
        registry
            .borrow()
            .iter()
            .rev()
            .find(|(_, entry, _)| entry == name)
            .map(|(_, _, frames)| Rc::clone(frames))
    })
}

/// Parse the body of an `@keyframes` rule (`from {..} 50% {..} to {..}`).
pub fn parse_keyframes_body(body: &str) -> Vec<Keyframe> {
    let mut frames: Vec<Keyframe> = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find('{') {
        let selectors = &rest[..open];
        let Some(close) = rest[open..].find('}').map(|offset| open + offset) else {
            break;
        };
        let block = &rest[open + 1..close];
        rest = &rest[close + 1..];
        let declarations: Vec<(String, String)> = block
            .split(';')
            .filter_map(|declaration| {
                let (property, value) = declaration.split_once(':')?;
                let (property, value) = (property.trim(), value.trim());
                // `!important` declarations in keyframes are ignored.
                (!property.is_empty() && !value.is_empty() && !value.contains("!important"))
                    .then(|| (property.to_ascii_lowercase(), value.to_string()))
            })
            .collect();
        for selector in selectors.split(',') {
            let selector = selector.trim().to_ascii_lowercase();
            let offset = match selector.as_str() {
                "from" => 0.0,
                "to" => 1.0,
                percent => match percent
                    .strip_suffix('%')
                    .and_then(|v| v.trim().parse::<f64>().ok())
                {
                    Some(value) if (0.0..=100.0).contains(&value) => value / 100.0,
                    _ => continue,
                },
            };
            // Keyframes sharing an offset cascade (later declarations win).
            if let Some(frame) = frames.iter_mut().find(|frame| frame.offset == offset) {
                for (property, value) in &declarations {
                    frame
                        .declarations
                        .retain(|(existing, _)| existing != property);
                    frame.declarations.push((property.clone(), value.clone()));
                }
            } else {
                frames.push(Keyframe {
                    offset,
                    declarations: declarations.clone(),
                });
            }
        }
    }
    frames.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    frames
}

/// Install, replace (same `id`) or remove (`None`) a script effect.
pub fn set_element_effect(node: u32, id: u64, effect: Option<ElementEffect>) {
    ELEMENT_EFFECTS.with(|effects| {
        let mut effects = effects.borrow_mut();
        let list = effects.entry(node).or_default();
        match effect {
            Some(effect) => {
                if let Some(slot) = list.iter_mut().find(|existing| existing.id == id) {
                    *slot = effect;
                } else {
                    list.push(effect);
                }
            }
            None => list.retain(|existing| existing.id != id),
        }
        if list.is_empty() {
            effects.remove(&node);
        }
    });
}

pub fn node_has_script_effects(node: u32) -> bool {
    ELEMENT_EFFECTS.with(|effects| {
        let effects = effects.borrow();
        !effects.is_empty() && effects.contains_key(&node)
    })
}

/// Nodes whose animated style changes with time; drained by the frame loop.
pub fn take_time_dependent_nodes() -> Vec<u32> {
    TIME_DEPENDENT.with(|nodes| nodes.borrow_mut().drain().collect())
}

pub fn reset() {
    ELEMENT_EFFECTS.with(|effects| effects.borrow_mut().clear());
    CSS_ANIMATIONS.with(|animations| animations.borrow_mut().clear());
    TIME_DEPENDENT.with(|nodes| nodes.borrow_mut().clear());
}

/// `<easing-function>` for script timing (`linear` when unparsable).
pub fn parse_easing(value: &str) -> Easing {
    crate::css_style::parse_easing(value.trim()).unwrap_or(Easing::Linear)
}

pub fn enabled() -> bool {
    !DISABLED.with(|disabled| *disabled)
}

fn css_timing(animation: &w3cos_std::style::Animation) -> Timing {
    use w3cos_std::style::{AnimationDirection, AnimationFillMode, AnimationIterationCount};
    Timing {
        delay: f64::from(animation.delay_ms),
        end_delay: 0.0,
        duration: f64::from(animation.duration_ms),
        iterations: match animation.iteration_count {
            AnimationIterationCount::Once => 1.0,
            AnimationIterationCount::Count(count) => f64::from(count),
            AnimationIterationCount::Infinite => f64::INFINITY,
        },
        iteration_start: 0.0,
        direction: match animation.direction {
            AnimationDirection::Normal => PlaybackDirection::Normal,
            AnimationDirection::Reverse => PlaybackDirection::Reverse,
            AnimationDirection::Alternate => PlaybackDirection::Alternate,
            AnimationDirection::AlternateReverse => PlaybackDirection::AlternateReverse,
        },
        fill: match animation.fill_mode {
            AnimationFillMode::None => FillMode::None,
            AnimationFillMode::Forwards => FillMode::Forwards,
            AnimationFillMode::Backwards => FillMode::Backwards,
            AnimationFillMode::Both => FillMode::Both,
        },
        // CSS keyframes ease per keyframe interval with the
        // animation-timing-function; the effect timing itself is linear.
        easing: Easing::Linear,
    }
}

/// Advance the CSS animation's clock for this style resolution
/// (`CSSAnimations::CalculateAnimationUpdate`: create on first sight, pause /
/// resume on `animation-play-state` changes, cancel when the name changes).
fn css_animation_clock(node: u32, name: &str, paused: bool, now: f64) -> PlaybackClock {
    CSS_ANIMATIONS.with(|animations| {
        let mut animations = animations.borrow_mut();
        let list = animations.entry(node).or_default();
        list.retain(|state| state.name == name);
        if list.is_empty() {
            list.push(CssAnimationState {
                name: name.to_string(),
                clock: PlaybackClock {
                    start_time: (!paused).then_some(now),
                    hold_time: paused.then_some(0.0),
                    playback_rate: 1.0,
                },
            });
        }
        let clock = &mut list[0].clock;
        match (paused, clock.hold_time) {
            (true, None) => {
                clock.hold_time = clock.current_time(now);
                clock.start_time = None;
            }
            (false, Some(hold)) => {
                clock.start_time = Some(now - hold);
                clock.hold_time = None;
            }
            _ => {}
        }
        *clock
    })
}

fn forget_css_animations(node: u32) {
    CSS_ANIMATIONS.with(|animations| {
        let mut animations = animations.borrow_mut();
        if !animations.is_empty() {
            animations.remove(&node);
        }
    });
}

/// Declarations of the animation cascade origin for `node`, in composite
/// order (CSS animations, then script animations by creation). `underlying`
/// reads the cascaded (pre-animation) value for neutral keyframes.
pub fn animated_declarations(
    node: u32,
    css_animation: Option<&w3cos_std::style::Animation>,
    underlying: &dyn Fn(&str) -> String,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if !enabled() {
        return out;
    }
    let css =
        css_animation.filter(|animation| !animation.name.is_empty() && animation.name != "none");
    let has_script = node_has_script_effects(node);
    if css.is_none() {
        forget_css_animations(node);
        if !has_script {
            return out;
        }
    }
    let now = timeline_now();
    let mut time_dependent = false;
    if let Some(animation) = css
        && let Some(frames) = keyframes_named(&animation.name)
    {
        let clock = css_animation_clock(node, &animation.name, animation.paused, now);
        time_dependent |= clock.hold_time.is_none();
        let timing = css_timing(animation);
        if let Some(local) = clock.current_time(now)
            && let Some(progress) = timing.transformed_progress(local)
        {
            sample_keyframes(
                &frames,
                progress,
                Some(animation.easing),
                underlying,
                &mut out,
            );
        }
    }
    if has_script {
        let effects = ELEMENT_EFFECTS.with(|effects| effects.borrow().get(&node).cloned());
        for effect in effects.into_iter().flatten() {
            time_dependent |= effect.clock.hold_time.is_none() && effect.clock.start_time.is_some();
            let Some(local) = effect.clock.current_time(now) else {
                continue;
            };
            let mut timing = effect.timing;
            if timing.fill == FillMode::Auto {
                timing.fill = FillMode::None;
            }
            if let Some(progress) = timing.transformed_progress(local) {
                let lookup = |property: &str| {
                    out.iter()
                        .rev()
                        .find(|(name, _)| name == property)
                        .map(|(_, value)| value.clone())
                        .unwrap_or_else(|| underlying(property))
                };
                let mut sampled = Vec::new();
                sample_keyframes(&effect.keyframes, progress, None, &lookup, &mut sampled);
                out.extend(sampled);
            }
        }
    }
    if time_dependent {
        TIME_DEPENDENT.with(|nodes| nodes.borrow_mut().insert(node));
    }
    out
}

/// Effect value of a keyframe effect (web-animations-1 §5.3.4) for every
/// property it animates. `interval_easing` is CSS's per-interval
/// animation-timing-function.
fn sample_keyframes(
    frames: &[Keyframe],
    progress: f64,
    interval_easing: Option<Easing>,
    underlying: &dyn Fn(&str) -> String,
    out: &mut Vec<(String, String)>,
) {
    let mut properties: Vec<&str> = Vec::new();
    for frame in frames {
        for (property, _) in &frame.declarations {
            if !properties.contains(&property.as_str()) {
                properties.push(property);
            }
        }
    }
    for property in properties {
        let mut stops: Vec<(f64, String)> = frames
            .iter()
            .filter_map(|frame| {
                frame
                    .declarations
                    .iter()
                    .rev()
                    .find(|(name, _)| name == property)
                    .map(|(_, value)| (frame.offset, value.clone()))
            })
            .collect();
        if stops.first().is_none_or(|(offset, _)| *offset != 0.0) {
            stops.insert(0, (0.0, underlying(property)));
        }
        if stops.last().is_none_or(|(offset, _)| *offset != 1.0) {
            stops.push((1.0, underlying(property)));
        }
        let value = if stops.len() == 1 {
            stops[0].1.clone()
        } else {
            let start_index = if progress < 0.0 {
                0
            } else if progress >= 1.0 {
                stops.len() - 2
            } else {
                stops
                    .iter()
                    .rposition(|(offset, _)| *offset <= progress)
                    .unwrap_or(0)
                    .min(stops.len() - 2)
            };
            let (from_offset, from) = &stops[start_index];
            let (to_offset, to) = &stops[start_index + 1];
            let span = to_offset - from_offset;
            let mut fraction = if span == 0.0 {
                0.0
            } else {
                (progress - from_offset) / span
            };
            if let Some(easing) = interval_easing
                && easing != Easing::Linear
            {
                fraction = f64::from(easing.interpolate(fraction as f32));
            }
            interpolate_value(property, from, to, fraction)
        };
        out.push((property.to_string(), value));
    }
}

#[derive(PartialEq)]
enum Token<'a> {
    Text(&'a str),
    Number(f64, &'a str),
}

fn tokenize(value: &str) -> Vec<Token<'_>> {
    let bytes = value.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut text_start = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let starts_number = byte.is_ascii_digit()
            || (byte == b'.' && bytes.get(index + 1).is_some_and(u8::is_ascii_digit))
            || (matches!(byte, b'-' | b'+')
                && bytes
                    .get(index + 1)
                    .is_some_and(|next| next.is_ascii_digit() || *next == b'.'));
        // A digit inside an identifier (e.g. `translate3d`, `#a1b2c3`) is text.
        let inside_word = index > 0
            && (bytes[index - 1].is_ascii_alphabetic()
                || bytes[index - 1] == b'#'
                || bytes[index - 1] == b'_');
        if !starts_number || inside_word {
            index += 1;
            continue;
        }
        if text_start < index {
            tokens.push(Token::Text(&value[text_start..index]));
        }
        let number_start = index;
        index += 1;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
            index += 1;
        }
        if index < bytes.len()
            && matches!(bytes[index], b'e' | b'E')
            && bytes
                .get(index + 1)
                .is_some_and(|next| next.is_ascii_digit() || matches!(next, b'-' | b'+'))
        {
            index += 2;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
        }
        let number = value[number_start..index].parse::<f64>().unwrap_or(0.0);
        let unit_start = index;
        while index < bytes.len() && (bytes[index].is_ascii_alphabetic() || bytes[index] == b'%') {
            index += 1;
        }
        tokens.push(Token::Number(number, &value[unit_start..index]));
        text_start = index;
    }
    if text_start < bytes.len() {
        tokens.push(Token::Text(&value[text_start..]));
    }
    tokens
}

fn format_number(value: f64) -> String {
    // Blink stores animated lengths/numbers as floats; round away f64 noise.
    let rounded = (value * 1_000_000.0).round() / 1_000_000.0;
    let rounded = if rounded == 0.0 { 0.0 } else { rounded };
    format!("{rounded}")
}

/// Interpolate two specified values. Values with the same shape interpolate
/// component-wise (numbers, lengths, transform lists with matching function
/// lists); colors interpolate in sRGB; anything else is discrete (flip at 0.5).
pub fn interpolate_value(property: &str, from: &str, to: &str, fraction: f64) -> String {
    let discrete = || if fraction < 0.5 { from } else { to }.to_string();
    let (from_trim, to_trim) = (from.trim(), to.trim());
    if from_trim.eq_ignore_ascii_case(to_trim) {
        return from_trim.to_string();
    }
    let lerp = |a: f64, b: f64| a + (b - a) * fraction;
    if (property.contains("color") || property == "background")
        && let (Some(a), Some(b)) = (
            w3cos_std::Color::from_css(from_trim),
            w3cos_std::Color::from_css(to_trim),
        )
    {
        let channel = |x: u8, y: u8| lerp(f64::from(x), f64::from(y)).round().clamp(0.0, 255.0);
        return format!(
            "rgba({}, {}, {}, {})",
            channel(a.r, b.r),
            channel(a.g, b.g),
            channel(a.b, b.b),
            format_number(lerp(f64::from(a.a), f64::from(b.a)) / 255.0)
        );
    }
    let from_tokens = tokenize(from_trim);
    let to_tokens = tokenize(to_trim);
    if from_tokens.len() != to_tokens.len() || from_tokens.is_empty() {
        return discrete();
    }
    let mut out = String::new();
    for (a, b) in from_tokens.iter().zip(&to_tokens) {
        match (a, b) {
            (Token::Text(x), Token::Text(y)) if x.eq_ignore_ascii_case(y) => out.push_str(x),
            (Token::Number(x, unit_x), Token::Number(y, unit_y)) => {
                let unit = if unit_x.eq_ignore_ascii_case(unit_y) {
                    unit_x
                } else if *x == 0.0 && unit_x.is_empty() {
                    unit_y
                } else if *y == 0.0 && unit_y.is_empty() {
                    unit_x
                } else {
                    return discrete();
                };
                let mut value = lerp(*x, *y);
                if property == "opacity" {
                    value = value.clamp(0.0, 1.0);
                }
                out.push_str(&format_number(value));
                out.push_str(unit);
            }
            _ => return discrete(),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing(duration: f64, delay: f64, fill: FillMode) -> Timing {
        Timing {
            duration,
            delay,
            fill,
            ..Timing::default()
        }
    }

    #[test]
    fn timing_model_phases_fill_and_negative_delay() {
        let both = timing(1000.0, -300.0, FillMode::Both);
        assert_eq!(both.transformed_progress(0.0), Some(0.3));
        assert_eq!(both.transformed_progress(5000.0), Some(1.0));
        let none = timing(1000.0, 200.0, FillMode::None);
        assert_eq!(none.transformed_progress(100.0), None);
        assert_eq!(none.transformed_progress(1200.0), None);
        assert_eq!(none.transformed_progress(700.0), Some(0.5));
        let backwards = timing(1000.0, 200.0, FillMode::Backwards);
        assert_eq!(backwards.transformed_progress(0.0), Some(0.0));
        let alternate = Timing {
            iterations: 3.0,
            direction: PlaybackDirection::Alternate,
            ..timing(100.0, 0.0, FillMode::Forwards)
        };
        assert_eq!(alternate.transformed_progress(125.0), Some(0.75));
        assert_eq!(alternate.transformed_progress(300.0), Some(1.0));
        let infinite = Timing {
            iterations: f64::INFINITY,
            ..timing(100.0, 0.0, FillMode::None)
        };
        assert_eq!(infinite.transformed_progress(1050.0), Some(0.5));
    }

    #[test]
    fn keyframes_body_parses_offsets_and_merges_duplicates() {
        let frames = parse_keyframes_body(
            "from{opacity:1;width:10px} 50%, 75% {opacity:.5} to{opacity:0} 50%{width:20px}",
        );
        assert_eq!(
            frames.iter().map(|f| f.offset).collect::<Vec<_>>(),
            vec![0.0, 0.5, 0.75, 1.0]
        );
        assert_eq!(frames[1].declarations.len(), 2);
    }

    #[test]
    fn interpolation_is_componentwise_or_discrete() {
        assert_eq!(interpolate_value("width", "10px", "110px", 0.3), "40px");
        assert_eq!(interpolate_value("opacity", "1", "0", 0.3), "0.7");
        assert_eq!(
            interpolate_value("transform", "translateX(0px)", "translateX(100px)", 0.25),
            "translateX(25px)"
        );
        assert_eq!(interpolate_value("left", "0", "10px", 0.5), "5px");
        assert_eq!(interpolate_value("width", "10px", "50%", 0.4), "10px");
        assert_eq!(interpolate_value("display", "block", "none", 0.6), "none");
        assert_eq!(
            interpolate_value("color", "rgb(0, 0, 0)", "rgb(255, 255, 255)", 0.5),
            "rgba(128, 128, 128, 1)"
        );
    }

    #[test]
    fn sampling_uses_underlying_value_for_neutral_keyframes() {
        let frames = vec![Keyframe {
            offset: 1.0,
            declarations: vec![("width".into(), "100px".into())],
        }];
        let mut out = Vec::new();
        sample_keyframes(&frames, 0.5, None, &|_| "0px".to_string(), &mut out);
        assert_eq!(out, vec![("width".to_string(), "50px".to_string())]);
    }
}
