//! Line break opportunities (CSS Text 3 section 5), shared by line layout
//! and paint so both wrap a run at the same positions.
//!
//! Behaviour follows Blink's LazyLineBreakIterator
//! (third_party/blink/renderer/platform/text/text_break_iterator.h):
//! LineBreakType (normal / break-all / keep-all / break-character),
//! BreakSpaceType (after a space run / after every space), a fast pair
//! table for U+0021..U+00FF, and the Unicode line breaking algorithm
//! (UAX #14) for everything else.  The iterator itself is written for this
//! engine; Blink's text_break_iterator.cc is LGPL and is not copied.
//!
//! The fast pair table is ported from Blink's LineBreakData
//! (third_party/blink/renderer/platform/text/
//! character_property_data_generator.cc):
//!
//!   Copyright 2016 The Chromium Authors
//!   Use of this source code is governed by a BSD-style license that can be
//!   found in the LICENSE file.
//!
//! Blink fills that table from ICU4C's line BreakIterator; here the pairs and
//! the UAX #14 rules come from ICU4X's `icu_segmenter` (Unicode License),
//! which implements the same rules and the CSS `line-break` / `word-break`
//! tailorings.

use icu_properties::CodePointMapData;
use icu_properties::props::{GeneralCategory, GeneralCategoryGroup, LineBreak};
use icu_segmenter::options::{LineBreakOptions, LineBreakStrictness, LineBreakWordOption};
use icu_segmenter::{GraphemeClusterSegmenter, LineSegmenter};
use std::sync::OnceLock;
use w3cos_std::style::{LineBreak as CssLineBreak, Style, WordBreak};

/// Blink LineBreakType.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum LineBreakType {
    Normal,
    /// `word-break: break-all`: breaks between letters and numbers too.
    BreakAll,
    /// `line-break: anywhere`: every grapheme cluster boundary.
    BreakCharacter,
    /// `word-break: keep-all`: no breaks between letters / ideographs.
    KeepAll,
}

/// Blink LineBreakStrictness (CSS `line-break`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Strictness {
    /// `line-break: auto`: the content language's default.
    Default,
    Normal,
    Strict,
    Loose,
}

/// Blink BreakSpaceType.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BreakSpace {
    /// Break after a run of white space (the UAX #14 / ICU behaviour).
    AfterSpaceRun,
    /// `white-space: break-spaces`: after every preserved space.
    AfterEverySpace,
}

/// How a run's text may wrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct BreakMode {
    pub kind: LineBreakType,
    pub strictness: Strictness,
    pub break_space: BreakSpace,
    /// The content language is Chinese or Japanese (the CSS `line-break`
    /// values only differ for CJ text).
    pub cj: bool,
    /// The content language is Japanese.
    pub ja: bool,
    /// `overflow-wrap: anywhere / break-word` or `word-break: break-word`:
    /// an otherwise unbreakable run may break at any grapheme boundary when
    /// it would overflow (NGLineBreaker's break_anywhere_if_overflow).
    pub break_anywhere_if_overflow: bool,
    /// LazyLineBreakIterator's disable_soft_hyphen: no opportunity after
    /// U+00AD.  Set for layout because no hyphen glyph is painted at such a
    /// break yet (and `hyphens: none` disables them).
    pub disable_soft_hyphen: bool,
    /// `hanging-punctuation: last`: a closing bracket or quote ending the
    /// text hangs instead of moving to a new line.
    pub hang_last_punctuation: bool,
}

impl Default for BreakMode {
    fn default() -> Self {
        Self {
            kind: LineBreakType::Normal,
            strictness: Strictness::Default,
            break_space: BreakSpace::AfterSpaceRun,
            cj: false,
            ja: false,
            break_anywhere_if_overflow: false,
            disable_soft_hyphen: false,
            hang_last_punctuation: false,
        }
    }
}

impl BreakMode {
    /// ComputedStyle::GetLineBreakType and the iterator setup of
    /// NGLineBreaker for a text run of `style`.
    pub fn of(style: &Style) -> Self {
        let lang = style
            .custom_properties
            .as_ref()
            .and_then(|p| p.get("--w3cos-internal-text-language"))
            .map(|v| v.trim().to_ascii_lowercase())
            .unwrap_or_default();
        let is = |l: &str| lang == l || lang.starts_with(&format!("{l}-"));
        let (ja, cj) = (is("ja"), is("ja") || is("zh"));
        // ComputedStyle::GetLineBreakType: line-break: anywhere wins over
        // word-break.
        let kind = match (style.line_break, style.word_break) {
            (CssLineBreak::Anywhere, _) => LineBreakType::BreakCharacter,
            (_, WordBreak::BreakAll) => LineBreakType::BreakAll,
            (_, WordBreak::KeepAll) => LineBreakType::KeepAll,
            (_, WordBreak::Normal | WordBreak::BreakWord) => LineBreakType::Normal,
        };
        let strictness = match style.line_break {
            CssLineBreak::Auto | CssLineBreak::Anywhere => Strictness::Default,
            CssLineBreak::Loose => Strictness::Loose,
            CssLineBreak::Normal => Strictness::Normal,
            CssLineBreak::Strict => Strictness::Strict,
        };
        Self {
            kind,
            strictness,
            cj,
            ja,
            break_anywhere_if_overflow: style.word_break == WordBreak::BreakWord,
            break_space: if style.break_spaces {
                BreakSpace::AfterEverySpace
            } else {
                BreakSpace::AfterSpaceRun
            },
            disable_soft_hyphen: true,
            hang_last_punctuation: style.hanging_punctuation_last,
        }
    }

    fn segmenter_options(&self) -> LineBreakOptions<'static> {
        static JA: icu_locale_core::LanguageIdentifier = icu_locale_core::langid!("ja");
        let mut options = LineBreakOptions::default();
        options.strictness = Some(if !self.cj {
            // LayoutLocale::LocaleWithBreakKeyword: without a content
            // language no `@lb` keyword is set, so `line-break` has no
            // effect and the root rules (CJ as ID) apply.
            LineBreakStrictness::Normal
        } else {
            match self.strictness {
                // ICU's ja / zh locales default to the normal tailoring.
                Strictness::Default | Strictness::Normal => LineBreakStrictness::Normal,
                Strictness::Strict => LineBreakStrictness::Strict,
                Strictness::Loose => LineBreakStrictness::Loose,
            }
        });
        options.word_option = Some(match self.kind {
            LineBreakType::BreakAll => LineBreakWordOption::BreakAll,
            // keep-all is applied on top of the normal opportunities
            // (see `keep_all_prohibits`), as Blink does.
            _ => LineBreakWordOption::Normal,
        });
        options.content_locale = self.cj.then_some(&JA);
        options
    }

    /// Whether U+301C / U+30A0 may not start a line in CJ text: Chrome's
    /// ja rules keep them for `line-break: auto` and `strict`, its zh rules
    /// and the explicit `normal` / `loose` tailorings break before them.
    fn cj_dash_no_break_before(&self) -> bool {
        self.strictness == Strictness::Strict || (self.ja && self.strictness == Strictness::Default)
    }
}

fn line_break_class(c: char) -> LineBreak {
    CodePointMapData::<LineBreak>::new().get(c)
}

/// ShouldBreakAfterBreakAll: the additional opportunities of
/// `word-break: break-all` between letters, numbers and hyphens (a
/// break-after character still may not start a line).
fn break_all_class(lb: LineBreak) -> bool {
    matches!(
        lb,
        LineBreak::Ambiguous
            | LineBreak::Alphabetic
            | LineBreak::BreakAfter
            | LineBreak::Hyphen
            | LineBreak::Numeric
            | LineBreak::ComplexContext
            | LineBreak::HebrewLetter
    )
}

/// `word-break: keep-all`: no opportunities between letters and numbers
/// (general category L or N), including ideographs.
fn keep_all_letter(c: char) -> bool {
    let gc = CodePointMapData::<GeneralCategory>::new().get(c);
    GeneralCategoryGroup::Letter.contains(gc) || GeneralCategoryGroup::Number.contains(gc)
}

/// U+301C WAVE DASH and U+30A0 KATAKANA-HIRAGANA DOUBLE HYPHEN: Chrome's
/// root rules break around them like ideographs (see
/// `cj_dash_no_break_before` for ja / zh).
fn is_cj_dash(c: char) -> bool {
    matches!(c, '\u{301C}' | '\u{30A0}')
}

/// LazyLineBreakIterator::IsBreakableSpace.
fn is_breakable_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n')
}

/// Zs other than U+0020 and U+00A0.
fn is_other_space_separator(c: char) -> bool {
    // U+2007 FIGURE SPACE and U+202F NARROW NO-BREAK SPACE are GL: they
    // never offer a break, break-spaces or not.
    matches!(
        c,
        '\u{1680}' | '\u{2000}'..='\u{2006}' | '\u{2008}'..='\u{200A}' | '\u{205F}' | '\u{3000}'
    )
}

const FAST_MIN: u32 = 0x21;
const FAST_MAX: u32 = 0xFF;
const FAST_N: usize = (FAST_MAX - FAST_MIN + 1) as usize;

type FastTable = [[bool; FAST_N]; FAST_N];

/// LineBreakData: the breakability between two characters of
/// U+0021..U+00FF (`true`: a break is allowed between them).
fn fast_table() -> &'static FastTable {
    static TABLE: OnceLock<Box<FastTable>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut pair: Box<FastTable> = Box::new([[false; FAST_N]; FAST_N]);
        // FillFromIcu: the default ("en") line break iterator.
        let segmenter = LineSegmenter::new_for_non_complex_scripts(LineBreakOptions::default());
        let mut buf = String::with_capacity(4);
        for a in FAST_MIN..=FAST_MAX {
            for b in FAST_MIN..=FAST_MAX {
                buf.clear();
                buf.push(char::from_u32(a).unwrap());
                let split = buf.len();
                buf.push(char::from_u32(b).unwrap());
                pair[(a - FAST_MIN) as usize][(b - FAST_MIN) as usize] =
                    segmenter.segment_str(&buf).any(|i| i == split);
            }
        }
        // FillAscii: printable ASCII pairs (compatible with other engines).
        let mut set = |a: (char, char), b: (char, char), v: bool| {
            for x in a.0 as u32..=a.1 as u32 {
                for y in b.0 as u32..=b.1 as u32 {
                    pair[(x - FAST_MIN) as usize][(y - FAST_MIN) as usize] = v;
                }
            }
        };
        let all = ('!', '\u{7F}');
        let one = |c: char| (c, c);
        set(all, all, false);
        set(all, one('('), true);
        set(all, one('<'), true);
        set(all, one('['), true);
        set(all, one('{'), true);
        set(one('-'), all, true);
        set(one('?'), all, true);
        set(one('-'), one('$'), false);
        set(all, one('!'), false);
        set(one('?'), one('"'), false);
        set(one('?'), one('\''), false);
        set(all, one(')'), false);
        set(all, one(','), false);
        set(all, one('.'), false);
        set(all, one('/'), false);
        // Between '-' and a digit depends on context (should_break_fast).
        set(one('-'), ('0', '9'), false);
        set(all, one(':'), false);
        set(all, one(';'), false);
        set(all, one('?'), false);
        set(all, one(']'), false);
        set(all, one('}'), false);
        set(one('$'), all, false);
        set(one('\''), all, false);
        set(one('('), all, false);
        set(one('/'), all, false);
        set(('0', '9'), all, false);
        set(one('<'), all, false);
        set(one('@'), all, false);
        set(('A', 'Z'), all, false);
        set(one('['), all, false);
        set(('^', '`'), all, false);
        set(('a', 'z'), all, false);
        set(one('{'), all, false);
        set(one('\u{7F}'), all, false);
        pair
    })
}

#[derive(PartialEq, Eq)]
enum Fast {
    NoBreak,
    CanBreak,
    Unknown,
}

fn should_break_fast(last_last: Option<char>, last: char, c: char) -> Fast {
    let (l, ch) = (last as u32, c as u32);
    if l < FAST_MIN || ch < FAST_MIN {
        return Fast::NoBreak;
    }
    if last == '-' {
        if ch <= 0x7F {
            if c.is_ascii_digit() {
                // A '-' that may be a minus sign keeps its digits; breaks
                // stay allowed in 'ABCD-1234' and '1234-5678' (long URLs).
                return if last_last.is_some_and(|p| p.is_ascii_alphanumeric()) {
                    Fast::CanBreak
                } else {
                    Fast::NoBreak
                };
            }
        } else {
            return Fast::Unknown;
        }
    }
    if l <= FAST_MAX && ch <= FAST_MAX {
        return if fast_table()[(l - FAST_MIN) as usize][(ch - FAST_MIN) as usize] {
            Fast::CanBreak
        } else {
            Fast::NoBreak
        };
    }
    Fast::Unknown
}

/// Break opportunities of one text run: byte offsets `i` (0 < i <= len)
/// such that a line may end before `text[i..]`.  The end of the text is
/// always included.
pub(crate) fn break_opportunities(text: &str, mode: BreakMode) -> Vec<usize> {
    if text.is_empty() {
        return Vec::new();
    }
    if mode.kind == LineBreakType::BreakCharacter {
        // line-break: anywhere: grapheme cluster boundaries; an ideographic
        // space still stays with the text before it.
        let mut out = grapheme_boundaries(text);
        out.retain(|&i| !text[i..].starts_with('\u{3000}'));
        return out;
    }
    // The UAX #14 opportunities (ICU's line BreakIterator in Blink).
    let icu: Vec<usize> = LineSegmenter::new_for_non_complex_scripts(mode.segmenter_options())
        .segment_str(text)
        .collect();
    let icu_break = |i: usize| icu.binary_search(&i).is_ok();
    let mut out = Vec::new();
    let mut last_last: Option<char> = None;
    let mut last: Option<char> = None;
    // The class of the last character that is not a combining mark.
    let mut last_class = LineBreak::Unknown;
    for (i, c) in text.char_indices() {
        let class = if mode.kind == LineBreakType::BreakAll {
            line_break_class(c)
        } else {
            LineBreak::Unknown
        };
        let Some(prev) = last else {
            last = Some(c);
            last_class = class;
            continue;
        };
        let opportunity = 'decide: {
            match mode.break_space {
                BreakSpace::AfterSpaceRun => {
                    if is_breakable_space(c) {
                        break 'decide false;
                    }
                    if is_breakable_space(prev) {
                        break 'decide true;
                    }
                }
                BreakSpace::AfterEverySpace => {
                    if is_breakable_space(prev) || is_other_space_separator(prev) {
                        break 'decide true;
                    }
                }
            }
            if mode.kind == LineBreakType::KeepAll && keep_all_letter(prev) && keep_all_letter(c) {
                break 'decide false;
            }
            if is_cj_dash(c) || is_cj_dash(prev) {
                if !mode.cj {
                    break 'decide true;
                }
                if mode.cj_dash_no_break_before() && is_cj_dash(c) {
                    break 'decide false;
                }
            }
            let fast = should_break_fast(last_last, prev, c);
            if fast == Fast::CanBreak {
                break 'decide true;
            }
            if mode.kind == LineBreakType::BreakAll
                && (icu_break(i)
                    || ((break_all_class(last_class)
                        // An ASCII prefix that FillAscii does not keep with
                        // the next character ('+', '\\').
                        || (last_class == LineBreak::PrefixNumeric
                            && prev.is_ascii()
                            && prev != '$'))
                        && class != LineBreak::BreakAfter
                        && break_all_class(class)))
            {
                break 'decide true;
            }
            if fast == Fast::NoBreak {
                break 'decide false;
            }
            icu_break(i)
        };
        if opportunity && !(mode.disable_soft_hyphen && prev == '\u{00AD}') {
            out.push(i);
        }
        last_last = Some(prev);
        last = Some(c);
        if class != LineBreak::CombiningMark {
            last_class = class;
        }
    }
    out.push(text.len());
    out
}

/// CSS Text 3 8.2 hanging-punctuation `last`: closing brackets (Pe) and
/// final / closing quotes (Pf, and the ambiguous quotes of Po / Pi).
pub(crate) fn is_hangable_last(c: char) -> bool {
    let gc = CodePointMapData::<GeneralCategory>::new().get(c);
    matches!(
        gc,
        GeneralCategory::ClosePunctuation | GeneralCategory::FinalPunctuation
    ) || matches!(
        c,
        '"' | '\'' | '\u{201C}' | '\u{201D}' | '\u{2018}' | '\u{2019}'
    )
}

/// Grapheme cluster boundaries (0 < i <= len): the emergency break
/// positions of `overflow-wrap: anywhere / break-word`.
pub(crate) fn grapheme_boundaries(text: &str) -> Vec<usize> {
    let mut out: Vec<usize> = GraphemeClusterSegmenter::new()
        .segment_str(text)
        .filter(|&i| i > 0)
        .collect();
    if out.last() != Some(&text.len()) {
        out.push(text.len());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn breaks(text: &str, mode: BreakMode) -> Vec<&str> {
        let mut prev = 0;
        break_opportunities(text, mode)
            .into_iter()
            .map(|i| {
                let s = &text[prev..i];
                prev = i;
                s
            })
            .collect()
    }

    #[test]
    fn latin_breaks_after_space_runs_and_hyphens() {
        let m = BreakMode::default();
        assert_eq!(breaks("Hello  world", m), ["Hello  ", "world"]);
        assert_eq!(breaks("well-known", m), ["well-", "known"]);
        // '-' before a digit keeps a minus sign, not inside 'ABCD-1234'.
        assert_eq!(breaks("x -1", m), ["x ", "-1"]);
        assert_eq!(breaks("ABCD-1234", m), ["ABCD-", "1234"]);
        // FillAscii: no break before '/' or after a letter, but before
        // '(' after other punctuation.
        assert_eq!(breaks("a/b", m), ["a/b"]);
        assert_eq!(breaks("a(b)", m), ["a(b)"]);
        assert_eq!(breaks("+(b)", m), ["+", "(b)"]);
    }

    #[test]
    fn cjk_breaks_between_ideographs_but_not_before_closing_punctuation() {
        let m = BreakMode::default();
        assert_eq!(breaks("中文。文本", m), ["中", "文。", "文", "本"]);
        assert_eq!(breaks("「中」", m), ["「中」"]);
    }

    #[test]
    fn small_kana_follow_line_break_strictness() {
        let strict = BreakMode {
            strictness: Strictness::Strict,
            cj: true,
            ..BreakMode::default()
        };
        let normal = BreakMode {
            strictness: Strictness::Normal,
            cj: true,
            ..BreakMode::default()
        };
        // U+3041 HIRAGANA LETTER SMALL A (CJ).
        assert_eq!(breaks("あぁ", strict), ["あぁ"]);
        assert_eq!(breaks("あぁ", normal), ["あ", "ぁ"]);
    }

    #[test]
    fn word_break_and_line_break_anywhere() {
        let all = BreakMode {
            kind: LineBreakType::BreakAll,
            ..BreakMode::default()
        };
        assert_eq!(breaks("abc", all), ["a", "b", "c"]);
        let keep = BreakMode {
            kind: LineBreakType::KeepAll,
            ..BreakMode::default()
        };
        assert_eq!(breaks("中文 本", keep), ["中文 ", "本"]);
        let anywhere = BreakMode {
            kind: LineBreakType::BreakCharacter,
            ..BreakMode::default()
        };
        assert_eq!(breaks("ab.", anywhere), ["a", "b", "."]);
    }

    /// Dumps the opportunities of the cases in `$W3COS_LINE_BREAK_CASES`
    /// (mode, lang, text per line) for comparison against Chrome.
    #[test]
    #[ignore]
    fn dump_cases() {
        let Ok(path) = std::env::var("W3COS_LINE_BREAK_CASES") else {
            return;
        };
        let mut out = String::new();
        for line in std::fs::read_to_string(path).unwrap().lines() {
            let mut f = line.splitn(3, '\t');
            let (mode, lang, text) = (f.next().unwrap(), f.next().unwrap(), f.next().unwrap());
            let mut m = BreakMode {
                cj: lang == "ja" || lang == "zh",
                ja: lang == "ja",
                ..BreakMode::default()
            };
            match mode {
                "strict" => m.strictness = Strictness::Strict,
                "lbnormal" => m.strictness = Strictness::Normal,
                "loose" => m.strictness = Strictness::Loose,
                "breakall" => m.kind = LineBreakType::BreakAll,
                "keepall" => m.kind = LineBreakType::KeepAll,
                "anywhere" => m.kind = LineBreakType::BreakCharacter,
                _ => {}
            }
            let segs = breaks(text, m).join("\u{1f}");
            out.push_str(&format!("{mode}\t{lang}\t{text}\t{segs}\n"));
        }
        std::fs::write("/tmp/lb-ours.tsv", out).unwrap();
    }

    /// Chrome's line breaks (one line per opportunity in a `width: 0`
    /// block), recorded with Chrome 155 by the probe in the fixture header.
    #[test]
    fn matches_chrome_line_breaks() {
        let fixture = include_str!("line_break/chrome-breaks.tsv");
        let mut diffs = Vec::new();
        let mut cases = 0;
        for line in fixture.lines().filter(|l| !l.starts_with('#')) {
            let f: Vec<&str> = line.split('\t').collect();
            let (mode, lang, text) = (f[0], f[1], f[2]);
            let chrome: Vec<&str> = f[3]
                .split('\u{1f}')
                .map(|s| s.trim_matches(' '))
                .filter(|s| !s.is_empty())
                .collect();
            let mut m = BreakMode {
                cj: lang == "ja" || lang == "zh",
                ja: lang == "ja",
                ..BreakMode::default()
            };
            match mode {
                "strict" => m.strictness = Strictness::Strict,
                "lbnormal" => m.strictness = Strictness::Normal,
                "loose" => m.strictness = Strictness::Loose,
                "breakall" => m.kind = LineBreakType::BreakAll,
                "keepall" => m.kind = LineBreakType::KeepAll,
                "anywhere" => m.kind = LineBreakType::BreakCharacter,
                _ => {}
            }
            let ours: Vec<&str> = breaks(text, m)
                .into_iter()
                .map(|s| s.trim_matches(' '))
                .filter(|s| !s.is_empty())
                .collect();
            cases += 1;
            if ours != chrome {
                diffs.push(format!(
                    "{mode} {lang} {text:?}: chrome {chrome:?} ours {ours:?}"
                ));
            }
        }
        assert!(cases > 1000);
        assert!(diffs.is_empty(), "{}", diffs.join("\n"));
    }
}
