//! Solid decoration lines for a shaped horizontal text run.
//!
//! Keep the same concrete faces and authored-fragment offsets as glyph paint.
//! Decoration changes paint only, never text advance or line layout.
use skia_safe::{Canvas, ClipOp, Rect, Typeface};
use w3cos_std::{Color, Style, style::TextDecoration};

fn decoration_color(style: &Style, current_color: Color) -> Color {
    style.custom_properties.as_ref()
        .and_then(|properties| properties.get(w3cos_dom::css_style::TEXT_DECORATION_COLOR_PROPERTY))
        .and_then(|value| Color::from_css(value))
        .unwrap_or(current_color)
}

thread_local! {
    static ANALYTIC_RECT: Option<(skia_safe::RuntimeEffect, skia_safe::Blender)> = {
        let shader = skia_safe::RuntimeEffect::make_for_shader(
            "uniform float4 bounds; uniform float4 rgba; half4 main(float2 p) {
                float2 pixel = floor(p);
                float2 covered = clamp(min(pixel + 1.0, bounds.zw) - max(pixel, bounds.xy), 0.0, 1.0);
                return half4(rgba * covered.x * covered.y);
            }", None).ok();
        let blender = skia_safe::RuntimeEffect::make_for_blender(
            // Compensate the CPU pipeline's intermediate FP32 rounding.
            // V564 matches the GPU's fused blend and UNORM conversion without
            // color-specific offsets, tolerances or a wider raster surface.
            "uniform float norm;
            float fused(float a, float b, float c) {
                float sa = a * 4097.0;
                float ah = sa - (sa - a);
                float al = a - ah;
                float sb = b * 4097.0;
                float bh = sb - (sb - b);
                float bl = b - bh;
                float p = a * b;
                float pe = ((ah * bh - p) + ah * bl + al * bh) + al * bl;
                float sum = p + c;
                float v = sum - p;
                float se = (p - (sum - v)) + (c - v);
                return sum + (pe + se);
            }
            float q(float v) {
                float p = v * 256.0;
                float hi = p - v;
                float lo = (p - hi) - v;
                float base = floor(hi);
                float delta = (hi - base) - 0.5;
                return (base + (delta + lo >= 0.0 ? 1.0 : 0.0)) / norm;
            }
            half4 main(half4 src, half4 dst) {
                float4 d = floor(float4(dst) * 255.0 + 0.5) / norm;
                float t = 1.0 - src.a;
                float4 v = float4(fused(d.r, t, src.r), fused(d.g, t, src.g),
                    fused(d.b, t, src.b), fused(d.a, t, src.a));
                return half4(q(v.r), q(v.g), q(v.b), q(v.a));
            }", None).ok().and_then(|effect| effect.make_blender(
                skia_safe::Data::new_copy(&255.0_f32.to_ne_bytes()), None));
        shader.zip(blender)
    };
}

fn draw_decoration_rect(canvas: &Canvas, rect: Rect, color: Color, opacity: f32) {
    // CPU AA rectangles quantize coverage before blending. Default Chromium's
    // GPU path instead blends fractional pixel area then packs to UNORM.
    // Preserve authored bounds/color for all colors, not just black endpoints.
    let matrix = canvas.local_to_device_as_3x3();
    let device_rect = matrix.is_scale_translate()
        .then(|| matrix.map_rect_scale_translate(rect)).flatten()
        .filter(|r| r.is_finite() && r.width() > 0.0 && r.height() > 0.0);
    let drawn = device_rect.is_some_and(|r| ANALYTIC_RECT.with(|effects| {
        let Some((effect, blender)) = effects else { return false; };
        let alpha = (color.a as f32 * opacity.clamp(0.0, 1.0)).round() / 255.0;
        let values = [r.left, r.top, r.right, r.bottom,
            color.r as f32 / 255.0 * alpha,
            color.g as f32 / 255.0 * alpha,
            color.b as f32 / 255.0 * alpha, alpha];
        let mut uniforms = [0_u8; 32];
        for (value, bytes) in values.iter().zip(uniforms.chunks_exact_mut(4)) {
            bytes.copy_from_slice(&value.to_ne_bytes());
        }
        let Some(shader) = effect.make_shader(skia_safe::Data::new_copy(&uniforms), &[], None) else {
            return false;
        };
        let mut paint = skia_safe::Paint::default();
        paint.set_shader(shader).set_blender(blender.clone());
        let save = canvas.save();
        // Existing device clips/layers remain intact; evaluate coverage in
        // device pixels so translation and axis scale cannot shift the grid.
        canvas.reset_matrix();
        canvas.draw_rect(Rect::new(r.left.floor(), r.top.floor(), r.right.ceil(), r.bottom.ceil()), &paint);
        canvas.restore_to_count(save);
        true
    }));
    if !drawn {
        // Non-axis/perspective transforms keep the existing Skia geometry path.
        canvas.draw_rect(rect, &decoration_paint(color, opacity));
    }
}

fn decoration_paint(color: Color, opacity: f32) -> skia_safe::Paint {
    let mut paint = super::color_paint(color, opacity);
    // Solid decoration rectangles use Skia's standard coverage SrcOver path.
    // A runtime blender changes adjacent fractional-edge rounding (V498);
    // neither the authored color nor the line geometry needs a correction.
    paint.set_blend_mode(skia_safe::BlendMode::SrcOver);
    paint
}

fn decoration_has_glyph_outlines(style: &Style) -> bool {
    // Registered Ahem paints real glyphs (including fallback font-stack runs),
    // so skip-ink must use their outlines too. Only the no-font deterministic
    // cell compatibility path lacks matching font outlines.
    !super::style_uses_ahem(style) || super::registered_ahem_face(style).is_some()
}

pub(super) fn paint_applied_in_rect(
    canvas: &Canvas,
    rect: crate::layout::LayoutRect,
    text: &str,
    child: &Style,
    typeface: &Typeface,
    metrics_font: &fontdue::Font,
    line_context: Option<crate::paint_artifact::InlineLineContext>,
    shaped_fragment: Option<&crate::inline_shaping::Fragment>,
    owner: &crate::paint_artifact::AppliedTextDecoration<'_>,
) {
    let paint_line =
        |canvas: &Canvas, x: f32, top: f32, text: &str, advance: f32, child: &Style,
            expansion: Option<crate::text_layout::JustificationExpansion>| {
            let visual = crate::text_layout::font_render_text_for_style(text, child);
            let expansion = expansion.map(|expansion| expansion.for_text(visual.as_ref()));
            let glyph_baseline = shaped_fragment.map_or_else(
                || super::text_baseline(top, child.font_size, typeface, child, visual.as_ref()),
                |fragment| fragment.baseline(rect),
            );
            // A descendant's vertical-align raises its glyph baseline, not the
            // originating decoration. Undo the same accumulated length shifts
            // that inline line metrics used when projecting those glyph boxes.
            let baseline = glyph_baseline + owner.baseline_shift;
            let runs = super::css_font_runs(" ", typeface, &owner.style);
            let face = runs.first().map_or(typeface, |run| &run.typeface);
            let (_, metrics) =
                crate::skia_text_run::css_font(face, owner.style.font_size).metrics();
            let ascent = -metrics.ascent;
            let thickness = (owner.style.font_size / 10.0).max(1.0);
            let y = match owner.style.text_decoration {
                TextDecoration::Underline => {
                    baseline + ascent.round() - ascent + (thickness / 2.0).ceil().max(1.0)
                }
                // Blink's TextTop overline offset is local to this text
                // fragment; unlike underline it does not undo descendant
                // vertical-align or use the originating box's ascent.
                TextDecoration::Overline => top - thickness.floor(),
                TextDecoration::LineThrough => baseline - ascent / 3.0 - thickness / 2.0,
                TextDecoration::None => return advance,
            };
            let save = canvas.save();
            if owner.style.text_decoration != TextDecoration::LineThrough
                && decoration_has_glyph_outlines(child)
            {
                let upper = y - glyph_baseline + 0.5;
                let lower = y - glyph_baseline + thickness - 0.5;
                let mut intervals = Vec::new();
                if let Some(fragment) = shaped_fragment {
                    intervals = fragment.ink_intercepts(rect, child, upper, lower);
                } else {
                    let mut cursor = x;
                    let mut byte_offset = 0;
                    let adjustments = super::authored_fragment_adjustments(visual.as_ref(), child);
                    for run in super::css_font_runs(visual.as_ref(), typeface, child) {
                        let font = crate::skia_text_run::css_font_for_style(&run.typeface, child.font_size, child);
                        let shaped = if let Some(expansion) = expansion {
                            crate::skia_text_run::shape_visual_run_with_expansion(run.text,
                                &run.typeface, child, adjustments.is_some(), expansion, byte_offset)
                        } else if adjustments.is_some() {
                            crate::skia_text_run::shape_visual_run_with_clusters(
                                run.text,
                                &run.typeface,
                                child,
                            )
                        } else {
                            crate::skia_text_run::shape_visual_run(run.text, &run.typeface, child)
                        };
                        if let Some(mut shaped) = shaped {
                            if let Some((ends, offsets, _)) = &adjustments {
                                for (position, cluster) in
                                    shaped.positions.iter_mut().zip(&shaped.glyph_clusters)
                                {
                                    let fragment =
                                        ends.partition_point(|end| *end <= byte_offset + cluster);
                                    position.x += offsets.get(fragment).copied().unwrap_or(0.0);
                                }
                            }
                            intervals.extend(
                                shaped.intercepts(&font, upper, lower)
                                .into_iter()
                                .map(|position| cursor + position),
                            );
                            cursor += shaped.advance;
                        } else {
                            cursor += font.measure_str(run.text, None).0;
                        }
                        byte_offset += run.text.len();
                    }
                }
                for interval in intervals.chunks_exact(2) {
                    canvas.clip_rect(
                        Rect::new(
                            interval[0] - thickness.min(13.0),
                            y - 1.0,
                            interval[1] + thickness.min(13.0),
                            y + thickness + 1.0,
                        ),
                        ClipOp::Difference,
                        Some(false),
                    );
                }
            }
            if advance > 0.0 {
                // Decoration spans the used inline advance (LayoutUnit), not
                // the unsnapped glyph advance. Glyph origins remain untouched.
                let decoration_advance = crate::text_layout::inline_layout_advance(advance);
                draw_decoration_rect(canvas,
                    Rect::from_xywh(x, y.round(), decoration_advance, thickness.floor().max(1.0)),
                    decoration_color(&owner.style, owner.style.color), child.opacity,
                );
            }
            canvas.restore_to_count(save);
            advance
        };
    if let Some(fragment) = shaped_fragment {
        let (x, advance) = fragment.decoration_span(rect, owner.style.text_decoration);
        if advance > 0.0 { paint_line(canvas, x, rect.y, text, advance, child, None); }
    } else {
        super::draw_text_in_rect_with_line_painter(
            canvas,
            rect,
            text,
            child,
            typeface,
            metrics_font,
            line_context,
            false,
            &mut { paint_line },
        );
    }
}

pub(super) fn paint(
    canvas: &Canvas,
    x: f32,
    top: f32,
    text: &str,
    advance: f32,
    font_size: f32,
    color: Color,
    opacity: f32,
    typeface: &Typeface,
    style: &Style,
) {
    paint_with_expansion(canvas, x, top, text, advance, font_size, color,
        opacity, typeface, style, None);
}

pub(super) fn paint_with_expansion(
    canvas: &Canvas, x: f32, top: f32, text: &str, advance: f32, font_size: f32,
    color: Color, opacity: f32, typeface: &Typeface, style: &Style,
    expansion: Option<crate::text_layout::JustificationExpansion>,
) {
    if style.text_decoration == TextDecoration::None || advance <= 0.0 {
        return;
    }
    let color = decoration_color(style, color);
    let visual = crate::text_layout::font_render_text_for_style(text, style);
    let expansion = expansion.map(|expansion| expansion.for_text(visual.as_ref()));
    let runs = super::css_font_runs(visual.as_ref(), typeface, style);
    let face = runs.first().map_or(typeface, |run| &run.typeface);
    let (_, metrics) = crate::skia_text_run::css_font(face, font_size).metrics();
    let ascent = -metrics.ascent;
    let baseline = if super::style_uses_ahem(style) {
        top + font_size * 0.8
    } else if super::style_uses_generic_monospace(style) {
        top + font_size
    } else {
        super::text_baseline(top, font_size, typeface, style, visual.as_ref())
    };
    // Auto thickness and solid-line snapping follow Blink's horizontal
    // decoration geometry, including its minimum one device-space pixel.
    let thickness = (font_size / 10.0).max(1.0);
    let y = match style.text_decoration {
        TextDecoration::Underline => {
            baseline + ascent.round() - ascent + (thickness / 2.0).ceil().max(1.0)
        }
        TextDecoration::Overline => top - thickness.floor(),
        TextDecoration::LineThrough => baseline - ascent / 3.0 - thickness / 2.0,
        TextDecoration::None => return,
    };
    let save = canvas.save();
    if style.text_decoration != TextDecoration::LineThrough && decoration_has_glyph_outlines(style) {
        let adjustments = super::authored_fragment_adjustments(visual.as_ref(), style);
        let mut cursor = x;
        let mut byte_offset = 0;
        for run in runs {
            let font = crate::skia_text_run::css_font_for_style(&run.typeface, font_size, style);
            let shaped = if let Some(expansion) = expansion {
                crate::skia_text_run::shape_visual_run_with_expansion(run.text,
                    &run.typeface, style, adjustments.is_some(), expansion, byte_offset)
            } else if adjustments.is_some() {
                crate::skia_text_run::shape_visual_run_with_clusters(run.text, &run.typeface, style)
            } else {
                crate::skia_text_run::shape_visual_run(run.text, &run.typeface, style)
            };
            if let Some(mut shaped) = shaped {
                if let Some((ends, offsets, _)) = &adjustments {
                    for (position, cluster) in
                        shaped.positions.iter_mut().zip(&shaped.glyph_clusters)
                    {
                        let fragment = ends.partition_point(|end| *end <= byte_offset + cluster);
                        position.x += offsets.get(fragment).copied().unwrap_or(0.0);
                    }
                }
                // Ignore an intersection shallower than half a pixel, then
                // dilate horizontally to leave clear space around descenders.
                let upper = y - baseline + 0.5;
                let lower = y - baseline + thickness - 0.5;
                let intercepts = shaped.intercepts(&font, upper, lower);
                let dilation = thickness.min(13.0);
                for interval in intercepts.chunks_exact(2) {
                    canvas.clip_rect(
                        Rect::new(
                            cursor + interval[0] - dilation,
                            y - 1.0,
                            cursor + interval[1] + dilation,
                            y + thickness + 1.0,
                        ),
                        ClipOp::Difference,
                        Some(false),
                    );
                }
                cursor += shaped.advance;
            } else {
                cursor += font.measure_str(run.text, None).0;
            }
            byte_offset += run.text.len();
        }
    }
    // Snap only the vertical band. Fractional horizontal endpoints still
    // need coverage AA, just like the adjacent shaped glyph positions.
    draw_decoration_rect(canvas,
        Rect::from_xywh(x, y.round(), crate::text_layout::inline_layout_advance(advance),
            thickness.floor().max(1.0)),
        color, opacity,
    );
    canvas.restore_to_count(save);
}

#[cfg(test)]
mod tests {
    use super::*;
    use skia_safe::{AlphaType, ColorType, FontMgr, ImageInfo, Surface};

    #[test]
    fn explicit_color_controls_decoration_without_changing_glyph_color() {
        let face = FontMgr::default().match_family_style("Times", skia_safe::FontStyle::normal()).unwrap();
        let render = |explicit: Option<&str>, current: Color| {
            let mut style = Style { color: current, font_size: 16.0,
                text_decoration: TextDecoration::Underline, ..Style::default() };
            if let Some(value) = explicit {
                style.custom_properties = Some(std::collections::HashMap::from([
                    (w3cos_dom::css_style::TEXT_DECORATION_COLOR_PROPERTY.into(), value.into())]));
            }
            let mut surface = Surface::new_raster_n32_premul((64, 32)).unwrap();
            surface.canvas().clear(skia_safe::Color::WHITE);
            paint(surface.canvas(), 4.0, 4.0, "test", 40.0, 16.0,
                style.color, 1.0, &face, &style);
            assert_eq!(style.color, current);
            let info = ImageInfo::new((64, 32), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 64 * 32 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 64 * 4, (0, 0)));
            pixels
        };
        let black = render(None, Color::BLACK);
        assert!(black.chunks_exact(4).any(|pixel| pixel == [0, 0, 0, 255]));
        assert_eq!(render(Some("black"), Color::rgb(0, 128, 0)), black);
        let green = render(None, Color::rgb(0, 128, 0));
        assert_ne!(green, black);
        assert_eq!(render(Some("currentcolor"), Color::rgb(0, 128, 0)), green);
        assert_eq!(render(Some("transparent"), Color::BLACK), vec![255; 64 * 32 * 4]);
    }

    #[test]
    fn analytic_decoration_browser_color_packing_at_half_boundaries() {
        let mut surface = Surface::new_raster_n32_premul((32, 4)).unwrap();
        surface.canvas().clear(skia_safe::Color::WHITE);
        let color = Color::rgb(37, 99, 211);
        for (left, right) in [(8.0, 16.0), (16.0, 23.375)] {
            draw_decoration_rect(surface.canvas(), Rect::new(left, 0.0, right, 4.0), color, 1.0);
        }
        let info = ImageInfo::new((32, 4), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 32 * 4 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 32 * 4, (0, 0)));
        // V537 case192 remains a real RED; do not weaken its default-browser
        // color-packing assertion merely because black coverage now agrees.
        assert_eq!(&pixels[(32 + 23) * 4..(32 + 23) * 4 + 4], &[173, 196, 239, 255]);
    }

    #[test]
    fn analytic_decoration_coverage_preserves_device_translation_and_scale() {
        let rect = Rect::from_xywh(8.375, 4.25, 20.28125, 2.5);
        let matrix = skia_safe::Matrix::scale_translate((2.0, 1.5), (0.25, 0.5));
        let mapped = matrix.map_rect_scale_translate(rect).unwrap();
        let render = |transformed| {
            let mut surface = Surface::new_raster_n32_premul((80, 24)).unwrap();
            surface.canvas().clear(skia_safe::Color::WHITE);
            if transformed { surface.canvas().concat(&matrix); }
            draw_decoration_rect(surface.canvas(), if transformed { rect } else { mapped },
                Color::rgb(37, 99, 211), 128.0 / 255.0);
            let info = ImageInfo::new((80, 24), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 80 * 24 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 80 * 4, (0, 0)));
            pixels
        };
        assert_eq!(render(true), render(false));
    }

    #[test]
    fn registered_ahem_has_skip_ink_outlines_without_changing_cell_fallback() {
        let style = Style { font_family: Some("Ahem, monospace".into()),
            font_size: 32.0, ..Style::default() };
        assert!(!decoration_has_glyph_outlines(&style), "unregistered cells have no font outlines");
        let owner = u64::MAX - 512;
        struct ClearOwner(u64);
        impl Drop for ClearOwner {
            fn drop(&mut self) { crate::font_face::FontRegistry::global().clear_owner(self.0); }
        }
        let guard = ClearOwner(owner);
        // Eligibility follows CSS font registration, not glyph shape or the
        // embedded face's original family name. Original Ahem pixels are WPT.
        crate::font_face::FontRegistry::global().register_for_owner(owner,
            crate::font_face::FontFace { family: "Ahem".into(),
                src: crate::font_face::FontSource::Bytes(include_bytes!("../assets/Inter-Regular.ttf").to_vec()),
                ..Default::default() }).unwrap();
        assert!(decoration_has_glyph_outlines(&style), "registered Ahem must not bypass skip-ink");
        drop(guard);
        assert!(!decoration_has_glyph_outlines(&style));
        assert!(decoration_has_glyph_outlines(&Style::default()));
    }

    #[test]
    fn generated_source_decoration_matches_its_unsplit_line() {
        use crate::layout::LayoutRect;
        use crate::render_skia;
        use skia_safe::{FontStyle, Color};
        use std::collections::HashMap;
        use w3cos_std::ComponentKind;
        use w3cos_std::style::Display;
        use crate::paint_artifact::{AppliedTextDecoration, PaintNode, PaintArtifact};
        let face = FontMgr::default().match_family_style("Times", FontStyle::normal()).unwrap();
        let texts = ["White", " ", "Space"];
        for opacity in [1.0, 0.5] {
            let child = Style { display: Display::Inline, font_family: Some("serif".into()),
                font_size: 16.0, line_height: 1.125, opacity,
                color: w3cos_std::Color::rgb(0, 0, 238),
                text_decoration: w3cos_std::style::TextDecoration::Underline,
                custom_properties: Some(HashMap::from([
                    (w3cos_std::inline_text::SOURCE_RUN.into(), "text:unit-source".into())])),
                ..Style::default() };
            let parent = Style { display: Display::Flex, custom_properties: Some(HashMap::from([
                ("--w3cos-internal-inline-formatting-context".into(), "1".into())])), ..Style::default() };
            let mut nodes = vec![PaintNode { kind: ComponentKind::Row, style: parent,
                parent: None, sticky_counter_signal: None }];
            let mut layouts = vec![(LayoutRect { x: 8.375, y: 16.0, width: 100.0, height: 18.0 }, 0)];
            let mut x = 8.375;
            let widths = render_skia::measure_skia_inline_fragment_advances(&texts, &child).unwrap();
            for (text, width) in texts.iter().zip(widths) {
                let width = crate::text_layout::inline_layout_advance(width);
                nodes.push(PaintNode { kind: ComponentKind::Text { content: (*text).into() },
                    style: child.clone(), parent: Some(0), sticky_counter_signal: None });
                layouts.push((LayoutRect { x, y: 16.0, width, height: 18.0 }, nodes.len() - 1));
                x += width;
            }
            let artifact = PaintArtifact::build(nodes, &layouts, 1);
            let fragments = crate::inline_shaping::build(&artifact, 1..4, &face);
            let owner = AppliedTextDecoration { style: std::borrow::Cow::Borrowed(&child), baseline_shift: 0.0 };
            let pixels = |split| {
                let mut surface = Surface::new_raster_n32_premul((120, 48)).unwrap();
                surface.canvas().clear(Color::WHITE);
                if split {
                    for index in 1..4 {
                        paint_applied_in_rect(surface.canvas(),
                            artifact.rect_by_index[index].unwrap(), texts[index - 1], &child,
                            &face, crate::layout::layout_font(), None, fragments.get(&index), &owner);
                    }
                } else {
                    paint_applied_in_rect(surface.canvas(),
                        LayoutRect { x: 8.375, y: 16.0, width: x - 8.375, height: 18.0 },
                        &texts.concat(), &child, &face, crate::layout::layout_font(), None, None, &owner);
                }
                let info = ImageInfo::new((120, 48), ColorType::RGBA8888, AlphaType::Premul, None);
                let mut pixels = vec![255_u8; 120 * 48 * 4];
                assert!(surface.read_pixels(&info, &mut pixels, 120 * 4, (0, 0)));
                pixels
            };
            let split = pixels(true);
            let whole = pixels(false);
            let different = split.chunks_exact(4).zip(whole.chunks_exact(4)).filter(|(a, b)| a != b).count();
            assert_eq!(different, 0, "generated words of one source must retain its decoration stripe; opacity={opacity}");
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn applied_decoration_endpoints_use_layout_unit_advances() {
        let face = FontMgr::default()
            .new_from_data(include_bytes!("../assets/Inter-Regular.ttf"), None).unwrap();
        let owner_style = Style { font_family: Some("serif".into()), font_size: 60.0,
            color: Color::BLACK, text_decoration: TextDecoration::Underline,
            ..Style::default() };
        let child = Style { display: w3cos_std::style::Display::Inline,
            font_family: Some("serif".into()), font_size: 24.0, line_height: 0.4,
            white_space: w3cos_std::style::WhiteSpace::NoWrap, ..Style::default() };
        let mut surface = Surface::new_raster_n32_premul((260, 120)).unwrap();
        surface.canvas().clear(skia_safe::Color::WHITE);
        for (x, y, width, shift, text) in [
            (68.0, 40.0, 55.96875, 24.0, "abcde"),
            (123.96875, 52.0, 45.328125, 12.0, "fghij"),
            (169.296875, 64.0, 61.34375, 0.0, "klmno"),
        ] {
            let owner = crate::paint_artifact::AppliedTextDecoration {
                style: std::borrow::Cow::Borrowed(&owner_style), baseline_shift: shift };
            paint_applied_in_rect(surface.canvas(), crate::layout::LayoutRect {
                x, y, width, height: 28.0 }, text, &child, &face,
                crate::layout::layout_font(), None, None, &owner);
        }
        let info = ImageInfo::new((260, 120), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![255_u8; 260 * 120 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 260 * 4, (0, 0)));
        assert_eq!(pixels[(89 * 260 + 80) * 4], 0, "visible black underline must start at y89");
        for y in 89..95 {
            // V525 captures default Chromium GPU; V528/V529 prove the old
            // 6/91 expectation was the software-browser path, not this oracle.
            assert_eq!(pixels[(y * 260 + 123) * 4], 8, "default Chromium141 adjacent fragment coverage at x123");
            assert_eq!(pixels[(y * 260 + 230) * 4], 92, "default Chromium141 terminal coverage at x230");
        }
    }

    #[test]
    fn applied_overline_tracks_fragment_text_top_without_owner_baseline_shift() {
        let face = FontMgr::default()
            .new_from_data(include_bytes!("../assets/Inter-Regular.ttf"), None).unwrap();
        let owner_style = Style { font_family: Some("serif".into()), font_size: 60.0,
            color: Color::BLACK, text_decoration: TextDecoration::Overline,
            ..Style::default() };
        let child = Style { display: w3cos_std::style::Display::Inline,
            font_family: Some("serif".into()), font_size: 24.0, line_height: 1.0,
            white_space: w3cos_std::style::WhiteSpace::NoWrap, ..Style::default() };
        let mut surface = Surface::new_raster_n32_premul((260, 140)).unwrap();
        surface.canvas().clear(skia_safe::Color::WHITE);
        for (x, top, width, shift, text) in [
            (68.0, 69.0, 55.96875, 12.0, "abcde"),
            (123.96875, 81.0, 45.328125, 0.0, "fghij"),
            (169.296875, 93.0, 61.34375, -12.0, "klmno"),
        ] {
            let owner = crate::paint_artifact::AppliedTextDecoration {
                style: std::borrow::Cow::Borrowed(&owner_style), baseline_shift: shift };
            paint_applied_in_rect(surface.canvas(), crate::layout::LayoutRect {
                x, y: top, width, height: 28.0 }, text, &child, &face,
                crate::layout::layout_font(), None, None, &owner);
        }
        let info = ImageInfo::new((260, 140), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![255_u8; 260 * 140 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 260 * 4, (0, 0)));
        // Original Chromium141 length001 has these three fragment-local
        // stripes; its strict WPT reference disagreement remains separate.
        for (x, top) in [(80, 63), (140, 75), (180, 87)] {
            assert_eq!(pixels[((top - 1) * 260 + x) * 4], 255);
            for y in top..top + 6 { assert_eq!(pixels[(y * 260 + x) * 4], 0); }
            assert_eq!(pixels[((top + 6) * 260 + x) * 4], 255);
        }
    }

    #[test]
    fn fractional_decoration_endpoints_preserve_coverage() {
        let face = FontMgr::default()
            .new_from_data(include_bytes!("../assets/Inter-Regular.ttf"), None)
            .unwrap();
        let style = Style {
            font_size: 16.0,
            text_decoration: TextDecoration::Underline,
            ..Style::default()
        };
        let mut surface = Surface::new_raster_n32_premul((64, 48)).unwrap();
        surface.canvas().clear(skia_safe::Color::WHITE);
        paint(
            surface.canvas(),
            8.375,
            8.0,
            " ",
            20.28125,
            16.0,
            Color::BLACK,
            1.0,
            &face,
            &style,
        );
        let info = ImageInfo::new((64, 48), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![255_u8; 64 * 48 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 64 * 4, (0, 0)));
        let painted_row = (0..48).find(|y| pixels[(y * 64 + 16) * 4] == 0).unwrap();
        for x in [8, 28] {
            let coverage = pixels[(painted_row * 64 + x) * 4];
            assert!(
                coverage > 0 && coverage < 255,
                "fractional endpoint x={x} lost partial coverage: {coverage}"
            );
        }
        assert_eq!(pixels[((painted_row - 1) * 64 + 16) * 4], 255);
        assert_eq!(pixels[((painted_row + 1) * 64 + 16) * 4], 255);
    }
}
