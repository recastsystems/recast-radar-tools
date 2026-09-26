//! Message 31 (Digital Radar Data Generic Format) decoding on real files.
//!
//! Golden values: `testdata/level2/golden/msg31/*.json`, written by
//! `python tools/level2_golden.py msg31` with MetPy 1.7.1's `Level2File`. For
//! every sweep MetPy forms, the tests compare the first radial field by field
//! and every radial through per-field summaries (distinct values, or count,
//! min, max and sum in radial order). MetPy reads a fixed 44-byte VOL block
//! and skips the RAD flags, so the ZDR bias estimate, radial flags and spare
//! bytes are checked against values read from the file bytes (offsets in the
//! tests), and against the ICD ranges.
//!
//! Where MetPy and the ICD disagree the tests follow the ICD and say so:
//! MetPy scales the SNR threshold by 0.1 dB, Table XVII-B by 0.125 dB. The
//! 0.125 scale is confirmed independently: every moment's threshold equals
//! the message 5 SNR threshold MetPy decodes for the same elevation cut,
//! except on TDWR TSTL's last cut (see `file_tstl_2023_tdwr`).
//!
//! Layout coverage: Build 10.0 (VOL 44, RAD 20, 68-byte header), 12.0, 13.1,
//! 14.0 (RAD 28), 18.2, 19.1 (72-byte header, CFP), 20.1 and 21.0 (VOL 52),
//! 22.0, 24.1 and TDWR, plus the committed KIWA real-time chunks, which keep
//! one golden test running offline.
//!
//! The mutation tests start from the first radial of the committed KIWA
//! intermediate chunk and change single fields to exercise unknown blocks,
//! layout selection by size, compression and pointer errors.

// A panic is how a test fails (clippy.toml), in helpers too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::io::{Read, Write};

use recast_radar_core::model::FieldName;
use recast_radar_io_nexrad::NexradError;
use recast_radar_io_nexrad::messages::msg31_blocks::{
    AzimuthResolution, CompressionIndicator, ControlFlags, DataMomentName, DigitalRadarDataGeneric,
    GateValue, ProcessingStatus, RadialBlockLayout, RdaBuild, VolumeBlockLayout,
};
use recast_radar_io_nexrad::messages::{self, MessageBody, MessageWalker};
use serde_json::Value;

const CHUNK_START: &str = "l2chunk-kiwa-307-20260917-003629-001-s";
const CHUNK_002: &str = "l2chunk-kiwa-307-20260917-003629-002-i";
const CHUNK_003: &str = "l2chunk-kiwa-307-20260917-003629-003-i";

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

/// Decompressed record bytes of several files, concatenated.
fn records(ids: &[&str]) -> Option<Vec<u8>> {
    let mut records = Vec::new();
    for id in ids {
        let raw = load(id)?;
        records.extend_from_slice(&messages::record_bytes(&raw).unwrap());
    }
    Some(records)
}

/// Every message 31 in walk order, decoded. A message 31 the decoder rejects
/// fails the test; framing errors of other message types (stale metadata
/// frames in 2008 files) are ignored.
fn radials(records: &[u8]) -> Vec<DigitalRadarDataGeneric<'_>> {
    let mut radials = Vec::new();
    for item in MessageWalker::new(records) {
        match item {
            Ok((header, MessageBody::DigitalRadarDataGeneric(radial))) => {
                assert_eq!(header.message_type, 31);
                radials.push(*radial);
            }
            Ok((header, _)) => assert_ne!(header.message_type, 31),
            Err(error) => assert!(
                !error.to_string().contains("message type 31"),
                "message 31 rejected: {error}"
            ),
        }
    }
    radials
}

fn golden(name: &str) -> Value {
    let path = format!(
        "{}/../../testdata/level2/golden/msg31/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap()
}

// MetPy's representation of a radial ------------------------------------------------

/// A MetPy value after the golden script's normalization.
#[derive(Clone, Debug)]
enum V {
    N(f64),
    S(String),
}

impl V {
    fn from_json(value: &Value) -> Self {
        match value {
            Value::Number(number) => Self::N(number.as_f64().unwrap()),
            Value::String(text) => Self::S(text.clone()),
            other => panic!("unexpected golden value {other}"),
        }
    }

    fn matches(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::N(a), Self::N(b)) => close(*a, *b),
            (Self::S(a), Self::S(b)) => a == b,
            _ => false,
        }
    }
}

/// Equal up to JSON float parsing (serde_json may land one ulp away).
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-12 * a.abs().max(b.abs()).max(1.0)
}

fn n(value: impl Into<f64>) -> V {
    V::N(value.into())
}

/// MetPy's `BitField(*names)` joined with "|" ("" for none).
fn bitfield(value: u32, names: &[&str]) -> V {
    let set: Vec<&str> = names
        .iter()
        .enumerate()
        .filter(|(bit, _)| value >> bit & 1 == 1)
        .map(|(_, name)| *name)
        .collect();
    V::S(set.join("|"))
}

/// MetPy's `remap_status` bitmask.
fn remap_status(code: u8) -> u32 {
    const START_ELEVATION: u32 = 0x1;
    const END_ELEVATION: u32 = 0x2;
    const START_VOLUME: u32 = 0x4;
    const END_VOLUME: u32 = 0x8;
    const LAST_ELEVATION: u32 = 0x10;
    const BAD_DATA: u32 = 0x20;
    let bad = if code & 0xF0 != 0 { BAD_DATA } else { 0 };
    let status = match code & 0x0F {
        0 => START_ELEVATION,
        2 => END_ELEVATION,
        3 => START_ELEVATION | START_VOLUME,
        4 => END_ELEVATION | END_VOLUME,
        5 => START_ELEVATION | LAST_ELEVATION,
        _ => 0,
    };
    status | bad
}

/// One radial flattened with MetPy's field names and conversions.
fn metpy_fields(radial: &DigitalRadarDataGeneric<'_>) -> BTreeMap<String, V> {
    let mut fields = BTreeMap::new();
    let mut put = |key: String, value: V| {
        fields.insert(key, value);
    };
    let h = &radial.header;
    put(
        "header.stid".into(),
        V::S(h.radar_identifier.iter().map(|&b| char::from(b)).collect()),
    );
    put("header.time_ms".into(), n(h.collection_time_ms));
    put("header.date".into(), n(h.modified_julian_date));
    put("header.az_num".into(), n(h.azimuth_number));
    put("header.az_angle".into(), n(h.azimuth_angle_deg));
    put("header.compression".into(), n(h.compression.code()));
    put("header.rad_length".into(), n(h.radial_length));
    put(
        "header.az_spacing".into(),
        match h.azimuth_resolution.code() {
            0 => n(0),
            1 => n(0.5),
            2 => n(1.0),
            other => V::S(format!("Unknown ({other})")),
        },
    );
    put(
        "header.rad_status".into(),
        n(remap_status(h.radial_status_code)),
    );
    put("header.el_num".into(), n(h.elevation_number));
    put("header.sector_num".into(), n(h.cut_sector_number));
    put("header.el_angle".into(), n(h.elevation_angle_deg));
    put(
        "header.spot_blanking".into(),
        bitfield(
            u32::from(h.spot_blanking.0),
            &["Radial", "Elevation", "Volume"],
        ),
    );
    put(
        "header.az_index_mode".into(),
        n(f64::from(h.azimuth_indexing_raw) * 0.01),
    );
    put("header.num_data_blks".into(), n(h.data_block_count));
    if let Some(vol) = &radial.volume {
        put("vol.size".into(), n(vol.block_size));
        put("vol.major".into(), n(vol.version_major));
        put("vol.minor".into(), n(vol.version_minor));
        put("vol.lat".into(), n(vol.latitude_deg));
        put("vol.lon".into(), n(vol.longitude_deg));
        put("vol.site_amsl".into(), n(vol.site_height_m));
        put("vol.feedhorn_agl".into(), n(vol.feedhorn_height_m));
        put("vol.calib_dbz".into(), n(vol.calibration_constant_db));
        put("vol.txpower_h".into(), n(vol.horizontal_shv_tx_power_kw));
        put("vol.txpower_v".into(), n(vol.vertical_shv_tx_power_kw));
        put(
            "vol.sys_zdr".into(),
            n(vol.system_differential_reflectivity_db),
        );
        put(
            "vol.phidp0".into(),
            n(vol.initial_system_differential_phase_deg),
        );
        put("vol.vcp".into(), n(vol.vcp_number));
        put(
            "vol.processing_status".into(),
            bitfield(u32::from(vol.processing_status.0), &["RxR Noise", "CBT"]),
        );
    }
    if let Some(elv) = &radial.elevation {
        put("elv.size".into(), n(elv.block_size));
        put(
            "elv.atmos_atten".into(),
            n(f64::from(elv.atmospheric_attenuation_raw) * 0.001),
        );
        put("elv.calib_dbz0".into(), n(elv.calibration_constant_db));
    }
    if let Some(rad) = &radial.radial {
        put("rad.size".into(), n(rad.block_size));
        put(
            "rad.unamb_range".into(),
            n(f64::from(rad.unambiguous_range_raw) * 0.1),
        );
        put("rad.noise_h".into(), n(rad.horizontal_noise_level_dbm));
        put("rad.noise_v".into(), n(rad.vertical_noise_level_dbm));
        put(
            "rad.nyq_vel".into(),
            n(f64::from(rad.nyquist_velocity_raw) * 0.01),
        );
        if let (Some(h), Some(v)) = (
            rad.horizontal_calibration_constant_dbz,
            rad.vertical_calibration_constant_dbz,
        ) {
            put("rad.calib_dbz0_h".into(), n(h));
            put("rad.calib_dbz0_v".into(), n(v));
        }
    }
    for moment in &radial.moments {
        let prefix = format!("moment.{}", moment.name.short_name());
        put(format!("{prefix}.reserved"), n(moment.reserved));
        put(format!("{prefix}.num_gates"), n(moment.gate_count));
        put(
            format!("{prefix}.first_gate"),
            n(f64::from(moment.first_gate_range_m) * 0.001),
        );
        put(
            format!("{prefix}.gate_width"),
            n(f64::from(moment.gate_spacing_m) * 0.001),
        );
        put(
            format!("{prefix}.tover"),
            n(f64::from(moment.tover_raw) * 0.1),
        );
        // MetPy's 0.1 dB scale, not the ICD's 0.125 (see the module docs).
        put(
            format!("{prefix}.snr_thresh"),
            n(f64::from(moment.snr_threshold_raw) * 0.1),
        );
        put(
            format!("{prefix}.recombined"),
            bitfield(
                u32::from(moment.control_flags.code()),
                &["Azimuths", "Gates"],
            ),
        );
        put(format!("{prefix}.data_size"), n(moment.data_word_size));
        put(format!("{prefix}.scale"), n(moment.scale));
        put(format!("{prefix}.offset"), n(moment.offset));
    }
    fields
}

/// Radials grouped into sweeps the way MetPy's `Level2File._add_sweep` does:
/// a new sweep at each start-of-elevation status, padded with empty sweeps
/// up to the elevation number.
fn metpy_sweeps<'r, 'a>(
    radials: &'r [DigitalRadarDataGeneric<'a>],
) -> Vec<Vec<&'r DigitalRadarDataGeneric<'a>>> {
    let mut sweeps: Vec<Vec<_>> = Vec::new();
    for radial in radials {
        if remap_status(radial.header.radial_status_code) & 0x1 != 0 {
            sweeps.push(Vec::new());
        }
        while sweeps.len() < usize::from(radial.header.elevation_number) {
            sweeps.push(Vec::new());
        }
        sweeps.last_mut().unwrap().push(radial);
    }
    sweeps
}

fn check_summary(context: &str, golden: &Value, values: &[&V]) {
    assert_eq!(
        golden["count"].as_u64().unwrap() as usize,
        values.len(),
        "{context}: count"
    );
    if let Some(distinct) = golden["distinct"].as_array() {
        let expected: Vec<V> = distinct.iter().map(V::from_json).collect();
        for value in values {
            assert!(
                expected.iter().any(|e| e.matches(value)),
                "{context}: {value:?} not in {expected:?}"
            );
        }
        for e in &expected {
            assert!(
                values.iter().any(|value| e.matches(value)),
                "{context}: golden value {e:?} never decoded"
            );
        }
        return;
    }
    let numbers: Vec<f64> = values
        .iter()
        .map(|value| match value {
            V::N(number) => *number,
            V::S(text) => panic!("{context}: string {text} in a numeric summary"),
        })
        .collect();
    let min = numbers.iter().copied().fold(f64::INFINITY, f64::min);
    let max = numbers.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let sum = numbers.iter().fold(0.0, |total, value| total + value);
    for (key, actual) in [("min", min), ("max", max), ("sum", sum)] {
        let expected = golden[key].as_f64().unwrap();
        assert!(
            close(actual, expected),
            "{context}: {key} {actual} != {expected}"
        );
    }
}

/// One real volume (or chunk set) with a MetPy golden, and the values its
/// radials carry that MetPy does not expose, read from the file bytes by a
/// separate Python reader.
struct FileCase {
    /// Golden file name under `testdata/level2/golden/msg31`.
    golden: &'static str,
    /// Manifest ids read as one concatenated file.
    ids: &'static [&'static str],
    /// First block pointer (the recorded Data Header Block length) of every
    /// radial.
    header_len: usize,
    /// VOL LRTUP size of every radial.
    vol_size: u16,
    /// RAD LRTUP size of every radial.
    rad_size: u16,
    /// VOL major version of every radial.
    vol_major: u8,
    /// VOL processing status bits of every radial.
    processing_status: u16,
    /// VOL bytes 44-45 of every radial (a volume constant); `None` for the
    /// 44-byte layout.
    zdr_bias_raw: Option<u16>,
    /// Word size, scale and offset of every ZDR moment block (Table XVII-B
    /// bytes 19-27); `None` when no radial carries ZDR. Builds 12 to 18 write
    /// 8-bit ZDR with scale 16 and offset 128; Build 19 on writes the 16-bit
    /// Table XVII-I encoding, scale 32 and offset 418.
    zdr_encoding: Option<(u8, f32, f32)>,
    /// Elevation number and moment pairs whose message 31 SNR threshold
    /// differs from message 5.
    snr_exceptions: &'static [(u8, &'static str)],
}

/// Everything about one file: MetPy golden sweeps, RDA build, message 5 SNR
/// thresholds, layouts, and the fields MetPy does not read.
fn check_file(case: &FileCase) {
    let name = case.golden;
    let Some(records) = records(case.ids) else {
        return;
    };
    let golden = golden(name);
    let sources: Vec<&str> = golden["source"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();
    assert_eq!(sources, case.ids, "{name}: golden sources");

    let radials = radials(&records);
    assert!(!radials.is_empty(), "{name}: no message 31");
    check_golden_sweeps(name, &golden, &radials);
    check_rda_build(name, &golden, case.ids[0]);
    check_snr_thresholds(case, &golden, &radials);
    check_layouts(case, &radials);
}

/// Compare every sweep MetPy forms with the golden.
fn check_golden_sweeps(name: &str, golden: &Value, radials: &[DigitalRadarDataGeneric<'_>]) {
    let sweeps = metpy_sweeps(radials);
    let golden_sweeps = golden["sweeps"].as_array().unwrap();
    assert_eq!(sweeps.len(), golden_sweeps.len(), "{name}: sweep count");

    for (index, (sweep, expected)) in sweeps.iter().zip(golden_sweeps).enumerate() {
        let context = format!("{name} sweep {index}");
        assert_eq!(
            sweep.len() as u64,
            expected["radials"].as_u64().unwrap(),
            "{context}: radials"
        );
        let flattened: Vec<BTreeMap<String, V>> = sweep.iter().map(|r| metpy_fields(r)).collect();

        let first = expected["first"].as_object().unwrap();
        if let Some(actual) = flattened.first() {
            let mut expected_keys: Vec<&String> = first.keys().collect();
            expected_keys.sort();
            let actual_keys: Vec<&String> = actual.keys().collect();
            assert_eq!(actual_keys, expected_keys, "{context}: first radial fields");
            for (key, value) in first {
                assert!(
                    actual[key].matches(&V::from_json(value)),
                    "{context}: first radial {key}: {:?} != {value}",
                    actual[key]
                );
            }
        } else {
            assert!(first.is_empty(), "{context}: MetPy has a first radial");
        }

        let fields = expected["fields"].as_object().unwrap();
        let mut keys: Vec<&String> = flattened.iter().flat_map(|f| f.keys()).collect();
        keys.sort();
        keys.dedup();
        let mut golden_keys: Vec<&String> = fields.keys().collect();
        golden_keys.sort();
        assert_eq!(keys, golden_keys, "{context}: field names");
        for (key, summary) in fields {
            let values: Vec<&V> = flattened.iter().filter_map(|f| f.get(key)).collect();
            check_summary(&format!("{context} {key}"), summary, &values);
        }
    }
}

/// The RDA build of the first message 2 against MetPy's `rda_build`, which
/// formats note 6's value with one decimal.
fn check_rda_build(name: &str, golden: &Value, id: &str) {
    let Some(raw) = load(id) else { return };
    let record = messages::metadata_record(&raw).unwrap();
    let build = RdaBuild::from_records(&record).unwrap();
    let metpy = format!("{:.1}", f64::from(build.hundredths()) / 100.0);
    assert_eq!(
        Value::String(metpy),
        golden["rda_build"],
        "{name}: RDA build raw {}",
        build.raw()
    );
}

/// Every moment's SNR threshold at the ICD's 0.125 dB scale equals the
/// message 5 threshold MetPy decodes for its elevation cut.
fn check_snr_thresholds(case: &FileCase, golden: &Value, radials: &[DigitalRadarDataGeneric<'_>]) {
    let name = case.golden;
    let cuts = golden["vcp_snr_thresholds_db"].as_array().unwrap();
    if cuts.is_empty() {
        // KVWX 2008 has no message 5.
        assert!(case.snr_exceptions.is_empty());
        return;
    }
    let keys: Vec<&str> = golden["vcp_snr_threshold_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap())
        .collect();
    let mut checked = std::collections::BTreeSet::new();
    let mut exceptions_seen = std::collections::BTreeSet::new();
    for radial in radials {
        let elevation = radial.header.elevation_number;
        for moment in &radial.moments {
            let key = match moment.name {
                DataMomentName::Reflectivity => "ref_thresh",
                DataMomentName::Velocity => "vel_thresh",
                DataMomentName::SpectrumWidth => "sw_thresh",
                DataMomentName::DifferentialReflectivity => "zdr_thresh",
                DataMomentName::DifferentialPhase => "phidp_thresh",
                DataMomentName::CorrelationCoefficient => "rhohv_thresh",
                // Note 31: the SNR threshold is not applied to CFP.
                _ => continue,
            };
            let column = keys.iter().position(|k| *k == key).unwrap();
            let expected = cuts[usize::from(elevation) - 1][column].as_f64().unwrap();
            let moment_name = moment.name.short_name();
            let context = format!("{name} elevation {elevation} {moment_name}");
            if case
                .snr_exceptions
                .iter()
                .any(|(el, m)| *el == elevation && *m == moment_name)
            {
                assert_eq!(moment.snr_threshold_raw, 0, "{context}");
                assert_eq!(expected, 1.0, "{context}");
                exceptions_seen.insert((elevation, moment_name.clone()));
            } else {
                assert_eq!(f64::from(moment.snr_threshold_db()), expected, "{context}");
            }
            checked.insert((elevation, moment_name));
        }
    }
    assert_eq!(
        exceptions_seen.len(),
        case.snr_exceptions.len(),
        "{name}: exceptions"
    );
    assert!(!checked.is_empty(), "{name}: no thresholds checked");
    eprintln!("{name}: {} cut/moment SNR thresholds", checked.len());
}

/// Layouts, versions and the fields MetPy does not read, on every radial.
fn check_layouts(case: &FileCase, radials: &[DigitalRadarDataGeneric<'_>]) {
    let name = case.golden;
    let manifest_size = |prefix: &str| -> Option<u16> {
        recast_radar_testdata::entry(case.ids[0])
            .unwrap()
            .tags
            .iter()
            .find_map(|t| t.strip_prefix(prefix))
            .map(|size| size.parse().unwrap())
    };
    if let Some(size) = manifest_size("vol-block:") {
        assert_eq!(size, case.vol_size, "{name}: manifest VOL size");
    }
    if let Some(size) = manifest_size("rad-block:") {
        assert_eq!(size, case.rad_size, "{name}: manifest RAD size");
    }
    let mut zdr_radials = 0;
    for radial in radials {
        let header = &radial.header;
        assert_eq!(header.blocks_offset(), case.header_len, "{name}");
        assert!(header.pointer_table_len() <= case.header_len, "{name}");
        assert_eq!(
            header.block_pointers.iter().filter(|p| **p != 0).count(),
            usize::from(header.data_block_count),
            "{name}: pointers in use"
        );
        assert_eq!(header.spare, 0, "{name}");
        assert_eq!(header.compression, CompressionIndicator::Uncompressed);
        assert!(radial.unknown_blocks.is_empty(), "{name}");

        let vol = radial.volume.unwrap();
        assert_eq!(vol.block_size, case.vol_size, "{name}");
        let layout = if case.vol_size >= 52 {
            VolumeBlockLayout::ZdrBias52
        } else {
            VolumeBlockLayout::Original44
        };
        assert_eq!(vol.layout(), layout, "{name}");
        assert_eq!(vol.version_major, case.vol_major, "{name}");
        assert_eq!(vol.processing_status.0, case.processing_status, "{name}");
        assert_eq!(vol.zdr_bias_estimate_raw, case.zdr_bias_raw, "{name}");
        // Notes 20 and 33: converted with this radial's ZDR block. Every
        // volume with the 52-byte VOL layout writes the Table XVII-I
        // encoding, so the fallback for radials without a ZDR block (Doppler
        // cuts) gives the same value there.
        let zdr = radial.moment(DataMomentName::DifferentialReflectivity);
        if let Some(zdr) = zdr {
            let expected = case
                .zdr_encoding
                .unwrap_or_else(|| panic!("{name}: unexpected ZDR block"));
            assert_eq!(
                (zdr.data_word_size, zdr.scale, zdr.offset),
                expected,
                "{name}: ZDR encoding"
            );
            zdr_radials += 1;
        }
        assert_eq!(
            radial.zdr_bias_estimate_db(),
            vol.zdr_bias_estimate_db(zdr),
            "{name}"
        );
        if let Some(db) = radial.zdr_bias_estimate_db() {
            if let Some(zdr) = zdr {
                assert_eq!((zdr.scale, zdr.offset), (32.0, 418.0), "{name}");
            }
            assert_eq!(
                radial.zdr_bias_estimate_db(),
                vol.zdr_bias_estimate_db(None),
                "{name}"
            );
            assert!((-13.0..=20.0).contains(&db), "{name}: ZDR bias {db}");
        }

        let rad = radial.radial.unwrap();
        assert_eq!(rad.block_size, case.rad_size, "{name}");
        let layout = if case.rad_size >= 28 {
            RadialBlockLayout::Calibration28
        } else {
            RadialBlockLayout::Original20
        };
        assert_eq!(rad.layout(), layout, "{name}");
        // Table XVII-H: radial flags are set to 0 (spare before Build 19).
        assert_eq!(rad.radial_flags, 0, "{name}");
        assert_eq!(radial.elevation.unwrap().block_size, 12, "{name}");
    }
    assert_eq!(
        zdr_radials > 0,
        case.zdr_encoding.is_some(),
        "{name}: {zdr_radials} radials with ZDR"
    );
}

// Real files against MetPy goldens --------------------------------------------------------

#[test]
fn file_kvwx_2008_blank_icao_no_message_5() {
    check_file(&FileCase {
        golden: "l2-kvwx-20080415-235337",
        ids: &["l2-kvwx-20080415-235337"],
        header_len: 68,
        vol_size: 44,
        rad_size: 20,
        vol_major: 1,
        processing_status: 0,
        zdr_bias_raw: None,
        zdr_encoding: None,
        snr_exceptions: &[],
    });
}

#[test]
fn file_kpah_2008_build_10() {
    check_file(&FileCase {
        golden: "l2-kpah-20080415-235014",
        ids: &["l2-kpah-20080415-235014"],
        header_len: 68,
        vol_size: 44,
        rad_size: 20,
        vol_major: 1,
        processing_status: 0,
        zdr_bias_raw: None,
        zdr_encoding: None,
        snr_exceptions: &[],
    });
}

#[test]
fn file_kdmx_2008_build_10_super_resolution() {
    check_file(&FileCase {
        golden: "l2-kdmx-20080525-205148",
        ids: &["l2-kdmx-20080525-205148"],
        header_len: 68,
        vol_size: 44,
        rad_size: 20,
        vol_major: 1,
        processing_status: 0,
        zdr_bias_raw: None,
        zdr_encoding: None,
        snr_exceptions: &[],
    });
}

#[test]
fn file_kvnx_2011_build_12() {
    check_file(&FileCase {
        golden: "l2-kvnx-20110315-000203",
        ids: &["l2-kvnx-20110315-000203"],
        header_len: 68,
        vol_size: 44,
        rad_size: 20,
        vol_major: 1,
        processing_status: 0,
        zdr_bias_raw: None,
        zdr_encoding: Some((8, 16.0, 128.0)),
        snr_exceptions: &[],
    });
}

#[test]
fn file_kgwx_2013_build_13_recombined() {
    check_file(&FileCase {
        golden: "l2-kgwx-20130601-235640",
        ids: &["l2-kgwx-20130601-235640"],
        header_len: 68,
        vol_size: 44,
        rad_size: 20,
        vol_major: 1,
        processing_status: 0,
        zdr_bias_raw: None,
        zdr_encoding: Some((8, 16.0, 128.0)),
        snr_exceptions: &[],
    });
}

#[test]
fn file_koax_2014_build_14_rad_28() {
    check_file(&FileCase {
        golden: "l2-koax-20140616-205305",
        ids: &["l2-koax-20140616-205305"],
        header_len: 68,
        vol_size: 44,
        rad_size: 28,
        vol_major: 2,
        processing_status: 1,
        zdr_bias_raw: None,
        zdr_encoding: Some((8, 16.0, 128.0)),
        snr_exceptions: &[],
    });
}

#[test]
fn file_kdvn_2020_build_18() {
    check_file(&FileCase {
        golden: "l2-kdvn-20200810-180401",
        ids: &["l2-kdvn-20200810-180401"],
        header_len: 68,
        vol_size: 44,
        rad_size: 28,
        vol_major: 2,
        processing_status: 1,
        zdr_bias_raw: None,
        zdr_encoding: Some((8, 16.0, 128.0)),
        snr_exceptions: &[],
    });
}

#[test]
fn file_klix_2021_build_19_cfp_72_byte_header() {
    check_file(&FileCase {
        golden: "l2-klix-20210829-180425",
        ids: &["l2-klix-20210829-180425"],
        header_len: 72,
        vol_size: 44,
        rad_size: 28,
        vol_major: 2,
        processing_status: 3,
        zdr_bias_raw: None,
        zdr_encoding: Some((16, 32.0, 418.0)),
        snr_exceptions: &[],
    });
}

#[test]
fn file_kbox_2022_build_20_vol_52() {
    check_file(&FileCase {
        golden: "l2-kbox-20220129-150537",
        ids: &["l2-kbox-20220129-150537"],
        header_len: 72,
        vol_size: 52,
        rad_size: 28,
        vol_major: 3,
        processing_status: 3,
        // -0.125 dB.
        zdr_bias_raw: Some(0x019e),
        zdr_encoding: Some((16, 32.0, 418.0)),
        snr_exceptions: &[],
    });
}

#[test]
fn file_kmaf_2023_build_21_zdr_bias_not_available() {
    check_file(&FileCase {
        golden: "l2-kmaf-20230331-230843",
        ids: &["l2-kmaf-20230331-230843"],
        header_len: 72,
        vol_size: 52,
        rad_size: 28,
        vol_major: 3,
        processing_status: 3,
        zdr_bias_raw: Some(0),
        zdr_encoding: Some((16, 32.0, 418.0)),
        snr_exceptions: &[],
    });
}

#[test]
fn file_tstl_2023_tdwr() {
    check_file(&FileCase {
        golden: "l2-tstl-20230331-230314",
        ids: &["l2-tstl-20230331-230314"],
        header_len: 68,
        vol_size: 44,
        rad_size: 20,
        vol_major: 1,
        processing_status: 0,
        zdr_bias_raw: None,
        zdr_encoding: None,
        // The last cut records 0 dB for its three moments where message 5
        // says 1.0 dB.
        snr_exceptions: &[(23, "REF"), (23, "VEL"), (23, "SW")],
    });
}

#[test]
fn file_ktlx_2024_build_22() {
    check_file(&FileCase {
        golden: "l2-ktlx-20240315-000217",
        ids: &["l2-ktlx-20240315-000217"],
        header_len: 72,
        vol_size: 52,
        rad_size: 28,
        vol_major: 3,
        processing_status: 3,
        zdr_bias_raw: Some(0),
        zdr_encoding: Some((16, 32.0, 418.0)),
        snr_exceptions: &[],
    });
}

#[test]
fn file_kiwa_2026_build_24() {
    check_file(&FileCase {
        golden: "l2-kiwa-20260917-003629",
        ids: &["l2-kiwa-20260917-003629"],
        header_len: 72,
        vol_size: 52,
        rad_size: 28,
        vol_major: 3,
        processing_status: 3,
        // +0.1875 dB.
        zdr_bias_raw: Some(0x01a8),
        zdr_encoding: Some((16, 32.0, 418.0)),
        snr_exceptions: &[],
    });
}

#[test]
fn file_kiwa_committed_chunks() {
    check_file(&FileCase {
        golden: "l2chunk-kiwa-307-20260917-003629-001-s..003",
        ids: &[CHUNK_START, CHUNK_002, CHUNK_003],
        header_len: 72,
        vol_size: 52,
        rad_size: 28,
        vol_major: 3,
        processing_status: 3,
        zdr_bias_raw: Some(0x01a8),
        zdr_encoding: Some((16, 32.0, 418.0)),
        snr_exceptions: &[],
    });
}

// RDA build ------------------------------------------------------------------------------

/// Raw halfword 10 of the first message 2 in each metadata record (body bytes
/// 18-19), read from the files, and the build it encodes.
#[test]
fn rda_build_from_metadata_records() {
    let expected: &[(&str, u16, &str)] = &[
        ("l2-ktlx-19910605-162126", 0, "0.0"),
        ("l2-klix-20050829-130035", 0, "0.0"),
        // KVWX 2008 (header version mismatch) records 1996, which decodes as
        // 19.96 per note 6, a build that did not exist in 2008.
        ("l2-kvwx-20080415-235337", 1996, "19.96"),
        ("l2-kpah-20080415-235014", 100, "10.0"),
        ("l2-kdmx-20080525-205148", 100, "10.0"),
        ("l2-kvnx-20110315-000203", 1200, "12.0"),
        ("l2-ktlx-20130520-201643", 1320, "13.2"),
        ("l2-kgwx-20130601-235640", 1310, "13.1"),
        ("l2-koax-20140616-205305", 1400, "14.0"),
        ("l2-kewx-20160413-022531", 1610, "16.1"),
        ("l2-kdvn-20200810-180401", 1820, "18.2"),
        ("l2-klix-20210829-180425", 1910, "19.1"),
        ("l2-kbox-20220129-150537", 2010, "20.1"),
        ("l2-tjua-20220918-190621", 2010, "20.1"),
        ("l2-kdgx-20230325-010651", 2110, "21.1"),
        ("l2-kmaf-20230331-230843", 2100, "21.0"),
        ("l2-tstl-20230331-230314", 20, "2.0"),
        ("l2-pgua-20230524-030945", 2110, "21.1"),
        ("l2-tbwi-20230601-175101-stub", 20, "2.0"),
        ("l2-kmtx-20240301-212827", 2200, "22.0"),
        ("l2-ktlx-20240315-000217", 2200, "22.0"),
        ("l2-ktlx-20240515-000014", 2210, "22.1"),
        ("l2-pahg-20250909-212549", 2310, "23.1"),
        ("l2-kilx-20260418-013553", 2310, "23.1"),
        ("l2-kiwa-20260917-003629", 2410, "24.1"),
        (CHUNK_START, 2410, "24.1"),
    ];
    let mut checked = 0;
    for (id, raw_build, text) in expected {
        let Some(raw) = load(id) else { continue };
        checked += 1;
        let record = messages::metadata_record(&raw).unwrap();
        let build = RdaBuild::from_records(&record).unwrap();
        assert_eq!(build.raw(), *raw_build, "{id}");
        assert_eq!(build.to_string(), *text, "{id}");
        // Manifest build tags agree wherever the manifest records one.
        let entry = recast_radar_testdata::entry(id).unwrap();
        if let Some(tag) = entry.tags.iter().find_map(|t| t.strip_prefix("build:")) {
            assert_eq!(tag, *text, "{id}: manifest build tag");
        }
    }
    let sources: Vec<Vec<&str>> = expected.iter().map(|(id, _, _)| vec![*id]).collect();
    common::assert_checked_every_available("RDA builds", checked, &sources);
}

// Exact values from the committed chunk -------------------------------------------------

/// The first radial of the committed KIWA intermediate chunk 002 (record
/// bytes offset 0, message body at offset 28, 9928 bytes), every field
/// checked against the bytes. Hex below is the body.
#[test]
fn first_radial_of_committed_chunk_decodes_every_field() {
    let Some(records) = records(&[CHUNK_002]) else {
        return;
    };
    let radials = radials(&records);
    assert_eq!(radials.len(), 120);
    let radial = &radials[0];

    // 4b495741 00216855 50ea 0001 431b3b38 00 00 26c8 01 03 01 01 3e886800
    // 00 19 0008, pointers 00000048 0000007c 00000088 000000a4 000007e8
    // 00001154 00001ac0 00001f84 then two zero slots.
    let h = &radial.header;
    assert_eq!(h.radar_identifier_str(), "KIWA");
    assert_eq!(h.collection_time_ms, 2_189_397);
    assert_eq!(h.modified_julian_date, 20714);
    assert_eq!(
        h.collection_time().to_rfc3339(),
        "2026-09-17T00:36:29.397+00:00"
    );
    assert_eq!(h.azimuth_number, 1);
    assert_eq!(h.azimuth_angle_deg, 155.231_32);
    assert_eq!(h.compression, CompressionIndicator::Uncompressed);
    assert_eq!(h.spare, 0);
    assert_eq!(h.radial_length, 9928);
    assert_eq!(h.azimuth_resolution, AzimuthResolution::HalfDegree);
    assert_eq!(h.azimuth_resolution.degrees(), Some(0.5));
    assert_eq!(h.radial_status_code, 3);
    assert_eq!(
        h.radial_status(),
        recast_radar_io_nexrad::RadialStatus::StartVolume
    );
    assert!(!h.is_bad_data());
    assert_eq!(h.elevation_number, 1);
    assert_eq!(h.cut_sector_number, 1);
    assert_eq!(h.elevation_angle_deg, 0.266_418_46);
    assert_eq!(h.spot_blanking.0, 0);
    assert!(!h.spot_blanking.radial() && !h.spot_blanking.elevation());
    assert_eq!(h.azimuth_indexing_deg(), Some(0.25));
    assert_eq!(h.data_block_count, 8);
    assert_eq!(
        h.block_pointers,
        [72, 124, 136, 164, 2024, 4436, 6848, 8068]
    );
    assert_eq!(h.pointer_table_len(), 64);
    assert_eq!(h.blocks_offset(), 72);

    // RVOL 0034 03 00 4205282d c2df56ff 019f 0013 c2343bfc 435c732e 4362299a
    // bf51f53d 42700000 00d7 0003 01a8 000000000000
    let vol = radial.volume.unwrap();
    assert_eq!(vol.block_size, 52);
    assert_eq!(vol.layout(), VolumeBlockLayout::ZdrBias52);
    assert_eq!((vol.version_major, vol.version_minor), (3, 0));
    assert_eq!(vol.latitude_deg, 33.289_234);
    assert_eq!(vol.longitude_deg, -111.669_914);
    assert_eq!(vol.site_height_m, 415);
    assert_eq!(vol.feedhorn_height_m, 19);
    assert_eq!(vol.calibration_constant_db, -45.058_58);
    assert_eq!(vol.horizontal_shv_tx_power_kw, 220.449_92);
    assert_eq!(vol.vertical_shv_tx_power_kw, 226.1625);
    assert_eq!(vol.system_differential_reflectivity_db, -0.820_148_3);
    assert_eq!(vol.initial_system_differential_phase_deg, 60.0);
    assert_eq!(vol.vcp_number, 215);
    assert_eq!(vol.processing_status, ProcessingStatus(3));
    assert!(vol.processing_status.rxr_noise() && vol.processing_status.cbt());
    assert_eq!(vol.zdr_bias_estimate_raw, Some(424));
    // (424 - 418) / 32 with the ZDR block's offset and scale (notes 20, 33).
    assert_eq!(radial.zdr_bias_estimate_db(), Some(0.1875));
    assert_eq!(vol.zdr_bias_estimate_db(None), Some(0.1875));

    // RELV 000c fff4 c22c8000
    let elv = radial.elevation.unwrap();
    assert_eq!(elv.block_size, 12);
    assert_eq!(elv.atmospheric_attenuation_raw, -12);
    assert_eq!(elv.atmospheric_attenuation_db_per_km(), -0.012);
    assert_eq!(elv.calibration_constant_db, -43.125);

    // RRAD 001c 123e c2a2d440 c2a1a343 0365 0000 c22e7774 c23079d3
    let rad = radial.radial.unwrap();
    assert_eq!(rad.block_size, 28);
    assert_eq!(rad.layout(), RadialBlockLayout::Calibration28);
    assert_eq!(rad.unambiguous_range_raw, 4670);
    assert_eq!(rad.unambiguous_range_km(), 467.0);
    assert_eq!(rad.horizontal_noise_level_dbm, -81.414_55);
    assert_eq!(rad.vertical_noise_level_dbm, -80.818_87);
    assert_eq!(rad.nyquist_velocity_raw, 869);
    assert!((rad.nyquist_velocity_mps() - 8.69).abs() < 1e-5);
    assert_eq!(rad.radial_flags, 0);
    assert_eq!(rad.horizontal_calibration_constant_dbz, Some(-43.616_653));
    assert_eq!(rad.vertical_calibration_constant_dbz, Some(-44.118_97));

    // Moment descriptors: name, gates, TOVER, SNR threshold, word size, scale,
    // offset. Every moment has its first gate at 2125 m, 250 m spacing and no
    // recombination.
    let expected = [
        (DataMomentName::Reflectivity, 1832, 50, 0, 8, 2.0, 66.0),
        (
            DataMomentName::DifferentialReflectivity,
            1192,
            50,
            16,
            16,
            32.0,
            418.0,
        ),
        (
            DataMomentName::DifferentialPhase,
            1192,
            50,
            16,
            16,
            2.836_1,
            2.0,
        ),
        (
            DataMomentName::CorrelationCoefficient,
            1192,
            50,
            16,
            8,
            300.0,
            -60.5,
        ),
        (
            DataMomentName::ClutterFilterPowerRemoved,
            1832,
            50,
            0,
            8,
            1.0,
            8.0,
        ),
    ];
    assert_eq!(radial.moments.len(), expected.len());
    for (moment, (name, gates, tover, snr, word, scale, offset)) in
        radial.moments.iter().zip(expected)
    {
        assert_eq!(moment.name, name);
        assert_eq!(moment.reserved, 0);
        assert_eq!(moment.gate_count, gates);
        assert_eq!(moment.first_gate_range_m, 2125);
        assert_eq!(moment.gate_spacing_m, 250);
        assert_eq!(moment.tover_raw, tover);
        assert_eq!(moment.tover_db(), 5.0);
        assert_eq!(moment.snr_threshold_raw, snr);
        assert_eq!(moment.snr_threshold_db(), f32::from(snr) * 0.125);
        assert_eq!(moment.control_flags, ControlFlags::None);
        assert_eq!(moment.data_word_size, word);
        assert_eq!((moment.scale, moment.offset), (scale, offset));
        assert_eq!(
            moment.data.len(),
            usize::from(gates) * usize::from(word / 8)
        );
    }
    assert_eq!(
        radial
            .moment(DataMomentName::ClutterFilterPowerRemoved)
            .unwrap()
            .field_name(),
        FieldName::Ccorh
    );
    assert!(radial.moment(DataMomentName::Velocity).is_none());
    assert!(radial.unknown_blocks.is_empty());
}

/// Gate code counts of the first radial, computed from the file bytes with a
/// separate Python reader: REF has 1250 below-threshold gates and a raw sum
/// of 49820; ZDR (16-bit) 621 and 245361; CFP 1565 gates with the clutter
/// filter not applied, 15 with the point clutter filter, 24 with dual-pol
/// only filtering, none reserved, and 228 values whose raw codes sum to 6985
/// with a maximum of 81 (73 dB, the top of the ICD range).
#[test]
fn gate_values_follow_table_xvii_i_codes() {
    let Some(records) = records(&[CHUNK_002]) else {
        return;
    };
    let radials = radials(&records);
    let radial = &radials[0];

    let reflectivity = radial.moment(DataMomentName::Reflectivity).unwrap();
    let raw: Vec<u32> = reflectivity.raw_gates().unwrap().collect();
    assert_eq!(raw.len(), 1832);
    assert_eq!(raw.iter().filter(|&&r| r == 0).count(), 1250);
    assert_eq!(raw.iter().map(|&r| u64::from(r)).sum::<u64>(), 49820);
    let values: Vec<GateValue> = reflectivity.gate_values().unwrap().collect();
    assert_eq!(
        values
            .iter()
            .filter(|v| **v == GateValue::BelowThreshold)
            .count(),
        1250
    );
    assert!(values.iter().all(|v| match v {
        GateValue::Value(dbz) => (-32.0..=94.5).contains(dbz),
        GateValue::BelowThreshold => true,
        _ => false,
    }));

    let zdr = radial
        .moment(DataMomentName::DifferentialReflectivity)
        .unwrap();
    let raw: Vec<u32> = zdr.raw_gates().unwrap().collect();
    assert_eq!(raw.len(), 1192);
    assert_eq!(raw.iter().filter(|&&r| r == 0).count(), 621);
    assert_eq!(raw.iter().map(|&r| u64::from(r)).sum::<u64>(), 245_361);
    for value in zdr.gate_values().unwrap() {
        if let GateValue::Value(db) = value {
            assert!((-13.0..=20.0).contains(&db), "ZDR {db}");
        }
    }

    let cfp = radial
        .moment(DataMomentName::ClutterFilterPowerRemoved)
        .unwrap();
    let values: Vec<GateValue> = cfp.gate_values().unwrap().collect();
    let count = |wanted: GateValue| values.iter().filter(|v| **v == wanted).count();
    assert_eq!(count(GateValue::ClutterFilterNotApplied), 1565);
    assert_eq!(count(GateValue::PointClutterFilterApplied), 15);
    assert_eq!(count(GateValue::DualPolOnlyFiltered), 24);
    assert!(!values.iter().any(|v| matches!(v, GateValue::Reserved(_))));
    let removed: Vec<f32> = values
        .iter()
        .filter_map(|v| match v {
            GateValue::Value(db) => Some(*db),
            _ => None,
        })
        .collect();
    assert_eq!(removed.len(), 228);
    assert_eq!(
        removed.iter().map(|db| f64::from(*db)).sum::<f64>(),
        6985.0 - 8.0 * 228.0
    );
    assert_eq!(removed.iter().copied().fold(f32::MIN, f32::max), 73.0);
    assert!(removed.iter().all(|db| (0.0..=73.0).contains(db)));
}

// ZDR bias estimate in builds without a golden ---------------------------------------------

/// The second LDM record of an LDM-compressed volume, decompressed: the
/// record after the 134-frame metadata record, starting with message 31.
fn second_ldm_record(raw: &[u8]) -> Vec<u8> {
    let control = |at: usize| {
        i32::from_be_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]).unsigned_abs() as usize
    };
    let second = 24 + 4 + control(24);
    let start = second + 4;
    let mut record = Vec::new();
    bzip2::read::BzDecoder::new(&raw[start..start + control(second)])
        .read_to_end(&mut record)
        .unwrap();
    record
}

/// The ZDR data moment block (type `D`, name "ZDR") of a message 31 body,
/// located through the Data Header Block pointers: `(block offset, scale,
/// offset)` read from the bytes (Table XVII-B bytes 20-23 and 24-27).
fn zdr_block_encoding_from_bytes(body: &[u8]) -> Option<(usize, f32, f32)> {
    let count = usize::from(u16::from_be_bytes([body[30], body[31]]));
    (0..count)
        .map(|slot| {
            let at = 32 + 4 * slot;
            u32::from_be_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]) as usize
        })
        .find(|&pointer| {
            body[pointer] == b'D'
                && DataMomentName::from_bytes([
                    body[pointer + 1],
                    body[pointer + 2],
                    body[pointer + 3],
                ]) == DataMomentName::DifferentialReflectivity
        })
        .map(|pointer| {
            let real = |at: usize| {
                f32::from_be_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]])
            };
            (pointer, real(pointer + 20), real(pointer + 24))
        })
}

/// VOL bytes 44-45 of the first radial of volumes without a golden. In each
/// file that radial is the first message of the second LDM record, with its
/// VOL block at body offset 72, so the estimate sits at offset
/// 12 + 16 + 72 + 44 = 144 of the decompressed record. The dB value follows
/// Table XVII-E notes 20 and 33: `(raw - offset) / scale` with the offset
/// and scale of the same radial's ZDR block, read here from the bytes.
#[test]
fn zdr_bias_estimate_in_other_builds() {
    let expected: &[(&str, u16, Option<f32>)] = &[
        ("l2-tjua-20220918-190621", 0x01ae, Some(0.375)),
        ("l2-kdgx-20230325-010651", 0x019f, Some(-0.093_75)),
        ("l2-pgua-20230524-030945", 0x01a4, Some(0.0625)),
        ("l2-kmtx-20240301-212827", 0x0197, Some(-0.343_75)),
        ("l2-ktlx-20240515-000014", 0, None),
        ("l2-pahg-20250909-212549", 0x019b, Some(-0.218_75)),
        ("l2-kilx-20260418-013553", 0x019c, Some(-0.1875)),
        ("l2-kiwa-20260917-003629", 0x01a8, Some(0.1875)),
    ];
    let mut checked = 0;
    for (id, raw_bias, db) in expected {
        let Some(raw) = load(id) else { continue };
        checked += 1;
        let record = second_ldm_record(&raw);
        assert_eq!(
            u16::from_be_bytes([record[144], record[145]]),
            *raw_bias,
            "{id}: bytes 144-145"
        );
        // Spare bytes 46-51 are zero.
        assert_eq!(&record[146..152], &[0; 6], "{id}: spare");
        let (header, body) = MessageWalker::new(&record).next().unwrap().unwrap();
        assert_eq!(header.message_type, 31, "{id}");
        let MessageBody::DigitalRadarDataGeneric(radial) = body else {
            panic!("{id}: not decoded as message 31");
        };
        let vol = radial.volume.unwrap();
        assert_eq!(vol.layout(), VolumeBlockLayout::ZdrBias52, "{id}");
        assert_eq!(vol.zdr_bias_estimate_raw, Some(*raw_bias), "{id}");

        // The first radial of every one of these volumes is a surveillance
        // radial with a ZDR block; its bytes give the encoding.
        let (pointer, scale, offset) = zdr_block_encoding_from_bytes(&record[28..])
            .unwrap_or_else(|| panic!("{id}: no ZDR block"));
        assert!(
            radial
                .header
                .block_pointers
                .contains(&u32::try_from(pointer).unwrap()),
            "{id}: ZDR pointer {pointer}"
        );
        let zdr = radial
            .moment(DataMomentName::DifferentialReflectivity)
            .unwrap();
        assert_eq!(
            (zdr.scale, zdr.offset),
            (scale, offset),
            "{id}: ZDR encoding"
        );
        assert_eq!((scale, offset), (32.0, 418.0), "{id}: Table XVII-I values");
        let from_bytes = (*raw_bias != 0).then(|| (f32::from(*raw_bias) - offset) / scale);
        assert_eq!(from_bytes, *db, "{id}");
        assert_eq!(radial.zdr_bias_estimate_db(), from_bytes, "{id}");
        assert_eq!(vol.zdr_bias_estimate_db(Some(zdr)), from_bytes, "{id}");
        assert_eq!(vol.zdr_bias_estimate_db(None), from_bytes, "{id}");
    }
    let sources: Vec<Vec<&str>> = expected.iter().map(|(id, _, _)| vec![*id]).collect();
    common::assert_checked_every_available("ZDR bias estimates", checked, &sources);
}

/// Per sweep, on a real volume with split cuts: surveillance radials carry
/// a ZDR block, Doppler radials do not. The estimate of a Doppler sweep's
/// first radial uses the Table XVII-I values, and equals the value the same
/// raw code gives with the surveillance radials' ZDR blocks.
#[test]
fn zdr_bias_estimate_per_sweep_with_and_without_a_zdr_block() {
    let id = "l2-kilx-20260418-013553";
    let Some(raw) = load(id) else { return };
    let decoded = recast_radar_io_nexrad::read_volume_with_metadata(&raw).unwrap();
    let sweeps = decoded.metadata.per_sweep_elevation_data.unwrap();
    let volume_sweeps = &decoded.volume.sweeps;
    assert!(sweeps.len() >= 4, "{id}: {} sweeps", sweeps.len());
    let mut with_zdr = 0;
    let mut without_zdr = 0;
    for sweep in &sweeps {
        let has_zdr = volume_sweeps[sweep.sweep_index]
            .field(&recast_radar_core::model::FieldName::Zdr)
            .is_some();
        let vol = sweep.volume.unwrap();
        let raw = vol.zdr_bias_estimate_raw.unwrap();
        assert_eq!(
            raw, 0x019c,
            "{id} sweep {}: same raw estimate",
            sweep.sweep_index
        );
        assert_eq!(
            sweep.zdr_bias_estimate_db,
            Some((f32::from(raw) - 418.0) / 32.0),
            "{id} sweep {}",
            sweep.sweep_index
        );
        assert_eq!(
            sweep.zdr_bias_estimate_db,
            vol.zdr_bias_estimate_db(None),
            "{id} sweep {}",
            sweep.sweep_index
        );
        if has_zdr {
            with_zdr += 1;
        } else {
            without_zdr += 1;
        }
    }
    assert!(
        with_zdr >= 2 && without_zdr >= 2,
        "{id}: {with_zdr} with ZDR, {without_zdr} without"
    );
}

/// The note 20 path on the committed KIWA radial: changing the ZDR block's
/// scale and offset bytes changes the estimate; a scale of 0 (floating-point
/// gates) and a radial without a ZDR block fall back to the Table XVII-I
/// values.
#[test]
fn zdr_bias_estimate_follows_the_radial_zdr_block_encoding() {
    let Some(body) = chunk_radial_body() else {
        return;
    };
    let (pointer, scale, offset) = zdr_block_encoding_from_bytes(&body).unwrap();
    assert_eq!((scale, offset), (32.0, 418.0));
    let original = DigitalRadarDataGeneric::decode(&body).unwrap();
    assert_eq!(original.volume.unwrap().zdr_bias_estimate_raw, Some(424));
    assert_eq!(original.zdr_bias_estimate_db(), Some(0.1875));

    // Scale 16, offset 256: (424 - 256) / 16.
    let mut rescaled = body.clone();
    rescaled[pointer + 20..pointer + 24].copy_from_slice(&16.0f32.to_be_bytes());
    rescaled[pointer + 24..pointer + 28].copy_from_slice(&256.0f32.to_be_bytes());
    let radial = DigitalRadarDataGeneric::decode(&rescaled).unwrap();
    let zdr = radial
        .moment(DataMomentName::DifferentialReflectivity)
        .unwrap();
    assert_eq!((zdr.scale, zdr.offset), (16.0, 256.0));
    assert_eq!(radial.zdr_bias_estimate_db(), Some(10.5));
    let vol = radial.volume.unwrap();
    assert_eq!(vol.zdr_bias_estimate_db(Some(zdr)), Some(10.5));
    assert_eq!(vol.zdr_bias_estimate_db(None), Some(0.1875));

    // Scale 0 means floating-point gates (note 15): the typical values.
    let mut floating = body.clone();
    floating[pointer + 20..pointer + 24].copy_from_slice(&0.0f32.to_be_bytes());
    let radial = DigitalRadarDataGeneric::decode(&floating).unwrap();
    assert_eq!(radial.zdr_bias_estimate_db(), Some(0.1875));

    // The ZDR block renamed: no ZDR block in the radial, the typical values.
    let mut renamed = body.clone();
    renamed[pointer + 1..pointer + 4].copy_from_slice(b"XYZ");
    let radial = DigitalRadarDataGeneric::decode(&renamed).unwrap();
    assert!(
        radial
            .moment(DataMomentName::DifferentialReflectivity)
            .is_none()
    );
    assert_eq!(radial.zdr_bias_estimate_db(), Some(0.1875));

    // Not available (raw 0) is None whatever the encoding.
    let mut unavailable = rescaled.clone();
    unavailable[72 + 44..72 + 46].copy_from_slice(&[0, 0]);
    let radial = DigitalRadarDataGeneric::decode(&unavailable).unwrap();
    assert_eq!(radial.volume.unwrap().zdr_bias_estimate_raw, Some(0));
    assert_eq!(radial.zdr_bias_estimate_db(), None);
}

// The volume decoder agrees ----------------------------------------------------------------

/// `read_volume_from_bytes` reads the same header, VOL and RAD fields on its
/// fast path; both decoders agree radial by radial on KPAH 2008.
#[test]
fn volume_decoder_agrees_with_typed_radials() {
    let id = "l2-kpah-20080415-235014";
    let Some(raw) = load(id) else { return };
    let volume = recast_radar_io_nexrad::read_volume_from_bytes(&raw).unwrap();
    let records = messages::record_bytes(&raw).unwrap();
    let radials = radials(&records);

    let vol = radials[0].volume.unwrap();
    assert_eq!(
        volume.location.latitude_deg,
        Some(f64::from(vol.latitude_deg))
    );
    assert_eq!(
        volume.location.longitude_deg,
        Some(f64::from(vol.longitude_deg))
    );
    assert_eq!(
        volume.location.altitude_m,
        Some(f64::from(
            f32::from(vol.site_height_m) + f32::from(vol.feedhorn_height_m)
        ))
    );
    assert_eq!(volume.scan.vcp_pattern, Some(vol.vcp_number));

    let nrays: usize = volume.sweeps.iter().map(|sweep| sweep.nrays()).sum();
    assert_eq!(nrays, radials.len());
    let mut by_sweep = volume
        .sweeps
        .iter()
        .flat_map(|sweep| (0..sweep.nrays()).map(move |ray| (sweep, ray)));
    for typed in &radials {
        let (sweep, ray) = by_sweep.next().unwrap();
        assert_eq!(
            sweep.elevation_number,
            Some(u16::from(typed.header.elevation_number))
        );
        assert_eq!(sweep.rays.azimuth_deg[ray], typed.header.azimuth_angle_deg);
        assert_eq!(
            sweep.rays.elevation_deg[ray],
            typed.header.elevation_angle_deg
        );
        assert_eq!(
            volume.ray_time(sweep.sweep_number as usize, ray).unwrap(),
            typed.header.collection_time()
        );
        let rad = typed.radial.unwrap();
        match sweep
            .ray_vars
            .nyquist_velocity_mps
            .as_ref()
            .map(|values| values[ray])
            .filter(|value| !value.is_nan())
        {
            Some(nyquist) => assert!((nyquist - rad.nyquist_velocity_mps()).abs() < 1e-4),
            None => assert_eq!(rad.nyquist_velocity_raw, 0),
        }
        // Every typed moment is a field of the sweep in the block's native
        // geometry.
        for moment in &typed.moments {
            let field = sweep.field(&moment.field_name()).unwrap();
            assert_eq!(field.ngates, u32::from(moment.gate_count));
            assert_eq!(
                field.native_geometry(&sweep.range),
                Some((
                    f64::from(moment.first_gate_range_m),
                    f64::from(moment.gate_spacing_m)
                ))
            );
        }
    }
}

// Mutations of a real radial --------------------------------------------------------------

/// The first message 31 body of the committed chunk 002, and its decode.
fn chunk_radial_body() -> Option<Vec<u8>> {
    let records = records(&[CHUNK_002])?;
    let size = usize::from(u16::from_be_bytes([records[12], records[13]])) * 2;
    assert_eq!(records[15], 31);
    Some(records[28..12 + size].to_vec())
}

#[test]
fn unknown_constant_block_is_preserved_by_name() {
    let Some(mut body) = chunk_radial_body() else {
        return;
    };
    let original = DigitalRadarDataGeneric::decode(&body).unwrap().into_owned();
    // RAD block at 136: rename to "RXYZ".
    body[137..140].copy_from_slice(b"XYZ");
    let radial = DigitalRadarDataGeneric::decode(&body).unwrap();
    assert!(radial.radial.is_none());
    assert_eq!(radial.unknown_blocks.len(), 1);
    let unknown = &radial.unknown_blocks[0];
    assert_eq!(unknown.label(), "RXYZ");
    assert_eq!(unknown.pointer, 136);
    // Sized by its LRTUP (28 bytes).
    assert_eq!(&unknown.bytes[..], &body[136..164]);
    assert_eq!(radial.volume, original.volume);
    assert_eq!(radial.moments, original.moments);
}

#[test]
fn unknown_block_type_extends_to_the_next_block() {
    let Some(mut body) = chunk_radial_body() else {
        return;
    };
    // ZDR moment at 2024: type "Q"; the next block (PHI) starts at 4436.
    body[2024] = b'Q';
    // CFP moment at 8068, the last block: type "Q"; it runs to the end.
    body[8068] = b'Q';
    let radial = DigitalRadarDataGeneric::decode(&body).unwrap();
    let labels: Vec<String> = radial.unknown_blocks.iter().map(|b| b.label()).collect();
    assert_eq!(labels, ["QZDR", "QCFP"]);
    assert_eq!(radial.unknown_blocks[0].bytes.len(), 4436 - 2024);
    assert_eq!(radial.unknown_blocks[1].bytes.len(), 9928 - 8068);
    let names: Vec<DataMomentName> = radial.moments.iter().map(|m| m.name).collect();
    assert_eq!(
        names,
        [
            DataMomentName::Reflectivity,
            DataMomentName::DifferentialPhase,
            DataMomentName::CorrelationCoefficient
        ]
    );
    let owned = radial.into_owned();
    assert_eq!(owned.unknown_blocks[1].bytes.len(), 1860);
}

#[test]
fn undefined_moment_name_is_kept_as_a_moment() {
    let Some(mut body) = chunk_radial_body() else {
        return;
    };
    // RHO moment at 6848 renamed "DXYZ".
    body[6849..6852].copy_from_slice(b"XYZ");
    let radial = DigitalRadarDataGeneric::decode(&body).unwrap();
    let moment = radial
        .moment(DataMomentName::Other(*b"XYZ"))
        .expect("renamed moment");
    assert_eq!(moment.name.short_name(), "XYZ");
    assert_eq!(moment.name.units(), "");
    assert_eq!(moment.field_name(), FieldName::Other("XYZ".into()));
    assert_eq!(moment.gate_count, 1192);
    assert!(radial.unknown_blocks.is_empty());
}

#[test]
fn block_sizes_select_layouts() {
    let Some(body) = chunk_radial_body() else {
        return;
    };
    // VOL at 72 declaring 44 bytes: the pre-Build 20 layout, no estimate.
    let mut older = body.clone();
    older[76..78].copy_from_slice(&44u16.to_be_bytes());
    // RAD at 136 declaring 20 bytes: no calibration constants.
    older[140..142].copy_from_slice(&20u16.to_be_bytes());
    let radial = DigitalRadarDataGeneric::decode(&older).unwrap();
    let vol = radial.volume.unwrap();
    assert_eq!(vol.layout(), VolumeBlockLayout::Original44);
    assert_eq!(vol.zdr_bias_estimate_raw, None);
    assert_eq!(vol.vcp_number, 215);
    let rad = radial.radial.unwrap();
    assert_eq!(rad.layout(), RadialBlockLayout::Original20);
    assert_eq!(rad.horizontal_calibration_constant_dbz, None);
    assert_eq!(rad.nyquist_velocity_raw, 869);

    // A larger VOL (as note 32 allows future builds) keeps the newest layout.
    let mut newer = body.clone();
    newer[76..78].copy_from_slice(&60u16.to_be_bytes());
    let vol = DigitalRadarDataGeneric::decode(&newer)
        .unwrap()
        .volume
        .unwrap();
    assert_eq!(vol.zdr_bias_estimate_raw, Some(424));

    // Smaller than the oldest layout: rejected.
    for (offset, size, name) in [(76, 40u16, "VOL"), (140, 16, "RAD"), (128, 8, "ELV")] {
        let mut short = body.clone();
        short[offset..offset + 2].copy_from_slice(&size.to_be_bytes());
        let error = DigitalRadarDataGeneric::decode(&short).unwrap_err();
        assert!(error.to_string().contains(name), "{error}");
    }
}

#[test]
fn compressed_radials_inflate_from_the_first_block() {
    let Some(body) = chunk_radial_body() else {
        return;
    };
    let original = DigitalRadarDataGeneric::decode(&body).unwrap().into_owned();
    // Re-encode the real blocks (from the first pointer, 72) with each
    // method the compression indicator defines.
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    zlib.write_all(&body[72..]).unwrap();
    let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
    bz.write_all(&body[72..]).unwrap();
    for (code, compressed) in [(2u8, zlib.finish().unwrap()), (1, bz.finish().unwrap())] {
        let mut mutated = body[..72].to_vec();
        mutated[16] = code;
        mutated.extend_from_slice(&compressed);
        let radial = DigitalRadarDataGeneric::decode(&mutated).unwrap();
        assert_eq!(radial.header.compression.code(), code);
        assert_eq!(radial.volume, original.volume);
        assert_eq!(radial.elevation, original.elevation);
        assert_eq!(radial.radial, original.radial);
        assert_eq!(radial.moments, original.moments);

        // A radial length too short for the inflated blocks is rejected.
        let mut short = mutated.clone();
        short[18..20].copy_from_slice(&1000u16.to_be_bytes());
        assert!(matches!(
            DigitalRadarDataGeneric::decode(&short),
            Err(NexradError::Compression(_))
        ));
    }
    let mut unknown = body.clone();
    unknown[16] = 3;
    assert!(DigitalRadarDataGeneric::decode(&unknown).is_err());
}

#[test]
fn pointer_and_length_errors_are_rejected() {
    let Some(body) = chunk_radial_body() else {
        return;
    };
    let set_pointer = |slot: usize, value: u32| {
        let mut mutated = body.clone();
        mutated[32 + slot * 4..36 + slot * 4].copy_from_slice(&value.to_be_bytes());
        mutated
    };
    // Past the end, and inside the pointer table.
    assert!(DigitalRadarDataGeneric::decode(&set_pointer(3, 9928)).is_err());
    assert!(DigitalRadarDataGeneric::decode(&set_pointer(3, 40)).is_err());
    // A second VOL block.
    let error = DigitalRadarDataGeneric::decode(&set_pointer(1, 72)).unwrap_err();
    assert!(error.to_string().contains("second VOL"), "{error}");
    // Gates running past the end of the radial.
    let truncated = &body[..9000];
    assert!(matches!(
        DigitalRadarDataGeneric::decode(truncated),
        Err(NexradError::Truncated { .. })
    ));
    // A block count whose pointer table runs past the body.
    let mut count = body.clone();
    count[30..32].copy_from_slice(&3000u16.to_be_bytes());
    assert!(matches!(
        DigitalRadarDataGeneric::decode(&count),
        Err(NexradError::Truncated { .. })
    ));
    // A word size that is not a multiple of 8 (CFP at 8068, byte 19).
    let mut word = body.clone();
    word[8068 + 19] = 12;
    assert!(DigitalRadarDataGeneric::decode(&word).is_err());
    // A zero pointer references nothing.
    let without_cfp = set_pointer(7, 0);
    let radial = DigitalRadarDataGeneric::decode(&without_cfp).unwrap();
    assert!(
        radial
            .moment(DataMomentName::ClutterFilterPowerRemoved)
            .is_none()
    );
}

/// A rejected message 31 is one walker error; the walk continues.
#[test]
fn walker_reports_a_bad_radial_and_continues() {
    let Some(mut records) = records(&[CHUNK_002]) else {
        return;
    };
    // First radial's REF pointer (body offset 44) past its end.
    records[28 + 44..28 + 48].copy_from_slice(&60_000u32.to_be_bytes());
    let items: Vec<_> = MessageWalker::new(&records).collect();
    assert_eq!(items.len(), 120);
    let error = items[0].as_ref().unwrap_err().to_string();
    assert!(error.contains("message type 31"), "{error}");
    assert!(
        items[1..]
            .iter()
            .all(|item| matches!(item, Ok((_, MessageBody::DigitalRadarDataGeneric(_)))))
    );
}
