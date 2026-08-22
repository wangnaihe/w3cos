//! Deterministic offscreen rendering for conformance and embedding tools.

use anyhow::{Result, bail};
use std::collections::HashMap;

use crate::layout::{self, LayoutRect};
use crate::paint_artifact::{PaintArtifact, PaintNode};
use crate::render_skia::SkiaRasterizer;

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
    if width == 0 || height == 0 {
        bail!("headless viewport dimensions must be positive");
    }

    let root = crate::dom::to_component_tree();
    let flat = layout::pre_flatten(&root);
    let layout_cache = layout::compute(&root, width as f32, height as f32)?;
    let artifact = PaintArtifact::build(
        flat.iter().map(|node| PaintNode {
            kind: node.kind.clone(),
            style: node.style.clone(),
            parent: node.parent,
            sticky_counter_signal: node.sticky_counter_signal,
        }),
        &layout_cache,
        1,
    );

    let mut nodes = layout_cache
        .iter()
        .filter_map(|&(rect, index)| {
            let node = flat.get(index)?;
            Some((index, rect, node.kind, node.style))
        })
        .collect::<Vec<(usize, LayoutRect, _, _)>>();
    nodes.sort_by_key(|(index, _, _, _)| artifact.z_order[*index]);

    let mut rasterizer = SkiaRasterizer::new(include_bytes!("../assets/Inter-Regular.ttf"))
        .ok_or_else(|| anyhow::anyhow!("bundled W3COS font is unavailable to Skia"))?;
    let scroll_info = vec![None; flat.len()];
    let rgba = rasterizer
        .render_frame(
            width,
            height,
            &nodes,
            layout::layout_font(),
            &scroll_info,
            &HashMap::new(),
            None,
            w3cos_std::color::Color::WHITE,
            Some(&artifact),
            None,
            1.0,
        )
        .ok_or_else(|| anyhow::anyhow!("Skia failed to rasterize the headless document"))?
        .to_vec();

    Ok(HeadlessFrame {
        width,
        height,
        rgba,
    })
}

#[cfg(test)]
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
}
