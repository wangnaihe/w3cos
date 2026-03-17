pub mod role;
pub mod tree;

pub use role::AriaRole;
pub use tree::A11yNode;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{build_a11y_tree, flatten_for_ai};
    use w3cos_dom::Document;

    // --- Role: test role_for_tag mapping ---
    #[test]
    fn role_from_tag_div_is_generic() {
        assert_eq!(AriaRole::from_tag("div"), AriaRole::Generic);
    }

    #[test]
    fn role_from_tag_button_is_button() {
        assert_eq!(AriaRole::from_tag("button"), AriaRole::Button);
    }

    #[test]
    fn role_from_tag_h1_is_heading() {
        assert_eq!(AriaRole::from_tag("h1"), AriaRole::Heading);
    }

    #[test]
    fn role_from_tag_h2_through_h6_are_heading() {
        for tag in ["h2", "h3", "h4", "h5", "h6"] {
            assert_eq!(AriaRole::from_tag(tag), AriaRole::Heading);
        }
    }

    #[test]
    fn role_from_tag_a_is_link() {
        assert_eq!(AriaRole::from_tag("a"), AriaRole::Link);
    }

    #[test]
    fn role_from_tag_input_is_textbox() {
        assert_eq!(AriaRole::from_tag("input"), AriaRole::Textbox);
    }

    #[test]
    fn role_from_tag_nav_is_navigation() {
        assert_eq!(AriaRole::from_tag("nav"), AriaRole::Navigation);
    }

    #[test]
    fn role_from_tag_main_is_main() {
        assert_eq!(AriaRole::from_tag("main"), AriaRole::Main);
    }

    #[test]
    fn role_from_tag_ul_ol_are_list() {
        assert_eq!(AriaRole::from_tag("ul"), AriaRole::List);
        assert_eq!(AriaRole::from_tag("ol"), AriaRole::List);
    }

    #[test]
    fn role_from_tag_li_is_list_item() {
        assert_eq!(AriaRole::from_tag("li"), AriaRole::ListItem);
    }

    #[test]
    fn role_from_tag_img_is_image() {
        assert_eq!(AriaRole::from_tag("img"), AriaRole::Image);
    }

    #[test]
    fn role_from_tag_unknown_is_generic() {
        assert_eq!(AriaRole::from_tag("custom-element"), AriaRole::Generic);
    }

    #[test]
    fn role_from_attr_overrides_tag() {
        assert_eq!(AriaRole::from_attr("button"), AriaRole::Button);
        assert_eq!(AriaRole::from_attr("heading"), AriaRole::Heading);
        assert_eq!(AriaRole::from_attr("none"), AriaRole::None);
    }

    #[test]
    fn role_is_interactive() {
        assert!(AriaRole::Button.is_interactive());
        assert!(AriaRole::Link.is_interactive());
        assert!(AriaRole::Textbox.is_interactive());
        assert!(!AriaRole::Generic.is_interactive());
        assert!(!AriaRole::Heading.is_interactive());
    }

    // --- Tree: test building a11y tree from DOM ---
    #[test]
    fn tree_build_from_empty_body() {
        let doc = Document::new();
        let tree = build_a11y_tree(&doc);
        assert_eq!(tree.id, 1);
        assert_eq!(tree.role, AriaRole::Application); // body maps to Application? No - body is not in from_tag. Let me check.
        // Actually body => Application in from_tag. So body's role is Application. Good.
        assert_eq!(tree.role, AriaRole::Application);
        assert!(tree.children.is_empty());
    }

    #[test]
    fn tree_build_with_button_and_heading() {
        let mut doc = Document::new();
        let body = doc.body();
        let btn = doc.create_element("button");
        btn.set_text_content(&mut doc, "Submit");
        body.append_child(&mut doc, btn);
        let h1 = doc.create_element("h1");
        h1.set_text_content(&mut doc, "Welcome");
        body.append_child(&mut doc, h1);

        let tree = build_a11y_tree(&doc);
        assert_eq!(tree.role, AriaRole::Application);
        assert_eq!(tree.children.len(), 2);

        let btn_node = tree
            .children
            .iter()
            .find(|c| c.role == AriaRole::Button)
            .unwrap();
        assert_eq!(btn_node.name.as_deref(), Some("Submit"));
        assert!(btn_node.interactive);

        let h1_node = tree
            .children
            .iter()
            .find(|c| c.role == AriaRole::Heading)
            .unwrap();
        assert_eq!(h1_node.name.as_deref(), Some("Welcome"));
        assert_eq!(h1_node.level, Some(1));
    }

    #[test]
    fn tree_build_nested_structure() {
        let mut doc = Document::new();
        let body = doc.body();
        let nav = doc.create_element("nav");
        let link = doc.create_element("a");
        link.set_text_content(&mut doc, "Home");
        nav.append_child(&mut doc, link);
        body.append_child(&mut doc, nav);

        let tree = build_a11y_tree(&doc);
        assert_eq!(tree.children.len(), 1);
        let nav_node = &tree.children[0];
        assert_eq!(nav_node.role, AriaRole::Navigation);
        assert_eq!(nav_node.children.len(), 1);
        assert_eq!(nav_node.children[0].role, AriaRole::Link);
        assert_eq!(nav_node.children[0].name.as_deref(), Some("Home"));
    }

    #[test]
    fn tree_aria_label_overrides_text_content() {
        let mut doc = Document::new();
        let body = doc.body();
        let btn = doc.create_element("button");
        btn.set_text_content(&mut doc, "Click me");
        btn.set_attribute(&mut doc, "aria-label", "Accessible label");
        body.append_child(&mut doc, btn);

        let tree = build_a11y_tree(&doc);
        let btn_node = tree
            .children
            .iter()
            .find(|c| c.role == AriaRole::Button)
            .unwrap();
        assert_eq!(btn_node.name.as_deref(), Some("Accessible label"));
    }

    #[test]
    fn tree_input_with_value() {
        let mut doc = Document::new();
        let body = doc.body();
        let input = doc.create_element("input");
        input.set_attribute(&mut doc, "value", "user@example.com");
        input.set_attribute(&mut doc, "placeholder", "Email");
        body.append_child(&mut doc, input);

        let tree = build_a11y_tree(&doc);
        let input_node = tree
            .children
            .iter()
            .find(|c| c.role == AriaRole::Textbox)
            .unwrap();
        assert_eq!(input_node.value.as_deref(), Some("user@example.com"));
        assert_eq!(input_node.name.as_deref(), Some("Email"));
    }

    // --- Flatten: test flatten_for_ai ---
    #[test]
    fn flatten_single_node() {
        let node = A11yNode {
            id: 1,
            role: AriaRole::Button,
            name: Some("Submit".to_string()),
            value: None,
            level: None,
            interactive: true,
            focused: false,
            disabled: false,
            visible: true,
            bounds: None,
            children: vec![],
        };
        let lines = flatten_for_ai(&node);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("button"));
        assert!(lines[0].contains("Submit"));
        assert!(lines[0].contains("[interactive]"));
    }

    #[test]
    fn flatten_with_value() {
        let node = A11yNode {
            id: 1,
            role: AriaRole::Textbox,
            name: Some("Email".to_string()),
            value: Some("user@example.com".to_string()),
            level: None,
            interactive: true,
            focused: false,
            disabled: false,
            visible: true,
            bounds: None,
            children: vec![],
        };
        let lines = flatten_for_ai(&node);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("textbox"));
        assert!(lines[0].contains("Email"));
        assert!(lines[0].contains("(value: user@example.com)"));
    }

    #[test]
    fn flatten_disabled_node() {
        let node = A11yNode {
            id: 1,
            role: AriaRole::Button,
            name: Some("Disabled".to_string()),
            value: None,
            level: None,
            interactive: true,
            focused: false,
            disabled: true,
            visible: true,
            bounds: None,
            children: vec![],
        };
        let lines = flatten_for_ai(&node);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("[disabled]"));
    }

    #[test]
    fn flatten_tree_with_children() {
        let node = A11yNode {
            id: 1,
            role: AriaRole::Region,
            name: None,
            value: None,
            level: None,
            interactive: false,
            focused: false,
            disabled: false,
            visible: true,
            bounds: None,
            children: vec![
                A11yNode {
                    id: 2,
                    role: AriaRole::Button,
                    name: Some("OK".to_string()),
                    value: None,
                    level: None,
                    interactive: true,
                    focused: false,
                    disabled: false,
                    visible: true,
                    bounds: None,
                    children: vec![],
                },
                A11yNode {
                    id: 3,
                    role: AriaRole::Button,
                    name: Some("Cancel".to_string()),
                    value: None,
                    level: None,
                    interactive: true,
                    focused: false,
                    disabled: false,
                    visible: true,
                    bounds: None,
                    children: vec![],
                },
            ],
        };
        let lines = flatten_for_ai(&node);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("region") || lines[0].contains("Region"));
        assert!(lines[1].contains("OK"));
        assert!(lines[2].contains("Cancel"));
    }

    #[test]
    fn flatten_skips_invisible_and_none() {
        let node = A11yNode {
            id: 1,
            role: AriaRole::Region,
            name: None,
            value: None,
            level: None,
            interactive: false,
            focused: false,
            disabled: false,
            visible: true,
            bounds: None,
            children: vec![
                A11yNode {
                    id: 2,
                    role: AriaRole::None,
                    name: None,
                    value: None,
                    level: None,
                    interactive: false,
                    focused: false,
                    disabled: false,
                    visible: true,
                    bounds: None,
                    children: vec![],
                },
                A11yNode {
                    id: 3,
                    role: AriaRole::Button,
                    name: Some("Visible".to_string()),
                    value: None,
                    level: None,
                    interactive: true,
                    focused: false,
                    disabled: false,
                    visible: false,
                    bounds: None,
                    children: vec![],
                },
            ],
        };
        let lines = flatten_for_ai(&node);
        // Only the parent region should appear; None and invisible are skipped
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn flatten_numbered_indices() {
        let node = A11yNode {
            id: 1,
            role: AriaRole::Generic,
            name: None,
            value: None,
            level: None,
            interactive: false,
            focused: false,
            disabled: false,
            visible: true,
            bounds: None,
            children: vec![A11yNode {
                id: 2,
                role: AriaRole::Generic,
                name: None,
                value: None,
                level: None,
                interactive: false,
                focused: false,
                disabled: false,
                visible: true,
                bounds: None,
                children: vec![],
            }],
        };
        let lines = flatten_for_ai(&node);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("[1]"));
        assert!(lines[1].starts_with("[2]"));
    }
}
