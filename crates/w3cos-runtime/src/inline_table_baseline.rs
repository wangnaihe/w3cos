//! Baselines exported by the first row of an atomic inline table.
use std::collections::HashMap;
use w3cos_std::{Component, ComponentKind};
use w3cos_std::style::{Display, Float, Position};
use crate::layout::{LayoutRect, count_nodes, inline_font_content_ascent};

pub(crate) fn first_row(component: &Component, index: usize,
    layouts: &[(LayoutRect, usize)], positions: &HashMap<usize, usize>) -> Option<f32> {
    fn in_flow(component: &Component) -> bool {
        component.style.display != Display::None && component.style.float == Float::None
            && !matches!(component.style.position, Position::Absolute | Position::Fixed)
    }
    fn content(component: &Component, index: usize, layouts: &[(LayoutRect, usize)],
        positions: &HashMap<usize, usize>) -> Option<f32> {
        if !in_flow(component) { return None; }
        let rect = layouts[*positions.get(&index)?].0;
        if let ComponentKind::Text { content } = &component.kind {
            if content.chars().any(|c| !c.is_whitespace()) {
                return Some(rect.y + inline_font_content_ascent(&component.style));
            }
        }
        if component.children.is_empty() && component.style.display == Display::InlineBlock {
            return Some(rect.y + rect.height + component.style.margin_lengths().bottom);
        }
        if matches!(component.style.display, Display::Table | Display::InlineTable) {
            return first_row(component, index, layouts, positions);
        }
        let mut child_index = index + 1;
        let mut baseline: Option<f32> = None;
        let mut first_top: Option<f32> = None;
        for child in &component.children {
            let start = child_index;
            child_index += count_nodes(child);
            if !in_flow(child) { continue; }
            let Some(&position) = positions.get(&start) else { continue; };
            let top = layouts[position].0.y;
            // A cell exports its first line; an inline-block exports its
            // last in-flow line. Do not include later block rows in a cell.
            if component.style.display != Display::InlineBlock
                && first_top.is_some_and(|first| top > first + 0.01) { break; }
            if let Some(value) = content(child, start, layouts, positions) {
                first_top.get_or_insert(top);
                baseline = Some(baseline.map_or(value, |old| old.max(value)));
            }
        }
        baseline.or_else(|| (component.style.display == Display::TableCell).then(|| {
            let border = component.style.border_bottom_width.unwrap_or(component.style.border_width);
            rect.y + rect.height - component.style.padding_lengths().bottom
                - border * if component.style.border_collapse { 0.5 } else { 1.0 }
        }))
    }
    fn row(component: &Component, index: usize, layouts: &[(LayoutRect, usize)],
        positions: &HashMap<usize, usize>) -> Option<f32> {
        let mut child_index = index + 1;
        let mut baseline: Option<f32> = None;
        for child in &component.children {
            let start = child_index;
            child_index += count_nodes(child);
            if child.style.display != Display::TableCell || !in_flow(child) { continue; }
            if let Some(value) = content(child, start, layouts, positions) {
                baseline = Some(baseline.map_or(value, |old| old.max(value)));
            }
        }
        baseline
    }
    fn find(component: &Component, index: usize, layouts: &[(LayoutRect, usize)],
        positions: &HashMap<usize, usize>) -> Option<f32> {
        let mut child_index = index + 1;
        for child in &component.children {
            let start = child_index;
            child_index += count_nodes(child);
            if !in_flow(child) { continue; }
            if child.style.display == Display::TableRow { return row(child, start, layouts, positions); }
            if matches!(child.style.display, Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup)
                && let Some(baseline) = find(child, start, layouts, positions) { return Some(baseline); }
        }
        None
    }
    find(component, index, layouts, positions).or_else(|| {
        let rect = layouts[*positions.get(&index)?].0;
        Some(rect.y + rect.height + component.style.margin_lengths().bottom)
    })
}
