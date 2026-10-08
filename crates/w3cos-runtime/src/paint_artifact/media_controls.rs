use super::*;

pub(super) fn append(nodes: &mut Vec<PaintNode>, rects: &mut Vec<Option<LayoutRect>>) {
    let hosts = nodes.len();
    for host in 0..hosts {
        let node = &nodes[host];
        if node.style.custom_properties.as_ref().and_then(|p|
            p.get("--w3cos-internal-video-controls")).map(String::as_str) != Some("no-source") {
            continue;
        }
        let Some(rect) = rects[host] else { continue; };
        // UA controls form a positioned paint-only subtree. Their box comes
        // from the replaced host, never from an author containing block.
        let style = Style {
            display: Display::Block, position: Position::Relative,
            width: w3cos_std::style::Dimension::Px(rect.width),
            height: w3cos_std::style::Dimension::Px(rect.height),
            visibility: node.style.visibility,
            background: Color::rgb(51, 51, 51),
            custom_properties: Some(std::collections::HashMap::from([
                ("--w3cos-internal-media-controls-layer".into(), "no-source".into()),
                ("--w3cos-internal-z-index-specified".into(), "1".into()),
            ])),
            ..Style::default()
        };
        nodes.push(PaintNode { kind: ComponentKind::Box, style, parent: Some(host), sticky_counter_signal: None });
        rects.push(Some(rect));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_controls_are_late_host_owned_paint_nodes() {
        let host_rect = LayoutRect { x: 12.0, y: 17.0, width: 87.0, height: 91.0 };
        let nodes = vec![
            PaintNode { kind: ComponentKind::Root, style: Style::default(), parent: None, sticky_counter_signal: None },
            PaintNode { kind: ComponentKind::Box, style: Style {
                display: Display::InlineBlock, opacity: 0.5, overflow: Overflow::Hidden,
                custom_properties: Some(std::collections::HashMap::from([
                    ("--w3cos-internal-video-controls".into(), "no-source".into()),
                ])), ..Style::default()
            }, parent: Some(0), sticky_counter_signal: None },
            PaintNode { kind: ComponentKind::Box, style: Style { display: Display::InlineBlock, position: Position::Relative, z_index: 1, ..Style::default() }, parent: Some(0), sticky_counter_signal: None },
        ];
        let artifact = PaintArtifact::build(nodes, &[(host_rect, 0), (host_rect, 1), (host_rect, 2)], 1);
        assert_eq!(artifact.nodes.len(), 4, "controls need their own paint phase");
        let controls = 3;
        assert_eq!(artifact.nodes[controls].parent, Some(1));
        assert_eq!(artifact.rect_by_index[controls], Some(host_rect));
        assert_eq!(artifact.node_properties[controls].effect, artifact.node_properties[1].effect, "host opacity is applied once");
        assert_eq!(artifact.self_clip[controls], artifact.node_properties[1].clip);
        // The host's opacity establishes a stacking context, so its controls
        // cannot escape above later siblings outside that context.
        assert!(artifact.paint_order_key(controls) < artifact.paint_order_key(2));

        let mut plain = artifact.nodes[..3].to_vec();
        plain[1].style.opacity = 1.0;
        plain[2].style.position = Position::Static;
        plain[2].style.z_index = 0;
        let artifact = PaintArtifact::build(plain, &[(host_rect, 0), (host_rect, 1), (host_rect, 2)], 2);
        assert!(artifact.paint_order_key(2) < artifact.paint_order_key(3), "normal sibling paints before positioned UA controls");
        let rebuilt = PaintArtifact::build(artifact.nodes.clone(), &[(host_rect, 0), (host_rect, 1), (host_rect, 2)], 3);
        assert_eq!(rebuilt.nodes.len(), 4, "reused snapshots must not duplicate UA paint children");
    }
}
