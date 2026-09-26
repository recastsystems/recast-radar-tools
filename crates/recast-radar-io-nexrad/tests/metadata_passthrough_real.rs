//! The Level II metadata messages reach the model and the FM301 view
//! (`src/passthrough.rs`).
//!
//! Each real file is decoded with `read_volume_from_bytes`, and every
//! `nexrad_*` root variable is read twice: from `Volume::extra_vars` and from
//! the FM301 view with every passthrough item (`Passthrough::All`). Both must
//! equal an independent reading of the file:
//!
//! - message 2: the halfwords of every message 2 in the file, read here at
//!   their Table IV positions (ICD 2620002AA for an Open RDA, 2620002B for a
//!   legacy RDA), and the first one also against MetPy 1.7.1's codes
//!   (`testdata/level2/golden/status`);
//! - messages 3 and 18: every field against the body bytes at the location
//!   the field names, read here in the field's type, and against MetPy 1.7.1
//!   wherever MetPy reads the same location in the same type
//!   (`golden/status`);
//! - message 5: every header and cut value against MetPy 1.7.1
//!   (`golden/vcp`), and each sweep's `target_scan_rate` against the
//!   azimuth rate of its cut;
//! - message 15: every range zone against MetPy 1.7.1 (`golden/clutter`);
//! - messages 13 and 32: the body bytes read here (MetPy does not decode
//!   message 32, and its message 13 golden holds summaries only).
//!
//! Message bodies are framed and joined with `messages::RawMessages`; no
//! value is taken from the crate's decoders except the list of message 3
//! and 18 fields and their locations, which the byte and MetPy comparisons
//! then check.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;

use recast_radar_core::fm301::{self, FirstDim, Flavor, Passthrough, ViewOptions};
use recast_radar_core::model::{ArrayBuf, AttrValue, ExtraVariable, Volume};
use recast_radar_io_nexrad::messages::{self, RawMessage, RawMessages};
use recast_radar_io_nexrad::passthrough::{
    FieldValue, Location, MessageField, adaptation_fields, performance_fields,
};
use recast_radar_io_nexrad::{NexradMetadata, read_volume_from_bytes};
use serde_json::Value;

const ALL: ViewOptions = ViewOptions {
    flavor: Flavor::Wmo2022,
    first_dim: FirstDim::Time,
    passthrough: Passthrough::All,
};

fn golden_dir(group: &str) -> std::path::PathBuf {
    recast_radar_testdata::testdata_dir().join(format!("level2/golden/{group}"))
}

/// The ids of a golden group.
fn golden_ids(group: &str) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(golden_dir(group))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids
}

fn golden(group: &str, id: &str) -> Value {
    let text = std::fs::read_to_string(golden_dir(group).join(format!("{id}.json"))).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// The manifest ids behind a golden id: the KIWA start chunk golden is
/// decoded with the next two chunks, so the volume has radials.
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

fn load(id: &str) -> Option<Vec<u8>> {
    let ids = source_ids(id);
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    common::load_all(&refs)
}

/// Every message in the file, framed and joined.
fn raw_messages(bytes: &[u8]) -> Vec<RawMessage<'static>> {
    let records = messages::record_bytes(bytes).unwrap().into_owned();
    RawMessages::new(&records)
        .filter_map(Result::ok)
        .map(|message| RawMessage {
            header: message.header,
            offset: message.offset,
            frames: message.frames,
            body: std::borrow::Cow::Owned(message.body.into_owned()),
        })
        .collect()
}

/// A root passthrough variable from the model and the view, which must agree.
fn root_variable<'v>(volume: &'v Volume, name: &str) -> Option<&'v ExtraVariable> {
    let model = volume.extra_vars.iter().find(|v| &*v.name == name);
    let view = fm301::volume_view(volume, ALL, None).unwrap();
    let viewed = view.root.variable(name);
    assert_eq!(model.is_some(), viewed.is_some(), "{name}: model and view");
    let model = model?;
    let viewed = viewed.unwrap();
    assert_eq!(
        viewed.values.materialize().unwrap(),
        model.values,
        "{name}: view values"
    );
    let dims: Vec<&str> = viewed.dims.iter().map(|dim| dim.as_ref()).collect();
    let model_dims: Vec<&str> = model.dims.iter().map(AsRef::as_ref).collect();
    assert_eq!(dims, model_dims, "{name}: dims");
    for (key, value) in &model.attrs {
        assert_eq!(viewed.attr(key), Some(value), "{name}: attribute {key}");
    }
    Some(model)
}

fn be_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

fn be_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn halfword(body: &[u8], number: usize) -> u16 {
    be_u16(body, (number - 1) * 2)
}

fn values_f64(values: &ArrayBuf) -> Vec<f64> {
    (0..values.len())
        .map(|index| values.get_f64(index).unwrap())
        .collect()
}

fn same(actual: f64, expected: f64) -> bool {
    actual == expected || (actual.is_nan() && expected.is_nan())
}

// ---------------------------------------------------------------------------
// Message 2

/// Table IV positions this test reads, per layout: (column, halfword,
/// signed hundredths?).
const ORDA_HALFWORDS: &[(&str, usize)] = &[
    ("rda_status", 1),
    ("operability_status", 2),
    ("control_status", 3),
    ("auxiliary_power_generator_state", 4),
    ("average_transmitter_power", 5),
    ("data_transmission_enabled", 7),
    ("rda_control_authorization", 9),
    ("rda_build_number", 10),
    ("operational_mode", 11),
    ("super_resolution_status", 12),
    ("clutter_mitigation_decision_status", 13),
    ("rda_scan_and_data_flags", 14),
    ("rda_alarm_summary", 15),
    ("command_acknowledgment", 16),
    ("channel_control_status", 17),
    ("spot_blanking_status", 18),
    ("bypass_map_generation_date", 19),
    ("bypass_map_generation_time", 20),
    ("clutter_filter_map_generation_date", 21),
    ("clutter_filter_map_generation_time", 22),
    ("transition_power_source_status", 24),
    ("rms_control_status", 25),
    ("performance_check_status", 26),
];

const ORDA_LONG_HALFWORDS: &[(&str, usize)] = &[
    ("signal_processor_options", 41),
    ("downloaded_pattern_number", 59),
    ("status_version", 60),
];

const LEGACY_HALFWORDS: &[(&str, usize)] = &[
    ("rda_status", 1),
    ("operability_status", 2),
    ("control_status", 3),
    ("auxiliary_power_generator_state", 4),
    ("average_transmitter_power", 5),
    ("data_transmission_enabled", 7),
    ("rda_control_authorization", 9),
    ("interference_detection_rate", 10),
    ("operational_mode", 11),
    ("interference_suppression_unit", 12),
    ("archive_ii_status", 13),
    ("archive_ii_remaining_capacity", 14),
    ("rda_alarm_summary", 15),
    ("command_acknowledgment", 16),
    ("channel_control_status", 17),
    ("spot_blanking_status", 18),
    ("bypass_map_generation_date", 19),
    ("bypass_map_generation_time", 20),
    ("notch_width_map_generation_date", 21),
    ("notch_width_map_generation_time", 22),
    ("transition_power_source_status", 24),
    ("rms_control_status", 25),
];

/// Expected value of a message 2 column for one message, read from its
/// body, or `None` when the message's layout has no such column.
fn status_expected(name: &str, message: &RawMessage<'_>) -> Option<Vec<f64>> {
    let body = &message.body;
    let orda = message.header.channels & 0x08 != 0;
    let long = body.len() >= 120;
    let table: Vec<(&str, usize)> = if orda {
        ORDA_HALFWORDS
            .iter()
            .chain(if long { ORDA_LONG_HALFWORDS } else { &[] })
            .copied()
            .collect()
    } else {
        LEGACY_HALFWORDS.to_vec()
    };
    if let Some((_, number)) = table.iter().find(|(column, _)| *column == name) {
        return Some(vec![f64::from(halfword(body, *number))]);
    }
    let signed = |number: usize| f64::from(halfword(body, number) as i16);
    match name {
        "volume_coverage_pattern" => Some(vec![signed(8)]),
        "horizontal_reflectivity_calibration_correction" if orda => {
            Some(vec![f64::from((halfword(body, 6) as i16) as f32 / 100.0)])
        }
        "vertical_reflectivity_calibration_correction" if orda => {
            Some(vec![f64::from((halfword(body, 23) as i16) as f32 / 100.0)])
        }
        "reflectivity_calibration_correction" if !orda => Some(vec![signed(6)]),
        "alarm_codes" => Some((27..=40).map(|n| f64::from(halfword(body, n))).collect()),
        _ => None,
    }
}

/// Every column of the message 2 table against the bodies; returns the
/// number of messages compared.
fn check_status_table(id: &str, volume: &Volume, bytes: &[u8]) -> usize {
    let statuses: Vec<RawMessage<'static>> = raw_messages(bytes)
        .into_iter()
        .filter(|message| message.header.message_type == 2 && message.body.len() >= 80)
        .collect();
    let Some(layout) = root_variable(volume, "nexrad_rda_status_layout") else {
        assert!(statuses.is_empty(), "{id}: message 2 not carried");
        return 0;
    };
    assert_eq!(layout.values.len(), statuses.len(), "{id}: message 2 count");
    let columns: Vec<&ExtraVariable> = volume
        .extra_vars
        .iter()
        .filter(|v| v.name.starts_with("nexrad_rda_status_"))
        .collect();
    let mut compared = 0;
    for variable in columns {
        let column = variable.name.strip_prefix("nexrad_rda_status_").unwrap();
        let viewed = root_variable(volume, &variable.name).unwrap();
        if column == "layout" {
            let ArrayBuf::Text(texts) = &viewed.values else {
                panic!("layout is text")
            };
            for (text, message) in texts.iter().zip(&statuses) {
                let orda = message.header.channels & 0x08 != 0;
                assert_eq!(&**text, if orda { "orda" } else { "legacy" }, "{id}");
            }
            continue;
        }
        if column == "channels" {
            // Table II halfword 2, high byte, read from the records here.
            let records = messages::record_bytes(bytes).unwrap();
            let expected: Vec<u8> = statuses
                .iter()
                .map(|message| records[message.offset + 2])
                .collect();
            assert_eq!(viewed.values, ArrayBuf::U8(expected), "{id}: channels");
            continue;
        }
        let values = values_f64(&viewed.values);
        if column == "time" {
            for (value, message) in values.iter().zip(&statuses) {
                let ms = (i64::from(message.header.date) - 1) * 86_400_000
                    + i64::from(message.header.milliseconds);
                let expected = (ms - volume.time_reference.timestamp_millis()) as f64 / 1000.0;
                assert_eq!(*value, expected, "{id}: message 2 time");
            }
            continue;
        }
        let width = values.len() / statuses.len();
        for (row, message) in statuses.iter().enumerate() {
            let actual = &values[row * width..(row + 1) * width];
            match status_expected(column, message) {
                Some(expected) => {
                    assert_eq!(actual.len(), expected.len(), "{id} {column}");
                    for (a, e) in actual.iter().zip(&expected) {
                        assert!(same(*a, *e), "{id} message 2 #{row} {column}: {a} != {e}");
                    }
                    compared += 1;
                }
                // A column of the other layout (or of a 60-halfword body):
                // the fill value.
                None => assert!(
                    actual
                        .iter()
                        .all(|v| v.is_nan() || *v == 65535.0 || *v == -32768.0),
                    "{id} {column}: {actual:?} for a message without it"
                ),
            }
        }
    }
    assert!(compared > 0, "{id}: no message 2 value compared");
    // Every Table IV position of the layout is carried.
    for message in &statuses {
        let orda = message.header.channels & 0x08 != 0;
        let names: Vec<&str> = if orda {
            ORDA_HALFWORDS.iter().map(|(n, _)| *n).collect()
        } else {
            LEGACY_HALFWORDS.iter().map(|(n, _)| *n).collect()
        };
        for name in names {
            assert!(
                volume
                    .extra_vars
                    .iter()
                    .any(|v| v.name.as_ref() == format!("nexrad_rda_status_{name}")),
                "{id}: {name} not carried"
            );
        }
    }
    statuses.len()
}

// ---------------------------------------------------------------------------
// Messages 3 and 18

/// Byte offset of a field's location in its body.
fn byte_offset(location: Location) -> usize {
    match location {
        Location::Halfword(number) => (usize::from(number) - 1) * 2,
        Location::HalfwordByte(number, byte) => (usize::from(number) - 1) * 2 + usize::from(byte),
        Location::Byte(offset) => usize::from(offset),
    }
}

/// Text field lengths (ICD Tables V and XV).
fn text_len(name: &str) -> usize {
    match name {
        "rcp_string" => 16,
        "adap_file_name" | "adap_date" | "adap_time" => 12,
        _ => 4,
    }
}

fn trim(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_matches(char::from(0))
        .trim()
        .to_owned()
}

/// The value at `at` read in the type of `like`.
fn read_like(like: &ArrayBuf, body: &[u8], at: usize, count: usize) -> ArrayBuf {
    let step = match like {
        ArrayBuf::U8(_) | ArrayBuf::I8(_) => 1,
        ArrayBuf::U16(_) | ArrayBuf::I16(_) => 2,
        ArrayBuf::U32(_) | ArrayBuf::I32(_) | ArrayBuf::F32(_) => 4,
        ArrayBuf::F64(_) | ArrayBuf::I64(_) => 8,
        ArrayBuf::Text(_) => unreachable!(),
    };
    let offsets = (0..count).map(|index| at + index * step);
    match like {
        ArrayBuf::U8(_) => ArrayBuf::U8(offsets.map(|o| body[o]).collect()),
        ArrayBuf::I8(_) => ArrayBuf::I8(offsets.map(|o| body[o] as i8).collect()),
        ArrayBuf::U16(_) => ArrayBuf::U16(offsets.map(|o| be_u16(body, o)).collect()),
        ArrayBuf::I16(_) => ArrayBuf::I16(offsets.map(|o| be_u16(body, o) as i16).collect()),
        ArrayBuf::U32(_) => ArrayBuf::U32(offsets.map(|o| be_u32(body, o)).collect()),
        ArrayBuf::I32(_) => ArrayBuf::I32(offsets.map(|o| be_u32(body, o) as i32).collect()),
        ArrayBuf::F32(_) => {
            ArrayBuf::F32(offsets.map(|o| f32::from_bits(be_u32(body, o))).collect())
        }
        ArrayBuf::F64(_) => ArrayBuf::F64(
            offsets
                .map(|o| f64::from_be_bytes(body[o..o + 8].try_into().unwrap()))
                .collect(),
        ),
        ArrayBuf::I64(_) => ArrayBuf::I64(
            offsets
                .map(|o| i64::from_be_bytes(body[o..o + 8].try_into().unwrap()))
                .collect(),
        ),
        ArrayBuf::Text(_) => unreachable!(),
    }
}

/// Bitwise equality, so NaN in the file equals NaN in the model.
fn same_bits(a: &ArrayBuf, b: &ArrayBuf) -> bool {
    match (a, b) {
        (ArrayBuf::F32(a), ArrayBuf::F32(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
        }
        (ArrayBuf::F64(a), ArrayBuf::F64(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
        }
        (a, b) => a == b,
    }
}

/// MetPy's value at a location, when it reads that location in the same
/// type.
fn metpy_matches(values: &ArrayBuf, entry: &Value) -> Option<bool> {
    let format = entry["format"].as_str()?;
    let value = &entry["value"];
    let first = |buf: &ArrayBuf| buf.get_f64(0);
    Some(match (format, values) {
        ("H", ArrayBuf::U16(_)) | ("h", ArrayBuf::I16(_)) => value.as_f64()? == first(values)?,
        ("H", ArrayBuf::I16(v)) => value.as_u64()? == u64::from(v[0] as u16),
        ("L" | "l", ArrayBuf::U32(v)) => value.as_i64()? as u32 == v[0],
        ("L" | "l", ArrayBuf::I32(v)) => value.as_i64()? as u32 == v[0] as u32,
        ("f", ArrayBuf::F32(v)) => match value {
            Value::Number(number) => (number.as_f64()? as f32).to_bits() == v[0].to_bits(),
            Value::String(text) if text == "NaN" => v[0].is_nan(),
            _ => return None,
        },
        ("d", ArrayBuf::F64(v)) => value.as_f64()? == v[0],
        (format, ArrayBuf::Text(v)) if format.ends_with('s') => {
            trim(value.as_str()?.as_bytes()) == *v[0]
        }
        _ => return None,
    })
}

/// Every field of a message 3 or 18 against its body and MetPy; returns
/// (fields compared with the body, fields also compared with MetPy).
fn check_fields(
    id: &str,
    volume: &Volume,
    prefix: &str,
    fields: &[MessageField<'_>],
    body: &[u8],
    metpy: Option<&serde_json::Map<String, Value>>,
    metpy_key: fn(Location) -> Option<usize>,
) -> (usize, usize) {
    let mut bytes_compared = 0;
    let mut metpy_compared = 0;
    for field in fields {
        let name = format!("{prefix}{}", field.name);
        let at = byte_offset(field.location);
        let variable = root_variable(volume, &name);
        let expected = match &field.value {
            FieldValue::Flag(_) => {
                let expected = match body[at] {
                    b'T' => Some("T"),
                    b'F' => Some("F"),
                    _ => None,
                };
                let variable = variable.unwrap_or_else(|| panic!("{id}: {name} missing"));
                match expected {
                    Some(flag) => assert_eq!(
                        variable.values,
                        ArrayBuf::Text(vec![flag.into()]),
                        "{id} {name}"
                    ),
                    // Neither "T" nor "F": the stored byte.
                    None => {
                        assert_eq!(variable.values, ArrayBuf::U8(vec![body[at]]), "{id} {name}")
                    }
                }
                bytes_compared += 1;
                continue;
            }
            FieldValue::Text(_) => {
                ArrayBuf::Text(vec![trim(&body[at..at + text_len(field.name)]).into()])
            }
            FieldValue::Scalar(_) | FieldValue::Array(_) => {
                let variable = variable.unwrap_or_else(|| panic!("{id}: {name} missing"));
                if name == "nexrad_adaptation_v_rnscale" {
                    // Elements 0-10 at bytes 8700-8743, 11-12 at 8752-8759.
                    let mut values = values_f64(&read_like(&variable.values, body, 8700, 11));
                    values.extend(values_f64(&read_like(&variable.values, body, 8752, 2)));
                    let actual = values_f64(&variable.values);
                    assert!(
                        actual.iter().zip(&values).all(|(a, e)| same(*a, *e)),
                        "{id} {name}"
                    );
                    bytes_compared += 1;
                    continue;
                }
                read_like(&variable.values, body, at, variable.values.len())
            }
        };
        let variable = variable.unwrap_or_else(|| panic!("{id}: {name} missing"));
        assert!(
            same_bits(&variable.values, &expected),
            "{id} {name}: model {:?}, file {:?}",
            variable.values,
            expected
        );
        bytes_compared += 1;
        if let (Some(metpy), Some(key)) = (metpy, metpy_key(field.location))
            && variable.values.len() == 1
            && let Some(entry) = metpy.get(&key.to_string())
        {
            match metpy_matches(&variable.values, entry) {
                Some(true) => metpy_compared += 1,
                Some(false) => panic!("{id} {name}: MetPy {entry}, ours {:?}", variable.values),
                None => {}
            }
        }
        // Units and the ICD location travel with the value.
        let attr = |key: &str| {
            variable
                .attrs
                .iter()
                .find(|(k, _)| &**k == key)
                .map(|(_, v)| v.clone())
        };
        if !field.units.is_empty() {
            assert_eq!(
                attr("units"),
                Some(AttrValue::text(field.units)),
                "{id} {name}"
            );
        }
        assert!(attr("comment").is_some(), "{id} {name}");
    }
    (bytes_compared, metpy_compared)
}

fn first_body(messages: &[RawMessage<'_>], kind: u8) -> Option<Vec<u8>> {
    messages
        .iter()
        .find(|message| message.header.message_type == kind)
        .map(|message| message.body.to_vec())
}

#[test]
fn status_performance_and_adaptation_reach_the_model_and_the_view() {
    let ids = golden_ids("status");
    let mut checked = 0;
    let mut totals = [0usize; 5];
    let mut sources = Vec::new();
    for id in &ids {
        sources.push(source_ids(id));
        let Some(bytes) = load(id) else {
            continue;
        };
        checked += 1;
        let volume = match read_volume_from_bytes(&bytes) {
            Ok(volume) => volume,
            Err(error) => {
                // The status-only stub has no radials to build a volume
                // from; NexradMetadata reads its message 2.
                assert!(id.ends_with("-stub"), "{id}: {error}");
                continue;
            }
        };
        let golden = golden("status", id);
        totals[0] += check_status_table(id, &volume, &bytes);

        // The first message 2 against MetPy's codes.
        if let Some(codes) = golden["message_2"]["codes"].as_object() {
            for (key, column) in [
                ("rda_status", "rda_status"),
                ("op_status", "operability_status"),
                ("control_status", "control_status"),
                ("op_mode", "operational_mode"),
                ("rda_build", "rda_build_number"),
            ] {
                let Some(code) = codes.get(key).and_then(Value::as_f64) else {
                    continue;
                };
                let Some(variable) = root_variable(&volume, &format!("nexrad_rda_status_{column}"))
                else {
                    continue;
                };
                assert_eq!(values_f64(&variable.values)[0], code, "{id} {key}");
                totals[1] += 1;
            }
        }

        let messages = raw_messages(&bytes);
        let metadata = NexradMetadata::from_metadata_record(&bytes);
        if let (Some(performance), Some(body)) = (&metadata.performance, first_body(&messages, 3)) {
            let (bytes_compared, metpy) = check_fields(
                id,
                &volume,
                "nexrad_performance_",
                &performance_fields(performance),
                &body,
                golden["message_3"]["halfwords"].as_object(),
                |location| match location {
                    Location::Halfword(number) => Some(usize::from(number)),
                    _ => None,
                },
            );
            totals[2] += bytes_compared;
            totals[3] += metpy;
        } else {
            assert!(
                !volume
                    .extra_vars
                    .iter()
                    .any(|v| v.name.starts_with("nexrad_performance_")),
                "{id}"
            );
        }
        if let (Some(adaptation), Some(body)) = (&metadata.adaptation, first_body(&messages, 18)) {
            let (bytes_compared, metpy) = check_fields(
                id,
                &volume,
                "nexrad_adaptation_",
                &adaptation_fields(adaptation),
                &body,
                golden["message_18"]["bytes"].as_object(),
                |location| match location {
                    Location::Byte(offset) => Some(usize::from(offset)),
                    _ => None,
                },
            );
            totals[2] += bytes_compared;
            totals[4] += metpy;
        }
    }
    common::assert_checked_every_available("status passthrough", checked, &sources);
    eprintln!(
        "message 2 rows {}, MetPy codes {}, message 3/18 fields vs bytes {}, vs MetPy: message 3 {}, message 18 {}",
        totals[0], totals[1], totals[2], totals[3], totals[4]
    );
    if checked == ids.len() {
        // Every source available: the counts are pinned, so a value that
        // stops being carried (or compared) fails here.
        assert_eq!(
            totals,
            [81, 131, 9555, 4641, 3402],
            "message 2 rows, MetPy codes, message 3/18 fields vs bytes, vs MetPy (3, 18)"
        );
    }
}

// ---------------------------------------------------------------------------
// Message 5

fn signed_elevation(angle: f64) -> f64 {
    if angle > 90.0 { angle - 360.0 } else { angle }
}

#[test]
fn vcp_reaches_the_model_the_view_and_the_sweep_scan_rates() {
    let ids = golden_ids("vcp");
    let mut checked = 0;
    let mut cuts_compared = 0;
    let mut rates = 0;
    let mut sources = Vec::new();
    for id in &ids {
        sources.push(source_ids(id));
        let Some(bytes) = load(id) else {
            continue;
        };
        checked += 1;
        let volume = read_volume_from_bytes(&bytes).unwrap();
        let golden = golden("vcp", id);
        let expected = &golden["message_5"];
        if expected.is_null() {
            // The zero-filled message 5 of KLIX 2005 decodes to nothing.
            assert!(root_variable(&volume, "nexrad_vcp_pattern_number").is_none());
            continue;
        }
        let scalar = |name: &str| {
            values_f64(
                &root_variable(&volume, &format!("nexrad_vcp_{name}"))
                    .unwrap()
                    .values,
            )[0]
        };
        for (name, key) in [
            ("message_size", "size_hw"),
            ("pattern_type", "pattern_type"),
            ("pattern_number", "num"),
            ("number_of_cuts", "num_el_cuts"),
            ("version", "vcp_version"),
            ("clutter_map_group", "clutter_map_group"),
            ("doppler_velocity_resolution", "dop_res_code"),
            ("pulse_width", "pulse_width_code"),
            ("sequencing", "vcp_sequencing"),
            ("supplemental_data", "vcp_supplemental_info"),
        ] {
            assert_eq!(scalar(name), expected[key].as_f64().unwrap(), "{id} {name}");
        }
        let golden_cuts = expected["els"].as_array().unwrap();
        let column = |name: &str| {
            values_f64(
                &root_variable(&volume, &format!("nexrad_vcp_{name}"))
                    .unwrap()
                    .values,
            )
        };
        let per_cut: [(&str, &str, bool); 15] = [
            ("elevation_angle", "el_angle", true),
            ("channel_configuration", "channel_config", false),
            ("waveform_type", "waveform", false),
            ("super_resolution_control", "super_res", false),
            ("surveillance_prf_number", "surv_prf_num", false),
            ("surveillance_pulse_count", "surv_pulse_count", false),
            ("azimuth_rate", "az_rate", false),
            ("snr_threshold_reflectivity", "ref_thresh", false),
            ("snr_threshold_velocity", "vel_thresh", false),
            ("snr_threshold_spectrum_width", "sw_thresh", false),
            (
                "snr_threshold_differential_reflectivity",
                "zdr_thresh",
                false,
            ),
            ("snr_threshold_differential_phase", "phidp_thresh", false),
            (
                "snr_threshold_correlation_coefficient",
                "rhohv_thresh",
                false,
            ),
            ("cut_supplemental_data", "supplemental_data", false),
            ("ebc_angle", "ebc_angle", true),
        ];
        for (name, key, angle) in per_cut {
            let values = column(name);
            assert_eq!(values.len(), golden_cuts.len(), "{id} {name}");
            for (value, cut) in values.iter().zip(golden_cuts) {
                let mut want = cut[key].as_f64().unwrap();
                if angle {
                    want = signed_elevation(want);
                }
                assert_eq!(*value, want, "{id} {name}");
            }
        }
        for (name, key) in [
            ("doppler_edge_angle", "edge"),
            ("doppler_prf_number", "doppler_prf_num"),
            ("doppler_pulse_count", "pulse_count"),
        ] {
            let values = column(name);
            assert_eq!(values.len(), golden_cuts.len() * 3, "{id} {name}");
            for (index, cut) in golden_cuts.iter().enumerate() {
                for sector in 0..3 {
                    let want = cut[format!("sector{}_{key}", sector + 1)].as_f64().unwrap();
                    assert_eq!(values[index * 3 + sector], want, "{id} {name}");
                }
            }
        }
        cuts_compared += golden_cuts.len();
        // Each sweep's FM301 target scan rate is its cut's azimuth rate.
        let view = fm301::volume_view(&volume, ALL, None).unwrap();
        for (index, sweep) in volume.sweeps.iter().enumerate() {
            let cut = sweep
                .elevation_number
                .and_then(|number| golden_cuts.get(usize::from(number) - 1));
            let want = cut.map(|cut| cut["az_rate"].as_f64().unwrap() as f32);
            assert_eq!(sweep.target_scan_rate_deg_per_s, want, "{id} sweep {index}");
            let viewed = view
                .group(&format!("sweep_{index}"))
                .unwrap()
                .variable("target_scan_rate")
                .map(|v| v.values.clone());
            assert_eq!(
                viewed,
                want.map(|rate| fm301::Values::Scalar(recast_radar_core::model::Scalar::F32(rate))),
                "{id} sweep {index}"
            );
            rates += usize::from(want.is_some());
        }
    }
    common::assert_checked_every_available("vcp passthrough", checked, &sources);
    eprintln!("VCP cuts compared {cuts_compared}, sweep scan rates {rates}");
    if checked == ids.len() {
        assert!(cuts_compared >= 300, "{cuts_compared}");
        assert!(rates >= 300, "{rates}");
    }
}

// ---------------------------------------------------------------------------
// Messages 13, 15 and 32

#[test]
fn clutter_maps_and_prf_data_reach_the_model_and_the_view() {
    let ids = golden_ids("clutter");
    let mut checked = 0;
    let mut sources = Vec::new();
    let (mut zone_azimuths, mut bypass_segments) = (0usize, 0usize);
    for id in &ids {
        sources.push(source_ids(id));
        let Some(bytes) = load(id) else {
            continue;
        };
        checked += 1;
        let volume = read_volume_from_bytes(&bytes).unwrap();
        let golden = golden("clutter", id);
        let messages = raw_messages(&bytes);

        // Message 15 against MetPy's azimuth runs.
        match golden["clutter_filter_map"].as_object() {
            Some(map) => {
                let counts = root_variable(&volume, "nexrad_clutter_filter_map_zone_count")
                    .expect("message 15 carried");
                let counts = values_f64(&counts.values);
                let ops = values_f64(
                    &root_variable(&volume, "nexrad_clutter_filter_map_op_code")
                        .unwrap()
                        .values,
                );
                let ends = values_f64(
                    &root_variable(&volume, "nexrad_clutter_filter_map_end_range")
                        .unwrap()
                        .values,
                );
                let segments = map["elevation_segments"].as_u64().unwrap() as usize;
                assert_eq!(counts.len(), segments * 360, "{id}");
                let mut cursor = 0;
                for (segment, runs) in map["azimuth_runs"].as_array().unwrap().iter().enumerate() {
                    for run in runs.as_array().unwrap() {
                        let first = run[0].as_u64().unwrap() as usize;
                        let last = run[1].as_u64().unwrap() as usize;
                        let zones = run[2].as_array().unwrap();
                        for azimuth in first..=last {
                            assert_eq!(
                                counts[segment * 360 + azimuth],
                                zones.len() as f64,
                                "{id} segment {segment} azimuth {azimuth}"
                            );
                            for zone in zones {
                                assert_eq!(ops[cursor], zone[0].as_f64().unwrap(), "{id}");
                                assert_eq!(ends[cursor], zone[1].as_f64().unwrap(), "{id}");
                                cursor += 1;
                            }
                            zone_azimuths += 1;
                        }
                    }
                }
                assert_eq!(cursor, ops.len(), "{id}: every zone compared");
            }
            None => assert!(
                root_variable(&volume, "nexrad_clutter_filter_map_zone_count").is_none(),
                "{id}"
            ),
        }

        // Message 13 against its body: date, time, count, then per segment
        // the segment number and 360 radials of 32 halfwords (current
        // layout; the legacy layout has 256 radials and no date or time).
        match first_body(&messages, 13) {
            Some(body) => {
                let Some(bins) = root_variable(&volume, "nexrad_bypass_map_bins") else {
                    // A bypass map that does not decode (KLIX 2005's orphan
                    // run joins nothing) is not carried.
                    continue;
                };
                let bins = values_f64(&bins.values);
                let numbers = values_f64(
                    &root_variable(&volume, "nexrad_bypass_map_segment_number")
                        .unwrap()
                        .values,
                );
                let legacy = (1..=5).contains(&be_u16(&body, 0));
                let (radials, mut cursor) = if legacy { (256, 2) } else { (360, 6) };
                if !legacy {
                    let date = values_f64(
                        &root_variable(&volume, "nexrad_bypass_map_generation_date")
                            .unwrap()
                            .values,
                    );
                    assert_eq!(date[0], f64::from(be_u16(&body, 0)), "{id}");
                }
                let mut index = 0;
                for (segment, number) in numbers.iter().enumerate() {
                    assert_eq!(*number, f64::from(be_u16(&body, cursor)), "{id}");
                    cursor += 2;
                    for _ in 0..radials * 32 {
                        assert_eq!(
                            bins[index],
                            f64::from(be_u16(&body, cursor)),
                            "{id} {segment}"
                        );
                        index += 1;
                        cursor += 2;
                    }
                    bypass_segments += 1;
                }
                assert_eq!(index, bins.len(), "{id}");
            }
            None => assert!(root_variable(&volume, "nexrad_bypass_map_bins").is_none()),
        }

        // Message 32 against its body: halfword 1 the waveform count,
        // halfword 2 spare, then per waveform the type, the PRF count and
        // the PRFs as 32-bit integers.
        match first_body(&messages, 32) {
            Some(body) => {
                let types = values_f64(
                    &root_variable(&volume, "nexrad_prf_waveform_type")
                        .unwrap()
                        .values,
                );
                let prfs = values_f64(&root_variable(&volume, "nexrad_prf_value").unwrap().values);
                let count = usize::from(be_u16(&body, 0));
                assert_eq!(types.len(), count, "{id}");
                let width = prfs.len() / count;
                let mut cursor = 4;
                for (waveform, kind) in types.iter().enumerate() {
                    assert_eq!(*kind, f64::from(be_u16(&body, cursor)), "{id}");
                    let n = usize::from(be_u16(&body, cursor + 2));
                    cursor += 4;
                    for prf in 0..n {
                        assert_eq!(
                            prfs[waveform * width + prf],
                            f64::from(be_u32(&body, cursor)),
                            "{id} waveform {waveform} PRF {prf}"
                        );
                        cursor += 4;
                    }
                }
            }
            None => assert!(root_variable(&volume, "nexrad_prf_value").is_none(), "{id}"),
        }
    }
    common::assert_checked_every_available("clutter passthrough", checked, &sources);
    eprintln!("clutter filter map azimuths {zone_azimuths}, bypass map segments {bypass_segments}");
    if checked == ids.len() {
        assert!(zone_azimuths >= 20 * 360, "{zone_azimuths}");
        assert!(bypass_segments >= 10, "{bypass_segments}");
    }
}

/// The variables of every message kind the corpus holds, counted per file:
/// a sanity check that the decoder does not drop a whole message.
#[test]
fn every_metadata_message_in_the_metadata_record_is_carried() {
    let prefixes: BTreeMap<u8, &str> = [
        (2, "nexrad_rda_status_"),
        (3, "nexrad_performance_"),
        (5, "nexrad_vcp_"),
        (13, "nexrad_bypass_map_"),
        (15, "nexrad_clutter_filter_map_"),
        (18, "nexrad_adaptation_"),
        (32, "nexrad_prf_"),
    ]
    .into_iter()
    .collect();
    let mut checked = 0;
    for id in [
        "l2-ktlx-20240315-000217-trim",
        "l2-kdvn-20200810-180401-trim",
    ] {
        let Some(bytes) = common::load(id) else {
            continue;
        };
        checked += 1;
        let volume = read_volume_from_bytes(&bytes).unwrap();
        let metadata = NexradMetadata::from_metadata_record(&bytes);
        let present: [(u8, bool); 7] = [
            (2, metadata.rda_status.is_some()),
            (3, metadata.performance.is_some()),
            (5, metadata.vcp.is_some()),
            (13, metadata.bypass_map.is_some()),
            (15, metadata.clutter_filter_map.is_some()),
            (18, metadata.adaptation.is_some()),
            (32, metadata.prf.is_some()),
        ];
        for (kind, decoded) in present {
            let prefix = prefixes[&kind];
            let carried = volume.extra_vars.iter().any(|v| v.name.starts_with(prefix));
            assert_eq!(carried, decoded, "{id}: message {kind}");
        }
    }
    assert_eq!(checked, 2);
}
