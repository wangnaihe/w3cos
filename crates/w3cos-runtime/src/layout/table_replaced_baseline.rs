//! Replaced content participates in cell baselines even without text glyphs.
//! Align before row-height settlement so an inline line's descent remains in
//! the grid, rather than compensating image positions during painting.
use super::*;

pub(super) fn project(
    layouts: &mut [(LayoutRect, usize)],
    root: &Component,
    flat: &[FlatNodeInfo<'_>],
    viewport_w: f32,
    viewport_h: f32,
) {
    if flat.iter().any(|node| {
        node.style.border_collapse
            && matches!(node.style.display, WDisplay::Table | WDisplay::InlineTable)
    }) {
        let mut resolved = root.clone();
        resolve_collapsed_table_layout_borders(&mut resolved);
        apply(layouts, &pre_flatten(&resolved), viewport_w, viewport_h);
    } else {
        apply(layouts, flat, viewport_w, viewport_h);
    }
}

fn descendant(flat: &[FlatNodeInfo<'_>], mut index: usize, cell: usize) -> bool {
    while let Some(parent) = flat[index].parent {
        if parent == cell {
            return true;
        }
        index = parent;
    }
    false
}

fn in_flow(flat: &[FlatNodeInfo<'_>], mut index: usize, cell: usize) -> bool {
    while index != cell {
        let style = flat[index].style;
        if style.display == WDisplay::None
            || style.float != WFloat::None
            || matches!(style.position, WPos::Absolute | WPos::Fixed)
        {
            return false;
        }
        let Some(parent) = flat[index].parent else {
            return false;
        };
        index = parent;
    }
    true
}

fn replaced(kind: &ComponentKind) -> bool {
    matches!(
        kind,
        ComponentKind::Image { .. }
            | ComponentKind::Canvas { .. }
            | ComponentKind::SvgDocument { .. }
    )
}

fn apply(
    layouts: &mut [(LayoutRect, usize)],
    flat: &[FlatNodeInfo<'_>],
    viewport_w: f32,
    viewport_h: f32,
) {
    let positions = layouts
        .iter()
        .enumerate()
        .map(|(p, (_, i))| (*i, p))
        .collect::<HashMap<_, _>>();
    for (row, node) in flat.iter().enumerate() {
        if node.style.display != WDisplay::TableRow {
            continue;
        }
        let cells = flat
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                node.parent == Some(row)
                    && node.style.display == WDisplay::TableCell
                    && matches!(
                        node.style.align_self,
                        WAlignSelf::Auto | WAlignSelf::Baseline
                    )
            })
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        if cells.len() < 2 {
            continue;
        }
        let flow_rect = |index: usize, cell: usize| -> Option<LayoutRect> {
            let mut rect = layouts[*positions.get(&index)?].0;
            let mut ancestor = index;
            while ancestor != cell {
                let parent = flat[ancestor].parent?;
                let parent_rect = layouts[*positions.get(&parent)?].0;
                rect.y -= relative_flow_offset(
                    flat[ancestor].style,
                    flat[parent].style,
                    parent_rect,
                    viewport_w,
                    viewport_h,
                )
                .1;
                ancestor = parent;
            }
            Some(rect)
        };
        let mut has_replaced = false;
        let baselines = cells
            .iter()
            .filter_map(|&cell| {
                let contents = flat
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| descendant(flat, *i, cell) && in_flow(flat, *i, cell))
                    .collect::<Vec<_>>();
                let images = contents
                    .iter()
                    .filter(|(_, node)| replaced(node.kind))
                    .collect::<Vec<_>>();
                has_replaced |= !images.is_empty();
                // Preserve the existing first text-line baseline for mixed cells.
                if let Some(&(index, text)) = contents.iter().find(|(_, node)| {
                    matches!(node.kind, ComponentKind::Text { content }
                    if content.chars().any(|c| !c.is_whitespace()))
                }) {
                    let rect = flow_rect(index, cell)?;
                    let top = if text.style.display == WDisplay::Inline {
                        rect.y
                    } else {
                        rect.y
                            + text.style.padding_lengths().top
                            + text
                                .style
                                .border_top_width
                                .unwrap_or(text.style.border_width)
                            + (text.style.font_size * text.style.line_height
                                - inline_font_height(text.style))
                                * 0.5
                    };
                    return Some((cell, top + inline_font_content_ascent(text.style)));
                }
                if let Some(&&(index, image)) = images.iter().find(|(_, image)| {
                    matches!(
                        image.style.display,
                        WDisplay::Inline | WDisplay::InlineBlock
                    ) && matches!(
                        image.style.align_self,
                        WAlignSelf::Auto | WAlignSelf::Baseline
                    )
                }) {
                    let rect = flow_rect(index, cell)?;
                    let offset = image
                        .style
                        .custom_properties
                        .as_ref()
                        .and_then(|p| p.get("--w3cos-internal-vertical-align-length"))
                        .and_then(|v| v.split_ascii_whitespace().next())
                        .and_then(|v| v.parse::<f32>().ok())
                        .unwrap_or(0.0);
                    return Some((
                        cell,
                        rect.y + rect.height + image.style.margin_lengths().bottom + offset,
                    ));
                }
                if images.is_empty() {
                    return None;
                }
                // Without an in-flow line, a cell exports its content bottom.
                // Measure actual children, not the row-stretched outer cell box.
                contents
                    .iter()
                    .filter(|(_, node)| node.parent == Some(cell))
                    .filter_map(|(index, node)| {
                        flow_rect(*index, cell)
                            .map(|r| r.y + r.height + node.style.margin_lengths().bottom)
                    })
                    .reduce(f32::max)
                    .map(|baseline| (cell, baseline))
            })
            .collect::<Vec<_>>();
        if !has_replaced {
            continue;
        }
        let Some(target) = baselines.iter().map(|(_, b)| *b).reduce(f32::max) else {
            continue;
        };
        for (cell, baseline) in baselines {
            let delta = target - baseline;
            if delta <= f32::EPSILON {
                continue;
            }
            for (rect, index) in layouts.iter_mut() {
                if descendant(flat, *index, cell) && in_flow(flat, *index, cell) {
                    rect.y += delta;
                }
            }
        }
    }
}
