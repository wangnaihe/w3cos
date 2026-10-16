//! Persistent component + Taffy tree for geometry flushes.
//!
//! Style-only edits patch the cached nodes and call `compute_layout` again.
//! Tree shape, text metrics, and inherited cascades rebuild once.

use std::cell::{Cell, RefCell};

use w3cos_dom::node::NodeId;
use w3cos_std::component::EventAction;
use w3cos_std::style::{Dimension, Display, Float, Overflow, Position, TextDirection};
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
    PAINT.with(|cache| *cache.borrow_mut() = None);
    PAINT_REUSE.with(|kind| kind.set(PaintReuse::None));
    #[cfg(any(feature = "skia", feature = "cpu-render"))]
    crate::headless::invalidate_retained_frame();
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
    let (structure, cascade) = crate::dom::with_document(|document| {
        (
            document.structure_generation(),
            document.cascade_generation(),
        )
    });
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
        if !cache
            .engine
            .patch_style(index, style, viewport_w, viewport_h)
        {
            return false;
        }
    }
    let Ok(results) = cache
        .engine
        .compute(&cache.component, &flat, viewport_w, viewport_h)
    else {
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
    let Ok(results) = cache
        .engine
        .compute(&cache.component, &flat, viewport_w, viewport_h)
    else {
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

fn assign_layout_style(
    component: &mut Component,
    id: u32,
    style: &w3cos_std::style::Style,
) -> bool {
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
    if matches!(
        component.style.position,
        Position::Absolute | Position::Fixed
    ) {
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

struct PaintCache {
    component: Component,
    engine: LayoutEngine,
    structure: u64,
    cascade: u64,
    viewport_w_bits: u32,
    viewport_h_bits: u32,
    node_ids: Vec<u32>,
    results: crate::layout::LayoutResults,
    /// Preorder child lists captured with the retained tree.
    children: Vec<Vec<usize>>,
    /// Absolute or fixed leaves whose used position is a px inset delta.
    shift_ok: Vec<bool>,
}

/// Why the next headless paint can keep its artifact and only move rects.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaintReuse {
    None,
    /// Scroll offset changed. Layout rects are the ones already painted.
    Scroll,
    /// Only px insets changed, so paint order and styles stay valid.
    Insets,
}

thread_local! {
    static PAINT: RefCell<Option<PaintCache>> = const { RefCell::new(None) };
    static PAINT_REUSE: Cell<PaintReuse> = const { Cell::new(PaintReuse::None) };
}

pub(crate) fn take_paint_reuse() -> PaintReuse {
    PAINT_REUSE.with(|kind| kind.replace(PaintReuse::None))
}

fn note_paint_reuse(kind: PaintReuse) {
    PAINT_REUSE.with(|slot| slot.set(kind));
}

#[cfg(test)]
thread_local! {
    static PAINT_REBUILDS: Cell<u32> = const { Cell::new(0) };
    static PAINT_PATCHES: Cell<u32> = const { Cell::new(0) };
    static PAINT_SHIFTS: Cell<u32> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn paint_layout_stats() -> (u32, u32) {
    (
        PAINT_REBUILDS.with(Cell::get),
        PAINT_PATCHES.with(Cell::get),
    )
}

#[cfg(test)]
pub(crate) fn paint_shift_count() -> u32 {
    PAINT_SHIFTS.with(Cell::get)
}

#[cfg(test)]
fn note_paint_rebuild() {
    PAINT_REBUILDS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(test)]
fn note_paint_patch() {
    PAINT_PATCHES.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(test)]
fn note_paint_shift() {
    PAINT_SHIFTS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(test))]
fn note_paint_rebuild() {}

#[cfg(not(test))]
fn note_paint_patch() {}

#[cfg(not(test))]
fn note_paint_shift() {}

/// Layout for a headless frame.
///
/// Scroll-only frames reuse the previous solve. Edits that change only
/// used-box geometry patch the retained Taffy nodes. Inherited style, tree
/// shape, and anything `style_patch_is_safe` rejects rebuild from the DOM so
/// the painted styles stay the ones `to_component_tree` would produce.
pub fn with_paint_layout<R>(
    viewport_w: f32,
    viewport_h: f32,
    use_layout: impl FnOnce(&Component, &crate::layout::LayoutResults) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    let (structure, cascade) = crate::dom::with_document(|document| {
        (
            document.structure_generation(),
            document.cascade_generation(),
        )
    });
    let dirty = crate::dom::with_document_mut(|document| document.take_precise_dirty());
    // A geometry read can set this and never paint. The paint that follows
    // records its own reason.
    note_paint_reuse(PaintReuse::None);
    PAINT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let reusable = slot.as_ref().is_some_and(|cache| {
            cache.structure == structure
                && cache.viewport_w_bits == viewport_w.to_bits()
                && cache.viewport_h_bits == viewport_h.to_bits()
        });
        if reusable {
            let cache = slot.as_mut().unwrap();
            if sync_paint_cache(cache, &dirty, cascade, viewport_w, viewport_h) {
                return use_layout(&cache.component, &cache.results);
            }
        }
        *slot = None;
        rebuild_paint_cache(&mut slot, structure, cascade, viewport_w, viewport_h)?;
        let cache = slot.as_ref().expect("paint layout rebuild stores a cache");
        use_layout(&cache.component, &cache.results)
    })
}

fn rebuild_paint_cache(
    slot: &mut Option<PaintCache>,
    structure: u64,
    cascade: u64,
    viewport_w: f32,
    viewport_h: f32,
) -> anyhow::Result<()> {
    let component = crate::dom::to_component_tree();
    let flat = pre_flatten(&component);
    let node_ids = flat.iter().map(flat_host_id).collect::<Vec<_>>();
    let mut engine = LayoutEngine::new();
    // Floats, tables, and inline-block need the post-Taffy projections. A
    // block/absolute tree already matches `compute_with_scroll_details`
    // without that walk, which is most of a retained frame.
    engine.set_skip_projections(can_skip_projections(&component));
    engine.set_measure_text(true);
    let results = engine.compute(&component, &flat, viewport_w, viewport_h)?;
    write_rects(&flat, &results.layout_cache);
    let children = child_lists(&flat);
    let shift_ok = absolute_shift_ok(&flat);
    *slot = Some(PaintCache {
        component,
        engine,
        structure,
        cascade,
        viewport_w_bits: viewport_w.to_bits(),
        viewport_h_bits: viewport_h.to_bits(),
        node_ids,
        results,
        children,
        shift_ok,
    });
    note_paint_rebuild();
    Ok(())
}

fn sync_paint_cache(
    cache: &mut PaintCache,
    dirty: &[NodeId],
    cascade: u64,
    viewport_w: f32,
    viewport_h: f32,
) -> bool {
    if dirty.is_empty() {
        if cache.cascade != cascade {
            return false;
        }
        note_paint_patch();
        note_paint_reuse(PaintReuse::Scroll);
        return true;
    }
    if let Some(applied) = try_shift_absolute_insets(cache, dirty, cascade, viewport_w, viewport_h)
    {
        return applied;
    }
    let mut styles = Vec::with_capacity(dirty.len());
    let mut insets_only = true;
    for id in dirty {
        let style = crate::dom::with_document(|document| document.computed_style_for(*id));
        let Some(old) = component_style(&cache.component, id.as_u32()) else {
            return false;
        };
        if !paint_geometry_edit(old, &style) || !style_patch_is_safe(&style) {
            return false;
        }
        insets_only &= paint_insets_only(old, &style);
        styles.push((id.as_u32(), style));
    }
    for (id, style) in &styles {
        if !assign_layout_style(&mut cache.component, *id, style) {
            return false;
        }
        let Some(index) = cache.node_ids.iter().position(|node| node == id) else {
            return false;
        };
        if !cache
            .engine
            .patch_style(index, style, viewport_w, viewport_h)
        {
            return false;
        }
    }
    let flat = pre_flatten(&cache.component);
    if flat.len() != cache.node_ids.len()
        || flat
            .iter()
            .zip(&cache.node_ids)
            .any(|(info, id)| flat_host_id(info) != *id)
    {
        return false;
    }
    // Inset edits do not change text wrap widths, so retained text metrics
    // stay valid. Width and padding edits measure again.
    cache.engine.set_measure_text(!insets_only);
    let Ok(results) = cache
        .engine
        .compute(&cache.component, &flat, viewport_w, viewport_h)
    else {
        cache.engine.set_measure_text(true);
        return false;
    };
    cache.engine.set_measure_text(true);
    write_rects(&flat, &results.layout_cache);
    cache.results = results;
    cache.cascade = cascade;
    note_paint_patch();
    if insets_only {
        note_paint_reuse(PaintReuse::Insets);
    }
    true
}

/// `Some(true)` applied a px shift. `Some(false)` mutated the cache and the
/// caller must rebuild. `None` left the cache untouched.
fn try_shift_absolute_insets(
    cache: &mut PaintCache,
    dirty: &[NodeId],
    cascade: u64,
    viewport_w: f32,
    viewport_h: f32,
) -> Option<bool> {
    let mut plan = Vec::with_capacity(dirty.len());
    for id in dirty {
        let style = crate::dom::with_document(|document| document.computed_style_for(*id));
        let Some(old) = component_style(&cache.component, id.as_u32()) else {
            return None;
        };
        let Some((dx, dy)) = absolute_px_delta(old, &style) else {
            return None;
        };
        if !style_patch_is_safe(&style) {
            return None;
        }
        let Some(flat_index) = cache.node_ids.iter().position(|node| *node == id.as_u32()) else {
            return None;
        };
        if !cache.shift_ok.get(flat_index).copied().unwrap_or(false) {
            return None;
        }
        if cache
            .results
            .scroll_ancestor
            .get(flat_index)
            .copied()
            .flatten()
            .is_some()
        {
            return None;
        }
        plan.push((id.as_u32(), style, flat_index, dx, dy));
    }
    if plan.is_empty() {
        return None;
    }
    for (id, style, index, _, _) in &plan {
        if !assign_insets(&mut cache.component, *id, style.top, style.left) {
            return Some(false);
        }
        if !cache
            .engine
            .patch_style(*index, style, viewport_w, viewport_h)
        {
            return Some(false);
        }
    }
    for (_, _, index, dx, dy) in &plan {
        if !shift_descendants(
            &mut cache.results.layout_cache,
            &cache.children,
            *index,
            *dx,
            *dy,
        ) {
            return Some(false);
        }
    }
    write_host_rects(&cache.node_ids, &cache.results.layout_cache);
    cache.cascade = cascade;
    note_paint_patch();
    note_paint_shift();
    note_paint_reuse(PaintReuse::Insets);
    Some(true)
}

fn child_lists(flat: &[crate::layout::FlatNodeInfo<'_>]) -> Vec<Vec<usize>> {
    let mut children = vec![Vec::new(); flat.len()];
    for (index, info) in flat.iter().enumerate() {
        if let Some(parent) = info.parent {
            children[parent].push(index);
        }
    }
    children
}

fn clips_overflow(style: &w3cos_std::style::Style) -> bool {
    fn clips(overflow: Overflow) -> bool {
        !matches!(overflow, Overflow::Visible)
    }
    clips(style.overflow)
        || style.overflow_x.is_some_and(clips)
        || style.overflow_y.is_some_and(clips)
}

/// Absolute and fixed boxes can be moved by a px inset delta when their
/// subtree has no scrollport, transform, float, table, or nested fixed box.
fn absolute_shift_ok(flat: &[crate::layout::FlatNodeInfo<'_>]) -> Vec<bool> {
    let mut ok = vec![true; flat.len()];
    for (index, info) in flat.iter().enumerate() {
        let style = info.style;
        let blocks_subtree = !style.transform.is_identity()
            || style.filter.is_some()
            || clips_overflow(style)
            || is_table(style.display)
            || style.float != Float::None
            || style.position == Position::Sticky
            || style.column_count.is_some();
        if blocks_subtree {
            let mut cursor = Some(index);
            while let Some(node) = cursor {
                ok[node] = false;
                cursor = flat[node].parent;
            }
        }
        if style.position == Position::Fixed {
            let mut cursor = info.parent;
            while let Some(node) = cursor {
                ok[node] = false;
                cursor = flat[node].parent;
            }
        }
    }
    for (index, info) in flat.iter().enumerate() {
        if !matches!(info.style.position, Position::Absolute | Position::Fixed) {
            ok[index] = false;
        }
    }
    ok
}

fn absolute_px_delta(
    old: &w3cos_std::style::Style,
    new: &w3cos_std::style::Style,
) -> Option<(f32, f32)> {
    if !matches!(old.position, Position::Absolute | Position::Fixed) || old.position != new.position
    {
        return None;
    }
    if old.right != new.right
        || old.bottom != new.bottom
        || old.width != new.width
        || old.height != new.height
    {
        return None;
    }
    if !matches!(old.right, Dimension::Auto) || !matches!(old.bottom, Dimension::Auto) {
        return None;
    }
    let Dimension::Px(old_width) = old.width else {
        return None;
    };
    let Dimension::Px(old_height) = old.height else {
        return None;
    };
    if !old_width.is_finite() || !old_height.is_finite() {
        return None;
    }
    let (Dimension::Px(old_top), Dimension::Px(new_top)) = (old.top, new.top) else {
        return None;
    };
    let (Dimension::Px(old_left), Dimension::Px(new_left)) = (old.left, new.left) else {
        return None;
    };
    if ![old_top, new_top, old_left, new_left]
        .iter()
        .all(|value| value.is_finite())
    {
        return None;
    }
    let mut adjusted = old.clone();
    adjusted.top = new.top;
    adjusted.left = new.left;
    (adjusted == *new).then_some((new_left - old_left, new_top - old_top))
}

fn assign_insets(component: &mut Component, id: u32, top: Dimension, left: Dimension) -> bool {
    if host_id(component) == Some(id) {
        component.style.top = top;
        component.style.left = left;
        return true;
    }
    component
        .children
        .iter_mut()
        .any(|child| assign_insets(child, id, top, left))
}

fn shift_descendants(
    layouts: &mut [(crate::layout::LayoutRect, usize)],
    children: &[Vec<usize>],
    index: usize,
    dx: f32,
    dy: f32,
) -> bool {
    let mut stack = vec![index];
    let mut seen = vec![false; children.len()];
    while let Some(node) = stack.pop() {
        if node >= seen.len() || seen[node] {
            continue;
        }
        seen[node] = true;
        let mut found = false;
        for (rect, layout_index) in layouts.iter_mut() {
            if *layout_index == node {
                rect.x += dx;
                rect.y += dy;
                found = true;
            }
        }
        if !found {
            return false;
        }
        if let Some(kids) = children.get(node) {
            stack.extend(kids.iter().copied());
        }
    }
    true
}

fn write_host_rects(node_ids: &[u32], layouts: &[(crate::layout::LayoutRect, usize)]) {
    crate::dom::with_document_mut(|document| {
        for (rect, index) in layouts {
            let Some(node_id) = node_ids.get(*index).copied() else {
                continue;
            };
            if node_id == u32::MAX {
                continue;
            }
            document.set_layout_rect(
                NodeId::from_u32(node_id),
                w3cos_dom::DOMRect::new(rect.x, rect.y, rect.width, rect.height),
            );
        }
    });
}

fn flat_host_id(info: &crate::layout::FlatNodeInfo<'_>) -> u32 {
    match info.on_click {
        EventAction::NativeHost { id, .. } => u32::try_from(*id).unwrap_or(u32::MAX),
        _ => u32::MAX,
    }
}

/// True when `new` differs from `old` only in fields the retained Taffy
/// style can absorb. Inherited paint fields (color, font, writing mode)
/// stay in the comparison, so those edits rebuild the component tree.
fn paint_insets_only(old: &w3cos_std::style::Style, new: &w3cos_std::style::Style) -> bool {
    if old == new {
        return true;
    }
    let mut adjusted = old.clone();
    adjusted.top = new.top;
    adjusted.right = new.right;
    adjusted.bottom = new.bottom;
    adjusted.left = new.left;
    adjusted == *new
}

fn paint_geometry_edit(old: &w3cos_std::style::Style, new: &w3cos_std::style::Style) -> bool {
    if old == new {
        return true;
    }
    let mut left = old.clone();
    let mut right = new.clone();
    neutralize_paint_geometry(&mut left);
    neutralize_paint_geometry(&mut right);
    left == right
}

fn neutralize_paint_geometry(style: &mut w3cos_std::style::Style) {
    use w3cos_std::style::{AlignSelf, Edges};
    style.top = Dimension::Auto;
    style.right = Dimension::Auto;
    style.bottom = Dimension::Auto;
    style.left = Dimension::Auto;
    style.width = Dimension::Auto;
    style.height = Dimension::Auto;
    style.min_width = Dimension::Auto;
    style.min_height = Dimension::Auto;
    style.max_width = Dimension::Auto;
    style.max_height = Dimension::Auto;
    style.margin = Edges::ZERO;
    style.padding = Edges::ZERO;
    style.flex_grow = 0.0;
    style.flex_shrink = 0.0;
    style.flex_basis = Dimension::Auto;
    style.gap = 0.0;
    style.row_gap = None;
    style.column_gap = None;
    style.order = 0;
    style.z_index = 0;
    style.align_self = AlignSelf::Auto;
}

#[cfg(test)]
mod paint_layout_tests {
    use super::*;
    use w3cos_dom::node::NodeId;

    fn host_rect(
        component: &Component,
        results: &crate::layout::LayoutResults,
        id: u32,
    ) -> crate::layout::LayoutRect {
        let flat = pre_flatten(component);
        results
            .layout_cache
            .iter()
            .find_map(|(rect, index)| {
                let info = flat.get(*index)?;
                (flat_host_id(info) == id).then_some(*rect)
            })
            .unwrap_or_else(|| panic!("node {id} has no layout rect"))
    }

    fn paint_rect(id: u32) -> crate::layout::LayoutRect {
        with_paint_layout(1024.0, 768.0, |component, results| {
            Ok(host_rect(component, results, id))
        })
        .expect("paint layout")
    }

    fn fresh_rect(id: u32) -> crate::layout::LayoutRect {
        let component = crate::dom::to_component_tree();
        let results = crate::layout::compute_with_scroll_details(&component, 1024.0, 768.0)
            .expect("full layout");
        host_rect(&component, &results, id)
    }

    fn component_color(component: &Component, id: u32) -> Option<w3cos_std::color::Color> {
        if host_id(component) == Some(id) {
            return Some(component.style.color);
        }
        component
            .children
            .iter()
            .find_map(|child| component_color(child, id))
    }

    #[test]
    fn absolute_top_patches_and_matches_full_layout() {
        crate::dom::reset_document();
        let layer = crate::dom::create_element("div");
        crate::dom::set_style_property(layer, "position", "relative");
        crate::dom::set_style_property(layer, "width", "400px");
        crate::dom::set_style_property(layer, "height", "200px");
        let box_node = crate::dom::create_element("div");
        for (name, value) in [
            ("position", "absolute"),
            ("width", "16px"),
            ("height", "16px"),
            ("left", "8px"),
            ("top", "0px"),
            ("background", "#3366ff"),
        ] {
            crate::dom::set_style_property(box_node, name, value);
        }
        crate::dom::append_child(layer, box_node);
        crate::dom::append_child(crate::dom::body_id(), layer);

        let (rebuilds, patches) = paint_layout_stats();
        let shifts = paint_shift_count();
        let first = paint_rect(box_node);
        let first_fresh = fresh_rect(box_node);
        assert!(
            (first.x - first_fresh.x).abs() < 0.05
                && (first.y - first_fresh.y).abs() < 0.05
                && (first.width - first_fresh.width).abs() < 0.05
                && (first.height - first_fresh.height).abs() < 0.05,
            "first retained rect {first:?} diverges from full layout {first_fresh:?}"
        );
        crate::dom::set_style_property(box_node, "top", "40px");
        let second = paint_rect(box_node);
        let (rebuilds_after, patches_after) = paint_layout_stats();
        assert_eq!(rebuilds_after - rebuilds, 1, "first frame builds the tree");
        assert_eq!(
            patches_after - patches,
            1,
            "absolute top must patch, not rebuild"
        );
        assert_eq!(
            paint_shift_count() - shifts,
            1,
            "a px absolute top skips another Taffy solve"
        );
        assert!(
            second.y > first.y + 30.0,
            "top 40px must move the box, {first:?} -> {second:?}"
        );
        let fresh = fresh_rect(box_node);
        assert!(
            (second.x - fresh.x).abs() < 0.05
                && (second.y - fresh.y).abs() < 0.05
                && (second.width - fresh.width).abs() < 0.05
                && (second.height - fresh.height).abs() < 0.05,
            "retained rect {second:?} diverges from full layout {fresh:?}"
        );
    }

    #[test]
    fn absolute_shift_moves_the_static_child_with_the_parent() {
        crate::dom::reset_document();
        let parent = crate::dom::create_element("div");
        for (name, value) in [
            ("position", "absolute"),
            ("width", "100px"),
            ("height", "80px"),
            ("left", "4px"),
            ("top", "0px"),
        ] {
            crate::dom::set_style_property(parent, name, value);
        }
        let child = crate::dom::create_element("div");
        crate::dom::set_style_property(child, "width", "10px");
        crate::dom::set_style_property(child, "height", "12px");
        crate::dom::append_child(parent, child);
        crate::dom::append_child(crate::dom::body_id(), parent);
        let before = paint_rect(child);
        let shifts = paint_shift_count();
        crate::dom::set_style_property(parent, "top", "50px");
        let after = paint_rect(child);
        let fresh = fresh_rect(child);
        assert_eq!(paint_shift_count() - shifts, 1);
        assert!(
            after.y > before.y + 40.0,
            "child must move with the absolute parent, {before:?} -> {after:?}"
        );
        assert!(
            (after.x - fresh.x).abs() < 0.05
                && (after.y - fresh.y).abs() < 0.05
                && (after.width - fresh.width).abs() < 0.05
                && (after.height - fresh.height).abs() < 0.05,
            "shifted child {after:?} diverges from full layout {fresh:?}"
        );
    }

    #[test]
    fn relative_top_does_not_use_the_absolute_shift() {
        crate::dom::reset_document();
        let first = crate::dom::create_element("div");
        crate::dom::set_style_property(first, "position", "relative");
        crate::dom::set_style_property(first, "width", "40px");
        crate::dom::set_style_property(first, "height", "20px");
        let second = crate::dom::create_element("div");
        crate::dom::set_style_property(second, "width", "40px");
        crate::dom::set_style_property(second, "height", "20px");
        crate::dom::append_child(crate::dom::body_id(), first);
        crate::dom::append_child(crate::dom::body_id(), second);
        let _ = paint_rect(second);
        let shifts = paint_shift_count();
        crate::dom::set_style_property(first, "top", "30px");
        let moved = paint_rect(second);
        let fresh = fresh_rect(second);
        assert_eq!(
            paint_shift_count(),
            shifts,
            "relative top still reflows following siblings"
        );
        assert!(
            (moved.x - fresh.x).abs() < 0.05
                && (moved.y - fresh.y).abs() < 0.05
                && (moved.width - fresh.width).abs() < 0.05
                && (moved.height - fresh.height).abs() < 0.05,
            "relative top {moved:?} diverges from full layout {fresh:?}"
        );
    }

    #[test]
    fn width_after_an_absolute_shift_matches_full_layout() {
        crate::dom::reset_document();
        let box_node = crate::dom::create_element("div");
        for (name, value) in [
            ("position", "absolute"),
            ("width", "16px"),
            ("height", "16px"),
            ("left", "8px"),
            ("top", "0px"),
        ] {
            crate::dom::set_style_property(box_node, name, value);
        }
        crate::dom::append_child(crate::dom::body_id(), box_node);
        let _ = paint_rect(box_node);
        crate::dom::set_style_property(box_node, "top", "40px");
        let _ = paint_rect(box_node);
        let shifts = paint_shift_count();
        crate::dom::set_style_property(box_node, "width", "40px");
        let rect = paint_rect(box_node);
        let fresh = fresh_rect(box_node);
        assert_eq!(
            paint_shift_count(),
            shifts,
            "a width edit is not an inset shift"
        );
        assert!(
            (rect.x - fresh.x).abs() < 0.05
                && (rect.y - fresh.y).abs() < 0.05
                && (rect.width - fresh.width).abs() < 0.05
                && (rect.height - fresh.height).abs() < 0.05,
            "width after shift {rect:?} diverges from full layout {fresh:?}"
        );
    }

    #[test]
    fn inherited_color_rebuilds_descendant_style() {
        crate::dom::reset_document();
        let parent = crate::dom::create_element("div");
        crate::dom::set_style_property(parent, "color", "red");
        let child = crate::dom::create_element("div");
        crate::dom::set_style_property(child, "width", "20px");
        crate::dom::set_style_property(child, "height", "20px");
        crate::dom::append_child(parent, child);
        crate::dom::append_child(crate::dom::body_id(), parent);

        let _ = paint_rect(child);
        let (rebuilds, patches) = paint_layout_stats();
        crate::dom::set_style_property(parent, "color", "blue");
        let painted = with_paint_layout(1024.0, 768.0, |component, _| {
            Ok(component_color(component, child).expect("child style"))
        })
        .expect("paint");
        let expected = crate::dom::with_document(|document| {
            document.computed_style_for(NodeId::from_u32(child)).color
        });
        let (rebuilds_after, patches_after) = paint_layout_stats();
        assert_eq!(painted, expected, "descendant color must follow the parent");
        assert_eq!(rebuilds_after - rebuilds, 1, "inherited color must rebuild");
        assert_eq!(patches_after, patches);
    }

    #[test]
    fn appended_box_is_in_the_next_paint_layout() {
        crate::dom::reset_document();
        let first = crate::dom::create_element("div");
        crate::dom::set_style_property(first, "width", "10px");
        crate::dom::set_style_property(first, "height", "10px");
        crate::dom::append_child(crate::dom::body_id(), first);
        let _ = paint_rect(first);
        let (rebuilds, _) = paint_layout_stats();
        let second = crate::dom::create_element("div");
        crate::dom::set_style_property(second, "width", "12px");
        crate::dom::set_style_property(second, "height", "14px");
        crate::dom::append_child(crate::dom::body_id(), second);
        let rect = paint_rect(second);
        let (rebuilds_after, _) = paint_layout_stats();
        assert!(rect.width > 10.0 && rect.height > 12.0, "{rect:?}");
        assert_eq!(rebuilds_after - rebuilds, 1, "tree shape must rebuild");
    }
}
