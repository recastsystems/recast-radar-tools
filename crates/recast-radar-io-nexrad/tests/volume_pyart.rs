//! `read_volume_from_bytes` against Py-ART: sweeps, rays and moment data.
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
//! the KTLX 2024-03-15 benchmark volume (16-bit ZDR and PHI, CFP); the
//! committed KIWA real-time chunks, which run offline.

mod common;

use std::collections::BTreeSet;

use recast_radar_core::model::{Field, FieldData, FieldName, LinearTransform, Sweep, Volume};
use recast_radar_io_nexrad::messages::{self, MessageBody, MessageWalker};
use recast_radar_io_nexrad::{
    parse_message_31_header, read_volume_from_bytes, read_volume_with_metadata,
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

/// The ICD moment name Py-ART reports for a field (the golden's keys).
fn icd_name(name: &FieldName) -> &str {
    match name {
        FieldName::Dbzh => "REF",
        FieldName::Vradh => "VEL",
        FieldName::Wradh => "SW",
        FieldName::Zdr => "ZDR",
        FieldName::Phidp => "PHI",
        FieldName::Rhohv => "RHO",
        FieldName::Ccorh => "CFP",
        other => other.as_str(),
    }
}

/// Rows of a field the source provided (absent rows hold padding).
fn present_rows(field: &Field) -> usize {
    field.nrays as usize - field.absent_rows.len()
}

/// Raw codes of the rows the source provided: (sum, count of 0, count of 1).
fn code_counts(field: &Field) -> (u64, u64, u64) {
    let mut totals = (0u64, 0u64, 0u64);
    let mut add = |code: u64| {
        totals.0 += code;
        totals.1 += u64::from(code == 0);
        totals.2 += u64::from(code == 1);
    };
    let ngates = field.ngates as usize;
    for ray in (0..field.nrays as usize).filter(|ray| !field.is_absent(*ray)) {
        let start = ray * ngates;
        match &field.data {
            FieldData::U8 { values, .. } => values[start..start + ngates]
                .iter()
                .for_each(|&v| add(u64::from(v))),
            FieldData::U16 { values, .. } => values[start..start + ngates]
                .iter()
                .for_each(|&v| add(u64::from(v))),
            _ => panic!("Level II fields hold u8 or u16 codes"),
        }
    }
    totals
}

/// Milliseconds of day of a ray's collection time (the Message 31 header
/// value, Py-ART's `collect_ms`).
fn collect_ms(volume: &Volume, sweep: usize, ray: usize) -> u64 {
    let time = volume.ray_time(sweep, ray).unwrap();
    let midnight = time.date_naive().and_time(chrono::NaiveTime::MIN).and_utc();
    u64::try_from((time - midnight).num_milliseconds()).unwrap()
}

fn check_volume(name: &str, volume: &Volume, golden: &Value) {
    assert_eq!(
        volume.attrs.instrument_name,
        golden["icao"].as_str().unwrap(),
        "{name}: instrument name is the volume header ICAO, as in Py-ART"
    );
    let rays: usize = volume.sweeps.iter().map(Sweep::nrays).sum();
    assert_eq!(rays as u64, uint(&golden["nrays"], "nrays"), "{name}: rays");
    assert_eq!(
        volume.provenance.decode.decoded_ray_count as u64,
        uint(&golden["nrays"], "nrays"),
        "{name}: decoded rays"
    );

    let scans = golden["scans"].as_array().unwrap();
    for sweep in &volume.sweeps {
        let number = u64::from(
            sweep
                .elevation_number
                .expect("message 31 sweeps are numbered"),
        );
        assert!(
            (1..=scans.len() as u64).contains(&number),
            "{name}: sweep with elevation number {number}, Py-ART has {} scans",
            scans.len()
        );
    }
    for scan in scans {
        let number = uint(&scan["elevation_number"], "elevation_number");
        let what = format!("{name} elevation {number}");
        let sweeps: Vec<(usize, &Sweep)> = volume
            .sweeps
            .iter()
            .enumerate()
            .filter(|(_, sweep)| sweep.elevation_number.map(u64::from) == Some(number))
            .collect();
        let nrays: usize = sweeps.iter().map(|(_, sweep)| sweep.nrays()).sum();
        assert_eq!(nrays as u64, uint(&scan["nrays"], "nrays"), "{what}: rays");
        let collect_ms_sum: u64 = sweeps
            .iter()
            .flat_map(|(index, sweep)| (0..sweep.nrays()).map(move |ray| (*index, ray)))
            .map(|(index, ray)| collect_ms(volume, index, ray))
            .sum();
        assert_eq!(
            collect_ms_sum,
            uint(&scan["collect_ms_sum"], "collect_ms_sum"),
            "{what}: collection times"
        );
        let azimuths: f64 = sweeps
            .iter()
            .flat_map(|(_, sweep)| sweep.rays.azimuth_deg.iter().map(|az| f64::from(*az)))
            .sum();
        let expected = scan["azimuth_sum"].as_f64().unwrap();
        assert!(
            (azimuths - expected).abs() <= 1e-9 * expected.abs().max(1.0),
            "{what}: azimuth sum {azimuths} != {expected}"
        );

        let moments = scan["moments"].as_object().unwrap();
        let decoded: BTreeSet<&str> = sweeps
            .iter()
            .flat_map(|(_, sweep)| sweep.fields.iter().map(|field| icd_name(&field.name)))
            .collect();
        let expected_names: BTreeSet<&str> = moments.keys().map(String::as_str).collect();
        assert_eq!(decoded, expected_names, "{what}: moments");

        for (moment, summary) in moments {
            let context = format!("{what} {moment}");
            let fields: Vec<(&Sweep, &Field)> = sweeps
                .iter()
                .flat_map(|(_, sweep)| sweep.fields.iter().map(move |field| (*sweep, field)))
                .filter(|(_, field)| icd_name(&field.name) == moment)
                .collect();
            let rows: usize = fields.iter().map(|(_, field)| present_rows(field)).sum();
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
            for (sweep, field) in &fields {
                assert_eq!(u64::from(field.ngates), widest, "{context}: gates");
                let (first_gate_m, spacing_m) = field.native_geometry(&sweep.range).unwrap();
                assert_eq!(
                    first_gate_m,
                    single(&summary["first_gate"], "first_gate")
                        .as_f64()
                        .unwrap(),
                    "{context}: first gate centre"
                );
                assert_eq!(
                    spacing_m,
                    single(&summary["gate_spacing"], "gate_spacing")
                        .as_f64()
                        .unwrap(),
                    "{context}: gate spacing"
                );
                let (word_size, transform) = match &field.data {
                    FieldData::U8 { coding, .. } => (8, coding.transform),
                    FieldData::U16 { coding, .. } => (16, coding.transform),
                    other => panic!("{context}: Level II fields are u8 or u16, got {other:?}"),
                };
                assert_eq!(
                    word_size,
                    uint(single(&summary["word_size"], "word_size"), "word_size"),
                    "{context}: word size"
                );
                let LinearTransform::IcdScaleOffset { scale, offset } = transform else {
                    panic!("{context}: Level II fields use the ICD scale/offset form");
                };
                assert_eq!(
                    scale,
                    single(&summary["scale"], "scale").as_f64().unwrap() as f32,
                    "{context}: scale"
                );
                assert_eq!(
                    offset,
                    single(&summary["offset"], "offset").as_f64().unwrap() as f32,
                    "{context}: offset"
                );
                let (field_sum, field_zeros, field_ones) = code_counts(field);
                sum += field_sum;
                zeros += field_zeros;
                ones += field_ones;
                cells += (present_rows(field) * field.ngates as usize) as u64;
            }
            // Rows shorter than the field are padded with code 0.
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
            read_volume_from_bytes(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        check_volume(name, &volume, golden);
        checked += 1;
    }
    assert_checked_every_available("volume goldens", checked, &sources);
}

/// KVWX 2008-04-15 writes four spaces as the radar identifier of all 2500
/// message 31 radials. The volume decodes (instrument name from the volume
/// header, "KVWX"), and the identifier falls back to the volume header ICAO.
#[test]
fn kvwx_2008_blank_radar_identifiers() {
    let Some(raw) = load_all(&[KVWX_2008]) else {
        return;
    };
    assert_eq!(&raw[..2], &[0x1f, 0x8b], "whole-file gzip");
    let volume = read_volume_from_bytes(&raw).unwrap();
    assert_eq!(volume.attrs.instrument_name, "KVWX");
    assert_eq!(volume.provenance.decode.decoded_ray_count, 2500);
    assert_eq!(volume.sweeps.len(), 7);

    let with_metadata = read_volume_with_metadata(&raw).unwrap();
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
        assert_eq!(
            identifier.radar_identifier_or(&volume.attrs.instrument_name),
            "KVWX"
        );
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
