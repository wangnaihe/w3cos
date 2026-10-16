//! Deterministic offscreen rendering for conformance and embedding tools.

use anyhow::{Result, bail};
use std::collections::HashMap;

use crate::layout::{self, LayoutRect};
use crate::paint_artifact::{PaintArtifact, PaintNode};
#[cfg(feature = "skia")]
use crate::render_skia::SkiaRasterizer;

/// Pixel checksum used to prove a headless frame was actually painted.
///
/// Equivalent to `len + Σ byte[i] * ((i % 251) + 1)` over every byte, with
/// wrapping arithmetic. The factor walks 1..=251 so the hot loop does not
/// divide once per byte.
pub(crate) fn frame_byte_checksum(rgba: &[u8]) -> u64 {
    let mut checksum = rgba.len() as u64;
    let mut factor = 1u64;
    for &byte in rgba {
        checksum = checksum.wrapping_add(u64::from(byte).wrapping_mul(factor));
        factor += 1;
        if factor == 252 {
            factor = 1;
        }
    }
    checksum
}

/// One fully rasterized document frame in premultiplied RGBA byte order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlessFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Render the current live DOM through the same component, layout and Skia
/// stages used by native windows, without creating an operating-system window.
pub fn render_document_rgba(width: u32, height: u32) -> Result<HeadlessFrame> {
    render_document_rgba_inner(width, height, false)
}

/// Rasterize with tiny-skia even when Skia is also compiled in.
#[cfg(feature = "cpu-render")]
pub fn render_document_rgba_cpu(width: u32, height: u32) -> Result<HeadlessFrame> {
    render_document_rgba_inner(width, height, true)
}

fn render_document_rgba_inner(width: u32, height: u32, force_cpu: bool) -> Result<HeadlessFrame> {
    if width == 0 || height == 0 {
        bail!("headless viewport dimensions must be positive");
    }

    crate::incremental_layout::with_paint_layout(
        width as f32,
        height as f32,
        |root, layout_results| {
            finish_headless_frame(width, height, force_cpu, root, layout_results)
        },
    )
}

fn finish_headless_frame(
    width: u32,
    height: u32,
    force_cpu: bool,
    root: &w3cos_std::Component,
    layout_results: &layout::LayoutResults,
) -> Result<HeadlessFrame> {
    let flat = layout::pre_flatten(root);
    let layout_cache = &layout_results.layout_cache;
    if std::env::var_os("W3COS_DUMP_HEADLESS_LAYOUT").is_some() {
        for (rect, index) in layout_cache {
            if let Some(node) = flat.get(*index) {
                eprintln!(
                    "W3COS_HEADLESS_LAYOUT index={index} parent={:?} display={:?} position={:?} left={:?} top={:?} float={:?} kind={:?} width={:?} height={:?} margin={:?} padding={:?} align_self={:?} text_align={:?} justify_content={:?} rect={rect:?} font_family={:?} font_size={} word_spacing={} white_space={:?} line_height={} normal={} direction={:?} rendered_text={:?} font_metric={:?}",
                    node.parent,
                    node.style.display,
                    node.style.position,
                    node.style.left,
                    node.style.top,
                    node.style.float,
                    node.kind,
                    node.style.width,
                    node.style.height,
                    node.style.margin,
                    node.style.padding,
                    node.style.align_self,
                    node.style.text_align,
                    node.style.justify_content,
                    node.style.font_family,
                    node.style.font_size,
                    node.style.word_spacing,
                    node.style.white_space,
                    node.style.line_height,
                    node.style.line_height_is_normal,
                    node.style.direction,
                    match node.kind {
                        w3cos_std::ComponentKind::Text { content } => Some(
                            crate::text_layout::font_render_text_for_style(content, node.style)
                        ),
                        _ => None,
                    },
                    crate::font_face::FontRegistry::global().normal_line_height(&node.style),
                );
            }
        }
    }
    let body_index = flat.iter().position(|node| {
        matches!(
            node.on_click,
            w3cos_std::EventAction::NativeHost { id, .. }
                if *id == u64::from(crate::dom::body_id())
        )
    });
    let mut artifact = PaintArtifact::build_with_body_background_and_viewport(
        flat.iter().map(|node| PaintNode {
            kind: node.kind.clone(),
            style: node.style.clone(),
            parent: node.parent,
            sticky_counter_signal: node.sticky_counter_signal,
        }),
        &layout_cache,
        1,
        body_index,
        Some((width as f32, height as f32)),
    );
    artifact.viewport_scroll = crate::jsdom::window_scroll_offset();
    let mut nodes = artifact
        .rect_by_index
        .iter()
        .enumerate()
        .filter_map(|(index, rect)| {
            let rect = (*rect)?;
            let node = artifact.nodes.get(index)?;
            Some((index, rect, &node.kind, &node.style))
        })
        .collect::<Vec<(usize, LayoutRect, _, _)>>();
    nodes.sort_by(|(left, _, _, _), (right, _, _, _)| {
        artifact
            .paint_order_key(*left)
            .cmp(artifact.paint_order_key(*right))
    });

    let table_replay = crate::table_paint::replay(&nodes, &artifact);
    let nodes = table_replay
        .iter()
        .map(|node| {
            (
                node.index,
                node.rect,
                node.kind.as_ref(),
                node.style.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    let bidi_replay = crate::bidi_paint::replay(&nodes, &artifact);
    let nodes = bidi_replay
        .iter()
        .map(|node| {
            (
                node.index,
                node.rect,
                node.kind.as_ref(),
                node.style.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    if std::env::var_os("W3COS_DUMP_HEADLESS_LAYOUT").is_some() {
        for (index, rect, kind, style) in &nodes {
            eprintln!(
                "W3COS_HEADLESS_REPLAY index={index} rect={rect:?} kind={kind:?} color={:?} background={:?} opacity={} properties={:?} text_decoration={:?} applied_decorations={:?}",
                style.color,
                style.background,
                style.opacity,
                style.custom_properties,
                style.text_decoration,
                artifact
                    .ancestor_text_decorations(*index)
                    .iter()
                    .map(|owner| (
                        owner.style.text_decoration,
                        owner.style.color,
                        owner.style.font_size,
                        owner.style.font_family.as_deref(),
                        owner.baseline_shift
                    ))
                    .collect::<Vec<_>>()
            );
        }
    }
    let scroll_offsets = layout_results
        .scrollable_nodes
        .iter()
        .filter_map(|(index, _, _)| {
            let w3cos_std::EventAction::NativeHost { id, .. } = flat[*index].on_click else {
                return None;
            };
            u32::try_from(*id)
                .ok()
                .map(|node| (*index, crate::dom::get_scroll_offset(node)))
        })
        .collect::<HashMap<_, _>>();
    let scroll_info = crate::window::build_headless_scroll_info(
        &layout_results.scroll_ancestor,
        &layout_results.scrollable_nodes,
        &layout_results.clip_only_nodes,
        &scroll_offsets,
        layout_cache,
        &flat,
        width as f32,
        height as f32,
    );
    let rgba = rasterize_prepared_frame(width, height, &nodes, &scroll_info, &artifact, force_cpu)?;

    Ok(HeadlessFrame {
        width,
        height,
        rgba,
    })
}

/// Write a headless frame as a PNG. Used by the native-gaps demo.
pub fn write_frame_png(path: &std::path::Path, frame: &HeadlessFrame) -> Result<()> {
    image::save_buffer(
        path,
        &frame.rgba,
        frame.width,
        frame.height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|err| anyhow::anyhow!("failed to write {}: {err}", path.display()))
}

fn rasterize_prepared_frame(
    width: u32,
    height: u32,
    nodes: &[(
        usize,
        LayoutRect,
        &w3cos_std::component::ComponentKind,
        &w3cos_std::style::Style,
    )],
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    artifact: &PaintArtifact,
    force_cpu: bool,
) -> Result<Vec<u8>> {
    if force_cpu {
        #[cfg(feature = "cpu-render")]
        {
            let _ = artifact;
            let mut pixmap = tiny_skia::Pixmap::new(width, height)
                .ok_or_else(|| anyhow::anyhow!("viewport is too large to rasterize on the CPU"))?;
            let mut clips = crate::render_cpu::ClipMaskCache::default();
            crate::render_cpu::render_frame(
                &mut pixmap,
                nodes,
                layout::layout_font(),
                scroll_info,
                &HashMap::new(),
                None,
                &mut clips,
            );
            return Ok(pixmap.take());
        }
        #[cfg(not(feature = "cpu-render"))]
        {
            let _ = (width, height, nodes, scroll_info, artifact);
            bail!("cpu renderer is not compiled in");
        }
    }
    // Skia software raster is the conformance path. Headless machines without
    // that feature use the tiny-skia CPU renderer instead of opening a GPU
    // window. Prefer Skia when both renderers are compiled in so existing
    // pixel tests keep a single backend.
    #[cfg(feature = "skia")]
    {
        let mut rasterizer = SkiaRasterizer::new_host()
            .ok_or_else(|| anyhow::anyhow!("host W3COS font is unavailable to Skia"))?;
        return rasterizer
            .render_frame(
                width,
                height,
                nodes,
                layout::layout_font(),
                scroll_info,
                &HashMap::new(),
                None,
                artifact.canvas_background,
                Some(artifact),
                None,
                1.0,
            )
            .map(|rgba| rgba.to_vec())
            .ok_or_else(|| anyhow::anyhow!("Skia failed to rasterize the headless document"));
    }
    #[cfg(all(feature = "cpu-render", not(feature = "skia")))]
    {
        let _ = artifact;
        let mut pixmap = tiny_skia::Pixmap::new(width, height)
            .ok_or_else(|| anyhow::anyhow!("viewport is too large to rasterize on the CPU"))?;
        let mut clips = crate::render_cpu::ClipMaskCache::default();
        crate::render_cpu::render_frame(
            &mut pixmap,
            nodes,
            layout::layout_font(),
            scroll_info,
            &HashMap::new(),
            None,
            &mut clips,
        );
        return Ok(pixmap.take());
    }
    #[cfg(not(any(feature = "skia", all(feature = "cpu-render", not(feature = "skia")))))]
    {
        let _ = (width, height, nodes, scroll_info, artifact);
        bail!("headless rasterizer is not compiled in");
    }
}

#[cfg(test)]
mod checksum_tests {
    use super::frame_byte_checksum;

    fn naive(rgba: &[u8]) -> u64 {
        let mut checksum = rgba.len() as u64;
        for (index, byte) in rgba.iter().enumerate() {
            checksum =
                checksum.wrapping_add(u64::from(*byte).wrapping_mul((index as u64 % 251) + 1));
        }
        checksum
    }

    #[test]
    fn frame_checksum_matches_the_modulo_formula() {
        for len in [0usize, 1, 250, 251, 252, 503, 4096] {
            let rgba = (0..len)
                .map(|index| (index.wrapping_mul(17)) as u8)
                .collect::<Vec<_>>();
            assert_eq!(frame_byte_checksum(&rgba), naive(&rgba), "len {len}");
        }
    }
}

#[cfg(all(test, feature = "skia"))]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_sized_viewports() {
        assert!(render_document_rgba(0, 600).is_err());
        assert!(render_document_rgba(800, 0).is_err());
    }

    #[test]
    fn renders_the_live_dom_at_the_requested_size() {
        crate::dom::reset_document();
        let box_node = crate::dom::create_element("div");
        crate::dom::set_style_property(box_node, "width", "20px");
        crate::dom::set_style_property(box_node, "height", "10px");
        crate::dom::set_style_property(box_node, "background", "#00ff00");
        crate::dom::append_child(crate::dom::body_id(), box_node);

        let frame = render_document_rgba(64, 32).expect("headless frame");
        assert_eq!((frame.width, frame.height), (64, 32));
        assert_eq!(frame.rgba.len(), 64 * 32 * 4);
        assert!(
            frame
                .rgba
                .chunks_exact(4)
                .any(|pixel| pixel != [255, 255, 255, 255]),
            "the document box must change at least one white background pixel"
        );
    }

    #[test]
    fn block_in_inline_collapsible_whitespace_matches_the_direct_block() {
        crate::dom::reset_document();
        let span = crate::dom::create_element("span");
        crate::dom::set_style_property(span, "opacity", "0.5");
        crate::dom::append_child(span, crate::dom::create_text_node("\n  "));
        let nested = crate::dom::create_element("div");
        crate::dom::set_style_property(nested, "width", "100px");
        crate::dom::set_style_property(nested, "height", "100px");
        crate::dom::set_style_property(nested, "background", "green");
        crate::dom::append_child(span, nested);
        crate::dom::append_child(span, crate::dom::create_text_node("\n"));
        crate::dom::append_child(crate::dom::body_id(), span);
        let actual = render_document_rgba(160, 120).expect("render block in inline");

        crate::dom::reset_document();
        let direct = crate::dom::create_element("div");
        crate::dom::set_style_property(direct, "width", "100px");
        crate::dom::set_style_property(direct, "height", "100px");
        crate::dom::set_style_property(direct, "background", "green");
        crate::dom::set_style_property(direct, "opacity", "0.5");
        crate::dom::append_child(crate::dom::body_id(), direct);
        let expected = render_document_rgba(160, 120).expect("render direct block");

        assert_eq!(actual, expected);
    }

    #[test]
    fn window_scroll_to_changes_the_captured_viewport() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::jsdom::set_viewport(64.0, 64.0);
        let body = crate::dom::body_id();
        crate::dom::set_style_property(body, "margin", "0");
        crate::dom::set_style_property(body, "height", "200px");
        let child = crate::dom::create_element("div");
        for (name, value) in [
            ("width", "20px"),
            ("height", "20px"),
            ("position", "absolute"),
            ("top", "20px"),
            ("left", "0"),
            ("background", "green"),
        ] {
            crate::dom::set_style_property(child, name, value);
        }
        crate::dom::append_child(body, child);
        let before = render_document_rgba(64, 64).unwrap();
        let window = crate::jsdom::window_value();
        window.call_method(
            "scrollTo",
            vec![
                w3cos_core::Value::Number(0.0),
                w3cos_core::Value::Number(20.0),
            ],
        );
        assert_eq!(window.get_property("scrollY").to_number(), 20.0);
        let after = render_document_rgba(64, 64).unwrap();
        let pixel = (10 * 64 + 10) * 4;
        assert_eq!(&before.rgba[pixel..pixel + 4], &[255, 255, 255, 255]);
        assert_eq!(
            &after.rgba[pixel..pixel + 4],
            &[0, 128, 0, 255],
            "window scrolling must move document content into the captured viewport"
        );
        crate::jsdom::reset_bridge();
    }

    #[test]
    fn window_scroll_keeps_fixed_descendants_in_the_viewport() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::jsdom::set_viewport(64.0, 64.0);
        let body = crate::dom::body_id();
        crate::dom::set_style_property(body, "margin", "0");
        crate::dom::set_style_property(body, "height", "200px");
        let fixed = crate::dom::create_element("div");
        for (name, value) in [
            ("position", "fixed"),
            ("top", "10px"),
            ("left", "10px"),
            ("width", "20px"),
            ("height", "20px"),
            ("background", "red"),
        ] {
            crate::dom::set_style_property(fixed, name, value);
        }
        let child = crate::dom::create_element("div");
        for (name, value) in [
            ("width", "10px"),
            ("height", "10px"),
            ("background", "green"),
        ] {
            crate::dom::set_style_property(child, name, value);
        }
        crate::dom::append_child(fixed, child);
        crate::dom::append_child(body, fixed);
        let before = render_document_rgba(64, 64).unwrap();
        crate::jsdom::window_value().call_method(
            "scrollTo",
            vec![
                w3cos_core::Value::Number(0.0),
                w3cos_core::Value::Number(20.0),
            ],
        );
        let after = render_document_rgba(64, 64).unwrap();
        crate::jsdom::reset_bridge();
        assert_eq!(
            before.rgba, after.rgba,
            "fixed subtree must not scroll with the document"
        );
        let pixel = (15 * 64 + 15) * 4;
        assert_eq!(&after.rgba[pixel..pixel + 4], &[0, 128, 0, 255]);
    }

    #[test]
    fn window_scroll_moves_nested_overflow_clips_with_the_document() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::jsdom::set_viewport(64.0, 64.0);
        let body = crate::dom::body_id();
        crate::dom::set_style_property(body, "margin", "0");
        crate::dom::set_style_property(body, "height", "200px");
        let container = crate::dom::create_element("div");
        for (name, value) in [
            ("position", "absolute"),
            ("top", "30px"),
            ("left", "0"),
            ("width", "20px"),
            ("height", "20px"),
            ("overflow", "hidden"),
        ] {
            crate::dom::set_style_property(container, name, value);
        }
        let child = crate::dom::create_element("div");
        for (name, value) in [
            ("width", "20px"),
            ("height", "40px"),
            ("background", "green"),
        ] {
            crate::dom::set_style_property(child, name, value);
        }
        crate::dom::append_child(container, child);
        crate::dom::append_child(body, container);
        crate::jsdom::window_value().call_method(
            "scrollTo",
            vec![
                w3cos_core::Value::Number(0.0),
                w3cos_core::Value::Number(20.0),
            ],
        );
        let frame = render_document_rgba(64, 64).unwrap();
        crate::jsdom::reset_bridge();
        for (y, expected) in [
            (5, [255, 255, 255, 255]),
            (15, [0, 128, 0, 255]),
            (25, [0, 128, 0, 255]),
            (35, [255, 255, 255, 255]),
        ] {
            let pixel = (y * 64 + 10) * 4;
            assert_eq!(&frame.rgba[pixel..pixel + 4], &expected, "clip at y={y}");
        }
    }

    #[test]
    fn hidden_overflow_honors_programmatic_scroll_offset() {
        crate::dom::reset_document();
        let container = crate::dom::create_element("div");
        for (name, value) in [
            ("overflow", "hidden"),
            ("width", "20px"),
            ("height", "20px"),
            ("background", "red"),
        ] {
            crate::dom::set_style_property(container, name, value);
        }
        let child = crate::dom::create_element("div");
        for (name, value) in [
            ("width", "20px"),
            ("height", "20px"),
            ("margin-top", "20px"),
            ("background", "green"),
        ] {
            crate::dom::set_style_property(child, name, value);
        }
        crate::dom::append_child(container, child);
        crate::dom::append_child(crate::dom::body_id(), container);
        let before = render_document_rgba(64, 64).unwrap();
        crate::dom::set_scroll_offset(container, None, Some(20.0));
        let after = render_document_rgba(64, 64).unwrap();
        let pixel = (16 * 64 + 16) * 4;
        assert!(
            before.rgba[pixel] > before.rgba[pixel + 1],
            "unscrolled clip shows its red background"
        );
        assert!(
            after.rgba[pixel + 1] > after.rgba[pixel],
            "scrolling the hidden overflow container reveals the green child: before={:?}, after={:?}",
            &before.rgba[pixel..pixel + 4],
            &after.rgba[pixel..pixel + 4]
        );
    }
}
