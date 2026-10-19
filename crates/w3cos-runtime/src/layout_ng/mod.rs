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
//! box-sizing, auto horizontal margins.  Stage S3b adds block-level floats
//! and clearance (exclusion.rs); text beside a float is not supported yet.
//! Anything else makes
//! `unsupported_tree` report a reason and the legacy engine runs.

mod block;
mod exclusion;
mod inline;
mod oof;

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
    /// The containing block's direction is rtl: CSS2 10.3.3 resolves auto
    /// and over-constrained inline margins from its inline-start (right)
    /// edge (Blink ResolveInlineAutoMargins with the container style).
    pub rtl: bool,
    /// NGBfcOffset: the estimated offset of the box's border box from its
    /// block formatting context root's border box (S3b).  Floats and
    /// clearance are resolved in these coordinates.
    pub bfc_x: f32,
    pub bfc_y: f32,
    /// NGConstraintSpace::MarginStrut: the margins adjoining our top edge
    /// from before us (excluding our own), and the BFC block offset they
    /// start at -- the hypothetical position of a first child whose
    /// margins collapse through us (CSS2 9.5.2 clearance).
    pub margin_base: f32,
    pub margin_strut: MarginStrut,
    /// NGConstraintSpace::ForcedBfcBlockOffset: the box was placed past
    /// clearance, so its BFC block offset is resolved at its border box
    /// (`bfc_y`) and nothing collapses through its top any more.
    pub forced_bfc: bool,
    /// Blink's adjoining object types: the exclusion-space index from
    /// which floats were placed while our BFC block offset is still
    /// unresolved (the margin strut that started at `margin_base`).
    pub adjoining_from: usize,
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

/// The last line box's baseline (NGBoxFragment::LastBaseline), as an
/// offset from the border-box top.  `Unknown` where paint lays out the
/// lines (a wrapped paragraph leaf): an inline-block depending on it falls
/// back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LastBaseline {
    NoLines,
    At(f32),
    Unknown,
}

impl LastBaseline {
    pub fn offset(self, by: f32) -> Self {
        match self {
            LastBaseline::At(b) => LastBaseline::At(b + by),
            other => other,
        }
    }
}

/// NGLayoutResult.
#[derive(Debug, Clone)]
pub(crate) struct LayoutResult {
    pub fragment: BoxFragment,
    pub last_baseline: LastBaseline,
    /// Collapsible margins adjoining the top border edge (own margin-top,
    /// plus first-child margins when nothing separates them).  The parent
    /// places the border box after this strut has been resolved.
    pub start_strut: MarginStrut,
    /// Collapsible margins adjoining the bottom border edge.
    pub end_strut: MarginStrut,
    /// Top and bottom margins adjoin each other (CSS2 8.3.1 "collapse
    /// through"): start and end strut belong to one collapsed margin.
    pub self_collapsing: bool,
    /// Out-of-flow descendants whose containing block is further up
    /// (static positions relative to this border box).
    pub oof: Vec<oof::OofCandidate>,
    /// The layout read or wrote the parent's exclusion space, so a
    /// different BFC offset may change it (Blink relayouts when the
    /// estimated BFC block offset was wrong).
    pub bfc_dependent: bool,
    /// Line boxes or a BFC root would have to avoid a float
    /// (NGLayoutOpportunity per line; not implemented yet).
    pub float_overlap: bool,
}

thread_local! {
    /// Why the last layout pass gave up at runtime (trace accounting).
    static RUNTIME_REASON: std::cell::Cell<&'static str> = const { std::cell::Cell::new("") };
}

/// A runtime fallback with its reason, reported by
/// `W3COS_LAYOUT_NG_TRACE` as `fallback runtime:<reason>`.
#[cold]
pub(crate) fn bail<T>(reason: &'static str) -> Option<T> {
    RUNTIME_REASON.with(|r| r.set(reason));
    None
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
    RUNTIME_REASON.with(|r| r.set(""));
    let Some((x, y, fragment)) = block::layout_root(flat, viewport_w, viewport_h) else {
        if trace() {
            let reason = RUNTIME_REASON.with(|r| r.get());
            let sep = if reason.is_empty() { "" } else { ":" };
            eprintln!(
                "[layout-ng] fallback runtime{sep}{reason} ({} boxes)",
                flat.len()
            );
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
