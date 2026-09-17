//! Rotation tracks and TDS flags on the KTLX 2013-05-20 20:16Z volume (Moore EF5
//! tornado 22 km west of the radar).
//!
//! Expected values: `testdata/golden/track/tracks.json`, written by
//! `tools/track_golden.py` (section `tracks`) from Py-ART: the strongest
//! low-level cyclonic azimuthal shear on the region-based-dealiased 0.5 deg
//! velocity, a no-echo location and a calm in-echo location on the same sweep,
//! the 4/3-Earth range where that beam leaves the 0-2 km layer with a strong
//! echo beyond it and one inside it, and every gate of the lowest dual-pol
//! sweep within 5 km of the circulation that meets the debris criterion
//! (RHOHV < 0.82 in > 30 dBZ echo).

// Test code panics on purpose: the workspace's unwrap/expect lints guard library code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{array, as_f64, as_str, as_usize, distance_km, golden, level2};
use recast_radar_core::Quantity;
use recast_radar_retrieve::{RotationSite, RotationStrength, detect_rotation_sites};
use recast_radar_testdata::require_file;
use recast_radar_track::tracks::{
    TDS_ANCHOR_RADIUS_KM, TDS_CC_MAX, TDS_MIN_DBZ, TRACK_DISPLAY_FLOOR_E3, TracksGridSpec,
    detect_tds_gates, low_level_azshear_cartesian, low_level_azshear_sweep_indices,
    max_composite_into, tds_anchor,
};
use serde_json::Value;

fn point(value: &Value) -> (f64, f64) {
    (as_f64(&value["east_km"]), as_f64(&value["north_km"]))
}

fn at(frame: &[f32], spec: &TracksGridSpec, point: (f64, f64)) -> f32 {
    frame[spec
        .cell_index(point.0 as f32, point.1 as f32)
        .expect("point inside the grid")]
}

/// The Moore circulation as a TDS anchor: position from Py-ART's shear maximum,
/// strength of the tornado it was (a TVS).
fn moore_anchor(circulation: &Value) -> RotationSite {
    RotationSite {
        azimuth_deg: as_f64(&circulation["azimuth_deg"]) as f32,
        ground_range_m: as_f64(&circulation["range_km"]) * 1000.0,
        vrot_mps: 50.0,
        gate_to_gate_dv_mps: 100.0,
        rank: 5,
        depth_tilts: 3,
        depth_m: 4_000.0,
        base_elevation_deg: 0.5,
        strength: RotationStrength::Tvs,
    }
}

/// The low-level shear frame carries display-strength cyclonic shear at the
/// tornado (its maximum inside 60 km lies within 2 km of Py-ART's shear peak),
/// nothing inside the 5 km clutter floor, nothing where the low-level sweeps
/// have no data, nothing on display where they have no echo, and a finite value
/// below the display floor inside calm echo.
#[test]
fn cartesian_frame_paints_couplet_location() {
    let golden = golden("tracks.json");
    let expected = &golden["moore"];
    let path = require_file!(as_str(&expected["entry"]));
    let volume = level2(&path);

    let cuts = low_level_azshear_sweep_indices(&volume);
    assert_eq!(
        cuts.first().copied(),
        Some(as_usize(&expected["doppler_sweep"]))
    );
    assert!(cuts.len() <= 3 && cuts.len() >= 2, "{cuts:?}");
    for pair in cuts.windows(2) {
        assert!(volume.sweeps[pair[0]].fixed_angle_deg < volume.sweeps[pair[1]].fixed_angle_deg);
    }
    for &index in &cuts {
        let cut = &volume.sweeps[index];
        assert!(cut.fixed_angle_deg <= 2.0 && cut.find(Quantity::RadialVelocity).is_some());
    }
    let doppler = &volume.sweeps[cuts[0]];
    assert!(
        (f64::from(doppler.fixed_angle_deg) - as_f64(&expected["doppler_elevation_deg"])).abs()
            < 0.05
    );

    let spec = TracksGridSpec::default();
    assert_eq!(spec.size(), 600);
    let frame = low_level_azshear_cartesian(&volume, &spec);
    assert_eq!(frame.len(), spec.cell_count());

    let circulation = point(&expected["circulation"]);
    assert!(
        as_f64(&expected["circulation"]["shear_e3"]) > 100.0,
        "Py-ART sees a TVS-strength couplet"
    );
    let couplet = at(&frame, &spec, circulation);
    assert!(
        couplet.is_finite() && couplet >= TRACK_DISPLAY_FLOOR_E3,
        "display-strength shear at the tornado: {couplet}"
    );
    let size = spec.size();
    let (peak_index, peak) = frame
        .iter()
        .enumerate()
        .filter(|(index, v)| {
            let (e, n) = spec.cell_center_km(index % size, index / size);
            v.is_finite() && e.hypot(n) <= 60.0
        })
        .max_by(|a, b| a.1.total_cmp(b.1))
        .expect("finite cells");
    let (peak_e, peak_n) = spec.cell_center_km(peak_index % size, peak_index / size);
    let offset = distance_km((f64::from(peak_e), f64::from(peak_n)), circulation);
    assert!(
        offset <= 2.0 && *peak > 10.0 * TRACK_DISPLAY_FLOOR_E3,
        "frame maximum {peak} at ({peak_e}, {peak_n}) is {offset:.1} km from Py-ART's shear peak"
    );
    // Clutter floor: nothing inside 5 km, whatever the data.
    for (east, north) in [(2.0, 2.0), (-3.0, 1.0), (0.5, -4.5), (-4.9, 0.0)] {
        assert!(
            at(&frame, &spec, (east, north)).is_nan(),
            "({east}, {north}) inside 5 km"
        );
    }
    // Neither echo nor velocity within 3 km on any low-level sweep: no data.
    let no_data = at(&frame, &spec, point(&expected["no_data"]));
    assert!(no_data.is_nan(), "no-data cell holds {no_data}");
    // Velocity but no echo reaching 20 dBZ within 3 km: the reflectivity floor
    // keeps clear-air shear off the display.
    let no_echo = at(&frame, &spec, point(&expected["no_echo"]));
    assert!(
        no_echo.is_nan() || no_echo < TRACK_DISPLAY_FLOOR_E3,
        "clear-air cell paints {no_echo}"
    );
    // Calm echo (>= 20 dBZ all around, |shear| < 2.5e-3 s^-1 on the raw
    // velocity): data present, nothing to display.
    let calm = at(&frame, &spec, point(&expected["calm_echo"]));
    assert!(
        (0.0..TRACK_DISPLAY_FLOOR_E3).contains(&calm),
        "calm in-echo cell reads {calm}"
    );
    // Only cyclonic shear is kept.
    assert!(frame.iter().all(|v| !v.is_finite() || *v >= 0.0));
    // One frame folded into an empty accumulator is the frame itself.
    let mut accumulator = vec![f32::NAN; frame.len()];
    max_composite_into(&mut accumulator, &frame);
    assert!(
        accumulator
            .iter()
            .zip(&frame)
            .all(|(a, f)| a.to_bits() == f.to_bits() || (a.is_nan() && f.is_nan()))
    );
}

/// The 0.48 deg beam (the VCP cut angle) leaves the 0-2 km layer near 126 km
/// (4/3-Earth): a 69 dBZ storm at 154 km stays empty while a 56 dBZ echo at
/// 110 km accumulates, and no cell beyond the bound is ever finite.
#[test]
fn height_cap_bounds_range_coverage() {
    let golden = golden("tracks.json");
    let expected = &golden["moore"];
    let path = require_file!(as_str(&expected["entry"]));
    let volume = level2(&path);
    let spec = TracksGridSpec::default();
    let frame = low_level_azshear_cartesian(&volume, &spec);

    let bound_km = as_f64(&expected["beam_top_range_km"]);
    assert!((bound_km - 126.0).abs() < 3.0, "golden bound {bound_km}");
    let beyond = &expected["beyond_bound"];
    assert!(as_f64(&beyond["dbz"]) >= 40.0 && as_f64(&beyond["range_km"]) > bound_km + 10.0);
    assert!(
        at(&frame, &spec, point(beyond)).is_nan(),
        "strong echo beyond the height cap accumulated"
    );
    let inside = &expected["inside_bound"];
    assert!(as_f64(&inside["dbz"]) >= 40.0 && as_f64(&inside["range_km"]) < bound_km - 5.0);
    let value = at(&frame, &spec, point(inside));
    assert!(
        value.is_finite(),
        "strong echo inside the height cap stayed empty"
    );

    let size = spec.size();
    let mut farthest = 0.0f64;
    let mut nearest = f64::INFINITY;
    for (index, v) in frame.iter().enumerate() {
        if v.is_finite() {
            let (e, n) = spec.cell_center_km(index % size, index / size);
            let range = f64::from(e).hypot(f64::from(n));
            farthest = farthest.max(range);
            nearest = nearest.min(range);
        }
    }
    assert!(
        farthest <= bound_km + 2.0,
        "finite cell at {farthest:.1} km, beyond the 0-2 km bound"
    );
    assert!(
        nearest >= 5.0,
        "finite cell at {nearest:.1} km, inside the clutter floor"
    );
}

/// With the Moore circulation as the anchor, the TDS flags are exactly the
/// gates of the lowest dual-pol sweep within 5 km of it that satisfy the debris
/// criterion in Py-ART's fields (218 gates, RHOHV down to 0.21); without an
/// anchor, or with a rank-insignificant one, nothing flags; the detector's own
/// significant circulations elsewhere in the volume flag only gates within 5 km
/// of themselves.
#[test]
fn tds_gates_require_anchor_proximity_and_criteria() {
    let golden = golden("tracks.json");
    let expected = &golden["moore"];
    let path = require_file!(as_str(&expected["entry"]));
    let volume = level2(&path);
    let anchor = moore_anchor(&expected["circulation"]);
    assert!(tds_anchor(&anchor));
    let tds = &expected["tds"];
    assert_eq!(as_f64(&tds["radius_km"]), TDS_ANCHOR_RADIUS_KM);
    assert_eq!(as_f64(&tds["cc_max"]) as f32, TDS_CC_MAX);
    assert_eq!(as_f64(&tds["min_dbz"]) as f32, TDS_MIN_DBZ);
    let debris = array(&tds["debris_gates"]);
    assert_eq!(debris.len(), as_usize(&tds["debris_gate_count"]));
    assert!(debris.len() > 100 && debris.len() < as_usize(&tds["gates_within_radius"]));

    let flagged = detect_tds_gates(&volume, &[anchor]);
    let anchor_point = point(&expected["circulation"]);
    let mut used = vec![false; debris.len()];
    for gate in &flagged {
        let position = (f64::from(gate.east_km), f64::from(gate.north_km));
        assert!(
            distance_km(position, anchor_point) <= TDS_ANCHOR_RADIUS_KM + 0.05,
            "{gate:?}"
        );
        assert!(gate.cc < TDS_CC_MAX && gate.dbz > TDS_MIN_DBZ, "{gate:?}");
        let (index, reference) = debris
            .iter()
            .enumerate()
            .map(|(i, g)| (i, array(g)))
            .min_by(|a, b| {
                distance_km((as_f64(&a.1[0]), as_f64(&a.1[1])), position)
                    .total_cmp(&distance_km((as_f64(&b.1[0]), as_f64(&b.1[1])), position))
            })
            .expect("golden gates");
        let offset = distance_km((as_f64(&reference[0]), as_f64(&reference[1])), position);
        assert!(
            offset <= 0.05,
            "flag {gate:?} matches no Py-ART debris gate ({offset:.3} km)"
        );
        assert!(!used[index], "two flags on one gate: {gate:?}");
        used[index] = true;
        assert!(
            (f64::from(gate.cc) - as_f64(&reference[2])).abs() <= 0.005,
            "{gate:?} vs {reference:?}"
        );
        assert!(
            (f64::from(gate.dbz) - as_f64(&reference[3])).abs() <= 0.05,
            "{gate:?} vs {reference:?}"
        );
    }
    assert_eq!(
        flagged.len(),
        debris.len(),
        "every Py-ART debris gate is flagged"
    );
    let min_cc = flagged.iter().map(|g| g.cc).fold(1.0f32, f32::min);
    assert!((f64::from(min_cc) - as_f64(&tds["min_cc"])).abs() <= 0.005);

    assert!(detect_tds_gates(&volume, &[]).is_empty());
    let weak = RotationSite {
        rank: 1,
        strength: RotationStrength::WeakCirculation,
        ..anchor
    };
    assert!(!tds_anchor(&weak));
    assert!(detect_tds_gates(&volume, &[weak]).is_empty());

    // The detector's own circulations of this volume.
    let sites = detect_rotation_sites(&volume);
    assert!(!sites.is_empty());
    let anchors: Vec<RotationSite> = sites.iter().filter(|s| tds_anchor(s)).cloned().collect();
    let others: Vec<RotationSite> = sites.iter().filter(|s| !tds_anchor(s)).cloned().collect();
    assert!(detect_tds_gates(&volume, &others).is_empty());
    for gate in detect_tds_gates(&volume, &anchors) {
        let position = (f64::from(gate.east_km), f64::from(gate.north_km));
        assert!(gate.cc < TDS_CC_MAX && gate.dbz > TDS_MIN_DBZ);
        assert!(anchors.iter().any(|site| {
            let az = f64::from(site.azimuth_deg).to_radians();
            let range = site.ground_range_m / 1000.0;
            distance_km((range * az.sin(), range * az.cos()), position)
                <= TDS_ANCHOR_RADIUS_KM + 0.05
        }));
    }
}
