//! A replaced image cannot create a CSS table cell layout object. Its used
//! layout is block-level, but the original table role still suppresses table
//! separator whitespace before anonymous-box generation. The CSSOM computed
//! style remains untouched; only the component's used style is lowered.
use w3cos_std::{
    Component,
    style::{Display, Style},
};

const CELL_ROLE: &str = "--w3cos-internal-replaced-table-cell";

pub(super) fn lower_image_style(style: &mut Style) {
    // Replaced elements create image layout boxes, not structural columns.
    // Keep the authored role in CSSOM, but allow their own box decorations.
    if matches!(style.display,Display::TableColumn | Display::TableColumnGroup) {
        style.display=Display::Block;
    }
    if style.display == Display::TableCell {
        style.display = Display::Block;
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(CELL_ROLE.into(), "1".into());
    }
}

pub(super) fn separator_role(component: &Component) -> Option<Display> {
    component
        .style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get(CELL_ROLE))
        .is_some_and(|value| value == "1")
        .then_some(Display::TableCell)
}
