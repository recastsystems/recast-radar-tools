//! GBVTD tropical-cyclone retrieval on the Hurricane Ida (KLIX, 2021-08-29
//! 18:04Z) and Hurricane Fiona (TJUA, 2022-09-18 19:06Z) volumes.
//!
//! Expected values come from `testdata/golden/retrieve/gbvtd.json`
//! (`tools/retrieve_golden.py gbvtd`): the NHC HURDAT2 best track (centre
//! interpolated to the sweep time, 1-min surface wind, radius of maximum wind,
//! storm motion), and a numpy implementation of the documented ring fit on
//! Py-ART's region-based dealiased velocity of the same sweep.

// Test code panics on purpose: the workspace's unwrap/expect lints guard library code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{array, as_f64, as_str, as_usize, assert_close, golden, level2, moment};
use recast_radar_core::FieldName;
use recast_radar_retrieve::{
    PolarVelocityField, RingFit, TcCirculation, find_center_and_retrieve, retrieve_axisymmetric,
};
use serde_json::Value;

/// HURDAT2 positions are given to 0.1 degree (about 10 km) and a single Doppler radar at
/// 100-140 km range resolves the circulation centre to a few km, so 20 km is the
/// agreement a best-track comparison can claim.
const CENTRE_TOLERANCE_KM: f32 = 20.0;

struct Storm {
    case: Value,
    field: PolarVelocityField,
    centre: (f32, f32),
    motion: (f64, f64),
    radii: Vec<f32>,
}

fn storm(index: usize) -> Option<Storm> {
    let golden = golden("retrieve/gbvtd.json");
    let case = array(&golden["cases"])[index].clone();
    let id = as_str(&case["id"]).to_owned();
    let path = match recast_radar_testdata::path(&id) {
        Ok(path) => path,
        Err(error) if error.is_offline() => {
            eprintln!("skipping: {error}");
            return None;
        }
        Err(error) => panic!("{error}"),
    };
    let volume = level2(&path);
    let cut = &volume.sweeps[as_usize(&case["doppler_sweep"])];
    assert_eq!(cut.nrays(), as_usize(&case["doppler_sweep_rays"]));
    assert_close(
        f64::from(cut.ray_vars.nyquist_velocity_mps.as_ref().expect("Nyquist")[0]),
        as_f64(&case["doppler_sweep_nyquist_mps"]),
        0.05,
        "Nyquist",
    );
    // Py-ART takes TJUA's position from its station table (18.1175 N) rather than the
    // file's VOL block (18.1157 N): 0.2 km, far below the tolerances below.
    assert_close(
        volume.location.latitude_deg.expect("latitude"),
        as_f64(&case["radar"]["latitude_deg"]),
        0.005,
        "radar latitude",
    );
    let velocity = moment(cut, &FieldName::Vradh);
    let dealiased = recast_radar_correct::dealias_velocity(cut, velocity);
    let field = PolarVelocityField::from_dealiased_velocity(cut, &dealiased);
    let track = &case["best_track"];
    let centre = (
        as_f64(&track["east_km"]) as f32,
        as_f64(&track["north_km"]) as f32,
    );
    let motion = (
        as_f64(&track["motion_east_mps"]),
        as_f64(&track["motion_north_mps"]),
    );
    let radii: Vec<f32> = array(&case["rings"]["radii_km"])
        .iter()
        .map(|r| as_f64(r) as f32)
        .collect();
    Some(Storm {
        case,
        field,
        centre,
        motion,
        radii,
    })
}

fn distance_km(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Rings whose fit agrees with the reference fit on Py-ART's dealiased field: the same
/// sample count and VT/VR within `tolerance` m/s.
fn agreeing_rings<'a>(
    circulation: &'a TcCirculation,
    storm: &'a Storm,
    tolerance: f32,
) -> Vec<(&'a RingFit, &'a Value)> {
    let fits = array(&storm.case["rings"]["fits"]);
    assert_eq!(fits.len(), storm.radii.len());
    let mut agreeing = Vec::new();
    for (radius, expected) in storm.radii.iter().zip(fits) {
        let ring = circulation
            .rings
            .iter()
            .find(|ring| (ring.radius_km - radius).abs() < 1e-3);
        match (ring, expected.is_null()) {
            (None, true) => {}
            (None, false) => panic!("ring {radius} km missing; reference {expected}"),
            (Some(ring), true) => panic!("ring {radius} km fitted; reference has none: {ring:?}"),
            (Some(ring), false) => {
                if ring.samples == as_usize(&expected["samples"])
                    && (f64::from(ring.vt) - as_f64(&expected["vt"])).abs() <= f64::from(tolerance)
                    && (f64::from(ring.vr) - as_f64(&expected["vr"])).abs() <= f64::from(tolerance)
                {
                    agreeing.push((ring, expected));
                }
            }
        }
    }
    agreeing
}

#[test]
fn axisymmetric_retrieval_at_the_best_track_centre_matches_intensity_and_reference_rings() {
    for index in 0..2 {
        let Some(storm) = storm(index) else { return };
        let track = &storm.case["best_track"];
        let circulation =
            retrieve_axisymmetric(&storm.field, storm.centre, &storm.radii, 72, storm.motion);
        let vt_max = circulation.vt_max.expect("a peak tangential wind");
        let rmw = circulation.rmw_km.expect("a radius of maximum wind");
        let best_track_wind = as_f64(&track["wind_mps"]) as f32;
        let best_track_rmw = as_f64(&track["rmw_km"]) as f32;
        // The best track gives the 1-min surface wind; the 0.5 deg beam samples the
        // circulation 1.5-2 km up at these ranges, so agree to 15 m/s (a quarter of a
        // major hurricane's wind) and to a factor of two in the RMW (ring step 4 km).
        assert!(
            (vt_max - best_track_wind).abs() <= 15.0,
            "{}: VT max {vt_max} m/s vs best track {best_track_wind} m/s",
            as_str(&storm.case["storm"])
        );
        assert!(
            rmw >= 0.5 * best_track_rmw && rmw <= 2.0 * best_track_rmw,
            "{}: RMW {rmw} km vs best track {best_track_rmw} km",
            as_str(&storm.case["storm"])
        );
        // Ring by ring against the reference fit on Py-ART's dealiased field: the two
        // dealiasers agree on most rings (all 19 for Fiona, whose winds sit under its
        // Nyquist; 14 of 19 for Ida, whose eyewall folds twice), and where they agree
        // the fits match to 0.5 m/s.
        let agreeing = agreeing_rings(&circulation, &storm, 0.5);
        assert!(
            agreeing.len() * 3 >= storm.radii.len() * 2,
            "{}: only {} of {} rings match the reference",
            as_str(&storm.case["storm"]),
            agreeing.len(),
            storm.radii.len()
        );
        for (ring, expected) in &agreeing {
            assert_close(
                f64::from(ring.rms),
                as_f64(&expected["rms"]),
                0.5,
                "ring rms",
            );
        }
    }
}

#[test]
fn simplex_centre_search_recovers_the_best_track_centre() {
    for index in 0..2 {
        let Some(storm) = storm(index) else { return };
        // Start 8 km off the best-track centre in both axes and search +-24 km.
        let guess = (storm.centre.0 + 8.0, storm.centre.1 - 8.0);
        let circulation =
            find_center_and_retrieve(&storm.field, guess, 24.0, 2.0, &storm.radii, storm.motion)
                .expect("a circulation");
        let miss = distance_km(circulation.center_km, storm.centre);
        assert!(
            miss <= CENTRE_TOLERANCE_KM,
            "{}: centre {:?} is {miss:.1} km from the best track {:?}",
            as_str(&storm.case["storm"]),
            circulation.center_km,
            storm.centre
        );
        let vt_max = circulation.vt_max.expect("peak wind");
        let best_track_wind = as_f64(&storm.case["best_track"]["wind_mps"]) as f32;
        assert!(
            vt_max >= 0.6 * best_track_wind && vt_max <= 1.4 * best_track_wind,
            "{}: VT max {vt_max} vs best track {best_track_wind}",
            as_str(&storm.case["storm"])
        );
        // The searched centre scores at least as well as the best-track centre.
        let at_track =
            retrieve_axisymmetric(&storm.field, storm.centre, &storm.radii, 72, storm.motion);
        assert!(vt_max >= at_track.vt_max.expect("peak wind at the best-track centre") - 0.01);
    }
}

#[test]
fn wavenumber_one_asymmetry_matches_the_reference_decomposition() {
    // Fiona (index 1): a clean, fully sampled eyewall ring set with a 5-8 m/s
    // wavenumber-1 tangential asymmetry between 24 and 40 km; Ida (index 0): the same
    // check on whichever rings the dealiasers agree on.
    for index in [1usize, 0] {
        let Some(storm) = storm(index) else { return };
        let circulation =
            retrieve_axisymmetric(&storm.field, storm.centre, &storm.radii, 72, storm.motion);
        let agreeing = agreeing_rings(&circulation, &storm, 0.5);
        let mut checked = 0usize;
        for (ring, expected) in &agreeing {
            assert!(ring.vt1_amp.is_finite() && ring.vt1_amp < ring.vt.abs().max(1.0));
            assert_close(
                f64::from(ring.vt1_cos),
                as_f64(&expected["vt1_cos"]),
                0.5,
                "VT1 cos",
            );
            assert_close(
                f64::from(ring.vt1_sin),
                as_f64(&expected["vt1_sin"]),
                0.5,
                "VT1 sin",
            );
            assert_close(
                f64::from(ring.vt1_amp),
                as_f64(&expected["vt1_amp"]),
                0.5,
                "VT1 amplitude",
            );
            if ring.vt1_amp > 3.0 {
                let delta = (f64::from(ring.vt1_phase_deg) - as_f64(&expected["vt1_phase_deg"])
                    + 540.0)
                    % 360.0
                    - 180.0;
                assert!(
                    delta.abs() <= 10.0,
                    "VT1 phase {} vs {}",
                    ring.vt1_phase_deg,
                    expected["vt1_phase_deg"]
                );
                checked += 1;
            }
        }
        assert!(
            checked >= 3,
            "{}: only {checked} asymmetric rings checked",
            as_str(&storm.case["storm"])
        );
        if index == 1 {
            // Fiona's eyewall asymmetry: 5-8 m/s on the 24-40 km rings, a steady phase.
            let eyewall: Vec<&RingFit> = circulation
                .rings
                .iter()
                .filter(|ring| (24.0..=40.0).contains(&ring.radius_km))
                .collect();
            assert_eq!(eyewall.len(), 5);
            for ring in &eyewall {
                assert!(ring.vt1_amp >= 4.0 && ring.vt1_amp <= 9.0, "{ring:?}");
            }
            let phases: Vec<f32> = eyewall.iter().map(|ring| ring.vt1_phase_deg).collect();
            let spread = phases.iter().cloned().fold(f32::NEG_INFINITY, f32::max)
                - phases.iter().cloned().fold(f32::INFINITY, f32::min);
            assert!(spread <= 30.0, "eyewall wavenumber-1 phases {phases:?}");
        }
    }
}
