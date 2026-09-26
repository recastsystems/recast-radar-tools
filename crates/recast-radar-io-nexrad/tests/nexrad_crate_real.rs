//! Messages 3 and 18 against the danielway nexrad crates, a Level II
//! decoder written independently of this workspace.
//!
//! `testdata/level2/golden/nexrad_crate/<id>.json`
//! (`tools/nexrad_crate_golden.py`, nexrad at 1591b64) lists every accessor
//! of the first message 3 (Performance/Maintenance Data) and message 18 (RDA
//! Adaptation Data) that the nexrad crates decode from each status golden
//! file, with the byte where they read it (from their own raw struct and
//! offsets) and the value.
//!
//! - Coverage: every byte an accessor reads must be held by a model
//!   variable (`passthrough::performance_fields` and `adaptation_fields` give
//!   the variables' locations), so the model's field lists are checked
//!   against an independent list, not only against themselves. The one
//!   exception is a byte both documents call spare ([`SPARE`]).
//! - Values: the variable must hold the accessor's value, in 21 files (7728
//!   values). For message 18 only in the first segment (body bytes 0 to
//!   2399): past it the nexrad crates' values differ from MetPy 1.7.1's at
//!   957 of the 1057 locations MetPy also reads, while the model agrees with
//!   MetPy there (`metadata_passthrough_real.rs`); SITE_NAME at byte 8368,
//!   for example, comes back empty where MetPy and the model read the site.
//!   Those 1316 later elements are checked for coverage only.
//!
//! Legacy RDA messages, which the model keeps verbatim
//! (`nexrad_unparsed_message_*`) and the nexrad crates read with the Open RDA
//! layout, are not compared.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use recast_radar_core::model::{ArrayBuf, ExtraVariable, Scalar, Volume};
use recast_radar_io_nexrad::passthrough::{
    FieldValue, Location, MessageField, adaptation_fields, performance_fields,
};
use recast_radar_io_nexrad::{NexradMetadata, read_volume_from_bytes};
use serde_json::Value;

/// Message 3 bytes an accessor reads that are spare in ICD 2620002AA Table
/// V and in the nexrad crates' own documentation: halfword 448 byte 1 (their
/// `rsp_status` is the whole halfword, "byte 0 is Code1 bitfield, byte 1 is
/// spare").
const SPARE: [(u8, usize); 1] = [(3, 895)];

/// Message 18 body bytes of the first segment (1208 halfwords less the
/// message header).
const FIRST_SEGMENT_BYTES: usize = 2400;

fn golden_dir() -> std::path::PathBuf {
    recast_radar_testdata::testdata_dir().join("level2/golden/nexrad_crate")
}

fn golden_ids() -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(golden_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids
}

/// The KIWA start chunk is decoded with the next two chunks, so the volume
/// has radials.
fn source_ids(id: &str) -> Vec<String> {
    if let Some(prefix) = id.strip_suffix("-001-s") {
        vec![
            id.to_owned(),
            format!("{prefix}-002-i"),
            format!("{prefix}-003-i"),
        ]
    } else {
        vec![id.to_owned()]
    }
}

fn byte_offset(location: Location) -> usize {
    match location {
        Location::Halfword(number) => (usize::from(number) - 1) * 2,
        Location::HalfwordByte(number, byte) => (usize::from(number) - 1) * 2 + usize::from(byte),
        Location::Byte(offset) => usize::from(offset),
    }
}

/// Bytes of one element of a model value; 0 for text and flags.
fn element_bytes(value: &FieldValue<'_>) -> usize {
    let of_array = |array: &ArrayBuf| match array {
        ArrayBuf::U8(_) | ArrayBuf::I8(_) => 1,
        ArrayBuf::U16(_) | ArrayBuf::I16(_) => 2,
        ArrayBuf::U32(_) | ArrayBuf::I32(_) | ArrayBuf::F32(_) => 4,
        ArrayBuf::F64(_) | ArrayBuf::I64(_) => 8,
        ArrayBuf::Text(_) => 0,
    };
    match value {
        FieldValue::Scalar(scalar) => match scalar {
            Scalar::I8(_) | Scalar::U8(_) => 1,
            Scalar::I16(_) | Scalar::U16(_) => 2,
            Scalar::I32(_) | Scalar::U32(_) | Scalar::F32(_) => 4,
            Scalar::I64(_) | Scalar::U64(_) | Scalar::F64(_) => 8,
        },
        FieldValue::Array(array) => of_array(array),
        FieldValue::Text(_) | FieldValue::Flag(_) => 0,
    }
}

/// A value as raw bits of its element size, or text.
#[derive(Debug, PartialEq)]
enum Held {
    Bits(u64),
    Text(String),
}

fn held(values: &ArrayBuf, index: usize) -> Held {
    match values {
        ArrayBuf::U8(v) => Held::Bits(u64::from(v[index])),
        ArrayBuf::I8(v) => Held::Bits(u64::from(v[index] as u8)),
        ArrayBuf::U16(v) => Held::Bits(u64::from(v[index])),
        ArrayBuf::I16(v) => Held::Bits(u64::from(v[index] as u16)),
        ArrayBuf::U32(v) => Held::Bits(u64::from(v[index])),
        ArrayBuf::I32(v) => Held::Bits(u64::from(v[index] as u32)),
        ArrayBuf::F32(v) => Held::Bits(u64::from(v[index].to_bits())),
        ArrayBuf::F64(v) => Held::Bits(v[index].to_bits()),
        ArrayBuf::I64(v) => Held::Bits(v[index] as u64),
        ArrayBuf::Text(v) => Held::Text(v[index].trim().to_owned()),
    }
}

/// The nexrad crates' value, element `index`, in the same terms; `None`
/// where their accessor returned nothing.
fn golden_value(value: &Value, element: usize, index: usize) -> Option<Held> {
    match value {
        Value::Null => None,
        Value::Number(number) => {
            let raw = number.as_i64().unwrap() as u64;
            Some(Held::Bits(raw & (u64::MAX >> (64 - 8 * element))))
        }
        Value::Array(items) => golden_value(&items[index], element, 0),
        Value::Object(object) => Some(if let Some(bits) = object.get("f32_bits") {
            Held::Bits(bits.as_u64().unwrap())
        } else if let Some(bits) = object.get("f64_bits") {
            Held::Bits(bits.as_str().unwrap().parse().unwrap())
        } else if let Some(text) = object.get("text") {
            Held::Text(text.as_str().unwrap().trim().to_owned())
        } else if let Some(bytes) = object.get("bytes") {
            let bytes: Vec<u8> = bytes
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b.as_u64().unwrap() as u8)
                .collect();
            Held::Text(
                String::from_utf8_lossy(&bytes)
                    .trim_matches(char::from(0))
                    .trim()
                    .to_owned(),
            )
        } else {
            panic!("unexpected golden value {object:?}")
        }),
        other => panic!("unexpected golden value {other}"),
    }
}

/// Where the model holds the element at `at` of `element` bytes: the
/// variable and the index in it.
fn holder<'f>(
    fields: &'f [MessageField<'f>],
    at: usize,
    element: usize,
    text: bool,
) -> Option<(&'f MessageField<'f>, usize)> {
    fields.iter().find_map(|field| {
        let start = byte_offset(field.location);
        let width = element_bytes(&field.value);
        match &field.value {
            FieldValue::Text(_) | FieldValue::Flag(_) => {
                (text && start == at).then_some((field, 0))
            }
            FieldValue::Scalar(_) => (start == at && width == element).then_some((field, 0)),
            FieldValue::Array(array) => (width == element
                && at >= start
                && at < start + array.len() * width
                && (at - start).is_multiple_of(width))
            .then(|| (field, (at - start) / width)),
        }
    })
}

fn root<'v>(volume: &'v Volume, name: &str) -> &'v ExtraVariable {
    volume
        .extra_vars
        .iter()
        .find(|v| &*v.name == name)
        .unwrap_or_else(|| panic!("{name} not in the model"))
}

#[derive(Default)]
struct Tally {
    compared: usize,
    coverage_only: usize,
    missing: Vec<String>,
}

/// Checks every accessor of one message.
fn check_message(
    id: &str,
    volume: &Volume,
    (kind, prefix): (u8, &str),
    fields: &[MessageField<'_>],
    golden: &[Value],
    tally: &mut Tally,
) {
    for accessor in golden {
        let name = accessor["name"].as_str().unwrap();
        let byte = accessor["byte"].as_u64().unwrap() as usize;
        let size = accessor["size"].as_u64().unwrap() as usize;
        let element = accessor["element"].as_u64().unwrap() as usize;
        let value = &accessor["value"];
        let text =
            matches!(value, Value::Object(o) if o.contains_key("text") || o.contains_key("bytes"));
        let count = if text { 1 } else { size / element };
        for index in 0..count {
            let at = byte + index * element;
            let Some(expected) = golden_value(value, element, index) else {
                continue;
            };
            let compare = kind != 18 || at < FIRST_SEGMENT_BYTES;
            if let Some((field, position)) = holder(fields, at, element, text) {
                if !compare {
                    tally.coverage_only += 1;
                    continue;
                }
                let variable = root(volume, &format!("{prefix}{}", field.name));
                let actual = held(&variable.values, position);
                let same = match (&expected, &actual) {
                    (Held::Text(e), Held::Text(a)) => e == a,
                    // A flag byte that is neither "T" nor "F": the byte.
                    (Held::Text(e), Held::Bits(a)) => {
                        u64::from(e.as_bytes().first().copied().unwrap_or(0)) == *a
                    }
                    (Held::Bits(e), Held::Bits(a)) => e == a,
                    (Held::Bits(_), Held::Text(_)) => false,
                };
                assert!(
                    same,
                    "{id}: nexrad crate {name}[{index}] (byte {at}) {expected:?}, model {prefix}{} {actual:?}",
                    field.name
                );
                tally.compared += 1;
                continue;
            }
            // Held byte by byte (a halfword the model splits into bytes).
            let Held::Bits(bits) = expected else {
                tally
                    .missing
                    .push(format!("message {kind} {name} byte {at}"));
                continue;
            };
            for offset in 0..element {
                let expected_byte = (bits >> (8 * (element - 1 - offset))) & 0xff;
                match holder(fields, at + offset, 1, false) {
                    Some((field, position)) => {
                        if compare {
                            let variable = root(volume, &format!("{prefix}{}", field.name));
                            assert_eq!(
                                held(&variable.values, position),
                                Held::Bits(expected_byte),
                                "{id}: nexrad crate {name} byte {}, model {prefix}{}",
                                at + offset,
                                field.name
                            );
                            tally.compared += 1;
                        } else {
                            tally.coverage_only += 1;
                        }
                    }
                    None if SPARE.contains(&(kind, at + offset)) => {}
                    None => tally
                        .missing
                        .push(format!("message {kind} {name} byte {}", at + offset)),
                }
            }
        }
    }
}

fn legacy_kinds(volume: &Volume) -> Vec<u8> {
    volume
        .extra_vars
        .iter()
        .find(|v| &*v.name == "nexrad_unparsed_message_type")
        .map(|v| match &v.values {
            ArrayBuf::U8(kinds) => kinds.clone(),
            _ => Vec::new(),
        })
        .unwrap_or_default()
}

#[test]
fn messages_3_and_18_match_the_nexrad_crates() {
    let ids = golden_ids();
    let mut checked = 0;
    let mut sources = Vec::new();
    let mut tally = Tally::default();
    let mut messages = 0;
    for id in &ids {
        let source = source_ids(id);
        sources.push(source.clone());
        let refs: Vec<&str> = source.iter().map(String::as_str).collect();
        let Some(bytes) = common::load_all(&refs) else {
            continue;
        };
        checked += 1;
        let text = std::fs::read_to_string(golden_dir().join(format!("{id}.json"))).unwrap();
        let golden: Value = serde_json::from_str(&text).unwrap();
        let Ok(volume) = read_volume_from_bytes(&bytes) else {
            // The status-only stub has no radials to build a volume from.
            assert!(id.ends_with("-stub"), "{id}");
            continue;
        };
        let metadata = NexradMetadata::from_metadata_record(&bytes);
        let legacy = legacy_kinds(&volume);
        for (key, kind, prefix) in [
            ("message_3", 3u8, "nexrad_performance_"),
            ("message_18", 18u8, "nexrad_adaptation_"),
        ] {
            let carried = volume.extra_vars.iter().any(|v| v.name.starts_with(prefix));
            let Some(accessors) = golden[key].as_array() else {
                assert!(!carried, "{id}: {prefix} without a nexrad-crate {key}");
                continue;
            };
            if legacy.contains(&kind) {
                assert!(!carried, "{id}: legacy message {kind} decoded");
                continue;
            }
            let fields = match kind {
                3 => performance_fields(metadata.performance.as_ref().unwrap()),
                _ => adaptation_fields(metadata.adaptation.as_ref().unwrap()),
            };
            check_message(id, &volume, (kind, prefix), &fields, accessors, &mut tally);
            messages += 1;
        }
    }
    common::assert_checked_every_available("nexrad crate", checked, &sources);
    tally.missing.sort();
    tally.missing.dedup();
    assert!(
        tally.missing.is_empty(),
        "bytes the nexrad crates read that the model does not hold: {:#?}",
        tally.missing
    );
    eprintln!(
        "{messages} messages: {} values compared, {} later message 18 bytes checked for coverage only",
        tally.compared, tally.coverage_only
    );
    if checked == ids.len() {
        assert_eq!(
            (messages, tally.compared, tally.coverage_only),
            (42, 7728, 1316),
            "messages, values compared, coverage only"
        );
    }
}
