//! Persistent component + Taffy tree for geometry flushes.
//!
//! Style-only edits patch the cached nodes and call `compute_layout` again.
//! Tree shape, text metrics, and inherited cascades rebuild once.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

/// CSSOM fragment rects per owner node (read per owner, never in order: a
/// fast hasher).
pub(crate) type ClientRectMap = w3cos_std::fxhash::FxHashMap<u32, Vec<w3cos_dom::DOMRect>>;

type FxHashSet<T> = std::collections::HashSet<T, w3cos_std::fxhash::FxBuildHasher>;

use w3cos_dom::node::NodeId;
use w3cos_std::component::EventAction;
use w3cos_std::style::{
    Dimension, Display, FlexDirection, FlexWrap, Float, Overflow, Position, ScrollMarkerGroup,
    TextDirection,
};
use w3cos_std::{Component, ComponentKind};

use crate::layout::{LayoutEngine, pre_flatten};

#[path = "incremental_layout_splice.rs"]
mod splice;
pub(crate) use splice::{
    PaintChanges, normalize_ranges, paint_changes_wanted, ranges_contain, ranges_gap_end,
    take_paint_changes,
};

struct WidthMemo {
    structure: u64,
    node: u32,
    width_bits: u32,
    viewport_w_bits: u32,
    viewport_h_bits: u32,
    rects: std::rc::Rc<Vec<(u32, f32, f32, f32, f32)>>,
    client_rects: MemoClientRects,
}

/// A width memo's CSSOM rects: the published map, or the layout they are
/// built from on first read (shared, not copied, per memo).
#[derive(Clone)]
enum MemoClientRects {
    Map(std::rc::Rc<ClientRectMap>),
    Pending(std::rc::Rc<PendingClientRects>),
}

/// The tree the last flush laid out, with its preorder flat view kept
/// across relayouts that do not edit it (Blink keeps its LayoutObjects
/// across a resize instead of rebuilding them).
struct RetainedTree {
    /// `pre_flatten_keyed(root, key)`. It borrows the nodes under `root`, so
    /// any edit (`DerefMut`) drops it first, as does dropping the tree.
    flat: Option<((u64, u64), Vec<crate::layout::FlatNodeInfo<'static>>)>,
    /// Owned like a `Box` (freed in `Drop`), but kept as a raw pointer so
    /// moving the tree does not reassert unique access over the nodes the
    /// flat view borrows.
    root: std::ptr::NonNull<Component>,
    /// `W3COS_PREORDER_SIZES`: the node count of each box's subtree, by
    /// preorder index (as [`crate::layout::count_nodes`] counts). Dropped by
    /// any edit but an in-place restyle; a child-list splice updates it.
    preorder: Option<Vec<u32>>,
}

impl RetainedTree {
    fn new(component: Component) -> Self {
        Self {
            flat: None,
            root: std::ptr::NonNull::from(Box::leak(Box::new(component))),
            preorder: None,
        }
    }

    /// The tree and its flat view under structure `key`, flattened only when
    /// the tree was edited (or the key changed) since the last call.
    fn flat(&mut self, key: (u64, u64)) -> (&Component, &[crate::layout::FlatNodeInfo<'_>]) {
        // SAFETY: `root` is the live allocation this tree owns; only
        // `deref_mut` hands out unique access, and it drops `flat` first.
        let root: &Component = unsafe { self.root.as_ref() };
        if self.flat.as_ref().is_none_or(|(at, _)| *at != key) {
            let flat = crate::layout::pre_flatten_keyed(root, Some(key));
            // SAFETY: the entries borrow nodes of `*root`, which outlive
            // them: `flat` is cleared before any edit and before the free.
            let flat = unsafe {
                std::mem::transmute::<
                    Vec<crate::layout::FlatNodeInfo<'_>>,
                    Vec<crate::layout::FlatNodeInfo<'static>>,
                >(flat)
            };
            self.flat = Some((key, flat));
        } else {
            // (A fresh flatten installs it; the kept view skips that.)
            crate::layout::install_font_unit_provider();
        }
        let flat = &self.flat.as_ref().expect("flat view").1;
        (root, flat)
    }
}

impl RetainedTree {
    /// Take the preorder subtree sizes (computed when missing). Whoever
    /// takes them hands them back with [`Self::put_preorder`] only while
    /// they still describe the tree.
    fn take_preorder(&mut self) -> Vec<u32> {
        match self.preorder.take() {
            Some(sizes) => sizes,
            None => preorder_sizes_of(self),
        }
    }

    fn put_preorder(&mut self, sizes: Vec<u32>) {
        self.preorder = Some(sizes);
    }

    /// Run `edit` on the box at `path` (child positions from the root),
    /// keeping the flat view. `edit` may change the box's own fields but
    /// not its children. The walk reaches the box through raw pointers and
    /// each child list's buffer pointer (`Vec::as_mut_ptr` makes no slice
    /// reference), so only the boxes on the path were accessed uniquely:
    /// their entries are rebuilt, every other entry stays valid.
    fn edit_at(&mut self, path: &[u32], edit: impl FnOnce(&mut Component)) -> bool {
        let indices = self.flat.as_ref().and_then(|(_, flat)| {
            let mut indices = Vec::with_capacity(path.len() + 1);
            let mut index = 0;
            indices.push(index);
            for &at in path {
                let end = flat.get(index)?.subtree_end;
                let mut child = index + 1;
                for _ in 0..at {
                    child = flat.get(child).filter(|_| child < end)?.subtree_end;
                }
                if child >= end {
                    return None;
                }
                index = child;
                indices.push(index);
            }
            Some(indices)
        });
        let Some(indices) = indices else {
            // An in-place restyle keeps the preorder sizes (only the child
            // lists decide them).
            let preorder = self.preorder.take();
            let kept = component_at_mut(self, path).map(|node| {
                let children = (node.children.as_ptr(), node.children.len());
                edit(node);
                children == (node.children.as_ptr(), node.children.len())
            });
            if kept == Some(true) {
                self.preorder = preorder;
            }
            return kept.is_some();
        };
        let mut nodes = Vec::with_capacity(indices.len());
        let mut node = self.root.as_ptr();
        // SAFETY: the flat view mirrors the tree (it is dropped on any other
        // edit), so `path` names live boxes: `indices` found each step
        // inside its parent's subtree. No reference to a child list's
        // elements is made, only to the boxes on the path.
        unsafe {
            nodes.push(node);
            for &at in path {
                node = (*node).children.as_mut_ptr().add(at as usize);
                nodes.push(node);
            }
            let children = ((*node).children.as_ptr(), (*node).children.len());
            edit(&mut *node);
            if children != ((*node).children.as_ptr(), (*node).children.len()) {
                // (Not an in-place restyle: flatten again.)
                self.flat = None;
                self.preorder = None;
                return true;
            }
            let flat = &mut self.flat.as_mut().expect("flat view").1;
            for (&index, &node) in indices.iter().zip(&nodes) {
                let node: &'static Component = &*node;
                let entry = &mut flat[index];
                entry.component = Some(node);
                entry.kind = &node.kind;
                entry.style = node.style.as_style();
                entry.on_click = &node.on_click;
                entry.sticky_counter_signal = node.sticky_counter_signal;
            }
        }
        true
    }
}

/// The node count of each box's subtree, by preorder index.
pub(super) fn preorder_sizes_of(root: &Component) -> Vec<u32> {
    fn walk(component: &Component, sizes: &mut Vec<u32>) -> u32 {
        let at = sizes.len();
        sizes.push(0);
        let mut size = 1;
        for child in &component.children {
            size += walk(child, sizes);
        }
        sizes[at] = size;
        size
    }
    let mut sizes = Vec::new();
    walk(root, &mut sizes);
    sizes
}

/// `W3COS_PREORDER_SIZES` (on by default; `=0` turns it off): the structural splice reads
/// subtree node counts from preorder sizes kept with the retained tree and
/// updated by each child-list splice, instead of counting the whole tree on
/// every splice (Blink keeps such counts on the layout tree rather than
/// walking it). `=verify` also counts and reports a difference.
pub(super) fn preorder_sizes_mode() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("W3COS_PREORDER_SIZES").as_deref() {
        Ok("0" | "false") => 0,
        Ok("verify") => 2,
        _ => 1,
    })
}

impl std::ops::Deref for RetainedTree {
    type Target = Component;
    fn deref(&self) -> &Component {
        // SAFETY: see `flat`.
        unsafe { self.root.as_ref() }
    }
}

impl std::ops::DerefMut for RetainedTree {
    fn deref_mut(&mut self) -> &mut Component {
        self.flat = None;
        self.preorder = None;
        // SAFETY: the flat view (the only other borrow) was just dropped.
        unsafe { self.root.as_mut() }
    }
}

impl Drop for RetainedTree {
    fn drop(&mut self) {
        self.flat = None;
        // SAFETY: `root` came from `Box::leak` in `new` and is freed once.
        drop(unsafe { Box::from_raw(self.root.as_ptr()) });
    }
}

struct Cache {
    component: RetainedTree,
    engine: LayoutEngine,
    /// `Document::layout_generation`. Repaint-only text does not bump it.
    structure: u64,
    /// `Document::repaint_text_generation` at the last build. A later text edit
    /// can leave these rects valid while the component text is stale, so the
    /// next geometry edit rebuilds from the DOM.
    source_structure: u64,
    cascade: u64,
    viewport_w_bits: u32,
    viewport_h_bits: u32,
    node_ids: HostIds,
    /// Full rect snapshots keyed by one node's px width. Repeat flushes of
    /// the same width restore the rects instead of solving Taffy again.
    width_memos: Vec<WidthMemo>,
    /// Nodes restyled by flushes that restored a width memo instead of
    /// patching `engine`: patched before its next solve.
    engine_pending: Vec<u32>,
    /// `Document::precise_drain_epoch` after this cache took the dirty list.
    /// Any other drain means a structural splice could miss a mutation.
    drain_epoch: u32,
    /// `stylesheet::generation` at the last build or splice.
    sheet_generation: u64,
    /// No float, out-of-flow (except outside list markers), sticky, or
    /// multicol box anywhere: a block subtree's used box cannot depend on
    /// anything but its containing block width.
    splice_ok: bool,
    /// [`splice_ok`] except that split-inline fragments are allowed: the
    /// structural splice republishes their CSSOM fragment rects per owner.
    splice_owners_ok: bool,
    /// Engine flags of the whole tree, reused for a spliced subtree.
    skip_projections: bool,
    measure_text: bool,
    /// The component tree was spliced; `node_ids` and the Taffy tree are stale.
    node_ids_stale: bool,
    /// Boxes per CSSOM fragment owner in `component` (see
    /// [`collect_cssom_owners`]); built on the first splice that needs it.
    owner_counts: Option<HashMap<u32, u32>>,
}

/// The CSSOM line rects one re-wrapped text run published, relative to its
/// line box, with the inputs they follow from.
struct TextRectsMemo {
    /// Unique per insert: a box-tree plan trusts an entry it validated
    /// only while the entry is that same insert.
    stamp: u64,
    content: String,
    style: w3cos_std::Style,
    width_bits: u32,
    fonts: u64,
    owners: Vec<u32>,
    rects: Vec<TextLineRect>,
}

/// One published line rect, kept as the terms its position is summed from
/// so a replay against a moved line box adds them in the same order.
struct TextLineRect {
    owner: u32,
    rtl: bool,
    prefix: f32,
    width: f32,
    line_top: f32,
    font_top: f32,
    font_height: f32,
}

/// The CSSOM markers `client_rects_for` reads from one box's custom
/// properties, with the (pinned) map they were read from.
#[derive(Default)]
struct BoxMarkers {
    props: Option<w3cos_std::style::CustomProperties>,
    synthetic: bool,
    split_owner: Option<u32>,
    box_owners: Vec<u32>,
    owner_ranges: bool,
}

thread_local! {
    /// Per flat index: the markers of the box at that index in the last
    /// pass.
    static BOX_MARKERS: RefCell<Vec<BoxMarkers>> = const { RefCell::new(Vec::new()) };
    /// Per flat index: a text run's re-wrapped CSSOM rects (the wrap is a
    /// function of the run, its style, the line box width, and the fonts).
    static TEXT_RECTS: RefCell<w3cos_std::fxhash::FxHashMap<usize, TextRectsMemo>> =
        RefCell::new(Default::default());
    static CACHE: RefCell<Option<Cache>> = const { RefCell::new(None) };
    static LAST_FLUSH: std::cell::Cell<(&'static str, u32, u32)> =
        const { std::cell::Cell::new(("none", 0, 0)) };
    static CLIENT_RECTS: RefCell<ClientRectMap> = RefCell::new(Default::default());
    /// Box-tree mode: the structure key of the patch flush publishing now.
    static CLIENT_KEY: std::cell::Cell<Option<(u64, u64)>> = const { std::cell::Cell::new(None) };
    /// Box-tree mode: which boxes can publish CSSOM rects, kept across the
    /// patch flushes of one structure.
    static CLIENT_PLAN: RefCell<Option<ClientPlan>> = const { RefCell::new(None) };
    static TEXT_RECTS_STAMP: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Nodes restyled in place since `CLIENT_PLAN` was refreshed.
    static CLIENT_RESTYLED: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    /// A retained flush's CSSOM rect inputs, not yet published.
    static CLIENT_PENDING: RefCell<Option<std::rc::Rc<PendingClientRects>>> = const { RefCell::new(None) };
}

/// The layout a retained flush produced, kept so its CSSOM rects are built
/// on the first read instead of on every flush (Blink also computes
/// `getClientRects()` on demand from the current fragments). The retained
/// component it was laid out from stays unchanged until the next flush,
/// which either publishes the map whole or settles this first.
struct PendingClientRects {
    /// The patch flush's structure key (`None`: a viewport recompute,
    /// published without the box-tree plan).
    key: Option<(u64, u64)>,
    layouts: Vec<(crate::layout::LayoutRect, usize)>,
    texts: Vec<crate::layout_ng::TextFragment>,
}

fn defer_client_rects(
    key: Option<(u64, u64)>,
    layouts: Vec<(crate::layout::LayoutRect, usize)>,
    texts: Vec<crate::layout_ng::TextFragment>,
) {
    CLIENT_PENDING.with(|slot| {
        *slot.borrow_mut() = Some(std::rc::Rc::new(PendingClientRects {
            key,
            layouts,
            texts,
        }))
    });
}

/// Take the deferred rects out (cloning them when a width memo shares them).
fn take_pending_client_rects() -> Option<PendingClientRects> {
    let pending = CLIENT_PENDING.with(|slot| slot.borrow_mut().take())?;
    Some(
        std::rc::Rc::try_unwrap(pending).unwrap_or_else(|shared| PendingClientRects {
            key: shared.key,
            layouts: shared.layouts.clone(),
            texts: shared.texts.clone(),
        }),
    )
}

fn drop_pending_client_rects() {
    CLIENT_PENDING.with(|slot| drop(slot.borrow_mut().take()));
}

/// The deferred rects, shared (for a width memo).
fn pending_client_rects() -> Option<std::rc::Rc<PendingClientRects>> {
    CLIENT_PENDING.with(|slot| slot.borrow().clone())
}

/// Defer to rects a width memo kept unbuilt (its tree is the current one).
fn restore_pending_client_rects(pending: std::rc::Rc<PendingClientRects>) {
    CLIENT_PENDING.with(|slot| *slot.borrow_mut() = Some(pending));
}

fn client_rects_pending() -> bool {
    CLIENT_PENDING.with(|slot| slot.borrow().is_some())
}

/// Publish the deferred rects of `component`, the retained tree the
/// deferring flush laid out (call before anything edits it).
fn settle_client_rects(component: &Component) {
    let Some(pending) = CLIENT_PENDING.with(|slot| slot.borrow_mut().take()) else {
        return;
    };
    let start = splice::debug_enabled().then(std::time::Instant::now);
    let flat = crate::layout::pre_flatten_keyed(
        component,
        pending.key.filter(|_| crate::layout_ng::box_tree_enabled()),
    );
    let flattened = start.map(|start| start.elapsed().as_micros());
    CLIENT_KEY.with(|key| key.set(pending.key));
    let published = client_rects_for(&flat, &pending.layouts, &pending.texts);
    if let (Some(start), Some(flattened)) = (start, flattened) {
        eprintln!(
            "[splice] settle: key {} flatten {flattened}us total {}us ({} boxes, {} owners)",
            pending.key.is_some(),
            start.elapsed().as_micros(),
            flat.len(),
            published.len()
        );
    }
    CLIENT_KEY.with(|key| key.set(None));
    CLIENT_RECTS.with(|rects| *rects.borrow_mut() = published);
}

/// Settle deferred rects against the retained tree, outside a flush.
fn settle_client_rects_from_cache() {
    if !client_rects_pending() {
        return;
    }
    CACHE.with(|slot| {
        if let Ok(slot) = slot.try_borrow()
            && let Some(cache) = slot.as_ref()
        {
            settle_client_rects(&cache.component);
        }
    });
}

/// The boxes `client_rects_for` can publish rects for (a split-inline
/// owner, block-in-inline box owners, or a text run with owner ranges or an
/// inherited synthetic owner) and the owners each text run inherits; a
/// function of the boxes' CSSOM markers and the tree shape only.
struct ClientPlan {
    key: (u64, u64),
    len: usize,
    host: Vec<Option<u32>>,
    publishers: Vec<usize>,
    /// Per box: the `TextRectsMemo` stamp last matched against this box's
    /// text and style (0 none); cleared when the box is restyled.
    validated: Vec<u64>,
    inherited_text_owners: Vec<Vec<u32>>,
    text_line_owners: HashSet<u32, w3cos_std::fxhash::FxBuildHasher>,
}

/// A node's style changed in place (box-tree mode).
pub(crate) fn note_client_restyled(id: u32) {
    CLIENT_RESTYLED.with(|ids| {
        let mut ids = ids.borrow_mut();
        if ids.len() >= 4096 {
            ids.clear();
            CLIENT_PLAN.with(|plan| *plan.borrow_mut() = None);
        }
        ids.push(id);
    });
}

/// The tree changed other than by an in-place restyle.
pub(crate) fn forget_client_plan() {
    CLIENT_PLAN.with(|plan| *plan.borrow_mut() = None);
    CLIENT_RESTYLED.with(|ids| ids.borrow_mut().clear());
}

fn box_markers(info: &crate::layout::FlatNodeInfo<'_>) -> BoxMarkers {
    let Some(now) = info.style.custom_properties.as_ref() else {
        return BoxMarkers::default();
    };
    BoxMarkers {
        props: Some(now.clone()),
        synthetic: now.contains_key("--w3cos-internal-split-inline-edge")
            || now.contains_key("--w3cos-internal-split-inline-fragment"),
        split_owner: split_inline_owner(info),
        box_owners: w3cos_std::inline_text::cssom_box_owners(info.style),
        owner_ranges: now.contains_key(w3cos_std::inline_text::CSSOM_OWNER_RANGES),
    }
}

/// Refresh `plan` and `markers` for the boxes of `ids` (and the host-less
/// boxes lowered below them); `None` when a box's publishing role changed.
fn refresh_client_plan(
    flat: &[crate::layout::FlatNodeInfo<'_>],
    markers: &mut [BoxMarkers],
    mut plan: ClientPlan,
    ids: &[u32],
) -> Option<ClientPlan> {
    if ids.len() > 8 {
        return None;
    }
    let mut dirty = Vec::new();
    if !ids.is_empty() {
        for index in 0..flat.len() {
            if markers[index]
                .split_owner
                .is_some_and(|id| ids.contains(&id))
            {
                dirty.push(index);
            }
            if plan.host[index].is_some_and(|id| ids.contains(&id)) {
                dirty.push(index);
                let end = flat[index].subtree_end.max(index + 1).min(flat.len());
                let mut child = index + 1;
                while child < end {
                    if plan.host[child].is_some() {
                        child = flat[child].subtree_end.max(child + 1);
                    } else {
                        dirty.push(child);
                        child += 1;
                    }
                }
            }
        }
    }
    for index in dirty {
        plan.validated[index] = 0;
        let info = &flat[index];
        let slot = &mut markers[index];
        match (info.style.custom_properties.as_ref(), slot.props.as_ref()) {
            (None, None) => continue,
            (Some(now), Some(read)) if now.ptr_eq(read) => continue,
            _ => {}
        }
        let next = box_markers(info);
        if next.synthetic != slot.synthetic
            || next.split_owner != slot.split_owner
            || next.box_owners != slot.box_owners
            || next.owner_ranges != slot.owner_ranges
        {
            *slot = next;
            return None;
        }
        *slot = next;
    }
    Some(plan)
}

pub(crate) fn client_rects(node: u32) -> Option<Vec<w3cos_dom::DOMRect>> {
    settle_client_rects_from_cache();
    CLIENT_RECTS.with(|rects| rects.borrow().get(&node).cloned())
}

fn client_rect_snapshot() -> ClientRectMap {
    settle_client_rects_from_cache();
    CLIENT_RECTS.with(|rects| rects.borrow().clone())
}

fn restore_client_rects(rects: ClientRectMap) {
    drop_pending_client_rects();
    CLIENT_RECTS.with(|current| *current.borrow_mut() = rects);
}

/// What the most recent [`flush`] solved. `solved_nodes` is the subtree that
/// was laid out; `shifted_nodes` only had a translation applied.
pub struct IncrementalFlushStats {
    pub kind: &'static str,
    pub solved_nodes: u32,
    pub shifted_nodes: u32,
}

pub fn last_flush_stats() -> IncrementalFlushStats {
    let (kind, solved_nodes, shifted_nodes) = LAST_FLUSH.with(std::cell::Cell::get);
    IncrementalFlushStats {
        kind,
        solved_nodes,
        shifted_nodes,
    }
}

fn note_flush(kind: &'static str, solved_nodes: u32, shifted_nodes: u32) {
    LAST_FLUSH.with(|slot| slot.set((kind, solved_nodes, shifted_nodes)));
}

fn phase_log_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("W3COS_LAYOUT_PHASES").is_some())
}

pub fn invalidate() {
    CACHE.with(|cache| *cache.borrow_mut() = None);
    splice::clear_paint_journal();
    PAINT.with(|cache| *cache.borrow_mut() = None);
    crate::layout::forget_paint_scroll_plan();
    drop_pending_client_rects();
    CLIENT_RECTS.with(|rects| rects.borrow_mut().clear());
    PAINT_REUSE.with(|kind| kind.set(PaintReuse::None));
    #[cfg(any(feature = "skia", feature = "cpu-render"))]
    crate::headless::invalidate_retained_frame();
}

/// Flush pending layout into stored DOM rects. Returns false when Taffy fails.
pub fn flush() -> bool {
    let (viewport_w, viewport_h, _) = crate::jsdom::viewport();
    let viewport_w = viewport_w as f32;
    let viewport_h = viewport_h as f32;
    // `structure` here is the repaint-only text generation: a splice leaves
    // it alone, so text edited without layout elsewhere stays detectable.
    let (layout_gen, structure, cascade) = crate::dom::with_document(|document| {
        (
            document.layout_generation(),
            document.repaint_text_generation(),
            document.cascade_generation(),
        )
    });
    let take = || {
        let (before, dirty, after) = crate::dom::with_document_mut(|document| {
            let before = document.precise_drain_epoch();
            let dirty = document.take_precise_dirty();
            (before, dirty, document.precise_drain_epoch())
        });
        take_paint_drained(before, dirty, after)
    };
    // A retained tree that only needs a structural splice skips the
    // document-wide definite-layout fingerprint below.
    let mut taken = None;
    let spliced = CACHE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(cache) = slot.as_mut() else {
            return false;
        };
        if !cache.splice_owners_ok || cache.structure == layout_gen {
            if !cache.splice_owners_ok && splice::debug_enabled() {
                eprintln!("[splice] retained tree is not spliceable");
            }
            return false;
        }
        let (before, dirty, after) = take();
        let complete = cache.drain_epoch == before;
        cache.drain_epoch = after;
        if !complete {
            if splice::debug_enabled() {
                eprintln!("[splice] dirty list drained elsewhere");
            }
            cache.splice_ok = false;
            cache.splice_owners_ok = false;
            taken = Some((after, dirty, after));
            return false;
        }
        // A splice edits the published map in place. With retained lowering
        // the splice settles it only once it is known to commit: a rebuild
        // republishes every rect anyway.
        if !splice::retain_lowering_enabled() {
            settle_client_rects(&cache.component);
        }
        if splice::try_splice(cache, &dirty, viewport_w, viewport_h) {
            cache.structure = layout_gen;
            cache.cascade = cascade;
            return true;
        }
        taken = Some((after, dirty, after));
        false
    });
    if spliced {
        crate::dom::clear_document_dirty();
        return true;
    }
    // Any other outcome changes the retained tree without a journal entry.
    splice::clear_paint_journal();
    if crate::definite_layout::try_apply() {
        let _ = crate::dom::with_document_mut(|document| document.take_precise_dirty());
        crate::dom::clear_document_dirty();
        invalidate();
        return true;
    }
    let (drain_before, dirty, drain_after) = match taken {
        Some(taken) => taken,
        None => take(),
    };
    let mut ok = false;
    CACHE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(cache) = slot.as_mut() {
            let complete = cache.drain_epoch == drain_before;
            cache.drain_epoch = drain_after;
            if !complete {
                // Another consumer drained mutations this cache never saw.
                cache.splice_ok = false;
                cache.splice_owners_ok = false;
            }
        }
        let structure_matches = slot
            .as_ref()
            .is_some_and(|cache| cache.structure == layout_gen);
        if structure_matches {
            let cache = slot.as_mut().unwrap();
            // Glyphs changed and a used box may now depend on them. Rebuild
            // from the DOM instead of patching the stale component tree.
            let text_stale = cache.source_structure != structure && !dirty.is_empty();
            if splice::debug_enabled() && !dirty.is_empty() {
                eprintln!(
                    "[splice] patch path: {} dirty, text_stale {text_stale}, first {:?}",
                    dirty.len(),
                    crate::dom::with_document(|document| document
                        .get_node(dirty[0])
                        .tag
                        .as_str()
                        .to_string())
                );
            }
            let geometry_only = cache.cascade != cascade;
            if !text_stale
                && patch_and_compute(cache, &dirty, viewport_w, viewport_h, geometry_only)
            {
                cache.cascade = cascade;
                ok = true;
                return;
            }
        }
        if splice::debug_enabled() {
            eprintln!(
                "[splice] full rebuild: cached {} structure_matches {structure_matches} dirty {}",
                slot.is_some(),
                dirty.len()
            );
        }
        ok = rebuild(
            &mut slot,
            layout_gen,
            structure,
            cascade,
            viewport_w,
            viewport_h,
            drain_after,
        );
        if ok {
            let solved = slot
                .as_ref()
                .map(|cache| cache.node_ids.len() as u32)
                .unwrap_or(0);
            note_flush("rebuild", solved, 0);
        }
    });
    if ok {
        crate::dom::clear_document_dirty();
    }
    ok
}

/// `W3COS_WARM_REBUILD` (on by default, `=0` turns it off): a rebuild lays the new tree out through its
/// retained flat view under the cache's structure key, as a patch flush
/// does, so the per-box data patch flushes keep (fingerprints, subtree
/// flags, the flat output, the eligibility walk) is there for the next
/// edit instead of being built on it (Blink's LayoutObjects carry it from
/// the first layout). Chat: mean class toggle -31% wall (16/16 rounds)
/// for +1.9% on the full layout.
#[cfg(test)]
thread_local! {
    pub(crate) static WARM_REBUILD_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

fn warm_rebuild_enabled() -> bool {
    #[cfg(test)]
    if let Some(on) = WARM_REBUILD_OVERRIDE.with(|o| o.get()) {
        return on;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !std::env::var("W3COS_WARM_REBUILD").is_ok_and(|v| v == "0" || v == "false"))
}

fn rebuild(
    slot: &mut Option<Cache>,
    layout_gen: u64,
    structure: u64,
    cascade: u64,
    viewport_w: f32,
    viewport_h: f32,
    drain_epoch: u32,
) -> bool {
    if warm_rebuild_enabled()
        && crate::layout_ng::mode() == crate::layout_ng::Mode::On
        && crate::layout_ng::box_tree_enabled()
        && std::env::var_os("W3COS_SPLICE_VERIFY").is_none()
    {
        let key = (layout_gen, structure);
        // A rebuild after a refused patch keeps the structure key (an
        // in-place restyle does not bump the layout generation) but lowers
        // a new tree: per-box data kept under the key describes the old
        // one, and a restyle that drops one box and adds another (a column
        // hidden in one table, shown in the next) keeps every length equal.
        crate::layout_ng::forget_patch_memo();
        let mut tree = RetainedTree::new(crate::dom::to_component_tree());
        let mut engine = LayoutEngine::new();
        let laid = {
            crate::layout_ng::forget_walk_fingerprints();
            let (component, flat) = tree.flat(key);
            configure_engine(&mut engine, component);
            crate::layout_ng::set_patch_hint(Some((flat, key)));
            let computed = engine.compute(component, flat, viewport_w, viewport_h);
            crate::layout_ng::set_patch_hint(None);
            match computed {
                Ok(results)
                    if !sync_column_scroll_marker_counts(
                        flat,
                        &results.layout_cache,
                        (viewport_w, viewport_h),
                    ) =>
                {
                    let node_ids = flat
                        .iter()
                        .map(|info| match info.on_click {
                            EventAction::NativeHost { id, .. } => *id as u32,
                            _ => u32::MAX,
                        })
                        .collect::<Vec<_>>();
                    let snaps = host_rect_snaps(flat, &results.layout_cache);
                    defer_client_rects(None, results.layout_cache, results.text_fragments);
                    Some((node_ids, snaps))
                }
                // A failed layout or a column-count resync: the cold path
                // (which lowers again for the resync).
                _ => None,
            }
        };
        if let Some((node_ids, snaps)) = laid {
            let width_memos = memos_for_tree(&tree, layout_gen, viewport_w, viewport_h, snaps);
            let splice_ok = splice::tree_is_spliceable(&tree);
            let splice_owners_ok = splice_ok || splice::tree_is_spliceable_with_owners(&tree);
            let skip_projections = can_skip_projections(&tree);
            let measure_text = needs_text_metric(&tree);
            *slot = Some(Cache {
                drain_epoch,
                sheet_generation: w3cos_dom::stylesheet::generation(),
                splice_ok,
                splice_owners_ok,
                skip_projections,
                measure_text,
                node_ids_stale: false,
                owner_counts: None,
                component: tree,
                engine,
                structure: layout_gen,
                source_structure: structure,
                cascade,
                node_ids: node_ids.into(),
                width_memos,
                engine_pending: Vec::new(),
                viewport_w_bits: viewport_w.to_bits(),
                viewport_h_bits: viewport_h.to_bits(),
            });
            return true;
        }
    }
    let phase = phase_log_enabled();
    let t0 = std::time::Instant::now();
    let mut component = crate::dom::to_component_tree();
    let t1 = std::time::Instant::now();
    let mut flat = pre_flatten(&component);
    let t2 = std::time::Instant::now();
    let mut engine = LayoutEngine::new();
    configure_engine(&mut engine, &component);
    let Ok(mut results) = engine.compute(&component, &flat, viewport_w, viewport_h) else {
        return false;
    };
    let t3 = std::time::Instant::now();
    if sync_column_scroll_marker_counts(&flat, &results.layout_cache, (viewport_w, viewport_h)) {
        // Column count is a layout-time fact. Rebuild markers before
        // publishing geometry so scrollWidth / scrollLeft clamp see
        // overflow columns (CSS Overflow 5 / CSS Multicol).
        component = crate::dom::to_component_tree();
        flat = pre_flatten(&component);
        engine = LayoutEngine::new();
        configure_engine(&mut engine, &component);
        let Ok(second) = engine.compute(&component, &flat, viewport_w, viewport_h) else {
            return false;
        };
        results = second;
    }
    let node_ids = flat
        .iter()
        .map(|info| match info.on_click {
            EventAction::NativeHost { id, .. } => *id as u32,
            _ => u32::MAX,
        })
        .collect::<Vec<_>>();
    let t4 = std::time::Instant::now();
    let snaps = if crate::layout_ng::mode() == crate::layout_ng::Mode::On
        && std::env::var_os("W3COS_SPLICE_VERIFY").is_none()
    {
        // CSSOM rects on first read, as for a patch flush or a recompute.
        let snaps = host_rect_snaps(&flat, &results.layout_cache);
        defer_client_rects(None, results.layout_cache, results.text_fragments);
        snaps
    } else {
        write_rects(&flat, &results.layout_cache, &results.text_fragments)
    };
    let t5 = std::time::Instant::now();
    let width_memos = memos_for_tree(&component, layout_gen, viewport_w, viewport_h, snaps);
    let splice_ok = splice::tree_is_spliceable(&component);
    let splice_owners_ok = splice_ok || splice::tree_is_spliceable_with_owners(&component);
    if phase {
        let us = |a: std::time::Instant, b: std::time::Instant| (b - a).as_micros();
        eprintln!(
            "[phases] rebuild lower {} flatten {} compute {} ids {} write {} memos {}",
            us(t0, t1),
            us(t1, t2),
            us(t2, t3),
            us(t3, t4),
            us(t4, t5),
            us(t5, std::time::Instant::now())
        );
    }
    let skip_projections = can_skip_projections(&component);
    let measure_text = needs_text_metric(&component);
    drop(flat);
    *slot = Some(Cache {
        drain_epoch,
        sheet_generation: w3cos_dom::stylesheet::generation(),
        splice_ok,
        splice_owners_ok,
        skip_projections,
        measure_text,
        node_ids_stale: false,
        owner_counts: None,
        component: RetainedTree::new(component),
        engine,
        structure: layout_gen,
        source_structure: structure,
        cascade,
        node_ids: node_ids.into(),
        width_memos,
        engine_pending: Vec::new(),
        viewport_w_bits: viewport_w.to_bits(),
        viewport_h_bits: viewport_h.to_bits(),
    });
    true
}

/// `W3COS_STYLE_LOCAL` (on by default; `=0` turns it off): a geometry edit on one element
/// that the style-subtree path refuses (it needs the whole document free of
/// split inlines and projection boxes) lays out only that element's subtree
/// through the structural splice (local eligibility, owner and projection
/// checks; the deferred CSSOM fragments of the following boxes are rebased,
/// not settled), and the paint tree replays it, like Blink re-lays out only
/// the boxes a style change dirtied. `=verify` compares every such splice
/// with a full layout (`[splice-verify]` lines).
fn style_local_mode() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("W3COS_STYLE_LOCAL").as_deref() {
        Ok("0" | "false") => 0,
        Ok("verify") => 2,
        _ => 1,
    })
}

fn style_splice_enabled() -> bool {
    style_local_mode() != 0
}

fn patch_and_compute(
    cache: &mut Cache,
    dirty: &[NodeId],
    viewport_w: f32,
    viewport_h: f32,
    geometry_only: bool,
) -> bool {
    // A patch can restyle boxes in place; recount fragment owners lazily.
    cache.owner_counts = None;
    if dirty.is_empty() {
        // A cascade change with no dirty box can restyle descendants. Rebuild.
        if geometry_only {
            return false;
        }
        if cache.viewport_w_bits == viewport_w.to_bits()
            && cache.viewport_h_bits == viewport_h.to_bits()
        {
            // Scroll and repaint-only text leave the used boxes in place.
            return true;
        }
        let ok = recompute(cache, viewport_w, viewport_h);
        if ok {
            cache.viewport_w_bits = viewport_w.to_bits();
            cache.viewport_h_bits = viewport_h.to_bits();
        }
        return ok;
    }
    let mut styles = Vec::with_capacity(dirty.len());
    let mut paint_only = true;
    let key = (cache.structure, cache.source_structure);
    let previous = (dirty.len() == 1)
        .then(|| located_style(key, &cache.component, dirty[0].as_u32()).cloned())
        .flatten();
    for id in dirty {
        // An iframe/object host's box style is not its computed style: the
        // DOM lowering makes it a replaced inline-block with the 300x150
        // fallback size (`--w3cos-internal-replaced-element`). Patching the
        // computed style in would lay it out as a plain inline box; lower
        // the document again, as Blink rebuilds a LayoutIFrame's style.
        if located_style(key, &cache.component, id.as_u32()).is_some_and(is_replaced_host) {
            return false;
        }
        let style = crate::dom::with_document(|document| document.computed_style_for(*id));
        if !style_patch_is_safe(&style) {
            return false;
        }
        if geometry_only {
            let Some(old) = located_style(key, &cache.component, id.as_u32()).cloned() else {
                return false;
            };
            if !geometry_only_change(&old, &style) {
                return false;
            }
            paint_only &= local_paint_only(&old, &style);
        } else if let Some(old) = located_style(key, &cache.component, id.as_u32()) {
            paint_only &= local_paint_only(old, &style);
        } else {
            paint_only = false;
        }
        styles.push((id.as_u32(), style));
    }
    // A width memo holds every box's rects for one width of one node, so it
    // stays valid only while the edits since are width-only on that node.
    // (LayoutNG only for now: the legacy Taffy path keeps its memos, since
    // its real layout misses the container end on the
    // align-content-*-rounding pages that a stale memo happens to pass.)
    let ng = crate::layout_ng::mode() == crate::layout_ng::Mode::On;
    if ng && !paint_only {
        let width_only = previous
            .as_ref()
            .filter(|_| styles.len() == 1)
            .is_some_and(|old| width_only_change(old, &styles[0].1));
        let edited = styles[0].0;
        cache
            .width_memos
            .retain(|memo| width_only && memo.node == edited);
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
            .map(|memo| (memo.rects.clone(), memo.client_rects.clone()))
    });
    // LayoutNG lays out the patched component itself (no Taffy tree to
    // patch). Only accept edits that leave the lowered subtree as a fresh
    // lower would produce it: the DOM's lowering reads the element's
    // formatting, font, width auto-ness, and text alignment (the sole text
    // run's line-width marker), so those edits rebuild.
    // (Checked after the subtree path below, which re-lowers what it needs.)
    let ng_relower = crate::layout_ng::mode() == crate::layout_ng::Mode::On
        && !cache.engine.tree_valid()
        && styles.iter().any(|(id, style)| {
            located_style(key, &cache.component, *id)
                .is_none_or(|old| !ng_lowering_stable(old, style))
        });
    // The subtree path below edits the published map in place: settle it
    // while the retained tree is still the one it was laid out from.
    // (`W3COS_STYLE_LOCAL` below rebases deferred rects or settles them
    // itself, after deciding the splice applies.)
    // A single px width edit stays on the full patch path, which records
    // the width memo the next edit back to that width hits (an alternating
    // toggle then costs a memo restore, not a splice).
    let style_splice = styles.len() == 1
        && style_splice_enabled()
        && !paint_only
        && single_px_width(&styles).is_none();
    if styles.len() == 1
        && let Some(old) = previous.as_ref()
        && splice::style_subtree_may_apply(cache, old, &styles[0].1, viewport_w, viewport_h)
    {
        settle_client_rects(&cache.component);
    }
    for (id, style) in &styles {
        if !assign_retained(key, &mut cache.component, *id, style) {
            return false;
        }
    }
    if paint_only
        && cache.viewport_w_bits == viewport_w.to_bits()
        && cache.viewport_h_bits == viewport_h.to_bits()
    {
        note_flush("paint", 0, 0);
        return true;
    }
    if let Some((rects, client_rects)) = memo_hit {
        if ng {
            cache
                .engine_pending
                .extend(styles.iter().map(|(id, _)| *id));
        }
        restore_rects(&rects);
        match client_rects {
            MemoClientRects::Map(map) => restore_client_rects((*map).clone()),
            MemoClientRects::Pending(pending) => restore_pending_client_rects(pending),
        }
        cache.viewport_w_bits = viewport_w.to_bits();
        cache.viewport_h_bits = viewport_h.to_bits();
        note_flush("memo", 0, 0);
        return true;
    }
    if styles.len() == 1
        && let Some(old) = previous.as_ref()
        && splice::try_style_subtree(
            cache,
            styles[0].0,
            old,
            &styles[0].1,
            viewport_w,
            viewport_h,
        )
    {
        crate::layout_ng::forget_patch_memo();
        cache.viewport_w_bits = viewport_w.to_bits();
        cache.viewport_h_bits = viewport_h.to_bits();
        return true;
    }
    // A geometry edit on one element the style-subtree path refused: splice
    // the element's subtree as a structural edit would (fresh lower, local
    // owner and projection checks, follower shifts, a paint journal entry).
    let mut held_paths = None;
    if style_splice && previous.is_some() && {
        // `W3COS_STYLE_LOCAL_REUSE`: splice the retained subtree (its
        // host style is patched above) when the lowering is stable.
        let reuse = splice::style_reuse_enabled()
            && previous
                .as_ref()
                .is_some_and(|old| ng_lowering_stable(old, &styles[0].1));
        splice::set_style_reuse(reuse.then_some(styles[0].0));
        // `W3COS_STYLE_PATHS_KEEP`: hold the known style paths across the
        // splice, which forgets them on each subtree it swaps.
        held_paths = (style_paths_keep_mode() != 0).then(take_style_paths);
        // The splice solves the root under a wrapper as wide as the root's
        // old margin box, which is its containing block only while the
        // root's own used width came from it: an edit to the root's own
        // width inputs relays out from its parent.
        let own_width = previous
            .as_ref()
            .is_some_and(|old| own_width_inputs_changed(old, &styles[0].1));
        let root = style_local_root_for(&cache.component, styles[0].0, own_width);
        let spliced = splice::try_splice(cache, &[NodeId::from_u32(root)], viewport_w, viewport_h);
        splice::set_style_reuse(None);
        if !spliced && let Some(paths) = held_paths.take() {
            let key = paths.0;
            restore_style_paths(paths, key);
        }
        spliced
    } {
        // The splice replaced the element's subtree in place: the known
        // style paths outside it still lead to their boxes (each use checks
        // the box id), so carry them over to the new structure key.
        let paths = held_paths.take().unwrap_or_else(take_style_paths);
        crate::layout_ng::forget_patch_memo();
        restore_style_paths(paths, (cache.structure, cache.source_structure));
        if style_local_mode() == 2 {
            settle_client_rects(&cache.component);
            splice::verify(viewport_w, viewport_h, styles[0].0);
        }
        return true;
    }
    if ng_relower {
        return false;
    }
    let phase = phase_log_enabled();
    let flatten_started = std::time::Instant::now();
    let fresh;
    let (component, flat) = if crate::layout_ng::box_tree_enabled() {
        // (See `recompute`.)
        crate::layout_ng::forget_walk_fingerprints();
        cache
            .component
            .flat((cache.structure, cache.source_structure))
    } else {
        fresh = pre_flatten(&cache.component);
        (&*cache.component, &fresh[..])
    };
    let flatten_us = flatten_started.elapsed().as_micros();
    if cache.node_ids_stale {
        cache.node_ids = flat.iter().map(flat_host_id).collect();
        cache.engine.invalidate();
        cache.engine_pending.clear();
        cache.node_ids_stale = false;
    }
    if flat.len() != cache.node_ids.len() {
        return false;
    }
    configure_engine(&mut cache.engine, component);
    if !patch_pending(
        &mut cache.engine,
        component,
        &cache.node_ids,
        &mut cache.engine_pending,
        viewport_w,
        viewport_h,
    ) {
        return false;
    }
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
    let compute_started = std::time::Instant::now();
    crate::layout_ng::set_patch_hint(Some((flat, (cache.structure, cache.source_structure))));
    let computed = cache
        .engine
        .compute(component, &flat, viewport_w, viewport_h);
    crate::layout_ng::set_patch_hint(None);
    let Ok(mut results) = computed else {
        return false;
    };
    let compute_us = compute_started.elapsed().as_micros();
    let write_started = std::time::Instant::now();
    // Publish CSSOM rects on first read, unless this flush snapshots them
    // now (a width memo) or a verifier compares them.
    let client_key = (cache.structure, cache.source_structure);
    // (A width memo keeps the deferred inputs; the legacy path, whose
    // memos outlive other edits, keeps a built map.)
    let width_memo = single_px_width(&styles).is_some();
    let snaps = if (!width_memo || ng) && std::env::var_os("W3COS_SPLICE_VERIFY").is_none() {
        let snaps = if width_memo {
            host_rect_snaps_by_ids(&flat, &cache.node_ids, &results.layout_cache)
        } else {
            if !lazy_dom_rects() {
                write_host_rects(&cache.node_ids, &results.layout_cache);
            }
            Vec::new()
        };
        let slots = results.layout_slots.take();
        defer_client_rects(
            Some(client_key),
            results.layout_cache,
            results.text_fragments,
        );
        if !width_memo && lazy_dom_rects() {
            publish_host_rects(&cache.node_ids, slots);
        }
        snaps
    } else {
        CLIENT_KEY.with(|key| key.set(Some(client_key)));
        let snaps = write_rects(&flat, &results.layout_cache, &results.text_fragments);
        CLIENT_KEY.with(|key| key.set(None));
        snaps
    };
    let write_us = write_started.elapsed().as_micros();
    if phase {
        eprintln!(
            "[layout-phase] retained nodes {} flatten_us {flatten_us} compute_us {compute_us} write_us {write_us} passes {}",
            flat.len(),
            cache.engine.last_compute_layout_passes
        );
    }
    note_flush("retained", flat.len() as u32, 0);
    if std::env::var_os("W3COS_SPLICE_VERIFY").is_some() {
        splice::verify(viewport_w, viewport_h, styles[0].0);
    }
    cache.viewport_w_bits = viewport_w.to_bits();
    cache.viewport_h_bits = viewport_h.to_bits();
    if let Some((node, bits)) = single_px_width(&styles) {
        remember_width(cache, node, bits, viewport_w, viewport_h, snaps);
    }
    true
}

/// Patch `engine` with the styles a width-memo flush assigned without it.
fn patch_pending(
    engine: &mut LayoutEngine,
    component: &Component,
    node_ids: &[u32],
    pending: &mut Vec<u32>,
    viewport_w: f32,
    viewport_h: f32,
) -> bool {
    for id in std::mem::take(pending) {
        let Some(style) = component_style(component, id) else {
            return false;
        };
        let Some(index) = node_ids.iter().position(|node| *node == id) else {
            return false;
        };
        if !engine.patch_style(index, style, viewport_w, viewport_h) {
            return false;
        }
    }
    true
}

fn recompute(cache: &mut Cache, viewport_w: f32, viewport_h: f32) -> bool {
    // The tree is the one the last flush laid out (no box was restyled):
    // as a patch flush with nothing to patch, keep the per-box data held
    // under its structure key (Blink keeps its LayoutObjects on resize).
    let key = (cache.structure, cache.source_structure);
    let box_tree = crate::layout_ng::box_tree_enabled();
    let fresh;
    let (component, flat) = if box_tree {
        // The flat view outlives this flush: nothing may take it for the
        // one an earlier flush's eligibility walk left behind.
        crate::layout_ng::forget_walk_fingerprints();
        cache.component.flat(key)
    } else {
        fresh = pre_flatten(&cache.component);
        (&*cache.component, &fresh[..])
    };
    if cache.node_ids_stale {
        cache.node_ids = flat.iter().map(flat_host_id).collect();
        cache.engine.invalidate();
        cache.engine_pending.clear();
        cache.node_ids_stale = false;
    }
    configure_engine(&mut cache.engine, component);
    if !cache.engine_pending.is_empty()
        && (flat.len() != cache.node_ids.len()
            || !patch_pending(
                &mut cache.engine,
                component,
                &cache.node_ids,
                &mut cache.engine_pending,
                viewport_w,
                viewport_h,
            ))
    {
        return false;
    }
    if box_tree && flat.len() == cache.node_ids.len() {
        crate::layout_ng::set_patch_hint(Some((flat, key)));
    }
    let computed = cache
        .engine
        .compute(component, &flat, viewport_w, viewport_h);
    crate::layout_ng::set_patch_hint(None);
    let Ok(mut results) = computed else {
        return false;
    };
    // A changed column count needs a tree rebuild so overflow-column
    // scroll geometry and ::column::scroll-marker siblings stay in sync.
    let resync =
        sync_column_scroll_marker_counts(flat, &results.layout_cache, (viewport_w, viewport_h));
    if crate::layout_ng::mode() == crate::layout_ng::Mode::On
        && std::env::var_os("W3COS_SPLICE_VERIFY").is_none()
    {
        // CSSOM rects on first read, as for a patch flush.
        let lazy = lazy_dom_rects() && flat.len() == cache.node_ids.len();
        if flat.len() == cache.node_ids.len() {
            debug_assert!(node_ids_match(flat, &cache.node_ids));
            if !lazy {
                write_host_rects(&cache.node_ids, &results.layout_cache);
            }
        } else {
            let _ = host_rect_snaps(flat, &results.layout_cache);
        }
        let slots = results.layout_slots.take();
        defer_client_rects(None, results.layout_cache, results.text_fragments);
        if lazy {
            publish_host_rects(&cache.node_ids, slots);
        }
    } else {
        write_rects(flat, &results.layout_cache, &results.text_fragments);
    }
    !resync
}

fn write_rects(
    flat: &[crate::layout::FlatNodeInfo<'_>],
    layouts: &[(crate::layout::LayoutRect, usize)],
    text_fragments: &[crate::layout_ng::TextFragment],
) -> Vec<(u32, f32, f32, f32, f32)> {
    publish_client_rects(flat, layouts, text_fragments);
    host_rect_snaps(flat, layouts)
}

/// Store each host box's layout rect on its DOM node; returns them.
fn host_rect_snaps(
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

/// [`host_rect_snaps`] reading the host ids the cache keeps per flat
/// index (`flat_host_id`) instead of each entry's component.
fn host_rect_snaps_by_ids(
    flat: &[crate::layout::FlatNodeInfo<'_>],
    node_ids: &[u32],
    layouts: &[(crate::layout::LayoutRect, usize)],
) -> Vec<(u32, f32, f32, f32, f32)> {
    if node_ids.len() != flat.len() {
        return host_rect_snaps(flat, layouts);
    }
    debug_assert!(node_ids_match(flat, node_ids));
    let mut snaps = Vec::with_capacity(layouts.len());
    crate::dom::with_document_mut(|document| {
        for (rect, index) in layouts {
            let Some(&node_id) = node_ids.get(*index) else {
                continue;
            };
            if node_id == u32::MAX {
                continue;
            }
            snaps.push((node_id, rect.x, rect.y, rect.width, rect.height));
            document.set_layout_rect(
                NodeId::from_u32(node_id),
                w3cos_dom::DOMRect::new(rect.x, rect.y, rect.width, rect.height),
            );
        }
    });
    snaps
}

fn node_ids_match(flat: &[crate::layout::FlatNodeInfo<'_>], node_ids: &[u32]) -> bool {
    flat.iter()
        .zip(node_ids)
        .all(|(info, &id)| flat_host_id(info) == id)
}

fn split_inline_owner(info: &crate::layout::FlatNodeInfo<'_>) -> Option<u32> {
    info.style
        .custom_properties
        .as_ref()?
        .get(w3cos_dom::document::SPLIT_INLINE_FRAGMENT_OWNER)?
        .parse()
        .ok()
}

/// Physical advances for one shaped line at every authored owner boundary.
/// This is produced during layout writeback and consumed by the retained
/// CSSOM rectangle map; later `getClientRects()` calls only read that map.
struct PhysicalTextLine {
    boundaries: Vec<usize>,
    positions: Vec<f32>,
}

impl PhysicalTextLine {
    fn advance(&self, start: usize, end: usize) -> Option<(f32, f32)> {
        let start_index = self.boundaries.binary_search(&start).ok()?;
        let end_index = self.boundaries.binary_search(&end).ok()?;
        let start_position = *self.positions.get(start_index)?;
        let end_position = *self.positions.get(end_index)?;
        Some((start_position, (end_position - start_position).max(0.0)))
    }

    fn total(&self) -> Option<f32> {
        self.positions.last().copied()
    }
}

fn physical_text_line(
    line: &str,
    style: &w3cos_std::Style,
    owner_ranges: &[(u32, usize, usize)],
) -> Option<PhysicalTextLine> {
    let mut boundaries = Vec::with_capacity(owner_ranges.len() * 2 + 2);
    boundaries.extend([0, line.len()]);
    for &(_, start, end) in owner_ranges {
        if start <= end
            && end <= line.len()
            && line.is_char_boundary(start)
            && line.is_char_boundary(end)
        {
            boundaries.extend([start, end]);
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let fragments = boundaries
        .windows(2)
        .map(|range| line.get(range[0]..range[1]))
        .collect::<Option<Vec<_>>>()?;
    let advances = crate::layout::physical_text_fragment_advances(&fragments, style)?;
    let mut positions = Vec::with_capacity(advances.len() + 1);
    positions.push(0.0);
    for advance in advances {
        positions.push(positions.last().copied().unwrap_or(0.0) + advance);
    }
    (positions.len() == boundaries.len()).then_some(PhysicalTextLine {
        boundaries,
        positions,
    })
}

fn publish_client_rects(
    flat: &[crate::layout::FlatNodeInfo<'_>],
    layouts: &[(crate::layout::LayoutRect, usize)],
    ng_text: &[crate::layout_ng::TextFragment],
) {
    drop_pending_client_rects();
    let published = client_rects_for(flat, layouts, ng_text);
    CLIENT_RECTS.with(|rects| *rects.borrow_mut() = published);
}

/// Every CSSOM fragment owner a box of `component` publishes rects for: the
/// split-inline owner of a synthetic fragment, block-in-inline box owners,
/// and the owner ranges of a text run. Counts boxes per owner.
pub(super) fn collect_cssom_owners(component: &Component, counts: &mut HashMap<u32, u32>) {
    let mut stack = vec![component];
    while let Some(component) = stack.pop() {
        let style = &component.style;
        if let Some(properties) = style.custom_properties.as_ref() {
            if let Some(owner) = properties
                .get(w3cos_dom::document::SPLIT_INLINE_FRAGMENT_OWNER)
                .and_then(|value| value.parse::<u32>().ok())
            {
                *counts.entry(owner).or_default() += 1;
            }
            for owner in w3cos_std::inline_text::cssom_box_owners(style) {
                *counts.entry(owner).or_default() += 1;
            }
            if let w3cos_std::ComponentKind::Text { content } = &component.kind {
                for (owner, _, _) in w3cos_std::inline_text::cssom_owner_ranges(content, style) {
                    *counts.entry(owner).or_default() += 1;
                }
            }
        }
        stack.extend(component.children.iter());
    }
}

/// Replace the published rects of the owners a splice rebuilt. `removed`
/// owners lose their entries, `inserted` ones get the subtree's rects moved
/// by (`dx`, `dy`), and every rect of a `shifted` owner moves down by `dh`.
pub(super) fn splice_client_rects(
    removed: impl Iterator<Item = u32>,
    inserted: ClientRectMap,
    dx: f32,
    dy: f32,
    shifted: impl Iterator<Item = u32>,
    dh: f32,
) {
    debug_assert!(!client_rects_pending());
    CLIENT_RECTS.with(|rects| {
        let mut rects = rects.borrow_mut();
        for owner in removed {
            rects.remove(&owner);
        }
        if dh != 0.0 {
            for owner in shifted {
                if let Some(list) = rects.get_mut(&owner) {
                    for rect in list {
                        rect.y += dh;
                    }
                }
            }
        }
        for (owner, mut list) in inserted {
            for rect in &mut list {
                rect.x += dx;
                rect.y += dy;
            }
            rects.insert(owner, list);
        }
    });
}

/// Every box's CSSOM markers (refreshed where their map changed), and the
/// boxes that can publish rects.
fn client_plan(
    flat: &[crate::layout::FlatNodeInfo<'_>],
    markers: &mut [BoxMarkers],
    key: Option<(u64, u64)>,
) -> ClientPlan {
    for (info, slot) in flat.iter().zip(markers.iter_mut()) {
        match (info.style.custom_properties.as_ref(), slot.props.as_ref()) {
            (None, None) => {}
            (Some(now), Some(read)) if now.ptr_eq(read) => {}
            (None, Some(_)) => *slot = BoxMarkers::default(),
            (Some(now), _) => {
                *slot = BoxMarkers {
                    props: Some(now.clone()),
                    synthetic: now.contains_key("--w3cos-internal-split-inline-edge")
                        || now.contains_key("--w3cos-internal-split-inline-fragment"),
                    split_owner: split_inline_owner(info),
                    box_owners: w3cos_std::inline_text::cssom_box_owners(info.style),
                    owner_ranges: now.contains_key(w3cos_std::inline_text::CSSOM_OWNER_RANGES),
                };
            }
        }
    }
    let synthetic_owner: Vec<Option<u32>> = markers
        .iter()
        .map(|m| m.synthetic.then_some(m.split_owner).flatten())
        .collect();
    let any_synthetic_owner = synthetic_owner.iter().any(Option::is_some);
    let mut inherited_text_owners = vec![Vec::new(); flat.len()];
    let mut text_line_owners: HashSet<u32, w3cos_std::fxhash::FxBuildHasher> = HashSet::default();
    for (index, info) in flat.iter().enumerate() {
        if !any_synthetic_owner {
            break;
        }
        if !info.component.is_some_and(|component| {
            matches!(component.kind, w3cos_std::ComponentKind::Text { .. })
        }) {
            continue;
        }
        let mut parent = info.parent;
        while let Some(parent_index) = parent {
            let Some(ancestor) = flat.get(parent_index) else {
                break;
            };
            if let Some(owner) = synthetic_owner[parent_index]
                && !inherited_text_owners[index].contains(&owner)
            {
                inherited_text_owners[index].push(owner);
                text_line_owners.insert(owner);
            }
            parent = ancestor.parent;
        }
    }
    let publishers = if key.is_some() {
        (0..flat.len())
            .filter(|&index| {
                let marker = &markers[index];
                marker.split_owner.is_some()
                    || !marker.box_owners.is_empty()
                    || (flat[index].component.is_some_and(|component| {
                        matches!(component.kind, w3cos_std::ComponentKind::Text { .. })
                    }) && (!inherited_text_owners[index].is_empty() || marker.owner_ranges))
            })
            .collect()
    } else {
        Vec::new()
    };
    ClientPlan {
        key: key.unwrap_or_default(),
        len: flat.len(),
        host: if key.is_some() {
            flat.iter().map(flat_host_id_opt).collect()
        } else {
            Vec::new()
        },
        validated: if key.is_some() {
            vec![0; flat.len()]
        } else {
            Vec::new()
        },
        publishers,
        inherited_text_owners,
        text_line_owners,
    }
}

fn flat_host_id_opt(info: &crate::layout::FlatNodeInfo<'_>) -> Option<u32> {
    match info.on_click {
        w3cos_std::EventAction::NativeHost { id, .. } => u32::try_from(*id).ok(),
        _ => None,
    }
}

pub(super) fn client_rects_for(
    flat: &[crate::layout::FlatNodeInfo<'_>],
    layouts: &[(crate::layout::LayoutRect, usize)],
    ng_text: &[crate::layout_ng::TextFragment],
) -> ClientRectMap {
    let mut by_index = vec![None; flat.len()];
    for (rect, index) in layouts {
        if let Some(slot) = by_index.get_mut(*index) {
            *slot = Some(*rect);
        }
    }
    let mut published = ClientRectMap::default();
    // (Per-owner bookkeeping, read per owner: a fast hasher, no order.)
    let mut owners_with_inline_fragments: FxHashSet<u32> = FxHashSet::default();
    let mut first_fragment_is_block: w3cos_std::fxhash::FxHashMap<u32, bool> = Default::default();
    let mut last_fragment_is_block: w3cos_std::fxhash::FxHashMap<u32, bool> = Default::default();
    // Read each box's markers once (the text walk below visits every
    // ancestor of every text leaf), and only for a marker map this index
    // did not read in the last pass.
    let mut markers = BOX_MARKERS.with(|memo| std::mem::take(&mut *memo.borrow_mut()));
    markers.resize_with(flat.len(), BoxMarkers::default);
    let plan_key = CLIENT_KEY
        .with(|key| key.get())
        .filter(|_| crate::layout_ng::box_tree_enabled());
    let ids = if plan_key.is_some() {
        CLIENT_RESTYLED.with(|ids| std::mem::take(&mut *ids.borrow_mut()))
    } else {
        Vec::new()
    };
    let mut plan = plan_key
        .and_then(|key| {
            CLIENT_PLAN
                .with(|plan| plan.borrow_mut().take())
                .filter(|plan| plan.key == key && plan.len == flat.len())
        })
        .and_then(|plan| refresh_client_plan(flat, &mut markers, plan, &ids))
        .unwrap_or_else(|| client_plan(flat, &mut markers, plan_key));
    let mut validated = std::mem::take(&mut plan.validated);
    let ClientPlan {
        publishers,
        inherited_text_owners,
        text_line_owners,
        ..
    } = &plan;
    let iteration: Box<dyn Iterator<Item = usize>> = if plan_key.is_some() {
        Box::new(publishers.iter().copied())
    } else {
        Box::new(0..flat.len())
    };
    // LayoutNG line fragments grouped by box, in emission order.
    let mut ng_lines_of: w3cos_std::fxhash::FxHashMap<usize, Vec<&crate::layout_ng::TextFragment>> =
        Default::default();
    for fragment in ng_text {
        if !fragment.vertical
            && (plan_key.is_none()
                || !inherited_text_owners[fragment.index].is_empty()
                || markers[fragment.index].owner_ranges)
        {
            ng_lines_of
                .entry(fragment.index)
                .or_default()
                .push(fragment);
        }
    }
    for index in iteration {
        let info = &flat[index];
        let synthetic_inline = markers[index].synthetic;
        let Some(mut rect) = by_index.get(index).copied().flatten() else {
            continue;
        };
        if synthetic_inline {
            // Taffy lays an anonymous inline carrier out at the containing
            // block's available width. CSSOM exposes the physical inline
            // fragment instead, whose inline extent is its max-content width.
            if let Some(component) = info.component {
                let width = crate::layout::component_max_content_width(component)
                    .min(rect.width)
                    .max(0.0);
                rect.width = width;
                if width == 0.0 && component.children.is_empty() {
                    rect.height = 0.0;
                }
            }
        } else if matches!(
            info.style.display,
            Display::Block | Display::Flex | Display::Grid | Display::Table | Display::ListItem
        ) && let Some(parent) = info.parent
            && let Some(containing) = by_index.get(parent).copied().flatten()
        {
            // CSS 2.1 block-in-inline creates an anonymous block around the
            // in-flow block. Its inline extent is the actual containing block,
            // while its block extent follows the authored child.
            rect.x = containing.x;
            rect.width = containing.width;
        }
        let physical = w3cos_dom::DOMRect::new(rect.x, rect.y, rect.width, rect.height);
        if let Some(owner) = markers[index].split_owner {
            if synthetic_inline {
                owners_with_inline_fragments.insert(owner);
            }
            let text_range_will_publish = info.component.is_some_and(|component| {
                let w3cos_std::ComponentKind::Text { content } = &component.kind else {
                    return false;
                };
                w3cos_std::inline_text::cssom_owner_ranges(content, info.style)
                    .iter()
                    .any(|(range_owner, _, _)| *range_owner == owner)
            });
            // Once physical text lines have been published for a split inline,
            // suppress only its positive-size anonymous inline carrier.  Real
            // block fragments and the zero-size split edge remain observable
            // CSSOM fragments and must keep their original ordering entries.
            let replaced_by_text_lines = synthetic_inline
                && text_line_owners.contains(&owner)
                && physical.width > 0.0
                && physical.height > 0.0;
            if !text_range_will_publish && !replaced_by_text_lines {
                published.entry(owner).or_default().push(physical);
                let is_block = !synthetic_inline;
                first_fragment_is_block.entry(owner).or_insert(is_block);
                last_fragment_is_block.insert(owner, is_block);
            }
        }
        for &owner in &markers[index].box_owners {
            published.entry(owner).or_default().push(physical);
            first_fragment_is_block.entry(owner).or_insert(true);
            last_fragment_is_block.insert(owner, true);
        }
        let Some(component) = info.component else {
            continue;
        };
        let w3cos_std::ComponentKind::Text { content } = &component.kind else {
            continue;
        };
        // Text fragments publish only for split-inline owners: explicit
        // owner ranges on the run or an inherited synthetic-inline owner.
        // Without either, wrapping the run again would publish nothing.
        if inherited_text_owners[index].is_empty() && !markers[index].owner_ranges {
            continue;
        }
        let line_box = info
            .parent
            .and_then(|parent| by_index.get(parent).copied().flatten())
            .unwrap_or(rect);
        let measure = |text: &str, style: &w3cos_std::Style| {
            crate::layout::component_max_content_width(&w3cos_std::Component::text_shared(
                text.to_string(),
                style.clone().into(),
            ))
        };
        let physical_advance = |text: &str, style: &w3cos_std::Style| measure(text, style);
        // LayoutNG laid this text out: its line fragments (byte ranges of
        // `content` with their physical rects) are where the owner ranges
        // sit, so CSSOM reads them instead of re-wrapping the run.
        let ng_lines = ng_lines_of.get(&index).map(Vec::as_slice).unwrap_or(&[]);
        if !ng_lines.is_empty() {
            let owner_ranges = w3cos_std::inline_text::cssom_owner_ranges(content, info.style);
            let explicit_owners = owner_ranges
                .iter()
                .map(|(owner, _, _)| *owner)
                .collect::<FxHashSet<_>>();
            for fragment in ng_lines {
                let (line_start, line_end) = (fragment.start, fragment.end.min(content.len()));
                if line_start > line_end
                    || !content.is_char_boundary(line_start)
                    || !content.is_char_boundary(line_end)
                {
                    continue;
                }
                let line_rect = fragment.rect;
                let mut push = |owner: u32, x: f32, width: f32, text: &str| {
                    let font_box = crate::font_face::font_box_metrics_for_text(info.style, text);
                    let font_height = font_box.map_or(line_rect.height, |metrics| metrics.height());
                    let font_top =
                        crate::layout::inline_upper_leading(line_rect.height, font_height);
                    owners_with_inline_fragments.insert(owner);
                    published
                        .entry(owner)
                        .or_default()
                        .push(w3cos_dom::DOMRect::new(
                            x,
                            line_rect.y + font_top,
                            width,
                            font_height,
                        ));
                    first_fragment_is_block.entry(owner).or_insert(false);
                    last_fragment_is_block.insert(owner, false);
                };
                for &(owner, start, end) in &owner_ranges {
                    let (a, b) = (start.max(line_start), end.min(line_end));
                    if a >= b {
                        continue;
                    }
                    let prefix = measure(&content[line_start..a], info.style);
                    let width = measure(&content[a..b], info.style);
                    let x = match info.style.direction {
                        w3cos_std::style::TextDirection::Ltr => line_rect.x + prefix,
                        w3cos_std::style::TextDirection::Rtl => {
                            line_rect.x + line_rect.width - prefix - width
                        }
                    };
                    push(owner, x, width, &content[a..b]);
                }
                for owner in inherited_text_owners[index]
                    .iter()
                    .copied()
                    .filter(|owner| !explicit_owners.contains(owner))
                {
                    push(
                        owner,
                        line_rect.x,
                        line_rect.width,
                        &content[line_start..line_end],
                    );
                }
            }
            continue;
        }
        // With LayoutNG the writeback re-wraps with the same UAX #14 mode as
        // layout and paint (line-break strictness, word-break, lang), so the
        // published fragments follow the painted lines.
        // keep-all leaves fall back to the legacy layout and keep its mode.
        let fonts = crate::font_face::FontRegistry::global().revision();
        let width_bits = line_box.width.to_bits();
        let replay =
            |rects: &[TextLineRect],
             published: &mut ClientRectMap,
             owners_with_inline_fragments: &mut FxHashSet<u32>,
             first_fragment_is_block: &mut w3cos_std::fxhash::FxHashMap<u32, bool>,
             last_fragment_is_block: &mut w3cos_std::fxhash::FxHashMap<u32, bool>| {
                for rect in rects {
                    let x = if rect.rtl {
                        line_box.x + line_box.width - rect.prefix - rect.width
                    } else {
                        line_box.x + rect.prefix
                    };
                    owners_with_inline_fragments.insert(rect.owner);
                    published
                        .entry(rect.owner)
                        .or_default()
                        .push(w3cos_dom::DOMRect::new(
                            x,
                            line_box.y + rect.line_top + rect.font_top,
                            rect.width,
                            rect.font_height,
                        ));
                    first_fragment_is_block.entry(rect.owner).or_insert(false);
                    last_fragment_is_block.insert(rect.owner, false);
                }
            };
        let hit = TEXT_RECTS.with(|memo| {
            let memo = memo.borrow();
            let Some(entry) = memo.get(&index) else {
                return false;
            };
            // (A box-tree plan already matched this entry's text and style
            // to the box, and the box was not restyled since.)
            let trusted = validated.get(index) == Some(&entry.stamp);
            if entry.width_bits != width_bits
                || entry.fonts != fonts
                || entry.owners != inherited_text_owners[index]
                || (!trusted && (entry.content != *content || entry.style != *info.style))
            {
                return false;
            }
            if let Some(slot) = validated.get_mut(index) {
                *slot = entry.stamp;
            }
            replay(
                &entry.rects,
                &mut published,
                &mut owners_with_inline_fragments,
                &mut first_fragment_is_block,
                &mut last_fragment_is_block,
            );
            true
        });
        if hit {
            continue;
        }
        let mut rects: Vec<TextLineRect> = Vec::new();
        let break_mode = if crate::layout_ng::mode() == crate::layout_ng::Mode::Off
            || info.style.word_break == w3cos_std::style::WordBreak::KeepAll
        {
            crate::line_break::BreakMode::default()
        } else {
            crate::line_break::BreakMode::of(info.style)
        };
        let lines = crate::text_layout::wrap_text_with_run_width_in(
            content,
            line_box.width.max(1.0),
            info.style.white_space,
            break_mode,
            |text| measure(text, info.style),
        );
        let line_styles = crate::text_layout::authored_line_styles(content, info.style, &lines);
        for (line_index, line) in lines.iter().enumerate() {
            let line_style = line_styles
                .as_ref()
                .and_then(|styles| styles.get(line_index))
                .unwrap_or(info.style);
            let owner_ranges = w3cos_std::inline_text::cssom_owner_ranges(line, line_style);
            let physical_line = physical_text_line(line, line_style, &owner_ranges);
            let explicit_owners = owner_ranges
                .iter()
                .map(|(owner, _, _)| *owner)
                .collect::<FxHashSet<_>>();
            for (owner, start, end) in owner_ranges {
                // Resolve every owner boundary together during layout
                // writeback. Contextual backends shape one physical run and
                // assign a crossing cluster to its start owner; the current
                // Vello/fontdue backend preserves its authored-fragment
                // LayoutUnit endpoints without later CSSOM remeasurement.
                let (prefix_width, width) = physical_line
                    .as_ref()
                    .and_then(|physical| physical.advance(start, end))
                    .unwrap_or_else(|| {
                        (
                            measure(&line[..start], line_style),
                            measure(&line[start..end], line_style),
                        )
                    });
                let rtl = line_style.direction == w3cos_std::style::TextDirection::Rtl;
                let line_height = crate::layout::inline_style_line_height(line_style);
                let font_box = crate::font_face::font_box_metrics_for_text(
                    line_style,
                    line.get(start..end).unwrap_or(line),
                );
                let font_height = font_box.map_or(line_height, |metrics| metrics.height());
                let font_top = crate::layout::inline_upper_leading(line_height, font_height);
                rects.push(TextLineRect {
                    owner,
                    rtl,
                    prefix: prefix_width,
                    width,
                    line_top: line_index as f32 * line_height,
                    font_top,
                    font_height,
                });
            }
            for owner in inherited_text_owners[index]
                .iter()
                .copied()
                .filter(|owner| !explicit_owners.contains(owner))
            {
                let width = physical_line
                    .as_ref()
                    .and_then(PhysicalTextLine::total)
                    .unwrap_or_else(|| physical_advance(line, line_style));
                let rtl = line_style.direction == w3cos_std::style::TextDirection::Rtl;
                let line_height = crate::layout::inline_style_line_height(line_style);
                let font_box = crate::font_face::font_box_metrics_for_text(line_style, line);
                let font_height = font_box.map_or(line_height, |metrics| metrics.height());
                let font_top = crate::layout::inline_upper_leading(line_height, font_height);
                // (`line_box.x + 0.0` is `line_box.x`, and subtracting a zero
                // prefix leaves the RTL sum as it was.)
                rects.push(TextLineRect {
                    owner,
                    rtl,
                    prefix: 0.0,
                    width,
                    line_top: line_index as f32 * line_height,
                    font_top,
                    font_height,
                });
            }
        }
        replay(
            &rects,
            &mut published,
            &mut owners_with_inline_fragments,
            &mut first_fragment_is_block,
            &mut last_fragment_is_block,
        );
        TEXT_RECTS.with(|memo| {
            let mut memo = memo.borrow_mut();
            if memo.len() > 2 * flat.len() + 64 {
                memo.clear();
            }
            let stamp = TEXT_RECTS_STAMP.with(|stamp| {
                stamp.set(stamp.get() + 1);
                stamp.get()
            });
            if let Some(slot) = validated.get_mut(index) {
                *slot = stamp;
            }
            memo.insert(
                index,
                TextRectsMemo {
                    stamp,
                    content: content.to_string(),
                    style: info.style.clone(),
                    width_bits,
                    fonts,
                    owners: inherited_text_owners[index].clone(),
                    rects,
                },
            );
        });
    }
    for owner in owners_with_inline_fragments.iter().copied() {
        let Some(rects) = published.get_mut(&owner) else {
            continue;
        };
        if rects.is_empty() {
            continue;
        }
        let left = rects
            .iter()
            .map(|rect| rect.x)
            .fold(f32::INFINITY, f32::min);
        let top = rects
            .iter()
            .map(|rect| rect.y)
            .fold(f32::INFINITY, f32::min);
        let bottom = rects
            .iter()
            .map(|rect| rect.y + rect.height)
            .fold(f32::NEG_INFINITY, f32::max);
        if first_fragment_is_block.get(&owner) == Some(&true) {
            rects.insert(0, w3cos_dom::DOMRect::new(left, top, 0.0, 0.0));
        }
        if last_fragment_is_block.get(&owner) == Some(&true) {
            rects.push(w3cos_dom::DOMRect::new(left, bottom, 0.0, 0.0));
        }
    }
    for (owner, rects) in &mut published {
        if owners_with_inline_fragments.contains(owner) || rects.is_empty() {
            continue;
        }
        let mut union = rects[0];
        for rect in &rects[1..] {
            let left = union.x.min(rect.x);
            let top = union.y.min(rect.y);
            let right = (union.x + union.width).max(rect.x + rect.width);
            let bottom = (union.y + union.height).max(rect.y + rect.height);
            union = w3cos_dom::DOMRect::new(left, top, right - left, bottom - top);
        }
        let zero_before = w3cos_dom::DOMRect::new(union.x, union.y, 0.0, 0.0);
        let middle = w3cos_dom::DOMRect::new(union.x, union.y, union.width, union.height);
        let zero_after = w3cos_dom::DOMRect::new(union.x, union.y + union.height, 0.0, 0.0);
        *rects = vec![zero_before, middle, zero_after];
    }
    BOX_MARKERS.with(|memo| *memo.borrow_mut() = markers);
    if plan_key.is_some() {
        plan.validated = validated;
        CLIENT_PLAN.with(|slot| *slot.borrow_mut() = Some(plan));
    }
    published
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

/// `new` differs from `old` in `width` alone.
fn width_only_change(old: &w3cos_std::style::Style, new: &w3cos_std::style::Style) -> bool {
    if old.width == new.width {
        return old == new;
    }
    let mut old = old.clone();
    old.width = new.width;
    old == *new
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
        rects: std::rc::Rc::new(rects),
        client_rects: pending_client_rects().map_or_else(
            || MemoClientRects::Map(std::rc::Rc::new(client_rect_snapshot())),
            MemoClientRects::Pending,
        ),
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
    if nodes.is_empty() {
        return Vec::new();
    }
    let rects = std::rc::Rc::new(rects);
    // (A rebuild that deferred its rects keeps them unbuilt here too.)
    let client_rects = pending_client_rects().map_or_else(
        || MemoClientRects::Map(std::rc::Rc::new(client_rect_snapshot())),
        MemoClientRects::Pending,
    );
    nodes
        .into_iter()
        .map(|(node, width_bits)| WidthMemo {
            structure,
            node,
            width_bits,
            viewport_w_bits,
            viewport_h_bits,
            rects: rects.clone(),
            client_rects: client_rects.clone(),
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
        && old.writing_mode == new.writing_mode
        && old.logical_overrides == new.logical_overrides
}

/// A style edit after which the element's lowered children are unchanged.
fn ng_lowering_stable(old: &w3cos_std::style::Style, new: &w3cos_std::style::Style) -> bool {
    let internal = |style: &w3cos_std::style::Style| {
        let mut keys = style
            .custom_properties
            .iter()
            .flat_map(|properties| properties.iter())
            .filter(|(key, _)| {
                key.starts_with("--w3cos-internal")
                    && key.as_str() != w3cos_dom::document::SPLIT_INLINE_FRAGMENT_OWNER
                    && key.as_str() != w3cos_std::inline_text::CSSOM_OWNER_RANGES
                    && key.as_str() != w3cos_std::inline_text::CSSOM_BOX_OWNERS
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        keys.sort_unstable();
        keys
    };
    geometry_only_change(old, new)
        && matches!(old.width, Dimension::Auto) == matches!(new.width, Dimension::Auto)
        && old.text_indent == new.text_indent
        && old.text_align == new.text_align
        && old.overflow_x == new.overflow_x
        && old.overflow_y == new.overflow_y
        && internal(old) == internal(new)
}

fn assign_layout_style(
    component: &mut Component,
    id: u32,
    style: &w3cos_std::style::Style,
) -> bool {
    if host_id(component) == Some(id) {
        assign_style_here(component, id, style);
        return true;
    }
    component
        .children
        .iter_mut()
        .any(|child| assign_layout_style(child, id, style))
}

/// Box-tree mode: the child-index path from the root to the first box of
/// each node looked up under one structure key (Blink reaches a
/// LayoutObject from its node directly instead of searching the tree).
type StylePaths = ((u64, u64), w3cos_std::fxhash::FxHashMap<u32, Vec<u32>>);

thread_local! {
    static STYLE_PATHS: RefCell<StylePaths> = RefCell::new(Default::default());
}

/// The box a `W3COS_STYLE_LOCAL` splice relays out for a restyle of `id`.
/// A box whose used size comes from its content in its parent's layout (a
/// flex or grid item, an inline-block or table-internal box, a float, or an
/// absolutely positioned box) is not a relayout root in Blink
/// (LayoutObject::IsRelayoutBoundary / ObjectIsRelayoutBoundary), and a
/// restyle below it changes its intrinsic size, which re-runs its parent's
/// algorithm (a table cell resizes its column). Start above the topmost such
/// box on the path to `id`.
fn style_local_root(component: &Component, id: u32) -> u32 {
    style_local_root_for(component, id, false)
}

/// [`style_local_root`]; `own_width` (the edit changed the element's own
/// width inputs) starts at its parent, since the element's old border box
/// no longer bounds its new used width.
fn style_local_root_for(component: &Component, id: u32, own_width: bool) -> u32 {
    let mut path = Vec::new();
    if !find_path(component, id, &mut path) {
        return id;
    }
    let mut depth = if own_width {
        path.len().saturating_sub(1)
    } else {
        path.len()
    };
    for at in 1..=path.len() {
        let (Some(parent), Some(node)) = (
            component_at(component, &path[..at - 1]),
            component_at(component, &path[..at]),
        ) else {
            break;
        };
        if sizes_from_content(parent, path[at - 1] as usize, node) {
            depth = at - 1;
            break;
        }
    }
    while depth > 0 {
        match component_at(component, &path[..depth]).and_then(host_id) {
            Some(host) => return host,
            None => depth -= 1,
        }
    }
    host_id(component).unwrap_or(id)
}

/// The edit changes an input of the element's own used width (CSS 2
/// 10.3.3: width, min/max-width, horizontal margins, box-sizing, display,
/// the intrinsic sizing keywords, or the font a font-relative width
/// resolves against), so its old border box is not its containing block's
/// width.
fn own_width_inputs_changed(old: &w3cos_std::style::Style, new: &w3cos_std::style::Style) -> bool {
    use w3cos_dom::css_style::{
        MAX_WIDTH_KEYWORD_PROPERTY, MIN_WIDTH_KEYWORD_PROPERTY, WIDTH_KEYWORD_PROPERTY,
    };
    let keyword = |style: &w3cos_std::style::Style, key: &str| {
        style
            .custom_properties
            .as_ref()
            .and_then(|properties| properties.get(key).cloned())
    };
    let font_relative = |style: &w3cos_std::style::Style| {
        [style.width, style.min_width, style.max_width]
            .iter()
            .any(|d| {
                !matches!(
                    d,
                    Dimension::Auto | Dimension::Px(_) | Dimension::Percent(_)
                )
            })
            || [style.margin.left, style.margin.right].iter().any(|m| {
                !matches!(
                    m,
                    w3cos_std::style::Spacing::Px(_)
                        | w3cos_std::style::Spacing::Percent(_)
                        | w3cos_std::style::Spacing::Auto
                )
            })
    };
    old.width != new.width
        || old.min_width != new.min_width
        || old.max_width != new.max_width
        || old.margin.left != new.margin.left
        || old.margin.right != new.margin.right
        || old.box_sizing != new.box_sizing
        || old.display != new.display
        || [
            WIDTH_KEYWORD_PROPERTY,
            MIN_WIDTH_KEYWORD_PROPERTY,
            MAX_WIDTH_KEYWORD_PROPERTY,
        ]
        .iter()
        .any(|key| keyword(old, key) != keyword(new, key))
        || ((font_relative(old) || font_relative(new))
            && (old.font_size != new.font_size || old.font_family != new.font_family))
}

/// `node` (child `index` of `parent`) takes its used size from its content
/// in `parent`'s layout.
fn sizes_from_content(parent: &Component, index: usize, node: &Component) -> bool {
    (matches!(
        parent.style.display,
        Display::Flex | Display::InlineFlex | Display::Grid
    ) && !row_flex_fill_item(parent, index)
        && !column_flex_stretch_item(parent, node))
        || matches!(
            node.style.display,
            Display::InlineBlock
                | Display::InlineFlex
                | Display::InlineTable
                | Display::Table
                | Display::TableRowGroup
                | Display::TableHeaderGroup
                | Display::TableFooterGroup
                | Display::TableRow
                | Display::TableCell
                | Display::TableCaption
        )
        || !matches!(node.style.float, Float::None)
        || matches!(node.style.position, Position::Absolute | Position::Fixed)
        || host_id(node).is_none()
}

/// A column flex item stretched to the container's width (auto width,
/// `align-items: stretch` or `align-self: stretch`): its width comes from
/// the container, not from its content; its height change shifts the
/// following items like a block's.
fn column_flex_stretch_item(parent: &Component, node: &Component) -> bool {
    matches!(parent.style.display, Display::Flex | Display::InlineFlex)
        && matches!(
            parent.style.flex_direction,
            FlexDirection::Column | FlexDirection::ColumnReverse
        )
        && matches!(node.style.width, Dimension::Auto)
        && match node.style.align_self {
            w3cos_std::style::AlignSelf::Stretch => true,
            w3cos_std::style::AlignSelf::Auto => {
                parent.style.align_items == w3cos_std::style::AlignItems::Stretch
            }
            _ => false,
        }
}

/// A row flex item whose used width does not depend on its own content:
/// the only growing item, with a definite flex basis and `min-width: 0`, next
/// to rigid fixed-width siblings. It fills the row's remaining space, so
/// its border box keeps its width when its padding, border or content
/// change (Blink still re-runs the flex algorithm there; here the parent's
/// result is known to be the same).
fn row_flex_fill_item(parent: &Component, index: usize) -> bool {
    let style = &parent.style;
    if !matches!(style.display, Display::Flex | Display::InlineFlex)
        || !matches!(
            style.flex_direction,
            FlexDirection::Row | FlexDirection::RowReverse
        )
        || style.flex_wrap != FlexWrap::NoWrap
    {
        return false;
    }
    let Some(item) = parent.children.get(index) else {
        return false;
    };
    let item_style = &item.style;
    if item_style.flex_grow <= 0.0
        || !matches!(item_style.flex_basis, Dimension::Px(_))
        || !matches!(item_style.min_width, Dimension::Px(min) if min == 0.0)
        || !matches!(item_style.max_width, Dimension::Auto)
    {
        return false;
    }
    parent.children.iter().enumerate().all(|(at, other)| {
        at == index
            || matches!(
                other.style.position,
                w3cos_std::style::Position::Absolute | w3cos_std::style::Position::Fixed
            )
            || (other.style.flex_grow == 0.0
                && other.style.flex_shrink == 0.0
                && matches!(other.style.width, Dimension::Px(_))
                && matches!(other.style.flex_basis, Dimension::Auto | Dimension::Px(_)))
    })
}

/// Test hook: the `W3COS_STYLE_LOCAL` relayout root for `id` in the
/// retained tree, and whether that path is enabled.
#[cfg(test)]
pub(crate) fn style_local_root_of(id: u32) -> Option<(u32, bool)> {
    CACHE.with(|slot| {
        let slot = slot.borrow();
        let cache = slot.as_ref()?;
        Some((
            style_local_root(&cache.component, id),
            style_splice_enabled(),
        ))
    })
}

fn find_path(component: &Component, id: u32, path: &mut Vec<u32>) -> bool {
    if host_id(component) == Some(id) {
        return true;
    }
    for (at, child) in component.children.iter().enumerate() {
        path.push(at as u32);
        if find_path(child, id, path) {
            return true;
        }
        path.pop();
    }
    false
}

fn component_at<'a>(mut component: &'a Component, path: &[u32]) -> Option<&'a Component> {
    for &at in path {
        component = component.children.get(at as usize)?;
    }
    Some(component)
}

fn component_at_mut<'a>(
    mut component: &'a mut Component,
    path: &[u32],
) -> Option<&'a mut Component> {
    for &at in path {
        component = component.children.get_mut(at as usize)?;
    }
    Some(component)
}

/// `W3COS_STYLE_PATHS_KEEP` (on by default; `=0` turns it off): a `W3COS_STYLE_LOCAL`
/// splice keeps the known style paths (each use still checks the box id at
/// the path) instead of losing them to the forgets of the subtrees it swaps,
/// so the next restyle of the same element does not search the whole tree.
/// `=verify` searches anyway and reports a different path.
fn style_paths_keep_mode() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_STYLE_PATHS_KEEP").as_deref() {
            Ok("0" | "false") => 0,
            Ok("verify") => 2,
            _ => 1,
        },
    )
}

fn take_style_paths() -> StylePaths {
    STYLE_PATHS.with(|paths| std::mem::take(&mut *paths.borrow_mut()))
}

fn restore_style_paths(mut paths: StylePaths, key: (u64, u64)) {
    paths.0 = key;
    STYLE_PATHS.with(|slot| *slot.borrow_mut() = paths);
}

/// The tree changed other than by an in-place restyle.
pub(crate) fn forget_style_paths() {
    STYLE_PATHS.with(|paths| *paths.borrow_mut() = Default::default());
}

/// The path to the first box of `id` (the box `component_style` finds).
fn style_path(key: (u64, u64), component: &Component, id: u32) -> Option<Vec<u32>> {
    let known = STYLE_PATHS.with(|paths| {
        let mut paths = paths.borrow_mut();
        if paths.0 != key || paths.1.len() > 4096 {
            *paths = (key, Default::default());
        }
        paths.1.get(&id).cloned()
    });
    if let Some(path) = known
        && component_at(component, &path).is_some_and(|found| host_id(found) == Some(id))
    {
        if style_paths_keep_mode() == 2 {
            let mut found = Vec::new();
            if !find_path(component, id, &mut found) || found != path {
                eprintln!(
                    "[style-paths-keep-verify] mismatch: id {id} kept {path:?} found {found:?}"
                );
            }
        }
        return Some(path);
    }
    let mut path = Vec::new();
    if !find_path(component, id, &mut path) {
        return None;
    }
    STYLE_PATHS.with(|paths| paths.borrow_mut().1.insert(id, path.clone()));
    Some(path)
}

fn located_style(
    key: (u64, u64),
    component: &Component,
    id: u32,
) -> Option<&w3cos_std::style::Style> {
    if !crate::layout_ng::box_tree_enabled() {
        return component_style(component, id);
    }
    let path = style_path(key, component, id)?;
    component_at(component, &path).map(|found| &*found.style)
}

fn assign_located(
    key: (u64, u64),
    component: &mut Component,
    id: u32,
    style: &w3cos_std::style::Style,
) -> bool {
    if !crate::layout_ng::box_tree_enabled() {
        return assign_layout_style(component, id, style);
    }
    let Some(path) = style_path(key, component, id) else {
        return false;
    };
    let Some(found) = component_at_mut(component, &path) else {
        return false;
    };
    assign_style_here(found, id, style);
    true
}

/// [`assign_located`] on the retained tree, keeping its flat view.
fn assign_retained(
    key: (u64, u64),
    tree: &mut RetainedTree,
    id: u32,
    style: &w3cos_std::style::Style,
) -> bool {
    if !crate::layout_ng::box_tree_enabled() {
        return assign_located(key, tree, id, style);
    }
    let Some(path) = style_path(key, tree, id) else {
        return false;
    };
    tree.edit_at(&path, |found| assign_style_here(found, id, style))
}

fn assign_style_here(component: &mut Component, id: u32, style: &w3cos_std::style::Style) {
    {
        crate::layout_ng::note_restyled(id);
        let private_metadata = component
            .style
            .custom_properties
            .as_ref()
            .map(|properties| {
                [
                    w3cos_dom::document::SPLIT_INLINE_FRAGMENT_OWNER,
                    w3cos_std::inline_text::CSSOM_OWNER_RANGES,
                    w3cos_std::inline_text::CSSOM_BOX_OWNERS,
                ]
                .into_iter()
                .filter_map(|key| properties.get(key).cloned().map(|value| (key, value)))
                .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        component.style = style.clone().into();
        if !private_metadata.is_empty() {
            let properties = component
                .style
                .custom_properties
                .get_or_insert_with(Default::default);
            for (key, value) in private_metadata {
                properties.insert(key.into(), value);
            }
        }
    }
}

/// Inline boxes and atomic inlines are items of their containing block's
/// inline formatting context (Blink `NGInlineNode`). Their used style and
/// position come from collecting that context, done when the paint tree is
/// built, so a layout edit on one rebuilds instead of patching in place.
fn inline_level_item(style: &w3cos_std::style::Style) -> bool {
    matches!(
        style.display,
        Display::Inline | Display::InlineBlock | Display::InlineFlex | Display::InlineTable
    )
}

/// Replaced content: the builder adjusts its box (atomic-inline flex
/// factors, used size) from the parent's formatting context.
fn component_is_replaced(component: &Component, id: u32) -> bool {
    if host_id(component) == Some(id) {
        return matches!(
            component.kind,
            ComponentKind::Image { .. }
                | ComponentKind::Canvas { .. }
                | ComponentKind::TextInput { .. }
                | ComponentKind::Button { .. }
                | ComponentKind::SvgDocument { .. }
        );
    }
    component
        .children
        .iter()
        .any(|child| component_is_replaced(child, id))
}

/// A block whose siblings are floats or inline-level items sits in a
/// formatting context the builder lays out together (anonymous inline
/// wrappers, float exclusions); Blink relays out that context. A layout
/// edit there rebuilds.
fn shares_inline_or_float_context(component: &Component, id: u32) -> bool {
    if component
        .children
        .iter()
        .any(|child| host_id(child) == Some(id))
    {
        return component.children.iter().any(|child| {
            host_id(child) != Some(id)
                && (child.style.float != Float::None || inline_level_item(&child.style))
        });
    }
    component
        .children
        .iter()
        .any(|child| shares_inline_or_float_context(child, id))
}

fn is_replaced_host(style: &w3cos_std::style::Style) -> bool {
    style
        .custom_properties
        .as_ref()
        .is_some_and(|properties| properties.contains_key("--w3cos-internal-replaced-element"))
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

struct ShiftLists {
    /// Preorder child lists.
    children: Vec<Vec<usize>>,
    /// Absolute or fixed leaves whose used position is a px inset delta.
    shift_ok: Vec<bool>,
}

impl ShiftLists {
    fn of(component: &Component) -> Self {
        let flat = pre_flatten(component);
        Self {
            children: child_lists(&flat),
            shift_ok: absolute_shift_ok(&flat),
        }
    }
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
    /// Preorder child lists and [`absolute_shift_ok`] of `component`, built
    /// on first use by an inset shift (`None` after a rebuild or splice).
    /// Style patches keep both: they change neither the tree nor the styles
    /// these read (see [`paint_geometry_edit`], [`local_paint_only`]).
    shift_lists: Option<ShiftLists>,
    /// The paint journal position this tree and layout equal the flush
    /// cache at (see `splice::paint_splice_enabled`).
    journal: Option<(u64, usize)>,
    /// Preorder subtree sizes of `component`, kept across paint splices
    /// (`splice::paint_splice_incremental`); `None` until known.
    subtree_sizes: Option<Vec<u32>>,
    /// The flat form of `component` the last paint splice made
    /// (`W3COS_PAINT_FLAT_KEEP`). Its references point into `component`;
    /// only the next paint splice reads it, through
    /// [`crate::layout::pre_flatten_patched`] and its change set.
    kept_flat: Option<Vec<crate::layout::FlatNodeInfo<'static>>>,
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
/// Mutations the paint cache drained, kept for the layout cache.
///
/// Both caches read the document's single precise dirty list. Without this,
/// a headless frame between two flushes leaves the retained layout tree
/// unable to tell what changed, so the next flush rebuilds the document.
struct PaintDrained {
    before: u32,
    after: u32,
    ids: Vec<NodeId>,
}

thread_local! {
    static PAINT_DRAINED: RefCell<Option<PaintDrained>> = const { RefCell::new(None) };
}

/// Past this many stashed ids a rebuild is cheaper than replaying them.
const PAINT_DRAINED_MAX: usize = 4096;

fn stash_paint_drained(before: u32, dirty: &[NodeId], after: u32) {
    PAINT_DRAINED.with(|slot| {
        let mut slot = slot.borrow_mut();
        match slot.as_mut() {
            Some(stash) if stash.after == before => {
                stash.after = after;
                stash.ids.extend_from_slice(dirty);
            }
            _ => {
                *slot = Some(PaintDrained {
                    before,
                    after,
                    ids: dirty.to_vec(),
                })
            }
        }
        if slot
            .as_ref()
            .is_some_and(|stash| stash.ids.len() > PAINT_DRAINED_MAX)
        {
            *slot = None;
        }
    });
}

/// Prepend the ids the paint cache drained just before this take, so the
/// layout cache sees an unbroken run of drains.
fn take_paint_drained(before: u32, dirty: Vec<NodeId>, after: u32) -> (u32, Vec<NodeId>, u32) {
    match PAINT_DRAINED.with(|slot| slot.borrow_mut().take()) {
        Some(stash) if stash.after == before => {
            let mut ids = stash.ids;
            ids.extend(dirty);
            (stash.before, ids, after)
        }
        _ => (before, dirty, after),
    }
}

pub fn with_paint_layout<R>(
    viewport_w: f32,
    viewport_h: f32,
    use_layout: impl FnOnce(&Component, &crate::layout::LayoutResults) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    with_paint_layout_flat(viewport_w, viewport_h, |component, results, _| {
        use_layout(component, results)
    })
}

/// `W3COS_SHARED_PAINT_FLAT=1|verify`: a paint splice hands the flat tree
/// it made of the spliced paint tree to the paint that follows, which
/// would flatten the same tree again; `verify` flattens it anyway and
/// compares. On by default (`=0` turns it off).
/// `W3COS_PAINT_FLAT_KEEP=1|verify`: a paint splice keeps the flat form of
/// the paint tree it made, and the next one re-flattens only the subtrees
/// its change set touched, copying the rest (Blink keeps its layout object
/// tree across frames). `verify` also flattens the whole tree and compares.
/// On by default (`=0` turns it off).
pub(crate) fn paint_flat_keep_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PAINT_FLAT_KEEP_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_FLAT_KEEP").ok().as_deref() {
            Some("0" | "false") => 0,
            Some("verify") => 2,
            _ => 1,
        },
    )
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_FLAT_KEEP_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// (patched flattens, full flattens, verify mismatches).
    static PAINT_FLAT_KEEP_STATS: std::cell::Cell<(u64, u64, u64)> =
        const { std::cell::Cell::new((0, 0, 0)) };
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn paint_flat_keep_stats() -> (u64, u64, u64) {
    PAINT_FLAT_KEEP_STATS.with(std::cell::Cell::get)
}

pub(crate) fn shared_paint_flat_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = SHARED_PAINT_FLAT_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_SHARED_PAINT_FLAT").ok().as_deref() {
            Some("0" | "false") => 0,
            Some("verify") => 2,
            _ => 1,
        },
    )
}

#[cfg(test)]
thread_local! {
    pub(crate) static SHARED_PAINT_FLAT_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

/// [`with_paint_layout`], also passing the paint tree's flat form when this
/// paint layout already made it (see [`shared_paint_flat_mode`]).
pub fn with_paint_layout_flat<R>(
    viewport_w: f32,
    viewport_h: f32,
    use_layout: impl FnOnce(
        &Component,
        &crate::layout::LayoutResults,
        Option<&[crate::layout::FlatNodeInfo<'_>]>,
    ) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    let mut use_layout = Some(use_layout);
    // Paint after layout (the document lifecycle): a structural change is
    // laid out by the retained flush first, and its splices are replayed on
    // the paint tree below.
    let flushed = splice::paint_splice_enabled() && flush_before_paint(viewport_w, viewport_h);
    let (structure, cascade) = crate::dom::with_document(|document| {
        (
            document.structure_generation(),
            document.cascade_generation(),
        )
    });
    let dirty = crate::dom::with_document_mut(|document| {
        let before = document.precise_drain_epoch();
        let dirty = document.take_precise_dirty();
        stash_paint_drained(before, &dirty, document.precise_drain_epoch());
        dirty
    });
    // The flush cache equals the document now, so a paint tree built or
    // spliced now can follow its later splices.
    let synced = flushed && dirty.is_empty();
    // A geometry read can set this and never paint. The paint that follows
    // records its own reason.
    note_paint_reuse(PaintReuse::None);
    PAINT_LAYOUT_GENERATION.with(|generation| generation.set(generation.get() + 1));
    PAINT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let same_viewport = |cache: &PaintCache| {
            cache.viewport_w_bits == viewport_w.to_bits()
                && cache.viewport_h_bits == viewport_h.to_bits()
        };
        let reusable = slot
            .as_ref()
            .is_some_and(|cache| cache.structure == structure && same_viewport(cache));
        // `W3COS_STYLE_LOCAL`: a style edit the flush spliced left a journal
        // entry; replay it on the paint tree (as a structural splice is)
        // instead of laying the paint tree out again, so paint also learns
        // which boxes changed.
        let journaled = reusable
            && style_splice_enabled()
            && synced
            && slot
                .as_ref()
                .and_then(|cache| cache.journal)
                .is_some_and(|point| {
                    let now = splice::paint_journal_point();
                    now.0 == point.0 && now.1 > point.1
                });
        if journaled {
            let cache = slot.as_mut().unwrap();
            let point = cache.journal.expect("journal point");
            if let Some(painted) = splice_paint_cache(
                cache,
                point,
                structure,
                cascade,
                viewport_w,
                viewport_h,
                &mut use_layout,
            ) {
                return painted;
            }
            // Not replayed (or partly): rebuild below.
        } else if reusable {
            let cache = slot.as_mut().unwrap();
            if sync_paint_cache(cache, &dirty, cascade, viewport_w, viewport_h) {
                let use_layout = use_layout.take().expect("paint layout user");
                return use_layout(&cache.component, &cache.results, None);
            }
        } else if synced
            && let Some(cache) = slot.as_mut()
            && same_viewport(cache)
            && let Some(point) = cache.journal
            && let Some(painted) = splice_paint_cache(
                cache,
                point,
                structure,
                cascade,
                viewport_w,
                viewport_h,
                &mut use_layout,
            )
        {
            return painted;
        }
        *slot = None;
        rebuild_paint_cache(&mut slot, structure, cascade, viewport_w, viewport_h)?;
        let cache = slot.as_mut().expect("paint layout rebuild stores a cache");
        if synced {
            cache.journal = Some(splice::paint_journal_point());
        }
        let use_layout = use_layout.take().expect("paint layout user");
        use_layout(&cache.component, &cache.results, None)
    })
}

/// Run the retained flush when the paint tree is behind the document and
/// the flush lays out the viewport this paint uses. True when the flush
/// cache now matches the document.
fn flush_before_paint(viewport_w: f32, viewport_h: f32) -> bool {
    let (vw, vh, _) = crate::jsdom::viewport();
    if (vw as f32).to_bits() != viewport_w.to_bits()
        || (vh as f32).to_bits() != viewport_h.to_bits()
    {
        return false;
    }
    if flush_cache_current(viewport_w, viewport_h) {
        return true;
    }
    let structure = crate::dom::with_document(|document| document.structure_generation());
    // No paint tree yet counts as behind: flushing first lets the first
    // paint start in sync with the flush cache, so the next structural
    // change splices the paint tree instead of building it again.
    let behind = PAINT.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_none_or(|cache| cache.structure != structure)
    });
    let retained = CACHE.with(|slot| slot.borrow().is_some());
    behind && retained && flush()
}

/// The flush cache has seen every mutation: nothing is recorded or drained
/// since its last flush, and no generation moved.
fn flush_cache_current(viewport_w: f32, viewport_h: f32) -> bool {
    let (layout_gen, text_gen, cascade, epoch, pending) = crate::dom::with_document(|document| {
        (
            document.layout_generation(),
            document.repaint_text_generation(),
            document.cascade_generation(),
            document.precise_drain_epoch(),
            document.has_precise_dirty(),
        )
    });
    if pending || PAINT_DRAINED.with(|stash| stash.borrow().is_some()) {
        return false;
    }
    CACHE.with(|slot| {
        slot.borrow().as_ref().is_some_and(|cache| {
            cache.structure == layout_gen
                && cache.source_structure == text_gen
                && cache.cascade == cascade
                && cache.drain_epoch == epoch
                && cache.sheet_generation == w3cos_dom::stylesheet::generation()
                && cache.viewport_w_bits == viewport_w.to_bits()
                && cache.viewport_h_bits == viewport_h.to_bits()
        })
    })
}

#[cfg(test)]
thread_local! {
    static PAINT_SPLICES: Cell<u32> = const { Cell::new(0) };
    static PAINT_SPLICE_MISMATCHES: Cell<u32> = const { Cell::new(0) };
}

/// Test override of the paint splice (`None`: the default).
#[cfg(test)]
pub(crate) fn force_paint_splice(forced: Option<bool>) {
    splice::FORCE_PAINT_SPLICE.with(|force| force.set(forced));
}

/// (paint layouts spliced, spliced layouts that differed from a full one).
#[cfg(test)]
pub(crate) fn paint_splice_stats() -> (u32, u32) {
    (
        PAINT_SPLICES.with(Cell::get),
        PAINT_SPLICE_MISMATCHES.with(Cell::get),
    )
}

/// Replay the flush cache's splices since `point` on the paint tree and
/// layout. `Some(true)`: the cache is current. `Some(false)`: it may be
/// partly edited and must be rebuilt. `None`: untouched.
/// Preorder subtree sizes from a flat tree's subtree ends.
thread_local! {
    static PAINT_LAYOUT_GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Counts paint layouts on this thread: a change set describes the step
/// from the paint layout before its own to its own.
pub(crate) fn paint_layout_generation() -> u64 {
    PAINT_LAYOUT_GENERATION.with(std::cell::Cell::get)
}

fn flat_subtree_sizes(flat: &[crate::layout::FlatNodeInfo<'_>]) -> Vec<u32> {
    flat.iter()
        .enumerate()
        .map(|(index, node)| (node.subtree_end - index) as u32)
        .collect()
}

fn splice_paint_cache<R, F>(
    cache: &mut PaintCache,
    point: (u64, usize),
    structure: u64,
    cascade: u64,
    viewport_w: f32,
    viewport_h: f32,
    use_layout: &mut Option<F>,
) -> Option<anyhow::Result<R>>
where
    F: FnOnce(
        &Component,
        &crate::layout::LayoutResults,
        Option<&[crate::layout::FlatNodeInfo<'_>]>,
    ) -> anyhow::Result<R>,
{
    let (applied, count) = splice::replay_paint_splices(
        point,
        &mut cache.component,
        &mut cache.results.layout_cache,
        &mut cache.results.text_fragments,
        &mut cache.subtree_sizes,
    )?;
    if !applied {
        let _ = splice::take_paint_changes();
        return None;
    }
    if count == 0 {
        let _ = splice::take_paint_changes();
        // The structure moved without a splice (an off-document edit, or
        // a change the flush did not lay out): rebuild.
        return None;
    }
    let flat_keep = paint_flat_keep_mode();
    let kept_flat = cache.kept_flat.take();
    let flat = match kept_flat {
        Some(kept) if flat_keep != 0 => {
            let patched = splice::with_paint_changes(|changes| {
                let changes =
                    changes.filter(|changes| changes.generation == paint_layout_generation())?;
                // SAFETY: `kept` is the flat form the last paint splice made
                // of `cache.component`, which only paint splices change in
                // structure (style patches assign in place); this replay's
                // change set names every box its splices touched.
                let flat = unsafe {
                    crate::layout::pre_flatten_patched(&cache.component, &kept, &changes.changed)
                };
                (flat.len() == changes.len).then_some(flat)
            });
            let mut stats = PAINT_FLAT_KEEP_STATS.with(std::cell::Cell::get);
            let flat = match patched {
                Some(flat) => {
                    stats.0 += 1;
                    if flat_keep == 2 {
                        let fresh = pre_flatten(&cache.component);
                        if crate::layout::same_flat(&flat, &fresh) {
                            flat
                        } else {
                            stats.2 += 1;
                            eprintln!(
                                "[paint-flat-keep-verify] mismatch: the patched flat tree differs"
                            );
                            fresh
                        }
                    } else {
                        flat
                    }
                }
                None => {
                    stats.1 += 1;
                    pre_flatten(&cache.component)
                }
            };
            PAINT_FLAT_KEEP_STATS.with(|cell| cell.set(stats));
            flat
        }
        _ => pre_flatten(&cache.component),
    };
    if splice::paint_splice_incremental() {
        cache.subtree_sizes = Some(flat_subtree_sizes(&flat));
    }
    let slots = crate::layout::layout_slots(&cache.results.layout_cache, flat.len());
    let (scrollable, clip_only, scroll_ancestor) = if crate::layout::paint_scroll_plan_mode() != 0 {
        splice::with_paint_changes(|changes| {
            crate::layout::scroll_containers_after_paint_splice(
                &cache.results.layout_cache,
                &flat,
                Some((viewport_w, viewport_h)),
                &slots,
                changes,
                paint_layout_generation(),
            )
        })
    } else {
        crate::layout::scroll_containers_from_layout(
            &cache.results.layout_cache,
            &flat,
            Some((viewport_w, viewport_h)),
        )
    };
    cache.results.scrollable_nodes = scrollable;
    cache.results.clip_only_nodes = clip_only;
    cache.results.scroll_ancestor = scroll_ancestor;
    cache.results.layout_slots = Some(slots);
    cache.node_ids = flat.iter().map(flat_host_id).collect();
    cache.shift_lists = None;
    let shared = shared_paint_flat_mode() != 0;
    let flat = if flat_keep != 0 {
        // SAFETY (`W3COS_PAINT_FLAT_KEEP`): kept with the tree it points
        // into; read only by this frame's paint (below, borrowed from the
        // cache) and the next paint splice of this cache (see the field).
        cache.kept_flat = Some(unsafe {
            std::mem::transmute::<
                Vec<crate::layout::FlatNodeInfo<'_>>,
                Vec<crate::layout::FlatNodeInfo<'static>>,
            >(flat)
        });
        None
    } else if shared {
        Some(flat)
    } else {
        drop(flat);
        None
    };
    // The retained Taffy tree no longer matches the spliced component.
    let mut engine = LayoutEngine::new();
    engine.set_skip_projections(can_skip_projections(&cache.component));
    engine.set_measure_text(true);
    cache.engine = engine;
    cache.structure = structure;
    cache.cascade = cascade;
    // The paint tree now matches the journal's end, and it is the journal's
    // only reader: drop the replayed entries (`W3COS_PAINT_JOURNAL_TRIM`), so
    // the journal's size caps never force a full paint rebuild.
    cache.journal = Some(if splice::paint_journal_trim_enabled() {
        splice::trim_paint_journal()
    } else {
        splice::paint_journal_point()
    });
    note_paint_patch();
    #[cfg(test)]
    PAINT_SPLICES.with(|count| count.set(count.get() + 1));
    if splice::paint_splice_verify() {
        verify_paint_splice(cache, count, viewport_w, viewport_h);
    }
    let use_layout = use_layout.take().expect("paint layout user");
    let kept = cache
        .kept_flat
        .as_deref()
        .filter(|_| shared && flat_keep != 0);
    Some(use_layout(
        &cache.component,
        &cache.results,
        flat.as_deref().or(kept),
    ))
}

/// [`splice::paint_splice_verify`]: compare the spliced paint tree and
/// layout with a full lowering and layout of the document.
fn verify_paint_splice(cache: &PaintCache, count: usize, viewport_w: f32, viewport_h: f32) {
    let component = crate::dom::to_component_tree();
    let flat = pre_flatten(&component);
    let mut engine = LayoutEngine::new();
    engine.set_skip_projections(can_skip_projections(&component));
    engine.set_measure_text(true);
    let Ok(fresh) = engine.compute(&component, &flat, viewport_w, viewport_h) else {
        eprintln!("[paint-splice-verify] full layout failed");
        return;
    };
    let tree_same = format!("{component:?}") == format!("{:?}", cache.component);
    let kept = &cache.results;
    let mut max_diff = 0f32;
    let layouts_same = kept.layout_cache.len() == fresh.layout_cache.len()
        && kept
            .layout_cache
            .iter()
            .zip(&fresh.layout_cache)
            .all(|((a, i), (b, j))| {
                for (x, y) in [
                    (a.x, b.x),
                    (a.y, b.y),
                    (a.width, b.width),
                    (a.height, b.height),
                ] {
                    max_diff = max_diff.max((x - y).abs());
                }
                i == j && a == b
            });
    let texts_same = kept.text_fragments == fresh.text_fragments;
    let scroll_same = format!("{:?}", kept.scrollable_nodes)
        == format!("{:?}", fresh.scrollable_nodes)
        && format!("{:?}", kept.clip_only_nodes) == format!("{:?}", fresh.clip_only_nodes)
        && kept.scroll_ancestor == fresh.scroll_ancestor;
    let same = tree_same && layouts_same && texts_same && scroll_same;
    eprintln!(
        "[paint-splice-verify] {count} splices: {} (tree {tree_same} layouts {layouts_same} {}/{} max {max_diff} texts {texts_same} {}/{} scroll {scroll_same})",
        if same { "same" } else { "DIFFERS" },
        kept.layout_cache.len(),
        fresh.layout_cache.len(),
        kept.text_fragments.len(),
        fresh.text_fragments.len(),
    );
    #[cfg(test)]
    if !same {
        PAINT_SPLICE_MISMATCHES.with(|count| count.set(count.get() + 1));
    }
}

/// After a multicol layout, record how many ::column boxes each
/// `scroll-marker-group` container produced so the next tree build can
/// emit that many `::column::scroll-marker` children (CSS Overflow 5).
pub(crate) fn sync_column_scroll_marker_counts(
    flat: &[crate::layout::FlatNodeInfo<'_>],
    layout_cache: &[(crate::layout::LayoutRect, usize)],
    viewport: (f32, f32),
) -> bool {
    let mut changed = false;
    let mut rects = vec![None; flat.len()];
    for &(rect, index) in layout_cache {
        if index < rects.len() {
            rects[index] = Some(rect);
        }
    }
    let nodes: Vec<(&w3cos_std::style::Style, Option<usize>)> =
        flat.iter().map(|node| (node.style, node.parent)).collect();
    let plans = crate::layout::resolve_column_fragment_plans(&nodes, &rects, Some(viewport));
    for (index, node) in flat.iter().enumerate() {
        if node.style.scroll_marker_group == ScrollMarkerGroup::None {
            continue;
        }
        let EventAction::NativeHost { id, .. } = node.on_click else {
            continue;
        };
        let count = plans
            .get(index)
            .and_then(|plan| plan.as_ref())
            .map(|plan| plan.bands.len())
            .unwrap_or(0);
        if crate::dom::set_column_pseudo_count(*id as u32, count) {
            changed = true;
        }
    }
    changed
}

fn rebuild_paint_cache(
    slot: &mut Option<PaintCache>,
    structure: u64,
    cascade: u64,
    viewport_w: f32,
    viewport_h: f32,
) -> anyhow::Result<()> {
    let mut component = crate::dom::to_component_tree();
    let mut flat = pre_flatten(&component);
    let mut engine = LayoutEngine::new();
    // Floats, tables, and inline-block need the post-Taffy projections. A
    // block/absolute tree already matches `compute_with_scroll_details`
    // without that walk, which is most of a retained frame.
    engine.set_skip_projections(can_skip_projections(&component));
    engine.set_measure_text(true);
    let mut results = engine.compute(&component, &flat, viewport_w, viewport_h)?;
    if sync_column_scroll_marker_counts(&flat, &results.layout_cache, (viewport_w, viewport_h)) {
        // Column count is a layout-time fact. Rebuild the adjunct
        // ::scroll-marker-group with the right number of markers and
        // lay the tree out once more (CSS Overflow 5 / Blink).
        component = crate::dom::to_component_tree();
        flat = pre_flatten(&component);
        engine = LayoutEngine::new();
        engine.set_skip_projections(can_skip_projections(&component));
        engine.set_measure_text(true);
        results = engine.compute(&component, &flat, viewport_w, viewport_h)?;
    }
    let node_ids = flat.iter().map(flat_host_id).collect::<Vec<_>>();
    write_rects(&flat, &results.layout_cache, &results.text_fragments);
    let subtree_sizes = splice::paint_splice_incremental().then(|| flat_subtree_sizes(&flat));
    drop(flat);
    *slot = Some(PaintCache {
        component,
        engine,
        structure,
        cascade,
        viewport_w_bits: viewport_w.to_bits(),
        viewport_h_bits: viewport_h.to_bits(),
        node_ids,
        results,
        shift_lists: None,
        journal: None,
        subtree_sizes,
        kept_flat: None,
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
    let mut paint_only = true;
    for id in dirty {
        let style = crate::dom::with_document(|document| document.computed_style_for(*id));
        let Some(old) = component_style(&cache.component, id.as_u32()) else {
            return false;
        };
        if !style_patch_is_safe(&style) {
            return false;
        }
        if paint_geometry_edit(old, &style) {
            // An inline box is laid out as items of its containing block's
            // inline formatting context. Blink marks that block
            // `NeedsCollectInlines` on a layout-affecting inline style
            // change; the items here are collected when the paint tree is
            // built, so such an edit rebuilds it.
            if old != &style
                && (inline_level_item(old)
                    || inline_level_item(&style)
                    || component_is_replaced(&cache.component, id.as_u32())
                    || shares_inline_or_float_context(&cache.component, id.as_u32()))
            {
                return false;
            }
            paint_only = false;
            insets_only &= paint_insets_only(old, &style);
        } else if local_paint_only(old, &style) {
            insets_only = false;
        } else {
            return false;
        }
        styles.push((id.as_u32(), style));
    }
    for (id, style) in &styles {
        if !assign_layout_style(&mut cache.component, *id, style) {
            return false;
        }
        if paint_only {
            continue;
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
    if paint_only {
        cache.cascade = cascade;
        note_paint_patch();
        return true;
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
    write_rects(&flat, &results.layout_cache, &results.text_fragments);
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
        let lists = cache
            .shift_lists
            .get_or_insert_with(|| ShiftLists::of(&cache.component));
        if !lists.shift_ok.get(flat_index).copied().unwrap_or(false) {
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
    let Some(lists) = cache.shift_lists.as_ref() else {
        return Some(false);
    };
    for (_, _, index, dx, dy) in &plan {
        if !shift_descendants(
            &mut cache.results.layout_cache,
            &lists.children,
            *index,
            *dx,
            *dy,
        ) {
            return Some(false);
        }
    }
    write_host_rects(&cache.node_ids, &cache.results.layout_cache);
    let flat = pre_flatten(&cache.component);
    publish_client_rects(&flat, &cache.results.layout_cache, &[]);
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

/// A flush's DOM host id per flat index (`u32::MAX` for none), with its
/// hosts grouped by DOM id on first use.
struct HostIds {
    ids: Vec<u32>,
    groups: std::cell::OnceCell<std::rc::Rc<HostGroups>>,
}

/// The flat indices of each DOM host id (CSR: `members[starts[g]..starts[g
/// + 1]]` are the flat indices of `ids[g]`, ascending).
struct HostGroups {
    ids: Vec<u32>,
    starts: Vec<u32>,
    members: Vec<u32>,
}

impl From<Vec<u32>> for HostIds {
    fn from(ids: Vec<u32>) -> Self {
        Self {
            ids,
            groups: std::cell::OnceCell::new(),
        }
    }
}

impl FromIterator<u32> for HostIds {
    fn from_iter<I: IntoIterator<Item = u32>>(iter: I) -> Self {
        Vec::from_iter(iter).into()
    }
}

impl std::ops::Deref for HostIds {
    type Target = [u32];

    fn deref(&self) -> &[u32] {
        &self.ids
    }
}

impl HostIds {
    fn groups(&self) -> &std::rc::Rc<HostGroups> {
        self.groups.get_or_init(|| {
            let mut pairs = self
                .ids
                .iter()
                .enumerate()
                .filter(|(_, id)| **id != u32::MAX)
                .map(|(index, id)| (*id, index as u32))
                .collect::<Vec<_>>();
            pairs.sort_unstable();
            let mut groups = HostGroups {
                ids: Vec::new(),
                starts: Vec::new(),
                members: Vec::with_capacity(pairs.len()),
            };
            for (id, index) in pairs {
                if groups.ids.last() != Some(&id) {
                    groups.ids.push(id);
                    groups.starts.push(groups.members.len() as u32);
                }
                groups.members.push(index);
            }
            groups.starts.push(groups.members.len() as u32);
            std::rc::Rc::new(groups)
        })
    }
}

impl HostGroups {
    /// Each host's last entry in `slots` (u32::MAX: no entry), as
    /// [`write_host_rects`] leaves it: the latest write of a DOM id wins.
    fn last_slots<'a>(&'a self, slots: &'a [u32]) -> impl Iterator<Item = (u32, u32)> + 'a {
        self.ids.iter().enumerate().map(move |(group, id)| {
            let members =
                &self.members[self.starts[group] as usize..self.starts[group + 1] as usize];
            let last = members
                .iter()
                .map(|index| slots[*index as usize])
                .filter(|slot| *slot != u32::MAX)
                .max()
                .unwrap_or(u32::MAX);
            (*id, last)
        })
    }
}

/// A retained flush stores DOM layout rects on their first read instead
/// of on every flush; on unless `W3COS_LAZY_DOM_RECTS=0`.
fn lazy_dom_rects() -> bool {
    static LAZY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LAZY.get_or_init(|| std::env::var_os("W3COS_LAZY_DOM_RECTS").is_none_or(|v| v != "0"))
}

/// A retained flush's host rects, stored when the document first reads one.
struct LazyHostRects {
    groups: std::rc::Rc<HostGroups>,
    slots: Vec<u32>,
    pending: std::rc::Rc<PendingClientRects>,
}

impl w3cos_dom::document::PendingLayoutRects for LazyHostRects {
    fn for_each(&self, f: &mut dyn FnMut(NodeId, w3cos_dom::DOMRect)) {
        for (id, slot) in self.groups.last_slots(&self.slots) {
            if let Some((rect, _)) = self.pending.layouts.get(slot as usize) {
                f(
                    NodeId::from_u32(id),
                    w3cos_dom::DOMRect::new(rect.x, rect.y, rect.width, rect.height),
                );
            }
        }
    }
}

thread_local! {
    /// The last deferred host rects: their groups and which groups they
    /// write (a later flush writing the same ones supersedes them).
    static LAST_HOST_DEFER: RefCell<Option<(usize, Vec<u64>)>> = const { RefCell::new(None) };
}

/// [`write_host_rects`] for the flush whose rects were just deferred
/// (`defer_client_rects`), stored on first read.
fn publish_host_rects(node_ids: &HostIds, slots: Option<Vec<u32>>) {
    let Some(pending) = pending_client_rects() else {
        return;
    };
    let Some(slots) = slots.filter(|slots| slots.len() == node_ids.len()) else {
        LAST_HOST_DEFER.with(|last| *last.borrow_mut() = None);
        write_host_rects(node_ids, &pending.layouts);
        return;
    };
    debug_assert_eq!(
        slots,
        crate::layout::layout_slots(&pending.layouts, node_ids.len())
    );
    let groups = node_ids.groups().clone();
    let mut written = vec![0u64; groups.ids.len().div_ceil(64)];
    for (group, (_, slot)) in groups.last_slots(&slots).enumerate() {
        if slot != u32::MAX {
            written[group / 64] |= 1 << (group % 64);
        }
    }
    let key = std::rc::Rc::as_ptr(&groups) as usize;
    let supersedes = LAST_HOST_DEFER.with(|last| {
        let mut last = last.borrow_mut();
        let same = last
            .as_ref()
            .is_some_and(|(prev, bits)| *prev == key && *bits == written);
        *last = Some((key, written));
        same
    });
    let lazy = LazyHostRects {
        groups,
        slots,
        pending,
    };
    #[cfg(debug_assertions)]
    {
        // The final rect per node, as `write_host_rects` stores them.
        let mut eager = HashMap::new();
        for (rect, index) in &lazy.pending.layouts {
            if let Some(&id) = node_ids.get(*index)
                && id != u32::MAX
            {
                eager.insert(id, *rect);
            }
        }
        let mut deferred = HashMap::new();
        w3cos_dom::document::PendingLayoutRects::for_each(&lazy, &mut |id, rect| {
            deferred.insert(id.as_u32(), (rect.x, rect.y, rect.width, rect.height));
        });
        debug_assert_eq!(eager.len(), deferred.len());
        for (id, rect) in eager {
            debug_assert_eq!(
                deferred.get(&id),
                Some(&(rect.x, rect.y, rect.width, rect.height))
            );
        }
    }
    crate::dom::with_document_mut(|document| {
        document.defer_layout_rects(Box::new(lazy), supersedes)
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

/// Background, opacity, border colors, radii, shadows, outlines, and
/// `z-index` do not change used boxes. Color and font stay in the comparison
/// because descendants copy them when the component tree is built.
fn local_paint_only(old: &w3cos_std::style::Style, new: &w3cos_std::style::Style) -> bool {
    if old == new {
        return true;
    }
    let mut left = old.clone();
    let mut right = new.clone();
    neutralize_local_paint(&mut left);
    neutralize_local_paint(&mut right);
    left == right
}

fn neutralize_local_paint(style: &mut w3cos_std::style::Style) {
    use w3cos_std::style::OutlineStyle;
    let clear = w3cos_std::color::Color::BLACK;
    style.background = clear;
    style.background_image = None;
    style.background_size = None;
    style.background_position = None;
    style.background_repeat = None;
    style.background_origin = None;
    style.background_clip = None;
    style.background_attachment = None;
    style.background_blend_mode = None;
    style.opacity = 1.0;
    style.border_color = clear;
    style.border_top_color = None;
    style.border_right_color = None;
    style.border_bottom_color = None;
    style.border_left_color = None;
    style.border_radius = 0.0;
    style.border_top_left_radius = None;
    style.border_top_right_radius = None;
    style.border_bottom_right_radius = None;
    style.border_bottom_left_radius = None;
    style.box_shadow = None;
    style.box_shadows.clear();
    style.outline_width = 0.0;
    style.outline_color = clear;
    style.outline_style = OutlineStyle::None;
    style.z_index = 0;
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
        // Same entry as the retained paint path (`LayoutEngine::compute`),
        // so the oracle follows the process mode instead of the always-legacy
        // Taffy helper.
        let component = crate::dom::to_component_tree();
        let flat = crate::layout::pre_flatten(&component);
        let results = crate::layout::LayoutEngine::new()
            .compute(&component, &flat, 1024.0, 768.0)
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
    fn paint_splice_replays_flush_splices_and_matches_full_layout() {
        struct Force;
        impl Drop for Force {
            fn drop(&mut self) {
                splice::FORCE_PAINT_SPLICE.with(|force| force.set(None));
            }
        }
        crate::dom::reset_document();
        invalidate();
        splice::FORCE_PAINT_SPLICE.with(|force| force.set(Some(true)));
        let _force = Force;
        let log = crate::dom::create_element("div");
        crate::dom::set_style_property(log, "width", "300px");
        // A closed bottom edge lets an appended message splice as a tail.
        crate::dom::set_style_property(log, "padding-bottom", "2px");
        let mut messages = Vec::new();
        for index in 0..6 {
            let message = crate::dom::create_element("div");
            crate::dom::set_style_property(message, "padding", "4px");
            let paragraph = crate::dom::create_element("p");
            let text = crate::dom::create_text_node(&format!("message {index} with some words"));
            crate::dom::append_child(paragraph, text);
            crate::dom::append_child(message, paragraph);
            crate::dom::append_child(log, message);
            messages.push(message);
        }
        let footer = crate::dom::create_element("div");
        crate::dom::set_style_property(footer, "height", "20px");
        crate::dom::append_child(crate::dom::body_id(), log);
        crate::dom::append_child(crate::dom::body_id(), footer);
        let streamed = crate::dom::create_element("p");
        let text = crate::dom::create_text_node("streamed");
        crate::dom::append_child(streamed, text);
        crate::dom::append_child(messages[2], streamed);
        assert!(flush());
        let (spliced_before, mismatches_before) = paint_splice_stats();
        let _ = paint_rect(footer);
        let mut content = String::from("streamed");
        for step in 0..8 {
            if step != 5 {
                content.push_str(" more streamed words that wrap onto new lines");
                crate::dom::set_text_content(text, &content);
            }
            if step % 2 == 0 {
                assert!(flush());
            }
            if step == 5 {
                // A child-list splice: a new message after the streamed one.
                let message = crate::dom::create_element("div");
                let paragraph = crate::dom::create_element("p");
                let words = crate::dom::create_text_node("a new message");
                crate::dom::append_child(paragraph, words);
                crate::dom::append_child(message, paragraph);
                crate::dom::append_child(log, message);
            }
            let painted = paint_rect(footer);
            let fresh = fresh_rect(footer);
            assert!(
                (painted.y - fresh.y).abs() < 0.05 && (painted.height - fresh.height).abs() < 0.05,
                "step {step}: spliced paint rect {painted:?} diverges from full layout {fresh:?}"
            );
        }
        let (spliced, mismatches) = paint_splice_stats();
        assert_eq!(
            mismatches - mismatches_before,
            0,
            "a spliced paint layout differs from a full one"
        );
        if crate::layout_ng::mode() == crate::layout_ng::Mode::On {
            assert!(
                spliced - spliced_before >= 8,
                "streamed text must splice the paint layout, got {}",
                spliced - spliced_before
            );
        }
    }

    #[test]
    fn trimmed_paint_journal_keeps_splicing_past_its_entry_cap() {
        struct Force;
        impl Drop for Force {
            fn drop(&mut self) {
                splice::FORCE_PAINT_SPLICE.with(|force| force.set(None));
                splice::PAINT_JOURNAL_TRIM_OVERRIDE.with(|trim| trim.set(None));
            }
        }
        crate::dom::reset_document();
        invalidate();
        splice::FORCE_PAINT_SPLICE.with(|force| force.set(Some(true)));
        splice::PAINT_JOURNAL_TRIM_OVERRIDE.with(|trim| trim.set(Some(true)));
        let _force = Force;
        let log = crate::dom::create_element("div");
        crate::dom::set_style_property(log, "width", "300px");
        let paragraph = crate::dom::create_element("p");
        let text = crate::dom::create_text_node("streamed");
        crate::dom::append_child(paragraph, text);
        crate::dom::append_child(log, paragraph);
        let footer = crate::dom::create_element("div");
        crate::dom::set_style_property(footer, "height", "20px");
        crate::dom::append_child(crate::dom::body_id(), log);
        crate::dom::append_child(crate::dom::body_id(), footer);
        assert!(flush());
        let _ = paint_rect(footer);
        let (spliced_before, mismatches_before) = paint_splice_stats();
        // More paints, each after one journaled splice, than the journal's
        // entry cap: untrimmed, the cap clears it and a paint rebuilds.
        let steps = 300;
        for step in 0..steps {
            crate::dom::set_text_content(text, &format!("streamed word {step}"));
            assert!(flush());
            let painted = paint_rect(footer);
            let fresh = fresh_rect(footer);
            assert!(
                (painted.y - fresh.y).abs() < 0.05,
                "step {step}: spliced paint rect {painted:?} diverges from full layout {fresh:?}"
            );
        }
        let (spliced, mismatches) = paint_splice_stats();
        assert_eq!(mismatches, mismatches_before);
        if crate::layout_ng::mode() == crate::layout_ng::Mode::On {
            assert_eq!(
                spliced - spliced_before,
                steps,
                "every paint spliced, none rebuilt"
            );
        }
    }

    #[test]
    fn first_paint_with_pending_mutations_starts_a_splice_journal() {
        struct Force;
        impl Drop for Force {
            fn drop(&mut self) {
                splice::FORCE_PAINT_SPLICE.with(|force| force.set(None));
            }
        }
        crate::dom::reset_document();
        invalidate();
        splice::FORCE_PAINT_SPLICE.with(|force| force.set(Some(true)));
        let _force = Force;
        let log = crate::dom::create_element("div");
        crate::dom::set_style_property(log, "width", "300px");
        crate::dom::set_style_property(log, "padding-bottom", "2px");
        let paragraph = crate::dom::create_element("p");
        let text = crate::dom::create_text_node("first words");
        crate::dom::append_child(paragraph, text);
        crate::dom::append_child(log, paragraph);
        let footer = crate::dom::create_element("div");
        crate::dom::set_style_property(footer, "height", "20px");
        crate::dom::append_child(crate::dom::body_id(), log);
        crate::dom::append_child(crate::dom::body_id(), footer);
        assert!(flush());
        // A mutation the flush cache has not seen yet when the first paint
        // runs: the paint must still leave a journal point behind.
        let mut content = String::from("first words");
        content.push_str(" and more words that wrap onto another line");
        crate::dom::set_text_content(text, &content);
        let (rebuilds_before, _) = paint_layout_stats();
        let (spliced_before, mismatches_before) = paint_splice_stats();
        let _ = paint_rect(footer);
        for step in 0..3 {
            content.push_str(" and more words that wrap onto another line");
            crate::dom::set_text_content(text, &content);
            let painted = paint_rect(footer);
            let fresh = fresh_rect(footer);
            assert!(
                (painted.y - fresh.y).abs() < 0.05 && (painted.height - fresh.height).abs() < 0.05,
                "step {step}: paint rect {painted:?} diverges from full layout {fresh:?}"
            );
        }
        let (rebuilds, _) = paint_layout_stats();
        let (spliced, mismatches) = paint_splice_stats();
        assert_eq!(mismatches - mismatches_before, 0);
        if crate::layout_ng::mode() == crate::layout_ng::Mode::On {
            assert_eq!(
                rebuilds - rebuilds_before,
                1,
                "only the first paint may build the paint tree"
            );
            assert!(spliced - spliced_before >= 3, "later paints must splice");
        }
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
        // The Taffy patch counters describe the legacy retained tree.
        // LayoutNG replaces that tree, so the default engine rebuilds.
        if crate::layout_ng::mode() == crate::layout_ng::Mode::Off {
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
        }
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
        if crate::layout_ng::mode() == crate::layout_ng::Mode::Off {
            assert_eq!(paint_shift_count() - shifts, 1);
        }
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
    fn absolute_shift_lists_survive_style_patches_and_appends() {
        crate::dom::reset_document();
        let layer = crate::dom::create_element("div");
        crate::dom::set_style_property(layer, "position", "relative");
        crate::dom::set_style_property(layer, "width", "400px");
        crate::dom::set_style_property(layer, "height", "300px");
        let mover = crate::dom::create_element("div");
        for (name, value) in [
            ("position", "absolute"),
            ("width", "20px"),
            ("height", "20px"),
            ("left", "8px"),
            ("top", "0px"),
        ] {
            crate::dom::set_style_property(mover, name, value);
        }
        let inner = crate::dom::create_element("div");
        crate::dom::set_style_property(inner, "width", "6px");
        crate::dom::set_style_property(inner, "height", "6px");
        crate::dom::append_child(mover, inner);
        let sibling = crate::dom::create_element("div");
        crate::dom::set_style_property(sibling, "width", "50px");
        crate::dom::set_style_property(sibling, "height", "10px");
        crate::dom::append_child(layer, sibling);
        crate::dom::append_child(layer, mover);
        crate::dom::append_child(crate::dom::body_id(), layer);
        let close = |a: crate::layout::LayoutRect, b: crate::layout::LayoutRect| {
            (a.x - b.x).abs() < 0.05
                && (a.y - b.y).abs() < 0.05
                && (a.width - b.width).abs() < 0.05
                && (a.height - b.height).abs() < 0.05
        };
        let _ = paint_rect(inner);
        let shifts = paint_shift_count();
        // First shift builds the lists; a geometry patch keeps them; the
        // second shift reuses them; an append drops them; the third shift
        // builds them again for the new tree.
        crate::dom::set_style_property(mover, "top", "30px");
        let first = paint_rect(inner);
        assert!(close(first, fresh_rect(inner)), "first shift {first:?}");
        crate::dom::set_style_property(sibling, "width", "70px");
        let _ = paint_rect(sibling);
        crate::dom::set_style_property(mover, "top", "60px");
        let second = paint_rect(inner);
        assert!(close(second, fresh_rect(inner)), "second shift {second:?}");
        assert!(second.y > first.y + 25.0, "{first:?} -> {second:?}");
        let extra = crate::dom::create_element("div");
        crate::dom::set_style_property(extra, "width", "5px");
        crate::dom::set_style_property(extra, "height", "5px");
        crate::dom::append_child(layer, extra);
        let _ = paint_rect(extra);
        crate::dom::set_style_property(mover, "top", "90px");
        let third = paint_rect(inner);
        assert!(close(third, fresh_rect(inner)), "third shift {third:?}");
        assert!(third.y > second.y + 25.0, "{second:?} -> {third:?}");
        if crate::layout_ng::mode() == crate::layout_ng::Mode::Off {
            assert!(paint_shift_count() - shifts >= 2, "inset edits must shift");
        }
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

    fn write_text(id: u32, text: &str) {
        crate::dom::with_document_mut(|document| {
            assert!(
                document
                    .set_inline_text_child(NodeId::from_u32(id), text)
                    .is_some(),
                "inline text"
            );
        });
    }

    fn layout_gen() -> u64 {
        crate::dom::with_document(|document| document.layout_generation())
    }

    fn assert_stored_matches_fresh(id: u32) {
        assert!(flush(), "layout flush");
        let stored = crate::dom::bounding_rect(id);
        let fresh = fresh_rect(id);
        assert!(
            (stored.x - fresh.x).abs() < 0.05
                && (stored.y - fresh.y).abs() < 0.05
                && (stored.width - fresh.width).abs() < 0.05
                && (stored.height - fresh.height).abs() < 0.05,
            "stored ({}, {}, {}, {}) diverges from full layout {fresh:?}",
            stored.x,
            stored.y,
            stored.width,
            stored.height
        );
    }

    #[test]
    fn definite_px_text_keeps_the_border_box() {
        crate::dom::reset_document();
        let node = crate::dom::create_element("div");
        crate::dom::set_style_property(node, "width", "160px");
        crate::dom::set_style_property(node, "height", "40px");
        crate::dom::append_child(crate::dom::body_id(), node);
        write_text(node, "short");
        assert_stored_matches_fresh(node);
        let before = crate::dom::bounding_rect(node).height;
        let generation = layout_gen();
        write_text(
            node,
            "a much longer sentence that would wrap onto extra lines if this box were auto sized",
        );
        assert_eq!(layout_gen(), generation, "definite text must not relayout");
        assert_stored_matches_fresh(node);
        let after = crate::dom::bounding_rect(node).height;
        assert!((after - before).abs() < 0.05, "height {before} -> {after}");
        assert!((after - 40.0).abs() < 1.0, "used height {after}");
    }

    #[test]
    fn auto_height_text_still_grows() {
        crate::dom::reset_document();
        let node = crate::dom::create_element("div");
        crate::dom::set_style_property(node, "width", "80px");
        crate::dom::append_child(crate::dom::body_id(), node);
        write_text(node, "hi");
        assert_stored_matches_fresh(node);
        let before = crate::dom::bounding_rect(node).height;
        let generation = layout_gen();
        write_text(
            node,
            "one two three four five six seven eight nine ten eleven twelve thirteen",
        );
        assert!(layout_gen() > generation, "auto height text must relayout");
        assert_stored_matches_fresh(node);
        let after = crate::dom::bounding_rect(node).height;
        assert!(
            after > before + 8.0,
            "text must grow the box, {before} -> {after}"
        );
    }

    #[test]
    fn clipped_table_cell_text_keeps_the_specified_height() {
        crate::dom::reset_document();
        let table = crate::dom::create_element("table");
        crate::dom::set_style_property(table, "border-collapse", "collapse");
        crate::dom::set_style_property(table, "border-spacing", "0");
        crate::dom::set_style_property(table, "table-layout", "fixed");
        crate::dom::set_style_property(table, "width", "240px");
        let row = crate::dom::create_element("tr");
        crate::dom::set_style_property(row, "height", "22px");
        let cell = crate::dom::create_element("td");
        for (name, value) in [
            ("width", "120px"),
            ("height", "22px"),
            ("padding", "0"),
            ("border", "0"),
            ("overflow", "hidden"),
            ("white-space", "nowrap"),
        ] {
            crate::dom::set_style_property(cell, name, value);
        }
        crate::dom::append_child(row, cell);
        crate::dom::append_child(table, row);
        crate::dom::append_child(crate::dom::body_id(), table);
        write_text(cell, "c 0-0");
        assert_stored_matches_fresh(cell);
        let generation = layout_gen();
        write_text(cell, "v 3-143 a much longer cell label");
        assert_eq!(
            layout_gen(),
            generation,
            "clipped cell text must not relayout"
        );
        assert_stored_matches_fresh(cell);
        let height = crate::dom::bounding_rect(cell).height;
        assert!((height - 22.0).abs() < 1.0, "cell height {height}");
        assert_stored_matches_fresh(table);
    }

    #[test]
    fn second_unchanged_flush_keeps_the_same_height() {
        crate::dom::reset_document();
        let node = crate::dom::create_element("div");
        crate::dom::set_style_property(node, "width", "100px");
        crate::dom::set_style_property(node, "height", "28px");
        crate::dom::append_child(crate::dom::body_id(), node);
        write_text(node, "stable");
        assert_stored_matches_fresh(node);
        let first = crate::dom::bounding_rect(node);
        let generation = layout_gen();
        assert_stored_matches_fresh(node);
        let second = crate::dom::bounding_rect(node);
        assert_eq!(layout_gen(), generation);
        assert!(
            (first.x - second.x).abs() < 0.05
                && (first.y - second.y).abs() < 0.05
                && (first.width - second.width).abs() < 0.05
                && (first.height - second.height).abs() < 0.05,
            "second flush moved the box"
        );
    }

    #[test]
    fn height_auto_after_repaint_only_text_matches_full_layout() {
        crate::dom::reset_document();
        let node = crate::dom::create_element("div");
        crate::dom::set_style_property(node, "width", "80px");
        crate::dom::set_style_property(node, "height", "40px");
        crate::dom::append_child(crate::dom::body_id(), node);
        write_text(node, "a");
        assert_stored_matches_fresh(node);
        write_text(
            node,
            "one two three four five six seven eight nine ten eleven twelve thirteen fourteen",
        );
        let clipped = layout_gen();
        crate::dom::set_style_property(node, "height", "auto");
        assert_eq!(
            layout_gen(),
            clipped,
            "the style edit is applied by the next flush"
        );
        assert_stored_matches_fresh(node);
        let height = crate::dom::bounding_rect(node).height;
        assert!(
            height > 40.0,
            "the new text must wrap once height is auto, got {height}"
        );
    }

    fn component_background(component: &Component, id: u32) -> Option<w3cos_std::color::Color> {
        if host_id(component) == Some(id) {
            return Some(component.style.background);
        }
        component
            .children
            .iter()
            .find_map(|child| component_background(child, id))
    }

    #[test]
    fn background_edit_patches_without_rebuilding_layout() {
        crate::dom::reset_document();
        let parent = crate::dom::create_element("div");
        crate::dom::set_style_property(parent, "display", "flex");
        let painted = crate::dom::create_element("div");
        crate::dom::set_style_property(painted, "width", "40px");
        crate::dom::set_style_property(painted, "height", "20px");
        crate::dom::set_style_property(painted, "background", "#112233");
        let sibling = crate::dom::create_element("div");
        crate::dom::set_style_property(sibling, "width", "40px");
        crate::dom::set_style_property(sibling, "height", "20px");
        crate::dom::append_child(parent, painted);
        crate::dom::append_child(parent, sibling);
        crate::dom::append_child(crate::dom::body_id(), parent);

        let (rebuilds, _) = paint_layout_stats();
        let sibling_before = paint_rect(sibling);
        let (rebuilds_after, patches_after) = paint_layout_stats();
        assert_eq!(rebuilds_after - rebuilds, 1, "first frame builds the tree");

        crate::dom::set_style_property(painted, "background", "#abcdef");
        let sibling_after = paint_rect(sibling);
        let (rebuilds_bg, patches_bg) = paint_layout_stats();
        assert_eq!(rebuilds_bg, rebuilds_after, "background must not rebuild");
        assert_eq!(patches_bg - patches_after, 1, "background patches the tree");
        assert!(
            (sibling_before.x - sibling_after.x).abs() < 0.05
                && (sibling_before.y - sibling_after.y).abs() < 0.05
                && (sibling_before.width - sibling_after.width).abs() < 0.05
                && (sibling_before.height - sibling_after.height).abs() < 0.05,
            "background moved the sibling {sibling_before:?} -> {sibling_after:?}"
        );
        let expected = crate::dom::with_document(|document| {
            document
                .computed_style_for(NodeId::from_u32(painted))
                .background
        });
        let stored = with_paint_layout(1024.0, 768.0, |component, _| {
            Ok(component_background(component, painted).expect("painted node"))
        })
        .expect("paint");
        assert_eq!(stored, expected);
        assert_ne!(expected, w3cos_std::color::Color::BLACK);
    }

    #[test]
    fn width_edit_still_relayouts() {
        crate::dom::reset_document();
        let node = crate::dom::create_element("div");
        crate::dom::set_style_property(node, "width", "40px");
        crate::dom::set_style_property(node, "height", "20px");
        crate::dom::append_child(crate::dom::body_id(), node);
        let before = paint_rect(node);
        let (rebuilds, _) = paint_layout_stats();
        crate::dom::set_style_property(node, "width", "120px");
        let after = paint_rect(node);
        let fresh = fresh_rect(node);
        let (rebuilds_after, _) = paint_layout_stats();
        if crate::layout_ng::mode() == crate::layout_ng::Mode::Off {
            assert_eq!(rebuilds_after, rebuilds, "width stays on the retained tree");
        }
        assert!(
            after.width > before.width + 40.0,
            "width edit must move the used box {before:?} -> {after:?}"
        );
        assert!(
            (after.x - fresh.x).abs() < 0.05
                && (after.y - fresh.y).abs() < 0.05
                && (after.width - fresh.width).abs() < 0.05
                && (after.height - fresh.height).abs() < 0.05,
            "width edit {after:?} diverges from full layout {fresh:?}"
        );
    }

    #[test]
    fn inherited_color_updates_child_without_own_color() {
        crate::dom::reset_document();
        let parent = crate::dom::create_element("div");
        crate::dom::set_style_property(parent, "color", "red");
        let child = crate::dom::create_element("span");
        crate::dom::append_child(parent, child);
        crate::dom::append_child(crate::dom::body_id(), parent);
        let red = crate::dom::with_document(|document| {
            document.computed_style_for(NodeId::from_u32(child)).color
        });
        crate::dom::set_style_property(parent, "color", "blue");
        let blue = crate::dom::with_document(|document| {
            document.computed_style_for(NodeId::from_u32(child)).color
        });
        let parent_blue = crate::dom::with_document(|document| {
            document.computed_style_for(NodeId::from_u32(parent)).color
        });
        assert_ne!(red, blue, "the child had no color of its own");
        assert_eq!(blue, parent_blue);
    }

    #[test]
    fn local_width_keeps_sibling_class_color() {
        crate::dom::reset_document();
        w3cos_dom::stylesheet::clear_rules();
        w3cos_dom::stylesheet::register_rule(".alt", &[("color", "green")]);
        let parent = crate::dom::create_element("div");
        let edited = crate::dom::create_element("div");
        let sibling = crate::dom::create_element("div");
        crate::dom::set_attribute(edited, "class", "alt");
        crate::dom::set_attribute(sibling, "class", "alt");
        crate::dom::append_child(parent, edited);
        crate::dom::append_child(parent, sibling);
        crate::dom::append_child(crate::dom::body_id(), parent);
        let before = crate::dom::with_document(|document| {
            document.computed_style_for(NodeId::from_u32(sibling)).color
        });
        crate::dom::set_style_property(edited, "width", "80px");
        let after = crate::dom::with_document(|document| {
            document.computed_style_for(NodeId::from_u32(sibling)).color
        });
        let edited_width = crate::dom::with_document(|document| {
            document.computed_style_for(NodeId::from_u32(edited)).width
        });
        w3cos_dom::stylesheet::clear_rules();
        assert_eq!(before, after);
        assert_eq!(
            after,
            w3cos_std::color::Color::from_css("green").expect("green")
        );
        assert_eq!(edited_width, w3cos_std::style::Dimension::Px(80.0));
    }

    #[test]
    fn same_class_different_ids_share_style_but_dir_does_not() {
        crate::dom::reset_document();
        w3cos_dom::stylesheet::clear_rules();
        w3cos_dom::stylesheet::register_rule(".cell", &[("color", "purple"), ("width", "30px")]);
        let parent = crate::dom::create_element("div");
        let first = crate::dom::create_element("div");
        let second = crate::dom::create_element("div");
        let directed = crate::dom::create_element("div");
        for (node, id) in [(first, "a"), (second, "b"), (directed, "c")] {
            crate::dom::set_attribute(node, "class", "cell");
            crate::dom::set_attribute(node, "id", id);
            crate::dom::append_child(parent, node);
        }
        crate::dom::set_attribute(directed, "dir", "rtl");
        crate::dom::append_child(crate::dom::body_id(), parent);
        let (first_style, second_style, directed_style) = crate::dom::with_document(|document| {
            (
                document.computed_style_for(NodeId::from_u32(first)),
                document.computed_style_for(NodeId::from_u32(second)),
                document.computed_style_for(NodeId::from_u32(directed)),
            )
        });
        w3cos_dom::stylesheet::clear_rules();
        assert_eq!(first_style.color, second_style.color);
        assert_eq!(first_style.width, second_style.width);
        assert_eq!(first_style.direction, second_style.direction);
        assert_eq!(
            first_style.color,
            w3cos_std::color::Color::from_css("purple").expect("purple")
        );
        assert_ne!(directed_style.direction, first_style.direction);
    }

    #[test]
    fn hiding_a_column_after_a_warm_rebuild_matches_full_layout() {
        // WPT css-tables/width-distribution/computing-column-measure-2.html
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                WARM_REBUILD_OVERRIDE.with(|o| o.set(None));
            }
        }
        let _reset = Reset;
        // The warm rebuild is a LayoutNG path. (The legacy engine's patch
        // flush keeps the old x after the wrapper's padding-left edit below,
        // 9 vs 10; that is outside this change.)
        if crate::layout_ng::mode() != crate::layout_ng::Mode::On {
            return;
        }
        for warm in [false, true] {
            WARM_REBUILD_OVERRIDE.with(|o| o.set(Some(warm)));
            crate::dom::reset_document();
            invalidate();
            let wrapper = crate::dom::create_element("div");
            crate::dom::set_style_property(wrapper, "width", "750px");
            let heading = crate::dom::create_element("h1");
            write_text(heading, "Width Distribution");
            crate::dom::append_child(wrapper, heading);
            let mut tables = Vec::new();
            let mut cols = Vec::new();
            for widths in [
                [("100px", false), ("200px", false), ("300px", false)],
                [("100px", true), ("200px", false), ("auto", false)],
                [("100px", false), ("200px", false), ("auto", false)],
            ] {
                let table = crate::dom::create_element("table");
                crate::dom::set_style_property(table, "border", "1px solid orange");
                let group = crate::dom::create_element("colgroup");
                let mut these = Vec::new();
                for (width, hidden) in widths {
                    let col = crate::dom::create_element("col");
                    crate::dom::set_style_property(col, "width", width);
                    if hidden {
                        crate::dom::set_style_property(col, "display", "none");
                    }
                    crate::dom::append_child(group, col);
                    these.push(col);
                }
                crate::dom::append_child(table, group);
                let row = crate::dom::create_element("tr");
                for label in ["100", "200", "300"] {
                    let cell = crate::dom::create_element("td");
                    crate::dom::set_style_property(cell, "padding", "0");
                    let text = crate::dom::create_text_node(label);
                    crate::dom::append_child(cell, text);
                    crate::dom::append_child(row, cell);
                }
                crate::dom::append_child(table, row);
                crate::dom::append_child(wrapper, table);
                tables.push(table);
                cols.push(these);
            }
            crate::dom::append_child(crate::dom::body_id(), wrapper);
            // No oracle layout before the edit: it would replace the
            // per-box data the rebuild left behind.
            assert!(flush(), "layout flush");
            // A patch flush first, so per-box data sits under the key.
            crate::dom::set_style_property(wrapper, "padding-left", "1px");
            assert!(flush(), "layout flush");
            let reference = crate::dom::bounding_rect(tables[2]).width;
            assert!(crate::dom::bounding_rect(tables[0]).width > reference);
            // One table loses a column and the other gains one, so the new
            // tree has as many boxes as the old one.
            crate::dom::set_style_property(cols[1][0], "display", "table-column");
            crate::dom::set_style_property(cols[0][2], "display", "none");
            assert!(flush(), "layout flush");
            for table in &tables[..2] {
                let width = crate::dom::bounding_rect(*table).width;
                assert!(
                    (width - reference).abs() < 0.05,
                    "warm {warm}: table width {width}, reference {reference}"
                );
            }
            // And a patch flush over the rebuilt tree.
            crate::dom::set_style_property(wrapper, "padding-left", "2px");
            assert!(flush(), "layout flush");
            for table in &tables[..2] {
                let width = crate::dom::bounding_rect(*table).width;
                assert!(
                    (width - reference).abs() < 0.05,
                    "warm {warm}: after a patch, table width {width}, reference {reference}"
                );
            }
            assert_stored_matches_fresh(tables[0]);
            assert_stored_matches_fresh(tables[1]);
        }
    }
}

#[cfg(test)]
mod retained_tree_tests {
    use super::RetainedTree;
    use w3cos_std::Component;
    use w3cos_std::style::{Dimension, Style};

    type Entry = (
        usize,
        u64,
        usize,
        usize,
        usize,
        Option<usize>,
        Option<usize>,
        usize,
    );

    fn entries(flat: &[crate::layout::FlatNodeInfo<'_>]) -> Vec<Entry> {
        flat.iter()
            .map(|info| {
                (
                    info.component.map_or(0, |c| c as *const Component as usize),
                    info.stable_id,
                    info.kind as *const _ as usize,
                    info.style as *const _ as usize,
                    info.on_click as *const _ as usize,
                    info.sticky_counter_signal,
                    info.parent,
                    info.subtree_end,
                )
            })
            .collect()
    }

    fn fresh(tree: &RetainedTree, key: (u64, u64)) -> Vec<Entry> {
        entries(&crate::layout::pre_flatten_keyed(tree, Some(key)))
    }

    fn sized(width: f32) -> Style {
        Style {
            width: Dimension::Px(width),
            ..Default::default()
        }
    }

    #[test]
    fn retained_flat_view_follows_every_edit() {
        let key = (7, 9);
        let mut tree = RetainedTree::new(Component::column(
            Style::default(),
            vec![
                Component::row(
                    Style::default(),
                    vec![
                        Component::text("a", Style::default()),
                        Component::text("b", Style::default()),
                    ],
                ),
                Component::column(
                    Style::default(),
                    vec![Component::text("c", Style::default())],
                ),
            ],
        ));
        let kept = tree.flat(key).1.as_ptr() as usize;
        assert_eq!(entries(tree.flat(key).1), fresh(&tree, key));
        assert_eq!(tree.flat(key).1.as_ptr() as usize, kept);

        // An in-place restyle keeps the view and rebuilds the path entries.
        assert!(tree.edit_at(&[1, 0], |node| node.style = sized(5.0).into()));
        let (_, flat) = tree.flat(key);
        assert_eq!(flat.as_ptr() as usize, kept);
        assert_eq!(flat[5].style.width, Dimension::Px(5.0));
        assert_eq!(entries(flat), fresh(&tree, key));
        assert!(tree.edit_at(&[], |node| node.style = sized(9.0).into()));
        assert_eq!(entries(tree.flat(key).1), fresh(&tree, key));

        // A path the tree does not have edits nothing.
        assert!(!tree.edit_at(&[0, 2], |node| node.style = sized(1.0).into()));
        assert_eq!(entries(tree.flat(key).1), fresh(&tree, key));

        // An edit of the children, by either route, flattens again.
        assert!(tree.edit_at(&[0], |node| {
            node.children.push(Component::text("d", Style::default()))
        }));
        assert_eq!(tree.flat(key).1.len(), 7);
        assert_eq!(entries(tree.flat(key).1), fresh(&tree, key));
        tree.children[1].children.clear();
        assert_eq!(tree.flat(key).1.len(), 6);
        assert_eq!(entries(tree.flat(key).1), fresh(&tree, key));

        // A new structure key flattens again.
        assert_eq!(entries(tree.flat((7, 10)).1), fresh(&tree, (7, 10)));
    }
}
