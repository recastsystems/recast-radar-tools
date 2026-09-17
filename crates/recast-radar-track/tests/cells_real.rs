//! Storm cell identification on real Level II volumes (enhanced watershed).
//!
//! Expected values: `testdata/golden/track/cells.json`, written by
//! `tools/track_golden.py` (section `cells`) from Py-ART's composite reflectivity
//! of the same volumes: connected components with true polar gate areas and
//! Z^(4/7)-weighted centroids, local maxima on a 1 km Cartesian image, and the
//! contiguous 40 dBZ envelope of the derecho line.

// Test code panics on purpose: the workspace's unwrap/expect lints guard library code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{array, as_f64, as_usize, distance_km, golden, level2};
use recast_radar_testdata::require_file;
use recast_radar_track::{StormCell, identify_storm_cells};

fn nearest(cells: &[StormCell], east: f64, north: f64) -> (&StormCell, f64) {
    cells
        .iter()
        .map(|cell| {
            (
                cell,
                distance_km((cell.east_km, cell.north_km), (east, north)),
            )
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .expect("at least one cell")
}

/// KEWX 2016-04-13 02:25Z (San Antonio hailstorm): every salient 60 dBZ core of
/// Py-ART's composite (area >= 20 km2) is identified as its own cell within
/// 3 km of the composite's mass-weighted centroid, the strongest one first, and
/// the cell peaks stay below the unsmoothed composite maximum.
#[test]
fn identifies_every_salient_hail_core_of_kewx() {
    let expected = golden("cells.json");
    let expected = &expected["kewx"];
    let path = require_file!("l2-kewx-20160413-022531");
    let volume = level2(&path);
    let cells = identify_storm_cells(&volume);
    assert!(!cells.is_empty(), "no cells in a hail outbreak");

    let cores = array(&expected["cores"]);
    assert!(cores.len() >= 4, "golden lists {} cores", cores.len());
    let composite_max = as_f64(&expected["composite_max_dbz"]) as f32;
    let mut matched = Vec::new();
    for core in cores {
        let (east, north) = (as_f64(&core["east_km"]), as_f64(&core["north_km"]));
        let (cell, distance) = nearest(&cells, east, north);
        assert!(
            distance <= 3.0,
            "no cell within 3 km of the {} dBZ core at ({east:.1}, {north:.1}): nearest {cell:?} at {distance:.1} km",
            as_f64(&core["max_dbz"])
        );
        assert!(
            cell.max_dbz >= 60.0 && cell.max_dbz <= composite_max + 0.5,
            "core peak {} dBZ outside [60, {composite_max}]: {cell:?}",
            cell.max_dbz
        );
        assert!(
            cell.area_km2 >= 20.0,
            "core area below the 20 km2 saliency floor: {cell:?}"
        );
        let key = (cell.east_km.to_bits(), cell.north_km.to_bits());
        assert!(!matched.contains(&key), "two cores share the cell {cell:?}");
        matched.push(key);
    }
    // Cells are sorted strongest first; the strongest core of the composite is
    // the strongest cell.
    let strongest = &cores[0];
    let (cell, distance) = nearest(
        &cells,
        as_f64(&strongest["east_km"]),
        as_f64(&strongest["north_km"]),
    );
    assert!(
        std::ptr::eq(cell, &cells[0]) && distance <= 3.0,
        "strongest cell {:?} is not at the strongest composite core",
        cells[0]
    );
    assert!(
        cells
            .windows(2)
            .all(|pair| pair[0].max_dbz >= pair[1].max_dbz),
        "cells must be sorted by peak reflectivity"
    );
    for cell in &cells {
        let expected_radius = (cell.area_km2 / std::f64::consts::PI).sqrt();
        assert!(
            (cell.eq_radius_km - expected_radius).abs() < 1e-9,
            "{cell:?}"
        );
        assert!(cell.mass > 0.0 && cell.hlevel_dbz >= 30.0, "{cell:?}");
    }
}

/// KTLX 2024-05-15 00:00Z (clear air, biological returns): Py-ART's composite
/// never reaches 41 dBZ and its largest 30 dBZ patch is a fraction of a square
/// kilometre, far below the 20 km2 saliency floor, so no cell is identified.
#[test]
fn clear_air_volume_yields_no_cells() {
    let expected = golden("cells.json");
    let expected = &expected["clear_air"];
    assert!(as_f64(&expected["composite_max_dbz"]) < 41.0);
    assert!(as_f64(&expected["largest_30dbz_patch_km2"]) < 1.0);
    let path = require_file!("l2-ktlx-20240515-000014");
    let volume = level2(&path);
    assert!(!volume.sweeps.is_empty());
    let cells = identify_storm_cells(&volume);
    assert!(cells.is_empty(), "clear air produced cells: {cells:?}");
}

/// TBWI 2023-06-01 17:51Z status-only archive object (three Message 2 records,
/// no radials; MetPy reads zero sweeps): the decoded volume has no sweeps and no
/// cell is identified.
#[test]
fn volume_without_radials_yields_no_cells() {
    let expected = golden("cells.json");
    assert_eq!(as_usize(&expected["stub"]["sweeps"]), 0);
    let path = require_file!("l2-tbwi-20230601-175101-stub");
    let volume = level2(&path);
    assert!(volume.sweeps.is_empty(), "{} sweeps", volume.sweeps.len());
    assert!(identify_storm_cells(&volume).is_empty());
}

/// KDVN 2020-08-10 18:04Z (Iowa derecho): Py-ART's composite has ONE contiguous
/// 40 dBZ envelope of about 15,600 km2 holding several distinct 60 dBZ cores 15
/// km or more apart (single-threshold connected components at 40 dBZ return one
/// blob: the ETITAN motivating case, Han et al. 2009). The watershed gives each
/// of the strongest cores its own cell within 5 km, and many cells inside the
/// envelope.
#[test]
fn watershed_splits_the_derecho_envelope_into_its_cores() {
    let expected = golden("cells.json");
    let expected = &expected["kdvn"];
    assert!(as_f64(&expected["envelope_area_km2"]) > 10_000.0);
    let peaks = array(&expected["line_peaks"]);
    assert!(peaks.len() >= 6, "golden lists {} line cores", peaks.len());
    for pair in peaks.windows(2) {
        assert!(as_f64(&pair[0]["smoothed_dbz"]) >= as_f64(&pair[1]["smoothed_dbz"]));
    }
    let path = require_file!("l2-kdvn-20200810-180401");
    let volume = level2(&path);
    let cells = identify_storm_cells(&volume);

    let mut matched: Vec<(u64, u64)> = Vec::new();
    for peak in peaks.iter().take(4) {
        let (east, north) = (as_f64(&peak["east_km"]), as_f64(&peak["north_km"]));
        let (cell, distance) = nearest(&cells, east, north);
        assert!(
            distance <= 5.0,
            "no cell within 5 km of the {} dBZ line core at ({east:.1}, {north:.1}): nearest {cell:?} at {distance:.1} km",
            as_f64(&peak["smoothed_dbz"])
        );
        let key = (cell.east_km.to_bits(), cell.north_km.to_bits());
        assert!(
            !matched.contains(&key),
            "line cores were not split: {cell:?} serves two cores"
        );
        matched.push(key);
        assert!(cell.max_dbz >= 58.0, "{cell:?}");
    }
    let east = array(&expected["envelope_east_km"]);
    let north = array(&expected["envelope_north_km"]);
    let inside = cells
        .iter()
        .filter(|cell| {
            cell.east_km >= as_f64(&east[0])
                && cell.east_km <= as_f64(&east[1])
                && cell.north_km >= as_f64(&north[0])
                && cell.north_km <= as_f64(&north[1])
        })
        .count();
    assert!(
        inside >= peaks.len(),
        "{inside} cells inside the envelope, fewer than its {} cores",
        peaks.len()
    );
}
