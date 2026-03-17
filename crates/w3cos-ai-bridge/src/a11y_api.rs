use w3cos_a11y::tree::{A11yNode, build_a11y_tree, flatten_for_ai};
use w3cos_dom::document::Document;

/// Layer 2: Accessibility tree API.
/// Returns structured ARIA tree for AI agents that prefer semantic understanding
/// over raw DOM manipulation.
/// Get the full accessibility tree as a structured JSON object.
pub fn get_tree(doc: &Document) -> A11yNode {
    build_a11y_tree(doc)
}

/// Get the accessibility tree as JSON string.
pub fn get_tree_json(doc: &Document) -> String {
    let tree = build_a11y_tree(doc);
    serde_json::to_string_pretty(&tree).unwrap_or_default()
}

/// Get a flat, numbered list of all interactive elements.
/// Optimized for AI agents: minimal tokens, maximum actionability.
///
/// Returns something like:
/// ```text
/// [1] navigation: Sidebar
/// [2] button: Overview [interactive]
/// [3] button: Applications [interactive]
/// [4] heading: System Overview (level 1)
/// [5] button: New App [interactive]
/// [6] text: CPU 23%
/// [7] progressbar (value: 23)
/// [8] button: Get Started [interactive]
/// ```
pub fn get_interactive_elements(doc: &Document) -> Vec<String> {
    let tree = build_a11y_tree(doc);
    flatten_for_ai(&tree)
}

/// Get a compact text summary of the current UI state.
/// Designed to fit within a single LLM message with minimal tokens.
pub fn get_ui_summary(doc: &Document) -> String {
    let tree = build_a11y_tree(doc);
    let flat = flatten_for_ai(&tree);
    let interactive_count = flat.iter().filter(|l| l.contains("[interactive]")).count();
    let total = flat.len();

    let mut summary = String::new();
    summary.push_str(&format!(
        "UI: {total} elements, {interactive_count} interactive\n"
    ));
    summary.push_str("---\n");
    for line in &flat {
        summary.push_str(line);
        summary.push('\n');
    }
    summary
}
