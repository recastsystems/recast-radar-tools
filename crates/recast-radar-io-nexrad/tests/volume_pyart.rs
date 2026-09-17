//! `decode_volume_from_bytes` against Py-ART: cuts, radials and moment data.
//!
//! Golden values: `testdata/level2/golden/volume/*.json`, written by
//! `python tools/level2_golden.py volume` with Py-ART 2.2.5's own Level II
//! reader (`NEXRADLevel2File`, used by `read_nexrad_archive`). Py-ART groups
//! rays into scans by elevation number. For each scan the golden has the ray
//! count, the sums of the rays' collection times and azimuths, and for each
//! moment: the rays that carry it, their gate counts, first gate range, gate
//! spacing, word size, scale and offset, and the sum of the raw gate codes
//! with the counts of codes 0 (below threshold) and 1 (range folded).
//!
//! Sources: KVWX 2008-04-15, whose message 31 radar identifiers are four
//! spaces; KPAH 2008-04-15 (Build 10.0, the same evening, identifier "KPAH");
//! the KTLX 2024-03-15 benchmark volume (16-bit ZDR and PHI, CFP); and the
//! committed KIWA real-time chunks, which run offline.

mod common;

use std::collections::BTreeSet;

use recast_radar_core::{MomentStorage, RadarVolume};
use recast_radar_io_nexrad::messages::{self, MessageBody, MessageWalker};
use recast_radar_io_nexrad::{
    decode_volume_from_bytes, decode_volume_with_metadata, parse_message_31_header,
};
use serde_json::Value;

use common::{assert_checked_every_available, load_all};

const KVWX_2008: &str = "l2-kvwx-20080415-235337";

/// Every golden of the group, sorted by name.
fn goldens() -> Vec<(String, Value)> {
    let dir = recast_radar_testdata::testdata_dir().join("level2/golden/volume");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            (name, serde_json::from_str(&text).unwrap())
        })
        .collect()
}

fn source_ids(golden: &Value) -> Vec<&str> {
    golden["source"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect()
}

fn uint(value: &Value, what: &str) -> u64 {
    value
        .as_u64()
        .unwrap_or_else(|| panic!("golden {what} is not an unsigned integer: {value}"))
}

/// The one distinct value of a golden list.
fn single<'a>(value: &'a Value, what: &str) -> &'a Value {
    match value.as_array().map(Vec::as_slice) {
        Some([only]) => only,
        _ => panic!("golden {what}: expected one distinct value, got {value}"),
    }
}

/// Raw codes of a grid: (sum, count of 0, count of 1).
fn code_counts(storage: &MomentStorage) -> (u64, u64, u64) {
    let mut totals = (0u64, 0u64, 0u64);
    let mut add = |code: u64| {
        totals.0 += code;
        totals.1 += u64::from(code == 0);
        totals.2 += u64::from(code == 1);
    };
    match storage {
        MomentStorage::U8(values) => values.iter().for_each(|&v| add(u64::from(v))),
        MomentStorage::U16(values) => values.iter().for_each(|&v| add(u64::from(v))),
        MomentStorage::F32(_) => panic!("Level II grids hold integer codes"),
    }
    totals
}

fn check_volume(name: &str, volume: &RadarVolume, golden: &Value) {
    assert_eq!(
        volume.site.id,
        golden["icao"].as_str().unwrap(),
        "{name}: site id is the volume header ICAO, as in Py-ART"
    );
    let radials: usize = volume.cuts.iter().map(|cut| cut.radials.len()).sum();
    assert_eq!(
        radials as u64,
        uint(&golden["nrays"], "nrays"),
        "{name}: rays"
    );
    assert_eq!(
        volume.metadata.decoded_radial_count as u64,
        uint(&golden["nrays"], "nrays"),
        "{name}: decoded radials"
    );

    let scans = golden["scans"].as_array().unwrap();
    for cut in &volume.cuts {
        let number = u64::from(cut.elevation_number.expect("message 31 cuts are numbered"));
        assert!(
            (1..=scans.len() as u64).contains(&number),
            "{name}: cut with elevation number {number}, Py-ART has {} scans",
            scans.len()
        );
    }
    for scan in scans {
        let number = uint(&scan["elevation_number"], "elevation_number");
        let what = format!("{name} elevation {number}");
        let cuts: Vec<_> = volume
            .cuts
            .iter()
            .filter(|cut| cut.elevation_number.map(u64::from) == Some(number))
            .collect();
        let rays: Vec<_> = cuts.iter().flat_map(|cut| &cut.radials).collect();
        assert_eq!(
            rays.len() as u64,
            uint(&scan["nrays"], "nrays"),
            "{what}: rays"
        );
        let collect_ms: u64 = rays
            .iter()
            .map(|ray| u64::try_from(ray.time_offset_ms).unwrap())
            .sum();
        assert_eq!(
            collect_ms,
            uint(&scan["collect_ms_sum"], "collect_ms_sum"),
            "{what}: collection times"
        );
        let azimuths: f64 = rays.iter().map(|ray| f64::from(ray.azimuth_deg)).sum();
        let expected = scan["azimuth_sum"].as_f64().unwrap();
        assert!(
            (azimuths - expected).abs() <= 1e-9 * expected.abs().max(1.0),
            "{what}: azimuth sum {azimuths} != {expected}"
        );

        let moments = scan["moments"].as_object().unwrap();
        let decoded: BTreeSet<&str> = cuts
            .iter()
            .flat_map(|cut| cut.moments.keys().map(|moment| moment.short_name()))
            .collect();
        let expected_names: BTreeSet<&str> = moments.keys().map(String::as_str).collect();
        assert_eq!(decoded, expected_names, "{what}: moments");

        for (moment, summary) in moments {
            let context = format!("{what} {moment}");
            let grids: Vec<_> = cuts
                .iter()
                .flat_map(|cut| cut.moments.values())
                .filter(|grid| grid.moment.short_name() == moment)
                .collect();
            let rows: usize = grids.iter().map(|grid| grid.radial_count()).sum();
            assert_eq!(
                rows as u64,
                uint(&summary["rays"], "rays"),
                "{context}: rays"
            );
            let widest = summary["ngates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|gates| uint(gates, "ngates"))
                .max()
                .unwrap();
            let (mut sum, mut zeros, mut ones, mut cells) = (0, 0, 0, 0u64);
            for grid in &grids {
                assert_eq!(
                    grid.gate_range.gate_count as u64, widest,
                    "{context}: gates"
                );
                assert_eq!(
                    i64::from(grid.gate_range.first_gate_m),
                    single(&summary["first_gate"], "first_gate")
                        .as_i64()
                        .unwrap(),
                    "{context}: first gate"
                );
                assert_eq!(
                    i64::from(grid.gate_range.gate_spacing_m),
                    single(&summary["gate_spacing"], "gate_spacing")
                        .as_i64()
                        .unwrap(),
                    "{context}: gate spacing"
                );
                assert_eq!(
                    u64::from(grid.storage.word_size_bits()),
                    uint(single(&summary["word_size"], "word_size"), "word_size"),
                    "{context}: word size"
                );
                assert_eq!(
                    grid.scale,
                    single(&summary["scale"], "scale").as_f64().unwrap() as f32,
                    "{context}: scale"
                );
                assert_eq!(
                    grid.offset,
                    single(&summary["offset"], "offset").as_f64().unwrap() as f32,
                    "{context}: offset"
                );
                let (grid_sum, grid_zeros, grid_ones) = code_counts(&grid.storage);
                sum += grid_sum;
                zeros += grid_zeros;
                ones += grid_ones;
                cells += (grid.radial_count() * grid.gate_range.gate_count) as u64;
            }
            // Rows shorter than the grid are padded with code 0.
            let padding = cells - uint(&summary["gates"], "gates");
            assert_eq!(
                sum,
                uint(&summary["code_sum"], "code_sum"),
                "{context}: code sum"
            );
            assert_eq!(
                zeros - padding,
                uint(&summary["code_0"], "code_0"),
                "{context}: below threshold"
            );
            assert_eq!(
                ones,
                uint(&summary["code_1"], "code_1"),
                "{context}: range folded"
            );
        }
    }
}

#[test]
fn volumes_match_pyart_rays_and_moments() {
    let goldens = goldens();
    assert_eq!(goldens.len(), 4, "volume goldens");
    let mut checked = 0;
    let mut sources = Vec::new();
    for (name, golden) in &goldens {
        let ids = source_ids(golden);
        sources.push(ids.clone());
        let Some(bytes) = load_all(&ids) else {
            continue;
        };
        let volume =
            decode_volume_from_bytes(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        check_volume(name, &volume, golden);
        checked += 1;
    }
    assert_checked_every_available("volume goldens", checked, &sources);
}

/// KVWX 2008-04-15 writes four spaces as the radar identifier of all 2500
/// message 31 radials. The volume decodes (site id from the volume header,
/// "KVWX"), and the identifier falls back to the volume header ICAO.
#[test]
fn kvwx_2008_blank_radar_identifiers() {
    let Some(raw) = load_all(&[KVWX_2008]) else {
        return;
    };
    assert_eq!(&raw[..2], &[0x1f, 0x8b], "whole-file gzip");
    let volume = decode_volume_from_bytes(&raw).unwrap();
    assert_eq!(volume.site.id, "KVWX");
    assert_eq!(volume.metadata.decoded_radial_count, 2500);
    assert_eq!(volume.cuts.len(), 7);

    let with_metadata = decode_volume_with_metadata(&raw).unwrap();
    assert_eq!(with_metadata.volume, volume);
    assert_eq!(
        with_metadata
            .metadata
            .per_sweep_elevation_data
            .as_ref()
            .map(Vec::len),
        Some(7)
    );

    let records = messages::record_bytes(&raw).unwrap();
    let mut radials = 0;
    for (header, body) in MessageWalker::new(&records).flatten() {
        let MessageBody::DigitalRadarDataGeneric(radial) = body else {
            continue;
        };
        assert_eq!(header.message_type, 31);
        let identifier = &radial.header;
        assert_eq!(&identifier.radar_identifier, b"    ");
        assert_eq!(identifier.radar_identifier_str(), "");
        assert_eq!(identifier.radar_identifier_or(&volume.site.id), "KVWX");
        assert_eq!(identifier.radar_identifier_or(" \0"), "");
        radials += 1;
    }
    assert_eq!(radials, 2500);

    // The volume decoder's header parser on the first radial's body: the
    // 24-byte volume header, 134 fixed frames, a 12-byte CTM header and the
    // 16-byte message header precede it.
    let normalized = recast_radar_io_nexrad::normalize_archive_bytes(&raw)
        .unwrap()
        .0;
    let body = 24 + 134 * 2432 + 12 + 16;
    assert_eq!(normalized[body - 13], 31, "first radial is message 31");
    let header = parse_message_31_header(&normalized, body).unwrap();
    assert_eq!(&header.radar_identifier, b"    ");
    assert_eq!(header.radar_identifier_or("KVWX"), "KVWX");
    assert_eq!(header.elevation_number, 1);
}
