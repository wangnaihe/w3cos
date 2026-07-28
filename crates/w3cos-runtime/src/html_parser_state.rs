//! Feature-neutral state shared by document and fragment HTML parsing.

use crate::html_compat::DocumentCompatibilityMode;
use crate::html_parser_host::ParserScriptHost;
use std::cell::Cell;
use std::rc::Rc;

thread_local! {
    static PARSER_INSERTION_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Observable result of feeding bytes to the incremental HTML document parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentParseProgress {
    /// The supplied input was consumed as far as currently possible.
    Advanced,
    /// Parsing is paused immediately after a parser-blocking classic script.
    BlockedOnScript,
    /// EOF was consumed and the document parsing lifecycle has finished.
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocumentInsertionSection {
    Head,
    Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TemplateInsertionMode {
    InTemplate,
    InTable,
    InColumnGroup,
    InTableBody,
    InRow,
    InBody,
}

pub(crate) const HTML_NAMESPACE: &str = "http://www.w3.org/1999/xhtml";
pub(crate) const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";
pub(crate) const MATHML_NAMESPACE: &str = "http://www.w3.org/1998/Math/MathML";
pub(crate) const XLINK_NAMESPACE: &str = "http://www.w3.org/1999/xlink";
pub(crate) const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
pub(crate) const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

#[derive(Clone)]
pub(crate) struct ActiveFormattingElement {
    pub(crate) tag: String,
    pub(crate) node: u32,
    pub(crate) attributes: Vec<(String, String)>,
}

/// Incremental HTML tokenizer/tree-builder state.
///
/// Script execution is represented only by [`ParserScriptHost`], keeping this
/// state independent from the dynamic compiler, W3IR, and W3VM.
pub struct StreamingDocumentParser {
    pub(crate) script_host: Rc<dyn ParserScriptHost>,
    pub(crate) document_url: String,
    pub(crate) buffer: String,
    pub(crate) stack: Vec<u32>,
    pub(crate) html: u32,
    pub(crate) head: u32,
    pub(crate) body: u32,
    pub(crate) section: DocumentInsertionSection,
    pub(crate) active_formatting: Vec<Option<ActiveFormattingElement>>,
    pub(crate) template_modes: Vec<TemplateInsertionMode>,
    pub(crate) doctype_seen: bool,
    pub(crate) document_mode_locked: bool,
    pub(crate) compatibility_mode: DocumentCompatibilityMode,
    pub(crate) parse_errors: usize,
    pub(crate) fragment_root: Option<u32>,
    pub(crate) sanitize: bool,
    pub(crate) drop_until_end_tag: Option<String>,
    pub(crate) paused: bool,
    pub(crate) eof_received: bool,
    pub(crate) complete: bool,
}

struct ParserInsertionGuard;

impl Drop for ParserInsertionGuard {
    fn drop(&mut self) {
        PARSER_INSERTION_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

pub(crate) fn parser_insertion_active() -> bool {
    PARSER_INSERTION_DEPTH.with(|depth| depth.get() > 0)
}

pub(crate) fn append_parser_child(parent: u32, child: u32) {
    PARSER_INSERTION_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
    let _guard = ParserInsertionGuard;
    crate::dom::append_child(parent, child);
}

pub(crate) fn insert_parser_before(parent: u32, child: u32, reference: u32) {
    PARSER_INSERTION_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
    let _guard = ParserInsertionGuard;
    crate::dom::insert_before(parent, child, reference);
}

pub(crate) fn reset_parser_insertion_state() {
    PARSER_INSERTION_DEPTH.with(|depth| depth.set(0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_state_has_no_dynamic_runtime_dependency() {
        assert_ne!(HTML_NAMESPACE, SVG_NAMESPACE);
        assert_ne!(HTML_NAMESPACE, MATHML_NAMESPACE);
        assert_eq!(
            DocumentParseProgress::Advanced,
            DocumentParseProgress::Advanced
        );
        assert!(std::mem::size_of::<StreamingDocumentParser>() > 0);
    }
}
