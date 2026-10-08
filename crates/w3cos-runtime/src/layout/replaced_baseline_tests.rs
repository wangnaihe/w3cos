use super::*;
use w3cos_std::Style;

#[test]
fn edge_aligned_cell_image_reserves_the_containing_line_strut() {
    for alignment in [WAlignSelf::Center, WAlignSelf::FlexEnd] {
        for image_alignment in [WAlignSelf::FlexStart, WAlignSelf::FlexEnd] {
            for extra in [0.0, 20.0] {
                let mut style = Style { display: WDisplay::TableCell,
                    align_self: alignment, font_family: Some("Ahem".into()),
                    font_size: 20.0, line_height: 1.1, line_height_is_normal: false,
                    ..Style::default() };
                style.padding.bottom = WSpacing::Px(20.0);
                let cell = Component::row(style.clone(), vec![Component::image(
                    "cell-image-strut.png", Style { display: WDisplay::InlineBlock,
                        align_self: image_alignment, width: WDim::Px(20.0),
                        height: WDim::Px(20.0), ..style.clone() })]);
                let image_top = if image_alignment == WAlignSelf::FlexStart { 64.0 } else { 66.0 };
                let mut layouts = vec![
                    (LayoutRect { x: 8.0, y: 64.0, width: 40.0, height: 42.0 + extra }, 0),
                    (LayoutRect { x: 8.0, y: image_top, width: 20.0, height: 20.0 }, 1),
                ];
                // Cell padding is not inherited by the image.
                let mut cell = cell;
                cell.children[0].style.padding = Default::default();
                let offset = if alignment == WAlignSelf::Center { extra * 0.5 } else { extra };
                align_table_cell_baselines(&mut layouts, &pre_flatten(&cell), 800.0, 600.0);
                assert_eq!(layouts[1].0.y, image_top + offset,
                    "align the line box, not image ink: cell={alignment:?}, image={image_alignment:?}, extra={extra}");
                assert_eq!(layouts[0].0.height, 42.0 + extra);
            }
        }
    }
}

#[test]
fn wrapped_atomic_line_projects_preceding_text_only_line() {
    let text_style = Style { display: WDisplay::Inline,
        font_family: Some("Ahem".into()), font_size: 20.0,
        line_height: 1.0, line_height_is_normal: false, ..Style::default() };
    let mut host = Style { display: WDisplay::Flex, width: WDim::Px(100.0),
        ..text_style.clone() };
    host.custom_properties.get_or_insert_with(Default::default).insert(
        "--w3cos-internal-inline-formatting-context".into(), "1".into());
    let root = Component::row(host, vec![
        Component::text("XX", text_style.clone()),
        Component::text("XX", text_style.clone()),
        Component::text("X", text_style.clone()),
        Component::boxed(Style { display: WDisplay::InlineBlock,
            width: WDim::Px(20.0), height: WDim::Px(30.0), ..text_style }, vec![]),
    ]);
    let mut layouts = vec![
        (LayoutRect { x: 0.0, y: 0.0, width: 100.0, height: 54.0 }, 0),
        (LayoutRect { x: 0.0, y: 5.0, width: 40.0, height: 20.0 }, 1),
        (LayoutRect { x: 40.0, y: 5.0, width: 40.0, height: 20.0 }, 2),
        (LayoutRect { x: 0.0, y: 35.0, width: 20.0, height: 20.0 }, 3),
        (LayoutRect { x: 20.0, y: 25.0, width: 20.0, height: 30.0 }, 4),
    ];
    align_inline_block_last_line_baselines(&mut layouts, &root);
    assert_eq!(layouts[1].0.y, 0.0,
        "text-only first line owns its strut even when the next line contains an atomic");
}

#[test]
fn image_only_cells_share_content_baseline_and_reserve_descent() {
    let style = Style {
        font_family: Some("serif".into()),
        font_size: 16.0,
        line_height: 1.375,
        ..Style::default()
    };
    let image = |display| {
        Component::image(
            "table-baseline-image-fixture.png",
            Style {
                display,
                width: WDim::Px(15.0),
                height: WDim::Px(15.0),
                ..style.clone()
            },
        )
    };
    let table = Component::row(
        Style {
            display: WDisplay::Table,
            ..style.clone()
        },
        vec![Component::row(
            Style {
                display: WDisplay::TableRow,
                ..style.clone()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisplay::TableCell,
                        width: WDim::Px(80.0),
                        ..style.clone()
                    },
                    vec![image(WDisplay::Block), image(WDisplay::Block)],
                ),
                Component::boxed(
                    Style {
                        display: WDisplay::TableCell,
                        width: WDim::Px(15.0),
                        ..style.clone()
                    },
                    vec![image(WDisplay::InlineBlock)],
                ),
            ],
        )],
    );
    let flat = pre_flatten(&table);
    let mut engine = LayoutEngine::new();
    let paths = [
        compute(&table, 800.0, 600.0).unwrap(),
        engine
            .compute(&table, &flat, 800.0, 600.0)
            .unwrap()
            .layout_cache,
        engine
            .compute(&table, &flat, 800.0, 600.0)
            .unwrap()
            .layout_cache,
    ];
    for layouts in paths {
        let rect = |index| layouts.iter().find(|(_, item)| *item == index).unwrap().0;
        assert_eq!(
            rect(4).y + rect(4).height,
            rect(6).y + rect(6).height,
            "a cell without an inline line exports its content bottom baseline"
        );
        let descent = 22.0 - inline_font_baseline_from_line_top(&style, 22.0);
        assert!(
            (rect(1).height - (30.0 + descent)).abs() < 0.01,
            "the aligned inline cell still reserves the font strut descent: {:?}",
            rect(1)
        );
        assert_eq!(rect(0).height, rect(1).height);
        assert_eq!(rect(2).height, rect(1).height);
        assert_eq!(rect(5).height, rect(1).height);
    }
}

#[test]
fn replaced_cell_baseline_keeps_relative_paint_offset() {
    let style = Style {
        display: WDisplay::TableCell,
        font_size: 16.0,
        line_height: 1.375,
        ..Style::default()
    };
    let block = Component::image(
        "block-baseline-fixture.png",
        Style {
            display: WDisplay::Block,
            ..style.clone()
        },
    );
    let relative = Component::image(
        "relative-baseline-fixture.png",
        Style {
            display: WDisplay::InlineBlock,
            position: WPos::Relative,
            top: WDim::Px(3.0),
            ..style.clone()
        },
    );
    let row = Component::row(
        Style {
            display: WDisplay::TableRow,
            ..style.clone()
        },
        vec![
            Component::boxed(style.clone(), vec![block]),
            Component::boxed(style, vec![relative]),
        ],
    );
    let mut layouts = [
        (30.0, 0.0, 30.0),
        (15.0, 0.0, 30.0),
        (15.0, 0.0, 30.0),
        (15.0, 4.0, 15.0),
        (15.0, 3.0, 15.0),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (w, y, h))| {
        (
            LayoutRect {
                x: 0.0,
                y,
                width: w,
                height: h,
            },
            i,
        )
    })
    .collect::<Vec<_>>();
    // Row0, cell1, block image2, cell3, relative inline image4.
    layouts[3].0.y = 0.0;
    table_replaced_baseline::project(&mut layouts, &row, &pre_flatten(&row), 800.0, 600.0);
    assert_eq!(
        layouts[2].0.y + layouts[2].0.height,
        layouts[4].0.y + layouts[4].0.height - 3.0
    );
    let once = layouts.clone();
    table_replaced_baseline::project(&mut layouts, &row, &pre_flatten(&row), 800.0, 600.0);
    assert_eq!(layouts, once);
}

#[test]
fn block_replaced_image_in_cell_keeps_intrinsic_width() {
    let source = "table-cell-intrinsic-image-fixture.svg";
    crate::image_loader::decode_and_install(
        source,
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="15" height="15"/>"#,
    )
    .unwrap();
    let cell = Component::boxed(
        Style {
            display: WDisplay::TableCell,
            width: WDim::Px(80.0),
            ..Style::default()
        },
        vec![Component::image(
            source,
            Style {
                display: WDisplay::Block,
                ..Style::default()
            },
        )],
    );
    let row = Component::row(
        Style {
            display: WDisplay::TableRow,
            ..Style::default()
        },
        vec![cell],
    );
    let table = Component::row(
        Style {
            display: WDisplay::Table,
            ..Style::default()
        },
        vec![row],
    );
    let layouts = compute(&table, 800.0, 600.0).unwrap();
    let image = layouts.iter().find(|(_, index)| *index == 3).unwrap().0;
    crate::image_loader::invalidate(source);
    assert_eq!((image.width, image.height), (15.0, 15.0));
}

#[test]
fn sole_cell_image_uses_font_strut_baseline() {
    let style = Style {
        display: WDisplay::TableCell,
        font_size: 16.0,
        line_height: 1.375,
        ..Style::default()
    };
    let root = Component::row(
        style.clone(),
        vec![Component::image(
            "baseline-fixture.png",
            Style {
                display: WDisplay::InlineBlock,
                ..style.clone()
            },
        )],
    );
    let mut layouts = vec![
        (
            LayoutRect {
                x: 0.0,
                y: 54.0,
                width: 15.0,
                height: 22.0,
            },
            0,
        ),
        (
            LayoutRect {
                x: 0.0,
                y: 54.0,
                width: 15.0,
                height: 15.0,
            },
            1,
        ),
    ];
    project_single_baseline_replaced_image(&mut layouts, &root);
    let expected = 54.0 + inline_font_baseline_from_line_top(&style, 22.0) - 15.0;
    assert!(expected > 54.0);
    assert_eq!(layouts[1].0.y, expected);
    let once = layouts.clone();
    project_single_baseline_replaced_image(&mut layouts, &root);
    assert_eq!(layouts, once);
}

#[test]
fn sole_cell_image_preserves_explicit_top_alignment() {
    let style = Style {
        display: WDisplay::TableCell,
        ..Style::default()
    };
    let root = Component::row(
        style.clone(),
        vec![Component::image(
            "top-fixture.png",
            Style {
                display: WDisplay::InlineBlock,
                align_self: WAlignSelf::FlexStart,
                ..style
            },
        )],
    );
    let mut layouts = vec![
        (
            LayoutRect {
                x: 0.0,
                y: 54.0,
                width: 15.0,
                height: 22.0,
            },
            0,
        ),
        (
            LayoutRect {
                x: 0.0,
                y: 54.0,
                width: 15.0,
                height: 15.0,
            },
            1,
        ),
    ];
    let before = layouts.clone();
    project_single_baseline_replaced_image(&mut layouts, &root);
    assert_eq!(layouts, before);
}
