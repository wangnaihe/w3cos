//! Token scan for Electron, Node, and dynamic code. This is not a full parser.
//! It skips comments, strings, and regex literals so a mention inside those
//! does not become a clearance item.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    pub line: u32,
    pub snippet: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicHit {
    pub kind: &'static str,
    pub line: u32,
    pub detail: String,
    pub snippet: String,
}

#[derive(Clone, Debug, Default)]
pub struct FileScan {
    pub electron_apis: BTreeSet<String>,
    pub electron_sites: Vec<Site>,
    pub node_builtins: BTreeMap<String, Vec<Site>>,
    pub dynamic: Vec<DynamicHit>,
}

pub fn scan_source(source: &str) -> FileScan {
    let tokens = lex(source);
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    interpret(&tokens, &lines)
}

fn snippet_at(lines: &[&str], line: u32) -> String {
    let raw = lines
        .get(line.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("");
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 180 {
        collapsed.chars().take(180).collect()
    } else {
        collapsed
    }
}

#[derive(Clone, Debug)]
struct Token {
    kind: Kind,
    text: String,
    line: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Ident,
    Keyword,
    String,
    Number,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBrack,
    RBrack,
    Dot,
    Eq,
    Comma,
    Colon,
    Semi,
    QuestionDot,
    Op,
}

struct Lexer<'a> {
    src: &'a str,
    i: usize,
    line: u32,
    tokens: Vec<Token>,
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<char> {
        self.src[self.i..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.i += ch.len_utf8();
        if ch == '\n' {
            self.line += 1;
        }
        Some(ch)
    }

    fn emit(&mut self, kind: Kind, text: impl Into<String>, line: u32) {
        self.tokens.push(Token {
            kind,
            text: text.into(),
            line,
        });
    }

    fn last_kind(&self) -> Option<Kind> {
        self.tokens.last().map(|token| token.kind)
    }

    fn last_text(&self) -> Option<&str> {
        self.tokens.last().map(|token| token.text.as_str())
    }

    fn skip_line_comment(&mut self) {
        while let Some(ch) = self.peek() {
            self.bump();
            if ch == '\n' {
                break;
            }
        }
    }

    fn skip_block_comment(&mut self) {
        while let Some(ch) = self.bump() {
            if ch == '*' && self.peek() == Some('/') {
                self.bump();
                break;
            }
        }
    }

    fn lex_quoted(&mut self, quote: char) -> String {
        let mut value = String::new();
        while let Some(ch) = self.bump() {
            if ch == '\\' {
                if let Some(escaped) = self.bump() {
                    value.push(escaped);
                }
                continue;
            }
            if ch == quote {
                break;
            }
            value.push(ch);
        }
        value
    }

    fn lex_template(&mut self) {
        let mut literal = String::new();
        let mut interpolated = false;
        let line = self.line;
        while let Some(ch) = self.peek() {
            if ch == '\\' {
                self.bump();
                if let Some(escaped) = self.bump() {
                    literal.push(escaped);
                }
                continue;
            }
            if ch == '`' {
                self.bump();
                if !interpolated {
                    self.emit(Kind::String, literal, line);
                }
                return;
            }
            if ch == '$' {
                let dollar = self.i;
                let dollar_line = self.line;
                self.bump();
                if self.peek() == Some('{') {
                    self.bump();
                    interpolated = true;
                    self.lex_code(true);
                    if self.peek() == Some('}') {
                        self.bump();
                    }
                    continue;
                }
                self.i = dollar;
                self.line = dollar_line;
            }
            literal.push(ch);
            self.bump();
        }
    }

    fn slash_starts_regex(&self) -> bool {
        let Some(kind) = self.last_kind() else {
            return true;
        };
        match kind {
            Kind::Ident | Kind::Number | Kind::String | Kind::RParen | Kind::RBrack => false,
            Kind::Keyword => matches!(
                self.last_text(),
                Some(
                    "new"
                        | "return"
                        | "typeof"
                        | "void"
                        | "case"
                        | "throw"
                        | "delete"
                        | "await"
                        | "else"
                        | "do"
                        | "in"
                        | "of"
                        | "instanceof"
                        | "yield"
                )
            ),
            Kind::RBrace => true,
            _ => true,
        }
    }

    fn lex_regex(&mut self) {
        let mut in_class = false;
        while let Some(ch) = self.peek() {
            if ch == '\n' {
                break;
            }
            self.bump();
            if ch == '\\' {
                self.bump();
                continue;
            }
            if ch == '[' && !in_class {
                in_class = true;
                continue;
            }
            if ch == ']' && in_class {
                in_class = false;
                continue;
            }
            if ch == '/' && !in_class {
                while let Some(flag) = self.peek() {
                    if flag.is_ascii_alphabetic() {
                        self.bump();
                    } else {
                        break;
                    }
                }
                break;
            }
        }
        self.emit(Kind::Op, "/", self.line);
    }

    fn lex_code(&mut self, stop_at_brace: bool) {
        let mut brace = 0u32;
        while let Some(ch) = self.peek() {
            if stop_at_brace && ch == '}' && brace == 0 {
                return;
            }
            if ch.is_whitespace() {
                self.bump();
                continue;
            }
            let line = self.line;
            match ch {
                '/' => {
                    self.bump();
                    match self.peek() {
                        Some('/') => {
                            self.bump();
                            self.skip_line_comment();
                        }
                        Some('*') => {
                            self.bump();
                            self.skip_block_comment();
                        }
                        _ if self.slash_starts_regex() => self.lex_regex(),
                        _ => self.emit(Kind::Op, "/", line),
                    }
                }
                '\'' | '"' => {
                    let quote = ch;
                    self.bump();
                    let value = self.lex_quoted(quote);
                    self.emit(Kind::String, value, line);
                }
                '`' => {
                    self.bump();
                    self.lex_template();
                }
                '(' => {
                    self.bump();
                    self.emit(Kind::LParen, "(", line);
                }
                ')' => {
                    self.bump();
                    self.emit(Kind::RParen, ")", line);
                }
                '{' => {
                    self.bump();
                    brace += 1;
                    self.emit(Kind::LBrace, "{", line);
                }
                '}' => {
                    self.bump();
                    brace = brace.saturating_sub(1);
                    self.emit(Kind::RBrace, "}", line);
                }
                '[' => {
                    self.bump();
                    self.emit(Kind::LBrack, "[", line);
                }
                ']' => {
                    self.bump();
                    self.emit(Kind::RBrack, "]", line);
                }
                '.' => {
                    self.bump();
                    self.emit(Kind::Dot, ".", line);
                }
                ',' => {
                    self.bump();
                    self.emit(Kind::Comma, ",", line);
                }
                ':' => {
                    self.bump();
                    self.emit(Kind::Colon, ":", line);
                }
                ';' => {
                    self.bump();
                    self.emit(Kind::Semi, ";", line);
                }
                '=' => {
                    self.bump();
                    if self.peek() == Some('>') {
                        self.bump();
                        self.emit(Kind::Op, "=>", line);
                    } else if self.peek() == Some('=') {
                        self.bump();
                        if self.peek() == Some('=') {
                            self.bump();
                        }
                        self.emit(Kind::Op, "==", line);
                    } else {
                        self.emit(Kind::Eq, "=", line);
                    }
                }
                '?' => {
                    self.bump();
                    if self.peek() == Some('.') {
                        self.bump();
                        self.emit(Kind::QuestionDot, "?.", line);
                    } else {
                        self.emit(Kind::Op, "?", line);
                    }
                }
                '0'..='9' => {
                    while self
                        .peek()
                        .is_some_and(|next| next.is_ascii_alphanumeric() || next == '.')
                    {
                        self.bump();
                    }
                    self.emit(Kind::Number, "0", line);
                }
                _ if is_ident_start(ch) => {
                    let mut text = String::new();
                    text.push(ch);
                    self.bump();
                    while self.peek().is_some_and(is_ident_continue) {
                        text.push(self.bump().unwrap());
                    }
                    let kind = if is_keyword(&text) {
                        Kind::Keyword
                    } else {
                        Kind::Ident
                    };
                    self.emit(kind, text, line);
                }
                _ => {
                    self.bump();
                    self.emit(Kind::Op, ch.to_string(), line);
                }
            }
        }
    }
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch == '$' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    is_ident_start(ch) || ch.is_ascii_digit()
}

fn is_keyword(text: &str) -> bool {
    matches!(
        text,
        "new"
            | "function"
            | "return"
            | "typeof"
            | "void"
            | "case"
            | "throw"
            | "delete"
            | "await"
            | "else"
            | "do"
            | "in"
            | "of"
            | "instanceof"
            | "yield"
            | "import"
            | "export"
            | "from"
            | "as"
            | "type"
            | "const"
            | "let"
            | "var"
            | "if"
            | "for"
            | "while"
            | "switch"
            | "class"
            | "extends"
            | "static"
            | "default"
    )
}

fn lex(source: &str) -> Vec<Token> {
    let mut lexer = Lexer {
        src: source,
        i: 0,
        line: 1,
        tokens: Vec::new(),
    };
    if source.starts_with("#!") {
        lexer.skip_line_comment();
    }
    lexer.lex_code(false);
    lexer.tokens
}

fn interpret(tokens: &[Token], lines: &[&str]) -> FileScan {
    let mut scan = FileScan::default();
    let mut namespaces = BTreeSet::new();
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        if token.kind == Kind::Keyword && token.text == "import" {
            if next_kind(tokens, index) == Some(Kind::LParen) {
                note_dynamic_import(&mut scan, tokens, lines, index);
                index += 1;
                continue;
            }
            if let Some(consumed) =
                note_import_declaration(&mut scan, tokens, lines, index, &mut namespaces)
            {
                index += consumed.max(1);
                continue;
            }
        }
        if token.kind == Kind::Keyword && token.text == "export" {
            if let Some(consumed) = note_export(&mut scan, tokens, lines, index, &mut namespaces) {
                index += consumed;
                continue;
            }
        }
        if token.kind == Kind::Ident
            && token.text == "require"
            && !prev_is_dot(tokens, index)
            && !prev_is_keyword(tokens, index, "function")
        {
            if next_kind(tokens, index) == Some(Kind::LParen) {
                let consumed = note_require(&mut scan, tokens, lines, index, &mut namespaces);
                index += consumed.max(1);
                continue;
            }
        }
        if token.kind == Kind::Ident
            && token.text == "eval"
            && call_follows(tokens, index)
            && !prev_is_dot(tokens, index)
        {
            scan.dynamic.push(DynamicHit {
                kind: "eval",
                line: token.line,
                detail: String::new(),
                snippet: snippet_at(lines, token.line),
            });
        }
        if token.kind == Kind::Keyword
            && token.text == "new"
            && ident_at(tokens, index + 1, "Function")
        {
            scan.dynamic.push(DynamicHit {
                kind: "new-function",
                line: token.line,
                detail: "new".to_string(),
                snippet: snippet_at(lines, token.line),
            });
        } else if token.kind == Kind::Ident
            && token.text == "Function"
            && call_follows(tokens, index)
            && !prev_is_dot(tokens, index)
            && !prev_is_keyword(tokens, index, "new")
            && !prev_is_keyword(tokens, index, "function")
        {
            scan.dynamic.push(DynamicHit {
                kind: "new-function",
                line: token.line,
                detail: "call".to_string(),
                snippet: snippet_at(lines, token.line),
            });
        }
        if token.kind == Kind::Ident
            && token.text == "createRequire"
            && call_follows(tokens, index)
            && !prev_is_dot(tokens, index)
        {
            scan.dynamic.push(DynamicHit {
                kind: "create-require",
                line: token.line,
                detail: String::new(),
                snippet: snippet_at(lines, token.line),
            });
        }
        if token.kind == Kind::Ident
            && token.text == "vm"
            && next_kind(tokens, index) == Some(Kind::Dot)
            && vm_method(tokens.get(index + 2).map(|next| next.text.as_str()))
        {
            let method = tokens[index + 2].text.clone();
            scan.dynamic.push(DynamicHit {
                kind: "vm",
                line: token.line,
                detail: method,
                snippet: snippet_at(lines, token.line),
            });
        }
        if token.kind == Kind::Ident
            && namespaces.contains(&token.text)
            && next_kind(tokens, index) == Some(Kind::Dot)
        {
            if let Some(prop) = tokens.get(index + 2) {
                if prop.kind == Kind::Ident && !is_promise_method(&prop.text) {
                    record_electron(&mut scan, lines, prop.line, &[prop.text.clone()]);
                }
            }
        }
        index += 1;
    }
    scan
}

fn vm_method(name: Option<&str>) -> bool {
    matches!(
        name,
        Some(
            "runInContext"
                | "runInNewContext"
                | "runInThisContext"
                | "compileFunction"
                | "createContext"
                | "Script"
        )
    )
}

fn is_promise_method(name: &str) -> bool {
    matches!(name, "then" | "catch" | "finally")
}

fn next_kind(tokens: &[Token], index: usize) -> Option<Kind> {
    tokens.get(index + 1).map(|token| token.kind)
}

fn ident_at(tokens: &[Token], index: usize, text: &str) -> bool {
    tokens
        .get(index)
        .is_some_and(|token| token.kind == Kind::Ident && token.text == text)
}

fn call_follows(tokens: &[Token], index: usize) -> bool {
    match next_kind(tokens, index) {
        Some(Kind::LParen) => true,
        Some(Kind::QuestionDot) => tokens
            .get(index + 2)
            .is_some_and(|token| token.kind == Kind::LParen),
        _ => false,
    }
}

fn prev_is_dot(tokens: &[Token], index: usize) -> bool {
    index > 0 && tokens[index - 1].kind == Kind::Dot
}

fn prev_is_keyword(tokens: &[Token], index: usize, text: &str) -> bool {
    index > 0 && tokens[index - 1].kind == Kind::Keyword && tokens[index - 1].text == text
}

fn note_dynamic_import(scan: &mut FileScan, tokens: &[Token], lines: &[&str], index: usize) {
    let line = tokens[index].line;
    let literal = argument_literal(tokens, index);
    let (kind, detail) = match &literal {
        Some(module) => ("import-literal", module.clone()),
        None => ("import", String::new()),
    };
    scan.dynamic.push(DynamicHit {
        kind,
        line,
        detail: detail.clone(),
        snippet: snippet_at(lines, line),
    });
    if let Some(module) = literal {
        record_module(scan, lines, line, &module, &[]);
    }
}

fn note_require(
    scan: &mut FileScan,
    tokens: &[Token],
    lines: &[&str],
    index: usize,
    namespaces: &mut BTreeSet<String>,
) -> usize {
    let line = tokens[index].line;
    let literal = argument_literal(tokens, index);
    let member = member_after_call(tokens, index);
    match literal {
        Some(module) => {
            let apis = binding_apis(tokens, index);
            if module == "electron" {
                if let Some(name) = namespace_binding(tokens, index) {
                    namespaces.insert(name);
                }
                let mut names = apis;
                if let Some(member) = member {
                    names.push(member);
                }
                record_electron(scan, lines, line, &names);
            } else {
                record_module(scan, lines, line, &module, &[]);
            }
            if module == "vm" || module == "node:vm" {
                scan.dynamic.push(DynamicHit {
                    kind: "vm",
                    line,
                    detail: "import".to_string(),
                    snippet: snippet_at(lines, line),
                });
            }
            1
        }
        None => {
            scan.dynamic.push(DynamicHit {
                kind: "require",
                line,
                detail: String::new(),
                snippet: snippet_at(lines, line),
            });
            1
        }
    }
}

fn import_clause_starts(tokens: &[Token], cursor: usize) -> bool {
    match tokens
        .get(cursor)
        .map(|token| (token.kind, token.text.as_str()))
    {
        Some((Kind::String, _)) | Some((Kind::LBrace, _)) | Some((Kind::Ident, _)) => true,
        Some((Kind::Keyword, "type")) => true,
        Some((Kind::Op, "*")) => true,
        _ => false,
    }
}

fn note_import_declaration(
    scan: &mut FileScan,
    tokens: &[Token],
    lines: &[&str],
    index: usize,
    namespaces: &mut BTreeSet<String>,
) -> Option<usize> {
    let line = tokens[index].line;
    let mut cursor = index + 1;
    if !import_clause_starts(tokens, cursor) {
        return None;
    }
    if keyword_at(tokens, cursor, "type") {
        cursor += 1;
    }
    if tokens
        .get(cursor)
        .is_some_and(|token| token.kind == Kind::String)
    {
        let module = tokens[cursor].text.clone();
        record_module(scan, lines, line, &module, &[]);
        note_vm_import(scan, lines, line, &module);
        return Some(cursor + 1 - index);
    }
    let mut names = Vec::new();
    let mut namespace = None;
    if tokens
        .get(cursor)
        .is_some_and(|token| token.kind == Kind::Op && token.text == "*")
    {
        cursor += 1;
        if keyword_at(tokens, cursor, "as") {
            cursor += 1;
        }
        if tokens
            .get(cursor)
            .is_some_and(|token| token.kind == Kind::Ident)
        {
            namespace = Some(tokens[cursor].text.clone());
            cursor += 1;
        }
    } else {
        if tokens
            .get(cursor)
            .is_some_and(|token| token.kind == Kind::Ident)
        {
            namespace = Some(tokens[cursor].text.clone());
            cursor += 1;
        }
        if tokens
            .get(cursor)
            .is_some_and(|token| token.kind == Kind::Comma)
        {
            cursor += 1;
        }
        if tokens
            .get(cursor)
            .is_some_and(|token| token.kind == Kind::LBrace)
        {
            let (parsed, next) = parse_brace_names(tokens, cursor);
            names.extend(parsed);
            cursor = next;
        }
    }
    let (module, module_at) = find_from_string(tokens, cursor, 80)?;
    if module == "electron" {
        if let Some(name) = namespace {
            namespaces.insert(name);
        }
    }
    record_module(scan, lines, line, &module, &names);
    note_vm_import(scan, lines, line, &module);
    Some(module_at + 2 - index)
}

fn note_vm_import(scan: &mut FileScan, lines: &[&str], line: u32, module: &str) {
    if module == "vm" || module == "node:vm" {
        scan.dynamic.push(DynamicHit {
            kind: "vm",
            line,
            detail: "import".to_string(),
            snippet: snippet_at(lines, line),
        });
    }
}

fn note_export(
    scan: &mut FileScan,
    tokens: &[Token],
    lines: &[&str],
    index: usize,
    _namespaces: &mut BTreeSet<String>,
) -> Option<usize> {
    let next = tokens.get(index + 1)?;
    let reexport = (next.kind == Kind::Op && next.text == "*") || next.kind == Kind::LBrace;
    if !reexport {
        return None;
    }
    let (module, module_at) = find_from_string(tokens, index + 1, 80)?;
    let names = if next.kind == Kind::LBrace {
        parse_brace_names(tokens, index + 1).0
    } else {
        Vec::new()
    };
    record_module(scan, lines, tokens[index].line, &module, &names);
    Some(module_at + 2 - index)
}

fn find_from_string(tokens: &[Token], start: usize, window: usize) -> Option<(String, usize)> {
    let end = tokens.len().min(start + window);
    let mut cursor = start;
    while cursor < end {
        if tokens[cursor].kind == Kind::Semi {
            break;
        }
        if tokens[cursor].kind == Kind::Keyword && tokens[cursor].text == "from" {
            let module = tokens.get(cursor + 1)?;
            if module.kind == Kind::String {
                return Some((module.text.clone(), cursor));
            }
        }
        cursor += 1;
    }
    None
}

fn parse_brace_names(tokens: &[Token], brace: usize) -> (Vec<String>, usize) {
    let mut names = Vec::new();
    let mut cursor = brace + 1;
    while cursor < tokens.len() {
        let token = &tokens[cursor];
        if token.kind == Kind::RBrace {
            return (names, cursor + 1);
        }
        if token.kind == Kind::Keyword && token.text == "type" {
            cursor += 1;
            continue;
        }
        if token.kind == Kind::Ident || (token.kind == Kind::Keyword && token.text != "as") {
            let name = token.text.clone();
            cursor += 1;
            if keyword_at(tokens, cursor, "as") {
                cursor += 2;
            } else if tokens
                .get(cursor)
                .is_some_and(|next| next.kind == Kind::Colon)
            {
                cursor += 2;
            }
            if name != "type" && name != "as" && name != "from" {
                names.push(name);
            }
            continue;
        }
        cursor += 1;
    }
    (names, cursor)
}

fn keyword_at(tokens: &[Token], index: usize, text: &str) -> bool {
    tokens
        .get(index)
        .is_some_and(|token| token.kind == Kind::Keyword && token.text == text)
}

fn argument_literal(tokens: &[Token], call: usize) -> Option<String> {
    let open = call + 1;
    if tokens.get(open).map(|token| token.kind) != Some(Kind::LParen) {
        return None;
    }
    let literal = tokens.get(open + 1)?;
    if literal.kind != Kind::String {
        return None;
    }
    let after = tokens.get(open + 2)?;
    if after.kind == Kind::RParen || after.kind == Kind::Comma {
        Some(literal.text.clone())
    } else {
        None
    }
}

fn member_after_call(tokens: &[Token], call: usize) -> Option<String> {
    let open = call + 1;
    if tokens.get(open).map(|token| token.kind) != Some(Kind::LParen) {
        return None;
    }
    let mut depth = 0i32;
    for (offset, token) in tokens.iter().enumerate().skip(open) {
        match token.kind {
            Kind::LParen => depth += 1,
            Kind::RParen => {
                depth -= 1;
                if depth == 0 {
                    if tokens
                        .get(offset + 1)
                        .is_some_and(|next| next.kind == Kind::Dot)
                    {
                        let prop = tokens.get(offset + 2)?;
                        if prop.kind == Kind::Ident {
                            return Some(prop.text.clone());
                        }
                    }
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

fn namespace_binding(tokens: &[Token], call: usize) -> Option<String> {
    if call < 2 || tokens[call - 1].kind != Kind::Eq {
        return None;
    }
    let mut cursor = call - 2;
    if keyword_at(tokens, cursor, "await") && cursor > 0 {
        cursor -= 1;
    }
    if tokens[cursor].kind == Kind::Ident {
        Some(tokens[cursor].text.clone())
    } else {
        None
    }
}

fn binding_apis(tokens: &[Token], call: usize) -> Vec<String> {
    if call < 2 || tokens[call - 1].kind != Kind::Eq {
        return Vec::new();
    }
    let mut cursor = call - 2;
    if keyword_at(tokens, cursor, "await") && cursor > 0 {
        cursor -= 1;
    }
    if tokens
        .get(cursor)
        .is_some_and(|token| token.kind != Kind::RBrace)
    {
        return Vec::new();
    }
    let mut depth = 0i32;
    loop {
        match tokens[cursor].kind {
            Kind::RBrace => depth += 1,
            Kind::LBrace => {
                depth -= 1;
                if depth == 0 {
                    return parse_brace_names(tokens, cursor).0;
                }
            }
            _ => {}
        }
        if cursor == 0 {
            return Vec::new();
        }
        cursor -= 1;
    }
}

fn record_module(scan: &mut FileScan, lines: &[&str], line: u32, module: &str, names: &[String]) {
    if module == "electron" {
        record_electron(scan, lines, line, names);
        return;
    }
    if let Some(builtin) = node_builtin(module) {
        scan.node_builtins.entry(builtin).or_default().push(Site {
            line,
            snippet: snippet_at(lines, line),
        });
    }
}

fn record_electron(scan: &mut FileScan, lines: &[&str], line: u32, names: &[String]) {
    for name in names {
        if is_electron_api_name(name) {
            scan.electron_apis.insert(name.clone());
        }
    }
    scan.electron_sites.push(Site {
        line,
        snippet: snippet_at(lines, line),
    });
}

fn is_electron_api_name(name: &str) -> bool {
    !name.is_empty()
        && name != "default"
        && name != "prototype"
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
}

pub fn node_builtin(module: &str) -> Option<String> {
    let bare = module.strip_prefix("node:").unwrap_or(module);
    if NODE_BUILTINS.contains(&bare)
        || NODE_BUILTINS
            .iter()
            .any(|builtin| bare.starts_with(&format!("{builtin}/")))
    {
        Some(bare.to_string())
    } else if module.starts_with("node:") && !bare.is_empty() && !bare.contains('\\') {
        Some(bare.to_string())
    } else {
        None
    }
}

const NODE_BUILTINS: &[&str] = &[
    "assert",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "domain",
    "events",
    "fs",
    "http",
    "http2",
    "https",
    "inspector",
    "module",
    "net",
    "os",
    "path",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "repl",
    "stream",
    "string_decoder",
    "sys",
    "timers",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_comments_strings_and_regex() {
        let scan = scan_source(
            r#"
// eval("no")
const s = "eval(no)";
const re = /eval\(/;
const t = `eval(no)`;
eval("yes");
"#,
        );
        assert_eq!(scan.dynamic.len(), 1);
        assert_eq!(scan.dynamic[0].kind, "eval");
        assert!(scan.dynamic[0].snippet.contains("yes"));
    }

    #[test]
    fn template_interpolation_is_code() {
        let scan = scan_source("const s = `${new Function(\"a\")}`;\n");
        assert!(scan.dynamic.iter().any(|hit| hit.kind == "new-function"));
    }

    #[test]
    fn classifies_electron_node_and_dynamic_calls() {
        let source = r#"
import { app, BrowserWindow as BW, type Session } from "electron";
import * as ns from 'electron';
const { dialog, shell: sh } = require("electron");
require("electron").nativeTheme;
ns.screen;
eval("nope");
const f = new Function("return 1");
Function("return 2");
require(pathToModule);
import(spec);
require("fs");
require("node:path");
import { promises as fs } from "fs/promises";
import vm from "node:vm";
vm.runInContext("1", {});
createRequire(import.meta.url);
import("node:crypto");
require("electron" + suffix);
declare function require(path: string): any;
"#;
        let scan = scan_source(source);
        let apis = scan.electron_apis.iter().cloned().collect::<Vec<_>>();
        assert!(apis.contains(&"app".to_string()), "{apis:?}");
        assert!(apis.contains(&"BrowserWindow".to_string()), "{apis:?}");
        assert!(apis.contains(&"Session".to_string()), "{apis:?}");
        assert!(apis.contains(&"dialog".to_string()), "{apis:?}");
        assert!(apis.contains(&"shell".to_string()), "{apis:?}");
        assert!(apis.contains(&"nativeTheme".to_string()), "{apis:?}");
        assert!(apis.contains(&"screen".to_string()), "{apis:?}");
        assert!(!apis.iter().any(|name| name == "sh" || name == "BW"));
        assert!(scan.node_builtins.contains_key("fs"));
        assert!(scan.node_builtins.contains_key("path"));
        assert!(scan.node_builtins.contains_key("fs/promises"));
        assert!(scan.node_builtins.contains_key("vm"));
        assert!(scan.node_builtins.contains_key("crypto"));
        let kinds = scan.dynamic.iter().map(|hit| hit.kind).collect::<Vec<_>>();
        assert!(kinds.contains(&"eval"));
        assert!(kinds.contains(&"new-function"));
        assert!(kinds.contains(&"require"));
        assert!(kinds.contains(&"import"));
        assert!(kinds.contains(&"import-literal"));
        assert!(kinds.contains(&"vm"));
        assert!(kinds.contains(&"create-require"));
        assert_eq!(kinds.iter().filter(|kind| **kind == "require").count(), 2);
    }
}
