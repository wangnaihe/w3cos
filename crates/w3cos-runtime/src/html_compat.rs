//! Feature-neutral HTML document compatibility-mode selection.
//!
//! This module deliberately has no dependency on the dynamic JavaScript
//! compiler, W3IR, or W3VM so both ordinary AOT and Browser builds can share
//! the same doctype token interpretation.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentCompatibilityMode {
    NoQuirks,
    LimitedQuirks,
    Quirks,
}

pub(crate) struct ParsedDocumentDoctype {
    pub(crate) name: String,
    pub(crate) public_id: String,
    pub(crate) system_id: String,
    pub(crate) mode: DocumentCompatibilityMode,
    pub(crate) malformed: bool,
}

pub(crate) fn parse_document_doctype(token: &str) -> ParsedDocumentDoctype {
    let mut rest = token
        .get("!doctype".len()..)
        .unwrap_or_default()
        .trim_start();
    let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = rest[..name_end].to_ascii_lowercase();
    rest = rest[name_end..].trim_start();
    let mut public_id = String::new();
    let mut system_id = String::new();
    let mut malformed = name.is_empty();
    if rest
        .get(.."public".len().min(rest.len()))
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("public"))
    {
        rest = rest["public".len()..].trim_start();
        if let Some((value, remaining)) = take_doctype_quoted(rest) {
            public_id = value;
            rest = remaining.trim_start();
            if !rest.is_empty() {
                if let Some((value, remaining)) = take_doctype_quoted(rest) {
                    system_id = value;
                    malformed |= !remaining.trim().is_empty();
                } else {
                    malformed = true;
                }
            }
        } else {
            malformed = true;
        }
    } else if rest
        .get(.."system".len().min(rest.len()))
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("system"))
    {
        rest = rest["system".len()..].trim_start();
        if let Some((value, remaining)) = take_doctype_quoted(rest) {
            system_id = value;
            malformed |= !remaining.trim().is_empty();
        } else {
            malformed = true;
        }
    } else if !rest.is_empty() {
        malformed = true;
    }
    let mode = document_compatibility_mode(&name, &public_id, &system_id, malformed);
    ParsedDocumentDoctype {
        name,
        public_id,
        system_id,
        mode,
        malformed,
    }
}

fn take_doctype_quoted(input: &str) -> Option<(String, &str)> {
    let quote = input.chars().next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    let body = &input[quote.len_utf8()..];
    let end = body.find(quote)?;
    Some((body[..end].to_string(), &body[end + quote.len_utf8()..]))
}

fn document_compatibility_mode(
    name: &str,
    public_id: &str,
    system_id: &str,
    force_quirks: bool,
) -> DocumentCompatibilityMode {
    if force_quirks || name != "html" {
        return DocumentCompatibilityMode::Quirks;
    }
    let public = public_id.to_ascii_lowercase();
    let system = system_id.to_ascii_lowercase();
    if matches!(
        public.as_str(),
        "-//w3o//dtd w3 html strict 3.0//en//" | "-/w3c/dtd html 4.0 transitional/en" | "html"
    ) || system == "http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd"
    {
        return DocumentCompatibilityMode::Quirks;
    }
    const QUIRKS_PREFIXES: &[&str] = &[
        "+//silmaril//dtd html pro v0r11 19970101//",
        "-//advasoft ltd//dtd html 3.0 aswedit + extensions//",
        "-//as//dtd html 3.0 aswedit + extensions//",
        "-//ietf//dtd html 2.0 level 1//",
        "-//ietf//dtd html 2.0 level 2//",
        "-//ietf//dtd html 2.0 strict level 1//",
        "-//ietf//dtd html 2.0 strict level 2//",
        "-//ietf//dtd html 2.0 strict//",
        "-//ietf//dtd html 2.0//",
        "-//ietf//dtd html 2.1e//",
        "-//ietf//dtd html 3.0//",
        "-//ietf//dtd html 3.2 final//",
        "-//ietf//dtd html 3.2//",
        "-//ietf//dtd html 3//",
        "-//ietf//dtd html level 0//",
        "-//ietf//dtd html level 1//",
        "-//ietf//dtd html level 2//",
        "-//ietf//dtd html level 3//",
        "-//ietf//dtd html strict level 0//",
        "-//ietf//dtd html strict level 1//",
        "-//ietf//dtd html strict level 2//",
        "-//ietf//dtd html strict level 3//",
        "-//ietf//dtd html strict//",
        "-//ietf//dtd html//",
        "-//metrius//dtd metrius presentational//",
        "-//microsoft//dtd internet explorer 2.0 html strict//",
        "-//microsoft//dtd internet explorer 2.0 html//",
        "-//microsoft//dtd internet explorer 2.0 tables//",
        "-//microsoft//dtd internet explorer 3.0 html strict//",
        "-//microsoft//dtd internet explorer 3.0 html//",
        "-//microsoft//dtd internet explorer 3.0 tables//",
        "-//netscape comm. corp.//dtd html//",
        "-//netscape comm. corp.//dtd strict html//",
        "-//o'reilly and associates//dtd html 2.0//",
        "-//o'reilly and associates//dtd html extended 1.0//",
        "-//o'reilly and associates//dtd html extended relaxed 1.0//",
        "-//sq//dtd html 2.0 hotmetal + extensions//",
        "-//softquad software//dtd hotmetal pro 6.0::19990601::extensions to html 4.0//",
        "-//softquad//dtd hotmetal pro 4.0::19971010::extensions to html 4.0//",
        "-//spyglass//dtd html 2.0 extended//",
        "-//sun microsystems corp.//dtd hotjava html//",
        "-//sun microsystems corp.//dtd hotjava strict html//",
        "-//w3c//dtd html 3 1995-03-24//",
        "-//w3c//dtd html 3.2 draft//",
        "-//w3c//dtd html 3.2 final//",
        "-//w3c//dtd html 3.2//",
        "-//w3c//dtd html 3.2s draft//",
        "-//w3c//dtd html 4.0 frameset//",
        "-//w3c//dtd html 4.0 transitional//",
        "-//w3c//dtd html experimental 19960712//",
        "-//w3c//dtd html experimental 970421//",
        "-//w3c//dtd w3 html//",
        "-//w3o//dtd w3 html 3.0//",
        "-//webtechs//dtd mozilla html 2.0//",
        "-//webtechs//dtd mozilla html//",
    ];
    if QUIRKS_PREFIXES
        .iter()
        .any(|prefix| public.starts_with(prefix))
    {
        return DocumentCompatibilityMode::Quirks;
    }
    let html_401 = public.starts_with("-//w3c//dtd html 4.01 frameset//")
        || public.starts_with("-//w3c//dtd html 4.01 transitional//");
    if system_id.is_empty() && html_401 {
        return DocumentCompatibilityMode::Quirks;
    }
    let xhtml_10 = public.starts_with("-//w3c//dtd xhtml 1.0 frameset//")
        || public.starts_with("-//w3c//dtd xhtml 1.0 transitional//");
    if xhtml_10 || (!system_id.is_empty() && html_401) {
        return DocumentCompatibilityMode::LimitedQuirks;
    }
    DocumentCompatibilityMode::NoQuirks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_modes_are_available_without_dynamic_javascript() {
        let standards = parse_document_doctype("!DOCTYPE html");
        assert_eq!(standards.mode, DocumentCompatibilityMode::NoQuirks);

        let limited = parse_document_doctype(
            "!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \
             \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\"",
        );
        assert_eq!(limited.mode, DocumentCompatibilityMode::LimitedQuirks);

        let quirks = parse_document_doctype(
            "!DOCTYPE html SYSTEM \
             \"http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd\"",
        );
        assert_eq!(quirks.mode, DocumentCompatibilityMode::Quirks);
    }
}
