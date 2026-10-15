//! On-device pinyin engine.
//!
//! librime is not linked (no system package, and building it here is not
//! practical). This engine segments a spelling with a bundled dictionary and
//! returns at most nine candidates. The shell keeps the spelling; lookup is
//! stateless. L2 stays off: the manifest sets `uses_l2` to false.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::{
    EngineManifest, InputEngine, InputIntent, IntentPayload, Modality, Proposal, Scope, Session,
};

const DICT_SRC: &str = include_str!("../dict/pinyin.dict");
const CANDIDATE_CAP: usize = 9;

struct Dict {
    syllables: BTreeMap<String, String>,
    phrases: BTreeMap<String, Vec<(String, u32)>>,
}

fn dict() -> &'static Dict {
    static DICT: OnceLock<Dict> = OnceLock::new();
    DICT.get_or_init(|| {
        let mut syllables = BTreeMap::new();
        let mut phrases = BTreeMap::new();
        for line in DICT_SRC.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(body) = line.strip_prefix('=') {
                let mut parts = body.split_whitespace();
                let Some(key) = parts.next() else { continue };
                let mut tokens: Vec<&str> = parts.collect();
                if tokens.is_empty() {
                    continue;
                }
                let freq = tokens
                    .last()
                    .filter(|token| token.bytes().all(|byte| byte.is_ascii_digit()))
                    .and_then(|token| token.parse::<u32>().ok())
                    .map(|freq| {
                        tokens.pop();
                        freq
                    })
                    .unwrap_or_else(|| phrase_frequency(key, phrases.len()));
                let phrase: String = tokens.concat();
                if !phrase.is_empty() {
                    phrases
                        .entry(key.to_string())
                        .or_insert_with(Vec::new)
                        .push((phrase, freq));
                }
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(key) = parts.next() else { continue };
            let hanzi: String = parts.collect::<String>();
            if !hanzi.is_empty() {
                syllables.insert(key.to_string(), hanzi);
            }
        }
        Dict { syllables, phrases }
    })
}

/// Common words outrank file order. Anything else keeps the dictionary order.
fn phrase_frequency(key: &str, order: usize) -> u32 {
    let known = match key {
        "women" => 90_000,
        "shenme" => 86_000,
        "yige" => 84_000,
        "keyi" => 82_000,
        "meiyou" => 80_000,
        "xiexie" => 76_000,
        "jintian" => 74_000,
        "mingtian" => 72_000,
        "woaini" => 68_000,
        "zhidao" => 66_000,
        "xianzai" => 62_000,
        "pengyou" => 60_000,
        "dakai" => 58_000,
        "meiguanxi" => 56_000,
        "duibuqi" => 54_000,
        "zaijian" => 52_000,
        _ => return 30_000u32.saturating_sub(order as u32),
    };
    known
}

fn char_weight(index: usize) -> u32 {
    match index {
        0 => 100,
        1 => 10,
        _ => 1,
    }
}

fn normalize(raw: &str) -> String {
    raw.chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

enum Split<'a> {
    Full(Vec<&'a str>),
    Partial { done: Vec<&'a str>, tail: &'a str },
    Failed,
}

fn reconstruct<'a>(spelling: &'a str, prev: &[usize], mut end: usize) -> Vec<&'a str> {
    let mut parts = Vec::new();
    while end > 0 {
        let start = prev[end];
        if start >= end {
            break;
        }
        parts.push(&spelling[start..end]);
        end = start;
    }
    parts.reverse();
    parts
}

/// Longest syllable from the left, backtracking when that choice cannot finish.
/// An unfinished tail that is still a syllable prefix stays partial.
fn split<'a>(spelling: &'a str, dict: &Dict) -> Split<'a> {
    let n = spelling.len();
    let mut reachable = vec![false; n + 1];
    let mut prev = vec![0usize; n + 1];
    reachable[0] = true;
    for i in 0..n {
        if !reachable[i] {
            continue;
        }
        let rest = &spelling[i..];
        let max = rest.len().min(6);
        for len in (1..=max).rev() {
            let end = i + len;
            if reachable[end] {
                continue;
            }
            if dict.syllables.contains_key(&rest[..len]) {
                reachable[end] = true;
                prev[end] = i;
            }
        }
    }
    if reachable[n] {
        return Split::Full(reconstruct(spelling, &prev, n));
    }
    let mut best = None;
    for i in 0..n {
        if !reachable[i] {
            continue;
        }
        let tail = &spelling[i..];
        let prefix = dict
            .syllables
            .keys()
            .any(|key| key.starts_with(tail) && key.len() > tail.len());
        if prefix {
            best = Some(best.map(|kept: usize| kept.max(i)).unwrap_or(i));
        }
    }
    if let Some(i) = best {
        return Split::Partial {
            done: reconstruct(spelling, &prev, i),
            tail: &spelling[i..],
        };
    }
    Split::Failed
}

fn push_unique(out: &mut Vec<String>, text: String) {
    if text.is_empty() || out.iter().any(|existing| existing == &text) || out.len() >= CANDIDATE_CAP
    {
        return;
    }
    out.push(text);
}

fn primary(sylls: &[&str], dict: &Dict) -> String {
    sylls
        .iter()
        .filter_map(|syll| {
            dict.syllables
                .get(*syll)
                .and_then(|chars| chars.chars().next())
        })
        .collect()
}

fn push_scored(out: &mut Vec<(u32, String)>, score: u32, text: String) {
    if text.is_empty() {
        return;
    }
    if let Some(existing) = out.iter_mut().find(|(_, found)| found == &text) {
        if score > existing.0 {
            existing.0 = score;
        }
        return;
    }
    out.push((score, text));
}

fn syllable_lists<'a>(sylls: &[&str], dict: &'a Dict) -> Option<Vec<Vec<char>>> {
    let lists: Vec<Vec<char>> = sylls
        .iter()
        .map(|syll| {
            dict.syllables
                .get(*syll)
                .map(|chars| chars.chars().collect())
                .unwrap_or_default()
        })
        .collect();
    if lists.iter().any(Vec::is_empty) {
        None
    } else {
        Some(lists)
    }
}

/// Character combinations. A multi-syllable guess has to stay near the best
/// pairing, so a rare second character (你号, 你豪) does not crowd out words.
fn add_syllable_candidates(sylls: &[&str], dict: &Dict, out: &mut Vec<(u32, String)>) {
    let Some(lists) = syllable_lists(sylls, dict) else {
        return;
    };
    let primary_score = lists
        .iter()
        .fold(1u32, |score, _| score.saturating_mul(100));
    push_scored(out, primary_score, primary(sylls, dict));
    if lists.len() == 1 {
        for (index, ch) in lists[0].iter().enumerate().skip(1) {
            push_scored(out, char_weight(index), ch.to_string());
        }
        return;
    }
    for (pos, chars) in lists.iter().enumerate() {
        for (index, ch) in chars.iter().enumerate().take(3).skip(1) {
            let mut text = String::new();
            let mut score = 1u32;
            for (other, list) in lists.iter().enumerate() {
                let (weight_index, chosen) = if other == pos {
                    (index, *ch)
                } else {
                    (0, list[0])
                };
                text.push(chosen);
                score = score.saturating_mul(char_weight(weight_index));
            }
            // 40% of the all-common pairing. 100×10 is only 10%, so it drops.
            if score.saturating_mul(5) >= primary_score.saturating_mul(2) {
                push_scored(out, score, text);
            }
        }
    }
}

/// Candidates for one spelling, most likely first. At most nine.
/// An unsplittable tail falls back to the raw spelling.
pub fn candidates(spelling: &str) -> Vec<String> {
    let spelling = normalize(spelling);
    if spelling.is_empty() {
        return Vec::new();
    }
    let dict = dict();
    let mut scored = Vec::new();
    if let Some(phrases) = dict.phrases.get(&spelling) {
        for (phrase, freq) in phrases {
            push_scored(&mut scored, *freq, phrase.clone());
        }
    }
    let mut longer = Vec::new();
    for (key, phrases) in &dict.phrases {
        if key.starts_with(&spelling) && key != &spelling {
            for (phrase, freq) in phrases {
                longer.push((*freq, phrase.clone()));
            }
        }
    }
    longer.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    for (freq, phrase) in longer {
        push_scored(&mut scored, freq, phrase);
    }
    let phrase_hit = !scored.is_empty();
    match split(&spelling, dict) {
        Split::Full(sylls) => {
            if !phrase_hit {
                add_syllable_candidates(&sylls, dict, &mut scored);
            }
        }
        Split::Partial { done, tail } => {
            for (syll, _) in dict.syllables.range(tail.to_string()..) {
                if !syll.starts_with(tail) {
                    break;
                }
                let mut full = done.clone();
                full.push(syll.as_str());
                // Stay under phrase scores so 你好 ranks ahead of a bare 你还.
                push_scored(&mut scored, 9_000, primary(&full, dict));
            }
        }
        Split::Failed => push_scored(&mut scored, 1, spelling.clone()),
    }
    if scored.is_empty() {
        return vec![spelling];
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut out = Vec::new();
    for (_, text) in scored {
        push_unique(&mut out, text);
    }
    out
}

/// Spaced syllables for the composition display. `nihao` is `ni hao`.
pub fn preedit_display(spelling: &str) -> String {
    let spelling = normalize(spelling);
    if spelling.is_empty() {
        return String::new();
    }
    match split(&spelling, dict()) {
        Split::Full(sylls) => sylls.join(" "),
        Split::Partial { done, tail } => {
            if done.is_empty() {
                tail.to_string()
            } else {
                format!("{} {tail}", done.join(" "))
            }
        }
        Split::Failed => spelling,
    }
}

/// Full syllable split, used by tests. Incomplete or illegal tails return none.
pub fn segment(spelling: &str) -> Option<Vec<String>> {
    let spelling = normalize(spelling);
    match split(&spelling, dict()) {
        Split::Full(sylls) => Some(sylls.into_iter().map(str::to_string).collect()),
        _ => None,
    }
}

#[derive(Clone, Debug, Default)]
pub struct PinyinEngine;

impl PinyinEngine {
    pub fn new() -> Self {
        Self
    }

    pub fn candidates(spelling: &str) -> Vec<String> {
        candidates(spelling)
    }
}

impl InputEngine for PinyinEngine {
    fn manifest(&self) -> EngineManifest {
        EngineManifest {
            id: "w3cos.pinyin".into(),
            modalities: vec![Modality::Key],
            locales: vec!["zh-Hans".into(), "zh-CN".into()],
            scopes: vec![Scope::Purpose],
            network: false,
            uses_l2: false,
        }
    }

    fn on_intent(&mut self, _session: &Session, intent: &InputIntent) -> Vec<Proposal> {
        let IntentPayload::Text { text } = &intent.payload else {
            return Vec::new();
        };
        let spelling = normalize(text);
        if spelling.is_empty() {
            return Vec::new();
        }
        let shown = preedit_display(&spelling);
        let mut out = vec![Proposal::Preedit {
            text: shown.clone(),
            cursor: shown.chars().count(),
        }];
        for (index, text) in candidates(&spelling).into_iter().enumerate() {
            out.push(Proposal::candidate(
                format!("pinyin-{index}"),
                text,
                1.0 - index as f32 * 0.05,
                "L0",
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ImeHub, InputIntent, Modality, Proposal, Purpose};

    #[test]
    fn nihao_prefers_the_phrase() {
        let found = candidates("nihao");
        assert_eq!(found.first().map(String::as_str), Some("你好"));
        assert!(found.len() <= CANDIDATE_CAP);
        assert!(found.iter().any(|item| item == "你好吗"));
        assert!(found.iter().any(|item| item == "您好"));
        assert!(
            !found
                .iter()
                .take(3)
                .any(|item| item == "你号" || item == "你豪")
        );
    }

    #[test]
    fn preedit_shows_the_syllable_split() {
        assert_eq!(preedit_display("nihao"), "ni hao");
        assert_eq!(preedit_display("NiHao"), "ni hao");
        assert_eq!(preedit_display("nih"), "ni h");
        assert_eq!(preedit_display("zhongguo"), "zhong guo");
        assert_eq!(preedit_display("nihaoqx"), "nihaoqx");
        assert_eq!(preedit_display(""), "");
    }

    #[test]
    fn zhongguo_and_women_segment() {
        assert_eq!(
            segment("zhongguo"),
            Some(vec!["zhong".to_string(), "guo".to_string()])
        );
        assert_eq!(
            candidates("zhongguo").first().map(String::as_str),
            Some("中国")
        );
        assert_eq!(
            segment("women"),
            Some(vec!["wo".to_string(), "men".to_string()])
        );
        assert_eq!(candidates("women")[0], "我们");
    }

    #[test]
    fn incomplete_tail_expands_to_the_phrase() {
        let found = candidates("nih");
        assert_eq!(found.first().map(String::as_str), Some("你好"));
        assert!(found.iter().any(|item| item == "你好吗"));
    }

    #[test]
    fn unsplittable_tail_falls_back_to_the_raw_spelling() {
        assert!(segment("nihaoqx").is_none());
        assert_eq!(candidates("nihaoqx"), vec!["nihaoqx".to_string()]);
    }

    #[test]
    fn every_phrase_segments() {
        for key in dict().phrases.keys() {
            assert!(segment(key).is_some(), "{key}");
        }
    }

    #[test]
    fn hub_returns_nihao_and_secure_sessions_skip_the_engine() {
        let mut engine = PinyinEngine::new();
        assert!(!engine.manifest().uses_l2);
        assert!(!engine.manifest().network);
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("field", Purpose::Text, vec!["zh-Hans".into()]);
        let intent = InputIntent::text(Modality::Key, "NiHao", &["zh-Hans"]);
        let proposals = hub.handle_intent(&session, &intent, &mut [&mut engine]);
        assert!(proposals.iter().any(|proposal| matches!(
            proposal,
            Proposal::Preedit { text, .. } if text == "ni hao"
        )));
        assert!(proposals.iter().any(|proposal| matches!(
            proposal,
            Proposal::Candidate { text, source, .. } if text == "你好" && source == "L0"
        )));
        let secure = hub.begin_session("lock", Purpose::Password, vec!["zh-Hans".into()]);
        assert!(
            hub.handle_intent(&secure, &intent, &mut [&mut engine])
                .is_empty()
        );
        let hidden = hub.begin_session("pin", Purpose::Secure, vec!["zh-Hans".into()]);
        assert!(
            hub.handle_intent(&hidden, &intent, &mut [&mut engine])
                .is_empty()
        );
    }
}
