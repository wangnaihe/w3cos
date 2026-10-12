//! Legacy HTML text alignment is a presentational hint, not a layout override.
use super::{Document, NodeId, NodeType};

pub(super) fn append_alignment_hint(
    document: &Document, id: NodeId, declarations: &mut Vec<(String, String, u32)>,
) {
    let node = document.get_node(id);
    if node.node_type != NodeType::Element || !node.is_html_element
        // These elements give align a different meaning (or keep an
        // independent UA text alignment).
        || matches!(node.tag.as_str().as_str(),
            "table" | "img" | "input" | "iframe" | "hr" | "object" | "embed"
                | "caption" | "button")
    { return; }
    let Some(value) = node.attributes.iter()
        .find(|(name, _)| name.as_str().eq_ignore_ascii_case("align"))
        .map(|(_, value)| {
            if value.as_str().eq_ignore_ascii_case("middle") { "center".into() }
            else { value.as_str().trim().to_ascii_lowercase() }
        })
        .filter(|value: &String| matches!(value.as_str(), "start" | "end" | "left" | "right"
            | "center" | "justify" | "inherit" | "initial" | "unset"))
    else { return; };
    declarations.push(("text-align".into(), value, 0));
}

#[cfg(test)]
mod tests {
    use super::super::Document;
    use w3cos_std::style::TextAlign;

    #[test]
    fn html_alignment_hint_is_inherited_and_author_styles_win() {
        crate::stylesheet::clear_rules();
        for (value, expected) in [("center", TextAlign::Center), (" center ", TextAlign::Center),
            ("middle", TextAlign::Center), ("end", TextAlign::End), ("RIGHT", TextAlign::Right),
            ("left", TextAlign::Left), ("justify", TextAlign::Justify)] {
            let mut document = Document::new();
            let block = document.create_element("div");
            block.set_attribute(&mut document, "align", value);
            let child = document.create_element("span");
            block.append_child(&mut document, child);
            document.body().append_child(&mut document, block);
            assert_eq!(document.computed_style_for(block.id).text_align, expected, "{value}");
            assert_eq!(document.computed_style_for(child.id).text_align, expected, "inherited {value}");
            block.style_mut(&mut document).set_property("text-align", "right");
            assert_eq!(document.computed_style_for(block.id).text_align, TextAlign::Right);
        }
    }

    #[test]
    fn invalid_alignment_and_non_text_alignment_tags_do_not_create_hints() {
        crate::stylesheet::clear_rules();
        for (tag, value) in [("div", "invalid"), ("table", "center"),
            ("img", "right"), ("input", "right")] {
            let mut document = Document::new();
            let element = document.create_element(tag);
            element.set_attribute(&mut document, "align", value);
            document.body().append_child(&mut document, element);
            assert_eq!(document.computed_style_for(element.id).text_align,
                w3cos_std::Style::default().text_align, "{tag} align={value:?}");
        }
    }

    #[test]
    fn alignment_hint_precedes_author_rules_and_user_important() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_user_rule("div", &[("text-align", "right")]);
        let mut document = Document::new();
        let block = document.create_element("div");
        block.set_attribute(&mut document, "align", "center");
        document.body().append_child(&mut document, block);
        assert_eq!(document.computed_style_for(block.id).text_align, TextAlign::Center);
        crate::stylesheet::register_rule("div", &[("text-align", "left")]);
        assert_eq!(document.computed_style_for(block.id).text_align, TextAlign::Left);
        crate::stylesheet::register_user_rule("div", &[("text-align", "right !important")]);
        assert_eq!(document.computed_style_for(block.id).text_align, TextAlign::Right);
        crate::stylesheet::clear_rules();
    }
}
