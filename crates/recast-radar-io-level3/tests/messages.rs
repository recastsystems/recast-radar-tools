//! Messages without a Product Description Block against the real Level III
//! corpus: the three General Status Messages (message code 2) and the Free Text
//! Message (WMO heading `NOUS63`).
//!
//! Expected values come from two sources:
//!
//! 1. **The files' bytes**, read by hand from a hex dump and written out in
//!    [`expected_status`] per ICD 2620001AD/2620001T Figure 3-17, and the text
//!    of the Free Text Message spelled out in [`free_text_message_matches_bytes_and_metpy`].
//! 2. **MetPy 1.7.1** `Level3File`, which reads all four files: the golden JSON
//!    (`testdata/level3/golden/<id>.json`, `metpy_detail.gsm` and
//!    `metpy_detail.text_sha256`) holds its General Status Message fields and
//!    the SHA-256 of its text. The golden JSON does not hold MetPy's
//!    `gsm_additional` (Build 14 fields), so [`METPY_GSM_ADDITIONAL`] records
//!    them, produced by the script at the end of this file.
//!
//! MetPy renders status halfwords with `BitField` name lists; [`metpy_bits`]
//! renders the decoded halfwords the same way, and
//! [`flag_constants_match_metpy_bit_names`] checks that the named flag
//! constants sit at the bits MetPy names.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{Entry, Json, entry, sha256_hex};
use recast_radar_io_level3::messages::{
    ClutterMitigation, DataTransmission, GSM_BLOCK_BYTES, GSM_SHORT_BLOCK_BYTES,
    GeneralStatusMessage, ProductAvailability, RdaAlarms, RdaOperability, RdaStatus, RpgAlarms,
    RpgNarrowband, RpgOperability, RpgStatus, SupplementalCuts, VcpSupplemental,
};
use recast_radar_io_level3::{
    Level3Error, Level3Message, MessageHeader, OperationalMode, decode_message, decode_product,
};

/// Corpus files holding a General Status Message.
const GSM_FILES: [&str; 3] = [
    "l3-ddc-gsm-20200817-1000",
    "l3-eax-gsm-20200817-0933",
    "l3-tlx-gsm-20130520-2100",
];

/// The corpus file holding a plain-text message.
const TEXT_FILE: &str = "l3-abr-ftm-20110428-1331";

/// MetPy 1.7.1 `Level3File.gsm_additional` without `spare`, as JSON, per file
/// (`None` when MetPy has no additional block: block length 82).
const METPY_GSM_ADDITIONAL: [(&str, Option<&str>); 3] = [
    (
        "l3-ddc-gsm-20200817-1000",
        Some(
            r#"{"el21": 0.0, "el22": 0.0, "el23": 0.0, "el24": 0.0, "el25": 0.0, "vcp_supplemental": ["AVSET", "SAILS", "RxR Noise", "CBT"], "supplemental_cut_map": [false, false, true, false, false, true, false, false, false, false, false, false, false, false, false, false], "supplemental_cut_count": 2, "supplemental_cut_map2": [false, false, false, false, false, false, false, false, false]}"#,
        ),
    ),
    (
        "l3-eax-gsm-20200817-0933",
        Some(
            r#"{"el21": 0.0, "el22": 0.0, "el23": 0.0, "el24": 0.0, "el25": 0.0, "vcp_supplemental": ["AVSET", "RxR Noise"], "supplemental_cut_map": [false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false], "supplemental_cut_count": 0, "supplemental_cut_map2": [false, false, false, false, false, false, false, false, false]}"#,
        ),
    ),
    ("l3-tlx-gsm-20130520-2100", None),
];

// MetPy 1.7.1 `BitField` name lists (`metpy/io/nexrad.py`, `Level3File.gsm_fmt`
// and `additional_gsm_fmt`), least significant bit first.
const METPY_OP_MODE: &[&str] = &["Clear Air", "Precip"];
const METPY_RDA_OP_STATUS: &[&str] = &[
    "Spare",
    "Online",
    "Maintenance Required",
    "Maintenance Mandatory",
    "Commanded Shutdown",
    "Inoperable",
    "Spare",
    "Wideband Disconnect",
];
const METPY_RDA_STATUS: &[&str] = &[
    "Spare",
    "Startup",
    "Standby",
    "Restart",
    "Operate",
    "Off-line Operate",
];
const METPY_RDA_ALARMS: &[&str] = &[
    "Indeterminate",
    "Tower/Utilities",
    "Pedestal",
    "Transmitter",
    "Receiver",
    "RDA Control",
    "RDA Communications",
    "Signal Processor",
];
const METPY_TRANSMISSION: &[&str] = &[
    "Spare",
    "None",
    "Reflectivity",
    "Velocity",
    "Spectrum Width",
    "Dual Pol",
];
const METPY_RPG_OP_STATUS: &[&str] = &[
    "Loadshed",
    "Online",
    "Maintenance Required",
    "Maintenance Mandatory",
    "Commanded shutdown",
];
/// MetPy's list lacks a comma after `'Product Storage Loadshed'` (Python joins
/// it with the next `'Spare'`) and has no `'Backup Comms'`, so from index 9 its
/// names sit one bit lower than the ICD's.
const METPY_RPG_ALARMS: &[&str] = &[
    "None",
    "Node Connectivity",
    "Wideband Failure",
    "RPG Control Task Failure",
    "Data Base Failure",
    "Spare",
    "RPG Input Buffer Loadshed",
    "Spare",
    "Product Storage LoadshedSpare",
    "Spare",
    "Spare",
    "RPG/RPG Intercomputer Link Failure",
    "Redundant Channel Error",
    "Task Failure",
    "Media Failure",
];
const METPY_RPG_STATUS: &[&str] = &["Restart", "Operate", "Standby"];
const METPY_NARROWBAND: &[&str] = &["Commanded Disconnect", "Narrowband Loadshed"];
const METPY_PROD_AVAIL: &[&str] = &[
    "Product Availability",
    "Degraded Availability",
    "Not Available",
];
const METPY_VCP_SUPPLEMENTAL: &[&str] = &[
    "AVSET",
    "SAILS",
    "Site VCP",
    "RxR Noise",
    "CBT",
    "VCP Sequence",
    "SPRT",
    "MRLE",
    "Base Tilt",
    "MPDA",
];

fn general_status(entry: &Entry) -> GeneralStatusMessage {
    match decode_message(&entry.bytes()) {
        Ok(Level3Message::GeneralStatus(gsm)) => *gsm,
        other => panic!(
            "{}: expected a General Status Message, got {other:?}",
            entry.id
        ),
    }
}

/// Values of one General Status Message read from the file's bytes.
struct ExpectedStatus {
    header: MessageHeader,
    wmo_heading: &'static str,
    awips_id: &'static str,
    block_length: u16,
    mode: OperationalMode,
    vcp: u16,
    elevation_cuts: u16,
    /// Elevation halfwords 16.. up to the last nonzero one, in 0.1 degree.
    elevations_tenths: &'static [i16],
    /// Halfwords 36-52.
    status: [u16; 17],
    /// Halfwords 58-60 (long form only).
    supplement: Option<[u16; 3]>,
}

/// Hand-read from `xxd` of each file. The message starts after the 30-byte WMO
/// heading and AWIPS line; halfword `n` is at file byte `30 + 2 * (n - 1)`.
fn expected_status(id: &str) -> ExpectedStatus {
    match id {
        // 0002 483c 0000 8ca1 0000 00c8 015e 0000 0002 | ffff 00b2 0002 0002 00d4 000b
        // 0005 0009 0005 000d 0012 0005 0018 001f 0028 0033 0040 0000 ...
        // 0010 0000 003c 0002 0001 0002 0000 0000 0001 002f 001f 0001 00be 0000 0000 0000 00be
        // 0000 0000 0000 0000 0000 001b 0024 0400, then 40 zero halfwords.
        "l3-ddc-gsm-20200817-1000" => ExpectedStatus {
            header: MessageHeader {
                code: 2,
                date: 18492,
                time: 36001,
                length: 200,
                source_id: 350,
                destination_id: 0,
                num_blocks: 2,
            },
            wmo_heading: "NXUS63 KDDC 171000",
            awips_id: "GSMDDC",
            block_length: 178,
            mode: OperationalMode::Precipitation,
            vcp: 212,
            elevation_cuts: 11,
            elevations_tenths: &[5, 9, 5, 13, 18, 5, 24, 31, 40, 51, 64],
            status: [
                0x10, 0, 0x3c, 2, 1, 2, 0, 0, 1, 0x2f, 0x1f, 1, 0xbe, 0, 0, 0, 0xbe,
            ],
            supplement: Some([0x1b, 0x24, 0x400]),
        },
        // 0002 483c 0000 867b 0000 00c8 0181 0000 0002 | ffff 00b2 0001 0002 0023 0009
        // 0005 0009 000d 0012 0018 001f 0028 0033 0040 0000 ...
        // 0010 0000 003c 0002 0001 0002 0000 0001 0001 0007 001f 0001 00b6 0000 0000 0000 00b6
        // 0000 0000 0000 0000 0000 0009 0000 0000, then 40 zero halfwords.
        "l3-eax-gsm-20200817-0933" => ExpectedStatus {
            header: MessageHeader {
                code: 2,
                date: 18492,
                time: 34427,
                length: 200,
                source_id: 385,
                destination_id: 0,
                num_blocks: 2,
            },
            wmo_heading: "NXUS63 KEAX 170933",
            awips_id: "GSMEAX",
            block_length: 178,
            mode: OperationalMode::ClearAir,
            vcp: 35,
            elevation_cuts: 9,
            elevations_tenths: &[5, 9, 13, 18, 24, 31, 40, 51, 64],
            status: [
                0x10, 0, 0x3c, 2, 1, 2, 0, 1, 1, 7, 0x1f, 1, 0xb6, 0, 0, 0, 0xb6,
            ],
            supplement: Some([9, 0, 0]),
        },
        // 0002 3de6 0001 278b 0000 0068 0001 0000 0002 | ffff 0052 0002 0002 000c 000e
        // 0005 0009 000d 0012 0018 001f 0028 0033 0040 0050 0064 007d 009c 00c3 0000 ...
        // 0010 0000 003c 0002 0001 0002 0000 0001 0001 0007 003f 0004 0084 0000 0000 0000 0084
        // (end of file: the 82-byte block of RPG builds before 14.0).
        "l3-tlx-gsm-20130520-2100" => ExpectedStatus {
            header: MessageHeader {
                code: 2,
                date: 15846,
                time: 75659,
                length: 104,
                source_id: 1,
                destination_id: 0,
                num_blocks: 2,
            },
            wmo_heading: "NXUS64 KOUN 202100",
            awips_id: "GSMTLX",
            block_length: 82,
            mode: OperationalMode::Precipitation,
            vcp: 12,
            elevation_cuts: 14,
            elevations_tenths: &[5, 9, 13, 18, 24, 31, 40, 51, 64, 80, 100, 125, 156, 195],
            status: [
                0x10, 0, 0x3c, 2, 1, 2, 0, 1, 1, 7, 0x3f, 4, 0x84, 0, 0, 0, 0x84,
            ],
            supplement: None,
        },
        other => panic!("no expected values for {other}"),
    }
}

#[test]
fn general_status_messages_match_file_bytes() {
    for id in GSM_FILES {
        let entry = entry(id);
        let gsm = general_status(&entry);
        let want = expected_status(id);

        assert_eq!(gsm.message_header, want.header, "{id}");
        let text_header = gsm.text_header.as_ref().unwrap();
        assert_eq!(text_header.wmo_heading, want.wmo_heading, "{id}");
        assert_eq!(text_header.awips_id.as_deref(), Some(want.awips_id), "{id}");
        assert_eq!(gsm.block_length, want.block_length, "{id}");
        assert_eq!(gsm.mode, want.mode, "{id}");
        assert_eq!(gsm.rda_operability, RdaOperability::ONLINE, "{id}");
        assert_eq!(gsm.vcp, want.vcp, "{id}");
        assert_eq!(gsm.elevation_cuts, want.elevation_cuts, "{id}");

        // All 20 (short form) or 25 (long form) slots; slots after the last cut are 0.
        let slots = if want.block_length == GSM_BLOCK_BYTES {
            25
        } else {
            20
        };
        let mut tenths = vec![0i16; slots];
        tenths[..want.elevations_tenths.len()].copy_from_slice(want.elevations_tenths);
        let degrees: Vec<f64> = tenths.iter().map(|&t| f64::from(t) / 10.0).collect();
        assert_eq!(gsm.elevations_deg, degrees, "{id}");
        assert_eq!(
            gsm.cut_elevations_deg(),
            &degrees[..usize::from(want.elevation_cuts)],
            "{id}"
        );

        // Halfwords 36-52, field by field.
        let s = want.status;
        assert_eq!(gsm.rda_status.bits(), s[0], "{id}");
        assert_eq!(gsm.rda_alarms.bits(), s[1], "{id}");
        assert_eq!(gsm.data_transmission.bits(), s[2], "{id}");
        assert_eq!(gsm.rpg_operability.bits(), s[3], "{id}");
        assert_eq!(gsm.rpg_alarms.bits(), s[4], "{id}");
        assert_eq!(gsm.rpg_status.bits(), s[5], "{id}");
        assert_eq!(gsm.rpg_narrowband.bits(), s[6], "{id}");
        assert_eq!(gsm.horizontal_calibration_db, f64::from(s[7]) / 4.0, "{id}");
        assert_eq!(gsm.product_availability.bits(), s[8], "{id}");
        assert_eq!(gsm.super_resolution_cuts, s[9], "{id}");
        assert_eq!(gsm.clutter_mitigation.bits(), s[10], "{id}");
        assert_eq!(gsm.vertical_calibration_db, f64::from(s[11]) / 4.0, "{id}");
        assert_eq!(gsm.rda_build, s[12], "{id}");
        assert_eq!(gsm.rda_channel, s[13], "{id}");
        assert_eq!(gsm.reserved, [s[14], s[15]], "{id}");
        assert_eq!(gsm.rpg_build, s[16], "{id}");

        // What the halfwords mean in these files (Figure 3-17).
        assert!(gsm.rda_status.contains(RdaStatus::OPERATE), "{id}");
        assert_eq!(gsm.rda_status.names(), ["Operate"], "{id}");
        assert!(gsm.rda_alarms.is_empty(), "{id}");
        assert_eq!(
            gsm.data_transmission.names(),
            [
                "Reflectivity",
                "Velocity",
                "Spectrum Width",
                "Dual Pol Data Expected"
            ],
            "{id}"
        );
        assert!(
            !gsm.data_transmission.contains(DataTransmission::NONE),
            "{id}"
        );
        assert_eq!(gsm.rpg_operability, RpgOperability::ONLINE, "{id}");
        assert_eq!(gsm.rpg_alarms, RpgAlarms::NO_ALARMS, "{id}");
        assert_eq!(gsm.rpg_status, RpgStatus::OPERATE, "{id}");
        assert!(gsm.rpg_narrowband.is_empty(), "{id}");
        assert_eq!(
            gsm.product_availability,
            ProductAvailability::AVAILABLE,
            "{id}"
        );
        assert!(
            gsm.clutter_mitigation.contains(ClutterMitigation::ENABLED),
            "{id}"
        );
        let super_res: Vec<usize> = (1..=16).filter(|&c| gsm.super_resolution(c)).collect();
        let bits: Vec<usize> = (0..16)
            .filter(|b| s[9] & (1 << b) != 0)
            .map(|b| b + 1)
            .collect();
        assert_eq!(super_res, bits, "{id}");
        assert!(
            !gsm.super_resolution(0) && !gsm.super_resolution(17),
            "{id}"
        );
        assert_eq!(gsm.rda_build_version(), f64::from(s[12]) / 10.0, "{id}");
        assert_eq!(gsm.rpg_build_version(), f64::from(s[16]) / 10.0, "{id}");

        match want.supplement {
            Some([vcp_supplemental, map1, map2]) => {
                assert_eq!(
                    gsm.vcp_supplemental.unwrap().bits(),
                    vcp_supplemental,
                    "{id}"
                );
                assert_eq!(
                    gsm.supplemental_cuts.unwrap(),
                    SupplementalCuts {
                        halfwords: [map1, map2]
                    },
                    "{id}"
                );
            }
            None => {
                assert_eq!(gsm.vcp_supplemental, None, "{id}");
                assert_eq!(gsm.supplemental_cuts, None, "{id}");
            }
        }

        // Raw halfwords from the block divider to the end of the block.
        let bytes = entry.bytes();
        let block = &bytes[30 + 18..30 + 18 + 4 + usize::from(want.block_length)];
        let raw: Vec<u16> = block
            .chunks_exact(2)
            .map(|p| u16::from_be_bytes([p[0], p[1]]))
            .collect();
        assert_eq!(gsm.halfwords, raw, "{id}");
        assert_eq!(gsm.halfwords[0], 0xFFFF, "{id}: block divider");
        assert_eq!(
            30 + usize::try_from(want.header.length).unwrap(),
            bytes.len(),
            "{id}: message length"
        );
    }
}

/// The KDDC message in VCP 212 with SAILS: the supplemental cut map marks the
/// two repeated 0.5 degree cuts (3 and 6) and counts two supplemental cuts.
#[test]
fn sails_supplemental_cuts_follow_the_elevations() {
    let gsm = general_status(&entry("l3-ddc-gsm-20200817-1000"));
    let supplemental = gsm.vcp_supplemental.unwrap();
    assert_eq!(supplemental.names(), ["AVSET", "SAILS", "RxRN", "CBT"]);
    assert!(supplemental.contains(VcpSupplemental::SAILS));
    assert!(!supplemental.contains(VcpSupplemental::MRLE));
    let cuts = gsm.supplemental_cuts.unwrap();
    let marked: Vec<usize> = (1..=25).filter(|&c| cuts.is_supplemental(c)).collect();
    assert_eq!(marked, [3, 6]);
    assert_eq!(cuts.supplemental_count(), 2);
    assert_eq!(cuts.mpda_count(), 0);
    let elevations = gsm.cut_elevations_deg();
    for cut in marked {
        // A SAILS cut repeats the lowest elevation.
        assert_eq!(elevations[cut - 1], elevations[0]);
    }
    assert_eq!(elevations.iter().filter(|&&e| e == 0.5).count(), 3);
}

/// Every General Status Message field MetPy decodes equals MetPy's value.
#[test]
fn general_status_messages_match_metpy() {
    for id in GSM_FILES {
        let entry = entry(id);
        let golden = entry.golden();
        let gsm = general_status(&entry);
        assert_eq!(golden.get("metpy").as_str(), Some("ok"), "{id}");
        let header = golden.get("metpy_detail").get("header");
        let m = golden.get("metpy_detail").get("gsm");
        let mut problems = Vec::new();
        macro_rules! check {
            ($what:expr, $decoded:expr, $metpy:expr $(,)?) => {{
                let (what, decoded, metpy): (&str, Json, &Json) = ($what, $decoded, $metpy);
                if !json_eq(&decoded, metpy) {
                    problems.push(format!("{what}: decoded {decoded:?}, MetPy {metpy:?}"));
                }
            }};
        }
        let h = &gsm.message_header;
        check!("header code", int(h.code), header.get("code"));
        check!("header date", int(h.date), header.get("date"));
        check!("header time", int(h.time), header.get("time"));
        check!("header msg_len", int(h.length), header.get("msg_len"));
        check!("header src_id", int(h.source_id), header.get("src_id"));
        check!(
            "header dest_id",
            int(h.destination_id),
            header.get("dest_id")
        );
        check!("header num_blks", int(h.num_blocks), header.get("num_blks"));

        check!(
            "divider",
            int(i16::from_be_bytes(gsm.halfwords[0].to_be_bytes())),
            m.get("divider")
        );
        check!("block_len", int(gsm.block_length), m.get("block_len"));
        check!(
            "op_mode",
            metpy_bits(gsm.mode.code(), METPY_OP_MODE),
            m.get("op_mode")
        );
        check!(
            "rda_op_status",
            metpy_bits(gsm.rda_operability.bits(), METPY_RDA_OP_STATUS),
            m.get("rda_op_status"),
        );
        check!("vcp", int(gsm.vcp), m.get("vcp"));
        check!("num_el", int(gsm.elevation_cuts), m.get("num_el"));
        for (i, elevation) in gsm.elevations_deg.iter().take(20).enumerate() {
            let key = format!("el{}", i + 1);
            check!(&key, Json::Num(*elevation), m.get(&key));
        }
        check!(
            "rda_status",
            metpy_bits(gsm.rda_status.bits(), METPY_RDA_STATUS),
            m.get("rda_status"),
        );
        check!(
            "rda_alarms",
            metpy_bits(gsm.rda_alarms.bits(), METPY_RDA_ALARMS),
            m.get("rda_alarms"),
        );
        check!(
            "tranmission_enable",
            metpy_bits(gsm.data_transmission.bits(), METPY_TRANSMISSION),
            m.get("tranmission_enable"),
        );
        check!(
            "rpg_op_status",
            metpy_bits(gsm.rpg_operability.bits(), METPY_RPG_OP_STATUS),
            m.get("rpg_op_status"),
        );
        check!(
            "rpg_alarms",
            metpy_bits(gsm.rpg_alarms.bits(), METPY_RPG_ALARMS),
            m.get("rpg_alarms"),
        );
        check!(
            "rpg_status",
            metpy_bits(gsm.rpg_status.bits(), METPY_RPG_STATUS),
            m.get("rpg_status"),
        );
        check!(
            "rpg_narrowband_status",
            metpy_bits(gsm.rpg_narrowband.bits(), METPY_NARROWBAND),
            m.get("rpg_narrowband_status"),
        );
        check!(
            "h_ref_calib",
            Json::Num(gsm.horizontal_calibration_db),
            m.get("h_ref_calib")
        );
        check!(
            "prod_avail",
            metpy_bits(gsm.product_availability.bits(), METPY_PROD_AVAIL),
            m.get("prod_avail"),
        );
        check!(
            "super_res_cuts",
            bools(16, |b| gsm.super_resolution(b + 1)),
            m.get("super_res_cuts")
        );
        check!(
            "cmd_status",
            bools(6, |b| gsm.clutter_mitigation.bits() & (1 << b) != 0),
            m.get("cmd_status"),
        );
        check!(
            "v_ref_calib",
            Json::Num(gsm.vertical_calibration_db),
            m.get("v_ref_calib")
        );
        check!(
            "rda_build",
            metpy_version(gsm.rda_build),
            m.get("rda_build")
        );
        check!("rda_channel", int(gsm.rda_channel), m.get("rda_channel"));
        check!("reserved", int(gsm.reserved[0]), m.get("reserved"));
        check!("reserved2", int(gsm.reserved[1]), m.get("reserved2"));
        check!(
            "build_version",
            metpy_version(gsm.rpg_build),
            m.get("build_version")
        );

        let additional = METPY_GSM_ADDITIONAL
            .iter()
            .find(|(file, _)| *file == id)
            .and_then(|(_, json)| *json)
            .map(|json| Json::parse(json).unwrap());
        match (&additional, gsm.vcp_supplemental, gsm.supplemental_cuts) {
            (Some(a), Some(supplemental), Some(cuts)) => {
                for (i, elevation) in gsm.elevations_deg[20..].iter().enumerate() {
                    let key = format!("el{}", i + 21);
                    check!(&key, Json::Num(*elevation), a.get(&key));
                }
                check!(
                    "vcp_supplemental",
                    metpy_bits(supplemental.bits(), METPY_VCP_SUPPLEMENTAL),
                    a.get("vcp_supplemental"),
                );
                check!(
                    "supplemental_cut_map",
                    bools(16, |b| cuts.is_supplemental(b + 1)),
                    a.get("supplemental_cut_map"),
                );
                check!(
                    "supplemental_cut_map2",
                    bools(9, |b| cuts.is_supplemental(b + 17)),
                    a.get("supplemental_cut_map2"),
                );
                // MetPy's count is halfword 60 shifted right by 9: the
                // supplemental count (ICD bits 6-3) and MPDA count (bits 2-0) together.
                check!(
                    "supplemental_cut_count",
                    int(cuts.supplemental_count() | (cuts.mpda_count() << 4)),
                    a.get("supplemental_cut_count"),
                );
            }
            (None, None, None) => {}
            other => problems.push(format!("Build 14 fields: {other:?}")),
        }
        assert!(problems.is_empty(), "{id}:\n  {}", problems.join("\n  "));
    }
}

/// The flag constants sit at the bits MetPy names, except where MetPy differs
/// from the ICD (noted per field).
#[test]
fn flag_constants_match_metpy_bit_names() {
    fn bit_of(names: &[&str], name: &str) -> u16 {
        let index = names.iter().position(|n| *n == name).unwrap();
        1 << index
    }
    assert_eq!(
        OperationalMode::ClearAir.code(),
        bit_of(METPY_OP_MODE, "Clear Air")
    );
    assert_eq!(
        OperationalMode::Precipitation.code(),
        bit_of(METPY_OP_MODE, "Precip")
    );
    for (flag, name) in [
        (RdaOperability::ONLINE, "Online"),
        (RdaOperability::MAINTENANCE_REQUIRED, "Maintenance Required"),
        (
            RdaOperability::MAINTENANCE_MANDATORY,
            "Maintenance Mandatory",
        ),
        (RdaOperability::COMMANDED_SHUTDOWN, "Commanded Shutdown"),
        (RdaOperability::INOPERABLE, "Inoperable"),
        (RdaOperability::WIDEBAND_DISCONNECT, "Wideband Disconnect"),
    ] {
        assert_eq!(flag.bits(), bit_of(METPY_RDA_OP_STATUS, name), "{name}");
    }
    // MetPy puts "Off-line Operate" at ICD bit 10; 2620001T Figure 3-17 has it at
    // bit 9 (bit 10 spare), which the decoder follows.
    for (flag, name) in [
        (RdaStatus::STARTUP, "Startup"),
        (RdaStatus::STANDBY, "Standby"),
        (RdaStatus::RESTART, "Restart"),
        (RdaStatus::OPERATE, "Operate"),
    ] {
        assert_eq!(flag.bits(), bit_of(METPY_RDA_STATUS, name), "{name}");
    }
    assert_eq!(
        RdaStatus::OFFLINE_OPERATE.bits(),
        bit_of(METPY_RDA_STATUS, "Off-line Operate") << 1
    );
    for (flag, name) in [
        (RdaAlarms::INDETERMINATE, "Indeterminate"),
        (RdaAlarms::TOWER_UTILITIES, "Tower/Utilities"),
        (RdaAlarms::PEDESTAL, "Pedestal"),
        (RdaAlarms::TRANSMITTER, "Transmitter"),
        (RdaAlarms::RECEIVER, "Receiver"),
        (RdaAlarms::RDA_CONTROL, "RDA Control"),
        (RdaAlarms::RDA_COMMUNICATIONS, "RDA Communications"),
        (RdaAlarms::SIGNAL_PROCESSOR, "Signal Processor"),
    ] {
        assert_eq!(flag.bits(), bit_of(METPY_RDA_ALARMS, name), "{name}");
    }
    for (flag, name) in [
        (DataTransmission::NONE, "None"),
        (DataTransmission::REFLECTIVITY, "Reflectivity"),
        (DataTransmission::VELOCITY, "Velocity"),
        (DataTransmission::SPECTRUM_WIDTH, "Spectrum Width"),
        (DataTransmission::DUAL_POL, "Dual Pol"),
    ] {
        assert_eq!(flag.bits(), bit_of(METPY_TRANSMISSION, name), "{name}");
    }
    for (flag, name) in [
        (RpgOperability::LOADSHED, "Loadshed"),
        (RpgOperability::ONLINE, "Online"),
        (RpgOperability::MAINTENANCE_REQUIRED, "Maintenance Required"),
        (
            RpgOperability::MAINTENANCE_MANDATORY,
            "Maintenance Mandatory",
        ),
        (RpgOperability::COMMANDED_SHUTDOWN, "Commanded shutdown"),
    ] {
        assert_eq!(flag.bits(), bit_of(METPY_RPG_OP_STATUS, name), "{name}");
    }
    // Up to Product Storage Loadshed the lists agree; MetPy's later names are
    // one bit low (see METPY_RPG_ALARMS).
    for (flag, name) in [
        (RpgAlarms::NO_ALARMS, "None"),
        (RpgAlarms::NODE_CONNECTIVITY, "Node Connectivity"),
        (RpgAlarms::WIDEBAND_FAILURE, "Wideband Failure"),
        (RpgAlarms::CONTROL_TASK_FAILURE, "RPG Control Task Failure"),
        (RpgAlarms::DATA_BASE_FAILURE, "Data Base Failure"),
        (
            RpgAlarms::INPUT_BUFFER_LOADSHED,
            "RPG Input Buffer Loadshed",
        ),
        (
            RpgAlarms::PRODUCT_STORAGE_LOADSHED,
            "Product Storage LoadshedSpare",
        ),
    ] {
        assert_eq!(flag.bits(), bit_of(METPY_RPG_ALARMS, name), "{name}");
    }
    for (flag, name) in [
        (
            RpgAlarms::INTERCOMPUTER_LINK_FAILURE,
            "RPG/RPG Intercomputer Link Failure",
        ),
        (
            RpgAlarms::REDUNDANT_CHANNEL_ERROR,
            "Redundant Channel Error",
        ),
        (RpgAlarms::TASK_FAILURE, "Task Failure"),
        (RpgAlarms::MEDIA_FAILURE, "Media Failure"),
    ] {
        assert_eq!(flag.bits(), bit_of(METPY_RPG_ALARMS, name) << 1, "{name}");
    }
    for (flag, name) in [
        (RpgStatus::RESTART, "Restart"),
        (RpgStatus::OPERATE, "Operate"),
        (RpgStatus::STANDBY, "Standby"),
    ] {
        assert_eq!(flag.bits(), bit_of(METPY_RPG_STATUS, name), "{name}");
    }
    for (flag, name) in [
        (RpgNarrowband::COMMANDED_DISCONNECT, "Commanded Disconnect"),
        (RpgNarrowband::LOADSHED, "Narrowband Loadshed"),
    ] {
        assert_eq!(flag.bits(), bit_of(METPY_NARROWBAND, name), "{name}");
    }
    for (flag, name) in [
        (ProductAvailability::AVAILABLE, "Product Availability"),
        (ProductAvailability::DEGRADED, "Degraded Availability"),
        (ProductAvailability::NOT_AVAILABLE, "Not Available"),
    ] {
        assert_eq!(flag.bits(), bit_of(METPY_PROD_AVAIL, name), "{name}");
    }
    for (flag, name) in [
        (VcpSupplemental::AVSET, "AVSET"),
        (VcpSupplemental::SAILS, "SAILS"),
        (VcpSupplemental::SITE_SPECIFIC_VCP, "Site VCP"),
        (VcpSupplemental::RXR_NOISE, "RxR Noise"),
        (VcpSupplemental::CBT, "CBT"),
        (VcpSupplemental::VCP_SEQUENCE, "VCP Sequence"),
        (VcpSupplemental::SPRT, "SPRT"),
        (VcpSupplemental::MRLE, "MRLE"),
        (VcpSupplemental::BASE_TILT, "Base Tilt"),
        (VcpSupplemental::MPDA, "MPDA"),
    ] {
        assert_eq!(flag.bits(), bit_of(METPY_VCP_SUPPLEMENTAL, name), "{name}");
    }
}

#[test]
fn free_text_message_matches_bytes_and_metpy() {
    let entry = entry(TEXT_FILE);
    let bytes = entry.bytes();
    let golden = entry.golden();
    let message = match decode_message(&bytes) {
        Ok(Level3Message::Text(text)) => text,
        other => panic!("expected a text message, got {other:?}"),
    };

    // File: "NOUS63 KABR 281331\r\r\nFTMABR\r\r\n", the text, then FF FF 0A 00.
    assert_eq!(&bytes[..30], b"NOUS63 KABR 281331\r\r\nFTMABR\r\r\n");
    assert_eq!(&bytes[bytes.len() - 4..], [0xFF, 0xFF, 0x0A, 0x00]);
    let expected = format!(
        "Message Date:  Apr 28 2011 13:31:23\n\nABR Radar will be down for maintenance until \
         1600UTC  SLG{}",
        " ".repeat(23)
    );
    assert_eq!(message.text, expected);
    assert_eq!(message.text.len(), bytes.len() - 30 - 4);

    let header = &message.text_header;
    assert_eq!(header.wmo_heading, "NOUS63 KABR 281331");
    assert_eq!(header.data_designator, "NOUS63");
    assert_eq!(header.originator, "KABR");
    assert_eq!(header.day_time, "281331");
    assert_eq!(header.indicator, None);
    assert_eq!(header.awips_id.as_deref(), Some("FTMABR"));
    assert_eq!(header.noaaport_sequence, None);
    assert_eq!(header.zlib_frames, 0);
    let framing = golden.get("framing");
    assert_eq!(framing.get("text_only").as_bool(), Some(true));
    assert_eq!(
        Some(header.wmo_heading.as_str()),
        framing.get("wmo_heading").as_str()
    );
    assert_eq!(header.awips_id.as_deref(), framing.get("awips_id").as_str());

    // MetPy 1.7.1: product name and the SHA-256 of `Level3File.text`.
    let metpy = golden.get("metpy_detail");
    assert_eq!(
        metpy.get("product_name").as_str(),
        Some("Free Text Message")
    );
    assert_eq!(
        Some(sha256_hex(message.text.as_bytes()).as_str()),
        metpy.get("text_sha256").as_str()
    );
}

/// `decode_product` keeps reporting non-products as errors, and
/// `decode_message` returns exactly what `decode_product` returns for every
/// product in the corpus.
#[test]
fn decode_message_agrees_with_decode_product() {
    let (mut products, mut gsm, mut text) = (0, 0, 0);
    for entry in common::level3_manifest() {
        let bytes = entry.bytes();
        let id = &entry.id;
        match (decode_message(&bytes), decode_product(&bytes)) {
            (Ok(Level3Message::Product(message)), Ok(product)) => {
                assert_eq!(*message, product, "{id}");
                products += 1;
            }
            (Ok(Level3Message::GeneralStatus(status)), Err(Level3Error::NotAProduct { code })) => {
                assert_eq!(code, 2, "{id}");
                assert_eq!(status.message_header.code, 2, "{id}");
                assert!(GSM_FILES.contains(&id.as_str()), "{id}");
                gsm += 1;
            }
            (Ok(Level3Message::Text(message)), Err(Level3Error::TextOnly { heading })) => {
                assert_eq!(message.text_header.wmo_heading, heading, "{id}");
                assert_eq!(id, TEXT_FILE);
                text += 1;
            }
            (message, product) => {
                panic!("{id}: decode_message {message:?}, decode_product {product:?}")
            }
        }
    }
    assert_eq!((gsm, text), (3, 1));
    assert!(products > 200, "{products} products");
}

/// Corrupted General Status Messages (real files with bytes changed or cut)
/// return errors, never panic, and a too-short block length is rejected.
#[test]
fn corrupted_general_status_messages_fail_cleanly() {
    for id in GSM_FILES {
        let bytes = entry(id).bytes();
        let length_offset = 30 + 18 + 2;

        let mut short = bytes.clone();
        short[length_offset..length_offset + 2]
            .copy_from_slice(&(GSM_SHORT_BLOCK_BYTES - 2).to_be_bytes());
        match decode_message(&short) {
            Err(Level3Error::InvalidMessage { code: 2, reason }) => {
                assert!(reason.contains("80"), "{id}: {reason}");
            }
            other => panic!("{id}: {other:?}"),
        }

        let mut no_divider = bytes.clone();
        no_divider[30 + 18] = 0;
        assert!(
            matches!(
                decode_message(&no_divider),
                Err(Level3Error::BadBlockHeader { .. })
            ),
            "{id}"
        );

        for cut in 30..bytes.len() {
            match decode_message(&bytes[..cut]) {
                Err(Level3Error::Truncated { .. }) => {}
                other => panic!("{id} cut at {cut}: {other:?}"),
            }
        }
    }

    // The short-form message claiming the long block runs out of data.
    let mut bytes = entry("l3-tlx-gsm-20130520-2100").bytes();
    bytes[30 + 20..30 + 22].copy_from_slice(&GSM_BLOCK_BYTES.to_be_bytes());
    assert!(matches!(
        decode_message(&bytes),
        Err(Level3Error::Truncated {
            what: "general status block",
            needed: 182,
            available: 86,
            ..
        })
    ));
}

/// MetPy `BitField` rendering of a halfword: `null` when zero, the one name when
/// a single named bit is set, otherwise the list of names (least significant first).
fn metpy_bits(value: u16, names: &[&str]) -> Json {
    if value == 0 {
        return Json::Null;
    }
    let mut v = value;
    let mut set = Vec::new();
    for name in names {
        if v & 1 != 0 {
            set.push(Json::Str((*name).to_string()));
        }
        v >>= 1;
        if v == 0 {
            break;
        }
    }
    if set.len() == 1 {
        set.pop().unwrap()
    } else {
        Json::Arr(set)
    }
}

/// MetPy's `version()` for build numbers: `value / 100` above 200, otherwise
/// `value / 10`, formatted with one decimal.
fn metpy_version(value: u16) -> Json {
    let version = if value > 200 {
        f64::from(value) / 100.0
    } else {
        f64::from(value) / 10.0
    };
    Json::Str(format!("{version:.1}"))
}

fn int(value: impl Into<i64>) -> Json {
    Json::Num(value.into() as f64)
}

fn bools(n: usize, bit: impl Fn(usize) -> bool) -> Json {
    Json::Arr((0..n).map(|b| Json::Bool(bit(b))).collect())
}

/// JSON equality with numbers equal within 1e-9 (MetPy scales by 0.1 in floating point).
fn json_eq(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Num(x), Json::Num(y)) => (x - y).abs() <= 1e-9,
        (Json::Arr(x), Json::Arr(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_eq(p, q))
        }
        _ => a == b,
    }
}

// Script that produced METPY_GSM_ADDITIONAL (MetPy 1.7.1, run from the
// workspace root):
//
//     import json
//     from metpy.io import Level3File
//     for n in ['l3-ddc-gsm-20200817-1000', 'l3-eax-gsm-20200817-0933',
//               'l3-tlx-gsm-20130520-2100']:
//         f = Level3File(f'testdata/files/level3/{n}')
//         add = getattr(f, 'gsm_additional', None)
//         if add is None:
//             print(n, None); continue
//         d = add._asdict(); d.pop('spare')
//         print(n, json.dumps(d))

/// KNQA 2008 Radar Observation bulletin (heading `SDUS44 KWBC`, AWIPS
/// `ROBNQA`): plain text after a non-`NOUS` heading decodes as a text
/// message whose text is the file's bytes after the heading lines.
#[test]
fn radar_observation_bulletin_is_a_text_message() {
    let bytes =
        std::fs::read(recast_radar_testdata::path("l3-knqa-20080205-0018-rob").unwrap()).unwrap();
    // `SDUS44 KWBC 050018\r\r\nROBNQA\r\r\n` is 30 bytes.
    assert_eq!(&bytes[..30], b"SDUS44 KWBC 050018\r\r\nROBNQA\r\r\n");
    assert!(recast_radar_io_level3::looks_like_level3(&bytes));
    match decode_message(&bytes).unwrap() {
        Level3Message::Text(text) => {
            assert_eq!(text.text_header.wmo_heading, "SDUS44 KWBC 050018");
            assert_eq!(text.text_header.awips_id.as_deref(), Some("ROBNQA"));
            let expected: String = bytes[30..].iter().map(|&b| char::from(b)).collect();
            assert_eq!(text.text, expected);
            assert!(text.text.starts_with("\u{1e}NQA 0035 AREA 4RW++"));
        }
        other => panic!("expected a text message, got {other:?}"),
    }
    assert!(matches!(
        decode_product(&bytes),
        Err(Level3Error::TextOnly { .. })
    ));
}
