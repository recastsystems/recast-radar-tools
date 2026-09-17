//! Test support shared by the Level III integration tests: reads the Level III
//! corpus manifest (`testdata/level3/manifest.toml`), the committed files and
//! their golden JSON (`testdata/level3/golden/<id>.json`, schema in
//! `tools/level3_golden.py`).
//!
//! The crate has no dev-dependencies, so this module carries a minimal reader
//! for the subset of TOML the manifest uses (`[[file]]` tables with string,
//! integer and string-array values on one line) and a small JSON parser.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

/// Workspace `testdata/` directory.
pub fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

/// One `[[file]]` entry of the Level III manifest (fields the tests use).
#[derive(Debug, Clone, Default)]
pub struct Entry {
    pub id: String,
    pub format: String,
    pub sha256: String,
    pub size: u64,
    pub committed: Option<String>,
    pub tags: Vec<String>,
}

impl Entry {
    /// Value of the first tag `prefix:value`, e.g. `tag("mnemonic")`.
    pub fn tag(&self, prefix: &str) -> Option<&str> {
        self.tags.iter().find_map(|t| {
            t.strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix(':'))
        })
    }

    /// Bytes of the committed file (size checked against the manifest).
    pub fn bytes(&self) -> Vec<u8> {
        let rel = self
            .committed
            .as_deref()
            .unwrap_or_else(|| panic!("{}: not committed", self.id));
        let rel = rel.strip_prefix("testdata/").unwrap_or(rel);
        let path = testdata_dir().join(rel);
        let bytes =
            fs::read(&path).unwrap_or_else(|e| panic!("{}: {}: {e}", self.id, path.display()));
        assert_eq!(
            bytes.len() as u64,
            self.size,
            "{}: size differs from manifest",
            self.id
        );
        bytes
    }

    /// Golden JSON for this entry.
    pub fn golden(&self) -> Json {
        let path = testdata_dir()
            .join("level3/golden")
            .join(format!("{}.json", self.id));
        let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        Json::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }
}

/// Entries of `testdata/level3/manifest.toml` in file order.
pub fn level3_manifest() -> Vec<Entry> {
    let path = testdata_dir().join("level3/manifest.toml");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    parse_manifest(&text)
}

fn parse_manifest(text: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[file]]" {
            entries.push(Entry::default());
            continue;
        }
        let (key, value) = line
            .split_once(" = ")
            .unwrap_or_else(|| panic!("manifest line {}: unsupported syntax: {raw}", n + 1));
        let entry = entries
            .last_mut()
            .unwrap_or_else(|| panic!("manifest line {}: key outside [[file]]", n + 1));
        let value = TomlValue::parse(value.trim())
            .unwrap_or_else(|e| panic!("manifest line {}: {e}", n + 1));
        match key.trim() {
            "id" => entry.id = value.string(),
            "format" => entry.format = value.string(),
            "sha256" => entry.sha256 = value.string(),
            "size" => entry.size = value.integer(),
            "committed" => entry.committed = Some(value.string()),
            "tags" => entry.tags = value.strings(),
            _ => {}
        }
    }
    entries
}

enum TomlValue {
    Str(String),
    Int(u64),
    Arr(Vec<String>),
}

impl TomlValue {
    fn parse(s: &str) -> Result<Self, String> {
        let mut chars = s.chars().peekable();
        let value = match chars.peek() {
            Some('"') => TomlValue::Str(toml_string(&mut chars)?),
            Some('[') => {
                chars.next();
                let mut items = Vec::new();
                loop {
                    while chars.peek().is_some_and(|c| c.is_whitespace() || *c == ',') {
                        chars.next();
                    }
                    match chars.peek() {
                        Some(']') => {
                            chars.next();
                            break;
                        }
                        Some('"') => items.push(toml_string(&mut chars)?),
                        other => return Err(format!("unsupported array element start {other:?}")),
                    }
                }
                TomlValue::Arr(items)
            }
            Some(c) if c.is_ascii_digit() => {
                let digits: String = chars
                    .by_ref()
                    .take_while(|c| c.is_ascii_digit() || *c == '_')
                    .collect();
                TomlValue::Int(
                    digits
                        .replace('_', "")
                        .parse()
                        .map_err(|e| format!("{e}"))?,
                )
            }
            other => return Err(format!("unsupported value start {other:?}")),
        };
        Ok(value)
    }

    fn string(self) -> String {
        match self {
            TomlValue::Str(s) => s,
            _ => panic!("expected a string"),
        }
    }

    fn integer(self) -> u64 {
        match self {
            TomlValue::Int(i) => i,
            _ => panic!("expected an integer"),
        }
    }

    fn strings(self) -> Vec<String> {
        match self {
            TomlValue::Arr(a) => a,
            _ => panic!("expected an array of strings"),
        }
    }
}

fn toml_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<String, String> {
    if chars.next() != Some('"') {
        return Err("expected '\"'".into());
    }
    let mut out = String::new();
    loop {
        match chars.next() {
            None => return Err("unterminated string".into()),
            Some('"') => return Ok(out),
            Some('\\') => match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(c @ ('u' | 'U')) => {
                    let len = if c == 'u' { 4 } else { 8 };
                    let hex: String = chars.by_ref().take(len).collect();
                    let code = u32::from_str_radix(&hex, 16).map_err(|e| format!("{e}"))?;
                    out.push(char::from_u32(code).ok_or("invalid unicode escape")?);
                }
                other => return Err(format!("unsupported escape {other:?}")),
            },
            Some(c) => out.push(c),
        }
    }
}

/// A parsed JSON value. Objects keep key order.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

static NULL: Json = Json::Null;

impl Json {
    /// Parses a complete JSON document.
    pub fn parse(text: &str) -> Result<Json, String> {
        let mut parser = JsonParser {
            b: text.as_bytes(),
            i: 0,
        };
        let value = parser.value()?;
        parser.ws();
        if parser.i != parser.b.len() {
            return Err(format!("trailing data at byte {}", parser.i));
        }
        Ok(value)
    }

    /// Member `key` of an object; `Null` when absent or not an object.
    pub fn get(&self, key: &str) -> &Json {
        match self {
            Json::Obj(members) => members
                .iter()
                .find(|(k, _)| k == key)
                .map_or(&NULL, |(_, v)| v),
            _ => &NULL,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    /// Elements of an array (empty when not an array).
    pub fn items(&self) -> &[Json] {
        match self {
            Json::Arr(items) => items,
            _ => &[],
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// Integer value (the number must be integral).
    pub fn as_i64(&self) -> Option<i64> {
        self.as_f64()
            .filter(|n| n.fract() == 0.0 && n.abs() < 9.0e15)
            .map(|n| n as i64)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Integer value, panicking with `what` when absent.
    pub fn int(&self, what: &str) -> i64 {
        self.as_i64()
            .unwrap_or_else(|| panic!("{what}: expected an integer, found {self:?}"))
    }
}

struct JsonParser<'a> {
    b: &'a [u8],
    i: usize,
}

impl JsonParser<'_> {
    fn ws(&mut self) {
        while self.b.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, lit: &str) -> Result<(), String> {
        if self.b[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            Ok(())
        } else {
            Err(format!("expected {lit} at byte {}", self.i))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.b.get(self.i) {
            Some(b'n') => self.eat("null").map(|()| Json::Null),
            Some(b't') => self.eat("true").map(|()| Json::Bool(true)),
            Some(b'f') => self.eat("false").map(|()| Json::Bool(false)),
            Some(b'"') => self.string().map(Json::Str),
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value()?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Arr(items));
                        }
                        _ => return Err(format!("expected , or ] at byte {}", self.i)),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut members = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Obj(members));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    self.ws();
                    self.eat(":")?;
                    let value = self.value()?;
                    members.push((key, value));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Obj(members));
                        }
                        _ => return Err(format!("expected , or }} at byte {}", self.i)),
                    }
                }
            }
            Some(c) if *c == b'-' || c.is_ascii_digit() => {
                let start = self.i;
                while self.b.get(self.i).is_some_and(|c| {
                    c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E')
                }) {
                    self.i += 1;
                }
                let text =
                    std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?;
                text.parse::<f64>()
                    .map(Json::Num)
                    .map_err(|e| format!("{text}: {e}"))
            }
            other => Err(format!("unexpected {other:?} at byte {}", self.i)),
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let hex = self
            .b
            .get(self.i..self.i + 4)
            .ok_or("truncated \\u escape")?;
        self.i += 4;
        u32::from_str_radix(std::str::from_utf8(hex).map_err(|e| e.to_string())?, 16)
            .map_err(|e| e.to_string())
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat("\"")?;
        let mut out = String::new();
        loop {
            let start = self.i;
            while self
                .b
                .get(self.i)
                .is_some_and(|c| *c != b'"' && *c != b'\\')
            {
                self.i += 1;
            }
            out.push_str(std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?);
            match self.b.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let escape = *self.b.get(self.i).ok_or("truncated escape")?;
                    self.i += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut code = self.hex4()?;
                            if (0xD800..0xDC00).contains(&code) {
                                self.eat("\\u")?;
                                let low = self.hex4()?;
                                code = 0x10000
                                    + ((code - 0xD800) << 10)
                                    + (low.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            // Lone surrogates cannot occur in valid golden files; map them to U+FFFD.
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        other => return Err(format!("bad escape \\{}", other as char)),
                    }
                }
                _ => return Err("unterminated string".into()),
            }
        }
    }
}
