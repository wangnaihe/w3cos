//! Deterministic offscreen rendering for conformance and embedding tools.

use anyhow::{Result, bail};
use std::cell::{Cell, RefCell};
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
    let mut index = 0usize;
    while index < rgba.len() {
        // The weight is `(index % 251) + 1` and wraps to 1 after 251.
        // Each run stays inside one cycle so the inner loop does not branch.
        let factor0 = (index % 251) + 1;
        let run = (252 - factor0).min(rgba.len() - index);
        let slice = &rgba[index..index + run];
        let mut factor = factor0 as u64;
        let mut offset = 0usize;
        while offset + 4 <= slice.len() {
            let factor1 = factor + 1;
            let factor2 = factor + 2;
            let factor3 = factor + 3;
            checksum = checksum
                .wrapping_add(u64::from(slice[offset]).wrapping_mul(factor))
                .wrapping_add(u64::from(slice[offset + 1]).wrapping_mul(factor1))
                .wrapping_add(u64::from(slice[offset + 2]).wrapping_mul(factor2))
                .wrapping_add(u64::from(slice[offset + 3]).wrapping_mul(factor3));
            factor += 4;
            offset += 4;
        }
        while offset < slice.len() {
            checksum = checksum.wrapping_add(u64::from(slice[offset]).wrapping_mul(factor));
            factor += 1;
            offset += 1;
        }
        index += run;
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

struct RetainedPaint {
    structure: u64,
    /// Paint styles do not depend on the layout rects, so an inset or scroll
    /// update can reuse the artifact.
    rect_independent: bool,
    artifact: PaintArtifact,
}

thread_local! {
    static CHECKSUM_ONLY: Cell<bool> = const { Cell::new(false) };
    static LAST_FRAME_CHECKSUM: Cell<u64> = const { Cell::new(0) };
    static RETAINED_PAINT: RefCell<Option<RetainedPaint>> = const { RefCell::new(None) };
}

#[cfg(feature = "cpu-render")]
thread_local! {
    static CPU_PIXMAP: RefCell<Option<tiny_skia::Pixmap>> = const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    static RETAINED_REUSES: Cell<u32> = const { Cell::new(0) };
}

pub(crate) fn invalidate_retained_frame() {
    RETAINED_PAINT.with(|slot| *slot.borrow_mut() = None);
}

#[cfg(test)]
pub(crate) fn retained_paint_reuses() -> u32 {
    RETAINED_REUSES.with(Cell::get)
}

#[cfg(test)]
fn note_retained_reuse() {
    RETAINED_REUSES.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(test))]
fn note_retained_reuse() {}

/// Render the current live DOM through the same component, layout and Skia
/// stages used by native windows, without creating an operating-system window.
pub fn render_document_rgba(width: u32, height: u32) -> Result<HeadlessFrame> {
    render_document_rgba_inner(width, height, false)
}

/// Rasterize and checksum the pixmap in place.
///
/// The CPU renderer keeps one viewport buffer and does not copy it into a
/// second `Vec` when the caller only needs the checksum.
pub(crate) fn render_document_checksum(width: u32, height: u32) -> Result<u64> {
    CHECKSUM_ONLY.with(|cell| cell.set(true));
    let rendered = render_document_rgba(width, height);
    CHECKSUM_ONLY.with(|cell| cell.set(false));
    let frame = rendered?;
    if frame.rgba.is_empty() {
        Ok(LAST_FRAME_CHECKSUM.with(Cell::get))
    } else {
        Ok(frame_byte_checksum(&frame.rgba))
    }
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
    if crate::webgl_draw::has_live_contexts() {
        let flat = layout::pre_flatten(root);
        crate::webgl_draw::publish_drawing_buffers(flat.iter().enumerate().filter_map(
            |(index, node)| match &node.on_click {
                w3cos_std::EventAction::NativeHost { id, .. } => {
                    u32::try_from(*id).ok().map(|id| (index, id))
                }
                _ => None,
            },
        ));
    }
    let structure = crate::dom::with_document(|document| document.structure_generation());
    let reuse = crate::incremental_layout::take_paint_reuse();
    if reuse != crate::incremental_layout::PaintReuse::None
        && layout_results.text_fragments.is_empty()
        && let Some(frame) = reuse_retained_paint(
            structure,
            reuse,
            width,
            height,
            force_cpu,
            root,
            layout_results,
        )?
    {
        return Ok(frame);
    }
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
    artifact.set_text_fragments(&layout_results.text_fragments);
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
    let rect_independent = component_rect_independent(root);
    RETAINED_PAINT.with(|slot| {
        *slot.borrow_mut() = Some(RetainedPaint {
            structure,
            rect_independent,
            artifact,
        });
    });

    Ok(HeadlessFrame {
        width,
        height,
        rgba,
    })
}

fn reuse_retained_paint(
    structure: u64,
    reuse: crate::incremental_layout::PaintReuse,
    width: u32,
    height: u32,
    force_cpu: bool,
    root: &w3cos_std::Component,
    layout_results: &layout::LayoutResults,
) -> Result<Option<HeadlessFrame>> {
    RETAINED_PAINT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(retained) = slot.as_mut() else {
            return Ok(None);
        };
        if retained.structure != structure || !retained.artifact.text_fragments.is_empty() {
            return Ok(None);
        }
        retained.artifact.viewport_scroll = crate::jsdom::window_scroll_offset();
        if reuse == crate::incremental_layout::PaintReuse::Insets {
            if !retained.rect_independent {
                return Ok(None);
            }
            if !assign_retained_rects(
                &mut retained.artifact.rect_by_index,
                &layout_results.layout_cache,
            ) {
                return Ok(None);
            }
        }
        let flat = layout::pre_flatten(root);
        let frame = paint_retained_artifact(
            width,
            height,
            force_cpu,
            &flat,
            layout_results,
            &retained.artifact,
        )?;
        note_retained_reuse();
        Ok(Some(frame))
    })
}

fn assign_retained_rects(
    rects: &mut [Option<LayoutRect>],
    layouts: &[(LayoutRect, usize)],
) -> bool {
    for &(rect, index) in layouts {
        let Some(slot) = rects.get_mut(index) else {
            return false;
        };
        *slot = Some(rect);
    }
    true
}

fn component_rect_independent(component: &w3cos_std::Component) -> bool {
    fn style_ok(style: &w3cos_std::style::Style) -> bool {
        use w3cos_std::style::{Dimension, Display, Float, Position, Spacing};
        let padding_percent = [
            style.padding.top,
            style.padding.right,
            style.padding.bottom,
            style.padding.left,
        ]
        .into_iter()
        .any(|edge| matches!(edge, Spacing::Percent(_)));
        let background_image = style.background_image.as_deref().is_some_and(|value| {
            value.split(',').any(|layer| {
                let layer = layer.trim();
                !layer.is_empty() && !layer.eq_ignore_ascii_case("none")
            })
        });
        style.float == Float::None
            && !matches!(
                style.display,
                Display::Table
                    | Display::InlineTable
                    | Display::TableRow
                    | Display::TableRowGroup
                    | Display::TableHeaderGroup
                    | Display::TableFooterGroup
                    | Display::TableColumn
                    | Display::TableColumnGroup
                    | Display::TableCell
                    | Display::TableCaption
            )
            && style.column_count.is_none()
            && matches!(style.column_width, Dimension::Auto)
            && style.position != Position::Sticky
            && style.filter.is_none()
            && style.transform.is_identity()
            && style.backdrop_blur.is_none()
            && style.clip.is_none()
            && !background_image
            && !padding_percent
    }
    fn walk(component: &w3cos_std::Component) -> bool {
        style_ok(&component.style) && component.children.iter().all(walk)
    }
    walk(component)
}

fn paint_retained_artifact(
    width: u32,
    height: u32,
    force_cpu: bool,
    flat: &[layout::FlatNodeInfo<'_>],
    layout_results: &layout::LayoutResults,
    artifact: &PaintArtifact,
) -> Result<HeadlessFrame> {
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
    let table_replay = crate::table_paint::replay(&nodes, artifact);
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
    let bidi_replay = crate::bidi_paint::replay(&nodes, artifact);
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
        &layout_results.layout_cache,
        flat,
        width as f32,
        height as f32,
    );
    let rgba = rasterize_prepared_frame(width, height, &nodes, &scroll_info, artifact, force_cpu)?;
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

#[cfg(feature = "cpu-render")]
fn cpu_pixmap_bytes(
    width: u32,
    height: u32,
    nodes: &[(
        usize,
        LayoutRect,
        &w3cos_std::component::ComponentKind,
        &w3cos_std::style::Style,
    )],
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
) -> Result<Vec<u8>> {
    CPU_PIXMAP.with(|slot| -> Result<()> {
        let mut slot = slot.borrow_mut();
        let same_size = slot
            .as_ref()
            .is_some_and(|pixmap| pixmap.width() == width && pixmap.height() == height);
        if !same_size {
            *slot =
                Some(tiny_skia::Pixmap::new(width, height).ok_or_else(|| {
                    anyhow::anyhow!("viewport is too large to rasterize on the CPU")
                })?);
        }
        let pixmap = slot.as_mut().expect("cpu pixmap");
        let mut clips = crate::render_cpu::ClipMaskCache::default();
        crate::render_cpu::render_frame(
            pixmap,
            nodes,
            layout::layout_font(),
            scroll_info,
            &HashMap::new(),
            None,
            &mut clips,
        );
        LAST_FRAME_CHECKSUM.with(|cell| cell.set(frame_byte_checksum(pixmap.data())));
        Ok(())
    })?;
    if CHECKSUM_ONLY.with(Cell::get) {
        return Ok(Vec::new());
    }
    CPU_PIXMAP.with(|slot| Ok(slot.borrow().as_ref().expect("cpu pixmap").data().to_vec()))
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
            return cpu_pixmap_bytes(width, height, nodes, scroll_info);
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
        return cpu_pixmap_bytes(width, height, nodes, scroll_info);
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
    fn absolute_top_reuses_the_paint_artifact_and_matches_a_fresh_frame() {
        crate::dom::reset_document();
        let layer = crate::dom::create_element("div");
        crate::dom::set_style_property(layer, "position", "relative");
        crate::dom::set_style_property(layer, "width", "80px");
        crate::dom::set_style_property(layer, "height", "80px");
        let box_node = crate::dom::create_element("div");
        for (name, value) in [
            ("position", "absolute"),
            ("width", "16px"),
            ("height", "16px"),
            ("left", "4px"),
            ("top", "0px"),
            ("background", "#0000ff"),
        ] {
            crate::dom::set_style_property(box_node, name, value);
        }
        crate::dom::append_child(layer, box_node);
        crate::dom::append_child(crate::dom::body_id(), layer);
        let first = render_document_rgba(64, 64).expect("first frame");
        let reuses = retained_paint_reuses();
        crate::dom::set_style_property(box_node, "top", "24px");
        let shifted = render_document_rgba(64, 64).expect("shifted frame");
        assert_eq!(
            retained_paint_reuses(),
            reuses + 1,
            "an absolute inset frame reuses the paint artifact"
        );
        assert_ne!(
            first.rgba, shifted.rgba,
            "moving the box must change pixels before the artifact is reused"
        );
        crate::incremental_layout::invalidate();
        let fresh = render_document_rgba(64, 64).expect("fresh frame");
        assert_eq!(
            shifted.rgba, fresh.rgba,
            "reused artifact pixels must match a rebuilt frame"
        );
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
