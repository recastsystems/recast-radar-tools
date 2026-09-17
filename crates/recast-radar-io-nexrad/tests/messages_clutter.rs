//! Clutter messages on real files: 15 (Clutter Filter Map, Table XIV),
//! 13 (Clutter Filter Bypass Map, Table IX) and 8 (Clutter Censor Zones,
//! Table XII).
//!
//! Golden values are in `testdata/level2/golden/clutter/<id>.json`, written
//! by `tools/level2_golden.py`. MetPy 1.7.1 `Level2File` reads the same
//! metadata record that `messages::metadata_record` returns. MetPy gives the
//! whole clutter filter map. For the bypass map it gives the generation time,
//! the segment and radial counts, and radial 0 of each segment. Its other
//! radials repeat radial 0, and it lists each halfword's bits least
//! significant bit first; the script explains both. The rest of the bypass
//! map is checked three ways: against halfwords read from the file bytes at
//! the record offsets below, against bypass-bin counts computed from those
//! bytes, and by range continuity, which confirms the bit order of Table IX
//! note 4.
//!
//! The RPG sends Message 8 to the RDA, so Archive II files do not contain
//! it. Its only real-byte test relabels a real Message 15 and checks that
//! the decoder rejects it.

use chrono::{DateTime, Datelike, Utc};
use recast_radar_io_nexrad::NexradError;
use recast_radar_io_nexrad::messages::bypass_map::{
    BypassMapLayout, ClutterFilterBypassMap, HALFWORDS_PER_RADIAL, RANGE_BINS,
};
use recast_radar_io_nexrad::messages::clutter_censor::ClutterCensorZones;
use recast_radar_io_nexrad::messages::clutter_filter_map::{
    AZIMUTH_SEGMENTS, ClutterFilterMap, LAST_ZONE_END_RANGE_KM, MAX_RANGE_ZONES, OperatorSelectCode,
};
use recast_radar_io_nexrad::messages::{self, MessageBody, MessageWalker, RawMessages};
use recast_radar_testdata::Format;
use serde_json::{Value, json};

const FRAME: usize = 2432;
const START_CHUNK: &str = "l2chunk-kiwa-307-20260917-003629-001-s";
const KLIX_2005: &str = "l2-klix-20050829-130035";
const KPAH_2008: &str = "l2-kpah-20080415-235014";

/// Files where MetPy joins segments that the walker cannot join. In KVWX
/// 2008 the first frames of Messages 13 and 15 say "segment 0 of 0", and
/// the frames after them say "2..14 of 14". MetPy buffers segments by
/// number and joins all 14. The walker reports the continuation frames as
/// orphans (see tests/message_walker.rs). Both maps contain only zeros.
const METPY_ONLY_JOINS: &[&str] = &["l2-kvwx-20080415-235337"];

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

/// Message type named by a walker error reason ("message type 13 ...",
/// "segmented message type 15 ...").
fn message_type_named(reason: &str) -> Option<u8> {
    let rest = reason.split("message type ").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Clutter messages, and errors about them, from walking one record.
#[derive(Default)]
struct Clutter {
    filter_maps: Vec<ClutterFilterMap>,
    bypass_maps: Vec<ClutterFilterBypassMap>,
    censor_zones: Vec<ClutterCensorZones>,
    errors: Vec<String>,
}

fn clutter(record: &[u8]) -> Clutter {
    let mut found = Clutter::default();
    for item in MessageWalker::new(record) {
        match item {
            Ok((_, MessageBody::ClutterFilterMap(map))) => found.filter_maps.push(map),
            Ok((_, MessageBody::BypassMap(map))) => found.bypass_maps.push(map),
            Ok((_, MessageBody::ClutterCensorZones(zones))) => found.censor_zones.push(zones),
            Ok(_) => {}
            Err(error) => {
                let reason = reason(&error);
                if matches!(message_type_named(&reason), Some(8 | 13 | 15)) {
                    found.errors.push(reason);
                }
            }
        }
    }
    found
}

/// Header time of the Message 2 in the metadata record: the volume start time
/// (tests/message_walker.rs checks that it matches the volume header).
fn status_time(record: &[u8]) -> Option<DateTime<Utc>> {
    RawMessages::new(record)
        .filter_map(Result::ok)
        .filter(|message| message.header.message_type == 2)
        .map(|message| message.header.timestamp())
        .last()
}

fn utc(time: DateTime<Utc>) -> String {
    time.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Golden documents in file-name order, with the manifest hash checked.
fn goldens() -> Vec<(String, Value)> {
    let dir = recast_radar_testdata::testdata_dir().join("level2/golden/clutter");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            let golden: Value = serde_json::from_str(&text).unwrap();
            let id = golden["id"].as_str().unwrap().to_owned();
            let entry = recast_radar_testdata::entry(&id)
                .unwrap_or_else(|| panic!("{}: unknown id {id}", path.display()));
            assert_eq!(
                golden["sha256"],
                entry.sha256.as_str(),
                "{id}: golden file hash"
            );
            (id, golden)
        })
        .collect()
}

/// Runs of consecutive azimuth segments with identical range zones, in the
/// golden layout `[first, last, [[op_code, end_range_km], ...]]`.
fn azimuth_runs(map: &ClutterFilterMap) -> Value {
    let segments: Vec<Value> = map
        .segments
        .iter()
        .map(|segment| {
            let mut runs: Vec<(usize, usize, Vec<[u16; 2]>)> = Vec::new();
            for (azimuth, zones) in segment.azimuths().enumerate() {
                let pairs: Vec<[u16; 2]> = zones
                    .iter()
                    .map(|zone| [zone.op_code.code(), zone.end_range_km])
                    .collect();
                match runs.last_mut() {
                    Some(run) if run.2 == pairs => run.1 = azimuth,
                    _ => runs.push((azimuth, azimuth, pairs)),
                }
            }
            json!(runs)
        })
        .collect();
    Value::Array(segments)
}

#[test]
fn clutter_filter_maps_match_metpy() {
    let goldens = goldens();
    assert!(goldens.len() >= 20, "golden files: {}", goldens.len());
    for (id, golden) in &goldens {
        let Some(raw) = load(id) else { continue };
        let record = messages::metadata_record(&raw).unwrap();
        let found = clutter(&record);
        let Some(expected) = golden.get("clutter_filter_map") else {
            // MetPy skipped the message (zero elevation segments); the decoder
            // rejects it.
            assert!(found.filter_maps.is_empty(), "{id}");
            assert!(
                found
                    .errors
                    .iter()
                    .any(|error| error.starts_with("message type 15: ")),
                "{id}: {:?}",
                found.errors
            );
            continue;
        };
        assert_eq!(found.filter_maps.len(), 1, "{id}: {:?}", found.errors);
        let map = &found.filter_maps[0];
        assert_eq!(
            expected["generation_time"],
            utc(map.generation_time()),
            "{id}"
        );
        assert_eq!(expected["elevation_segments"], map.segments.len(), "{id}");
        let azimuths: Vec<usize> = map
            .segments
            .iter()
            .map(|segment| segment.azimuth_count())
            .collect();
        assert_eq!(expected["azimuths_per_segment"], json!(azimuths), "{id}");
        assert_eq!(expected["azimuth_runs"], azimuth_runs(map), "{id}");
    }
}

/// Golden files with a bypass map that the walker can join: KLIX 2005
/// (legacy layout), then KPAH and KDMX 2008 through KDVN 2020 (Build 18.2).
fn joinable_bypass_goldens(goldens: &[(String, Value)]) -> usize {
    goldens
        .iter()
        .filter(|(id, golden)| {
            golden.get("clutter_filter_bypass_map").is_some()
                && !METPY_ONLY_JOINS.contains(&id.as_str())
        })
        .count()
}

#[test]
fn bypass_maps_match_metpy_radial_0() {
    let goldens = goldens();
    assert_eq!(joinable_bypass_goldens(&goldens), 9);
    for (id, golden) in &goldens {
        let Some(raw) = load(id) else { continue };
        let record = messages::metadata_record(&raw).unwrap();
        let found = clutter(&record);
        let Some(expected) = golden.get("clutter_filter_bypass_map") else {
            assert!(found.bypass_maps.is_empty(), "{id}");
            continue;
        };
        if METPY_ONLY_JOINS.contains(&id.as_str()) {
            assert!(found.bypass_maps.is_empty(), "{id}");
            assert!(
                found.errors.contains(
                    &"message type 13 segments 2..=14 of 14 have no first segment".into()
                ),
                "{id}: {:?}",
                found.errors
            );
            continue;
        }
        assert_eq!(found.bypass_maps.len(), 1, "{id}: {:?}", found.errors);
        let map = &found.bypass_maps[0];
        assert_eq!(
            expected["generation_time"],
            json!(map.generation_time().map(utc)),
            "{id}"
        );
        let radials: Vec<usize> = map.segments.iter().map(|s| s.radials.len()).collect();
        assert_eq!(expected["radials_per_segment"], json!(radials), "{id}");
        let legacy = expected["radials_per_segment"][0] == 256;
        assert_eq!(
            map.layout,
            if legacy {
                BypassMapLayout::Legacy
            } else {
                BypassMapLayout::Current
            },
            "{id}"
        );
        assert_eq!(expected["elevation_segments"], map.segments.len(), "{id}");
        assert_eq!(expected["bins_per_radial"], RANGE_BINS, "{id}");
        let radial_0: Vec<&[u16; HALFWORDS_PER_RADIAL]> =
            map.segments.iter().map(|s| &s.radials[0]).collect();
        assert_eq!(expected["radial_0_halfwords"], json!(radial_0), "{id}");
    }
}

/// Level II volumes with no golden file: the 1991-2003 ARCHIVE2 files (no
/// metadata record), the 2021 model-data file and the TDWR files. MetPy
/// finds no clutter messages in them, and neither does the walker.
#[test]
fn files_without_golden_have_no_clutter_messages() {
    let golden_ids: Vec<String> = goldens().into_iter().map(|(id, _)| id).collect();
    let without: Vec<&str> = recast_radar_testdata::manifest()
        .files
        .iter()
        .filter(|entry| entry.format == Format::NexradLevel2 && !golden_ids.contains(&entry.id))
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(
        without,
        [
            "l2-ktlx-19910605-162126",
            "l2-ktlx-19990503-230052",
            "l2-ktlx-19990504-002218",
            "l2-ktlx-20030508-221041",
            "l2-klix-20210829-175748-mdm",
            "l2-tstl-20230331-230314",
            "l2-tbwi-20230601-175101-stub",
        ]
    );
    for id in without {
        let Some(raw) = load(id) else { continue };
        let found = clutter(&messages::metadata_record(&raw).unwrap());
        assert!(found.filter_maps.is_empty(), "{id}");
        assert!(found.bypass_maps.is_empty(), "{id}");
        assert!(found.censor_zones.is_empty(), "{id}");
        assert!(found.errors.is_empty(), "{id}: {:?}", found.errors);
    }
}

/// Table XIV and IX ranges and structure on every decoded map.
#[test]
fn decoded_maps_are_within_icd_ranges() {
    for (id, _) in &goldens() {
        let Some(raw) = load(id) else { continue };
        let record = messages::metadata_record(&raw).unwrap();
        let volume_time = status_time(&record).unwrap();
        let found = clutter(&record);
        for map in &found.filter_maps {
            let generated = map.generation_time();
            assert!(
                generated <= volume_time,
                "{id}: {generated} after {volume_time}"
            );
            assert!(generated.year() >= 2005, "{id}: {generated}");
            assert!((1..=5).contains(&map.segments.len()), "{id}");
            for segment in &map.segments {
                assert_eq!(segment.azimuth_count(), AZIMUTH_SEGMENTS, "{id}");
                for zones in segment.azimuths() {
                    assert!((1..=usize::from(MAX_RANGE_ZONES)).contains(&zones.len()));
                    assert!(
                        zones
                            .windows(2)
                            .all(|w| w[0].end_range_km < w[1].end_range_km)
                    );
                    assert_eq!(zones.last().unwrap().end_range_km, LAST_ZONE_END_RANGE_KM);
                    assert!(
                        zones
                            .iter()
                            .all(|zone| !matches!(zone.op_code, OperatorSelectCode::Unknown(_))),
                        "{id}: {zones:?}"
                    );
                }
            }
            let expected_trailing = if id == KPAH_2008 { 172_800 } else { 0 };
            assert_eq!(map.trailing_bytes, expected_trailing, "{id}");
        }
        for map in &found.bypass_maps {
            if let Some(generated) = map.generation_time() {
                assert!(
                    generated <= volume_time,
                    "{id}: {generated} after {volume_time}"
                );
                assert!(generated.year() >= 2005, "{id}: {generated}");
            }
            let numbers: Vec<u16> = map.segments.iter().map(|s| s.segment_number).collect();
            let expected: Vec<u16> = (1..=numbers.len() as u16).collect();
            assert_eq!(numbers, expected, "{id}: segment numbers");
            for segment in &map.segments {
                assert_eq!(segment.radials.len(), map.layout.radial_count(), "{id}");
            }
            assert_eq!(map.trailing_bytes, 0, "{id}");
        }
    }
}

/// KLIX 2005-08-29 (legacy RDA). Message 15 is 62 segments of zeros:
/// generation date, time and elevation segment count are all 0. MetPy logs
/// "num_el is outside (0, 5]" and skips it, and the decoder rejects it.
/// Message 13 uses the 2620002B layout with 2 segments of 256 radials.
#[test]
fn klix_2005_legacy_bypass_map_and_zero_filled_message_15() {
    let Some(raw) = load(KLIX_2005) else { return };
    let record = messages::metadata_record(&raw).unwrap();
    let found = clutter(&record);
    assert!(found.filter_maps.is_empty());
    assert_eq!(
        found.errors,
        [
            "message type 15: invalid message at offset 4: clutter filter map elevation segment count 0 outside 1..=5",
            "message type 13 segments 15..=48 of 48 have no first segment",
        ]
    );
    let golden = goldens()
        .into_iter()
        .find(|(id, _)| id == KLIX_2005)
        .unwrap()
        .1;
    assert!(golden["metpy_log"].as_array().unwrap().iter().any(|line| {
        line.as_str()
            .unwrap()
            .starts_with("Message 15 num_el is outside (0, 5]")
    }));

    let [map] = found.bypass_maps.as_slice() else {
        panic!("{:?}", found.errors)
    };
    assert_eq!(map.layout, BypassMapLayout::Legacy);
    assert_eq!(map.generation_date, None);
    assert_eq!(map.generation_minutes, None);
    assert_eq!(map.generation_time(), None);
    assert_eq!(map.segments.len(), 2);
    assert_eq!(map.trailing_bytes, 0);
}

/// KPAH 2008-04-15 (Build 10). The walker joins 77 Message 15 segments (see
/// tests/message_walker.rs). The map uses the first 5 403 halfwords, and the
/// other 86 400 halfwords are stale frames. MetPy logs the same split:
/// "Used: 5400 Avail: 91800" counts halfwords after the 3-halfword header.
#[test]
fn kpah_2008_message_15_keeps_stale_segments_as_trailing_bytes() {
    let Some(raw) = load(KPAH_2008) else { return };
    let record = messages::metadata_record(&raw).unwrap();
    let found = clutter(&record);
    let [map] = found.filter_maps.as_slice() else {
        panic!("{:?}", found.errors)
    };
    assert_eq!(map.trailing_bytes, (91_800 - 5_400) * 2);
    let golden = goldens()
        .into_iter()
        .find(|(id, _)| id == KPAH_2008)
        .unwrap()
        .1;
    assert_eq!(
        golden["metpy_log"],
        json!(["Message 15 left data -- Used: 5400 Avail: 91800"])
    );
}

struct HexCase {
    id: &'static str,
    layout: BypassMapLayout,
    /// Set (bypass) bins per segment, counted from the file bytes.
    bypass_bins: &'static [usize],
    /// Segment 1 radials: (radial, metadata record offset of the radial's
    /// first halfword, the 8 bytes found there).
    radials: &'static [(usize, usize, [u8; 8])],
}

/// Halfwords read from the metadata record bytes (checked here against the
/// record itself), then compared with the decoded map. Offsets: Message 13
/// segment k (0-based) is frame `f0 + k`. Its body bytes start 28 bytes into
/// the frame (12-byte CTM header plus 16-byte message header), and every
/// segment except the last carries 2400 body bytes. The current layout puts
/// segment 1 radial `r` at body offset `6 + 2 + 64 r`, and the legacy layout
/// at `2 + 2 + 64 r`. `f0` is 77 in KTLX 2013 and KDVN 2020 (record offset
/// 187 264) and 62 in KLIX 2005 (record offset 150 784).
///
/// Radials 257 (KTLX) and 138 (KDVN) filter more bins than any other radial
/// in segment 1, and their first 64 bins are all filtered.
const HEX_CASES: &[HexCase] = &[
    HexCase {
        id: "l2-ktlx-20130520-201643",
        layout: BypassMapLayout::Current,
        bypass_bins: &[168_431, 177_782, 180_319, 181_602, 181_802],
        radials: &[
            (
                90,
                193_124,
                [0x00, 0x07, 0x81, 0x00, 0x08, 0x70, 0xff, 0xff],
            ),
            (
                180,
                198_948,
                [0x00, 0x00, 0x00, 0x00, 0x03, 0xf0, 0xff, 0xfc],
            ),
            (257, 203_940, [0; 8]),
            (
                359,
                210_564,
                [0x00, 0x03, 0xc3, 0xc0, 0xff, 0xff, 0xff, 0xe3],
            ),
        ],
    },
    HexCase {
        id: "l2-kdvn-20200810-180401",
        layout: BypassMapLayout::Current,
        bypass_bins: &[166_044, 173_384, 173_019, 176_728, 177_954],
        radials: &[
            (
                90,
                193_124,
                [0x00, 0x00, 0x00, 0x00, 0x70, 0x3f, 0x3f, 0xff],
            ),
            (138, 196_228, [0; 8]),
            (
                180,
                198_948,
                [0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xff, 0xf9],
            ),
            (
                359,
                210_564,
                [0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xff],
            ),
        ],
    },
    HexCase {
        id: KLIX_2005,
        layout: BypassMapLayout::Legacy,
        bypass_bins: &[125_087, 129_494],
        radials: &[
            (
                60,
                154_688,
                [0x00, 0x17, 0x84, 0xff, 0xff, 0xff, 0xf0, 0x21],
            ),
            (
                64,
                154_944,
                [0x00, 0x17, 0xe3, 0xff, 0xff, 0xff, 0xff, 0xff],
            ),
            (
                128,
                159_104,
                [0x00, 0x35, 0xdf, 0xff, 0x7f, 0xff, 0xf7, 0xff],
            ),
            (
                255,
                167_328,
                [0x00, 0x00, 0x07, 0xbe, 0xff, 0xff, 0xff, 0xff],
            ),
        ],
    },
];

#[test]
fn bypass_map_halfwords_match_file_bytes() {
    for case in HEX_CASES {
        let id = case.id;
        let Some(raw) = load(id) else { continue };
        let record = messages::metadata_record(&raw).unwrap();
        let found = clutter(&record);
        let [map] = found.bypass_maps.as_slice() else {
            panic!("{id}: {:?}", found.errors)
        };
        assert_eq!(map.layout, case.layout, "{id}");
        let counts: Vec<usize> = map
            .segments
            .iter()
            .map(|segment| segment.bypass_bin_count())
            .collect();
        assert_eq!(counts, case.bypass_bins, "{id}");
        let radial_count = map.layout.radial_count();
        for (segment, bypass) in map.segments.iter().zip(case.bypass_bins) {
            let filtered = (0..radial_count)
                .flat_map(|radial| (0..RANGE_BINS).map(move |bin| (radial, bin)))
                .filter(|&(radial, bin)| segment.bypass(radial, bin) == Some(false))
                .count();
            assert_eq!(filtered, radial_count * RANGE_BINS - bypass, "{id}");
        }
        for &(radial, offset, bytes) in case.radials {
            assert_eq!(record[offset..offset + 8], bytes, "{id} radial {radial}");
            let halfwords: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            assert_eq!(
                map.segments[0].radials[radial][..4],
                halfwords,
                "{id} radial {radial}"
            );
        }
    }

    // Note 4, bin by bin: KTLX 2013 segment 1 radial 90 starts with 0x0007,
    // so bins 0-12 are filtered and bins 13-15 bypass the filters; 0x8100
    // then sets bins 16 and 23.
    let Some(raw) = load("l2-ktlx-20130520-201643") else {
        return;
    };
    let record = messages::metadata_record(&raw).unwrap();
    let map = &clutter(&record).bypass_maps[0];
    let segment = &map.segments[0];
    let bins: Vec<bool> = (0..24)
        .map(|bin| segment.bypass(90, bin).unwrap())
        .collect();
    let mut expected = vec![false; 24];
    for bin in [13, 14, 15, 16, 23] {
        expected[bin] = true;
    }
    assert_eq!(bins, expected);
    assert_eq!(segment.bypass(90, RANGE_BINS), None);
    assert_eq!(segment.bypass(360, 0), None);
}

/// Table IX note 4 says the MSB of each halfword is its lowest-numbered bin.
/// Clutter is contiguous in range, so when two neighbouring bins include a
/// filtered bin, both are usually filtered. That holds across halfword
/// boundaries (bins 15|16, 31|32, ...) nearly as often as inside a halfword
/// when bits are read MSB first. Read LSB first, it holds far less often
/// across boundaries. In the corpus, the boundary rate is at least 71% of
/// the inner rate (MSB first) and at least 1.97 times the LSB-first boundary
/// rate.
#[test]
fn bypass_map_bit_order_follows_icd_note_4() {
    let goldens = goldens();
    assert_eq!(joinable_bypass_goldens(&goldens), 9);
    for (id, golden) in &goldens {
        if golden.get("clutter_filter_bypass_map").is_none()
            || METPY_ONLY_JOINS.contains(&id.as_str())
        {
            continue;
        }
        let Some(raw) = load(id) else { continue };
        let found = clutter(&messages::metadata_record(&raw).unwrap());
        let map = &found.bypass_maps[0];
        // [pairs with a filtered bin, pairs with both filtered] for inner
        // pairs, boundary pairs (MSB first), boundary pairs (LSB first).
        let mut counts = [[0usize; 2]; 3];
        for segment in &map.segments {
            for (radial, halfwords) in segment.radials.iter().enumerate() {
                let reversed = |bin: usize| halfwords[bin / 16] & (1 << (bin % 16)) != 0;
                for bin in 0..RANGE_BINS - 1 {
                    let pairs = [
                        (segment.bypass(radial, bin), segment.bypass(radial, bin + 1)),
                        (Some(reversed(bin)), Some(reversed(bin + 1))),
                    ];
                    for (order, (a, b)) in pairs.into_iter().enumerate() {
                        let (a, b) = (a.unwrap(), b.unwrap());
                        if a && b {
                            continue;
                        }
                        let slot = match (bin % 16 == 15, order) {
                            (false, 0) => 0,
                            (false, _) => continue,
                            (true, 0) => 1,
                            (true, _) => 2,
                        };
                        counts[slot][0] += 1;
                        counts[slot][1] += usize::from(a == b);
                    }
                }
            }
        }
        let rate = |[pairs, both]: [usize; 2]| both as f64 / pairs as f64;
        let (inner, msb, lsb) = (rate(counts[0]), rate(counts[1]), rate(counts[2]));
        eprintln!("{id}: inner {inner:.3}, boundary MSB first {msb:.3}, LSB first {lsb:.3}");
        assert!(
            msb >= 0.6 * inner,
            "{id}: inner {inner}, MSB-first boundary {msb}"
        );
        assert!(msb >= 1.5 * lsb, "{id}: MSB-first {msb}, LSB-first {lsb}");
    }
}

/// The committed start chunk of the KIWA 2026 volume (Build 24.1) has the
/// same Message 15 as the archive file, so this comparison with MetPy runs
/// offline.
#[test]
fn start_chunk_clutter_filter_map_matches_archive_golden() {
    let raw = recast_radar_testdata::bytes(START_CHUNK).unwrap();
    let found = clutter(&messages::metadata_record(&raw).unwrap());
    assert!(found.errors.is_empty(), "{:?}", found.errors);
    assert!(found.bypass_maps.is_empty(), "no Message 13 after Build 19");
    let [map] = found.filter_maps.as_slice() else {
        panic!()
    };
    let golden = goldens()
        .into_iter()
        .find(|(id, _)| id == "l2-kiwa-20260917-003629")
        .unwrap()
        .1;
    let expected = &golden["clutter_filter_map"];
    assert_eq!(expected["generation_time"], utc(map.generation_time()));
    assert_eq!(expected["azimuth_runs"], azimuth_runs(map));
    assert_eq!((map.generation_date, map.generation_minutes), (20_713, 575));
    assert_eq!(map.trailing_bytes, 0);
    let zone = map.segments[4].azimuth(359).unwrap();
    assert_eq!(zone.len(), 1);
    assert_eq!(zone[0].op_code, OperatorSelectCode::BypassMapInControl);
    assert_eq!(zone[0].end_range_km, 511);
    assert_eq!(map.segments[0].azimuth(360), None);
}

/// Message 8 has no real sample. Relabel the five Message 15 frames of the
/// committed start chunk as Message 8: the first halfword, the map date
/// 20 713, becomes the override region count, which is above 25, so the
/// message is rejected and the walk continues.
#[test]
fn relabelled_message_15_is_rejected_as_clutter_censor_zones() {
    let raw = recast_radar_testdata::bytes(START_CHUNK).unwrap();
    let mut record = messages::metadata_record(&raw).unwrap().into_owned();
    for frame in 0..5 {
        assert_eq!(record[frame * FRAME + 15], 15);
        record[frame * FRAME + 15] = 8;
    }
    let items: Vec<_> = MessageWalker::new(&record).collect();
    let error = items[0].as_ref().unwrap_err();
    assert!(matches!(
        error,
        NexradError::InvalidMessage { offset: 12, .. }
    ));
    assert_eq!(
        reason(error),
        "message type 8: invalid message at offset 0: clutter censor zone count 20713 exceeds 25"
    );
    let after: Vec<u8> = items[1..]
        .iter()
        .map(|item| item.as_ref().unwrap().0.message_type)
        .collect();
    assert_eq!(after, [32, 18, 3, 5, 2]);
}
