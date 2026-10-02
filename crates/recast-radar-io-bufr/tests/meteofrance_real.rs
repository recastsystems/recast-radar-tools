//! Meteo-France radar files against numpy_bufr, an independent BUFR decoder
//! (`tools/meteofrance_bufr_golden.py`, goldens under
//! `testdata/conformance/meteofrance/`), and the volumes they make.
//!
//! The files are not redistributed (`testdata/other/manifest.toml`): every
//! test skips when they are not in the testdata cache.

// A panic is how a test fails (clippy.toml), in helpers too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use recast_radar_core::model::{FieldName, Volume, merge_volumes};
use recast_radar_io_bufr::{
    Item, Tables, decode_message, expand, looks_like_bufr_bytes, messages, read_meteofrance_volume,
};
use serde_json::Value as Json;

const PAG_ARCIS_C: &str = "meteofrance-pag-07168-20130619-1200-c";
const PAG_BLAISY_E: &str = "meteofrance-pag-07274-20130619-1200-e";
const PAG_BLAISY_A: &str = "meteofrance-pag-07274-20130619-1200-a";
const PAM_BLAISY_A: &str = "meteofrance-pam-07274-20130619-1200-a";
const PAG_BLAISY_VERTICAL: &str = "meteofrance-pag-07274-20130619-1205-a";
const ALL: [&str; 5] = [
    PAG_ARCIS_C,
    PAG_BLAISY_E,
    PAG_BLAISY_A,
    PAM_BLAISY_A,
    PAG_BLAISY_VERTICAL,
];

fn golden(id: &str) -> Json {
    let path = recast_radar_testdata::workspace_root()
        .join("testdata/conformance/meteofrance")
        .join(format!("{id}.json"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn first(items: &[Item], descriptor: u32) -> Option<f64> {
    items
        .iter()
        .find(|item| item.descriptor() == descriptor)?
        .number()
}

/// Every polar image numpy_bufr decodes, ours the same: geometry, time,
/// station, scaling, the code table, and the pixel codes (count, sum and a
/// position-weighted sum, all-ones codes included as numpy_bufr gives them).
#[test]
fn images_match_numpy_bufr() {
    for id in ALL {
        let Some(bytes) = recast_radar_testdata::bytes_if_available(id) else {
            continue;
        };
        let expanded = expand(&bytes).unwrap();
        let messages = messages(&expanded).unwrap();
        let golden = golden(id);
        for image in golden["images"].as_array().unwrap() {
            let index = image["message"].as_u64().unwrap() as usize;
            let at = format!("{id} message {index}");
            let message = &messages[index];
            let items = decode_message(
                &message.descriptors,
                message.data,
                message.subsets,
                message.compressed,
                Tables::for_message(message.centre, message.local_version),
            )
            .unwrap();
            let number = |key: &str| image[key].as_f64();
            assert_eq!(first(&items, 2135), number("elevation_deg"), "{at}");
            assert_eq!(first(&items, 30022), number("rays"), "{at}");
            assert_eq!(first(&items, 30021), number("gates"), "{at}");
            assert_eq!(first(&items, 55233), number("gate_m"), "{at}");
            assert_eq!(first(&items, 5001), number("latitude"), "{at}");
            assert_eq!(first(&items, 6001), number("longitude"), "{at}");
            assert_eq!(first(&items, 49241), number("velocity_minimum"), "{at}");
            assert_eq!(first(&items, 49231), number("velocity_step"), "{at}");
            let station = first(&items, 1001).unwrap() * 1000.0 + first(&items, 1002).unwrap();
            assert_eq!(Some(station), number("station"), "{at}");
            let time: Vec<u32> = [4001, 4002, 4003, 4004, 4005, 4006]
                .iter()
                .map(|d| first(&items, *d).unwrap() as u32)
                .collect();
            let time = format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                time[0], time[1], time[2], time[3], time[4], time[5]
            );
            assert_eq!(Some(time.as_str()), image["time"].as_str(), "{at}");

            // The pixel codes.
            let rays = image["rays"].as_u64().unwrap() as usize;
            let gates = image["gates"].as_u64().unwrap() as usize;
            let codes = items
                .iter()
                .rev()
                .find_map(|item| match item {
                    Item::Run {
                        descriptor: 30001,
                        codes,
                        ..
                    } if codes.len() == rays * gates => Some(codes),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{at}: no pixel run"));
            let expected = &image["codes"];
            let sum: u64 = codes.iter().map(|&c| u64::from(c)).sum();
            let weighted: u64 = codes
                .iter()
                .enumerate()
                .map(|(i, &c)| u64::from(c) * (i as u64 % 9973 + 1))
                .sum();
            assert_eq!(Some(codes.len() as u64), expected["count"].as_u64(), "{at}");
            assert_eq!(Some(sum), expected["sum"].as_u64(), "{at}");
            assert_eq!(Some(weighted), expected["weighted_sum"].as_u64(), "{at}");
            assert_eq!(
                codes.iter().max().map(|&c| u64::from(c)),
                expected["max"].as_u64(),
                "{at}"
            );

            // The code table: numpy_bufr keeps each reflectivity class's
            // upper bound; the PAG standard deviation has one value a code.
            match image["table"].as_array() {
                Some(table) if table.first().is_some_and(Json::is_array) => {
                    let start = items.iter().position(|i| i.descriptor() == 31002).unwrap();
                    let triples: Vec<(f64, f64)> = items[start + 1..]
                        .chunks_exact(3)
                        .take(table.len())
                        .map(|t| (t[0].number().unwrap(), t[2].number().unwrap()))
                        .collect();
                    let expected: Vec<(f64, f64)> = table
                        .iter()
                        .map(|row| (row[0].as_f64().unwrap(), row[1].as_f64().unwrap()))
                        .collect();
                    assert_eq!(triples, expected, "{at}");
                }
                Some(table) => {
                    let Some(Item::Run {
                        codes,
                        scale,
                        reference,
                        ..
                    }) = items.iter().find(|item| {
                        matches!(
                            item,
                            Item::Run {
                                descriptor: 21216,
                                ..
                            }
                        )
                    })
                    else {
                        panic!("{at}: no 0-21-216 list");
                    };
                    let ours: Vec<f64> = codes
                        .iter()
                        .map(|&c| (f64::from(c) + *reference as f64) / 10f64.powi(*scale))
                        .collect();
                    let expected: Vec<f64> = table.iter().map(|v| v.as_f64().unwrap()).collect();
                    assert_eq!(ours, expected, "{at}");
                }
                None => {}
            }
        }
    }
}

fn read(id: &str) -> Option<Volume> {
    let bytes = recast_radar_testdata::bytes_if_available(id)?;
    Some(read_meteofrance_volume(&bytes).unwrap_or_else(|err| panic!("{id}: {err}")))
}

fn names(volume: &Volume) -> Vec<Vec<String>> {
    volume
        .sweeps
        .iter()
        .map(|sweep| sweep.fields.iter().map(|f| f.name.to_string()).collect())
        .collect()
}

fn range_of(volume: &Volume, name: &FieldName) -> (f32, f32, usize) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    let mut n = 0;
    for sweep in &volume.sweeps {
        if let Some(field) = sweep.field(name) {
            for ray in 0..field.nrays as usize {
                for gate in 0..field.ngates as usize {
                    if let Some(value) = field.value(ray, gate) {
                        lo = lo.min(value);
                        hi = hi.max(value);
                        n += 1;
                    }
                }
            }
        }
    }
    (lo, hi, n)
}

/// A PAG file: a reflectivity sweep of 720 rays and a Doppler sweep of 360
/// at one elevation; a PAM file: one sweep of 720 rays of 240 m with the
/// dual-polarization fields; values within each code's range.
#[test]
fn files_make_sweeps_of_their_images() {
    if let Some(volume) = read(PAG_ARCIS_C) {
        assert_eq!(volume.attrs.instrument_name, "07168");
        assert_eq!(names(&volume), [vec!["DBZH"], vec!["DBZH_SD", "VRADH"]]);
        let doppler = &volume.sweeps[1];
        assert_eq!((volume.sweeps[0].nrays(), doppler.nrays()), (720, 360));
        assert!((doppler.rays.azimuth_deg[0] - 0.5).abs() < 1e-6);
        assert!((volume.sweeps[0].rays.azimuth_deg[0] - 0.25).abs() < 1e-6);
        let nyquist = doppler.ray_vars.nyquist_velocity_mps.as_ref().unwrap();
        assert!((nyquist[0] - 60.2).abs() < 1e-4);
        let (lo, hi, _) = range_of(&volume, &FieldName::Vradh);
        assert!(lo >= -60.25 && hi <= 60.85, "{lo} {hi}");
        let (lo, hi, _) = range_of(&volume, &FieldName::Dbzh);
        assert!(lo >= -9.0 && hi <= 68.0, "{lo} {hi}");
        assert_eq!(
            volume.time_reference.to_rfc3339(),
            "2013-06-19T12:00:05+00:00"
        );
    }
    if let Some(volume) = read(PAM_BLAISY_A) {
        assert_eq!(names(&volume), [vec!["DBZH", "RHOHV", "PHIDP", "ZDR"]]);
        let sweep = &volume.sweeps[0];
        assert_eq!((sweep.nrays(), sweep.range.ngates()), (720, 1066));
        assert_eq!(sweep.range.spacing_m(), Some(240.0));
        // The Cartesian image and the rain accumulation are left out.
        assert_eq!(volume.provenance.decode.message_count, 6);
        assert_eq!(volume.provenance.decode.skipped_message_count, 2);
        let (lo, hi, _) = range_of(&volume, &FieldName::Rhohv);
        assert!(lo >= 0.3 - 1e-6 && hi <= 1.09, "{lo} {hi}");
        let (lo, hi, _) = range_of(&volume, &FieldName::Phidp);
        assert!(lo >= 0.0 && hi <= 359.0, "{lo} {hi}");
        let (lo, hi, _) = range_of(&volume, &FieldName::Zdr);
        assert!(lo >= -10.0 && hi <= 9.9 + 1e-4, "{lo} {hi}");
    }
    if let Some(volume) = read(PAG_BLAISY_VERTICAL) {
        assert!(
            volume
                .sweeps
                .iter()
                .all(|sweep| sweep.fixed_angle_deg == 90.0)
        );
        assert!(
            volume
                .sweeps
                .iter()
                .all(|sweep| sweep.sweep_mode
                    == recast_radar_core::model::SweepMode::VerticalPointing)
        );
    }
}

/// The PAM and PAG files of one elevation merge, in either order, into the
/// PAM sweep (its 240 m reflectivity with the dual-polarization fields) and
/// the PAG Doppler sweep; the PAG 1 km reflectivity is the field left out.
#[test]
fn pam_and_pag_merge_in_either_order() {
    let (Some(pam), Some(pag)) = (read(PAM_BLAISY_A), read(PAG_BLAISY_A)) else {
        return;
    };
    for parts in [vec![pam.clone(), pag.clone()], vec![pag, pam]] {
        let (volume, report) = merge_volumes(parts).unwrap();
        assert_eq!(
            names(&volume),
            [
                vec!["DBZH", "RHOHV", "PHIDP", "ZDR"],
                vec!["DBZH_SD", "VRADH"]
            ]
        );
        assert_eq!(volume.sweeps[0].range.spacing_m(), Some(240.0));
        assert_eq!(report.field_collisions, 1);
    }
}

/// The gzip members of a real file: sniffed as BUFR from its first 512
/// bytes; a damaged CRC-32, a truncated member and bytes after the members
/// are refused. A gzip file of another format is not BUFR.
#[test]
fn containers_are_checked() {
    let Some(bytes) = recast_radar_testdata::bytes_if_available(PAG_BLAISY_A) else {
        return;
    };
    assert!(looks_like_bufr_bytes(&bytes[..512]));
    assert_eq!(messages(&expand(&bytes).unwrap()).unwrap().len(), 3);
    let mut damaged = bytes.clone();
    let n = damaged.len();
    damaged[n - 6] ^= 0x10; // the last member's CRC-32
    assert!(expand(&damaged).is_err());
    assert!(expand(&bytes[..bytes.len() - 100]).is_err());
    let mut trailing = bytes.clone();
    trailing.extend_from_slice(&bytes[10..40]);
    assert!(expand(&trailing).is_err());
    let level2 = recast_radar_testdata::bytes("l2-ktlx-20130520-201643").unwrap();
    assert!(!looks_like_bufr_bytes(&level2));
}
