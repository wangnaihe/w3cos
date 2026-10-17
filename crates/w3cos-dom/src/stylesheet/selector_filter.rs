use super::{
    AttributeSelector, Combinator, CompoundSelector, Document, NodeId, NodeType, SelectorContext,
};

/// Build the ancestor filter only if a surviving candidate needs ancestor
/// features. Most simple selectors have an empty requirement, so eagerly
/// walking and hashing the entire ancestor chain adds work without filtering
/// anything. The cache is local to one matching operation, never a DOM snapshot.
pub(super) struct LazyAncestorBloom<F> {
    build: F,
    bloom: std::cell::Cell<Option<AncestorBloom>>,
    known_empty: bool,
}

impl<F: Fn() -> AncestorBloom> LazyAncestorBloom<F> {
    /// `known_empty` may only be true when `build` would return the empty
    /// filter, such as an empty ancestor slice. Builders must not reenter this
    /// matching operation; the current builders only read ancestor features.
    pub(super) fn new(build: F, known_empty: bool) -> Self {
        Self {
            build,
            bloom: std::cell::Cell::new(None),
            known_empty,
        }
    }

    pub(super) fn might_contain(&self, required: AncestorBloom) -> bool {
        if required.0 == 0 {
            return true;
        }
        if self.known_empty {
            return false;
        }
        let available = if let Some(available) = self.bloom.get() {
            available
        } else {
            let available = (self.build)();
            self.bloom.set(Some(available));
            available
        };
        available.might_contain(required)
    }
}

std::thread_local! {
    static RECALC_BLOOMS: std::cell::RefCell<Option<RecalcBlooms>> =
        const { std::cell::RefCell::new(None) };
}

/// Per-pass memo of "ancestor features seen by this node's children" —
/// Blink's SelectorFilter, which pushes each parent while RecalcStyle walks
/// the tree so an element's ancestor bloom is O(1) instead of a walk to the
/// root per element.
struct RecalcBlooms {
    document: *const Document,
    children: Vec<Option<AncestorBloom>>,
}

impl RecalcBlooms {
    fn children_of(&mut self, document: &Document, parent: NodeId) -> AncestorBloom {
        let mut pending = Vec::new();
        let mut current = Some(parent);
        let mut base = AncestorBloom::default();
        while let Some(id) = current {
            if let Some(Some(bloom)) = self.children.get(id.0 as usize) {
                base = *bloom;
                break;
            }
            pending.push(id);
            current = document.get_node(id).parent;
        }
        while let Some(id) = pending.pop() {
            base.insert_node_features(document, id);
            let index = id.0 as usize;
            if self.children.len() <= index {
                self.children.resize(index + 1, None);
            }
            self.children[index] = Some(base);
        }
        base
    }
}

/// While alive, ancestor blooms are memoized per parent for `document`. It
/// borrows the document, so the tree cannot change during the pass.
pub struct StyleRecalcScope<'a> {
    owner: bool,
    _document: std::marker::PhantomData<&'a Document>,
}

impl<'a> StyleRecalcScope<'a> {
    pub fn enter(document: &'a Document) -> Self {
        let owner = RECALC_BLOOMS.with(|slot| {
            let Ok(mut slot) = slot.try_borrow_mut() else {
                return false;
            };
            if slot.is_some() {
                return false;
            }
            *slot = Some(RecalcBlooms {
                document: document as *const Document,
                children: Vec::new(),
            });
            true
        });
        Self {
            owner,
            _document: std::marker::PhantomData,
        }
    }
}

impl Drop for StyleRecalcScope<'_> {
    fn drop(&mut self) {
        if self.owner {
            RECALC_BLOOMS.with(|slot| {
                if let Ok(mut slot) = slot.try_borrow_mut() {
                    *slot = None;
                }
            });
        }
    }
}

/// Compact, no-false-negative prefilter for ancestor-dependent selectors.
/// Hash collisions only retain extra rules; the full selector matcher remains
/// authoritative for every rule that passes.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct AncestorBloom(u128);

impl AncestorBloom {
    pub(super) fn for_rule(chain: &[CompoundSelector], combinators: &[Combinator]) -> Self {
        let mut bloom = Self::default();
        let mut index = chain.len().saturating_sub(1);
        while index > 0 {
            match combinators[index - 1] {
                Combinator::Child | Combinator::Descendant => {
                    bloom.insert_compound_requirement(&chain[index - 1]);
                    index -= 1;
                }
                // Compounds beyond a sibling edge are not necessarily on the
                // target's ancestor chain, so filtering on them could reject
                // a valid selector.
                Combinator::AdjacentSibling | Combinator::GeneralSibling => break,
            }
        }
        bloom
    }

    pub(super) fn for_contexts(ancestors: &[SelectorContext]) -> Self {
        let mut bloom = Self::default();
        for ancestor in ancestors {
            bloom.insert_context(ancestor);
        }
        bloom
    }

    pub(super) fn for_node(document: &Document, node: NodeId) -> Self {
        let parent = document.get_node(node).parent;
        let Some(parent) = parent else {
            return Self::default();
        };
        if let Some(bloom) = RECALC_BLOOMS.with(|slot| {
            let mut slot = slot.try_borrow_mut().ok()?;
            let blooms = slot.as_mut()?;
            std::ptr::eq(blooms.document, document).then(|| blooms.children_of(document, parent))
        }) {
            return bloom;
        }
        let mut bloom = Self::default();
        let mut ancestor = Some(parent);
        while let Some(candidate) = ancestor {
            bloom.insert_node_features(document, candidate);
            ancestor = document.get_node(candidate).parent;
        }
        bloom
    }

    fn insert_node_features(&mut self, document: &Document, id: NodeId) {
        let node = document.get_node(id);
        if node.node_type != NodeType::Element {
            return;
        }
        let element_id = node
            .attributes
            .iter()
            .find_map(|(name, value)| (name.as_str() == "id").then(|| value.as_str()));
        if let Some(element_id) = element_id {
            self.insert(FEATURE_ID, &element_id, false);
        }
        for class in &node.class_list {
            self.insert(FEATURE_CLASS, &class.as_str(), false);
        }
        for (name, _) in &node.attributes {
            self.insert(
                FEATURE_ATTRIBUTE,
                attribute_local_name(&name.as_str()),
                true,
            );
        }
        self.insert_tag(&node.tag.as_str());
    }

    pub(super) fn might_contain(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    fn insert_context(&mut self, context: &SelectorContext) {
        if let Some(id) = &context.id {
            self.insert(FEATURE_ID, id, false);
        }
        for class in &context.classes {
            self.insert(FEATURE_CLASS, class, false);
        }
        for (name, _) in &context.attributes {
            self.insert(FEATURE_ATTRIBUTE, attribute_local_name(name), true);
        }
        self.insert_tag(&context.tag);
    }

    fn insert_compound_requirement(&mut self, compound: &CompoundSelector) {
        if compound.unsupported {
            return;
        }
        if let Some(id) = &compound.id {
            self.insert(FEATURE_ID, id, false);
        } else if let Some(class) = compound.classes.first() {
            self.insert(FEATURE_CLASS, class, false);
        } else if let Some(attribute) = compound.attributes.first() {
            self.insert(
                FEATURE_ATTRIBUTE,
                attribute_local_name(attribute_name(attribute)),
                true,
            );
        } else if let Some(tag) = &compound.tag {
            let tag = if compound.any_namespace {
                tag_local_name(tag)
            } else {
                tag
            };
            self.insert(FEATURE_TAG, tag, true);
        }
    }

    fn insert_tag(&mut self, tag: &str) {
        self.insert(FEATURE_TAG, tag, true);
        if let Some((_, local_name)) = tag.rsplit_once(['|', ':']) {
            if !local_name.is_empty() {
                self.insert(FEATURE_TAG, local_name, true);
            }
        }
    }

    fn insert(&mut self, kind: u8, value: &str, lowercase_ascii: bool) {
        let mut hash = 0xcbf29ce484222325u64 ^ u64::from(kind);
        for mut byte in value.bytes() {
            if lowercase_ascii {
                byte = byte.to_ascii_lowercase();
            }
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        let mixed = hash ^ hash.rotate_left(29).wrapping_mul(0x9e3779b185ebca87);
        self.0 |= 1u128 << (hash & 127);
        self.0 |= 1u128 << (mixed & 127);
    }
}

const FEATURE_ID: u8 = 1;
const FEATURE_CLASS: u8 = 2;
const FEATURE_ATTRIBUTE: u8 = 3;
const FEATURE_TAG: u8 = 4;

fn attribute_name(attribute: &AttributeSelector) -> &str {
    match attribute {
        AttributeSelector::Present(name)
        | AttributeSelector::Equals(name, _, _)
        | AttributeSelector::Includes(name, _, _)
        | AttributeSelector::DashMatch(name, _, _)
        | AttributeSelector::Prefix(name, _, _)
        | AttributeSelector::Suffix(name, _, _)
        | AttributeSelector::Substring(name, _, _) => name,
    }
}

fn attribute_local_name(name: &str) -> &str {
    name.rsplit_once(['|', ':'])
        .map_or(name, |(_, local_name)| local_name)
}

fn tag_local_name(tag: &str) -> &str {
    tag.rsplit_once(['|', ':'])
        .map_or(tag, |(_, local_name)| local_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn lazy_filter_skips_empty_requirements_and_builds_only_once() {
        let builds = Cell::new(0);
        let lazy = LazyAncestorBloom::new(
            || {
                builds.set(builds.get() + 1);
                AncestorBloom(0b101)
            },
            false,
        );
        assert!(lazy.might_contain(AncestorBloom::default()));
        assert_eq!(builds.get(), 0);
        assert!(lazy.might_contain(AncestorBloom(0b001)));
        assert!(!lazy.might_contain(AncestorBloom(0b010)));
        assert!(lazy.might_contain(AncestorBloom(0b101)));
        assert_eq!(builds.get(), 1);
    }

    #[test]
    fn lazy_filter_agrees_with_eager_filter_for_every_small_bit_pattern() {
        for available in 0..256 {
            let eager = AncestorBloom(available);
            let lazy = LazyAncestorBloom::new(|| eager, false);
            for required in 0..256 {
                let required = AncestorBloom(required);
                assert_eq!(lazy.might_contain(required), eager.might_contain(required));
            }
        }
    }

    #[test]
    fn known_empty_filter_never_builds() {
        let lazy =
            LazyAncestorBloom::new(|| panic!("empty ancestor chain needs no traversal"), true);
        assert!(lazy.might_contain(AncestorBloom::default()));
        assert!(!lazy.might_contain(AncestorBloom(1)));
    }
}
