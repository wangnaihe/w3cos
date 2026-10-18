use super::*;

#[test]
fn parsed_block_only_inline_block_distinguishes_content_and_empty_baselines() {
    use crate::html_parser_host::InertParserScriptHost;
    use crate::html_parser_state::StreamingDocumentParser;
    use std::rc::Rc;
    for (empty, height, host_y, used_height, probe_y) in [
        (false, "auto", 8.0, 44.0, 30.0),
        (false, "80px", 8.0, 80.0, 30.0),
        (true, "auto", 25.0, 0.0, 8.0),
        (true, "80px", 8.0, 80.0, 71.0),
    ] {
        w3cos_dom::stylesheet::clear_rules();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let content = if empty {
            ""
        } else {
            "baseline must stay inside the block"
        };
        let mut parser = StreamingDocumentParser::new_with_script_host(
            Rc::new(InertParserScriptHost),
            "https://example.test/block-baseline.html",
        )
        .unwrap();
        parser.write(&format!("<!doctype html><body style='font:16px/normal serif'><div style='display:inline-block;width:180px'><div style='display:block;height:{height}'>{content}</div></div><span>Probe</span></body>")).unwrap();
        parser.finish().unwrap();
        let root = crate::dom::to_component_tree();
        let flat = pre_flatten(&root);
        let host = flat
            .iter()
            .position(|node| node.style.display == WDisplay::InlineBlock)
            .unwrap();
        let probe = flat
            .iter()
            .position(|node| matches!(node.kind,ComponentKind::Text{content}if content=="Probe"))
            .unwrap();
        for layouts in [
            compute(&root, 800.0, 600.0).unwrap(),
            LayoutEngine::new()
                .compute(&root, &flat, 800.0, 600.0)
                .unwrap()
                .layout_cache,
        ] {
            let rect = |index| layouts.iter().find(|(_, node)| *node == index).unwrap().0;
            assert_eq!(
                rect(host),
                LayoutRect {
                    x: 8.0,
                    y: host_y,
                    width: 180.0,
                    height: used_height
                },
                "empty={empty}, height={height}: {layouts:?}"
            );
            assert_eq!(
                rect(probe),
                LayoutRect {
                    x: 188.0,
                    y: probe_y,
                    width: 44.828125,
                    height: 22.0
                },
                "empty={empty}, height={height}: {layouts:?}"
            );
        }
    }
    w3cos_dom::stylesheet::clear_rules();
}

#[test]
fn parsed_anonymous_table_rows_resolve_authored_cell_height_floors() {
    use crate::html_parser_host::InertParserScriptHost;
    use crate::html_parser_state::StreamingDocumentParser;
    use std::rc::Rc;
    for heights in [[100, 80, 60], [60, 120, 80], [0, 0, 0]] {
        for anonymous in [true, false] {
            w3cos_dom::stylesheet::clear_rules();
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            let cells = ["Before", "Inner", "After"]
                .iter()
                .enumerate()
                .map(|(i, text)| {
                    format!(
                        "<div style='display:table-cell;height:{}px'>{text}</div>",
                        heights[i]
                    )
                })
                .collect::<String>();
            let content = if anonymous {
                cells
            } else {
                format!(
                    "<div style='display:table'><div style='display:table-row'>{cells}</div></div>"
                )
            };
            let mut parser = StreamingDocumentParser::new_with_script_host(
                Rc::new(InertParserScriptHost),
                "https://example.test/anonymous-height.html",
            )
            .unwrap();
            parser.write(&format!("<!doctype html><body style='font:16px/normal serif'><div>{content}</div></body>")).unwrap();
            parser.finish().unwrap();
            let root = crate::dom::to_component_tree();
            let flat = pre_flatten(&root);
            let table = flat
                .iter()
                .position(|node| node.style.display == WDisplay::Table)
                .unwrap();
            let row = flat
                .iter()
                .position(|node| node.style.display == WDisplay::TableRow)
                .unwrap();
            let cells: Vec<_> = flat
                .iter()
                .enumerate()
                .filter(|(_, node)| node.style.display == WDisplay::TableCell)
                .map(|(index, _)| index)
                .collect();
            assert_eq!(cells.len(), 3);
            let height = (*heights.iter().max().unwrap()).max(22) as f32;
            for layouts in [
                compute(&root, 800.0, 600.0).unwrap(),
                LayoutEngine::new()
                    .compute(&root, &flat, 800.0, 600.0)
                    .unwrap()
                    .layout_cache,
            ] {
                let rect = |index| layouts.iter().find(|(_, node)| *node == index).unwrap().0;
                let outer = LayoutRect {
                    x: 8.0,
                    y: 8.0,
                    width: 131.79688,
                    height,
                };
                assert_eq!(
                    rect(table),
                    outer,
                    "anonymous={anonymous}, heights={heights:?}: {layouts:?}"
                );
                assert_eq!(
                    rect(row),
                    outer,
                    "anonymous={anonymous}, heights={heights:?}"
                );
                for (index, (x, width)) in cells.iter().zip([
                    (8.0, 50.265625),
                    (58.265625, 42.609375),
                    (100.875, 38.921875),
                ]) {
                    assert_eq!(
                        rect(*index),
                        LayoutRect {
                            x,
                            y: 8.0,
                            width,
                            height
                        }
                    );
                }
            }
        }
    }
    w3cos_dom::stylesheet::clear_rules();
}

#[test]
fn parsed_generated_strings_and_attributes_preserve_browser_inline_geometry() {
    use crate::html_parser_host::InertParserScriptHost;
    use crate::html_parser_state::StreamingDocumentParser;
    use std::rc::Rc;
    for generated in [true, false] {
        w3cos_dom::stylesheet::clear_rules();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let mut parser = StreamingDocumentParser::new_with_script_host(
            Rc::new(InertParserScriptHost),
            "https://example.test/generated-order.html",
        )
        .unwrap();
        let content = if generated {
            "body"
        } else {
            "<span style='color:green'>before value</span>body after"
        };
        let id = if generated { "item" } else { "reference" };
        parser.write(&format!("<!doctype html><body style='font:16px/normal serif'><div id='{id}' data-label='value' style='display:inline-block'>{content}</div></body>")).unwrap();
        parser.finish().unwrap();
        if generated {
            w3cos_dom::stylesheet::register_rule(
                "#item:before",
                &[
                    ("content", "'before ' attr(data-label)"),
                    ("color", "green"),
                ],
            );
            w3cos_dom::stylesheet::register_rule("#item::after", &[("content", "' after'")]);
        }
        let root = crate::dom::to_component_tree();
        let flat = pre_flatten(&root);
        let host = flat
            .iter()
            .position(|node| node.style.display == WDisplay::InlineBlock)
            .unwrap();
        let text: String = flat
            .iter()
            .filter_map(|node| match node.kind {
                ComponentKind::Text { content } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "before valuebody after", "generated={generated}");
        let colored_text = |color| {
            flat.iter()
                .filter_map(|node| match node.kind {
                    ComponentKind::Text { content } if node.style.color == color => {
                        Some(content.as_str())
                    }
                    _ => None,
                })
                .collect::<String>()
        };
        assert_eq!(
            colored_text(w3cos_std::Color::rgb(0, 128, 0)),
            "before value"
        );
        assert_eq!(colored_text(w3cos_std::Color::rgb(0, 0, 0)), "body after");
        for layouts in [
            compute(&root, 800.0, 600.0).unwrap(),
            LayoutEngine::new()
                .compute(&root, &flat, 800.0, 600.0)
                .unwrap()
                .layout_cache,
        ] {
            let rect = layouts.iter().find(|(_, index)| *index == host).unwrap().0;
            assert_eq!(
                rect,
                LayoutRect {
                    x: 8.0,
                    y: 8.0,
                    width: 174.46875,
                    height: 22.0
                },
                "Chrome155 generated={generated}"
            );
        }
    }
    w3cos_dom::stylesheet::clear_rules();
}

#[test]
fn parsed_inline_tables_and_nowrap_siblings_follow_normal_font_lines() {
    use crate::html_parser_host::InertParserScriptHost;
    use crate::html_parser_state::StreamingDocumentParser;
    use std::rc::Rc;
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (family, height, width, line_width) in [
        ("initial", 22.0, 8.46875, 69.203125),
        ("serif", 22.0, 9.25, 73.8125),
        ("sans-serif", 22.0, 8.46875, 69.203125),
    ] {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let mut parser = StreamingDocumentParser::new_with_script_host(
            Rc::new(InertParserScriptHost),
            "https://example.test/normal-lines.html",
        )
        .unwrap();
        parser.write(&format!("<!doctype html><body style='font:16px/normal {family}'><div style='display:inline-table'>x</div><div style='display:inline-table'><div>x</div></div><div style='width:50px'><span style='white-space:nowrap'>Filler text</span>\n<span style='white-space:nowrap'>Filler text</span></div></body>")).unwrap();
        parser.finish().unwrap();
        let root = crate::dom::to_component_tree();
        let flat = pre_flatten(&root);
        let tables: Vec<_> = flat
            .iter()
            .enumerate()
            .filter(|(_, node)| node.style.display == WDisplay::InlineTable)
            .map(|(index, _)| index)
            .collect();
        let lines: Vec<_> = flat
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                matches!(node.kind,
            ComponentKind::Text { content } if content == "Filler text")
            })
            .map(|(index, _)| index)
            .collect();
        assert_eq!(tables.len(), 2);
        assert_eq!(lines.len(), 2);
        for layouts in [
            compute(&root, 800.0, 600.0).unwrap(),
            LayoutEngine::new()
                .compute(&root, &flat, 800.0, 600.0)
                .unwrap()
                .layout_cache,
        ] {
            let rect = |index| layouts.iter().find(|(_, node)| *node == index).unwrap().0;
            actual.push((
                family,
                rect(tables[0]),
                rect(tables[1]),
                rect(lines[0]),
                rect(lines[1]),
            ));
            expected.push((
                family,
                LayoutRect {
                    x: 8.0,
                    y: 8.0,
                    width,
                    height,
                },
                LayoutRect {
                    x: 8.0 + width,
                    y: 8.0,
                    width,
                    height,
                },
                LayoutRect {
                    x: 8.0,
                    y: 8.0 + height,
                    width: line_width,
                    height,
                },
                LayoutRect {
                    x: 8.0,
                    y: 8.0 + 2.0 * height,
                    width: line_width,
                    height,
                },
            ));
        }
    }
    assert_eq!(
        actual, expected,
        "independent Chrome155 normal-line table/nowrap matrix"
    );
}

#[test]
fn parsed_table_inline_padding_preserves_browser_font_box_origin() {
    use crate::html_parser_host::InertParserScriptHost;
    use crate::html_parser_state::StreamingDocumentParser;
    use std::rc::Rc;
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (family, text_y, font_height) in [("serif", 190.0, 22.0), ("sans-serif", 190.0, 22.0)] {
        for padding in [0, 160] {
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            let mut parser = StreamingDocumentParser::new_with_script_host(
                Rc::new(InertParserScriptHost),
                "https://example.test/table-padding.html",
            )
            .unwrap();
            parser.write(&format!("<!doctype html><body style='margin:0'><table style='position:absolute;left:8px;top:192px;border-spacing:0;font:16px/1.2 {family}'><tr><td style='padding:0;width:200px'><span style='padding-top:{padding}px'>control</span></td></tr></table></body>")).unwrap();
            parser.finish().unwrap();
            let root = crate::dom::to_component_tree();
            let flat = pre_flatten(&root);
            let cell = flat
                .iter()
                .position(|node| node.style.display == WDisplay::TableCell)
                .unwrap();
            let text = flat
                .iter()
                .position(|node| {
                    matches!(node.kind,
                ComponentKind::Text { content } if content == "control")
                })
                .unwrap();
            for layouts in [
                compute(&root, 800.0, 600.0).unwrap(),
                LayoutEngine::new()
                    .compute(&root, &flat, 800.0, 600.0)
                    .unwrap()
                    .layout_cache,
            ] {
                let rect = |index| layouts.iter().find(|(_, node)| *node == index).unwrap().0;
                // This text rect owns the available 200px layout slot, not
                // a DOM Range's shaped width. Compare the font-box origin
                // and extent independently of the horizontal slot contract.
                actual.push((family, padding, rect(cell), rect(text).y, rect(text).height));
                expected.push((
                    family,
                    padding,
                    LayoutRect {
                        x: 8.0,
                        y: 192.0,
                        width: 200.0,
                        height: 19.1875,
                    },
                    text_y - padding as f32,
                    font_height + padding as f32,
                ));
            }
        }
    }
    assert_eq!(
        actual, expected,
        "Chrome155 standards-mode table vertical font/padding matrix, including retained layout"
    );
}

#[test]
fn full_width_empty_atomic_margin_and_alignment_boundaries() {
    use crate::html_parser_host::InertParserScriptHost;
    use crate::html_parser_state::StreamingDocumentParser;
    use std::rc::Rc;
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (top, bottom, align, atomic_y, line_y) in [
        (0, 0, "baseline", 32.0, 37.0),
        (20, 0, "baseline", 35.0, 40.0),
        (0, 20, "baseline", 15.0, 40.0),
        (-5, 0, "baseline", 32.0, 37.0),
        (0, 0, "top", 15.0, 37.0),
        (0, 0, "bottom", 37.0, 37.0),
        (0, 0, "middle", 27.703125, 37.0),
    ] {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let mut parser = StreamingDocumentParser::new_with_script_host(
            Rc::new(InertParserScriptHost),
            "https://example.test/atomic-boundary.html",
        )
        .unwrap();
        parser.write(&format!("<!doctype html><div style='border:5px solid black;font:16px serif;margin:10px;width:550px'>\
            <span><span style='display:inline-block;width:550px;margin-top:{top}px;margin-bottom:{bottom}px;vertical-align:{align}'></span>Filler Text Filler Text</span>\
            <img src='blue.png' style='height:48px;width:96px'>\
            <span style='background:orange;float:left;height:48px;width:96px'></span></div>")).unwrap();
        parser.finish().unwrap();
        let root = crate::dom::to_component_tree();
        let flat = pre_flatten(&root);
        let atomic = flat
            .iter()
            .position(|node| {
                node.style.display == WDisplay::InlineBlock && node.style.width == WDim::Px(550.0)
            })
            .unwrap();
        let image = flat
            .iter()
            .position(|node| matches!(node.kind, ComponentKind::Image { .. }))
            .unwrap();
        let floating = flat
            .iter()
            .position(|node| node.style.float == WFloat::Left)
            .unwrap();
        let parent = flat[atomic].parent.unwrap();
        let layouts = compute(&root, 800.0, 600.0).unwrap();
        let rect = |index| layouts.iter().find(|(_, node)| *node == index).unwrap().0;
        actual.push((
            top,
            bottom,
            align,
            rect(atomic),
            rect(image),
            rect(floating),
            rect(parent),
        ));
        expected.push((
            top,
            bottom,
            align,
            LayoutRect {
                x: 23.0,
                y: atomic_y,
                width: 550.0,
                height: 0.0,
            },
            LayoutRect {
                x: 277.528,
                y: line_y,
                width: 96.0,
                height: 48.0,
            },
            LayoutRect {
                x: 23.0,
                y: line_y,
                width: 96.0,
                height: 48.0,
            },
            LayoutRect {
                x: 18.0,
                y: 10.0,
                width: 560.0,
                height: line_y + 48.0,
            },
        ));
    }
    assert_eq!(
        actual, expected,
        "independent standards-mode Chrome155 margin/alignment matrix"
    );
}

#[test]
#[cfg(all(feature = "skia", target_os = "macos"))]
fn interior_authored_endpoints_are_not_snapped_again_at_font_boundary() {
    use w3cos_std::Style;
    let text = ".Test passes if there is a green stripe on the right and ";
    let mut style = Style {
        display: WDisplay::Inline,
        custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
            "--w3cos-internal-html-standard-font".into(),
            "1".into(),
        )])),
        ..Style::default()
    };
    w3cos_std::inline_text::set_fragment_ends(&mut style, text, &[1, 56, 57]);
    let expected: f32 = crate::render_skia::measure_skia_inline_fragment_advances(
        &[&text[..1], &text[1..56], &text[56..]],
        &style,
    )
    .unwrap()
    .iter()
    .map(|advance| crate::text_layout::inline_layout_advance(*advance))
    .sum();
    let mut children = Vec::new();
    for word in text.split(' ').filter(|word| !word.is_empty()) {
        children.push(Component::text(word, style.clone()));
        children.push(Component::text(" ", style.clone()));
    }
    let count = children.len();
    children.push(Component::text(
        "bold",
        Style {
            font_weight: 700,
            ..style.clone()
        },
    ));
    let root = Component::row(
        Style {
            display: WDisplay::Inline,
            custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
                "--w3cos-internal-inline-formatting-context".into(),
                "1".into(),
            )])),
            ..Style::default()
        },
        children,
    );
    let used: f32 = inline_fragment_shaped_advances(&root)[..count]
        .iter()
        .map(|width| width.unwrap())
        .sum();
    assert!(
        (used - expected).abs() < 0.0002,
        "completed source endpoints must not gain another LayoutUnit: {used} != {expected}"
    );
}

#[test]
#[cfg(all(feature = "skia", target_os = "macos"))]
fn authored_boundary_inside_generated_word_keeps_layout_unit_endpoint() {
    use w3cos_std::Style;
    let mut style = Style {
        display: WDisplay::Inline,
        custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
            "--w3cos-internal-html-standard-font".into(),
            "1".into(),
        )])),
        ..Style::default()
    };
    let fragments = [".Test", " ", "passes"];
    let text = fragments.concat();
    w3cos_std::inline_text::set_fragment_ends(&mut style, &text, &[1, text.len()]);
    let source_widths = crate::render_skia::measure_skia_inline_fragment_advances(
        &[&text[..1], &text[1..]],
        &style,
    )
    .unwrap();
    let expected: f32 = source_widths
        .iter()
        .map(|advance| crate::text_layout::inline_layout_advance(*advance))
        .sum();
    let root = Component::row(
        Style {
            display: WDisplay::Inline,
            custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
                "--w3cos-internal-inline-formatting-context".into(),
                "1".into(),
            )])),
            ..Style::default()
        },
        fragments
            .into_iter()
            .map(|text| Component::text(text, style.clone()))
            .collect(),
    );
    let used: f32 = inline_fragment_shaped_advances(&root)
        .into_iter()
        .map(Option::unwrap)
        .sum();
    assert!(
        (used - expected).abs() < 0.0001,
        "interior source boundary: {used} != {expected}"
    );
}

#[test]
#[cfg(all(feature = "skia", target_os = "macos"))]
fn resolved_bidi_words_keep_contextual_advances_in_rtl_container() {
    use w3cos_std::{Style, style::TextDirection};
    let style = Style {
        display: WDisplay::Inline,
        custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([
            (
                w3cos_std::inline_text::SOURCE_RUN.into(),
                "text:bidi".into(),
            ),
            ("--w3cos-internal-bidi-visual-order".into(), "1".into()),
            ("--w3cos-internal-bidi-resolved-level".into(), "2".into()),
            ("--w3cos-internal-bidi-logical-order".into(), "0".into()),
            ("--w3cos-internal-html-standard-font".into(), "1".into()),
        ])),
        ..Style::default()
    };
    let mut root = Component::row(
        Style {
            display: WDisplay::Flex,
            direction: TextDirection::Rtl,
            custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
                "--w3cos-internal-inline-formatting-context".into(),
                "1".into(),
            )])),
            ..Style::default()
        },
        ["Test", " ", "passes"]
            .into_iter()
            .map(|text| Component::text(text, style.clone()))
            .collect(),
    );
    let rtl = inline_fragment_shaped_advances(&root);
    root.style.direction = TextDirection::Ltr;
    let ltr = inline_fragment_shaped_advances(&root);
    assert!(ltr.iter().all(Option::is_some));
    assert_eq!(
        rtl, ltr,
        "already resolved visual words must share shaping in either container"
    );
    root.style.direction = TextDirection::Rtl;
    root.children[1]
        .style
        .custom_properties
        .as_mut()
        .unwrap()
        .remove("--w3cos-internal-bidi-resolved-level");
    assert!(
        inline_fragment_shaped_advances(&root)
            .iter()
            .all(Option::is_none),
        "unresolved RTL content must keep its separate bidi path"
    );
}

#[test]
#[cfg(all(feature = "skia", target_os = "macos"))]
fn terminal_authored_inline_source_uses_layout_unit_endpoint() {
    use w3cos_std::Style;
    let style = Style {
        display: WDisplay::Inline,
        font_size: 16.0,
        font_weight: 700,
        custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([
            (
                w3cos_std::inline_text::SOURCE_RUN.into(),
                "text:terminal".into(),
            ),
            ("--w3cos-internal-html-standard-font".into(), "1".into()),
        ])),
        ..Style::default()
    };
    let fragments = ["no", " ", "red"];
    let raw: f32 = crate::render_skia::measure_skia_inline_fragment_advances(&fragments, &style)
        .unwrap()
        .iter()
        .sum();
    let root = Component::row(
        Style {
            display: WDisplay::Inline,
            custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
                "--w3cos-internal-inline-formatting-context".into(),
                "1".into(),
            )])),
            ..Style::default()
        },
        fragments
            .into_iter()
            .map(|text| Component::text(text, style.clone()))
            .collect(),
    );
    let used: f32 = inline_fragment_shaped_advances(&root)
        .into_iter()
        .map(Option::unwrap)
        .sum();
    let expected = crate::text_layout::inline_layout_advance(raw);
    assert_ne!(
        raw, expected,
        "fixture must exercise fractional source width"
    );
    assert!(
        (used - expected).abs() < 0.0001,
        "terminal source: {used} != {expected}"
    );
}

#[test]
#[cfg(all(feature = "skia", target_os = "macos"))]
fn absolute_first_child_static_position_uses_inline_block_content_top() {
    use crate::font_face::{FontFace, FontRegistry, FontSource};
    use w3cos_std::Style;
    const OWNER: u64 = 2026101004223;
    struct FontOwner;
    impl Drop for FontOwner {
        fn drop(&mut self) {
            FontRegistry::global().clear_owner(OWNER);
        }
    }
    let _font = FontOwner;
    FontRegistry::global()
        .register_for_owner(
            OWNER,
            FontFace {
                family: "Ahem".into(),
                src: FontSource::Path(std::path::PathBuf::from(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../../wpt/fonts/Ahem.ttf"
                ))),
                ..Default::default()
            },
        )
        .unwrap();
    let text = Style {
        display: WDisplay::Inline,
        font_family: Some("Ahem".into()),
        font_size: 20.0,
        line_height: 1.0,
        ..Style::default()
    };
    let inline = Component::row(
        Style {
            position: WPos::Relative,
            border_width: 1.0,
            ..text.clone()
        },
        vec![
            Component::text("static ", text.clone()),
            Component::boxed(
                Style {
                    display: WDisplay::InlineBlock,
                    width: WDim::Px(30.0),
                    height: WDim::Px(10.0),
                    ..text.clone()
                },
                vec![Component::boxed(
                    Style {
                        display: WDisplay::Block,
                        position: WPos::Absolute,
                        width: WDim::Px(10.0),
                        height: WDim::Px(10.0),
                        ..text.clone()
                    },
                    vec![],
                )],
            ),
            Component::boxed(
                Style {
                    display: WDisplay::InlineBlock,
                    width: WDim::Px(30.0),
                    height: WDim::Px(10.0),
                    ..text.clone()
                },
                vec![],
            ),
        ],
    );
    let root = Component::row(
        Style {
            display: WDisplay::Block,
            position: WPos::Relative,
            ..text.clone()
        },
        vec![Component::text("x ", text), inline],
    );
    let layouts = compute(&root, 800.0, 600.0).unwrap();
    let rect = |index| layouts.iter().find(|(_, i)| *i == index).unwrap().0;
    assert_eq!(rect(5).x, rect(4).x);
    assert_eq!(
        rect(5).y,
        rect(4).y,
        "auto static child must start at atomic content top"
    );
}

#[test]
fn final_inline_containing_block_reanchors_inset_descendant_after_float_projection() {
    use w3cos_std::Style;
    let root = Component::boxed(
        Style {
            display: WDisplay::Inline,
            position: WPos::Relative,
            padding: w3cos_std::style::Edges {
                left: WSpacing::Px(100.0),
                ..w3cos_std::style::Edges::ZERO
            },
            ..Style::default()
        },
        vec![Component::boxed(
            Style {
                display: WDisplay::Block,
                float: WFloat::Left,
                ..Style::default()
            },
            vec![Component::boxed(
                Style {
                    display: WDisplay::Block,
                    position: WPos::Absolute,
                    left: WDim::Px(0.0),
                    right: WDim::Px(0.0),
                    top: WDim::Px(0.0),
                    height: WDim::Px(100.0),
                    ..Style::default()
                },
                vec![],
            )],
        )],
    );
    let rect = |x, y, width, height| LayoutRect {
        x,
        y,
        width,
        height,
    };
    let mut layouts = vec![
        (rect(8.0, 54.0, 100.0, 22.0), 0),
        (rect(8.0, 54.0, 0.0, 0.0), 1),
        (rect(-92.0, 54.0, 100.0, 100.0), 2),
    ];
    project_absolute_insets_from_final_containing_blocks(
        &mut layouts,
        &pre_flatten(&root),
        800.0,
        600.0,
    );
    assert_eq!(layouts[2].0, rect(8.0, 54.0, 100.0, 100.0));
}

#[test]
#[cfg(all(feature = "skia", target_os = "macos"))]
fn table_cell_single_inline_text_excludes_collapsible_line_edges_from_max_content() {
    use w3cos_std::Style;
    let cell = |content: &str, white_space| {
        Component::boxed(
            Style {
                display: WDisplay::TableCell,
                white_space,
                ..Style::default()
            },
            vec![Component::text(
                content,
                Style {
                    display: WDisplay::Inline,
                    white_space,
                    ..Style::default()
                },
            )],
        )
    };
    for whitespace in [WWhiteSpace::Normal, WWhiteSpace::NoWrap] {
        let plain = component_max_content_width(&cell("1 two", whitespace));
        let padded = component_max_content_width(&cell(" 1 two ", whitespace));
        assert!(
            (plain - padded).abs() < 0.01,
            "{whitespace:?}: {plain} != {padded}"
        );
    }
    let plain = component_max_content_width(&cell("1 two", WWhiteSpace::Pre));
    let padded = component_max_content_width(&cell(" 1 two ", WWhiteSpace::Pre));
    assert!(padded > plain);
}

#[test]
#[cfg(all(feature = "skia", target_os = "macos"))]
fn decorated_source_uses_one_layout_unit_endpoint() {
    use w3cos_std::Style;
    let mut link = Style {
        display: WDisplay::Inline,
        font_family: Some("sans-serif".into()),
        font_size: 16.0,
        text_decoration: w3cos_std::style::TextDecoration::Underline,
        custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
            w3cos_std::inline_text::SOURCE_RUN.into(),
            "text:link".into(),
        )])),
        ..Style::default()
    };
    let mut suffix = link.clone();
    suffix.text_decoration = w3cos_std::style::TextDecoration::None;
    suffix.custom_properties.as_mut().unwrap().insert(
        w3cos_std::inline_text::SOURCE_RUN.into(),
        "text:suffix".into(),
    );
    let root = Component::row(
        Style {
            display: WDisplay::Inline,
            custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
                "--w3cos-internal-inline-formatting-context".into(),
                "1".into(),
            )])),
            ..Style::default()
        },
        vec![
            Component::text("White", link.clone()),
            Component::text(" ", link.clone()),
            Component::text("Space", link.clone()),
            Component::text("'", suffix),
        ],
    );
    let widths = inline_fragment_shaped_advances(&root);
    assert!(
        widths[..3].iter().all(Option::is_some),
        "paint decoration must not force independently rounded word widths: {widths:?}"
    );
    link.text_decoration = w3cos_std::style::TextDecoration::None;
    let raw =
        crate::render_skia::measure_skia_inline_fragment_advances(&["White", " ", "Space"], &link)
            .unwrap();
    assert_eq!(
        widths[..3].iter().flatten().sum::<f32>(),
        text_layout::inline_layout_advance(raw.iter().sum()),
        "the authored source endpoint is snapped once, not once per word"
    );
}

#[test]
fn empty_decorated_inline_exports_its_font_box_baseline() {
    use w3cos_std::Style;
    let inline = Style {
        display: WDisplay::Inline,
        font_size: 50.0,
        border_top_width: Some(1.0),
        border_bottom_width: Some(1.0),
        padding: w3cos_std::style::Edges {
            left: WSpacing::Em(1.0),
            ..w3cos_std::style::Edges::ZERO
        },
        ..Style::default()
    };
    let root = Component::row(
        Style {
            display: WDisplay::InlineBlock,
            ..inline.clone()
        },
        vec![Component::row(inline.clone(), vec![])],
    );
    let layouts = vec![
        (
            LayoutRect {
                x: 8.0,
                y: 54.0,
                width: 50.0,
                height: 61.0,
            },
            0,
        ),
        (
            LayoutRect {
                x: 8.0,
                y: 53.0,
                width: 50.0,
                height: 63.0,
            },
            1,
        ),
    ];
    let positions = [(0, 0), (1, 1)]
        .into_iter()
        .collect::<crate::layout::LayoutPositions>();
    assert_eq!(
        last_text_baseline(&root, 0, &layouts, &positions),
        Some(54.0 + inline_font_content_ascent(&inline)),
        "empty inline decoration creates a line, not a bottom-edge atomic baseline"
    );
    let empty = Component::row(
        Style {
            display: WDisplay::Inline,
            ..Style::default()
        },
        vec![],
    );
    let zero = vec![(
        LayoutRect {
            x: 8.0,
            y: 54.0,
            width: 0.0,
            height: 0.0,
        },
        0,
    )];
    assert_eq!(
        last_text_baseline(
            &empty,
            0,
            &zero,
            &[(0, 0)]
                .into_iter()
                .collect::<crate::layout::LayoutPositions>()
        ),
        None
    );
}

#[test]
fn content_atomic_line_does_not_add_bottom_edge_descent() {
    use w3cos_std::Style;
    let parent = Style {
        display: WDisplay::Block,
        font_size: 16.0,
        line_height: 1.375,
        ..Style::default()
    };
    for display in [WDisplay::InlineBlock, WDisplay::InlineTable] {
        let atomic = Style {
            display,
            margin: w3cos_std::style::Edges {
                top: WSpacing::Px(3.0),
                bottom: WSpacing::Px(3.0),
                ..w3cos_std::style::Edges::ZERO
            },
            ..parent.clone()
        };
        let text = Component::text(
            "abcde",
            Style {
                display: WDisplay::Inline,
                ..parent.clone()
            },
        );
        let child = if display == WDisplay::InlineTable {
            Component::row(
                atomic,
                vec![Component::row(
                    Style {
                        display: WDisplay::TableRow,
                        ..parent.clone()
                    },
                    vec![Component::row(
                        Style {
                            display: WDisplay::TableCell,
                            ..parent.clone()
                        },
                        vec![text],
                    )],
                )],
            )
        } else {
            Component::row(atomic, vec![text])
        };
        let root = Component::row(parent.clone(), vec![child]);
        let rect = |y, height| LayoutRect {
            x: 13.0,
            y,
            width: 45.328125,
            height,
        };
        let mut layouts = vec![(rect(29.0, 108.0), 0), (rect(32.0, 102.0), 1)];
        if display == WDisplay::InlineTable {
            layouts.extend([
                (rect(50.0, 48.0), 2),
                (rect(50.0, 48.0), 3),
                (rect(63.0, 22.0), 4),
            ]);
        } else {
            layouts.push((rect(99.0, 22.0), 2));
        }
        project_inline_block_atomic_line_descent(&mut layouts, &root);
        assert_eq!(
            layouts[0].0.height, 108.0,
            "{display:?}: internal line baseline already leaves enough descent"
        );
    }
}

#[test]
fn nested_columns_fragment_float_child_margin_into_outer_continuation() {
    use crate::paint_artifact::{PaintArtifact, PaintNode};
    use w3cos_std::Style;
    // Canonical Chrome fragments in V3942: outer height22; the child's16px
    // margin consumes the tail of the inner second column. Its complete22px
    // line continues in the first inner column of the next outer fragment.
    let block = Style {
        display: WDisplay::Block,
        font_size: 16.0,
        line_height: 1.375,
        line_height_is_normal: true,
        ..Style::default()
    };
    let outer = Style {
        width: WDim::Px(300.0),
        column_count: Some(2),
        column_gap: Some(16.0),
        ..block.clone()
    };
    let inner = Style {
        width: WDim::Auto,
        ..outer.clone()
    };
    let text = Style {
        display: WDisplay::Inline,
        ..block.clone()
    };
    let float = Style {
        float: WFloat::Left,
        ..block.clone()
    };
    let child = Style {
        margin: w3cos_std::style::Edges {
            top: WSpacing::Em(1.0),
            ..w3cos_std::style::Edges::ZERO
        },
        ..block.clone()
    };
    let nodes = vec![
        (&outer, None),
        (&inner, Some(0)),
        (&block, Some(1)),
        (&text, Some(2)),
        (&float, Some(1)),
        (&child, Some(4)),
        (&text, Some(5)),
    ];
    let rect = |y, width, height| {
        Some(LayoutRect {
            x: 8.0,
            y,
            width,
            height,
        })
    };
    let rects = vec![
        rect(8.0, 300.0, 60.0),
        rect(8.0, 142.0, 60.0),
        rect(8.0, 63.0, 22.0),
        rect(8.0, 5.328125, 22.0),
        rect(30.0, 33.0, 38.0),
        rect(46.0, 33.0, 22.0),
        rect(46.0, 33.0, 22.0),
    ];
    let plans = resolve_column_fragment_plans(&nodes, &rects, Some((800.0, 600.0)));
    assert_eq!(
        plans[0].as_ref().map(|plan| plan.height),
        Some(22.0),
        "outer fragment capacity must constrain nested balancing, not treat its float as indivisible"
    );
    let paint_nodes: Vec<PaintNode> = nodes
        .iter()
        .enumerate()
        .map(|(index, (style, parent))| PaintNode {
            kind: if index == 3 || index == 6 {
                ComponentKind::Text {
                    content: if index == 6 { "ABC" } else { " " }.into(),
                }
            } else {
                ComponentKind::Column
            },
            style: (*style).clone(),
            parent: *parent,
            sticky_counter_signal: None,
        })
        .collect();
    let layouts: Vec<_> = rects
        .iter()
        .enumerate()
        .map(|(index, rect)| (rect.unwrap(), index))
        .collect();
    let artifact = PaintArtifact::build(paint_nodes, &layouts, 1);
    let line = artifact
        .display_items
        .iter()
        .find(|item| item.client_index == 6)
        .unwrap();
    assert_eq!(
        line.visual_rect,
        LayoutRect {
            x: 166.0,
            y: 8.0,
            width: 33.0,
            height: 22.0
        },
        "the complete line continues into the outer next column, not inner column overflow"
    );
    let fragments = |index: usize| {
        artifact.column_fragments[index]
            .iter()
            .map(|fragment| fragment.visual_rect)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        fragments(4),
        vec![
            LayoutRect {
                x: 87.0,
                y: 8.0,
                width: 33.0,
                height: 22.0
            },
            LayoutRect {
                x: 166.0,
                y: 8.0,
                width: 33.0,
                height: 22.0
            }
        ],
        "floating decoration spans both outer fragments"
    );
    assert_eq!(
        fragments(5),
        vec![
            LayoutRect {
                x: 87.0,
                y: 24.0,
                width: 33.0,
                height: 6.0
            },
            LayoutRect {
                x: 166.0,
                y: 8.0,
                width: 33.0,
                height: 22.0
            }
        ],
        "retained16px margin leaves a6px empty decoration tail before the complete line"
    );
}

#[test]
fn floated_bfc_retains_first_child_top_margin_inside_its_border_box() {
    use w3cos_std::Style;
    let base = Style {
        display: WDisplay::Block,
        font_size: 16.0,
        ..Style::default()
    };
    let root = Component::column(
        Style {
            display: WDisplay::FlowRoot,
            width: WDim::Px(200.0),
            ..base.clone()
        },
        vec![Component::column(
            Style {
                float: WFloat::Left,
                width: WDim::Px(40.0),
                ..base.clone()
            },
            vec![Component::column(
                Style {
                    height: WDim::Px(22.0),
                    margin: w3cos_std::style::Edges {
                        top: WSpacing::Em(1.0),
                        ..w3cos_std::style::Edges::ZERO
                    },
                    ..base
                },
                vec![],
            )],
        )],
    );
    let flat = pre_flatten(&root);
    for layouts in [
        compute(&root, 800.0, 600.0).unwrap(),
        LayoutEngine::new()
            .compute(&root, &flat, 800.0, 600.0)
            .unwrap()
            .layout_cache,
    ] {
        let rect = |index| layouts.iter().find(|(_, id)| *id == index).unwrap().0;
        assert_eq!(
            rect(2).y - rect(1).y,
            16.0,
            "a floating BFC does not collapse with its first child's margin: {layouts:?}"
        );
        assert_eq!(
            rect(1).height,
            38.0,
            "the float contains its child margin and22px content"
        );
    }
}

#[test]
fn auto_column_height_reflows_following_block_and_ancestors() {
    use w3cos_std::Style;
    let base = Style {
        display: WDisplay::Block,
        width: WDim::Px(300.0),
        font_family: Some("Times".into()),
        font_size: 16.0,
        line_height: 1.375,
        line_height_is_normal: true,
        ..Style::default()
    };
    let owner = Component::column(
        Style {
            column_count: Some(2),
            column_gap: Some(16.0),
            ..base.clone()
        },
        ["A", "B", "C"]
            .into_iter()
            .map(|content| {
                Component::column(
                    Style {
                        height: WDim::Px(22.0),
                        width: WDim::Auto,
                        ..base.clone()
                    },
                    vec![Component::text(
                        content,
                        Style {
                            display: WDisplay::Inline,
                            width: WDim::Auto,
                            ..base.clone()
                        },
                    )],
                )
            })
            .collect(),
    );
    let root = Component::column(
        base.clone(),
        vec![
            owner,
            Component::column(
                Style {
                    height: WDim::Px(10.0),
                    ..base
                },
                vec![],
            ),
        ],
    );
    let flat = pre_flatten(&root);
    let owner = flat
        .iter()
        .position(|node| node.style.column_count == Some(2))
        .unwrap();
    let following = flat
        .iter()
        .position(|node| node.style.height == WDim::Px(10.0))
        .unwrap();
    for layouts in [
        compute(&root, 800.0, 600.0).unwrap(),
        LayoutEngine::new()
            .compute(&root, &flat, 800.0, 600.0)
            .unwrap()
            .layout_cache,
    ] {
        let rect = |index| layouts.iter().find(|(_, id)| *id == index).unwrap().0;
        assert_eq!(
            rect(owner).height,
            44.0,
            "automatic principal must use the planned height: {layouts:?}"
        );
        assert_eq!(
            rect(following).y,
            rect(owner).y + 44.0,
            "following flow must not retain the66px logical extent"
        );
        assert_eq!(
            rect(0).height,
            54.0,
            "the settled height reaches normal auto ancestors"
        );
    }
}

#[test]
fn auto_column_artifact_uses_real_breaks_not_uniform_height_division() {
    use crate::paint_artifact::{PaintArtifact, PaintNode};
    use w3cos_std::Style;
    let owner = Style {
        display: WDisplay::Block,
        column_count: Some(2),
        font_size: 16.0,
        ..Style::default()
    };
    let text = Style {
        display: WDisplay::Inline,
        font_size: 16.0,
        line_height: 1.375,
        line_height_is_normal: true,
        ..Style::default()
    };
    for (lines, gap, height) in [(3, 0.0, 44.0), (2, 16.0, 30.0)] {
        let extent = lines as f32 * 22.0 + (lines - 1) as f32 * gap;
        let mut nodes = vec![PaintNode {
            kind: ComponentKind::Column,
            style: owner.clone(),
            parent: None,
            sticky_counter_signal: None,
        }];
        let mut layouts = vec![(
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: extent,
            },
            0,
        )];
        for line in 0..lines {
            nodes.push(PaintNode {
                kind: ComponentKind::Text {
                    content: "X".into(),
                },
                style: text.clone(),
                parent: Some(0),
                sticky_counter_signal: None,
            });
            layouts.push((
                LayoutRect {
                    x: 0.0,
                    y: line as f32 * (22.0 + gap),
                    width: 10.0,
                    height: 22.0,
                },
                line + 1,
            ));
        }
        let artifact = PaintArtifact::build(nodes, &layouts, 1);
        let last = artifact
            .display_items
            .iter()
            .find(|item| item.client_index == lines)
            .unwrap();
        assert_eq!(
            last.visual_rect,
            LayoutRect {
                x: 158.0,
                y: 0.0,
                width: 10.0,
                height: 22.0
            },
            "ragged/discarded-margin break must move a complete line, lines={lines}, gap={gap}"
        );
        let principal = artifact
            .display_items
            .iter()
            .find(|item| item.client_index == 0)
            .unwrap();
        assert_eq!(
            principal.visual_rect.height, height,
            "automatic principal height uses the shared plan"
        );
    }
}

#[test]
fn auto_column_balance_propagates_nested_height_and_keeps_text_lines_whole() {
    use w3cos_std::Style;
    let outer = Style {
        display: WDisplay::Block,
        width: WDim::Px(300.0),
        column_count: Some(2),
        font_size: 16.0,
        ..Style::default()
    };
    let inner = Style {
        width: WDim::Auto,
        ..outer.clone()
    };
    let block = Style {
        display: WDisplay::Block,
        ..Style::default()
    };
    let text = Style {
        display: WDisplay::Inline,
        font_size: 16.0,
        line_height: 1.375,
        line_height_is_normal: true,
        ..Style::default()
    };
    let float = Style {
        float: WFloat::Left,
        ..block.clone()
    };
    let nodes = vec![
        (&outer, None),
        (&inner, Some(0)),
        (&block, Some(1)),
        (&text, Some(2)),
        (&float, Some(1)),
        (&block, Some(4)),
        (&text, Some(5)),
    ];
    let rect = |x, y, width, height| {
        Some(LayoutRect {
            x,
            y,
            width,
            height,
        })
    };
    let rects = vec![
        rect(8.0, 8.0, 300.0, 60.0),
        rect(8.0, 8.0, 142.0, 60.0),
        rect(8.0, 8.0, 63.0, 22.0),
        rect(8.0, 8.0, 5.0, 22.0),
        rect(8.0, 30.0, 33.0, 22.0),
        rect(8.0, 30.0, 33.0, 22.0),
        rect(8.0, 30.0, 33.0, 22.0),
    ];
    let heights: Vec<_> = resolve_column_fragment_plans(&nodes, &rects, Some((800.0, 600.0)))
        .into_iter()
        .map(|plan| plan.map(|plan| plan.height))
        .collect();
    assert_eq!(
        heights[1],
        Some(22.0),
        "two complete22px lines balance, not glyph slices"
    );
    assert_eq!(
        heights[0],
        Some(22.0),
        "outer context sees the balanced nested height"
    );
}

#[test]
fn auto_column_balance_respects_line_breaks_after_the_continuous_lower_bound() {
    use w3cos_std::Style;
    let owner = Style {
        display: WDisplay::Block,
        column_count: Some(2),
        font_size: 16.0,
        ..Style::default()
    };
    let block = Style {
        display: WDisplay::Block,
        ..Style::default()
    };
    let text = Style {
        display: WDisplay::Inline,
        font_size: 16.0,
        line_height: 1.375,
        line_height_is_normal: true,
        ..Style::default()
    };
    for (lines, gap, expected) in [
        (2, 0.0, 22.0),
        (3, 0.0, 44.0),
        (4, 0.0, 44.0),
        (2, 16.0, 30.0),
        (3, 16.0, 60.0),
    ] {
        let extent = lines as f32 * 22.0 + (lines - 1) as f32 * gap;
        let mut nodes = vec![(&owner, None)];
        let mut rects = vec![Some(LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 300.0,
            height: extent,
        })];
        for line in 0..lines {
            let index = nodes.len();
            let rect = LayoutRect {
                x: 0.0,
                y: line as f32 * (22.0 + gap),
                width: 142.0,
                height: 22.0,
            };
            nodes.push((&block, Some(0)));
            rects.push(Some(rect));
            nodes.push((&text, Some(index)));
            rects.push(Some(rect));
        }
        let heights: Vec<_> = resolve_column_fragment_plans(&nodes, &rects, Some((800.0, 600.0)))
            .into_iter()
            .map(|plan| plan.map(|plan| plan.height))
            .collect();
        assert_eq!(
            heights[0],
            Some(expected),
            "Chrome155 legal complete-line breaks: lines={lines}, gap={gap}"
        );
    }
}

#[test]
fn next_line_float_does_not_add_leading_twice_to_normal_text_box() {
    use crate::html_parser_host::InertParserScriptHost;
    use crate::html_parser_state::StreamingDocumentParser;
    use std::rc::Rc;
    crate::dom::reset_document();
    crate::jsdom::reset_bridge();
    let source = std::fs::read_to_string(format!(
        "{}/assets/wpt/css/CSS2/floats/floats-placement-vertical-004.xht",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("pinned WPT original");
    let mut parser = StreamingDocumentParser::new_with_script_host(
        Rc::new(InertParserScriptHost),
        "https://example.test/float.xht",
    )
    .unwrap();
    parser.write(&source).unwrap();
    parser.finish().unwrap();
    let root = crate::dom::to_component_tree();
    let flat = pre_flatten(&root);
    let floats: Vec<_> = flat
        .iter()
        .enumerate()
        .filter(|(_, node)| node.style.float != WFloat::None)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(floats.len(), 2);
    for layouts in [
        compute(&root, 800.0, 600.0).unwrap(),
        LayoutEngine::new()
            .compute(&root, &flat, 800.0, 600.0)
            .unwrap()
            .layout_cache,
    ] {
        let text = flat
            .iter()
            .position(|node| matches!(node.kind, ComponentKind::Text { .. }))
            .unwrap();
        let rect = |index| layouts.iter().find(|(_, id)| *id == index).unwrap().0;
        assert_eq!(
            rect(floats[1]).y - rect(floats[0]).y,
            rect(text).height,
            "the next float starts on the following line box, not an extra leading; {layouts:?}"
        );
    }
}

#[test]
fn nonfitting_bfc_margin_separates_at_the_adjoining_float_band() {
    use crate::html_parser_host::InertParserScriptHost;
    use crate::html_parser_state::StreamingDocumentParser;
    use std::rc::Rc;
    for (name, width, branch_margin, expected_height) in [
        ("new-fc-separates-from-float.html", 200.0, 0.0, 201.0),
        ("new-fc-separates-from-float-2.html", 200.0, -50.0, 200.0),
        ("new-fc-separates-from-float-3.html", 100.0, 0.0, 100.0),
        ("new-fc-separates-from-float-4.html", 200.0, 0.0, 201.0),
    ] {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let source = std::fs::read_to_string(format!(
            "{}/assets/wpt/css/CSS2/floats/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("pinned WPT original");
        let mut parser = StreamingDocumentParser::new_with_script_host(
            Rc::new(InertParserScriptHost),
            &format!("https://example.test/{name}"),
        )
        .unwrap();
        parser.write(&source).unwrap();
        parser.finish().unwrap();
        let root = crate::dom::to_component_tree();
        let flat = pre_flatten(&root);
        let owner = flat
            .iter()
            .position(|node| {
                node.style.width == WDim::Px(width)
                    && node.style.float == WFloat::None
                    && node.style.margin.top == WSpacing::Px(0.0)
                    && establishes_float_bfc(node.style, false)
            })
            .unwrap();
        let floating = flat
            .iter()
            .position(|node| node.style.float != WFloat::None)
            .unwrap();
        let separated = flat
            .iter()
            .position(|node| {
                node.style.margin_lengths().top > 0.0
                    && node.style.width == WDim::Px(width)
                    && establishes_float_bfc(node.style, false)
            })
            .unwrap();
        for layouts in [
            compute(&root, 800.0, 600.0).unwrap(),
            LayoutEngine::new()
                .compute(&root, &flat, 800.0, 600.0)
                .unwrap()
                .layout_cache,
        ] {
            let rect = |index| layouts.iter().find(|(_, id)| *id == index).unwrap().0;
            assert_eq!(
                rect(floating).y,
                rect(owner).y + branch_margin,
                "non-fitting BFC margin must not pull the preceding float down: {name}, {layouts:?}"
            );
            assert_eq!(
                rect(separated).y,
                rect(floating).y + rect(floating).height,
                "the separated BFC starts below the float: {name}, {layouts:?}"
            );
            assert_eq!(
                rect(owner).height,
                expected_height,
                "non-adjoining margin must not enlarge its containing BFC: {name}, {layouts:?}"
            );
        }
    }
}

#[test]
fn character_unit_uses_font_zero_not_shaped_fixed_point_advance() {
    install_font_unit_provider();
    let style = w3cos_std::Style {
        font_size: 16.0,
        custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
            w3cos_dom::user_agent::HTML_STANDARD_FONT_PROPERTY.into(),
            "1".into(),
        )])),
        ..w3cos_std::Style::default()
    };
    assert_eq!(
        style.ch_advance() * 20.0,
        180.0,
        "Chrome155 original align001 has exact20ch192 and10ch96 boxes"
    );
}

#[test]
fn pure_image_soft_lines_keep_containing_font_descent() {
    let style = w3cos_std::Style {
        display: WDisplay::Flex,
        flex_wrap: WWrap::Wrap,
        width: WDim::Px(130.0),
        font_family: Some("Arial".into()),
        font_size: 20.0,
        line_height: 1.0,
        line_height_is_normal: false,
        custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
            "--w3cos-internal-inline-formatting-context".into(),
            "1".into(),
        )])),
        ..w3cos_std::Style::default()
    };
    let image = || {
        Component::image(
            "white.png",
            w3cos_std::Style {
                display: WDisplay::InlineBlock,
                width: WDim::Px(130.0),
                height: WDim::Px(20.0),
                ..style.clone()
            },
        )
    };
    let root = Component::row(style.clone(), vec![image(), image()]);
    let flat = pre_flatten(&root);
    for layouts in [
        compute(&root, 800.0, 600.0).unwrap(),
        LayoutEngine::new()
            .compute(&root, &flat, 800.0, 600.0)
            .unwrap()
            .layout_cache,
    ] {
        let rect = |index| layouts.iter().find(|(_, id)| *id == index).unwrap().0;
        assert_eq!(
            (rect(2).y, rect(0).height),
            (24.0, 48.0),
            "independent Chrome155 Arial20/20 geometry; {layouts:?}"
        );
    }
}

#[test]
fn horizontal_decorated_collapsible_separator_stays_on_completed_line() {
    let style = w3cos_std::Style {
        display: WDisplay::Flex,
        flex_wrap: WWrap::Wrap,
        width: WDim::Px(130.0),
        font_family: Some("Arial".into()),
        font_size: 20.0,
        line_height: 1.0,
        line_height_is_normal: false,
        custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
            "--w3cos-internal-inline-formatting-context".into(),
            "1".into(),
        )])),
        ..w3cos_std::Style::default()
    };
    let image = || {
        Component::image(
            "white.png",
            w3cos_std::Style {
                display: WDisplay::InlineBlock,
                width: WDim::Px(130.0),
                height: WDim::Px(20.0),
                ..style.clone()
            },
        )
    };
    let separator = Component::text(
        " ",
        w3cos_std::Style {
            display: WDisplay::Inline,
            padding: w3cos_std::style::Edges {
                top: WSpacing::Px(20.0),
                bottom: WSpacing::Px(20.0),
                left: WSpacing::Px(1.0),
                right: WSpacing::Px(1.0),
            },
            border_width: 10.0,
            border_left_width: Some(0.0),
            border_right_width: Some(0.0),
            ..style.clone()
        },
    );
    let root = Component::row(style.clone(), vec![image(), separator, image()]);
    let flat = pre_flatten(&root);
    for layouts in [
        compute(&root, 800.0, 600.0).unwrap(),
        LayoutEngine::new()
            .compute(&root, &flat, 800.0, 600.0)
            .unwrap()
            .layout_cache,
    ] {
        let rect = |index| layouts.iter().find(|(_, id)| *id == index).unwrap().0;
        assert_eq!(
            (rect(3).y, rect(0).height),
            (24.0, 48.0),
            "independent Chrome155: padding preserves painted width, not a third line; {layouts:?}"
        );
        assert_eq!(
            rect(2),
            LayoutRect {
                x: 130.0,
                y: -26.0,
                width: 2.0,
                height: 80.0
            },
            "the collapsed glyph still owns the browser's hanging decoration box"
        );
    }
}

#[test]
fn zero_width_split_inline_keeps_overflowing_words_on_separate_lines() {
    w3cos_dom::stylesheet::clear_rules();
    w3cos_dom::stylesheet::register_rule("span", &[("display", "inline")]);
    w3cos_dom::stylesheet::register_rule("body", &[("font", "16px/22px sans-serif")]);
    let mut document = w3cos_dom::Document::new();
    let html = document.create_element("html");
    let body = document.body();
    html.append_child(&mut document, body);
    document
        .body()
        .style_mut(&mut document)
        .set_property("width", "0px");
    let inline = document.create_element("span");
    inline
        .style_mut(&mut document)
        .set_property("border", "2px solid blue");
    let first = document.create_element("span");
    first
        .style_mut(&mut document)
        .set_property("display", "block");
    inline.append_child(&mut document, first);
    let text = document.create_text_node(" a b ");
    inline.append_child(&mut document, text);
    let second = document.create_element("span");
    second
        .style_mut(&mut document)
        .set_property("display", "block");
    inline.append_child(&mut document, second);
    let text = document.create_text_node(" c ");
    inline.append_child(&mut document, text);
    document.body().append_child(&mut document, inline);
    let tree = document.to_component_tree();
    let flat = pre_flatten(&tree);
    for layouts in [
        compute(&tree, 800.0, 600.0).unwrap(),
        LayoutEngine::new()
            .compute(&tree, &flat, 800.0, 600.0)
            .unwrap()
            .layout_cache,
    ] {
        let body = layouts.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert_eq!(
            body.height, 88.0,
            "leading decorated line plus a, b and c overflow lines"
        );
    }
    w3cos_dom::stylesheet::clear_rules();
}

#[test]
fn floated_descendant_retains_relative_inline_ancestor_offset() {
    w3cos_dom::stylesheet::clear_rules();
    let mut document = w3cos_dom::Document::new();
    let preceding = document.create_element("div");
    let text = document.create_text_node("aaa");
    preceding.append_child(&mut document, text);
    document.body().append_child(&mut document, preceding);
    let inline = document.create_element("span");
    inline
        .style_mut(&mut document)
        .set_property("position", "relative");
    inline
        .style_mut(&mut document)
        .set_property("left", "100px");
    let floating = document.create_element("span");
    floating
        .style_mut(&mut document)
        .set_property("float", "left");
    let text = document.create_text_node("bbb");
    floating.append_child(&mut document, text);
    inline.append_child(&mut document, floating);
    document.body().append_child(&mut document, inline);
    let tree = document.to_component_tree();
    let flat = pre_flatten(&tree);
    let floating = flat
        .iter()
        .position(|node| node.style.float == WFloat::Left)
        .unwrap();
    for layouts in [
        compute(&tree, 800.0, 600.0).unwrap(),
        LayoutEngine::new()
            .compute(&tree, &flat, 800.0, 600.0)
            .unwrap()
            .layout_cache,
    ] {
        let rect = layouts
            .iter()
            .find(|(_, index)| *index == floating)
            .unwrap()
            .0;
        assert_eq!(
            rect.x, 108.0,
            "float flow projection must retain the ancestor's visual offset"
        );
    }
    w3cos_dom::stylesheet::clear_rules();
}

#[test]
fn split_inline_ending_in_block_has_no_empty_trailing_line() {
    w3cos_dom::stylesheet::clear_rules();
    w3cos_dom::stylesheet::register_rule("td", &[("font", "13px/1 monospace")]);
    w3cos_dom::stylesheet::register_rule(".block", &[("display", "block"), ("margin", "1em 0")]);
    w3cos_dom::stylesheet::register_rule(".inline", &[("display", "inline")]);
    let mut document = w3cos_dom::Document::new();
    let table = document.create_element("table");
    let row = document.create_element("tr");
    let left = document.create_element("td");
    let right = document.create_element("td");
    let outer = document.create_element("div");
    outer.set_attribute(&mut document, "class", "block");
    let inline = document.create_element("div");
    inline.set_attribute(&mut document, "class", "inline");
    for letter in ['A', 'B', 'C', 'D', 'E', 'F', 'G'] {
        let text = document.create_text_node(&letter.to_string());
        if matches!(letter, 'B' | 'C' | 'E' | 'G') {
            let block = document.create_element("div");
            block.set_attribute(&mut document, "class", "block");
            block.append_child(&mut document, text);
            inline.append_child(&mut document, block);
        } else {
            inline.append_child(&mut document, text);
        }
    }
    outer.append_child(&mut document, inline);
    left.append_child(&mut document, outer);
    let pre = document.create_element("pre");
    let text = document.create_text_node("A\n\nB\n\nC\n\nD\n\nE\n\nF\n\nG");
    pre.append_child(&mut document, text);
    right.append_child(&mut document, pre);
    row.append_child(&mut document, left);
    row.append_child(&mut document, right);
    table.append_child(&mut document, row);
    document.body().append_child(&mut document, table);
    let tree = document.to_component_tree();
    let flat = pre_flatten(&tree);
    let text_index = |text: &str| {
        flat.iter()
            .position(
                |node| matches!(node.kind, ComponentKind::Text { content } if content == text),
            )
            .unwrap()
    };
    let first = text_index("A");
    let outer = flat[flat[first].parent.unwrap()].parent.unwrap();
    let pre = text_index("A\n\nB\n\nC\n\nD\n\nE\n\nF\n\nG");
    for layouts in [
        compute(&tree, 800.0, 600.0).unwrap(),
        LayoutEngine::new()
            .compute(&tree, &flat, 800.0, 600.0)
            .unwrap()
            .layout_cache,
    ] {
        let rect = |index| layouts.iter().find(|(_, item)| *item == index).unwrap().0;
        assert_eq!(
            rect(outer).height,
            169.0,
            "Chrome155 V3833: trailing block margin collapses through the inline end"
        );
        assert_eq!(
            rect(first).y,
            rect(pre).y,
            "equivalent column content has the same middle alignment"
        );
    }
    w3cos_dom::stylesheet::clear_rules();
}

#[test]
fn first_letter_font_boundary_uses_the_completed_layout_advance() {
    for character in [
        '\u{0708}',
        '\u{0709}',
        '\u{070a}',
        '\u{10a51}',
        '\u{10a52}',
        '\u{10a53}',
        '\u{10a54}',
        '\u{10a57}',
        '\u{10a58}',
        '\u{060d}',
        '\u{061f}',
    ] {
        w3cos_dom::stylesheet::clear_rules();
        w3cos_dom::stylesheet::register_rule("div", &[("font-family", "PingFang SC")]);
        w3cos_dom::stylesheet::register_rule(
            "div::first-letter",
            &[
                ("font-size", "36px"),
                ("line-height", "2"),
                ("color", "green"),
            ],
        );
        let mut document = w3cos_dom::Document::new();
        let block = document.create_element("div");
        let prefix = format!("{character}T{character}");
        let text = document.create_text_node(&format!("{prefix}est"));
        block.append_child(&mut document, text);
        document.body().append_child(&mut document, block);
        let tree = document.to_component_tree();
        let flat = pre_flatten(&tree);
        let index =
            |text: &str| {
                flat.iter().position(|node|
            matches!(node.kind, ComponentKind::Text { content } if content == text)).unwrap()
            };
        let prefix_index = index(&prefix);
        let suffix_index = index("est");
        let style = flat[prefix_index].style;
        let advance = text_intrinsic_size(&prefix, style).0;
        for layouts in [
            compute(&tree, 800.0, 600.0).unwrap(),
            LayoutEngine::new()
                .compute(&tree, &flat, 800.0, 600.0)
                .unwrap()
                .layout_cache,
        ] {
            let rect = |index| layouts.iter().find(|(_, item)| *item == index).unwrap().0;
            let actual = rect(suffix_index).x - rect(prefix_index).x;
            assert!(
                (actual - advance).abs() < 1.0 / 64.0,
                "completed layout advance, {character:?}, layout={actual}, measured={advance}, prefix={:?}, suffix={:?}",
                rect(prefix_index),
                rect(suffix_index)
            );
        }
    }
    w3cos_dom::stylesheet::clear_rules();
}

#[test]
fn generated_border_words_use_shared_layout_advances() {
    let text_style = w3cos_std::Style {
        display: WDisplay::Inline,
        font_size: 16.0,
        font_family: Some("sans-serif".into()),
        border_top_width: Some(3.0),
        custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
            w3cos_std::inline_text::SOURCE_RUN.into(),
            "text:1".into(),
        )])),
        ..Default::default()
    };
    let words = ["Filler", " ", "Text", " ", "Filler", " ", "Text"];
    let root = Component::row(
        w3cos_std::Style {
            display: WDisplay::Flex,
            custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
                "--w3cos-internal-inline-formatting-context".into(),
                "1".into(),
            )])),
            ..Default::default()
        },
        words
            .iter()
            .map(|word| Component::text(*word, text_style.clone()))
            .collect(),
    );
    let measured = inline_fragment_shaped_advances_for_width(&root, Some(800.0));
    let expected =
        crate::render_skia::measure_skia_inline_fragment_advances(&words, &text_style).unwrap();
    for (index, width) in measured.iter().enumerate() {
        let actual = width.expect("paint-only border must not block shared layout measurement");
        assert!(
            (actual - expected[index]).abs() < 1.0 / 64.0,
            "fragment {index}: {actual} != {}",
            expected[index]
        );
    }
    let mut followed = root.clone();
    followed.children.push(Component::text(
        " ",
        w3cos_std::Style {
            display: WDisplay::Inline,
            font_size: 16.0,
            font_family: Some("sans-serif".into()),
            ..Default::default()
        },
    ));
    let completed = inline_fragment_shaped_advances_for_width(&followed, Some(800.0));
    let advance: f32 = completed[..words.len()]
        .iter()
        .map(|width| width.unwrap())
        .sum();
    let raw: f32 = expected.iter().sum();
    assert!(
        (advance - text_layout::inline_layout_advance(raw)).abs() < 1.0 / 64.0,
        "completed authored source uses a LayoutUnit advance: {advance} vs {raw}"
    );
}

#[test]
fn generated_border_box_words_keep_horizontal_edges_in_used_width() {
    let words = ["Filler", " ", "Text"];
    let text_style = w3cos_std::Style {
        display: WDisplay::Inline,
        font_size: 16.0,
        font_family: Some("sans-serif".into()),
        box_sizing: WBoxSizing::BorderBox,
        border_top_width: Some(3.0),
        custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
            w3cos_std::inline_text::SOURCE_RUN.into(),
            "text:1".into(),
        )])),
        ..Default::default()
    };
    let mut children = words
        .iter()
        .map(|word| Component::text(*word, text_style.clone()))
        .collect::<Vec<_>>();
    children[0].style.border_left_width = Some(3.0);
    children[2].style.border_right_width = Some(5.0);
    let root = Component::row(
        w3cos_std::Style {
            display: WDisplay::Flex,
            custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
                "--w3cos-internal-inline-formatting-context".into(),
                "1".into(),
            )])),
            ..Default::default()
        },
        children,
    );
    let measured = inline_fragment_shaped_advances_for_width(&root, Some(800.0));
    let expected =
        crate::render_skia::measure_skia_inline_fragment_advances(&words, &text_style).unwrap();
    for (index, edge) in [3.0, 0.0, 5.0].into_iter().enumerate() {
        assert!(
            (measured[index].unwrap() - expected[index] - edge).abs() < 1.0 / 64.0,
            "border-box fragment {index} must include its own logical edge: {:?}",
            measured[index]
        );
    }
}

#[test]
fn nested_normal_inline_text_wraps_without_making_nowrap_shrink() {
    let sentence = "This should have a green border, because the computed value of border-color is currentColor.";
    let word =
        "WWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWW";
    for (white_space, content, wraps) in [
        (WWhiteSpace::Normal, sentence, true),
        (WWhiteSpace::NoWrap, sentence, false),
        (WWhiteSpace::Pre, sentence, false),
        (WWhiteSpace::Normal, word, false),
    ] {
        let text_style = w3cos_std::style::Style {
            display: WDisplay::Inline,
            font_size: 16.0,
            font_family: Some("sans-serif".into()),
            white_space,
            ..Default::default()
        };
        let text = Component::text(content, text_style.clone());
        let root = Component::row(
            w3cos_std::style::Style {
                display: WDisplay::Flex,
                width: WDim::Px(300.0),
                flex_wrap: WWrap::Wrap,
                custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
                    "--w3cos-internal-inline-formatting-context".into(),
                    "1".into(),
                )])),
                ..text_style.clone()
            },
            vec![Component::row(text_style, vec![text])],
        );
        for layouts in [
            compute(&root, 800.0, 600.0).unwrap(),
            LayoutEngine::new()
                .compute(&root, &pre_flatten(&root), 800.0, 600.0)
                .unwrap()
                .layout_cache,
        ] {
            let rect = |index| layouts.iter().find(|(_, item)| *item == index).unwrap().0;
            if wraps {
                assert!(
                    rect(2).width <= 300.0,
                    "normal inline escaped its containing line: {:?}",
                    rect(2)
                );
                assert!(
                    rect(2).height > inline_style_line_height(&root.children[0].style),
                    "normal inline must wrap into multiple lines: {:?}",
                    rect(2)
                );
            } else {
                assert!(
                    rect(2).width > 300.0,
                    "unbroken text keeps its advance: {:?}",
                    rect(2)
                );
            }
        }
    }
}
