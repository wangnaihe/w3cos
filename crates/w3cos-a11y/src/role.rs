use serde::{Deserialize, Serialize};

/// W3C ARIA roles — mapped from DOM tags and explicit `role` attributes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AriaRole {
    Application,
    Button,
    Checkbox,
    Dialog,
    Generic,
    Grid,
    GridCell,
    Group,
    Heading,
    Image,
    Link,
    List,
    ListItem,
    Main,
    Menu,
    MenuItem,
    Navigation,
    None,
    Progressbar,
    Radio,
    Region,
    Search,
    Separator,
    Slider,
    Status,
    Switch,
    Tab,
    TabList,
    TabPanel,
    Text,
    Textbox,
    Toolbar,
    Tooltip,
    Tree,
    TreeItem,
}

impl AriaRole {
    /// Infer ARIA role from an HTML tag name.
    pub fn from_tag(tag: &str) -> Self {
        match tag {
            "button" => Self::Button,
            "a" => Self::Link,
            "input" => Self::Textbox,
            "textarea" => Self::Textbox,
            "select" => Self::Textbox,
            "img" | "image" => Self::Image,
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Self::Heading,
            "nav" => Self::Navigation,
            "main" => Self::Main,
            "header" | "footer" => Self::Region,
            "section" | "article" | "aside" => Self::Region,
            "ul" | "ol" => Self::List,
            "li" => Self::ListItem,
            "dialog" => Self::Dialog,
            "form" => Self::Group,
            "p" | "span" | "label" => Self::Text,
            "hr" => Self::Separator,
            "progress" => Self::Progressbar,
            "menu" => Self::Menu,
            "table" | "grid" => Self::Grid,
            "tr" | "td" | "th" => Self::GridCell,
            "body" => Self::Application,
            _ => Self::Generic,
        }
    }

    /// Parse from explicit `role` attribute string.
    pub fn from_attr(role: &str) -> Self {
        match role {
            "button" => Self::Button,
            "link" => Self::Link,
            "textbox" => Self::Textbox,
            "heading" => Self::Heading,
            "navigation" => Self::Navigation,
            "main" => Self::Main,
            "region" => Self::Region,
            "dialog" => Self::Dialog,
            "img" | "image" => Self::Image,
            "list" => Self::List,
            "listitem" => Self::ListItem,
            "menu" => Self::Menu,
            "menuitem" => Self::MenuItem,
            "tab" => Self::Tab,
            "tablist" => Self::TabList,
            "tabpanel" => Self::TabPanel,
            "progressbar" => Self::Progressbar,
            "slider" => Self::Slider,
            "checkbox" => Self::Checkbox,
            "radio" => Self::Radio,
            "switch" => Self::Switch,
            "search" => Self::Search,
            "toolbar" => Self::Toolbar,
            "tooltip" => Self::Tooltip,
            "status" => Self::Status,
            "separator" => Self::Separator,
            "tree" => Self::Tree,
            "treeitem" => Self::TreeItem,
            "none" | "presentation" => Self::None,
            _ => Self::Generic,
        }
    }

    pub fn is_interactive(&self) -> bool {
        matches!(
            self,
            Self::Button
                | Self::Link
                | Self::Textbox
                | Self::Checkbox
                | Self::Radio
                | Self::Switch
                | Self::Slider
                | Self::MenuItem
                | Self::Tab
                | Self::TreeItem
        )
    }
}
