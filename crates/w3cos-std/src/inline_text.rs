//! Source fragment boundaries retained through anonymous text coalescing.
use crate::Style;

pub const FRAGMENT_ENDS: &str = "--w3cos-internal-text-fragment-ends";
pub const SOURCE_RUN: &str = "--w3cos-internal-text-source-run";
pub const DECORATION_COLOR: &str = "--w3cos-internal-text-decoration-color";

/// Anonymous text paints its decorating element's used properties; CSS
/// element boxes still do not inherit these properties by default.
pub fn copy_used_decoration(style: &mut Style, owner: &Style) {
    style.text_decoration = owner.text_decoration;
    let value = owner.custom_properties.as_ref().and_then(|p| p.get(DECORATION_COLOR));
    if let Some(value) = value {
        style.custom_properties.get_or_insert_with(Default::default)
            .insert(DECORATION_COLOR.into(), value.clone());
    } else if let Some(properties) = style.custom_properties.as_mut() {
        properties.remove(DECORATION_COLOR);
    }
}

/// Explicit hyphens stay on the preceding line; following glue suppresses
/// their break opportunity. DOM fragments and font wrapping share this rule.
pub fn explicit_hyphen_break_end(text: &str, index: usize) -> Option<usize> {
    let character = text.get(index..)?.chars().next()?;
    if !matches!(character, '-' | '\u{2010}') {
        return None;
    }
    let end = index + character.len_utf8();
    text[end..].chars().next()
        .filter(|next| !matches!(next, '\u{00a0}' | '\u{202f}' | '\u{2060}' | '\u{feff}'))
        .map(|_| end)
}

const DECORATION_COUNT: &str = "--w3cos-internal-decoration-owner-count";
const DECORATION_OWNER: &str = "--w3cos-internal-decoration-owner-";

/// A dissolved split-inline owner is paint metadata, not CSS inheritance.
/// Keep family text after the first newline, so commas/quotes in family names
/// are never interpreted as numeric record separators. No new wire fields.
pub fn prepend_decoration_owner(style: &mut Style, owner: &Style) {
    use crate::style::{FontStyle, FontVariant, TextDecoration};
    let line = match owner.text_decoration {
        TextDecoration::None => return, TextDecoration::Underline => "u",
        TextDecoration::Overline => "o", TextDecoration::LineThrough => "s",
    };
    let font_style = match owner.font_style {
        FontStyle::Normal => "n", FontStyle::Italic => "i", FontStyle::Oblique => "o",
    };
    let p = style.custom_properties.get_or_insert_with(Default::default);
    let count = p.get(DECORATION_COUNT).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0).min(p.len());
    for index in (0..count).rev() {
        if let Some(value) = p.remove(&format!("{DECORATION_OWNER}{index}")) {
            p.insert(format!("{DECORATION_OWNER}{}", index + 1), value);
        }
    }
    let variant = if owner.font_variant == FontVariant::SmallCaps { ",s" } else { "" };
    let decoration_color = owner.custom_properties.as_ref().and_then(|p| p.get(DECORATION_COLOR))
        .and_then(|value| crate::Color::from_css(value))
        .map(|color| format!(",c#{:02x}{:02x}{:02x}{:02x}", color.r, color.g, color.b, color.a))
        .unwrap_or_default();
    p.insert(format!("{DECORATION_OWNER}0"), format!("{line},{},{},{font_style},{},{},{},{},{}{variant}{decoration_color}\n{}",
        owner.font_size, owner.font_weight, owner.color.r, owner.color.g,
        owner.color.b, owner.color.a, u8::from(owner.font_family.is_some()),
        owner.font_family.as_deref().unwrap_or("")));
    p.insert(DECORATION_COUNT.into(), (count + 1).to_string());
}

pub fn decoration_owners(style: &Style) -> Vec<Style> {
    use crate::style::{FontStyle, FontVariant, TextDecoration};
    let Some(p) = &style.custom_properties else { return Vec::new(); };
    let count = p.get(DECORATION_COUNT).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0).min(p.len());
    (0..count).filter_map(|index| {
        let (record, family) = p.get(&format!("{DECORATION_OWNER}{index}"))?.split_once('\n')?;
        let mut parts = record.split(',').collect::<Vec<_>>();
        let decoration_color = parts.last().and_then(|part| part.strip_prefix("c#"))
            .and_then(|hex| crate::Color::from_css(&format!("#{hex}")));
        if decoration_color.is_some() { parts.pop(); }
        if !matches!(parts.len(), 9 | 10) { return None; }
        let text_decoration = match parts[0] { "u" => TextDecoration::Underline,
            "o" => TextDecoration::Overline, "s" => TextDecoration::LineThrough, _ => return None };
        let font_style = match parts[3] { "n" => FontStyle::Normal,
            "i" => FontStyle::Italic, "o" => FontStyle::Oblique, _ => return None };
        let font_variant = match parts.get(9).copied() {
            None => FontVariant::Normal, Some("s") => FontVariant::SmallCaps, _ => return None,
        };
        let font_size: f32 = parts[1].parse().ok()?;
        if !font_size.is_finite() || font_size <= 0.0 { return None; }
        Some(Style { text_decoration, font_size, font_weight: parts[2].parse().ok()?, font_style, font_variant,
            color: crate::Color::rgba(parts[4].parse().ok()?, parts[5].parse().ok()?,
                parts[6].parse().ok()?, parts[7].parse().ok()?),
            font_family: match parts[8] { "0" => None, "1" => Some(family.into()), _ => return None },
            custom_properties: decoration_color.map(|color| std::collections::HashMap::from([
                (DECORATION_COLOR.into(), format!("#{:02x}{:02x}{:02x}{:02x}", color.r, color.g, color.b, color.a))])),
            ..Style::default() })
    }).collect()
}

pub fn metadata_cache_key(style: &Style) -> u64 {
    style.custom_properties.as_ref().and_then(|properties| properties.get(FRAGMENT_ENDS))
        .map_or(0, |value| fingerprint(value))
}

fn fingerprint(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |hash, byte|
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3))
}

/// Ignore inherited/stale metadata when a lowering pass replaced the text.
/// These offsets are UTF-8 byte ends, never glyph or character indices.
pub fn fragment_ends(text: &str, style: &Style) -> Option<Vec<usize>> {
    let value = style.custom_properties.as_ref()?.get(FRAGMENT_ENDS)?;
    let (hash, ends) = value.split_once(':')?;
    if u64::from_str_radix(hash, 16).ok()? != fingerprint(text) { return None; }
    let ends = ends.split(',').map(str::parse).collect::<Result<Vec<usize>, _>>().ok()?;
    if ends.last().copied() != Some(text.len()) || ends.first().copied()? == 0
        || ends.windows(2).any(|pair| pair[0] >= pair[1])
        || ends.iter().any(|end| !text.is_char_boundary(*end)) { return None; }
    Some(ends)
}

pub fn append_fragment_ends(output: &mut Vec<usize>, offset: usize, text: &str, style: &Style) {
    if text.is_empty() { return; }
    output.extend(fragment_ends(text, style).unwrap_or_else(|| vec![text.len()])
        .into_iter().map(|end| offset + end));
}

/// Generated word/space pieces still belong to their one original text run.
/// Rejoining them must not manufacture a new CSS inline shaping boundary.
pub fn append_source_fragment_ends(output: &mut Vec<usize>, offset: usize, text: &str,
    style: &Style, previous_source: &mut Option<String>) {
    if text.is_empty() { return; }
    let source = style.custom_properties.as_ref().and_then(|p| p.get(SOURCE_RUN)).cloned();
    if source.is_some() && source == *previous_source && output.last().copied() == Some(offset) {
        output.pop();
    }
    append_fragment_ends(output, offset, text, style);
    *previous_source = source;
}

pub fn set_fragment_ends(style: &mut Style, text: &str, ends: &[usize]) {
    let properties = style.custom_properties.get_or_insert_with(Default::default);
    properties.remove(FRAGMENT_ENDS);
    let mut ends = ends.iter().copied().filter(|end| *end > 0 && *end <= text.len()
        && text.is_char_boundary(*end)).collect::<Vec<_>>();
    ends.sort_unstable();
    ends.dedup();
    if ends.last().copied() != Some(text.len()) && !text.is_empty() { ends.push(text.len()); }
    if ends.len() > 1 {
        properties.remove(SOURCE_RUN);
        let offsets = ends.iter().map(usize::to_string).collect::<Vec<_>>().join(",");
        properties.insert(FRAGMENT_ENDS.into(), format!("{:x}:{offsets}", fingerprint(text)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_decoration_color_survives_used_text_and_owner_snapshots() {
        let owner = Style { color: crate::Color::rgb(0, 128, 0),
            text_decoration: crate::style::TextDecoration::Underline,
            font_variant: crate::style::FontVariant::SmallCaps,
            custom_properties: Some(std::collections::HashMap::from([
                (DECORATION_COLOR.into(), "rgba(37, 99, 211, 0.5)".into())])),
            ..Style::default() };
        let mut text = Style { color: crate::Color::BLACK, ..Style::default() };
        copy_used_decoration(&mut text, &owner);
        assert_eq!(text.text_decoration, owner.text_decoration);
        assert_eq!(text.color, crate::Color::BLACK);
        assert_eq!(text.custom_properties.as_ref().unwrap().get(DECORATION_COLOR),
            owner.custom_properties.as_ref().unwrap().get(DECORATION_COLOR));
        prepend_decoration_owner(&mut text, &owner);
        let snapshots = decoration_owners(&text);
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].color, owner.color);
        assert_eq!(snapshots[0].font_variant, owner.font_variant);
        let value = snapshots[0].custom_properties.as_ref().unwrap().get(DECORATION_COLOR).unwrap();
        assert_eq!(crate::Color::from_css(value), crate::Color::from_css("rgba(37, 99, 211, 0.5)"));
        copy_used_decoration(&mut text, &Style::default());
        assert!(!text.custom_properties.as_ref().unwrap().contains_key(DECORATION_COLOR));
    }

    #[test]
    fn decoration_owners_preserve_nested_order_and_independent_text_style() {
        use crate::{Color, style::{FontStyle, FontVariant, TextDecoration}};
        let mut child = Style { color: Color::rgb(0, 128, 0), font_size: 24.0, ..Style::default() };
        let inner = Style { text_decoration: TextDecoration::Overline,
            font_family: Some("'Comma, Family',\nserif".into()), font_style: FontStyle::Italic,
            font_variant: FontVariant::SmallCaps,
            font_weight: 700, color: Color::rgb(0, 0, 238), font_size: 16.5, ..Style::default() };
        let outer = Style { text_decoration: TextDecoration::Underline,
            color: Color::rgba(255, 0, 0, 128), font_size: 12.0, ..Style::default() };
        prepend_decoration_owner(&mut child, &inner);
        prepend_decoration_owner(&mut child, &outer);
        let owners = decoration_owners(&child);
        assert_eq!(owners.len(), 2);
        assert_eq!(owners[0].text_decoration, outer.text_decoration);
        assert_eq!(owners[0].color, outer.color);
        assert_eq!(owners[1].font_family, inner.font_family);
        assert_eq!(owners[1].font_style, inner.font_style);
        assert_eq!(owners[1].font_variant, FontVariant::SmallCaps);
        assert_eq!(owners[0].font_variant, FontVariant::Normal, "legacy nine-field metadata remains valid");
        assert_eq!(owners[1].font_weight, inner.font_weight);
        assert_eq!(owners[1].font_size, inner.font_size);
        assert_eq!(child.text_decoration, TextDecoration::None);
        assert_eq!(child.color, Color::rgb(0, 128, 0));
        assert_eq!(child.font_size, 24.0);
    }

    #[test]
    fn source_offsets_are_utf8_checked_and_bound_to_the_actual_text() {
        let mut style = Style::default();
        set_fragment_ends(&mut style, "é界X", &[1, 2, 5, 6]);
        assert_eq!(fragment_ends("é界X", &style), Some(vec![2, 5, 6]));
        assert_eq!(fragment_ends("é界Y", &style), None);
        let mut ends = Vec::new();
        append_fragment_ends(&mut ends, 8, "é界X", &style);
        assert_eq!(ends, [10, 13, 14]);
    }
}
