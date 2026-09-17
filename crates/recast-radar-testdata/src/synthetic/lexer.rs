//! A small Rust lexer: enough to find items, attributes, identifiers, literals
//! and bracket structure without a parser dependency. Comments are dropped and
//! string contents are kept only as literal tokens, so words inside strings
//! and comments never look like code.

/// Token kinds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    /// Identifier or keyword (`r#` prefix removed).
    Ident(String),
    /// `'a`, `'static`, loop labels.
    Lifetime,
    /// String literal (normal, raw or C string), escapes decoded.
    Str(String),
    /// Byte string literal (normal or raw), escapes decoded.
    ByteStr(Vec<u8>),
    /// Character or byte character literal.
    Char,
    /// Numeric literal, as written.
    Num(String),
    /// Punctuation (`::`, `=>`, `==`, `..`, `.`, `;`, `!`, `#`, ...).
    Punct(&'static str),
    /// `(`, `[` or `{`.
    Open(char),
    /// `)`, `]` or `}`.
    Close(char),
}

/// A token and the 1-based line it starts on.
#[derive(Clone, Debug)]
pub(crate) struct Token {
    pub(crate) tok: Tok,
    pub(crate) line: usize,
}

/// Lexed file: tokens plus, for every bracket token, the index of its partner
/// (`usize::MAX` for unbalanced brackets and non-bracket tokens).
pub(crate) struct Lexed {
    pub(crate) tokens: Vec<Token>,
    pub(crate) partner: Vec<usize>,
}

impl Lexed {
    pub(crate) fn new(source: &str) -> Self {
        let tokens = lex(source);
        let mut partner = vec![usize::MAX; tokens.len()];
        let mut stack: Vec<usize> = Vec::new();
        for (index, token) in tokens.iter().enumerate() {
            match token.tok {
                Tok::Open(_) => stack.push(index),
                Tok::Close(close) => {
                    // Pop until the matching opener; tolerate stray closers.
                    let expected = match close {
                        ')' => '(',
                        ']' => '[',
                        _ => '{',
                    };
                    if let Some(position) = stack
                        .iter()
                        .rposition(|&open| tokens[open].tok == Tok::Open(expected))
                    {
                        let open = stack[position];
                        stack.truncate(position);
                        partner[open] = index;
                        partner[index] = open;
                    }
                }
                _ => {}
            }
        }
        Self { tokens, partner }
    }

    pub(crate) fn len(&self) -> usize {
        self.tokens.len()
    }

    pub(crate) fn tok(&self, index: usize) -> Option<&Tok> {
        self.tokens.get(index).map(|token| &token.tok)
    }

    pub(crate) fn line(&self, index: usize) -> usize {
        self.tokens
            .get(index)
            .or_else(|| self.tokens.last())
            .map_or(1, |token| token.line)
    }

    pub(crate) fn ident(&self, index: usize) -> Option<&str> {
        match self.tok(index) {
            Some(Tok::Ident(name)) => Some(name),
            _ => None,
        }
    }

    pub(crate) fn is_ident(&self, index: usize, name: &str) -> bool {
        self.ident(index) == Some(name)
    }

    pub(crate) fn is_punct(&self, index: usize, punct: &str) -> bool {
        matches!(self.tok(index), Some(Tok::Punct(p)) if *p == punct)
    }

    pub(crate) fn is_open(&self, index: usize, open: char) -> bool {
        matches!(self.tok(index), Some(Tok::Open(c)) if *c == open)
    }

    /// Index of the partner of the bracket at `index`, if balanced.
    pub(crate) fn close_of(&self, index: usize) -> Option<usize> {
        self.partner
            .get(index)
            .copied()
            .filter(|&close| close != usize::MAX && close > index)
    }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

const PUNCT3: [&str; 4] = ["..=", "...", "<<=", ">>="];
const PUNCT2: [&str; 18] = [
    "::", "=>", "==", "!=", "<=", ">=", "->", "..", "&&", "||", "+=", "-=", "*=", "/=", "%=", "^=",
    "&=", "|=",
];
const PUNCT1: [&str; 21] = [
    ".", ",", ";", ":", "#", "!", "?", "=", "<", ">", "&", "|", "+", "-", "*", "/", "%", "^", "@",
    "~", "$",
];

fn lex(source: &str) -> Vec<Token> {
    let cs: Vec<char> = source.chars().collect();
    let len = cs.len();
    let at = |i: usize| cs.get(i).copied().unwrap_or('\0');
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1;
    while i < len {
        let c = cs[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && at(i + 1) == '/' {
            while i < len && cs[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && at(i + 1) == '*' {
            let mut depth = 1;
            i += 2;
            while i < len && depth > 0 {
                if cs[i] == '\n' {
                    line += 1;
                    i += 1;
                } else if cs[i] == '/' && at(i + 1) == '*' {
                    depth += 1;
                    i += 2;
                } else if cs[i] == '*' && at(i + 1) == '/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        let start_line = line;
        let mut push = |tok: Tok| {
            out.push(Token {
                tok,
                line: start_line,
            })
        };

        // Prefixed literals: b"..", b'.', br".."/br#".."#, r".."/r#".."#, c"..".
        if matches!(c, 'b' | 'r' | 'c') {
            let (byte, raw, quote_at) = match (c, at(i + 1), at(i + 2)) {
                ('b', '"', _) => (true, false, i + 1),
                ('b', '\'', _) => {
                    let end = char_literal_end(&cs, i + 1);
                    push(Tok::Char);
                    i = end;
                    continue;
                }
                ('b', 'r', '"' | '#') => (true, true, i + 2),
                ('r', '"', _) => (false, true, i + 1),
                ('r', '#', next) if next == '"' || next == '#' => (false, true, i + 1),
                ('c', '"', _) => (false, false, i + 1),
                ('c', 'r', '"' | '#') => (false, true, i + 2),
                _ => (false, false, usize::MAX),
            };
            if quote_at != usize::MAX {
                let (content, end, newlines) = if raw {
                    raw_string(&cs, quote_at)
                } else {
                    escaped_string(&cs, quote_at)
                };
                line += newlines;
                if byte {
                    push(Tok::ByteStr(content));
                } else {
                    push(Tok::Str(String::from_utf8_lossy(&content).into_owned()));
                }
                i = end;
                continue;
            }
        }
        if is_ident_start(c) {
            // Raw identifiers: r#name.
            let begin = if c == 'r' && at(i + 1) == '#' && is_ident_start(at(i + 2)) {
                i + 2
            } else {
                i
            };
            let mut end = begin;
            while end < len && is_ident_continue(cs[end]) {
                end += 1;
            }
            push(Tok::Ident(cs[begin..end].iter().collect()));
            i = end;
            continue;
        }
        if c.is_ascii_digit() {
            let end = number_end(&cs, i);
            push(Tok::Num(cs[i..end].iter().collect()));
            i = end;
            continue;
        }
        if c == '"' {
            let (content, end, newlines) = escaped_string(&cs, i);
            line += newlines;
            push(Tok::Str(String::from_utf8_lossy(&content).into_owned()));
            i = end;
            continue;
        }
        if c == '\'' {
            if at(i + 1) == '\\' || (at(i + 2) == '\'' && at(i + 1) != '\'') {
                let end = char_literal_end(&cs, i);
                push(Tok::Char);
                i = end;
            } else {
                let mut end = i + 1;
                while end < len && is_ident_continue(cs[end]) {
                    end += 1;
                }
                push(Tok::Lifetime);
                i = end.max(i + 1);
            }
            continue;
        }
        match c {
            '(' | '[' | '{' => {
                push(Tok::Open(c));
                i += 1;
                continue;
            }
            ')' | ']' | '}' => {
                push(Tok::Close(c));
                i += 1;
                continue;
            }
            _ => {}
        }
        let matches_at = |p: &str| p.chars().enumerate().all(|(k, pc)| at(i + k) == pc);
        if let Some(p) = PUNCT3.iter().find(|p| matches_at(p)) {
            push(Tok::Punct(p));
            i += 3;
        } else if let Some(p) = PUNCT2.iter().find(|p| matches_at(p)) {
            push(Tok::Punct(p));
            i += 2;
        } else if let Some(p) = PUNCT1.iter().find(|p| matches_at(p)) {
            push(Tok::Punct(p));
            i += 1;
        } else {
            i += 1;
        }
    }
    out
}

/// End (exclusive) of a character literal whose opening quote is at `quote`.
fn char_literal_end(cs: &[char], quote: usize) -> usize {
    let mut j = quote + 1;
    if cs.get(j) == Some(&'\\') {
        j += 2;
    } else {
        j += 1;
    }
    while j < cs.len() && cs[j] != '\'' && cs[j] != '\n' {
        j += 1;
    }
    (j + 1).min(cs.len())
}

fn number_end(cs: &[char], start: usize) -> usize {
    let len = cs.len();
    let at = |i: usize| cs.get(i).copied().unwrap_or('\0');
    let mut j = start;
    if at(j) == '0' && matches!(at(j + 1), 'x' | 'o' | 'b') {
        j += 2;
        while j < len && (cs[j].is_ascii_alphanumeric() || cs[j] == '_') {
            j += 1;
        }
        return j;
    }
    let mut seen_dot = false;
    let mut seen_exp = false;
    while j < len {
        let ch = cs[j];
        if ch.is_ascii_digit() || ch == '_' {
            j += 1;
        } else if ch == '.' && !seen_dot && !seen_exp && at(j + 1).is_ascii_digit() {
            seen_dot = true;
            j += 1;
        } else if matches!(ch, 'e' | 'E')
            && !seen_exp
            && (at(j + 1).is_ascii_digit()
                || (matches!(at(j + 1), '+' | '-') && at(j + 2).is_ascii_digit()))
        {
            seen_exp = true;
            j += if at(j + 1).is_ascii_digit() { 1 } else { 2 };
        } else if ch.is_ascii_alphabetic() {
            while j < len && (cs[j].is_ascii_alphanumeric() || cs[j] == '_') {
                j += 1;
            }
            break;
        } else {
            break;
        }
    }
    j
}

/// Raw (byte) string starting at `r_or_hash`: the index of the first `#` or
/// `"` after the `r`. Returns (content, end, newlines).
fn raw_string(cs: &[char], start: usize) -> (Vec<u8>, usize, usize) {
    let mut j = start;
    let mut hashes = 0;
    while cs.get(j) == Some(&'#') {
        hashes += 1;
        j += 1;
    }
    // cs[j] is the opening quote.
    j += 1;
    let mut content = String::new();
    let mut newlines = 0;
    while j < cs.len() {
        if cs[j] == '"' && (1..=hashes).all(|k| cs.get(j + k) == Some(&'#')) {
            return (content.into_bytes(), j + 1 + hashes, newlines);
        }
        if cs[j] == '\n' {
            newlines += 1;
        }
        content.push(cs[j]);
        j += 1;
    }
    (content.into_bytes(), cs.len(), newlines)
}

/// Escaped (byte) string whose opening quote is at `quote`. Returns
/// (decoded bytes, end, newlines).
fn escaped_string(cs: &[char], quote: usize) -> (Vec<u8>, usize, usize) {
    let mut out: Vec<u8> = Vec::new();
    let mut j = quote + 1;
    let mut newlines = 0;
    let mut utf8 = [0u8; 4];
    while j < cs.len() {
        let ch = cs[j];
        match ch {
            '"' => return (out, j + 1, newlines),
            '\\' => {
                let escape = cs.get(j + 1).copied().unwrap_or('\0');
                j += 2;
                match escape {
                    'n' => out.push(b'\n'),
                    'r' => out.push(b'\r'),
                    't' => out.push(b'\t'),
                    '0' => out.push(0),
                    '\\' => out.push(b'\\'),
                    '\'' => out.push(b'\''),
                    '"' => out.push(b'"'),
                    'x' => {
                        let hex: String = cs.iter().skip(j).take(2).collect();
                        if let Ok(value) = u8::from_str_radix(&hex, 16) {
                            out.push(value);
                        }
                        j += 2;
                    }
                    'u' => {
                        // \u{XXXX}
                        let mut k = j;
                        while k < cs.len() && cs[k] != '}' {
                            k += 1;
                        }
                        let hex: String = cs
                            .iter()
                            .skip(j + 1)
                            .take(k.saturating_sub(j + 1))
                            .collect();
                        if let Some(decoded) =
                            u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                        {
                            out.extend_from_slice(decoded.encode_utf8(&mut utf8).as_bytes());
                        }
                        j = k + 1;
                    }
                    '\n' => {
                        newlines += 1;
                        while j < cs.len() && cs[j].is_whitespace() {
                            if cs[j] == '\n' {
                                newlines += 1;
                            }
                            j += 1;
                        }
                    }
                    other => out.extend_from_slice(other.encode_utf8(&mut utf8).as_bytes()),
                }
            }
            _ => {
                if ch == '\n' {
                    newlines += 1;
                }
                out.extend_from_slice(ch.encode_utf8(&mut utf8).as_bytes());
                j += 1;
            }
        }
    }
    (out, cs.len(), newlines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<Tok> {
        Lexed::new(source)
            .tokens
            .into_iter()
            .map(|token| token.tok)
            .collect()
    }

    #[test]
    fn comments_and_string_contents_are_not_identifiers() {
        let toks = kinds("// fn a() {}\n/* nested /* x */ y */ let s = \"fn b\"; r#\"q\"#;");
        let idents: Vec<&Tok> = toks
            .iter()
            .filter(|tok| matches!(tok, Tok::Ident(_)))
            .collect();
        assert_eq!(
            idents,
            vec![&Tok::Ident("let".into()), &Tok::Ident("s".into())]
        );
        assert!(toks.contains(&Tok::Str("fn b".into())));
        assert!(toks.contains(&Tok::Str("q".into())));
    }

    #[test]
    fn byte_strings_decode_escapes() {
        let toks = kinds(r#"x(b"CDF\x01", br"A\B", b'\'')"#);
        assert!(toks.contains(&Tok::ByteStr(vec![b'C', b'D', b'F', 1])));
        assert!(toks.contains(&Tok::ByteStr(b"A\\B".to_vec())));
        assert!(toks.contains(&Tok::Char));
    }

    #[test]
    fn lifetimes_chars_numbers_and_ranges() {
        let toks = kinds("fn f<'a>(x: &'a str) { let c = 'z'; let r = 0..5; let v = 1.5e-3f32; }");
        assert_eq!(toks.iter().filter(|t| **t == Tok::Lifetime).count(), 2);
        assert!(toks.contains(&Tok::Char));
        assert!(toks.contains(&Tok::Num("0".into())));
        assert!(toks.contains(&Tok::Punct("..")));
        assert!(toks.contains(&Tok::Num("1.5e-3f32".into())));
    }

    #[test]
    fn brackets_are_paired_and_lines_counted() {
        let lexed = Lexed::new("fn f() {\n  g([1, 2]);\n}\n");
        let open = lexed
            .tokens
            .iter()
            .position(|t| t.tok == Tok::Open('{'))
            .unwrap_or(usize::MAX);
        let close = lexed.close_of(open).unwrap_or(0);
        assert_eq!(lexed.tokens[close].tok, Tok::Close('}'));
        assert_eq!(lexed.tokens[close].line, 3);
    }
}
