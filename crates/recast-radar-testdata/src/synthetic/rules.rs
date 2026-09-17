//! Detection rules applied to test-code items.

use std::collections::{BTreeMap, BTreeSet};

use super::Rule;
use super::items::{Item, ItemKind};
use super::lexer::{Lexed, Tok};

/// Radar model types whose hand construction in test code stands in for
/// decoded radar data, as `(crate, type)`. An empty crate means every crate:
/// the `recast-radar-core` volume, sweep, ray and moment types. The others are
/// crate-level containers of polar radar fields or radar-derived detections,
/// matched only inside their own crate.
pub const MODEL_TYPES: &[(&str, &str)] = &[
    ("", "RadarVolume"),
    ("", "ElevationCut"),
    ("", "Radial"),
    ("", "MomentGrid"),
    ("", "RayInstrumentMetadata"),
    // One velocity tilt prepared for dealiasing / scored by the bench.
    ("recast-radar-correct", "TiltField"),
    ("recast-radar-bench", "Field"),
    // Polar velocity sweep for GBVTD.
    ("recast-radar-retrieve", "PolarVelocityField"),
    // Reflectivity cells and TDS gates detected in volumes.
    ("recast-radar-track", "StormCell"),
    ("recast-radar-track", "TdsGate"),
    // Sweep headers used to group DORADE sweep files.
    ("recast-radar-io-dorade", "GroupableSweep"),
    // Level III VWP product contents.
    ("recast-radar-io-nexrad", "VwpProduct"),
];

/// Methods that add radar content to a model value.
const MODEL_METHODS: &[&str] = &[
    "push_cut",
    "find_or_insert_cut",
    "push_row",
    "push_u8_row_slice",
    "push_u16_be_row_bytes",
];

/// Integer and float encoders.
const ENCODERS: &[&str] = &["to_be_bytes", "to_le_bytes", "to_ne_bytes"];

/// Calls that write into a byte buffer or stream.
const SINKS: &[&str] = &[
    "extend_from_slice",
    "extend",
    "copy_from_slice",
    "clone_from_slice",
    "push",
    "insert",
    "splice",
    "append",
    "write_all",
    "write",
    "put_slice",
];

/// Iterator adaptors that gather encoded bytes (`.flat_map(to_be_bytes).collect()`).
const COLLECTING_SINKS: &[&str] = &["collect", "concat"];

/// Signatures of radar and container formats (file magic, block and message
/// identifiers).
const MAGICS: &[&[u8]] = &[
    // HDF5 file signature and internal object signatures
    b"\x89HDF",
    b"TREE",
    b"HEAP",
    b"SNOD",
    b"OHDR",
    b"OCHK",
    b"GCOL",
    b"FRHP",
    b"FHDB",
    b"FHIB",
    b"FSHD",
    b"FSSE",
    b"BTHD",
    b"BTIN",
    b"BTLF",
    // netCDF classic
    b"CDF\x01",
    b"CDF\x02",
    b"CDF\x05",
    // NEXRAD Archive II volume header and Message 31 blocks
    b"AR2V",
    b"ARCHIVE2",
    b"RVOL",
    b"RELV",
    b"RRAD",
    b"DREF",
    b"DVEL",
    b"DSW",
    b"DZDR",
    b"DPHI",
    b"DRHO",
    b"DCFP",
    // NEXRAD Level III WMO heading
    b"SDUS",
    // DORADE descriptors
    b"SSWB",
    b"VOLD",
    b"RADD",
    b"PARM",
    b"CELV",
    b"CSFD",
    b"CFAC",
    b"SWIB",
    b"RYIB",
    b"ASIB",
    b"RDAT",
    b"QDAT",
    b"COMM",
    // GRIB2 and JMA archive members
    b"GRIB",
    b"7777",
    b"Z__C_RJTD",
    b"ustar",
    // zip, gzip, bzip2
    b"PK\x03\x04",
    b"PK\x05\x06",
    b"\x1f\x8b",
    b"BZh",
];

/// Words that mark a name as synthetic wherever they appear in it.
const SYNTHETIC_WORDS: &[&str] = &[
    "synth",
    "fake",
    "fabricat",
    "handcraft",
    "handmade",
    "handbuilt",
    "hand_built",
    "mock",
    "dummy",
    "phony",
];

/// `<verb>_..._<noun>` builder names.
const BUILDER_VERBS: &[&str] = &[
    "build",
    "make",
    "mk",
    "create",
    "craft",
    "encode",
    "write",
    "assemble",
    "gen",
    "generate",
    "construct",
    "forge",
    "fabricate",
    "emit",
    "pack",
    "serialize",
];

const BUILDER_NOUNS: &[&str] = &[
    "archive",
    "volume",
    "vol",
    "message",
    "msg",
    "record",
    "header",
    "file",
    "byte",
    "packet",
    "block",
    "sweep",
    "cut",
    "tilt",
    "radial",
    "ray",
    "chunk",
    "hdf",
    "h5",
    "netcdf",
    "cdf",
    "nc",
    "grib",
    "tar",
    "zip",
    "dorade",
    "odim",
    "cfradial",
    "cfrad",
    "level",
    "superblock",
    "moment",
    "gate",
    "grid",
    "field",
    "scan",
    "ppi",
    "rhi",
    "pvol",
    "product",
    "section",
    "member",
    "frame",
    "plane",
    "dataset",
    "descriptor",
    "radar",
    "vwp",
    "cell",
];

/// Identifiers naming polar field dimensions in `vec![value; rows * gates]`.
const FIELD_DIMS: &[&str] = &[
    "rows",
    "row_count",
    "rays",
    "ray_count",
    "nrays",
    "n_rays",
    "radials",
    "radial_count",
    "n_radials",
    "nradials",
    "azimuths",
    "az_count",
    "azimuth_count",
    "n_az",
    "gates",
    "gate_count",
    "ngates",
    "n_gates",
    "bins",
    "n_bins",
    "nbins",
];

/// Splits `snake_case` and `CamelCase` into lowercase words.
pub(crate) fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in name.split('_').filter(|part| !part.is_empty()) {
        let mut current = String::new();
        let mut previous_lower = false;
        for ch in part.chars() {
            if ch.is_uppercase() && previous_lower && !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            previous_lower = ch.is_lowercase() || ch.is_ascii_digit();
            current.extend(ch.to_lowercase());
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}

fn noun_matches(word: &str) -> bool {
    let stripped = word.trim_end_matches(|c: char| c.is_ascii_digit());
    let singular = stripped.strip_suffix('s').unwrap_or(stripped);
    [word, stripped, singular]
        .iter()
        .any(|candidate| !candidate.is_empty() && BUILDER_NOUNS.contains(candidate))
}

/// True when an item or helper name says it fabricates inputs.
pub(crate) fn is_synthetic_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if SYNTHETIC_WORDS.iter().any(|word| lower.contains(word)) {
        return true;
    }
    let words = words(name);
    match words.split_first() {
        Some((verb, rest)) => {
            BUILDER_VERBS.contains(&verb.as_str()) && rest.iter().any(|word| noun_matches(word))
        }
        None => false,
    }
}

/// Everything the rules need to know about one item.
#[derive(Default)]
pub(crate) struct ItemFacts {
    /// Direct rule hits: (rule, line, detail).
    pub(crate) hits: Vec<(Rule, usize, String)>,
    /// Hits that real-data evidence suppresses.
    pub(crate) suppressible: Vec<(Rule, usize, String)>,
    /// Direct real-data evidence (line, detail).
    pub(crate) evidence: Option<(usize, String)>,
    /// Referenced names (calls of lowercase names, uses of capitalized names).
    pub(crate) references: BTreeMap<String, usize>,
    /// Paths named by `include_bytes!`/`include_str!` (line, literal path).
    pub(crate) includes: Vec<(usize, String)>,
}

fn prev_is_dot(lexed: &Lexed, index: usize) -> bool {
    index > 0 && lexed.is_punct(index - 1, ".")
}

/// Collects rule hits, evidence and references for `item`.
pub(crate) fn item_facts(lexed: &Lexed, item: &Item, crate_name: &str) -> ItemFacts {
    let mut facts = ItemFacts::default();
    let range = item.start..item.end.min(lexed.len());

    // Names: the item's own (unless it is a #[test] function, whose name
    // describes behaviour) and any function or type defined inside it.
    if !item.is_test_fn && is_synthetic_name(&item.name) {
        facts.hits.push((
            Rule::SyntheticName,
            lexed.line(item.name_token),
            format!("`{}`", item.name),
        ));
    }
    for k in range.clone() {
        if k + 1 == item.name_token {
            continue;
        }
        if matches!(lexed.ident(k), Some("fn" | "struct" | "enum"))
            && let Some(name) = lexed.ident(k + 1)
            && is_synthetic_name(name)
        {
            facts.hits.push((
                Rule::SyntheticName,
                lexed.line(k + 1),
                format!("nested `{name}`"),
            ));
        }
    }

    let mut encoder_line = None;
    let mut sink: Option<(usize, String)> = None;
    let mut bytes_in_sink: Option<(usize, String)> = None;

    for k in range.clone() {
        let line = lexed.line(k);
        match lexed.tok(k) {
            Some(Tok::Ident(name)) => {
                let name = name.as_str();
                // Evidence of real input.
                if facts.evidence.is_none() {
                    let evidence = if name == "recast_radar_testdata" {
                        Some("uses recast_radar_testdata".to_owned())
                    } else if name == "require_file" && lexed.is_punct(k + 1, "!") {
                        Some("require_file!".to_owned())
                    } else if (name.ends_with("_from_path") || name.ends_with("_for_path"))
                        && lexed.is_open(k + 1, '(')
                    {
                        Some(format!("reads a file with `{name}`"))
                    } else if matches!(name, "read" | "read_to_string" | "read_dir")
                        && k >= 2
                        && lexed.is_punct(k - 1, "::")
                        && lexed.is_ident(k - 2, "fs")
                    {
                        Some(format!("fs::{name}"))
                    } else if name == "open" && k >= 2 && lexed.is_ident(k - 2, "File") {
                        Some("File::open".to_owned())
                    } else {
                        None
                    };
                    if let Some(detail) = evidence {
                        facts.evidence = Some((line, detail));
                    }
                }

                if ENCODERS.contains(&name) && encoder_line.is_none() {
                    encoder_line = Some(line);
                }
                if prev_is_dot(lexed, k)
                    && (lexed.is_open(k + 1, '(')
                        || (COLLECTING_SINKS.contains(&name) && lexed.is_punct(k + 1, "::")))
                    && (SINKS.contains(&name) || COLLECTING_SINKS.contains(&name))
                {
                    if sink.is_none() {
                        sink = Some((line, name.to_owned()));
                    }
                    // Byte-string literals and `.as_bytes()` written straight
                    // into a buffer.
                    if bytes_in_sink.is_none()
                        && SINKS.contains(&name)
                        && !matches!(name, "write" | "insert" | "push")
                        && let Some(close) = lexed.close_of(k + 1)
                    {
                        bytes_in_sink = (k + 2..close).find_map(|j| match lexed.tok(j) {
                            Some(Tok::ByteStr(bytes)) => {
                                let shown: Vec<u8> = bytes.iter().copied().take(12).collect();
                                Some((
                                    lexed.line(j),
                                    format!(
                                        "b\"{}\" written with .{name}(..)",
                                        escape_bytes(&shown)
                                    ),
                                ))
                            }
                            Some(Tok::Ident(called)) if called == "as_bytes" => Some((
                                lexed.line(j),
                                format!("text .as_bytes() written with .{name}(..)"),
                            )),
                            _ => None,
                        });
                    }
                }
                if prev_is_dot(lexed, k)
                    && lexed.is_open(k + 1, '(')
                    && MODEL_METHODS.contains(&name)
                {
                    facts
                        .hits
                        .push((Rule::ModelConstruction, line, format!(".{name}(..)")));
                }

                if MODEL_TYPES
                    .iter()
                    .any(|(owner, ty)| *ty == name && (owner.is_empty() || *owner == crate_name))
                    && let Some(detail) = model_construction(lexed, k, name)
                {
                    facts.hits.push((Rule::ModelConstruction, line, detail));
                }

                if matches!(name, "include_bytes" | "include_str")
                    && lexed.is_punct(k + 1, "!")
                    && let Some(close) = lexed.close_of(k + 2)
                    && let Some(path) = (k + 3..close).find_map(|j| match lexed.tok(j) {
                        Some(Tok::Str(path)) => Some(path.clone()),
                        _ => None,
                    })
                {
                    facts.includes.push((line, path));
                }

                if name == "vec"
                    && lexed.is_punct(k + 1, "!")
                    && lexed.is_open(k + 2, '[')
                    && let Some(detail) = gate_field(lexed, k + 3)
                {
                    facts.suppressible.push((Rule::GateField, line, detail));
                }

                // References to other items.
                if k != item.name_token && !prev_is_dot(lexed, k) {
                    let capitalized = name.chars().next().is_some_and(char::is_uppercase);
                    let defines = k > 0
                        && matches!(
                            lexed.ident(k - 1),
                            Some("fn" | "struct" | "enum" | "const" | "static" | "type" | "mod")
                        );
                    let called = lexed.is_open(k + 1, '(')
                        || (lexed.is_punct(k + 1, "::") && lexed.is_punct(k + 2, "<"))
                        || (lexed.is_punct(k + 1, "!") && !lexed.is_punct(k + 2, "="));
                    if !defines && (capitalized || called) {
                        facts.references.entry(name.to_owned()).or_insert(line);
                    }
                }
            }
            Some(Tok::ByteStr(bytes)) => {
                if let Some(magic) = MAGICS.iter().find(|magic| bytes.starts_with(magic)) {
                    facts.suppressible.push((
                        Rule::MagicLiteral,
                        line,
                        format!("b\"{}\"", escape_bytes(magic)),
                    ));
                }
            }
            _ => {}
        }
    }

    if let (Some(line), Some((_, sink_name))) = (encoder_line, &sink) {
        facts.suppressible.push((
            Rule::ByteEncoding,
            line,
            format!("to_*_bytes written with .{sink_name}(..)"),
        ));
    } else if let Some((line, detail)) = bytes_in_sink {
        facts.suppressible.push((Rule::ByteEncoding, line, detail));
    }
    facts
}

fn escape_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|&b| std::ascii::escape_default(b))
        .map(char::from)
        .collect()
}

/// `Type::new(..)`-style constructors and `Type { field: .. }` literals of a
/// model type at token `k`; `None` for type positions and patterns.
fn model_construction(lexed: &Lexed, k: usize, name: &str) -> Option<String> {
    if lexed.is_punct(k + 1, "::")
        && let Some(function) = lexed.ident(k + 2)
        && lexed.is_open(k + 3, '(')
        && ["new", "default", "empty", "builder"]
            .iter()
            .any(|prefix| function.starts_with(prefix))
    {
        return Some(format!("{name}::{function}(..)"));
    }
    if !lexed.is_open(k + 1, '{') {
        return None;
    }
    // Walk back over the path (`a::b::Name`), references and `mut`.
    let mut before = k;
    while before >= 2 && lexed.is_punct(before - 1, "::") && lexed.ident(before - 2).is_some() {
        before -= 2;
    }
    while before >= 1
        && (lexed.is_punct(before - 1, "&")
            || lexed.is_ident(before - 1, "mut")
            || matches!(lexed.tok(before - 1), Some(Tok::Lifetime)))
    {
        before -= 1;
    }
    if before >= 1 {
        let type_position = lexed.is_punct(before - 1, "->")
            || matches!(
                lexed.ident(before - 1),
                Some("struct" | "enum" | "union" | "impl" | "for" | "trait" | "type" | "dyn")
            );
        if type_position {
            return None;
        }
    }
    let close = lexed.close_of(k + 1)?;
    // Patterns: `Name { .. }` rest patterns, and `Name { a, b } =`/`=>`.
    if close >= 1 && lexed.is_punct(close - 1, "..") {
        return None;
    }
    if lexed.is_punct(close + 1, "=") || lexed.is_punct(close + 1, "=>") {
        return None;
    }
    // An empty block after a type (`-> Radial {}` was handled above); a
    // literal needs at least one field or `..base`.
    if close == k + 2 {
        return None;
    }
    Some(format!("{name} {{ .. }} literal"))
}

/// `vec![<float>; <dim> * <dim>]` starting after `[` at `j`.
fn gate_field(lexed: &Lexed, j: usize) -> Option<String> {
    let mut k = j;
    let sign = if lexed.is_punct(k, "-") {
        k += 1;
        "-"
    } else {
        ""
    };
    let fill = match lexed.tok(k) {
        Some(Tok::Num(text))
            if text.contains('.') || text.ends_with("f32") || text.ends_with("f64") =>
        {
            k += 1;
            format!("{sign}{text}")
        }
        Some(Tok::Ident(ty))
            if (ty == "f32" || ty == "f64")
                && lexed.is_punct(k + 1, "::")
                && matches!(
                    lexed.ident(k + 2),
                    Some("NAN" | "INFINITY" | "NEG_INFINITY")
                ) =>
        {
            let text = format!("{ty}::{}", lexed.ident(k + 2).unwrap_or_default());
            k += 3;
            text
        }
        _ => return None,
    };
    if !lexed.is_punct(k, ";") || !lexed.is_punct(k + 2, "*") {
        return None;
    }
    let operand = |index: usize| match lexed.tok(index) {
        Some(Tok::Ident(name)) => Some((name.clone(), FIELD_DIMS.contains(&name.as_str()))),
        Some(Tok::Num(text)) => Some((text.clone(), false)),
        _ => None,
    };
    let (a, a_dim) = operand(k + 1)?;
    let (b, b_dim) = operand(k + 3)?;
    if !matches!(lexed.tok(k + 4), Some(Tok::Close(']'))) || !(a_dim || b_dim) {
        return None;
    }
    Some(format!("vec![{fill}; {a} * {b}]"))
}

/// Resolves references by name within a set of items: same module first,
/// then anywhere in the set. Returns, for every item, the indices it uses.
pub(crate) fn resolve_references(items: &[(Item, ItemFacts)]) -> Vec<BTreeSet<usize>> {
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, (item, _)) in items.iter().enumerate() {
        by_name.entry(item.name.as_str()).or_default().push(index);
        if let Some(impl_type) = &item.impl_type
            && item.kind == ItemKind::Fn
        {
            // A method makes its type suspect wherever the type is used.
            by_name.entry(impl_type.as_str()).or_default().push(index);
        }
    }
    items
        .iter()
        .enumerate()
        .map(|(index, (item, facts))| {
            let mut used = BTreeSet::new();
            for name in facts.references.keys() {
                let Some(candidates) = by_name.get(name.as_str()) else {
                    continue;
                };
                let same_module: Vec<usize> = candidates
                    .iter()
                    .copied()
                    .filter(|&c| items[c].0.mod_path == item.mod_path)
                    .collect();
                let chosen = if same_module.is_empty() {
                    candidates.clone()
                } else {
                    same_module
                };
                used.extend(chosen.into_iter().filter(|&c| c != index));
            }
            used
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_vocabulary() {
        for name in [
            "synthetic_archive",
            "synth_rays",
            "Synth",
            "fake_volume",
            "build_message31_body",
            "make_test_volume",
            "write_zip",
            "encode_sweeps",
            "gen_cfradial_fixture",
        ] {
            assert!(is_synthetic_name(name), "{name}");
        }
        for name in [
            "decode_volume",
            "read_member",
            "build_index",
            "make_request",
            "assert_close",
            "real_bejab_pvol_decodes",
            "rows_for_level",
            "writer",
        ] {
            assert!(!is_synthetic_name(name), "{name}");
        }
        assert_eq!(words("ByteBuilderV2_x"), vec!["byte", "builder", "v2", "x"]);
    }
}
