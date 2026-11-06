//! Embedded font and image detection.
//!
//! Two signals are recorded. A structural scan recognizes font and image
//! containers inside an executable. An optional file scan reports source assets
//! whose full bytes occur in that executable. Overlapping hits are counted once.

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

const MIN_FILE_MATCH: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmbeddedResource {
    pub kind: String,
    pub offset: u64,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

pub fn scan_bytes(bytes: &[u8], roots: &[PathBuf]) -> Vec<EmbeddedResource> {
    let mut found = structural(bytes);
    for root in roots {
        found.extend(files_in_root(bytes, root));
    }
    found.sort_by(|left, right| {
        left.offset
            .cmp(&right.offset)
            .then(right.bytes.cmp(&left.bytes))
            .then(left.kind.cmp(&right.kind))
    });
    dedup_overlaps(found)
}

pub fn embedded_bytes(resources: &[EmbeddedResource]) -> u64 {
    resources.iter().map(|resource| resource.bytes).sum()
}

fn dedup_overlaps(resources: Vec<EmbeddedResource>) -> Vec<EmbeddedResource> {
    let mut kept: Vec<EmbeddedResource> = Vec::new();
    for resource in resources {
        let start = resource.offset;
        let end = resource.offset + resource.bytes;
        if let Some(existing) = kept.iter_mut().find(|item| {
            let item_end = item.offset + item.bytes;
            start < item_end && item.offset < end
        }) {
            if resource.path.is_some() && existing.path.is_none() {
                existing.path = resource.path;
            }
            if resource.bytes > existing.bytes {
                existing.kind = resource.kind;
                existing.offset = resource.offset;
                existing.bytes = resource.bytes;
            }
            continue;
        }
        kept.push(resource);
    }
    kept
}

fn files_in_root(bytes: &[u8], root: &Path) -> Vec<EmbeddedResource> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(file) = fs::read(&path) else {
                continue;
            };
            if file.len() < MIN_FILE_MATCH || file.len() > bytes.len() {
                continue;
            }
            if let Some(offset) = find_slice(bytes, &file) {
                let label = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                found.push(EmbeddedResource {
                    kind: "file".to_string(),
                    offset: offset as u64,
                    bytes: file.len() as u64,
                    path: Some(label),
                });
            }
        }
    }
    found
}

fn structural(bytes: &[u8]) -> Vec<EmbeddedResource> {
    let mut found = Vec::new();
    if bytes.len() < 12 {
        return found;
    }
    let mut index = 0;
    while index + 12 <= bytes.len() {
        if let Some(size) = sfnt_size(bytes, index) {
            found.push(EmbeddedResource {
                kind: if &bytes[index..index + 4] == b"OTTO" {
                    "font-otf".to_string()
                } else {
                    "font-ttf".to_string()
                },
                offset: index as u64,
                bytes: size,
                path: None,
            });
            index += size as usize;
            continue;
        }
        if let Some((kind, size)) = woff_size(bytes, index) {
            found.push(EmbeddedResource {
                kind,
                offset: index as u64,
                bytes: size,
                path: None,
            });
            index += size as usize;
            continue;
        }
        if let Some(size) = png_size(bytes, index) {
            found.push(EmbeddedResource {
                kind: "png".to_string(),
                offset: index as u64,
                bytes: size,
                path: None,
            });
            index += size as usize;
            continue;
        }
        if let Some(size) = webp_size(bytes, index) {
            found.push(EmbeddedResource {
                kind: "webp".to_string(),
                offset: index as u64,
                bytes: size,
                path: None,
            });
            index += size as usize;
            continue;
        }
        index += 1;
    }
    found
}

fn sfnt_size(bytes: &[u8], offset: usize) -> Option<u64> {
    let scaler = u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?);
    if scaler != 0x0001_0000
        && scaler != u32::from_be_bytes(*b"OTTO")
        && scaler != u32::from_be_bytes(*b"true")
    {
        return None;
    }
    let tables = u16::from_be_bytes(bytes.get(offset + 4..offset + 6)?.try_into().ok()?) as usize;
    if !(1..=64).contains(&tables) {
        return None;
    }
    let directory = offset + 12;
    let directory_end = directory.checked_add(tables * 16)?;
    if directory_end > bytes.len() {
        return None;
    }
    let mut end = directory_end;
    let mut core = false;
    for table in 0..tables {
        let entry = directory + table * 16;
        let tag = bytes.get(entry..entry + 4)?;
        if !tag.iter().all(|byte| byte.is_ascii_graphic()) {
            return None;
        }
        core |= matches!(
            tag,
            b"head" | b"maxp" | b"cmap" | b"glyf" | b"CFF " | b"name"
        );
        let table_off =
            u32::from_be_bytes(bytes.get(entry + 8..entry + 12)?.try_into().ok()?) as usize;
        let table_len =
            u32::from_be_bytes(bytes.get(entry + 12..entry + 16)?.try_into().ok()?) as usize;
        if table_len == 0 || table_off < 12 {
            return None;
        }
        let abs = offset.checked_add(table_off)?;
        let abs_end = abs.checked_add(table_len)?;
        if abs_end > bytes.len() {
            return None;
        }
        end = end.max(abs_end);
    }
    if !core {
        return None;
    }
    let size = end - offset;
    (size >= 64).then_some(size as u64)
}

fn woff_size(bytes: &[u8], offset: usize) -> Option<(String, u64)> {
    let magic = bytes.get(offset..offset + 4)?;
    let kind = match magic {
        b"wOFF" => "font-woff",
        b"wOF2" => "font-woff2",
        _ => return None,
    };
    let size = u32::from_be_bytes(bytes.get(offset + 8..offset + 12)?.try_into().ok()?) as usize;
    if !(44..=32 * 1024 * 1024).contains(&size) {
        return None;
    }
    let end = offset.checked_add(size)?;
    if end > bytes.len() {
        return None;
    }
    Some((kind.to_string(), size as u64))
}

fn png_size(bytes: &[u8], offset: usize) -> Option<u64> {
    if bytes.get(offset..offset + 8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut cursor = offset + 8;
    let mut saw_ihdr = false;
    while cursor + 12 <= bytes.len() {
        let length = u32::from_be_bytes(bytes.get(cursor..cursor + 4)?.try_into().ok()?) as usize;
        let kind = bytes.get(cursor + 4..cursor + 8)?;
        if !kind.iter().all(|byte| byte.is_ascii_alphabetic()) {
            return None;
        }
        if kind == b"IHDR" {
            saw_ihdr = true;
        }
        let next = cursor.checked_add(12)?.checked_add(length)?;
        if next > bytes.len() {
            return None;
        }
        cursor = next;
        if kind == b"IEND" {
            return saw_ihdr.then_some((cursor - offset) as u64);
        }
        if cursor - offset > 32 * 1024 * 1024 {
            return None;
        }
    }
    None
}

fn webp_size(bytes: &[u8], offset: usize) -> Option<u64> {
    if bytes.get(offset..offset + 4)? != b"RIFF" || bytes.get(offset + 8..offset + 12)? != b"WEBP" {
        return None;
    }
    let payload = u32::from_le_bytes(bytes.get(offset + 4..offset + 8)?.try_into().ok()?) as usize;
    let size = payload.checked_add(8)?;
    if !(16..=32 * 1024 * 1024).contains(&size) {
        return None;
    }
    let end = offset.checked_add(size)?;
    (end <= bytes.len()).then_some(size as u64)
}

fn find_slice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let last = needle.len() - 1;
    let mut shift = [needle.len(); 256];
    for (index, &byte) in needle[..last].iter().enumerate() {
        shift[byte as usize] = last - index;
    }
    let mut pos = 0;
    while pos + needle.len() <= haystack.len() {
        let mut index = last;
        while haystack[pos + index] == needle[index] {
            if index == 0 {
                return Some(pos);
            }
            index -= 1;
        }
        pos += shift[haystack[pos + last] as usize];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sfnt() -> Vec<u8> {
        let mut bytes = vec![0; 68];
        bytes[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        bytes[4..6].copy_from_slice(&1u16.to_be_bytes());
        bytes[12..16].copy_from_slice(b"head");
        bytes[20..24].copy_from_slice(&28u32.to_be_bytes());
        bytes[24..28].copy_from_slice(&40u32.to_be_bytes());
        bytes
    }

    #[test]
    fn structural_scan_finds_a_font_and_png_and_ignores_noise() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        push_chunk(
            &mut png,
            b"IHDR",
            b"\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00",
        );
        push_chunk(&mut png, b"IDAT", &[0, 1, 2, 3]);
        push_chunk(&mut png, b"IEND", &[]);
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&12u32.to_le_bytes());
        webp.extend_from_slice(b"WEBP");
        webp.extend_from_slice(&[0; 8]);
        let mut bytes = vec![0u8; 16];
        bytes.extend(sample_sfnt());
        bytes.extend(png);
        bytes.extend(webp);
        bytes.extend_from_slice(&[9, 9, 9]);
        let found = structural(&bytes);
        assert!(
            found
                .iter()
                .any(|item| item.kind == "font-ttf" && item.bytes == 68)
        );
        assert!(found.iter().any(|item| item.kind == "png"));
        assert!(found.iter().any(|item| item.kind == "webp"));
        assert!(structural(&[0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]).is_empty());
    }

    fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&0u32.to_be_bytes());
    }

    #[test]
    fn file_scan_labels_an_embedded_asset_once() {
        let dir = std::env::temp_dir().join(format!("w3cos-size-assets-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let payload = vec![0xA5; 300];
        fs::write(dir.join("face.bin"), &payload).unwrap();
        let mut bytes = vec![1, 2, 3, 4];
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&payload);
        let found = scan_bytes(&bytes, std::slice::from_ref(&dir));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path.as_deref(), Some("face.bin"));
        assert_eq!(found[0].bytes, 300);
        let _ = fs::remove_dir_all(&dir);
    }
}
