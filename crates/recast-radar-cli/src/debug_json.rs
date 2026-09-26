//! JSON values from derived `Debug` output.
//!
//! The Level III and NEXRAD metadata types have no serde support, but all of
//! them derive `Debug`, whose compact form (`{:?}`) is a small, regular
//! grammar. [`to_json`] parses it into a JSON tree:
//!
//! | `Debug` text | JSON |
//! |---|---|
//! | `Name { a: 1, b: "x" }` (struct, struct variant) | `{"@type": "Name", "a": 1, "b": "x"}` |
//! | `Some(v)` / `None` | `v` / `null` |
//! | `Name(v)`, `Name(v, w)` (tuple struct or variant) | `{"Name": v}`, `{"Name": [v, w]}` |
//! | `Name` (unit variant) | `"Name"` |
//! | `[a, b]`, `(a, b)` | `[a, b]` |
//! | `{k: v}` (map) | `{"k": v}`; `{a, b}` (set) is `[a, b]` |
//! | `"text"`, `'c'` | strings, escapes decoded |
//! | `true`, `12`, `-1.5e-7` | booleans and numbers; `NaN` and `inf` are `null` |
//! | any other token (`2020-08-10T18:04:01Z`) | the token as a string |
//!
//! `@type` keeps the type or variant name of a struct, so struct variants of
//! one enum (such as a Level III generic component's `Text` and `Undecoded`)
//! stay apart; no Rust field can be called `@type`. Text that does not parse
//! (a hand-written `Debug`) comes back whole as a string, so nothing is lost.

use serde_json::{Map, Number, Value};

/// Deepest nesting parsed; the types printed nest a handful of levels.
const MAX_DEPTH: usize = 128;

/// Key of a struct's type or variant name in its JSON object.
pub const TYPE_KEY: &str = "@type";

/// The JSON form of `value`'s `Debug` text.
pub fn to_json(value: &dyn std::fmt::Debug) -> Value {
    let text = format!("{value:?}");
    parse(&text).unwrap_or(Value::String(text))
}

/// Parse compact `Debug` text; `None` when it does not follow the grammar.
pub fn parse(text: &str) -> Option<Value> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        pos: 0,
    };
    let value = parser.value(0)?;
    parser.skip_spaces();
    (parser.pos == parser.bytes.len()).then_some(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    /// The unparsed text (empty if `pos` is not on a character boundary,
    /// which the grammar never leaves it at).
    fn rest(&self) -> &'a str {
        self.text.get(self.pos..).unwrap_or("")
    }

    /// The text from `start` to the current position.
    fn since(&self, start: usize) -> &'a str {
        self.text.get(start..self.pos).unwrap_or("")
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_spaces(&mut self) {
        while self.peek() == Some(b' ') {
            self.pos += 1;
        }
    }

    fn eat(&mut self, token: &str) -> bool {
        if self.rest().starts_with(token) {
            self.pos += token.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_spaces();
        match self.peek()? {
            b'"' => self.string().map(Value::String),
            b'\'' => self.char_literal().map(Value::String),
            b'[' => {
                self.pos += 1;
                self.sequence(b']', depth).map(Value::Array)
            }
            b'(' => {
                self.pos += 1;
                self.sequence(b')', depth).map(Value::Array)
            }
            b'{' => {
                self.pos += 1;
                self.map_or_set(depth)
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => self.named(depth),
            _ => self.token(),
        }
    }

    /// `a, b, c` up to `close`.
    fn sequence(&mut self, close: u8, depth: usize) -> Option<Vec<Value>> {
        let mut items = Vec::new();
        loop {
            self.skip_spaces();
            if self.peek()? == close {
                self.pos += 1;
                return Some(items);
            }
            if !items.is_empty() {
                if !self.eat(",") {
                    return None;
                }
                self.skip_spaces();
                if self.peek()? == close {
                    self.pos += 1;
                    return Some(items);
                }
            }
            items.push(self.value(depth + 1)?);
        }
    }

    /// `{k: v, ...}` (a map) or `{a, b}` (a set), after the `{`.
    fn map_or_set(&mut self, depth: usize) -> Option<Value> {
        let mut map = Map::new();
        let mut set = Vec::new();
        loop {
            self.skip_spaces();
            if self.eat("}") {
                return Some(if set.is_empty() {
                    Value::Object(map)
                } else {
                    Value::Array(set)
                });
            }
            let first = map.is_empty() && set.is_empty();
            if !first && !self.eat(",") {
                return None;
            }
            self.skip_spaces();
            if self.eat("}") {
                return Some(if set.is_empty() {
                    Value::Object(map)
                } else {
                    Value::Array(set)
                });
            }
            let key = self.value(depth + 1)?;
            if self.eat(": ") {
                if !set.is_empty() {
                    return None;
                }
                let value = self.value(depth + 1)?;
                map.insert(key_text(&key), value);
            } else {
                if !map.is_empty() {
                    return None;
                }
                set.push(key);
            }
        }
    }

    /// An identifier path (`Name`, `a::b::Name<T>`) and what follows it.
    fn named(&mut self, depth: usize) -> Option<Value> {
        let start = self.pos;
        let mut angle = 0usize;
        while let Some(byte) = self.peek() {
            match byte {
                b'<' => angle += 1,
                b'>' if angle > 0 => angle -= 1,
                b':' if self.rest().starts_with("::") => {
                    self.pos += 2;
                    continue;
                }
                b',' | b' ' if angle > 0 => {}
                _ if byte.is_ascii_alphanumeric() || byte == b'_' => {}
                _ if angle > 0 => {}
                _ => break,
            }
            self.pos += 1;
        }
        let name = self.since(start);
        // Not an identifier after all: a token such as a date or `inf`.
        if matches!(self.peek(), Some(b'-' | b'.' | b':' | b'+')) && !self.rest().starts_with(": ")
        {
            self.pos = start;
            return self.token();
        }
        if self.rest().starts_with(" { ") || self.rest().starts_with(" {}") {
            self.pos += 2;
            return self.fields(name, depth);
        }
        if self.peek() == Some(b'(') {
            self.pos += 1;
            let mut items = self.sequence(b')', depth)?;
            return Some(match (name, items.len()) {
                ("Some", 1) => items.pop()?,
                (_, 1) => single(name, items.pop()?),
                _ => single(name, Value::Array(items)),
            });
        }
        Some(match name {
            "None" => Value::Null,
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            "NaN" | "inf" => Value::Null,
            _ => Value::String(name.to_owned()),
        })
    }

    /// `a: 1, b: 2 }` (after `Name {`); `..` marks a non-exhaustive struct.
    fn fields(&mut self, name: &str, depth: usize) -> Option<Value> {
        let mut map = Map::new();
        map.insert(TYPE_KEY.to_owned(), Value::String(name.to_owned()));
        loop {
            self.skip_spaces();
            if self.eat("}") {
                return Some(Value::Object(map));
            }
            if map.len() > 1 && !self.eat(",") {
                return None;
            }
            self.skip_spaces();
            if self.eat("..") {
                continue;
            }
            if self.eat("}") {
                return Some(Value::Object(map));
            }
            let start = self.pos;
            while self
                .peek()
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                self.pos += 1;
            }
            let key = self.since(start);
            // Raw identifiers print as `r#type`.
            let key = if key == "r" && self.eat("#") {
                let start = self.pos;
                while self
                    .peek()
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    self.pos += 1;
                }
                self.since(start)
            } else {
                key
            };
            if key.is_empty() || !self.eat(": ") {
                return None;
            }
            let value = self.value(depth + 1)?;
            map.insert(key.to_owned(), value);
        }
    }

    /// A number, or another bare token kept as a string. Tokens end at a
    /// delimiter or at `: ` (a map key); a `:` inside (times) is kept.
    fn token(&mut self) -> Option<Value> {
        let start = self.pos;
        while let Some(byte) = self.peek() {
            let ends = match byte {
                b',' | b')' | b']' | b'}' | b' ' => true,
                b':' => self.bytes.get(self.pos + 1) == Some(&b' '),
                _ => false,
            };
            if ends {
                break;
            }
            self.pos += 1;
        }
        let token = self.since(start);
        if token.is_empty() {
            return None;
        }
        Some(number(token).unwrap_or_else(|| Value::String(token.to_owned())))
    }

    fn string(&mut self) -> Option<String> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let rest = self.rest();
            let mut chars = rest.chars();
            let c = chars.next()?;
            self.pos += c.len_utf8();
            match c {
                '"' => return Some(out),
                '\\' => out.push(self.escape()?),
                c => out.push(c),
            }
        }
    }

    fn char_literal(&mut self) -> Option<String> {
        self.pos += 1;
        let c = self.rest().chars().next()?;
        self.pos += c.len_utf8();
        let c = if c == '\\' { self.escape()? } else { c };
        self.eat("'").then(|| c.to_string())
    }

    /// The character of an escape, after its backslash.
    fn escape(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(match c {
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'0' => '\0',
            b'\\' => '\\',
            b'"' => '"',
            b'\'' => '\'',
            b'u' => {
                if !self.eat("{") {
                    return None;
                }
                let start = self.pos;
                while self.peek().is_some_and(|byte| byte.is_ascii_hexdigit()) {
                    self.pos += 1;
                }
                let code = u32::from_str_radix(self.since(start), 16).ok()?;
                if !self.eat("}") {
                    return None;
                }
                char::from_u32(code)?
            }
            _ => return None,
        })
    }
}

/// `{"Name": value}`.
fn single(name: &str, value: Value) -> Value {
    let mut map = Map::new();
    map.insert(name.to_owned(), value);
    Value::Object(map)
}

/// A map key as text: strings as they are, anything else as compact JSON.
fn key_text(key: &Value) -> String {
    match key {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn number(token: &str) -> Option<Value> {
    let first = token.as_bytes().first()?;
    if !(first.is_ascii_digit() || *first == b'-' || *first == b'+') {
        return None;
    }
    if token == "-inf" || token == "+inf" || token == "-NaN" {
        return Some(Value::Null);
    }
    if let Ok(int) = token.parse::<i64>() {
        return Some(Value::Number(int.into()));
    }
    if let Ok(int) = token.parse::<u64>() {
        return Some(Value::Number(int.into()));
    }
    if !token
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'E' | b'-' | b'+'))
    {
        return None;
    }
    let float = token.parse::<f64>().ok()?;
    Some(Number::from_f64(float).map_or(Value::Null, Value::Number))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug)]
    #[allow(dead_code)]
    enum Kind {
        Unit,
        Tuple(u8, i16),
        Wrapped(Inner),
        Fields { code: u16, bytes: Vec<u8> },
    }

    #[derive(Debug)]
    #[allow(dead_code)]
    struct Inner {
        name: String,
        letter: char,
        scale: f32,
        tiny: f64,
        missing: Option<u8>,
        present: Option<Box<Inner2>>,
        array: [u16; 3],
        pair: (bool, i64),
        nan: f32,
    }

    #[derive(Debug)]
    #[allow(dead_code)]
    struct Inner2(u32);

    #[test]
    fn derived_debug_text_becomes_json() {
        let value = vec![
            Kind::Unit,
            Kind::Tuple(1, -2),
            Kind::Wrapped(Inner {
                name: "a \"quoted\"\nline, {with} [delims]: \u{1}".to_owned(),
                letter: '\'',
                scale: 0.1,
                tiny: 1e-300,
                missing: None,
                present: Some(Box::new(Inner2(7))),
                array: [1, 2, 3],
                pair: (true, i64::MIN),
                nan: f32::NAN,
            }),
            Kind::Fields {
                code: 0xAF1F,
                bytes: vec![],
            },
        ];
        assert_eq!(
            to_json(&value),
            json!([
                "Unit",
                {"Tuple": [1, -2]},
                {"Wrapped": {
                    "@type": "Inner",
                    "name": "a \"quoted\"\nline, {with} [delims]: \u{1}",
                    "letter": "'",
                    "scale": 0.1,
                    "tiny": 1e-300,
                    "missing": null,
                    "present": {"Inner2": 7},
                    "array": [1, 2, 3],
                    "pair": [true, i64::MIN],
                    "nan": null,
                }},
                {"@type": "Fields", "code": 44831, "bytes": []},
            ])
        );
    }

    #[test]
    fn maps_sets_and_times_parse() {
        let mut map = std::collections::BTreeMap::new();
        map.insert("k", vec![1.5f64, -0.0]);
        assert_eq!(to_json(&map), json!({"k": [1.5, -0.0]}));
        let set: std::collections::BTreeSet<u8> = [3, 1].into_iter().collect();
        assert_eq!(to_json(&set), json!([1, 3]));
        let time = chrono::DateTime::from_timestamp(1_597_082_641, 0);
        assert_eq!(to_json(&time), json!("2020-08-10T18:04:01Z"));
        assert_eq!(to_json(&()), json!([]));
    }

    #[test]
    fn text_outside_the_grammar_is_kept_whole() {
        struct Odd;
        impl std::fmt::Debug for Odd {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("<odd: {unbalanced")
            }
        }
        assert_eq!(to_json(&Odd), json!("<odd: {unbalanced"));
        assert_eq!(parse("[1, 2"), None);
        assert_eq!(parse(&"[".repeat(1000)), None);
    }
}
