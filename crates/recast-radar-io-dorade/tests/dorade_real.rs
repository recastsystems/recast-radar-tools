//! Golden-fixture tests for the DORADE decoder against real radar bytes.
//!
//! `dorade-cow2-20260521-225514-sur-head24` is the first
//! 37,380 bytes (all descriptor blocks + the first 24 ray groups, cut at a
//! block boundary) of a real CSWR COW2 sweepfile from the 2026-05-21
//! deployment (Radx-written, big-endian, HRD RLE compressed, CSFD gate
//! geometry, staggered PRT). Expected values below were extracted with an
//! independent Python block walker + RLE decoder, not with this crate. The
//! file is not redistributed with the repository (its license is unknown),
//! so these tests skip unless a copy is in the testdata cache.

use chrono::{TimeZone, Utc};
use recast_radar_core::model::Quantity;
use recast_radar_io_dorade::dorade::{
    looks_like_dorade_bytes, peek_dorade_sweep, read_dorade_sweep_volume,
};

/// The COW2 sweep's bytes, or skip the test when the file is not available.
macro_rules! fixture {
    () => {
        std::fs::read(recast_radar_testdata::require_file!(
            "dorade-cow2-20260521-225514-sur-head24"
        ))
        .expect("read COW2 fixture")
    };
}

fn assert_close(actual: f32, expected: f32, tolerance: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} != {expected} (tolerance {tolerance})"
    );
}

#[test]
fn real_cow2_sweep_decodes_site_and_geometry() {
    let fixture = fixture!();
    assert!(looks_like_dorade_bytes(&fixture));
    let volume = read_dorade_sweep_volume(&fixture).expect("decode COW2 fixture");

    // Site identity and deployment coordinates come from RADD.
    assert_eq!(volume.attrs.instrument_name, "COW2");
    assert_close(
        volume.location.latitude_deg.unwrap() as f32,
        39.74,
        1e-4,
        "latitude",
    );
    assert_close(
        volume.location.longitude_deg.unwrap() as f32,
        -103.2927,
        1e-4,
        "longitude",
    );
    assert_close(
        volume.location.altitude_m.unwrap() as f32,
        1519.0,
        0.5,
        "altitude",
    );

    // Time reference from SSWB.
    assert_eq!(
        volume.time_reference,
        Utc.with_ymd_and_hms(2026, 5, 21, 22, 55, 14).unwrap()
    );
    assert_eq!(
        volume.provenance.compression.as_deref(),
        Some("dorade-hrd-rle")
    );

    // One sweep; the fixture's 24 rays are all kept, and the first 3 (RYIB
    // ray_status 1) are flagged as antenna transition.
    assert_eq!(volume.sweeps.len(), 1);
    let sweep = &volume.sweeps[0];
    assert_close(sweep.fixed_angle_deg, 1.005_255_6, 1e-5, "fixed angle");
    assert_eq!(sweep.nrays(), 24);
    let transition = sweep.ray_vars.antenna_transition.as_ref().unwrap();
    assert_eq!(transition[..4], [1, 1, 1, 0]);
    assert!(transition[3..].iter().all(|flag| *flag == 0));
    assert_eq!(volume.provenance.decode.skipped_message_count, 0);

    // CSFD gate geometry: 375 gates, first centre at 50 m, 100 m spacing.
    assert_eq!(sweep.range.ngates(), 375);
    assert_close(
        sweep.range.center_m(0).unwrap() as f32,
        50.0,
        1e-3,
        "gate 0",
    );
    assert_close(
        sweep.range.center_m(1).unwrap() as f32,
        150.0,
        1e-3,
        "gate 1",
    );

    // First ray: the transition ray at az 71.5, 22:55:14.229. First
    // scanning ray (file ray 3): az 73.0, el 0.8184814453125, 22:55:14.280
    // (0.280 s from the SSWB start).
    assert_close(sweep.rays.azimuth_deg[0], 71.5, 1e-4, "azimuth");
    assert_close(sweep.rays.time_s[0] as f32, 0.229, 1e-6, "ray time");
    assert_close(sweep.rays.azimuth_deg[3], 73.0, 1e-4, "azimuth");
    assert_close(
        sweep.rays.elevation_deg[3],
        0.818_481_4,
        1e-5,
        "ray elevation",
    );
    assert_close(sweep.rays.time_s[3] as f32, 0.280, 1e-6, "ray time");

    // RADD eff_unamb_vel (staggered-PRT extended Nyquist) on every ray.
    let nyquist = sweep
        .ray_vars
        .nyquist_velocity_mps
        .as_ref()
        .expect("nyquist");
    assert_eq!(nyquist.len(), 24);
    assert_close(nyquist[0], 68.76, 0.01, "nyquist");
    assert!(nyquist.iter().all(|value| value.is_finite()));
}

#[test]
fn real_cow2_sweep_decodes_known_moment_values() {
    let fixture = fixture!();
    let volume = read_dorade_sweep_volume(&fixture).expect("decode COW2 fixture");
    let sweep = &volume.sweeps[0];

    for quantity in [
        Quantity::Reflectivity,
        Quantity::RadialVelocity,
        Quantity::DifferentialReflectivity,
        Quantity::CorrelationCoefficient,
    ] {
        let field = sweep.find(quantity).unwrap_or_else(|| {
            panic!("missing {quantity:?}");
        });
        assert_eq!(field.nrays, 24, "{quantity:?} rows");
        assert!(field.absent_rows.is_empty(), "{quantity:?} rows");
        assert_eq!(field.ngates, 375, "{quantity:?} gates");
    }

    // Row 3 = file ray index 3, the first scanning ray. Raw i16 values from the
    // independent decoder: REF (scale 100) -3030, bad, -69; VEL (scale 100)
    // -586, 478, 452, ..., 5805; ZDR (scale 100) -189, bad, 221; RHOHV
    // (scale 10000) 3235, bad, 9759.
    let reflectivity = sweep.find(Quantity::Reflectivity).unwrap();
    assert_close(
        reflectivity.value(3, 0).unwrap(),
        -30.30,
        1e-3,
        "REF gate 0",
    );
    assert_eq!(reflectivity.value(3, 50), None, "REF gate 50 is bad");
    assert_close(
        reflectivity.value(3, 100).unwrap(),
        -0.69,
        1e-3,
        "REF gate 100",
    );

    let velocity = sweep.find(Quantity::RadialVelocity).unwrap();
    assert_close(velocity.value(3, 0).unwrap(), -5.86, 1e-3, "VEL 0");
    assert_close(velocity.value(3, 50).unwrap(), 4.78, 1e-3, "VEL 50");
    assert_close(velocity.value(3, 100).unwrap(), 4.52, 1e-3, "VEL 100");
    assert_close(
        velocity.value(3, 374).unwrap(),
        58.05,
        1e-3,
        "VEL 374 (last gate)",
    );

    let zdr = sweep.find(Quantity::DifferentialReflectivity).unwrap();
    assert_close(zdr.value(3, 0).unwrap(), -1.89, 1e-3, "ZDR 0");
    assert_close(zdr.value(3, 100).unwrap(), 2.21, 1e-3, "ZDR 100");

    let rhohv = sweep.find(Quantity::CorrelationCoefficient).unwrap();
    assert_close(rhohv.value(3, 0).unwrap(), 0.3235, 1e-4, "RHO 0");
    assert_close(rhohv.value(3, 100).unwrap(), 0.9759, 1e-4, "RHO 100");
}

#[test]
fn real_cow2_sweep_header_peek_matches_full_decode() {
    let fixture = fixture!();
    let header = peek_dorade_sweep(&fixture).expect("peek COW2 fixture");
    assert_eq!(header.instrument, "COW2");
    assert_eq!(header.volume_number, 215);
    assert_eq!(header.sweep_number, 6);
    assert_close(header.fixed_angle_deg, 1.005_255_6, 1e-5, "fixed angle");
    assert_eq!(
        header.start_time,
        Some(Utc.with_ymd_and_hms(2026, 5, 21, 22, 55, 14).unwrap())
    );
}

/// Whole-corpus regression: decode every deployment zip and loose sweepfile
/// under the local mobile-radar corpus (DOW7 Goshen 2009, RaXPol Sulphur
/// 2016, COW2/DOW7low Goodland + COW2 deployment 2026, GR2 msg31 twins).
#[test]
#[ignore = "requires BOWECHO_MOBILE_RADAR_DIR"]
fn mobile_radar_corpus_decodes_every_archive() {
    let Some(corpus) = std::env::var_os("BOWECHO_MOBILE_RADAR_DIR").map(std::path::PathBuf::from)
    else {
        eprintln!("skipping corpus test; BOWECHO_MOBILE_RADAR_DIR is not set");
        return;
    };
    if !corpus.is_dir() {
        eprintln!("skipping corpus test; {} not found", corpus.display());
        return;
    }

    let mut archives = 0usize;
    let mut volumes = 0usize;
    for entry in std::fs::read_dir(&corpus)
        .expect("read corpus dir")
        .flatten()
    {
        let path = entry.path();
        if !recast_radar_io_dorade::mobile_archive::looks_like_zip_path(&path) {
            continue;
        }
        archives += 1;
        let decoded = recast_radar_io_dorade::mobile_archive::read_mobile_archive_from_path(
            &path,
            recast_radar_io_nexrad::read_volume_from_bytes,
        )
        .unwrap_or_else(|err| panic!("decode {}: {err}", path.display()));
        assert!(!decoded.is_empty(), "{} has no volumes", path.display());
        for entry in &decoded {
            let volume = &entry.volume;
            assert!(!volume.attrs.instrument_name.is_empty());
            assert!(
                !volume.sweeps.is_empty(),
                "{} empty volume",
                entry.member_label
            );
            assert!(
                volume.location.latitude_deg.is_some() && volume.location.longitude_deg.is_some(),
                "{} missing site coords",
                entry.member_label
            );
            for sweep in &volume.sweeps {
                assert!(sweep.nrays() > 0);
                assert!(!sweep.fields.is_empty());
                for field in &sweep.fields {
                    assert_eq!(field.nrays as usize, sweep.nrays());
                }
            }
        }
        volumes += decoded.len();
        eprintln!("{}: {} volumes", path.display(), decoded.len());
    }
    assert!(archives > 0, "no zip archives found in corpus");
    eprintln!("corpus total: {archives} archives, {volumes} volumes");
}
