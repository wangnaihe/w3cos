//! Framework-neutral HTML user-agent defaults.
//!
//! These declarations are the lowest-priority layer in the CSS cascade.
//! Framework adapters may request the same defaults while they still lower
//! elements directly to native components, but must not define private copies.

use w3cos_std::color::Color;
use w3cos_std::style::{
    AlignSelf, BorderLineStyle, BoxSizing, Display, Edges, FlexDirection, FontStyle, Spacing, Style, UnicodeBidi,
};

/// Distinguish an HTML document's initial standard face from an embedding's
/// unspecified primary face. Explicit author font families still take priority.
pub const HTML_STANDARD_FONT_PROPERTY: &str = "--w3cos-internal-html-standard-font";

/// Inherited content language shared by font selection and text transforms.
/// An empty value explicitly resets an ancestor's language to unknown.
pub const TEXT_LANGUAGE_PROPERTY: &str = "--w3cos-internal-text-language";

/// Used appearance is separate from computed CSS border geometry. HTML theme
/// painting must not apply to a native component merely sharing its kind.
pub const CONTROL_APPEARANCE_PROPERTY: &str = "--w3cos-internal-html-control-appearance";

pub(crate) fn control_appearance<'a>(
    tag: &str, input_type: Option<&str>,
    declarations: impl Iterator<Item = (&'a str, &'a str)>,
) -> &'static str {
    let part = match tag {
        "button" => "button",
        "input" if matches!(input_type.unwrap_or("text").to_ascii_lowercase().as_str(),
            "text" | "search" | "url" | "tel" | "email" | "password") => "textfield",
        _ => return "none",
    };
    let mut automatic = true;
    let mut decorated = false;
    for (property, value) in declarations {
        let property = property.to_ascii_lowercase();
        if matches!(property.as_str(), "appearance" | "-webkit-appearance" | "webkitappearance") {
            match value.trim().to_ascii_lowercase().as_str() {
                "none" | "initial" | "unset" | "inherit" => automatic = false,
                "auto" | "revert" | "revert-layer" => automatic = true,
                _ => {} // Invalid values must not override a valid declaration.
            }
        }
        // Blink's author background/border flags turn off automatic painting.
        // Color, typography, dimensions and border-radius alone do not.
        decorated |= matches!(property.as_str(), "background" | "background-color" | "backgroundcolor" | "background-image" | "backgroundimage" | "border")
            || (property.starts_with("border-") && !property.contains("radius") && !property.contains("image"));
        decorated |= part == "textfield" && matches!(property.as_str(), "box-shadow" | "boxshadow")
            && !value.trim().eq_ignore_ascii_case("none");
    }
    if automatic && !decorated { part } else { "none" }
}

/// Apply W3COS's default HTML presentation to an existing style.
///
/// Call this before author styles so stylesheet and inline declarations keep
/// their normal precedence over the user-agent origin.
pub fn apply_html_default_style(style: &mut Style, local_name: &str) {
    // XML keeps qualified element names in the DOM. Defaults must use the
    // same local name as component lowering, not treat `svg:svg` as unknown.
    let local_name = local_name
        .rsplit_once(':')
        .map_or(local_name, |(_, name)| name);
    let vertical_margin = |style: &mut Style, em: f32| {
        style.margin.top = Spacing::Em(em);
        style.margin.bottom = Spacing::Em(em);
    };

    // CSS initial value. `Style::default()` remains column-oriented for native
    // component ergonomics, so the HTML user-agent origin owns this correction.
    style.flex_direction = FlexDirection::Row;
    style.display = match local_name {
        "base" | "head" | "link" | "meta" | "noembed" | "noframes" | "param" | "script"
        | "style" | "template" | "title" => Display::None,
        "a" | "abbr" | "b" | "bdi" | "bdo" | "br" | "code" | "del" | "em" | "i" | "iframe" | "ins" | "label" | "object"
        | "small" | "span" | "strong" | "u" => Display::Inline,
        "button" | "canvas" | "img" | "input" | "select" | "svg" | "textarea" | "video" => {
            Display::InlineBlock
        }
        "table" => Display::Table,
        "caption" => Display::TableCaption,
        "colgroup" => Display::TableColumnGroup,
        "col" => Display::TableColumn,
        "thead" => Display::TableHeaderGroup,
        "tbody" => Display::TableRowGroup,
        "tfoot" => Display::TableFooterGroup,
        "tr" => Display::TableRow,
        "td" | "th" => Display::TableCell,
        "li" => Display::ListItem,
        _ => Display::Block,
    };

    if matches!(local_name, "input" | "button" | "select" | "textarea") {
        // The UA control font is a declaration, not inherited document text.
        // Author font declarations (including explicit inherit) run later.
        style.font_family = Some("Arial".into());
        style.font_weight = 400;
        style.font_style = FontStyle::Normal;
        style.line_height_is_normal = true;
        style.line_height_computed_px = None;
    }

    match local_name {
        "bdo" => style.unicode_bidi = UnicodeBidi::BidiOverride,
        "bdi" => style.unicode_bidi = UnicodeBidi::Isolate,
        // The initial browser font is the host's standard face, not an
        // explicit generic serif declaration. Retain that distinction through
        // inheritance without replacing authored font-family declarations.
        "html" => {
            style.custom_properties.get_or_insert_with(Default::default)
                .insert(HTML_STANDARD_FONT_PROPERTY.into(), "1".into());
        }
        "body" => style.margin = Edges::all(8.0),
        "iframe" => {
            // HTML frames are inline replaced elements. Component lowering
            // makes the host atomic, keeping its nested document isolated.
            // The UA border surrounds, rather than replaces,300x150 content.
            style.border_width = 2.0;
            style.border_styles = [Some(BorderLineStyle::Inset); 4];
            style.border_current_color = Some([true; 4]);
        }
        "table" => {
            style.border_spacing_x = 2.0;
            style.border_spacing_y = 2.0;
            style.box_sizing = w3cos_std::style::BoxSizing::BorderBox;
            style.border_color = Color::rgb(128,128,128);
            style.border_current_color = Some([false;4]);
        }
        "caption" => style.text_align = w3cos_std::style::TextAlign::Center,
        "td" | "th" => {
            style.padding = Edges::all(1.0);
            style.align_self = AlignSelf::Center;
            if local_name == "th" {
                style.font_weight = 700;
                style.text_align = w3cos_std::style::TextAlign::Center;
            }
        }
        "button" => {
            style.box_sizing = BoxSizing::BorderBox;
            style.background = Color::rgb(239, 239, 239);
            style.color = Color::BLACK;
            style.font_size = 13.333_333;
            style.padding = Edges::xy(6.0, 1.0);
            style.border_width = 2.0;
            style.border_color = Color::rgb(118, 118, 118);
            style.border_current_color = Some([false; 4]);
            style.border_radius = 2.0;
        }
        "input" | "textarea" => {
            if local_name == "textarea" {
                style.font_family = Some("monospace".into());
            }
            style.background = Color::WHITE;
            style.color = Color::BLACK;
            style.font_size = 13.333_333;
            style.padding = Edges::xy(2.0, 1.0);
            style.border_width = if local_name == "input" { 2.0 } else { 1.0 };
            style.border_color = Color::rgb(118, 118, 118);
            style.border_current_color = Some([false; 4]);
            style.border_radius = 0.0;
        }
        "select" => {
            style.box_sizing = BoxSizing::BorderBox;
            style.background = Color::WHITE;
            style.color = Color::BLACK;
            style.font_size = 13.333_333;
            style.padding = Edges::xy(2.0, 1.0);
            style.border_width = 1.0;
            style.border_color = Color::rgb(118, 118, 118);
            style.border_current_color = Some([false; 4]);
            style.border_radius = 2.0;
        }
        "h1" => {
            style.font_size *= 2.0;
            style.font_weight = 700;
            vertical_margin(style, 0.67);
        }
        "h2" => {
            style.font_size *= 1.5;
            style.font_weight = 700;
            vertical_margin(style, 0.83);
        }
        "h3" => {
            style.font_size *= 1.17;
            style.font_weight = 700;
            vertical_margin(style, 1.0);
        }
        "h4" => {
            style.font_weight = 700;
            vertical_margin(style, 1.33);
        }
        "h5" => {
            style.font_size *= 0.83;
            style.font_weight = 700;
            vertical_margin(style, 1.67);
        }
        "h6" => {
            style.font_size *= 0.67;
            style.font_weight = 700;
            vertical_margin(style, 2.33);
        }
        "p" => vertical_margin(style, 1.0),
        "ul" | "ol" => {
            vertical_margin(style, 1.0);
            style.padding.left = Spacing::Em(2.5);
        }
        "pre" => {
            style.white_space = w3cos_std::style::WhiteSpace::Pre;
            style.font_family = Some("monospace".to_string());
            vertical_margin(style, 1.0);
        }
        "b" | "strong" => style.font_weight = 700,
        "em" | "i" | "address" => style.font_style = FontStyle::Italic,
        "blockquote" => {
            vertical_margin(style, 1.0);
            style.margin.left = Spacing::Px(40.0);
            style.margin.right = Spacing::Px(40.0);
        }
        "ins" | "u" => style.text_decoration = w3cos_std::style::TextDecoration::Underline,
        "del" => style.text_decoration = w3cos_std::style::TextDecoration::LineThrough,
        _ => {}
    }
}

/// Return the user-agent style for a standalone HTML element.
pub fn html_default_style(local_name: &str) -> Style {
    let mut style = Style::default();
    style.line_height_is_normal = true;
    style.color = Color::BLACK;
    apply_html_default_style(&mut style, local_name);
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modification_elements_are_inline_with_ua_text_decoration() {
        for (tag, decoration) in [("ins", w3cos_std::style::TextDecoration::Underline),
            ("del", w3cos_std::style::TextDecoration::LineThrough)] {
            let style = html_default_style(tag);
            assert_eq!(style.display, Display::Inline, "{tag} must not create a block box");
            assert_eq!(style.text_decoration, decoration, "{tag} UA decoration");
        }
    }

    #[test]
    fn initial_document_font_is_distinct_from_explicit_serif() {
        let root = html_default_style("html");
        assert_eq!(root.font_family, None,
            "HTML's initial standard font must not become an explicit serif declaration");
        assert_eq!(root.custom_properties.as_ref().unwrap()
            .get(HTML_STANDARD_FONT_PROPERTY).map(String::as_str), Some("1"));
    }

    #[test]
    fn form_control_defaults_are_framework_neutral() {
        let input = html_default_style("input");
        let button = html_default_style("button");

        assert_eq!(input.background, Color::WHITE);
        assert_eq!(input.box_sizing, BoxSizing::ContentBox);
        assert_eq!(input.display, Display::InlineBlock);
        assert_eq!(input.padding, Edges::xy(2.0, 1.0));
        assert_eq!(input.border_color, Color::rgb(118, 118, 118));
        assert_eq!(input.border_radius, 0.0);
        assert_eq!(button.background, Color::rgb(239, 239, 239));
        assert_eq!(button.box_sizing, BoxSizing::BorderBox);
        assert_eq!(button.padding, Edges::xy(6.0, 1.0));
        assert_eq!(
            html_default_style("textarea").box_sizing,
            BoxSizing::ContentBox
        );
        assert_eq!(
            html_default_style("select").box_sizing,
            BoxSizing::BorderBox
        );

        assert_eq!(html_default_style("div").display, Display::Block);
        assert_eq!(html_default_style("div").flex_direction, FlexDirection::Row);
        assert_eq!(
            html_default_style("html").font_family.as_deref(),
            None
        );
        assert_eq!(html_default_style("body").margin, Edges::all(8.0));
        assert_eq!(html_default_style("p").margin.top, Spacing::Em(1.0));
        assert_eq!(html_default_style("p").margin.bottom, Spacing::Em(1.0));
        assert_eq!(html_default_style("span").display, Display::Inline);
        assert_eq!(html_default_style("li").display, Display::ListItem);
        assert_eq!(html_default_style("bdo").display, Display::Inline);
        assert_eq!(
            html_default_style("bdo").unicode_bidi,
            UnicodeBidi::BidiOverride
        );
        assert_eq!(html_default_style("bdi").display, Display::Inline);
        assert_eq!(html_default_style("bdi").unicode_bidi, UnicodeBidi::Isolate);
        assert_eq!(html_default_style("br").display, Display::Inline);
        assert_eq!(html_default_style("img").display, Display::InlineBlock);
        assert_eq!(html_default_style("svg").display, Display::InlineBlock);
        assert_eq!(html_default_style("canvas").display, Display::InlineBlock);
        assert_eq!(html_default_style("video").display, Display::InlineBlock);
        assert_eq!(html_default_style("table").display, Display::Table);
        assert_eq!(
            html_default_style("thead").display,
            Display::TableHeaderGroup
        );
        assert_eq!(html_default_style("tbody").display, Display::TableRowGroup);
        assert_eq!(
            html_default_style("tfoot").display,
            Display::TableFooterGroup
        );
        assert_eq!(html_default_style("tr").display, Display::TableRow);
        assert_eq!(html_default_style("td").display, Display::TableCell);
        assert_eq!(html_default_style("td").padding, Edges::all(1.0));
        assert_eq!(html_default_style("td").align_self, AlignSelf::Center);
        assert_eq!(html_default_style("th").padding, Edges::all(1.0));
        assert_eq!(html_default_style("th").font_weight, 700);
        assert_eq!(html_default_style("td").font_weight, 400);
        assert_eq!(html_default_style("script").display, Display::None);
        assert_eq!(html_default_style("style").display, Display::None);
        assert_eq!(html_default_style("head").display, Display::None);
    }

    #[test]
    fn iframe_defaults_are_inline_with_inset_border_and_author_overrides() {
        use w3cos_std::style::BorderLineStyle;
        let style = html_default_style("iframe");
        assert_eq!(style.display, Display::Inline, "Chromium141 iframe computed display");
        assert_eq!(style.border_width, 2.0, "default frame border surrounds300x150 content");
        assert_eq!(style.border_styles, [Some(BorderLineStyle::Inset); 4]);
        assert_eq!(style.border_current_color, Some([true; 4]));
        let mut document = crate::document::Document::new();
        let frame = document.create_element("iframe");
        frame.style_mut(&mut document).set_property("border", "0");
        frame.style_mut(&mut document).set_property("display", "block");
        document.body().append_child(&mut document, frame);
        let authored = document.computed_style_for(frame.id);
        assert_eq!(authored.display, Display::Block, "author display overrides UA");
        assert_eq!(authored.border_width, 0.0, "author border overrides UA");
    }

    #[test]
    fn html_list_defaults_use_em_spacing() {
        for tag in ["ul", "ol"] {
            let style = html_default_style(tag);
            assert_eq!(style.margin.top, Spacing::Em(1.0));
            assert_eq!(style.margin.bottom, Spacing::Em(1.0));
            assert_eq!(style.padding.left, Spacing::Em(2.5));
        }
    }

    #[test]
    fn table_uses_the_html_default_border_spacing() {
        let table = html_default_style("table");
        assert_eq!(table.border_spacing_x, 2.0);
        assert_eq!(table.border_spacing_y, 2.0);
    }

    #[test]
    fn table_header_keeps_its_ua_weight_when_display_is_inline() {
        let mut document = crate::document::Document::new();
        let table = document.create_element("table");
        let th = document.create_element("th");
        th.style_mut(&mut document)
            .set_property("display", "inline");
        table.append_child(&mut document, th);
        document.body().append_child(&mut document, table);
        assert_eq!(document.computed_style_for(th.id).font_weight, 700);
        th.style_mut(&mut document)
            .set_property("font-weight", "normal");
        assert_eq!(document.computed_style_for(th.id).font_weight, 400);
    }

    #[test]
    fn caption_default_alignment_is_center_unless_author_overrides() {
        use w3cos_std::style::TextAlign;
        crate::stylesheet::clear_rules();
        for (parent_align, child_align, expected) in [
            ("", "", TextAlign::Center), ("left", "", TextAlign::Center),
            ("right", "", TextAlign::Center), ("right", "inherit", TextAlign::Right),
            ("left", "unset", TextAlign::Left), ("", "left", TextAlign::Left),
        ] {
            let mut document = crate::document::Document::new();
            let table = document.create_element("table");
            if !parent_align.is_empty() { table.style_mut(&mut document).set_property("text-align", parent_align); }
            let caption = document.create_element("caption");
            if !child_align.is_empty() { caption.style_mut(&mut document).set_property("text-align", child_align); }
            table.append_child(&mut document, caption);
            document.body().append_child(&mut document, table);
            assert_eq!(document.computed_style_for(caption.id).text_align, expected,
                "parent={parent_align:?}, child={child_align:?}");
        }
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn table_header_default_alignment_respects_author_inheritance() {
        use w3cos_std::style::TextAlign;
        crate::stylesheet::clear_rules();
        for (parent_align, child_align, expected) in [
            ("", "", TextAlign::Center), ("start", "", TextAlign::Center),
            ("left", "", TextAlign::Left), ("right", "", TextAlign::Right),
            ("", "inherit", TextAlign::Start), ("", "unset", TextAlign::Start),
            ("", "left", TextAlign::Left),
        ] {
            let mut document = crate::document::Document::new();
            let table = document.create_element("table");
            if !parent_align.is_empty() { table.style_mut(&mut document).set_property("text-align", parent_align); }
            let th = document.create_element("th");
            if !child_align.is_empty() { th.style_mut(&mut document).set_property("text-align", child_align); }
            table.append_child(&mut document, th);
            document.body().append_child(&mut document, table);
            assert_eq!(document.computed_style_for(th.id).text_align, expected,
                "parent={parent_align:?}, child={child_align:?}");
        }
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn anonymous_table_header_row_preserves_html_font_context() {
        crate::stylesheet::clear_rules();
        let mut document = crate::document::Document::new();
        let body = document.body();
        body.style_mut(&mut document).set_property(HTML_STANDARD_FONT_PROPERTY, "1");
        let table = document.create_element("table");
        let tr = document.create_element("tr");
        let th = document.create_element("th");
        let text = document.create_text_node("Test");
        th.append_child(&mut document, text);
        tr.append_child(&mut document, th);
        table.append_child(&mut document, tr);
        body.append_child(&mut document, table);
        let tree = document.to_component_tree();
        fn check(component: &w3cos_std::Component, seen: &mut usize) {
            if component.style.custom_properties.as_ref().is_some_and(|properties|
                properties.contains_key("--w3cos-internal-inline-formatting-context"))
            {
                *seen += 1;
                assert_eq!(component.style.custom_properties.as_ref()
                    .and_then(|properties| properties.get(HTML_STANDARD_FONT_PROPERTY)).map(String::as_str),
                    Some("1"), "anonymous line strut must use the same HTML face as its text");
            }
            for child in &component.children { check(child, seen); }
        }
        let mut seen = 0;
        check(&tree, &mut seen);
        assert!(seen > 0, "test must exercise an anonymous inline row");
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn pre_preserves_whitespace_at_user_agent_origin() {
        let pre = html_default_style("pre");
        assert_eq!(pre.white_space, w3cos_std::style::WhiteSpace::Pre);
        assert_eq!(pre.font_family.as_deref(), Some("monospace"));
        assert_eq!(pre.margin.top, Spacing::Em(1.0));
        assert_eq!(pre.margin.bottom, Spacing::Em(1.0));
    }

    #[test]
    fn pre_computed_style_keeps_defaults_and_accepts_author_inheritance() {
        let mut document = crate::document::Document::new();
        let pre = document.create_element("pre");
        document.body().append_child(&mut document, pre);
        let style = document.computed_style_for(pre.id);
        assert_eq!(style.white_space, w3cos_std::style::WhiteSpace::Pre);
        assert_eq!(style.font_family.as_deref(), Some("monospace"));
        pre.style_mut(&mut document)
            .set_property("white-space", "inherit");
        pre.style_mut(&mut document)
            .set_property("font-family", "inherit");
        let inherited = document.computed_style_for(pre.id);
        let parent = document.computed_style_for(document.body().id);
        assert_eq!(inherited.white_space, parent.white_space);
        assert_eq!(inherited.font_family, parent.font_family);
    }

    /// The `font` shorthand carries `font-family` too, so `font: inherit` has
    /// to reach it as well. `fonts-010` is `div { font: 1.25em/1 Ahem }` with
    /// `pre { font: inherit }`: the size inherited but the face did not, so the
    /// `pre` kept the UA monospace.
    #[test]
    fn pre_accepts_author_inheritance_through_the_font_shorthand() {
        let mut document = crate::document::Document::new();
        let div = document.create_element("div");
        document.body().append_child(&mut document, div);
        let pre = document.create_element("pre");
        div.append_child(&mut document, pre);
        div.style_mut(&mut document)
            .set_property("font", "20px/1 Ahem");
        pre.style_mut(&mut document).set_property("font", "inherit");
        let inherited = document.computed_style_for(pre.id);
        assert_eq!(inherited.font_family.as_deref(), Some("Ahem"));
        assert_eq!(inherited.font_size, 20.0);
        // `white-space` is not part of the shorthand, so `pre` keeps it.
        assert_eq!(inherited.white_space, w3cos_std::style::WhiteSpace::Pre);
    }

    #[test]
    fn quotation_default_margins_keep_author_overrides() {
        let style = html_default_style("blockquote");
        assert_eq!(style.margin.left, Spacing::Px(40.0));
        assert_eq!(style.margin.right, Spacing::Px(40.0));
        assert_eq!(style.margin.top, Spacing::Em(1.0));
        assert_eq!(style.margin.bottom, Spacing::Em(1.0));
        let mut document = crate::document::Document::new();
        let quotation = document.create_element("blockquote");
        document.body().append_child(&mut document, quotation);
        quotation.style_mut(&mut document).set_property("margin", "0");
        assert_eq!(document.computed_style_for(quotation.id).margin, Edges::ZERO);
    }

    #[test]
    fn address_default_italic_reaches_descendants_and_keeps_author_overrides() {
        let mut document = crate::document::Document::new();
        let address = document.create_element("address");
        let span = document.create_element("span");
        address.append_child(&mut document, span);
        document.body().append_child(&mut document, address);
        assert_eq!(document.computed_style_for(address.id).font_style, FontStyle::Italic);
        assert_eq!(document.computed_style_for(span.id).font_style, FontStyle::Italic);
        address.style_mut(&mut document).set_property("font-style", "normal");
        assert_eq!(document.computed_style_for(span.id).font_style, FontStyle::Normal);
    }

    #[test]
    fn lower_heading_levels_have_ua_font_weight_size_and_margins() {
        for (tag, scale, margin) in [("h4", 1.0, 1.33), ("h5", 0.83, 1.67), ("h6", 0.67, 2.33)] {
            let mut style = Style::default();
            apply_html_default_style(&mut style, tag);
            assert_eq!(style.font_weight, 700, "{tag} UA weight");
            assert!((style.font_size - 16.0 * scale).abs() < 0.0001, "{tag} UA size");
            assert_eq!(style.margin.top, Spacing::Em(margin), "{tag} UA margin");
            assert_eq!(style.margin.bottom, Spacing::Em(margin));
        }
        let mut document = crate::document::Document::new();
        let heading = document.create_element("h4");
        let span = document.create_element("span");
        heading.append_child(&mut document, span);
        document.body().append_child(&mut document, heading);
        assert_eq!(document.computed_style_for(span.id).font_weight, 700);
        heading.style_mut(&mut document).set_property("font-weight", "normal");
        heading.style_mut(&mut document).set_property("font-size", "20px");
        assert_eq!(document.computed_style_for(span.id).font_weight, 400);
        assert_eq!(document.computed_style_for(span.id).font_size, 20.0);
    }

    #[test]
    fn heading_lowering_keeps_author_font_weight_and_size() {
        let mut document = crate::document::Document::new();
        let heading = document.create_element("h4");
        heading.set_text_content(&mut document, "authored heading");
        heading.style_mut(&mut document).set_property("font-weight", "normal");
        heading.style_mut(&mut document).set_property("font-size", "16px");
        document.body().append_child(&mut document, heading);
        fn find(component: &w3cos_std::Component) -> Option<&Style> {
            if matches!(&component.kind, w3cos_std::ComponentKind::Text { content }
                if content == "authored heading") { return Some(&component.style); }
            component.children.iter().find_map(find)
        }
        let tree = document.to_component_tree();
        let style = find(&tree).expect("heading text must be lowered");
        assert_eq!(style.font_weight, 400, "lowering must not restore UA bold over author normal");
        assert_eq!(style.font_size, 16.0, "lowering must retain authored 16px");
    }

    #[test]
    fn negative_indent_pre_line_items_keep_the_preserved_newline() {
        fn has_newline(component: &w3cos_std::Component) -> bool {
            matches!(&component.kind, w3cos_std::ComponentKind::Text { content }
                if content.contains('\n'))
                || component.children.iter().any(has_newline)
        }
        let mut document = crate::document::Document::new();
        let pre = document.create_element("pre");
        pre.style_mut(&mut document)
            .set_property("text-indent", "-36px");
        for (i, width) in [60, 12].into_iter().enumerate() {
            if i == 1 {
                let newline = document.create_text_node("\n");
                pre.append_child(&mut document, newline);
            }
            let span = document.create_element("span");
            span.style_mut(&mut document)
                .set_property("display", "inline-block");
            span.style_mut(&mut document)
                .set_property("width", &format!("{width}px"));
            span.style_mut(&mut document).set_property("height", "12px");
            pre.append_child(&mut document, span);
        }
        document.body().append_child(&mut document, pre);
        assert!(has_newline(&document.to_component_tree()));
    }
}
