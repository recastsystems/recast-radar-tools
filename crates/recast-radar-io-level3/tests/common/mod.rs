//! Test support shared by the Level III integration tests: reads the Level III
//! corpus manifest (`testdata/level3/manifest.toml`), the committed files and
//! their golden JSON (`testdata/level3/golden/<id>.json`, schema in
//! `tools/level3_golden.py`), and reads golden facts several tests use
//! (message location, packet codes, product-or-not).
//!
//! The crate has no dev-dependencies, so this module carries a minimal reader
//! for the subset of TOML the manifest uses (`[[file]]` tables with string,
//! integer and string-array values on one line), a small JSON parser and a
//! SHA-256 implementation (checked in `tests/framing.rs`).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use recast_radar_io_level3::{Level3Product, decode_product};

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

/// The manifest entry with id `id`.
pub fn entry(id: &str) -> Entry {
    level3_manifest()
        .into_iter()
        .find(|e| e.id == id)
        .unwrap_or_else(|| panic!("{id} not in the manifest"))
}

// ---------------------------------------------------------------------------------
// Golden facts
// ---------------------------------------------------------------------------------

/// File byte range of the Level III message (Message Header Block to the end
/// of the product) in a committed file, from the golden `framing`:
/// `message_bytes` long, followed by the 4-byte NOAAPort trailer when
/// `trailer` is set. Panics for a file whose message is split into zlib frames
/// (`zlib_frames` > 0), which is not contiguous in the file.
pub fn message_range(golden: &Json, file_len: usize) -> Range<usize> {
    let framing = golden.get("framing");
    assert_eq!(
        framing.get("zlib_frames").int("zlib_frames"),
        0,
        "message range of a zlib-framed file"
    );
    let trailer = if framing.get("trailer").is_null() {
        0
    } else {
        4
    };
    let end = file_len - trailer;
    let len = usize::try_from(framing.get("message_bytes").int("message_bytes")).unwrap();
    end - len..end
}

/// Whether the golden JSON marks the product as bzip2-compressed after its
/// Product Description Block.
pub fn is_bzip2(golden: &Json) -> bool {
    golden
        .get("compression")
        .get("bzip2")
        .as_bool()
        .unwrap_or_else(|| panic!("golden compression.bzip2 missing"))
}

/// File byte offset of the Message Header Block of a message that is neither
/// zlib-framed nor bzip2-compressed, so that ICD message offsets index the
/// file from there.
pub fn uncompressed_message_start(golden: &Json, file_len: usize) -> usize {
    assert!(!is_bzip2(golden), "message is bzip2-compressed");
    message_range(golden, file_len).start
}

/// Packet codes of a golden code list (`packet_codes`, or the `packets` of a
/// layer, page, nested list or cell trend block); empty when absent.
pub fn packet_codes(list: &Json) -> Vec<u16> {
    list.items()
        .iter()
        .map(|code| u16::try_from(code.int("packet code")).unwrap())
        .collect()
}

/// Every packet code the golden ICD walker found (`packet_codes`: sorted,
/// unique, including codes nested in SCIT packets).
pub fn golden_packet_codes(golden: &Json) -> Vec<u16> {
    packet_codes(golden.get("packet_codes"))
}

/// Counts of the `family` packet codes the golden walker found at top level:
/// symbology layers, graphic alphanumeric pages and product 62 cell trend data
/// (not packets nested in SCIT packets).
pub fn golden_top_level_counts(golden: &Json, family: &[u16]) -> BTreeMap<u16, usize> {
    let blocks = golden.get("blocks");
    let layers = blocks.get("symbology").get("layers").items();
    let pages = blocks.get("graphic").get("pages").items();
    let lists = layers
        .iter()
        .chain(pages)
        .map(|list| list.get("packets"))
        .chain([blocks.get("cell_trend").get("packets")]);
    let mut counts = BTreeMap::new();
    for code in lists.flat_map(packet_codes) {
        if family.contains(&code) {
            *counts.entry(code).or_default() += 1;
        }
    }
    counts
}

/// The decoded product of an entry the golden JSON gives a product code;
/// `None` for text-only messages and messages without a Product Description
/// Block. Panics when a product fails to decode.
pub fn decode_golden_product(entry: &Entry, golden: &Json) -> Option<Level3Product> {
    if golden.get("product_code").is_null() {
        return None;
    }
    Some(
        decode_product(&entry.bytes())
            .unwrap_or_else(|e| panic!("{}: decode_product failed: {e}", entry.id)),
    )
}

// ---------------------------------------------------------------------------------
// SHA-256
// ---------------------------------------------------------------------------------

/// SHA-256 of `data` as lowercase hex (the corpus crate's digest, which
/// `testdata/**/manifest.toml` checksums are verified with).
pub fn sha256_hex(data: &[u8]) -> String {
    recast_radar_testdata::sha256_hex(data)
}

/// SHA-256 of `levels` as big-endian 16-bit values, lowercase hex: the
/// golden `raw_sha256` of 16-bit level grids.
pub fn sha256_hex_u16_be(levels: &[u16]) -> String {
    let bytes: Vec<u8> = levels
        .iter()
        .flat_map(|level| level.to_be_bytes())
        .collect();
    recast_radar_testdata::sha256_hex(&bytes)
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

/// Relative tolerance of physical min/max/mean against MetPy (Task L3.3
/// acceptance: within 1e-4 relative).
pub const PHYSICAL_TOLERANCE: f64 = 1e-4;

/// A summary of physical values in the form of a golden `data[].physical`
/// object: `tools/level3_golden.py` summarizes MetPy's `map_data` output with
/// NaN counted as masked.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PhysicalSummary {
    pub finite: u64,
    pub masked: u64,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub mean: Option<f64>,
}

impl PhysicalSummary {
    /// Summary of `values`; anything not finite is masked.
    pub fn of(values: impl IntoIterator<Item = f64>) -> Self {
        let (mut finite, mut masked) = (0u64, 0u64);
        let (mut min, mut max, mut sum) = (f64::INFINITY, f64::NEG_INFINITY, 0.0);
        for value in values {
            if value.is_finite() {
                finite += 1;
                min = min.min(value);
                max = max.max(value);
                sum += value;
            } else {
                masked += 1;
            }
        }
        let some = |v: f64| (finite > 0).then_some(v);
        Self {
            finite,
            masked,
            min: some(min),
            max: some(max),
            mean: some(sum / finite.max(1) as f64),
        }
    }

    /// Summary of `f32` values with NaN for no value, as the decoder's
    /// `values` methods return them.
    pub fn of_f32(values: &[f32]) -> Self {
        Self::of(values.iter().map(|&v| f64::from(v)))
    }

    /// The golden `physical` object.
    pub fn from_golden(physical: &Json) -> Self {
        let count = |key: &str| u64::try_from(physical.get(key).int(key)).unwrap();
        Self {
            finite: count("finite"),
            masked: count("masked"),
            min: physical.get("min").as_f64(),
            max: physical.get("max").as_f64(),
            mean: physical.get("mean").as_f64(),
        }
    }

    /// How this summary differs from `golden`: counts must be equal and
    /// min/max/mean within [`PHYSICAL_TOLERANCE`] relative (absent on both
    /// sides when nothing is finite). Empty when they agree.
    pub fn mismatches(&self, golden: &Self) -> Vec<String> {
        let mut out = Vec::new();
        for (name, decoded, expected) in [
            ("finite", self.finite, golden.finite),
            ("masked", self.masked, golden.masked),
        ] {
            if decoded != expected {
                out.push(format!("{name}: decoded {decoded}, MetPy {expected}"));
            }
        }
        for (name, decoded, expected) in [
            ("min", self.min, golden.min),
            ("max", self.max, golden.max),
            ("mean", self.mean, golden.mean),
        ] {
            let agree = match (decoded, expected) {
                (Some(a), Some(b)) => {
                    (a - b).abs() <= PHYSICAL_TOLERANCE * a.abs().max(b.abs()) + 1e-12
                }
                (None, None) => true,
                _ => false,
            };
            if !agree {
                out.push(format!("{name}: decoded {decoded:?}, MetPy {expected:?}"));
            }
        }
        out
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
