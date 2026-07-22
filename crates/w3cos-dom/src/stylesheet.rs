//! Runtime stylesheet registry + selector matcher.
//!
//! CSS imported by ESM modules (`import "./x.css"`) is parsed at compile time
//! by `w3cos-compiler` and baked into the generated bundle as a sequence of
//! [`register_rule`] calls. [`Document::to_component_tree`](crate::Document)
//! then applies matching rules *before* inline styles (inline wins).
//!
//! Supported selectors (v1):
//! - `*`, `tag`, `.class`, `#id`
//! - compound `tag.a.b` / `tag.a#id`
//! - descendant `A B` and child `A > B` combinators
//! - comma groups (split into separate rules at registration)
//!
//! Selectors containing pseudo-classes (`:hover`, `:first-child`, ...),
//! pseudo-elements (`::before`), attribute selectors (`[disabled]`), or
//! sibling combinators (`+`, `~`) are parsed but NEVER match in v1: applying
//! state-driven rules statically would paint every element as if it were
//! hovered/focused. This is a deliberate, documented limitation.

use std::cell::RefCell;

/// Ancestor-chain entry used for descendant/child combinator matching.
#[derive(Debug, Clone, Default)]
pub struct SelectorContext {
    pub tag: String,
    pub id: Option<String>,
    pub classes: Vec<String>,
}

impl SelectorContext {
    pub fn new(tag: &str, id: Option<&str>, classes: &[&str]) -> Self {
        Self {
            tag: tag.to_string(),
            id: id.map(|s| s.to_string()),
            classes: classes.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Combinator linking a compound selector to the compound on its left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Combinator {
    /// `A B` — any ancestor.
    Descendant,
    /// `A > B` — direct parent only.
    Child,
}

/// A single compound selector (no combinators), e.g. `div.item#main`.
#[derive(Debug, Clone, Default)]
struct CompoundSelector {
    universal: bool,
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    /// Contains a pseudo-class/element, attribute selector, or anything else
    /// v1 cannot evaluate statically — the rule never matches.
    unsupported: bool,
}

impl CompoundSelector {
    fn matches(&self, ctx: &SelectorContext) -> bool {
        if self.unsupported {
            return false;
        }
        if let Some(tag) = &self.tag
            && !tag.eq_ignore_ascii_case(&ctx.tag)
        {
            return false;
        }
        if let Some(id) = &self.id
            && ctx.id.as_deref() != Some(id.as_str())
        {
            return false;
        }
        self.classes
            .iter()
            .all(|c| ctx.classes.iter().any(|have| have == c))
    }

    fn specificity(&self) -> u32 {
        let ids = u32::from(self.id.is_some());
        let classes = self.classes.len() as u32;
        let tags = u32::from(self.tag.is_some());
        ids * 1_000_000 + classes * 1_000 + tags
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
}

thread_local! {
    static RULES: RefCell<Vec<Rule>> = const { RefCell::new(Vec::new()) };
}

/// Register a stylesheet rule. Comma-separated selector groups are split into
/// independent rules. Unparseable selectors are ignored (never match).
pub fn register_rule(selector: &str, declarations: &[(&str, &str)]) {
    if declarations.is_empty() {
        return;
    }
    RULES.with(|rules| {
        let mut rules = rules.borrow_mut();
        for single in split_selector_group(selector) {
            let Some((chain, combinators)) = parse_selector_chain(&single) else {
                continue;
            };
            let specificity = chain.iter().map(CompoundSelector::specificity).sum();
            let order = rules.len() as u32;
            rules.push(Rule {
                chain,
                combinators,
                declarations: declarations
                    .iter()
                    .map(|(p, v)| (p.to_string(), v.to_string()))
                    .collect(),
                specificity,
                order,
            });
        }
    });
}

/// Remove all registered rules.
pub fn clear_rules() {
    RULES.with(|rules| rules.borrow_mut().clear());
}

/// Number of registered rules (after comma-group splitting).
pub fn rule_count() -> usize {
    RULES.with(|rules| rules.borrow().len())
}

/// Whether any rules are registered — fast path for the DOM walk.
pub fn has_rules() -> bool {
    RULES.with(|rules| !rules.borrow().is_empty())
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
    RULES.with(|rules| {
        let rules = rules.borrow();
        let mut matched: Vec<&Rule> = rules
            .iter()
            .filter(|rule| rule_matches(rule, &ctx, ancestors))
            .collect();
        matched.sort_by_key(|rule| (rule.specificity, rule.order));
        let mut out = Vec::new();
        for rule in matched {
            for (prop, value) in &rule.declarations {
                out.push((prop.clone(), value.clone(), rule.specificity));
            }
        }
        out
    })
}

fn rule_matches(rule: &Rule, ctx: &SelectorContext, ancestors: &[SelectorContext]) -> bool {
    let Some(subject) = rule.chain.last() else {
        return false;
    };
    if !subject.matches(ctx) {
        return false;
    }
    // Walk the rest of the chain right-to-left against the ancestor chain
    // (ancestors is ordered root..parent, nearest parent last).
    let mut cursor = ancestors.len(); // next ancestor index to consider (exclusive)
    for i in (0..rule.chain.len().saturating_sub(1)).rev() {
        let compound = &rule.chain[i];
        let combinator = rule.combinators[i];
        match combinator {
            Combinator::Child => {
                if cursor == 0 {
                    return false;
                }
                let parent = &ancestors[cursor - 1];
                if !compound_matches_ctx(compound, parent) {
                    return false;
                }
                cursor -= 1;
            }
            Combinator::Descendant => {
                let mut found = false;
                while cursor > 0 {
                    cursor -= 1;
                    if compound_matches_ctx(compound, &ancestors[cursor]) {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return false;
                }
            }
        }
    }
    true
}

fn compound_matches_ctx(compound: &CompoundSelector, ctx: &SelectorContext) -> bool {
    compound.matches(ctx)
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
                let trimmed = current.trim();
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
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        parts.push(trimmed.to_string());
    }
    parts
}

fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch >= '\u{80}'
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
                pending_combinator = Some(Combinator::Child);
                saw_space = false;
                pos += 1;
                continue;
            }
            '+' | '~' => return None, // sibling combinators unsupported in v1
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

/// Parse one compound selector starting at `pos`. Returns the compound and
/// the index just past it.
fn parse_compound(chars: &[char], mut pos: usize) -> Option<(CompoundSelector, usize)> {
    let mut compound = CompoundSelector::default();
    let mut consumed_any = false;

    // Optional leading element name or universal `*`.
    if pos < chars.len() {
        if chars[pos] == '*' {
            compound.universal = true;
            consumed_any = true;
            pos += 1;
        } else if is_ident_char(chars[pos]) {
            let start = pos;
            while pos < chars.len() && is_ident_char(chars[pos]) {
                pos += 1;
            }
            compound.tag = Some(chars[start..pos].iter().collect());
            consumed_any = true;
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
                let start = pos;
                while pos < chars.len() && is_ident_char(chars[pos]) {
                    pos += 1;
                }
                if start == pos {
                    return None; // lone '.' or '#'
                }
                let name: String = chars[start..pos].iter().collect();
                if is_class {
                    compound.classes.push(name);
                } else {
                    compound.id = Some(name);
                }
                consumed_any = true;
            }
            '[' => {
                // Attribute selector — parsed but never matches in v1.
                compound.unsupported = true;
                let mut depth = 1i32;
                pos += 1;
                while pos < chars.len() && depth > 0 {
                    if chars[pos] == '[' {
                        depth += 1;
                    } else if chars[pos] == ']' {
                        depth -= 1;
                    }
                    pos += 1;
                }
                consumed_any = true;
            }
            ':' => {
                // Pseudo-class (`:hover`) or pseudo-element (`::before`) —
                // parsed but never matches in v1.
                compound.unsupported = true;
                pos += 1;
                if pos < chars.len() && chars[pos] == ':' {
                    pos += 1;
                }
                while pos < chars.len() && is_ident_char(chars[pos]) {
                    pos += 1;
                }
                // Optional parenthesized argument, e.g. `:nth-child(2n+1)`.
                if pos < chars.len() && chars[pos] == '(' {
                    let mut depth = 1i32;
                    pos += 1;
                    while pos < chars.len() && depth > 0 {
                        if chars[pos] == '(' {
                            depth += 1;
                        } else if chars[pos] == ')' {
                            depth -= 1;
                        }
                        pos += 1;
                    }
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
    fn pseudo_class_and_attr_rules_never_match() {
        setup();
        register_rule(".btn:hover", &[("color", "red")]);
        register_rule("input[disabled]", &[("opacity", "0.5")]);
        register_rule("li:first-child", &[("color", "blue")]);
        register_rule("a::before", &[("color", "green")]);

        assert!(matching_declarations("button", None, &["btn"], &[]).is_empty());
        assert!(matching_declarations("input", None, &[], &[]).is_empty());
        assert!(matching_declarations("li", None, &[], &[]).is_empty());
        assert!(matching_declarations("a", None, &[], &[]).is_empty());
        // Rules are still registered (counted) — they just never match.
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
    fn sibling_combinators_dropped() {
        setup();
        register_rule(".a + .b", &[("color", "red")]);
        register_rule(".a ~ .b", &[("color", "blue")]);
        assert_eq!(rule_count(), 0);
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
}
