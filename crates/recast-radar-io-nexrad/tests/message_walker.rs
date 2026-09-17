//! Level II message walker on real files.
//!
//! Expected frame counts and message sequences come from the files
//! themselves: `tools/level2_message_scan.py` (standard-library Python,
//! written separately from the Rust walker) prints them for any manifest id.
//! A message token is `type:segments/frames`, with `segments` from the first
//! segment's header and `frames` the number of frames joined; reassembly
//! errors are `error: reason`. Tokens are run-length encoded.
//!
//! The mutation tests start from the committed KIWA start chunk and change
//! single header fields to exercise the error paths.

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use recast_radar_io_nexrad::NexradError;
use recast_radar_io_nexrad::messages::{self, MessageWalker, RawMessages};

const FRAME: usize = 2432;
const START_CHUNK: &str = "l2chunk-kiwa-307-20260917-003629-001-s";

/// Real file bytes, or `None` (with a message) when the file cannot be
/// downloaded right now.
fn load(id: &str) -> Option<Vec<u8>> {
    match recast_radar_testdata::bytes(id) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.is_offline() => {
            eprintln!("skipping {id}: {error}");
            None
        }
        Err(error) => panic!("{error}"),
    }
}

fn reason(error: &NexradError) -> String {
    match error {
        NexradError::InvalidMessage { reason, .. } => reason.clone(),
        other => other.to_string(),
    }
}

struct Walk {
    frames: usize,
    empty_frames: usize,
    position: usize,
    tokens: Vec<(String, usize)>,
    counts: std::collections::BTreeMap<u8, usize>,
}

fn walk(records: &[u8]) -> Walk {
    let mut raw = RawMessages::new(records);
    let mut tokens: Vec<(String, usize)> = Vec::new();
    let mut counts = std::collections::BTreeMap::new();
    for item in raw.by_ref() {
        let token = match item {
            Ok(message) => {
                *counts.entry(message.header.message_type).or_insert(0) += 1;
                format!(
                    "{}:{}/{}",
                    message.header.message_type,
                    message.header.segment_count(),
                    message.frames
                )
            }
            Err(error) => format!("error: {}", reason(&error)),
        };
        match tokens.last_mut() {
            Some((last, repeat)) if *last == token => *repeat += 1,
            _ => tokens.push((token, 1)),
        }
    }
    Walk {
        frames: raw.frame_count(),
        empty_frames: raw.empty_frame_count(),
        position: raw.position(),
        tokens,
        counts,
    }
}

fn owned(tokens: &[(&str, usize)]) -> Vec<(String, usize)> {
    tokens
        .iter()
        .map(|(token, repeat)| ((*token).to_owned(), *repeat))
        .collect()
}

fn check_metadata_record(id: &str, frames: usize, empty_frames: usize, tokens: &[(&str, usize)]) {
    let Some(raw) = load(id) else { return };
    let record = messages::metadata_record(&raw).unwrap();
    let walked = walk(&record);
    assert_eq!(walked.frames, frames, "{id}: frames");
    assert_eq!(walked.empty_frames, empty_frames, "{id}: empty frames");
    assert_eq!(
        walked.position,
        record.len(),
        "{id}: walk ends at record end"
    );
    assert_eq!(walked.tokens, owned(tokens), "{id}: message sequence");
}

// Metadata records: LDM bzip2 files (first LDM record) and whole-file gzip
// files (first 134 frames).

#[test]
fn metadata_record_build_22_ldm() {
    check_metadata_record(
        "l2-ktlx-20240315-000217",
        134,
        122,
        &[
            ("15:5/5", 1),
            ("18:4/4", 1),
            ("3:1/1", 1),
            ("5:1/1", 1),
            ("2:1/1", 1),
        ],
    );
}

#[test]
fn metadata_record_build_24_start_chunk_with_message_32() {
    check_metadata_record(
        START_CHUNK,
        134,
        121,
        &[
            ("15:5/5", 1),
            ("32:1/1", 1),
            ("18:4/4", 1),
            ("3:1/1", 1),
            ("5:1/1", 1),
            ("2:1/1", 1),
        ],
    );
}

#[test]
fn metadata_record_build_18_with_49_segment_bypass_map() {
    check_metadata_record(
        "l2-kdvn-20200810-180401",
        134,
        73,
        &[
            ("15:5/5", 1),
            ("13:49/49", 1),
            ("18:4/4", 1),
            ("3:1/1", 1),
            ("5:1/1", 1),
            ("2:1/1", 1),
        ],
    );
}

#[test]
fn metadata_record_build_13_gzip() {
    check_metadata_record(
        "l2-ktlx-20130520-201643",
        134,
        71,
        &[
            ("15:7/7", 1),
            ("13:49/49", 1),
            ("18:4/4", 1),
            ("3:1/1", 1),
            ("5:1/1", 1),
            ("2:1/1", 1),
        ],
    );
}

#[test]
fn metadata_record_build_21_six_segment_clutter_map() {
    check_metadata_record(
        "l2-kmaf-20230331-230843",
        134,
        121,
        &[
            ("15:6/6", 1),
            ("18:4/4", 1),
            ("3:1/1", 1),
            ("5:1/1", 1),
            ("2:1/1", 1),
        ],
    );
}

#[test]
fn metadata_record_tdwr_has_only_vcp_and_status() {
    check_metadata_record(
        "l2-tstl-20230331-230314",
        134,
        132,
        &[("5:1/1", 1), ("2:1/1", 1)],
    );
}

#[test]
fn metadata_record_2005_katrina_with_orphan_bypass_segments() {
    // Message 13 frames 1-14 say 14 segments; frames 15-48 say 48 and hold
    // zero date, time and data.
    check_metadata_record(
        "l2-klix-20050829-130035",
        134,
        0,
        &[
            ("15:62/62", 1),
            ("13:14/14", 1),
            (
                "error: message type 13 segments 15..=48 of 48 have no first segment",
                1,
            ),
            ("18:4/4", 1),
            ("3:1/1", 1),
            ("5:1/1", 1),
            ("2:1/1", 1),
            ("1:1/1", 17),
        ],
    );
}

#[test]
fn metadata_record_build_10_segment_count_disagreement() {
    // Message 15 segment 1 says 5 segments, segments 2-77 say 77; all share
    // one generation time. A stale type-0 frame numbered 5 of 4 follows the
    // 4-segment Message 18.
    check_metadata_record(
        "l2-kpah-20080415-235014",
        134,
        0,
        &[
            ("15:5/77", 1),
            ("13:49/49", 1),
            ("18:4/4", 1),
            (
                "error: message type 0 segments 5..=5 of 4 have no first segment",
                1,
            ),
            ("3:1/1", 1),
            ("5:1/1", 1),
            ("2:1/1", 1),
        ],
    );
}

#[test]
fn metadata_record_2008_first_segments_without_counts() {
    // The first frames of Messages 15 and 13 carry segment count and number
    // 0, so they stand alone and their continuation frames are orphans;
    // stale frames with the type zeroed continue the numbering.
    check_metadata_record(
        "l2-kvwx-20080415-235337",
        134,
        0,
        &[
            ("15:0/1", 1),
            (
                "error: message type 15 segments 2..=14 of 14 have no first segment",
                1,
            ),
            (
                "error: message type 0 segments 15..=77 of 14 have no first segment",
                1,
            ),
            ("13:0/1", 1),
            (
                "error: message type 13 segments 2..=14 of 14 have no first segment",
                1,
            ),
            (
                "error: message type 0 segments 15..=49 of 14 have no first segment",
                1,
            ),
            ("0:5/5", 1),
            ("0:1/1", 2),
            ("2:1/1", 1),
        ],
    );
}

#[test]
fn metadata_record_1991_archive2_has_no_metadata_messages() {
    check_metadata_record(
        "l2-ktlx-19910605-162126",
        134,
        0,
        &[("2:1/1", 1), ("1:1/1", 133)],
    );
}

#[test]
fn metadata_record_status_only_stub() {
    check_metadata_record("l2-tbwi-20230601-175101-stub", 1, 0, &[("2:1/1", 1)]);
}

/// Messages 2, 3 and 5 at the start of the volume carry the volume time.
#[test]
fn status_message_headers_carry_the_volume_time() {
    for id in [
        "l2-ktlx-20240315-000217",
        START_CHUNK,
        "l2-kdvn-20200810-180401",
        "l2-kmaf-20230331-230843",
        "l2-tstl-20230331-230314",
    ] {
        let Some(raw) = load(id) else { continue };
        let date = u32::from_be_bytes(raw[12..16].try_into().unwrap());
        let milliseconds = u32::from_be_bytes(raw[16..20].try_into().unwrap());
        let volume_time: DateTime<Utc> = Utc.timestamp_opt(0, 0).unwrap()
            + TimeDelta::days(i64::from(date) - 1)
            + TimeDelta::milliseconds(i64::from(milliseconds));
        let record = messages::metadata_record(&raw).unwrap();
        let mut checked = 0;
        for message in RawMessages::new(&record) {
            let header = message.unwrap().header;
            if matches!(header.message_type, 2 | 3 | 5) {
                let offset = (header.timestamp() - volume_time).abs();
                assert!(
                    offset <= TimeDelta::seconds(5),
                    "{id}: message {} at {} vs volume {volume_time}",
                    header.message_type,
                    header.timestamp()
                );
                checked += 1;
            }
        }
        assert!(checked >= 2, "{id}: status messages checked");
    }
}

// Whole files: every record decompressed and walked.

struct WholeFileCase {
    id: &'static str,
    frames: usize,
    empty_frames: usize,
    counts: &'static [(u8, usize)],
    errors: usize,
}

fn check_whole_file(case: &WholeFileCase) {
    let Some(raw) = load(case.id) else { return };
    let records = messages::record_bytes(&raw).unwrap();
    let walked = walk(&records);
    let id = case.id;
    assert_eq!(walked.frames, case.frames, "{id}: frames");
    assert_eq!(walked.empty_frames, case.empty_frames, "{id}: empty frames");
    assert_eq!(
        walked.position,
        records.len(),
        "{id}: walk ends at data end"
    );
    let counts: Vec<(u8, usize)> = walked.counts.into_iter().collect();
    assert_eq!(counts, case.counts, "{id}: message counts");
    let errors: usize = walked
        .tokens
        .iter()
        .filter(|(token, _)| token.starts_with("error: "))
        .map(|(_, repeat)| repeat)
        .sum();
    assert_eq!(errors, case.errors, "{id}: reassembly errors");

    let radial_messages = counts
        .iter()
        .filter(|(message_type, _)| matches!(message_type, 1 | 31))
        .map(|(_, count)| count)
        .sum::<usize>();
    if radial_messages > 0 && messages::volume_header_len(&raw) > 0 {
        let volume = recast_radar_io_nexrad::decode_volume_from_bytes(&raw).unwrap();
        assert_eq!(
            volume.metadata.decoded_radial_count, radial_messages,
            "{id}: radials decoded by the volume decoder"
        );
    }
}

#[test]
fn whole_file_start_and_intermediate_chunks() {
    check_whole_file(&WholeFileCase {
        id: START_CHUNK,
        frames: 134,
        empty_frames: 121,
        counts: &[(2, 1), (3, 1), (5, 1), (15, 1), (18, 1), (32, 1)],
        errors: 0,
    });
    check_whole_file(&WholeFileCase {
        id: "l2chunk-kiwa-307-20260917-003629-002-i",
        frames: 120,
        empty_frames: 0,
        counts: &[(31, 120)],
        errors: 0,
    });
}

#[test]
fn whole_file_ldm_build_22() {
    check_whole_file(&WholeFileCase {
        id: "l2-ktlx-20240315-000217",
        frames: 11654,
        empty_frames: 122,
        counts: &[(2, 1), (3, 1), (5, 1), (15, 1), (18, 1), (31, 11520)],
        errors: 0,
    });
}

#[test]
fn whole_file_tdwr() {
    check_whole_file(&WholeFileCase {
        id: "l2-tstl-20230331-230314",
        frames: 8415,
        empty_frames: 132,
        counts: &[(2, 2), (5, 1), (31, 8280)],
        errors: 0,
    });
}

#[test]
fn whole_file_gzip_message_1_era() {
    check_whole_file(&WholeFileCase {
        id: "l2-klix-20050829-130035",
        frames: 7370,
        empty_frames: 0,
        counts: &[(1, 7252), (2, 2), (3, 1), (5, 1), (13, 1), (15, 1), (18, 1)],
        errors: 1,
    });
}

#[test]
fn whole_file_gzip_2008_message_31() {
    check_whole_file(&WholeFileCase {
        id: "l2-kvwx-20080415-235337",
        frames: 2635,
        empty_frames: 0,
        counts: &[(0, 3), (2, 2), (13, 1), (15, 1), (31, 2500)],
        errors: 4,
    });
}

#[test]
fn whole_file_status_only_stub_of_three_ldm_records() {
    check_whole_file(&WholeFileCase {
        id: "l2-tbwi-20230601-175101-stub",
        frames: 3,
        empty_frames: 0,
        counts: &[(2, 3)],
        errors: 0,
    });
}

/// The model-data file has no volume header and one LDM record holding a
/// single Message 29 whose size field is 65535, with the size in bytes in
/// header bytes 12-15 (Table II notes 6 and 7).
#[test]
fn extended_size_message_29_in_model_data_file() {
    let Some(raw) = load("l2-klix-20210829-175748-mdm") else {
        return;
    };
    assert_eq!(messages::volume_header_len(&raw), 0);
    let records = messages::record_bytes(&raw).unwrap();
    assert_eq!(records.len(), 809_241);
    let mut walker = RawMessages::new(&records);
    let message = walker.next().unwrap().unwrap();
    assert!(walker.next().is_none());
    assert_eq!(walker.position(), records.len());
    assert_eq!(message.header.message_type, 29);
    assert!(message.header.has_extended_size());
    assert!(message.header.is_variable_length());
    assert_eq!(message.header.segment_count(), 1);
    assert_eq!(message.header.message_len(), 809_229);
    assert_eq!(message.body.len(), 809_229 - 16);
    assert_eq!(message.offset, 12);
}

/// Every metadata message of a current-build record decodes without error.
#[test]
fn message_walker_decodes_start_chunk_metadata() {
    let raw = recast_radar_testdata::bytes(START_CHUNK).unwrap();
    let record = messages::metadata_record(&raw).unwrap();
    let types: Vec<u8> = MessageWalker::new(&record)
        .map(|item| item.unwrap().0.message_type)
        .collect();
    assert_eq!(types, [15, 32, 18, 3, 5, 2]);
    assert_eq!(
        messages::message_type_name(32),
        Some("RDA PRF Data"),
        "Table I name"
    );
}

// Mutations of the committed start chunk.

fn start_chunk_metadata() -> Vec<u8> {
    let raw = recast_radar_testdata::bytes(START_CHUNK).unwrap();
    messages::metadata_record(&raw).unwrap().into_owned()
}

#[test]
fn missing_segment_yields_incomplete_and_orphan_errors() {
    let mut record = start_chunk_metadata();
    // Zero the size of frame 2 (Message 15 segment 3 of 5): an empty frame.
    record[2 * FRAME + 12..2 * FRAME + 14].copy_from_slice(&[0, 0]);
    let walked = walk(&record);
    assert_eq!(walked.frames, 134);
    assert_eq!(walked.empty_frames, 122);
    assert_eq!(
        walked.tokens,
        owned(&[
            (
                "error: segmented message type 15 ended after 2 of 5 segments",
                1
            ),
            (
                "error: message type 15 segments 4..=5 of 5 have no first segment",
                1
            ),
            ("32:1/1", 1),
            ("18:4/4", 1),
            ("3:1/1", 1),
            ("5:1/1", 1),
            ("2:1/1", 1),
        ])
    );
}

#[test]
fn rejected_body_does_not_stop_the_walk() {
    let mut record = start_chunk_metadata();
    // Relabel frame 125 (Message 32) as Message 33. Its bytes 34-37 then read
    // as the reserved compression type 0xF384000D.
    assert_eq!(record[125 * FRAME + 15], 32);
    record[125 * FRAME + 15] = 33;
    let items: Vec<_> = MessageWalker::new(&record).collect();
    assert_eq!(items.len(), 6);
    let error = items[1].as_ref().unwrap_err();
    assert!(matches!(
        error,
        NexradError::InvalidMessage { offset, .. } if *offset == 125 * FRAME + 12
    ));
    assert_eq!(
        reason(error),
        "message type 33: invalid message at offset 34: reserved RDA log compression type 4085514253"
    );
    let after: Vec<u8> = items[2..]
        .iter()
        .map(|item| item.as_ref().unwrap().0.message_type)
        .collect();
    assert_eq!(after, [18, 3, 5, 2]);
}

#[test]
fn truncated_record_ends_the_walk() {
    let record = start_chunk_metadata();
    // Keep 100 bytes of frame 131 (Message 3, 976 bytes with its header).
    let truncated = &record[..131 * FRAME + 100];
    let walked = walk(truncated);
    assert_eq!(walked.position, truncated.len());
    assert_eq!(
        walked.tokens,
        owned(&[
            ("15:5/5", 1),
            ("32:1/1", 1),
            ("18:4/4", 1),
            (
                "error: truncated message body at offset 318604: need 976 bytes, have 88",
                1
            ),
        ])
    );
}
