//! Framework-neutral HTML user-agent defaults.
//!
//! These declarations are the lowest-priority layer in the CSS cascade.
//! Framework adapters may request the same defaults while they still lower
//! elements directly to native components, but must not define private copies.

use w3cos_std::color::Color;
use w3cos_std::style::{BoxSizing, Display, Edges, FlexDirection, FontStyle, Spacing, Style};

/// Apply W3COS's default HTML presentation to an existing style.
///
/// Call this before author styles so stylesheet and inline declarations keep
/// their normal precedence over the user-agent origin.
pub fn apply_html_default_style(style: &mut Style, local_name: &str) {
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
        "a" | "abbr" | "b" | "br" | "code" | "em" | "i" | "label" | "small" | "span" | "strong" => {
            Display::Inline
        }
        "button" | "img" | "input" | "select" | "textarea" => Display::InlineBlock,
        "table" => Display::Table,
        "caption" => Display::TableCaption,
        "colgroup" => Display::TableColumnGroup,
        "col" => Display::TableColumn,
        "thead" => Display::TableHeaderGroup,
        "tbody" => Display::TableRowGroup,
        "tfoot" => Display::TableFooterGroup,
        "tr" => Display::TableRow,
        "td" | "th" => Display::TableCell,
        _ => Display::Block,
    };

    match local_name {
        "body" => style.margin = Edges::all(8.0),
        "table" => {
            style.border_spacing_x = 2.0;
            style.border_spacing_y = 2.0;
        }
        "td" | "th" => style.padding = Edges::all(1.0),
        "button" => {
            style.box_sizing = BoxSizing::BorderBox;
            style.background = Color::rgb(239, 239, 239);
            style.color = Color::BLACK;
            style.font_size = 13.333_333;
            style.padding = Edges::xy(6.0, 1.0);
            style.border_width = 1.0;
            style.border_color = Color::rgb(118, 118, 118);
            style.border_radius = 2.0;
        }
        "input" | "select" | "textarea" => {
            style.box_sizing = BoxSizing::BorderBox;
            style.background = Color::WHITE;
            style.color = Color::BLACK;
            style.font_size = 13.333_333;
            style.padding = Edges::xy(2.0, 1.0);
            style.border_width = 1.0;
            style.border_color = Color::rgb(118, 118, 118);
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
        "p" => vertical_margin(style, 1.0),
        "b" | "strong" => style.font_weight = 700,
        "em" | "i" => style.font_style = FontStyle::Italic,
        _ => {}
    }
}

/// Return the user-agent style for a standalone HTML element.
pub fn html_default_style(local_name: &str) -> Style {
    let mut style = Style::default();
    style.color = Color::BLACK;
    apply_html_default_style(&mut style, local_name);
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_control_defaults_are_framework_neutral() {
        let input = html_default_style("input");
        let button = html_default_style("button");

        assert_eq!(input.background, Color::WHITE);
        assert_eq!(input.box_sizing, BoxSizing::BorderBox);
        assert_eq!(input.display, Display::InlineBlock);
        assert_eq!(input.padding, Edges::xy(2.0, 1.0));
        assert_eq!(input.border_color, Color::rgb(118, 118, 118));
        assert_eq!(button.background, Color::rgb(239, 239, 239));
        assert_eq!(button.box_sizing, BoxSizing::BorderBox);
        assert_eq!(button.padding, Edges::xy(6.0, 1.0));

        assert_eq!(html_default_style("div").display, Display::Block);
        assert_eq!(html_default_style("div").flex_direction, FlexDirection::Row);
        assert_eq!(html_default_style("body").margin, Edges::all(8.0));
        assert_eq!(html_default_style("p").margin.top, Spacing::Em(1.0));
        assert_eq!(html_default_style("p").margin.bottom, Spacing::Em(1.0));
        assert_eq!(html_default_style("span").display, Display::Inline);
        assert_eq!(html_default_style("br").display, Display::Inline);
        assert_eq!(html_default_style("img").display, Display::InlineBlock);
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
        assert_eq!(html_default_style("th").padding, Edges::all(1.0));
        assert_eq!(html_default_style("script").display, Display::None);
        assert_eq!(html_default_style("style").display, Display::None);
        assert_eq!(html_default_style("head").display, Display::None);
    }

    #[test]
    fn table_uses_the_html_default_border_spacing() {
        let table = html_default_style("table");
        assert_eq!(table.border_spacing_x, 2.0);
        assert_eq!(table.border_spacing_y, 2.0);
    }
}
