//! `read_volume_with_metadata` on real files.
//!
//! Golden values:
//!
//! - `testdata/level2/golden/metadata/*.json`, written by
//!   `python tools/level2_golden.py metadata` with Py-ART 2.2.5's own Level II
//!   reader (`NEXRADLevel2File`, used by `read_nexrad_archive`): the VCP
//!   number from message 5, and per elevation number the ray count, the
//!   message 5 target angle, and the first ray's Data Header Block, VOL, ELV
//!   and RAD blocks (raw values). 26 archive volumes from 1991 to 2026 and the
//!   committed KIWA real-time chunks.
//! - The MetPy 1.7.1 goldens of the message decoder tests (`status`, `vcp`,
//!   `clutter`, `msg31`), checked here against the fields of
//!   [`NexradMetadata`]: message 2 layout, build and VCP; message 3 and 18
//!   presence; message 5 pattern and cut count; message 32 presence; clutter
//!   map generation times and segment counts; and per sweep, the first
//!   radial's constant blocks.
//!
//! The field-by-field decoding of each message is verified by the
//! `messages_*` tests; these tests verify that `read_volume_with_metadata`
//! picks the right messages and radials, aligns the per-sweep data with the
//! volume's sweeps, returns the same volume as `read_volume_from_bytes`, and
//! keeps decoding when a metadata message is broken.

mod common;

use std::io::{Read, Write};
use std::path::PathBuf;

use recast_radar_core::model::RadarParameters;
use recast_radar_io_nexrad::messages::rda_status::RdaStatus;
use recast_radar_io_nexrad::messages::{self, MessageBody, MessageWalker};
use recast_radar_io_nexrad::{
    NexradMetadata, NexradVolume, SweepElevationData, read_volume_from_bytes,
    read_volume_with_metadata,
};
use serde_json::Value;

use common::{assert_checked_every_available, load, load_all};

const CHUNKS: [&str; 3] = [
    "l2chunk-kiwa-307-20260917-003629-001-s",
    "l2chunk-kiwa-307-20260917-003629-002-i",
    "l2chunk-kiwa-307-20260917-003629-003-i",
];

fn golden_dir(group: &str) -> PathBuf {
    recast_radar_testdata::testdata_dir().join(format!("level2/golden/{group}"))
}

/// Every golden file of a group, sorted by name.
fn goldens(group: &str) -> Vec<(String, Value)> {
    let dir = golden_dir(group);
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
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            (name, serde_json::from_str(&text).unwrap())
        })
        .collect()
}

/// The golden of `group` for one id, if the group has one.
fn golden(group: &str, id: &str) -> Option<Value> {
    let path = golden_dir(group).join(format!("{id}.json"));
    path.exists()
        .then(|| serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap())
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

fn int(value: &Value, what: &str) -> i64 {
    value
        .as_i64()
        .unwrap_or_else(|| panic!("golden {what} is not an integer: {value}"))
}

/// A golden REAL*4 as the `f32` it was unpacked from. Rounding the parsed
/// double to `f32` absorbs any last-bit difference in decimal parsing.
fn real(value: &Value, what: &str) -> f32 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("golden {what} is not a number: {value}")) as f32
}

/// A golden value MetPy computed by scaling a raw integer.
fn assert_scaled(actual: f64, golden: &Value, what: &str) {
    let expected = golden
        .as_f64()
        .unwrap_or_else(|| panic!("golden {what} is not a number: {golden}"));
    assert!(
        (actual - expected).abs() <= 1e-9 * expected.abs().max(1.0),
        "{what}: {actual} != golden {expected}"
    );
}

/// Degrees of a message 5 angle code: Table III-A binary angle, negative
/// above 90 degrees (the rule the VCP decoder applies).
fn vcp_angle_deg(code: u64) -> f64 {
    let degrees = code as f64 * 360.0 / 65536.0;
    if degrees > 90.0 {
        degrees - 360.0
    } else {
        degrees
    }
}

/// Structural equality through `Debug` text, so NaN fields (unset REAL*4
/// values in messages 3 and 18) compare equal to themselves. On a mismatch,
/// prints the text around the first difference instead of both values.
fn assert_same<T: std::fmt::Debug>(actual: &T, expected: &T, what: &str) {
    let actual = format!("{actual:?}");
    let expected = format!("{expected:?}");
    if actual != expected {
        let at = actual
            .bytes()
            .zip(expected.bytes())
            .position(|(a, b)| a != b)
            .unwrap_or(actual.len().min(expected.len()));
        let window = |text: &str| {
            let start = text.floor_char_boundary(at.saturating_sub(120));
            let end = text.ceil_char_boundary((at + 120).min(text.len()));
            text[start..end].to_owned()
        };
        panic!(
            "{what}: differs at byte {at}
  actual: ...{}...
expected: ...{}...",
            window(&actual),
            window(&expected)
        );
    }
}

/// `metadata` matches, field by field, the first decoded message of each
/// type in the file's metadata record, and
/// [`NexradMetadata::from_metadata_record`].
fn assert_matches_metadata_record(name: &str, bytes: &[u8], metadata: &NexradMetadata) {
    let alone = NexradMetadata::from_metadata_record(bytes);
    assert!(
        metadata.volume_header_time.is_some(),
        "{name}: the volume decode records the header time"
    );
    let without_sweeps = NexradMetadata {
        volume_header_time: None,
        per_sweep_elevation_data: None,
        errors: alone.errors.clone(),
        ..metadata.clone()
    };
    assert_same(
        &without_sweeps,
        &alone,
        &format!("{name}: metadata record fields"),
    );
    assert!(
        alone
            .errors
            .iter()
            .all(|error| metadata.errors.contains(error)),
        "{name}: metadata record errors are kept"
    );

    let record = messages::metadata_record(bytes).unwrap();
    let mut first = NexradMetadata::default();
    for (_, body) in MessageWalker::new(&record).flatten() {
        match body {
            MessageBody::RdaStatus(value) if first.rda_status.is_none() => {
                first.rda_status = Some(value);
            }
            MessageBody::Performance(value) if first.performance.is_none() => {
                first.performance = Some(value);
            }
            MessageBody::Vcp(value) if first.vcp.is_none() => first.vcp = Some(value),
            MessageBody::Adaptation(value) if first.adaptation.is_none() => {
                first.adaptation = Some(value);
            }
            MessageBody::ClutterFilterMap(value) if first.clutter_filter_map.is_none() => {
                first.clutter_filter_map = Some(value);
            }
            MessageBody::BypassMap(value) if first.bypass_map.is_none() => {
                first.bypass_map = Some(value);
            }
            MessageBody::ClutterCensorZones(value) if first.clutter_censor_zones.is_none() => {
                first.clutter_censor_zones = Some(value);
            }
            MessageBody::Prf(value) if first.prf.is_none() => first.prf = Some(value),
            _ => {}
        }
    }
    first.build = first.rda_status.as_ref().and_then(RdaStatus::rda_build);
    first.errors = alone.errors.clone();
    assert_same(
        &first,
        &alone,
        &format!("{name}: first message of each type"),
    );
}

/// Per-sweep entries exist for every sweep, in sweep order.
fn sweeps<'a>(name: &str, decoded: &'a NexradVolume) -> &'a [SweepElevationData] {
    let sweeps = decoded
        .metadata
        .per_sweep_elevation_data
        .as_deref()
        .unwrap_or_else(|| panic!("{name}: message 31 volume without per-sweep data"));
    assert_eq!(
        sweeps.len(),
        decoded.volume.sweeps.len(),
        "{name}: one per sweep"
    );
    for (index, (data, sweep)) in sweeps.iter().zip(&decoded.volume.sweeps).enumerate() {
        assert_eq!(data.sweep_index, index, "{name}");
        assert_eq!(
            Some(u16::from(data.elevation_number)),
            sweep.elevation_number,
            "{name}"
        );
    }
    sweeps
}

/// Milliseconds of day of a ray's collection time (the Message 31 header
/// value).
fn collect_ms(volume: &recast_radar_core::Volume, sweep: usize, ray: usize) -> i64 {
    let time = volume.ray_time(sweep, ray).unwrap();
    let midnight = time.date_naive().and_time(chrono::NaiveTime::MIN).and_utc();
    (time - midnight).num_milliseconds()
}

fn check_pyart_scan(name: &str, decoded: &NexradVolume, scan: &Value) {
    let volume = &decoded.volume;
    let metadata = &decoded.metadata;
    let number = uint(&scan["elevation_number"], "elevation_number");
    let what = format!("{name} elevation {number}");
    let cuts: Vec<usize> = (0..volume.sweeps.len())
        .filter(|&index| volume.sweeps[index].elevation_number.map(u64::from) == Some(number))
        .collect();
    let rays: usize = cuts.iter().map(|&index| volume.sweeps[index].nrays()).sum();
    assert_eq!(rays as u64, uint(&scan["nrays"], "nrays"), "{what}: rays");
    if let Some(code) = scan["target_angle_code"].as_u64() {
        let vcp = metadata.vcp.as_ref().unwrap();
        let cut = &vcp.cuts[number as usize - 1];
        assert!(
            (f64::from(cut.elevation_angle_deg) - vcp_angle_deg(code)).abs() < 1e-4,
            "{what}: message 5 elevation {} vs Py-ART code {code}",
            cut.elevation_angle_deg
        );
    }
    let Some(&cut_index) = cuts.first() else {
        return;
    };
    let sweep = &metadata.per_sweep_elevation_data.as_ref().unwrap()[cut_index];

    let header = &scan["header"];
    assert_eq!(
        collect_ms(volume, cut_index, 0),
        int(&header["collect_ms"], "collect_ms"),
        "{what}: first ray"
    );
    assert_eq!(
        volume.sweeps[cut_index].rays.azimuth_deg[0],
        real(&header["azimuth_angle"], "azimuth_angle"),
        "{what}"
    );
    assert_eq!(
        sweep.elevation_angle_deg,
        real(&header["elevation_angle"], "elevation_angle"),
        "{what}"
    );
    assert_eq!(
        u64::from(sweep.elevation_number),
        uint(&header["elevation_number"], "header elevation_number"),
        "{what}"
    );

    let vol = &scan["VOL"];
    match &sweep.volume {
        None => assert!(vol.is_null(), "{what}: VOL"),
        Some(block) => {
            assert_eq!(
                u64::from(block.block_size),
                uint(&vol["lrtup"], "lrtup"),
                "{what}"
            );
            assert_eq!(
                u64::from(block.version_major),
                uint(&vol["version_major"], "major")
            );
            assert_eq!(
                u64::from(block.version_minor),
                uint(&vol["version_minor"], "minor")
            );
            assert_eq!(block.latitude_deg, real(&vol["lat"], "lat"), "{what}");
            assert_eq!(block.longitude_deg, real(&vol["lon"], "lon"), "{what}");
            assert_eq!(
                i64::from(block.site_height_m),
                int(&vol["height"], "height")
            );
            assert_eq!(
                u64::from(block.feedhorn_height_m),
                uint(&vol["feedhorn_height"], "feedhorn_height")
            );
            assert_eq!(
                block.calibration_constant_db,
                real(&vol["refl_calib"], "refl_calib"),
                "{what}"
            );
            assert_eq!(
                block.horizontal_shv_tx_power_kw,
                real(&vol["power_h"], "power_h")
            );
            assert_eq!(
                block.vertical_shv_tx_power_kw,
                real(&vol["power_v"], "power_v")
            );
            assert_eq!(
                block.system_differential_reflectivity_db,
                real(&vol["diff_refl_calib"], "diff_refl_calib")
            );
            assert_eq!(
                block.initial_system_differential_phase_deg,
                real(&vol["init_phase"], "init_phase")
            );
            assert_eq!(u64::from(block.vcp_number), uint(&vol["vcp"], "vcp"));
            assert_eq!(
                u64::from(block.processing_status.0),
                uint(&vol["spare"], "processing status")
            );
        }
    }

    let elv = &scan["ELV"];
    match &sweep.elevation {
        None => assert!(elv.is_null(), "{what}: ELV"),
        Some(block) => {
            assert_eq!(
                u64::from(block.block_size),
                uint(&elv["lrtup"], "lrtup"),
                "{what}"
            );
            assert_eq!(
                i64::from(block.atmospheric_attenuation_raw),
                int(&elv["atmos"], "atmos"),
                "{what}"
            );
            assert_eq!(
                block.calibration_constant_db,
                real(&elv["refl_calib"], "refl_calib"),
                "{what}"
            );
        }
    }

    let rad = &scan["RAD"];
    match &sweep.radial {
        None => assert!(rad.is_null(), "{what}: RAD"),
        Some(block) => {
            assert_eq!(
                u64::from(block.block_size),
                uint(&rad["lrtup"], "lrtup"),
                "{what}"
            );
            assert_eq!(
                i64::from(block.unambiguous_range_raw),
                int(&rad["unambig_range"], "unambig_range"),
                "{what}"
            );
            assert_eq!(
                block.horizontal_noise_level_dbm,
                real(&rad["noise_h"], "noise_h")
            );
            assert_eq!(
                block.vertical_noise_level_dbm,
                real(&rad["noise_v"], "noise_v")
            );
            assert_eq!(
                i64::from(block.nyquist_velocity_raw),
                int(&rad["nyquist_vel"], "nyquist_vel"),
                "{what}"
            );
            assert_eq!(
                u64::from(block.radial_flags),
                uint(&rad["spare"], "radial flags")
            );
            // The first ray's Nyquist velocity in the volume comes from the
            // same RAD block.
            if block.nyquist_velocity_raw > 0 {
                assert_eq!(
                    volume.sweeps[cut_index]
                        .ray_vars
                        .nyquist_velocity_mps
                        .as_ref()
                        .map(|values| values[0]),
                    Some(f32::from(block.nyquist_velocity_raw as i16) / 100.0),
                    "{what}"
                );
            }
        }
    }
}

/// Every volume with a Py-ART golden: the volume equals
/// `read_volume_from_bytes`, the metadata record fields equal the walker's
/// first messages, and the VCP and per-elevation constant blocks equal
/// Py-ART's.
#[test]
fn matches_pyart_and_the_volume_decoder() {
    let mut checked = 0;
    let mut message_31_volumes = 0;
    let mut sources = Vec::new();
    let mut message_31_goldens = 0;
    let mut site_constants = 0;
    for (name, golden) in goldens("metadata") {
        let ids: Vec<String> = source_ids(&golden)
            .iter()
            .map(|id| (*id).to_owned())
            .collect();
        sources.push(ids);
        let Some(bytes) = load_all(&source_ids(&golden)) else {
            continue;
        };
        message_31_goldens += usize::from(uint(&golden["msg_type"], "msg_type") == 31);
        let decoded =
            read_volume_with_metadata(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            decoded.volume,
            read_volume_from_bytes(&bytes).unwrap(),
            "{name}: same volume as read_volume_from_bytes"
        );
        assert_matches_metadata_record(&name, &bytes, &decoded.metadata);
        site_constants += usize::from(assert_site_constants(&name, &decoded));

        match golden["vcp_pattern"].as_u64() {
            // Py-ART reads KLIX 2005's zero-filled message 5 as pattern 0;
            // the VCP decoder rejects it.
            Some(pattern) if pattern != 0 => assert_eq!(
                decoded
                    .metadata
                    .vcp
                    .as_ref()
                    .map(|vcp| u64::from(vcp.pattern_number)),
                Some(pattern),
                "{name}: VCP"
            ),
            _ => assert!(decoded.metadata.vcp.is_none(), "{name}: no VCP"),
        }

        match uint(&golden["msg_type"], "msg_type") {
            1 => {
                assert_eq!(
                    decoded.metadata.per_sweep_elevation_data, None,
                    "{name}: message 1 volume"
                );
                for scan in golden["scans"].as_array().unwrap() {
                    let number = uint(&scan["elevation_number"], "elevation_number");
                    let rays: usize = decoded
                        .volume
                        .sweeps
                        .iter()
                        .filter(|sweep| sweep.elevation_number.map(u64::from) == Some(number))
                        .map(|sweep| sweep.nrays())
                        .sum();
                    assert_eq!(
                        rays as u64,
                        uint(&scan["nrays"], "nrays"),
                        "{name} elevation {number}: rays"
                    );
                }
            }
            31 => {
                message_31_volumes += 1;
                sweeps(&name, &decoded);
                let scans = golden["scans"].as_array().unwrap();
                let numbered = decoded
                    .volume
                    .sweeps
                    .iter()
                    .filter_map(|sweep| sweep.elevation_number)
                    .max()
                    .unwrap_or(0);
                assert_eq!(scans.len(), usize::from(numbered), "{name}: elevations");
                for scan in scans {
                    check_pyart_scan(&name, &decoded, scan);
                }
            }
            other => panic!("{name}: message type {other}"),
        }
        checked += 1;
    }
    // 27 goldens: 23 message 31 sources (the committed chunks among them,
    // which keep the test meaningful offline) and 4 message 1 files.
    assert_eq!(sources.len(), 27, "metadata goldens");
    assert_checked_every_available("metadata goldens", checked, &sources);
    assert_eq!(message_31_volumes, message_31_goldens);
    // Every Open RDA volume carries message 18 (the committed KIWA chunks
    // among them).
    assert!(
        site_constants >= 1,
        "no volume with message 18 site constants"
    );
    eprintln!(
        "checked {checked} volumes ({message_31_volumes} message 31, {site_constants} with message 18 site constants)"
    );
}

/// `Volume::radar_parameters` of the volume decoder, which reads message 18's
/// first segment, against the walker's decode of the whole message: the
/// transmitter frequency, the antenna gain and the beam width within their
/// plausibility ranges (1000 to 40000 MHz, 20 to 60 dB, 0.1 to 10 degrees:
/// wider than the WSR-88D's ICD ranges so Level II written from C- and X-band
/// radars keeps them), for both polarizations. `true` when the volume has any
/// of them.
fn assert_site_constants(name: &str, decoded: &NexradVolume) -> bool {
    let parameters = &decoded.volume.radar_parameters;
    let Some(adaptation) = decoded.metadata.adaptation.as_deref() else {
        assert_eq!(
            *parameters,
            RadarParameters::default(),
            "{name}: no message 18, no radar parameters"
        );
        return false;
    };
    let frequency = (1000..=40_000)
        .contains(&adaptation.tfreq_mhz)
        .then(|| f64::from(adaptation.tfreq_mhz) * 1e6);
    let gain = (20.0..=60.0)
        .contains(&adaptation.antenna_gain)
        .then_some(adaptation.antenna_gain);
    let beam = (0.1..=10.0)
        .contains(&adaptation.beamwidth)
        .then_some(adaptation.beamwidth);
    let expected = RadarParameters {
        frequency_hz: frequency.into_iter().collect(),
        antenna_gain_h_db: gain,
        antenna_gain_v_db: gain,
        beam_width_h_deg: beam,
        beam_width_v_deg: beam,
        ..RadarParameters::default()
    };
    assert_eq!(*parameters, expected, "{name}: radar parameters");
    *parameters != RadarParameters::default()
}

/// Message 2, 3, 5, 13, 15, 18 and 32 fields against the MetPy goldens of
/// the message decoder tests, for every file with a status golden.
#[test]
fn matches_metpy_metadata_messages() {
    let mut checked = 0;
    let mut sources = Vec::new();
    for (id, status) in goldens("status") {
        sources.push(vec![id.clone()]);
        let Some(bytes) = load(&id) else {
            continue;
        };
        let metadata = NexradMetadata::from_metadata_record(&bytes);

        match &status["message_2"] {
            Value::Null => {
                assert!(metadata.rda_status.is_none(), "{id}: message 2");
                assert!(metadata.build.is_none(), "{id}: build");
            }
            message => {
                let rda_status = metadata.rda_status.as_ref().unwrap();
                let orda = uint(&message["channels"], "channels") & 8 != 0;
                assert_eq!(
                    matches!(rda_status, RdaStatus::Orda(_)),
                    orda,
                    "{id}: layout"
                );
                let build = uint(&message["codes"]["rda_build"], "rda_build");
                assert_eq!(
                    metadata.build.map(|build| u64::from(build.raw())),
                    orda.then_some(build),
                    "{id}: build"
                );
                assert_eq!(
                    i64::from(rda_status.volume_coverage_pattern().signed()),
                    int(&message["fields"]["vcp_num"], "vcp_num"),
                    "{id}: message 2 VCP"
                );
            }
        }
        assert_eq!(
            metadata.performance.is_some(),
            !status["message_3"].is_null(),
            "{id}: message 3"
        );
        assert_eq!(
            metadata.adaptation.is_some(),
            !status["message_18"].is_null(),
            "{id}: message 18"
        );

        match golden("vcp", &id) {
            Some(vcp) => {
                match &vcp["message_5"] {
                    Value::Null => assert!(metadata.vcp.is_none(), "{id}: message 5"),
                    message => {
                        let decoded = metadata.vcp.as_ref().unwrap();
                        assert_eq!(
                            u64::from(decoded.pattern_number),
                            uint(&message["num"], "num"),
                            "{id}"
                        );
                        assert_eq!(
                            decoded.cuts.len() as u64,
                            uint(&message["num_el_cuts"], "num_el_cuts"),
                            "{id}"
                        );
                    }
                }
                if vcp.get("message_31_sweeps").is_some() {
                    assert!(metadata.prf.is_some(), "{id}: message 32");
                }
            }
            None => assert!(metadata.vcp.is_none(), "{id}: no message 5 golden"),
        }
        if metadata.build.is_some_and(|build| build.major() < 23) {
            assert!(metadata.prf.is_none(), "{id}: message 32 before Build 23");
        }
        assert!(metadata.clutter_censor_zones.is_none(), "{id}: message 8");

        // The clutter goldens cover archive files only; the start chunk's
        // maps are compared with KIWA's archive golden in
        // messages_clutter.rs.
        if id.starts_with("l2chunk-") {
            checked += 1;
            continue;
        }
        let clutter = golden("clutter", &id).unwrap_or(Value::Null);
        match (&clutter["clutter_filter_map"], &metadata.clutter_filter_map) {
            (Value::Null, None) => {}
            (golden, Some(map)) if !golden.is_null() => {
                assert_eq!(
                    map.generation_time()
                        .format("%Y-%m-%dT%H:%M:%SZ")
                        .to_string(),
                    golden["generation_time"].as_str().unwrap(),
                    "{id}: message 15 time"
                );
                assert_eq!(
                    map.segments.len() as u64,
                    uint(&golden["elevation_segments"], "segments"),
                    "{id}"
                );
            }
            (golden, map) => panic!(
                "{id}: message 15 golden present {}, decoded {}",
                !golden.is_null(),
                map.is_some()
            ),
        }
        match (&clutter["clutter_filter_bypass_map"], &metadata.bypass_map) {
            (Value::Null, None) => {}
            // MetPy joins KVWX 2008's zero-filled frames numbered from 0; the
            // walker does not (see messages_clutter.rs).
            (_, None) if id == "l2-kvwx-20080415-235337" => {}
            (golden, Some(map)) if !golden.is_null() => {
                assert_eq!(
                    map.generation_time()
                        .map(|time| time.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
                    golden["generation_time"].as_str().map(str::to_owned),
                    "{id}: message 13 time"
                );
                assert_eq!(
                    map.segments.len() as u64,
                    uint(&golden["elevation_segments"], "segments"),
                    "{id}"
                );
            }
            (golden, map) => panic!(
                "{id}: message 13 golden present {}, decoded {}",
                !golden.is_null(),
                map.is_some()
            ),
        }
        checked += 1;
    }
    // 27 status goldens, the committed start chunk among them.
    assert_eq!(sources.len(), 27, "status goldens");
    assert_checked_every_available("status goldens", checked, &sources);
}

/// Per-sweep constant blocks against MetPy's sweeps (the `msg31` goldens):
/// same sweep count and radials per sweep, and the first radial's VOL, ELV
/// and RAD values.
#[test]
fn per_sweep_data_matches_metpy_sweeps() {
    let mut checked = 0;
    let mut sources = Vec::new();
    for (name, golden) in goldens("msg31") {
        sources.push(
            source_ids(&golden)
                .iter()
                .map(|id| (*id).to_owned())
                .collect::<Vec<_>>(),
        );
        let Some(bytes) = load_all(&source_ids(&golden)) else {
            continue;
        };
        let decoded = read_volume_with_metadata(&bytes).unwrap();
        let sweeps = sweeps(&name, &decoded);
        let metpy = golden["sweeps"].as_array().unwrap();
        assert_eq!(sweeps.len(), metpy.len(), "{name}: sweeps");
        for ((sweep, cut), metpy) in sweeps.iter().zip(&decoded.volume.sweeps).zip(metpy) {
            let what = format!("{name} sweep {}", sweep.sweep_index);
            let first = &metpy["first"];
            assert_eq!(
                cut.nrays() as u64,
                uint(&metpy["radials"], "radials"),
                "{what}"
            );
            assert_eq!(
                u64::from(sweep.elevation_number),
                uint(&first["header.el_num"], "el_num"),
                "{what}"
            );
            assert_eq!(
                collect_ms(&decoded.volume, sweep.sweep_index, 0),
                int(&first["header.time_ms"], "time_ms"),
                "{what}"
            );
            assert_eq!(
                sweep.elevation_angle_deg,
                real(&first["header.el_angle"], "el_angle"),
                "{what}"
            );

            let vol = sweep.volume.as_ref().unwrap();
            assert_eq!(
                u64::from(vol.block_size),
                uint(&first["vol.size"], "vol.size")
            );
            assert_eq!(
                vol.latitude_deg,
                real(&first["vol.lat"], "vol.lat"),
                "{what}"
            );
            assert_eq!(
                vol.longitude_deg,
                real(&first["vol.lon"], "vol.lon"),
                "{what}"
            );
            assert_eq!(
                vol.calibration_constant_db,
                real(&first["vol.calib_dbz"], "vol.calib_dbz"),
                "{what}"
            );
            assert_eq!(
                vol.system_differential_reflectivity_db,
                real(&first["vol.sys_zdr"], "vol.sys_zdr"),
                "{what}"
            );
            assert_eq!(
                u64::from(vol.vcp_number),
                uint(&first["vol.vcp"], "vol.vcp")
            );

            let elv = sweep.elevation.as_ref().unwrap();
            assert_eq!(
                u64::from(elv.block_size),
                uint(&first["elv.size"], "elv.size")
            );
            assert_scaled(
                f64::from(elv.atmospheric_attenuation_raw) * 0.001,
                &first["elv.atmos_atten"],
                &format!("{what} elv.atmos_atten"),
            );
            assert_eq!(
                elv.calibration_constant_db,
                real(&first["elv.calib_dbz0"], "elv.calib_dbz0"),
                "{what}"
            );

            let rad = sweep.radial.as_ref().unwrap();
            assert_eq!(
                u64::from(rad.block_size),
                uint(&first["rad.size"], "rad.size")
            );
            assert_scaled(
                f64::from(rad.unambiguous_range_raw) * 0.1,
                &first["rad.unamb_range"],
                &format!("{what} rad.unamb_range"),
            );
            assert_scaled(
                f64::from(rad.nyquist_velocity_raw) * 0.01,
                &first["rad.nyq_vel"],
                &format!("{what} rad.nyq_vel"),
            );
            assert_eq!(
                rad.horizontal_noise_level_dbm,
                real(&first["rad.noise_h"], "rad.noise_h"),
                "{what}"
            );
            assert_eq!(
                rad.horizontal_calibration_constant_dbz,
                first
                    .get("rad.calib_dbz0_h")
                    .map(|value| real(value, "rad.calib_dbz0_h")),
                "{what}"
            );
        }
        checked += 1;
    }
    // 14 msg31 goldens, the committed chunks among them.
    assert_eq!(sources.len(), 14, "msg31 goldens");
    assert_checked_every_available("msg31 goldens", checked, &sources);
}

/// Decompress one bzip2 LDM record.
fn bunzip(compressed: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    bzip2::read::BzDecoder::new(compressed)
        .read_to_end(&mut out)
        .unwrap();
    out
}

fn bzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// The decompressed metadata record of a real-time start chunk: a 24-byte
/// volume header, then one LDM record (a big-endian byte count, negative for
/// the last record, and a bzip2 stream).
fn start_chunk_record(start: &[u8]) -> Vec<u8> {
    let control = i32::from_be_bytes(start[24..28].try_into().unwrap());
    assert_eq!(
        start.len(),
        28 + control.unsigned_abs() as usize,
        "one record in the start chunk"
    );
    bunzip(&start[28..])
}

/// The committed KIWA start chunk (`CHUNKS[0]`) with its record replaced by
/// `record`, recompressed.
fn rebuild_start_chunk(record: &[u8]) -> Vec<u8> {
    let start = load(CHUNKS[0]).unwrap_or_else(|| panic!("{} is committed", CHUNKS[0]));
    let control = i32::from_be_bytes(start[24..28].try_into().unwrap());
    let compressed = bzip(record);
    let mut rebuilt = start[..24].to_vec();
    let length = i32::try_from(compressed.len()).unwrap() * control.signum();
    rebuilt.extend_from_slice(&length.to_be_bytes());
    rebuilt.extend_from_slice(&compressed);
    rebuilt
}

/// Offset of the first 2432-byte frame of `message_type` in a record.
fn frame_of(record: &[u8], message_type: u8) -> usize {
    (0..record.len() / 2432)
        .map(|index| index * 2432)
        .find(|&offset| record[offset + 12 + 3] == message_type)
        .unwrap_or_else(|| panic!("no message {message_type} frame"))
}

/// A broken metadata message does not fail the decode: the committed KIWA
/// start chunk with its Message 15 elevation segment count set to 9 (the
/// ICD allows 1 to 5) still decodes the same volume and every other
/// metadata field, and the error is reported.
#[test]
fn broken_metadata_message_is_reported_not_fatal() {
    let Some(start) = load(CHUNKS[0]) else {
        return;
    };
    let Some(rest) = load_all(&CHUNKS[1..]) else {
        return;
    };
    let original = [start.clone(), rest.clone()].concat();
    let expected = read_volume_with_metadata(&original).unwrap();
    assert!(expected.metadata.clutter_filter_map.is_some());
    assert!(
        expected.metadata.errors.is_empty(),
        "{:?}",
        expected.metadata.errors
    );

    let mut record = start_chunk_record(&start);
    // Body halfword 3 (after date and minutes) is the elevation segment count.
    let segments = frame_of(&record, 15) + 12 + 16 + 4;
    assert_eq!(&record[segments..segments + 2], &[0, 5]);
    record[segments + 1] = 9;
    let mut mutated = rebuild_start_chunk(&record);
    mutated.extend_from_slice(&rest);

    let decoded = read_volume_with_metadata(&mutated).unwrap();
    assert_eq!(decoded.volume, expected.volume);
    assert_eq!(decoded.metadata.clutter_filter_map, None);
    assert_eq!(
        decoded.metadata.errors.len(),
        1,
        "{:?}",
        decoded.metadata.errors
    );
    assert!(
        decoded.metadata.errors[0].contains("message type 15"),
        "{}",
        decoded.metadata.errors[0]
    );
    let unchanged = NexradMetadata {
        clutter_filter_map: expected.metadata.clutter_filter_map.clone(),
        errors: Vec::new(),
        ..decoded.metadata.clone()
    };
    assert_same(&unchanged, &expected.metadata, "other metadata fields");
}

/// Files whose metadata record has broken or stale frames, and a legacy RDA
/// file: the problems are listed, the rest decodes.
#[test]
fn legacy_and_stale_metadata_records() {
    // KLIX 2005: legacy RDA status (no build), message 1 radials (no
    // per-sweep data), a zero-filled message 5 and message 15, and an orphan
    // run of message 13 segments.
    if let Some(bytes) = load("l2-klix-20050829-130035") {
        let decoded = read_volume_with_metadata(&bytes).unwrap();
        let metadata = &decoded.metadata;
        assert!(matches!(metadata.rda_status, Some(RdaStatus::Legacy(_))));
        assert_eq!(metadata.build, None);
        assert_eq!(metadata.per_sweep_elevation_data, None);
        assert!(metadata.vcp.is_none() && metadata.clutter_filter_map.is_none());
        assert!(metadata.bypass_map.is_some());
        for message_type in [5, 13, 15] {
            assert!(
                metadata
                    .errors
                    .iter()
                    .any(|error| error.contains(&format!("message type {message_type}"))),
                "message {message_type} in {:?}",
                metadata.errors
            );
        }
    }
    // KDMX 2008 (Build 10.0): stale frames after the metadata messages are
    // reported; every metadata message still decodes.
    if let Some(bytes) = load("l2-kdmx-20080525-205148") {
        let decoded = read_volume_with_metadata(&bytes).unwrap();
        let metadata = &decoded.metadata;
        assert!(!metadata.errors.is_empty());
        assert_eq!(
            metadata.build.map(|build| build.to_string()).as_deref(),
            Some("10.0")
        );
        assert!(metadata.vcp.is_some() && metadata.adaptation.is_some());
        assert!(metadata.clutter_filter_map.is_some() && metadata.bypass_map.is_some());
        assert_eq!(sweeps("KDMX", &decoded).len(), decoded.volume.sweeps.len());
    }
}

/// The first message of a type wins. The start chunk's message 2 is its
/// last frame; a copy with another VCP number, written into an empty frame
/// before it, becomes the first message 2 and is the one kept.
#[test]
fn first_message_of_a_type_is_kept() {
    let Some(start) = load(CHUNKS[0]) else {
        return;
    };
    let original = NexradMetadata::from_metadata_record(&start);
    let vcp = |metadata: &NexradMetadata| {
        metadata
            .rda_status
            .as_ref()
            .map(|status| status.volume_coverage_pattern().signed())
    };
    assert_eq!(vcp(&original), Some(215));

    let mut record = start_chunk_record(&start);
    let status = frame_of(&record, 2);
    let empty = (0..status)
        .step_by(2432)
        .find(|&offset| record[offset + 12..offset + 14] == [0, 0])
        .expect("an empty frame before message 2");
    let copy = record[status..status + 2432].to_vec();
    record[empty..empty + 2432].copy_from_slice(&copy);
    // Body halfword 8: VCP number, changed from 215 to 35 in the copy.
    let vcp_halfword = empty + 12 + 16 + 14;
    assert_eq!(
        &record[vcp_halfword..vcp_halfword + 2],
        &215u16.to_be_bytes()
    );
    record[vcp_halfword..vcp_halfword + 2].copy_from_slice(&35u16.to_be_bytes());
    let mutated = rebuild_start_chunk(&record);

    let messages_2: Vec<i32> = MessageWalker::new(&messages::metadata_record(&mutated).unwrap())
        .flatten()
        .filter_map(|(_, body)| match body {
            MessageBody::RdaStatus(status) => Some(status.volume_coverage_pattern().signed()),
            _ => None,
        })
        .collect();
    assert_eq!(messages_2, [35, 215]);
    let metadata = NexradMetadata::from_metadata_record(&mutated);
    assert_eq!(vcp(&metadata), Some(35));
    let restored = NexradMetadata {
        rda_status: original.rda_status.clone(),
        ..metadata
    };
    assert_same(&restored, &original, "other fields unchanged");
}

/// Real-time chunks decoded out of order: the first chunk of elevation 3
/// (014, at 1.0 degrees) arrives before the rest of elevation 1 (003, at
/// 0.5 degrees). The volume decoder puts the radials of 003 back into the
/// first cut; the per-sweep data stays with the radials that opened each
/// cut.
#[test]
fn out_of_order_chunks_keep_per_sweep_alignment() {
    // Chunk 014 is the only one not committed: skip (with a message) only
    // when it cannot be downloaded. The others are committed and always
    // load.
    let chunk_014 = recast_radar_testdata::require_file!("l2chunk-kiwa-307-20260917-003629-014-i");
    let chunk_014 = std::fs::read(&chunk_014).unwrap();
    let committed = |id: &str| recast_radar_testdata::bytes(id).unwrap();
    let first_two = [committed(CHUNKS[0]), committed(CHUNKS[1])].concat();
    let bytes = [first_two.clone(), chunk_014, committed(CHUNKS[2])].concat();
    let decoded = read_volume_with_metadata(&bytes).unwrap();
    assert_eq!(decoded.volume, read_volume_from_bytes(&bytes).unwrap());
    let volume = &decoded.volume;
    assert_eq!(volume.sweeps.len(), 2);
    assert_eq!(volume.sweeps[0].elevation_number, Some(1));
    assert_eq!(volume.sweeps[1].elevation_number, Some(3));
    assert_eq!(volume.sweeps[0].nrays(), 240, "chunks 002 and 003");
    assert_eq!(volume.sweeps[1].nrays(), 120, "chunk 014");
    let per_sweep = sweeps("out of order", &decoded);
    assert!(
        decoded.metadata.errors.is_empty(),
        "{:?}",
        decoded.metadata.errors
    );

    // Cut 0 opened in chunk 002: the same per-sweep data as when chunks 001
    // and 002 are decoded alone, whose radials are the first 120 here. Every
    // radial of both cuts is listed, chunk 003's after chunk 014's.
    let alone = read_volume_with_metadata(&first_two).unwrap();
    let alone_sweeps = sweeps("chunks 001-002", &alone);
    assert_eq!(alone_sweeps.len(), 1);
    assert_eq!(alone_sweeps[0].radials.len(), 120);
    let mut cut_0 = per_sweep[0].clone();
    assert_eq!(cut_0.radials.len(), 240);
    cut_0.radials.truncate(120);
    assert_same(&cut_0, &alone_sweeps[0], "cut 0 per-sweep data");
    assert_eq!(per_sweep[1].radials.len(), 120);
    assert_eq!(per_sweep[1].elevation_number, 3);
    assert_eq!(per_sweep[1].sweep_index, 1);
    assert_eq!(
        per_sweep[1].elevation_angle_deg,
        volume.sweeps[1].rays.elevation_deg[0]
    );
}
