use std::collections::HashMap;

use super::{AttributeSelector, CompoundSelector, Document, NodeId, Rule, SelectorContext};

#[derive(Debug, Default)]
pub(super) struct RuleSet {
    pub(super) rules: Vec<Rule>,
    pub(super) document_roots: HashMap<NodeId, u64>,
    /// Rules without a pseudo-element, bucketed by their subject compound.
    main: Buckets,
    /// `::before`/`::after`/... rules. Kept apart so the per-element cascade
    /// never gathers them, and pseudo lookups never scan the element rules.
    pseudo: Buckets,
    has_sibling_dependencies: bool,
    has_relational_dependencies: bool,
    has_structural_dependencies: bool,
    /// Some selector reads the `style` attribute, so an inline-style write
    /// can change which rules match on that element or its descendants.
    has_style_attribute_dependencies: bool,
    /// A `:has()` argument uses `+` or `~`, so the subject sits before the mutation.
    has_sibling_scoped_relational: bool,
    /// Static part (no relational pseudo) of every compound that carries `:has()`,
    /// and its reach: 0 subject, 1 earlier compound (descendants may change),
    /// 2 earlier compound followed by a sibling combinator.
    relational_anchors: Vec<(CompoundSelector, u8)>,
    /// Compounds whose match can change when an element is inserted into or
    /// removed from the sibling list: those carrying a positional pseudo-class
    /// (`:first-child`, `:nth-child()`, ...) and those to the right of a `+`/`~`.
    positional_anchors: Vec<PositionalAnchor>,
    /// Compounds carrying `:empty`, which watch their own child list.
    empty_anchors: Vec<CompoundSelector>,
    /// Some positional/sibling dependency is not captured by the anchors
    /// (nested selector lists, `:nth-child(.. of S)`), so callers must fall
    /// back to restyling every sibling.
    positional_unbounded: bool,
}

/// Static part (pseudo-classes dropped, which only widens the match) of a
/// compound whose result depends on its position among its siblings.
#[derive(Debug)]
struct PositionalAnchor {
    subject: CompoundSelector,
    /// Static part of the parent compound when joined by `>`; every sibling
    /// shares one parent, so the anchor can only apply when it matches.
    parent: Option<CompoundSelector>,
    /// The compound sits right of `+` or `~`.
    sibling_rhs: bool,
}

#[derive(Debug, Default)]
struct Buckets {
    id_rules: HashMap<String, Vec<usize>>,
    class_rules: HashMap<String, Vec<usize>>,
    attribute_rules: HashMap<String, Vec<usize>>,
    tag_rules: HashMap<String, Vec<usize>>,
    universal_rules: Vec<usize>,
}

impl Buckets {
    fn clear(&mut self) {
        self.id_rules.clear();
        self.class_rules.clear();
        self.attribute_rules.clear();
        self.tag_rules.clear();
        self.universal_rules.clear();
    }

    fn insert(&mut self, bucket: RuleBucket<'_>, index: usize) {
        match bucket {
            RuleBucket::Id(id) => self.id_rules.entry(id.to_string()).or_default().push(index),
            RuleBucket::Class(class) => self
                .class_rules
                .entry(class.to_string())
                .or_default()
                .push(index),
            RuleBucket::Attribute(name) => self
                .attribute_rules
                .entry(attribute_index_key(name))
                .or_default()
                .push(index),
            RuleBucket::Tag(tag) => self
                .tag_rules
                .entry(tag.to_ascii_lowercase())
                .or_default()
                .push(index),
            RuleBucket::Universal => self.universal_rules.push(index),
        }
    }

    /// Candidate rule indices. Order is unspecified (callers sort by
    /// specificity and rule order); each rule lives in exactly one bucket, so
    /// only repeated attribute/class keys can duplicate an index.
    fn candidates_for_node(&self, document: &Document, node: NodeId) -> Vec<usize> {
        let node = document.get_node(node);
        let mut candidates = Vec::with_capacity(self.universal_rules.len() + 16);
        candidates.extend_from_slice(&self.universal_rules);
        let start = candidates.len();
        if let Some((_, id)) = node
            .attributes
            .iter()
            .find(|(name, _)| name.as_str() == "id")
            && let Some(indices) = self.id_rules.get(id.as_str())
        {
            candidates.extend(indices);
        }
        for class in &node.class_list {
            let class = class.as_str();
            if let Some(indices) = self.class_rules.get(class.as_str()) {
                candidates.extend(indices);
            }
        }
        for (name, _) in &node.attributes {
            let name = name.as_str();
            if let Some(indices) = self
                .attribute_rules
                .get(&attribute_index_key(name.as_str()))
            {
                candidates.extend(indices);
            }
        }
        append_tag_candidates(&self.tag_rules, &node.tag.as_str(), &mut candidates);
        normalize_tail(&mut candidates, start);
        candidates
    }

    fn candidates_for_context(&self, ctx: &SelectorContext) -> Vec<usize> {
        let mut candidates = Vec::with_capacity(self.universal_rules.len() + 16);
        candidates.extend_from_slice(&self.universal_rules);
        let start = candidates.len();
        if let Some(id) = &ctx.id
            && let Some(indices) = self.id_rules.get(id)
        {
            candidates.extend(indices);
        }
        for class in &ctx.classes {
            if let Some(indices) = self.class_rules.get(class) {
                candidates.extend(indices);
            }
        }
        for (name, _) in &ctx.attributes {
            if let Some(indices) = self.attribute_rules.get(&attribute_index_key(name)) {
                candidates.extend(indices);
            }
        }
        append_tag_candidates(&self.tag_rules, &ctx.tag, &mut candidates);
        normalize_tail(&mut candidates, start);
        candidates
    }
}

enum RuleBucket<'a> {
    Id(&'a str),
    Class(&'a str),
    Attribute(&'a str),
    Tag(&'a str),
    Universal,
}

impl AttributeSelector {
    fn name(&self) -> &str {
        match self {
            Self::Present(name)
            | Self::Equals(name, _, _)
            | Self::Includes(name, _, _)
            | Self::DashMatch(name, _, _)
            | Self::Prefix(name, _, _)
            | Self::Suffix(name, _, _)
            | Self::Substring(name, _, _) => name,
        }
    }
}

impl RuleSet {
    pub(super) fn document_scope_for_node(
        &self,
        document: &Document,
        node: NodeId,
    ) -> Option<NodeId> {
        if self.document_roots.is_empty() {
            return None;
        }
        let mut current = Some(node);
        while let Some(node) = current {
            if self.document_roots.contains_key(&node) {
                return Some(node);
            }
            current = document.get_node(node).parent;
        }
        None
    }

    pub(super) fn push(&mut self, rule: Rule) {
        let index = self.rules.len();
        self.rules.push(rule);
        self.index_rule(index);
    }

    pub(super) fn rebuild_index(&mut self) {
        self.main.clear();
        self.pseudo.clear();
        self.has_sibling_dependencies = false;
        self.has_relational_dependencies = false;
        self.has_structural_dependencies = false;
        self.has_style_attribute_dependencies = false;
        self.has_sibling_scoped_relational = false;
        self.relational_anchors.clear();
        self.positional_anchors.clear();
        self.empty_anchors.clear();
        self.positional_unbounded = false;
        for index in 0..self.rules.len() {
            self.index_rule(index);
        }
    }

    fn index_rule(&mut self, index: usize) {
        self.has_sibling_dependencies |= self.rules[index].combinators.iter().any(|combinator| {
            matches!(
                combinator,
                super::Combinator::AdjacentSibling | super::Combinator::GeneralSibling
            )
        }) || self.rules[index]
            .chain
            .iter()
            .flat_map(|compound| &compound.pseudo_classes)
            .any(pseudo_has_sibling_dependency);
        self.has_relational_dependencies |= self.rules[index]
            .chain
            .iter()
            .flat_map(|compound| &compound.pseudo_classes)
            .any(pseudo_has_relational_dependency);
        self.has_structural_dependencies |= self.rules[index]
            .chain
            .iter()
            .flat_map(|compound| &compound.pseudo_classes)
            .any(pseudo_has_structural_dependency);

        self.index_positional_anchors(index);

        self.has_style_attribute_dependencies |= self.rules[index]
            .chain
            .iter()
            .any(compound_reads_style_attribute);

        let last = self.rules[index].chain.len().saturating_sub(1);
        let mut anchors = Vec::new();
        let mut sibling_scoped = false;
        for (position, compound) in self.rules[index].chain.iter().enumerate() {
            if !compound
                .pseudo_classes
                .iter()
                .any(pseudo_has_relational_dependency)
            {
                continue;
            }
            if compound
                .pseudo_classes
                .iter()
                .any(pseudo_has_sibling_scoped_relational)
            {
                sibling_scoped = true;
            }
            let mut anchor = compound.clone();
            anchor
                .pseudo_classes
                .retain(|pseudo| !pseudo_has_relational_dependency(pseudo));
            let reach = if position == last {
                0
            } else if self.rules[index].combinators[position..]
                .iter()
                .any(|combinator| {
                    matches!(
                        combinator,
                        super::Combinator::AdjacentSibling | super::Combinator::GeneralSibling
                    )
                })
            {
                2
            } else {
                1
            };
            anchors.push((anchor, reach));
        }
        self.has_sibling_scoped_relational |= sibling_scoped;
        self.relational_anchors.extend(anchors);

        // A rule lives in one best subject bucket. Matching gathers the
        // element's buckets plus universal rules, so no per-node RuleSet scan
        // or cross-bucket rule deduplication is required.
        let bucket = self.rules[index]
            .chain
            .last()
            .map(rule_bucket)
            .unwrap_or(RuleBucket::Universal);
        if self.rules[index].pseudo_element.is_some() {
            self.pseudo.insert(bucket, index);
        } else {
            self.main.insert(bucket, index);
        }
    }

    fn index_positional_anchors(&mut self, index: usize) {
        let rule = &self.rules[index];
        let mut anchors = Vec::new();
        let mut empty = Vec::new();
        let mut unbounded = false;
        for (position, compound) in rule.chain.iter().enumerate() {
            unbounded |= compound
                .pseudo_classes
                .iter()
                .any(pseudo_is_positionally_opaque);
            let sibling_rhs = position > 0
                && matches!(
                    rule.combinators[position - 1],
                    super::Combinator::AdjacentSibling | super::Combinator::GeneralSibling
                );
            let positional = compound.pseudo_classes.iter().any(pseudo_is_positional);
            let watches_children = compound
                .pseudo_classes
                .iter()
                .any(|pseudo| matches!(pseudo, super::PseudoClass::Empty));
            if !(positional || sibling_rhs || watches_children) {
                continue;
            }
            let mut subject = compound.clone();
            subject.pseudo_classes.clear();
            if watches_children {
                empty.push(subject.clone());
            }
            if positional || sibling_rhs {
                let parent = (position > 0
                    && matches!(rule.combinators[position - 1], super::Combinator::Child))
                .then(|| {
                    let mut parent = rule.chain[position - 1].clone();
                    parent.pseudo_classes.clear();
                    parent
                });
                anchors.push(PositionalAnchor {
                    subject,
                    parent,
                    sibling_rhs,
                });
            }
        }
        self.positional_unbounded |= unbounded;
        self.positional_anchors.extend(anchors);
        self.empty_anchors.extend(empty);
    }

    pub(super) fn positional_unbounded(&self) -> bool {
        self.positional_unbounded
    }

    /// Whether `ctx` (a child of `parent`) could change match result when its
    /// sibling list changes. `siblings_only` keeps the `+`/`~` anchors, the
    /// only ones a class/attribute change on a sibling can reach.
    pub(super) fn positional_anchor_matches(
        &self,
        ctx: &SelectorContext,
        parent: &SelectorContext,
        siblings_only: bool,
    ) -> bool {
        self.positional_anchors.iter().any(|anchor| {
            (!siblings_only || anchor.sibling_rhs)
                && anchor.subject.matches(ctx)
                && anchor
                    .parent
                    .as_ref()
                    .is_none_or(|compound| compound.matches(parent))
        })
    }

    /// Whether `ctx` carries `:empty` somewhere in a rule, so its own child
    /// list is observed.
    pub(super) fn empty_anchor_matches(&self, ctx: &SelectorContext) -> bool {
        self.empty_anchors.iter().any(|anchor| anchor.matches(ctx))
    }

    /// Candidates among rules without a pseudo-element.
    pub(super) fn candidate_indices_for_context(&self, ctx: &SelectorContext) -> Vec<usize> {
        self.main.candidates_for_context(ctx)
    }

    /// Candidates among rules without a pseudo-element.
    pub(super) fn candidate_indices_for_node(
        &self,
        document: &Document,
        node: NodeId,
    ) -> Vec<usize> {
        self.main.candidates_for_node(document, node)
    }

    /// Candidates among `::pseudo-element` rules only.
    pub(super) fn pseudo_candidate_indices_for_node(
        &self,
        document: &Document,
        node: NodeId,
    ) -> Vec<usize> {
        self.pseudo.candidates_for_node(document, node)
    }

    pub(super) fn has_sibling_dependencies(&self) -> bool {
        self.has_sibling_dependencies
    }

    pub(super) fn has_relational_dependencies(&self) -> bool {
        self.has_relational_dependencies
    }

    pub(super) fn has_structural_dependencies(&self) -> bool {
        self.has_structural_dependencies
    }

    pub(super) fn has_style_attribute_dependencies(&self) -> bool {
        self.has_style_attribute_dependencies
    }

    pub(super) fn has_sibling_scoped_relational(&self) -> bool {
        self.has_sibling_scoped_relational
    }

    /// `None` when `ctx` can never be the subject of, or an earlier compound
    /// of, a `:has()` rule. Otherwise the largest reach of a matching anchor.
    pub(super) fn relational_anchor(&self, ctx: &SelectorContext) -> Option<u8> {
        let mut found = None;
        for (anchor, reach) in &self.relational_anchors {
            if found.is_none_or(|current| *reach > current) && anchor.matches(ctx) {
                found = Some(*reach);
            }
        }
        found
    }
}

/// Conservative: an attribute selector on `style`, or a functional pseudo-class
/// whose argument mentions it (`:not([style])`, `:is(...)`, `:has(...)`).
fn compound_reads_style_attribute(compound: &super::CompoundSelector) -> bool {
    use super::AttributeSelector as A;
    compound.attributes.iter().any(|attribute| {
        let (A::Present(name)
        | A::Equals(name, ..)
        | A::Includes(name, ..)
        | A::DashMatch(name, ..)
        | A::Prefix(name, ..)
        | A::Suffix(name, ..)
        | A::Substring(name, ..)) = attribute;
        name.eq_ignore_ascii_case("style")
    }) || compound.pseudo_classes.iter().any(|pseudo| match pseudo {
        super::PseudoClass::Has(selector)
        | super::PseudoClass::Not(selector)
        | super::PseudoClass::Is(selector)
        | super::PseudoClass::Where(selector) => selector.to_ascii_lowercase().contains("style"),
        _ => false,
    })
}

fn pseudo_has_sibling_dependency(pseudo: &super::PseudoClass) -> bool {
    match pseudo {
        super::PseudoClass::Has(_) => true,
        super::PseudoClass::Not(selector) => selector.contains('+') || selector.contains('~'),
        _ => false,
    }
}

/// Position-dependent pseudo-classes whose subject is the sibling itself.
fn pseudo_is_positional(pseudo: &super::PseudoClass) -> bool {
    matches!(
        pseudo,
        super::PseudoClass::FirstChild
            | super::PseudoClass::LastChild
            | super::PseudoClass::FirstOfType
            | super::PseudoClass::LastOfType
            | super::PseudoClass::OnlyChild
            | super::PseudoClass::OnlyOfType
            | super::PseudoClass::NthChild(_)
            | super::PseudoClass::NthLastChild(_)
            | super::PseudoClass::NthOfType(_)
            | super::PseudoClass::NthLastOfType(_)
    )
}

/// Pseudo-classes whose sibling/structural reach the anchors cannot bound:
/// selector lists that may nest a positional or sibling selector, and
/// `:nth-*(An+B of S)`, which also reads sibling attributes.
fn pseudo_is_positionally_opaque(pseudo: &super::PseudoClass) -> bool {
    match pseudo {
        super::PseudoClass::Has(_) => true,
        super::PseudoClass::Not(selector)
        | super::PseudoClass::Is(selector)
        | super::PseudoClass::Where(selector) => {
            let selector = selector.to_ascii_lowercase();
            selector.contains('+')
                || selector.contains('~')
                || selector.contains(":nth-")
                || [
                    ":empty",
                    ":first-child",
                    ":last-child",
                    ":first-of-type",
                    ":last-of-type",
                    ":only-child",
                    ":only-of-type",
                ]
                .iter()
                .any(|needle| selector.contains(needle))
        }
        super::PseudoClass::NthChild(expression)
        | super::PseudoClass::NthLastChild(expression)
        | super::PseudoClass::NthOfType(expression)
        | super::PseudoClass::NthLastOfType(expression) => {
            expression.to_ascii_lowercase().contains(" of ")
        }
        _ => false,
    }
}

fn pseudo_has_structural_dependency(pseudo: &super::PseudoClass) -> bool {
    match pseudo {
        super::PseudoClass::FirstChild
        | super::PseudoClass::LastChild
        | super::PseudoClass::FirstOfType
        | super::PseudoClass::LastOfType
        | super::PseudoClass::OnlyChild
        | super::PseudoClass::OnlyOfType
        | super::PseudoClass::Empty
        | super::PseudoClass::NthChild(_)
        | super::PseudoClass::NthLastChild(_)
        | super::PseudoClass::NthOfType(_)
        | super::PseudoClass::NthLastOfType(_) => true,
        super::PseudoClass::Not(selector) | super::PseudoClass::Has(selector) => {
            let selector = selector.to_ascii_lowercase();
            [
                ":empty",
                ":first-child",
                ":last-child",
                ":first-of-type",
                ":last-of-type",
                ":only-child",
                ":only-of-type",
                ":nth-child",
                ":nth-last-child",
                ":nth-of-type",
                ":nth-last-of-type",
            ]
            .iter()
            .any(|needle| selector.contains(needle))
        }
        _ => false,
    }
}

fn pseudo_has_sibling_scoped_relational(pseudo: &super::PseudoClass) -> bool {
    match pseudo {
        super::PseudoClass::Has(selector) => selector.contains('+') || selector.contains('~'),
        super::PseudoClass::Not(selector) => {
            let lower = selector.to_ascii_lowercase();
            lower.contains(":has(") && (lower.contains('+') || lower.contains('~'))
        }
        _ => false,
    }
}

fn pseudo_has_relational_dependency(pseudo: &super::PseudoClass) -> bool {
    match pseudo {
        super::PseudoClass::Has(_) => true,
        super::PseudoClass::Not(selector) => selector.to_ascii_lowercase().contains(":has("),
        _ => false,
    }
}

fn rule_bucket(subject: &CompoundSelector) -> RuleBucket<'_> {
    if subject.unsupported {
        return RuleBucket::Universal;
    }
    if let Some(id) = &subject.id {
        return RuleBucket::Id(id);
    }
    if let Some(class) = subject.classes.first() {
        return RuleBucket::Class(class);
    }
    if let Some(attribute) = subject.attributes.first() {
        return RuleBucket::Attribute(attribute.name());
    }
    if let Some(tag) = &subject.tag {
        return RuleBucket::Tag(tag);
    }
    RuleBucket::Universal
}

fn attribute_index_key(name: &str) -> String {
    name.rsplit_once(['|', ':'])
        .map_or(name, |(_, local_name)| local_name)
        .to_ascii_lowercase()
}

fn append_tag_candidates(
    tag_rules: &HashMap<String, Vec<usize>>,
    tag: &str,
    candidates: &mut Vec<usize>,
) {
    let tag = tag.to_ascii_lowercase();
    if let Some(indices) = tag_rules.get(&tag) {
        candidates.extend(indices);
    }
    if let Some((_, local_name)) = tag.rsplit_once(':')
        && let Some(indices) = tag_rules.get(local_name)
    {
        candidates.extend(indices);
    }
}

/// Universal rules (before `start`) are unique; only the bucketed tail can repeat.
fn normalize_tail(candidates: &mut Vec<usize>, start: usize) {
    if candidates.len() - start < 2 {
        return;
    }
    candidates[start..].sort_unstable();
    let mut write = start + 1;
    for read in start + 1..candidates.len() {
        if candidates[read] != candidates[write - 1] {
            candidates[write] = candidates[read];
            write += 1;
        }
    }
    candidates.truncate(write);
}
