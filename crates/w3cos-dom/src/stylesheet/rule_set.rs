use std::collections::HashMap;

use super::{AttributeSelector, CompoundSelector, Document, NodeId, Rule, SelectorContext};

#[derive(Debug, Default)]
pub(super) struct RuleSet {
    pub(super) rules: Vec<Rule>,
    pub(super) document_roots: HashMap<NodeId, u64>,
    id_rules: HashMap<String, Vec<usize>>,
    class_rules: HashMap<String, Vec<usize>>,
    attribute_rules: HashMap<String, Vec<usize>>,
    tag_rules: HashMap<String, Vec<usize>>,
    universal_rules: Vec<usize>,
    has_sibling_dependencies: bool,
    has_relational_dependencies: bool,
    has_structural_dependencies: bool,
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
        self.id_rules.clear();
        self.class_rules.clear();
        self.attribute_rules.clear();
        self.tag_rules.clear();
        self.universal_rules.clear();
        self.has_sibling_dependencies = false;
        self.has_relational_dependencies = false;
        self.has_structural_dependencies = false;
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

        // A rule lives in one best subject bucket. Matching gathers the
        // element's buckets plus universal rules, so no per-node RuleSet scan
        // or cross-bucket rule deduplication is required.
        let class_rules = &self.class_rules;
        let bucket = self.rules[index]
            .chain
            .last()
            .map(|subject| rule_bucket(subject, |class| class_rules.get(class).map_or(0, Vec::len)))
            .unwrap_or(RuleBucket::Universal);
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

    pub(super) fn candidate_indices_for_context(&self, ctx: &SelectorContext) -> Vec<usize> {
        let mut candidates = self.universal_rules.clone();
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
        normalize_candidates(&mut candidates);
        candidates
    }

    pub(super) fn candidate_indices_for_node(
        &self,
        document: &Document,
        node: NodeId,
    ) -> Vec<usize> {
        let node = document.get_node(node);
        let mut candidates = self.universal_rules.clone();
        if let Some((_, id)) = node
            .attributes
            .iter()
            .find(|(name, _)| name.as_static() == "id")
            && let Some(indices) = self.id_rules.get(id.as_str())
        {
            candidates.extend(indices);
        }
        for class in &node.class_list {
            if let Some(indices) = self.class_rules.get(class.as_static()) {
                candidates.extend(indices);
            }
        }
        if !self.attribute_rules.is_empty() {
            for (name, _) in &node.attributes {
                if let Some(indices) = self
                    .attribute_rules
                    .get(&attribute_index_key(name.as_static()))
                {
                    candidates.extend(indices);
                }
            }
        }
        if !self.tag_rules.is_empty() {
            append_tag_candidates(&self.tag_rules, node.tag.as_static(), &mut candidates);
        }
        normalize_candidates(&mut candidates);
        candidates
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
}

fn pseudo_has_sibling_dependency(pseudo: &super::PseudoClass) -> bool {
    match pseudo {
        super::PseudoClass::Has(_) => true,
        super::PseudoClass::Not(selector) => selector.contains('+') || selector.contains('~'),
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

fn pseudo_has_relational_dependency(pseudo: &super::PseudoClass) -> bool {
    match pseudo {
        super::PseudoClass::Has(_) => true,
        super::PseudoClass::Not(selector) => selector.to_ascii_lowercase().contains(":has("),
        _ => false,
    }
}

/// Blink's RuleSet files each rule under one subject key (id, class,
/// attribute, tag, universal). Any class of the subject compound is a valid
/// key; take the one whose bucket is currently smallest so `.cell.k5` lands
/// under `k5` instead of the shared `cell` bucket every cell would scan.
fn rule_bucket(
    subject: &CompoundSelector,
    class_bucket_len: impl Fn(&str) -> usize,
) -> RuleBucket<'_> {
    if subject.unsupported {
        return RuleBucket::Universal;
    }
    if let Some(id) = &subject.id {
        return RuleBucket::Id(id);
    }
    if let Some(class) = subject
        .classes
        .iter()
        .min_by_key(|class| class_bucket_len(class))
    {
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
    let lowered;
    let tag = if tag.bytes().any(|byte| byte.is_ascii_uppercase()) {
        lowered = tag.to_ascii_lowercase();
        lowered.as_str()
    } else {
        tag
    };
    if let Some(indices) = tag_rules.get(tag) {
        candidates.extend(indices);
    }
    if let Some((_, local_name)) = tag.rsplit_once(':')
        && let Some(indices) = tag_rules.get(local_name)
    {
        candidates.extend(indices);
    }
}

fn normalize_candidates(candidates: &mut Vec<usize>) {
    candidates.sort_unstable();
    candidates.dedup();
}
