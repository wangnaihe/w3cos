//! Font-derived outside marker rectangles shared by the layout/paint backends.
use std::collections::HashMap;
use w3cos_std::{ComponentKind, Style};
use w3cos_std::style::{Display, Float, Position, TextDirection};
use crate::layout::{FlatNodeInfo, LayoutRect, inline_font_content_ascent, inline_font_height,
    inline_style_alignment_keyword, inline_style_baseline_offset, float_content_contexts,
    float_line_band, FloatExclusion, resolve_spacing_for_layout, relative_flow_offset};

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

fn flow_translation(flat: &[FlatNodeInfo<'_>], positions: &HashMap<usize, usize>,
    layouts: &[(LayoutRect, usize)], mut index: usize, context: usize,
    viewport_w: f32, viewport_h: f32) -> (f32, f32) {
    let mut result = (0.0, 0.0);
    while index != context {
        let Some(parent) = flat[index].parent else { break; };
        if let Some(position) = positions.get(&parent) {
            let offset = relative_flow_offset(flat[index].style, flat[parent].style,
                layouts[*position].0, viewport_w, viewport_h);
            result.0 += offset.0;
            result.1 += offset.1;
        }
        index = parent;
    }
    result
}

pub(crate) fn project(layouts: &mut [(LayoutRect, usize)], flat: &[FlatNodeInfo<'_>],
    viewport_w: f32, viewport_h: f32) {
    let markers = flat.iter().enumerate().filter_map(|(index, node)|
        property(node.style, "--w3cos-internal-outside-list-marker").map(|kind| (index, kind)))
        .collect::<Vec<_>>();
    if markers.is_empty() { return; }
    let positions: HashMap<_, _> = layouts.iter().enumerate()
        .map(|(position, (_, index))| (*index, position)).collect();
    let contexts = float_content_contexts(flat);
    let mut visible = vec![true; flat.len()];
    let mut floats = Vec::new();
    for (index, node) in flat.iter().enumerate() {
        visible[index] = node.style.display != Display::None
            && node.parent.is_none_or(|parent| visible[parent]);
        if !visible[index] || node.style.float == Float::None
            || matches!(node.style.position, Position::Absolute | Position::Fixed) { continue; }
        let (Some(parent), Some(position)) = (node.parent, positions.get(&index)) else { continue; };
        let mut containing = parent;
        while flat[containing].style.display == Display::Inline {
            let Some(ancestor) = flat[containing].parent else { break; };
            containing = ancestor;
        }
        let basis = positions.get(&containing).map_or(viewport_w, |position| layouts[*position].0.width);
        let spacing = |value| resolve_spacing_for_layout(value, basis, node.style, viewport_w, viewport_h);
        let rect = layouts[*position].0;
        let margin = node.style.margin;
        let ml = spacing(margin.left);
        let mr = spacing(margin.right);
        let mt = spacing(margin.top);
        let mb = spacing(margin.bottom);
        let (rx, ry) = flow_translation(flat, &positions, layouts, index, contexts[parent],
            viewport_w, viewport_h);
        floats.push((index, contexts[parent], FloatExclusion {
            margin_box: LayoutRect { x: rect.x - ml - rx, y: rect.y - mt - ry,
                width: rect.width + ml + mr, height: rect.height + mt + mb },
            side: node.style.float,
        }));
    }
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
        let first_text = (owner + 1..flat.len()).filter(|child|
            matches!(flat[*child].kind, ComponentKind::Text { content } if !content.is_empty())
                && in_flow_descendant(flat, *child, owner)
                && inline_style_alignment_keyword(flat[*child].style) == "baseline")
            .find_map(|child| positions.get(&child).map(|position| (child, layouts[*position].0)));
        let baseline = first_text.map(|(child, rect)| rect.y + inline_font_content_ascent(flat[child].style)
                    + inline_style_baseline_offset(flat[child].style))
            .unwrap_or_else(|| rect.y + item.style.padding_lengths().top
                + item.style.border_top_width.unwrap_or(item.style.border_width)
                + (item.style.font_size * item.style.line_height - inline_font_height(item.style)) * 0.5
                + inline_font_content_ascent(item.style));
        // Padding/indentation do not move the outside edge, but an active
        // preceding float in this BFC narrows the first line's marker band.
        let exclusions = floats.iter().filter(|(index, context, _)|
            *context == contexts[owner] && *index < first_text.map_or(owner, |(index, _)| index))
            .map(|(_, _, exclusion)| *exclusion).collect::<Vec<_>>();
        let (rx, ry) = flow_translation(flat, &positions, layouts, owner, contexts[owner],
            viewport_w, viewport_h);
        let band = float_line_band(LayoutRect { x: rect.x - rx, y: rect.y - ry, ..rect },
            baseline - ry - inline_font_content_ascent(item.style),
            (item.style.font_size * item.style.line_height).max(0.0), &exclusions);
        let start = band.x + rx;
        let end = band.x + band.width + rx;
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
        } else if matches!(kind, "decimal" | "string") {
            used.x = if item.style.direction == TextDirection::Ltr { start - used.width } else { end };
            used.y = baseline - inline_font_content_ascent(marker.style);
        } else if kind == "image" {
            // Blink ListMarker::InlineMarginsForOutside gives images a 7px
            // inline-end gap. Replaced image content exports its bottom edge
            // as the baseline instead of a text ascent.
            used.x = if item.style.direction == TextDirection::Ltr {
                start - used.width - 7.0
            } else { end + 7.0 };
            used.y = baseline - used.height;
        } else { continue; }
        layouts[position].0 = used;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{compute, pre_flatten};

    #[test]
    fn outside_string_marker_uses_text_advance_and_first_line_baseline() {
        let item = Style { display:Display::Block,
            custom_properties:Some(HashMap::from([("--w3cos-internal-list-item".into(),"1".into())])),
            ..Style::default() };
        let mut marker = item.clone();
        marker.display = Display::Inline;
        marker.position = Position::Absolute;
        marker.custom_properties.as_mut().unwrap().insert(
            "--w3cos-internal-outside-list-marker".into(),"string".into());
        let tree = w3cos_std::Component::boxed(item.clone(),vec![
            w3cos_std::Component::text("•",marker),
            w3cos_std::Component::text("PASS",Style { display:Display::Inline,..item }),
        ]);
        let flat = pre_flatten(&tree);
        let index = flat.iter().position(|node| property(node.style,
            "--w3cos-internal-outside-list-marker")==Some("string")).unwrap();
        let mut layouts = flat.iter().enumerate().map(|(index,_)|
            (LayoutRect { x:48.0,y:54.0,width:100.0,height:16.0 },index)).collect::<Vec<_>>();
        layouts[index].0 = LayoutRect { x:8.0,y:8.0,width:12.0,height:16.0 };
        project(&mut layouts,&flat,800.0,600.0);
        assert_eq!(layouts[index].0.x,36.0,"string advance ends at the item's inline start");
        assert_eq!(layouts[index].0.y,54.0,"string text aligns to the first content baseline");
    }

    #[test]
    fn outside_marker_uses_first_line_float_margin_band_in_its_own_context() {
        use w3cos_std::style::{Overflow, Spacing};
        for direction in [TextDirection::Ltr, TextDirection::Rtl] {
            for scenario in ["sibling", "nested", "isolated", "expired", "following", "hidden",
                "relative-float", "relative-item"] {
                let root_style = Style { display: Display::Block, ..Style::default() };
                let mut item = Style { display: Display::Block, font_family: Some("Ahem".into()),
                    font_size: 16.0, line_height: 1.0, direction,
                    custom_properties: Some(HashMap::from([
                        ("--w3cos-internal-list-item".into(), "1".into())])), ..Style::default() };
                if scenario == "isolated" { item.overflow = Overflow::Hidden; }
                if scenario == "relative-item" {
                    item.position = Position::Relative;
                    item.left = w3cos_std::style::Dimension::Px(40.0);
                }
                let mut symbol = item.clone();
                symbol.position = Position::Absolute;
                symbol.custom_properties.as_mut().unwrap().insert(
                    "--w3cos-internal-outside-list-marker".into(), "square".into());
                let mut float_style = root_style.clone();
                float_style.float = if direction == TextDirection::Ltr { Float::Left } else { Float::Right };
                if direction == TextDirection::Ltr { float_style.margin.right = Spacing::Px(8.0); }
                else { float_style.margin.left = Spacing::Px(8.0); }
                if scenario == "hidden" { float_style.display = Display::None; }
                if scenario == "relative-float" {
                    float_style.position = Position::Relative;
                    float_style.left = w3cos_std::style::Dimension::Px(40.0);
                }
                let floating = w3cos_std::Component::boxed(float_style, vec![]);
                let content = w3cos_std::Component::boxed(item.clone(), vec![
                    w3cos_std::Component::boxed(symbol, vec![]),
                    w3cos_std::Component::text("X", Style { display: Display::Inline, ..item }),
                ]);
                let content = if scenario == "nested" {
                    w3cos_std::Component::boxed(root_style.clone(), vec![content])
                } else { content };
                let children = if scenario == "following" { vec![content, floating] }
                    else { vec![floating, content] };
                let tree = w3cos_std::Component::boxed(root_style, children);
                let flat = pre_flatten(&tree);
                let owner = flat.iter().position(|node| property(node.style,
                    "--w3cos-internal-list-item").is_some()).unwrap();
                let marker = flat.iter().position(|node| property(node.style,
                    "--w3cos-internal-outside-list-marker").is_some()).unwrap();
                let float_index = flat.iter().position(|node| node.style.float != Float::None).unwrap();
                let mut layouts = flat.iter().enumerate().map(|(index, _)|
                    (LayoutRect { x: 8.0, y: 54.0, width: 784.0, height: 16.0 }, index)).collect::<Vec<_>>();
                layouts[float_index].0 = LayoutRect {
                    x: if direction == TextDirection::Ltr { 8.0 } else { 696.0 },
                    y: if scenario == "expired" { -42.0 } else { 54.0 }, width: 96.0, height: 96.0,
                };
                if scenario == "relative-float" { layouts[float_index].0.x += 40.0; }
                if scenario == "relative-item" {
                    for (rect, index) in &mut layouts {
                        if *index >= owner { rect.x += 40.0; }
                    }
                }
                layouts[marker].0.width = 0.0;
                layouts[marker].0.height = 0.0;
                let mut without_float = layouts.clone();
                without_float[float_index].0.height = -1.0;
                project(&mut without_float, &flat, 800.0, 600.0);
                project(&mut layouts, &flat, 800.0, 600.0);
                let expected_shift = if matches!(scenario, "sibling" | "nested" | "relative-float" | "relative-item") {
                    if direction == TextDirection::Ltr { 104.0 } else { -104.0 }
                } else { 0.0 };
                assert_eq!(layouts[marker].0.x - without_float[marker].0.x, expected_shift,
                    "{direction:?}/{scenario}: marker follows the line's float band, not padding or another BFC");
                assert_eq!(layouts[owner].0, without_float[owner].0, "marker projection does not change principal geometry");
            }
        }
    }

    #[test]
    fn empty_outside_marker_creates_a_font_line_inside_atomic_inline() {
        for display in ["inline-block","inline-table"] {
            let mut document=w3cos_dom::Document::new();
            document.body().style_mut(&mut document).set_property(
                w3cos_dom::user_agent::HTML_STANDARD_FONT_PROPERTY,"1");
            let paragraph=document.create_element("p");
            paragraph.set_text_content(&mut document,"Test passes if there is a single square below.");
            document.body().append_child(&mut document,paragraph);
            let container=document.create_element("div");
            container.style_mut(&mut document).set_property("display",display);
            container.style_mut(&mut document).set_property("margin-left","96px");
            container.style_mut(&mut document).set_property("list-style-type","square");
            let item=document.create_element("span");
            item.style_mut(&mut document).set_property("display","list-item");
            container.append_child(&mut document,item);
            document.body().append_child(&mut document,container);
            let tree=document.to_component_tree();
            let flat=pre_flatten(&tree);
            let layouts=compute(&tree,800.0,600.0).unwrap();
            let owner=flat.iter().position(|node|property(node.style,"--w3cos-internal-list-item").is_some()).unwrap();
            let marker=flat.iter().position(|node|property(node.style,"--w3cos-internal-outside-list-marker")==Some("square")).unwrap();
            let rect=|index|layouts.iter().find(|(_,node)|*node==index).unwrap().0;
            assert_eq!(rect(owner).height,22.0,"{display}: outside marker retains a real first font line");
            assert_eq!(rect(owner).width,0.0,"the marker does not consume inline width");
            #[cfg(all(feature="skia",target_os="macos"))]
            assert_eq!(rect(marker),LayoutRect{x:rect(owner).x-18.0,
                y:rect(owner).y.round()+9.0,width:6.0,height:6.0},
                "V3984 browser marker offset from the actual list item's first line");
        }
    }

    #[test]
    fn zero_width_table_track_keeps_empty_list_line_background_zero_width() {
        let mut document=w3cos_dom::Document::new();
        document.body().style_mut(&mut document).set_property(
            w3cos_dom::user_agent::HTML_STANDARD_FONT_PROPERTY,"1");
        let table=document.create_element("div");
        table.style_mut(&mut document).set_property("display","table");
        let column=document.create_element("div");
        column.style_mut(&mut document).set_property("display","table-column-group");
        table.append_child(&mut document,column);
        let row=document.create_element("div");
        row.style_mut(&mut document).set_property("display","table-row");
        let cell=document.create_element("div");
        cell.style_mut(&mut document).set_property("display","table-cell");
        let item=document.create_element("div");
        item.style_mut(&mut document).set_property("display","list-item");
        item.style_mut(&mut document).set_property("background","orange");
        cell.append_child(&mut document,item);
        row.append_child(&mut document,cell);
        table.append_child(&mut document,row);
        document.body().append_child(&mut document,table);
        let tree=document.to_component_tree();
        let flat=pre_flatten(&tree);
        let layouts=compute(&tree,800.0,600.0).unwrap();
        let owner=flat.iter().position(|node|property(node.style,"--w3cos-internal-list-item").is_some()).unwrap();
        let rect=layouts.iter().find(|(_,index)|*index==owner).unwrap().0;
        assert_eq!(rect.width,0.0,"zero track is resolved, not an unspecified containing width");
        assert_eq!(rect.height,22.0,"the list still exports its first font line");
    }

    #[test]
    fn outside_image_uses_replaced_baseline_and_directional_seven_pixel_gap() {
        for direction in [TextDirection::Ltr, TextDirection::Rtl] {
            let item = Style { font_family: Some("Ahem".into()), font_size: 20.0,
                line_height: 1.0, direction,
                custom_properties: Some(HashMap::from([("--w3cos-internal-list-item".into(), "1".into())])),
                ..Style::default() };
            let mut image = item.clone();
            image.position = Position::Absolute;
            image.custom_properties.as_mut().unwrap()
                .insert("--w3cos-internal-outside-list-marker".into(), "image".into());
            let tree = w3cos_std::Component::row(item.clone(), vec![
                w3cos_std::Component::image("image.png", image),
                w3cos_std::Component::text("X", item),
            ]);
            let mut layouts = vec![
                (LayoutRect { x: 100.0, y: 40.0, width: 80.0, height: 20.0 }, 0),
                (LayoutRect { x: 0.0, y: 0.0, width: 3.0, height: 2.0 }, 1),
                (LayoutRect { x: 100.0, y: 40.0, width: 20.0, height: 20.0 }, 2),
            ];
            project(&mut layouts, &pre_flatten(&tree), 800.0, 600.0);
            assert_eq!(layouts[1].0, LayoutRect {
                x: if direction == TextDirection::Ltr { 90.0 } else { 187.0 },
                y: 54.0, width: 3.0, height: 2.0,
            });
            assert_eq!(layouts[2].0.x, 100.0, "outside image must not indent text");
        }
    }

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
        project(&mut layouts, &flat, 800.0, 600.0);
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
