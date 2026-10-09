//! Font-derived outside marker rectangles shared by the layout/paint backends.
use std::collections::HashMap;
use w3cos_std::{ComponentKind, Style};
use w3cos_std::style::{Display, Float, Position, TextDirection};
use crate::layout::{FlatNodeInfo, LayoutRect, inline_font_content_ascent, inline_font_height,
    inline_style_alignment_keyword, inline_style_baseline_offset};

fn property<'a>(style: &'a Style, key: &str) -> Option<&'a str> {
    style.custom_properties.as_ref()?.get(key).map(String::as_str)
}

pub(crate) fn inside_kind(style: &Style) -> Option<&str> {
    property(style, "--w3cos-internal-inside-list-marker")
        .filter(|kind| matches!(*kind, "disc" | "circle" | "square"))
}

pub(crate) fn inside_symbol_rect(origin: LayoutRect, style: &Style) -> LayoutRect {
    // Blink141 RelativeSymbolMarkerRect: reserved advance includes two pixels,
    // but symbol ink starts one pixel into the marker's content box.
    let ascent = inline_font_content_ascent(style).round().max(0.0) as i32;
    let offset = ascent * 2 / 3;
    let width = ((offset + 1) / 2) as f32;
    LayoutRect { x: origin.x + 1.0,
        y: origin.y + (3 * (ascent - offset) / 2) as f32,
        width, height: width }
}

pub(crate) fn inside_advance(style: &Style) -> Option<f32> {
    inside_kind(style)?;
    Some(inside_symbol_rect(LayoutRect { x: 0.0, y: 0.0,
        width: 0.0, height: 0.0 }, style).width + 2.0)
}

fn in_flow_descendant(flat: &[FlatNodeInfo<'_>], mut index: usize, owner: usize) -> bool {
    loop {
        if index == owner { return true; }
        let node = &flat[index];
        if node.style.display == Display::None || node.style.float != Float::None
            || matches!(node.style.position, Position::Absolute | Position::Fixed) { return false; }
        let Some(parent) = node.parent else { return false; };
        index = parent;
    }
}

pub(crate) fn project(layouts: &mut [(LayoutRect, usize)], flat: &[FlatNodeInfo<'_>]) {
    let markers = flat.iter().enumerate().filter_map(|(index, node)|
        property(node.style, "--w3cos-internal-outside-list-marker").map(|kind| (index, kind)))
        .collect::<Vec<_>>();
    if markers.is_empty() { return; }
    let positions: HashMap<_, _> = layouts.iter().enumerate()
        .map(|(position, (_, index))| (*index, position)).collect();
    for (index, kind) in markers {
        let marker = &flat[index];
        let Some(&position) = positions.get(&index) else { continue; };
        let mut owner = marker.parent;
        while let Some(parent) = owner {
            if property(flat[parent].style, "--w3cos-internal-list-item").is_some() { break; }
            owner = flat[parent].parent;
        }
        let Some(owner) = owner else { continue; };
        let Some(&owner_position) = positions.get(&owner) else { continue; };
        let item = &flat[owner];
        let rect = layouts[owner_position].0;
        let ascent = inline_font_content_ascent(marker.style).round().max(0.0) as i32;
        let baseline = (owner + 1..flat.len()).filter(|child|
            matches!(flat[*child].kind, ComponentKind::Text { content } if !content.is_empty())
                && in_flow_descendant(flat, *child, owner)
                && inline_style_alignment_keyword(flat[*child].style) == "baseline")
            .find_map(|child| positions.get(&child).map(|position|
                layouts[*position].0.y + inline_font_content_ascent(flat[child].style)
                    + inline_style_baseline_offset(flat[child].style)))
            .unwrap_or_else(|| rect.y + item.style.padding_lengths().top
                + item.style.border_top_width.unwrap_or(item.style.border_width)
                + (item.style.font_size * item.style.line_height - inline_font_height(item.style)) * 0.5
                + inline_font_content_ascent(item.style));
        // Outside markers anchor at the list item's border edge. Borders,
        // padding and indentation move content, not that outside edge.
        let start = rect.x;
        let end = rect.x + rect.width;
        let mut used = layouts[position].0;
        if matches!(kind, "disc" | "circle" | "square") {
            // Blink141 ListMarker::RelativeSymbolMarkerRect and outside margins:
            // integer font ascent, integer symbol size/offset, then pixel snap.
            let offset = ascent * 2 / 3;
            let width = ((offset + 1) / 2) as f32;
            let top = (baseline - ascent as f32).round() + (3 * (ascent - offset) / 2) as f32;
            let x = match item.style.direction {
                TextDirection::Ltr => start - offset as f32 - 7.0,
                TextDirection::Rtl => end + offset as f32 + 7.0 - width,
            }.round();
            // Circle's1px centered stroke is the ring between these outer/inner
            // radii. The ordinary CSS border path keeps all backends in sync.
            let stroke = if kind == "circle" { 0.5 } else { 0.0 };
            used = LayoutRect { x: x - stroke, y: top - stroke,
                width: width + stroke * 2.0, height: width + stroke * 2.0 };
        } else if kind == "decimal" {
            used.x = if item.style.direction == TextDirection::Ltr { start - used.width } else { end };
            used.y = baseline - inline_font_content_ascent(marker.style);
        } else { continue; }
        layouts[position].0 = used;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{compute, pre_flatten};

    #[test]
    fn outside_symbol_uses_item_border_edge_and_not_middle_text_baseline() {
        let mut item_style = w3cos_dom::user_agent::html_default_style("html");
        item_style.font_size = 24.0;
        item_style.line_height = 1.375;
        item_style.border_left_width = Some(1.0);
        item_style.custom_properties.as_mut().unwrap()
            .insert("--w3cos-internal-list-item".into(), "1".into());
        let mut symbol_style = item_style.clone();
        symbol_style.position = Position::Absolute;
        symbol_style.custom_properties.as_mut().unwrap()
            .insert("--w3cos-internal-outside-list-marker".into(), "square".into());
        let text_style = Style { display: Display::Inline, font_size: 16.08,
            line_height: 22.0 / 16.08,
            align_self: w3cos_std::style::AlignSelf::Center, ..item_style.clone() };
        let root = w3cos_std::Component::row(item_style, vec![
            w3cos_std::Component::row(symbol_style, vec![]),
            w3cos_std::Component::text("blue square", text_style),
        ]);
        let flat = pre_flatten(&root);
        let mut layouts = vec![
            (LayoutRect { x: 104.0, y: 129.59375, width: 688.0, height: 33.0 }, 0),
            (LayoutRect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 }, 1),
            (LayoutRect { x: 105.0, y: 137.390625, width: 90.03125, height: 22.0 }, 2),
        ];
        project(&mut layouts, &flat);
        #[cfg(all(feature = "skia", target_os = "macos"))]
        assert_eq!(layouts[1].0, LayoutRect { x: 81.0, y: 143.0, width: 8.0, height: 8.0 },
            "V2891 browser marker: a middle-aligned child's baseline is not the list line's baseline");
    }

    #[test]
    fn outside_marker_follows_single_word_first_line_indent() {
        let mut document = w3cos_dom::Document::new();
        let item = document.create_element("div");
        for (name, value) in [("display", "list-item"), ("font", "16px/1 Ahem"),
            ("text-indent", "32px")] {
            item.style_mut(&mut document).set_property(name, value);
        }
        let content = document.create_text_node("XXXXX");
        item.append_child(&mut document, content);
        document.body().append_child(&mut document, item);
        let tree = document.to_component_tree();
        let flat = pre_flatten(&tree);
        let layouts = compute(&tree, 800.0, 600.0).unwrap();
        let marker = flat.iter().position(|node| property(node.style,
            "--w3cos-internal-outside-list-marker") == Some("disc")).unwrap();
        let text = flat.iter().position(|node| matches!(node.kind,
            ComponentKind::Text { content } if content == "XXXXX")).unwrap();
        let owner = flat.iter().position(|node| property(node.style,
            "--w3cos-internal-list-item").is_some()).unwrap();
        let rect = |index| layouts.iter().find(|(_, node)| *node == index).unwrap().0;
        assert_eq!(rect(text).height, 16.0, "a single unbroken word fits its containing line");
        assert_eq!(rect(text).x, rect(owner).x + 32.0);
        assert_eq!(rect(marker).x, rect(owner).x - 15.0,
            "the outside marker stays at the list item's edge");
    }

    #[test]
    fn outside_marker_does_not_follow_list_item_padding() {
        let mut document = w3cos_dom::Document::new();
        let item = document.create_element("div");
        for (name, value) in [("display", "list-item"), ("font", "16px/1 Ahem"),
            ("padding-left", "32px")] {
            item.style_mut(&mut document).set_property(name, value);
        }
        let content = document.create_text_node("XXXXX");
        item.append_child(&mut document, content);
        document.body().append_child(&mut document, item);
        let tree = document.to_component_tree();
        let flat = pre_flatten(&tree);
        let layouts = compute(&tree, 800.0, 600.0).unwrap();
        let marker = flat.iter().position(|node| property(node.style,
            "--w3cos-internal-outside-list-marker") == Some("disc")).unwrap();
        let owner = flat.iter().position(|node| property(node.style,
            "--w3cos-internal-list-item").is_some()).unwrap();
        let rect = |index| layouts.iter().find(|(_, node)| *node == index).unwrap().0;
        assert_eq!(rect(marker).x, rect(owner).x - 15.0);
    }

    #[test]
    fn inside_symbol_advance_uses_font_metrics_not_bullet_glyph() {
        for kind in ["disc", "circle", "square"] {
            let style = Style { font_family: Some("Ahem".into()), font_size: 16.0,
                custom_properties: Some(HashMap::from([
                    ("--w3cos-internal-inside-list-marker".into(), kind.into())])),
                ..Style::default() };
            assert_eq!(crate::layout::text_intrinsic_size("•", &style).0, 6.0, "{kind}");
        }
    }

    #[test]
    fn outside_marker_geometry_preserves_content_origin_and_wrap_width() {
        for (kind, inset, size) in [("disc", 0.0, 4.0), ("circle", 0.5, 5.0), ("square", 0.0, 4.0)] {
            let mut document = w3cos_dom::Document::new();
            let item = document.create_element("div");
            for (name, value) in [("display", "list-item"), ("font", "16px/1 Ahem"),
                ("width", "112px"), ("white-space", "pre"), ("list-style-type", kind)] {
                item.style_mut(&mut document).set_property(name, value);
            }
            item.set_text_content(&mut document, "XX   XX");
            document.body().append_child(&mut document, item);
            let tree = document.to_component_tree();
            let flat = pre_flatten(&tree);
            let layouts = compute(&tree, 800.0, 600.0).unwrap();
            let marker = flat.iter().position(|node|
                property(node.style, "--w3cos-internal-outside-list-marker") == Some(kind)).unwrap();
            let owner = flat.iter().position(|node|
                property(node.style, "--w3cos-internal-list-item").is_some()).unwrap();
            let text = flat.iter().position(|node|
                matches!(node.kind, ComponentKind::Text { content } if content == "XX   XX")).unwrap();
            let rect = |index| layouts.iter().find(|(_, node)| *node == index).unwrap().0;
            assert_eq!(rect(text).x, rect(owner).x);
            assert_eq!(rect(text).width, 112.0);
            assert_eq!(rect(marker).x, rect(owner).x - 15.0 - inset, "{kind}");
            assert_eq!(rect(marker).y, (rect(text).y + 7.0).round() - inset, "{kind}");
            assert_eq!((rect(marker).width, rect(marker).height), (size, size));
        }
    }
}
