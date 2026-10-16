//! Local personal lexicon.
//!
//! `W3P1` is ChaCha20-Poly1305. The key is PBKDF2-HMAC-SHA256 of the unlock
//! passphrase, 100_000 iterations, the same stretch the identity verifier uses.
//! The file holds words, phrases, frequencies, and entity aliases. It does not
//! hold key streams, audio, secure-field text, or documents. Sync is not part
//! of this format.

use std::num::NonZeroU32;

use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use ring::pbkdf2;

use crate::pinyin::candidates;

const MAGIC: &[u8; 4] = b"W3P1";
const PBKDF2_ITERATIONS: u32 = 100_000;
const SALT_LEN: usize = 16;
const KEY_LEN: usize = 32;
const RANK_BASE: i64 = 100_000;
const RANK_STEP: i64 = 1_000;
/// One accept is one dictionary slot. A single accept cannot pass the head,
/// and three accepts can lift a word that the dictionary placed two slots down.
const FREQ_WEIGHT: i64 = RANK_STEP;
const NEG_WEIGHT: i64 = 200;
/// A brand-new word sits under the head after one accept and can pass it on the second.
const NEW_WORD_GAP: i64 = 1_500;
const TEXT_CAP: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Word,
    Phrase,
    Alias,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    pub text: String,
    /// Spelling for a word or phrase. Target for an alias.
    pub extra: String,
    pub freq: u32,
    pub neg: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lexicon {
    pub stealth: bool,
    pub stealth_apps: Vec<String>,
    entries: Vec<Entry>,
}

impl Lexicon {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn clear_entries(&mut self) {
        self.entries.clear();
    }

    pub fn set_stealth(&mut self, on: bool) {
        self.stealth = on;
    }

    pub fn toggle_app(&mut self, app: &str) {
        if let Some(index) = self.stealth_apps.iter().position(|item| item == app) {
            self.stealth_apps.remove(index);
        } else if !app.is_empty() {
            self.stealth_apps.push(app.to_string());
        }
    }

    pub fn app_hidden(&self, app: &str) -> bool {
        self.stealth || self.stealth_apps.iter().any(|item| item == app)
    }

    pub fn accept(&mut self, spelling: &str, text: &str) -> bool {
        let Some((kind, text, spelling)) = learnable(spelling, text) else {
            return false;
        };
        if let Some(entry) = self.entries.iter_mut().find(|entry| {
            entry.kind != EntryKind::Alias && entry.text == text && entry.extra == spelling
        }) {
            entry.freq = entry.freq.saturating_add(1);
            return true;
        }
        self.entries.push(Entry {
            kind,
            text,
            extra: spelling,
            freq: 1,
            neg: 0,
        });
        true
    }

    /// Remember a handwriting pick. `extra` is `"*"` so it never matches a
    /// pinyin spelling. Showing candidates does not call this.
    pub fn accept_surface(&mut self, text: &str) -> bool {
        let text = text.trim();
        if !short_text(text) {
            return false;
        }
        let extra = "*";
        if let Some(entry) = self.entries.iter_mut().find(|entry| {
            entry.kind != EntryKind::Alias && entry.text == text && entry.extra == extra
        }) {
            entry.freq = entry.freq.saturating_add(1);
            return true;
        }
        let kind = if text.chars().count() >= 3 {
            EntryKind::Phrase
        } else {
            EntryKind::Word
        };
        self.entries.push(Entry {
            kind,
            text: text.to_string(),
            extra: extra.to_string(),
            freq: 1,
            neg: 0,
        });
        true
    }

    pub fn reject(&mut self, spelling: &str, text: &str) {
        let Some((_, text, spelling)) = learnable(spelling, text) else {
            return;
        };
        if let Some(entry) = self.entries.iter_mut().find(|entry| {
            entry.kind != EntryKind::Alias && entry.text == text && entry.extra == spelling
        }) {
            entry.neg = entry.neg.saturating_add(1);
            return;
        }
        self.entries.push(Entry {
            kind: EntryKind::Word,
            text,
            extra: spelling,
            freq: 0,
            neg: 1,
        });
    }

    pub fn upsert_alias(&mut self, alias: &str, target: &str) -> bool {
        let alias = alias.trim();
        let target = target.trim();
        if !short_text(alias) || !short_text(target) || alias == target {
            return false;
        }
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.kind == EntryKind::Alias && entry.text == alias)
        {
            entry.extra = target.to_string();
            entry.freq = entry.freq.saturating_add(1);
            return true;
        }
        self.entries.push(Entry {
            kind: EntryKind::Alias,
            text: alias.to_string(),
            extra: target.to_string(),
            freq: 1,
            neg: 0,
        });
        true
    }

    pub fn remove_text(&mut self, text: &str) -> bool {
        let text = text.trim();
        let before = self.entries.len();
        self.entries.retain(|entry| entry.text != text);
        self.entries.len() != before
    }

    pub fn encode(&self) -> String {
        let mut lines = vec![format!("S {}", u8::from(self.stealth))];
        for app in &self.stealth_apps {
            if !app.is_empty() && !app.contains([' ', '\n', '\t']) {
                lines.push(format!("A {app}"));
            }
        }
        for entry in &self.entries {
            let kind = match entry.kind {
                EntryKind::Word => "W",
                EntryKind::Phrase => "P",
                EntryKind::Alias => "E",
            };
            if entry.text.contains([' ', '\n', '\t']) || entry.extra.contains([' ', '\n', '\t']) {
                continue;
            }
            lines.push(format!(
                "{kind} {} {} {} {}",
                entry.extra, entry.text, entry.freq, entry.neg
            ));
        }
        lines.join("\n")
    }

    pub fn decode(text: &str) -> Option<Self> {
        let mut lexicon = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split_whitespace();
            match parts.next()? {
                "S" => lexicon.stealth = parts.next()? == "1",
                "A" => {
                    let app = parts.next()?.to_string();
                    if !lexicon.stealth_apps.contains(&app) {
                        lexicon.stealth_apps.push(app);
                    }
                }
                "W" | "P" | "E" => {
                    let kind = match &line[..1] {
                        "P" => EntryKind::Phrase,
                        "E" => EntryKind::Alias,
                        _ => EntryKind::Word,
                    };
                    let extra = parts.next()?.to_string();
                    let text = parts.next()?.to_string();
                    let freq: u32 = parts.next()?.parse().ok()?;
                    let neg: u32 = parts.next()?.parse().ok()?;
                    lexicon.entries.push(Entry {
                        kind,
                        text,
                        extra,
                        freq,
                        neg,
                    });
                }
                _ => return None,
            }
        }
        Some(lexicon)
    }

    pub fn export_json(&self) -> String {
        let mut words = Vec::new();
        let mut aliases = Vec::new();
        for entry in &self.entries {
            if entry.kind == EntryKind::Alias {
                aliases.push(format!(
                    "{{\"alias\":\"{}\",\"target\":\"{}\"}}",
                    json_escape(&entry.text),
                    json_escape(&entry.extra)
                ));
            } else {
                words.push(format!(
                    "{{\"text\":\"{}\",\"spelling\":\"{}\",\"freq\":{},\"neg\":{}}}",
                    json_escape(&entry.text),
                    json_escape(&entry.extra),
                    entry.freq,
                    entry.neg
                ));
            }
        }
        format!(
            "{{\"words\":[{}],\"aliases\":[{}]}}",
            words.join(","),
            aliases.join(",")
        )
    }
}

/// Secure sessions neither read nor write. Returns whether the store changed.
pub fn learn(
    lexicon: &mut Lexicon,
    secure: bool,
    spelling: &str,
    chosen: &str,
    skipped: &[String],
) -> bool {
    if secure {
        return false;
    }
    let mut changed = false;
    for skipped in skipped {
        let before = lexicon.entries.len();
        lexicon.reject(spelling, skipped);
        changed |= lexicon.entries.len() != before
            || lexicon
                .entries
                .iter()
                .any(|entry| entry.text == skipped.trim() && entry.neg > 0);
    }
    changed |= lexicon.accept(spelling, chosen);
    changed
}

/// Empty lexicon keeps the dictionary order. Personal frequency is a bonus on
/// top of that order, so a first-time store does not reorder L0.
pub fn rank_candidates(spelling: &str, lexicon: &Lexicon) -> Vec<String> {
    let base = candidates(spelling);
    if !lexicon.relevant(&base, spelling) {
        return base;
    }
    let mut scored: Vec<(i64, usize, String)> = base
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let bonus = lexicon.bonus(spelling, text);
            (
                RANK_BASE - index as i64 * RANK_STEP + bonus,
                index,
                text.clone(),
            )
        })
        .collect();
    for entry in lexicon
        .entries
        .iter()
        .filter(|entry| entry.kind != EntryKind::Alias && entry.extra == spelling && entry.freq > 0)
    {
        if base.iter().any(|text| text == &entry.text) {
            continue;
        }
        let bonus = entry.freq as i64 * FREQ_WEIGHT - entry.neg as i64 * NEG_WEIGHT;
        scored.push((
            RANK_BASE - NEW_WORD_GAP + bonus,
            usize::MAX,
            entry.text.clone(),
        ));
    }
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    let mut out: Vec<String> = scored.into_iter().map(|(_, _, text)| text).collect();
    let mut inserts = Vec::new();
    for entry in lexicon
        .entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::Alias && entry.freq > 0)
    {
        if let Some(index) = out.iter().position(|text| text == &entry.text)
            && !out.iter().any(|text| text == &entry.extra)
        {
            inserts.push((index + 1, entry.extra.clone()));
        }
    }
    for (offset, (index, target)) in inserts.into_iter().enumerate() {
        let at = (index + offset).min(out.len());
        out.insert(at, target);
    }
    out.truncate(9);
    out
}

impl Lexicon {
    fn relevant(&self, base: &[String], spelling: &str) -> bool {
        self.entries.iter().any(|entry| match entry.kind {
            EntryKind::Alias => base.iter().any(|text| text == &entry.text) && entry.freq > 0,
            _ => entry.extra == spelling && (entry.freq > 0 || entry.neg > 0),
        })
    }

    fn bonus(&self, spelling: &str, text: &str) -> i64 {
        self.entries
            .iter()
            .find(|entry| {
                entry.kind != EntryKind::Alias && entry.extra == spelling && entry.text == text
            })
            .map(|entry| entry.freq as i64 * FREQ_WEIGHT - entry.neg as i64 * NEG_WEIGHT)
            .unwrap_or(0)
    }
}

pub fn derive_lexicon_key(passphrase: &str, salt: &[u8; SALT_LEN]) -> Option<[u8; KEY_LEN]> {
    if passphrase.is_empty() {
        return None;
    }
    Some(derive_key(passphrase, salt))
}

pub fn seal_with_key(
    lexicon: &Lexicon,
    key: &[u8; KEY_LEN],
    salt: &[u8; SALT_LEN],
) -> Option<Vec<u8>> {
    let mut nonce_bytes = [0u8; 12];
    getrandom::getrandom(&mut nonce_bytes).ok()?;
    let unbound = UnboundKey::new(&CHACHA20_POLY1305, key).ok()?;
    let aead = LessSafeKey::new(unbound);
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);
    let mut body = lexicon.encode().into_bytes();
    aead.seal_in_place_append_tag(nonce, Aad::from(MAGIC), &mut body)
        .ok()?;
    let mut out = Vec::with_capacity(4 + SALT_LEN + 4 + 12 + body.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(salt);
    out.extend_from_slice(&PBKDF2_ITERATIONS.to_be_bytes());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&body);
    Some(out)
}

pub fn seal_lexicon(lexicon: &Lexicon, passphrase: &str, salt: &[u8; SALT_LEN]) -> Option<Vec<u8>> {
    let key = derive_lexicon_key(passphrase, salt)?;
    seal_with_key(lexicon, &key, salt)
}

pub fn open_lexicon(
    bytes: &[u8],
    passphrase: &str,
) -> Option<(Lexicon, [u8; KEY_LEN], [u8; SALT_LEN])> {
    if passphrase.is_empty() || bytes.len() < 4 + SALT_LEN + 4 + 12 + 16 {
        return None;
    }
    if &bytes[..4] != MAGIC {
        return None;
    }
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&bytes[4..4 + SALT_LEN]);
    let iterations = u32::from_be_bytes(bytes[4 + SALT_LEN..4 + SALT_LEN + 4].try_into().ok()?);
    if iterations != PBKDF2_ITERATIONS {
        return None;
    }
    let nonce_at = 4 + SALT_LEN + 4;
    let mut nonce_bytes = [0u8; 12];
    nonce_bytes.copy_from_slice(&bytes[nonce_at..nonce_at + 12]);
    let key = derive_key(passphrase, &salt);
    let unbound = UnboundKey::new(&CHACHA20_POLY1305, &key).ok()?;
    let aead = LessSafeKey::new(unbound);
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);
    let mut body = bytes[nonce_at + 12..].to_vec();
    let plain = aead
        .open_in_place(nonce, Aad::from(MAGIC), &mut body)
        .ok()?;
    let text = std::str::from_utf8(plain).ok()?;
    Some((Lexicon::decode(text)?, key, salt))
}

fn derive_key(passphrase: &str, salt: &[u8]) -> [u8; KEY_LEN] {
    let mut out = [0u8; KEY_LEN];
    let iterations = NonZeroU32::new(PBKDF2_ITERATIONS).expect("iterations");
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        salt,
        passphrase.as_bytes(),
        &mut out,
    );
    out
}

fn learnable(spelling: &str, text: &str) -> Option<(EntryKind, String, String)> {
    let spelling = spelling.trim().to_string();
    let text = text.trim().to_string();
    if spelling.is_empty() || !short_text(&text) || text == spelling {
        return None;
    }
    if spelling.chars().any(|ch| !ch.is_ascii_alphanumeric()) || spelling.len() > 48 {
        return None;
    }
    let kind = if text.chars().count() >= 3 {
        EntryKind::Phrase
    } else {
        EntryKind::Word
    };
    Some((kind, text, spelling))
}

fn short_text(text: &str) -> bool {
    let count = text.chars().count();
    (1..=TEXT_CAP).contains(&count) && !text.chars().any(|ch| ch.is_control() || ch.is_whitespace())
}

fn json_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn w3p1_roundtrip_hides_the_word() {
        let mut lexicon = Lexicon::default();
        assert!(lexicon.accept("nihao", "词库甲"));
        lexicon.upsert_alias("老王", "王明");
        let mut salt = [7u8; 16];
        salt[0] = 9;
        let sealed = seal_lexicon(&lexicon, "stub-passkey", &salt).expect("seal");
        assert!(sealed.starts_with(b"W3P1"));
        let word = "词库甲".as_bytes();
        assert!(!sealed.windows(word.len()).any(|window| window == word));
        assert!(!String::from_utf8_lossy(&sealed).contains("stub-passkey"));
        let (opened, _key, opened_salt) = open_lexicon(&sealed, "stub-passkey").expect("open");
        assert_eq!(opened_salt, salt);
        assert_eq!(opened.entries().len(), 2);
        assert!(
            opened
                .entries()
                .iter()
                .any(|entry| entry.text == "词库甲" && entry.freq == 1)
        );
        assert!(opened.entries().iter().any(|entry| {
            entry.kind == EntryKind::Alias && entry.text == "老王" && entry.extra == "王明"
        }));
        assert!(open_lexicon(&sealed, "other-pass").is_none());
        let encoded = opened.encode();
        assert!(!encoded.contains("audio"));
        assert!(!encoded.contains("keydown"));
    }

    #[test]
    fn empty_store_matches_the_dictionary_order() {
        let lexicon = Lexicon::default();
        for spelling in ["nihao", "zhongguo", "wo"] {
            assert_eq!(rank_candidates(spelling, &lexicon), candidates(spelling));
        }
    }

    #[test]
    fn a_few_accepts_raise_a_personal_word_and_one_does_not() {
        let mut lexicon = Lexicon::default();
        assert!(learn(
            &mut lexicon,
            false,
            "nihao",
            "您好",
            &["你好".to_string()]
        ));
        let once = rank_candidates("nihao", &lexicon);
        assert_eq!(once.first().map(String::as_str), Some("你好"));
        assert!(once.iter().any(|item| item == "您好"));
        learn(&mut lexicon, false, "nihao", "您好", &[]);
        learn(&mut lexicon, false, "nihao", "您好", &[]);
        let raised = rank_candidates("nihao", &lexicon);
        assert_eq!(raised.first().map(String::as_str), Some("您好"));
        let freq = lexicon
            .entries()
            .iter()
            .find(|entry| entry.text == "您好")
            .map(|entry| entry.freq);
        assert_eq!(freq, Some(3));
    }

    #[test]
    fn secure_sessions_do_not_read_or_write() {
        let mut lexicon = Lexicon::default();
        assert!(lexicon.accept("nihao", "您好"));
        let before = lexicon.clone();
        assert!(!learn(
            &mut lexicon,
            true,
            "mima",
            "密码词",
            &["另一词".to_string()]
        ));
        assert_eq!(lexicon, before);
        assert!(!lexicon.entries().iter().any(|entry| entry.text == "密码词"));
        assert_eq!(
            rank_candidates("nihao", &lexicon)
                .first()
                .map(String::as_str),
            Some("你好")
        );
    }

    #[test]
    fn a_picked_ink_word_stays_out_of_pinyin_order() {
        let mut lexicon = Lexicon::default();
        let before = rank_candidates("nihao", &lexicon);
        assert!(!lexicon.accept_surface("  "));
        assert!(!lexicon.accept_surface("你 好"));
        assert!(lexicon.accept_surface("永"));
        assert_eq!(rank_candidates("nihao", &lexicon), before);
        assert!(lexicon.accept_surface("永"));
        let entry = lexicon
            .entries()
            .iter()
            .find(|entry| entry.text == "永")
            .expect("picked");
        assert_eq!(entry.extra, "*");
        assert_eq!(entry.freq, 2);
        assert_eq!(rank_candidates("nihao", &lexicon), before);
    }
}
