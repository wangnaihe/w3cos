//! Shared glyph positions for compatible word fragments on one resolved line.
use crate::{layout::LayoutRect, paint_artifact::PaintArtifact, render_skia, skia_text_run};
use skia_safe::{Canvas, GlyphId, Point, Typeface};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use w3cos_std::style::{Display, TextDirection};
use w3cos_std::{ComponentKind, Style};

#[derive(Clone)]
struct GlyphSlice {
    origin_x: f32,
    face: Typeface,
    glyphs: Vec<GlyphId>,
    positions: Vec<Point>,
    glyph_font_sizes: Vec<f32>,
}

struct SourceDecoration {
    x: f32,
    width: f32,
    runs: Vec<GlyphSlice>,
}

pub(crate) struct Fragment {
    source_x: f32,
    ascent: f32,
    runs: Vec<GlyphSlice>,
    decoration: Option<(Arc<SourceDecoration>, bool, bool)>,
}

/// Recover authored text nodes from their generated word/space pieces. Only
/// whitespace-delimited, cluster-safe boundaries may be shaped independently.
pub(crate) fn authored_word_groups(
    texts: &[&str], style: &Style,
) -> Option<Vec<(std::ops::Range<usize>, Vec<f32>)>> {
    let text = texts.concat();
    let ends = w3cos_std::inline_text::fragment_ends(&text, style)?;
    render_skia::measure_skia_inline_fragment_advances(texts, style)?;
    let mut boundaries = vec![0];
    for part in texts { boundaries.push(boundaries.last()? + part.len()); }
    let mut start = 0;
    let mut groups = Vec::with_capacity(ends.len());
    for end in ends {
        if end < text.len()
            && !text[..end].chars().last().is_some_and(char::is_whitespace)
            && !text[end..].chars().next().is_some_and(char::is_whitespace)
        { return None; }
        let stop = boundaries.binary_search(&end).ok()?;
        if stop <= start { return None; }
        let mut advances = render_skia::measure_skia_inline_fragment_advances(
            &texts[start..stop], style,
        )?;
        let advance: f32 = advances.iter().sum();
        *advances.last_mut()? += crate::text_layout::inline_layout_advance(advance) - advance;
        groups.push((start..stop, advances));
        start = stop;
    }
    Some(groups)
}

impl Fragment {
    pub(crate) fn decoration_span(&self, rect: LayoutRect,
        kind: w3cos_std::style::TextDecoration) -> (f32, f32) {
        self.decoration.as_ref().map_or((rect.x, rect.width), |(source, first, last)| {
            let leader = if kind == w3cos_std::style::TextDecoration::LineThrough { *last } else { *first };
            (rect.x - self.source_x + source.x, if leader { source.width } else { 0.0 })
        })
    }

    pub(crate) fn baseline(&self, rect: LayoutRect) -> f32 {
        rect.y + self.ascent
    }

    pub(crate) fn ink_intercepts(&self, rect: LayoutRect, style: &Style, upper: f32, lower: f32) -> Vec<f32> {
        let mut intervals = Vec::new();
        let runs = self.decoration.as_ref().map_or(&self.runs, |(source, _, _)| &source.runs);
        for run in runs {
            let origin = rect.x - self.source_x + run.origin_x;
            let font = skia_text_run::css_font_for_style(&run.face, style.font_size, style);
            skia_text_run::visit_glyph_fonts(&font, &run.glyph_font_sizes, run.glyphs.len(), |font, range|
                intervals.extend(font.get_intercepts(&run.glyphs[range.clone()], &run.positions[range],
                    (upper, lower), None).into_iter().map(|position| origin + position)));
        }
        intervals
    }

    pub(crate) fn paint(&self, canvas: &Canvas, rect: LayoutRect, style: &Style) {
        let paint = render_skia::color_paint(style.color, style.opacity);
        static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let trace = *TRACE.get_or_init(|| std::env::var_os("W3COS_DUMP_HEADLESS_LAYOUT").is_some());
        for run in &self.runs {
            let font = skia_text_run::css_font_for_style(&run.face, style.font_size, style);
            let x = rect.x - self.source_x + run.origin_x;
            let baseline = rect.y + self.ascent;
            let visible = render_skia::glyph_slice_intersects_clip(
                canvas, &font, &run.glyphs, &run.positions, &run.glyph_font_sizes, x, baseline,
            );
            if trace {
                eprintln!("W3COS_HEADLESS_GLYPH rect={rect:?} source_x={} origin={} baseline={baseline} face={:?} glyphs={} first={:?} visible={visible} clip={:?}",
                    self.source_x, x, run.face.family_name(), run.glyphs.len(), run.positions.first(), canvas.local_clip_bounds());
            }
            if !visible { continue; }
            skia_text_run::visit_glyph_fonts(&font, &run.glyph_font_sizes, run.glyphs.len(), |font, range|
                canvas.draw_glyphs_at(&run.glyphs[range.clone()], &run.positions[range], (x, baseline), font, &paint));
        }
    }
}

pub(crate) fn build(
    artifact: &PaintArtifact,
    clients: impl Iterator<Item = usize>,
    face: &Typeface,
) -> HashMap<usize, Fragment> {
    let clients = clients.collect::<HashSet<_>>();
    // No-break quote words constrain line fitting, not the shaping extent
    // of the adjacent authored text. Recover the transparent word's inline
    // owner so its text can share glyph slices with later spaces/words.
    // Keep quote operators themselves as distinct source/style barriers.
    let quote_words = artifact.nodes.iter().filter_map(|node| {
        if !node.style.custom_properties.as_ref().is_some_and(|p|
            p.contains_key("--w3cos-internal-generated-quote")) { return None; }
        let parent = node.parent?;
        artifact.nodes[parent].style.custom_properties.as_ref().is_some_and(|p|
            p.contains_key("--w3cos-internal-unbroken-inline-word")).then_some(parent)
    }).collect::<HashSet<_>>();
    let inline_owner = |mut parent: usize| -> Option<usize> {
        while quote_words.contains(&parent) { parent = artifact.nodes[parent].parent?; }
        Some(parent)
    };
    let same_source_style = |first: usize, current: usize| {
        let a = &artifact.nodes[first];
        let b = &artifact.nodes[current];
        if a.style == b.style { return true; }
        if ![a.parent, b.parent].into_iter().flatten().any(|parent|
            quote_words.contains(&parent)) { return false; }
        // The no-break wrapper assigns flex-shrink:0 to its children for
        // fitting only. This must not become an authored shaping boundary.
        let mut a = a.style.clone();
        let mut b = b.style.clone();
        a.flex_shrink = 1.0;
        b.flex_shrink = 1.0;
        a == b
    };
    let parents = clients.iter()
        .filter_map(|index| inline_owner(artifact.nodes.get(*index)?.parent?))
        .collect::<HashSet<_>>();
    let mut siblings: HashMap<usize, Vec<usize>> = HashMap::new();
    for (index, node) in artifact.nodes.iter().enumerate() {
        if quote_words.contains(&index) { continue; }
        if let Some(parent) = node.parent.and_then(inline_owner)
            .filter(|parent| parents.contains(parent)) {
            siblings.entry(parent).or_default().push(index);
        }
    }
    let mut result = HashMap::new();
    for (parent, children) in siblings {
        let style = &artifact.nodes[parent].style;
        if !matches!(style.display, Display::Block | Display::Flex | Display::Inline)
            || style.direction != TextDirection::Ltr
            || !style.custom_properties.as_ref().is_some_and(|properties| {
                properties.contains_key("--w3cos-internal-inline-formatting-context")
            })
        {
            continue;
        }
        let eligible = |index: usize| {
            artifact.rect_by_index[index].is_some_and(|rect| {
                rect.width > 0.0
                    && {
                        let node = &artifact.nodes[index];
                        let mut style = node.style.clone();
                        if style.custom_properties.as_ref().is_some_and(|properties|
                            properties.contains_key(w3cos_std::inline_text::SOURCE_RUN)) {
                            // Decoration is paint-only for generated pieces of
                            // one source. Do not relax the layout classifier.
                            style.text_decoration = w3cos_std::style::TextDecoration::None;
                        }
                        crate::layout::is_plain_inline_word_fragment(&node.kind, &style)
                    }
            })
        };
        let mut start = 0;
        while start < children.len() {
            let first = children[start];
            if !eligible(first) {
                start += 1;
                continue;
            }
            let mut end = start + 1;
            while end < children.len() {
                let previous = artifact.rect_by_index[children[end - 1]].unwrap();
                let current = children[end];
                if !eligible(current)
                    || !same_source_style(first, current)
                {
                    break;
                }
                let rect = artifact.rect_by_index[current].unwrap();
                if (rect.y - previous.y).abs() > 0.01
                    || (rect.x - previous.x - previous.width).abs() > 0.01
                {
                    break;
                }
                end += 1;
            }
            if let Some(mut fragments) = shape_group(&children[start..end], artifact, face) {
                share_source_decoration(&mut fragments, artifact, &clients);
                result.extend(fragments);
            }
            start = end;
        }
    }
    result
}

fn share_source_decoration(fragments: &mut [(usize, Fragment)], artifact: &PaintArtifact,
    clients: &HashSet<usize>) {
    let Some((first_index, _)) = fragments.first() else { return; };
    let style = &artifact.nodes[*first_index].style;
    if !style.custom_properties.as_ref().is_some_and(|properties|
        properties.contains_key(w3cos_std::inline_text::SOURCE_RUN))
        || (style.text_decoration == w3cos_std::style::TextDecoration::None
            && artifact.ancestor_text_decorations(*first_index).is_empty()) { return; }
    let Some(first) = fragments.iter().position(|(index, _)| clients.contains(index)) else { return; };
    let last = fragments.iter().rposition(|(index, _)| clients.contains(index)).unwrap();
    let start = artifact.rect_by_index[*first_index].unwrap();
    let end = artifact.rect_by_index[fragments.last().unwrap().0].unwrap();
    let source = Arc::new(SourceDecoration { x: start.x, width: end.x + end.width - start.x,
        runs: fragments.iter().flat_map(|(_, fragment)| fragment.runs.iter().cloned()).collect() });
    for (position, (_, fragment)) in fragments.iter_mut().enumerate() {
        fragment.decoration = Some((source.clone(), position == first, position == last));
    }
}

fn shape_group(
    group: &[usize],
    artifact: &PaintArtifact,
    face: &Typeface,
) -> Option<Vec<(usize, Fragment)>> {
    let style = &artifact.nodes[*group.first()?].style;
    let texts = group
        .iter()
        .map(|index| {
            let ComponentKind::Text { content } = &artifact.nodes[*index].kind else {
                unreachable!()
            };
            content.as_str()
        })
        .collect::<Vec<_>>();
    if texts.len() < 3 || !texts.contains(&" ") {
        return None;
    }
    // The same validation as line fitting rejects cluster cuts, reordering,
    // transforms and the deterministic Ahem/monospace compatibility paths.
    render_skia::measure_skia_inline_fragment_advances(&texts, style)?;
    let text = texts.concat();
    let mut boundaries = vec![0];
    for part in &texts {
        boundaries.push(boundaries.last()? + part.len());
    }
    let ascent = render_skia::text_baseline(0.0, style.font_size, face, style, &text);
    let origin = artifact.rect_by_index[group[0]]?.x;
    let mut fragments = group
        .iter()
        .map(|index| {
            Some((
                *index,
                Fragment {
                    source_x: artifact.rect_by_index[*index]?.x,
                    ascent,
                    runs: Vec::new(),
                    decoration: None,
                },
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    let mut byte_offset = 0;
    let mut run_origin = origin;
    let authored_groups = authored_word_groups(&texts, style);
    let mut source_cursor_bases = HashMap::<usize, f64>::new();
    for run in render_skia::css_font_runs(&text, face, style) {
        let shaped = skia_text_run::shape_visual_run_with_clusters(run.text, &run.typeface, style)?;
        let mut cursor = 0.0_f64;
        let cluster_origins = shaped.clusters.iter().map(|(cluster, advance)| {
            let entry = (*cluster, cursor);
            cursor += f64::from(*advance);
            entry
        }).collect::<HashMap<_, _>>();
        let mut slices = group
            .iter()
            .map(|_| GlyphSlice {
                origin_x: run_origin,
                face: run.typeface.clone(),
                glyphs: Vec::new(),
                positions: Vec::new(),
                glyph_font_sizes: Vec::new(),
            })
            .collect::<Vec<_>>();
        for (glyph_index, ((glyph, position), cluster)) in shaped
            .glyphs
            .iter()
            .zip(&shaped.positions)
            .zip(&shaped.glyph_clusters)
            .enumerate()
        {
            let fragment = boundaries
                .partition_point(|boundary| *boundary <= byte_offset + cluster)
                .checked_sub(1)?;
            let slice = slices.get_mut(fragment)?;
            slice.glyphs.push(*glyph);
            if let Some(size) = shaped.glyph_font_sizes.get(glyph_index) {
                slice.glyph_font_sizes.push(*size);
            }
            if let Some((range, _)) = authored_groups.as_ref()
                .and_then(|groups| groups.iter().find(|(range, _)| range.contains(&fragment))) {
                // Like a ShapeResultView, retain the full shaping context but
                // start this authored slice at its used layout origin. Subtract
                // the logical cursor, not the first glyph's offset (marks may
                // have non-zero offsets). Generated words are not new slices.
                let run_delta = f64::from(run_origin) - f64::from(origin);
                let base = *source_cursor_bases.entry(range.start)
                    .or_insert(run_delta + cluster_origins.get(cluster)?);
                slice.origin_x = artifact.rect_by_index[group[range.start]]?.x;
                slice.positions.push(Point::new(
                    (run_delta + f64::from(position.x) - base) as f32, position.y));
            } else {
                // Without validated authored boundaries, keep full-run points
                // intact; rebasing each generated word loses subpixel context.
                slice.positions.push(*position);
            }
        }
        for ((_, fragment), slice) in fragments.iter_mut().zip(slices) {
            if !slice.glyphs.is_empty() {
                fragment.runs.push(slice);
            }
        }
        byte_offset += run.text.len();
        run_origin += shaped.advance;
    }
    Some(fragments)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::paint_artifact::PaintNode;
    use skia_safe::{AlphaType, Color, ColorType, FontMgr, FontStyle, ImageInfo, Surface};

    #[test]
    fn quote_word_wrapper_keeps_the_adjacent_text_source_shaped_together() {
        let face = FontMgr::default().match_family_style("Times", FontStyle::normal()).unwrap();
        let text_style = Style { display: Display::Inline, font_family: Some("serif".into()),
            font_size: 16.0, custom_properties: Some(HashMap::from([
                (w3cos_std::inline_text::SOURCE_RUN.into(), "pseudo:1:before:2".into()),
            ])), ..Style::default() };
        let mut quote_style = text_style.clone();
        quote_style.custom_properties.as_mut().unwrap().insert(
            "--w3cos-internal-generated-quote".into(), "1".into());
        let parent = Style { display: Display::Flex, custom_properties: Some(HashMap::from([
            ("--w3cos-internal-inline-formatting-context".into(), "1".into()),
        ])), ..Style::default() };
        let word = Style { display: Display::InlineFlex, custom_properties: Some(HashMap::from([
            ("--w3cos-internal-unbroken-inline-word".into(), "1".into()),
        ])), ..Style::default() };
        let nodes = vec![
            PaintNode { kind: ComponentKind::Row, style: parent, parent: None, sticky_counter_signal: None },
            PaintNode { kind: ComponentKind::Row, style: word, parent: Some(0), sticky_counter_signal: None },
            PaintNode { kind: ComponentKind::Text { content: "\"".into() }, style: quote_style, parent: Some(1), sticky_counter_signal: None },
            PaintNode { kind: ComponentKind::Text { content: "Before".into() }, style: Style { flex_shrink: 0.0, ..text_style.clone() }, parent: Some(1), sticky_counter_signal: None },
            PaintNode { kind: ComponentKind::Text { content: " ".into() }, style: text_style.clone(), parent: Some(0), sticky_counter_signal: None },
            PaintNode { kind: ComponentKind::Text { content: "block".into() }, style: text_style.clone(), parent: Some(0), sticky_counter_signal: None },
        ];
        let advances = render_skia::measure_skia_inline_fragment_advances(
            &["Before", " ", "block"], &text_style).unwrap();
        let mut layouts = vec![
            (LayoutRect { x: 20.0, y: 20.0, width: 300.0, height: 22.0 }, 0),
            (LayoutRect { x: 20.0, y: 20.0, width: 6.0 + advances[0], height: 22.0 }, 1),
            (LayoutRect { x: 20.0, y: 20.0, width: 6.0, height: 22.0 }, 2),
        ];
        let mut x = 26.0;
        for (index, width) in advances.into_iter().enumerate() {
            layouts.push((LayoutRect { x, y: 20.0, width, height: 22.0 }, index + 3));
            x += width;
        }
        let artifact = PaintArtifact::build(nodes, &layouts, 1);
        let slices = build(&artifact, [2, 3, 4, 5].into_iter(), &face);
        assert!([3, 4, 5].iter().all(|index| slices.contains_key(index)),
            "a no-break quote wrapper must not split the adjacent text's shared glyph slices");
    }

    #[test]
    fn merged_bidi_replay_does_not_reuse_the_first_word_glyph_slice() {
        let style = Style { display: Display::Inline, font_family: Some("serif".into()),
            font_size: 16.0, color: w3cos_std::Color::BLACK,
            custom_properties: Some(HashMap::from([
                ("--w3cos-internal-bidi-logical-order".into(), "0".into())])),
            ..Style::default() };
        let face = FontMgr::default().match_family_style("Times", FontStyle::normal()).unwrap();
        let parent = Style { display: Display::Flex, custom_properties: Some(HashMap::from([
            ("--w3cos-internal-inline-formatting-context".into(), "1".into())])),
            ..Style::default() };
        let mut nodes = vec![PaintNode { kind: ComponentKind::Row, style: parent,
            parent: None, sticky_counter_signal: None }];
        let mut layouts = vec![(LayoutRect { x: 20.0, y: 20.0, width: 360.0, height: 22.0 }, 0)];
        let mut x = 20.0;
        for text in ["This", " ", "is", " ", "text"] {
            let width = skia_text_run::css_font_for_style(&face, style.font_size, &style)
                .measure_str(text, None).0;
            let index = nodes.len();
            nodes.push(PaintNode { kind: ComponentKind::Text { content: text.into() },
                style: style.clone(), parent: Some(0), sticky_counter_signal: None });
            layouts.push((LayoutRect { x, y: 20.0, width, height: 22.0 }, index));
            x += width;
        }
        let artifact = PaintArtifact::build(nodes, &layouts, 1);
        let input = layouts[1..].iter().map(|(rect,index)|
            (*index,*rect,&artifact.nodes[*index].kind,&artifact.nodes[*index].style))
            .collect::<Vec<_>>();
        let replay = crate::bidi_paint::replay(&input, &artifact);
        let merged = replay.iter().find(|node| matches!(node.kind.as_ref(),
            ComponentKind::Text { content } if content == "This is text")).unwrap();
        let actual_nodes = replay.iter().map(|node|
            (node.index,node.rect,node.kind.as_ref(),node.style.as_ref())).collect::<Vec<_>>();
        let expected_rect = LayoutRect { width: 360.0, ..merged.rect };
        let expected_nodes = [(merged.index,expected_rect,merged.kind.as_ref(),merged.style.as_ref())];
        let paint = |nodes: &[(usize,LayoutRect,&ComponentKind,&Style)], artifact| {
            // Independent captures cannot inherit retained recordings from
            // the other side of the comparison under the same client IDs.
            let mut renderer = render_skia::SkiaRasterizer::new_host().unwrap();
            renderer.render_frame(400,80,nodes,crate::layout::layout_font(),
                &[],&HashMap::new(),None,w3cos_std::Color::WHITE,
                artifact,None,1.0).unwrap().to_vec()
        };
        let expected = paint(&expected_nodes, None);
        let actual = paint(&actual_nodes, Some(&artifact));
        let different = actual.chunks_exact(4).zip(expected.chunks_exact(4))
            .filter(|(actual, expected)| actual != expected).count();
        assert_eq!(different, 0, "a merged replay Text must paint all its glyphs, not its original ID's first-word slice");
    }


    #[test]
    fn authored_glyph_view_starts_at_its_used_inline_origin() {
        let face = FontMgr::default().match_family_style("Times", FontStyle::normal()).unwrap();
        let texts = ["This", " ", "line", " ", "is", " ", "all", " ",
            "in", " ", "one", " ", "font", " ", "size", " "];
        let text = texts.concat();
        let mut style = Style { display: Display::Inline, font_family: Some("serif".into()),
            font_size: 19.2, ..Style::default() };
        w3cos_std::inline_text::set_fragment_ends(&mut style, &text, &[17, 28, text.len()]);
        let groups = authored_word_groups(&texts, &style).unwrap();
        let starts = groups.iter().map(|(range, _)| range.start + 1).collect::<Vec<_>>();
        let widths = groups.into_iter().flat_map(|(_, widths)| widths).collect::<Vec<_>>();
        let parent = Style { display: Display::Flex, custom_properties: Some(HashMap::from([
            ("--w3cos-internal-inline-formatting-context".into(), "1".into())])), ..Style::default() };
        let mut nodes = vec![PaintNode { kind: ComponentKind::Row, style: parent, parent: None,
            sticky_counter_signal: None }];
        let mut layouts = vec![(LayoutRect { x: 11.0, y: 20.0, width: 300.0, height: 24.0 }, 0)];
        let mut x = 11.0;
        for (text, width) in texts.iter().zip(widths) {
            nodes.push(PaintNode { kind: ComponentKind::Text { content: (*text).into() },
                style: style.clone(), parent: Some(0), sticky_counter_signal: None });
            layouts.push((LayoutRect { x, y: 20.0, width, height: 24.0 }, nodes.len() - 1));
            x += width;
        }
        let count = nodes.len();
        let artifact = PaintArtifact::build(nodes, &layouts, 1);
        let fragments = build(&artifact, 1..count, &face);
        assert_eq!(artifact.rect_by_index[starts[1]].unwrap().x, 125.125);
        for index in starts {
            let run = &fragments[&index].runs[0];
            assert_eq!(run.origin_x + run.positions[0].x,
                artifact.rect_by_index[index].unwrap().x,
                "an authored glyph view must start at its used inline origin");
        }
    }

    #[test]
    fn lowered_link_words_keep_their_shared_decoration() {
        w3cos_dom::stylesheet::clear_rules();
        let mut document = w3cos_dom::Document::new();
        let p = document.create_element("p");
        let before = document.create_text_node("PREREQUISITE: Operating system needs to have the '");
        let link = document.create_element("a");
        link.set_attribute(&mut document, "href", "support/AHEM_whitespace.ttf");
        let text = document.create_text_node("White Space");
        link.append_child(&mut document, text);
        p.append_child(&mut document, before);
        p.append_child(&mut document, link);
        let after = document.create_text_node("' font installed.");
        p.append_child(&mut document, after);
        document.body().append_child(&mut document, p);
        let root = document.to_component_tree();
        let flat = crate::layout::pre_flatten(&root);
        let nodes = flat.iter().map(|node| PaintNode {
            kind: node.kind.clone(), style: node.style.clone(), parent: node.parent,
            sticky_counter_signal: None,
        }).collect::<Vec<_>>();
        let layouts = crate::layout::compute(&root, 800.0, 600.0).unwrap();
        let artifact = PaintArtifact::build(nodes, &layouts, 1);
        let face = FontMgr::default().match_family_style("Times", FontStyle::normal()).unwrap();
        let fragments = build(&artifact, 0..artifact.nodes.len(), &face);
        let words = artifact.nodes.iter().enumerate().filter(|(_,node)|
            matches!(&node.kind, ComponentKind::Text { content } if content == "White" || content == "Space"))
            .collect::<Vec<_>>();
        assert_eq!(words.len(), 2, "actual DOM must lower to the original split words");
        for (index, node) in words {
            assert!(fragments.get(&index).is_some_and(|fragment| fragment.decoration.is_some()),
                "lowered decorated source needs shared coverage: index={index}, style={:?}, parent={:?}",
                node.style, node.parent.map(|parent| &artifact.nodes[parent].style));
        }
        w3cos_dom::stylesheet::clear_rules();
    }

    #[test]
    fn first_line_background_words_keep_the_authored_glyph_run() {
        w3cos_dom::stylesheet::clear_rules();
        w3cos_dom::stylesheet::register_rule("div::first-line", &[("background", "orange")]);
        let mut document = w3cos_dom::Document::new();
        let block = document.create_element("div");
        for (index, text) in ["First line in 2nd block box.", "Second line.",
            "First line after block-in-inline is not ::first-line.", "Second line."].iter().enumerate() {
            if index > 0 {
                let br = document.create_element("br");
                block.append_child(&mut document, br);
            }
            let text = document.create_text_node(text);
            block.append_child(&mut document, text);
        }
        document.body().append_child(&mut document, block);
        let root = document.to_component_tree();
        let flat = crate::layout::pre_flatten(&root);
        let nodes = flat.iter().map(|node| PaintNode {
            kind: node.kind.clone(), style: node.style.clone(), parent: node.parent,
            sticky_counter_signal: None,
        }).collect::<Vec<_>>();
        let layouts = crate::layout::compute(&root, 800.0, 600.0).unwrap();
        let artifact = PaintArtifact::build(nodes, &layouts, 1);
        let face = FontMgr::default().match_family_style("Times", FontStyle::normal()).unwrap();
        let fragments = build(&artifact, 0..artifact.nodes.len(), &face);
        let (index, node) = artifact.nodes.iter().enumerate().find(|(_, node)|
            matches!(&node.kind, ComponentKind::Text { content } if content == "in")).unwrap();
        assert!(fragments.contains_key(&index),
            "first-line paint must not discard authored shaping: index={index}, style={:?}", node.style);
        assert_eq!(node.style.background_image.as_deref(), Some("none"));
        let mut image_style = node.style.clone();
        image_style.background_image = Some("url(background.png)".into());
        assert!(!crate::layout::is_plain_inline_word_fragment(&node.kind, &image_style),
            "an actual background image remains a fragment barrier");
        w3cos_dom::stylesheet::clear_rules();
    }

    #[test]
    fn internal_float_markers_preserve_authored_glyph_spacing() {
        for spacing in [0.0, 2.0] {
            let plain = Style {
                display: Display::Inline,
                font_family: Some("serif".into()),
                font_size: 16.0,
                word_spacing: spacing,
                ..Style::default()
            };
            let expected = render_skia::measure_skia_text_intrinsic_size("Filler Text", &plain).0;
            for marker in [
                "--w3cos-internal-float-text",
                "--w3cos-internal-float-line-bands",
            ] {
                let mut marked = plain.clone();
                marked.clear = w3cos_std::style::Clear::Left;
                marked.custom_properties = Some(HashMap::from([(marker.into(), "1".into())]));
                let actual =
                    render_skia::measure_skia_text_intrinsic_size("Filler Text", &marked).0;
                assert_eq!(actual, expected, "marker={marker}, word-spacing={spacing}");
            }
        }
    }

    #[test]
    fn shared_glyph_slices_match_the_whole_fallback_font_run() {
        let face = FontMgr::default()
            .match_family_style("Times", FontStyle::normal())
            .unwrap();
        for kerning in [true, false] {
            for origin in [8.0, 400.3] {
                for background in [
                    w3cos_std::Color::TRANSPARENT,
                    w3cos_std::Color::rgb(0, 0, 255),
                ] {
                    for parent_display in [Display::Flex, Display::Inline] {
                    let style = Style {
                        display: Display::Inline,
                        font_family: Some("serif".into()),
                        font_size: 16.0,
                        line_height: 1.125,
                        font_kerning: kerning,
                        color: w3cos_std::Color::BLACK,
                        background,
                        ..Style::default()
                    };
                    let parent = Style {
                        display: parent_display,
                        custom_properties: Some(HashMap::from([(
                            "--w3cos-internal-inline-formatting-context".into(),
                            "1".into(),
                        )])),
                        ..Style::default()
                    };
                    let lines = [["⇦", " ", "There"], ["There", " ", "should"]];
                    let mut nodes = vec![PaintNode {
                        kind: ComponentKind::Row,
                        style: parent,
                        parent: None,
                        sticky_counter_signal: None,
                    }];
                    let mut layouts = vec![(
                        LayoutRect {
                            x: origin,
                            y: 8.0,
                            width: 112.0,
                            height: 36.0,
                        },
                        0,
                    )];
                    for (row, texts) in lines.iter().enumerate() {
                        let mut x = origin;
                        let widths =
                            render_skia::measure_skia_inline_fragment_advances(texts, &style)
                                .unwrap();
                        for (text, width) in texts.iter().zip(widths) {
                            nodes.push(PaintNode {
                                kind: ComponentKind::Text {
                                    content: (*text).into(),
                                },
                                style: style.clone(),
                                parent: Some(0),
                                sticky_counter_signal: None,
                            });
                            layouts.push((
                                LayoutRect {
                                    x,
                                    y: 8.0 + row as f32 * 18.0,
                                    width,
                                    height: 18.0,
                                },
                                nodes.len() - 1,
                            ));
                            x += width;
                        }
                    }
                    let count = nodes.len();
                    let artifact = PaintArtifact::build(nodes, &layouts, 1);
                    let fragments = build(&artifact, (1..count).into_iter(), &face);
                    assert_eq!(fragments.len(), 6);
                    let pixels = |split: bool| {
                        let mut surface = Surface::new_raster_n32_premul((800, 48)).unwrap();
                        surface.canvas().clear(Color::WHITE);
                        if split {
                            for index in 1..count {
                                fragments[&index].paint(
                                    surface.canvas(),
                                    artifact.rect_by_index[index].unwrap(),
                                    &style,
                                );
                            }
                        } else {
                            for (row, line) in lines.iter().enumerate() {
                                render_skia::draw_text_line(
                                    surface.canvas(),
                                    origin,
                                    8.0 + row as f32 * 18.0,
                                    &line.concat(),
                                    16.0,
                                    style.color,
                                    style.opacity,
                                    &face,
                                    &style,
                                );
                            }
                        }
                        let info =
                            ImageInfo::new((800, 48), ColorType::RGBA8888, AlphaType::Premul, None);
                        let mut pixels = vec![0; 800 * 48 * 4];
                        assert!(surface.read_pixels(&info, &mut pixels, 800 * 4, (0, 0)));
                        pixels
                    };
                    let whole = pixels(false);
                    assert!(whole.chunks_exact(4).any(|pixel| pixel[0] < 200));
                    assert_eq!(pixels(true), whole, "kerning={kerning}, origin={origin}");
                    }
                }
            }
        }
    }
}
