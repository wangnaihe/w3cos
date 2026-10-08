//! macOS browser generic defaults, selected by content language/script.
//! These are platform defaults, not replacements for authored font families.

#[derive(Clone, Copy)]
pub(super) enum GenericFamily {
    Standard,
    Serif,
    Sans,
    Monospace,
}

#[derive(Clone, Copy)]
enum FontScript {
    Common,
    SimplifiedHan,
    TraditionalHan,
    Japanese,
    Korean,
}

fn script(language: &str) -> Option<FontScript> {
    let mut subtags = language.trim().split('-');
    let primary = subtags.next()?;
    if primary.is_empty() { return None; }
    let mut traditional_region = false;
    // A script subtag overrides the language's usual script. Stop before
    // extensions/private use: `ja-x-latn` does not declare Latin script.
    for subtag in subtags.take_while(|part| part.len() > 1) {
        if subtag.len() == 4 && subtag.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            return Some(if subtag.eq_ignore_ascii_case("hans") { FontScript::SimplifiedHan }
                else if subtag.eq_ignore_ascii_case("hant") { FontScript::TraditionalHan }
                else if ["jpan", "hrkt", "hira", "kana"].iter().any(|name| subtag.eq_ignore_ascii_case(name)) { FontScript::Japanese }
                else if ["kore", "hang"].iter().any(|name| subtag.eq_ignore_ascii_case(name)) { FontScript::Korean }
                else { FontScript::Common });
        }
        traditional_region |= ["tw", "hk", "mo"].iter().any(|region| subtag.eq_ignore_ascii_case(region));
    }
    Some(if primary.eq_ignore_ascii_case("ja") { FontScript::Japanese }
        else if primary.eq_ignore_ascii_case("ko") { FontScript::Korean }
        else if primary.eq_ignore_ascii_case("zh") {
            if traditional_region { FontScript::TraditionalHan } else { FontScript::SimplifiedHan }
        } else { FontScript::Common })
}

pub(super) fn family(language: &str, generic: GenericFamily) -> Option<&'static str> {
    use FontScript::*;
    use GenericFamily::*;
    Some(match (script(language)?, generic) {
        (Common, Standard | Serif) | (SimplifiedHan, Serif) => "Times",
        (Common, Sans) => "Helvetica",
        (SimplifiedHan, Standard | Sans) => "PingFang SC",
        (TraditionalHan, Standard | Sans) => "PingFang TC",
        (TraditionalHan, Serif) => "Songti TC",
        (Japanese, Standard | Sans) => "Hiragino Kaku Gothic ProN",
        (Japanese, Serif) => "Hiragino Mincho ProN",
        (Japanese, Monospace) => "Osaka",
        (Korean, Standard | Sans) => "Apple SD Gothic Neo",
        (Korean, Serif) => "AppleMyungjo",
        (_, Monospace) => "Courier",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_generic_defaults_respect_script_region_and_extensions() {
        use GenericFamily::*;
        for (language, standard, serif, sans, mono) in [
            ("en-US", "Times", "Times", "Helvetica", "Courier"),
            ("und", "Times", "Times", "Helvetica", "Courier"),
            ("ar", "Times", "Times", "Helvetica", "Courier"),
            ("zh-CN", "PingFang SC", "Times", "PingFang SC", "Courier"),
            ("zh-Hant", "PingFang TC", "Songti TC", "PingFang TC", "Courier"),
            ("zh-HK", "PingFang TC", "Songti TC", "PingFang TC", "Courier"),
            ("zh-Latn", "Times", "Times", "Helvetica", "Courier"),
            ("en-Hant", "PingFang TC", "Songti TC", "PingFang TC", "Courier"),
            ("ja", "Hiragino Kaku Gothic ProN", "Hiragino Mincho ProN", "Hiragino Kaku Gothic ProN", "Osaka"),
            ("ko", "Apple SD Gothic Neo", "AppleMyungjo", "Apple SD Gothic Neo", "Courier"),
        ] {
            for (generic, expected) in [(Standard, standard), (Serif, serif), (Sans, sans), (Monospace, mono)] {
                assert_eq!(family(language, generic), Some(expected), "{language}");
            }
        }
        assert_eq!(family("JA-latn", Standard), Some("Times"));
        assert_eq!(family("ja-x-latn", Standard), Some("Hiragino Kaku Gothic ProN"));
        assert_eq!(family("zh-Hans-TW", Standard), Some("PingFang SC"));
        assert_eq!(family("", Standard), None, "unknown language retains host defaults");
    }
}
