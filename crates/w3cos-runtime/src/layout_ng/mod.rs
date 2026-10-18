//! LayoutNG-style layout core.
//!
//! Structure follows Blink LayoutNG (third_party/blink/renderer/core/layout/,
//! BSD-style license, Copyright The Chromium Authors): one layout algorithm
//! per formatting context, each a function
//! `layout(node, &ConstraintSpace) -> LayoutResult` producing a box fragment
//! with child offsets; a margin strut carries collapsible margins between
//! siblings and through parents (NGMarginStrut).  Positions are made absolute
//! only when the fragment tree is flattened into the engine's
//! `(LayoutRect, flat index)` output, so paint, hit-testing and the CSSOM
//! geometry getters are unchanged.
//!
//! Rollout (see notes / PR): `W3COS_LAYOUT_NG=1` uses this engine for trees
//! it fully supports and falls back to the legacy Taffy + projection engine
//! otherwise; `W3COS_LAYOUT_NG_VERIFY=1` runs both on supported trees,
//! keeps the legacy output and reports every rect that differs.
//!
//! Stage S1: block flow without floats -- block-level boxes in normal flow,
//! margin collapsing (siblings, parent/first-child, parent/last-child,
//! empty boxes), auto/px/percent widths and heights, min/max sizes,
//! box-sizing, auto horizontal margins.  Anything else makes
//! `unsupported_tree` report a reason and the legacy engine runs.

mod block;
mod inline;

use crate::layout::{FlatNodeInfo, LayoutRect};
use std::sync::OnceLock;
use w3cos_std::Component;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Off,
    On,
    Verify,
}

pub(crate) fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| {
        if std::env::var_os("W3COS_LAYOUT_NG_VERIFY").is_some_and(|v| v != "0") {
            Mode::Verify
        } else if std::env::var_os("W3COS_LAYOUT_NG").is_some_and(|v| v != "0") {
            Mode::On
        } else {
            Mode::Off
        }
    })
}

/// `W3COS_LAYOUT_NG_TRACE=1`: log whether each compute used NG or fell back
/// (coverage accounting for the WPT gate).
fn trace() -> bool {
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("W3COS_LAYOUT_NG_TRACE").is_some_and(|v| v != "0"))
}

/// NGConstraintSpace: the input of every layout algorithm.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ConstraintSpace {
    /// Available inline size (the containing block's content width).
    pub available_inline: f32,
    /// Definite containing-block block size, if any (percent heights).
    pub percentage_block: Option<f32>,
    pub viewport_w: f32,
    pub viewport_h: f32,
}

/// NGMarginStrut: the positive and negative extremes of adjoining margins.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct MarginStrut {
    positive: f32,
    negative: f32,
}

impl MarginStrut {
    pub fn append(&mut self, margin: f32) {
        if margin >= 0.0 {
            self.positive = self.positive.max(margin);
        } else {
            self.negative = self.negative.min(margin);
        }
    }

    pub fn merge(&mut self, other: MarginStrut) {
        self.positive = self.positive.max(other.positive);
        self.negative = self.negative.min(other.negative);
    }

    pub fn sum(&self) -> f32 {
        self.positive + self.negative
    }
}

/// NGPhysicalBoxFragment (logical == physical in S1: horizontal-tb, ltr).
#[derive(Debug, Clone)]
pub(crate) struct BoxFragment {
    pub index: usize,
    pub width: f32,
    pub height: f32,
    /// Children with their border-box offset from this border box.
    pub children: Vec<(f32, f32, BoxFragment)>,
}

/// NGLayoutResult.
#[derive(Debug, Clone)]
pub(crate) struct LayoutResult {
    pub fragment: BoxFragment,
    /// Collapsible margins adjoining the top border edge (own margin-top,
    /// plus first-child margins when nothing separates them).  The parent
    /// places the border box after this strut has been resolved.
    pub start_strut: MarginStrut,
    /// Collapsible margins adjoining the bottom border edge.
    pub end_strut: MarginStrut,
    /// Top and bottom margins adjoin each other (CSS2 8.3.1 "collapse
    /// through"): start and end strut belong to one collapsed margin.
    pub self_collapsing: bool,
}

/// Entry point used by `LayoutEngine::compute`.  Returns `None` when the tree
/// is outside the supported stage, so the legacy engine runs.
pub(crate) fn layout_tree(
    root: &Component,
    flat: &[FlatNodeInfo],
    viewport_w: f32,
    viewport_h: f32,
) -> Option<Vec<(LayoutRect, usize)>> {
    let _ = root;
    let reason = if flat.is_empty() {
        Some("empty")
    } else {
        block::unsupported_tree(flat)
    };
    if trace() {
        match reason {
            None => eprintln!("[layout-ng] ng ({} boxes)", flat.len()),
            Some(_) => eprintln!(
                "[layout-ng] fallback {} ({} boxes)",
                block::unsupported_reasons(flat).join(","),
                flat.len()
            ),
        }
    }
    if std::env::var_os("W3COS_LAYOUT_NG_TRACE").is_some_and(|v| v == "2") {
        // Tree dump for diagnosing pages whose scripts the unit probe cannot run.
        for (i, node) in flat.iter().enumerate() {
            let s = node.style;
            let kind = match node.kind {
                w3cos_std::ComponentKind::Text { content } => format!("Text {content:?}"),
                other => format!("{:?}", std::mem::discriminant(other)),
            };
            eprintln!(
                "[layout-ng tree] #{i} end={} {kind} disp={:?} ws={:?} fs={} lh={:?} ff={:?} h={:?} minh={:?} m=({:?},{:?}) p=({:?},{:?}) markers={:?}",
                node.subtree_end,
                s.display,
                s.white_space,
                s.font_size,
                s.line_height,
                s.font_family,
                s.height,
                s.min_height,
                s.margin.top,
                s.margin.bottom,
                s.padding.top,
                s.padding.bottom,
                s.custom_properties.as_ref().map(|p| p
                    .keys()
                    .filter(|k| k.starts_with("--w3cos-internal"))
                    .collect::<Vec<_>>())
            );
        }
    }
    if reason.is_some() {
        return None;
    }
    let Some((x, y, fragment)) = block::layout_root(flat, viewport_w, viewport_h) else {
        if trace() {
            eprintln!("[layout-ng] fallback runtime ({} boxes)", flat.len());
        }
        return None;
    };
    let mut out = Vec::with_capacity(flat.len());
    flatten(&fragment, x, y, &mut out);
    Some(out)
}

fn flatten(fragment: &BoxFragment, x: f32, y: f32, out: &mut Vec<(LayoutRect, usize)>) {
    out.push((
        LayoutRect {
            x,
            y,
            width: fragment.width,
            height: fragment.height,
        },
        fragment.index,
    ));
    for (dx, dy, child) in &fragment.children {
        flatten(child, x + dx, y + dy, out);
    }
}

/// Verify mode: compare NG output with the legacy engine's.  Returns the
/// number of differing entries and logs the first few.
pub(crate) fn verify(
    flat: &[FlatNodeInfo],
    legacy: &[(LayoutRect, usize)],
    ng: &[(LayoutRect, usize)],
) -> usize {
    use std::collections::HashMap;
    let legacy_map: HashMap<usize, LayoutRect> = legacy.iter().map(|(r, i)| (*i, *r)).collect();
    let ng_map: HashMap<usize, LayoutRect> = ng.iter().map(|(r, i)| (*i, *r)).collect();
    let mut indices: Vec<usize> = legacy_map.keys().chain(ng_map.keys()).copied().collect();
    indices.sort_unstable();
    indices.dedup();
    let close = |a: f32, b: f32| (a - b).abs() <= 1.0 / 64.0;
    let mut diffs = 0;
    // W3COS_LAYOUT_NG_VERIFY=all also logs matching boxes.
    let all = std::env::var_os("W3COS_LAYOUT_NG_VERIFY").is_some_and(|v| v == "all");
    for index in indices {
        let (l, n) = (legacy_map.get(&index), ng_map.get(&index));
        let same = match (l, n) {
            (Some(l), Some(n)) => {
                close(l.x, n.x)
                    && close(l.y, n.y)
                    && close(l.width, n.width)
                    && close(l.height, n.height)
            }
            _ => false,
        };
        if same && all {
            eprintln!("[layout-ng verify] = #{index} {l:?}");
        }
        if !same {
            diffs += 1;
            if diffs <= 8 || all {
                let tag = flat
                    .get(index)
                    .map(|info| format!("{:?} {:?}", info.kind, info.style.display))
                    .unwrap_or_default();
                eprintln!("[layout-ng verify] #{index} {tag}: legacy {l:?} ng {n:?}");
            }
        }
    }
    if diffs > 0 {
        eprintln!(
            "[layout-ng verify] {diffs} differing boxes of {}",
            flat.len()
        );
    }
    diffs
}

#[cfg(test)]
mod inline_tests;
#[cfg(test)]
mod tests;
