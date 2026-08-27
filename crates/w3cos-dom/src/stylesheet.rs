//! Runtime stylesheet registry + selector matcher.
//!
//! CSS imported by ESM modules (`import "./x.css"`) is parsed at compile time
//! by `w3cos-compiler` and baked into the generated bundle as precompiled
//! selector bytecode. [`Document::to_component_tree`](crate::Document) then
//! applies matching rules *before* inline styles (inline wins).
//!
//! Supported selectors:
//! - `*`, `tag`, `.class`, `#id`
//! - compound `tag.a.b` / `tag.a#id`
//! - descendant `A B` and child `A > B` combinators
//! - attribute presence/equality/token/dash/prefix/suffix/substring matching
//! - structural `:first-child` and `:root`
//! - comma groups (split into separate rules at registration)
//!
//! Dynamic pseudo-classes (`:hover`, `:focus`, ...) remain inert until their
//! required runtime state is represented. Generated `::before`/`::after`
//! declarations are exposed separately for the component-tree bridge.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::{Document, NodeId, NodeType};

mod rule_set;
mod selector_bytecode;
mod selector_filter;
mod selector_parse_cache;

use rule_set::RuleSet;
use selector_filter::AncestorBloom;

/// Ancestor-chain entry used for descendant/child combinator matching.
#[derive(Debug, Clone, Default)]
pub struct SelectorContext {
    pub tag: String,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attributes: Vec<(String, String)>,
    pub is_first_child: bool,
    pub is_root: bool,
    pub previous_siblings: Vec<Rc<SelectorContext>>,
    /// HTML defines a small set of enumerated attribute values as ASCII
    /// case-insensitive. XML/XHTML attribute values remain case-sensitive.
    pub html_document: bool,
    /// HTML element names and attribute names are ASCII case-insensitive in
    /// selectors; foreign and XML element names are case-sensitive.
    pub html_element: bool,
}

impl SelectorContext {
    pub fn new(tag: &str, id: Option<&str>, classes: &[&str]) -> Self {
        Self {
            tag: tag.to_string(),
            id: id.map(|s| s.to_string()),
            classes: classes.iter().map(|s| s.to_string()).collect(),
            attributes: Vec::new(),
            is_first_child: false,
            is_root: false,
            previous_siblings: Vec::new(),
            html_document: true,
            html_element: true,
        }
    }

    pub fn with_attributes(mut self, attributes: &[(&str, &str)]) -> Self {
        self.attributes = attributes
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        self
    }

    pub fn with_tree_state(mut self, is_first_child: bool, is_root: bool) -> Self {
        self.is_first_child = is_first_child;
        self.is_root = is_root;
        self
    }

    pub fn with_previous_siblings(mut self, previous_siblings: Vec<SelectorContext>) -> Self {
        self.previous_siblings = previous_siblings.into_iter().map(Rc::new).collect();
        self
    }

    pub(crate) fn with_shared_previous_siblings(
        mut self,
        previous_siblings: Vec<Rc<SelectorContext>>,
    ) -> Self {
        self.previous_siblings = previous_siblings;
        self
    }

    pub fn with_html_document(mut self, html_document: bool) -> Self {
        self.html_document = html_document;
        self
    }

    pub fn with_html_element(mut self, html_element: bool) -> Self {
        self.html_element = html_element;
        self
    }
}

#[derive(Debug, Clone)]
enum AttributeSelector {
    Present(String),
    Equals(String, String, bool),
    Includes(String, String, bool),
    DashMatch(String, String, bool),
    Prefix(String, String, bool),
    Suffix(String, String, bool),
    Substring(String, String, bool),
}

#[derive(Debug, Clone)]
enum PseudoClass {
    FirstChild,
    LastChild,
    FirstOfType,
    LastOfType,
    OnlyChild,
    OnlyOfType,
    Empty,
    Root,
    Link,
    Visited,
    Target,
    Enabled,
    Disabled,
    Checked,
    Dir(String),
    Lang(String),
    Has(String),
    Not(String),
    NthChild(String),
    NthLastChild(String),
    NthOfType(String),
    NthLastOfType(String),
}

/// Combinator linking a compound selector to the compound on its left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Combinator {
    /// `A B` — any ancestor.
    Descendant,
    /// `A > B` — direct parent only.
    Child,
    /// `A + B` — immediately preceding element sibling.
    AdjacentSibling,
    /// `A ~ B` — any preceding element sibling.
    GeneralSibling,
}

/// A single compound selector (no combinators), e.g. `div.item#main`.
#[derive(Debug, Clone, Default)]
struct CompoundSelector {
    universal: bool,
    any_namespace: bool,
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attributes: Vec<AttributeSelector>,
    pseudo_classes: Vec<PseudoClass>,
    /// Contains a selector feature that cannot yet be evaluated — the rule
    /// never matches.
    unsupported: bool,
}

impl CompoundSelector {
    fn matches(&self, ctx: &SelectorContext) -> bool {
        if self.unsupported {
            return false;
        }
        if let Some(tag) = &self.tag {
            let actual_tag = if self.any_namespace {
                ctx.tag
                    .rsplit_once(':')
                    .map_or(ctx.tag.as_str(), |(_, local)| local)
            } else {
                &ctx.tag
            };
            let tag_matches = if ctx.html_element {
                tag.eq_ignore_ascii_case(actual_tag)
            } else {
                tag == actual_tag
            };
            if !tag_matches {
                return false;
            }
        }
        if let Some(id) = &self.id
            && ctx.id.as_deref() != Some(id.as_str())
        {
            return false;
        }
        self.classes
            .iter()
            .all(|c| ctx.classes.iter().any(|have| have == c))
            && self.attributes.iter().all(|selector| match selector {
                AttributeSelector::Present(name) => ctx
                    .attributes
                    .iter()
                    .any(|(have, _)| attribute_name_eq(ctx, have, name)),
                AttributeSelector::Equals(name, value, insensitive) => {
                    ctx.attributes.iter().any(|(have, actual)| {
                        attribute_name_eq(ctx, have, name)
                            && attribute_value_eq(ctx, name, actual, value, *insensitive)
                    })
                }
                AttributeSelector::Includes(name, value, insensitive) => {
                    !value.is_empty()
                        && ctx.attributes.iter().any(|(have, actual)| {
                            attribute_name_eq(ctx, have, name)
                                && actual.split_ascii_whitespace().any(|token| {
                                    attribute_value_eq(ctx, name, token, value, *insensitive)
                                })
                        })
                }
                AttributeSelector::DashMatch(name, value, insensitive) => {
                    !value.is_empty()
                        && ctx.attributes.iter().any(|(have, actual)| {
                            attribute_name_eq(ctx, have, name)
                                && (attribute_value_eq(ctx, name, actual, value, *insensitive)
                                    || attribute_value_dash_prefix(
                                        ctx,
                                        name,
                                        actual,
                                        value,
                                        *insensitive,
                                    ))
                        })
                }
                AttributeSelector::Prefix(name, value, insensitive) => {
                    !value.is_empty()
                        && ctx.attributes.iter().any(|(have, actual)| {
                            attribute_name_eq(ctx, have, name)
                                && attribute_value_starts_with(actual, value, *insensitive)
                        })
                }
                AttributeSelector::Suffix(name, value, insensitive) => {
                    !value.is_empty()
                        && ctx.attributes.iter().any(|(have, actual)| {
                            attribute_name_eq(ctx, have, name)
                                && attribute_value_ends_with(actual, value, *insensitive)
                        })
                }
                AttributeSelector::Substring(name, value, insensitive) => {
                    !value.is_empty()
                        && ctx.attributes.iter().any(|(have, actual)| {
                            attribute_name_eq(ctx, have, name)
                                && attribute_value_contains(actual, value, *insensitive)
                        })
                }
            })
            && self.pseudo_classes.iter().all(|pseudo| match pseudo {
                PseudoClass::FirstChild => ctx.is_first_child,
                PseudoClass::Root => ctx.is_root,
                _ => false,
            })
    }

    fn specificity(&self) -> u32 {
        let ids = u32::from(self.id.is_some());
        let classes =
            (self.classes.len() + self.attributes.len() + self.pseudo_classes.len()) as u32;
        let tags = u32::from(self.tag.is_some());
        ids * 1_000_000 + classes * 1_000 + tags
    }
}

fn attribute_name_eq(ctx: &SelectorContext, actual: &str, expected: &str) -> bool {
    if let Some(local_name) = expected.strip_prefix("*|") {
        let actual_local_name = actual
            .rsplit_once(':')
            .map_or(actual, |(_, actual_local_name)| actual_local_name);
        return if ctx.html_element {
            actual_local_name.eq_ignore_ascii_case(local_name)
        } else {
            actual_local_name == local_name
        };
    }
    if ctx.html_element {
        actual.eq_ignore_ascii_case(expected)
    } else {
        actual == expected
    }
}

fn attribute_value_eq(
    ctx: &SelectorContext,
    attribute_name: &str,
    actual: &str,
    expected: &str,
    insensitive: bool,
) -> bool {
    if insensitive || ctx.html_document && attribute_name.eq_ignore_ascii_case("lang") {
        actual.eq_ignore_ascii_case(expected)
    } else {
        actual == expected
    }
}

fn attribute_value_dash_prefix(
    ctx: &SelectorContext,
    attribute_name: &str,
    actual: &str,
    expected: &str,
    insensitive: bool,
) -> bool {
    let Some(prefix) = actual.get(..expected.len()) else {
        return false;
    };
    attribute_value_eq(ctx, attribute_name, prefix, expected, insensitive)
        && actual
            .get(expected.len()..)
            .is_some_and(|suffix| suffix.starts_with('-'))
}

fn attribute_value_starts_with(actual: &str, expected: &str, insensitive: bool) -> bool {
    if insensitive {
        actual
            .to_ascii_lowercase()
            .starts_with(&expected.to_ascii_lowercase())
    } else {
        actual.starts_with(expected)
    }
}

fn attribute_value_ends_with(actual: &str, expected: &str, insensitive: bool) -> bool {
    if insensitive {
        actual
            .to_ascii_lowercase()
            .ends_with(&expected.to_ascii_lowercase())
    } else {
        actual.ends_with(expected)
    }
}

fn attribute_value_contains(actual: &str, expected: &str, insensitive: bool) -> bool {
    if insensitive {
        actual
            .to_ascii_lowercase()
            .contains(&expected.to_ascii_lowercase())
    } else {
        actual.contains(expected)
    }
}

/// A registered rule: a selector chain (rightmost = subject) + declarations.
#[derive(Debug, Clone)]
struct Rule {
    /// Compounds left-to-right; `combinators[i]` links `chain[i]` to `chain[i+1]`.
    chain: Vec<CompoundSelector>,
    combinators: Vec<Combinator>,
    declarations: Vec<(String, String)>,
    specificity: u32,
    order: u32,
    /// `None` identifies process/application styles registered by native AOT.
    /// Browser page styles use a loader-owned id so navigation can release
    /// only that page's rules without deleting the host application's CSS.
    owner: Option<u64>,
    /// Pseudo-elements participate in their own cascade and never style the
    /// originating element's principal box.
    pseudo_element: Option<String>,
    /// Required features on the target's ancestor chain. This is only a
    /// no-false-negative prefilter; the complete matcher remains authoritative.
    ancestor_filter: AncestorBloom,
}

thread_local! {
    static RULES: RefCell<RuleSet> = RefCell::new(RuleSet::default());
    static STYLE_GENERATION: Cell<u64> = const { Cell::new(1) };
}

fn bump_generation() {
    STYLE_GENERATION.with(|generation| generation.set(generation.get().wrapping_add(1)));
}

pub(crate) fn generation() -> u64 {
    STYLE_GENERATION.with(Cell::get)
}

pub(crate) fn has_sibling_dependencies() -> bool {
    RULES.with(|rules| rules.borrow().has_sibling_dependencies())
}

pub(crate) fn has_relational_dependencies() -> bool {
    RULES.with(|rules| rules.borrow().has_relational_dependencies())
}

const CONTAINER_QUERY_MARKER: &str = "__w3cos_container_query";

fn rule_has_container_query(rule: &Rule) -> bool {
    rule.declarations
        .iter()
        .any(|(property, _)| property == CONTAINER_QUERY_MARKER)
}

/// Register a stylesheet rule. Comma-separated selector groups are split into
/// independent rules. One syntactically invalid selector invalidates the
/// complete selector list, as required by CSS 2.1.
pub fn register_rule(selector: &str, declarations: &[(&str, &str)]) {
    register_rule_with_owner(None, selector, declarations);
}

/// Register one page-owned Browser rule.
pub fn register_rule_for_owner(owner: u64, selector: &str, declarations: &[(&str, &str)]) {
    register_rule_with_owner(Some(owner), selector, declarations);
}

fn register_rule_with_owner(owner: Option<u64>, selector: &str, declarations: &[(&str, &str)]) {
    if declarations.is_empty() {
        return;
    }
    let Some(parsed) = parse_selector_list(selector) else {
        return;
    };
    register_parsed_rules(owner, parsed, declarations);
}

fn parse_selector_list(
    selector: &str,
) -> Option<Vec<(Vec<CompoundSelector>, Vec<Combinator>, Option<String>)>> {
    split_selector_group(selector)
        .into_iter()
        .map(|single| {
            let (single, pseudo_element) = strip_terminal_pseudo_element(&single);
            parse_selector_chain_cached(&single).map(|parsed| {
                (
                    parsed.chain.clone(),
                    parsed.combinators.clone(),
                    pseudo_element,
                )
            })
        })
        .collect()
}

/// Compile one authored selector list into the versioned bytecode consumed by
/// [`register_compiled_rule`]. Static ESM CSS uses this at W3COS build time;
/// dynamic stylesheets continue to call [`register_rule`].
pub fn compile_selector_bytecode(selector: &str) -> Option<Vec<Vec<u8>>> {
    parse_selector_list(selector).map(|parsed| {
        parsed
            .into_iter()
            .map(|(chain, combinators, pseudo_element)| {
                selector_bytecode::encode(&chain, &combinators, pseudo_element.as_deref())
            })
            .collect()
    })
}

/// Register a selector that was parsed and encoded by
/// [`compile_selector_bytecode`] during AOT compilation.
pub fn register_compiled_rule(bytecode: &[u8], declarations: &[(&str, &str)]) {
    if declarations.is_empty() {
        return;
    }
    let Some((chain, combinators, pseudo_element)) = selector_bytecode::decode(bytecode) else {
        return;
    };
    register_parsed_rules(
        None,
        vec![(chain, combinators, pseudo_element)],
        declarations,
    );
}

fn register_parsed_rules(
    owner: Option<u64>,
    parsed: Vec<(Vec<CompoundSelector>, Vec<Combinator>, Option<String>)>,
    declarations: &[(&str, &str)],
) {
    let added = RULES.with(|rules| {
        let mut rules = rules.borrow_mut();
        let initial_len = rules.rules.len();
        for (chain, combinators, pseudo_element) in parsed {
            let specificity = chain.iter().map(CompoundSelector::specificity).sum();
            let ancestor_filter = AncestorBloom::for_rule(&chain, &combinators);
            let order = rules.rules.len() as u32;
            rules.push(Rule {
                chain,
                combinators,
                declarations: declarations
                    .iter()
                    .map(|(p, v)| (p.to_string(), v.to_string()))
                    .collect(),
                specificity,
                order,
                owner,
                pseudo_element,
                ancestor_filter,
            });
        }
        rules.rules.len() != initial_len
    });
    if added {
        bump_generation();
    }
}

fn strip_terminal_pseudo_element(selector: &str) -> (String, Option<String>) {
    let selector = selector.trim_end();
    let lower = selector.to_ascii_lowercase();
    for (authored, canonical) in [
        ("::before", "::before"),
        ("::after", "::after"),
        (":before", "::before"),
        (":after", "::after"),
    ] {
        if lower.ends_with(authored) {
            let subject = &selector[..selector.len() - authored.len()];
            return (subject.trim_end().to_string(), Some(canonical.to_string()));
        }
    }
    (selector.to_string(), None)
}

/// Remove Browser rules owned by one page/loader while preserving native AOT
/// and other page contexts.
pub fn clear_owner(owner: u64) {
    // Loaders can be retained by another thread-local and dropped during
    // thread teardown after this registry has already been destroyed.
    // Cleanup is idempotent, so a late teardown must become a no-op instead
    // of panicking on TLS destruction order.
    let removed = RULES.try_with(|rules| {
        let mut rules = rules.borrow_mut();
        let initial_len = rules.rules.len();
        rules.rules.retain(|rule| rule.owner != Some(owner));
        rules.rebuild_index();
        rules.rules.len() != initial_len
    });
    if matches!(removed, Ok(true)) {
        bump_generation();
    }
}

/// Remove all registered rules.
pub fn clear_rules() {
    let removed = RULES.with(|rules| {
        let mut rules = rules.borrow_mut();
        let removed = !rules.rules.is_empty();
        *rules = RuleSet::default();
        removed
    });
    if removed {
        bump_generation();
    }
}

/// Number of registered rules (after comma-group splitting).
pub fn rule_count() -> usize {
    RULES.with(|rules| rules.borrow().rules.len())
}

/// Whether any rules are registered — fast path for the DOM walk.
pub fn has_rules() -> bool {
    RULES.with(|rules| !rules.borrow().rules.is_empty())
}

/// Declarations of every rule matching the given element, ordered for
/// application: ascending (specificity, registration order) so that applying
/// them sequentially leaves the winning value last. Each declaration carries
/// its rule's specificity.
///
/// `ancestors` is the element's ancestor chain, nearest parent LAST.
pub fn matching_declarations(
    tag: &str,
    id: Option<&str>,
    classes: &[&str],
    ancestors: &[SelectorContext],
) -> Vec<(String, String, u32)> {
    let ctx = SelectorContext::new(tag, id, classes);
    matching_declarations_for_context(&ctx, ancestors)
}

pub fn matching_declarations_for_context(
    ctx: &SelectorContext,
    ancestors: &[SelectorContext],
) -> Vec<(String, String, u32)> {
    let ancestor_bloom = AncestorBloom::for_contexts(ancestors);
    RULES.with(|rules| {
        let rules = rules.borrow();
        let mut matched: Vec<&Rule> = rules
            .candidate_indices_for_context(ctx)
            .into_iter()
            .map(|index| &rules.rules[index])
            .filter(|rule| {
                rule.pseudo_element.is_none()
                    && !rule_has_container_query(rule)
                    && ancestor_bloom.might_contain(rule.ancestor_filter)
                    && rule_matches(rule, &ctx, ancestors)
            })
            .collect();
        matched.sort_by_key(|rule| (rule.specificity, rule.order));
        let mut out = Vec::new();
        for rule in matched {
            for (prop, value) in &rule.declarations {
                if prop != CONTAINER_QUERY_MARKER {
                    out.push((prop.clone(), value.clone(), rule.specificity));
                }
            }
        }
        out
    })
}

#[cfg(test)]
fn candidate_rule_count_for_context(ctx: &SelectorContext) -> usize {
    RULES.with(|rules| rules.borrow().candidate_indices_for_context(ctx).len())
}

#[cfg(test)]
fn ancestor_filtered_candidate_rule_count_for_context(
    ctx: &SelectorContext,
    ancestors: &[SelectorContext],
) -> usize {
    let ancestor_bloom = AncestorBloom::for_contexts(ancestors);
    RULES.with(|rules| {
        let rules = rules.borrow();
        rules
            .candidate_indices_for_context(ctx)
            .into_iter()
            .filter(|index| ancestor_bloom.might_contain(rules.rules[*index].ancestor_filter))
            .count()
    })
}

fn matched_property_value(
    rules: &[Rule],
    document: &Document,
    node: NodeId,
    property: &str,
) -> Option<String> {
    let ancestor_bloom = AncestorBloom::for_node(document, node);
    let mut declarations = rules
        .iter()
        .filter(|rule| rule.pseudo_element.is_none() && !rule_has_container_query(rule))
        .filter(|rule| ancestor_bloom.might_contain(rule.ancestor_filter))
        .filter(|rule| {
            matches_chain_node(
                document,
                node,
                &rule.chain,
                &rule.combinators,
                rule.chain.len() - 1,
                None,
                None,
            )
        })
        .filter_map(|rule| {
            rule.declarations
                .iter()
                .rev()
                .find(|(name, _)| name.eq_ignore_ascii_case(property))
                .map(|(_, value)| (rule.specificity, rule.order, value.clone()))
        })
        .collect::<Vec<_>>();
    declarations.sort_by_key(|(specificity, order, _)| (*specificity, *order));
    declarations.pop().map(|(_, _, value)| value)
}

fn css_length_px(value: &str) -> Option<f32> {
    let value = value.trim();
    value
        .strip_suffix("px")
        .and_then(|number| number.trim().parse::<f32>().ok())
        .or_else(|| {
            value
                .strip_suffix("rem")
                .and_then(|number| number.trim().parse::<f32>().ok())
                .map(|number| number * 16.0)
        })
}

fn container_condition_matches(prelude: &str, width: f32, height: f32) -> bool {
    let Some(condition_start) = prelude.find('(') else {
        return false;
    };
    prelude[condition_start..].split(" and ").all(|condition| {
        let condition = condition
            .trim()
            .trim_matches(|character| character == '(' || character == ')')
            .trim();
        if let Some((feature, value)) = condition.split_once(':') {
            let Some(length) = css_length_px(value) else {
                return false;
            };
            return match feature.trim() {
                "min-width" => width >= length,
                "max-width" => width <= length,
                "min-height" => height >= length,
                "max-height" => height <= length,
                _ => false,
            };
        }
        let mut parts = condition.split_ascii_whitespace();
        let feature = parts.next().unwrap_or_default();
        let operator = parts.next().unwrap_or_default();
        let Some(length) = parts.next().and_then(css_length_px) else {
            return false;
        };
        match (feature, operator) {
            ("width", "=") => (width - length).abs() < f32::EPSILON,
            ("width", ">") => width > length,
            ("width", ">=") => width >= length,
            ("width", "<") => width < length,
            ("width", "<=") => width <= length,
            ("height", "=") => (height - length).abs() < f32::EPSILON,
            ("height", ">") => height > length,
            ("height", ">=") => height >= length,
            ("height", "<") => height < length,
            ("height", "<=") => height <= length,
            _ => false,
        }
    })
}

fn container_query_matches(rules: &[Rule], document: &Document, node: NodeId, query: &str) -> bool {
    let mut ancestor = document.get_node(node).parent;
    while let Some(candidate) = ancestor {
        let container_type = matched_property_value(rules, document, candidate, "container-type");
        if container_type
            .as_deref()
            .is_some_and(|value| matches!(value.trim(), "size" | "inline-size"))
        {
            let width = matched_property_value(rules, document, candidate, "width")
                .as_deref()
                .and_then(css_length_px)
                .unwrap_or_default();
            let height = matched_property_value(rules, document, candidate, "height")
                .as_deref()
                .and_then(css_length_px)
                .unwrap_or_default();
            return container_condition_matches(query, width, height);
        }
        ancestor = document.get_node(candidate).parent;
    }
    false
}

/// Declarations matching a concrete DOM element. Tree-dependent selectors
/// such as `:has()` cannot be evaluated from a flattened [`SelectorContext`],
/// so live documents use this path for cascade matching.
pub fn matching_declarations_for_node(
    document: &Document,
    node: NodeId,
) -> Vec<(String, String, u32)> {
    let ancestor_bloom = AncestorBloom::for_node(document, node);
    RULES.with(|rules| {
        let rules = rules.borrow();
        let mut matched: Vec<&Rule> = rules
            .candidate_indices_for_node(document, node)
            .into_iter()
            .map(|index| &rules.rules[index])
            .filter(|rule| {
                rule.pseudo_element.is_none()
                    && ancestor_bloom.might_contain(rule.ancestor_filter)
                    && matches_chain_node(
                        document,
                        node,
                        &rule.chain,
                        &rule.combinators,
                        rule.chain.len() - 1,
                        None,
                        None,
                    )
                    && rule
                        .declarations
                        .iter()
                        .filter(|(property, _)| property == CONTAINER_QUERY_MARKER)
                        .all(|(_, query)| {
                            container_query_matches(&rules.rules, document, node, query)
                        })
            })
            .collect();
        matched.sort_by_key(|rule| (rule.specificity, rule.order));
        let mut out = Vec::new();
        for rule in matched {
            for (prop, value) in &rule.declarations {
                if prop != CONTAINER_QUERY_MARKER {
                    out.push((prop.clone(), value.clone(), rule.specificity));
                }
            }
        }
        out
    })
}

/// Declarations matching one generated pseudo-element box. The selector is
/// evaluated against its originating element, while cascade results stay
/// isolated from the element's principal style.
pub fn matching_pseudo_declarations_for_node(
    document: &Document,
    node: NodeId,
    pseudo_element: &str,
) -> Vec<(String, String, u32)> {
    let ancestor_bloom = AncestorBloom::for_node(document, node);
    RULES.with(|rules| {
        let rules = rules.borrow();
        let mut matched: Vec<&Rule> = rules
            .candidate_indices_for_node(document, node)
            .into_iter()
            .map(|index| &rules.rules[index])
            .filter(|rule| rule.pseudo_element.as_deref() == Some(pseudo_element))
            .filter(|rule| ancestor_bloom.might_contain(rule.ancestor_filter))
            .filter(|rule| {
                matches_chain_node(
                    document,
                    node,
                    &rule.chain,
                    &rule.combinators,
                    rule.chain.len() - 1,
                    None,
                    None,
                ) && rule
                    .declarations
                    .iter()
                    .filter(|(property, _)| property == CONTAINER_QUERY_MARKER)
                    .all(|(_, query)| container_query_matches(&rules.rules, document, node, query))
            })
            .collect();
        matched.sort_by_key(|rule| (rule.specificity, rule.order));
        let mut out = Vec::new();
        for rule in matched {
            for (property, value) in &rule.declarations {
                if property != CONTAINER_QUERY_MARKER {
                    out.push((property.clone(), value.clone(), rule.specificity));
                }
            }
        }
        out
    })
}

/// Parse and match an authored selector list without registering a stylesheet
/// rule. This is the shared selector path used by DOM `matches()`.
pub fn selector_matches_context(
    selector: &str,
    ctx: &SelectorContext,
    ancestors: &[SelectorContext],
) -> Result<bool, ()> {
    let selector = trim_css_whitespace(selector);
    if selector.is_empty() || selector.starts_with(',') || selector.ends_with(',') {
        return Err(());
    }
    let parsed = split_selector_group(selector)
        .into_iter()
        .map(|single| parse_selector_chain_cached(&single))
        .collect::<Option<Vec<_>>>()
        .ok_or(())?;
    let ancestor_bloom = AncestorBloom::for_contexts(ancestors);
    Ok(parsed.into_iter().any(|parsed| {
        ancestor_bloom.might_contain(parsed.ancestor_filter)
            && selector_chain_matches_context(&parsed.chain, &parsed.combinators, ctx, ancestors)
    }))
}

/// Match an authored selector list directly against DOM node ids. Unlike the
/// cascade path this walks parents and siblings lazily, avoiding recursive
/// `SelectorContext` cloning for DOM API calls that may execute thousands of
/// matches against the same large document.
pub fn selector_matches_node(
    selector: &str,
    document: &Document,
    node: NodeId,
) -> Result<bool, ()> {
    selector_matches_node_with_target(selector, document, node, None)
}

/// Match a node while supplying its browsing context's decoded URL target.
/// Location state belongs to the runtime document, not to DOM attributes, so
/// browsing-context callers provide it explicitly for `:target` evaluation.
pub fn selector_matches_node_with_target(
    selector: &str,
    document: &Document,
    node: NodeId,
    target_id: Option<&str>,
) -> Result<bool, ()> {
    let selector = trim_css_whitespace(selector);
    if selector.is_empty() || selector.starts_with(',') || selector.ends_with(',') {
        return Err(());
    }
    let parsed = split_selector_group(selector)
        .into_iter()
        .map(|single| parse_selector_chain_cached(&single))
        .collect::<Option<Vec<_>>>()
        .ok_or(())?;
    Ok(parsed.into_iter().any(|parsed| {
        matches_chain_node(
            document,
            node,
            &parsed.chain,
            &parsed.combinators,
            parsed.chain.len() - 1,
            target_id,
            None,
        )
    }))
}

/// Match prefixes of a relative selector under a reference element. This is
/// the legacy Selectors API overload exercised by the upstream WPT: the
/// reference element supplies the implicit scope, and nodes on a matching
/// relative-selector path are observable as matches.
pub fn selector_matches_node_relative_to_scope(
    selector: &str,
    document: &Document,
    node: NodeId,
    scope: NodeId,
    target_id: Option<&str>,
) -> Result<bool, ()> {
    let selector = trim_css_whitespace(selector);
    if selector.is_empty() || selector.starts_with(',') || selector.ends_with(',') {
        return Err(());
    }
    let parsed = split_selector_group(selector)
        .into_iter()
        .map(|single| parse_selector_chain_cached(&single))
        .collect::<Option<Vec<_>>>()
        .ok_or(())?;
    Ok(parsed.into_iter().any(|parsed| {
        (0..parsed.chain.len()).any(|index| {
            matches_chain_node(
                document,
                node,
                &parsed.chain,
                &parsed.combinators,
                index,
                target_id,
                Some(scope),
            )
        })
    }))
}

fn matches_chain_node(
    document: &Document,
    node: NodeId,
    chain: &[CompoundSelector],
    combinators: &[Combinator],
    index: usize,
    target_id: Option<&str>,
    scope: Option<NodeId>,
) -> bool {
    if document.get_node(node).node_type != NodeType::Element
        || !compound_matches_node(document, node, &chain[index], target_id)
    {
        return false;
    }
    if index == 0 {
        return scope
            .is_none_or(|scope| node != scope && node_is_descendant_of(document, node, scope));
    }
    match combinators[index - 1] {
        Combinator::Child => document.get_node(node).parent.is_some_and(|parent| {
            matches_chain_node(
                document,
                parent,
                chain,
                combinators,
                index - 1,
                target_id,
                scope,
            )
        }),
        Combinator::Descendant => {
            let mut ancestor = document.get_node(node).parent;
            while let Some(candidate) = ancestor {
                if matches_chain_node(
                    document,
                    candidate,
                    chain,
                    combinators,
                    index - 1,
                    target_id,
                    scope,
                ) {
                    return true;
                }
                ancestor = document.get_node(candidate).parent;
            }
            false
        }
        Combinator::AdjacentSibling => {
            previous_element_sibling(document, node).is_some_and(|sibling| {
                matches_chain_node(
                    document,
                    sibling,
                    chain,
                    combinators,
                    index - 1,
                    target_id,
                    scope,
                )
            })
        }
        Combinator::GeneralSibling => {
            let mut sibling = previous_element_sibling(document, node);
            while let Some(candidate) = sibling {
                if matches_chain_node(
                    document,
                    candidate,
                    chain,
                    combinators,
                    index - 1,
                    target_id,
                    scope,
                ) {
                    return true;
                }
                sibling = previous_element_sibling(document, candidate);
            }
            false
        }
    }
}

fn node_is_descendant_of(document: &Document, node: NodeId, ancestor: NodeId) -> bool {
    let mut parent = document.get_node(node).parent;
    while let Some(candidate) = parent {
        if candidate == ancestor {
            return true;
        }
        parent = document.get_node(candidate).parent;
    }
    false
}

fn compound_matches_node(
    document: &Document,
    node: NodeId,
    compound: &CompoundSelector,
    target_id: Option<&str>,
) -> bool {
    let mut basic = compound.clone();
    basic.pseudo_classes.clear();
    if !basic.matches(&selector_context_for_node(document, node)) {
        return false;
    }
    compound
        .pseudo_classes
        .iter()
        .all(|pseudo| pseudo_matches_node(document, node, pseudo, target_id))
}

fn pseudo_matches_node(
    document: &Document,
    node: NodeId,
    pseudo: &PseudoClass,
    target_id: Option<&str>,
) -> bool {
    let dom_node = document.get_node(node);
    let element_siblings = || {
        dom_node.parent.map_or_else(Vec::new, |parent| {
            document
                .children_ids(parent)
                .into_iter()
                .filter(|sibling| document.get_node(*sibling).node_type == NodeType::Element)
                .collect::<Vec<_>>()
        })
    };
    let type_siblings = || {
        element_siblings()
            .into_iter()
            .filter(|sibling| {
                document
                    .get_node(*sibling)
                    .tag
                    .as_str()
                    .eq_ignore_ascii_case(&dom_node.tag.as_str())
            })
            .collect::<Vec<_>>()
    };
    match pseudo {
        PseudoClass::FirstChild => element_siblings().first().copied() == Some(node),
        PseudoClass::LastChild => element_siblings().last().copied() == Some(node),
        PseudoClass::FirstOfType => type_siblings().first().copied() == Some(node),
        PseudoClass::LastOfType => type_siblings().last().copied() == Some(node),
        PseudoClass::OnlyChild => element_siblings().as_slice() == [node],
        PseudoClass::OnlyOfType => type_siblings().as_slice() == [node],
        PseudoClass::Empty => document.children_ids(node).into_iter().all(|child| {
            let child = document.get_node(child);
            match child.node_type {
                NodeType::Element => false,
                NodeType::Text | NodeType::CdataSection => {
                    child.text_content.as_deref().unwrap_or_default().is_empty()
                }
                _ => true,
            }
        }),
        PseudoClass::Root => dom_node
            .parent
            .is_none_or(|parent| document.get_node(parent).node_type == NodeType::Document),
        PseudoClass::Link => {
            matches!(dom_node.tag.as_str().as_str(), "a" | "area")
                && dom_node
                    .attributes
                    .iter()
                    .any(|(name, _)| name.as_str().eq_ignore_ascii_case("href"))
        }
        PseudoClass::Visited => false,
        PseudoClass::Target => target_id.is_some_and(|target_id| {
            dom_node
                .attributes
                .iter()
                .any(|(name, value)| name.as_str().eq_ignore_ascii_case("id") && value == target_id)
        }),
        PseudoClass::Enabled => {
            is_disableable_element(dom_node.tag.as_str().as_str())
                && !has_attribute(document, node, "disabled")
        }
        PseudoClass::Disabled => {
            is_disableable_element(dom_node.tag.as_str().as_str())
                && has_attribute(document, node, "disabled")
        }
        PseudoClass::Checked => {
            (dom_node.tag.as_str() == "input" && has_attribute(document, node, "checked"))
                || (dom_node.tag.as_str() == "option" && has_attribute(document, node, "selected"))
        }
        PseudoClass::Dir(expected) => node_direction(document, node) == *expected,
        PseudoClass::Lang(expected) => node_language(document, node).is_some_and(|language| {
            language.eq_ignore_ascii_case(expected)
                || language
                    .get(expected.len()..)
                    .is_some_and(|suffix| suffix.starts_with('-'))
                    && language
                        .get(..expected.len())
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(expected))
        }),
        PseudoClass::Has(selector) => {
            relative_selector_matches(document, node, selector, target_id)
        }
        PseudoClass::Not(selector) => {
            selector_matches_node_with_target(selector, document, node, target_id)
                .is_ok_and(|matched| !matched)
        }
        PseudoClass::NthChild(expression) => {
            matches_nth_position(&element_siblings(), node, expression, false)
        }
        PseudoClass::NthLastChild(expression) => {
            matches_nth_position(&element_siblings(), node, expression, true)
        }
        PseudoClass::NthOfType(expression) => {
            matches_nth_position(&type_siblings(), node, expression, false)
        }
        PseudoClass::NthLastOfType(expression) => {
            matches_nth_position(&type_siblings(), node, expression, true)
        }
    }
}

fn relative_selector_matches(
    document: &Document,
    anchor: NodeId,
    selector: &str,
    target_id: Option<&str>,
) -> bool {
    split_selector_group(selector).into_iter().any(|relative| {
        let relative = trim_css_whitespace(&relative);
        let (combinator, selector) = relative
            .chars()
            .next()
            .filter(|character| matches!(character, '>' | '+' | '~'))
            .map_or((None, relative), |character| {
                (
                    Some(character),
                    trim_css_whitespace(&relative[character.len_utf8()..]),
                )
            });
        if selector.is_empty() {
            return false;
        }
        let matches = |candidate| {
            document.get_node(candidate).node_type == NodeType::Element
                && selector_matches_node_with_target(selector, document, candidate, target_id)
                    .unwrap_or(false)
        };
        match combinator {
            Some('>') => document.children_ids(anchor).into_iter().any(matches),
            Some('+') => next_element_sibling(document, anchor).is_some_and(matches),
            Some('~') => following_element_siblings(document, anchor)
                .into_iter()
                .any(matches),
            None => {
                let mut pending = document.children_ids(anchor);
                while let Some(candidate) = pending.pop() {
                    if matches(candidate) {
                        return true;
                    }
                    pending.extend(document.children_ids(candidate));
                }
                false
            }
            Some(_) => false,
        }
    })
}

fn next_element_sibling(document: &Document, node: NodeId) -> Option<NodeId> {
    following_element_siblings(document, node)
        .into_iter()
        .next()
}

fn following_element_siblings(document: &Document, node: NodeId) -> Vec<NodeId> {
    let Some(parent) = document.get_node(node).parent else {
        return Vec::new();
    };
    document
        .children_ids(parent)
        .into_iter()
        .skip_while(|candidate| *candidate != node)
        .skip(1)
        .filter(|candidate| document.get_node(*candidate).node_type == NodeType::Element)
        .collect()
}

fn is_disableable_element(tag: &str) -> bool {
    matches!(
        tag,
        "button" | "fieldset" | "input" | "optgroup" | "option" | "select" | "textarea"
    )
}

fn has_attribute(document: &Document, node: NodeId, expected: &str) -> bool {
    document
        .get_node(node)
        .attributes
        .iter()
        .any(|(name, _)| name.as_str().eq_ignore_ascii_case(expected))
}

fn node_language(document: &Document, mut node: NodeId) -> Option<String> {
    loop {
        let current = document.get_node(node);
        if let Some((_, language)) = current.attributes.iter().find(|(name, _)| {
            matches!(
                name.as_str().to_ascii_lowercase().as_str(),
                "lang" | "xml:lang"
            )
        }) {
            return Some(language.clone());
        }
        node = current.parent?;
    }
}

fn node_direction(document: &Document, mut node: NodeId) -> String {
    loop {
        let current = document.get_node(node);
        if let Some((_, direction)) = current
            .attributes
            .iter()
            .find(|(name, _)| name.as_str().eq_ignore_ascii_case("dir"))
        {
            let direction = direction.trim().to_ascii_lowercase();
            if matches!(direction.as_str(), "ltr" | "rtl") {
                return direction;
            }
        }
        let Some(parent) = current.parent else {
            return "ltr".to_string();
        };
        node = parent;
    }
}

fn matches_nth_position(
    siblings: &[NodeId],
    node: NodeId,
    expression: &str,
    from_end: bool,
) -> bool {
    let Some(index) = siblings.iter().position(|sibling| *sibling == node) else {
        return false;
    };
    let position = if from_end {
        siblings.len() - index
    } else {
        index + 1
    } as i64;
    let Some((a, b)) = parse_an_plus_b(expression) else {
        return false;
    };
    if a == 0 {
        return position == b;
    }
    let delta = position - b;
    delta % a == 0 && delta / a >= 0
}

fn parse_an_plus_b(expression: &str) -> Option<(i64, i64)> {
    let expression = expression
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    match expression.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    let Some(n) = expression.find('n') else {
        return expression.parse().ok().map(|value| (0, value));
    };
    let a = match &expression[..n] {
        "" | "+" => 1,
        "-" => -1,
        value => value.parse().ok()?,
    };
    let b = match &expression[n + 1..] {
        "" => 0,
        value => value.parse().ok()?,
    };
    Some((a, b))
}

fn previous_element_sibling(document: &Document, node: NodeId) -> Option<NodeId> {
    let mut sibling = document.get_node(node).prev_sibling;
    while let Some(candidate) = sibling {
        if document.get_node(candidate).node_type == NodeType::Element {
            return Some(candidate);
        }
        sibling = document.get_node(candidate).prev_sibling;
    }
    None
}

fn selector_context_for_node(document: &Document, id: NodeId) -> SelectorContext {
    let node = document.get_node(id);
    let id_attribute = node
        .attributes
        .iter()
        .find(|(name, _)| name.as_str() == "id")
        .map(|(_, value)| value.as_str());
    let classes = node
        .class_list
        .iter()
        .map(|class| class.as_str().to_string())
        .collect::<Vec<_>>();
    let class_refs = classes.iter().map(String::as_str).collect::<Vec<_>>();
    let attributes = node
        .attributes
        .iter()
        .map(|(name, value)| (name.as_str().to_string(), value.as_str()))
        .collect::<Vec<_>>();
    let attribute_refs = attributes
        .iter()
        .map(|(name, value)| (name.as_str(), *value))
        .collect::<Vec<_>>();
    let is_first_child = node.parent.is_some_and(|parent| {
        document
            .children_ids(parent)
            .into_iter()
            .find(|sibling| document.get_node(*sibling).node_type == NodeType::Element)
            == Some(id)
    });
    let is_root = node
        .parent
        .is_none_or(|parent| document.get_node(parent).node_type == NodeType::Document);
    SelectorContext::new(&node.tag.as_str(), id_attribute, &class_refs)
        .with_attributes(&attribute_refs)
        .with_tree_state(is_first_child, is_root)
        .with_html_document(document.is_html_document())
        .with_html_element(document.is_html_document() && node.is_html_element)
}

fn rule_matches(rule: &Rule, ctx: &SelectorContext, ancestors: &[SelectorContext]) -> bool {
    selector_chain_matches_context(&rule.chain, &rule.combinators, ctx, ancestors)
}

fn selector_chain_matches_context(
    chain: &[CompoundSelector],
    combinators: &[Combinator],
    ctx: &SelectorContext,
    ancestors: &[SelectorContext],
) -> bool {
    let Some(subject) = chain.last() else {
        return false;
    };
    if !subject.matches(ctx) {
        return false;
    }
    // Walk the rest of the chain right-to-left against the ancestor chain
    // (ancestors is ordered root..parent, nearest parent last).
    let mut current = ctx;
    let mut cursor = ancestors.len(); // next ancestor index to consider (exclusive)
    for i in (0..chain.len().saturating_sub(1)).rev() {
        let compound = &chain[i];
        let combinator = combinators[i];
        match combinator {
            Combinator::Child => {
                if cursor == 0 {
                    return false;
                }
                let parent = &ancestors[cursor - 1];
                if !compound_matches_ctx(compound, parent) {
                    return false;
                }
                current = parent;
                cursor -= 1;
            }
            Combinator::Descendant => {
                let mut found = false;
                while cursor > 0 {
                    cursor -= 1;
                    if compound_matches_ctx(compound, &ancestors[cursor]) {
                        current = &ancestors[cursor];
                        found = true;
                        break;
                    }
                }
                if !found {
                    return false;
                }
            }
            Combinator::AdjacentSibling => {
                let Some(sibling) = current.previous_siblings.last() else {
                    return false;
                };
                if !compound_matches_ctx(compound, sibling) {
                    return false;
                }
                current = sibling;
            }
            Combinator::GeneralSibling => {
                let Some(sibling) = current
                    .previous_siblings
                    .iter()
                    .rev()
                    .find(|sibling| compound_matches_ctx(compound, sibling))
                else {
                    return false;
                };
                current = sibling;
            }
        }
    }
    true
}

fn compound_matches_ctx(compound: &CompoundSelector, ctx: &SelectorContext) -> bool {
    compound.matches(ctx)
}

/// CSS whitespace is intentionally narrower than Unicode whitespace. In
/// particular NBSP and EM SPACE are valid identifier code points and must not
/// be removed from selectors.
pub fn trim_css_whitespace(value: &str) -> &str {
    value.trim_matches(|character| matches!(character, ' ' | '\t' | '\n' | '\r' | '\u{000c}'))
}

/// Split a selector group on top-level commas (paren/bracket aware).
fn split_selector_group(selector: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    for ch in selector.chars() {
        match ch {
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => bracket += 1,
            ']' => bracket -= 1,
            ',' if paren == 0 && bracket == 0 => {
                let trimmed = trim_css_whitespace(&current);
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_string());
                }
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(ch);
    }
    let trimmed = trim_css_whitespace(&current);
    if !trimmed.is_empty() {
        parts.push(trimmed.to_string());
    }
    parts
}

fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch >= '\u{80}'
}

fn consume_css_escape(chars: &[char], pos: usize) -> Option<(char, usize)> {
    if chars.get(pos) != Some(&'\\') {
        return None;
    }
    let Some(next) = chars.get(pos + 1).copied() else {
        return Some(('\u{fffd}', pos + 1));
    };
    if matches!(next, '\n' | '\r' | '\u{c}') {
        return None;
    }
    if next.is_ascii_hexdigit() {
        let mut end = pos + 1;
        let mut digits = String::new();
        while end < chars.len() && digits.len() < 6 && chars[end].is_ascii_hexdigit() {
            digits.push(chars[end]);
            end += 1;
        }
        if end < chars.len() && chars[end].is_whitespace() {
            let whitespace = chars[end];
            end += 1;
            if whitespace == '\r' && chars.get(end) == Some(&'\n') {
                end += 1;
            }
        }
        let codepoint = u32::from_str_radix(&digits, 16).ok()?;
        let value = if codepoint == 0 {
            '\u{fffd}'
        } else {
            char::from_u32(codepoint).unwrap_or('\u{fffd}')
        };
        Some((value, end))
    } else {
        Some((next, pos + 2))
    }
}

fn parse_css_identifier(chars: &[char], mut pos: usize) -> Option<(String, usize)> {
    let mut value = String::new();
    match *chars.get(pos)? {
        '\\' => {
            let (escaped, next) = consume_css_escape(chars, pos)?;
            value.push(escaped);
            pos = next;
        }
        '-' => {
            let next = *chars.get(pos + 1)?;
            if next.is_ascii_digit() {
                return None;
            }
            value.push('-');
            pos += 1;
        }
        first
            if first == '_'
                || first.is_ascii_alphabetic()
                || first >= '\u{80}'
                || first == '\0' =>
        {
            value.push(if first == '\0' { '\u{fffd}' } else { first });
            pos += 1;
        }
        _ => return None,
    }

    while pos < chars.len() {
        if is_ident_char(chars[pos]) || chars[pos] == '\0' {
            value.push(if chars[pos] == '\0' {
                '\u{fffd}'
            } else {
                chars[pos]
            });
            pos += 1;
        } else if chars[pos] == '\\' {
            let (escaped, next) = consume_css_escape(chars, pos)?;
            value.push(escaped);
            pos = next;
        } else {
            break;
        }
    }
    Some((value, pos))
}

fn parse_complete_css_identifier(value: &str) -> Option<String> {
    let chars = value.chars().collect::<Vec<_>>();
    let (identifier, end) = parse_css_identifier(&chars, 0)?;
    (end == chars.len()).then_some(identifier)
}

/// Parse a complete `#id` selector through the same CSS identifier consumer
/// used by the authored selector engine. Callers can then use indexed DOM id
/// lookup without maintaining a second escape implementation.
pub fn parse_simple_id_selector(selector: &str) -> Option<String> {
    parse_complete_css_identifier(selector.strip_prefix('#')?)
}

pub(crate) fn css_unescape(value: &str) -> Option<String> {
    let chars = value.chars().collect::<Vec<_>>();
    let mut output = String::new();
    let mut pos = 0usize;
    while pos < chars.len() {
        if chars[pos] == '\\' {
            // A newline escaped inside a CSS string is a continuation, not a
            // character escape. CSS Syntax removes the pair entirely; CRLF
            // is one newline and therefore consumes all three characters.
            if let Some(next) = chars.get(pos + 1).copied()
                && matches!(next, '\n' | '\r' | '\u{000c}')
            {
                pos += 2;
                if next == '\r' && chars.get(pos) == Some(&'\n') {
                    pos += 1;
                }
                continue;
            }
            let (escaped, next) = consume_css_escape(&chars, pos)?;
            output.push(escaped);
            pos = next;
        } else {
            output.push(chars[pos]);
            pos += 1;
        }
    }
    Some(output)
}

fn parse_attribute_name(value: &str) -> Option<String> {
    let local_name = if let Some(local_name) = value.strip_prefix("*|") {
        return parse_complete_css_identifier(local_name).map(|name| format!("*|{name}"));
    } else if value.contains('|') {
        return None;
    } else {
        value
    };
    parse_complete_css_identifier(local_name)
}

/// Parse a selector chain into compounds + combinators. Returns `None` when
/// the selector is structurally unparseable (empty compound, sibling
/// combinator, dangling combinator) — the rule is dropped.
fn parse_selector_chain(selector: &str) -> Option<(Vec<CompoundSelector>, Vec<Combinator>)> {
    let chars: Vec<char> = selector.chars().collect();
    let mut chain = Vec::new();
    let mut combinators = Vec::new();
    let mut pos = 0usize;
    let mut pending_combinator: Option<Combinator> = None;
    let mut saw_space = false;

    while pos < chars.len() {
        let ch = chars[pos];
        if ch.is_whitespace() {
            saw_space = true;
            pos += 1;
            continue;
        }
        match ch {
            '>' => {
                if chain.len() == combinators.len() {
                    return None; // dangling combinator before any compound
                }
                if pending_combinator.is_some() {
                    return None;
                }
                pending_combinator = Some(Combinator::Child);
                saw_space = false;
                pos += 1;
                continue;
            }
            '+' => {
                if chain.len() == combinators.len() {
                    return None;
                }
                if pending_combinator.is_some() {
                    return None;
                }
                pending_combinator = Some(Combinator::AdjacentSibling);
                saw_space = false;
                pos += 1;
                continue;
            }
            '~' => {
                if chain.len() == combinators.len() {
                    return None;
                }
                if pending_combinator.is_some() {
                    return None;
                }
                pending_combinator = Some(Combinator::GeneralSibling);
                saw_space = false;
                pos += 1;
                continue;
            }
            _ => {}
        }
        // Start of a compound selector.
        if !chain.is_empty() {
            let combinator = match pending_combinator.take() {
                Some(c) => c,
                None if saw_space => Combinator::Descendant,
                None => return None, // two compounds without a combinator
            };
            combinators.push(combinator);
        }
        saw_space = false;
        let (compound, next) = parse_compound(&chars, pos)?;
        chain.push(compound);
        pos = next;
    }

    if chain.is_empty() || pending_combinator.is_some() {
        return None;
    }
    Some((chain, combinators))
}

fn parse_selector_chain_cached(
    selector: &str,
) -> Option<Rc<selector_parse_cache::ParsedSelectorChain>> {
    selector_parse_cache::get_or_parse(selector, parse_selector_chain)
}

/// Parse one compound selector starting at `pos`. Returns the compound and
/// the index just past it.
fn parse_compound(chars: &[char], mut pos: usize) -> Option<(CompoundSelector, usize)> {
    let mut compound = CompoundSelector::default();
    let mut consumed_any = false;

    // Optional leading element name or universal `*`.
    if pos < chars.len() {
        if chars[pos] == '*' && chars.get(pos + 1) == Some(&'|') && chars.get(pos + 2) == Some(&'*')
        {
            compound.any_namespace = true;
            compound.universal = true;
            consumed_any = true;
            pos += 3;
        } else if chars[pos] == '*' && chars.get(pos + 1) == Some(&'|') {
            let (tag, next) = parse_css_identifier(chars, pos + 2)?;
            compound.any_namespace = true;
            compound.tag = Some(tag);
            consumed_any = true;
            pos = next;
        } else if chars[pos] == '*' {
            compound.universal = true;
            consumed_any = true;
            pos += 1;
        } else if let Some((tag, next)) = parse_css_identifier(chars, pos) {
            compound.tag = Some(tag);
            consumed_any = true;
            pos = next;
        }
    }

    loop {
        if pos >= chars.len() {
            break;
        }
        match chars[pos] {
            '.' | '#' => {
                let is_class = chars[pos] == '.';
                pos += 1;
                let (name, next) = parse_css_identifier(chars, pos)?;
                pos = next;
                if is_class {
                    compound.classes.push(name);
                } else {
                    compound.id = Some(name);
                }
                consumed_any = true;
            }
            '[' => {
                let start = pos + 1;
                pos = start;
                while pos < chars.len() && chars[pos] != ']' {
                    pos += 1;
                }
                let expression: String = chars[start..pos].iter().collect();
                if pos < chars.len() {
                    pos += 1;
                }
                let expression = trim_css_whitespace(&expression);
                if let Some((name, operator, value)) = parse_attribute_matcher(expression) {
                    let name = parse_attribute_name(trim_css_whitespace(name))?;
                    let (raw_value, insensitive) = parse_attribute_value_modifier(value)?;
                    let quoted = (raw_value.starts_with('"') && raw_value.ends_with('"'))
                        || (raw_value.starts_with('\'') && raw_value.ends_with('\''));
                    if raw_value.is_empty() {
                        return None;
                    }
                    let value = if quoted {
                        css_unescape(&raw_value[1..raw_value.len().saturating_sub(1)])?
                    } else {
                        parse_complete_css_identifier(raw_value)?
                    };
                    let selector = match operator {
                        "=" => AttributeSelector::Equals(name, value, insensitive),
                        "~=" => AttributeSelector::Includes(name, value, insensitive),
                        "|=" => AttributeSelector::DashMatch(name, value, insensitive),
                        "^=" => AttributeSelector::Prefix(name, value, insensitive),
                        "$=" => AttributeSelector::Suffix(name, value, insensitive),
                        "*=" => AttributeSelector::Substring(name, value, insensitive),
                        _ => return None,
                    };
                    compound.attributes.push(selector);
                } else if let Some(name) = parse_attribute_name(expression) {
                    compound.attributes.push(AttributeSelector::Present(name));
                } else {
                    return None;
                }
                consumed_any = true;
            }
            ':' => {
                pos += 1;
                let pseudo_element = pos < chars.len() && chars[pos] == ':';
                if pseudo_element {
                    pos += 1;
                }
                let start = pos;
                while pos < chars.len() && is_ident_char(chars[pos]) {
                    pos += 1;
                }
                let name = chars[start..pos].iter().collect::<String>();
                // Optional parenthesized argument, e.g. `:nth-child(2n+1)`.
                let has_arguments = pos < chars.len() && chars[pos] == '(';
                let mut argument = String::new();
                if has_arguments {
                    let mut depth = 1i32;
                    pos += 1;
                    let argument_start = pos;
                    while pos < chars.len() && depth > 0 {
                        if chars[pos] == '(' {
                            depth += 1;
                        } else if chars[pos] == ')' {
                            depth -= 1;
                        }
                        pos += 1;
                    }
                    if depth == 0 {
                        argument = chars[argument_start..pos - 1].iter().collect();
                    } else {
                        argument = chars[argument_start..pos].iter().collect();
                    }
                }
                match (pseudo_element, has_arguments, name.as_str()) {
                    (false, false, "first-child") => {
                        compound.pseudo_classes.push(PseudoClass::FirstChild)
                    }
                    (false, false, "last-child") => {
                        compound.pseudo_classes.push(PseudoClass::LastChild)
                    }
                    (false, false, "first-of-type") => {
                        compound.pseudo_classes.push(PseudoClass::FirstOfType)
                    }
                    (false, false, "last-of-type") => {
                        compound.pseudo_classes.push(PseudoClass::LastOfType)
                    }
                    (false, false, "only-child") => {
                        compound.pseudo_classes.push(PseudoClass::OnlyChild)
                    }
                    (false, false, "only-of-type") => {
                        compound.pseudo_classes.push(PseudoClass::OnlyOfType)
                    }
                    (false, false, "empty") => compound.pseudo_classes.push(PseudoClass::Empty),
                    (false, false, "root") => compound.pseudo_classes.push(PseudoClass::Root),
                    (false, false, "link") => compound.pseudo_classes.push(PseudoClass::Link),
                    (false, false, "visited") => compound.pseudo_classes.push(PseudoClass::Visited),
                    (false, false, "target") => compound.pseudo_classes.push(PseudoClass::Target),
                    (false, false, "enabled") => compound.pseudo_classes.push(PseudoClass::Enabled),
                    (false, false, "disabled") => {
                        compound.pseudo_classes.push(PseudoClass::Disabled)
                    }
                    (false, false, "checked") => compound.pseudo_classes.push(PseudoClass::Checked),
                    (
                        false,
                        false,
                        "active" | "focus" | "focus-visible" | "focus-within" | "hover"
                        | "indeterminate" | "placeholder-shown",
                    ) => compound.unsupported = true,
                    (
                        true,
                        false,
                        "after" | "backdrop" | "before" | "first-letter" | "first-line" | "marker"
                        | "placeholder" | "selection",
                    ) => compound.unsupported = true,
                    (false, false, "after" | "before" | "first-letter" | "first-line") => {
                        compound.unsupported = true
                    }
                    (true, true, "slotted") if !argument.is_empty() => compound.unsupported = true,
                    (false, true, "dir") => {
                        let direction = trim_css_whitespace(&argument).to_ascii_lowercase();
                        if !matches!(direction.as_str(), "ltr" | "rtl") {
                            return None;
                        }
                        compound.pseudo_classes.push(PseudoClass::Dir(direction))
                    }
                    (false, true, "lang") => {
                        compound.pseudo_classes.push(PseudoClass::Lang(argument))
                    }
                    (false, true, "has") => {
                        let argument = trim_css_whitespace(&argument).to_string();
                        if argument.is_empty()
                            || split_selector_group(&argument).iter().any(|relative| {
                                let relative = trim_css_whitespace(relative);
                                let selector = relative
                                    .chars()
                                    .next()
                                    .filter(|character| matches!(character, '>' | '+' | '~'))
                                    .map_or(relative, |character| {
                                        trim_css_whitespace(&relative[character.len_utf8()..])
                                    });
                                selector.is_empty() || parse_selector_chain(selector).is_none()
                            })
                        {
                            return None;
                        }
                        compound.pseudo_classes.push(PseudoClass::Has(argument))
                    }
                    (false, true, "not") => {
                        let argument = trim_css_whitespace(&argument).to_string();
                        if argument.is_empty()
                            || split_selector_group(&argument)
                                .iter()
                                .any(|selector| parse_selector_chain(selector).is_none())
                        {
                            return None;
                        }
                        compound.pseudo_classes.push(PseudoClass::Not(argument))
                    }
                    (false, true, "nth-child") => compound
                        .pseudo_classes
                        .push(PseudoClass::NthChild(argument)),
                    (false, true, "nth-last-child") => compound
                        .pseudo_classes
                        .push(PseudoClass::NthLastChild(argument)),
                    (false, true, "nth-of-type") => compound
                        .pseudo_classes
                        .push(PseudoClass::NthOfType(argument)),
                    (false, true, "nth-last-of-type") => compound
                        .pseudo_classes
                        .push(PseudoClass::NthLastOfType(argument)),
                    _ => return None,
                }
                consumed_any = true;
            }
            _ => break,
        }
    }

    if !consumed_any {
        return None;
    }
    Some((compound, pos))
}

fn parse_attribute_matcher(expression: &str) -> Option<(&str, &str, &str)> {
    for operator in ["~=", "|=", "^=", "$=", "*=", "="] {
        if let Some((name, value)) = expression.split_once(operator) {
            return Some((name, operator, value));
        }
    }
    None
}

fn parse_attribute_value_modifier(value: &str) -> Option<(&str, bool)> {
    let value = trim_css_whitespace(value);
    let value_end = if let Some(quote @ ('\'' | '"')) = value.chars().next() {
        let mut escaped = false;
        value
            .char_indices()
            .skip(1)
            .find_map(|(index, character)| {
                if escaped {
                    escaped = false;
                    return None;
                }
                if character == '\\' {
                    escaped = true;
                    return None;
                }
                (character == quote).then_some(index + character.len_utf8())
            })?
    } else {
        value
            .char_indices()
            .find_map(|(index, character)| {
                matches!(character, ' ' | '\t' | '\n' | '\r' | '\u{000c}').then_some(index)
            })
            .unwrap_or(value.len())
    };
    let raw_value = &value[..value_end];
    let modifier = trim_css_whitespace(&value[value_end..]);
    let insensitive = match modifier {
        "" | "s" | "S" => false,
        "i" | "I" => true,
        _ => return None,
    };
    Some((raw_value, insensitive))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(tag: &str, id: Option<&str>, classes: &[&str]) -> SelectorContext {
        SelectorContext::new(tag, id, classes)
    }

    fn setup() {
        clear_rules();
    }

    #[test]
    fn matches_tag_class_id_universal() {
        setup();
        register_rule("div", &[("color", "red")]);
        register_rule(".item", &[("width", "10px")]);
        register_rule("#main", &[("height", "20px")]);
        register_rule("*", &[("gap", "1px")]);

        let matched = matching_declarations("div", Some("main"), &["item"], &[]);
        let props: Vec<&str> = matched.iter().map(|(p, _, _)| p.as_str()).collect();
        assert!(props.contains(&"color"));
        assert!(props.contains(&"width"));
        assert!(props.contains(&"height"));
        assert!(props.contains(&"gap"));

        let none = matching_declarations("span", None, &[], &[]);
        assert!(none.iter().all(|(p, _, _)| p == "gap"));
    }

    #[test]
    fn candidate_index_skips_unrelated_subject_buckets() {
        setup();
        for index in 0..128 {
            register_rule(&format!(".unrelated-{index}"), &[("unused", "true")]);
        }
        register_rule("#target", &[("id-hit", "true")]);
        register_rule(".active", &[("class-hit", "true")]);
        register_rule("[data-state]", &[("attribute-hit", "true")]);
        register_rule("button", &[("tag-hit", "true")]);
        register_rule("*", &[("universal-hit", "true")]);

        let element = SelectorContext::new("button", Some("target"), &["active"])
            .with_attributes(&[("data-state", "ready")]);
        assert_eq!(rule_count(), 133);
        assert_eq!(candidate_rule_count_for_context(&element), 5);
        assert_eq!(matching_declarations_for_context(&element, &[]).len(), 5);
    }

    #[test]
    fn ancestor_filter_skips_same_subject_rules_before_chain_matching() {
        setup();
        for _ in 0..128 {
            register_rule(".missing .target", &[("unused", "true")]);
        }
        register_rule(".present .target", &[("matched", "true")]);

        let element = SelectorContext::new("span", None, &["target"]);
        let ancestors = [SelectorContext::new("div", None, &["present"])];
        assert_eq!(candidate_rule_count_for_context(&element), 129);
        assert_eq!(
            ancestor_filtered_candidate_rule_count_for_context(&element, &ancestors),
            1
        );
        assert_eq!(
            matching_declarations_for_context(&element, &ancestors).len(),
            1
        );
    }

    #[test]
    fn selector_parse_cache_reuses_valid_and_invalid_results() {
        selector_parse_cache::clear_for_test();
        let target = SelectorContext::new("span", None, &["target"]);

        assert_eq!(selector_matches_context(".target", &target, &[]), Ok(true));
        assert_eq!(selector_matches_context(".target", &target, &[]), Ok(true));
        assert_eq!(selector_matches_context("[data-x=", &target, &[]), Err(()));
        assert_eq!(selector_matches_context("[data-x=", &target, &[]), Err(()));

        assert_eq!(selector_parse_cache::parse_count_for_test(), 2);
    }

    #[test]
    fn selector_parse_cache_is_bounded_and_evicts_the_least_recently_used_entry() {
        selector_parse_cache::clear_for_test();
        let target = SelectorContext::new("span", None, &[]);
        for index in 0..selector_parse_cache::SELECTOR_PARSE_CACHE_CAPACITY {
            let selector = format!(".item-{index}");
            assert_eq!(selector_matches_context(&selector, &target, &[]), Ok(false));
        }

        assert_eq!(selector_matches_context(".item-0", &target, &[]), Ok(false));
        assert_eq!(
            selector_matches_context(".overflow", &target, &[]),
            Ok(false)
        );
        assert_eq!(selector_matches_context(".item-1", &target, &[]), Ok(false));
        assert_eq!(
            selector_parse_cache::parse_count_for_test(),
            selector_parse_cache::SELECTOR_PARSE_CACHE_CAPACITY + 2
        );
    }

    #[test]
    fn invalidation_dependencies_track_active_rules_and_owner_cleanup() {
        setup();
        assert!(!has_sibling_dependencies());
        assert!(!has_relational_dependencies());

        register_rule_for_owner(7, ".a + .b", &[("color", "red")]);
        assert!(has_sibling_dependencies());
        assert!(!has_relational_dependencies());

        register_rule_for_owner(8, ".host:not(:has(.flag))", &[("color", "blue")]);
        assert!(has_relational_dependencies());

        clear_owner(8);
        assert!(has_sibling_dependencies());
        assert!(!has_relational_dependencies());
        clear_owner(7);
        assert!(!has_sibling_dependencies());
    }

    #[test]
    fn any_namespace_type_selector_uses_the_local_tag_bucket() {
        setup();
        register_rule("*|rect", &[("fill", "red")]);
        let element = SelectorContext::new("svg:rect", None, &[]).with_html_element(false);
        assert_eq!(candidate_rule_count_for_context(&element), 1);
        assert_eq!(matching_declarations_for_context(&element, &[]).len(), 1);
    }

    #[test]
    fn compiled_selector_bytecode_preserves_the_dynamic_parser_ir() {
        for selector in [
            "section#main.card[data-role='USER' i] > span.item:first-child::before",
            "[*|href][class~=active][lang|=en][data-id^=pre][data-id$=post][data-id*=middle]",
            ":last-child:last-of-type:first-of-type:only-child:only-of-type:empty:root",
            "a:link:visited:target:enabled:disabled:checked:dir(rtl):lang(en)",
            "div:has(> button):not(.hidden):nth-child(2n+1):nth-last-child(2):nth-of-type(odd):nth-last-of-type(even)",
        ] {
            let parsed = parse_selector_list(selector).expect("dynamic parser accepts selector");
            let bytecodes =
                compile_selector_bytecode(selector).expect("AOT compiler accepts selector");
            assert_eq!(parsed.len(), bytecodes.len());
            for (expected, bytecode) in parsed.iter().zip(bytecodes) {
                let decoded = selector_bytecode::decode(&bytecode).expect("bytecode decodes");
                assert_eq!(format!("{expected:?}"), format!("{decoded:?}"));
            }
        }
    }

    #[test]
    fn compiled_rules_share_registration_and_fail_closed_on_invalid_bytecode() {
        setup();
        let bytecodes = compile_selector_bytecode(".active, #target").unwrap();
        for bytecode in bytecodes {
            register_compiled_rule(&bytecode, &[("color", "red")]);
        }
        register_compiled_rule(&[255, 0, 0], &[("invalid", "true")]);

        assert_eq!(rule_count(), 2);
        assert_eq!(
            matching_declarations("div", None, &["active"], &[]).len(),
            1
        );
        assert_eq!(
            matching_declarations("div", Some("target"), &[], &[]).len(),
            1
        );
    }

    #[test]
    fn matches_compound() {
        setup();
        register_rule("div.item.active", &[("color", "red")]);
        register_rule("span.item", &[("color", "blue")]);

        let hit = matching_declarations("div", None, &["item", "active"], &[]);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].1, "red");

        let miss = matching_declarations("div", None, &["item"], &[]);
        assert!(miss.is_empty());
    }

    #[test]
    fn matches_descendant_and_child() {
        setup();
        register_rule(".monaco-editor .find-widget", &[("position", "absolute")]);
        register_rule(".outer > .inner", &[("color", "red")]);

        let ancestors = vec![ctx("body", None, &[]), ctx("div", None, &["monaco-editor"])];
        let hit = matching_declarations("div", None, &["find-widget"], &ancestors);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].1, "absolute");

        // Grandparent .outer with parent div: descendant `> ` must fail...
        let ancestors = vec![ctx("div", None, &["outer"]), ctx("div", None, &[])];
        let miss = matching_declarations("span", None, &["inner"], &ancestors);
        assert!(miss.is_empty());
        // ...but with the direct parent being .outer it matches.
        let ancestors = vec![ctx("div", None, &["outer"])];
        let hit = matching_declarations("span", None, &["inner"], &ancestors);
        assert_eq!(hit.len(), 1);
    }

    #[test]
    fn descendant_chain_skips_levels() {
        setup();
        register_rule(".a .b .c", &[("color", "red")]);
        let ancestors = vec![
            ctx("div", None, &["a"]),
            ctx("div", None, &["x"]),
            ctx("div", None, &["b"]),
        ];
        let hit = matching_declarations("span", None, &["c"], &ancestors);
        assert_eq!(hit.len(), 1);
    }

    #[test]
    fn specificity_then_registration_order() {
        setup();
        // Registered in this order on purpose: class first, then id, then tag.
        register_rule(".item", &[("color", "class")]);
        register_rule("#main", &[("color", "id")]);
        register_rule("div", &[("color", "tag")]);
        register_rule(".item", &[("width", "first")]);
        register_rule(".item", &[("width", "second")]);

        let matched = matching_declarations("div", Some("main"), &["item"], &[]);
        let applied: Vec<(&str, &str)> = matched
            .iter()
            .map(|(p, v, _)| (p.as_str(), v.as_str()))
            .collect();
        // tag < class < id specificity; same-specificity keeps registration order.
        assert_eq!(
            applied,
            [
                ("color", "tag"),
                ("color", "class"),
                ("width", "first"),
                ("width", "second"),
                ("color", "id")
            ]
        );
    }

    #[test]
    fn structural_pseudo_classes_match_but_dynamic_and_pseudo_elements_do_not() {
        setup();
        register_rule(".btn:hover", &[("color", "red")]);
        register_rule("li:first-child", &[("color", "blue")]);
        register_rule(":root", &[("display", "block")]);
        register_rule("a::before", &[("color", "green")]);

        assert!(matching_declarations("button", None, &["btn"], &[]).is_empty());
        assert!(matching_declarations("li", None, &[], &[]).is_empty());
        let first = SelectorContext::new("li", None, &[]).with_tree_state(true, false);
        assert_eq!(matching_declarations_for_context(&first, &[]).len(), 1);
        let root = SelectorContext::new("html", None, &[]).with_tree_state(true, true);
        assert_eq!(matching_declarations_for_context(&root, &[]).len(), 1);
        assert!(matching_declarations("a", None, &[], &[]).is_empty());
        assert_eq!(rule_count(), 4);
    }

    #[test]
    fn attribute_presence_and_equality_selectors_match_runtime_attributes() {
        setup();
        register_rule("button[disabled]", &[("opacity", "0.5")]);
        register_rule("[data-role='user']", &[("color", "white")]);

        let disabled =
            SelectorContext::new("button", None, &[]).with_attributes(&[("disabled", "")]);
        let user =
            SelectorContext::new("article", None, &[]).with_attributes(&[("data-role", "user")]);
        let assistant = SelectorContext::new("article", None, &[])
            .with_attributes(&[("data-role", "assistant")]);

        assert!(rule_matches(
            &RULES.with(|rules| rules.borrow().rules[0].clone()),
            &disabled,
            &[]
        ));
        assert!(rule_matches(
            &RULES.with(|rules| rules.borrow().rules[1].clone()),
            &user,
            &[]
        ));
        assert!(!rule_matches(
            &RULES.with(|rules| rules.borrow().rules[1].clone()),
            &assistant,
            &[]
        ));
    }

    #[test]
    fn attribute_match_operators_and_html_attribute_name_case_work() {
        setup();
        register_rule("[CLASS]", &[("present", "yes")]);
        register_rule("[class~=active]", &[("token", "yes")]);
        register_rule("[lang|=en]", &[("language", "yes")]);
        register_rule("[data-id^=pre]", &[("prefix", "yes")]);
        register_rule("[data-id$=post]", &[("suffix", "yes")]);
        register_rule("[data-id*=middle]", &[("substring", "yes")]);
        register_rule("[name*=user i]", &[("insensitive", "yes")]);
        let element = SelectorContext::new("div", None, &["button", "active"]).with_attributes(&[
            ("class", "button active"),
            ("lang", "en-GB"),
            ("data-id", "pre-middle-post"),
            ("name", "User"),
        ]);
        assert_eq!(matching_declarations_for_context(&element, &[]).len(), 7);
    }

    #[test]
    fn foreign_element_type_and_attribute_names_are_case_sensitive() {
        setup();
        register_rule("lineargradient[viewbox]", &[("wrong-case", "yes")]);
        register_rule("linearGradient[viewBox]", &[("exact-case", "yes")]);
        let element = SelectorContext::new("linearGradient", None, &[])
            .with_attributes(&[("viewBox", "0 0 10 10")])
            .with_html_element(false);
        let declarations = matching_declarations_for_context(&element, &[]);
        assert_eq!(declarations.len(), 1);
        assert_eq!(
            (declarations[0].0.as_str(), declarations[0].1.as_str()),
            ("exact-case", "yes")
        );
    }

    #[test]
    fn any_namespace_attribute_selector_matches_by_local_name() {
        setup();
        register_rule("[*|href]", &[("present", "yes")]);
        register_rule("[*|href=foo]", &[("equals", "yes")]);
        let element = SelectorContext::new("svg", None, &[])
            .with_attributes(&[("xlink:href", "foo")])
            .with_html_element(false);
        assert_eq!(matching_declarations_for_context(&element, &[]).len(), 2);
    }

    #[test]
    fn selector_api_only_pseudo_elements_are_valid_but_never_match_elements() {
        setup();
        for selector in [
            ":not(*|*)",
            "div:first-line",
            "::slotted(foo)",
            "::slotted(foo",
        ] {
            register_rule(selector, &[("matched", "no")]);
        }
        let element = SelectorContext::new("div", None, &[]);
        assert!(matching_declarations_for_context(&element, &[]).is_empty());
        assert_eq!(rule_count(), 4);
    }

    #[test]
    fn comma_groups_split_into_rules() {
        setup();
        register_rule(".a, div.b , #c", &[("color", "red")]);
        assert_eq!(rule_count(), 3);
        assert_eq!(matching_declarations("span", None, &["a"], &[]).len(), 1);
        assert_eq!(matching_declarations("div", None, &["b"], &[]).len(), 1);
        assert_eq!(matching_declarations("p", Some("c"), &[], &[]).len(), 1);
    }

    #[test]
    fn sibling_combinators_match_preceding_element_contexts() {
        setup();
        register_rule(".a + .b", &[("color", "red")]);
        register_rule(".a ~ .c", &[("color", "blue")]);
        let a = ctx("div", None, &["a"]);
        let b = ctx("div", None, &["b"]).with_previous_siblings(vec![a.clone()]);
        let c = ctx("div", None, &["c"]).with_previous_siblings(vec![a, b.clone()]);
        assert_eq!(matching_declarations_for_context(&b, &[])[0].1, "red");
        assert_eq!(matching_declarations_for_context(&c, &[])[0].1, "blue");
        assert_eq!(rule_count(), 2);
    }

    #[test]
    fn ancestor_filter_stops_at_sibling_edges_without_false_negatives() {
        setup();
        register_rule(".a + .b > .target", &[("color", "red")]);
        let a = ctx("div", None, &["a"]);
        let parent = ctx("div", None, &["b"]).with_previous_siblings(vec![a]);
        let target = ctx("span", None, &["target"]);

        assert_eq!(
            ancestor_filtered_candidate_rule_count_for_context(&target, &[parent.clone()]),
            1
        );
        assert_eq!(
            matching_declarations_for_context(&target, &[parent])[0].1,
            "red"
        );
    }

    #[test]
    fn clear_empties_registry() {
        setup();
        register_rule(".a", &[("color", "red")]);
        assert!(has_rules());
        clear_rules();
        assert!(!has_rules());
        assert_eq!(rule_count(), 0);
    }

    #[test]
    fn invalid_selector_member_discards_complete_group() {
        setup();
        register_rule("[1digit], div", &[("color", "red")]);
        register_rule("[title~=], p.valid", &[("color", "red")]);
        assert!(matching_declarations("div", None, &[], &[]).is_empty());
        assert!(matching_declarations("p", None, &["valid"], &[]).is_empty());
    }

    #[test]
    fn quoted_empty_token_selector_is_valid_and_never_matches() {
        setup();
        register_rule("[title~=\"\"], p.valid", &[("color", "green")]);
        let empty = SelectorContext::new("p", None, &[]).with_attributes(&[("title", "")]);
        assert!(matching_declarations_for_context(&empty, &[]).is_empty());
        assert_eq!(
            matching_declarations("p", None, &["valid"], &[])[0].1,
            "green"
        );
    }

    #[test]
    fn simple_id_selector_consumes_css_escapes_and_replacement_characters() {
        assert_eq!(
            parse_simple_id_selector(r"#\30 next"),
            Some("0next".to_string())
        );
        assert_eq!(
            parse_simple_id_selector("#spac\\65\r\ns"),
            Some("spaces".to_string())
        );
        assert_eq!(
            parse_simple_id_selector("#eof\\"),
            Some("eof\u{fffd}".to_string())
        );
        assert_eq!(
            parse_simple_id_selector("#ab\0c"),
            Some("ab\u{fffd}c".to_string())
        );
        assert_eq!(
            parse_simple_id_selector("#\u{a0}"),
            Some("\u{a0}".to_string())
        );
        assert_eq!(trim_css_whitespace(" \t#\u{2003}\r\n"), "#\u{2003}");
    }

    #[test]
    fn css_string_unescape_removes_escaped_line_continuations() {
        assert_eq!(css_unescape("left\\\nright"), Some("leftright".to_string()));
        assert_eq!(css_unescape("left\\\rright"), Some("leftright".to_string()));
        assert_eq!(
            css_unescape("left\\\r\nright"),
            Some("leftright".to_string())
        );
        assert_eq!(
            css_unescape("left\\\u{000c}right"),
            Some("leftright".to_string())
        );
    }

    #[test]
    fn html_lang_attribute_values_are_ascii_case_insensitive_only_in_html() {
        setup();
        register_rule("[lang|=es]", &[("color", "green")]);
        let html = SelectorContext::new("div", None, &[])
            .with_attributes(&[("lang", "ES-mx")])
            .with_html_document(true);
        let xhtml = html.clone().with_html_document(false);
        assert_eq!(matching_declarations_for_context(&html, &[]).len(), 1);
        assert!(matching_declarations_for_context(&xhtml, &[]).is_empty());
    }

    #[test]
    fn dir_pseudo_class_follows_the_current_ancestor_chain() {
        let mut document = Document::new();
        let source = document.create_element("div");
        source.set_attribute(&mut document, "dir", "ltr");
        let target = document.create_element("span");
        document.append_child(source.id, target.id);
        document.append_child(document.body().id, source.id);
        assert_eq!(document.matches_selector(target.id, ":dir(ltr)"), Ok(true));
        assert_eq!(document.matches_selector(target.id, ":dir(rtl)"), Ok(false));

        let destination = document.create_element("div");
        destination.set_attribute(&mut document, "dir", "rtl");
        document.append_child(document.body().id, destination.id);
        document.append_child(destination.id, target.id);
        assert_eq!(document.matches_selector(target.id, ":dir(ltr)"), Ok(false));
        assert_eq!(document.matches_selector(target.id, ":dir(rtl)"), Ok(true));
    }

    #[test]
    fn has_pseudo_class_tracks_descendants_in_selector_and_cascade_matching() {
        setup();
        let mut document = Document::new();
        let parent = document.create_element("div");
        parent.set_attribute(&mut document, "id", "parent");
        document.append_child(document.body().id, parent.id);
        let button = document.create_element("button");
        register_rule("#parent:has(button)", &[("display", "none")]);

        assert_eq!(
            document.matches_selector(parent.id, ":has(button)"),
            Ok(false)
        );
        assert_ne!(
            document.computed_style_for(parent.id).display,
            w3cos_std::style::Display::None
        );

        document.append_child(parent.id, button.id);

        assert_eq!(
            document.matches_selector(parent.id, ":has(button)"),
            Ok(true)
        );
        assert_eq!(
            document.computed_style_for(parent.id).display,
            w3cos_std::style::Display::None
        );
    }

    #[test]
    fn pseudo_element_rules_use_an_isolated_originating_element_cascade() {
        setup();
        let mut document = Document::new();
        let item = document.create_element("div");
        item.set_attribute(&mut document, "id", "item");
        document.append_child(document.body().id, item.id);
        register_rule("#item", &[("left", "10px")]);
        register_rule(
            "#item::before",
            &[("left", "20px"), ("transition", "left 1s")],
        );
        register_rule("#item:after", &[("left", "30px")]);
        register_rule("#item.big::before", &[("left", "40px")]);

        assert_eq!(
            document.computed_style_for(item.id).left,
            w3cos_std::style::Dimension::Px(10.0)
        );
        assert_eq!(
            document.computed_pseudo_style_for(item.id, "::before").left,
            w3cos_std::style::Dimension::Px(20.0)
        );
        assert_eq!(
            document.computed_pseudo_style_for(item.id, "::after").left,
            w3cos_std::style::Dimension::Px(30.0)
        );
        item.set_attribute(&mut document, "class", "big");
        assert_eq!(
            document.computed_pseudo_style_for(item.id, "::before").left,
            w3cos_std::style::Dimension::Px(40.0)
        );
    }

    #[test]
    fn clearing_page_owner_preserves_native_and_other_page_rules() {
        setup();
        register_rule(".native", &[("color", "native")]);
        register_rule_for_owner(7, ".page-a", &[("color", "page-a")]);
        register_rule_for_owner(8, ".page-b", &[("color", "page-b")]);

        clear_owner(7);

        assert_eq!(rule_count(), 2);
        assert_eq!(
            matching_declarations("div", None, &["native"], &[])[0].1,
            "native"
        );
        assert!(matching_declarations("div", None, &["page-a"], &[]).is_empty());
        assert_eq!(
            matching_declarations("div", None, &["page-b"], &[])[0].1,
            "page-b"
        );
    }
}
