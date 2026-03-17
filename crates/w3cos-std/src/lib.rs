pub mod color;
pub mod component;
pub mod style;

pub use color::Color;
pub use component::{Component, ComponentKind};
pub use style::Style;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{Dimension, Style};

    // --- Color tests ---

    #[test]
    fn color_from_hex_3_char_shorthand() {
        // #fff -> #ffffff (white)
        let c = Color::from_hex("#fff");
        assert_eq!(c.r, 255);
        assert_eq!(c.g, 255);
        assert_eq!(c.b, 255);
        assert_eq!(c.a, 255);

        // #f00 -> red
        let c = Color::from_hex("#f00");
        assert_eq!(c.r, 255);
        assert_eq!(c.g, 0);
        assert_eq!(c.b, 0);

        // #0f0 -> green
        let c = Color::from_hex("#0f0");
        assert_eq!(c.r, 0);
        assert_eq!(c.g, 255);
        assert_eq!(c.b, 0);

        // #00f -> blue
        let c = Color::from_hex("#00f");
        assert_eq!(c.r, 0);
        assert_eq!(c.g, 0);
        assert_eq!(c.b, 255);
    }

    #[test]
    fn color_from_hex_6_char() {
        let c = Color::from_hex("#ffffff");
        assert_eq!(c.r, 255);
        assert_eq!(c.g, 255);
        assert_eq!(c.b, 255);
        assert_eq!(c.a, 255);

        let c = Color::from_hex("#000000");
        assert_eq!(c.r, 0);
        assert_eq!(c.g, 0);
        assert_eq!(c.b, 0);

        let c = Color::from_hex("ff0000");
        assert_eq!(c.r, 255);
        assert_eq!(c.g, 0);
        assert_eq!(c.b, 0);
    }

    #[test]
    fn color_from_hex_8_char_rrggbbaa() {
        // Red with 50% opacity
        let c = Color::from_hex("#ff000080");
        assert_eq!(c.r, 255);
        assert_eq!(c.g, 0);
        assert_eq!(c.b, 0);
        assert_eq!(c.a, 128);

        // Fully transparent
        let c = Color::from_hex("#ffffff00");
        assert_eq!(c.r, 255);
        assert_eq!(c.g, 255);
        assert_eq!(c.b, 255);
        assert_eq!(c.a, 0);

        // Opaque white
        let c = Color::from_hex("#ffffffff");
        assert_eq!(c.r, 255);
        assert_eq!(c.g, 255);
        assert_eq!(c.b, 255);
        assert_eq!(c.a, 255);
    }

    #[test]
    fn color_named_colors() {
        let white = Color::from_named("white").unwrap();
        assert_eq!(white.r, 255);
        assert_eq!(white.g, 255);
        assert_eq!(white.b, 255);

        let black = Color::from_named("black").unwrap();
        assert_eq!(black.r, 0);
        assert_eq!(black.g, 0);
        assert_eq!(black.b, 0);

        let red = Color::from_named("red").unwrap();
        assert_eq!(red.r, 255);
        assert_eq!(red.g, 0);
        assert_eq!(red.b, 0);

        let blue = Color::from_named("blue").unwrap();
        assert_eq!(blue.r, 0);
        assert_eq!(blue.g, 0);
        assert_eq!(blue.b, 255);

        assert!(Color::from_named("unknown").is_none());
    }

    #[test]
    fn color_rgba() {
        let c = Color::rgba(100, 150, 200, 128);
        assert_eq!(c.r, 100);
        assert_eq!(c.g, 150);
        assert_eq!(c.b, 200);
        assert_eq!(c.a, 128);

        let c = Color::rgba(0, 0, 0, 0);
        assert_eq!(c.a, 0);
    }

    #[test]
    fn color_constants() {
        assert_eq!(Color::WHITE.r, 255);
        assert_eq!(Color::WHITE.g, 255);
        assert_eq!(Color::WHITE.b, 255);
        assert_eq!(Color::WHITE.a, 255);

        assert_eq!(Color::BLACK.r, 0);
        assert_eq!(Color::BLACK.g, 0);
        assert_eq!(Color::BLACK.b, 0);

        assert_eq!(Color::TRANSPARENT.a, 0);
    }

    // --- Style tests ---

    #[test]
    fn style_default() {
        let style = Style::default();
        assert!(matches!(style.display, crate::style::Display::Flex));
        assert!(matches!(style.position, crate::style::Position::Relative));
        assert!(matches!(
            style.flex_direction,
            crate::style::FlexDirection::Column
        ));
        assert!(matches!(
            style.justify_content,
            crate::style::JustifyContent::FlexStart
        ));
        assert!(matches!(
            style.align_items,
            crate::style::AlignItems::Stretch
        ));
        assert_eq!(style.flex_grow, 0.0);
        assert_eq!(style.flex_shrink, 1.0);
        assert_eq!(style.gap, 0.0);
        assert_eq!(style.font_size, 16.0);
        assert_eq!(style.font_weight, 400);
        assert_eq!(style.opacity, 1.0);
        assert_eq!(style.background.r, 0);
        assert_eq!(style.background.a, 0);
        assert_eq!(style.color.r, 255);
        assert_eq!(style.color.g, 255);
        assert_eq!(style.color.b, 255);
    }

    #[test]
    fn dimension_variants() {
        let auto = Dimension::Auto;
        assert!(matches!(auto, Dimension::Auto));

        let px = Dimension::Px(100.0);
        assert!(matches!(px, Dimension::Px(v) if v == 100.0));

        let percent = Dimension::Percent(50.0);
        assert!(matches!(percent, Dimension::Percent(v) if v == 50.0));

        let rem = Dimension::Rem(1.5);
        assert!(matches!(rem, Dimension::Rem(v) if v == 1.5));

        let em = Dimension::Em(2.0);
        assert!(matches!(em, Dimension::Em(v) if v == 2.0));

        let vw = Dimension::Vw(10.0);
        assert!(matches!(vw, Dimension::Vw(v) if v == 10.0));

        let vh = Dimension::Vh(25.0);
        assert!(matches!(vh, Dimension::Vh(v) if v == 25.0));
    }

    #[test]
    fn dimension_resolve() {
        let root_font = 16.0;
        let local_font = 14.0;
        let parent_w = 200.0;
        let viewport_w = 1920.0;
        let viewport_h = 1080.0;

        assert!(
            Dimension::Auto
                .resolve(parent_w, root_font, local_font, viewport_w, viewport_h)
                .is_none()
        );

        assert_eq!(
            Dimension::Px(50.0).resolve(parent_w, root_font, local_font, viewport_w, viewport_h),
            Some(50.0)
        );

        assert_eq!(
            Dimension::Percent(50.0)
                .resolve(parent_w, root_font, local_font, viewport_w, viewport_h),
            Some(100.0)
        );

        assert_eq!(
            Dimension::Rem(2.0).resolve(parent_w, root_font, local_font, viewport_w, viewport_h),
            Some(32.0)
        );

        assert_eq!(
            Dimension::Em(1.5).resolve(parent_w, root_font, local_font, viewport_w, viewport_h),
            Some(21.0)
        );

        assert_eq!(
            Dimension::Vw(10.0).resolve(parent_w, root_font, local_font, viewport_w, viewport_h),
            Some(192.0)
        );

        assert_eq!(
            Dimension::Vh(50.0).resolve(parent_w, root_font, local_font, viewport_w, viewport_h),
            Some(540.0)
        );
    }

    // --- Component tests ---

    #[test]
    fn component_text() {
        let style = Style::default();
        let text = Component::text("Hello World", style.clone());
        assert!(matches!(&text.kind, ComponentKind::Text { content } if content == "Hello World"));
        assert_eq!(text.children.len(), 0);
        assert_eq!(text.style.font_size, 16.0);
    }

    #[test]
    fn component_button() {
        let style = Style::default();
        let btn = Component::button("Click me", style.clone());
        assert!(matches!(&btn.kind, ComponentKind::Button { label } if label == "Click me"));
        assert_eq!(btn.children.len(), 0);
    }

    #[test]
    fn component_column() {
        let style = Style::default();
        let child = Component::text("Child", Style::default());
        let column = Component::column(style.clone(), vec![child.clone()]);
        assert!(matches!(column.kind, ComponentKind::Column));
        assert_eq!(column.children.len(), 1);
        assert!(matches!(
            &column.children[0].kind,
            ComponentKind::Text { content } if content == "Child"
        ));
        assert!(matches!(
            column.style.flex_direction,
            crate::style::FlexDirection::Column
        ));
    }

    #[test]
    fn component_row() {
        let style = Style::default();
        let child1 = Component::text("A", Style::default());
        let child2 = Component::text("B", Style::default());
        let row = Component::row(style.clone(), vec![child1, child2]);
        assert!(matches!(row.kind, ComponentKind::Row));
        assert_eq!(row.children.len(), 2);
        assert!(matches!(
            row.style.flex_direction,
            crate::style::FlexDirection::Row
        ));
    }

    #[test]
    fn component_root() {
        let child = Component::text("Root child", Style::default());
        let root = Component::root(vec![child.clone()]);
        assert!(matches!(root.kind, ComponentKind::Root));
        assert_eq!(root.children.len(), 1);
    }

    #[test]
    fn component_boxed() {
        let style = Style::default();
        let child = Component::text("Box content", Style::default());
        let boxed = Component::boxed(style.clone(), vec![child]);
        assert!(matches!(boxed.kind, ComponentKind::Box));
        assert_eq!(boxed.children.len(), 1);
    }
}
