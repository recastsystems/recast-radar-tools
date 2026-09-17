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

/// SHA-256 (FIPS 180-4) of `data` as lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    for block in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (word, bytes) in w.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_be_bytes(bytes.try_into().unwrap());
        }
        for t in 16..64 {
            let s0 = w[t - 15].rotate_right(7) ^ w[t - 15].rotate_right(18) ^ (w[t - 15] >> 3);
            let s1 = w[t - 2].rotate_right(17) ^ w[t - 2].rotate_right(19) ^ (w[t - 2] >> 10);
            w[t] = w[t - 16]
                .wrapping_add(s0)
                .wrapping_add(w[t - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for t in 0..64 {
            let sigma1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(sigma1)
                .wrapping_add(choose)
                .wrapping_add(K[t])
                .wrapping_add(w[t]);
            let sigma0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = sigma0.wrapping_add(majority);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (state, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *state = state.wrapping_add(value);
        }
    }
    h.iter().map(|v| format!("{v:08x}")).collect()
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
