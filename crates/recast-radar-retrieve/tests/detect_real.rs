//! Mesocyclone / TVS detection on real tornadic and clear-air volumes.
//!
//! Expected positions come from `testdata/golden/retrieve/detect.json`
//! (`tools/retrieve_golden.py detect`): the SPC tornado database path of each
//! tornado interpolated at constant speed over the surveyed duration to the
//! Py-ART ray time of the lowest Doppler sweep, in radar-relative coordinates
//! from the Py-ART radar position; Py-ART reflectivity statistics for the
//! clear-air volumes; MetPy sweep counts for the trimmed single-tilt file.

mod common;

use common::{array, as_f64, as_str, as_usize, find_case, golden, level2};
use recast_radar_core::{Field, Quantity, Volume};
use recast_radar_retrieve::{
    RotationSite, RotationStrength, detect_rotation_sites, detect_rotation_sites_from_dealiased,
    rotation_features_per_tilt, rotation_velocity_sweep_indices,
};
use recast_radar_testdata::require_file;
use serde_json::Value;

/// The SPC path interpolation, the survey's start-time rounding and the sweep timing put
/// the tornado within a few km of the expected point; the detector reports the lowest
/// tilt's feature position on a 0.25 km x 0.5 deg grid.
const POSITION_TOLERANCE_KM: f64 = 6.0;

fn east_north_km(site: &RotationSite) -> (f64, f64) {
    let azimuth = f64::from(site.azimuth_deg).to_radians();
    let range_km = site.ground_range_m / 1000.0;
    (range_km * azimuth.sin(), range_km * azimuth.cos())
}

fn distance_to_expected_km(site: &RotationSite, case: &Value) -> f64 {
    let (east, north) = east_north_km(site);
    ((east - as_f64(&case["east_km"])).powi(2) + (north - as_f64(&case["north_km"])).powi(2)).sqrt()
}

fn tornado_volume(golden: &Value, id: &str) -> Option<(Volume, Value)> {
    let case = find_case(&golden["tornadoes"], &[("id", id)]).clone();
    let path = match recast_radar_testdata::path(id) {
        Ok(path) => path,
        Err(error) if error.is_offline() => {
            eprintln!("skipping: {error}");
            return None;
        }
        Err(error) => panic!("{error}"),
    };
    let volume = level2(&path);
    // Py-ART reports the volume start to the second (from the first radial), as
    // the model's time reference does.
    let expected = chrono::DateTime::parse_from_rfc3339(as_str(&case["volume_time"]))
        .expect("golden volume time");
    let delta = (volume.time_reference - expected.with_timezone(&chrono::Utc)).num_milliseconds();
    assert!(
        delta.abs() <= 1000,
        "volume time {} vs {expected}",
        volume.time_reference
    );
    Some((volume, case))
}

#[test]
fn violent_tornadoes_are_detected_where_the_damage_survey_puts_them() {
    // Rolling Fork, MS (EF4, KDGX at 108 km), Stanton, NE (EF4, KOAX at 103 km) and
    // Moore, OK (EF5, KTLX at 21 km): the strongest site of each volume is a
    // vertically continuous TVS-class circulation within a few km of the surveyed
    // tornado position at the sweep time. Moore is close enough to the radar that
    // its 3 km rank core reaches the 6-20 deg tilts.
    let golden = golden("retrieve/detect.json");
    for id in [
        "l2-kdgx-20230325-010651",
        "l2-koax-20140616-205305",
        "l2-ktlx-20130520-201643",
    ] {
        let Some((volume, case)) = tornado_volume(&golden, id) else {
            return;
        };
        assert!(matches!(as_str(&case["spc"]["mag"]), "4" | "5"));
        let sites = detect_rotation_sites(&volume);
        assert!(!sites.is_empty(), "{id}: no rotation sites");
        let best = &sites[0];
        let miss = distance_to_expected_km(best, &case);
        assert!(
            miss <= POSITION_TOLERANCE_KM,
            "{id}: strongest site at az {:.1} deg, {:.1} km is {miss:.1} km from the survey \
             position (az {} deg, {} km): {best:?}",
            best.azimuth_deg,
            best.ground_range_m / 1000.0,
            case["azimuth_deg"],
            case["range_km"]
        );
        assert!(
            matches!(
                best.strength,
                RotationStrength::Tvs | RotationStrength::Mesocyclone
            ),
            "{id}: {best:?}"
        );
        assert!(best.depth_tilts >= 3, "{id}: {best:?}");
        assert!(best.depth_m >= 3_000.0, "{id}: {best:?}");
        assert!(best.gate_to_gate_dv_mps >= 25.0, "{id}: {best:?}");
        assert!(best.vrot_mps >= 20.0, "{id}: {best:?}");
        // The lowest-tilt feature sits on the lowest Doppler tilt of the volume.
        let lowest = rotation_velocity_sweep_indices(&volume)[0];
        let lowest_elevation = volume.tilt_elevation_deg(lowest).expect("sweep");
        assert!((best.base_elevation_deg - lowest_elevation).abs() < 0.05);
        // No other site is reported within the survey tolerance (one tornado, one site).
        let nearby = sites
            .iter()
            .filter(|site| distance_to_expected_km(site, &case) <= POSITION_TOLERANCE_KM)
            .count();
        assert_eq!(nearby, 1, "{id}: {sites:?}");
    }
}

#[test]
fn explicit_rotation_api_never_falls_back_to_an_internal_engine() {
    let golden = golden("retrieve/detect.json");
    let Some((volume, case)) = tornado_volume(&golden, "l2-kdgx-20230325-010651") else {
        return;
    };
    // No caller-provided grids: nothing to inspect, no hidden dealias pass.
    let missing: Vec<Option<&Field>> = vec![None; volume.sweeps.len()];
    assert!(detect_rotation_sites_from_dealiased(&volume, &missing).is_empty());

    // The crate's own dealiased grids supplied explicitly reproduce the internal path.
    let owned: Vec<Option<Field>> = volume
        .sweeps
        .iter()
        .map(|cut| {
            cut.find(Quantity::RadialVelocity)
                .map(|velocity| recast_radar_correct::dealias_velocity(cut, velocity))
        })
        .collect();
    let supplied: Vec<Option<&Field>> = owned.iter().map(Option::as_ref).collect();
    let explicit = detect_rotation_sites_from_dealiased(&volume, &supplied);
    let internal = detect_rotation_sites(&volume);
    assert_eq!(explicit.len(), internal.len());
    for (a, b) in explicit.iter().zip(&internal) {
        assert_eq!(a.azimuth_deg, b.azimuth_deg);
        assert_eq!(a.ground_range_m, b.ground_range_m);
        assert_eq!(a.rank, b.rank);
        assert_eq!(a.strength, b.strength);
    }
    assert!(distance_to_expected_km(&explicit[0], &case) <= POSITION_TOLERANCE_KM);

    // The raw (folded) velocity grids supplied explicitly are consumed as given: sites
    // are still found, but the aliased gates change the 2D features, so the result is
    // not the internal (dealiased) one.
    let raw: Vec<Option<&Field>> = volume
        .sweeps
        .iter()
        .map(|cut| cut.find(Quantity::RadialVelocity))
        .collect();
    let from_raw = detect_rotation_sites_from_dealiased(&volume, &raw);
    assert!(!from_raw.is_empty());
    let same = from_raw.len() == internal.len()
        && from_raw.iter().zip(&internal).all(|(a, b)| {
            a.azimuth_deg == b.azimuth_deg
                && a.ground_range_m == b.ground_range_m
                && a.vrot_mps == b.vrot_mps
        });
    assert!(
        !same,
        "raw and dealiased grids gave identical sites: {from_raw:?}"
    );
}

#[test]
fn single_doppler_tilt_yields_features_but_no_site() {
    // The trimmed Moore volume keeps one Doppler tilt: its 2D features (the Moore
    // circulation among them) cannot be associated vertically.
    let golden = golden("retrieve/detect.json");
    let case = &golden["single_doppler_tilt"];
    let path = require_file!(as_str(&case["id"]));
    let volume = level2(&path);
    assert_eq!(volume.sweeps.len(), as_usize(&case["sweeps"]));
    assert_eq!(
        rotation_velocity_sweep_indices(&volume).len(),
        as_usize(&case["velocity_sweeps"])
    );
    assert_eq!(as_usize(&case["velocity_sweeps"]), 1);
    let per_tilt = rotation_features_per_tilt(&volume);
    assert_eq!(per_tilt.len(), 1);
    assert!(
        per_tilt[0].1 > 0,
        "no 2D features on the Doppler tilt: {per_tilt:?}"
    );
    assert!(detect_rotation_sites(&volume).is_empty());
}

#[test]
fn circulations_without_echo_are_rejected() {
    // The Rolling Fork volume with every reflectivity grid removed: the same couplets
    // now sit in "empty" air, and the QC mask refuses all of them (the "20 circles on
    // an empty radar" failure mode).
    let golden = golden("retrieve/detect.json");
    let Some((mut volume, _)) = tornado_volume(&golden, "l2-kdgx-20230325-010651") else {
        return;
    };
    assert!(!detect_rotation_sites(&volume).is_empty());
    for cut in &mut volume.sweeps {
        cut.fields
            .retain(|field| field.quantity != Quantity::Reflectivity);
    }
    assert!(
        rotation_velocity_sweep_indices(&volume).len() >= 2,
        "velocity tilts survive the reflectivity removal"
    );
    assert!(detect_rotation_sites(&volume).is_empty());
    assert!(
        rotation_features_per_tilt(&volume)
            .iter()
            .all(|(_, features, rank)| *features == 0 && *rank == 0)
    );
}

#[test]
fn quiet_volumes_detect_nothing() {
    // Clear-air VCPs (35 and 31) with biological and ground-clutter returns only.
    let golden = golden("retrieve/detect.json");
    for case in array(&golden["quiet"]) {
        let path = require_file!(as_str(&case["id"]));
        let volume = level2(&path);
        assert_eq!(volume.scan.vcp_pattern, Some(as_usize(&case["vcp"]) as u16));
        assert_eq!(volume.sweeps.len(), as_usize(&case["sweeps"]));
        // No precipitation: the strongest returns (clutter, biota) stay under 50 dBZ.
        assert!(as_f64(&case["max_reflectivity_dbz"]) < 50.0);
        assert!(rotation_velocity_sweep_indices(&volume).len() >= 2);
        let sites = detect_rotation_sites(&volume);
        assert!(sites.is_empty(), "{}: {sites:?}", case["id"]);
    }
}
