use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tiny_skia::{
    Color as SkColor, FillRule, Mask, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform,
};
use w3cos_std::SvgPathCommand;
use w3cos_std::color::Color;
use w3cos_std::component::ComponentKind;
use w3cos_std::style::{Style, TextAlign};

use crate::filter::{self, CssFilter};
use crate::layout::LayoutRect;
use crate::text_layout;

fn page_bg() -> SkColor {
    SkColor::from_rgba8(11, 18, 32, 255)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ClipMaskKey {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// Retained clip property cache. Scroll frames reuse the same rasterized clip
/// until the framebuffer or clip geometry changes.
#[derive(Default)]
pub struct ClipMaskCache {
    framebuffer_size: (u32, u32),
    masks: HashMap<ClipMaskKey, Mask>,
}

impl ClipMaskCache {
    fn get_or_create(&mut self, pixmap: &Pixmap, rect: LayoutRect) -> Option<&Mask> {
        let size = (pixmap.width(), pixmap.height());
        if self.framebuffer_size != size {
            self.framebuffer_size = size;
            self.masks.clear();
        } else if self.masks.len() >= 8 {
            self.masks.clear();
        }

        let key = ClipMaskKey {
            x: rect.x.to_bits(),
            y: rect.y.to_bits(),
            width: rect.width.to_bits(),
            height: rect.height.to_bits(),
        };
        if !self.masks.contains_key(&key) {
            self.masks.insert(key, make_clip_mask(pixmap, rect)?);
        }
        self.masks.get(&key)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    ch: char,
    font: u64,
    font_size: u32,
}

struct CachedGlyph {
    metrics: fontdue::Metrics,
    bitmap: Vec<u8>,
}

#[derive(Default)]
struct GlyphRasterCache {
    glyphs: HashMap<GlyphKey, CachedGlyph>,
}

impl GlyphRasterCache {
    fn get_or_rasterize(&mut self, font: &fontdue::Font, ch: char, font_size: f32) -> &CachedGlyph {
        let mut font_hasher = DefaultHasher::new();
        font.hash(&mut font_hasher);
        let key = GlyphKey {
            ch,
            font: font_hasher.finish(),
            font_size: font_size.to_bits(),
        };
        if self.glyphs.len() >= 2048 && !self.glyphs.contains_key(&key) {
            self.glyphs.clear();
        }
        self.glyphs.entry(key).or_insert_with(|| {
            let (metrics, bitmap) = font.rasterize(ch, font_size);
            CachedGlyph { metrics, bitmap }
        })
    }
}

thread_local! {
    /// CPU equivalent of Chromium/Skia's retained glyph atlas. Missing glyphs
    /// are drawn from a fallback face, so one thread can cache more than the
    /// primary UI font.
    static GLYPH_RASTER_CACHE: RefCell<GlyphRasterCache> = RefCell::new(GlyphRasterCache::default());
}

pub fn clear_glyph_cache() {
    GLYPH_RASTER_CACHE.with(|cache| cache.borrow_mut().glyphs.clear());
}

pub fn prepaint_text_interest_rect(
    requests: &[text_layout::TextPrepaintRequest],
    font: &fontdue::Font,
    budget: Duration,
) -> usize {
    let started = Instant::now();
    let mut prepared = 0;
    for request in requests {
        if prepared > 0 && started.elapsed() >= budget {
            break;
        }
        let layout = text_layout::retained_text_paint_layout(
            &request.text,
            request.width.max(1.0),
            request.font_size,
            font,
            request.white_space,
        );
        GLYPH_RASTER_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            for line in &layout.lines {
                for ch in line.chars() {
                    cache.get_or_rasterize(font, ch, request.font_size);
                }
            }
        });
        prepared += 1;
    }
    prepared
}

/// Full-frame paint (content dirty or first frame).
pub fn render_frame(
    pixmap: &mut Pixmap,
    nodes: &[(usize, LayoutRect, &ComponentKind, &Style)],
    font: &fontdue::Font,
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    text_input_values: &HashMap<usize, String>,
    focused_index: Option<usize>,
    clip_masks: &mut ClipMaskCache,
) {
    pixmap.fill(page_bg());
    paint_nodes(
        pixmap,
        nodes,
        font,
        scroll_info,
        text_input_values,
        focused_index,
        None,
        None,
        None,
        clip_masks,
    );
}

/// Chromium-style scroll damage: move the retained raster and repaint only the
/// newly exposed strip when the scroll container is safe to copy.
pub fn render_scroll_damage(
    pixmap: &mut Pixmap,
    nodes: &[(usize, LayoutRect, &ComponentKind, &Style)],
    font: &fontdue::Font,
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    text_input_values: &HashMap<usize, String>,
    focused_index: Option<usize>,
    scroll_damages: &[(usize, f32)],
    scrollable: &[(usize, LayoutRect, crate::layout::ScrollExtent)],
    scroll_ancestor: &[Option<usize>],
    clip_masks: &mut ClipMaskCache,
) -> bool {
    let all_copy_safe = scroll_damages.iter().all(|&(scroll_idx, delta_y)| {
        let rect = scrollable
            .iter()
            .find(|(idx, _, _)| *idx == scroll_idx)
            .map(|(_, rect, _)| *rect);
        let style = nodes
            .iter()
            .find(|(idx, _, _, _)| *idx == scroll_idx)
            .map(|(_, _, _, style)| *style);
        rect.is_some_and(|rect| exposed_scroll_strip(rect, delta_y).is_some())
            && style.is_some_and(scroll_raster_copy_safe)
    });
    if !all_copy_safe {
        render_frame(
            pixmap,
            nodes,
            font,
            scroll_info,
            text_input_values,
            focused_index,
            clip_masks,
        );
        return false;
    }

    let mut paint_damages = Vec::with_capacity(scroll_damages.len());
    for &(scroll_idx, delta_y) in scroll_damages {
        let Some((_, rect, _)) = scrollable.iter().find(|(i, _, _)| *i == scroll_idx) else {
            continue;
        };
        let style = nodes
            .iter()
            .find(|(idx, _, _, _)| *idx == scroll_idx)
            .map(|(_, _, _, style)| *style);
        debug_assert!(style.is_some_and(scroll_raster_copy_safe));
        let sticky_protected =
            sticky_scroll_protected_rects(nodes, scroll_info, scroll_ancestor, scroll_idx, *rect);
        let Some(damage_rects) =
            shift_scroll_raster_excluding(pixmap, *rect, delta_y, &sticky_protected)
        else {
            render_frame(
                pixmap,
                nodes,
                font,
                scroll_info,
                text_input_values,
                focused_index,
                clip_masks,
            );
            return false;
        };
        // The framebuffer is retained independently of the platform drawable,
        // so the same raster copy is safe on iOS. Clear the exposed strip with
        // the scrollport's actual opaque background; the old dark fallback was
        // the source of black bands after scroll/keyboard transitions.
        for damage_rect in damage_rects {
            clear_rect_with_color(
                pixmap,
                damage_rect,
                style.map(|style| style.background).unwrap_or(Color::WHITE),
            );
            paint_damages.push((scroll_idx, damage_rect));
        }
    }
    // Damage strips around protected sticky layers are intentionally painted
    // separately. Unioning them would turn a few scanlines into a nearly
    // full-height repaint whenever the sticky panel is large.
    for damage in &paint_damages {
        paint_nodes(
            pixmap,
            nodes,
            font,
            scroll_info,
            text_input_values,
            focused_index,
            Some(std::slice::from_ref(damage)),
            Some(scroll_ancestor),
            None,
            clip_masks,
        );
    }
    true
}

/// Scroll a retained virtualized viewport after its offscreen window changes.
///
/// Virtualizers replace rows at the overscan edge, outside the visible
/// viewport. The retained pixels that remain visible therefore have the same
/// payload and can be shifted exactly like a normal scroll; the newly exposed
/// strip is the only region that must be painted from the updated tree. A
/// full-scrollport repaint here makes every kinetic-scroll frame proportional
/// to the viewport area and defeats virtualization.
pub fn render_scroll_content_change(
    pixmap: &mut Pixmap,
    nodes: &[(usize, LayoutRect, &ComponentKind, &Style)],
    font: &fontdue::Font,
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    text_input_values: &HashMap<usize, String>,
    focused_index: Option<usize>,
    scroll_damages: &[(usize, f32)],
    scrollable: &[(usize, LayoutRect, crate::layout::ScrollExtent)],
    scroll_ancestor: &[Option<usize>],
    clip_masks: &mut ClipMaskCache,
) -> bool {
    render_scroll_damage(
        pixmap,
        nodes,
        font,
        scroll_info,
        text_input_values,
        focused_index,
        scroll_damages,
        scrollable,
        scroll_ancestor,
        clip_masks,
    )
}

/// Reconcile fixed host-tree nodes whose paint payload changed during scrolling.
///
/// Virtual rows already repaint through retained scroll damage. Restricting
/// the gesture-completion pass to exact changed-node rectangles keeps a sticky
/// counter update from rasterizing the rest of the fixed chrome.
///
/// Returns `Some(damaged_rects)` when the retained path handled the update
/// (possibly with an empty rect list). Returns `None` when the caller must
/// fall back to a full-frame paint.
pub fn render_damage_nodes(
    pixmap: &mut Pixmap,
    nodes: &[(usize, LayoutRect, &ComponentKind, &Style)],
    font: &fontdue::Font,
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    text_input_values: &HashMap<usize, String>,
    focused_index: Option<usize>,
    damage_indices: &[usize],
    clip_masks: &mut ClipMaskCache,
) -> Option<Vec<LayoutRect>> {
    if damage_indices.is_empty() {
        return None;
    }
    let full = LayoutRect {
        x: 0.0,
        y: 0.0,
        width: pixmap.width() as f32,
        height: pixmap.height() as f32,
    };
    let mut damages = Vec::new();
    for (_, rect, _, _) in nodes
        .iter()
        .filter(|(index, _, _, _)| damage_indices.contains(index))
    {
        let Some(damage) = intersect_rect(expand_rect(*rect, 4.0), full) else {
            continue;
        };
        if let Some(existing) = damages
            .iter_mut()
            .find(|existing| rects_intersect(expand_rect(**existing, 2.0), damage))
        {
            *existing = union_rect(*existing, damage);
        } else {
            damages.push(damage);
        }
    }
    if damages.is_empty() {
        // The changed host slot can disappear in the final virtual-window
        // commit. Its scrollport pixels were already reconciled by
        // `render_scroll_content_change`, so there is no external work left
        // and a full-frame fallback would only introduce a stop hitch.
        return Some(Vec::new());
    }
    for damage in &damages {
        clear_rect_with_sk_color(pixmap, *damage, page_bg());
        paint_nodes(
            pixmap,
            nodes,
            font,
            scroll_info,
            text_input_values,
            focused_index,
            None,
            None,
            Some(*damage),
            clip_masks,
        );
    }
    Some(damages)
}

fn is_within_scroll_container(
    idx: usize,
    scroll_idx: usize,
    scroll_ancestor: &[Option<usize>],
) -> bool {
    if idx == scroll_idx {
        return true;
    }
    let mut current = scroll_ancestor.get(idx).copied().flatten();
    while let Some(ancestor) = current {
        if ancestor == scroll_idx {
            return true;
        }
        current = scroll_ancestor.get(ancestor).copied().flatten();
    }
    false
}

fn sticky_scroll_protected_rects(
    nodes: &[(usize, LayoutRect, &ComponentKind, &Style)],
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    scroll_ancestor: &[Option<usize>],
    scroll_idx: usize,
    scroll_rect: LayoutRect,
) -> Vec<LayoutRect> {
    nodes
        .iter()
        .filter_map(|(idx, rect, _, style)| {
            if !matches!(style.position, w3cos_std::style::Position::Sticky)
                || !is_within_scroll_container(*idx, scroll_idx, scroll_ancestor)
            {
                return None;
            }
            let mut visual = *rect;
            if let Some((sx, sy, _)) = scroll_info.get(*idx).copied().flatten() {
                visual.x -= sx;
                visual.y -= sy;
            }
            // Sticky pixels are an overlay layer. Keep their retained rows in
            // place instead of copying them with the scrolling content. This
            // is the CPU equivalent of a compositor scroll-exclusion layer.
            intersect_rect(expand_rect(visual, 16.0), scroll_rect)
        })
        .collect()
}

fn shift_scroll_raster_excluding(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    delta_y: f32,
    protected: &[LayoutRect],
) -> Option<Vec<LayoutRect>> {
    if protected.is_empty() {
        shift_scroll_raster(pixmap, rect, delta_y)?;
        return Some(vec![exposed_scroll_strip(rect, delta_y)?]);
    }

    let rect_top = rect.y.floor();
    let rect_bottom = (rect.y + rect.height).ceil();
    let mut ranges: Vec<(f32, f32)> = protected
        .iter()
        .map(|protected| {
            (
                protected.y.floor().max(rect_top),
                (protected.y + protected.height).ceil().min(rect_bottom),
            )
        })
        .filter(|(top, bottom)| top < bottom)
        .collect();
    ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f32, f32)> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(last) = merged.last_mut().filter(|last| range.0 <= last.1) {
            last.1 = last.1.max(range.1);
        } else {
            merged.push(range);
        }
    }

    let mut segments = Vec::with_capacity(merged.len() + 1);
    let mut cursor = rect_top;
    for (top, bottom) in merged {
        if cursor < top {
            segments.push((cursor, top));
        }
        cursor = cursor.max(bottom);
    }
    if cursor < rect_bottom {
        segments.push((cursor, rect_bottom));
    }

    let amount = delta_y.round().abs();
    if amount == 0.0 || amount >= rect.height.floor() {
        return None;
    }
    let mut damages = Vec::with_capacity(segments.len());
    for (top, bottom) in segments {
        let height = bottom - top;
        if height <= amount {
            damages.push(LayoutRect {
                x: rect.x,
                y: top,
                width: rect.width,
                height,
            });
            continue;
        }
        let segment = LayoutRect {
            x: rect.x,
            y: top,
            width: rect.width,
            height,
        };
        shift_scroll_raster(pixmap, segment, delta_y)?;
        damages.push(exposed_scroll_strip(segment, delta_y)?);
    }
    Some(damages)
}

fn node_scroll_damage(
    idx: usize,
    scroll_damages: &[(usize, LayoutRect)],
    scroll_ancestor: &[Option<usize>],
) -> Option<LayoutRect> {
    scroll_damages.iter().find_map(|&(scroll_idx, damage)| {
        // The damage path has already cleared the scrollport with its opaque
        // background. Painting the container again is both redundant and
        // expensive for a viewport-sized box; only descendants can add new
        // pixels after scrolling or a virtual-window swap.
        (idx != scroll_idx && is_within_scroll_container(idx, scroll_idx, scroll_ancestor))
            .then_some(damage)
    })
}

fn scroll_raster_copy_safe(style: &Style) -> bool {
    style.background.a == 255
        && style.opacity >= 0.999
        && style.border_width <= 0.001
        && style.border_radius <= 0.001
        && !style.has_box_shadow()
        && style.backdrop_blur.is_none()
        && style.text_shadows.is_empty()
        && style.filter.is_none()
        && style.transform.translate_x.abs() <= 0.001
        && style.transform.translate_y.abs() <= 0.001
        && (style.transform.scale_x - 1.0).abs() <= 0.001
        && (style.transform.scale_y - 1.0).abs() <= 0.001
}

fn exposed_scroll_strip(rect: LayoutRect, delta_y: f32) -> Option<LayoutRect> {
    let dy = delta_y.round();
    if dy == 0.0 || dy.abs() >= rect.height.floor() {
        return None;
    }
    if dy > 0.0 {
        Some(LayoutRect {
            x: rect.x,
            y: rect.y + rect.height - dy,
            width: rect.width,
            height: dy,
        })
    } else {
        Some(LayoutRect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: -dy,
        })
    }
}

fn shift_scroll_raster(pixmap: &mut Pixmap, rect: LayoutRect, delta_y: f32) -> Option<()> {
    let shift = -(delta_y.round() as i32);
    let width = pixmap.width() as i32;
    let height = pixmap.height() as i32;
    let x0 = (rect.x.floor() as i32).clamp(0, width);
    let x1 = ((rect.x + rect.width).ceil() as i32).clamp(0, width);
    let y0 = (rect.y.floor() as i32).clamp(0, height);
    let y1 = ((rect.y + rect.height).ceil() as i32).clamp(0, height);
    if shift == 0 || shift.unsigned_abs() >= (y1 - y0) as u32 || x0 >= x1 || y0 >= y1 {
        return None;
    }

    let row_width = width as usize;
    let pixels = pixmap.pixels_mut();
    if shift < 0 {
        let amount = -shift;
        for source_y in (y0 + amount)..y1 {
            let destination_y = source_y - amount;
            let source = source_y as usize * row_width + x0 as usize;
            let destination = destination_y as usize * row_width + x0 as usize;
            pixels.copy_within(source..source + (x1 - x0) as usize, destination);
        }
    } else {
        for source_y in (y0..(y1 - shift)).rev() {
            let destination_y = source_y + shift;
            let source = source_y as usize * row_width + x0 as usize;
            let destination = destination_y as usize * row_width + x0 as usize;
            pixels.copy_within(source..source + (x1 - x0) as usize, destination);
        }
    }
    Some(())
}

fn clear_rect_with_color(pixmap: &mut Pixmap, rect: LayoutRect, color: Color) {
    clear_rect_with_sk_color(
        pixmap,
        rect,
        SkColor::from_rgba8(color.r, color.g, color.b, color.a),
    );
}

fn clear_rect_with_sk_color(pixmap: &mut Pixmap, rect: LayoutRect, color: SkColor) {
    let Some(sk) = Rect::from_xywh(rect.x, rect.y, rect.width, rect.height) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color(color);
    pixmap.fill_rect(sk, &paint, Transform::identity(), None);
}

fn intersect_rect(a: LayoutRect, b: LayoutRect) -> Option<LayoutRect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    (right > x && bottom > y).then_some(LayoutRect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

fn union_rect(a: LayoutRect, b: LayoutRect) -> LayoutRect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    LayoutRect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

fn rects_intersect(a: LayoutRect, b: LayoutRect) -> bool {
    intersect_rect(a, b).is_some()
}

fn expand_rect(rect: LayoutRect, amount: f32) -> LayoutRect {
    LayoutRect {
        x: rect.x - amount,
        y: rect.y - amount,
        width: rect.width + amount * 2.0,
        height: rect.height + amount * 2.0,
    }
}

fn paint_nodes(
    pixmap: &mut Pixmap,
    nodes: &[(usize, LayoutRect, &ComponentKind, &Style)],
    font: &fontdue::Font,
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    text_input_values: &HashMap<usize, String>,
    focused_index: Option<usize>,
    scroll_damage: Option<&[(usize, LayoutRect)]>,
    scroll_ancestor: Option<&[Option<usize>]>,
    absolute_damage: Option<LayoutRect>,
    clip_masks: &mut ClipMaskCache,
) {
    for &(idx, rect, kind, style) in nodes {
        let (offset_rect, clip) = match scroll_info.get(idx) {
            Some(Some((sx, sy, clip_rect))) => {
                let offset_rect = LayoutRect {
                    x: rect.x - sx,
                    y: rect.y - sy,
                    width: rect.width,
                    height: rect.height,
                };
                (offset_rect, Some(*clip_rect))
            }
            _ => (rect, None),
        };
        let damage_rect = if let Some(damage) = absolute_damage {
            if !rects_intersect(expand_rect(offset_rect, 64.0), damage) {
                continue;
            }
            Some(damage)
        } else {
            match (scroll_damage, scroll_ancestor) {
                (Some(damages), Some(ancestor)) => {
                    match node_scroll_damage(idx, damages, ancestor) {
                        Some(damage) => Some(damage),
                        None => continue,
                    }
                }
                _ => None,
            }
        };
        if damage_rect
            .is_some_and(|damage| !rects_intersect(expand_rect(offset_rect, 64.0), damage))
        {
            continue;
        }
        let effective_clip = match (clip, damage_rect) {
            (Some(clip), Some(damage)) => intersect_rect(clip, damage),
            (Some(clip), None) => Some(clip),
            (None, Some(damage)) => Some(damage),
            (None, None) => None,
        };
        let clip_mask = effective_clip.and_then(|rect| clip_masks.get_or_create(pixmap, rect));
        let text_value = match kind {
            ComponentKind::TextInput { value, .. } => Some(
                text_input_values
                    .get(&idx)
                    .map(|s| s.as_str())
                    .unwrap_or_else(|| value.as_str()),
            ),
            _ => None,
        };
        let is_focused = focused_index == Some(idx);
        render_node(
            pixmap,
            offset_rect,
            kind,
            style,
            font,
            clip_mask,
            text_value,
            is_focused,
            false,
            damage_rect,
        );
    }
}

struct ChromePaint {
    bounds: LayoutRect,
    tag: u64,
    underlay: u64,
}

fn chrome_bounds(rect: LayoutRect, style: &Style) -> LayoutRect {
    let mut left = rect.x;
    let mut top = rect.y;
    let mut right = rect.x + rect.width;
    let mut bottom = rect.y + rect.height;
    for shadow in style.box_shadow_layers() {
        if shadow.inset {
            continue;
        }
        let pad = shadow.blur_radius + shadow.spread_radius.abs();
        left = left.min(rect.x + shadow.offset_x - pad);
        top = top.min(rect.y + shadow.offset_y - pad);
        right = right.max(rect.x + rect.width + shadow.offset_x + pad);
        bottom = bottom.max(rect.y + rect.height + shadow.offset_y + pad);
    }
    LayoutRect {
        x: left,
        y: top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
    }
}

fn color_bits(color: Color) -> u32 {
    u32::from(color.r)
        | (u32::from(color.g) << 8)
        | (u32::from(color.b) << 16)
        | (u32::from(color.a) << 24)
}

fn chrome_style_tag(rect: LayoutRect, style: &Style) -> u64 {
    let mut shadows = 0u64;
    for shadow in style.box_shadow_layers() {
        shadows = shadows.wrapping_mul(0x100000001b3).wrapping_add(
            (shadow.offset_x.to_bits() as u64)
                ^ (shadow.offset_y.to_bits() as u64).rotate_left(11)
                ^ (shadow.blur_radius.to_bits() as u64).rotate_left(22)
                ^ (shadow.spread_radius.to_bits() as u64).rotate_left(33)
                ^ u64::from(color_bits(shadow.color))
                ^ u64::from(u8::from(shadow.inset)),
        );
    }
    crate::blur::chrome_tag(
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        style.border_radius,
        style.backdrop_blur.unwrap_or(0.0),
        color_bits(style.background),
        color_bits(style.border_color) ^ (style.border_width.to_bits()),
        shadows,
    )
}

/// `true` when a cached chrome sprite was written and decorations can be skipped.
fn begin_chrome(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    style: &Style,
    in_layer: bool,
    clip_mask: Option<&Mask>,
) -> (bool, Option<ChromePaint>) {
    if in_layer || clip_mask.is_some() || style.filter.is_some() {
        return (false, None);
    }
    let blurred = style.backdrop_blur.is_some_and(|sigma| sigma > 0.0);
    if !blurred && !style.has_box_shadow() {
        return (false, None);
    }
    let bounds = chrome_bounds(rect, style);
    let tag = chrome_style_tag(rect, style);
    let width = pixmap.width();
    let height = pixmap.height();
    let underlay = crate::blur::hash_pixmap_rect(pixmap.data(), width, height, bounds);
    if crate::blur::restore_chrome(pixmap.data_mut(), width, height, bounds, tag, underlay) {
        return (true, None);
    }
    (
        false,
        Some(ChromePaint {
            bounds,
            tag,
            underlay,
        }),
    )
}

#[allow(clippy::too_many_arguments)]
fn render_node(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    kind: &ComponentKind,
    style: &Style,
    font: &fontdue::Font,
    clip_mask: Option<&Mask>,
    text_input_value: Option<&str>,
    is_focused: bool,
    in_layer: bool,
    damage: Option<LayoutRect>,
) {
    if style.opacity <= 0.0 {
        return;
    }
    // Apply transform offset
    let tx = style.transform.translate_x;
    let ty = style.transform.translate_y;
    let rect = LayoutRect {
        x: rect.x + tx,
        y: rect.y + ty,
        width: rect.width * style.transform.scale_x,
        height: rect.height * style.transform.scale_y,
    };
    let rect = crate::paint_artifact::table_grid_paint_rect(style, rect);
    let decoration_rect = if style.display == w3cos_std::style::Display::Inline
        && matches!(kind, ComponentKind::Row | ComponentKind::Box)
        && style.position == w3cos_std::style::Position::Static
        && (style.background.a > 0
            || style.border_width > 0.0
            || style.border_top_width.is_some_and(|width| width > 0.0)
            || style.border_right_width.is_some_and(|width| width > 0.0)
            || style.border_bottom_width.is_some_and(|width| width > 0.0)
            || style.border_left_width.is_some_and(|width| width > 0.0))
    {
        let line_height = style.font_size * style.line_height
            + style.padding_lengths().top
            + style.padding_lengths().bottom
            + style.border_top_width.unwrap_or(style.border_width)
            + style.border_bottom_width.unwrap_or(style.border_width);
        LayoutRect {
            y: rect.y,
            height: rect.height.max(line_height),
            ..rect
        }
    } else {
        rect
    };

    // Box shadow (render before the element; shadows stay outside filtered layer)
    let css_filter = style.filter.as_deref().and_then(filter::parse_css_filter);
    let opacity = style.opacity;
    let color_chain = if in_layer { None } else { css_filter.as_ref() };
    let bg = node_color(style.background, opacity, color_chain);
    let (chrome_restored, chrome_paint) = begin_chrome(pixmap, rect, style, in_layer, clip_mask);
    if !chrome_restored {
        if !in_layer {
            if let Some(ref chain) = css_filter {
                if let Some(shadow) = chain.drop_shadow() {
                    draw_box_shadow(
                        pixmap,
                        rect,
                        shadow,
                        style.border_radius,
                        style.opacity,
                        clip_mask,
                        damage,
                    );
                }
            }
            let outer_shadows = style
                .box_shadow_layers()
                .filter(|shadow| !shadow.inset)
                .collect::<Vec<_>>();
            for shadow in outer_shadows.into_iter().rev() {
                draw_box_shadow(
                    pixmap,
                    rect,
                    shadow,
                    style.border_radius,
                    style.opacity,
                    clip_mask,
                    damage,
                );
            }
            if let Some(sigma) = style.backdrop_blur.filter(|sigma| *sigma > 0.0) {
                draw_backdrop_blur(pixmap, rect, style.border_radius, sigma, clip_mask, damage);
            }

            if css_filter.as_ref().is_some_and(|c| c.has_blur()) {
                if let Some(ref chain) = css_filter {
                    render_node_layer_blur(
                        pixmap,
                        rect,
                        kind,
                        style,
                        font,
                        clip_mask,
                        text_input_value,
                        is_focused,
                        chain,
                    );
                }
                return;
            }
        }

        let bg = node_color(style.background, opacity, color_chain);
        for background_rect in
            crate::paint_artifact::box_background_paint_rects(style, decoration_rect)
        {
            if bg.a > 0 {
                draw_rect(pixmap, background_rect, bg, style.border_radius, clip_mask);
            }
            for layer in crate::background_image::background_paint_layers_with_overrides(
                style,
                decoration_rect,
                crate::paint_artifact::box_background_positioning_rect(style, rect),
                Some(background_rect),
            )
            .into_iter()
            .rev()
            {
                match layer {
                    crate::background_image::BackgroundPaintLayer::Raster(layer) => {
                        if let Some(decoded) = crate::image_loader::get_or_load(&layer.source) {
                            for tile in layer.tiles {
                                let viewport = decoded.svg_viewport(tile.width, tile.height);
                                let decoded = viewport.as_ref().unwrap_or(&decoded);
                                draw_image_pixels(
                                    pixmap,
                                    tile,
                                    decoded.width,
                                    decoded.height,
                                    &decoded.data,
                                    opacity,
                                    clip_mask,
                                    Some(layer.clip),
                                    layer.blend_mode,
                                );
                            }
                        }
                    }
                    crate::background_image::BackgroundPaintLayer::Gradient(layer) => {
                        for tile in layer.geometry.tiles {
                            draw_gradient_pixels(
                                pixmap,
                                tile,
                                &layer.kind,
                                &layer.stops,
                                opacity,
                                clip_mask,
                                layer.geometry.clip,
                                layer.blend_mode,
                            );
                        }
                    }
                }
            }
        }

        let inset_shadows = style
            .box_shadow_layers()
            .filter(|shadow| shadow.inset)
            .collect::<Vec<_>>();
        for shadow in inset_shadows.into_iter().rev() {
            draw_box_shadow(
                pixmap,
                rect,
                shadow,
                style.border_radius,
                style.opacity,
                clip_mask,
                damage,
            );
        }

        let has_edge_border = style.border_top_width.is_some()
            || style.border_right_width.is_some()
            || style.border_bottom_width.is_some()
            || style.border_left_width.is_some()
            || style.border_top_color.is_some()
            || style.border_right_color.is_some()
            || style.border_bottom_color.is_some()
            || style.border_left_color.is_some()
            || (style.border_collapse && style.display == w3cos_std::style::Display::TableCell);
        let has_edge_border = has_edge_border
            || (0..4).any(|side| crate::border_paint::is_three_dimensional(style, side));
        if !has_edge_border && style.border_width > 0.0 && style.border_color.a > 0 {
            draw_border(
                pixmap,
                rect,
                node_color(style.border_color, opacity, color_chain),
                style.border_width,
                style.border_radius,
                clip_mask,
            );
        } else if has_edge_border {
            let widths = [
                style.border_top_width.unwrap_or(style.border_width),
                style.border_right_width.unwrap_or(style.border_width),
                style.border_bottom_width.unwrap_or(style.border_width),
                style.border_left_width.unwrap_or(style.border_width),
            ];
            let colors = [
                style.border_top_color.unwrap_or(style.border_color),
                style.border_right_color.unwrap_or(style.border_color),
                style.border_bottom_color.unwrap_or(style.border_color),
                style.border_left_color.unwrap_or(style.border_color),
            ];
            let edges =
                crate::paint_artifact::border_edge_paint_rects(style, decoration_rect, widths);
            if (0..4).any(|side| crate::border_paint::is_three_dimensional(style, side)) {
                let origin = (rect.x.floor(), rect.y.floor());
                let layer_width = (rect.x + rect.width).ceil() - origin.0;
                let layer_height = (rect.y + rect.height).ceil() - origin.1;
                let mut border_layer =
                    Pixmap::new(layer_width.max(1.0) as u32, layer_height.max(1.0) as u32);
                for layer in
                    crate::border_paint::three_dimensional_layers(style, rect, widths, colors)
                {
                    let mut builder = PathBuilder::new();
                    for points in layer.polygons {
                        builder.move_to(points[0].0 - origin.0, points[0].1 - origin.1);
                        for point in &points[1..] {
                            builder.line_to(point.0 - origin.0, point.1 - origin.1);
                        }
                        builder.close();
                    }
                    if let Some(path) = builder.finish() {
                        let mut color = node_color(layer.color, 1.0, color_chain);
                        if layer.shadow {
                            color.a = 255;
                        }
                        let mut paint = Paint::default();
                        paint.set_color(SkColor::from_rgba8(color.r, color.g, color.b, color.a));
                        paint.anti_alias = true;
                        if layer.shadow {
                            paint.blend_mode = tiny_skia::BlendMode::SourceAtop;
                        }
                        if let Some(border_layer) = border_layer.as_mut() {
                            border_layer.fill_path(
                                &path,
                                &paint,
                                FillRule::Winding,
                                Transform::identity(),
                                None,
                            );
                        }
                    }
                }
                if let Some(border_layer) = border_layer {
                    let paint = tiny_skia::PixmapPaint {
                        opacity,
                        ..Default::default()
                    };
                    pixmap.draw_pixmap(
                        origin.0 as i32,
                        origin.1 as i32,
                        border_layer.as_ref(),
                        &paint,
                        Transform::identity(),
                        clip_mask,
                    );
                }
            } else {
                for ((edge, width), color) in edges.into_iter().zip(widths).zip(colors) {
                    if width > 0.0 && color.a > 0 {
                        draw_rect(
                            pixmap,
                            edge,
                            node_color(color, opacity, color_chain),
                            0.0,
                            clip_mask,
                        );
                    }
                }
            }
        }

        if let Some(paint) = chrome_paint {
            let width = pixmap.width();
            let height = pixmap.height();
            crate::blur::store_chrome(
                pixmap.data(),
                width,
                height,
                paint.bounds,
                paint.tag,
                paint.underlay,
            );
        }
    }

    let text_color = node_color(style.color, opacity, color_chain);

    match kind {
        ComponentKind::Text { content } => {
            draw_text_in_rect(pixmap, rect, content, style, text_color, font, clip_mask);
        }
        ComponentKind::Button { label } => {
            if text_color.a > 0 {
                draw_text_centered_in_rect(pixmap, rect, label, style, text_color, font, clip_mask);
            }
        }
        ComponentKind::Image { src } => {
            let content_rect = text_content_box(rect, style);
            if let Some(decoded) = crate::image_loader::get_or_load(src) {
                let viewport = decoded.svg_viewport(content_rect.width, content_rect.height);
                let decoded = viewport.as_ref().unwrap_or(&decoded);
                draw_image_pixels(
                    pixmap,
                    content_rect,
                    decoded.width,
                    decoded.height,
                    &decoded.data,
                    opacity,
                    clip_mask,
                    None,
                    crate::background_image::BackgroundBlendMode::Normal,
                );
            } else {
                let placeholder_bg = if bg.a == 0 {
                    apply_opacity(Color::rgb(40, 40, 50), opacity)
                } else {
                    bg
                };
                draw_rect(
                    pixmap,
                    content_rect,
                    placeholder_bg,
                    style.border_radius,
                    clip_mask,
                );
                let border_color = if style.border_width > 0.0 && style.border_color.a > 0 {
                    apply_opacity(style.border_color, opacity)
                } else {
                    apply_opacity(Color::rgb(100, 100, 120), opacity)
                };
                draw_border(
                    pixmap,
                    rect,
                    border_color,
                    style.border_width.max(1.0),
                    style.border_radius,
                    clip_mask,
                );
                let label = format!("[Image: {}]", src);
                draw_text_line(
                    pixmap,
                    rect.x + 8.0,
                    rect.y + 8.0,
                    &label,
                    style.font_size,
                    text_color,
                    font,
                    style,
                    clip_mask,
                );
            }
        }
        ComponentKind::TextInput {
            value,
            placeholder,
            secure,
        } => {
            let display_value = text_input_value.unwrap_or(value.as_str());
            let masked_value = secure.then(|| "•".repeat(display_value.chars().count()));
            let (display_text, text_color_override) = if display_value.is_empty() {
                (
                    placeholder.as_str(),
                    Some(apply_opacity(Color::rgb(107, 114, 128), opacity)),
                )
            } else if let Some(masked) = masked_value.as_deref() {
                (masked, None)
            } else {
                (display_value, None)
            };
            let content = text_content_box(rect, style);
            let text_x = content.x;
            let ink = crate::font_face::FontRegistry::global().measure_style_ink_bounds(
                style,
                display_text,
                style.font_size,
                font,
            );
            let text_y = content.y + (content.height - ink.height) * 0.5 - ink.top;
            let tc = text_color_override.unwrap_or(text_color);
            draw_text_line(
                pixmap,
                text_x,
                text_y,
                display_text,
                style.font_size,
                tc,
                font,
                style,
                clip_mask,
            );
            if !secure && !display_value.is_empty() {
                draw_ime_preedit_underline(
                    pixmap,
                    text_x,
                    text_y + ink.height.max(style.font_size * 0.85) + 1.0,
                    display_text,
                    style,
                    font,
                    tc,
                    clip_mask,
                );
            }
            if is_focused {
                draw_blinking_cursor(
                    pixmap,
                    content,
                    display_value,
                    style.font_size,
                    text_color,
                    font,
                    style,
                    clip_mask,
                );
            }
        }
        ComponentKind::SvgPath {
            commands,
            fill,
            stroke,
            stroke_width,
        } => draw_svg_path(
            pixmap,
            rect,
            commands,
            node_color(*fill, opacity, color_chain),
            stroke.map(|color| node_color(color, opacity, color_chain)),
            *stroke_width,
            clip_mask,
        ),
        ComponentKind::SvgDocument { source, .. } => {
            let rect = crate::svg_renderer::content_box(rect, style);
            if let Some(raster) = crate::svg_renderer::get_or_render_inline(
                source,
                rect.width.ceil().max(1.0) as u32,
                rect.height.ceil().max(1.0) as u32,
            ) {
                draw_image_pixels(
                    pixmap,
                    rect,
                    raster.width,
                    raster.height,
                    raster.data.as_slice(),
                    opacity,
                    clip_mask,
                    None,
                    crate::background_image::BackgroundBlendMode::Normal,
                );
            }
        }
        _ => {}
    }
}

fn draw_svg_path(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    commands: &[SvgPathCommand],
    fill: Color,
    stroke: Option<Color>,
    stroke_width: f32,
    clip_mask: Option<&Mask>,
) {
    let mut builder = PathBuilder::new();
    for command in commands {
        match *command {
            SvgPathCommand::MoveTo(x, y) => builder.move_to(rect.x + x, rect.y + y),
            SvgPathCommand::LineTo(x, y) => builder.line_to(rect.x + x, rect.y + y),
            SvgPathCommand::QuadTo(cx, cy, x, y) => {
                builder.quad_to(rect.x + cx, rect.y + cy, rect.x + x, rect.y + y)
            }
            SvgPathCommand::CubicTo(c1x, c1y, c2x, c2y, x, y) => builder.cubic_to(
                rect.x + c1x,
                rect.y + c1y,
                rect.x + c2x,
                rect.y + c2y,
                rect.x + x,
                rect.y + y,
            ),
            SvgPathCommand::Close => builder.close(),
        }
    }
    let Some(path) = builder.finish() else {
        return;
    };
    if fill.a > 0 {
        let mut paint = Paint::default();
        paint.set_color(SkColor::from_rgba8(fill.r, fill.g, fill.b, fill.a));
        paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            clip_mask,
        );
    }
    if let Some(stroke) = stroke.filter(|color| color.a > 0)
        && stroke_width > 0.0
    {
        let mut paint = Paint::default();
        paint.set_color(SkColor::from_rgba8(stroke.r, stroke.g, stroke.b, stroke.a));
        paint.anti_alias = true;
        pixmap.stroke_path(
            &path,
            &paint,
            &Stroke {
                width: stroke_width,
                ..Stroke::default()
            },
            Transform::identity(),
            clip_mask,
        );
    }
}

fn apply_opacity(c: Color, opacity: f32) -> Color {
    Color::rgba(c.r, c.g, c.b, (c.a as f32 * opacity) as u8)
}

fn node_color(c: Color, opacity: f32, chain: Option<&CssFilter>) -> Color {
    let c = chain
        .map(|f| filter::apply_filter_to_color(c, f))
        .unwrap_or(c);
    apply_opacity(c, opacity)
}

#[allow(clippy::too_many_arguments)]
fn render_node_layer_blur(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    kind: &ComponentKind,
    style: &Style,
    font: &fontdue::Font,
    clip_mask: Option<&Mask>,
    text_input_value: Option<&str>,
    is_focused: bool,
    chain: &CssFilter,
) {
    let pad = chain.max_blur_px().ceil() as u32 + 2;
    let lw = (rect.width as u32 + pad * 2).max(1);
    let lh = (rect.height as u32 + pad * 2).max(1);
    let Some(mut layer) = Pixmap::new(lw, lh) else {
        return;
    };
    layer.fill(SkColor::TRANSPARENT);

    let inner = LayoutRect {
        x: pad as f32,
        y: pad as f32,
        width: rect.width,
        height: rect.height,
    };
    render_node(
        &mut layer,
        inner,
        kind,
        style,
        font,
        None,
        text_input_value,
        is_focused,
        true,
        None,
    );
    filter::apply_chain_to_rgba(layer.data_mut(), lw, lh, chain);

    let paint = tiny_skia::PixmapPaint::default();
    pixmap.draw_pixmap(
        (rect.x - pad as f32) as i32,
        (rect.y - pad as f32) as i32,
        layer.as_ref(),
        &paint,
        Transform::identity(),
        clip_mask,
    );
}

fn make_clip_mask(pixmap: &Pixmap, rect: LayoutRect) -> Option<Mask> {
    let mut mask = Mask::new(pixmap.width(), pixmap.height())?;
    let path = rounded_rect_path(rect.x, rect.y, rect.width, rect.height, 0.0)?;
    mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
    Some(mask)
}

fn resolved_color(c: Color, opacity: f32, filter: Option<&CssFilter>) -> Color {
    node_color(c, opacity, filter)
}

fn draw_backdrop_blur(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    radius: f32,
    sigma: f32,
    clip_mask: Option<&Mask>,
    damage: Option<LayoutRect>,
) {
    let sigma = sigma.min(crate::blur::MAX_SIGMA);
    if damage.is_some_and(|damage| !rects_intersect(rect, damage)) {
        return;
    }
    let pad = crate::blur::kernel_radius(sigma);
    if pad == 0 {
        return;
    }
    let src_w = pixmap.width() as i32;
    let src_h = pixmap.height() as i32;
    let x0 = (rect.x.floor() as i32 - pad).clamp(0, src_w);
    let y0 = (rect.y.floor() as i32 - pad).clamp(0, src_h);
    let x1 = ((rect.x + rect.width).ceil() as i32 + pad).clamp(0, src_w);
    let y1 = ((rect.y + rect.height).ceil() as i32 + pad).clamp(0, src_h);
    let width = (x1 - x0) as u32;
    let height = (y1 - y0) as u32;
    if width == 0 || height == 0 {
        return;
    }
    let mut region = vec![0_u8; (width as usize) * (height as usize) * 4];
    {
        let data = pixmap.data();
        let row_bytes = src_w as usize * 4;
        for row in 0..height as usize {
            let src = (y0 as usize + row) * row_bytes + x0 as usize * 4;
            let dst = row * width as usize * 4;
            region[dst..dst + width as usize * 4]
                .copy_from_slice(&data[src..src + width as usize * 4]);
        }
    }
    crate::blur::blur_premultiplied_cached(
        &mut region,
        width,
        height,
        sigma,
        crate::blur::BlurEdge::Clamp,
    );
    let row_bytes = pixmap.width() as usize * 4;
    let data = pixmap.data_mut();
    for row in 0..height {
        for col in 0..width {
            let px = x0 + col as i32;
            let py = y0 + row as i32;
            if damage.is_some_and(|damage| {
                (px as f32) < damage.x
                    || (py as f32) < damage.y
                    || (px as f32) >= damage.x + damage.width
                    || (py as f32) >= damage.y + damage.height
            }) {
                continue;
            }
            let cover =
                crate::blur::round_rect_coverage(px as f32 + 0.5, py as f32 + 0.5, rect, radius);
            if cover <= 0.0 {
                continue;
            }
            if let Some(mask) = clip_mask {
                let index = (py as u32 * mask.width() + px as u32) as usize;
                if mask.data().get(index).copied().unwrap_or(0) == 0 {
                    continue;
                }
            }
            let src = ((row * width + col) * 4) as usize;
            let dst = (py as usize) * row_bytes + px as usize * 4;
            if cover >= 0.99 {
                data[dst..dst + 4].copy_from_slice(&region[src..src + 4]);
            } else {
                for channel in 0..4 {
                    let blurred = f32::from(region[src + channel]);
                    let original = f32::from(data[dst + channel]);
                    data[dst + channel] =
                        (blurred * cover + original * (1.0 - cover)).round() as u8;
                }
            }
        }
    }
}

fn sprite_damage_span(
    sprite: &crate::blur::PremulSprite,
    damage: Option<LayoutRect>,
) -> (u32, u32, u32, u32) {
    let Some(damage) = damage else {
        return (0, sprite.width, 0, sprite.height);
    };
    let x0 = damage.x.floor() as i32;
    let y0 = damage.y.floor() as i32;
    let x1 = (damage.x + damage.width).ceil() as i32;
    let y1 = (damage.y + damage.height).ceil() as i32;
    let origin_x = sprite.origin_x.floor() as i32;
    let origin_y = sprite.origin_y.floor() as i32;
    let col0 = (x0 - origin_x).clamp(0, sprite.width as i32) as u32;
    let row0 = (y0 - origin_y).clamp(0, sprite.height as i32) as u32;
    let col1 = (x1 - origin_x).clamp(0, sprite.width as i32) as u32;
    let row1 = (y1 - origin_y).clamp(0, sprite.height as i32) as u32;
    (col0, col1, row0, row1)
}

fn blit_premul_sprite(
    pixmap: &mut Pixmap,
    sprite: &crate::blur::PremulSprite,
    opacity: f32,
    clip_mask: Option<&Mask>,
    damage: Option<LayoutRect>,
) {
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 0.0 {
        return;
    }
    let dst_w = pixmap.width() as i32;
    let dst_h = pixmap.height() as i32;
    let origin_x = sprite.origin_x as i32;
    let origin_y = sprite.origin_y as i32;
    let (col_start, col_end, row_start, row_end) = sprite_damage_span(sprite, damage);
    let data = pixmap.data_mut();
    let row_bytes = dst_w as usize * 4;
    for row in row_start..row_end {
        for col in col_start..col_end {
            let px = origin_x + col as i32;
            let py = origin_y + row as i32;
            if px < 0 || py < 0 || px >= dst_w || py >= dst_h {
                continue;
            }
            if let Some(mask) = clip_mask {
                let index = (py as u32 * mask.width() + px as u32) as usize;
                if mask.data().get(index).copied().unwrap_or(0) == 0 {
                    continue;
                }
            }
            let src = ((row * sprite.width + col) * 4) as usize;
            let src_a = sprite.pixels[src + 3];
            if src_a == 0 {
                continue;
            }
            let sa = (f32::from(src_a) * opacity).round() as u8;
            if sa == 0 {
                continue;
            }
            let scale = u16::from(sa);
            let base = u16::from(src_a).max(1);
            let premul = [
                (u16::from(sprite.pixels[src]) * scale / base) as u8,
                (u16::from(sprite.pixels[src + 1]) * scale / base) as u8,
                (u16::from(sprite.pixels[src + 2]) * scale / base) as u8,
                sa,
            ];
            let dst = py as usize * row_bytes + px as usize * 4;
            let inv = 255 - u16::from(sa);
            for channel in 0..4 {
                let mixed = u16::from(premul[channel]) + u16::from(data[dst + channel]) * inv / 255;
                data[dst + channel] = mixed.min(255) as u8;
            }
        }
    }
}

fn draw_box_shadow(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    shadow: &w3cos_std::style::BoxShadow,
    radius: f32,
    opacity: f32,
    clip_mask: Option<&Mask>,
    damage: Option<LayoutRect>,
) {
    if let Some(damage) = damage {
        let pad = shadow.blur_radius + shadow.spread_radius.abs() + 2.0;
        let bounds = LayoutRect {
            x: rect.x + shadow.offset_x - pad,
            y: rect.y + shadow.offset_y - pad,
            width: rect.width + pad * 2.0,
            height: rect.height + pad * 2.0,
        };
        if !rects_intersect(bounds, damage) && !shadow.inset {
            return;
        }
        if shadow.inset && !rects_intersect(rect, damage) {
            return;
        }
    }
    if let Some(sprite) = crate::blur::raster_box_shadow(rect, radius, shadow) {
        blit_premul_sprite(pixmap, &sprite, opacity, clip_mask, damage);
        return;
    }
    if shadow.inset {
        draw_inset_box_shadow(pixmap, rect, shadow, radius, opacity, clip_mask);
        return;
    }
    let spread = shadow.spread_radius;
    let shadow_rect = LayoutRect {
        x: rect.x + shadow.offset_x - spread,
        y: rect.y + shadow.offset_y - spread,
        width: rect.width + spread * 2.0,
        height: rect.height + spread * 2.0,
    };
    let color = apply_opacity(shadow.color, opacity);

    // Approximate blur by drawing multiple expanding rectangles with decreasing alpha
    let steps = (shadow.blur_radius / 2.0).max(1.0) as u32;
    for i in 0..steps {
        let t = i as f32 / steps as f32;
        let expand = shadow.blur_radius * t;
        let alpha = ((1.0 - t) * color.a as f32 / steps as f32) as u8;
        if alpha == 0 {
            continue;
        }
        let c = Color::rgba(color.r, color.g, color.b, alpha);
        let r = LayoutRect {
            x: shadow_rect.x - expand,
            y: shadow_rect.y - expand,
            width: shadow_rect.width + expand * 2.0,
            height: shadow_rect.height + expand * 2.0,
        };
        draw_rect(pixmap, r, c, radius + expand, clip_mask);
    }
}

fn draw_inset_box_shadow(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    shadow: &w3cos_std::style::BoxShadow,
    radius: f32,
    opacity: f32,
    clip_mask: Option<&Mask>,
) {
    let Some(mut mask) = Mask::new(pixmap.width(), pixmap.height()) else {
        return;
    };
    let Some(outer) = rounded_rect_path(rect.x, rect.y, rect.width, rect.height, radius) else {
        return;
    };
    mask.fill_path(&outer, FillRule::Winding, true, Transform::identity());
    if let Some(parent) = clip_mask {
        for (pixel, parent_pixel) in mask.data_mut().iter_mut().zip(parent.data().iter()) {
            *pixel = ((*pixel as u16 * *parent_pixel as u16) / 255) as u8;
        }
    }
    let color = apply_opacity(shadow.color, opacity);
    let steps = ((shadow.blur_radius / 2.0).ceil() as u32).clamp(1, 8);
    for step in 0..steps {
        let t = step as f32 / steps as f32;
        let expand = shadow.blur_radius * t;
        let alpha = ((1.0 - t) * color.a as f32 / steps as f32) as u8;
        if alpha == 0 {
            continue;
        }
        let shrink = shadow.spread_radius + expand;
        let inner = LayoutRect {
            x: rect.x + shadow.offset_x + shrink,
            y: rect.y + shadow.offset_y + shrink,
            width: (rect.width - shrink * 2.0).max(0.0),
            height: (rect.height - shrink * 2.0).max(0.0),
        };
        let mut builder = PathBuilder::new();
        push_round_rect(
            &mut builder,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            radius,
        );
        if inner.width > 0.5 && inner.height > 0.5 {
            push_round_rect(
                &mut builder,
                inner.x,
                inner.y,
                inner.width,
                inner.height,
                (radius - shrink).max(0.0),
            );
        }
        let Some(path) = builder.finish() else {
            continue;
        };
        let mut paint = Paint::default();
        paint.set_color(SkColor::from_rgba8(color.r, color.g, color.b, alpha));
        paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::EvenOdd,
            Transform::identity(),
            Some(&mask),
        );
    }
}

fn push_round_rect(builder: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32, radius: f32) {
    let radius = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    builder.move_to(x + radius, y);
    builder.line_to(x + w - radius, y);
    builder.quad_to(x + w, y, x + w, y + radius);
    builder.line_to(x + w, y + h - radius);
    builder.quad_to(x + w, y + h, x + w - radius, y + h);
    builder.line_to(x + radius, y + h);
    builder.quad_to(x, y + h, x, y + h - radius);
    builder.line_to(x, y + radius);
    builder.quad_to(x, y, x + radius, y);
    builder.close();
}

fn draw_ime_preedit_underline(
    pixmap: &mut Pixmap,
    text_x: f32,
    underline_y: f32,
    display_text: &str,
    style: &Style,
    font: &fontdue::Font,
    color: Color,
    clip_mask: Option<&Mask>,
) {
    let Some(suffix) = style
        .custom_properties
        .as_ref()
        .and_then(|props| props.get("--w3cos-ime-preedit"))
        .map(String::as_str)
        .filter(|suffix| !suffix.is_empty())
    else {
        return;
    };
    let Some((prefix, suffix)) = text_layout::split_preedit_suffix(display_text, suffix) else {
        return;
    };
    let registry = crate::font_face::FontRegistry::global();
    let prefix_w: f32 = prefix
        .chars()
        .map(|ch| registry.style_char_advance(style, ch, style.font_size, font))
        .sum();
    let suffix_w: f32 = suffix
        .chars()
        .map(|ch| registry.style_char_advance(style, ch, style.font_size, font))
        .sum();
    if suffix_w <= 0.0 {
        return;
    }
    draw_rect(
        pixmap,
        crate::layout::LayoutRect {
            x: text_x + prefix_w,
            y: underline_y,
            width: suffix_w,
            height: 1.5,
        },
        color,
        0.0,
        clip_mask,
    );
}

fn draw_rect(
    pixmap: &mut Pixmap,
    r: LayoutRect,
    color: Color,
    radius: f32,
    clip_mask: Option<&Mask>,
) {
    let mut paint = Paint::default();
    paint.set_color(SkColor::from_rgba8(color.r, color.g, color.b, color.a));
    paint.anti_alias = true;

    if let Some(sk_rect) = Rect::from_xywh(r.x, r.y, r.width, r.height) {
        if radius > 0.0 {
            if let Some(path) = rounded_rect_path(r.x, r.y, r.width, r.height, radius) {
                pixmap.fill_path(
                    &path,
                    &paint,
                    FillRule::Winding,
                    Transform::identity(),
                    clip_mask,
                );
            }
        } else {
            pixmap.fill_rect(sk_rect, &paint, Transform::identity(), clip_mask);
        }
    }
}

fn draw_border(
    pixmap: &mut Pixmap,
    r: LayoutRect,
    color: Color,
    width: f32,
    radius: f32,
    clip_mask: Option<&Mask>,
) {
    if width <= 0.0 {
        return;
    }
    let mut paint = Paint::default();
    paint.set_color(SkColor::from_rgba8(color.r, color.g, color.b, color.a));
    paint.anti_alias = true;

    if radius > 0.5 {
        let inset = width * 0.5;
        let inner = LayoutRect {
            x: r.x + inset,
            y: r.y + inset,
            width: (r.width - width).max(0.0),
            height: (r.height - width).max(0.0),
        };
        if let Some(path) = rounded_rect_path(inner.x, inner.y, inner.width, inner.height, radius) {
            let stroke = Stroke {
                width,
                ..Stroke::default()
            };
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), clip_mask);
        }
        return;
    }

    let rects = [
        Rect::from_xywh(r.x, r.y, r.width, width),
        Rect::from_xywh(r.x, r.y + r.height - width, r.width, width),
        Rect::from_xywh(r.x, r.y, width, r.height),
        Rect::from_xywh(r.x + r.width - width, r.y, width, r.height),
    ];
    for rect in rects.into_iter().flatten() {
        pixmap.fill_rect(rect, &paint, Transform::identity(), clip_mask);
    }
}

#[allow(clippy::too_many_arguments)]
fn measure_text_width(text: &str, font_size: f32, font: &fontdue::Font) -> f32 {
    text_layout::measure_text_width_font(text, font_size, font)
}

fn text_content_box(rect: LayoutRect, style: &Style) -> LayoutRect {
    let border = style.border_width;
    let (left, right) = crate::paint_artifact::paint_inline_border_widths(style);
    let pad = style.padding_lengths();
    LayoutRect {
        x: rect.x + pad.left + left,
        y: rect.y + pad.top + border,
        width: (rect.width - pad.left - pad.right - left - right).max(1.0),
        height: (rect.height - pad.top - pad.bottom - border * 2.0).max(0.0),
    }
}

fn text_paint_box(rect: LayoutRect, style: &Style) -> LayoutRect {
    text_content_box(rect, style)
}

fn draw_text_ink_in_box(
    pixmap: &mut Pixmap,
    box_rect: LayoutRect,
    text: &str,
    font_size: f32,
    color: Color,
    font: &fontdue::Font,
    style: &Style,
    align: TextAlign,
    clip_mask: Option<&Mask>,
) {
    let ink = crate::font_face::FontRegistry::global()
        .measure_style_ink_bounds(style, text, font_size, font);
    if ink.width <= 0.0 && ink.height <= 0.0 {
        draw_text_line(
            pixmap, box_rect.x, box_rect.y, text, font_size, color, font, style, clip_mask,
        );
        return;
    }

    let alignment_ink_left = if text.chars().next().is_some_and(char::is_whitespace) {
        crate::font_face::FontRegistry::global()
            .measure_style_ink_bounds(
                style,
                text.trim_start_matches(char::is_whitespace),
                font_size,
                font,
            )
            .left
    } else {
        ink.left
    };
    let x = match align {
        TextAlign::Right => box_rect.x + box_rect.width - ink.width - alignment_ink_left,
        TextAlign::Center => box_rect.x + (box_rect.width - ink.width) * 0.5 - alignment_ink_left,
        TextAlign::Left | TextAlign::Justify | TextAlign::Start | TextAlign::End => {
            box_rect.x - alignment_ink_left
        }
    };
    let y = box_rect.y + (box_rect.height - ink.height) * 0.5 - ink.top;
    draw_text_line(pixmap, x, y, text, font_size, color, font, style, clip_mask);
}

fn single_line_h_align(style: &Style, box_w: f32, ink_w: f32) -> TextAlign {
    if style.display == w3cos_std::style::Display::Inline {
        return TextAlign::Left;
    }
    let text_align = match (style.text_align, style.direction) {
        (TextAlign::Start, w3cos_std::style::TextDirection::Ltr)
        | (TextAlign::End, w3cos_std::style::TextDirection::Rtl) => TextAlign::Left,
        (TextAlign::Start, w3cos_std::style::TextDirection::Rtl)
        | (TextAlign::End, w3cos_std::style::TextDirection::Ltr) => TextAlign::Right,
        (align, _) => align,
    };
    match text_align {
        TextAlign::Center | TextAlign::Right => style.text_align,
        TextAlign::Left
            if matches!(
                style.white_space,
                w3cos_std::style::WhiteSpace::NoWrap | w3cos_std::style::WhiteSpace::Pre
            ) && box_w > ink_w + 1.5 =>
        {
            TextAlign::Center
        }
        TextAlign::Left | TextAlign::Justify | TextAlign::Start | TextAlign::End => TextAlign::Left,
    }
}

fn text_align_last(style: &Style) -> Option<TextAlign> {
    let value = style
        .custom_properties
        .as_ref()?
        .get("--w3cos-internal-text-align-last")?;
    Some(match value.as_str() {
        "left" => TextAlign::Left,
        "right" => TextAlign::Right,
        "center" => TextAlign::Center,
        "start" if style.direction == w3cos_std::style::TextDirection::Rtl => TextAlign::Right,
        "end" if style.direction == w3cos_std::style::TextDirection::Ltr => TextAlign::Right,
        "auto" => single_line_h_align(style, 0.0, 0.0),
        _ => TextAlign::Left,
    })
}

fn draw_text_in_rect(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    text: &str,
    style: &Style,
    color: Color,
    font: &fontdue::Font,
    clip_mask: Option<&Mask>,
) {
    let content = text_paint_box(rect, style);
    let continuation_content = text_layout::inline_text_continuation_box(content, style);
    let indent =
        style.resolved_text_indent(content.width, pixmap.width() as f32, pixmap.height() as f32);
    let first_line_content = match style.direction {
        w3cos_std::style::TextDirection::Ltr => LayoutRect {
            x: content.x + indent,
            width: (content.width - indent).max(1.0),
            ..content
        },
        w3cos_std::style::TextDirection::Rtl => LayoutRect {
            width: (content.width - indent).max(1.0),
            ..content
        },
    };
    let clips_own_overflow = matches!(
        style.resolved_overflow_x(),
        w3cos_std::style::Overflow::Hidden
            | w3cos_std::style::Overflow::Scroll
            | w3cos_std::style::Overflow::Auto
    ) || matches!(
        style.resolved_overflow_y(),
        w3cos_std::style::Overflow::Hidden
            | w3cos_std::style::Overflow::Scroll
            | w3cos_std::style::Overflow::Auto
    );
    let mut own_clip_mask = clips_own_overflow
        .then(|| make_clip_mask(pixmap, rect))
        .flatten();
    if let (Some(own), Some(ancestor)) = (own_clip_mask.as_mut(), clip_mask) {
        for (own_alpha, ancestor_alpha) in own.data_mut().iter_mut().zip(ancestor.data()) {
            *own_alpha = (*own_alpha).min(*ancestor_alpha);
        }
    }
    let effective_clip = own_clip_mask.as_ref().or(clip_mask);
    let line_h = style.font_size * style.line_height;
    let registry = crate::font_face::FontRegistry::global();
    let layout = text_layout::retained_text_paint_layout_with_first_line(
        text,
        continuation_content.width,
        first_line_content.width,
        style.font_size,
        style.white_space,
        registry.cascade_cache_key(style, text) ^ 0x4350_5554_4558_5401,
        |character| registry.style_char_advance(style, character, style.font_size, font),
        |line| registry.measure_style_ink_bounds(style, line, style.font_size, font),
    );
    let lines = &layout.lines;

    if lines.len() == 1 {
        let align = text_align_last(style).unwrap_or_else(|| {
            single_line_h_align(style, first_line_content.width, layout.ink_bounds[0].width)
        });
        draw_text_ink_in_box(
            pixmap,
            first_line_content,
            &lines[0],
            style.font_size,
            color,
            font,
            style,
            align,
            effective_clip,
        );
        return;
    }

    let block_h = lines.len() as f32 * line_h;
    let block_top = content.y + (content.height - block_h).max(0.0) * 0.5;

    for (i, line) in lines.iter().enumerate() {
        let ink = layout.ink_bounds[i];
        let alignment_ink_left = if line.chars().next().is_some_and(char::is_whitespace) {
            registry
                .measure_style_ink_bounds(
                    style,
                    line.trim_start_matches(char::is_whitespace),
                    style.font_size,
                    font,
                )
                .left
        } else {
            ink.left
        };
        let line_content = if i == 0 {
            first_line_content
        } else {
            continuation_content
        };
        let align = if i + 1 == lines.len() {
            text_align_last(style)
                .unwrap_or_else(|| single_line_h_align(style, line_content.width, ink.width))
        } else {
            single_line_h_align(style, line_content.width, ink.width)
        };
        let x = match align {
            TextAlign::Right => {
                line_content.x + line_content.width - ink.width - alignment_ink_left
            }
            TextAlign::Center => {
                line_content.x + (line_content.width - ink.width) * 0.5 - alignment_ink_left
            }
            TextAlign::Left | TextAlign::Justify | TextAlign::Start | TextAlign::End => {
                line_content.x - alignment_ink_left
            }
        };
        let y = block_top + i as f32 * line_h;
        draw_text_line(
            pixmap,
            x,
            y,
            line,
            style.font_size,
            color,
            font,
            style,
            effective_clip,
        );
    }
}

fn draw_text_centered_in_rect(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    text: &str,
    style: &Style,
    color: Color,
    font: &fontdue::Font,
    clip_mask: Option<&Mask>,
) {
    let clips_own_overflow = matches!(
        style.resolved_overflow_x(),
        w3cos_std::style::Overflow::Hidden
            | w3cos_std::style::Overflow::Scroll
            | w3cos_std::style::Overflow::Auto
    ) || matches!(
        style.resolved_overflow_y(),
        w3cos_std::style::Overflow::Hidden
            | w3cos_std::style::Overflow::Scroll
            | w3cos_std::style::Overflow::Auto
    );
    let mut own_clip_mask = clips_own_overflow
        .then(|| make_clip_mask(pixmap, rect))
        .flatten();
    if let (Some(own), Some(ancestor)) = (own_clip_mask.as_mut(), clip_mask) {
        for (own_alpha, ancestor_alpha) in own.data_mut().iter_mut().zip(ancestor.data()) {
            *own_alpha = (*own_alpha).min(*ancestor_alpha);
        }
    }
    let effective_clip = own_clip_mask.as_ref().or(clip_mask);
    draw_text_ink_in_box(
        pixmap,
        text_paint_box(rect, style),
        text,
        style.font_size,
        color,
        font,
        style,
        TextAlign::Center,
        effective_clip,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_text_line(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    text: &str,
    font_size: f32,
    color: Color,
    font: &fontdue::Font,
    style: &Style,
    clip_mask: Option<&Mask>,
) {
    let mut cursor_x = x;
    let cursor_y = y + font_size;

    let px_w = pixmap.width() as i32;
    let px_h = pixmap.height() as i32;

    let in_clip = |px: i32, py: i32| -> bool {
        if let Some(mask) = clip_mask {
            if px < 0 || py < 0 || px >= mask.width() as i32 || py >= mask.height() as i32 {
                return false;
            }
            let idx = (py * mask.width() as i32 + px) as usize;
            mask.data().get(idx).copied().unwrap_or(0) > 0
        } else {
            true
        }
    };

    if !style.text_shadows.is_empty() {
        paint_text_shadows(pixmap, x, y, text, font_size, font, style, clip_mask);
    }

    GLYPH_RASTER_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let render_text = text_layout::font_render_text_for_style(text, style);
        for ch in render_text.chars() {
            let registered = crate::font_face::FontRegistry::global()
                .resolve_style_for_character(style, ch)
                .and_then(|font| font.parsed());
            let system = if registered
                .as_ref()
                .is_some_and(|font| font.chars().contains_key(&ch))
                || font.chars().contains_key(&ch)
            {
                None
            } else {
                crate::font_face::parsed_fallback(ch)
            };
            let glyph_font = registered.as_deref().or(system.as_deref()).unwrap_or(font);
            let glyph = cache.get_or_rasterize(glyph_font, ch, font_size);
            let metrics = &glyph.metrics;
            let advance = if metrics.advance_width > 0.0 {
                metrics.advance_width
            } else {
                text_layout::estimated_char_width(ch, font_size)
            };
            let advance = text_layout::snapped_glyph_advance(ch, advance, font_size);
            if metrics.width == 0 || metrics.height == 0 {
                cursor_x += advance;
                continue;
            }

            let (gx, gy) = text_layout::glyph_pixel_origin(cursor_x, cursor_y, metrics);

            paint_glyph_bitmap(
                pixmap,
                gx,
                gy,
                metrics,
                &glyph.bitmap,
                color,
                px_w,
                px_h,
                &in_clip,
            );
            // The bundled face is Regular. A small overstrike stands in for
            // the missing bold/semibold outlines.
            if style.font_weight >= 600 {
                paint_glyph_bitmap(
                    pixmap,
                    gx + 1,
                    gy,
                    metrics,
                    &glyph.bitmap,
                    color,
                    px_w,
                    px_h,
                    &in_clip,
                );
            }

            cursor_x += advance;
        }
    });
}

fn paint_glyph_bitmap(
    pixmap: &mut Pixmap,
    gx: i32,
    gy: i32,
    metrics: &fontdue::Metrics,
    bitmap: &[u8],
    color: Color,
    px_w: i32,
    px_h: i32,
    in_clip: &dyn Fn(i32, i32) -> bool,
) {
    let pixels = pixmap.pixels_mut();
    for row in 0..metrics.height {
        for col in 0..metrics.width {
            let px = gx + col as i32;
            let py = gy + row as i32;
            if px < 0 || py < 0 || px >= px_w || py >= px_h {
                continue;
            }
            if !in_clip(px, py) {
                continue;
            }
            let alpha = bitmap[row * metrics.width + col];
            if alpha == 0 {
                continue;
            }
            let alpha = coverage_lut()[alpha as usize];
            let idx = (py * px_w + px) as usize;
            let a = (alpha as u16 * color.a as u16 / 255) as u8;
            let dst = pixels[idx];
            let blended = blend_pixel(dst, color.r, color.g, color.b, a);
            pixels[idx] = blended;
        }
    }
}

fn coverage_lut() -> &'static [u8; 256] {
    static LUT: std::sync::OnceLock<[u8; 256]> = std::sync::OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0_u8; 256];
        for (index, slot) in lut.iter_mut().enumerate() {
            let coverage = (index as f32 / 255.0).powf(0.85);
            *slot = (coverage * 255.0).round() as u8;
        }
        lut
    })
}

fn paint_text_shadows(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    text: &str,
    font_size: f32,
    font: &fontdue::Font,
    style: &Style,
    clip_mask: Option<&Mask>,
) {
    let render_text = text_layout::font_render_text_for_style(text, style);
    if render_text.is_empty() {
        return;
    }
    GLYPH_RASTER_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let mut cursor_x = x;
        let cursor_y = y + font_size;
        let mut stamps = Vec::new();
        for ch in render_text.chars() {
            let registered = crate::font_face::FontRegistry::global()
                .resolve_style_for_character(style, ch)
                .and_then(|font| font.parsed());
            let system = if registered
                .as_ref()
                .is_some_and(|font| font.chars().contains_key(&ch))
                || font.chars().contains_key(&ch)
            {
                None
            } else {
                crate::font_face::parsed_fallback(ch)
            };
            let glyph_font = registered.as_deref().or(system.as_deref()).unwrap_or(font);
            let glyph = cache.get_or_rasterize(glyph_font, ch, font_size);
            let metrics = &glyph.metrics;
            let advance = if metrics.advance_width > 0.0 {
                metrics.advance_width
            } else {
                text_layout::estimated_char_width(ch, font_size)
            };
            let advance = text_layout::snapped_glyph_advance(ch, advance, font_size);
            if metrics.width > 0 && metrics.height > 0 {
                let (gx, gy) = text_layout::glyph_pixel_origin(cursor_x, cursor_y, metrics);
                stamps.push((gx, gy, metrics.width, metrics.height, glyph.bitmap.clone()));
            }
            cursor_x += advance;
        }
        drop(cache);
        for shadow in style.text_shadows.iter().rev() {
            if stamps.is_empty() {
                break;
            }
            let sigma = crate::blur::box_shadow_sigma(shadow.blur_radius);
            let pad = crate::blur::kernel_radius(sigma).max(0) as i32;
            let mut min_x = i32::MAX;
            let mut min_y = i32::MAX;
            let mut max_x = i32::MIN;
            let mut max_y = i32::MIN;
            for (gx, gy, width, height, _) in &stamps {
                min_x = min_x.min(*gx);
                min_y = min_y.min(*gy);
                max_x = max_x.max(*gx + *width as i32);
                max_y = max_y.max(*gy + *height as i32);
            }
            let origin_x = min_x + shadow.offset_x.round() as i32 - pad;
            let origin_y = min_y + shadow.offset_y.round() as i32 - pad;
            let width =
                (max_x - min_x + pad * 2 + shadow.offset_x.abs().ceil() as i32 + 2).max(1) as u32;
            let height =
                (max_y - min_y + pad * 2 + shadow.offset_y.abs().ceil() as i32 + 2).max(1) as u32;
            if width > 2048 || height > 512 {
                continue;
            }
            let mut alpha = vec![0_u8; (width * height) as usize];
            for (gx, gy, gw, gh, bitmap) in &stamps {
                let dest_x = *gx + shadow.offset_x.round() as i32 - origin_x;
                let dest_y = *gy + shadow.offset_y.round() as i32 - origin_y;
                for row in 0..*gh {
                    for col in 0..*gw {
                        let coverage = bitmap[row * *gw + col];
                        if coverage == 0 {
                            continue;
                        }
                        let px = dest_x + col as i32;
                        let py = dest_y + row as i32;
                        if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                            continue;
                        }
                        let index = (py as u32 * width + px as u32) as usize;
                        alpha[index] = alpha[index].max(coverage);
                    }
                }
            }
            if sigma >= 0.15 {
                crate::blur::blur_alpha(
                    &mut alpha,
                    width,
                    height,
                    sigma,
                    crate::blur::BlurEdge::Zero,
                );
            }
            let sprite = crate::blur::PremulSprite {
                origin_x: origin_x as f32,
                origin_y: origin_y as f32,
                width,
                height,
                pixels: {
                    let mut pixels = Vec::with_capacity(alpha.len() * 4);
                    for coverage in &alpha {
                        let a = (u16::from(*coverage) * u16::from(shadow.color.a) / 255) as u8;
                        pixels.push((u16::from(shadow.color.r) * u16::from(a) / 255) as u8);
                        pixels.push((u16::from(shadow.color.g) * u16::from(a) / 255) as u8);
                        pixels.push((u16::from(shadow.color.b) * u16::from(a) / 255) as u8);
                        pixels.push(a);
                    }
                    pixels
                },
            };
            blit_premul_sprite(pixmap, &sprite, style.opacity, clip_mask, None);
        }
    });
}

fn draw_blinking_cursor(
    pixmap: &mut Pixmap,
    content: LayoutRect,
    text: &str,
    font_size: f32,
    color: Color,
    font: &fontdue::Font,
    style: &Style,
    clip_mask: Option<&Mask>,
) {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    if (ms / 500).is_multiple_of(2) {
        return;
    }
    let mut cursor_x = content.x;
    let cursor_y = content.y + (content.height - font_size) / 2.0 + font_size;

    let registry = crate::font_face::FontRegistry::global();
    for character in text.chars() {
        cursor_x += registry.style_char_advance(style, character, font_size, font);
    }
    let px_w = pixmap.width() as i32;
    let px_h = pixmap.height() as i32;
    let in_clip = |px: i32, py: i32| -> bool {
        if let Some(mask) = clip_mask {
            if px < 0 || py < 0 || px >= mask.width() as i32 || py >= mask.height() as i32 {
                return false;
            }
            let idx = (py * mask.width() as i32 + px) as usize;
            mask.data().get(idx).copied().unwrap_or(0) > 0
        } else {
            true
        }
    };
    let cw = 2.0f32.max(font_size * 0.1);
    let gx = cursor_x as i32;
    let gy = (cursor_y - font_size) as i32;
    let gw = cw.ceil() as i32;
    let gh = font_size.ceil() as i32;
    let pixels = pixmap.pixels_mut();
    for row in 0..gh {
        for col in 0..gw {
            let px = gx + col;
            let py = gy + row;
            if px < 0 || py < 0 || px >= px_w || py >= px_h {
                continue;
            }
            if !in_clip(px, py) {
                continue;
            }
            let idx = (py * px_w + px) as usize;
            let dst = pixels[idx];
            let blended = blend_pixel(dst, color.r, color.g, color.b, 255);
            pixels[idx] = blended;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_image_pixels(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    img_w: u32,
    img_h: u32,
    rgba: &[u8],
    opacity: f32,
    clip_mask: Option<&Mask>,
    background_clip: Option<crate::background_image::BackgroundClip>,
    blend_mode: crate::background_image::BackgroundBlendMode,
) {
    let dest_w = rect.width.ceil() as u32;
    let dest_h = rect.height.ceil() as u32;
    if dest_w == 0 || dest_h == 0 || img_w == 0 || img_h == 0 {
        return;
    }
    let px_w = pixmap.width() as i32;
    let px_h = pixmap.height() as i32;
    let pixels = pixmap.pixels_mut();

    for dy in 0..dest_h {
        for dx in 0..dest_w {
            let px = rect.x as i32 + dx as i32;
            let py = rect.y as i32 + dy as i32;
            if px < 0 || py < 0 || px >= px_w || py >= px_h {
                continue;
            }
            if background_clip.is_some_and(|clip| {
                !point_in_background_clip(px as f32 + 0.5, py as f32 + 0.5, clip)
            }) {
                continue;
            }
            if let Some(mask) = clip_mask {
                if px >= mask.width() as i32 || py >= mask.height() as i32 {
                    continue;
                }
                let mask_idx = (py * mask.width() as i32 + px) as usize;
                if mask.data().get(mask_idx).copied().unwrap_or(0) == 0 {
                    continue;
                }
            }
            let src_x = ((dx as f32 / dest_w as f32) * img_w as f32) as u32;
            let src_y = ((dy as f32 / dest_h as f32) * img_h as f32) as u32;
            let src_x = src_x.min(img_w - 1);
            let src_y = src_y.min(img_h - 1);
            let src_idx = ((src_y * img_w + src_x) * 4) as usize;

            let r = rgba[src_idx];
            let g = rgba[src_idx + 1];
            let b = rgba[src_idx + 2];
            let a = (rgba[src_idx + 3] as f32 * opacity) as u8;
            if a == 0 {
                continue;
            }

            let dst_idx = (py * px_w + px) as usize;
            let dst = pixels[dst_idx];
            pixels[dst_idx] = blend_background_pixel(dst, r, g, b, a, blend_mode);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_gradient_pixels(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    kind: &crate::background_image::GradientKind,
    stops: &[crate::background_image::GradientStop],
    opacity: f32,
    clip_mask: Option<&Mask>,
    background_clip: crate::background_image::BackgroundClip,
    blend_mode: crate::background_image::BackgroundBlendMode,
) {
    let start_x = rect.x.floor() as i32;
    let start_y = rect.y.floor() as i32;
    let end_x = (rect.x + rect.width).ceil() as i32;
    let end_y = (rect.y + rect.height).ceil() as i32;
    let px_w = pixmap.width() as i32;
    let px_h = pixmap.height() as i32;
    let linear = match kind {
        crate::background_image::GradientKind::Linear { angle_degrees } => Some(
            crate::background_image::linear_gradient_points(rect, *angle_degrees),
        ),
        _ => None,
    };
    let radial = match kind {
        crate::background_image::GradientKind::Radial {
            center_x,
            center_y,
            shape,
            radius,
        } => Some(crate::background_image::radial_gradient_axes(
            rect, *center_x, *center_y, *shape, *radius,
        )),
        _ => None,
    };
    let pixels = pixmap.pixels_mut();
    for py in start_y.max(0)..end_y.min(px_h) {
        for px in start_x.max(0)..end_x.min(px_w) {
            let x = px as f32 + 0.5;
            let y = py as f32 + 0.5;
            if !point_in_background_clip(x, y, background_clip) {
                continue;
            }
            if let Some(mask) = clip_mask {
                let mask_idx = (py * mask.width() as i32 + px) as usize;
                if mask.data().get(mask_idx).copied().unwrap_or(0) == 0 {
                    continue;
                }
            }
            let t = if let Some((start, end)) = linear {
                let dx = end.0 - start.0;
                let dy = end.1 - start.1;
                ((x - start.0) * dx + (y - start.1) * dy) / (dx * dx + dy * dy).max(1.0)
            } else if let Some((center, (radius_x, radius_y))) = radial {
                (((x - center.0) / radius_x).powi(2) + ((y - center.1) / radius_y).powi(2)).sqrt()
            } else {
                continue;
            }
            .clamp(0.0, 1.0);
            let color = interpolate_gradient_color(stops, t);
            let alpha = (color.a as f32 * opacity.clamp(0.0, 1.0)).round() as u8;
            let index = (py * px_w + px) as usize;
            pixels[index] =
                blend_background_pixel(pixels[index], color.r, color.g, color.b, alpha, blend_mode);
        }
    }
}

fn interpolate_gradient_color(
    stops: &[crate::background_image::GradientStop],
    position: f32,
) -> Color {
    let upper = stops
        .iter()
        .position(|stop| stop.position >= position)
        .unwrap_or(stops.len() - 1);
    if upper == 0 {
        return stops[0].color;
    }
    let lower = upper - 1;
    let span = (stops[upper].position - stops[lower].position).max(f32::EPSILON);
    let t = ((position - stops[lower].position) / span).clamp(0.0, 1.0);
    let lerp = |from: u8, to: u8| {
        (from as f32 + (to as f32 - from as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::rgba(
        lerp(stops[lower].color.r, stops[upper].color.r),
        lerp(stops[lower].color.g, stops[upper].color.g),
        lerp(stops[lower].color.b, stops[upper].color.b),
        lerp(stops[lower].color.a, stops[upper].color.a),
    )
}

fn point_in_background_clip(x: f32, y: f32, clip: crate::background_image::BackgroundClip) -> bool {
    let rect = clip.rect;
    if x < rect.x || y < rect.y || x >= rect.x + rect.width || y >= rect.y + rect.height {
        return false;
    }
    let radius = clip
        .radius
        .min(rect.width * 0.5)
        .min(rect.height * 0.5)
        .max(0.0);
    if radius == 0.0 {
        return true;
    }
    let center_x = if x < rect.x + radius {
        rect.x + radius
    } else if x > rect.x + rect.width - radius {
        rect.x + rect.width - radius
    } else {
        x
    };
    let center_y = if y < rect.y + radius {
        rect.y + radius
    } else if y > rect.y + rect.height - radius {
        rect.y + rect.height - radius
    } else {
        y
    };
    (x - center_x).powi(2) + (y - center_y).powi(2) <= radius.powi(2)
}

fn blend_pixel(
    dst: tiny_skia::PremultipliedColorU8,
    sr: u8,
    sg: u8,
    sb: u8,
    sa: u8,
) -> tiny_skia::PremultipliedColorU8 {
    let da = dst.alpha() as u16;
    let dr = dst.red() as u16;
    let dg = dst.green() as u16;
    let db = dst.blue() as u16;
    let sa16 = sa as u16;
    let inv = 255 - sa16;

    let out_a = (sa16 + da * inv / 255).min(255) as u8;
    let out_r = (sr as u16 * sa16 / 255 + dr * inv / 255).min(255) as u8;
    let out_g = (sg as u16 * sa16 / 255 + dg * inv / 255).min(255) as u8;
    let out_b = (sb as u16 * sa16 / 255 + db * inv / 255).min(255) as u8;

    tiny_skia::PremultipliedColorU8::from_rgba(out_r, out_g, out_b, out_a).unwrap()
}

fn blend_background_pixel(
    dst: tiny_skia::PremultipliedColorU8,
    sr: u8,
    sg: u8,
    sb: u8,
    sa: u8,
    mode: crate::background_image::BackgroundBlendMode,
) -> tiny_skia::PremultipliedColorU8 {
    use crate::background_image::BackgroundBlendMode as Mode;
    if mode == Mode::Normal || dst.alpha() == 0 {
        return blend_pixel(dst, sr, sg, sb, sa);
    }
    let da = f32::from(dst.alpha()) / 255.0;
    let backdrop =
        |channel: u8| (f32::from(channel) / 255.0 / da.max(f32::EPSILON)).clamp(0.0, 1.0);
    let blend = |source: u8, destination: u8| {
        let source = f32::from(source) / 255.0;
        let destination = backdrop(destination);
        let value = match mode {
            Mode::Normal => source,
            Mode::Multiply => source * destination,
            Mode::Screen => source + destination - source * destination,
            Mode::Overlay => {
                if destination <= 0.5 {
                    2.0 * source * destination
                } else {
                    1.0 - 2.0 * (1.0 - source) * (1.0 - destination)
                }
            }
            Mode::Darken => source.min(destination),
            Mode::Lighten => source.max(destination),
            Mode::ColorDodge => {
                if source >= 1.0 {
                    1.0
                } else {
                    (destination / (1.0 - source)).min(1.0)
                }
            }
            Mode::ColorBurn => {
                if source <= 0.0 {
                    0.0
                } else {
                    1.0 - ((1.0 - destination) / source).min(1.0)
                }
            }
            Mode::HardLight => {
                if source <= 0.5 {
                    2.0 * source * destination
                } else {
                    1.0 - 2.0 * (1.0 - source) * (1.0 - destination)
                }
            }
            Mode::SoftLight => {
                (1.0 - 2.0 * source) * destination * destination + 2.0 * source * destination
            }
            Mode::Difference => (destination - source).abs(),
            Mode::Exclusion => source + destination - 2.0 * source * destination,
        };
        (value.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    blend_pixel(
        dst,
        blend(sr, dst.red()),
        blend(sg, dst.green()),
        blend(sb, dst.blue()),
        sa,
    )
}

fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

#[cfg(test)]
mod font_cjk_tests {
    use super::*;

    #[test]
    fn text_align_last_overrides_the_final_inline_line() {
        let mut style = Style::default();
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                "--w3cos-internal-text-align-last".to_string(),
                "right".to_string(),
            );
        assert_eq!(text_align_last(&style), Some(TextAlign::Right));
    }

    #[test]
    fn cjk_subset_font_loads() {
        let data = include_bytes!("../assets/CJK-Subset.ttf");
        let font =
            fontdue::Font::from_bytes(data as &[u8], fontdue::FontSettings::default()).unwrap();
        let (m, bmp) = font.rasterize('话', 16.0);
        assert!(m.width > 0 && !bmp.is_empty());
        let (m2, _) = font.rasterize('A', 16.0);
        assert!(m2.advance_width > 0.0);
    }

    fn button_label(
        label: &str,
        overflow: w3cos_std::style::Overflow,
        color: Color,
    ) -> (ComponentKind, Style) {
        let mut style = Style::default();
        style.color = color;
        style.overflow = overflow;
        style.font_size = 16.0;
        (
            ComponentKind::Button {
                label: label.to_string(),
            },
            style,
        )
    }

    fn inside(rect: LayoutRect, x: u32, y: u32) -> bool {
        x as f32 >= rect.x
            && y as f32 >= rect.y
            && (x as f32) < rect.x + rect.width
            && (y as f32) < rect.y + rect.height
    }

    fn bright_where(pixmap: &Pixmap, pred: impl Fn(u32, u32) -> bool) -> usize {
        pixmap
            .pixels()
            .iter()
            .enumerate()
            .filter(|(index, pixel)| {
                let x = (*index as u32) % pixmap.width();
                let y = (*index as u32) / pixmap.width();
                pred(x, y) && pixel.red() > 180 && pixel.green() > 180 && pixel.blue() > 180
            })
            .count()
    }

    #[test]
    fn overflowing_button_labels_do_not_paint_into_a_neighbor() {
        let data = include_bytes!("../assets/CJK-Subset.ttf");
        let font =
            fontdue::Font::from_bytes(data as &[u8], fontdue::FontSettings::default()).unwrap();
        let left = LayoutRect {
            x: 8.0,
            y: 4.0,
            width: 40.0,
            height: 32.0,
        };
        let right = LayoutRect {
            x: 56.0,
            y: 4.0,
            width: 40.0,
            height: 32.0,
        };
        let mut clipped = Pixmap::new(104, 40).unwrap();
        clipped.fill(SkColor::from_rgba8(16, 22, 29, 255));
        for (rect, label) in [(left, "Applications"), (right, "Notifications")] {
            let (kind, style) =
                button_label(label, w3cos_std::style::Overflow::Hidden, Color::WHITE);
            render_node(
                &mut clipped,
                rect,
                &kind,
                &style,
                &font,
                None,
                None,
                false,
                false,
                None,
            );
        }
        assert_eq!(
            bright_where(&clipped, |x, y| !inside(left, x, y) && !inside(right, x, y)),
            0,
            "clipped labels crossed the gap"
        );
        assert!(bright_where(&clipped, |x, y| inside(left, x, y)) > 0);

        let mut visible = Pixmap::new(104, 40).unwrap();
        visible.fill(SkColor::from_rgba8(16, 22, 29, 255));
        let (kind, style) = button_label(
            "Applications",
            w3cos_std::style::Overflow::Visible,
            Color::WHITE,
        );
        render_node(
            &mut visible,
            left,
            &kind,
            &style,
            &font,
            None,
            None,
            false,
            false,
            None,
        );
        assert!(
            bright_where(&visible, |x, y| !inside(left, x, y)) > 0,
            "a visible label wider than its icon must be able to spill, otherwise the clip assertion is vacuous"
        );

        let mut hidden = Pixmap::new(104, 40).unwrap();
        hidden.fill(SkColor::from_rgba8(16, 22, 29, 255));
        let (kind, style) = button_label(
            "Applications",
            w3cos_std::style::Overflow::Hidden,
            Color::TRANSPARENT,
        );
        render_node(
            &mut hidden,
            left,
            &kind,
            &style,
            &font,
            None,
            None,
            false,
            false,
            None,
        );
        assert!(
            hidden
                .pixels()
                .iter()
                .all(|pixel| pixel.red() < 40 && pixel.green() < 50),
            "an icon button's accessible name must not paint"
        );
    }

    #[test]
    fn svg_path_fill_and_stroke_are_rasterized() {
        let path = w3cos_std::SvgPathData::parse("M2 2 L18 2 L10 18 Z").unwrap();
        let mut pixmap = Pixmap::new(24, 24).unwrap();
        draw_svg_path(
            &mut pixmap,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 16.0,
                height: 16.0,
            },
            &path.commands,
            Color::rgb(255, 0, 0),
            Some(Color::rgb(0, 0, 255)),
            2.0,
            None,
        );

        assert!(pixmap.pixels().iter().any(|pixel| pixel.red() > 0));
        assert!(pixmap.pixels().iter().any(|pixel| pixel.blue() > 0));
    }

    #[test]
    fn inset_shadow_stays_inside_the_box_and_outer_shadow_does_not() {
        let rect = LayoutRect {
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
        };
        let mut inset = Pixmap::new(40, 40).unwrap();
        draw_box_shadow(
            &mut inset,
            rect,
            &w3cos_std::style::BoxShadow {
                offset_x: 0.0,
                offset_y: 4.0,
                blur_radius: 0.0,
                spread_radius: 0.0,
                color: Color::rgb(255, 0, 0),
                inset: true,
            },
            0.0,
            1.0,
            None,
            None,
        );
        assert_eq!(inset.pixel(1, 1).unwrap().alpha(), 0);
        assert!(inset.pixel(20, 12).unwrap().red() > 200);
        assert_eq!(inset.pixel(20, 24).unwrap().alpha(), 0);

        let mut outer = Pixmap::new(40, 40).unwrap();
        draw_box_shadow(
            &mut outer,
            rect,
            &w3cos_std::style::BoxShadow::new(0.0, 6.0, 0.0, 0.0, Color::rgb(0, 0, 255)),
            0.0,
            1.0,
            None,
            None,
        );
        assert!(outer.pixel(20, 32).unwrap().blue() > 0);
    }

    #[test]
    fn gaussian_outer_shadow_is_soft_and_backdrop_mixes_the_seam() {
        let rect = LayoutRect {
            x: 16.0,
            y: 16.0,
            width: 28.0,
            height: 20.0,
        };
        let mut pixmap = Pixmap::new(64, 56).unwrap();
        draw_box_shadow(
            &mut pixmap,
            rect,
            &w3cos_std::style::BoxShadow::new(0.0, 0.0, 8.0, 0.0, Color::rgb(0, 0, 255)),
            0.0,
            1.0,
            None,
            None,
        );
        let edge = pixmap.pixel(45, 26).unwrap().alpha();
        let further = pixmap.pixel(50, 26).unwrap().alpha();
        assert!(edge > further);
        assert!(further > 0);
        assert!(edge < 250);

        let mut backdrop = Pixmap::new(40, 20).unwrap();
        backdrop.fill(tiny_skia::Color::from_rgba8(220, 20, 20, 255));
        for x in 20..40 {
            for y in 0..20 {
                if let Some(pixel) = backdrop.pixels_mut().get_mut((y * 40 + x) as usize) {
                    *pixel = tiny_skia::PremultipliedColorU8::from_rgba(20, 20, 220, 255)
                        .unwrap_or(*pixel);
                }
            }
        }
        let before = backdrop.pixel(4, 10).unwrap().red();
        draw_backdrop_blur(
            &mut backdrop,
            LayoutRect {
                x: 8.0,
                y: 2.0,
                width: 24.0,
                height: 16.0,
            },
            0.0,
            4.0,
            None,
            None,
        );
        let mixed = backdrop.pixel(20, 10).unwrap();
        assert!(mixed.red() > 40 && mixed.blue() > 40);
        assert_eq!(backdrop.pixel(4, 10).unwrap().red(), before);
    }

    #[test]
    fn text_shadow_paints_beside_the_glyph() {
        let font = fontdue::Font::from_bytes(
            include_bytes!("../assets/Inter-Regular.ttf") as &[u8],
            fontdue::FontSettings::default(),
        )
        .unwrap();
        let mut style = Style::default();
        style.font_size = 18.0;
        style.color = Color::rgb(255, 255, 255);
        style.text_shadows = vec![w3cos_std::style::TextShadow {
            offset_x: 3.0,
            offset_y: 0.0,
            blur_radius: 0.0,
            color: Color::rgb(255, 0, 0),
        }];
        let mut pixmap = Pixmap::new(48, 32).unwrap();
        draw_text_line(
            &mut pixmap,
            4.0,
            4.0,
            "A",
            18.0,
            Color::rgb(255, 255, 255),
            &font,
            &style,
            None,
        );
        assert!(
            pixmap
                .pixels()
                .iter()
                .any(|pixel| pixel.red() > pixel.blue().saturating_add(40) && pixel.alpha() > 0)
        );
    }

    #[test]
    fn layered_wallpaper_gradient_is_not_a_flat_fill() {
        let font = fontdue::Font::from_bytes(
            include_bytes!("../assets/Inter-Regular.ttf") as &[u8],
            fontdue::FontSettings::default(),
        )
        .unwrap();
        let style = Style {
            background: Color::from_hex("#101820"),
            background_image: Some(
                "radial-gradient(40px 16px at 0% 0%, rgba(120, 168, 186, 0.95), transparent 70%), linear-gradient(180deg, #1b3344 0%, #101820 100%)"
                    .to_string(),
            ),
            ..Style::default()
        };
        let kind = ComponentKind::Box;
        let nodes = [(
            0usize,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 80.0,
                height: 40.0,
            },
            &kind,
            &style,
        )];
        let mut pixmap = Pixmap::new(80, 40).unwrap();
        render_frame(
            &mut pixmap,
            &nodes,
            &font,
            &[None],
            &HashMap::new(),
            None,
            &mut ClipMaskCache::default(),
        );
        let top = pixmap.pixel(2, 2).unwrap();
        let bottom = pixmap.pixel(2, 36).unwrap();
        assert!(
            top.red() != bottom.red()
                || top.green() != bottom.green()
                || top.blue() != bottom.blue(),
            "top {:?} bottom {:?}",
            (top.red(), top.green(), top.blue()),
            (bottom.red(), bottom.green(), bottom.blue())
        );
    }

    #[test]
    fn clip_mask_cache_reuses_geometry_and_invalidates_on_resize() {
        let pixmap = Pixmap::new(100, 100).unwrap();
        let rect = LayoutRect {
            x: 10.0,
            y: 20.0,
            width: 60.0,
            height: 40.0,
        };
        let mut cache = ClipMaskCache::default();

        assert!(cache.get_or_create(&pixmap, rect).is_some());
        assert!(cache.get_or_create(&pixmap, rect).is_some());
        assert_eq!(cache.masks.len(), 1);

        let resized = Pixmap::new(120, 100).unwrap();
        assert!(cache.get_or_create(&resized, rect).is_some());
        assert_eq!(cache.framebuffer_size, (120, 100));
        assert_eq!(cache.masks.len(), 1);
    }

    #[test]
    fn glyph_raster_cache_reuses_character_at_same_size() {
        let data = include_bytes!("../assets/CJK-Subset.ttf");
        let font =
            fontdue::Font::from_bytes(data as &[u8], fontdue::FontSettings::default()).unwrap();
        let latin = fontdue::Font::from_bytes(
            include_bytes!("../assets/Inter-Regular.ttf") as &[u8],
            fontdue::FontSettings::default(),
        )
        .unwrap();
        let mut cache = GlyphRasterCache::default();

        assert!(!cache.get_or_rasterize(&font, '话', 16.0).bitmap.is_empty());
        assert!(!cache.get_or_rasterize(&font, '话', 16.0).bitmap.is_empty());
        assert_eq!(cache.glyphs.len(), 1);

        assert!(!cache.get_or_rasterize(&font, '话', 18.0).bitmap.is_empty());
        assert_eq!(cache.glyphs.len(), 2);

        cache.get_or_rasterize(&font, 'A', 16.0);
        cache.get_or_rasterize(&latin, 'A', 16.0);
        assert_eq!(
            cache.glyphs.len(),
            4,
            "the same character and size in two fonts needs two glyph entries"
        );
    }

    #[test]
    fn cpu_text_raster_uses_unicode_range_font_runs() {
        const OWNER: u64 = 0x4350_5553_5542_5345;
        const FAMILY: &str = "W3COS CPU Subset Test";

        fn scaled_inter(divisor: u16) -> Vec<u8> {
            fn table_offset(bytes: &[u8], tag: &[u8; 4]) -> Option<usize> {
                let count = u16::from_be_bytes(bytes.get(4..6)?.try_into().ok()?) as usize;
                (0..count).find_map(|index| {
                    let entry = 12 + index * 16;
                    (bytes.get(entry..entry + 4)? == tag).then(|| {
                        u32::from_be_bytes(
                            bytes[entry + 8..entry + 12]
                                .try_into()
                                .expect("table offset"),
                        ) as usize
                    })
                })
            }

            let mut bytes = include_bytes!("../assets/Inter-Regular.ttf").to_vec();
            let hhea = table_offset(&bytes, b"hhea").expect("hhea table");
            let hmtx = table_offset(&bytes, b"hmtx").expect("hmtx table");
            let count =
                u16::from_be_bytes(bytes[hhea + 34..hhea + 36].try_into().unwrap()) as usize;
            for index in 0..count {
                let offset = hmtx + index * 4;
                let advance = u16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap());
                bytes[offset..offset + 2]
                    .copy_from_slice(&(advance / divisor).max(1).to_be_bytes());
            }
            bytes
        }

        for (range, divisor) in [("U+0057", 4), ("U+0030-0039", 2)] {
            crate::font_face::FontRegistry::global()
                .register_for_owner(
                    OWNER,
                    crate::font_face::FontFace {
                        family: FAMILY.to_string(),
                        src: crate::font_face::FontSource::Bytes(scaled_inter(divisor)),
                        unicode_range: Some(range.to_string()),
                        ..crate::font_face::FontFace::default()
                    },
                )
                .expect("register CPU subset");
        }
        let style = Style {
            font_family: Some(FAMILY.to_string()),
            ..Style::default()
        };
        let fallback = fontdue::Font::from_bytes(
            include_bytes!("../assets/CJK-Subset.ttf") as &[u8],
            fontdue::FontSettings::default(),
        )
        .unwrap();
        let mut pixmap = Pixmap::new(160, 40).unwrap();
        clear_glyph_cache();
        draw_text_line(
            &mut pixmap,
            0.0,
            0.0,
            "W3W",
            20.0,
            Color::rgb(0, 0, 0),
            &fallback,
            &style,
            None,
        );
        GLYPH_RASTER_CACHE.with(|cache| {
            let cache = cache.borrow();
            assert_eq!(cache.glyphs.len(), 2);
            assert_eq!(
                cache
                    .glyphs
                    .keys()
                    .map(|key| key.font)
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                2,
                "W and digit subsets must rasterize with their own font instances"
            );
        });
        crate::font_face::FontRegistry::global().clear_owner(OWNER);
    }

    #[test]
    fn exposed_scroll_strip_tracks_scroll_direction() {
        let rect = LayoutRect {
            x: 5.0,
            y: 10.0,
            width: 80.0,
            height: 100.0,
        };

        let down = exposed_scroll_strip(rect, 12.0).unwrap();
        assert_eq!((down.y, down.height), (98.0, 12.0));

        let up = exposed_scroll_strip(rect, -7.0).unwrap();
        assert_eq!((up.y, up.height), (10.0, 7.0));
    }

    #[test]
    fn shift_scroll_raster_moves_existing_rows() {
        let mut pixmap = Pixmap::new(2, 4).unwrap();
        for y in 0..4usize {
            let color =
                tiny_skia::PremultipliedColorU8::from_rgba((y as u8 + 1) * 10, 0, 0, 255).unwrap();
            pixmap.pixels_mut()[y * 2..y * 2 + 2].fill(color);
        }

        shift_scroll_raster(
            &mut pixmap,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 4.0,
            },
            1.0,
        )
        .unwrap();

        assert_eq!(pixmap.pixels()[0].red(), 20);
        assert_eq!(pixmap.pixels()[2].red(), 30);
        assert_eq!(pixmap.pixels()[4].red(), 40);
    }

    #[test]
    fn nested_scroller_is_within_outer_scroll_damage() {
        let ancestors = vec![None, Some(0), Some(1), Some(2)];
        assert!(is_within_scroll_container(3, 0, &ancestors));
        assert!(is_within_scroll_container(3, 1, &ancestors));
        assert!(!is_within_scroll_container(0, 1, &ancestors));
    }

    #[test]
    fn sticky_rows_are_excluded_from_raster_scroll() {
        let mut style = Style::default();
        style.position = w3cos_std::style::Position::Sticky;
        let kind = ComponentKind::Column;
        let nodes = vec![(
            1,
            LayoutRect {
                x: 20.0,
                y: 100.0,
                width: 200.0,
                height: 50.0,
            },
            &kind,
            &style,
        )];
        let protected = sticky_scroll_protected_rects(
            &nodes,
            &[None, None],
            &[None, Some(0)],
            0,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: 500.0,
            },
        );

        assert_eq!(protected.len(), 1);
        assert_eq!(protected[0].y, 84.0);
        assert_eq!(protected[0].height, 82.0);
    }

    #[test]
    fn excluded_sticky_rows_create_only_boundary_damage_strips() {
        let mut pixmap = Pixmap::new(10, 100).unwrap();
        let damages = shift_scroll_raster_excluding(
            &mut pixmap,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 100.0,
            },
            4.0,
            &[LayoutRect {
                x: 0.0,
                y: 20.0,
                width: 10.0,
                height: 40.0,
            }],
        )
        .unwrap();

        assert_eq!(damages.len(), 2);
        assert_eq!((damages[0].y, damages[0].height), (16.0, 4.0));
        assert_eq!((damages[1].y, damages[1].height), (96.0, 4.0));
    }

    #[test]
    fn transparent_scroll_container_requires_full_repaint() {
        let mut style = Style::default();
        style.background = Color::rgba(255, 255, 255, 0);
        assert!(!scroll_raster_copy_safe(&style));

        style.background = Color::rgb(255, 255, 255);
        assert!(scroll_raster_copy_safe(&style));
    }

    #[test]
    fn cpu_gradient_sampling_and_rounded_clip_use_shared_description() {
        let stops = [
            crate::background_image::GradientStop {
                color: Color::rgb(255, 0, 0),
                position: 0.0,
            },
            crate::background_image::GradientStop {
                color: Color::rgb(0, 0, 255),
                position: 1.0,
            },
        ];
        let mut pixmap = Pixmap::new(10, 10).unwrap();
        draw_gradient_pixels(
            &mut pixmap,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            &crate::background_image::GradientKind::Linear {
                angle_degrees: 90.0,
            },
            &stops,
            1.0,
            None,
            crate::background_image::BackgroundClip {
                rect: LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                },
                radius: 4.0,
            },
            crate::background_image::BackgroundBlendMode::Normal,
        );
        assert_eq!(pixmap.pixel(0, 0).unwrap().alpha(), 0);
        assert!(pixmap.pixel(2, 5).unwrap().red() > pixmap.pixel(2, 5).unwrap().blue());
        assert!(pixmap.pixel(8, 5).unwrap().blue() > pixmap.pixel(8, 5).unwrap().red());
    }

    #[test]
    fn dock_and_intent_bar_reuse_cached_chrome_under_4ms_when_release() {
        crate::blur::clear_frame_caches();
        let font = fontdue::Font::from_bytes(
            include_bytes!("../assets/Inter-Regular.ttf") as &[u8],
            fontdue::FontSettings::default(),
        )
        .unwrap();
        let mut wallpaper = Style::default();
        wallpaper.background = Color::rgb(32, 48, 72);
        let mut dock = Style::default();
        dock.background = Color::rgba(12, 18, 24, 190);
        dock.border_radius = 22.0;
        dock.border_width = 1.0;
        dock.border_color = Color::rgba(244, 241, 234, 40);
        dock.backdrop_blur = Some(18.0);
        dock.box_shadow = Some(w3cos_std::style::BoxShadow::new(
            0.0,
            8.0,
            24.0,
            0.0,
            Color::rgba(0, 0, 0, 82),
        ));
        let mut intent = Style::default();
        intent.background = Color::rgba(16, 22, 29, 210);
        intent.border_radius = 16.0;
        intent.border_width = 1.0;
        intent.border_color = Color::rgba(244, 241, 234, 40);
        intent.backdrop_blur = Some(16.0);
        intent.box_shadow = Some(w3cos_std::style::BoxShadow::new(
            0.0,
            8.0,
            24.0,
            0.0,
            Color::rgba(0, 0, 0, 82),
        ));
        let mut corner = Style::default();
        corner.background = Color::rgb(200, 80, 40);
        let kind = ComponentKind::Box;
        let nodes = [
            (
                0usize,
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1200.0,
                    height: 800.0,
                },
                &kind,
                &wallpaper,
            ),
            (
                1usize,
                LayoutRect {
                    x: 16.0,
                    y: 708.0,
                    width: 1168.0,
                    height: 72.0,
                },
                &kind,
                &dock,
            ),
            (
                2usize,
                LayoutRect {
                    x: 280.0,
                    y: 636.0,
                    width: 640.0,
                    height: 52.0,
                },
                &kind,
                &intent,
            ),
            (
                3usize,
                LayoutRect {
                    x: 8.0,
                    y: 8.0,
                    width: 24.0,
                    height: 24.0,
                },
                &kind,
                &corner,
            ),
        ];
        let scroll = [None, None, None, None];
        let values = HashMap::new();
        let mut masks = ClipMaskCache {
            framebuffer_size: (0, 0),
            masks: HashMap::new(),
        };
        let mut pixmap = Pixmap::new(1200, 800).unwrap();
        let cold = Instant::now();
        render_frame(
            &mut pixmap,
            &nodes,
            &font,
            &scroll,
            &values,
            None,
            &mut masks,
        );
        let cold_ms = cold.elapsed().as_secs_f64() * 1000.0;
        let dock_pixel = pixmap.pixel(600, 740).unwrap();
        let warm_started = Instant::now();
        let frames = 4u32;
        for _ in 0..frames {
            render_frame(
                &mut pixmap,
                &nodes,
                &font,
                &scroll,
                &values,
                None,
                &mut masks,
            );
        }
        let warm_ms = warm_started.elapsed().as_secs_f64() * 1000.0 / f64::from(frames);
        assert_eq!(pixmap.pixel(600, 740).unwrap().red(), dock_pixel.red());
        assert_eq!(pixmap.pixel(600, 740).unwrap().blue(), dock_pixel.blue());
        let damage = render_damage_nodes(
            &mut pixmap,
            &nodes,
            &font,
            &scroll,
            &values,
            None,
            &[3],
            &mut masks,
        );
        assert!(damage.is_some());
        assert_eq!(pixmap.pixel(600, 740).unwrap().red(), dock_pixel.red());
        let budget = if cfg!(debug_assertions) { 80.0 } else { 4.0 };
        assert!(
            warm_ms < budget,
            "cold {cold_ms:.2}ms warm {warm_ms:.2}ms budget {budget}ms hits {}",
            crate::blur::cache_hit_count()
        );
        eprintln!("dock+intent 1200x800 cold {cold_ms:.2}ms warm {warm_ms:.2}ms");
    }
}
