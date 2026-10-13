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

struct Cache {
    component: Component,
    engine: LayoutEngine,
    structure: u64,
    cascade: u64,
    node_ids: Vec<u32>,
}

thread_local! {
    static CACHE: RefCell<Option<Cache>> = const { RefCell::new(None) };
}

pub fn invalidate() {
    CACHE.with(|cache| *cache.borrow_mut() = None);
}

/// Flush pending layout into stored DOM rects. Returns false when Taffy fails.
pub fn flush() -> bool {
    let (viewport_w, viewport_h, _) = crate::jsdom::viewport();
    let viewport_w = viewport_w as f32;
    let viewport_h = viewport_h as f32;
    let (structure, cascade) =
        crate::dom::with_document(|document| (document.structure_generation(), document.cascade_generation()));
    let dirty = crate::dom::with_document_mut(|document| document.take_precise_dirty());
    let mut ok = false;
    CACHE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let reusable = slot
            .as_ref()
            .is_some_and(|cache| cache.structure == structure && cache.cascade == cascade);
        if reusable {
            let cache = slot.as_mut().unwrap();
            if patch_and_compute(cache, &dirty, viewport_w, viewport_h) {
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
    engine.set_skip_projections(can_skip_projections(&component));
    let Ok(results) = engine.compute(&component, &flat, viewport_w, viewport_h) else {
        return false;
    };
    write_rects(&flat, &results.layout_cache);
    *slot = Some(Cache {
        component,
        engine,
        structure,
        cascade,
        node_ids,
    });
    true
}

fn patch_and_compute(cache: &mut Cache, dirty: &[NodeId], viewport_w: f32, viewport_h: f32) -> bool {
    if dirty.is_empty() {
        return recompute(cache, viewport_w, viewport_h);
    }
    let mut styles = Vec::with_capacity(dirty.len());
    for id in dirty {
        let style = crate::dom::with_document(|document| document.computed_style_for(*id));
        if !style_patch_is_safe(&style) {
            return false;
        }
        styles.push((id.as_u32(), style));
    }
    for (id, style) in &styles {
        if !assign_layout_style(&mut cache.component, *id, style) {
            return false;
        }
    }
    let flat = pre_flatten(&cache.component);
    if flat.len() != cache.node_ids.len() {
        return false;
    }
    cache.engine.set_skip_projections(can_skip_projections(&cache.component));
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
    write_rects(&flat, &results.layout_cache);
    true
}

fn recompute(cache: &mut Cache, viewport_w: f32, viewport_h: f32) -> bool {
    let flat = pre_flatten(&cache.component);
    cache.engine.set_skip_projections(can_skip_projections(&cache.component));
    let Ok(results) = cache.engine.compute(&cache.component, &flat, viewport_w, viewport_h) else {
        return false;
    };
    write_rects(&flat, &results.layout_cache);
    true
}

fn write_rects(flat: &[crate::layout::FlatNodeInfo<'_>], layouts: &[(crate::layout::LayoutRect, usize)]) {
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
            document.set_layout_rect(
                NodeId::from_u32(node_id),
                w3cos_dom::DOMRect::new(rect.x, rect.y, rect.width, rect.height),
            );
        }
    });
}

fn host_id(component: &Component) -> Option<u32> {
    match component.on_click {
        EventAction::NativeHost { id, .. } => u32::try_from(id).ok(),
        _ => None,
    }
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

/// Post-Taffy projections repair floats, tables, and text-driven auto heights.
/// A tree of definite boxes (and flex/grid containers of those boxes) already
/// has its border boxes after `compute_layout`.
fn can_skip_projections(root: &Component) -> bool {
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
        if matches!(style.height, Dimension::Auto)
            && matches!(style.display, Display::Block | Display::FlowRoot | Display::ListItem)
            && needs_text_metric(component)
        {
            return false;
        }
        component.children.iter().all(walk)
    }
    walk(root)
}
