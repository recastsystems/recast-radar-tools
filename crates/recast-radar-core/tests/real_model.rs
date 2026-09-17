//! Model values of real volumes: moment grids, cuts and ray metadata.
//!
//! Expected values: `testdata/golden/core/model.json`, written by
//! `tools/core_golden.py` with Py-ART 2.2.5 (raw moment codes and data-block
//! headers), MetPy 1.7.1 (sweep layouts), netCDF4 1.7.4 (per-ray instrument
//! variables) and a GRIB2 section walker (JMA sweep ladder).

mod common;

use common::{
    array, as_f64, as_opt_f64, as_str, as_usize, assert_close, cfradial, golden, jma, level2,
    moment, raw_code, raw_row_sums,
};
use recast_radar_core::{
    MomentGrid, MomentStorage, MomentType, RayInstrumentMetadataAlignmentError,
};
use serde_json::Value;

/// Every physical cell of a decoded u8/u16 grid against Py-ART's raw codes:
/// header fields, per-row code sums, probes, and the count of cells the
/// model reports as no data (codes 0 and 1).
fn assert_grid_matches_raw_codes(grid: &MomentGrid, expected: &Value, what: &str) {
    assert_eq!(
        grid.radial_count(),
        as_usize(&expected["rays"]),
        "{what}: rows"
    );
    assert_eq!(
        grid.gate_range.gate_count,
        as_usize(&expected["gates"]),
        "{what}: gates"
    );
    assert_eq!(
        grid.gate_range.first_gate_m,
        as_f64(&expected["first_gate_m"]) as i32,
        "{what}: first gate"
    );
    assert_eq!(
        grid.gate_range.gate_spacing_m,
        as_f64(&expected["gate_spacing_m"]) as i32,
        "{what}: gate spacing"
    );
    assert_eq!(
        u32::from(grid.storage.word_size_bits()),
        as_usize(&expected["word_size"]) as u32,
        "{what}: word size"
    );
    assert_eq!(
        f64::from(grid.scale),
        as_f64(&expected["scale"]),
        "{what}: scale"
    );
    assert_eq!(
        f64::from(grid.offset),
        as_f64(&expected["offset"]),
        "{what}: offset"
    );
    assert_eq!(
        grid.nodata,
        Some(0),
        "{what}: Level II code 0 is below threshold"
    );
    assert_eq!(
        grid.range_folded,
        Some(1),
        "{what}: Level II code 1 is range folded"
    );
    // Rows index the cut's radials one to one and storage is exactly rows x gates.
    assert_eq!(
        grid.radial_indices,
        (0..grid.radial_count()).collect::<Vec<_>>(),
        "{what}: radial indices"
    );
    assert_eq!(
        grid.storage.len(),
        grid.radial_count() * grid.gate_range.gate_count,
        "{what}: storage length"
    );

    let sums: Vec<u64> = array(&expected["raw_row_sums"])
        .iter()
        .map(|v| v.as_u64().expect("row sum"))
        .collect();
    assert_eq!(raw_row_sums(grid), sums, "{what}: raw row sums");

    let mut no_data = 0usize;
    for row in 0..grid.radial_count() {
        for gate in 0..grid.gate_range.gate_count {
            if grid.scaled_value(row, gate).is_none() {
                no_data += 1;
            }
        }
    }
    let padding: usize = array(&expected["gates_per_ray"])
        .iter()
        .map(|count| grid.gate_range.gate_count - as_usize(count))
        .sum();
    assert_eq!(
        no_data,
        as_usize(&expected["code_0_count"]) + as_usize(&expected["code_1_count"]) + padding,
        "{what}: no-data cells are exactly the code 0 and code 1 cells plus the padding of short rays"
    );
    assert!(grid.scaled_value(0, grid.gate_range.gate_count).is_none());
    assert!(grid.scaled_value(grid.radial_count(), 0).is_none());

    for probe in array(&expected["probes"]) {
        let row = as_usize(&probe["ray"]);
        let gate = as_usize(&probe["gate"]);
        let scaled = grid.scaled_value(row, gate);
        match as_opt_f64(&probe["scaled"]) {
            Some(value) => assert_close(
                f64::from(scaled.unwrap_or(f32::NAN)),
                value,
                1e-4,
                &format!("{what}: ray {row} gate {gate}"),
            ),
            None => assert_eq!(scaled, None, "{what}: ray {row} gate {gate} has no data"),
        }
        if let Some(raw) = probe["raw"].as_u64() {
            assert_eq!(
                u64::from(raw_code(grid, row, gate)),
                raw,
                "{what}: raw code at ray {row} gate {gate}"
            );
        }
    }
}

/// Hurricane Irene SMART-R2 sweeps (CfRadial with per-ray `prt`,
/// `unambiguous_range` and `n_samples`): the decoded sidecar is aligned with
/// the radials and carries netCDF4's values; a Level II volume has none; a
/// sidecar with one entry fewer than the radials is an alignment error.
#[test]
fn ray_instrument_metadata_is_optional_but_must_align() {
    let expected = golden();
    let irene = &expected["irene"];
    let path =
        recast_radar_testdata::require_file!("cfrad1-irene-sr2-20110827-120420-sur-sweeps01");
    let volume = cfradial(&path);
    assert_eq!(volume.cuts.len(), array(&irene["sweeps"]).len());
    for (index, (cut, sweep)) in volume.cuts.iter().zip(array(&irene["sweeps"])).enumerate() {
        assert_eq!(
            cut.radials.len(),
            as_usize(&sweep["rays"]),
            "sweep {index} rays"
        );
        let metadata = cut
            .aligned_ray_instrument_metadata()
            .expect("aligned")
            .expect("CfRadial supplies per-ray instrument variables");
        assert_eq!(metadata.len(), cut.radials.len());
        let prt: Vec<f64> = array(&sweep["prt_s_unique"]).iter().map(as_f64).collect();
        let range: Vec<f64> = array(&sweep["unambiguous_range_km_unique"])
            .iter()
            .map(as_f64)
            .collect();
        for (ray, entry) in metadata.iter().enumerate() {
            let prt_s = f64::from(entry.prt_s.expect("prt"));
            assert!(
                prt.iter().any(|v| (v - prt_s).abs() < 1e-12),
                "sweep {index} ray {ray}: prt {prt_s} not in {prt:?}"
            );
            let range_km = f64::from(entry.unambiguous_range_km.expect("unambiguous range"));
            assert!(
                range.iter().any(|v| (v - range_km).abs() < 1e-6),
                "sweep {index} ray {ray}: range {range_km} not in {range:?}"
            );
            // The file has `n_samples`, not the CfRadial `pulse_count` /
            // `independent_samples` variables the reader maps.
            assert!(!irene["has_pulse_count"].as_bool().unwrap());
            assert_eq!(entry.pulse_count, None);
            assert!(!irene["has_independent_samples"].as_bool().unwrap());
            assert_eq!(entry.independent_samples, None);
        }
    }

    // Misalign the real sidecar of sweep 1 by one entry.
    let mut cut = volume.cuts[0].clone();
    let radial_count = cut.radials.len();
    cut.ray_instrument_metadata.pop();
    assert_eq!(
        cut.aligned_ray_instrument_metadata(),
        Err(RayInstrumentMetadataAlignmentError {
            radial_count,
            metadata_count: radial_count - 1,
        })
    );
    let first = cut.ray_instrument_metadata[0];
    cut.ray_instrument_metadata.push(first);
    cut.ray_instrument_metadata.push(first);
    assert_eq!(
        cut.aligned_ray_instrument_metadata(),
        Err(RayInstrumentMetadataAlignmentError {
            radial_count,
            metadata_count: radial_count + 1,
        })
    );
    // Empty means the source supplied none.
    cut.ray_instrument_metadata.clear();
    assert_eq!(cut.aligned_ray_instrument_metadata(), Ok(None));

    // Archive II carries no per-ray instrument variables.
    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
    let level2 = level2(&path);
    for cut in &level2.cuts {
        assert!(cut.ray_instrument_metadata.is_empty());
        assert_eq!(cut.aligned_ray_instrument_metadata(), Ok(None));
    }
}

/// KTLX 2024-03-15 surveillance sweep: the 8-bit REF grid (1832 gates x 480
/// radials, scale 2, offset 66) scales every raw code as Py-ART reads it,
/// with codes 0 and 1 reported as no data.
#[test]
fn decoded_u8_reflectivity_grid_scales_real_codes() {
    let expected = golden();
    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
    let volume = level2(&path);
    let grid = volume.cuts[0]
        .moments
        .get(&MomentType::Reflectivity)
        .expect("REF");
    assert!(matches!(grid.storage, MomentStorage::U8(_)));
    assert_grid_matches_raw_codes(
        grid,
        &expected["grids"]["ktlx_2024_ref_sweep0"],
        "KTLX 2024 REF",
    );
    assert!(
        expected["grids"]["ktlx_2024_ref_sweep0"]["all_rays_same_gate_count"]
            .as_bool()
            .unwrap()
    );
}

/// KDMX 2008-05-25 sweep 11 (5.0 deg): the VEL data blocks shrink from 836
/// to 816 gates along the sweep, so the grid keeps the longest row's gate
/// count and pads the short rows with the no-data code (Py-ART: per-radial
/// `ngates`).
#[test]
fn decoded_grid_pads_rows_with_fewer_gates() {
    let expected = golden();
    let expected = &expected["grids"]["kdmx_2008_vel_sweep10"];
    let path = recast_radar_testdata::require_file!("l2-kdmx-20080525-205148");
    let volume = level2(&path);
    let cut = &volume.cuts[as_usize(&expected["sweep_index"])];
    assert_close(
        f64::from(cut.elevation_deg),
        as_f64(&expected["elevation_deg"]),
        1e-6,
        "sweep elevation",
    );
    let grid = cut.moments.get(&MomentType::Velocity).expect("VEL");
    assert_grid_matches_raw_codes(grid, expected, "KDMX VEL sweep 11");

    let counts: Vec<usize> = array(&expected["gates_per_ray"])
        .iter()
        .map(as_usize)
        .collect();
    let longest = *counts.iter().max().unwrap();
    assert_eq!(grid.gate_range.gate_count, longest);
    assert_eq!(counts[0], longest, "the longest radial comes first");
    assert_eq!(
        counts.iter().filter(|&&c| c < longest).count(),
        as_usize(&expected["short_ray_count"])
    );
    assert_eq!(
        *counts.iter().min().unwrap(),
        as_usize(&expected["shortest_ray_gates"])
    );
    for (row, &count) in counts.iter().enumerate() {
        for gate in count..longest {
            assert_eq!(
                raw_code(grid, row, gate),
                0,
                "ray {row} gate {gate} is padding"
            );
            assert_eq!(grid.scaled_value(row, gate), None);
        }
    }
}

/// KTLX 2024-03-15 Doppler sweep: the 8-bit VEL grid (1192 gates, scale 2,
/// offset 129) with 342 range-folded (code 1) gates.
#[test]
fn decoded_u8_velocity_grid_reports_range_folded_gates() {
    let expected = golden();
    let expected = &expected["grids"]["ktlx_2024_vel_sweep1"];
    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
    let volume = level2(&path);
    let grid = volume.cuts[1]
        .moments
        .get(&MomentType::Velocity)
        .expect("VEL");
    assert_grid_matches_raw_codes(grid, expected, "KTLX 2024 VEL");
    assert!(as_usize(&expected["code_1_count"]) > 0);
    let folded = (0..grid.radial_count())
        .flat_map(|row| (0..grid.gate_range.gate_count).map(move |gate| (row, gate)))
        .filter(|&(row, gate)| raw_code(grid, row, gate) == 1)
        .count();
    assert_eq!(folded, as_usize(&expected["code_1_count"]));
}

/// KTLX 2013-05-20 surveillance sweep: PHI is a 16-bit moment (scale
/// 2.8361, offset 2), decoded from big-endian words into u16 storage.
#[test]
fn decoded_u16_differential_phase_grid_matches_big_endian_codes() {
    let expected = golden();
    let expected = &expected["grids"]["ktlx_2013_phi_sweep0"];
    let path = recast_radar_testdata::require_file!("l2-ktlx-20130520-201643-trim");
    let volume = level2(&path);
    let grid = volume.cuts[0]
        .moments
        .get(&MomentType::DifferentialPhase)
        .expect("PHI");
    assert!(matches!(grid.storage, MomentStorage::U16(_)));
    assert_grid_matches_raw_codes(grid, expected, "KTLX 2013 PHI");
    // ZDR and RHO of the same radials are 8-bit in this Build 13.2 file.
    for name in ["ZDR", "RHO"] {
        let grid = volume.cuts[0].moments.get(&moment(name)).expect(name);
        assert!(matches!(grid.storage, MomentStorage::U8(_)), "{name}");
    }
}

/// The moments a cut carries are the data blocks MetPy lists on its radials:
/// the surveillance sweep (REF, ZDR, PHI, RHO and, from Build 19, CFP) and
/// the Doppler sweep (REF, VEL, SW) of the KTLX 2024 and 2013 split cuts.
#[test]
fn cut_tracks_available_moments() {
    let expected = golden();
    for (id, key) in [
        (
            "l2-ktlx-20240315-000217-trim",
            "l2-ktlx-20240315-000217-trim",
        ),
        (
            "l2-ktlx-20130520-201643-trim",
            "l2-ktlx-20130520-201643-trim",
        ),
    ] {
        let layout = &expected["level2_layouts"][key];
        let path = recast_radar_testdata::require_file!(id);
        let volume = level2(&path);
        assert_eq!(volume.site.id, as_str(&layout["icao"]));
        let sweeps = array(&layout["sweeps"]);
        assert_eq!(volume.cuts.len(), sweeps.len(), "{id}: sweeps");
        for (index, (cut, sweep)) in volume.cuts.iter().zip(sweeps).enumerate() {
            let names: std::collections::BTreeSet<MomentType> = array(&sweep["moments"])
                .iter()
                .map(|m| moment(as_str(m)))
                .collect();
            assert_eq!(
                cut.moments_available(),
                names,
                "{id}: sweep {index} moments"
            );
            assert_eq!(cut.radials.len(), as_usize(&sweep["rays"]));
            assert_close(
                f64::from(cut.elevation_deg),
                as_f64(&sweep["elevation_deg"]),
                1e-6,
                &format!("{id}: sweep {index} elevation"),
            );
            assert_close(
                f64::from(cut.radials[0].azimuth_deg),
                as_f64(&sweep["azimuth_first"]),
                1e-4,
                &format!("{id}: sweep {index} first azimuth"),
            );
            for name in &names {
                assert!(cut.moments_available().contains(name));
            }
            assert!(
                !cut.moments_available()
                    .contains(&MomentType::SpecificDifferentialPhase)
            );
        }
    }
}

/// The JMA Osaka N5 member carries 26 sweeps in four descending ladders that
/// repeat the 0.0, 0.3, 0.7, 1.2, 1.8, 2.5 and 5.0 deg tilts: every sweep
/// stays its own cut (never elevation-merged), sorted lowest first with the
/// scan order kept among equal elevations and numbered 1..=26 (GRIB2
/// walker: product-definition elevation of every member section).
#[test]
fn volume_can_keep_repeated_elevation_cuts_separate() {
    let expected = golden();
    let n5 = &expected["jma"]["n5"];
    let path = recast_radar_testdata::require_file!("jma-n5-20191012-090000-rs47773");
    let volume = jma(&path);
    assert_eq!(volume.site.id, as_str(&n5["station_id"]));
    let scan_order: Vec<(f32, usize)> = array(&n5["sweeps_scan_order"])
        .iter()
        .map(|s| (as_f64(&s["elevation_deg"]) as f32, as_usize(&s["radials"])))
        .collect();
    assert_eq!(volume.cuts.len(), scan_order.len());
    let mut sorted = scan_order.clone();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0)); // stable, like the reader
    let decoded: Vec<(f32, usize)> = volume
        .cuts
        .iter()
        .map(|cut| (cut.elevation_deg, cut.radials.len()))
        .collect();
    assert_eq!(decoded, sorted);
    let numbers: Vec<Option<u8>> = volume.cuts.iter().map(|cut| cut.elevation_number).collect();
    assert_eq!(numbers, (1..=26).map(Some).collect::<Vec<_>>());
    // Repeated tilts: 5.0 deg appears three times, 0.3 deg four times.
    let count = |elevation: f32| {
        volume
            .cuts
            .iter()
            .filter(|cut| (cut.elevation_deg - elevation).abs() < 1e-6)
            .count()
    };
    assert_eq!(count(5.0), 3);
    assert_eq!(count(0.3), 4);
    assert_eq!(count(0.0), 2);
}
