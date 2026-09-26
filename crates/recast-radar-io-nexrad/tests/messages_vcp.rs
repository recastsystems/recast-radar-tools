//! Messages 5 and 7 (Volume Coverage Pattern, ICD 2620002AA Table XI) and
//! message 32 (RDA PRF Data, Table XVIII) on real files.
//!
//! Message 5: every field is compared with MetPy 1.7.1's
//! `Level2File.vcp_info`, stored per file in
//! `testdata/level2/golden/vcp/<id>.json` by `tools/level2_golden.py vcp`
//! (22 metadata records from Build 10.0 to 24.1, one TDWR, one real-time
//! start chunk, and the zero-filled 2005 message MetPy skips). MetPy's
//! decoded names are mapped back to codes by the script, so enum and bit
//! fields are compared as codes.
//!
//! Message 32: MetPy, Py-ART and xradar do not decode it. The expected
//! values are read from the message bytes, quoted in the tests. The PRFs
//! that messages 5 and 32 select for each sweep are also checked against the
//! unambiguous range MetPy reads from the Message 31 radial blocks of the
//! same volume (`message_31_sweeps` in the golden files): the unambiguous
//! range is c / (2 PRF).
//!
//! Message 7 has no real sample (the RPG sends it to the RDA); one test
//! relabels a real message 5 frame as type 7 to check that the walker routes
//! it to the same decoder.

// A panic is how a test fails (clippy.toml), in helpers too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;

use recast_radar_io_nexrad::NexradError;
use recast_radar_io_nexrad::messages::prf::RdaPrfData;
use recast_radar_io_nexrad::messages::vcp::{
    ChannelConfiguration, DopplerVelocityResolution, PatternType, PulseWidth,
    VolumeCoveragePattern, WaveformType,
};
use recast_radar_io_nexrad::messages::{self, MessageBody, MessageWalker, RawMessages};
use serde_json::Value;

const START_CHUNK: &str = "l2chunk-kiwa-307-20260917-003629-001-s";

/// Speed of light in m/s, for unambiguous range c / (2 PRF).
const SPEED_OF_LIGHT_M_S: f64 = 299_792_458.0;

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

fn golden_dir() -> PathBuf {
    recast_radar_testdata::testdata_dir().join("level2/golden/vcp")
}

fn golden(id: &str) -> Value {
    let path = golden_dir().join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn reason(error: &NexradError) -> String {
    match error {
        NexradError::InvalidMessage { reason, .. } => reason.clone(),
        other => other.to_string(),
    }
}

/// Decoded messages 5 and 32 of a metadata record, plus the reasons of any
/// message errors naming type 5 or 32.
struct Metadata {
    vcp: Vec<VolumeCoveragePattern>,
    prf: Vec<RdaPrfData>,
    errors: Vec<String>,
}

fn decode_metadata(record: &[u8]) -> Metadata {
    let mut metadata = Metadata {
        vcp: Vec::new(),
        prf: Vec::new(),
        errors: Vec::new(),
    };
    for item in MessageWalker::new(record) {
        match item {
            Ok((_, MessageBody::Vcp(vcp))) => metadata.vcp.push(vcp),
            Ok((_, MessageBody::Prf(prf))) => metadata.prf.push(prf),
            Ok((header, MessageBody::Unparsed(_))) => assert!(
                !matches!(header.message_type, 5 | 7 | 32),
                "message type {} left unparsed",
                header.message_type
            ),
            Ok(_) => {}
            Err(error) => {
                let reason = reason(&error);
                if reason.starts_with("message type 5:") || reason.starts_with("message type 32:") {
                    metadata.errors.push(reason);
                }
            }
        }
    }
    metadata
}

fn metadata_of(id: &str) -> Option<Metadata> {
    let raw = load(id)?;
    let record = messages::metadata_record(&raw).unwrap();
    Some(decode_metadata(&record))
}

fn int(value: &Value, key: &str) -> u64 {
    value[key]
        .as_u64()
        .unwrap_or_else(|| panic!("golden `{key}` is not an unsigned integer: {value}"))
}

fn float(value: &Value, key: &str) -> f64 {
    value[key]
        .as_f64()
        .unwrap_or_else(|| panic!("golden `{key}` is not a number: {value}"))
}

/// A Table III-A elevation angle as the decoder reports it: codes above 90
/// degrees are negative.
fn signed_elevation(angle: f64) -> f64 {
    if angle > 90.0 { angle - 360.0 } else { angle }
}

// ---------------------------------------------------------------------------
// Message 5 against MetPy

fn check_vcp_golden(id: &str) {
    let Some(metadata) = metadata_of(id) else {
        return;
    };
    let golden = golden(id);
    assert_eq!(golden["id"], id);
    let expected = &golden["message_5"];
    if expected.is_null() {
        // MetPy skips a message 5 whose size halfword is 0; the decoder
        // reports it.
        assert!(metadata.vcp.is_empty(), "{id}: decoded a VCP MetPy skipped");
        assert_eq!(
            metadata.errors,
            [
                "message type 5: invalid message at offset 0: VCP message size is 0 (the message \
                 holds no pattern)"
            ],
            "{id}"
        );
        return;
    }
    assert!(metadata.errors.is_empty(), "{id}: {:?}", metadata.errors);
    let [vcp] = metadata.vcp.as_slice() else {
        panic!("{id}: expected one message 5, got {}", metadata.vcp.len());
    };

    assert_eq!(
        u64::from(vcp.message_size),
        int(expected, "size_hw"),
        "{id}"
    );
    assert_eq!(int(expected, "pattern_type"), 2, "{id}");
    assert_eq!(vcp.pattern_type, PatternType::ConstantElevationCut, "{id}");
    assert_eq!(u64::from(vcp.pattern_number), int(expected, "num"), "{id}");
    assert_eq!(
        u64::from(vcp.number_of_cuts),
        int(expected, "num_el_cuts"),
        "{id}"
    );
    assert_eq!(vcp.cuts.len(), usize::from(vcp.number_of_cuts), "{id}");
    assert_eq!(u64::from(vcp.version), int(expected, "vcp_version"), "{id}");
    assert_eq!(
        u64::from(vcp.clutter_map_group),
        int(expected, "clutter_map_group"),
        "{id}"
    );
    let resolution = match int(expected, "dop_res_code") {
        2 => DopplerVelocityResolution::HalfMetrePerSecond,
        4 => DopplerVelocityResolution::OneMetrePerSecond,
        other => panic!("{id}: unexpected golden velocity resolution code {other}"),
    };
    assert_eq!(vcp.doppler_velocity_resolution, resolution, "{id}");
    let pulse_width = match int(expected, "pulse_width_code") {
        2 => PulseWidth::Short,
        4 => PulseWidth::Long,
        other => panic!("{id}: unexpected golden pulse width code {other}"),
    };
    assert_eq!(vcp.pulse_width, pulse_width, "{id}");
    assert_eq!(
        u64::from(vcp.sequencing.code),
        int(expected, "vcp_sequencing"),
        "{id}"
    );
    assert_eq!(
        u64::from(vcp.supplemental.code),
        int(expected, "vcp_supplemental_info"),
        "{id}"
    );

    let golden_cuts = expected["els"].as_array().unwrap();
    assert_eq!(golden_cuts.len(), vcp.cuts.len(), "{id}");
    for (index, (cut, want)) in vcp.cuts.iter().zip(golden_cuts).enumerate() {
        let at = format!("{id} cut {}", index + 1);
        assert_eq!(
            f64::from(cut.elevation_angle_deg),
            signed_elevation(float(want, "el_angle")),
            "{at} elevation"
        );
        let channel = match int(want, "channel_config") {
            0 => ChannelConfiguration::ConstantPhase,
            1 => ChannelConfiguration::RandomPhase,
            2 => ChannelConfiguration::Sz2Phase,
            other => panic!("{at}: unexpected golden channel configuration {other}"),
        };
        assert_eq!(cut.channel_configuration, channel, "{at}");
        assert_eq!(
            u64::from(cut.waveform.code()),
            int(want, "waveform"),
            "{at} waveform"
        );
        assert!(
            !matches!(cut.waveform, WaveformType::Unknown(_)),
            "{at} waveform"
        );
        assert_eq!(
            u64::from(cut.super_resolution.code),
            int(want, "super_res"),
            "{at} super resolution"
        );
        assert_eq!(
            u64::from(cut.surveillance_prf_number),
            int(want, "surv_prf_num"),
            "{at}"
        );
        assert_eq!(
            u64::from(cut.surveillance_pulse_count),
            int(want, "surv_pulse_count"),
            "{at}"
        );
        assert_eq!(
            f64::from(cut.azimuth_rate_deg_per_s),
            float(want, "az_rate"),
            "{at} azimuth rate"
        );
        let snr = &cut.snr_threshold_db;
        for (decoded, key) in [
            (snr.reflectivity, "ref_thresh"),
            (snr.velocity, "vel_thresh"),
            (snr.spectrum_width, "sw_thresh"),
            (snr.differential_reflectivity, "zdr_thresh"),
            (snr.differential_phase, "phidp_thresh"),
            (snr.correlation_coefficient, "rhohv_thresh"),
        ] {
            assert_eq!(f64::from(decoded), float(want, key), "{at} {key}");
        }
        for (number, sector) in cut.doppler_sectors.iter().enumerate() {
            let prefix = format!("sector{}", number + 1);
            assert_eq!(
                f64::from(sector.edge_angle_deg),
                float(want, &format!("{prefix}_edge")),
                "{at} {prefix} edge"
            );
            assert_eq!(
                u64::from(sector.prf_number),
                int(want, &format!("{prefix}_doppler_prf_num")),
                "{at} {prefix} PRF number"
            );
            assert_eq!(
                u64::from(sector.pulse_count),
                int(want, &format!("{prefix}_pulse_count")),
                "{at} {prefix} pulse count"
            );
        }
        assert_eq!(
            u64::from(cut.supplemental.code),
            int(want, "supplemental_data"),
            "{at} supplemental"
        );
        assert_eq!(
            f64::from(cut.ebc_angle_deg),
            signed_elevation(float(want, "ebc_angle")),
            "{at} EBC angle"
        );
    }
}

/// Manifest ids with a golden file, one test each.
macro_rules! vcp_golden_tests {
    ($($name:ident => $id:literal,)*) => {
        $(
            #[test]
            fn $name() {
                check_vcp_golden($id);
            }
        )*

        const VCP_GOLDEN_IDS: &[&str] = &[$($id),*];
    };
}

vcp_golden_tests! {
    message_5_klix_2005_zero_filled_is_rejected => "l2-klix-20050829-130035",
    message_5_kpah_2008_build_10 => "l2-kpah-20080415-235014",
    message_5_kdmx_2008_first_super_resolution => "l2-kdmx-20080525-205148",
    message_5_kvnx_2011_vcp_32 => "l2-kvnx-20110315-000203",
    message_5_ktlx_2013_vcp_12 => "l2-ktlx-20130520-201643",
    message_5_kgwx_2013_legacy_resolution => "l2-kgwx-20130601-235640",
    message_5_koax_2014_sails => "l2-koax-20140616-205305",
    message_5_kewx_2016_sails => "l2-kewx-20160413-022531",
    message_5_kdvn_2020_meso_sails_and_sequencing => "l2-kdvn-20200810-180401",
    message_5_klix_2021_mpda => "l2-klix-20210829-180425",
    message_5_kbox_2022_vcp_215 => "l2-kbox-20220129-150537",
    message_5_tjua_2022_sails => "l2-tjua-20220918-190621",
    message_5_kdgx_2023_base_tilt_negative_ebc => "l2-kdgx-20230325-010651",
    message_5_kmaf_2023_long_pulse => "l2-kmaf-20230331-230843",
    message_5_tstl_2023_tdwr => "l2-tstl-20230331-230314",
    message_5_pgua_2023_meso_sails_3 => "l2-pgua-20230524-030945",
    message_5_kmtx_2024_base_tilt_zero_degrees => "l2-kmtx-20240301-212827",
    message_5_ktlx_2024_meso_sails_3 => "l2-ktlx-20240315-000217",
    message_5_ktlx_2024_vcp_35 => "l2-ktlx-20240515-000014",
    message_5_pahg_2025_build_23 => "l2-pahg-20250909-212549",
    message_5_kilx_2026_mrle => "l2-kilx-20260418-013553",
    message_5_kiwa_2026_build_24 => "l2-kiwa-20260917-003629",
    message_5_kiwa_2026_start_chunk => "l2chunk-kiwa-307-20260917-003629-001-s",
}

#[test]
fn every_vcp_golden_file_has_a_test() {
    let on_disk: BTreeSet<String> = std::fs::read_dir(golden_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter_map(|name| name.strip_suffix(".json").map(str::to_owned))
        .collect();
    let tested: BTreeSet<String> = VCP_GOLDEN_IDS.iter().map(|id| (*id).to_owned()).collect();
    assert_eq!(on_disk, tested);
}

// ---------------------------------------------------------------------------
// Message 5 flags against the cuts and the corpus manifest

/// VCP-level SAILS, MRLE, MPDA and base tilt flags (halfword 10) agree with
/// the per-cut supplemental data (E15) and elevations, and with the manifest
/// tags curated for each file.
#[test]
fn message_5_supplemental_flags_agree_with_cuts_and_manifest() {
    let mut checked = 0;
    let mut sources = Vec::new();
    for id in VCP_GOLDEN_IDS {
        if golden(id)["message_5"].is_null() {
            continue;
        }
        sources.push(vec![*id]);
        let Some(metadata) = metadata_of(id) else {
            continue;
        };
        let vcp = &metadata.vcp[0];
        let supplemental = vcp.supplemental;
        let cuts = &vcp.cuts;
        let lowest = cuts
            .iter()
            .map(|cut| cut.elevation_angle_deg)
            .fold(f32::INFINITY, f32::min);

        let sails: BTreeSet<u8> = cuts
            .iter()
            .filter(|cut| cut.supplemental.sails_cut())
            .map(|cut| cut.supplemental.sails_sequence_number())
            .collect();
        assert_eq!(supplemental.sails(), !sails.is_empty(), "{id}");
        assert_eq!(usize::from(supplemental.sails_cuts()), sails.len(), "{id}");
        assert!(
            sails.iter().copied().eq(1..=supplemental.sails_cuts()),
            "{id}"
        );
        for cut in cuts.iter().filter(|cut| cut.supplemental.sails_cut()) {
            // A SAILS cut repeats the first cut of the pattern.
            assert_eq!(cut.elevation_angle_deg, cuts[0].elevation_angle_deg, "{id}");
        }

        // MRLE sequence numbers are the RPG elevation index of the cut
        // (split cuts share an index): the n-th distinct elevation.
        let mut elevations: Vec<f32> = Vec::new();
        for cut in cuts.iter().filter(|cut| cut.supplemental.code & 0x11 == 0) {
            if elevations.last() != Some(&cut.elevation_angle_deg) {
                elevations.push(cut.elevation_angle_deg);
            }
        }
        let mrle: BTreeSet<u8> = cuts
            .iter()
            .filter(|cut| cut.supplemental.mrle_cut())
            .map(|cut| {
                let sequence = cut.supplemental.mrle_sequence_number();
                assert_eq!(
                    Some(&cut.elevation_angle_deg),
                    elevations.get(usize::from(sequence) - 1),
                    "{id}: MRLE sequence {sequence}"
                );
                sequence
            })
            .collect();
        assert_eq!(supplemental.mrle(), !mrle.is_empty(), "{id}");
        assert_eq!(usize::from(supplemental.mrle_cuts()), mrle.len(), "{id}");

        let mpda_cuts = cuts
            .iter()
            .filter(|cut| cut.supplemental.mpda_cut())
            .count();
        assert_eq!(supplemental.mpda(), mpda_cuts > 0, "{id}");

        let base_tilts: Vec<f32> = cuts
            .iter()
            .filter(|cut| cut.supplemental.base_tilt_cut())
            .map(|cut| cut.elevation_angle_deg)
            .collect();
        assert_eq!(supplemental.base_tilt(), !base_tilts.is_empty(), "{id}");
        assert!(base_tilts.iter().all(|angle| *angle == lowest), "{id}");

        let tags = &recast_radar_testdata::entry(id).unwrap().tags;
        let tag_number = |prefix: &str| {
            tags.iter()
                .find_map(|tag| tag.strip_prefix(prefix))
                .map(|value| value.parse::<u16>().unwrap())
        };
        assert_eq!(tag_number("vcp:"), Some(vcp.pattern_number), "{id}");
        if let Some(count) = tag_number("meso-sails:") {
            assert_eq!(u16::from(supplemental.sails_cuts()), count, "{id}");
        }
        if tags.iter().any(|tag| tag == "sails") {
            // Builds 14 and 16 (VCP version 0) leave halfword 10 at 0.
            assert!(
                supplemental.sails_cuts() == 1 || (supplemental.code == 0 && vcp.version == 0),
                "{id}"
            );
        }
        if let Some(count) = tag_number("mrle:") {
            assert_eq!(u16::from(supplemental.mrle_cuts()), count, "{id}");
        }
        assert_eq!(
            tags.iter().any(|tag| tag == "mpda"),
            supplemental.mpda(),
            "{id}"
        );
        if let Some(count) = tag_number("base-tilt:") {
            assert_eq!(u16::from(supplemental.base_tilt_cuts()), count, "{id}");
        }
        let long_pulse = tags.iter().any(|tag| tag == "long-pulse");
        assert_eq!(vcp.pulse_width == PulseWidth::Long, long_pulse, "{id}");
        if tags.iter().any(|tag| tag == "radar:tdwr") {
            assert_eq!(
                vcp.doppler_velocity_resolution.metres_per_second(),
                Some(1.0),
                "{id}"
            );
        }
        checked += 1;
    }
    // 22 of the 23 VCP goldens have a message 5 (KLIX 2005's is zero-filled).
    assert_eq!(sources.len(), 22, "VCP goldens with a message 5");
    common::assert_checked_every_available("VCP flags", checked, &sources);
}

/// Message 7 has no real sample; a real message 5 frame relabelled as type 7
/// decodes to the same pattern.
#[test]
fn message_7_uses_the_message_5_layout() {
    let Some(raw) = load(START_CHUNK) else { return };
    let mut record = messages::metadata_record(&raw).unwrap().into_owned();
    let original = decode_metadata(&record);
    let [vcp] = original.vcp.as_slice() else {
        panic!("expected one message 5");
    };
    let offset = RawMessages::new(&record)
        .map(Result::unwrap)
        .find(|message| message.header.message_type == 5)
        .unwrap()
        .offset;
    record[offset + 3] = 7;
    let relabelled: Vec<_> = MessageWalker::new(&record)
        .map(Result::unwrap)
        .filter(|(header, _)| header.message_type == 7)
        .collect();
    assert_eq!(relabelled.len(), 1);
    assert_eq!(relabelled[0].1, MessageBody::Vcp(vcp.clone()));
}

/// Mutations of the real KIWA message 5 body exercise the size checks.
#[test]
fn message_5_size_errors_on_real_body() {
    let Some(raw) = load(START_CHUNK) else { return };
    let record = messages::metadata_record(&raw).unwrap();
    let body = RawMessages::new(&record)
        .map(Result::unwrap)
        .find(|message| message.header.message_type == 5)
        .unwrap()
        .body
        .into_owned();
    // 11 + 23 * 20 cuts = 471 halfwords, the whole body.
    assert_eq!(
        &body[..8],
        &[0x01, 0xD7, 0x00, 0x02, 0x00, 0xD7, 0x00, 0x14]
    );
    assert_eq!(body.len(), 942);
    assert!(VolumeCoveragePattern::decode(&body).is_ok());

    let truncated = VolumeCoveragePattern::decode(&body[..941]).unwrap_err();
    assert!(
        matches!(
            truncated,
            NexradError::Truncated {
                needed: 942,
                available: 941,
                ..
            }
        ),
        "{truncated:?}"
    );

    // 21 cuts do not fit 471 halfwords.
    let mut extra_cut = body.clone();
    extra_cut[7] = 21;
    assert_eq!(
        reason(&VolumeCoveragePattern::decode(&extra_cut).unwrap_err()),
        "VCP message size 471 halfwords does not hold 21 cuts of at least 23 halfwords after \
         the 11-halfword header"
    );

    let mut no_cuts = body.clone();
    no_cuts[7] = 0;
    assert_eq!(
        reason(&VolumeCoveragePattern::decode(&no_cuts).unwrap_err()),
        "VCP has no elevation cuts"
    );

    // 10 cuts of 46 halfwords each: every E1 of the stride is read.
    let mut halved = body.clone();
    halved[7] = 10;
    let wide = VolumeCoveragePattern::decode(&halved).unwrap();
    let full = VolumeCoveragePattern::decode(&body).unwrap();
    assert_eq!(wide.cuts.len(), 10);
    for (index, cut) in wide.cuts.iter().enumerate() {
        assert_eq!(*cut, full.cuts[index * 2]);
    }
}

// ---------------------------------------------------------------------------
// Message 32

fn prf_of(id: &str) -> Option<(VolumeCoveragePattern, RdaPrfData)> {
    let metadata = metadata_of(id)?;
    assert!(metadata.errors.is_empty(), "{id}: {:?}", metadata.errors);
    let [vcp] = metadata.vcp.as_slice() else {
        panic!("{id}: expected one message 5");
    };
    let [prf] = metadata.prf.as_slice() else {
        panic!("{id}: expected one message 32");
    };
    Some((vcp.clone(), prf.clone()))
}

/// Checks exact message 32 values: `tables` lists (waveform code, PRFs in
/// mHz) per section.
fn check_prf(prf: &RdaPrfData, tables: &[(u16, [u32; 8])]) {
    assert_eq!(usize::from(prf.number_of_waveforms), tables.len());
    assert_eq!(prf.waveforms.len(), tables.len());
    for (section, (code, prfs)) in prf.waveforms.iter().zip(tables) {
        assert_eq!(section.waveform.code(), *code);
        assert_eq!(section.prfs_mhz, prfs);
        // ICD ranges: waveform 1, 2 or 5; PRFs 0 to 1,500,000 mHz.
        assert!(matches!(code, 1 | 2 | 5));
        assert!(section.prfs_mhz.iter().all(|mhz| *mhz <= 1_500_000));
        assert!(section.prfs_mhz.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(section.prf_hz(0), None);
        assert_eq!(section.prf_hz(9), None);
        assert_eq!(section.prf_hz(1), Some(f64::from(prfs[0]) / 1000.0));
    }
}

/// PAHG 2025-09-09 (Build 23.1). Message 32 is the fixed frame at byte
/// 304000 of the first LDM record (header `00 40 0a 20 00 0a 4f 62 04 27 73
/// 32 00 01 00 01`: 64 halfwords, type 0x20 = 32). Body, 112 bytes:
///
/// ```text
/// 00 03 00 00 00 01 00 08 00 04 dc 06 00 05 52 a8 00 05 d6 ba 00 06 bb 5c
/// 00 07 c2 36 00 09 0b fa 00 0a e6 32 00 0d 05 16 00 02 00 08 00 06 bb 5c
/// 00 0c 2c ae 00 0c ee 64 00 0d f4 c6 00 0f 42 40 00 10 77 64 00 11 be 26
/// 00 13 3b d4 00 05 00 08 00 08 c4 f9 00 09 6f bd 00 0a 36 a0 00 0b 21 33
/// 00 0c 3a 02 00 0d 90 39 00 0f 3a 74 00 11 4c db
/// ```
///
/// 3 waveforms, spare 0; waveform 1, 8 PRFs from 0x0004dc06 = 318470 mHz;
/// waveform 2 (from byte 40), 8 PRFs; waveform 5 (from byte 76), 8 PRFs.
#[test]
fn message_32_pahg_2025_exact_values() {
    let Some((_, prf)) = prf_of("l2-pahg-20250909-212549") else {
        return;
    };
    check_prf(
        &prf,
        &[
            (
                1,
                [
                    318470, 348840, 382650, 441180, 508470, 592890, 714290, 853270,
                ],
            ),
            (
                2,
                [
                    441180, 797870, 847460, 914630, 1000000, 1079140, 1162790, 1260500,
                ],
            ),
            (
                5,
                [
                    574713, 618429, 669344, 729395, 801282, 888889, 998004, 1133787,
                ],
            ),
        ],
    );
}

/// KILX 2026-04-18 (Build 23.1). Message 32 frame at byte 304000 of the
/// first LDM record (header `00 40 08 20 00 0c 50 4e 03 ff bf a9 00 01 00
/// 01`). Body:
///
/// ```text
/// 00 03 00 00 00 01 00 08 00 04 e9 62 00 05 55 d2 00 05 ed f8 00 06 cf de
/// 00 07 cf ce 00 09 31 2a 00 0a f3 84 00 0d 27 76 00 02 00 08 00 06 cf de
/// 00 0c 3d 5c 00 0d 14 34 00 0e 20 ea 00 0f 77 06 00 10 b4 ea 00 12 05 ac
/// 00 13 90 02 00 05 00 08 00 08 c4 f9 00 09 6f bd 00 0a 36 a0 00 0b 21 33
/// 00 0c 3a 02 00 0d 90 39 00 0f 3a 74 00 11 4c db
/// ```
#[test]
fn message_32_kilx_2026_exact_values() {
    let Some((_, prf)) = prf_of("l2-kilx-20260418-013553") else {
        return;
    };
    check_prf(
        &prf,
        &[
            (
                1,
                [
                    321890, 349650, 388600, 446430, 511950, 602410, 717700, 862070,
                ],
            ),
            (
                2,
                [
                    446430, 802140, 857140, 925930, 1013510, 1094890, 1181100, 1282050,
                ],
            ),
            (
                5,
                [
                    574713, 618429, 669344, 729395, 801282, 888889, 998004, 1133787,
                ],
            ),
        ],
    );
}

/// KIWA real-time start chunk 2026-09-17 (Build 24.1, committed file, runs
/// offline). Message 32 frame at byte 304000 of the LDM record (header `00 40
/// 08 20 00 09 50 e9 02 69 9b 48 00 01 00 01`). Body:
///
/// ```text
/// 00 03 00 00 00 01 00 08 00 04 e9 62 00 05 55 d2 00 05 ed f8 00 06 cf de
/// 00 07 cf ce 00 09 31 2a 00 0a f3 84 00 0d 27 76 00 02 00 08 00 06 cf de
/// 00 0c 3d 5c 00 0d 14 34 00 0e 20 ea 00 0f 77 06 00 10 b4 ea 00 12 05 ac
/// 00 13 90 02 00 05 00 08 00 08 c3 6b 00 09 6e 91 00 0a 35 fb 00 0b 21 4e
/// 00 0c 3b 30 00 0d 92 f7 00 0f 3f 8b 00 11 53 d9
/// ```
///
/// The archive volume `l2-kiwa-20260917-003629` starts with the same bytes.
#[test]
fn message_32_kiwa_2026_start_chunk_exact_values() {
    let expected = [
        (
            1,
            [
                321890, 349650, 388600, 446430, 511950, 602410, 717700, 862070,
            ],
        ),
        (
            2,
            [
                446430, 802140, 857140, 925930, 1013510, 1094890, 1181100, 1282050,
            ],
        ),
        (
            5,
            [
                574315, 618129, 669179, 729422, 801584, 889591, 999307, 1135577,
            ],
        ),
    ];
    for id in [START_CHUNK, "l2-kiwa-20260917-003629"] {
        let Some((_, prf)) = prf_of(id) else { continue };
        check_prf(&prf, &expected);
    }
}

/// The waveform table a Doppler PRF number indexes (Table XVIII note 1).
fn doppler_table(waveform: WaveformType) -> WaveformType {
    match waveform {
        WaveformType::ContiguousDopplerWithoutAmbiguityResolution | WaveformType::Batch => {
            WaveformType::ContiguousDopplerWithAmbiguityResolution
        }
        other => other,
    }
}

fn unambiguous_range_km(prf_hz: f64) -> f64 {
    SPEED_OF_LIGHT_M_S / (2.0 * prf_hz) / 1000.0
}

/// For every sweep, the PRF that messages 5 and 32 select gives the
/// unambiguous range recorded in the sweep's Message 31 radial blocks (as
/// MetPy reads them) to within 0.5%, and is the closest PRF of its table:
/// every other PRF of the table is at least 3% off. Contiguous surveillance
/// sweeps are checked through the surveillance PRF; all other sweeps through
/// the Doppler PRF of each sector (the radial block reports the Doppler
/// range for batch cuts).
fn check_prf_against_message_31(id: &str) {
    let Some((vcp, prf)) = prf_of(id) else { return };
    let golden = golden(id);
    let sweeps = golden["message_31_sweeps"].as_array().unwrap();
    assert!(!sweeps.is_empty(), "{id}");
    for sweep in sweeps {
        let elevation_number = int(sweep, "el_num");
        let cut = &vcp.cuts[usize::try_from(elevation_number).unwrap() - 1];
        let at = format!("{id} elevation {elevation_number}");
        let (table, prf_numbers, prfs_hz): (WaveformType, Vec<u16>, Vec<f64>) =
            if cut.waveform == WaveformType::ContiguousSurveillance {
                (
                    WaveformType::ContiguousSurveillance,
                    vec![u16::from(cut.surveillance_prf_number)],
                    vec![prf.surveillance_prf_hz(cut).unwrap()],
                )
            } else {
                let sectors = cut.doppler_sectors.iter().enumerate();
                let used: Vec<_> = sectors
                    .filter(|(_, sector)| sector.prf_number != 0)
                    .collect();
                assert!(!used.is_empty(), "{at}: no Doppler sector");
                (
                    doppler_table(cut.waveform),
                    used.iter().map(|(_, sector)| sector.prf_number).collect(),
                    used.iter()
                        .map(|(index, _)| prf.doppler_prf_hz(cut, *index).unwrap())
                        .collect(),
                )
            };
        let table_prfs = &prf.waveform(table).unwrap().prfs_mhz;
        let ranges = sweep["unamb_range_km"].as_array().unwrap();
        assert_eq!(ranges.len(), 1, "{at}: one unambiguous range per sweep");
        let recorded = ranges[0].as_f64().unwrap();
        for (prf_number, prf_hz) in prf_numbers.iter().zip(&prfs_hz) {
            let error = (unambiguous_range_km(*prf_hz) - recorded).abs() / recorded;
            assert!(
                error < 0.005,
                "{at}: PRF {prf_number} ({prf_hz} Hz) gives {} km, radials say {recorded} km",
                unambiguous_range_km(*prf_hz)
            );
            for (index, other) in table_prfs.iter().enumerate() {
                if index + 1 == usize::from(*prf_number) {
                    continue;
                }
                let other_range = unambiguous_range_km(f64::from(*other) / 1000.0);
                assert!(
                    (other_range - recorded).abs() / recorded > 0.03,
                    "{at}: PRF {} of the table also matches {recorded} km",
                    index + 1
                );
            }
        }
    }
}

#[test]
fn message_32_prfs_match_message_31_unambiguous_range_pahg_2025() {
    check_prf_against_message_31("l2-pahg-20250909-212549");
}

#[test]
fn message_32_prfs_match_message_31_unambiguous_range_kilx_2026() {
    check_prf_against_message_31("l2-kilx-20260418-013553");
}

#[test]
fn message_32_prfs_match_message_31_unambiguous_range_kiwa_2026() {
    check_prf_against_message_31("l2-kiwa-20260917-003629");
}

/// Mutations of the real KIWA message 32 body exercise the length checks.
#[test]
fn message_32_errors_on_real_body() {
    let Some(raw) = load(START_CHUNK) else { return };
    let record = messages::metadata_record(&raw).unwrap();
    let body = RawMessages::new(&record)
        .map(Result::unwrap)
        .find(|message| message.header.message_type == 32)
        .unwrap()
        .body
        .into_owned();
    assert_eq!(body.len(), 112);

    // The last PRF ends at byte 112; one byte less truncates it.
    let truncated = RdaPrfData::decode(&body[..111]).unwrap_err();
    assert!(
        matches!(
            truncated,
            NexradError::Truncated {
                what: "PRF data values",
                offset: 80,
                needed: 32,
                available: 31,
            }
        ),
        "{truncated:?}"
    );

    // A fourth waveform section would start past the end.
    let mut four = body.clone();
    four[1] = 4;
    assert!(matches!(
        RdaPrfData::decode(&four).unwrap_err(),
        NexradError::Truncated {
            what: "PRF data waveform section",
            offset: 112,
            ..
        }
    ));

    let mut none = body.clone();
    none[1] = 0;
    assert_eq!(
        reason(&RdaPrfData::decode(&none).unwrap_err()),
        "PRF data has no waveforms"
    );

    // Two waveforms: the third section is ignored.
    let mut two = body.clone();
    two[1] = 2;
    let decoded = RdaPrfData::decode(&two).unwrap();
    assert_eq!(decoded.waveforms.len(), 2);
    assert_eq!(
        decoded.waveforms[..],
        RdaPrfData::decode(&body).unwrap().waveforms[..2]
    );
}
