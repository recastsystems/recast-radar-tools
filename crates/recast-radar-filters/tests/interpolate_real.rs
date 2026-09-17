//! Display interpolation (polar upsampling) on real sweeps.
//!
//! Expected values: `testdata/golden/filters/interpolate.json`, written by
//! `tools/filters_map_golden.py interpolate`. Inputs to the reference are read
//! with MetPy 1.7.1 (Level II), netCDF4 1.7.4 (CfRadial) and a standalone
//! DORADE block walker; the reference is a numpy float32 implementation of the
//! documented upsampler (policy table, cell-centred range subdivision,
//! sub-rows only across believable azimuth gaps, nearest-parent coverage and
//! echo edges, velocity and correlation-coefficient guards).
//!
//! Cases:
//! - `legacy_reflectivity`: KTLX 1999-05-04 Message 1 reflectivity, 367
//!   radials at ~1 deg, 1 km gates (4 x 4).
//! - `aliased_velocity`: KLIX 2005-08-29 (Katrina) Message 1 velocity, 362
//!   radials, 250 m gates, Nyquist 32.1 m/s (4 x 1).
//! - `correlation_coefficient`: KGWX 2013-06-01 dual-pol at 1 deg azimuth
//!   with 250 m gates, lowest sweep RHOHV (4 x 1).
//! - `sector`: NOXP 2009-05-25 DORADE sector PPI, 100 rays over 200-300 deg.
//! - `fine_rhi`: DOW8 CfRadial RHI (azimuth steps far below 0.25 deg, 125 m).

mod common;

use common::{array, as_f64, as_i64, as_opt_f64, as_usize, assert_close, cell, golden, row_stats};
use recast_radar_core::{Field, FieldName, Quantity, Sweep};
use recast_radar_filters::{UpsampledSweep, upsample_field};
use serde_json::Value;

struct Case {
    golden: Value,
    cut: Sweep,
    quantity: Quantity,
}

impl Case {
    fn grid(&self) -> &Field {
        self.cut.find(self.quantity).expect("case field")
    }

    fn upsample(&self) -> UpsampledSweep {
        upsample_field(&self.cut, self.grid()).expect("coarse field upsamples")
    }
}

/// The upsampled field (the only field of the upsampled sweep).
fn out_field(up: &UpsampledSweep) -> &Field {
    assert_eq!(up.sweep.fields.len(), 1);
    &up.sweep.fields[0]
}

/// Centre of native gate 0 and spacing of `field` on `sweep`'s range.
fn geometry(sweep: &Sweep, field: &Field) -> (f64, f64) {
    field.native_geometry(&sweep.range).expect("gate geometry")
}

/// Decode the case's real file and check the native grid against the golden
/// input (MetPy / netCDF4 / DORADE walker): geometry, azimuths, coverage.
fn load(label: &str) -> Option<Case> {
    let all = golden("filters/interpolate.json");
    let golden = array(&all["cases"])
        .iter()
        .find(|case| case["label"] == label)
        .unwrap_or_else(|| panic!("no golden case {label}"))
        .clone();
    let id = golden["id"].as_str().expect("id");
    let path = match recast_radar_testdata::path(id) {
        Ok(path) => path,
        Err(error) if error.is_offline() => {
            eprintln!("skipping: {error}");
            return None;
        }
        Err(error) => panic!("{error}"),
    };
    let (mut volume, quantity) = match label {
        "legacy_reflectivity" => (common::level2(&path), Quantity::Reflectivity),
        "aliased_velocity" => (common::level2(&path), Quantity::RadialVelocity),
        "correlation_coefficient" => (common::level2(&path), Quantity::CorrelationCoefficient),
        "sector" => (common::dorade(&path), Quantity::Reflectivity),
        "fine_rhi" => (common::cfradial(&path), Quantity::Reflectivity),
        _ => unreachable!("{label}"),
    };
    let cut = volume.sweeps.swap_remove(as_usize(&golden["sweep"]));
    let case = Case {
        golden,
        cut,
        quantity,
    };
    let grid = case.grid();
    let g = &case.golden;
    assert_eq!(grid.shape().0, as_usize(&g["rows"]), "{label} rows");
    if label == "sector" {
        // CSFD declares 1001 cells; the RDAT blocks carry one more word
        // (padding to a 4-byte boundary), which the reader drops.
        let cells = as_usize(&g["csfd_cell_count"]);
        let words = as_usize(&g["rdat_word_count"]);
        assert_eq!(cells, as_usize(&g["gates"]));
        assert_eq!(words, cells + 1);
    }
    assert_eq!(grid.shape().1, as_usize(&g["gates"]), "{label} gates");
    // The golden records the Level II and DORADE first gate centre, and for
    // CfRadial the start of gate 0 (range[0] minus half a gate), in whole
    // metres; the model keeps the file's exact centres.
    let (first_center_m, spacing_m) = geometry(&case.cut, grid);
    let first_gate_m = if label == "fine_rhi" {
        first_center_m - spacing_m / 2.0
    } else {
        first_center_m
    };
    assert_eq!(
        first_gate_m.round() as i64,
        as_i64(&g["first_gate_m"]),
        "{label} first gate"
    );
    assert_eq!(
        spacing_m.round() as i64,
        as_i64(&g["gate_spacing_m"]),
        "{label} spacing"
    );
    assert!(grid.absent_rows.is_empty(), "{label}: every ray has a row");
    for (row, expected) in array(&g["native_azimuths_deg"]).iter().enumerate() {
        assert_close(
            f64::from(case.cut.rays.azimuth_deg[row].rem_euclid(360.0)),
            as_f64(expected),
            1e-4,
            &format!("{label} native azimuth {row}"),
        );
    }
    let native_row_valid = array(&g["native_row_valid"]);
    for (row, (count, _)) in row_stats(grid).iter().enumerate() {
        assert_eq!(
            *count,
            as_usize(&native_row_valid[row]),
            "{label} native row {row}"
        );
    }
    Some(case)
}

/// Per-row coverage and value sums of the upsampled grid equal the reference.
fn assert_rows_match(case: &Case, up: &UpsampledSweep) {
    let g = &case.golden;
    let label = g["label"].as_str().unwrap_or_default();
    assert_eq!(
        out_field(up).shape().0,
        as_usize(&g["out_rows"]),
        "{label} rows"
    );
    let row_valid = array(&g["row_valid"]);
    let row_sum = array(&g["row_sum"]);
    for (row, (count, sum)) in row_stats(out_field(up)).iter().enumerate() {
        assert_eq!(
            *count,
            as_usize(&row_valid[row]),
            "{label} row {row} coverage"
        );
        // f32 values summed over at most 1840 gates; the golden sum is rounded
        // to 1e-4.
        let tolerance = 1e-3 + 1e-6 * as_f64(&row_sum[row]).abs();
        assert_close(
            *sum,
            as_f64(&row_sum[row]),
            tolerance,
            &format!("{label} row {row} sum"),
        );
    }
}

/// Golden sample `[row, gate, value, nearest_native, parents_min, parents_max]`.
struct Sample {
    row: usize,
    gate: usize,
    value: Option<f64>,
    nearest: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
}

fn samples<'a>(case: &'a Case, class: &str) -> (usize, impl Iterator<Item = Sample> + 'a) {
    let class = &case.golden["classes"][class];
    let iter = array(&class["samples"]).iter().map(|sample| {
        let sample = array(sample);
        Sample {
            row: as_usize(&sample[0]),
            gate: as_usize(&sample[1]),
            value: as_opt_f64(&sample[2]),
            nearest: as_opt_f64(&sample[3]),
            min: as_opt_f64(&sample[4]),
            max: as_opt_f64(&sample[5]),
        }
    });
    (as_usize(&class["count"]), iter)
}

fn value_at(up: &UpsampledSweep, sample: &Sample) -> Option<f64> {
    cell(out_field(up), sample.row, sample.gate).map(f64::from)
}

/// KTLX 1999 Message 1 reflectivity (1 deg x 1 km): 4 x 4 subdivision with
/// the rendered annulus preserved, every native radial kept at its exact
/// azimuth, sub-rows at quarter steps between neighbours, and each output row
/// linked to its nearest parent radial.
#[test]
fn geometry_subdivides_exactly() {
    let Some(case) = load("legacy_reflectivity") else {
        return;
    };
    let g = &case.golden;
    assert_eq!(array(&g["factors"]), &vec![Value::from(4), Value::from(4)]);
    let native_field = case.grid();
    let (native_first, native_spacing) = geometry(&case.cut, native_field);
    let native_gates = native_field.shape().1;
    let up = case.upsample();
    let out = out_field(&up);
    let (out_first, out_spacing) = geometry(&up.sweep, out);
    let out_gates = out.shape().1;
    assert_eq!(out_spacing, as_f64(&g["out_gate_spacing_m"]));
    assert_eq!(out_gates, as_usize(&g["out_gate_count"]));
    assert_eq!(out_first, as_f64(&g["out_first_gate_m"]));
    assert_eq!((out_spacing, out_gates, out_first), (250.0, 1840, -375.0));

    // Gate centres: inner and outer edges of the rendered annulus are
    // identical on both geometries.
    let inner = |first: f64, spacing: f64| first - spacing / 2.0;
    let outer = |first: f64, spacing: f64, gates: usize| first + (gates as f64 - 0.5) * spacing;
    assert_eq!(
        inner(native_first, native_spacing),
        inner(out_first, out_spacing)
    );
    assert_eq!(
        outer(native_first, native_spacing, native_gates),
        outer(out_first, out_spacing, out_gates)
    );

    let parents = array(&g["row_parents"]);
    let azimuths = array(&g["row_azimuths_deg"]);
    let radial_index = array(&g["row_radial_index"]);
    let native_azimuths = array(&g["native_azimuths_deg"]);
    assert_eq!(up.sweep.rays.azimuth_deg.len(), azimuths.len());
    assert_eq!(up.parent_rays.len(), radial_index.len());
    let mut native_rows = 0;
    for (row, parent) in parents.iter().enumerate() {
        let parent = array(parent);
        let actual = f64::from(up.sweep.rays.azimuth_deg[row]);
        if as_f64(&parent[2]) == 0.0 {
            // A native row: exactly the file's radial azimuth.
            native_rows += 1;
            let lo = as_usize(&parent[0]);
            assert_close(
                actual,
                as_f64(&native_azimuths[lo]),
                1e-4,
                &format!("native row {row}"),
            );
        }
        assert_close(
            actual,
            as_f64(&azimuths[row]),
            1e-3,
            &format!("row {row} azimuth"),
        );
        assert_eq!(
            up.parent_rays[row] as usize,
            as_usize(&radial_index[row]),
            "row {row} parent"
        );
    }
    assert_eq!(native_rows, 367);
    assert_rows_match(&case, &up);
}

/// Sub-rows follow the scan across north (a native radial just below 360 deg
/// to the next just above 0 deg) and between the sweep's last and first
/// radials (the 367-radial sweep overlaps its start, so that step runs
/// backwards from 11.6 to 10.3 deg): every azimuth stays in [0, 360) and
/// equals the reference.
#[test]
fn azimuth_wraps_between_last_and_first_row() {
    let Some(case) = load("legacy_reflectivity") else {
        return;
    };
    let g = &case.golden;
    let up = case.upsample();
    let parents = array(&g["row_parents"]);
    let azimuths = array(&g["row_azimuths_deg"]);
    let native = array(&g["native_azimuths_deg"]);
    let last = as_usize(&g["rows"]) - 1;
    let (mut north, mut last_to_first) = (0, 0);
    for (row, parent) in parents.iter().enumerate() {
        let parent = array(parent);
        let (lo, hi, t) = (
            as_usize(&parent[0]),
            as_usize(&parent[1]),
            as_f64(&parent[2]),
        );
        let actual = up.sweep.rays.azimuth_deg[row];
        assert!((0.0..360.0).contains(&actual), "row {row}: {actual}");
        if t == 0.0 {
            continue;
        }
        let (from, to) = (as_f64(&native[lo]), as_f64(&native[hi]));
        if from > 300.0 && to < 60.0 {
            north += 1;
            assert_close(
                f64::from(actual),
                as_f64(&azimuths[row]),
                1e-3,
                &format!("row {row}"),
            );
        }
        if lo == last && hi == 0 {
            last_to_first += 1;
            assert!(
                f64::from(actual) < from && f64::from(actual) > to,
                "row {row}: {actual}"
            );
            assert_close(
                f64::from(actual),
                as_f64(&azimuths[row]),
                1e-3,
                &format!("row {row}"),
            );
        }
    }
    assert_eq!(north, 3, "sub-rows across north");
    assert_eq!(
        last_to_first, 3,
        "sub-rows between the last and first radial"
    );
}

/// Where the four bilinear parents carry one value, the sub-cell carries it
/// unchanged (reflectivity, velocity and a DORADE sector); a grid already at
/// the display targets (the DOW8 RHI: azimuth steps below 0.25 deg, 125 m
/// gates) is not upsampled at all.
#[test]
fn uniform_field_is_unchanged_and_fine_grids_pass_through() {
    for label in ["legacy_reflectivity", "aliased_velocity", "sector"] {
        let Some(case) = load(label) else {
            return;
        };
        let up = case.upsample();
        let (count, samples) = samples(&case, "uniform");
        assert!(count > 1000, "{label}: {count} uniform cells");
        for sample in samples {
            let expected = sample.nearest.expect("uniform parents are valid");
            assert_eq!(sample.min, Some(expected));
            assert_eq!(sample.max, Some(expected));
            let actual = value_at(&up, &sample).expect("uniform cell renders");
            assert_close(
                actual,
                expected,
                1e-5,
                &format!("{label} row {} gate {}", sample.row, sample.gate),
            );
        }
    }

    let Some(case) = load("fine_rhi") else {
        return;
    };
    assert_eq!(case.golden["identity"], Value::Bool(true));
    assert!(as_f64(&case.golden["nominal_azimuth_deg"]) <= 0.25);
    assert!(upsample_field(&case.cut, case.grid()).is_none());
}

/// No sub-cell renders where its nearest native parent has no data, and a
/// sub-row on a beam boundary renders only where both beams have data: per
/// output row the coverage equals the reference for every case, and the
/// reference's blocked boundary cells stay empty.
#[test]
fn coverage_does_not_grow() {
    for label in [
        "legacy_reflectivity",
        "aliased_velocity",
        "correlation_coefficient",
        "sector",
    ] {
        let Some(case) = load(label) else {
            return;
        };
        let up = case.upsample();
        assert_rows_match(&case, &up);

        let native = case.grid();
        let range_factor = as_usize(&case.golden["factors"][1]);
        let parents = array(&case.golden["row_parents"]);
        for (row, parent) in parents.iter().enumerate() {
            let parent = array(parent);
            let nearest_row = if as_f64(&parent[2]) <= 0.5 {
                as_usize(&parent[0])
            } else {
                as_usize(&parent[1])
            };
            for gate in 0..out_field(&up).shape().1 {
                if cell(out_field(&up), row, gate).is_some() {
                    assert!(
                        cell(native, nearest_row, gate / range_factor).is_some(),
                        "{label} row {row} gate {gate}: coverage grew"
                    );
                }
            }
        }
        let (count, blocked) = samples(&case, "boundary_blocked");
        assert!(count > 0, "{label}: no boundary cells");
        for sample in blocked {
            assert!(sample.nearest.is_some());
            assert_eq!(value_at(&up, &sample), None, "{label} row {}", sample.row);
        }
        let native_total = as_usize(&case.golden["native_valid_total"]);
        let up_total: usize = row_stats(out_field(&up))
            .iter()
            .map(|(count, _)| count)
            .sum();
        let factors = as_usize(&case.golden["factors"][0]) * range_factor;
        assert!(
            up_total <= native_total * factors,
            "{label}: {up_total} cells"
        );
    }
}

/// At echo edges (a bilinear parent without data) the sub-cell takes its
/// nearest parent's value from the file, never a partial blend.
#[test]
fn echo_edges_use_nearest_parent_not_partial_blends() {
    for label in [
        "legacy_reflectivity",
        "aliased_velocity",
        "correlation_coefficient",
        "sector",
    ] {
        let Some(case) = load(label) else {
            return;
        };
        let up = case.upsample();
        let (count, edges) = samples(&case, "edge");
        assert!(count > 1000, "{label}: {count} edge cells");
        for sample in edges {
            let expected = sample
                .nearest
                .expect("edge cells have a valid nearest parent");
            // The reference output is the f32 of the native value.
            assert_close(
                sample.value.expect("value"),
                expected,
                1e-6,
                "reference edge value",
            );
            let actual = value_at(&up, &sample).expect("edge cell renders");
            assert_close(
                actual,
                expected,
                1e-5,
                &format!("{label} row {} gate {}", sample.row, sample.gate),
            );
        }
    }
}

/// Katrina velocity (Nyquist 32.1 m/s, strongly aliased): where the four
/// parents spread more than 30 m/s the sub-cell keeps the nearest parent's
/// velocity; smaller spreads blend inside the parents' range.
#[test]
fn velocity_fold_guard_uses_nearest_parent() {
    let Some(case) = load("aliased_velocity") else {
        return;
    };
    assert!(as_f64(&case.golden["nyquist_mps"]) > 30.0);
    let up = case.upsample();
    let (count, guarded) = samples(&case, "guarded");
    assert!(count > 10_000, "{count} guarded cells");
    for sample in guarded {
        let (min, max) = (sample.min.expect("min"), sample.max.expect("max"));
        assert!(max - min > 30.0);
        let expected = sample.nearest.expect("nearest");
        let actual = value_at(&up, &sample).expect("guarded cell renders");
        assert_close(
            actual,
            expected,
            1e-5,
            &format!("row {} gate {}", sample.row, sample.gate),
        );
    }
    let (count, blended) = samples(&case, "blend");
    assert!(count > 100_000, "{count} blended cells");
    for sample in blended {
        let (min, max) = (sample.min.expect("min"), sample.max.expect("max"));
        assert!(max - min <= 30.0);
        let actual = value_at(&up, &sample).expect("blended cell renders");
        assert!(actual >= min - 1e-4 && actual <= max + 1e-4);
        assert_close(
            actual,
            sample.value.expect("value"),
            1e-4,
            &format!("row {} gate {}", sample.row, sample.gate),
        );
    }
    assert_rows_match(&case, &up);
}

/// KGWX correlation coefficient: wherever a bilinear parent is below 0.97
/// (melting layer, non-meteorological echo) the sub-cell keeps the nearest
/// parent's RHOHV; only all-high neighbourhoods blend.
#[test]
fn cc_guard_never_blends_through_the_melting_layer() {
    let Some(case) = load("correlation_coefficient") else {
        return;
    };
    let up = case.upsample();
    let (count, guarded) = samples(&case, "guarded");
    assert!(count > 10_000, "{count} guarded cells");
    for sample in guarded {
        assert!(sample.min.expect("min") < 0.97);
        let expected = sample.nearest.expect("nearest");
        let actual = value_at(&up, &sample).expect("guarded cell renders");
        assert_close(
            actual,
            expected,
            1e-5,
            &format!("row {} gate {}", sample.row, sample.gate),
        );
    }
    let (count, blended) = samples(&case, "blend");
    assert!(count > 10_000, "{count} blended cells");
    for sample in blended {
        let (min, max) = (sample.min.expect("min"), sample.max.expect("max"));
        assert!(min >= 0.97);
        let actual = value_at(&up, &sample).expect("blended cell renders");
        assert!(actual >= min - 1e-5 && actual <= max + 1e-5);
        assert_close(
            actual,
            sample.value.expect("value"),
            1e-5,
            &format!("row {} gate {}", sample.row, sample.gate),
        );
    }
    assert_rows_match(&case, &up);
}

/// NOXP sector scan (100 rays from 200 to 300 deg): sub-rows refine the
/// sector to quarter steps but none bridges the 260 deg gap back to the
/// first ray.
#[test]
fn sector_scan_gap_stays_native() {
    let Some(case) = load("sector") else {
        return;
    };
    let g = &case.golden;
    let up = case.upsample();
    let native = array(&g["native_azimuths_deg"]);
    let first = as_f64(&native[0]);
    let last = as_f64(&native[native.len() - 1]);
    assert!(first < 201.0 && last > 299.0);
    assert_eq!(up.sweep.rays.azimuth_deg.len(), as_usize(&g["out_rows"]));
    assert_eq!(up.sweep.rays.azimuth_deg.len(), 100 + 99 * 3);
    for (row, expected) in array(&g["row_azimuths_deg"]).iter().enumerate() {
        let actual = f64::from(up.sweep.rays.azimuth_deg[row]);
        assert!(
            actual >= first - 1e-3 && actual <= last + 1e-3,
            "row {row} at {actual} deg bridges the sector gap"
        );
        assert_close(actual, as_f64(expected), 1e-3, &format!("row {row}"));
    }
    assert_rows_match(&case, &up);
}

/// Every grid of a whole legacy volume (16 sweeps of REF, VEL and SW)
/// upsamples to the dimensions the policy gives for MetPy's geometry; pass
/// times are printed.
#[test]
fn upsample_cost_smoke() {
    let expected = golden("filters/interpolate.json");
    let path = recast_radar_testdata::require_file!("l2-ktlx-19990504-002218");
    let volume = common::level2(&path);
    let grids = array(&expected["volume"]["grids"]);
    let mut checked = 0;
    let mut timings = Vec::new();
    for item in grids {
        let cut = &volume.sweeps[as_usize(&item["sweep"])];
        let name = match item["moment"].as_str() {
            Some("REF") => FieldName::Dbzh,
            Some("VEL") => FieldName::Vradh,
            Some("SW") => FieldName::Wradh,
            other => panic!("unexpected moment {other:?}"),
        };
        let grid = cut.field(&name).expect("field in sweep");
        assert_eq!(grid.shape().0, as_usize(&item["rows"]));
        assert_eq!(grid.shape().1, as_usize(&item["gates"]));
        let start = std::time::Instant::now();
        let up = upsample_field(cut, grid).expect("legacy grids upsample");
        timings.push(start.elapsed().as_secs_f64() * 1000.0);
        let out = out_field(&up);
        assert_eq!(out.shape().0, as_usize(&item["out_rows"]), "{item}");
        assert_eq!(out.shape().1, as_usize(&item["out_gate_count"]), "{item}");
        assert_eq!(
            geometry(&up.sweep, out).1.round() as i64,
            as_i64(&item["out_gate_spacing_m"]),
            "{item}"
        );
        assert_eq!(up.sweep.rays.azimuth_deg.len(), out.shape().0);
        checked += 1;
    }
    let moments: usize = volume.sweeps.iter().map(|cut| cut.fields.len()).sum();
    assert_eq!(checked, moments);
    let total: f64 = timings.iter().sum();
    let slowest = timings.iter().copied().fold(0.0, f64::max);
    println!("upsample cost: {checked} grids in {total:.1} ms (slowest {slowest:.1} ms)");
}
