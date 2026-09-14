//! Font positioning shared by Skia advance measurement, glyph paint and ink bounds.
//! Input is the visual-order font run supplied by the existing bidi stage.
use skia_safe::{Font, GlyphId, Point, Rect, Typeface};
use std::{cell::RefCell, collections::HashMap, sync::Arc};
use w3cos_std::style::Style;

thread_local! {
    static FONT_TABLES: RefCell<HashMap<u32, Arc<Vec<u8>>>> = RefCell::new(HashMap::new());
}

pub(crate) struct GlyphRun {
    pub glyphs: Vec<GlyphId>,
    pub positions: Vec<Point>,
    pub advance: f32,
}

impl GlyphRun {
    pub fn ink_bounds(&self, font: &Font) -> Option<Rect> {
        let mut bounds = vec![Rect::default(); self.glyphs.len()];
        font.get_bounds(&self.glyphs, &mut bounds, None);
        let mut ink = None::<Rect>;
        for (bounds, position) in bounds.into_iter().zip(&self.positions) {
            if bounds.is_empty() {
                continue;
            }
            let shifted = Rect::new(
                bounds.left + position.x,
                bounds.top + position.y,
                bounds.right + position.x,
                bounds.bottom + position.y,
            );
            if let Some(ink) = &mut ink {
                ink.join(shifted);
            } else {
                ink = Some(shifted);
            }
        }
        ink
    }
}

// Skia resolves the concrete system/registered face. Repackage its tables in
// memory so shaping cannot silently select a different font from painting.
fn font_tables(face: &Typeface) -> Option<Arc<Vec<u8>>> {
    FONT_TABLES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(data) = cache.get(&face.unique_id()) {
            return Some(data.clone());
        }
        let mut tables: Vec<_> = face
            .read_table_tags()?
            .into_iter()
            .filter_map(|tag| face.copy_table_data(tag).map(|data| (tag, data)))
            .collect();
        tables.sort_by_key(|(tag, _)| *tag);
        let count = u16::try_from(tables.len()).ok()?;
        if count == 0 {
            return None;
        }
        let mut bytes = vec![0; 12 + tables.len() * 16];
        let cff = tables.iter().any(|(tag, _)| {
            *tag == u32::from_be_bytes(*b"CFF ") || *tag == u32::from_be_bytes(*b"CFF2")
        });
        bytes[..4].copy_from_slice(if cff { b"OTTO" } else { &[0, 1, 0, 0] });
        bytes[4..6].copy_from_slice(&count.to_be_bytes());
        let power = 1u16 << count.ilog2();
        bytes[6..8].copy_from_slice(&(power * 16).to_be_bytes());
        bytes[8..10].copy_from_slice(&(count.ilog2() as u16).to_be_bytes());
        bytes[10..12].copy_from_slice(&((count - power) * 16).to_be_bytes());
        for (i, (tag, data)) in tables.into_iter().enumerate() {
            let offset = u32::try_from(bytes.len()).ok()?;
            let length = u32::try_from(data.size()).ok()?;
            let entry = 12 + i * 16;
            bytes[entry..entry + 4].copy_from_slice(&tag.to_be_bytes());
            bytes[entry + 8..entry + 12].copy_from_slice(&offset.to_be_bytes());
            bytes[entry + 12..entry + 16].copy_from_slice(&length.to_be_bytes());
            bytes.extend_from_slice(data.as_bytes());
            while bytes.len() % 4 != 0 {
                bytes.push(0);
            }
        }
        // The cache is bounded by concrete faces, not by arbitrary text runs.
        if cache.len() >= 64 {
            cache.clear();
        }
        let data = Arc::new(bytes);
        cache.insert(face.unique_id(), data.clone());
        Some(data)
    })
}

pub(crate) fn shape_visual_run(text: &str, face: &Typeface, style: &Style) -> Option<GlyphRun> {
    let bytes = font_tables(face)?;
    let shaping_face = rustybuzz::Face::from_slice(&bytes, 0)?;
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    // Bidi ordering and mirrored characters have already been resolved by
    // font_render_text_for_style. Do not reorder this visual run a second time.
    buffer.set_direction(rustybuzz::Direction::LeftToRight);
    let features: Vec<_> = if style.letter_spacing != 0.0 {
        ["liga=0", "clig=0"]
            .into_iter()
            .filter_map(|feature| feature.parse().ok())
            .collect()
    } else {
        Vec::new()
    };
    let shaped = rustybuzz::shape(&shaping_face, &features, buffer);
    let scale = style.font_size / shaping_face.units_per_em() as f32;
    let mut glyphs = Vec::with_capacity(shaped.len());
    let mut positions = Vec::with_capacity(shaped.len());
    let mut cursor = 0.0;
    for (i, (info, position)) in shaped
        .glyph_infos()
        .iter()
        .zip(shaped.glyph_positions())
        .enumerate()
    {
        glyphs.push(u16::try_from(info.glyph_id).ok()?);
        positions.push(Point::new(
            cursor + position.x_offset as f32 * scale,
            -(position.y_offset as f32) * scale,
        ));
        cursor += position.x_advance as f32 * scale;
        let cluster_ends = shaped
            .glyph_infos()
            .get(i + 1)
            .is_none_or(|next| next.cluster != info.cluster);
        if cluster_ends {
            // CSS inline advances retain spacing after the final typographic
            // character, including when the next character is in another run.
            cursor += style.letter_spacing;
            if text
                .get(info.cluster as usize..)
                .and_then(|text| text.chars().next())
                .is_some_and(|character| matches!(character, ' ' | '\u{00a0}'))
            {
                cursor += style.word_spacing;
            }
        }
    }
    Some(GlyphRun {
        glyphs,
        positions,
        advance: cursor,
    })
}
