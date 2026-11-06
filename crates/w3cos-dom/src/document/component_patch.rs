//! Incremental component-tree patching.
//!
//! `to_component_tree` lowers the whole DOM every time a layout read finds the
//! structure changed. For a small edit (a keystroke, a palette row) almost all
//! of that work reproduces components that did not change. The functions here
//! record what changed since a baseline and re-lower only the smallest safe
//! subtrees ("frontiers"), splicing them into the previous component tree.
//!
//! The patch is deliberately conservative. Whenever a precondition cannot be
//! proven it reports [`ComponentPatch::NeedsRebuild`] and the caller falls
//! back to the full lowering, which stays the single source of truth.

use std::collections::{HashMap, HashSet};

use w3cos_std::style::{Display, TextTransform};
use w3cos_std::{Component, ComponentKind, EventAction};

use super::{Document, NodeId, NodeType};

/// Snapshot of every change counter the patch consults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentBaseline {
    mutation_clock: u64,
    style_clock: u64,
    global_seq: u64,
    stylesheet_generation: u64,
    listener_changes: u64,
    node_slots: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentPatch {
    /// Nothing observable changed; the tree is already current.
    Unchanged,
    /// The tree was updated by re-lowering `frontiers` subtrees.
    Patched { frontiers: usize },
    /// The change cannot be patched safely; lower the whole tree instead.
    NeedsRebuild,
}

/// Above this share of live node slots a patch is not cheaper than a rebuild.
const MAX_DIRTY_FRACTION_DENOM: usize = 4;

fn host_id(component: &Component) -> Option<u32> {
    match component.on_click {
        EventAction::NativeHost { id, .. } => u32::try_from(id).ok(),
        _ => None,
    }
}

fn block_level(display: Display) -> bool {
    matches!(
        display,
        Display::Block | Display::Flex | Display::Grid | Display::FlowRoot | Display::None
    )
}

/// What the children of a visited component need to know about it.
struct ParentCtx {
    host: Option<u32>,
    /// The parent is a block/flex/grid container whose children are all
    /// block-level, so inline-run wrapping, whitespace collapsing and list
    /// numbering cannot couple one child to another.
    isolated_children: bool,
    /// The lowered parent still carries exactly its computed style, so a
    /// child lowered on its own inherits the same values it did in the tree.
    style_is_computed: bool,
}

struct PatchCx<'a> {
    finish: &'a mut dyn FnMut(&mut Component),
    doc: &'a Document,
    ancestors: HashSet<u32>,
    anchors: HashSet<u32>,
    host_counts: HashMap<u32, u32>,
    frontiers: usize,
}

enum Visit {
    Done,
    NeedParent,
}

impl Document {
    #[inline]
    pub(super) fn note_mutation(&mut self, id: NodeId) {
        self.mutation_clock += 1;
        let index = id.0 as usize;
        if index >= self.mutation_seq.len() {
            self.mutation_seq.resize(index + 1, 0);
        }
        self.mutation_seq[index] = self.mutation_clock;
    }

    /// Counters describing the document as the component tree being built now
    /// will see it. Take this right before lowering.
    pub fn component_baseline(&self) -> ComponentBaseline {
        ComponentBaseline {
            mutation_clock: self.mutation_clock,
            style_clock: self.style_revision_clock,
            global_seq: self.global_seq,
            stylesheet_generation: crate::stylesheet::generation(),
            listener_changes: self.events.change_count(),
            node_slots: self.nodes.len(),
        }
    }

    fn live_node(&self, id: NodeId) -> Option<&crate::node::DomNode> {
        self.nodes.get(id.0 as usize).and_then(Option::as_ref)
    }

    /// Nodes touched since `base`, or `None` when the history is unusable.
    fn nodes_changed_since(&self, base: &ComponentBaseline) -> Option<Vec<NodeId>> {
        if base.global_seq != self.global_seq
            || base.stylesheet_generation != crate::stylesheet::generation()
            || base.node_slots > self.nodes.len()
        {
            return None;
        }
        let mut changed: Vec<NodeId> = Vec::new();
        for (index, &seq) in self.mutation_seq.iter().enumerate() {
            if seq > base.mutation_clock {
                changed.push(NodeId(index as u32));
            }
        }
        for (index, &revision) in self.style_revisions.iter().enumerate() {
            if revision != 0 && revision > base.style_clock {
                changed.push(NodeId(index as u32));
            }
        }
        changed.extend(self.events.changes_since(base.listener_changes)?);
        changed.sort_unstable_by_key(|n| n.0);
        changed.dedup();
        Some(changed)
    }

    /// Bring `tree` (lowered when `base` was taken) up to date by re-lowering
    /// only the subtrees that changed.
    pub fn patch_component_tree(
        &self,
        tree: &mut Component,
        base: &ComponentBaseline,
    ) -> ComponentPatch {
        self.patch_component_tree_with(tree, base, &mut |_| {})
    }

    /// Like [`Self::patch_component_tree`], running `finish` on every freshly
    /// lowered subtree. Hosts use it for the same post-lowering passes they
    /// apply to a full tree (grafting external subtrees, font-dependent
    /// leading), so a patched subtree is indistinguishable from a rebuilt one.
    pub fn patch_component_tree_with(
        &self,
        tree: &mut Component,
        base: &ComponentBaseline,
        finish: &mut dyn FnMut(&mut Component),
    ) -> ComponentPatch {
        let Some(changed) = self.nodes_changed_since(base) else {
            return ComponentPatch::NeedsRebuild;
        };
        if changed.is_empty() {
            return ComponentPatch::Unchanged;
        }
        if changed.len() > (self.nodes.len() / MAX_DIRTY_FRACTION_DENOM).max(8) {
            return ComponentPatch::NeedsRebuild;
        }

        // An element is re-lowered itself (its own style, attributes and child
        // list changed). Character data and other non-elements are lowered
        // through their parent (text fusion, collapsing), so the parent is the
        // start for those.
        let mut starts: Vec<NodeId> = Vec::new();
        let mut ancestors: HashSet<u32> = HashSet::new();
        let mut connected: HashSet<u32> = HashSet::new();
        for id in changed {
            let Some(node) = self.live_node(id) else {
                continue;
            };
            if id == NodeId::ROOT {
                return ComponentPatch::NeedsRebuild;
            }
            let start = if node.node_type == NodeType::Element {
                id
            } else {
                match node.parent {
                    Some(parent) => parent,
                    None => continue,
                }
            };
            let parent = start;
            if !self.climb_to_root(parent, &mut connected, &mut ancestors) {
                continue; // detached: not part of the lowered tree
            }
            starts.push(parent);
        }
        if starts.is_empty() {
            return ComponentPatch::Unchanged;
        }

        let mut cx = PatchCx {
            doc: self,
            ancestors,
            anchors: HashSet::new(),
            host_counts: HashMap::new(),
            frontiers: 0,
            finish,
        };
        cx.count_hosts(tree);
        for start in starts {
            let mut current = Some(start);
            let mut anchor = None;
            while let Some(id) = current {
                if cx.host_counts.contains_key(&id.0) {
                    anchor = Some(id.0);
                    break;
                }
                current = self.live_node(id).and_then(|node| node.parent);
            }
            let Some(anchor) = anchor else {
                return ComponentPatch::NeedsRebuild;
            };
            cx.anchors.insert(anchor);
        }

        match cx.visit(tree, None) {
            Visit::Done => ComponentPatch::Patched {
                frontiers: cx.frontiers,
            },
            Visit::NeedParent => ComponentPatch::NeedsRebuild,
        }
    }

    /// Walk from `start` to the document root, recording the chain in
    /// `ancestors`. Returns whether `start` is attached to the document.
    fn climb_to_root(
        &self,
        start: NodeId,
        connected: &mut HashSet<u32>,
        ancestors: &mut HashSet<u32>,
    ) -> bool {
        let mut path: Vec<u32> = Vec::new();
        let mut current = Some(start);
        let mut attached = false;
        while let Some(id) = current {
            if connected.contains(&id.0) {
                attached = true;
                break;
            }
            let Some(node) = self.live_node(id) else {
                break;
            };
            path.push(id.0);
            if node.parent.is_none() {
                attached = id == NodeId::ROOT;
                break;
            }
            current = node.parent;
        }
        if attached {
            for id in path {
                connected.insert(id);
                ancestors.insert(id);
            }
        }
        attached
    }
}

impl PatchCx<'_> {
    /// Count lowered components per host node, descending only through the
    /// chains that lead to a changed node.
    fn count_hosts(&mut self, component: &Component) {
        if let Some(host) = host_id(component) {
            if !self.ancestors.contains(&host) {
                return;
            }
            *self.host_counts.entry(host).or_insert(0) += 1;
        }
        for child in &component.children {
            self.count_hosts(child);
        }
    }

    fn parent_ctx(&self, component: &Component) -> ParentCtx {
        let host = host_id(component);
        // Flex and grid items are independent of each other: whitespace and
        // anonymous wrappers do not couple one item to the next the way they
        // do in ordinary block flow. A mixed flex/grid parent is therefore
        // safe to patch under. Block and flow-root parents still require
        // every child to be block-level so inline runs stay intact.
        let isolated_children = host.is_some()
            && match component.style.display {
                Display::Flex | Display::Grid => true,
                Display::Block | Display::FlowRoot => component.children.iter().all(|child| {
                    !matches!(child.kind, ComponentKind::Text { .. })
                        && block_level(child.style.display)
                }),
                _ => false,
            };
        let style_is_computed =
            host.is_some_and(|host| self.doc.computed_style_for(NodeId(host)) == component.style);
        ParentCtx {
            host,
            isolated_children,
            style_is_computed,
        }
    }

    fn visit(&mut self, component: &mut Component, parent: Option<&ParentCtx>) -> Visit {
        let host = host_id(component);
        if let Some(host) = host
            && !self.ancestors.contains(&host)
        {
            return Visit::Done;
        }
        let mut needs_self = host.is_some_and(|host| self.anchors.contains(&host));
        let ctx = self.parent_ctx(component);
        for child in &mut component.children {
            if matches!(self.visit(child, Some(&ctx)), Visit::NeedParent) {
                needs_self = true;
            }
        }
        if !needs_self {
            return Visit::Done;
        }
        match (host, parent) {
            (Some(host), Some(parent)) => self.rebuild(component, host, parent),
            _ => Visit::NeedParent,
        }
    }

    fn rebuild(&mut self, component: &mut Component, host: u32, parent: &ParentCtx) -> Visit {
        let doc = self.doc;
        let id = NodeId(host);
        let Some(node) = doc.live_node(id) else {
            return Visit::NeedParent;
        };
        let Some(dom_parent) = node.parent else {
            return Visit::NeedParent;
        };
        if node.node_type != NodeType::Element
            || self.host_counts.get(&host) != Some(&1)
            || Some(id) == doc.document_element_id()
            || id == doc.body_id
            || dom_parent == doc.body_id
            || parent.host != Some(dom_parent.0)
            || !parent.isolated_children
            || !parent.style_is_computed
            || !matches!(
                component.style.display,
                Display::Block | Display::Flex | Display::Grid
            )
        {
            return Visit::NeedParent;
        }
        // `text-transform: capitalize` continues a word across the previous
        // sibling, so a following sibling may read what is lowered here.
        let mut next = crate::node::sib(node.next_sibling);
        while let Some(sibling) = next {
            let Some(sibling_node) = doc.live_node(sibling) else {
                break;
            };
            if sibling_node.node_type == NodeType::Element {
                if doc.computed_style_for(sibling).text_transform != TextTransform::None {
                    return Visit::NeedParent;
                }
                break;
            }
            next = crate::node::sib(sibling_node.next_sibling);
        }

        let mut fresh = doc.to_component_subtree(id);
        let same_box = fresh.style.display == component.style.display
            && fresh.style.position == component.style.position
            && fresh.style.float == component.style.float
            && host_id(&fresh) == Some(host);
        if !same_box {
            return Visit::NeedParent;
        }
        (self.finish)(&mut fresh);
        *component = fresh;
        self.frontiers += 1;
        Visit::Done
    }
}
