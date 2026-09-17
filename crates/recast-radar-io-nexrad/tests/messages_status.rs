//! Messages 2 (RDA Status Data), 3 (Performance/Maintenance Data) and 18 (RDA
//! Adaptation Data) on real files.
//!
//! Expected values come from MetPy 1.7.1's `Level2File`, written by
//! `tools/level2_golden.py status` to `testdata/level2/golden/status/`. The
//! decoders follow ICD 2620002AA (Build 24.0); MetPy's layouts for messages 3
//! and 18 predate Build 17, so those comparisons go by location and type
//! rather than by name: every MetPy field whose location and type match a
//! Build 24.0 field is compared exactly, and the locations where they do not
//! match are listed (they were reassigned or made spare). The Build 24.0
//! fields MetPy does not read are checked against values read from the hex of
//! the KIWA 2026-09-17 volume (Build 24.1) and against ICD ranges in every
//! Build 19+ file.

mod common;

use std::collections::BTreeMap;

use recast_radar_io_nexrad::messages::adaptation::RdaAdaptationData;
use recast_radar_io_nexrad::messages::performance::PerformanceMaintenance;
use recast_radar_io_nexrad::messages::rda_status::{
    ChannelControlStatus, CommandAcknowledgment, ControlAuthorization, ControlStatus, EnableStatus,
    LegacyRdaStatus, OperabilityState, OperabilityStatus, OperationalMode, OrdaRdaStatus,
    PerformanceCheckStatus, RdaAlarm, RdaState, RdaStatus, RmsControl, SpotBlanking,
    TransitionPowerSource, VcpSelection,
};
use recast_radar_io_nexrad::messages::{self, MessageBody};
use recast_radar_io_nexrad::{MessageHeader, NexradError};
use serde_json::Value;

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

fn golden(id: &str) -> Value {
    let path = recast_radar_testdata::testdata_dir()
        .join("level2/golden/status")
        .join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// What the walker produced for the first message of a type.
#[derive(Debug)]
enum First<T> {
    Absent,
    Decoded(MessageHeader, T),
    Unparsed(MessageHeader, usize),
}

struct Metadata {
    status: First<RdaStatus>,
    performance: First<PerformanceMaintenance>,
    adaptation: First<RdaAdaptationData>,
    /// Body bytes of the first message of each type, for mutation tests.
    bodies: BTreeMap<u8, Vec<u8>>,
}

/// Walk the metadata record and keep the first message 2, 3 and 18.
fn metadata(raw: &[u8]) -> Metadata {
    let record = messages::metadata_record(raw).unwrap();
    let mut out = Metadata {
        status: First::Absent,
        performance: First::Absent,
        adaptation: First::Absent,
        bodies: BTreeMap::new(),
    };
    for item in messages::RawMessages::new(&record) {
        let Ok(message) = item else { continue };
        let kind = message.header.message_type;
        if !matches!(kind, 2 | 3 | 18) || out.bodies.contains_key(&kind) {
            continue;
        }
        out.bodies.insert(kind, message.body.to_vec());
        let (header, body) = message.decode().unwrap();
        match (kind, body) {
            (2, MessageBody::RdaStatus(status)) => out.status = First::Decoded(header, status),
            (3, MessageBody::Performance(data)) => {
                out.performance = First::Decoded(header, *data);
            }
            (18, MessageBody::Adaptation(data)) => {
                out.adaptation = First::Decoded(header, *data);
            }
            (3, MessageBody::Unparsed(bytes)) => {
                out.performance = First::Unparsed(header, bytes.len());
            }
            (18, MessageBody::Unparsed(bytes)) => {
                out.adaptation = First::Unparsed(header, bytes.len());
            }
            (kind, other) => panic!("message {kind} decoded as {other:?}"),
        }
    }
    out
}

/// A decoded value, typed as in the decoder.
#[derive(Clone, Debug, PartialEq)]
enum Num {
    U8(u8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    F32(f32),
    F64(f64),
    Str(String),
    Flag(Option<bool>),
}

fn json_u32_bits(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .map(|number| number as u32)
        .or_else(|| value.as_i64().map(|number| number as u32))
}

/// MetPy floats are f32 values widened to f64; compare after narrowing, so an
/// f64 parse that is off by one unit in the last place still matches.
fn json_f32_matches(ours: f32, value: &Value) -> bool {
    match value {
        Value::String(text) if text == "NaN" => ours.is_nan(),
        Value::String(text) if text == "Infinity" => ours == f32::INFINITY,
        Value::String(text) if text == "-Infinity" => ours == f32::NEG_INFINITY,
        Value::Number(number) => number
            .as_f64()
            .is_some_and(|golden| (golden as f32).to_bits() == ours.to_bits()),
        _ => false,
    }
}

fn trim_text(text: &str) -> &str {
    text.trim_matches('\0').trim()
}

/// Compare one of our values with a MetPy entry of struct format `format`, or
/// `None` when the types differ (a different field at that location).
fn matches_metpy(ours: &Num, format: &str, value: &Value) -> Option<bool> {
    Some(match (format, ours) {
        ("H", Num::U16(v)) => value.as_u64() == Some(u64::from(*v)),
        ("H", Num::I16(v)) => value.as_u64() == Some(u64::from(*v as u16)),
        ("L" | "l", Num::U32(v)) => json_u32_bits(value) == Some(*v),
        ("L" | "l", Num::I32(v)) => json_u32_bits(value) == Some(*v as u32),
        ("f", Num::F32(v)) => json_f32_matches(*v, value),
        ("12s" | "4s", Num::Str(text)) => value.as_str().map(trim_text) == Some(text.as_str()),
        ("4s", Num::Flag(flag)) => {
            let golden = value.as_str().map(|text| match text.as_bytes().first() {
                Some(b'T') => Some(true),
                Some(b'F') => Some(false),
                _ => None,
            });
            golden == Some(*flag)
        }
        _ => return None,
    })
}

/// Compare every golden entry (keyed by location) with our value at that
/// location. Returns the golden locations skipped for a type mismatch or a
/// missing field, and our locations that no golden entry was compared with.
fn compare_by_location(
    id: &str,
    what: &str,
    ours: &[(usize, Num)],
    entries: &serde_json::Map<String, Value>,
) -> (Vec<usize>, Vec<usize>) {
    let by_location: BTreeMap<usize, &Num> = ours.iter().map(|(at, value)| (*at, value)).collect();
    let mut skipped = Vec::new();
    let mut compared = Vec::new();
    let mut mismatches = Vec::new();
    for (key, entry) in entries {
        let location: usize = key.parse().unwrap();
        let format = entry["format"].as_str().unwrap();
        let value = &entry["value"];
        let Some(our_value) = by_location.get(&location) else {
            skipped.push(location);
            continue;
        };
        match matches_metpy(our_value, format, value) {
            None => skipped.push(location),
            Some(true) => compared.push(location),
            Some(false) => mismatches.push(format!(
                "{location} ({} {format}): MetPy {value}, ours {our_value:?}",
                entry["name"]
            )),
        }
    }
    assert!(
        mismatches.is_empty(),
        "{id} {what}: {} mismatches:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    skipped.sort_unstable();
    let uncovered = by_location
        .keys()
        .copied()
        .filter(|location| !compared.contains(location))
        .collect();
    (skipped, uncovered)
}

/// MetPy halfwords (its pre-Build-17 layout) with no Build 24.0 field of the
/// same type: CSU loss of signal/frames and alarm counts (13-20), LAN switch
/// memory used (39), NTP and GPS counters (45-52), DAU tests (55-57), UPS
/// (99-110), power meter zero as Integer*2 (215), DAU UART (239), DAU +28 V
/// (279), pedestal supplies (291-300), self tests (331-333), transmit burst
/// (415-418) and pedestal communication status (463).
const METPY_ONLY_HALFWORDS: [usize; 32] = [
    13, 15, 17, 19, 39, 45, 47, 49, 51, 55, 56, 57, 99, 101, 103, 105, 107, 109, 215, 239, 279,
    291, 293, 295, 297, 299, 331, 332, 333, 415, 417, 463,
];

/// Build 24.0 halfwords MetPy does not read with the same type; checked in
/// `build_24_fields_metpy_does_not_read` and `icd_ranges_from_build_19`.
const BUILD_24_ONLY_HALFWORDS: [usize; 21] = [
    12, 13, 14, 15, 45, 46, 47, 99, 100, 108, 113, 223, 279, 300, 357, 444, 448, 449, 450, 468, 480,
];

/// MetPy byte offsets with no Build 24.0 field of the same type (spare since
/// Build 17 or 18, or Integer*4 since Build 19 at 1164 and 1172).
const METPY_ONLY_OFFSETS: [usize; 31] = [
    144, 164, 168, 172, 220, 684, 696, 716, 760, 764, 776, 784, 788, 792, 804, 840, 856, 916, 928,
    932, 1132, 1144, 1148, 1164, 1172, 1188, 1192, 1228, 8896, 8900, 8904,
];

/// Build 24.0 byte offsets MetPy does not read with the same type; checked in
/// `build_24_fields_metpy_does_not_read` and `icd_ranges_from_build_19`.
const BUILD_24_ONLY_OFFSETS: [usize; 49] = [
    756, 844, 864, 868, 1164, 1172, 2500, 2508, 8396, 8400, 8404, 8408, 8412, 8416, 8420, 8424,
    8428, 8432, 8436, 8440, 8444, 8448, 8452, 8456, 8460, 8496, 8500, 8504, 8508, 8688, 8700, 8704,
    8708, 8712, 8716, 8720, 8724, 8728, 8732, 8736, 8740, 8752, 8756, 8828, 9000, 9036, 9040, 9044,
    9048,
];

// --- message 2 ----------------------------------------------------------------

fn code(message: &Value, name: &str) -> u16 {
    let value = &message["codes"][name];
    value
        .as_u64()
        .map(|number| number as u16)
        .or_else(|| value.as_i64().map(|number| number as u16))
        .unwrap_or_else(|| panic!("code {name}: {value}"))
}

fn field_u16(message: &Value, name: &str) -> u16 {
    let value = &message["fields"][name];
    value
        .as_u64()
        .map(|number| number as u16)
        .or_else(|| value.as_i64().map(|number| number as u16))
        .unwrap_or_else(|| panic!("field {name}: {value}"))
}

fn field_f64(message: &Value, name: &str) -> f64 {
    message["fields"][name]
        .as_f64()
        .unwrap_or_else(|| panic!("field {name}"))
}

fn alarm_codes(message: &Value) -> Vec<u16> {
    message["fields"]["alarms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_u64().unwrap() as u16)
        .collect()
}

/// Halfwords 1 to 9 and 15 to 40 have one location in both layouts; MetPy
/// reads them with its ORDA struct for legacy files too.
fn check_shared_status_halfwords(id: &str, status: &RdaStatus, message: &Value) {
    let (rda_state, operability, control, aux, power, data, vcp, auth) = match status {
        RdaStatus::Orda(s) => (
            s.rda_state,
            s.operability,
            s.control_status,
            s.auxiliary_power.0,
            s.average_transmitter_power,
            s.data_transmission.0,
            s.volume_coverage_pattern,
            s.control_authorization,
        ),
        RdaStatus::Legacy(s) => (
            s.rda_state,
            s.operability,
            s.control_status,
            s.auxiliary_power.0,
            s.average_transmitter_power,
            s.data_transmission.0,
            s.volume_coverage_pattern,
            s.control_authorization,
        ),
    };
    assert_eq!(
        rda_state,
        RdaState::from_code(code(message, "rda_status")),
        "{id}"
    );
    assert_eq!(
        operability,
        OperabilityStatus::from_code(code(message, "op_status")),
        "{id}"
    );
    assert_eq!(
        control,
        ControlStatus::from_code(code(message, "control_status")),
        "{id}"
    );
    assert_eq!(aux, code(message, "aux_power_gen_state"), "{id}");
    assert_eq!(power, field_u16(message, "avg_tx_pwr"), "{id}");
    assert_eq!(data, code(message, "data_transmission_enabled"), "{id}");
    assert_eq!(
        vcp.signed(),
        i32::from(field_u16(message, "vcp_num") as i16),
        "{id}"
    );
    assert_eq!(
        auth,
        ControlAuthorization::from_code(code(message, "rda_control_auth")),
        "{id}"
    );
    assert_eq!(
        status.alarms().collect::<Vec<_>>(),
        alarm_codes(message)
            .into_iter()
            .filter_map(RdaAlarm::from_halfword)
            .collect::<Vec<_>>(),
        "{id}"
    );
    let (ack, channel, blanking, bypass, map, tps, rms, alarms, summary) = match status {
        RdaStatus::Orda(s) => (
            s.command_acknowledgment,
            s.channel_control,
            s.spot_blanking,
            s.bypass_map_generation,
            s.clutter_filter_map_generation,
            s.transition_power_source,
            s.rms_control,
            s.alarm_codes,
            s.alarm_summary.0,
        ),
        RdaStatus::Legacy(s) => (
            s.command_acknowledgment,
            s.channel_control,
            s.spot_blanking,
            s.bypass_map_generation,
            s.notch_width_map_generation,
            s.transition_power_source,
            s.rms_control,
            s.alarm_codes,
            s.alarm_summary.0,
        ),
    };
    assert_eq!(summary, code(message, "rda_alarm_status"), "{id}");
    assert_eq!(
        ack,
        CommandAcknowledgment::from_code(code(message, "command_acknowledge")),
        "{id}"
    );
    assert_eq!(
        channel,
        ChannelControlStatus::from_code(field_u16(message, "channel_control_status")),
        "{id}"
    );
    assert_eq!(
        blanking,
        SpotBlanking::from_code(code(message, "spot_blanking")),
        "{id}"
    );
    assert_eq!(
        bypass.date,
        field_u16(message, "bypass_map_gen_date"),
        "{id}"
    );
    assert_eq!(
        bypass.minutes,
        field_u16(message, "bypass_map_gen_time"),
        "{id}"
    );
    assert_eq!(
        map.date,
        field_u16(message, "clutter_filter_map_gen_date"),
        "{id}"
    );
    assert_eq!(
        map.minutes,
        field_u16(message, "clutter_filter_map_gen_time"),
        "{id}"
    );
    assert_eq!(
        tps,
        TransitionPowerSource::from_code(code(message, "transition_pwr_src_state")),
        "{id}"
    );
    assert_eq!(
        rms,
        RmsControl::from_code(code(message, "RMS_control_status")),
        "{id}"
    );
    assert_eq!(alarms.to_vec(), alarm_codes(message), "{id}");
}

fn check_orda_status(id: &str, status: &OrdaRdaStatus, message: &Value) {
    let hundredths = |value: f32| (f64::from(value) * 100.0).round() as i64;
    assert_eq!(
        hundredths(status.horizontal_reflectivity_calibration_correction),
        i64::from(code(message, "ref_calib_cor") as i16),
        "{id}"
    );
    assert!(
        (f64::from(status.horizontal_reflectivity_calibration_correction)
            - field_f64(message, "ref_calib_cor"))
        .abs()
            < 1e-6,
        "{id}"
    );
    assert_eq!(status.rda_build.0, code(message, "rda_build"), "{id}");
    assert_eq!(
        format!("{:.1}", status.rda_build.version()),
        message["fields"]["rda_build"].as_str().unwrap(),
        "{id}"
    );
    assert_eq!(
        status.operational_mode,
        OperationalMode::from_code(code(message, "op_mode")),
        "{id}"
    );
    assert_eq!(
        status.super_resolution,
        EnableStatus::from_code(code(message, "super_res_status")),
        "{id}"
    );
    assert_eq!(
        status.clutter_mitigation_decision.0,
        code(message, "cmd_status"),
        "{id}"
    );
    assert_eq!(
        status.scan_data_flags.0,
        code(message, "avset_status"),
        "{id}"
    );
    assert_eq!(
        hundredths(status.vertical_reflectivity_calibration_correction),
        i64::from(code(message, "refv_calib_cor") as i16),
        "{id}"
    );
    match &message["additional"] {
        Value::Null => {
            assert_eq!(status.signal_processing_options, None, "{id}");
            assert_eq!(status.downloaded_pattern_number, None, "{id}");
            assert_eq!(status.status_version, None, "{id}");
        }
        additional => {
            assert_eq!(
                status.signal_processing_options.map(|options| options.0),
                Some(code(additional, "sig_proc_options")),
                "{id}"
            );
            assert_eq!(
                status.status_version,
                Some(field_u16(additional, "status_version")),
                "{id}"
            );
            // Halfword 59 is not read by MetPy; 0x0000 in every corpus file.
            assert_eq!(status.downloaded_pattern_number, Some(0), "{id}");
        }
    }
    // Halfword 26 is not read by MetPy; 0x0000 in every corpus file.
    assert_eq!(
        status.performance_check,
        PerformanceCheckStatus::NoCommandPending,
        "{id}"
    );
}

/// Legacy halfwords 10 to 14 hold other fields; MetPy's ORDA struct reads the
/// same halfwords under ORDA names.
fn check_legacy_status(id: &str, status: &LegacyRdaStatus, message: &Value) {
    assert_eq!(
        status.reflectivity_calibration_correction_raw,
        code(message, "ref_calib_cor") as i16,
        "{id}"
    );
    assert_eq!(
        status.interference_detection_rate,
        code(message, "rda_build"),
        "{id}"
    );
    assert_eq!(
        status.operational_mode,
        OperationalMode::from_code(code(message, "op_mode")),
        "{id}"
    );
    assert_eq!(
        status.interference_suppression_unit,
        EnableStatus::from_code(code(message, "super_res_status")),
        "{id}"
    );
    assert_eq!(
        status.archive_ii_status,
        code(message, "cmd_status"),
        "{id}"
    );
    assert_eq!(
        status.archive_ii_remaining_capacity,
        code(message, "avset_status"),
        "{id}"
    );
}

// --- per-file comparison ------------------------------------------------------

fn check_against_metpy(id: &str) {
    let Some(raw) = load(id) else { return };
    let golden = golden(id);
    let meta = metadata(&raw);

    let message_2 = &golden["message_2"];
    let First::Decoded(header, status) = &meta.status else {
        panic!("{id}: no message 2 decoded: {:?}", meta.status);
    };
    assert_eq!(
        u64::from(header.channels),
        message_2["channels"].as_u64().unwrap(),
        "{id}"
    );
    assert_eq!(
        u64::from(header.size_halfwords),
        message_2["size_hw"].as_u64().unwrap(),
        "{id}"
    );
    check_shared_status_halfwords(id, status, message_2);
    match status {
        RdaStatus::Orda(orda) => {
            assert_ne!(header.channels & 8, 0, "{id}");
            check_orda_status(id, orda, message_2);
        }
        RdaStatus::Legacy(legacy) => {
            assert_eq!(header.channels & 8, 0, "{id}");
            check_legacy_status(id, legacy, message_2);
        }
    }

    match (&golden["message_3"], &meta.performance) {
        (Value::Null, First::Absent) => {}
        (Value::Null, First::Unparsed(header, len)) => {
            assert_eq!(
                header.channels & 8,
                0,
                "{id}: only legacy message 3 is unparsed"
            );
            assert_eq!(*len, 1040, "{id}: legacy Table V is 520 halfwords");
        }
        (Value::Object(message), First::Decoded(_, data)) => {
            let (skipped, uncovered) = compare_by_location(
                id,
                "message 3",
                &performance_values(data),
                message["halfwords"].as_object().unwrap(),
            );
            assert_eq!(
                skipped, METPY_ONLY_HALFWORDS,
                "{id}: message 3 skipped halfwords"
            );
            assert_eq!(
                uncovered, BUILD_24_ONLY_HALFWORDS,
                "{id}: message 3 uncovered"
            );
        }
        (golden, ours) => panic!("{id}: message 3 golden {golden:?}, walker {ours:?}"),
    }

    match (&golden["message_18"], &meta.adaptation) {
        (Value::Null, First::Absent) => {}
        (Value::Null, First::Unparsed(header, len)) => {
            assert_eq!(
                header.channels & 8,
                0,
                "{id}: only legacy message 18 is unparsed"
            );
            assert_eq!(*len, 9600, "{id}: legacy message 18 is 9600 bytes");
        }
        (Value::Object(message), First::Decoded(_, data)) => {
            let (skipped, uncovered) = compare_by_location(
                id,
                "message 18",
                &adaptation_values(data),
                message["bytes"].as_object().unwrap(),
            );
            assert_eq!(
                skipped, METPY_ONLY_OFFSETS,
                "{id}: message 18 skipped offsets"
            );
            assert_eq!(
                uncovered, BUILD_24_ONLY_OFFSETS,
                "{id}: message 18 uncovered"
            );
        }
        (golden, ours) => panic!("{id}: message 18 golden {golden:?}, walker {ours:?}"),
    }
}

macro_rules! metpy_tests {
    ($($name:ident => $id:literal,)*) => {
        $(
            #[test]
            fn $name() {
                check_against_metpy($id);
            }
        )*
    };
}

metpy_tests! {
    metpy_1991_ktlx_archive2_legacy_status => "l2-ktlx-19910605-162126",
    metpy_1999_ktlx_truncated_legacy_status => "l2-ktlx-19990503-230052",
    metpy_2005_klix_legacy_rda => "l2-klix-20050829-130035",
    metpy_2008_kvwx_legacy_status_only => "l2-kvwx-20080415-235337",
    metpy_2008_kpah_build_10 => "l2-kpah-20080415-235014",
    metpy_2008_kdmx_build_10 => "l2-kdmx-20080525-205148",
    metpy_2011_kvnx_build_12 => "l2-kvnx-20110315-000203",
    metpy_2013_ktlx_build_13_2 => "l2-ktlx-20130520-201643",
    metpy_2013_kgwx_build_13_1 => "l2-kgwx-20130601-235640",
    metpy_2014_koax_build_14 => "l2-koax-20140616-205305",
    metpy_2016_kewx_build_16_1 => "l2-kewx-20160413-022531",
    metpy_2020_kdvn_build_18_2 => "l2-kdvn-20200810-180401",
    metpy_2021_klix_build_19_1 => "l2-klix-20210829-180425",
    metpy_2022_kbox_build_20_1 => "l2-kbox-20220129-150537",
    metpy_2022_tjua_build_20_1 => "l2-tjua-20220918-190621",
    metpy_2023_kdgx_build_21_1 => "l2-kdgx-20230325-010651",
    metpy_2023_kmaf_build_21 => "l2-kmaf-20230331-230843",
    metpy_2023_tstl_tdwr => "l2-tstl-20230331-230314",
    metpy_2023_pgua_build_21_1 => "l2-pgua-20230524-030945",
    metpy_2023_tbwi_tdwr_stub => "l2-tbwi-20230601-175101-stub",
    metpy_2024_kmtx_build_22 => "l2-kmtx-20240301-212827",
    metpy_2024_ktlx_build_22 => "l2-ktlx-20240315-000217",
    metpy_2024_ktlx_build_22_1 => "l2-ktlx-20240515-000014",
    metpy_2025_pahg_build_23_1 => "l2-pahg-20250909-212549",
    metpy_2026_kilx_build_23_1 => "l2-kilx-20260418-013553",
    metpy_2026_kiwa_build_24_1 => "l2-kiwa-20260917-003629",
    metpy_2026_kiwa_start_chunk => "l2chunk-kiwa-307-20260917-003629-001-s",
}

// --- fields MetPy does not read -----------------------------------------------

/// KIWA 2026-09-17 00:36Z (Build 24.1). Expected values are the big-endian
/// bytes of the metadata record, noted in the comments.
#[test]
fn build_24_fields_metpy_does_not_read() {
    let Some(raw) = load("l2-kiwa-20260917-003629") else {
        return;
    };
    let meta = metadata(&raw);
    let First::Decoded(_, RdaStatus::Orda(status)) = &meta.status else {
        panic!("no ORDA message 2");
    };
    assert_eq!(
        status.performance_check,
        PerformanceCheckStatus::NoCommandPending
    ); // hw26 0000
    assert_eq!(status.downloaded_pattern_number, Some(0)); // hw59 0000
    assert_eq!(status.status_version, Some(11)); // hw60 000b
    assert!(
        status
            .signal_processing_options
            .unwrap()
            .cmd_rho_hv_test_enabled()
    ); // hw41 0001

    let First::Decoded(_, p) = &meta.performance else {
        panic!("no message 3");
    };
    let c = &p.communications;
    assert_eq!(c.route_to_rpg, 1); // hw12 0001: backup in use
    assert_eq!(c.t1_port_status, 2); // hw13 0002: down
    assert_eq!(c.router_dedicated_ethernet_port_status, 2); // hw14 0002
    assert_eq!(c.router_commercial_ethernet_port_status, 2); // hw15 0002
    assert_eq!(c.ifdr_chassis_temperature, 38); // hw45 0026
    assert_eq!(c.ifdr_fpga_temperature, 40); // hw46 0028
    assert_eq!(c.ntp_status, 0); // hw47 0000
    assert_eq!(p.rcp_spip.rcp_status, 0); // hw99 0000
    assert_eq!(p.rcp_spip.rcp_string, "IRIS"); // hw100-107 49524953 00..
    assert_eq!(p.rcp_spip.spip_power_buttons, 0); // hw108 0000
    assert_eq!(
        p.power.expansion_power_administrator_load,
        f32::from_bits(0x3fea_3d71)
    ); // 1.83 A
    assert_eq!(
        p.transmitter.xmtr_power_meter_zero,
        f32::from_bits(0x404c_cccd)
    ); // 3.2 V
    assert_eq!(p.equipment_shelter.spip_28v_ps_status, 1); // hw279 0001: OK
    assert_eq!(p.antenna_pedestal.elevation_pos_dead_limit, 0); // hw300 0000
    assert_eq!(
        p.rf_generator_receiver.vertical_short_pulse_noise,
        f32::from_bits(0xc2a5_0884) // -82.5166 dBm
    );
    assert_eq!(p.file_status.prf_sets_read, 7); // hw444 0007: all three OK
    assert_eq!(p.file_status.rsp_status, 0); // hw448 0000
    assert_eq!(p.file_status.rsp_cpu1_temperature, 0x32); // hw449 3234: 50 deg C
    assert_eq!(p.file_status.rsp_cpu2_temperature, 0x34); // 52 deg C
    assert_eq!(p.file_status.rsp_motherboard_power, 0); // hw450 0000
    assert_eq!(p.device_status.interpanel_link_status, 2); // hw468 0002: N/A
    assert_eq!(p.version, 26); // hw480 001a

    let First::Decoded(_, a) = &meta.adaptation else {
        panic!("no message 18");
    };
    assert_eq!(a.h_coupler_xmt_loss, f32::from_bits(0xc1f2_51ec)); // -30.29 dB
    assert_eq!(a.ame_ts_bias, f32::from_bits(0xbeb8_51ec)); // -0.36 dB
    assert_eq!(a.pwr_sense_bias, f32::from_bits(0xbd75_c28f)); // -0.06 dB
    assert_eq!(a.ame_v_noise_enr, f32::from_bits(0x41b7_851f)); // 22.94 dB
    assert_eq!(a.h_min_noisetemp, 100); // 00000064
    assert_eq!(a.v_min_noisetemp, 100); // 00000064
    assert_eq!(a.dig_rcvr_clock_freq, f64::from_bits(0x4057_611b_3d07_c84b)); // 93.5172875 MHz
    assert_eq!(a.coho_freq, f64::from_bits(0x404c_c648_e8a7_1de7)); // 57.5491 MHz
    let drive = [
        (a.az_pos_sustain_drive, 0x3e05_1eb8),   // 0.13
        (a.az_neg_sustain_drive, 0xbfb7_0a3d),   // -1.43
        (a.az_nom_pos_drive_slope, 0x3e88_b439), // 0.267
        (a.az_nom_neg_drive_slope, 0x3ea6_e979), // 0.326
        (a.az_feedback_slope, 0x408a_76c9),      // 4.327
        (a.el_pos_sustain_drive, 0x3ff9_999a),   // 1.95
        (a.el_neg_sustain_drive, 0xc03a_e148),   // -2.92
        (a.el_nom_pos_drive_slope, 0x3e85_a1cb), // 0.261
        (a.el_nom_neg_drive_slope, 0x3f0a_c083), // 0.542
        (a.el_feedback_slope, 0x4046_872b),      // 3.102
        (a.el_first_slope, 0x4137_ae14),         // 11.48
        (a.el_second_slope, 0x40ad_1eb8),        // 5.41
        (a.el_third_slope, 0x4051_47ae),         // 3.27
        (a.el_droop_pos, 0x41a2_6666),           // 20.3 deg
        (a.el_off_neutral_drive, 0x0000_0000),   // 0.0
        (a.az_inertia, 0x4026_6666),             // 2.6
        (a.el_inertia, 0x3f80_0000),             // 1.0
        (a.az_stow_angle, 0),
        (a.el_stow_angle, 0),
        (a.az_encoder_alignment, 0),
        (a.el_encoder_alignment, 0),
    ];
    for (index, (value, bits)) in drive.iter().enumerate() {
        assert_eq!(value.to_bits(), *bits, "pedestal parameter {index}");
    }
    assert_eq!(a.refined_park, Some(true)); // 54000000 "T"
    let v_rnscale: [u32; 13] = [
        0x3fc8_9375,
        0x3fc8_d4fe,
        0x3fcf_be77,
        0x3fd3_f7cf,
        0x3fd0_4189,
        0x3fb2_6e98,
        0x3f92_2d0e,
        0x3f88_d4fe,
        0x3f88_3127,
        0x3f87_0a3d,
        0x3f85_a1cb, // bytes 8700-8743
        0x3f84_3958,
        0x3f80_0000, // bytes 8752-8759
    ];
    assert_eq!(a.v_rnscale.map(f32::to_bits), v_rnscale);
    assert_eq!(a.baseline_zdr_offset, f32::from_bits(0xbf04_60aa)); // -0.5171 dB
    assert_eq!(a.rfp_stepper_enabled, Some(false)); // 46000000 "F"
    assert_eq!(a.power_meter_zero, f32::from_bits(0xbf33_3333)); // -0.7 V
    assert_eq!(a.txb_baseline, f32::from_bits(0xbca3_d70a)); // -0.02 dB
    assert_eq!(a.txb_alarm_thresh, 2.0); // 40000000
    assert_eq!(a.normal_tps_power_time, 30); // 0000001e
}

fn in_range<T: PartialOrd + std::fmt::Debug>(id: &str, name: &str, value: T, low: T, high: T) {
    assert!(
        value >= low && value <= high,
        "{id}: {name} = {value:?} outside ICD range {low:?} to {high:?}"
    );
}

/// ICD 2620002AA ranges for the Build 24.0 fields MetPy does not read, in
/// every corpus volume from Build 19.0 on (where they are defined).
#[test]
fn icd_ranges_from_build_19() {
    let ids = [
        "l2-klix-20210829-180425",
        "l2-klix-20210829-173117",
        "l2-klix-20210829-175748",
        "l2-kbox-20220129-150537",
        "l2-tjua-20220918-190621",
        "l2-kdgx-20230325-010651",
        "l2-kmaf-20230331-230843",
        "l2-pgua-20230524-030945",
        "l2-kmtx-20240301-212827",
        "l2-ktlx-20240315-000217",
        "l2-ktlx-20240515-000014",
        "l2-pahg-20250909-212549",
        "l2-kilx-20260418-013553",
        "l2-kiwa-20260917-003629",
    ];
    // Every archive volume from Build 19.0 on: the manifest's build tags
    // name these 14 and no other.
    let tagged: Vec<&str> = recast_radar_testdata::manifest()
        .files
        .iter()
        .filter(|entry| {
            entry.tags.iter().any(|tag| {
                tag.strip_prefix("build:")
                    .and_then(|build| build.parse::<f32>().ok())
                    .is_some_and(|build| build >= 19.0)
            }) && entry.derived_from.is_none()
                && !entry.id.starts_with("l2chunk-")
        })
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(tagged, ids, "manifest volumes tagged Build 19.0 or later");
    let mut checked = 0;
    for id in ids {
        let Some(raw) = load(id) else { continue };
        checked += 1;
        let meta = metadata(&raw);
        let First::Decoded(_, RdaStatus::Orda(status)) = &meta.status else {
            panic!("{id}: no ORDA message 2");
        };
        let build = status.rda_build.version();
        assert!(build >= 19.0, "{id}: build {build}");
        in_range(
            id,
            "downloaded pattern",
            status.downloaded_pattern_number.unwrap(),
            0,
            767,
        );
        assert_eq!(status.signal_processing_options.unwrap().0 & !1, 0, "{id}");

        let First::Decoded(_, p) = &meta.performance else {
            panic!("{id}: no message 3");
        };
        let c = &p.communications;
        in_range(id, "route to RPG", c.route_to_rpg, 0, 4);
        if build >= 23.0 {
            in_range(id, "T1 port status", c.t1_port_status, 0, 3);
            in_range(
                id,
                "dedicated Ethernet",
                c.router_dedicated_ethernet_port_status,
                0,
                3,
            );
            in_range(
                id,
                "commercial Ethernet",
                c.router_commercial_ethernet_port_status,
                0,
                3,
            );
        }
        in_range(
            id,
            "IFDR chassis temperature",
            c.ifdr_chassis_temperature,
            -30,
            150,
        );
        in_range(
            id,
            "IFDR FPGA temperature",
            c.ifdr_fpga_temperature,
            -30,
            150,
        );
        in_range(id, "NTP status", c.ntp_status, 0, 2);
        in_range(id, "RCP status", p.rcp_spip.rcp_status, 0, 1);
        assert!(
            !p.rcp_spip.rcp_string.is_empty()
                && p.rcp_spip
                    .rcp_string
                    .chars()
                    .all(|ch| ch.is_ascii_graphic()),
            "{id}: RCP string {:?}",
            p.rcp_spip.rcp_string
        );
        assert_eq!(p.rcp_spip.spip_power_buttons & !0x1f, 0, "{id}");
        in_range(
            id,
            "expansion load",
            p.power.expansion_power_administrator_load,
            0.0,
            12.0,
        );
        in_range(
            id,
            "power meter zero",
            p.transmitter.xmtr_power_meter_zero,
            0.01,
            8.0,
        );
        in_range(
            id,
            "SPIP +28 V status",
            p.equipment_shelter.spip_28v_ps_status,
            0,
            1,
        );
        in_range(
            id,
            "elevation + dead limit",
            p.antenna_pedestal.elevation_pos_dead_limit,
            0,
            1,
        );
        in_range(
            id,
            "vertical short pulse noise",
            p.rf_generator_receiver.vertical_short_pulse_noise,
            -100.0,
            -50.0,
        );
        in_range(id, "PRF set read status", p.file_status.prf_sets_read, 0, 7);
        in_range(
            id,
            "interpanel link",
            p.device_status.interpanel_link_status,
            0,
            2,
        );
        in_range(id, "version", p.version, 1, u16::MAX);

        let First::Decoded(_, a) = &meta.adaptation else {
            panic!("{id}: no message 18");
        };
        in_range(id, "H_COUPLER_XMT_LOSS", a.h_coupler_xmt_loss, -40.0, -20.0);
        in_range(id, "PWR_SENSE_BIAS", a.pwr_sense_bias, -10.0, 10.0);
        in_range(id, "AME_V_NOISE_ENR", a.ame_v_noise_enr, 10.0, 35.0);
        in_range(id, "H_MIN_NOISETEMP", a.h_min_noisetemp, 1, 150);
        in_range(id, "V_MIN_NOISETEMP", a.v_min_noisetemp, 1, 150);
        in_range(id, "AZ_POS_SUSTAIN_DRIVE", a.az_pos_sustain_drive, 0.0, 7.0);
        in_range(
            id,
            "AZ_NEG_SUSTAIN_DRIVE",
            a.az_neg_sustain_drive,
            -7.0,
            0.0,
        );
        in_range(id, "EL_POS_SUSTAIN_DRIVE", a.el_pos_sustain_drive, 0.0, 7.0);
        in_range(
            id,
            "EL_NEG_SUSTAIN_DRIVE",
            a.el_neg_sustain_drive,
            -7.0,
            0.0,
        );
        in_range(id, "AZ_FEEDBACK_SLOPE", a.az_feedback_slope, 0.0, 15.0);
        in_range(id, "EL_FEEDBACK_SLOPE", a.el_feedback_slope, 0.0, 15.0);
        in_range(id, "EL_DROOP_POS", a.el_droop_pos, -360.0, 360.0);
        in_range(id, "AZ_INERTIA", a.az_inertia, 0.5, 7.0);
        in_range(id, "EL_INERTIA", a.el_inertia, 0.5, 7.0);
        // REFINED_PARK is zero bytes before Build 21.0 in the corpus.
        if build >= 21.0 {
            assert!(a.refined_park.is_some(), "{id}: REFINED_PARK");
        }
        assert!(a.rfp_stepper_enabled.is_some(), "{id}: RFP_STEPPER_ENABLED");
        // The ICD gives 1.000 to 1.800, but blocked sites exceed it in both
        // channels: KMAF 2023 has H_RNSCALE(2) 1.981 (compared with MetPy) and
        // V_RNSCALE(2) 2.059, KMTX 2024 H_RNSCALE(0) 2.539 and V_RNSCALE(0)
        // 2.233. The sector above 5 deg is 1.000 in both channels everywhere.
        for value in a.v_rnscale {
            in_range(id, "V_RNSCALE", value, 1.0, 2.6);
        }
        assert_eq!(a.v_rnscale[12], 1.0, "{id}");
        assert_eq!(a.h_rnscale[12], 1.0, "{id}");
        in_range(
            id,
            "BASELINE_ZDR_OFFSET",
            a.baseline_zdr_offset,
            -10.0,
            10.0,
        );
        in_range(id, "SUN_BIAS", a.sun_bias, -5.0, 5.0);
        in_range(id, "POWER_METER_ZERO", a.power_meter_zero, -10.0, 10.0);
        in_range(id, "TXB_BASELINE", a.txb_baseline, -1.0, 1.0);
        in_range(id, "TXB_ALARM_THRESH", a.txb_alarm_thresh, 0.0, 5.0);
        in_range(
            id,
            "NORMAL_TPS_POWER_TIME",
            a.normal_tps_power_time,
            0,
            3600,
        );
        if a.dig_rcvr_clock_freq != 0.0 {
            in_range(
                id,
                "DIG_RCVR_CLOCK_FREQ",
                a.dig_rcvr_clock_freq,
                50.0,
                250.0,
            );
            in_range(id, "COHO_FREQ", a.coho_freq, 0.0, 100.0);
        }
    }
    let sources: Vec<Vec<&str>> = ids.iter().map(|id| vec![*id]).collect();
    common::assert_checked_every_available("ICD ranges from Build 19", checked, &sources);
}

// --- halfword positions ---------------------------------------------------------

/// ORDA halfwords (Table IV) with a non-zero value in at least one corpus
/// file: 1-8, 10-15, 19-24, 41 and 60. The others are zero in every file, so
/// their positions are verified only as zero: 9 (control authorization), 16
/// (command acknowledgment), 17 (channel control), 18 (spot blanking), 25
/// (RMS control), 26 (performance check), 27-40 (alarm codes), 42-58 (spare)
/// and 59 (downloaded pattern number).
const ORDA_NONZERO_HALFWORDS: &[usize] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13, 14, 15, 19, 20, 21, 22, 23, 24, 41, 60,
];

/// Legacy halfwords (ICD 2620002B Table IV) with a non-zero value in at least
/// one of the 4 legacy files (KTLX 1991 and 1999, KLIX 2005, KVWX 2008):
/// 1-8, 10-14, 19-22 and 24. Verified only as zero: 9 (control
/// authorization), 15 (alarm summary), 16, 17, 18, 25 (RMS control) and
/// 27-40 (alarm codes).
const LEGACY_NONZERO_HALFWORDS: &[usize] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13, 14, 19, 20, 21, 22, 24,
];

/// Every decoded field against the halfword read from the body bytes at its
/// Table IV position, through the same code mapping. Returns the halfword
/// numbers with a non-zero value in this body.
fn assert_orda_halfwords(id: &str, status: &OrdaRdaStatus, body: &[u8]) -> Vec<usize> {
    let halfwords = body.len() / 2;
    let hw =
        |number: usize| u16::from_be_bytes([body[(number - 1) * 2], body[(number - 1) * 2 + 1]]);
    let hundredths = |number: usize| f32::from(hw(number) as i16) / 100.0;
    assert_eq!(
        status.rda_state,
        RdaState::from_code(hw(1)),
        "{id}: halfword 1"
    );
    assert_eq!(
        status.operability,
        OperabilityStatus::from_code(hw(2)),
        "{id}: halfword 2"
    );
    assert_eq!(
        status.control_status,
        ControlStatus::from_code(hw(3)),
        "{id}: halfword 3"
    );
    assert_eq!(status.auxiliary_power.0, hw(4), "{id}: halfword 4");
    assert_eq!(status.average_transmitter_power, hw(5), "{id}: halfword 5");
    assert_eq!(
        status.horizontal_reflectivity_calibration_correction,
        hundredths(6),
        "{id}: halfword 6"
    );
    assert_eq!(status.data_transmission.0, hw(7), "{id}: halfword 7");
    assert_eq!(
        status.volume_coverage_pattern,
        VcpSelection::from_code(hw(8)),
        "{id}: halfword 8"
    );
    assert_eq!(
        status.control_authorization,
        ControlAuthorization::from_code(hw(9)),
        "{id}: halfword 9"
    );
    assert_eq!(status.rda_build.0, hw(10), "{id}: halfword 10");
    assert_eq!(
        status.operational_mode,
        OperationalMode::from_code(hw(11)),
        "{id}: halfword 11"
    );
    assert_eq!(
        status.super_resolution,
        EnableStatus::from_code(hw(12)),
        "{id}: halfword 12"
    );
    assert_eq!(
        status.clutter_mitigation_decision.0,
        hw(13),
        "{id}: halfword 13"
    );
    assert_eq!(status.scan_data_flags.0, hw(14), "{id}: halfword 14");
    assert_eq!(status.alarm_summary.0, hw(15), "{id}: halfword 15");
    assert_eq!(
        status.command_acknowledgment,
        CommandAcknowledgment::from_code(hw(16)),
        "{id}: halfword 16"
    );
    assert_eq!(
        status.channel_control,
        ChannelControlStatus::from_code(hw(17)),
        "{id}: halfword 17"
    );
    assert_eq!(
        status.spot_blanking,
        SpotBlanking::from_code(hw(18)),
        "{id}: halfword 18"
    );
    assert_eq!(
        (
            status.bypass_map_generation.date,
            status.bypass_map_generation.minutes
        ),
        (hw(19), hw(20)),
        "{id}: halfwords 19-20"
    );
    assert_eq!(
        (
            status.clutter_filter_map_generation.date,
            status.clutter_filter_map_generation.minutes
        ),
        (hw(21), hw(22)),
        "{id}: halfwords 21-22"
    );
    assert_eq!(
        status.vertical_reflectivity_calibration_correction,
        hundredths(23),
        "{id}: halfword 23"
    );
    assert_eq!(
        status.transition_power_source,
        TransitionPowerSource::from_code(hw(24)),
        "{id}: halfword 24"
    );
    assert_eq!(
        status.rms_control,
        RmsControl::from_code(hw(25)),
        "{id}: halfword 25"
    );
    assert_eq!(
        status.performance_check,
        PerformanceCheckStatus::from_code(hw(26)),
        "{id}: halfword 26"
    );
    for slot in 0..14 {
        assert_eq!(
            status.alarm_codes[slot],
            hw(27 + slot),
            "{id}: halfword {}",
            27 + slot
        );
    }
    if halfwords >= 60 {
        assert_eq!(
            status.signal_processing_options.map(|options| options.0),
            Some(hw(41)),
            "{id}: halfword 41"
        );
        assert_eq!(
            status.downloaded_pattern_number,
            Some(hw(59)),
            "{id}: halfword 59"
        );
        assert_eq!(status.status_version, Some(hw(60)), "{id}: halfword 60");
    } else {
        assert_eq!(halfwords, 40, "{id}: body halfwords");
        assert_eq!(status.signal_processing_options, None, "{id}");
        assert_eq!(status.downloaded_pattern_number, None, "{id}");
        assert_eq!(status.status_version, None, "{id}");
    }
    (1..=halfwords).filter(|&number| hw(number) != 0).collect()
}

/// [`assert_orda_halfwords`] for the legacy layout (40 halfwords).
fn assert_legacy_halfwords(id: &str, status: &LegacyRdaStatus, body: &[u8]) -> Vec<usize> {
    assert_eq!(body.len() / 2, 40, "{id}: body halfwords");
    let hw =
        |number: usize| u16::from_be_bytes([body[(number - 1) * 2], body[(number - 1) * 2 + 1]]);
    assert_eq!(
        status.rda_state,
        RdaState::from_code(hw(1)),
        "{id}: halfword 1"
    );
    assert_eq!(
        status.operability,
        OperabilityStatus::from_code(hw(2)),
        "{id}: halfword 2"
    );
    assert_eq!(
        status.control_status,
        ControlStatus::from_code(hw(3)),
        "{id}: halfword 3"
    );
    assert_eq!(status.auxiliary_power.0, hw(4), "{id}: halfword 4");
    assert_eq!(status.average_transmitter_power, hw(5), "{id}: halfword 5");
    assert_eq!(
        status.reflectivity_calibration_correction_raw,
        hw(6) as i16,
        "{id}: halfword 6"
    );
    assert_eq!(status.data_transmission.0, hw(7), "{id}: halfword 7");
    assert_eq!(
        status.volume_coverage_pattern,
        VcpSelection::from_code(hw(8)),
        "{id}: halfword 8"
    );
    assert_eq!(
        status.control_authorization,
        ControlAuthorization::from_code(hw(9)),
        "{id}: halfword 9"
    );
    assert_eq!(
        status.interference_detection_rate,
        hw(10),
        "{id}: halfword 10"
    );
    assert_eq!(
        status.operational_mode,
        OperationalMode::from_code(hw(11)),
        "{id}: halfword 11"
    );
    assert_eq!(
        status.interference_suppression_unit,
        EnableStatus::from_code(hw(12)),
        "{id}: halfword 12"
    );
    assert_eq!(status.archive_ii_status, hw(13), "{id}: halfword 13");
    assert_eq!(
        status.archive_ii_remaining_capacity,
        hw(14),
        "{id}: halfword 14"
    );
    assert_eq!(status.alarm_summary.0, hw(15), "{id}: halfword 15");
    assert_eq!(
        status.command_acknowledgment,
        CommandAcknowledgment::from_code(hw(16)),
        "{id}: halfword 16"
    );
    assert_eq!(
        status.channel_control,
        ChannelControlStatus::from_code(hw(17)),
        "{id}: halfword 17"
    );
    assert_eq!(
        status.spot_blanking,
        SpotBlanking::from_code(hw(18)),
        "{id}: halfword 18"
    );
    assert_eq!(
        (
            status.bypass_map_generation.date,
            status.bypass_map_generation.minutes
        ),
        (hw(19), hw(20)),
        "{id}: halfwords 19-20"
    );
    assert_eq!(
        (
            status.notch_width_map_generation.date,
            status.notch_width_map_generation.minutes
        ),
        (hw(21), hw(22)),
        "{id}: halfwords 21-22"
    );
    assert_eq!(
        status.transition_power_source,
        TransitionPowerSource::from_code(hw(24)),
        "{id}: halfword 24"
    );
    assert_eq!(
        status.rms_control,
        RmsControl::from_code(hw(25)),
        "{id}: halfword 25"
    );
    for slot in 0..14 {
        assert_eq!(
            status.alarm_codes[slot],
            hw(27 + slot),
            "{id}: halfword {}",
            27 + slot
        );
    }
    (1..=40).filter(|&number| hw(number) != 0).collect()
}

/// Halfword positions pinned by real bytes: in every file with a status
/// golden, each decoded field equals the halfword read from the message 2
/// body at its Table IV position (through the same code mapping). A field
/// whose halfword is non-zero in some file is thereby verified at its
/// position; a field that is zero in every file is verified only as zero.
/// The two lists of non-zero halfwords are pinned so that the documentation
/// ("verified only as zero") tracks the corpus.
#[test]
fn halfword_positions_pinned_by_nonzero_corpus_values() {
    let ids: Vec<String> =
        std::fs::read_dir(recast_radar_testdata::testdata_dir().join("level2/golden/status"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter_map(|name| name.strip_suffix(".json").map(str::to_owned))
            .collect();
    assert_eq!(ids.len(), 27, "status goldens");
    let mut checked = 0;
    let mut orda_nonzero = std::collections::BTreeSet::new();
    let mut legacy_nonzero = std::collections::BTreeSet::new();
    let mut legacy_files = 0;
    for id in &ids {
        let Some(raw) = load(id) else { continue };
        checked += 1;
        let meta = metadata(&raw);
        let First::Decoded(header, status) = &meta.status else {
            panic!("{id}: no message 2");
        };
        let body = &meta.bodies[&2];
        assert_eq!(
            header.channels & 8 != 0,
            matches!(status, RdaStatus::Orda(_)),
            "{id}: layout"
        );
        match status {
            RdaStatus::Orda(status) => orda_nonzero.extend(assert_orda_halfwords(id, status, body)),
            RdaStatus::Legacy(status) => {
                legacy_files += 1;
                legacy_nonzero.extend(assert_legacy_halfwords(id, status, body));
            }
        }
    }
    let sources: Vec<Vec<&str>> = ids.iter().map(|id| vec![id.as_str()]).collect();
    common::assert_checked_every_available("halfword positions", checked, &sources);
    let orda_nonzero: Vec<usize> = orda_nonzero.into_iter().collect();
    let legacy_nonzero: Vec<usize> = legacy_nonzero.into_iter().collect();
    if checked == ids.len() {
        assert_eq!(legacy_files, 4);
        assert_eq!(orda_nonzero, ORDA_NONZERO_HALFWORDS);
        assert_eq!(legacy_nonzero, LEGACY_NONZERO_HALFWORDS);
    } else {
        assert!(
            orda_nonzero
                .iter()
                .all(|n| ORDA_NONZERO_HALFWORDS.contains(n)),
            "{orda_nonzero:?}"
        );
        assert!(
            legacy_nonzero
                .iter()
                .all(|n| LEGACY_NONZERO_HALFWORDS.contains(n)),
            "{legacy_nonzero:?}"
        );
    }
}

// --- layouts and walker behaviour ---------------------------------------------

/// The site position in message 18 matches the message 31 volume data block of
/// the same volume, and the site name matches the volume header.
#[test]
fn adaptation_site_matches_volume() {
    const IDS: [&str; 2] = ["l2-ktlx-20240315-000217", "l2-pahg-20250909-212549"];
    let mut checked = 0;
    for id in IDS {
        let Some(raw) = load(id) else { continue };
        checked += 1;
        let meta = metadata(&raw);
        let First::Decoded(_, a) = &meta.adaptation else {
            panic!("{id}: no message 18");
        };
        let volume = recast_radar_io_nexrad::read_volume_from_bytes(&raw).unwrap();
        assert_eq!(a.site_name, volume.attrs.instrument_name, "{id}");
        let latitude = volume.location.latitude_deg.unwrap();
        let longitude = volume.location.longitude_deg.unwrap();
        assert!(
            (a.latitude() - latitude).abs() < 1e-3,
            "{id}: {} vs {latitude}",
            a.latitude()
        );
        assert!(
            (a.longitude() - longitude).abs() < 1e-3,
            "{id}: {} vs {longitude}",
            a.longitude()
        );
        in_range(id, "TFREQ_MHZ", a.tfreq_mhz, 2700, 3000);
        in_range(
            id,
            "IELMIN deg",
            a.manual_setup_min_elevation(),
            -39.99573,
            39.99573,
        );
        in_range(
            id,
            "IELMAX deg",
            a.manual_setup_max_elevation(),
            0.0,
            219.99573,
        );
    }
    let sources: Vec<Vec<&str>> = IDS.iter().map(|id| vec![*id]).collect();
    common::assert_checked_every_available("adaptation site", checked, &sources);
}

/// KLIX 2005-08-29 (legacy RDA, channel byte 0). Message 2 values not read by
/// MetPy under their legacy names, from the metadata record bytes: halfword 10
/// 0000, 11 0004, 12 0004, 13 0001, 14 0000, 21 32e0 (13024), 22 035a (858),
/// 24 0003, 25 0000. Messages 3 and 18 are the legacy 1040- and 9600-byte
/// bodies and are yielded unparsed.
#[test]
fn legacy_rda_2005() {
    let Some(raw) = load("l2-klix-20050829-130035") else {
        return;
    };
    let meta = metadata(&raw);
    let First::Decoded(header, RdaStatus::Legacy(status)) = &meta.status else {
        panic!("expected legacy message 2: {:?}", meta.status);
    };
    assert_eq!(header.channels, 0);
    assert_eq!(status.rda_state, RdaState::Operate);
    assert_eq!(status.operability.state, OperabilityState::OnLine);
    assert_eq!(status.control_status, ControlStatus::RemoteOnly);
    assert_eq!(status.volume_coverage_pattern, VcpSelection::Remote(121));
    assert_eq!(status.interference_detection_rate, 0);
    assert_eq!(status.operational_mode, OperationalMode::Operational);
    assert_eq!(status.interference_suppression_unit, EnableStatus::Disabled);
    assert_eq!(status.archive_ii_status, 1);
    assert_eq!(status.archive_ii_remaining_capacity, 0);
    assert_eq!(status.notch_width_map_generation.date, 13024);
    assert_eq!(status.notch_width_map_generation.minutes, 858);
    assert_eq!(
        status
            .notch_width_map_generation
            .datetime()
            .map(|time| time.to_rfc3339()),
        Some("2005-08-28T14:18:00+00:00".to_owned())
    );
    assert_eq!(status.transition_power_source, TransitionPowerSource::Ok);
    assert_eq!(status.rms_control, RmsControl::NonRms);
    assert!(matches!(meta.performance, First::Unparsed(_, 1040)));
    assert!(matches!(meta.adaptation, First::Unparsed(_, 9600)));
}

/// TDWR files (channel byte 8 or 9) use the ORDA layout; halfword 10 is 0014,
/// build 2.0, and halfword 8 ffb0 is local pattern 80.
#[test]
fn tdwr_status_uses_orda_layout() {
    let Some(raw) = load("l2-tstl-20230331-230314") else {
        return;
    };
    let meta = metadata(&raw);
    let First::Decoded(header, RdaStatus::Orda(status)) = &meta.status else {
        panic!("expected ORDA message 2: {:?}", meta.status);
    };
    assert_eq!(header.channels, 9);
    assert_eq!(status.rda_build.version(), 2.0);
    assert_eq!(status.volume_coverage_pattern, VcpSelection::Local(80));
    assert_eq!(status.signal_processing_options, None);
    assert!(matches!(meta.performance, First::Absent));
    assert!(matches!(meta.adaptation, First::Absent));
}

/// Build 22.0 (KTLX 2024-03-15) through the public walker: halfword 1 0010
/// operate, 8 00d4 VCP 212 remote, 10 0898 build 22.0, 13 003f CMD on in
/// segments 1-5, 14 003a, 15 0000; halfwords 19-20 4d55 059c: bypass map
/// generated on day 19797 (2024-03-14) at minute 1436.
#[test]
fn build_22_status_codes() {
    let Some(raw) = load("l2-ktlx-20240315-000217") else {
        return;
    };
    let meta = metadata(&raw);
    let First::Decoded(_, RdaStatus::Orda(status)) = &meta.status else {
        panic!("expected ORDA message 2");
    };
    assert_eq!(status.rda_state, RdaState::Operate);
    assert_eq!(status.volume_coverage_pattern, VcpSelection::Remote(212));
    assert_eq!(status.rda_build.version(), 22.0);
    assert!(status.clutter_mitigation_decision.enabled());
    assert!((1..=5).all(|segment| {
        status
            .clutter_mitigation_decision
            .applied_in_segment(segment)
    }));
    let flags = status.scan_data_flags;
    assert!(flags.avset_enabled() && !flags.avset_disabled());
    assert!(flags.ebc_enabled() && flags.rda_log_data_enabled() && flags.time_series_recording());
    assert!(status.alarm_summary.no_alarms());
    assert!(status.data_transmission.reflectivity());
    assert!(status.data_transmission.velocity() && status.data_transmission.width());
    assert!(status.auxiliary_power.utility_power_available());
    assert_eq!(
        status
            .bypass_map_generation
            .datetime()
            .map(|t| t.to_rfc3339()),
        Some("2024-03-14T23:56:00+00:00".to_owned())
    );
}

/// Real bodies cut one halfword short are rejected with a truncation error.
#[test]
fn truncated_bodies_are_errors() {
    let Some(raw) = load("l2-ktlx-20240515-000014") else {
        return;
    };
    let meta = metadata(&raw);
    let status = &meta.bodies[&2];
    assert_eq!(status.len(), 120);
    assert!(matches!(
        RdaStatus::decode(8, &status[..78]),
        Err(NexradError::Truncated { .. })
    ));
    assert!(matches!(
        RdaStatus::decode(0, &status[..78]),
        Err(NexradError::Truncated { .. })
    ));
    // A 40-halfword prefix of a 60-halfword body decodes without halfwords 41-60.
    let short = RdaStatus::decode(8, &status[..80]).unwrap();
    let RdaStatus::Orda(short) = short else {
        panic!("layout");
    };
    assert_eq!(short.status_version, None);
    let performance = &meta.bodies[&3];
    assert_eq!(performance.len(), 960);
    assert!(matches!(
        PerformanceMaintenance::decode(&performance[..958]),
        Err(NexradError::Truncated { .. })
    ));
    let adaptation = &meta.bodies[&18];
    assert_eq!(adaptation.len(), 9468);
    assert!(matches!(
        RdaAdaptationData::decode(&adaptation[..9467]),
        Err(NexradError::Truncated { .. })
    ));
}

// --- accessor tables ----------------------------------------------------------

/// Every Table V field as (halfword, value), halfword order; bytes of a
/// halfword are keyed by that halfword.
fn performance_values(p: &PerformanceMaintenance) -> Vec<(usize, Num)> {
    let mut values = vec![
        (2, Num::U16(p.communications.loop_back_test_status)),
        (3, Num::U32(p.communications.t1_output_frames)),
        (5, Num::U32(p.communications.t1_input_frames)),
        (7, Num::U32(p.communications.router_memory_used)),
        (9, Num::U32(p.communications.router_memory_free)),
        (11, Num::U16(p.communications.router_memory_utilization)),
        (12, Num::U16(p.communications.route_to_rpg)),
        (13, Num::U16(p.communications.t1_port_status)),
        (
            14,
            Num::U16(p.communications.router_dedicated_ethernet_port_status),
        ),
        (
            15,
            Num::U16(p.communications.router_commercial_ethernet_port_status),
        ),
        (21, Num::U32(p.communications.csu_24hr_errored_seconds)),
        (
            23,
            Num::U32(p.communications.csu_24hr_severely_errored_seconds),
        ),
        (
            25,
            Num::U32(p.communications.csu_24hr_severely_errored_framing_seconds),
        ),
        (27, Num::U32(p.communications.csu_24hr_unavailable_seconds)),
        (
            29,
            Num::U32(p.communications.csu_24hr_controlled_slip_seconds),
        ),
        (
            31,
            Num::U32(p.communications.csu_24hr_path_coding_violations),
        ),
        (33, Num::U32(p.communications.csu_24hr_line_errored_seconds)),
        (
            35,
            Num::U32(p.communications.csu_24hr_bursty_errored_seconds),
        ),
        (37, Num::U32(p.communications.csu_24hr_degraded_minutes)),
        (41, Num::U32(p.communications.lan_switch_cpu_utilization)),
        (43, Num::U16(p.communications.lan_switch_memory_utilization)),
        (45, Num::I16(p.communications.ifdr_chassis_temperature)),
        (46, Num::I16(p.communications.ifdr_fpga_temperature)),
        (47, Num::U16(p.communications.ntp_status)),
        (53, Num::U16(p.communications.ipc_status)),
        (54, Num::U16(p.communications.commanded_channel_control)),
        (58, Num::U16(p.ame.polarization)),
        (59, Num::F32(p.ame.internal_temperature)),
        (61, Num::F32(p.ame.receiver_module_temperature)),
        (63, Num::F32(p.ame.bite_cal_module_temperature)),
        (65, Num::U16(p.ame.peltier_pulse_width_modulation)),
        (66, Num::U16(p.ame.peltier_status)),
        (67, Num::U16(p.ame.ad_converter_status)),
        (68, Num::U16(p.ame.state)),
        (69, Num::F32(p.ame.ps_3_3v_voltage)),
        (71, Num::F32(p.ame.ps_5v_voltage)),
        (73, Num::F32(p.ame.ps_6_5v_voltage)),
        (75, Num::F32(p.ame.ps_15v_voltage)),
        (77, Num::F32(p.ame.ps_48v_voltage)),
        (79, Num::F32(p.ame.stalo_power)),
        (81, Num::F32(p.ame.peltier_current)),
        (83, Num::F32(p.ame.adc_calibration_reference_voltage)),
        (85, Num::U16(p.ame.mode)),
        (86, Num::U16(p.ame.peltier_mode)),
        (87, Num::F32(p.ame.peltier_inside_fan_current)),
        (89, Num::F32(p.ame.peltier_outside_fan_current)),
        (91, Num::F32(p.ame.horizontal_tr_limiter_voltage)),
        (93, Num::F32(p.ame.vertical_tr_limiter_voltage)),
        (95, Num::F32(p.ame.adc_calibration_offset_voltage)),
        (97, Num::F32(p.ame.adc_calibration_gain_correction)),
        (99, Num::U16(p.rcp_spip.rcp_status)),
        (100, Num::Str(p.rcp_spip.rcp_string.clone())),
        (108, Num::U16(p.rcp_spip.spip_power_buttons)),
        (111, Num::F32(p.power.master_power_administrator_load)),
        (113, Num::F32(p.power.expansion_power_administrator_load)),
        (137, Num::U16(p.transmitter.ps_5vdc)),
        (138, Num::U16(p.transmitter.ps_15vdc)),
        (139, Num::U16(p.transmitter.ps_28vdc)),
        (140, Num::U16(p.transmitter.ps_neg_15vdc)),
        (141, Num::U16(p.transmitter.ps_45vdc)),
        (142, Num::U16(p.transmitter.filament_ps_voltage)),
        (143, Num::U16(p.transmitter.vacuum_pump_ps_voltage)),
        (144, Num::U16(p.transmitter.focus_coil_ps_voltage)),
        (145, Num::U16(p.transmitter.filament_ps)),
        (146, Num::U16(p.transmitter.klystron_warmup)),
        (147, Num::U16(p.transmitter.transmitter_available)),
        (148, Num::U16(p.transmitter.wg_switch_position)),
        (149, Num::U16(p.transmitter.wg_pfn_transfer_interlock)),
        (150, Num::U16(p.transmitter.maintenance_mode)),
        (151, Num::U16(p.transmitter.maintenance_required)),
        (152, Num::U16(p.transmitter.pfn_switch_position)),
        (153, Num::U16(p.transmitter.modulator_overload)),
        (154, Num::U16(p.transmitter.modulator_inv_current)),
        (155, Num::U16(p.transmitter.modulator_switch_fail)),
        (156, Num::U16(p.transmitter.main_power_voltage)),
        (157, Num::U16(p.transmitter.charging_system_fail)),
        (158, Num::U16(p.transmitter.inverse_diode_current)),
        (159, Num::U16(p.transmitter.trigger_amplifier)),
        (160, Num::U16(p.transmitter.circulator_temperature)),
        (161, Num::U16(p.transmitter.spectrum_filter_pressure)),
        (162, Num::U16(p.transmitter.wg_arc_vswr)),
        (163, Num::U16(p.transmitter.cabinet_interlock)),
        (164, Num::U16(p.transmitter.cabinet_air_temperature)),
        (165, Num::U16(p.transmitter.cabinet_airflow)),
        (166, Num::U16(p.transmitter.klystron_current)),
        (167, Num::U16(p.transmitter.klystron_filament_current)),
        (168, Num::U16(p.transmitter.klystron_vacion_current)),
        (169, Num::U16(p.transmitter.klystron_air_temperature)),
        (170, Num::U16(p.transmitter.klystron_airflow)),
        (171, Num::U16(p.transmitter.modulator_switch_maintenance)),
        (
            172,
            Num::U16(p.transmitter.post_charge_regulator_maintenance),
        ),
        (173, Num::U16(p.transmitter.wg_pressure_humidity)),
        (174, Num::U16(p.transmitter.transmitter_overvoltage)),
        (175, Num::U16(p.transmitter.transmitter_overcurrent)),
        (176, Num::U16(p.transmitter.focus_coil_current)),
        (177, Num::U16(p.transmitter.focus_coil_airflow)),
        (178, Num::U16(p.transmitter.oil_temperature)),
        (179, Num::U16(p.transmitter.prf_limit)),
        (180, Num::U16(p.transmitter.transmitter_oil_level)),
        (181, Num::U16(p.transmitter.transmitter_battery_charging)),
        (182, Num::U16(p.transmitter.high_voltage_status)),
        (183, Num::U16(p.transmitter.transmitter_recycling_summary)),
        (184, Num::U16(p.transmitter.transmitter_inoperable)),
        (185, Num::U16(p.transmitter.transmitter_air_filter)),
        (202, Num::U16(p.transmitter.xmtr_spip_interface)),
        (203, Num::U16(p.transmitter.transmitter_summary_status)),
        (205, Num::F32(p.transmitter.transmitter_rf_power)),
        (207, Num::F32(p.transmitter.horizontal_xmtr_peak_power)),
        (209, Num::F32(p.transmitter.xmtr_peak_power)),
        (211, Num::F32(p.transmitter.vertical_xmtr_peak_power)),
        (213, Num::F32(p.transmitter.xmtr_rf_avg_power)),
        (217, Num::U32(p.transmitter.xmtr_recycle_count)),
        (219, Num::F32(p.transmitter.receiver_bias)),
        (221, Num::F32(p.transmitter.transmit_imbalance)),
        (223, Num::F32(p.transmitter.xmtr_power_meter_zero)),
        (
            229,
            Num::U16(p.tower_utilities.ac_unit_1_compressor_shut_off),
        ),
        (
            230,
            Num::U16(p.tower_utilities.ac_unit_2_compressor_shut_off),
        ),
        (
            231,
            Num::U16(p.tower_utilities.generator_maintenance_required),
        ),
        (232, Num::U16(p.tower_utilities.generator_battery_voltage)),
        (233, Num::U16(p.tower_utilities.generator_engine)),
        (234, Num::U16(p.tower_utilities.generator_volt_frequency)),
        (235, Num::U16(p.tower_utilities.power_source)),
        (236, Num::U16(p.tower_utilities.transitional_power_source)),
        (
            237,
            Num::U16(p.tower_utilities.generator_auto_run_off_switch),
        ),
        (238, Num::U16(p.tower_utilities.aircraft_hazard_lighting)),
        (250, Num::U16(p.equipment_shelter.fire_detection_system)),
        (
            251,
            Num::U16(p.equipment_shelter.equipment_shelter_fire_smoke),
        ),
        (
            252,
            Num::U16(p.equipment_shelter.generator_shelter_fire_smoke),
        ),
        (253, Num::U16(p.equipment_shelter.utility_voltage_frequency)),
        (254, Num::U16(p.equipment_shelter.site_security_alarm)),
        (255, Num::U16(p.equipment_shelter.security_equipment)),
        (256, Num::U16(p.equipment_shelter.security_system)),
        (
            257,
            Num::U16(p.equipment_shelter.receiver_connected_to_antenna),
        ),
        (258, Num::U16(p.equipment_shelter.radome_hatch)),
        (259, Num::U16(p.equipment_shelter.ac_unit_1_filter_dirty)),
        (260, Num::U16(p.equipment_shelter.ac_unit_2_filter_dirty)),
        (
            261,
            Num::F32(p.equipment_shelter.equipment_shelter_temperature),
        ),
        (
            263,
            Num::F32(p.equipment_shelter.outside_ambient_temperature),
        ),
        (
            265,
            Num::F32(p.equipment_shelter.transmitter_leaving_air_temperature),
        ),
        (
            267,
            Num::F32(p.equipment_shelter.ac_unit_1_discharge_air_temperature),
        ),
        (
            269,
            Num::F32(p.equipment_shelter.generator_shelter_temperature),
        ),
        (271, Num::F32(p.equipment_shelter.radome_air_temperature)),
        (
            273,
            Num::F32(p.equipment_shelter.ac_unit_2_discharge_air_temperature),
        ),
        (275, Num::F32(p.equipment_shelter.spip_15v_ps)),
        (277, Num::F32(p.equipment_shelter.spip_neg_15v_ps)),
        (279, Num::U16(p.equipment_shelter.spip_28v_ps_status)),
        (281, Num::F32(p.equipment_shelter.spip_5v_ps)),
        (
            283,
            Num::U16(p.equipment_shelter.converted_generator_fuel_level),
        ),
        (300, Num::U16(p.antenna_pedestal.elevation_pos_dead_limit)),
        (301, Num::U16(p.antenna_pedestal.pos_150v_overvoltage)),
        (302, Num::U16(p.antenna_pedestal.pos_150v_undervoltage)),
        (
            303,
            Num::U16(p.antenna_pedestal.elevation_servo_amp_inhibit),
        ),
        (
            304,
            Num::U16(p.antenna_pedestal.elevation_servo_amp_short_circuit),
        ),
        (
            305,
            Num::U16(p.antenna_pedestal.elevation_servo_amp_overtemp),
        ),
        (306, Num::U16(p.antenna_pedestal.elevation_motor_overtemp)),
        (307, Num::U16(p.antenna_pedestal.elevation_stow_pin)),
        (308, Num::U16(p.antenna_pedestal.elevation_housing_5v_ps)),
        (309, Num::U16(p.antenna_pedestal.elevation_neg_dead_limit)),
        (310, Num::U16(p.antenna_pedestal.elevation_pos_normal_limit)),
        (311, Num::U16(p.antenna_pedestal.elevation_neg_normal_limit)),
        (312, Num::U16(p.antenna_pedestal.elevation_encoder_light)),
        (313, Num::U16(p.antenna_pedestal.elevation_gearbox_oil)),
        (314, Num::U16(p.antenna_pedestal.elevation_handwheel)),
        (315, Num::U16(p.antenna_pedestal.elevation_amp_ps)),
        (316, Num::U16(p.antenna_pedestal.azimuth_servo_amp_inhibit)),
        (
            317,
            Num::U16(p.antenna_pedestal.azimuth_servo_amp_short_circuit),
        ),
        (318, Num::U16(p.antenna_pedestal.azimuth_servo_amp_overtemp)),
        (319, Num::U16(p.antenna_pedestal.azimuth_motor_overtemp)),
        (320, Num::U16(p.antenna_pedestal.azimuth_stow_pin)),
        (321, Num::U16(p.antenna_pedestal.azimuth_housing_5v_ps)),
        (322, Num::U16(p.antenna_pedestal.azimuth_encoder_light)),
        (323, Num::U16(p.antenna_pedestal.azimuth_gearbox_oil)),
        (324, Num::U16(p.antenna_pedestal.azimuth_bull_gear_oil)),
        (325, Num::U16(p.antenna_pedestal.azimuth_handwheel)),
        (326, Num::U16(p.antenna_pedestal.azimuth_servo_amp_ps)),
        (327, Num::U16(p.antenna_pedestal.servo)),
        (328, Num::U16(p.antenna_pedestal.pedestal_interlock_switch)),
        (341, Num::U16(p.rf_generator_receiver.coho_clock)),
        (
            342,
            Num::U16(p.rf_generator_receiver.frequency_select_oscillator),
        ),
        (343, Num::U16(p.rf_generator_receiver.rf_stalo)),
        (344, Num::U16(p.rf_generator_receiver.phase_shifted_coho)),
        (345, Num::U16(p.rf_generator_receiver.receiver_ps_9v)),
        (346, Num::U16(p.rf_generator_receiver.receiver_ps_5v)),
        (347, Num::U16(p.rf_generator_receiver.receiver_ps_18v)),
        (348, Num::U16(p.rf_generator_receiver.receiver_ps_neg_9v)),
        (349, Num::U16(p.rf_generator_receiver.rdaiu_ps_5v)),
        (
            351,
            Num::F32(p.rf_generator_receiver.horizontal_short_pulse_noise),
        ),
        (
            353,
            Num::F32(p.rf_generator_receiver.horizontal_long_pulse_noise),
        ),
        (
            355,
            Num::F32(p.rf_generator_receiver.horizontal_noise_temperature),
        ),
        (
            357,
            Num::F32(p.rf_generator_receiver.vertical_short_pulse_noise),
        ),
        (
            359,
            Num::F32(p.rf_generator_receiver.vertical_long_pulse_noise),
        ),
        (
            361,
            Num::F32(p.rf_generator_receiver.vertical_noise_temperature),
        ),
        (363, Num::F32(p.calibration.horizontal_linearity)),
        (365, Num::F32(p.calibration.horizontal_dynamic_range)),
        (367, Num::F32(p.calibration.horizontal_delta_dbz0)),
        (369, Num::F32(p.calibration.vertical_delta_dbz0)),
        (371, Num::F32(p.calibration.kd_peak_measured)),
        (375, Num::F32(p.calibration.short_pulse_horizontal_dbz0)),
        (377, Num::F32(p.calibration.long_pulse_horizontal_dbz0)),
        (379, Num::U16(p.calibration.velocity_processed)),
        (380, Num::U16(p.calibration.width_processed)),
        (381, Num::U16(p.calibration.velocity_rf_gen)),
        (382, Num::U16(p.calibration.width_rf_gen)),
        (383, Num::F32(p.calibration.horizontal_i0)),
        (385, Num::F32(p.calibration.vertical_i0)),
        (387, Num::F32(p.calibration.vertical_dynamic_range)),
        (389, Num::F32(p.calibration.short_pulse_vertical_dbz0)),
        (391, Num::F32(p.calibration.long_pulse_vertical_dbz0)),
        (397, Num::F32(p.calibration.horizontal_power_sense)),
        (399, Num::F32(p.calibration.vertical_power_sense)),
        (401, Num::F32(p.calibration.zdr_offset)),
        (409, Num::F32(p.calibration.clutter_suppression_delta)),
        (
            411,
            Num::F32(p.calibration.clutter_suppression_unfiltered_power),
        ),
        (
            413,
            Num::F32(p.calibration.clutter_suppression_filtered_power),
        ),
        (425, Num::F32(p.calibration.vertical_linearity)),
        (431, Num::U16(p.file_status.state_file_read)),
        (432, Num::U16(p.file_status.state_file_write)),
        (433, Num::U16(p.file_status.bypass_map_file_read)),
        (434, Num::U16(p.file_status.bypass_map_file_write)),
        (437, Num::U16(p.file_status.current_adaptation_file_read)),
        (438, Num::U16(p.file_status.current_adaptation_file_write)),
        (439, Num::U16(p.file_status.censor_zone_file_read)),
        (440, Num::U16(p.file_status.censor_zone_file_write)),
        (441, Num::U16(p.file_status.remote_vcp_file_read)),
        (442, Num::U16(p.file_status.remote_vcp_file_write)),
        (443, Num::U16(p.file_status.baseline_adaptation_file_read)),
        (444, Num::U16(p.file_status.prf_sets_read)),
        (445, Num::U16(p.file_status.clutter_filter_map_file_read)),
        (446, Num::U16(p.file_status.clutter_filter_map_file_write)),
        (447, Num::U16(p.file_status.general_disk_io_error)),
        (448, Num::U8(p.file_status.rsp_status)),
        (449, Num::U8(p.file_status.rsp_cpu1_temperature)),
        (450, Num::U16(p.file_status.rsp_motherboard_power)),
        (461, Num::U16(p.device_status.spip_comm_status)),
        (462, Num::U16(p.device_status.hci_comm_status)),
        (
            464,
            Num::U16(p.device_status.signal_processor_command_status),
        ),
        (465, Num::U16(p.device_status.ame_communication_status)),
        (466, Num::U16(p.device_status.rms_link_status)),
        (467, Num::U16(p.device_status.rpg_link_status)),
        (468, Num::U16(p.device_status.interpanel_link_status)),
        (469, Num::U32(p.device_status.performance_check_time)),
        (480, Num::U16(p.version)),
    ];
    values.extend(
        p.transmitter
            .zero_test_bits
            .iter()
            .enumerate()
            .map(|(bit, value)| (186 + bit, Num::U16(*value))),
    );
    values.extend(
        p.transmitter
            .one_test_bits
            .iter()
            .enumerate()
            .map(|(bit, value)| (194 + bit, Num::U16(*value))),
    );
    values.sort_by_key(|(halfword, _)| *halfword);
    values
}

/// Every Table XV field as (byte offset, value); array elements are listed
/// at their own offsets.
fn adaptation_values(a: &RdaAdaptationData) -> Vec<(usize, Num)> {
    let mut values = vec![
        (0, Num::Str(a.adap_file_name.clone())),
        (12, Num::Str(a.adap_format.clone())),
        (16, Num::Str(a.adap_revision.clone())),
        (20, Num::Str(a.adap_date.clone())),
        (32, Num::Str(a.adap_time.clone())),
        (44, Num::F32(a.lower_pre_limit)),
        (48, Num::F32(a.az_lat)),
        (52, Num::F32(a.upper_pre_limit)),
        (56, Num::F32(a.el_lat)),
        (60, Num::F32(a.parkaz)),
        (64, Num::F32(a.parkel)),
        (112, Num::F32(a.a_min_shelter_temp)),
        (116, Num::F32(a.a_max_shelter_temp)),
        (120, Num::F32(a.a_min_shelter_ac_temp_diff)),
        (124, Num::F32(a.a_max_xmtr_air_temp)),
        (128, Num::F32(a.a_max_rad_temp)),
        (132, Num::F32(a.a_max_rad_temp_rise)),
        (136, Num::F32(a.lower_dead_limit)),
        (140, Num::F32(a.upper_dead_limit)),
        (148, Num::F32(a.a_min_gen_room_temp)),
        (152, Num::F32(a.a_max_gen_room_temp)),
        (156, Num::F32(a.spip_5v_reg_lim)),
        (160, Num::F32(a.spip_15v_reg_lim)),
        (176, Num::Flag(a.rpg_co_located)),
        (180, Num::Flag(a.spec_filter_installed)),
        (184, Num::Flag(a.tps_installed)),
        (188, Num::Flag(a.rms_installed)),
        (192, Num::I32(a.a_hvdl_tst_int)),
        (196, Num::I32(a.a_rpg_lt_int)),
        (200, Num::I32(a.a_min_stab_util_pwr_time)),
        (204, Num::I32(a.a_gen_auto_exer_interval)),
        (208, Num::I32(a.a_util_pwr_sw_req_interval)),
        (212, Num::F32(a.a_low_fuel_level)),
        (216, Num::I32(a.config_chan_number)),
        (224, Num::I32(a.redundant_chan_config)),
        (668, Num::F32(a.path_losses_7)),
        (692, Num::F32(a.path_losses_13)),
        (752, Num::F32(a.path_losses_28)),
        (756, Num::F32(a.h_coupler_xmt_loss)),
        (768, Num::F32(a.path_losses_32)),
        (772, Num::F32(a.path_losses_33)),
        (780, Num::F32(a.path_losses_35)),
        (796, Num::F32(a.path_losses_39)),
        (800, Num::F32(a.path_losses_40)),
        (808, Num::F32(a.path_losses_42)),
        (812, Num::F32(a.path_losses_43)),
        (816, Num::F32(a.path_losses_44)),
        (820, Num::F32(a.path_losses_45)),
        (824, Num::F32(a.path_losses_46)),
        (828, Num::F32(a.path_losses_47)),
        (832, Num::F32(a.h_coupler_cw_loss)),
        (836, Num::F32(a.v_coupler_xmt_loss)),
        (844, Num::F32(a.ame_ts_bias)),
        (848, Num::F32(a.path_losses_52)),
        (852, Num::F32(a.v_coupler_cw_loss)),
        (864, Num::F32(a.pwr_sense_bias)),
        (868, Num::F32(a.ame_v_noise_enr)),
        (872, Num::F32(a.path_losses_58)),
        (876, Num::F32(a.path_losses_59)),
        (880, Num::F32(a.path_losses_60)),
        (884, Num::F32(a.path_losses_61)),
        (892, Num::F32(a.path_losses_63)),
        (896, Num::F32(a.path_losses_64)),
        (900, Num::F32(a.path_losses_65)),
        (904, Num::F32(a.path_losses_66)),
        (908, Num::F32(a.path_losses_67)),
        (912, Num::F32(a.path_losses_68)),
        (920, Num::F32(a.chan_cal_diff)),
        (936, Num::F32(a.v_ts_cw)),
        (1092, Num::I32(a.tfreq_mhz)),
        (1096, Num::F32(a.base_data_tcn)),
        (1100, Num::F32(a.refl_data_tover)),
        (1104, Num::F32(a.tar_h_dbz0_lp)),
        (1108, Num::F32(a.tar_v_dbz0_lp)),
        (1112, Num::I32(a.init_phi_dp)),
        (1116, Num::I32(a.norm_init_phi_dp)),
        (1120, Num::F32(a.lx_lp)),
        (1124, Num::F32(a.lx_sp)),
        (1128, Num::F32(a.meteor_param)),
        (1136, Num::F32(a.antenna_gain)),
        (1152, Num::F32(a.vel_degrad_limit)),
        (1156, Num::F32(a.wth_degrad_limit)),
        (1160, Num::F32(a.h_noisetemp_dgrad_limit)),
        (1164, Num::I32(a.h_min_noisetemp)),
        (1168, Num::F32(a.v_noisetemp_dgrad_limit)),
        (1172, Num::I32(a.v_min_noisetemp)),
        (1176, Num::F32(a.kly_degrade_limit)),
        (1180, Num::F32(a.ts_coho)),
        (1184, Num::F32(a.h_ts_cw)),
        (1196, Num::F32(a.ts_stalo)),
        (1200, Num::F32(a.ame_h_noise_enr)),
        (1204, Num::F32(a.xmtr_peak_pwr_high_limit)),
        (1208, Num::F32(a.xmtr_peak_pwr_low_limit)),
        (1212, Num::F32(a.h_dbz0_delta_limit)),
        (1216, Num::F32(a.threshold1)),
        (1220, Num::F32(a.threshold2)),
        (1224, Num::F32(a.clut_supp_dgrad_lim)),
        (1232, Num::F32(a.range0_value)),
        (1236, Num::F32(a.xmtr_pwr_mtr_scale)),
        (1240, Num::F32(a.v_dbz0_delta_limit)),
        (1244, Num::F32(a.tar_h_dbz0_sp)),
        (1248, Num::F32(a.tar_v_dbz0_sp)),
        (1252, Num::I32(a.deltaprf)),
        (1264, Num::I32(a.tau_sp)),
        (1268, Num::I32(a.tau_lp)),
        (1272, Num::I32(a.nc_dead_value)),
        (1276, Num::I32(a.tau_rf_sp)),
        (1280, Num::I32(a.tau_rf_lp)),
        (1284, Num::F32(a.seg1lim)),
        (1288, Num::F32(a.slatsec)),
        (1292, Num::F32(a.slonsec)),
        (1300, Num::I32(a.slatdeg)),
        (1304, Num::I32(a.slatmin)),
        (1308, Num::I32(a.slondeg)),
        (1312, Num::I32(a.slonmin)),
        (1316, Num::Str(a.slatdir.clone())),
        (1320, Num::Str(a.slondir.clone())),
        (2500, Num::F64(a.dig_rcvr_clock_freq)),
        (2508, Num::F64(a.coho_freq)),
        (8360, Num::F32(a.az_correction_factor)),
        (8364, Num::F32(a.el_correction_factor)),
        (8368, Num::Str(a.site_name.clone())),
        (8372, Num::I32(a.ant_manual_setup_ielmin)),
        (8376, Num::I32(a.ant_manual_setup_ielmax)),
        (8380, Num::I32(a.ant_manual_setup_fazvelmax)),
        (8384, Num::I32(a.ant_manual_setup_felvelmax)),
        (8388, Num::I32(a.ant_manual_setup_ignd_hgt)),
        (8392, Num::I32(a.ant_manual_setup_irad_hgt)),
        (8396, Num::F32(a.az_pos_sustain_drive)),
        (8400, Num::F32(a.az_neg_sustain_drive)),
        (8404, Num::F32(a.az_nom_pos_drive_slope)),
        (8408, Num::F32(a.az_nom_neg_drive_slope)),
        (8412, Num::F32(a.az_feedback_slope)),
        (8416, Num::F32(a.el_pos_sustain_drive)),
        (8420, Num::F32(a.el_neg_sustain_drive)),
        (8424, Num::F32(a.el_nom_pos_drive_slope)),
        (8428, Num::F32(a.el_nom_neg_drive_slope)),
        (8432, Num::F32(a.el_feedback_slope)),
        (8436, Num::F32(a.el_first_slope)),
        (8440, Num::F32(a.el_second_slope)),
        (8444, Num::F32(a.el_third_slope)),
        (8448, Num::F32(a.el_droop_pos)),
        (8452, Num::F32(a.el_off_neutral_drive)),
        (8456, Num::F32(a.az_inertia)),
        (8460, Num::F32(a.el_inertia)),
        (8496, Num::F32(a.az_stow_angle)),
        (8500, Num::F32(a.el_stow_angle)),
        (8504, Num::F32(a.az_encoder_alignment)),
        (8508, Num::F32(a.el_encoder_alignment)),
        (8688, Num::Flag(a.refined_park)),
        (8696, Num::I32(a.rvp8nv_iwaveguide_length)),
        (8744, Num::F32(a.vel_data_tover)),
        (8748, Num::F32(a.width_data_tover)),
        (8764, Num::F32(a.doppler_range_start)),
        (8768, Num::I32(a.max_el_index)),
        (8772, Num::F32(a.seg2lim)),
        (8776, Num::F32(a.seg3lim)),
        (8780, Num::F32(a.seg4lim)),
        (8784, Num::I32(a.nbr_el_segments)),
        (8788, Num::F32(a.h_noise_long)),
        (8792, Num::F32(a.ant_noise_temp)),
        (8796, Num::F32(a.h_noise_short)),
        (8800, Num::F32(a.h_noise_tolerance)),
        (8804, Num::F32(a.min_h_dyn_range)),
        (8808, Num::Flag(a.gen_installed)),
        (8812, Num::Flag(a.gen_exercise)),
        (8816, Num::F32(a.v_noise_tolerance)),
        (8820, Num::F32(a.min_v_dyn_range)),
        (8824, Num::F32(a.zdr_offset_dgrad_lim)),
        (8828, Num::F32(a.baseline_zdr_offset)),
        (8844, Num::F32(a.v_noise_long)),
        (8848, Num::F32(a.v_noise_short)),
        (8852, Num::F32(a.zdr_data_tover)),
        (8856, Num::F32(a.phi_data_tover)),
        (8860, Num::F32(a.rho_data_tover)),
        (8864, Num::F32(a.stalo_power_dgrad_limit)),
        (8868, Num::F32(a.stalo_power_maint_limit)),
        (8872, Num::F32(a.min_h_pwr_sense)),
        (8876, Num::F32(a.min_v_pwr_sense)),
        (8880, Num::F32(a.h_pwr_sense_offset)),
        (8884, Num::F32(a.v_pwr_sense_offset)),
        (8888, Num::F32(a.ps_gain_ref)),
        (8892, Num::F32(a.rf_pallet_broad_loss)),
        (8960, Num::F32(a.ame_ps_tolerance)),
        (8964, Num::F32(a.ame_max_temp)),
        (8968, Num::F32(a.ame_min_temp)),
        (8972, Num::F32(a.rcvr_mod_max_temp)),
        (8976, Num::F32(a.rcvr_mod_min_temp)),
        (8980, Num::F32(a.bite_mod_max_temp)),
        (8984, Num::F32(a.bite_mod_min_temp)),
        (8988, Num::I32(a.default_polarization)),
        (8992, Num::F32(a.tr_limit_dgrad_limit)),
        (8996, Num::F32(a.tr_limit_fail_limit)),
        (9000, Num::Flag(a.rfp_stepper_enabled)),
        (9008, Num::F32(a.ame_current_tolerance)),
        (9012, Num::I32(a.h_only_polarization)),
        (9016, Num::I32(a.v_only_polarization)),
        (9028, Num::F32(a.sun_bias)),
        (9032, Num::F32(a.a_min_shelter_temp_warn)),
        (9036, Num::F32(a.power_meter_zero)),
        (9040, Num::F32(a.txb_baseline)),
        (9044, Num::F32(a.txb_alarm_thresh)),
        (9048, Num::I32(a.normal_tps_power_time)),
    ];
    values.extend(
        a.a_fuel_conv
            .iter()
            .enumerate()
            .map(|(index, value)| (68 + 4 * index, Num::F32(*value))),
    );
    values.extend(
        a.atten_table
            .iter()
            .enumerate()
            .map(|(index, value)| (228 + 4 * index, Num::F32(*value))),
    );
    values.extend(
        a.h_rnscale
            .iter()
            .enumerate()
            .map(|(index, value)| (940 + 4 * index, Num::F32(*value))),
    );
    values.extend(
        a.atmos
            .iter()
            .enumerate()
            .map(|(index, value)| (992 + 4 * index, Num::F32(*value))),
    );
    values.extend(
        a.el_index
            .iter()
            .enumerate()
            .map(|(index, value)| (1044 + 4 * index, Num::F32(*value))),
    );
    values.extend(a.v_rnscale.iter().enumerate().map(|(index, value)| {
        let offset = if index < 11 {
            8700 + 4 * index
        } else {
            8752 + 4 * (index - 11)
        };
        (offset, Num::F32(*value))
    }));
    values.sort_by_key(|(offset, _)| *offset);
    values
}
