//! External SVG paint-tree rasterization with subpixel coverage precision.
use crate::image_loader::DecodedImage;
use quick_xml::{
    Reader, Writer,
    events::{BytesStart, Event},
};
use skia_safe::{AlphaType, ColorType, FontMgr, ImageInfo};
use std::sync::Arc;

fn rectangle(data: &str) -> Option<[f32; 4]> {
    let tokens: Vec<_> = data.split_ascii_whitespace().collect();
    if tokens.len() != 13
        || tokens[0] != "M"
        || tokens[3] != "L"
        || tokens[6] != "L"
        || tokens[9] != "L"
        || tokens[12] != "Z"
    {
        return None;
    }
    let mut points = [[0.0_f32; 2]; 4];
    for (i, point) in points.iter_mut().enumerate() {
        point[0] = tokens[i * 3 + 1].parse().ok()?;
        point[1] = tokens[i * 3 + 2].parse().ok()?;
        if !point.iter().all(|v| v.is_finite()) {
            return None;
        }
    }
    let left = points.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
    let top = points.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
    let right = points
        .iter()
        .map(|p| p[0])
        .fold(f32::NEG_INFINITY, f32::max);
    let bottom = points
        .iter()
        .map(|p| p[1])
        .fold(f32::NEG_INFINITY, f32::max);
    if right <= left || bottom <= top {
        return None;
    }
    let corners = [[left, top], [right, top], [right, bottom], [left, bottom]];
    for i in 0..4 {
        if !corners.contains(&points[i]) || points[..i].contains(&points[i]) {
            return None;
        }
        let next = points[(i + 1) % 4];
        if points[i][0] != next[0] && points[i][1] != next[1] {
            return None;
        }
    }
    Some([left, top, right - left, bottom - top])
}

fn primitive_rectangles(source: &str) -> Option<String> {
    let mut reader = Reader::from_str(source);
    let mut writer = Writer::new(Vec::new());
    loop {
        match reader.read_event().ok()? {
            Event::Empty(node) if node.local_name().as_ref() == b"path" => {
                let attributes = node.attributes().collect::<Result<Vec<_>, _>>().ok()?;
                let no_stroke = attributes
                    .iter()
                    .any(|a| a.key.as_ref() == b"stroke" && a.value.as_ref() == b"none");
                let rect = no_stroke
                    .then(|| attributes.iter().find(|a| a.key.as_ref() == b"d"))
                    .flatten()
                    .and_then(|a| std::str::from_utf8(&a.value).ok())
                    .and_then(rectangle);
                if let Some(rect) = rect {
                    let mut replacement = BytesStart::new("rect");
                    for attribute in attributes {
                        if attribute.key.as_ref() != b"d" {
                            replacement.push_attribute(attribute);
                        }
                    }
                    let values = rect.map(|v| v.to_string());
                    for (key, value) in ["x", "y", "width", "height"].into_iter().zip(values.iter())
                    {
                        replacement.push_attribute((key, value.as_str()));
                    }
                    writer.write_event(Event::Empty(replacement)).ok()?;
                } else {
                    writer.write_event(Event::Empty(node)).ok()?;
                }
            }
            Event::Eof => break,
            event => writer.write_event(event).ok()?,
        }
    }
    String::from_utf8(writer.into_inner()).ok()
}

pub(crate) fn rasterize(tree: &resvg::usvg::Tree, width: u32, height: u32) -> Option<DecodedImage> {
    let source = primitive_rectangles(&tree.to_string(&resvg::usvg::WriteOptions::default()))?;
    let mut dom = skia_safe::svg::Dom::from_str(&source, FontMgr::default()).ok()?;
    let size = tree.size();
    dom.set_container_size((size.width(), size.height()));
    // Keep the temporary surface and returned pixels within the decoder's
    // 256 MiB bound. Large documents retain the ordinary vector raster scale.
    let scale = if u64::from(width) * u64::from(height) * 20 <= 256 * 1024 * 1024 {
        2
    } else {
        1
    };
    let raster_width = width.checked_mul(scale)?;
    let raster_height = height.checked_mul(scale)?;
    let mut surface =
        skia_safe::surfaces::raster_n32_premul((raster_width as i32, raster_height as i32))?;
    surface.canvas().clear(skia_safe::Color::TRANSPARENT);
    surface.canvas().scale((
        raster_width as f32 / size.width(),
        raster_height as f32 / size.height(),
    ));
    dom.render(surface.canvas());
    let info = ImageInfo::new(
        (raster_width as i32, raster_height as i32),
        ColorType::RGBA8888,
        AlphaType::Premul,
        None,
    );
    let mut high = vec![0_u8; raster_width as usize * raster_height as usize * 4];
    if !surface.read_pixels(&info, &mut high, raster_width as usize * 4, (0, 0)) {
        return None;
    }
    let mut rgba = vec![0_u8; width as usize * height as usize * 4];
    for y in 0..height {
        for x in 0..width {
            let mut sum = [0_u32; 4];
            for dy in 0..scale {
                for dx in 0..scale {
                    let index = (((y * scale + dy) * raster_width + x * scale + dx) * 4) as usize;
                    for k in 0..4 {
                        sum[k] += u32::from(high[index + k]);
                    }
                }
            }
            let index = ((y * width + x) * 4) as usize;
            for k in 0..4 {
                rgba[index + k] = (sum[k] / (scale * scale)) as u8;
            }
            let alpha = u32::from(rgba[index + 3]);
            if alpha != 0 {
                for k in 0..3 {
                    rgba[index + k] =
                        ((u32::from(rgba[index + k]) * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
    }
    Some(DecodedImage {
        width,
        height,
        intrinsic_width: width,
        intrinsic_height: height,
        svg_intrinsic_size: None,
        svg_source: None,
        data: Arc::new(rgba),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rectangular_path_recognition_preserves_non_rectangles() {
        assert_eq!(
            rectangle("M 0 0 L 4 0 L 4 6 L 0 6 Z"),
            Some([0.0, 0.0, 4.0, 6.0])
        );
        assert_eq!(
            rectangle("M 4 6 L 4 0 L 0 0 L 0 6 Z"),
            Some([0.0, 0.0, 4.0, 6.0])
        );
        assert!(rectangle("M 0 0 L 4 6 L 4 0 L 0 6 Z").is_none());
        assert!(rectangle("M 0 0 L 0 0 L 4 6 L 0 6 Z").is_none());
        assert!(rectangle("M 0 0 C 4 0 4 6 0 6 Z").is_none());
    }
    #[test]
    fn primitive_conversion_keeps_stroked_paths_and_style() {
        let source = r##"<svg><path fill="#008000" stroke="none" d="M 0 0 L 4 0 L 4 6 L 0 6 Z"/><path stroke="red" d="M 0 0 L 4 0 L 4 6 L 0 6 Z"/></svg>"##;
        let output = primitive_rectangles(source).unwrap();
        assert!(output.contains("<rect fill=\"#008000\""));
        assert!(output.contains("<path stroke=\"red\""));
    }
}
