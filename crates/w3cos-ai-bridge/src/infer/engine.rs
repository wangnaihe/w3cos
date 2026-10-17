//! CPU weight interpreter. Compiled only with `in-process-infer`.
//!
//! The file is a linear scorer, not a GGUF. `w3cos-inferd` still runs the
//! Qwen weights. This engine never opens a socket and never starts a process.
//!
//! Layout, little-endian: `W3IW`, version `1`, `dim` as a byte, `count` as
//! `u16`, then that many rows. A row is `bias` as `i16`, a byte length, the
//! name, an `u16` length, the argument text, and `dim` signed weights. The
//! name `final` is a sentence. Anything else is one tool call whose arguments
//! are JSON. Scoring lowercases the utterance, buckets each byte, and keeps
//! the highest dot product.

use serde_json::{Value, json};

use super::model::{self, MODEL_FILE};
use crate::planner::{PlannedCall, PlannerCompletion, PlannerStep, PlannerTurn};

#[derive(Debug)]
pub struct InProcessBackend {
    weights: Vec<u8>,
}

impl InProcessBackend {
    /// Keep `bytes` only when the SHA-256 matches and the table parses.
    /// A mismatch drops the buffer and does not score it.
    pub fn from_verified(bytes: &[u8], expected_sha256: &str) -> Result<Self, String> {
        let actual = model::sha256_hex(bytes);
        let expected = expected_sha256.trim().to_ascii_lowercase();
        if actual != expected {
            return Err(format!("checksum mismatch: expected {expected}"));
        }
        parse_weights(bytes)?;
        Ok(Self {
            weights: bytes.to_vec(),
        })
    }

    /// Load [`MODEL_FILE`] from the inference directory.
    ///
    /// The length and SHA-256 come from `W3COS_INFER_LEN` and
    /// `W3COS_INFER_SHA256`. A missing pin or a bad file returns before any
    /// score. This does not download.
    pub fn load_configured() -> Result<Self, String> {
        let sha = std::env::var("W3COS_INFER_SHA256").unwrap_or_default();
        let sha = sha.trim();
        if sha.is_empty() {
            return Err("in-process model pin is unset".into());
        }
        let len: u64 = std::env::var("W3COS_INFER_LEN")
            .ok()
            .and_then(|value| value.trim().parse().ok())
            .ok_or_else(|| "in-process model length is unset".to_string())?;
        let path = model::default_infer_dir().join(MODEL_FILE);
        if !model::file_matches(&path, sha, len) {
            return Err("in-process model is not verified".into());
        }
        let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
        Self::from_verified(&bytes, sha)
    }

    pub fn complete_turn(&self, turn: &PlannerTurn) -> Result<PlannerCompletion, String> {
        let started = std::time::Instant::now();
        let table = parse_weights(&self.weights)?;
        let step = score(&table, turn.utterance());
        let elapsed = started.elapsed().as_millis();
        Ok(PlannerCompletion {
            step,
            first_token_ms: Some(elapsed),
            total_ms: elapsed,
        })
    }
}

pub struct WeightRow {
    pub bias: i16,
    pub name: String,
    pub arguments: String,
    pub weights: Vec<i8>,
}

struct Table {
    dim: usize,
    rows: Vec<WeightRow>,
}

#[cfg(test)]
pub fn encode_weights(dim: u8, rows: &[WeightRow]) -> Result<Vec<u8>, String> {
    if dim == 0 {
        return Err("dim".into());
    }
    if rows.is_empty() || rows.len() > usize::from(u16::MAX) {
        return Err("rows".into());
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"W3IW");
    out.push(1);
    out.push(dim);
    out.extend_from_slice(&u16::try_from(rows.len()).unwrap_or(0).to_le_bytes());
    for row in rows {
        if row.name.is_empty() || row.name.len() > 64 || !row.name.is_ascii() {
            return Err("name".into());
        }
        if row.arguments.len() > usize::from(u16::MAX) {
            return Err("arguments".into());
        }
        if row.weights.len() != usize::from(dim) {
            return Err("weights".into());
        }
        out.extend_from_slice(&row.bias.to_le_bytes());
        out.push(u8::try_from(row.name.len()).unwrap_or(0));
        out.extend_from_slice(row.name.as_bytes());
        out.extend_from_slice(
            &u16::try_from(row.arguments.len())
                .unwrap_or(0)
                .to_le_bytes(),
        );
        out.extend_from_slice(row.arguments.as_bytes());
        for weight in &row.weights {
            out.push(*weight as u8);
        }
    }
    Ok(out)
}

fn parse_weights(bytes: &[u8]) -> Result<Table, String> {
    if bytes.len() < 8 || &bytes[..4] != b"W3IW" {
        return Err("in-process weights are not a W3IW table".into());
    }
    if bytes[4] != 1 {
        return Err("unsupported in-process weight version".into());
    }
    let dim = usize::from(bytes[5]);
    if dim == 0 || dim > 32 {
        return Err("in-process weight dimension".into());
    }
    let count = u16::from_le_bytes([bytes[6], bytes[7]]);
    if count == 0 {
        return Err("in-process weight table is empty".into());
    }
    let mut cursor = 8usize;
    let mut rows = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        if cursor + 2 > bytes.len() {
            return Err("truncated in-process weights".into());
        }
        let bias = i16::from_le_bytes([bytes[cursor], bytes[cursor + 1]]);
        cursor += 2;
        let name_len = *bytes.get(cursor).ok_or("truncated in-process weights")? as usize;
        cursor += 1;
        if name_len == 0 || cursor + name_len > bytes.len() {
            return Err("truncated in-process weights".into());
        }
        let name = std::str::from_utf8(&bytes[cursor..cursor + name_len])
            .map_err(|_| "in-process weight name")?
            .to_string();
        cursor += name_len;
        if cursor + 2 > bytes.len() {
            return Err("truncated in-process weights".into());
        }
        let arg_len = usize::from(u16::from_le_bytes([bytes[cursor], bytes[cursor + 1]]));
        cursor += 2;
        if cursor + arg_len + dim > bytes.len() {
            return Err("truncated in-process weights".into());
        }
        let arguments = std::str::from_utf8(&bytes[cursor..cursor + arg_len])
            .map_err(|_| "in-process weight arguments")?
            .to_string();
        cursor += arg_len;
        let weights = bytes[cursor..cursor + dim]
            .iter()
            .map(|byte| *byte as i8)
            .collect();
        cursor += dim;
        rows.push(WeightRow {
            bias,
            name,
            arguments,
            weights,
        });
    }
    if cursor != bytes.len() {
        return Err("trailing bytes in in-process weights".into());
    }
    Ok(Table { dim, rows })
}

fn features(text: &str, dim: usize) -> Vec<i32> {
    let bytes = text.to_lowercase();
    let bytes = bytes.as_bytes();
    let mut values = vec![0i32; dim];
    for (index, &byte) in bytes.iter().enumerate() {
        values[usize::from(byte) % dim] += 1;
        if index > 0 {
            let mixed = usize::from(bytes[index - 1]).saturating_add(usize::from(byte));
            values[mixed % dim] += 1;
        }
    }
    values
}

fn score(table: &Table, utterance: &str) -> PlannerStep {
    let feat = features(utterance, table.dim);
    let mut best_index = 0usize;
    let mut best_score = i32::MIN;
    for (index, row) in table.rows.iter().enumerate() {
        let mut total = i32::from(row.bias);
        for (feature, weight) in feat.iter().zip(row.weights.iter()) {
            total += feature * i32::from(*weight);
        }
        if total > best_score {
            best_score = total;
            best_index = index;
        }
    }
    let row = &table.rows[best_index];
    if row.name == "final" {
        return PlannerStep::Final(row.arguments.clone());
    }
    let arguments = serde_json::from_str::<Value>(&row.arguments).unwrap_or_else(|_| json!({}));
    PlannerStep::Calls(vec![PlannedCall {
        id: "inproc-0".into(),
        name: row.name.clone(),
        arguments,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::ChatMessage;

    fn sample_rows() -> Vec<WeightRow> {
        vec![
            WeightRow {
                bias: 0,
                name: "propose_intent".into(),
                arguments: r#"{"utterance":"open files"}"#.into(),
                weights: vec![0, 0, 0, 0, 0, 0, 10, 0],
            },
            WeightRow {
                bias: 1,
                name: "final".into(),
                arguments: "No shell command matched this request.".into(),
                weights: vec![0, 0, 0, 0, 0, 0, 0, 0],
            },
        ]
    }

    #[test]
    fn verified_weights_score_in_process_and_a_bad_hash_is_dropped() {
        let bytes = encode_weights(8, &sample_rows()).unwrap();
        let sha = model::sha256_hex(&bytes);
        let backend = InProcessBackend::from_verified(&bytes, &sha).unwrap();
        let files = PlannerTurn {
            messages: vec![ChatMessage::user("please open the files application")],
        };
        match backend.complete_turn(&files).unwrap().step {
            PlannerStep::Calls(calls) => {
                assert_eq!(calls[0].name, "propose_intent");
                assert_eq!(calls[0].arguments["utterance"], "open files");
                assert_eq!(calls[0].id, "inproc-0");
            }
            PlannerStep::Final(text) => panic!("{text}"),
        }
        let other = PlannerTurn {
            messages: vec![ChatMessage::user("zzzz")],
        };
        match backend.complete_turn(&other).unwrap().step {
            PlannerStep::Final(text) => {
                assert!(text.contains("No shell command"));
            }
            PlannerStep::Calls(calls) => panic!("{calls:?}"),
        }
        let err =
            InProcessBackend::from_verified(&bytes, &model::sha256_hex(b"other")).unwrap_err();
        assert!(err.contains("checksum mismatch"), "{err}");
        let mut flipped = bytes.clone();
        flipped[0] ^= 0xff;
        assert!(InProcessBackend::from_verified(&flipped, &sha).is_err());
    }
}
