//! Framework-neutral HTML user-agent defaults.
//!
//! These declarations are the lowest-priority layer in the CSS cascade.
//! Framework adapters may request the same defaults while they still lower
//! elements directly to native components, but must not define private copies.

use w3cos_std::color::Color;
use w3cos_std::style::{Display, Edges, FlexDirection, FontStyle, Spacing, Style};

/// Apply W3COS's default HTML presentation to an existing style.
///
/// Call this before author styles so stylesheet and inline declarations keep
/// their normal precedence over the user-agent origin.
pub fn apply_html_default_style(style: &mut Style, local_name: &str) {
    let vertical_margin = |style: &mut Style, value: f32| {
        style.margin.top = Spacing::Px(value);
        style.margin.bottom = Spacing::Px(value);
    };

    // CSS initial value. `Style::default()` remains column-oriented for native
    // component ergonomics, so the HTML user-agent origin owns this correction.
    style.flex_direction = FlexDirection::Row;
    style.display = match local_name {
        "a" | "abbr" | "b" | "code" | "em" | "i" | "img" | "label" | "small" | "span"
        | "strong" => Display::Inline,
        "button" | "input" | "select" | "textarea" => Display::InlineBlock,
        _ => Display::Block,
    };

    match local_name {
        "button" => {
            style.background = Color::rgb(239, 239, 239);
            style.color = Color::BLACK;
            style.font_size = 13.333_333;
            style.padding = Edges::xy(6.0, 1.0);
            style.border_width = 1.0;
            style.border_color = Color::rgb(118, 118, 118);
            style.border_radius = 2.0;
        }
        "input" | "select" | "textarea" => {
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
            vertical_margin(style, style.font_size * 0.67);
        }
        "h2" => {
            style.font_size *= 1.5;
            style.font_weight = 700;
            vertical_margin(style, style.font_size * 0.83);
        }
        "h3" => {
            style.font_size *= 1.17;
            style.font_weight = 700;
            vertical_margin(style, style.font_size);
        }
        "p" => vertical_margin(style, style.font_size),
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
        assert_eq!(input.display, Display::InlineBlock);
        assert_eq!(input.padding, Edges::xy(2.0, 1.0));
        assert_eq!(input.border_color, Color::rgb(118, 118, 118));
        assert_eq!(button.background, Color::rgb(239, 239, 239));
        assert_eq!(button.padding, Edges::xy(6.0, 1.0));

        assert_eq!(html_default_style("div").display, Display::Block);
        assert_eq!(html_default_style("div").flex_direction, FlexDirection::Row);
        assert_eq!(html_default_style("span").display, Display::Inline);
    }
}
