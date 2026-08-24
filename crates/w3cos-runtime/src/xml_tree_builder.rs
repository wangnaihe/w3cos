//! Namespace-aware XML/SVG document parsing for top-level navigation.

use crate::html_compat::parse_document_doctype;
use crate::html_parser_host::ParserScriptHost;
use crate::html_parser_state::{DocumentParseProgress, XMLNS_NAMESPACE, append_parser_child};
use anyhow::{Result, anyhow};
use quick_xml::NsReader;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use std::collections::HashMap;
use std::rc::Rc;

pub(crate) struct StreamingXmlDocumentParser {
    script_host: Rc<dyn ParserScriptHost>,
    document_url: String,
    source: String,
    parsed: bool,
    complete: bool,
}

impl StreamingXmlDocumentParser {
    pub(crate) fn from_started_navigation(
        script_host: Rc<dyn ParserScriptHost>,
        document_url: &str,
    ) -> Self {
        // Constructing the live Document can materialize the default HTML
        // shell before response MIME selection is known. XML/SVG navigation
        // replaces that shell so document-scoped selectors and script scans
        // are rooted at the XML document element.
        for child in crate::dom::children(0) {
            if crate::dom::node_type(child) == 1 {
                crate::dom::remove_child(0, child);
            }
        }
        Self {
            script_host,
            document_url: document_url.to_string(),
            source: String::new(),
            parsed: false,
            complete: false,
        }
    }

    pub(crate) fn write(&mut self, chunk: &str) -> Result<DocumentParseProgress> {
        if self.complete || self.parsed {
            return Err(anyhow!("cannot write after XML document parser completion"));
        }
        self.source.push_str(chunk);
        Ok(DocumentParseProgress::Advanced)
    }

    pub(crate) fn finish(&mut self) -> Result<DocumentParseProgress> {
        self.drive()
    }

    pub(crate) fn resume(&mut self) -> Result<DocumentParseProgress> {
        self.drive()
    }

    pub(crate) fn is_blocked(&self) -> bool {
        !self.complete && self.script_host.has_pending_parser_blocking_script()
    }

    fn drive(&mut self) -> Result<DocumentParseProgress> {
        if self.complete {
            return Ok(DocumentParseProgress::Complete);
        }
        if self.script_host.has_pending_parser_blocking_script() {
            return Ok(DocumentParseProgress::BlockedOnScript);
        }
        if !self.parsed {
            parse_xml_document(&self.source, self.script_host.as_ref())?;
            self.parsed = true;
        }
        self.script_host
            .execute_pending_document_scripts(&self.document_url)?;
        if self.script_host.has_pending_parser_blocking_script() {
            return Ok(DocumentParseProgress::BlockedOnScript);
        }
        self.script_host.finish_document_parse();
        self.complete = true;
        Ok(DocumentParseProgress::Complete)
    }
}

fn parse_xml_document(source: &str, script_host: &dyn ParserScriptHost) -> Result<()> {
    parse_xml_document_into(source, script_host, 0)
}

pub(crate) fn append_xml_document_fragment(parent: u32, source: &str) -> Result<()> {
    parse_xml_document_into(
        source,
        &crate::html_parser_host::InertParserScriptHost,
        parent,
    )
}

fn parse_xml_document_into(
    source: &str,
    script_host: &dyn ParserScriptHost,
    root_parent: u32,
) -> Result<()> {
    let entities = internal_general_entities(source);
    let expanded = expand_general_entities(source, &entities);
    let mut reader = NsReader::from_str(&expanded);
    reader.config_mut().trim_text(false);
    let mut stack = Vec::new();

    loop {
        let (namespace, event) = reader.read_resolved_event()?;
        let namespace = resolved_namespace(namespace)?;
        let event = event.into_owned();
        match event {
            Event::Start(element) => {
                let node = create_element(&reader, &namespace, &element, script_host)?;
                append_xml_child(&stack, root_parent, node);
                stack.push(node);
            }
            Event::Empty(element) => {
                let node = create_element(&reader, &namespace, &element, script_host)?;
                append_xml_child(&stack, root_parent, node);
            }
            Event::End(_) => {
                stack.pop();
            }
            Event::Text(text) => {
                let decoded = text.xml_content()?;
                let text = quick_xml::escape::unescape(&decoded)?;
                append_text(&stack, &text);
            }
            Event::CData(text) => {
                let decoded = text.xml_content()?;
                if let Some(parent) = stack.last().copied() {
                    append_parser_child(parent, crate::dom::create_cdata_section(&decoded));
                }
            }
            Event::Comment(comment) => {
                let decoded = comment.decode()?;
                let node = crate::dom::create_comment(&decoded);
                append_xml_child(&stack, root_parent, node);
            }
            Event::DocType(doctype) => {
                if root_parent == 0 {
                    let decoded = doctype.decode()?;
                    let external_subset = decoded.split_once('[').map_or(decoded.as_ref(), |v| v.0);
                    let parsed = parse_document_doctype(&format!("!DOCTYPE {external_subset}"));
                    crate::jsdom::install_document_doctype(
                        &parsed.name,
                        &parsed.public_id,
                        &parsed.system_id,
                    );
                }
            }
            Event::GeneralRef(reference) => {
                let name = reference.decode()?;
                if let Some(value) = decoded_general_reference(&name, &entities) {
                    append_text(&stack, &value);
                }
            }
            Event::PI(instruction) => {
                let target = std::str::from_utf8(instruction.target())?;
                let data = std::str::from_utf8(instruction.content())?.trim_start_matches([
                    '\u{0009}', '\u{000a}', '\u{000c}', '\u{000d}', '\u{0020}',
                ]);
                let node = crate::dom::create_processing_instruction(target, data);
                append_xml_child(&stack, root_parent, node);
            }
            Event::Eof => break,
            Event::Decl(_) => {}
        }
    }

    if root_parent == 0
        && crate::dom::children(root_parent)
            .into_iter()
            .all(|node| crate::dom::node_type(node) != 1)
    {
        return Err(anyhow!("XML document has no document element"));
    }
    if root_parent == 0 {
        crate::jsdom::sync_global_document_child_relationships();
    }
    Ok(())
}

fn decoded_general_reference(name: &str, entities: &HashMap<String, String>) -> Option<String> {
    let reference = format!("&{name};");
    let decoded = crate::jsdom::decode_html_entities(&reference);
    if decoded != reference {
        return Some(decoded);
    }
    entities.get(name).cloned()
}

fn create_element(
    reader: &NsReader<&[u8]>,
    namespace: &str,
    element: &BytesStart<'_>,
    script_host: &dyn ParserScriptHost,
) -> Result<u32> {
    let qualified_name = std::str::from_utf8(element.name().as_ref())?.to_string();
    let local_name = std::str::from_utf8(element.local_name().as_ref())?.to_string();
    let stored_name = if local_name == "script" {
        local_name.as_str()
    } else {
        qualified_name.as_str()
    };
    let node = crate::jsdom::create_namespaced_element(namespace, stored_name);

    for attribute in element.attributes() {
        let attribute = attribute?;
        let qualified = std::str::from_utf8(attribute.key.as_ref())?.to_string();
        let value = attribute.decode_and_unescape_value(reader.decoder())?;
        let (namespace, local_name) = if qualified == "xmlns" {
            (Some(XMLNS_NAMESPACE.to_string()), "xmlns".to_string())
        } else if let Some(local_name) = qualified.strip_prefix("xmlns:") {
            (Some(XMLNS_NAMESPACE.to_string()), local_name.to_string())
        } else {
            let (namespace, local_name) = reader.resolve_attribute(attribute.key);
            let namespace = resolved_namespace(namespace)?;
            (
                (!namespace.is_empty()).then_some(namespace),
                std::str::from_utf8(local_name.as_ref())?.to_string(),
            )
        };
        let prefix = qualified.split_once(':').map(|(prefix, _)| prefix);
        crate::jsdom::apply_html_attribute_ns(
            node,
            namespace.as_deref(),
            &qualified,
            prefix,
            &local_name,
            &value,
        );
        if namespace.is_none()
            && let Some(event_type) = qualified
                .strip_prefix("on")
                .filter(|event_type| !event_type.is_empty())
        {
            script_host.register_inline_event_handler(node, event_type, &value);
        }
    }
    Ok(node)
}

fn resolved_namespace(namespace: ResolveResult<'_>) -> Result<String> {
    match namespace {
        ResolveResult::Unbound => Ok(String::new()),
        ResolveResult::Bound(namespace) => Ok(std::str::from_utf8(namespace.as_ref())?.to_string()),
        ResolveResult::Unknown(prefix) => Err(anyhow!(
            "unknown XML namespace prefix {}",
            String::from_utf8_lossy(&prefix)
        )),
    }
}

fn append_xml_child(stack: &[u32], root_parent: u32, child: u32) {
    append_parser_child(stack.last().copied().unwrap_or(root_parent), child);
}

fn append_text(stack: &[u32], text: &str) {
    if let Some(parent) = stack.last().copied()
        && !text.is_empty()
    {
        append_parser_child(parent, crate::dom::create_text_node(text));
    }
}

fn internal_general_entities(source: &str) -> HashMap<String, String> {
    let mut entities = HashMap::new();
    let mut cursor = source;
    while let Some(start) = cursor.find("<!ENTITY") {
        let declaration = cursor[start + "<!ENTITY".len()..].trim_start();
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
        let Some(end) = quoted.find(quote) else {
            break;
        };
        if !name.is_empty() {
            entities.insert(name.to_string(), quoted[..end].to_string());
        }
        cursor = &quoted[end + quote.len_utf8()..];
    }
    entities
}

fn expand_general_entities(source: &str, entities: &HashMap<String, String>) -> String {
    let mut expanded = source.to_string();
    for (name, value) in entities {
        expanded = expanded.replace(&format!("&{name};"), value);
    }
    expanded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html_parser_host::InertParserScriptHost;
    use w3cos_core::Value;

    #[test]
    fn fragment_parser_allows_a_processing_instruction_without_an_element() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let parent = crate::dom::create_document_fragment();

        append_xml_document_fragment(parent, "<?foo attr=\"value\"?>").unwrap();

        let children = crate::dom::children(parent);
        assert_eq!(children.len(), 1);
        assert_eq!(crate::dom::node_type(children[0]), 7);
    }

    #[test]
    fn parses_svg_namespaces_cdata_and_internal_entities() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::dom::set_html_document(false);
        crate::jsdom::set_document_content_type("image/svg+xml");
        let mut parser = StreamingXmlDocumentParser::from_started_navigation(
            Rc::new(InertParserScriptHost),
            "https://example.test/document.svg",
        );
        parser
            .write(
                "<!DOCTYPE svg [<!ENTITY tree \"<tspan id='leaf'>value</tspan>\">]>\
                 <svg xmlns='http://www.w3.org/2000/svg' xmlns:p='urn:pickle'>\
                 <g id='parent'>&tree;<p:dill id='pickle' p:flavor='sour'/></g>\
                 <script><![CDATA[const answer = 42;]]></script></svg>",
            )
            .unwrap();
        assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);

        let document = crate::jsdom::document_value();
        assert_eq!(
            document
                .get_property("documentElement")
                .get_property("localName"),
            Value::string("svg")
        );
        let parent = document.call_method("getElementById", vec![Value::string("parent")]);
        assert_eq!(parent.get_property("childElementCount").to_u32(), 2);
        assert_eq!(
            parent.get_property("firstElementChild").get_property("id"),
            Value::string("leaf")
        );
        let pickle = document.call_method("getElementById", vec![Value::string("pickle")]);
        assert_eq!(pickle.get_property("localName"), Value::string("dill"));
        assert_eq!(
            pickle.get_property("namespaceURI"),
            Value::string("urn:pickle")
        );
        let flavor = pickle.call_method(
            "getAttributeNodeNS",
            vec![Value::string("urn:pickle"), Value::string("flavor")],
        );
        assert_eq!(flavor.get_property("value"), Value::string("sour"));
        assert_eq!(flavor.get_property("prefix"), Value::string("p"));
    }

    #[cfg(feature = "dynamic-js")]
    #[test]
    fn xhtml_parser_executes_inline_scripts_in_the_document_realm() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::dom::set_html_document(false);
        crate::jsdom::set_document_content_type("application/xhtml+xml");
        let loader = crate::dynamic_script::ScriptLoader::new(
            crate::dynamic_script::ScriptPolicy::default(),
        );
        loader
            .begin_document_parse("https://example.test/document.xhtml")
            .unwrap();
        let mut parser = StreamingXmlDocumentParser::from_started_navigation(
            Rc::new(loader),
            "https://example.test/document.xhtml",
        );
        parser
            .write(
                "<?xml-stylesheet href='support/style.css' type='text/css'?>\
                 <!DOCTYPE html PUBLIC 'STAFF' 'staffNS.dtd'>\
                 <html xmlns='http://www.w3.org/1999/xhtml'><head/><body>\
                 <iframe onload='markFrameLoaded()'/>\
                 <script>function markFrameLoaded() { document.documentElement.setAttribute('data-frame-loaded', 'yes'); } for (var i = 0; i &lt; 1; i++) { document.documentElement.setAttribute('data-ran', 'yes'); }</script>\
                 </body></html>",
            )
            .unwrap();
        assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("documentElement")
                .call_method("getAttribute", vec![Value::string("data-ran")]),
            Value::string("yes")
        );
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("documentElement")
                .call_method("getAttribute", vec![Value::string("data-frame-loaded")],),
            Value::string("yes")
        );
        let doctype = crate::jsdom::document_value().get_property("doctype");
        assert_eq!(doctype.get_property("name"), Value::string("html"));
        assert_eq!(doctype.get_property("publicId"), Value::string("STAFF"));
        assert_eq!(
            doctype.get_property("systemId"),
            Value::string("staffNS.dtd")
        );
        let instruction = crate::jsdom::document_value().get_property("firstChild");
        assert_eq!(instruction.get_property("nodeType"), Value::Number(7.0));
        assert_eq!(
            instruction.get_property("target"),
            Value::string("xml-stylesheet")
        );
        assert_eq!(
            instruction.get_property("data"),
            Value::string("href='support/style.css' type='text/css'")
        );
        assert_eq!(
            crate::dom::body_id(),
            crate::jsdom::node_id_of(&crate::jsdom::document_value().get_property("body"))
                .expect("XHTML body")
        );
    }

    #[cfg(feature = "dynamic-js")]
    #[test]
    fn xhtml_body_onload_routes_to_the_window_after_shell_replacement() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::dom::set_html_document(false);
        crate::jsdom::set_document_content_type("application/xhtml+xml");
        let loader = crate::dynamic_script::ScriptLoader::new(
            crate::dynamic_script::ScriptPolicy::default(),
        );
        loader
            .begin_document_parse("https://example.test/onload.xhtml")
            .unwrap();
        let mut parser = StreamingXmlDocumentParser::from_started_navigation(
            Rc::new(loader),
            "https://example.test/onload.xhtml",
        );
        parser
            .write(
                "<html xmlns='http://www.w3.org/1999/xhtml'><head><script>function fixupDOM() { document.body.setAttribute('data-loaded', 'yes'); }</script></head><body onload='fixupDOM()'/></html>",
            )
            .unwrap();
        assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);
        crate::jsdom::drain_microtasks();

        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-loaded")]),
            Value::string("yes")
        );
    }

    #[cfg(feature = "dynamic-js")]
    #[test]
    fn xhtml_generated_and_authored_inline_text_share_one_principal_text_box() {
        w3cos_dom::stylesheet::clear_rules();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::dom::set_html_document(false);
        crate::jsdom::set_document_content_type("application/xhtml+xml");
        let loader = Rc::new(crate::dynamic_script::ScriptLoader::new(
            crate::dynamic_script::ScriptPolicy::default(),
        ));
        loader
            .begin_document_parse("https://example.test/generated.xhtml")
            .unwrap();
        let mut parser = StreamingXmlDocumentParser::from_started_navigation(
            loader.clone(),
            "https://example.test/generated.xhtml",
        );
        parser
            .write(
                "<html xmlns='http://www.w3.org/1999/xhtml'><head>\
                 <style type='text/css'><![CDATA[div { position: relative; color: red; } span { position: absolute; top: 0; left: 0; } .test:before { content: 'TEST &#x46;&#x41;&#x49;&#x4c;'; }]]></style>\
                 </head><body><div><span class='test'>ED</span></div></body></html>",
            )
            .unwrap();
        assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);
        let stylesheet = w3cos_compiler::esm_css::parse_css_source(
            "div { position: relative; color: red; } span { position: absolute; top: 0; left: 0; } .test:before { content: 'TEST &#x46;&#x41;&#x49;&#x4c;'; }",
            "inline <style>",
        );
        assert!(
            stylesheet.rules.iter().any(|rule| {
                rule.selector == ".test:before"
                    && rule.declarations.iter().any(|(property, value)| {
                        property == "content" && value == "'TEST &#x46;&#x41;&#x49;&#x4c;'"
                    })
            }),
            "compiled CSS must retain the XHTML entity spelling: {:#?}",
            stylesheet.rules
        );
        for rule in stylesheet.rules {
            let declarations = rule
                .declarations
                .iter()
                .map(|(property, value): &(String, String)| {
                    (property.as_str(), value.as_str())
                })
                .collect::<Vec<_>>();
            w3cos_dom::stylesheet::register_rule(&rule.selector, &declarations);
        }

        let tree = crate::dom::with_document(|document| document.to_component_tree());
        fn find_generated_run(component: &w3cos_std::Component) -> Option<&w3cos_std::Component> {
            if matches!(
                &component.kind,
                w3cos_std::ComponentKind::Text { content }
                    if content == "TEST &#x46;&#x41;&#x49;&#x4c;ED"
            ) {
                return Some(component);
            }
            component.children.iter().find_map(find_generated_run)
        }
        let principal = find_generated_run(&tree).unwrap_or_else(|| {
            fn collect_text(component: &w3cos_std::Component, output: &mut Vec<String>) {
                if let w3cos_std::ComponentKind::Text { content } = &component.kind {
                    output.push(format!(
                        "{content:?} display={:?} position={:?} color={:?}",
                        component.style.display, component.style.position, component.style.color
                    ));
                }
                for child in &component.children {
                    collect_text(child, output);
                }
            }
            let mut text = Vec::new();
            collect_text(&tree, &mut text);
            panic!("XHTML generated and authored text must shape as one run: {text:#?}")
        });
        assert!(principal.children.is_empty());
        w3cos_dom::stylesheet::clear_rules();
    }

    #[cfg(feature = "dynamic-js")]
    #[test]
    fn xhtml_inline_text_keeps_one_collapsed_space_before_preformatted_after_content() {
        w3cos_dom::stylesheet::clear_rules();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::dom::set_html_document(false);
        crate::jsdom::set_document_content_type("application/xhtml+xml");
        let loader = Rc::new(crate::dynamic_script::ScriptLoader::new(
            crate::dynamic_script::ScriptPolicy::default(),
        ));
        loader
            .begin_document_parse("https://example.test/inline-content.xhtml")
            .unwrap();
        let mut parser = StreamingXmlDocumentParser::from_started_navigation(
            loader,
            "https://example.test/inline-content.xhtml",
        );
        parser
            .write(
                "<html xmlns='http://www.w3.org/1999/xhtml'><head/>\
                 <body><div class='test'><div>\n   This test has failed.\n  </div></div></body></html>",
            )
            .unwrap();
        assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);
        let stylesheet = w3cos_compiler::esm_css::parse_css_source(
            ".test div { display: inline; padding: 0 1em 0 0; background: navy; color: navy; }\
             .test div:after { content: '  \\A'; white-space: pre; }",
            "inline <style>",
        );
        for rule in stylesheet.rules {
            let declarations = rule
                .declarations
                .iter()
                .map(|(property, value): &(String, String)| {
                    (property.as_str(), value.as_str())
                })
                .collect::<Vec<_>>();
            w3cos_dom::stylesheet::register_rule(&rule.selector, &declarations);
        }

        let tree = crate::dom::with_document(|document| document.to_component_tree());
        fn navy_inline(component: &w3cos_std::Component) -> Option<&w3cos_std::Component> {
            if component.style.background == w3cos_std::color::Color::rgb(0, 0, 128) {
                return Some(component);
            }
            component.children.iter().find_map(navy_inline)
        }
        let inline = navy_inline(&tree).expect("navy inline component");
        let text = inline
            .children
            .iter()
            .filter_map(|child| match &child.kind {
                w3cos_std::ComponentKind::Text { content } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(text, ["This test has failed. ", "  \n"]);
        let split_width = inline
            .children
            .iter()
            .map(|child| match &child.kind {
                w3cos_std::ComponentKind::Text { content } => {
                    crate::layout::text_intrinsic_size(content, &child.style).0
                }
                _ => 0.0,
            })
            .sum::<f32>();
        let mut reference_style = inline.style.clone();
        reference_style.padding = w3cos_std::style::Edges::ZERO;
        let reference_width = crate::layout::text_intrinsic_size(
            "This test has failed.\u{00a0}\u{00a0}\u{00a0}",
            &reference_style,
        )
        .0;
        assert!(
            (split_width - reference_width).abs() < 0.01,
            "split generated inline width {split_width} must match the single reference run {reference_width}"
        );
        let flat = crate::layout::pre_flatten(&tree);
        let inline_index = flat
            .iter()
            .position(|node| node.style.background == w3cos_std::color::Color::rgb(0, 0, 128))
            .expect("navy inline layout node");
        let inline_rect = crate::layout::compute(&tree, 800.0, 600.0)
            .unwrap()
            .into_iter()
            .find_map(|(rect, index)| (index == inline_index).then_some(rect))
            .expect("navy inline layout rect");
        let padding = inline.style.padding_lengths();
        assert!(
            (inline_rect.width - split_width - padding.left - padding.right).abs() < 0.01,
            "inline layout width {} must equal content {split_width} plus horizontal padding {}",
            inline_rect.width,
            padding.left + padding.right
        );
        w3cos_dom::stylesheet::clear_rules();

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::dom::set_html_document(false);
        crate::jsdom::set_document_content_type("application/xhtml+xml");
        let loader = Rc::new(crate::dynamic_script::ScriptLoader::new(
            crate::dynamic_script::ScriptPolicy::default(),
        ));
        loader
            .begin_document_parse("https://example.test/inline-reference.xhtml")
            .unwrap();
        let mut parser = StreamingXmlDocumentParser::from_started_navigation(
            loader,
            "https://example.test/inline-reference.xhtml",
        );
        parser
            .write(
                "<!DOCTYPE html PUBLIC '-//W3C//DTD XHTML 1.0 Strict//EN' \
                 'http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd'>\
                 <html xmlns='http://www.w3.org/1999/xhtml'><head/>\
                 <body><div>This test has failed.&nbsp;&nbsp;&nbsp;</div></body></html>",
            )
            .unwrap();
        assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);
        let stylesheet = w3cos_compiler::esm_css::parse_css_source(
            "div { display: inline; padding-right: 1em; background: navy; color: navy; }",
            "inline reference <style>",
        );
        for rule in stylesheet.rules {
            let declarations = rule
                .declarations
                .iter()
                .map(|(property, value): &(String, String)| {
                    (property.as_str(), value.as_str())
                })
                .collect::<Vec<_>>();
            w3cos_dom::stylesheet::register_rule(&rule.selector, &declarations);
        }
        let reference_tree = crate::dom::with_document(|document| document.to_component_tree());
        fn collect_text(component: &w3cos_std::Component, output: &mut String) {
            if let w3cos_std::ComponentKind::Text { content } = &component.kind {
                output.push_str(content);
            }
            for child in &component.children {
                collect_text(child, output);
            }
        }
        let mut reference_text = String::new();
        collect_text(&reference_tree, &mut reference_text);
        assert!(
            reference_text.contains("This test has failed.\u{00a0}\u{00a0}\u{00a0}"),
            "reference NBSP text must survive XHTML lowering: {reference_text:?}"
        );
        let reference_flat = crate::layout::pre_flatten(&reference_tree);
        let reference_index = reference_flat
            .iter()
            .position(|node| node.style.background == w3cos_std::color::Color::rgb(0, 0, 128))
            .expect("navy reference layout node");
        let reference_rect = crate::layout::compute(&reference_tree, 800.0, 600.0)
            .unwrap()
            .into_iter()
            .find_map(|(rect, index)| (index == reference_index).then_some(rect))
            .expect("navy reference layout rect");
        assert!(
            (inline_rect.width - reference_rect.width).abs() < 0.01,
            "generated inline width {} must match reference width {}",
            inline_rect.width,
            reference_rect.width
        );
        assert!(
            (inline_rect.height - reference_rect.height).abs() < 0.01,
            "a terminal generated line break must not add an empty painted inline fragment: generated height {}, reference height {}",
            inline_rect.height,
            reference_rect.height
        );
        w3cos_dom::stylesheet::clear_rules();
    }
}
