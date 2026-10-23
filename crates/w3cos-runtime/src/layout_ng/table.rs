//! Table formatting context.
//!
//! The column and row geometry is the legacy engine's (CSS 2.1 §17, the
//! same track and projection passes). LayoutNG copies those border boxes
//! into the fragment tree. A table this stage does not place — inline-table,
//! a flex or grid item, a float beside it, rtl, auto inline margins, a
//! scrollport, or a sticky box — stays on the legacy engine for the whole
//! page.

use std::cell::Cell;

use super::block::{self, is_author_flex, is_author_grid};
use super::inline::{self, VALIGN_LENGTH_MARKER, VALIGN_MARKER};
use super::oof;
use super::{BoxFragment, ConstraintSpace, LastBaseline, LayoutResult, MarginStrut};
use crate::layout::{FlatNodeInfo, LayoutRect};
use w3cos_std::Component;
use w3cos_std::style::{
    Dimension, Display, Float, Overflow, Position, Spacing, Style, TextDirection,
};

const TABLE_MARKERS: &[&str] = &[
    "--w3cos-internal-html-table-element",
    "--w3cos-internal-border-spacing-source",
    "--w3cos-internal-table-cell-ua-padding",
    "--w3cos-internal-table-row-span",
    "--w3cos-internal-table-column-span",
    "--w3cos-internal-table-column-start",
    "--w3cos-internal-anonymous-table",
    VALIGN_MARKER,
    VALIGN_LENGTH_MARKER,
];

thread_local! {
    /// The component `layout_tree_with_text` is laying out. Valid only for
    /// that call.
    static ROOT: Cell<*const Component> = const { Cell::new(std::ptr::null()) };
}

pub(super) struct RootGuard;

impl Drop for RootGuard {
    fn drop(&mut self) {
        ROOT.with(|slot| slot.set(std::ptr::null()));
    }
}

pub(super) fn bind_root(root: &Component) -> RootGuard {
    ROOT.with(|slot| slot.set(root as *const Component));
    RootGuard
}

pub(super) fn is_table_display(display: Display) -> bool {
    matches!(
        display,
        Display::Table
            | Display::TableRow
            | Display::TableRowGroup
            | Display::TableHeaderGroup
            | Display::TableFooterGroup
            | Display::TableCell
            | Display::TableCaption
            | Display::TableColumn
            | Display::TableColumnGroup
    )
}

fn ancestor_table(flat: &[FlatNodeInfo], index: usize) -> Option<usize> {
    let mut current = Some(index);
    while let Some(at) = current {
        if flat[at].style.display == Display::Table {
            return Some(at);
        }
        current = flat[at].parent;
    }
    None
}

/// `None` when this table box can be transplanted. Rows and cells follow
/// their table.
pub(super) fn reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let Some(table) = ancestor_table(flat, index) else {
        return Some("table");
    };
    if let Some(reason) = block::unsupported_marker(flat[index].style, TABLE_MARKERS) {
        return Some(reason);
    }
    structural_reason(flat, table)
}

fn structural_reason(flat: &[FlatNodeInfo], table: usize) -> Option<&'static str> {
    let style = flat[table].style;
    if style.direction == TextDirection::Rtl
        || style.position != Position::Static
        || style.clear != w3cos_std::style::Clear::None
        || matches!(style.margin.left, Spacing::Auto)
        || matches!(style.margin.right, Spacing::Auto)
        || style.column_count.is_some()
        || style.column_width != Dimension::Auto
    {
        return Some("table");
    }
    if let Some(parent) = flat[table].parent {
        let parent_style = flat[parent].style;
        if is_author_flex(parent_style)
            || is_author_grid(parent_style)
            || inline::is_inline_container(flat, parent)
        {
            return Some("table");
        }
        let beside_float = inline::rendered_children(flat, parent).any(|child| {
            child != table
                && flat[child].style.float != Float::None
                && !oof::is_out_of_flow(flat[child].style)
        });
        if beside_float {
            return Some("table");
        }
    }
    // Scroll offsets are not in the transplanted border boxes.
    let end = flat[table].subtree_end;
    for node in &flat[table..end] {
        if node.style.display == Display::None {
            continue;
        }
        if node.style.position == Position::Sticky {
            return Some("table");
        }
        let scrollish = |overflow: Overflow| matches!(overflow, Overflow::Scroll | Overflow::Auto);
        if scrollish(node.style.resolved_overflow_x())
            || scrollish(node.style.resolved_overflow_y())
        {
            return Some("table");
        }
    }
    None
}

fn clone_at(index: usize) -> Option<Component> {
    ROOT.with(|slot| {
        // SAFETY: `bind_root` stores this pointer for the duration of
        // `layout_root`, and this function runs only from that layout.
        let root = unsafe { slot.get().as_ref() }?;
        let mut cursor = 0usize;
        fn walk(component: &Component, cursor: &mut usize, index: usize) -> Option<Component> {
            let here = *cursor;
            *cursor += 1;
            if here == index {
                return Some(component.clone());
            }
            for child in &component.children {
                if let Some(found) = walk(child, cursor, index) {
                    return Some(found);
                }
            }
            None
        }
        walk(root, &mut cursor, index)
    })
}

/// Legacy border boxes for `index`'s subtree, in page flat indices.
fn legacy_subtree(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<Vec<Option<LayoutRect>>> {
    let end = flat[index].subtree_end;
    let len = end - index;
    let (rects, base) = if flat[index].parent.is_none() {
        // The table is the document element. Its containing block is the
        // initial one, so the legacy root is the table itself.
        let root = clone_at(index)?;
        let legacy = crate::layout::legacy_layout_rects(&root, space.viewport_w, space.viewport_h)
            .why("table-legacy")?;
        (legacy, 0usize)
    } else {
        // A block of the table's containing-block size. Percentage widths
        // resolve against that content box, not the viewport.
        let table = clone_at(index)?;
        let wrapper_style = Style {
            display: Display::Block,
            width: Dimension::Px(space.available_inline.max(0.0)),
            height: match space.percentage_block {
                Some(height) => Dimension::Px(height.max(0.0)),
                None => Dimension::Auto,
            },
            ..Style::default()
        };
        let wrapper = Component::boxed(wrapper_style, vec![table]);
        let legacy =
            crate::layout::legacy_layout_rects(&wrapper, space.viewport_w, space.viewport_h)
                .why("table-legacy")?;
        // Local 0 is the wrapper. The table is local 1.
        (legacy, 1usize)
    };
    let mut placed = vec![None; len];
    for (rect, local) in rects {
        if local < base {
            continue;
        }
        let offset = local - base;
        if offset < len {
            placed[offset] = Some(rect);
        }
    }
    Some(placed)
}

fn build_fragment(
    flat: &[FlatNodeInfo],
    page: usize,
    placed: &[Option<LayoutRect>],
    origin: usize,
) -> Option<BoxFragment> {
    let abs = placed[page - origin]?;
    let mut children = Vec::new();
    for child in inline::rendered_children(flat, page) {
        if child < origin || child - origin >= placed.len() {
            continue;
        }
        let Some(child_abs) = placed[child - origin] else {
            // A rendered descendant the legacy pass did not box.
            return super::bail("table-missing");
        };
        let fragment = build_fragment(flat, child, placed, origin)?;
        children.push((child_abs.x - abs.x, child_abs.y - abs.y, fragment));
    }
    Some(BoxFragment {
        index: page,
        text_range: None,
        width: abs.width,
        height: abs.height,
        children,
    })
}

pub(super) fn layout(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<LayoutResult> {
    if space.rtl {
        return super::bail("table-rtl");
    }
    let placed = legacy_subtree(flat, index, space)?;
    let fragment = build_fragment(flat, index, &placed, index).why("table-fragment")?;
    // The parent's margin strut uses the same used margins as a block box.
    // Internal table margins do not escape (CSS 2.1 §8.3.1).
    let metrics = block::resolve_inline(flat[index].style, space);
    let mut start_strut = MarginStrut::default();
    start_strut.append(metrics.margin[0]);
    let mut end_strut = MarginStrut::default();
    end_strut.append(metrics.margin[2]);
    Some(LayoutResult {
        fragment,
        last_baseline: LastBaseline::NoLines,
        start_strut,
        end_strut,
        self_collapsing: false,
        oof: Vec::new(),
        bfc_dependent: false,
        float_overlap: false,
    })
}

trait WhyLegacy<T> {
    fn why(self, reason: &'static str) -> Option<T>;
}

impl<T> WhyLegacy<T> for Option<T> {
    fn why(self, reason: &'static str) -> Option<T> {
        super::Why::why(self, reason)
    }
}
