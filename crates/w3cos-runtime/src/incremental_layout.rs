//! Persistent component + Taffy tree for geometry flushes.
//!
//! Style-only edits patch the cached nodes and call `compute_layout` again.
//! Tree shape, text metrics, and inherited cascades rebuild once.

use std::cell::RefCell;

use w3cos_dom::node::NodeId;
use w3cos_std::component::EventAction;
use w3cos_std::style::{Dimension, Display, Float, Position, TextDirection};
use w3cos_std::{Component, ComponentKind};

use crate::layout::{LayoutEngine, pre_flatten};

struct WidthMemo {
    structure: u64,
    node: u32,
    width_bits: u32,
    viewport_w_bits: u32,
    viewport_h_bits: u32,
    rects: Vec<(u32, f32, f32, f32, f32)>,
}

struct Cache {
    component: Component,
    engine: LayoutEngine,
    structure: u64,
    cascade: u64,
    node_ids: Vec<u32>,
    /// Full rect snapshots keyed by one node's px width. Repeat flushes of
    /// the same width restore the rects instead of solving Taffy again.
    width_memos: Vec<WidthMemo>,
}

thread_local! {
    static CACHE: RefCell<Option<Cache>> = const { RefCell::new(None) };
}

pub fn invalidate() {
    CACHE.with(|cache| *cache.borrow_mut() = None);
}

/// Flush pending layout into stored DOM rects. Returns false when Taffy fails.
pub fn flush() -> bool {
    if crate::definite_layout::try_apply() {
        let _ = crate::dom::with_document_mut(|document| document.take_precise_dirty());
        crate::dom::clear_document_dirty();
        invalidate();
        return true;
    }
    let (viewport_w, viewport_h, _) = crate::jsdom::viewport();
    let viewport_w = viewport_w as f32;
    let viewport_h = viewport_h as f32;
    let (structure, cascade) =
        crate::dom::with_document(|document| (document.structure_generation(), document.cascade_generation()));
    let dirty = crate::dom::with_document_mut(|document| document.take_precise_dirty());
    let mut ok = false;
    CACHE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let structure_matches = slot
            .as_ref()
            .is_some_and(|cache| cache.structure == structure);
        if structure_matches {
            let cache = slot.as_mut().unwrap();
            let geometry_only = cache.cascade != cascade;
            if patch_and_compute(cache, &dirty, viewport_w, viewport_h, geometry_only) {
                cache.cascade = cascade;
                ok = true;
                return;
            }
        }
        ok = rebuild(&mut slot, structure, cascade, viewport_w, viewport_h);
    });
    if ok {
        crate::dom::clear_document_dirty();
    }
    ok
}

fn rebuild(
    slot: &mut Option<Cache>,
    structure: u64,
    cascade: u64,
    viewport_w: f32,
    viewport_h: f32,
) -> bool {
    let component = crate::dom::to_component_tree();
    let flat = pre_flatten(&component);
    let node_ids = flat
        .iter()
        .map(|info| match info.on_click {
            EventAction::NativeHost { id, .. } => *id as u32,
            _ => u32::MAX,
        })
        .collect::<Vec<_>>();
    let mut engine = LayoutEngine::new();
    configure_engine(&mut engine, &component);
    let Ok(results) = engine.compute(&component, &flat, viewport_w, viewport_h) else {
        return false;
    };
    let snaps = write_rects(&flat, &results.layout_cache);
    let width_memos = memos_for_tree(&component, structure, viewport_w, viewport_h, snaps);
    *slot = Some(Cache {
        component,
        engine,
        structure,
        cascade,
        node_ids,
        width_memos,
    });
    true
}

fn patch_and_compute(
    cache: &mut Cache,
    dirty: &[NodeId],
    viewport_w: f32,
    viewport_h: f32,
    geometry_only: bool,
) -> bool {
    if dirty.is_empty() {
        // A cascade change with no dirty box can restyle descendants. Rebuild.
        if geometry_only {
            return false;
        }
        return recompute(cache, viewport_w, viewport_h);
    }
    let mut styles = Vec::with_capacity(dirty.len());
    for id in dirty {
        let style = crate::dom::with_document(|document| document.computed_style_for(*id));
        if !style_patch_is_safe(&style) {
            return false;
        }
        if geometry_only {
            let Some(old) = component_style(&cache.component, id.as_u32()).cloned() else {
                return false;
            };
            if !geometry_only_change(&old, &style) {
                return false;
            }
        }
        styles.push((id.as_u32(), style));
    }
    let memo_hit = single_px_width(&styles).and_then(|(node, bits)| {
        let viewport_w_bits = viewport_w.to_bits();
        let viewport_h_bits = viewport_h.to_bits();
        cache
            .width_memos
            .iter()
            .find(|memo| {
                memo.structure == cache.structure
                    && memo.node == node
                    && memo.width_bits == bits
                    && memo.viewport_w_bits == viewport_w_bits
                    && memo.viewport_h_bits == viewport_h_bits
            })
            .map(|memo| memo.rects.clone())
    });
    for (id, style) in &styles {
        if !assign_layout_style(&mut cache.component, *id, style) {
            return false;
        }
    }
    if let Some(rects) = memo_hit {
        restore_rects(&rects);
        return true;
    }
    let flat = pre_flatten(&cache.component);
    if flat.len() != cache.node_ids.len() {
        return false;
    }
    configure_engine(&mut cache.engine, &cache.component);
    for (id, style) in &styles {
        let Some(index) = cache.node_ids.iter().position(|node| node == id) else {
            return false;
        };
        if !cache.engine.patch_style(index, style, viewport_w, viewport_h) {
            return false;
        }
    }
    let Ok(results) = cache.engine.compute(&cache.component, &flat, viewport_w, viewport_h) else {
        return false;
    };
    let snaps = write_rects(&flat, &results.layout_cache);
    if let Some((node, bits)) = single_px_width(&styles) {
        remember_width(cache, node, bits, viewport_w, viewport_h, snaps);
    }
    true
}

fn recompute(cache: &mut Cache, viewport_w: f32, viewport_h: f32) -> bool {
    let flat = pre_flatten(&cache.component);
    configure_engine(&mut cache.engine, &cache.component);
    let Ok(results) = cache.engine.compute(&cache.component, &flat, viewport_w, viewport_h) else {
        return false;
    };
    write_rects(&flat, &results.layout_cache);
    true
}

fn write_rects(
    flat: &[crate::layout::FlatNodeInfo<'_>],
    layouts: &[(crate::layout::LayoutRect, usize)],
) -> Vec<(u32, f32, f32, f32, f32)> {
    let mut snaps = Vec::with_capacity(layouts.len());
    crate::dom::with_document_mut(|document| {
        for (rect, index) in layouts {
            let Some(info) = flat.get(*index) else {
                continue;
            };
            let w3cos_std::EventAction::NativeHost { id, .. } = info.on_click else {
                continue;
            };
            let Ok(node_id) = u32::try_from(*id) else {
                continue;
            };
            snaps.push((node_id, rect.x, rect.y, rect.width, rect.height));
            document.set_layout_rect(
                NodeId::from_u32(node_id),
                w3cos_dom::DOMRect::new(rect.x, rect.y, rect.width, rect.height),
            );
        }
    });
    snaps
}

fn restore_rects(rects: &[(u32, f32, f32, f32, f32)]) {
    crate::dom::with_document_mut(|document| {
        for &(node_id, x, y, width, height) in rects {
            document.set_layout_rect(
                NodeId::from_u32(node_id),
                w3cos_dom::DOMRect::new(x, y, width, height),
            );
        }
    });
}

fn single_px_width(styles: &[(u32, w3cos_std::style::Style)]) -> Option<(u32, u32)> {
    if styles.len() != 1 {
        return None;
    }
    match styles[0].1.width {
        Dimension::Px(px) if px.is_finite() => Some((styles[0].0, px.to_bits())),
        _ => None,
    }
}

fn remember_width(
    cache: &mut Cache,
    node: u32,
    bits: u32,
    viewport_w: f32,
    viewport_h: f32,
    rects: Vec<(u32, f32, f32, f32, f32)>,
) {
    let viewport_w_bits = viewport_w.to_bits();
    let viewport_h_bits = viewport_h.to_bits();
    if cache.width_memos.iter().any(|memo| {
        memo.structure == cache.structure
            && memo.node == node
            && memo.width_bits == bits
            && memo.viewport_w_bits == viewport_w_bits
            && memo.viewport_h_bits == viewport_h_bits
    }) {
        return;
    }
    if cache.width_memos.len() >= 8 {
        cache.width_memos.remove(0);
    }
    cache.width_memos.push(WidthMemo {
        structure: cache.structure,
        node,
        width_bits: bits,
        viewport_w_bits,
        viewport_h_bits,
        rects,
    });
}

fn memos_for_tree(
    component: &Component,
    structure: u64,
    viewport_w: f32,
    viewport_h: f32,
    rects: Vec<(u32, f32, f32, f32, f32)>,
) -> Vec<WidthMemo> {
    let mut nodes = Vec::new();
    collect_px_widths(component, &mut nodes);
    nodes.truncate(8);
    let viewport_w_bits = viewport_w.to_bits();
    let viewport_h_bits = viewport_h.to_bits();
    nodes
        .into_iter()
        .map(|(node, width_bits)| WidthMemo {
            structure,
            node,
            width_bits,
            viewport_w_bits,
            viewport_h_bits,
            rects: rects.clone(),
        })
        .collect()
}

fn collect_px_widths(component: &Component, out: &mut Vec<(u32, u32)>) {
    if let Some(id) = host_id(component)
        && let Dimension::Px(px) = component.style.width
        && px.is_finite()
    {
        out.push((id, px.to_bits()));
    }
    for child in &component.children {
        collect_px_widths(child, out);
    }
}

fn host_id(component: &Component) -> Option<u32> {
    match component.on_click {
        EventAction::NativeHost { id, .. } => u32::try_from(id).ok(),
        _ => None,
    }
}

fn component_style(component: &Component, id: u32) -> Option<&w3cos_std::style::Style> {
    if host_id(component) == Some(id) {
        return Some(&component.style);
    }
    component
        .children
        .iter()
        .find_map(|child| component_style(child, id))
}

/// Width and height edits can reuse the Taffy tree. Font, line box, and
/// formatting-context edits restyle descendants, so those rebuild.
fn geometry_only_change(old: &w3cos_std::style::Style, new: &w3cos_std::style::Style) -> bool {
    old.display == new.display
        && old.position == new.position
        && old.float == new.float
        && old.clear == new.clear
        && old.flex_direction == new.flex_direction
        && old.flex_wrap == new.flex_wrap
        && old.font_size == new.font_size
        && old.font_weight == new.font_weight
        && old.font_family == new.font_family
        && old.font_style == new.font_style
        && old.line_height == new.line_height
        && old.line_height_is_normal == new.line_height_is_normal
        && old.line_height_computed_px == new.line_height_computed_px
        && old.white_space == new.white_space
        && old.word_break == new.word_break
        && old.letter_spacing == new.letter_spacing
        && old.direction == new.direction
}

fn assign_layout_style(component: &mut Component, id: u32, style: &w3cos_std::style::Style) -> bool {
    if host_id(component) == Some(id) {
        component.style = style.clone();
        return true;
    }
    component
        .children
        .iter_mut()
        .any(|child| assign_layout_style(child, id, style))
}

fn style_patch_is_safe(style: &w3cos_std::style::Style) -> bool {
    !matches!(style.height, Dimension::Percent(_))
        && style.float == Float::None
        && !is_table(style.display)
}

fn is_table(display: Display) -> bool {
    matches!(
        display,
        Display::Table
            | Display::InlineTable
            | Display::TableRow
            | Display::TableRowGroup
            | Display::TableHeaderGroup
            | Display::TableFooterGroup
            | Display::TableColumn
            | Display::TableColumnGroup
            | Display::TableCell
            | Display::TableCaption
    )
}

fn configure_engine(engine: &mut LayoutEngine, root: &Component) {
    engine.set_skip_projections(can_skip_projections(root));
    engine.set_measure_text(needs_text_metric(root));
}

/// Px and out-of-flow boxes do not need a second text measure. An auto-height
/// text leaf does, even when the projection walk is skipped.
fn needs_text_metric(component: &Component) -> bool {
    if matches!(component.style.position, Position::Absolute | Position::Fixed) {
        return false;
    }
    if matches!(component.style.height, Dimension::Px(_)) {
        return false;
    }
    if matches!(component.kind, ComponentKind::Text { .. }) {
        return true;
    }
    component.children.iter().any(needs_text_metric)
}

/// Post-Taffy projections repair floats, tables, and inline-block struts.
/// Text-driven auto heights are measured in `update_text_leaf_heights` and do
/// not by themselves need that projection walk.
fn can_skip_projections(root: &Component) -> bool {
    fn walk(component: &Component) -> bool {
        let style = &component.style;
        if style.float != Float::None
            || is_table(style.display)
            || style.column_count.is_some()
            || style.direction == TextDirection::Rtl
            || matches!(style.display, Display::InlineBlock | Display::InlineFlex)
        {
            return false;
        }
        component.children.iter().all(walk)
    }
    walk(root)
}
