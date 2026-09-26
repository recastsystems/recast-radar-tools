//! Level II metadata the volume carries beyond the typed tables that
//! `metadata_passthrough_real.rs` checks (`src/passthrough.rs`):
//!
//! - the generation time and the channel byte in the message header of
//!   every carried message 3, 5, 13, 15, 18 and 32 (`<prefix>message_time`,
//!   `<prefix>message_channels`), against the header at the message's
//!   offset in the file (ICD 2620002AA Table II halfwords 2 and 4 to 6);
//! - the body bytes after a clutter filter map
//!   (`nexrad_clutter_filter_map_trailing_bytes`): their count against the
//!   halfwords MetPy 1.7.1 reports left over ("left data -- Used: ...
//!   Avail: ..."), their bytes against the file's message 15 body;
//! - legacy RDA messages 3 and 18, whose layouts the decoders do not read,
//!   verbatim (`nexrad_unparsed_message_*`): every frame against the file's
//!   frames, in KLIX 2005;
//! - the message header of every non-radial frame (the
//!   `nexrad_metadata_message_*` table): type, channel byte, size, sequence
//!   number, generation date and time, segment count and number, against
//!   the frames this test walks in the file (Table II framing);
//!
//! and, on real bytes rearranged because no real file has the input
//! (synthetic inputs: the helpers are named `fabricate_*`, and
//! `testdata/synthetic-allowlist.toml` lists them and their tests as pending
//! the owner's decision):
//!
//! - a second, different message of each type (the metadata record of
//!   another real volume inserted after the first's; no real file has two),
//!   carried as `_message1` copies equal to what that volume carries on its
//!   own, and a repeated one counted;
//! - messages 6, 9, 11 and 12, which no real file holds (none in the Level
//!   II files of the corpus and the test-data cache), as
//!   real message 2 frames relabelled, carried with the halfwords at their
//!   ICD positions.
//!
//! Every value is read from the model and from the FM301 view with every
//! passthrough item, which must agree.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use recast_radar_core::fm301::{self, FirstDim, Flavor, Passthrough, ViewOptions};
use recast_radar_core::model::{ArrayBuf, AttrValue, ExtraVariable, Volume};
use recast_radar_io_nexrad::messages::{self, MessageBody, RawMessages};
use recast_radar_io_nexrad::{
    ArchiveCompression, normalize_archive_bytes, read_normalized_volume_bytes,
    read_volume_from_bytes,
};
use serde_json::Value;

const ALL: ViewOptions = ViewOptions {
    flavor: Flavor::Wmo2022,
    first_dim: FirstDim::Time,
    passthrough: Passthrough::All,
};

const VOLUME_HEADER_LEN: usize = 24;
const CONTROL_WORD_LEN: usize = 12;
const MESSAGE_HEADER_LEN: usize = 16;
const RECORD_BYTES: usize = 2432;
const METADATA_RECORDS: usize = 134;

/// Each carried single message and its variable prefix.
const SINGLE_MESSAGES: [(u8, &str); 8] = [
    (3, "nexrad_performance_"),
    (5, "nexrad_vcp_"),
    (7, "nexrad_vcp_"),
    (8, "nexrad_clutter_censor_"),
    (13, "nexrad_bypass_map_"),
    (15, "nexrad_clutter_filter_map_"),
    (18, "nexrad_adaptation_"),
    (32, "nexrad_prf_"),
];

fn golden_dir(group: &str) -> std::path::PathBuf {
    recast_radar_testdata::testdata_dir().join(format!("level2/golden/{group}"))
}

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

/// The KIWA start chunk golden is decoded with the next two chunks.
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

/// A root variable from the model and the view, which must agree.
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
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Epoch milliseconds of the Table II date (halfword 4, days with 1 January
/// 1970 = 1) and time (halfwords 5 and 6, ms after midnight) of the message
/// header at `header` in `records`, read from the bytes.
fn header_epoch_ms(records: &[u8], header: usize) -> i64 {
    let date = i64::from(be_u16(records, header + 6));
    let ms = i64::from(be_u32(records, header + 8));
    (date - 1) * 86_400_000 + ms
}

fn seconds_since(volume: &Volume, epoch_ms: i64) -> f64 {
    (epoch_ms - volume.time_reference.timestamp_millis()) as f64 / 1000.0
}

fn scalar_f64(variable: &ExtraVariable) -> f64 {
    assert_eq!(variable.values.len(), 1, "{}", variable.name);
    variable.values.get_f64(0).unwrap()
}

/// The generation time of the first carried message of each type is its
/// header's, read from the file.
#[test]
fn message_header_times_reach_the_model() {
    let ids = golden_ids("status");
    let mut checked = 0;
    let mut sources = Vec::new();
    let mut compared = 0;
    for id in &ids {
        sources.push(source_ids(id));
        let Some(bytes) = load(id) else {
            continue;
        };
        checked += 1;
        let Ok(volume) = read_volume_from_bytes(&bytes) else {
            // The status-only stub has no radials to build a volume from.
            assert!(id.ends_with("-stub"), "{id}");
            continue;
        };
        let records = messages::record_bytes(&bytes).unwrap().into_owned();
        let mut seen = Vec::new();
        for message in RawMessages::new(&records).filter_map(Result::ok) {
            let kind = message.header.message_type;
            let Some((_, prefix)) = SINGLE_MESSAGES.iter().find(|(k, _)| *k == kind) else {
                continue;
            };
            // The first message of the prefix that decodes is the carried
            // one; its header time is read here from the bytes.
            let decoded = message.clone().decode();
            if seen.contains(prefix)
                || !matches!(decoded, Ok((_, ref body)) if !matches!(body, MessageBody::Unparsed(_)))
            {
                continue;
            }
            seen.push(*prefix);
            let variable = root_variable(&volume, &format!("{prefix}message_time"))
                .unwrap_or_else(|| panic!("{id}: {prefix}message_time missing"));
            let expected = if be_u16(&records, message.offset + 6) == 0 {
                f64::NAN
            } else {
                seconds_since(&volume, header_epoch_ms(&records, message.offset))
            };
            let actual = scalar_f64(variable);
            assert!(
                actual == expected || (actual.is_nan() && expected.is_nan()),
                "{id} {prefix}: {actual} != {expected}"
            );
            let units = variable.attrs.iter().find(|(k, _)| &**k == "units");
            assert!(
                matches!(units, Some((_, AttrValue::Text(text))) if text.starts_with("seconds since ")),
                "{id} {prefix}"
            );
            // The channel byte, Table II halfword 2 high byte.
            let channels = root_variable(&volume, &format!("{prefix}message_channels"))
                .unwrap_or_else(|| panic!("{id}: {prefix}message_channels missing"));
            assert_eq!(
                channels.values,
                ArrayBuf::U8(vec![records[message.offset + 2]]),
                "{id} {prefix}: channels"
            );
            compared += 1;
        }
        // No message time without its message.
        for (_, prefix) in SINGLE_MESSAGES {
            let carried = volume.extra_vars.iter().any(|v| {
                v.name.starts_with(prefix)
                    && !v.name.ends_with("message_time")
                    && !v.name.ends_with("message_channels")
            });
            let timed = volume
                .extra_vars
                .iter()
                .any(|v| *v.name == *format!("{prefix}message_time"));
            assert_eq!(carried, timed, "{id} {prefix}");
        }
    }
    common::assert_checked_every_available("message times", checked, &sources);
    eprintln!("message header times compared: {compared}");
    if checked == ids.len() {
        assert_eq!(compared, 98, "message header times compared");
    }
}

/// The message header of every non-radial message frame of normalized
/// Archive II bytes, in file order, walked here by the Table II framing: 134
/// fixed 2432-byte metadata frames (an empty one is skipped), then records
/// until an empty frame; a size of 65535 (the size in bytes in halfwords
/// 7-8) or message 29 frames the message by its size, as does a message 31
/// after the metadata record, or inside it when the next message 31 follows
/// directly (as some converted files store it); every other message fills a
/// fixed frame. Each header is its eight fields as stored: type, channel
/// byte, size, sequence number, date, milliseconds, segments, segment
/// number.
fn non_radial_headers(bytes: &[u8]) -> Vec<[u64; 8]> {
    let mut headers = Vec::new();
    let mut cursor = VOLUME_HEADER_LEN;
    let mut frame = 0usize;
    // Once a message 31 inside the metadata record is variable-framed, every
    // later one is.
    let mut early_variable = false;
    while cursor + CONTROL_WORD_LEN + MESSAGE_HEADER_LEN <= bytes.len() {
        let header = cursor + CONTROL_WORD_LEN;
        let size = be_u16(bytes, header);
        let kind = bytes[header + 3];
        if size == 0 {
            if frame >= METADATA_RECORDS {
                break;
            }
            cursor += RECORD_BYTES;
            frame += 1;
            continue;
        }
        let length = if size == 0xFFFF {
            (usize::from(be_u16(bytes, header + 12)) << 16)
                | usize::from(be_u16(bytes, header + 14))
        } else {
            usize::from(size) * 2
        };
        let next_is_message_31 = |at: usize| {
            at == bytes.len()
                || bytes
                    .get(at + CONTROL_WORD_LEN + 3)
                    .is_some_and(|next_kind| *next_kind == 31)
        };
        let variable = size == 0xFFFF
            || kind == 29
            || (kind == 31
                && (frame >= METADATA_RECORDS
                    || early_variable
                    || next_is_message_31(header + length)));
        early_variable |= kind == 31 && variable;
        if kind != 1 && kind != 31 {
            headers.push([
                u64::from(kind),
                u64::from(bytes[header + 2]),
                u64::from(size),
                u64::from(be_u16(bytes, header + 4)),
                u64::from(be_u16(bytes, header + 6)),
                u64::from(be_u32(bytes, header + 8)),
                u64::from(be_u16(bytes, header + 12)),
                u64::from(be_u16(bytes, header + 14)),
            ]);
        }
        cursor = if variable {
            header + length
        } else {
            cursor + RECORD_BYTES
        };
        frame += 1;
    }
    headers
}

/// The `nexrad_metadata_message_*` table: one entry per non-radial frame,
/// each field as the file stores it, in the model and the view.
#[test]
fn every_non_radial_message_header_reaches_the_model() {
    const COLUMNS: [&str; 8] = [
        "nexrad_metadata_message_type",
        "nexrad_metadata_message_channels",
        "nexrad_metadata_message_size",
        "nexrad_metadata_message_sequence_number",
        "nexrad_metadata_message_date",
        "nexrad_metadata_message_milliseconds",
        "nexrad_metadata_message_segments",
        "nexrad_metadata_message_segment_number",
    ];
    let ids = golden_ids("status");
    let mut checked = 0;
    let mut frames = 0;
    let mut sources = Vec::new();
    for id in &ids {
        sources.push(source_ids(id));
        let Some(raw) = load(id) else { continue };
        let (bytes, _) = normalize_archive_bytes(&raw).unwrap();
        let expected = non_radial_headers(&bytes);
        let volume = read_volume_from_bytes(&raw).unwrap();
        let columns: Vec<Vec<u64>> = COLUMNS
            .iter()
            .map(|name| {
                let variable =
                    root_variable(&volume, name).unwrap_or_else(|| panic!("{id}: {name} missing"));
                assert_eq!(
                    variable.dims,
                    vec![Box::<str>::from("nexrad_metadata_message")],
                    "{id} {name}"
                );
                (0..variable.values.len())
                    .map(|index| variable.values.get_f64(index).unwrap() as u64)
                    .collect()
            })
            .collect();
        assert_eq!(columns[0].len(), expected.len(), "{id}: frame count");
        for (index, header) in expected.iter().enumerate() {
            for (column, (name, value)) in columns.iter().zip(COLUMNS.iter().zip(header)) {
                assert_eq!(column[index], *value, "{id} frame {index} {name}");
            }
        }
        frames += expected.len();
        checked += 1;
    }
    common::assert_checked_every_available("metadata message headers", checked, &sources);
    assert!(frames > 0, "{frames} frames in {checked} files");
}

/// MetPy's report of a message 15 with more body than its map: halfwords
/// used and available.
fn metpy_left_halfwords(golden: &Value) -> Option<usize> {
    golden["metpy_log"].as_array()?.iter().find_map(|line| {
        let line = line.as_str()?;
        let rest = line.strip_prefix("Message 15 left data -- Used: ")?;
        let (used, avail) = rest.split_once(" Avail: ")?;
        let used: usize = used.trim().parse().ok()?;
        let avail: usize = avail.trim().parse().ok()?;
        Some(avail - used)
    })
}

/// The body bytes after each clutter filter map: as many as MetPy leaves
/// unread, equal to the end of the file's message 15 body.
#[test]
fn bytes_after_the_clutter_filter_map_reach_the_model() {
    let ids = golden_ids("clutter");
    let mut checked = 0;
    let mut sources = Vec::new();
    let mut with_trailing = 0;
    for id in &ids {
        sources.push(vec![id.clone()]);
        let Some(bytes) = common::load(id) else {
            continue;
        };
        checked += 1;
        let volume = read_volume_from_bytes(&bytes).unwrap();
        let name = "nexrad_clutter_filter_map_trailing_bytes";
        let carried = root_variable(&volume, name);
        let Some(left) = metpy_left_halfwords(&golden("clutter", id)) else {
            assert!(carried.is_none(), "{id}: MetPy reads the whole map");
            continue;
        };
        let carried = carried.unwrap_or_else(|| panic!("{id}: {name} missing"));
        let ArrayBuf::U8(trailing) = &carried.values else {
            panic!("{id}: {name} is bytes")
        };
        assert_eq!(
            trailing.len(),
            left * 2,
            "{id}: MetPy's left-over halfwords"
        );
        let records = messages::record_bytes(&bytes).unwrap().into_owned();
        let body = RawMessages::new(&records)
            .filter_map(Result::ok)
            .find(|m| m.header.message_type == 15)
            .unwrap()
            .body;
        assert_eq!(trailing[..], body[body.len() - trailing.len()..], "{id}");
        assert_eq!(
            carried.dims[0].as_ref(),
            "nexrad_clutter_filter_map_trailing_byte"
        );
        with_trailing += 1;
    }
    common::assert_checked_every_available("clutter trailing bytes", checked, &sources);
    if checked == ids.len() {
        // KPAH 2008: 77 joined segments, a 5403-halfword map, 172800 bytes
        // after it.
        assert_eq!(with_trailing, 1);
    }
}

/// The frames of every legacy RDA message 3 and 18 in the file: each frame
/// from its message header to the frame end, in file order.
fn legacy_frames(records: &[u8]) -> Vec<(u8, u8, Vec<u8>)> {
    let mut out = Vec::new();
    for message in RawMessages::new(records).filter_map(Result::ok) {
        let kind = message.header.message_type;
        let legacy = message.header.channels & 0x08 == 0;
        if !(legacy && matches!(kind, 3 | 18)) {
            continue;
        }
        let mut frames = Vec::new();
        for frame in 0..message.frames {
            let start = message.offset + frame * RECORD_BYTES;
            let end = (start - CONTROL_WORD_LEN + RECORD_BYTES).min(records.len());
            frames.extend_from_slice(&records[start..end]);
        }
        out.push((kind, message.header.channels, frames));
    }
    out
}

#[test]
fn legacy_messages_3_and_18_reach_the_model_verbatim() {
    let ids = ["l2-klix-20050829-130035-trim"];
    let mut checked = 0;
    for id in ids {
        let bytes = common::load(id).expect("committed");
        checked += 1;
        let volume = read_volume_from_bytes(&bytes).unwrap();
        let records = messages::record_bytes(&bytes).unwrap().into_owned();
        let expected = legacy_frames(&records);
        assert!(!expected.is_empty(), "{id}: a legacy message 3 or 18");
        let kinds = root_variable(&volume, "nexrad_unparsed_message_type").unwrap();
        assert_eq!(
            kinds.values,
            ArrayBuf::U8(expected.iter().map(|(kind, _, _)| *kind).collect()),
            "{id}"
        );
        let channels = root_variable(&volume, "nexrad_unparsed_message_channels").unwrap();
        assert_eq!(
            channels.values,
            ArrayBuf::U8(expected.iter().map(|(_, channels, _)| *channels).collect()),
            "{id}"
        );
        let lengths = root_variable(&volume, "nexrad_unparsed_message_length").unwrap();
        assert_eq!(
            lengths.values,
            ArrayBuf::U32(
                expected
                    .iter()
                    .map(|(_, _, frames)| frames.len() as u32)
                    .collect()
            ),
            "{id}"
        );
        let frames = root_variable(&volume, "nexrad_unparsed_message_frames").unwrap();
        // The LDM block decoder and the decoder of normalized bytes keep the
        // same frames.
        let (normalized, compression) = normalize_archive_bytes(&bytes).unwrap();
        let other = read_normalized_volume_bytes(&normalized, compression).unwrap();
        assert_eq!(
            root_variable(&other, "nexrad_unparsed_message_frames"),
            Some(frames),
            "{id}"
        );
        assert_eq!(
            frames.values,
            ArrayBuf::U8(
                expected
                    .iter()
                    .flat_map(|(_, _, frames)| frames.iter().copied())
                    .collect()
            ),
            "{id}"
        );
        let times = root_variable(&volume, "nexrad_unparsed_message_time").unwrap();
        for (index, (_, _, frames)) in expected.iter().enumerate() {
            let actual = times.values.get_f64(index).unwrap();
            if be_u16(frames, 6) == 0 {
                // KLIX 2005's legacy messages have no header date.
                assert!(actual.is_nan(), "{id}");
            } else {
                assert_eq!(
                    actual,
                    seconds_since(&volume, header_epoch_ms(frames, 0)),
                    "{id}"
                );
            }
        }
    }
    assert_eq!(checked, 1);
}

// ---------------------------------------------------------------------------
// Real bytes rearranged

/// Normalized bytes of a real volume.
fn normalized(ids: &[&str]) -> (Vec<u8>, ArchiveCompression) {
    let raw = common::load_all(ids).expect("committed");
    normalize_archive_bytes(&raw).unwrap()
}

/// The frames of the metadata record (the 134 fixed frames after the
/// volume header) that hold a message. Empty frames are left out: after the
/// metadata record an empty frame ends the volume.
fn metadata_record(bytes: &[u8]) -> Vec<u8> {
    bytes[VOLUME_HEADER_LEN..VOLUME_HEADER_LEN + METADATA_RECORDS * RECORD_BYTES]
        .chunks_exact(RECORD_BYTES)
        .filter(|frame| be_u16(frame, CONTROL_WORD_LEN) != 0)
        .flatten()
        .copied()
        .collect()
}

/// Rearranged real bytes, a synthetic input: `extra` frames inserted after
/// the metadata record of `bytes`.
fn fabricate_frames_after_metadata(bytes: &[u8], extra: &[u8]) -> Vec<u8> {
    let at = VOLUME_HEADER_LEN + METADATA_RECORDS * RECORD_BYTES;
    let mut out = bytes[..at].to_vec();
    out.extend_from_slice(extra);
    out.extend_from_slice(&bytes[at..]);
    out
}

const KIWA_CHUNKS: [&str; 3] = [
    "l2chunk-kiwa-307-20260917-003629-001-s",
    "l2chunk-kiwa-307-20260917-003629-002-i",
    "l2chunk-kiwa-307-20260917-003629-003-i",
];

/// The same variable renamed as copy 1 of its message type.
fn as_copy_1(variable: &ExtraVariable) -> ExtraVariable {
    let rename = |name: &str| -> Box<str> { format!("{name}_message1").into_boxed_str() };
    ExtraVariable {
        name: rename(&variable.name),
        dims: variable.dims.iter().map(|dim| rename(dim)).collect(),
        shape: variable.shape.clone(),
        values: variable.values.clone(),
        attrs: variable
            .attrs
            .iter()
            .map(|(key, value)| match (key.as_ref(), value) {
                ("sample_dimension", AttrValue::Text(dim)) => {
                    (key.clone(), AttrValue::Text(rename(dim)))
                }
                _ => (key.clone(), value.clone()),
            })
            .collect(),
    }
}

/// The metadata record of the real KTLX 2024 volume inserted after the real
/// KIWA 2026 one: each of its messages 3, 5, 13, 15, 18 and 32 differs from
/// KIWA's and is carried as copy 1, equal to what KTLX carries alone; the
/// first copies and the sweeps' VCP values stay KIWA's.
#[test]
fn a_later_different_message_is_carried_as_a_copy() {
    let (kiwa, compression) = normalized(&KIWA_CHUNKS);
    assert_eq!(compression, ArchiveCompression::Bzip2Blocks);
    let (ktlx, _) = normalized(&["l2-ktlx-20240315-000217-trim"]);
    let alone_kiwa = read_normalized_volume_bytes(&kiwa, compression).unwrap();
    let alone_ktlx =
        read_volume_from_bytes(&common::load("l2-ktlx-20240315-000217-trim").unwrap()).unwrap();
    let both = read_normalized_volume_bytes(
        &fabricate_frames_after_metadata(&kiwa, &metadata_record(&ktlx)),
        compression,
    )
    .unwrap();
    let rays = |volume: &Volume| volume.provenance.decode.decoded_ray_count;
    assert_eq!(rays(&both), rays(&alone_kiwa));

    let shift =
        (alone_ktlx.time_reference - both.time_reference).num_milliseconds() as f64 / 1000.0;
    // A KTLX variable as the combined volume carries it: `renamed` for a
    // second copy, and its message time in seconds since KIWA's reference.
    let check = |variable: &ExtraVariable, renamed: bool| {
        let expected = if renamed {
            as_copy_1(variable)
        } else {
            variable.clone()
        };
        let carried = root_variable(&both, &expected.name)
            .unwrap_or_else(|| panic!("{} missing", expected.name));
        if variable.name.ends_with("message_time") {
            assert_eq!(scalar_f64(carried), scalar_f64(variable) + shift);
        } else {
            assert_eq!(*carried, expected, "{}", expected.name);
        }
    };
    let mut copies = 0;
    let mut types_copied = Vec::new();
    for prefix in [
        "nexrad_performance_",
        "nexrad_vcp_",
        "nexrad_clutter_censor_",
        "nexrad_bypass_map_",
        "nexrad_clutter_filter_map_",
        "nexrad_adaptation_",
        "nexrad_prf_",
    ] {
        let of = |volume: &Volume| -> Vec<ExtraVariable> {
            volume
                .extra_vars
                .iter()
                .filter(|v| v.name.starts_with(prefix) && !v.name.contains("_message1"))
                .cloned()
                .collect()
        };
        let (kiwa_vars, ktlx_vars) = (of(&alone_kiwa), of(&alone_ktlx));
        if kiwa_vars.is_empty() {
            // KTLX's is the first of its type.
            for variable in &ktlx_vars {
                check(variable, false);
            }
            continue;
        }
        // The first copy is KIWA's, unchanged.
        assert_eq!(of(&both), kiwa_vars, "{prefix}: first copy");
        for variable in &ktlx_vars {
            check(variable, true);
            copies += 1;
        }
        if !ktlx_vars.is_empty() {
            types_copied.push(prefix);
        }
    }
    eprintln!("copied: {types_copied:?}");
    assert_eq!(
        types_copied,
        [
            "nexrad_performance_",
            "nexrad_vcp_",
            "nexrad_clutter_filter_map_",
            "nexrad_adaptation_"
        ]
    );
    // Each copied type's variables, its message time and channel byte.
    assert_eq!(copies, 497, "copied variables");
    assert!(
        !both
            .extra_vars
            .iter()
            .any(|v| v.name.starts_with("nexrad_metadata_messages_")),
        "nothing repeated or past the cap"
    );
    // The sweeps' scan rates come from the first message 5.
    let rates = |volume: &Volume| -> Vec<Option<f32>> {
        volume
            .sweeps
            .iter()
            .map(|sweep| sweep.target_scan_rate_deg_per_s)
            .collect()
    };
    assert_eq!(rates(&both), rates(&alone_kiwa));
    // Both files' message 2 rows are in the status table.
    let rows = |volume: &Volume| {
        root_variable(volume, "nexrad_rda_status_time")
            .unwrap()
            .values
            .len()
    };
    assert_eq!(rows(&both), rows(&alone_kiwa) + rows(&alone_ktlx));
}

/// The real KIWA metadata record inserted again after itself: every
/// message is a repeat of one already carried, counted and not copied.
#[test]
fn a_repeated_message_is_counted_not_copied() {
    let (kiwa, compression) = normalized(&KIWA_CHUNKS);
    let alone = read_normalized_volume_bytes(&kiwa, compression).unwrap();
    let twice = read_normalized_volume_bytes(
        &fabricate_frames_after_metadata(&kiwa, &metadata_record(&kiwa)),
        compression,
    )
    .unwrap();
    assert_eq!(
        twice.provenance.decode.decoded_ray_count,
        alone.provenance.decode.decoded_ray_count
    );
    assert!(
        !twice
            .extra_vars
            .iter()
            .any(|v| v.name.contains("_message1"))
    );
    let singles = SINGLE_MESSAGES
        .iter()
        .map(|(_, prefix)| *prefix)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|prefix| alone.extra_vars.iter().any(|v| v.name.starts_with(prefix)))
        .count();
    let repeated = root_variable(&twice, "nexrad_metadata_messages_repeated").unwrap();
    assert_eq!(repeated.values, ArrayBuf::U32(vec![singles as u32]));
}

/// Edited real bytes, a synthetic input: a real message 2 frame of the KIWA
/// metadata record relabelled as `kind`.
fn fabricate_relabelled_frame(frame: &[u8], kind: u8) -> Vec<u8> {
    let mut frame = frame.to_vec();
    frame[CONTROL_WORD_LEN + 3] = kind;
    frame
}

/// Messages 6, 9, 11 and 12 have no real sample: real message 2 frames
/// relabelled as each are carried, with the halfwords at their ICD
/// positions (Tables X, XIII and VIII), the header time and the header
/// channel byte.
#[test]
fn relabelled_real_frames_carry_messages_6_9_11_and_12() {
    let (kiwa, compression) = normalized(&KIWA_CHUNKS);
    let record = metadata_record(&kiwa);
    let frame = record
        .chunks_exact(RECORD_BYTES)
        .find(|frame| frame[CONTROL_WORD_LEN + 3] == 2)
        .expect("a real message 2 frame");
    let body = &frame[CONTROL_WORD_LEN + MESSAGE_HEADER_LEN..];
    let halfword = |number: usize| f64::from(be_u16(body, (number - 1) * 2));
    let mut extra = Vec::new();
    for kind in [6, 9, 11, 12] {
        extra.extend(fabricate_relabelled_frame(frame, kind));
    }
    let volume =
        read_normalized_volume_bytes(&fabricate_frames_after_metadata(&kiwa, &extra), compression)
            .unwrap();
    assert_eq!(volume.provenance.decode.decoded_ray_count, 240);
    let time = seconds_since(&volume, header_epoch_ms(frame, CONTROL_WORD_LEN));
    let value = |name: &str| -> Vec<f64> {
        let variable = root_variable(&volume, name).unwrap_or_else(|| panic!("{name} missing"));
        (0..variable.values.len())
            .map(|i| variable.values.get_f64(i).unwrap())
            .collect()
    };

    // Table II halfword 2, high byte.
    let channels = f64::from(frame[CONTROL_WORD_LEN + 2]);
    assert_eq!(value("nexrad_control_commands_time"), [time]);
    assert_eq!(value("nexrad_control_commands_channels"), [channels]);
    for (column, number) in [
        ("rda_state", 1),
        ("rda_log", 2),
        ("auxiliary_power", 3),
        ("control_authorization", 4),
        ("restart", 5),
        ("select_local_vcp", 6),
        ("super_resolution", 8),
        ("clutter_mitigation_decision", 9),
        ("avset", 10),
        ("channel_control", 12),
        ("performance_check", 13),
        ("zdr_bias_estimate", 14),
        ("spot_blanking", 21),
    ] {
        assert_eq!(
            value(&format!("nexrad_control_commands_{column}")),
            [halfword(number)],
            "{column}"
        );
    }

    assert_eq!(value("nexrad_request_for_data_time"), [time]);
    assert_eq!(value("nexrad_request_for_data_channels"), [channels]);
    assert_eq!(value("nexrad_request_for_data_type"), [halfword(1)]);

    let size = be_u16(body, 0);
    let pattern = &body[2..usize::from(size) * 2];
    assert_eq!(value("nexrad_loopback_message_type"), [11.0, 12.0]);
    assert_eq!(value("nexrad_loopback_time"), [time, time]);
    assert_eq!(value("nexrad_loopback_channels"), [channels, channels]);
    assert_eq!(
        value("nexrad_loopback_message_size"),
        [f64::from(size), f64::from(size)]
    );
    let length = pattern.len() as f64;
    assert_eq!(value("nexrad_loopback_pattern_length"), [length, length]);
    let bits = root_variable(&volume, "nexrad_loopback_bit_pattern").unwrap();
    assert_eq!(bits.values, ArrayBuf::U8([pattern, pattern].concat()));
}
