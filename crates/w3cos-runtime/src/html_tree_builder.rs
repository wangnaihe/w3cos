//! Incremental HTML tree-builder lifecycle.
//!
//! The method body is being migrated here from the dynamic loading module
//! without duplicating parser behavior. Script checkpoints cross only the
//! feature-neutral `ParserScriptHost` boundary.

use crate::html_compat::{DocumentCompatibilityMode, parse_document_doctype};
use crate::html_fragment_policy::{
    adjust_foreign_attribute, adjust_foreign_tag_name, consumes_content_when_filtered,
    find_raw_text_end, is_active_fragment_element, is_foreign_html_breakout, is_formatting_element,
    is_head_element, is_html_void_element, is_special_html_element, is_unsafe_fragment_attribute,
};
use crate::html_parser_host::{InertParserScriptHost, ParserScriptHost};
use crate::html_parser_state::{
    ActiveFormattingElement, DocumentInsertionSection, DocumentParseProgress, HTML_NAMESPACE,
    MATHML_NAMESPACE, SVG_NAMESPACE, StreamingDocumentParser, TemplateInsertionMode,
    append_parser_child, insert_parser_before,
};
use anyhow::{Result, anyhow};
use std::rc::Rc;

impl StreamingDocumentParser {
    fn push_active_formatting(&mut self, entry: ActiveFormattingElement) {
        let start = self
            .active_formatting
            .iter()
            .rposition(Option::is_none)
            .map_or(0, |position| position + 1);
        let identical = self.active_formatting[start..]
            .iter()
            .enumerate()
            .filter_map(|(offset, candidate)| {
                candidate
                    .as_ref()
                    .filter(|candidate| {
                        candidate.tag == entry.tag && candidate.attributes == entry.attributes
                    })
                    .map(|_| start + offset)
            })
            .collect::<Vec<_>>();
        if identical.len() >= 3 {
            self.active_formatting.remove(identical[0]);
        }
        self.active_formatting.push(Some(entry));
    }

    fn reconstruct_active_formatting(&mut self) {
        let start_boundary = self
            .active_formatting
            .iter()
            .rposition(Option::is_none)
            .map_or(0, |position| position + 1);
        let mut start = self.active_formatting.len();
        for index in (start_boundary..self.active_formatting.len()).rev() {
            let Some(entry) = self.active_formatting[index].as_ref() else {
                break;
            };
            if self.stack.contains(&entry.node) {
                break;
            }
            start = index;
        }
        for index in start..self.active_formatting.len() {
            let Some(entry) = self.active_formatting[index].clone() else {
                continue;
            };
            let replacement = crate::dom::create_element(&entry.tag);
            for (attribute, value) in &entry.attributes {
                crate::jsdom::apply_html_attribute(replacement, attribute, value);
            }
            self.register_inline_event_handlers(replacement, &entry.attributes);
            append_parser_child(self.current_parent(), replacement);
            self.stack.push(replacement);
            if let Some(active) = self.active_formatting[index].as_mut() {
                active.node = replacement;
            }
        }
    }

    fn run_adoption_agency(&mut self, name: &str) -> bool {
        let marker = self
            .active_formatting
            .iter()
            .rposition(Option::is_none)
            .map_or(0, |position| position + 1);
        if !self.active_formatting[marker..].iter().any(|entry| {
            entry
                .as_ref()
                .is_some_and(|entry| entry.tag.as_str() == name)
        }) {
            return false;
        }

        for _ in 0..8 {
            let Some(active_position) = self.active_formatting[marker..]
                .iter()
                .rposition(|entry| {
                    entry
                        .as_ref()
                        .is_some_and(|entry| entry.tag.as_str() == name)
                })
                .map(|position| marker + position)
            else {
                return true;
            };
            let formatting_entry = self.active_formatting[active_position]
                .as_ref()
                .expect("matching active formatting entry exists")
                .clone();
            let formatting_node = formatting_entry.node;
            let Some(stack_position) = self.stack.iter().rposition(|node| *node == formatting_node)
            else {
                self.active_formatting.remove(active_position);
                return true;
            };
            if self.stack.last().copied() == Some(formatting_node) {
                self.stack.pop();
                self.active_formatting.remove(active_position);
                return true;
            }

            let furthest_block = self.stack[stack_position + 1..]
                .iter()
                .copied()
                .find(|node| is_special_html_element(*node));
            let Some(furthest_block) = furthest_block else {
                self.stack.truncate(stack_position);
                self.active_formatting.remove(active_position);
                return true;
            };
            let Some(common_ancestor) = stack_position
                .checked_sub(1)
                .and_then(|position| self.stack.get(position))
                .copied()
            else {
                self.active_formatting.remove(active_position);
                return true;
            };

            let mut bookmark = active_position;
            let mut last_node = furthest_block;
            let mut node = furthest_block;
            let mut inner_count = 0usize;
            loop {
                let Some(node_position) = self.stack.iter().position(|entry| *entry == node) else {
                    break;
                };
                let Some(previous_position) = node_position.checked_sub(1) else {
                    break;
                };
                node = self.stack[previous_position];
                if node == formatting_node {
                    break;
                }
                inner_count += 1;

                let mut node_active_position = self.active_formatting[marker..]
                    .iter()
                    .position(|entry| entry.as_ref().is_some_and(|entry| entry.node == node))
                    .map(|position| marker + position);
                if inner_count > 3
                    && let Some(position) = node_active_position
                {
                    self.active_formatting.remove(position);
                    if position < bookmark {
                        bookmark = bookmark.saturating_sub(1);
                    }
                    node_active_position = None;
                }
                let Some(node_active_position) = node_active_position else {
                    self.stack.remove(previous_position);
                    continue;
                };

                let replacement = crate::dom::clone_node(node, false);
                self.stack[previous_position] = replacement;
                if let Some(active) = self.active_formatting[node_active_position].as_mut() {
                    active.node = replacement;
                }
                if last_node == furthest_block {
                    bookmark = node_active_position + 1;
                }
                append_parser_child(replacement, last_node);
                last_node = replacement;
            }

            self.insert_at_adoption_location(common_ancestor, last_node);

            let replacement = crate::dom::clone_node(formatting_node, false);
            for child in crate::dom::children(furthest_block) {
                append_parser_child(replacement, child);
            }
            append_parser_child(furthest_block, replacement);

            if let Some(position) = self.active_formatting[marker..]
                .iter()
                .position(|entry| {
                    entry
                        .as_ref()
                        .is_some_and(|entry| entry.node == formatting_node)
                })
                .map(|position| marker + position)
            {
                self.active_formatting.remove(position);
                if position < bookmark {
                    bookmark = bookmark.saturating_sub(1);
                }
            }
            self.active_formatting.insert(
                bookmark.min(self.active_formatting.len()),
                Some(ActiveFormattingElement {
                    node: replacement,
                    ..formatting_entry
                }),
            );

            if let Some(position) = self
                .stack
                .iter()
                .position(|entry| *entry == formatting_node)
            {
                self.stack.remove(position);
            }
            if let Some(position) = self.stack.iter().position(|entry| *entry == furthest_block) {
                self.stack.insert(position + 1, replacement);
            }
        }
        true
    }

    fn insert_at_adoption_location(&self, common_ancestor: u32, child: u32) {
        if matches!(
            crate::dom::tag_name(common_ancestor).as_str(),
            "table" | "tbody" | "tfoot" | "thead" | "tr"
        ) {
            self.insert_at_foster_parent(child);
        } else if crate::dom::tag_name(common_ancestor).eq_ignore_ascii_case("template")
            && crate::jsdom::namespace_uri(common_ancestor) == HTML_NAMESPACE
        {
            append_parser_child(
                crate::jsdom::ensure_template_content(common_ancestor),
                child,
            );
        } else {
            append_parser_child(common_ancestor, child);
        }
    }

    pub(crate) fn clear_active_formatting_to_marker(&mut self) {
        while let Some(entry) = self.active_formatting.pop() {
            if entry.is_none() {
                break;
            }
        }
    }

    fn pop_current_table_cell(&mut self) {
        let boundary = self.template_scope_start();
        if let Some(position) = self.stack[boundary..]
            .iter()
            .rposition(|node| matches!(crate::dom::tag_name(*node).as_str(), "td" | "th"))
            .map(|position| boundary + position)
        {
            self.stack.truncate(position);
            self.clear_active_formatting_to_marker();
        }
    }

    fn pop_current_table_row(&mut self) {
        self.pop_current_table_cell();
        let boundary = self.template_scope_start();
        if let Some(position) = self.stack[boundary..]
            .iter()
            .rposition(|node| crate::dom::tag_name(*node) == "tr")
            .map(|position| boundary + position)
        {
            self.stack.truncate(position);
        }
    }

    fn pop_current_table_section(&mut self) {
        let boundary = self.template_scope_start();
        if let Some(position) = self.stack[boundary..]
            .iter()
            .rposition(|node| {
                matches!(
                    crate::dom::tag_name(*node).as_str(),
                    "tbody" | "thead" | "tfoot" | "colgroup"
                )
            })
            .map(|position| boundary + position)
        {
            self.stack.truncate(position);
        }
    }

    fn pop_current_table_caption(&mut self) {
        let boundary = self.template_scope_start();
        if let Some(position) = self.stack[boundary..]
            .iter()
            .rposition(|node| crate::dom::tag_name(*node) == "caption")
            .map(|position| boundary + position)
        {
            self.stack.truncate(position);
            self.clear_active_formatting_to_marker();
        }
    }

    pub(crate) fn has_open_template(&self) -> bool {
        self.stack.iter().any(|node| {
            crate::dom::tag_name(*node).eq_ignore_ascii_case("template")
                && crate::jsdom::namespace_uri(*node) == HTML_NAMESPACE
        })
    }

    pub(crate) fn template_scope_start(&self) -> usize {
        let floor = self.stack_floor();
        self.stack
            .iter()
            .rposition(|node| {
                crate::dom::tag_name(*node).eq_ignore_ascii_case("template")
                    && crate::jsdom::namespace_uri(*node) == HTML_NAMESPACE
            })
            .map_or(floor, |position| position.max(floor))
    }

    pub(crate) fn stack_floor(&self) -> usize {
        usize::from(self.fragment_root.is_some())
    }

    fn update_template_insertion_mode_for_start_tag(&mut self, incoming: &str) {
        let Some(mode) = self.template_modes.last_mut() else {
            return;
        };
        *mode = match incoming {
            "caption" | "colgroup" | "tbody" | "tfoot" | "thead" => TemplateInsertionMode::InTable,
            "col" => TemplateInsertionMode::InColumnGroup,
            "tr" => TemplateInsertionMode::InTableBody,
            "td" | "th" => TemplateInsertionMode::InRow,
            "base" | "basefont" | "bgsound" | "link" | "meta" | "noscript" | "script" | "style"
            | "template" | "title" => return,
            _ => TemplateInsertionMode::InBody,
        };
    }

    fn should_foster_parent_text(&self, text: &str) -> bool {
        !text.chars().all(char::is_whitespace) && self.in_table_insertion_context()
    }

    fn should_foster_parent_element(&self, incoming: &str) -> bool {
        self.in_table_insertion_context()
            && !matches!(
                incoming,
                "caption"
                    | "col"
                    | "colgroup"
                    | "tbody"
                    | "tfoot"
                    | "thead"
                    | "tr"
                    | "td"
                    | "th"
                    | "script"
                    | "style"
                    | "template"
            )
    }

    fn in_table_insertion_context(&self) -> bool {
        let boundary = self.template_scope_start();
        let current_is_table_context = self
            .stack
            .last()
            .map(|node| crate::dom::tag_name(*node))
            .is_some_and(|tag| {
                matches!(
                    tag.as_str(),
                    "table" | "tbody" | "thead" | "tfoot" | "tr" | "colgroup"
                )
            });
        current_is_table_context
            && (self.stack[boundary..]
                .iter()
                .any(|node| crate::dom::tag_name(*node) == "table")
                || (boundary > 0
                    && self.template_modes.last().is_some_and(|mode| {
                        matches!(
                            mode,
                            TemplateInsertionMode::InTable
                                | TemplateInsertionMode::InColumnGroup
                                | TemplateInsertionMode::InTableBody
                                | TemplateInsertionMode::InRow
                        )
                    })))
    }

    pub(crate) fn insert_at_foster_parent(&self, child: u32) {
        let template_position = self.stack.iter().rposition(|node| {
            crate::dom::tag_name(*node).eq_ignore_ascii_case("template")
                && crate::jsdom::namespace_uri(*node) == HTML_NAMESPACE
        });
        let table_position = self
            .stack
            .iter()
            .rposition(|node| crate::dom::tag_name(*node) == "table");
        if let Some(template_position) = template_position
            && table_position.is_none_or(|table_position| template_position > table_position)
        {
            let template = self.stack[template_position];
            append_parser_child(crate::jsdom::ensure_template_content(template), child);
            return;
        }
        let Some((position, table)) = self
            .stack
            .iter()
            .enumerate()
            .rev()
            .find(|(_, node)| crate::dom::tag_name(**node) == "table")
        else {
            append_parser_child(self.current_parent(), child);
            return;
        };
        if let Some(parent) = crate::dom::parent_node(*table) {
            insert_parser_before(parent, child, *table);
        } else if position > 0 {
            append_parser_child(self.stack[position - 1], child);
        } else {
            append_parser_child(self.body, child);
        }
    }

    fn apply_implied_end_tags(&mut self, incoming: &str) {
        let current = self
            .stack
            .last()
            .map(|node| crate::dom::tag_name(*node))
            .unwrap_or_default();
        let closes_paragraph = matches!(
            incoming,
            "address"
                | "article"
                | "aside"
                | "blockquote"
                | "div"
                | "dl"
                | "fieldset"
                | "footer"
                | "form"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "header"
                | "hr"
                | "menu"
                | "nav"
                | "ol"
                | "p"
                | "pre"
                | "section"
                | "table"
                | "ul"
        );
        if current == "p" && closes_paragraph {
            self.stack.pop();
        } else if current == "li" && incoming == "li" {
            self.stack.pop();
        } else if matches!(current.as_str(), "dt" | "dd") && matches!(incoming, "dt" | "dd") {
            self.stack.pop();
        }
    }

    pub(crate) fn close_element(&mut self, name: &str) {
        let name = name
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if is_formatting_element(&name) && self.run_adoption_agency(&name) {
            return;
        }
        if name == "template" {
            let floor = self.stack_floor();
            if let Some(position) = self.stack[floor..]
                .iter()
                .rposition(|node| {
                    crate::dom::tag_name(*node) == "template"
                        && crate::jsdom::namespace_uri(*node) == HTML_NAMESPACE
                })
                .map(|position| floor + position)
            {
                self.stack.truncate(position);
                if self.stack.is_empty() {
                    self.stack.push(self.html);
                }
                self.clear_active_formatting_to_marker();
                self.template_modes.pop();
            }
            return;
        }
        if self.fragment_root.is_some() && matches!(name.as_str(), "html" | "head" | "body") {
            return;
        }
        let has_open_template = self.has_open_template();
        if name == "head" && !has_open_template {
            self.stack.clear();
            self.stack.push(self.html);
            return;
        }
        if matches!(name.as_str(), "body" | "html") && !has_open_template {
            self.stack.clear();
            self.stack.push(self.html);
            return;
        }
        let boundary = self.template_scope_start();
        if let Some(position) = self.stack[boundary..]
            .iter()
            .rposition(|node| crate::dom::tag_name(*node).eq_ignore_ascii_case(&name))
            .map(|position| boundary + position)
        {
            self.stack.truncate(position);
            if matches!(name.as_str(), "caption" | "td" | "th") {
                self.clear_active_formatting_to_marker();
            }
            if self.stack.is_empty() {
                self.stack.push(self.html);
            }
        } else {
            self.parse_errors += 1;
        }
    }

    fn script_checkpoint(&mut self, _script: u32) -> Result<()> {
        if self.fragment_root.is_some() {
            return Ok(());
        }
        self.script_host.perform_microtask_checkpoint();
        self.script_host
            .execute_pending_document_scripts(&self.document_url)?;
        // The parser pauses while the script runs. Deliver mutations and
        // promise jobs created by that script before tokenization resumes and
        // inserts the next parser-authored node.
        self.script_host.perform_microtask_checkpoint();
        self.paused = self.script_host.has_pending_parser_blocking_script();
        Ok(())
    }

    fn complete_document(&mut self) -> Result<DocumentParseProgress> {
        self.lock_missing_doctype_mode();
        if self.fragment_root.is_some() {
            self.complete = true;
            return Ok(DocumentParseProgress::Complete);
        }
        if self.script_host.has_pending_parser_blocking_script() {
            self.paused = true;
            return Ok(DocumentParseProgress::BlockedOnScript);
        }
        self.script_host.finish_document_parse();
        self.complete = true;
        Ok(DocumentParseProgress::Complete)
    }

    pub(crate) fn open_element(&mut self, token: &str) -> Result<()> {
        self.lock_missing_doctype_mode();
        let self_closing = token.trim_end().ends_with('/');
        let token = token.trim_end_matches('/').trim_end();
        let name_end = token.find(char::is_whitespace).unwrap_or(token.len());
        let name = token[..name_end].to_ascii_lowercase();
        if name.is_empty() {
            return Ok(());
        }
        let attributes = crate::jsdom::parse_html_attributes(&token[name_end..]);
        if self.sanitize && is_active_fragment_element(&name) {
            if consumes_content_when_filtered(&name) {
                self.drop_until_end_tag = Some(name);
            }
            return Ok(());
        }
        if self.fragment_root.is_some() && matches!(name.as_str(), "html" | "head" | "body") {
            return Ok(());
        }
        let has_open_template = self.has_open_template();
        self.leave_foreign_content_for_html_breakout(&name, &attributes);
        match name.as_str() {
            "html" if !has_open_template => {
                for (attribute, value) in &attributes {
                    crate::jsdom::apply_html_attribute(self.html, attribute, value);
                }
                self.register_inline_event_handlers(self.html, &attributes);
                self.stack.clear();
                self.stack.push(self.html);
                return Ok(());
            }
            "head" if !has_open_template => {
                self.section = DocumentInsertionSection::Head;
                for (attribute, value) in &attributes {
                    crate::jsdom::apply_html_attribute(self.head, attribute, value);
                }
                self.register_inline_event_handlers(self.head, &attributes);
                self.stack.clear();
                self.stack.extend([self.html, self.head]);
                return Ok(());
            }
            "body" if !has_open_template => {
                self.enter_body();
                for (attribute, value) in &attributes {
                    crate::jsdom::apply_html_attribute(self.body, attribute, value);
                }
                self.register_inline_event_handlers(self.body, &attributes);
                return Ok(());
            }
            _ => {}
        }
        if has_open_template && matches!(name.as_str(), "html" | "head" | "body") {
            return Ok(());
        }
        self.update_template_insertion_mode_for_start_tag(&name);

        let at_head_boundary = self.fragment_root.is_none()
            && self
                .stack
                .last()
                .is_some_and(|node| *node == self.html || *node == self.head);
        if self.section == DocumentInsertionSection::Head
            && at_head_boundary
            && !is_head_element(&name)
        {
            self.enter_body();
        } else if self.stack.len() == 1 && self.section == DocumentInsertionSection::Head {
            self.stack.push(self.head);
        }
        if !matches!(
            name.as_str(),
            "caption"
                | "col"
                | "colgroup"
                | "tbody"
                | "tfoot"
                | "thead"
                | "tr"
                | "td"
                | "th"
                | "template"
        ) && self
            .stack
            .last()
            .is_some_and(|node| crate::jsdom::namespace_uri(*node) == HTML_NAMESPACE)
        {
            self.reconstruct_active_formatting();
        }
        self.prepare_table_context(&name);
        self.apply_implied_end_tags(&name);
        let namespace = self.namespace_for_element(&name);
        let adjusted_name = adjust_foreign_tag_name(namespace, &name);
        let element = if namespace == HTML_NAMESPACE {
            crate::dom::create_element(&name)
        } else {
            crate::jsdom::create_namespaced_element(namespace, adjusted_name)
        };
        if namespace == HTML_NAMESPACE && name == "template" {
            crate::jsdom::ensure_template_content(element);
        }
        let formatting_attributes = attributes.clone();
        for (attribute, value) in &attributes {
            if self.sanitize && is_unsafe_fragment_attribute(attribute, value) {
                continue;
            }
            let adjusted_attribute = adjust_foreign_attribute(namespace, attribute);
            crate::jsdom::apply_html_attribute_ns(
                element,
                adjusted_attribute.namespace,
                adjusted_attribute.qualified_name,
                adjusted_attribute.prefix,
                adjusted_attribute.local_name,
                value,
            );
        }
        if !self.sanitize {
            self.register_inline_event_handlers(element, &attributes);
        }
        let declarative_shadow_root = if namespace == HTML_NAMESPACE && name == "template" {
            attributes
                .iter()
                .find(|(attribute, value)| {
                    attribute.eq_ignore_ascii_case("shadowrootmode")
                        && matches!(value.to_ascii_lowercase().as_str(), "open" | "closed")
                })
                .map(|(_, mode)| {
                    crate::jsdom::activate_declarative_shadow_root(
                        self.current_parent(),
                        element,
                        &mode.to_ascii_lowercase(),
                    )
                })
                .unwrap_or(false)
        } else {
            false
        };
        if declarative_shadow_root {
            // A declarative shadow template is consumed by the parser. Its
            // template contents target is the newly-created shadow root.
        } else if self.should_foster_parent_element(&name) {
            self.insert_at_foster_parent(element);
        } else {
            append_parser_child(self.current_parent(), element);
        }
        let is_void = is_html_void_element(&name);
        if namespace == HTML_NAMESPACE && self_closing && !is_void {
            self.parse_errors += 1;
        }
        let pushes_element = !is_void && (namespace == HTML_NAMESPACE || !self_closing);
        if pushes_element {
            self.stack.push(element);
            if namespace == HTML_NAMESPACE && name == "template" {
                self.active_formatting.push(None);
                self.template_modes.push(TemplateInsertionMode::InTemplate);
            } else if namespace == HTML_NAMESPACE
                && matches!(name.as_str(), "caption" | "td" | "th")
            {
                self.active_formatting.push(None);
            } else if namespace == HTML_NAMESPACE && is_formatting_element(&name) {
                self.push_active_formatting(ActiveFormattingElement {
                    tag: name,
                    node: element,
                    attributes: formatting_attributes,
                });
            }
        } else if name == "script" {
            self.script_checkpoint(element)?;
        }
        Ok(())
    }

    fn register_inline_event_handlers(&self, node: u32, attributes: &[(String, String)]) {
        for (name, source) in attributes {
            let Some(event_type) = name
                .strip_prefix("on")
                .filter(|event_type| !event_type.is_empty())
            else {
                continue;
            };
            self.script_host
                .register_inline_event_handler(node, event_type, source);
        }
    }

    fn prepare_table_context(&mut self, incoming: &str) {
        if matches!(incoming, "td" | "th") {
            self.pop_current_table_cell();
        }
        if incoming == "tr" {
            self.pop_current_table_row();
        }
        if matches!(
            incoming,
            "caption" | "colgroup" | "tbody" | "tfoot" | "thead"
        ) {
            self.pop_current_table_cell();
            self.pop_current_table_row();
            self.pop_current_table_section();
            self.pop_current_table_caption();
        }
        let current = self
            .stack
            .last()
            .map(|node| crate::dom::tag_name(*node))
            .unwrap_or_default();
        if current == "template" {
            match incoming {
                "col" => {
                    let colgroup = crate::dom::create_element("colgroup");
                    append_parser_child(self.current_parent(), colgroup);
                    self.stack.push(colgroup);
                }
                "tr" => {
                    let tbody = crate::dom::create_element("tbody");
                    append_parser_child(self.current_parent(), tbody);
                    self.stack.push(tbody);
                }
                "td" | "th" => {
                    let tbody = crate::dom::create_element("tbody");
                    append_parser_child(self.current_parent(), tbody);
                    self.stack.push(tbody);
                    let tr = crate::dom::create_element("tr");
                    append_parser_child(self.current_parent(), tr);
                    self.stack.push(tr);
                }
                _ => {}
            }
        }
        let current = self
            .stack
            .last()
            .map(|node| crate::dom::tag_name(*node))
            .unwrap_or_default();
        if incoming == "tr" && current == "table" {
            let tbody = crate::dom::create_element("tbody");
            append_parser_child(self.current_parent(), tbody);
            self.stack.push(tbody);
        }
        if incoming == "col" && current == "table" {
            let colgroup = crate::dom::create_element("colgroup");
            append_parser_child(self.current_parent(), colgroup);
            self.stack.push(colgroup);
        }
        let current = self
            .stack
            .last()
            .map(|node| crate::dom::tag_name(*node))
            .unwrap_or_default();
        if matches!(incoming, "td" | "th") {
            if current == "table" {
                let tbody = crate::dom::create_element("tbody");
                append_parser_child(self.current_parent(), tbody);
                self.stack.push(tbody);
            }
            let current = self
                .stack
                .last()
                .map(|node| crate::dom::tag_name(*node))
                .unwrap_or_default();
            if matches!(current.as_str(), "tbody" | "thead" | "tfoot") {
                let tr = crate::dom::create_element("tr");
                append_parser_child(self.current_parent(), tr);
                self.stack.push(tr);
            }
        }
    }

    fn namespace_for_element(&self, incoming: &str) -> &'static str {
        let Some(parent) = self.stack.last().copied() else {
            return HTML_NAMESPACE;
        };
        let parent_namespace = crate::jsdom::namespace_uri(parent);
        if parent_namespace == HTML_NAMESPACE || self.is_html_integration_point(parent) {
            return match incoming {
                "svg" => SVG_NAMESPACE,
                "math" => MATHML_NAMESPACE,
                _ => HTML_NAMESPACE,
            };
        }
        if parent_namespace == MATHML_NAMESPACE
            && self.is_mathml_text_integration_point(parent)
            && !matches!(incoming, "mglyph" | "malignmark")
        {
            return match incoming {
                "svg" => SVG_NAMESPACE,
                "math" => MATHML_NAMESPACE,
                _ => HTML_NAMESPACE,
            };
        }
        if parent_namespace == MATHML_NAMESPACE
            && crate::dom::tag_name(parent) == "annotation-xml"
            && incoming == "svg"
        {
            return SVG_NAMESPACE;
        }
        if parent_namespace == SVG_NAMESPACE {
            SVG_NAMESPACE
        } else {
            MATHML_NAMESPACE
        }
    }

    fn is_html_integration_point(&self, node: u32) -> bool {
        let namespace = crate::jsdom::namespace_uri(node);
        let tag = crate::dom::tag_name(node).to_ascii_lowercase();
        if namespace == SVG_NAMESPACE {
            return matches!(tag.as_str(), "foreignobject" | "desc" | "title");
        }
        if namespace != MATHML_NAMESPACE || tag != "annotation-xml" {
            return false;
        }
        crate::dom::get_attribute(node, "encoding").is_some_and(|encoding| {
            matches!(
                encoding.trim().to_ascii_lowercase().as_str(),
                "text/html" | "application/xhtml+xml"
            )
        })
    }

    fn is_mathml_text_integration_point(&self, node: u32) -> bool {
        crate::jsdom::namespace_uri(node) == MATHML_NAMESPACE
            && matches!(
                crate::dom::tag_name(node).as_str(),
                "mi" | "mo" | "mn" | "ms" | "mtext"
            )
    }

    fn leave_foreign_content_for_html_breakout(
        &mut self,
        incoming: &str,
        attributes: &[(String, String)],
    ) {
        let Some(current) = self.stack.last().copied() else {
            return;
        };
        if crate::jsdom::namespace_uri(current) == HTML_NAMESPACE
            || self.is_html_integration_point(current)
            || (self.is_mathml_text_integration_point(current)
                && !matches!(incoming, "mglyph" | "malignmark"))
            || !is_foreign_html_breakout(incoming, attributes)
        {
            return;
        }
        while self.stack.len() > 1 {
            let Some(node) = self.stack.last().copied() else {
                break;
            };
            if crate::jsdom::namespace_uri(node) == HTML_NAMESPACE
                || self.is_html_integration_point(node)
                || self.is_mathml_text_integration_point(node)
            {
                break;
            }
            self.stack.pop();
        }
    }

    pub(crate) fn new_with_script_host(
        script_host: Rc<dyn ParserScriptHost>,
        document_url: &str,
    ) -> Result<Self> {
        script_host.begin_document_parse(document_url)?;
        Self::from_started_navigation(script_host, document_url)
    }

    pub(crate) fn from_started_navigation(
        script_host: Rc<dyn ParserScriptHost>,
        document_url: &str,
    ) -> Result<Self> {
        let document = crate::jsdom::document_value();
        let html = crate::jsdom::node_id_of(&document.get_property("documentElement"))
            .ok_or_else(|| anyhow!("live document has no documentElement"))?;
        let head = crate::jsdom::node_id_of(&document.get_property("head"))
            .ok_or_else(|| anyhow!("live document has no head"))?;
        let body = crate::jsdom::node_id_of(&document.get_property("body"))
            .ok_or_else(|| anyhow!("live document has no body"))?;
        Ok(Self {
            script_host,
            document_url: document_url.to_string(),
            buffer: String::new(),
            stack: vec![html],
            html,
            head,
            body,
            section: DocumentInsertionSection::Head,
            active_formatting: Vec::new(),
            template_modes: Vec::new(),
            custom_entities: std::collections::HashMap::new(),
            doctype_seen: false,
            document_mode_locked: false,
            compatibility_mode: DocumentCompatibilityMode::NoQuirks,
            parse_errors: 0,
            fragment_root: None,
            sanitize: false,
            drop_until_end_tag: None,
            paused: false,
            eof_received: false,
            complete: false,
        })
    }

    pub(crate) fn for_fragment(parent: u32, sanitize: bool) -> Self {
        Self {
            script_host: Rc::new(InertParserScriptHost),
            document_url: "about:blank".to_string(),
            buffer: String::new(),
            stack: vec![parent],
            html: parent,
            head: parent,
            body: parent,
            section: DocumentInsertionSection::Body,
            active_formatting: Vec::new(),
            template_modes: Vec::new(),
            custom_entities: std::collections::HashMap::new(),
            doctype_seen: true,
            document_mode_locked: true,
            compatibility_mode: DocumentCompatibilityMode::NoQuirks,
            parse_errors: 0,
            fragment_root: Some(parent),
            sanitize,
            drop_until_end_tag: None,
            paused: false,
            eof_received: false,
            complete: false,
        }
    }

    /// Feed one decoded HTML chunk. Chunk boundaries may occur inside tags,
    /// quoted attributes, comments, entities, or raw script text.
    pub fn write(&mut self, chunk: &str) -> Result<DocumentParseProgress> {
        if self.complete {
            return Err(anyhow!(
                "cannot write after HTML document parser completion"
            ));
        }
        self.buffer.push_str(chunk);
        self.drive()
    }

    /// Signal EOF. If a parser-blocking script is outstanding this returns
    /// `BlockedOnScript`; call [`resume`](Self::resume) from the browser task
    /// pump after the script settles.
    pub fn finish(&mut self) -> Result<DocumentParseProgress> {
        self.eof_received = true;
        self.drive()
    }

    /// Continue from the exact token following a parser-blocking script.
    pub fn resume(&mut self) -> Result<DocumentParseProgress> {
        self.drive()
    }

    pub fn is_blocked(&self) -> bool {
        self.paused && self.script_host.has_pending_parser_blocking_script()
    }

    /// Compatibility mode selected by the document doctype.
    pub fn compatibility_mode(&self) -> DocumentCompatibilityMode {
        self.compatibility_mode
    }

    /// Number of tokenizer/tree-builder recovery checkpoints observed so far.
    pub fn parse_error_count(&self) -> usize {
        self.parse_errors
    }

    fn register_custom_entities_from_doctype(&mut self, token: &str) {
        if crate::jsdom::document_value()
            .get_property("contentType")
            .to_js_string()
            != "application/xhtml+xml"
        {
            return;
        }
        let mut cursor = token;
        while let Some(entity_start) = cursor.find("<!ENTITY") {
            let declaration = cursor[entity_start + "<!ENTITY".len()..].trim_start();
            if declaration.starts_with('%') {
                cursor = &declaration[1..];
                continue;
            }
            let name_end = declaration
                .find(char::is_whitespace)
                .unwrap_or(declaration.len());
            let name = &declaration[..name_end];
            let value = declaration[name_end..].trim_start();
            let Some(quote @ ('\'' | '"')) = value.chars().next() else {
                cursor = &declaration[name_end..];
                continue;
            };
            let quoted = &value[quote.len_utf8()..];
            let Some(value_end) = quoted.find(quote) else {
                break;
            };
            if !name.is_empty() {
                self.custom_entities
                    .insert(name.to_string(), quoted[..value_end].to_string());
            }
            cursor = &quoted[value_end + quote.len_utf8()..];
        }
    }

    fn expand_custom_entity_before_markup(&mut self) -> bool {
        if self.custom_entities.is_empty() {
            return false;
        }
        let text_end = self.buffer.find('<').unwrap_or(self.buffer.len());
        let text = &self.buffer[..text_end];
        let replacement = self
            .custom_entities
            .iter()
            .filter_map(|(name, value)| {
                let entity = format!("&{name};");
                text.find(&entity)
                    .map(|offset| (offset, entity.len(), value.clone()))
            })
            .min_by_key(|(offset, _, _)| *offset);
        let Some((offset, entity_len, replacement)) = replacement else {
            return false;
        };
        self.buffer
            .replace_range(offset..offset + entity_len, &replacement);
        true
    }

    pub(crate) fn drive(&mut self) -> Result<DocumentParseProgress> {
        if self.complete {
            return Ok(DocumentParseProgress::Complete);
        }
        if self.paused {
            if self.script_host.has_pending_parser_blocking_script() {
                return Ok(DocumentParseProgress::BlockedOnScript);
            }
            self.paused = false;
        }

        loop {
            if let Some(drop_tag) = self.drop_until_end_tag.clone() {
                let lower = self.buffer.to_ascii_lowercase();
                let close = format!("</{drop_tag}");
                let Some(close_start) = lower.find(&close) else {
                    if self.eof_received {
                        self.buffer.clear();
                        self.drop_until_end_tag = None;
                        continue;
                    }
                    return Ok(DocumentParseProgress::Advanced);
                };
                let Some(relative_end) = crate::jsdom::html_tag_end(&self.buffer[close_start..])
                else {
                    return Ok(DocumentParseProgress::Advanced);
                };
                self.buffer.drain(..close_start + relative_end + 1);
                self.drop_until_end_tag = None;
                continue;
            }
            if let Some(raw_tag) = self.current_raw_text_tag() {
                let lower = self.buffer.to_ascii_lowercase();
                let Some(close_start) = find_raw_text_end(&lower, &raw_tag) else {
                    if self.eof_received {
                        let text = std::mem::take(&mut self.buffer);
                        self.append_raw_text(&raw_tag, &text);
                        let node = self.stack.pop();
                        if let Some(node) = node {
                            if raw_tag == "style" {
                                self.script_host
                                    .finish_parser_style(node, &self.document_url)?;
                            } else if raw_tag == "script" {
                                self.script_checkpoint(node)?;
                                if self.paused {
                                    return Ok(DocumentParseProgress::BlockedOnScript);
                                }
                            }
                        }
                        continue;
                    }
                    return Ok(DocumentParseProgress::Advanced);
                };
                let Some(relative_end) = crate::jsdom::html_tag_end(&self.buffer[close_start..])
                else {
                    return Ok(DocumentParseProgress::Advanced);
                };
                let text = self.buffer[..close_start].to_string();
                self.append_raw_text(&raw_tag, &text);
                self.buffer.drain(..close_start + relative_end + 1);
                let node = self.stack.pop();
                if let Some(node) = node {
                    if raw_tag == "style" {
                        self.script_host
                            .finish_parser_style(node, &self.document_url)?;
                    } else if raw_tag == "script" {
                        self.script_checkpoint(node)?;
                        if self.paused {
                            return Ok(DocumentParseProgress::BlockedOnScript);
                        }
                    }
                }
                continue;
            }

            if self.expand_custom_entity_before_markup() {
                continue;
            }

            let Some(tag_start) = self.buffer.find('<') else {
                if self.eof_received {
                    let text = std::mem::take(&mut self.buffer);
                    self.append_text(&text);
                    return self.complete_document();
                }
                return Ok(DocumentParseProgress::Advanced);
            };
            if tag_start > 0 {
                let text = self.buffer[..tag_start].to_string();
                self.buffer.drain(..tag_start);
                self.append_text(&text);
                continue;
            }

            if self.buffer.starts_with("<!--") {
                let Some(end) = self.buffer.find("-->") else {
                    if self.eof_received {
                        self.parse_errors += 1;
                        let text = std::mem::take(&mut self.buffer);
                        self.append_text(&text);
                        return self.complete_document();
                    }
                    return Ok(DocumentParseProgress::Advanced);
                };
                let comment = self.buffer[4..end].to_string();
                self.buffer.drain(..end + 3);
                let node = crate::dom::create_comment(&comment);
                if self.fragment_root.is_none() && !self.doctype_seen {
                    crate::dom::insert_before(0, node, self.html);
                } else {
                    append_parser_child(self.current_parent(), node);
                }
                continue;
            }

            if self.buffer.starts_with("<![CDATA[")
                && self
                    .stack
                    .last()
                    .is_some_and(|node| crate::jsdom::namespace_uri(*node) != HTML_NAMESPACE)
            {
                let Some(end) = self.buffer.find("]]>") else {
                    if self.eof_received {
                        self.parse_errors += 1;
                        let text = self.buffer["<![CDATA[".len()..].to_string();
                        self.buffer.clear();
                        self.append_text_to_current(&text);
                        continue;
                    }
                    return Ok(DocumentParseProgress::Advanced);
                };
                let text = self.buffer["<![CDATA[".len()..end].to_string();
                self.buffer.drain(..end + 3);
                self.append_text_to_current(&text);
                continue;
            }

            let Some(end) = crate::jsdom::html_tag_end(&self.buffer) else {
                if self.eof_received {
                    let text = std::mem::take(&mut self.buffer);
                    self.append_text(&text);
                    return self.complete_document();
                }
                return Ok(DocumentParseProgress::Advanced);
            };
            let token = self.buffer[1..end].trim().to_string();
            self.buffer.drain(..end + 1);
            if token
                .get(0.."!doctype".len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("!doctype"))
            {
                self.handle_doctype(&token);
                continue;
            }
            if token.starts_with('?') {
                let instruction = token
                    .strip_prefix('?')
                    .unwrap_or(&token)
                    .strip_suffix('?')
                    .unwrap_or_else(|| token.strip_prefix('?').unwrap_or(&token));
                let split = instruction
                    .find(char::is_whitespace)
                    .unwrap_or(instruction.len());
                let target = &instruction[..split];
                let data = instruction[split..].trim_start_matches(char::is_whitespace);
                let node = crate::dom::create_processing_instruction(target, data);
                if self.fragment_root.is_none() && !self.doctype_seen {
                    crate::dom::insert_before(0, node, self.html);
                } else {
                    append_parser_child(self.current_parent(), node);
                }
                continue;
            }
            if token.starts_with('!') {
                self.parse_errors += 1;
                continue;
            }
            if let Some(name) = token.strip_prefix('/') {
                self.close_element(name.trim());
                continue;
            }
            self.open_element(&token)?;
            if self.paused {
                return Ok(DocumentParseProgress::BlockedOnScript);
            }
        }
    }

    fn current_raw_text_tag(&self) -> Option<String> {
        let node = *self.stack.last()?;
        let tag = crate::dom::tag_name(node);
        let namespace = crate::jsdom::namespace_uri(node);
        if namespace == HTML_NAMESPACE {
            matches!(
                tag.as_str(),
                "script"
                    | "style"
                    | "title"
                    | "textarea"
                    | "xmp"
                    | "iframe"
                    | "noembed"
                    | "noframes"
            )
            .then_some(tag)
        } else if namespace == SVG_NAMESPACE {
            matches!(tag.as_str(), "script" | "style").then_some(tag)
        } else {
            None
        }
    }

    pub(crate) fn current_parent(&self) -> u32 {
        let parent = self.stack.last().copied().unwrap_or(self.body);
        if crate::dom::tag_name(parent) == "template"
            && crate::jsdom::namespace_uri(parent) == HTML_NAMESPACE
        {
            crate::jsdom::ensure_template_content(parent)
        } else {
            parent
        }
    }

    fn append_text_to_current(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let node = crate::dom::create_text_node(text);
        append_parser_child(self.current_parent(), node);
    }

    fn append_raw_text(&self, tag: &str, text: &str) {
        if matches!(tag, "title" | "textarea") {
            self.append_text_to_current(&crate::jsdom::decode_html_entities(text));
        } else if matches!(tag, "script" | "style") {
            let text = strip_cdata_wrapper(text).unwrap_or(text);
            if crate::jsdom::document_value()
                .get_property("contentType")
                .to_js_string()
                == "application/xhtml+xml"
            {
                self.append_text_to_current(&crate::jsdom::decode_html_entities(text));
            } else {
                self.append_text_to_current(text);
            }
        } else {
            self.append_text_to_current(text);
        }
    }

    fn append_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !text.chars().all(char::is_whitespace) {
            self.lock_missing_doctype_mode();
        }
        if self.fragment_root.is_none()
            && self.stack.len() == 1
            && self.section == DocumentInsertionSection::Head
        {
            if text.chars().all(char::is_whitespace) {
                return;
            }
            self.enter_body();
        }
        self.reconstruct_active_formatting();
        let decoded = crate::jsdom::decode_html_entities(text);
        if self.should_foster_parent_text(&decoded) {
            let node = crate::dom::create_text_node(&decoded);
            self.insert_at_foster_parent(node);
        } else {
            self.append_text_to_current(&decoded);
        }
    }

    pub(crate) fn enter_body(&mut self) {
        self.section = DocumentInsertionSection::Body;
        self.stack.clear();
        self.stack.extend([self.html, self.body]);
    }

    fn handle_doctype(&mut self, token: &str) {
        if self.fragment_root.is_some() {
            self.parse_errors += 1;
            return;
        }
        if self.document_mode_locked || self.doctype_seen {
            self.parse_errors += 1;
            return;
        }
        self.register_custom_entities_from_doctype(token);
        let parsed = parse_document_doctype(token);
        if parsed.malformed {
            self.parse_errors += 1;
        }
        crate::jsdom::install_document_doctype(&parsed.name, &parsed.public_id, &parsed.system_id);
        crate::jsdom::set_document_compat_mode(parsed.mode == DocumentCompatibilityMode::Quirks);
        self.compatibility_mode = parsed.mode;
        self.doctype_seen = true;
        self.document_mode_locked = true;
    }

    pub(crate) fn lock_missing_doctype_mode(&mut self) {
        if self.fragment_root.is_some() || self.document_mode_locked {
            return;
        }
        self.parse_errors += 1;
        crate::jsdom::set_document_compat_mode(true);
        self.compatibility_mode = DocumentCompatibilityMode::Quirks;
        self.document_mode_locked = true;
    }
}

fn strip_cdata_wrapper(text: &str) -> Option<&str> {
    text.trim().strip_prefix("<![CDATA[")?.strip_suffix("]]>")
}

pub(crate) fn append_html_fragment_with_streaming_parser(
    parent: u32,
    html: &str,
    sanitize: bool,
) -> Result<()> {
    let mut parser = StreamingDocumentParser::for_fragment(parent, sanitize);
    parser.write(html)?;
    let progress = parser.finish()?;
    if progress != DocumentParseProgress::Complete {
        return Err(anyhow!("inert fragment parser did not reach EOF"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use w3cos_core::Value;

    fn parse_document(source: &str) -> (Value, StreamingDocumentParser) {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let mut parser = StreamingDocumentParser::new_with_script_host(
            Rc::new(InertParserScriptHost),
            "https://example.test/shared-parser.html",
        )
        .expect("create feature-neutral parser");
        parser.write(source).expect("write HTML");
        assert_eq!(
            parser.finish().expect("finish HTML"),
            DocumentParseProgress::Complete
        );
        (crate::jsdom::document_value(), parser)
    }

    #[test]
    fn feature_neutral_parser_preserves_table_and_template_scopes() {
        let (document, _) = parse_document(
            "<!doctype html><body><table id=outer><template id=rows>\
               <tr id=template-row>foster<td>T</td></tr>\
               </table><p id=template-after-close>P</p>\
             </template><tr id=live-row><td>L</td></tr></table>",
        );
        let template = document.call_method("querySelector", vec![Value::string("#rows")]);
        let content = template.get_property("content");
        assert!(
            !content
                .call_method("querySelector", vec![Value::string("#template-row")])
                .is_null()
        );
        assert!(
            !content
                .call_method(
                    "querySelector",
                    vec![Value::string("#template-after-close")]
                )
                .is_null()
        );
        assert!(
            content
                .get_property("textContent")
                .to_js_string()
                .contains("foster")
        );
        assert!(
            document
                .call_method(
                    "querySelector",
                    vec![Value::string("#template-after-close")]
                )
                .is_null()
        );
        let outer = document.call_method("querySelector", vec![Value::string("#outer")]);
        let live_row = document.call_method("querySelector", vec![Value::string("#live-row")]);
        assert_eq!(
            crate::dom::parent_node(crate::jsdom::node_id_of(&live_row).unwrap())
                .and_then(crate::dom::parent_node),
            crate::jsdom::node_id_of(&outer)
        );
    }

    #[test]
    fn feature_neutral_parser_creates_template_table_wrappers() {
        let (document, _) = parse_document(
            "<!doctype html><body>\
             <template id=columns><col id=column></template>\
             <template id=rows><tr id=row><td>R</td></tr></template>\
             <template id=cells><td id=cell>C</td></template>",
        );
        let content = |template_id: &str| {
            document
                .call_method("querySelector", vec![Value::string(template_id)])
                .get_property("content")
        };
        let column =
            content("#columns").call_method("querySelector", vec![Value::string("#column")]);
        assert_eq!(
            crate::dom::parent_node(crate::jsdom::node_id_of(&column).unwrap())
                .map(crate::dom::tag_name)
                .as_deref(),
            Some("colgroup")
        );

        let row = content("#rows").call_method("querySelector", vec![Value::string("#row")]);
        assert_eq!(
            crate::dom::parent_node(crate::jsdom::node_id_of(&row).unwrap())
                .map(crate::dom::tag_name)
                .as_deref(),
            Some("tbody")
        );

        let cell = content("#cells").call_method("querySelector", vec![Value::string("#cell")]);
        let cell_parent = crate::dom::parent_node(crate::jsdom::node_id_of(&cell).unwrap())
            .expect("implicit row");
        assert_eq!(crate::dom::tag_name(cell_parent), "tr");
        assert_eq!(
            crate::dom::parent_node(cell_parent)
                .map(crate::dom::tag_name)
                .as_deref(),
            Some("tbody")
        );
    }

    #[test]
    fn feature_neutral_parser_repairs_formatting_and_foreign_content() {
        let (document, _) = parse_document(
            "<!doctype html><body><b>one<div id=block>two</b>three</div>\
             <svg><lineargradient id=gradient viewbox='0 0 1 1'>\
               <textpath id=path><![CDATA[A&B]]></textpath>\
             </lineargradient>\
             <use id=use xmlns:xlink='http://www.w3.org/1999/xlink' \
               xml:lang=en xlink:href='#shape'/></svg>\
             <math><annotation-xml id=annotation definitionurl=urn:test /></math>",
        );
        let bold = document.call_method("querySelectorAll", vec![Value::string("b")]);
        assert_eq!(bold.get_property("length").to_u32(), 2);
        assert_eq!(
            document
                .call_method("querySelector", vec![Value::string("#block")])
                .get_property("textContent")
                .to_js_string(),
            "twothree"
        );
        let gradient = document.call_method("querySelector", vec![Value::string("#gradient")]);
        assert_eq!(
            gradient.get_property("localName").to_js_string(),
            "linearGradient"
        );
        assert_eq!(
            gradient
                .call_method("getAttribute", vec![Value::string("viewBox")])
                .to_js_string(),
            "0 0 1 1"
        );
        assert_eq!(
            document
                .call_method("querySelector", vec![Value::string("#path")])
                .get_property("textContent")
                .to_js_string(),
            "A&B"
        );
        assert_eq!(
            document
                .call_method("querySelector", vec![Value::string("#annotation")])
                .call_method("getAttribute", vec![Value::string("definitionURL")])
                .to_js_string(),
            "urn:test"
        );

        let use_element = document.call_method("querySelector", vec![Value::string("#use")]);
        assert_eq!(
            use_element
                .call_method(
                    "getAttributeNS",
                    vec![
                        Value::string(crate::html_parser_state::XLINK_NAMESPACE),
                        Value::string("href"),
                    ],
                )
                .to_js_string(),
            "#shape"
        );
        let xlink_href = use_element.get_property("attributes").call_method(
            "getNamedItemNS",
            vec![
                Value::string(crate::html_parser_state::XLINK_NAMESPACE),
                Value::string("href"),
            ],
        );
        assert_eq!(xlink_href.get_property("name").to_js_string(), "xlink:href");
        assert_eq!(xlink_href.get_property("localName").to_js_string(), "href");
        assert_eq!(xlink_href.get_property("prefix").to_js_string(), "xlink");
        assert_eq!(
            xlink_href.get_property("namespaceURI").to_js_string(),
            crate::html_parser_state::XLINK_NAMESPACE
        );
        assert_eq!(
            use_element
                .call_method(
                    "getAttributeNS",
                    vec![
                        Value::string(crate::html_parser_state::XML_NAMESPACE),
                        Value::string("lang"),
                    ],
                )
                .to_js_string(),
            "en"
        );

        use_element.call_method(
            "setAttributeNS",
            vec![
                Value::string("urn:vendor"),
                Value::string("vendor:flag"),
                Value::string("yes"),
            ],
        );
        assert_eq!(
            use_element
                .call_method(
                    "getAttributeNS",
                    vec![Value::string("urn:vendor"), Value::string("flag")],
                )
                .to_js_string(),
            "yes"
        );
        assert!(
            use_element
                .call_method(
                    "hasAttributeNS",
                    vec![Value::string("urn:vendor"), Value::string("flag")],
                )
                .to_bool()
        );
        use_element.call_method(
            "removeAttributeNS",
            vec![Value::string("urn:vendor"), Value::string("flag")],
        );
        assert!(
            !use_element
                .call_method(
                    "hasAttributeNS",
                    vec![Value::string("urn:vendor"), Value::string("flag")],
                )
                .to_bool()
        );
    }

    #[test]
    fn parser_preserves_question_mark_markup_as_processing_instructions() {
        let (document, parser) =
            parse_document("<!doctype html><body><p id=target><?processing data?></p>");
        let instruction = document
            .call_method("querySelector", vec![Value::string("#target")])
            .get_property("firstChild");
        assert_eq!(instruction.get_property("nodeType").to_u32(), 7);
        assert_eq!(
            instruction.get_property("target").to_js_string(),
            "processing"
        );
        assert_eq!(instruction.get_property("data").to_js_string(), "data");
        assert_eq!(parser.parse_error_count(), 0);
    }

    #[test]
    fn feature_neutral_parser_unwraps_xhtml_cdata_in_script_and_style() {
        let (document, _) = parse_document(
            "<!doctype html><head>\
             <style id=style><![CDATA[.target { color: green; }]]></style>\
             <script id=script><![CDATA[const answer = 42;]]></script>\
             </head>",
        );
        let source = |selector: &str| {
            document
                .call_method("querySelector", vec![Value::string(selector)])
                .get_property("textContent")
                .to_js_string()
        };
        assert_eq!(source("#style"), ".target { color: green; }");
        assert_eq!(source("#script"), "const answer = 42;");
    }

    #[test]
    fn xhtml_parser_decodes_entities_in_script_source() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::dom::set_html_document(false);
        crate::jsdom::set_document_content_type("application/xhtml+xml");
        let mut parser = StreamingDocumentParser::new_with_script_host(
            Rc::new(InertParserScriptHost),
            "https://example.test/document.xhtml",
        )
        .expect("create XHTML parser");
        parser
            .write("<html><body><script id='script'>for (let i = 0; i &lt; 2; i++) {}</script></body></html>")
            .expect("write XHTML");
        assert_eq!(
            parser.finish().expect("finish XHTML"),
            DocumentParseProgress::Complete
        );
        let script = crate::jsdom::document_value()
            .call_method("querySelector", vec![Value::string("#script")]);
        assert_eq!(
            script.get_property("textContent").to_js_string(),
            "for (let i = 0; i < 2; i++) {}"
        );
    }

    #[test]
    fn xhtml_parser_expands_internal_general_entities_as_markup() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::dom::set_html_document(false);
        crate::jsdom::set_document_content_type("application/xhtml+xml");
        let mut parser = StreamingDocumentParser::new_with_script_host(
            Rc::new(InertParserScriptHost),
            "https://example.test/document.xhtml",
        )
        .expect("create XHTML parser");
        parser
            .write(
                "<!DOCTYPE html [<!ENTITY tree \"<span id='leaf'>value</span>\">]><html><body><p id='parent'>&tree;</p></body></html>",
            )
            .expect("write XHTML entity document");
        assert_eq!(
            parser.finish().expect("finish XHTML"),
            DocumentParseProgress::Complete
        );
        let parent = crate::jsdom::document_value()
            .call_method("getElementById", vec![Value::string("parent")]);
        let child = parent.get_property("firstElementChild");
        assert_eq!(child.get_property("id").to_js_string(), "leaf");
        assert_eq!(child.get_property("textContent").to_js_string(), "value");
    }

    #[test]
    fn feature_neutral_parser_reports_document_compatibility_mode() {
        let (standards, parser) = parse_document("<!doctype html><p>standards");
        assert_eq!(
            parser.compatibility_mode(),
            DocumentCompatibilityMode::NoQuirks
        );
        assert_eq!(parser.parse_error_count(), 0);
        assert_eq!(
            standards.get_property("compatMode").to_js_string(),
            "CSS1Compat"
        );

        let (quirks, parser) = parse_document("<html><body>missing doctype");
        assert_eq!(
            parser.compatibility_mode(),
            DocumentCompatibilityMode::Quirks
        );
        assert!(parser.parse_error_count() >= 1);
        assert_eq!(
            quirks.get_property("compatMode").to_js_string(),
            "BackCompat"
        );
    }
}
