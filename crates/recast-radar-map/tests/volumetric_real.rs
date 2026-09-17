//! Volume products and cross-sections on real NEXRAD volumes.
//!
//! Expected values: `testdata/golden/map/volumetric.json`, written by
//! `tools/filters_map_golden.py volumetric` from MetPy 1.7.1 (tilt geometry
//! and reflectivity/velocity gates) with a numpy column-walk reference:
//! composite = column maximum, echo top = highest beam with Z >= 18.3 dBZ,
//! VIL (Greene and Clark 1972, 56 dBZ hail cap, surface layer from the lowest
//! beam), MEHS (Witt et al. 1998 SHI with a 3.2 km melting level and 6.4 km
//! -20 C level), VIL density, and MRMS-style cross-sections (Zhang et al.
//! 2005). Py-ART 2.2.5 `dealias_region_based` selects the velocity path that
//! needs no unfolding.
//!
//! Volumes: KEWX 2016-04-13 02:25Z San Antonio hailstorm (19 tilts, VCP 212
//! with SAILS), KTLX 2024-05-15 clear air (VCP 35), KTLX 2013-05-20 Moore
//! tornado (17 tilts), KTLX 1999-05-03 truncated after 68 radials, a TDWR
//! status-only object with no radials, and the JMA radial-velocity product.

mod common;

use common::{array, as_f64, as_i64, as_opt_f64, as_usize, assert_close, golden, row_stats};
use recast_radar_core::{Field, FieldData, FieldName, Quantity, Sweep, Volume};
use recast_radar_map as products;
use recast_radar_map::{
    CrossSection, CrossSectionSmoothing, ECHO_TOP_THRESHOLD_DBZ, InterpPolicy, MeshCalibration,
    VolumeDealiasCache, box_resample, field_section_with_smoothing, reflectivity_section,
    reflectivity_section_with_smoothing, velocity_section, velocity_section_cached_with_smoothing,
};
use serde_json::Value;

/// The sweep's reflectivity field.
fn reflectivity(sweep: &Sweep) -> &Field {
    sweep.find(Quantity::Reflectivity).expect("REF")
}

/// Decode a Level II volume for a golden case and check its reflectivity
/// tilts (sweep, tilt elevation, rays, gate geometry) against MetPy. The tilt
/// elevation is the fixed angle, the Message 5 cut angle, so the split cuts and
/// SAILS repeats of one angle tie; the golden lists the tilts sorted by
/// elevation with ties in acquisition order, as the products walk them.
fn product_volume(key: &str) -> Option<(Value, Volume)> {
    let golden = golden("map/volumetric.json")[key].clone();
    let id = golden["id"].as_str().expect("id").to_owned();
    let path = match recast_radar_testdata::path(&id) {
        Ok(path) => path,
        Err(error) if error.is_offline() => {
            eprintln!("skipping: {error}");
            return None;
        }
        Err(error) => panic!("{error}"),
    };
    let volume = common::level2(&path);
    let tilts = array(&golden["tilts"]);
    let reflectivity_cuts = volume
        .sweeps
        .iter()
        .filter(|cut| cut.find(Quantity::Reflectivity).is_some())
        .count();
    assert_eq!(reflectivity_cuts, tilts.len(), "{id} reflectivity tilts");
    let mut by_elevation: Vec<usize> = volume
        .sweeps
        .iter()
        .enumerate()
        .filter(|(_, cut)| cut.find(Quantity::Reflectivity).is_some())
        .map(|(index, _)| index)
        .collect();
    by_elevation.sort_by(|a, b| {
        volume.sweeps[*a]
            .fixed_angle_deg
            .total_cmp(&volume.sweeps[*b].fixed_angle_deg)
    });
    let golden_order: Vec<usize> = tilts.iter().map(|tilt| as_usize(&tilt["sweep"])).collect();
    assert_eq!(by_elevation, golden_order, "{id} tilt order");
    for tilt in tilts {
        let cut = &volume.sweeps[as_usize(&tilt["sweep"])];
        let grid = reflectivity(cut);
        assert_close(
            f64::from(cut.fixed_angle_deg),
            as_f64(&tilt["elevation_deg"]),
            1e-5,
            &format!("{id} tilt {} elevation", tilt["sweep"]),
        );
        assert_eq!(grid.shape().0, as_usize(&tilt["rays"]));
        assert_eq!(grid.shape().1, as_usize(&tilt["gates"]));
        assert!(grid.absent_rows.is_empty());
        let (first_m, spacing_m) = grid.native_geometry(&cut.range).expect("geometry");
        assert_eq!(first_m, as_i64(&tilt["first_gate_m"]) as f64);
        assert_eq!(spacing_m, as_i64(&tilt["gate_spacing_m"]) as f64);
    }
    Some((golden, volume))
}

/// Grid on the base tilt's geometry whose per-row coverage and sums equal the
/// reference, with the reference maximum at the same row and gate.
fn assert_product(grid: &Field, golden: &Value, what: &str) {
    assert_eq!(grid.shape().0, as_usize(&golden["rows"]), "{what} rows");
    assert_eq!(grid.shape().1, as_usize(&golden["gates"]), "{what} gates");
    let expected = &golden[what];
    let row_valid = array(&expected["row_valid"]);
    let row_sum = array(&expected["row_sum"]);
    let stats = row_stats(grid);
    for (row, (count, sum)) in stats.iter().enumerate() {
        assert_eq!(
            *count,
            as_usize(&row_valid[row]),
            "{what} row {row} coverage"
        );
        let reference = as_f64(&row_sum[row]);
        let tolerance = 2e-3 + 1e-6 * reference.abs();
        assert_close(*sum, reference, tolerance, &format!("{what} row {row} sum"));
    }
    let valid: usize = stats.iter().map(|(count, _)| count).sum();
    assert_eq!(valid, as_usize(&expected["valid"]), "{what} valid cells");
    if let Some(max) = expected["max"].as_object() {
        let (row, gate) = (as_usize(&max["row"]), as_usize(&max["gate"]));
        let value = common::cell(grid, row, gate).expect("maximum cell");
        let reference = as_f64(&max["value"]);
        assert_close(f64::from(value), reference, 1e-4 * reference.abs(), what);
        let largest = (0..grid.shape().0)
            .flat_map(|r| (0..grid.shape().1).map(move |g| (r, g)))
            .filter_map(|(r, g)| common::cell(grid, r, g))
            .fold(f32::MIN, f32::max);
        assert_eq!(largest, value, "{what} maximum");
    }
}

fn base_cut<'a>(golden: &Value, volume: &'a Volume) -> &'a Sweep {
    &volume.sweeps[as_usize(&golden["base_sweep"])]
}

/// KEWX hailstorm: the composite is the column maximum over all 19 tilts
/// (76.5 dBZ in the core at 251 deg, 55.6 km), never below the base tilt, and
/// above it wherever a higher tilt is stronger.
#[test]
fn composite_takes_column_max() {
    let Some((golden, volume)) = product_volume("hail") else {
        return;
    };
    let composite = products::composite_reflectivity(&volume).expect("composite");
    assert_product(&composite, &golden, "composite");
    assert_eq!(
        as_f64(&golden["composite"]["max"]["value"]),
        as_f64(&golden["volume_max_dbz"])
    );

    let base = reflectivity(base_cut(&golden, &volume));
    assert_eq!(composite.shape(), base.shape());
    assert_eq!(composite.gates, base.gates);
    assert_eq!(composite.absent_rows, base.absent_rows);
    let mut above = 0;
    for row in 0..base.shape().0 {
        for gate in 0..base.shape().1 {
            if let Some(low) = common::cell(base, row, gate) {
                let column = common::cell(&composite, row, gate).expect("composite cell");
                assert!(
                    column >= low,
                    "row {row} gate {gate}: {column} < base {low}"
                );
                above += usize::from(column > low);
            }
        }
    }
    assert_eq!(above, as_usize(&golden["composite_above_base"]));
}

/// KEWX: echo tops (18.3 dBZ) come from the highest tilt with echo; at most
/// gates that is above the base beam (21.0 km maximum at 75 deg, 386 km).
#[test]
fn echo_top_rises_with_higher_tilt() {
    let Some((golden, volume)) = product_volume("hail") else {
        return;
    };
    assert_eq!(ECHO_TOP_THRESHOLD_DBZ, 18.3);
    let tops = products::echo_top(&volume, ECHO_TOP_THRESHOLD_DBZ).expect("echo top");
    assert_product(&tops, &golden, "echo_top");

    let cut = base_cut(&golden, &volume);
    let base = reflectivity(cut);
    let (first_m, spacing_m) = base.native_geometry(&cut.range).expect("geometry");
    let mut above = 0;
    for gate in 0..base.shape().1 {
        let slant = first_m + gate as f64 * spacing_m;
        let beam =
            recast_radar_core::beam_height_above_radar_m(slant, f64::from(cut.fixed_angle_deg))
                as f32;
        for row in 0..tops.shape().0 {
            if let Some(top) = common::cell(&tops, row, gate) {
                above += usize::from(top > beam);
            }
        }
    }
    assert_eq!(above, as_usize(&golden["echo_top_above_base_beam"]));
    assert!(above * 2 > as_usize(&golden["echo_top"]["valid"]));
}

/// KTLX Moore tornado: a vertical section W of the radar through the
/// supercell (45 km to 5 km west, 0.8 km south; 160 x 90 pixels to 18 km)
/// equals the reference pixel by pixel, natively and with the path smoothing.
#[test]
fn cross_section_reconstructs_a_reflectivity_column() {
    let sections = golden("map/volumetric.json")["cross_sections"].clone();
    let path = recast_radar_testdata::require_file!(sections["id"].as_str().expect("id"));
    let volume = common::level2(&path);
    for spec in array(&sections["reflectivity"]) {
        let (start, end) = endpoints(spec);
        let (width, height) = (as_usize(&spec["width"]), as_usize(&spec["height"]));
        let top = as_f64(&spec["top_m"]) as f32;
        let native = reflectivity_section_with_smoothing(
            &volume,
            start,
            end,
            width,
            height,
            top,
            CrossSectionSmoothing::Native,
        )
        .expect("native section");
        assert_section(&native, &spec["native"], "reflectivity native");
        let smoothed =
            reflectivity_section(&volume, start, end, width, height, top).expect("section");
        assert_section(&smoothed, &spec["smoothed"], "reflectivity smoothed");
        if start.1 < 0.0 {
            // Through the supercell: pixels between tilts, below the lowest
            // beam and above the highest, and a core past 60 dBZ.
            let kinds = &spec["kinds"];
            for kind in ["blend", "below", "above"] {
                assert!(as_usize(&kinds[kind]) > 100, "{kind} pixels");
            }
            let peak = native
                .values
                .iter()
                .copied()
                .filter(|v| v.is_finite())
                .fold(f32::MIN, f32::max);
            assert!(peak > 60.0, "section peak {peak}");
        } else {
            // Across the radar: no tilt samples within about 2 km of the site
            // (gates start at 2.125 km of slant range), so the middle columns
            // are empty while both ends carry echo.
            let column_filled = |x: usize| {
                (0..native.height).any(|y| native.values[y * native.width + x].is_finite())
            };
            assert!(!column_filled(native.width / 2));
            assert!(column_filled(0) && column_filled(native.width - 1));
        }
    }
    assert_eq!(array(&sections["reflectivity"]).len(), 2);
}

/// KTLX Moore tornado velocity: across the mesocyclone the raw velocity
/// section keeps the nearest tilt wherever bracketing tilts differ by more
/// than 30 m/s; on a path Py-ART's region-based dealiasing leaves untouched,
/// the dealiased velocity section equals the raw reference.
#[test]
fn velocity_cross_section_reconstructs_velocity() {
    let sections = golden("map/volumetric.json")["cross_sections"].clone();
    let path = recast_radar_testdata::require_file!(sections["id"].as_str().expect("id"));
    let volume = common::level2(&path);
    let mut cache = VolumeDealiasCache::new();
    for spec in array(&sections["velocity"]) {
        let label = spec["label"].as_str().expect("label");
        let (start, end) = endpoints(spec);
        let (width, height) = (as_usize(&spec["width"]), as_usize(&spec["height"]));
        let top = as_f64(&spec["top_m"]) as f32;
        let raw = field_section_with_smoothing(
            &volume,
            &FieldName::Vradh,
            InterpPolicy::VelocityGuard,
            start,
            end,
            width,
            height,
            top,
            CrossSectionSmoothing::Native,
        )
        .expect("raw velocity section");
        assert_section(&raw, &spec["native"], label);
        match label {
            "couplet" => {
                assert!(as_usize(&spec["kinds"]["guarded"]) > 500);
                assert!(as_usize(&spec["pyart_region_dealias_changed_gates"]) > 0);
            }
            "quiet_north" => {
                assert_eq!(as_usize(&spec["pyart_region_dealias_changed_gates"]), 0);
                assert!(as_usize(&spec["sampled_gates"]) > 1000);
                let dealiased = velocity_section_cached_with_smoothing(
                    &volume,
                    &mut cache,
                    start,
                    end,
                    width,
                    height,
                    top,
                    CrossSectionSmoothing::Native,
                )
                .expect("dealiased velocity section");
                assert_section(&dealiased, &spec["native"], "dealiased quiet_north");
                let default = velocity_section(&volume, start, end, width, height, top)
                    .expect("smoothed velocity section");
                assert_eq!(default.values.len(), width * height);
                let finite = default.values.iter().filter(|v| v.is_finite()).count();
                let native_finite = dealiased.values.iter().filter(|v| v.is_finite()).count();
                assert!(finite >= native_finite, "path smoothing only fills gaps");
            }
            _ => panic!("unexpected section {label}"),
        }
    }
}

/// Real degraded inputs: a TDWR archive object with no radials, a legacy
/// volume truncated after 68 reflectivity radials (products equal the
/// reference; hail needs two tilts), the same volume with every reflectivity
/// gate overwritten by the no-data code, the JMA velocity-only product, and
/// degenerate section arguments.
#[test]
fn derived_products_handle_degraded_inputs_without_panicking() {
    let path = recast_radar_testdata::require_file!("l2-tbwi-20230601-175101-stub");
    let empty = common::level2(&path);
    assert!(empty.sweeps.is_empty());
    assert!(products::composite_reflectivity(&empty).is_none());
    assert!(products::echo_top(&empty, ECHO_TOP_THRESHOLD_DBZ).is_none());
    assert!(products::vil(&empty).is_none());
    assert!(products::vil_density(&empty).is_none());
    assert!(products::mehs(&empty, 3200.0, 6400.0).is_none());
    assert!(products::poh(&empty, 3200.0).is_none());
    assert!(products::hail(&empty, 3200.0, 6400.0, MeshCalibration::Witt1998).is_none());
    assert!(reflectivity_section(&empty, (0.0, 0.0), (50.0, 0.0), 64, 32, 18_000.0).is_none());
    assert!(velocity_section(&empty, (0.0, 0.0), (50.0, 0.0), 64, 32, 18_000.0).is_none());
    assert!(box_resample(&empty, 0.0, 0.0, 50.0, 16, 8, 15_000.0).is_none());

    let Some((golden, mut truncated)) = product_volume("truncated") else {
        return;
    };
    assert_eq!(truncated.sweeps.len(), as_usize(&golden["sweeps"]));
    let composite = products::composite_reflectivity(&truncated).expect("composite");
    assert_product(&composite, &golden, "composite");
    assert_eq!(
        as_usize(&golden["composite"]["valid"]),
        as_usize(&golden["reflectivity_valid"]),
        "one tilt: the composite is the tilt"
    );
    let tops = products::echo_top(&truncated, ECHO_TOP_THRESHOLD_DBZ).expect("echo top");
    assert_product(&tops, &golden, "echo_top");
    assert_product(&products::vil(&truncated).expect("vil"), &golden, "vil");
    assert_product(
        &products::vil_density(&truncated).expect("density"),
        &golden,
        "vil_density",
    );
    let mehs = products::mehs(&truncated, 3200.0, 6400.0).expect("mehs grid");
    assert_eq!(as_usize(&golden["mehs"]["valid"]), 0);
    assert_product(&mehs, &golden, "mehs");
    assert!(velocity_section(&truncated, (0.0, 0.0), (50.0, 0.0), 64, 32, 18_000.0).is_none());
    let section = reflectivity_section(
        &truncated,
        (-60.0, -100.0),
        (-40.0, -150.0),
        64,
        32,
        18_000.0,
    )
    .expect("reflectivity section");
    assert_eq!(section.values.len(), 64 * 32);

    // Mutate the real codes: every reflectivity gate becomes the no-data code.
    for cut in &mut truncated.sweeps {
        let index = cut.field_index(&FieldName::Dbzh).expect("REF");
        match &mut cut.fields[index].data {
            FieldData::U8 { values, coding } => {
                values.fill(coding.fill_value.expect("Level II no-data code"));
            }
            FieldData::U16 { values, coding } => {
                values.fill(coding.fill_value.expect("Level II no-data code"));
            }
            other => panic!("Level II reflectivity is coded, not {}", other.dtype()),
        }
    }
    for grid in [
        products::composite_reflectivity(&truncated).expect("composite"),
        products::echo_top(&truncated, ECHO_TOP_THRESHOLD_DBZ).expect("echo top"),
        products::vil(&truncated).expect("vil"),
        products::vil_density(&truncated).expect("density"),
    ] {
        assert!(row_stats(&grid).iter().all(|(count, _)| *count == 0));
    }

    let path = recast_radar_testdata::require_file!("jma-n6-20191012-090000-rs47773");
    let velocity_only = common::jma(&path);
    assert!(!velocity_only.sweeps.is_empty());
    assert!(products::composite_reflectivity(&velocity_only).is_none());
    assert!(products::echo_top(&velocity_only, ECHO_TOP_THRESHOLD_DBZ).is_none());
    assert!(products::vil(&velocity_only).is_none());
    assert!(
        reflectivity_section(&velocity_only, (0.0, 0.0), (50.0, 0.0), 64, 32, 18_000.0).is_none()
    );
    let section = velocity_section(&velocity_only, (0.0, 5.0), (60.0, 5.0), 64, 32, 12_000.0)
        .expect("velocity section");
    assert!(
        section.values.iter().any(|v| v.is_finite()),
        "Hagibis velocity on the path"
    );

    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
    let two_cuts = common::level2(&path);
    assert_eq!(two_cuts.sweeps.len(), 2);
    let section = |w, h, top| reflectivity_section(&two_cuts, (0.0, 0.0), (50.0, 0.0), w, h, top);
    assert!(section(1, 32, 18_000.0).is_none());
    assert!(section(64, 1, 18_000.0).is_none());
    assert!(section(64, 32, 0.0).is_none());
    assert!(section(64, 32, 18_000.0).is_some());
    assert!(box_resample(&two_cuts, 0.0, 0.0, 50.0, 4, 8, 15_000.0).is_none());
}

/// KEWX: VIL over the whole volume equals the reference; the heaviest column
/// holds 56.4 kg/m2.
#[test]
fn vil_positive_for_deep_reflectivity() {
    let Some((golden, volume)) = product_volume("hail") else {
        return;
    };
    let vil = products::vil(&volume).expect("vil");
    assert_product(&vil, &golden, "vil");
    let max = as_f64(&golden["vil"]["max"]["value"]);
    assert!(max > 40.0 && max < 80.0, "VIL maximum {max}");
    for row in 0..vil.shape().0 {
        for gate in 0..vil.shape().1 {
            if let Some(value) = common::cell(&vil, row, gate) {
                assert!(value > 0.0);
            }
        }
    }
}

/// MEHS flags the San Antonio hail core (111 mm at 250 deg, 55.4 km, one gate
/// from the 76.5 dBZ composite maximum) and nothing in KTLX clear air.
#[test]
fn mehs_flags_deep_intense_cores_only() {
    let Some((golden, volume)) = product_volume("hail") else {
        return;
    };
    let freezing = as_f64(&golden_root()["freezing_level_m"]) as f32;
    let minus20 = as_f64(&golden_root()["minus20c_level_m"]) as f32;
    let mehs = products::mehs(&volume, freezing, minus20).expect("mehs");
    assert_product(&mehs, &golden, "mehs");
    let max = &golden["mehs"]["max"];
    assert!(as_f64(&max["value"]) > 25.0);
    let composite_max = &golden["composite"]["max"];
    assert!((as_i64(&max["gate"]) - as_i64(&composite_max["gate"])).abs() <= 1);
    assert!((as_i64(&max["row"]) - as_i64(&composite_max["row"])).abs() <= 3);
    // hail_grids with the Witt calibration reports the same MESH.
    let hail = products::hail(&volume, freezing, minus20, MeshCalibration::Witt1998).expect("hail");
    assert_product(&hail.mesh_mm, &golden, "mehs");

    let Some((clear, volume)) = product_volume("clear_air") else {
        return;
    };
    assert!(as_f64(&clear["volume_max_dbz"]) < 45.0);
    let mehs = products::mehs(&volume, freezing, minus20).expect("mehs grid");
    assert_eq!(as_usize(&clear["mehs"]["valid"]), 0);
    assert_product(&mehs, &clear, "mehs");
}

/// KEWX: VIL density (VIL over the echo top where the top is above 1.5 km)
/// equals the reference and stays within physical column densities.
#[test]
fn vil_density_is_in_physical_range() {
    let Some((golden, volume)) = product_volume("hail") else {
        return;
    };
    let density = products::vil_density(&volume).expect("vil density");
    assert_product(&density, &golden, "vil_density");
    for row in 0..density.shape().0 {
        for gate in 0..density.shape().1 {
            if let Some(value) = common::cell(&density, row, gate) {
                assert!(
                    value > 0.0 && value < 10.0,
                    "row {row} gate {gate}: {value} g/m3"
                );
            }
        }
    }
    let max = as_f64(&golden["vil_density"]["max"]["value"]);
    assert!(max > 3.5, "large-hail density signal {max}");
}

fn golden_root() -> Value {
    golden("map/volumetric.json")
}

fn endpoints(spec: &Value) -> ((f32, f32), (f32, f32)) {
    let start = array(&spec["start_km"]);
    let end = array(&spec["end_km"]);
    (
        (as_f64(&start[0]) as f32, as_f64(&start[1]) as f32),
        (as_f64(&end[0]) as f32, as_f64(&end[1]) as f32),
    )
}

/// Section values equal the reference (rows of `null` for no data).
fn assert_section(section: &CrossSection, expected: &Value, what: &str) {
    let rows = array(expected);
    assert_eq!(rows.len(), section.height, "{what} height");
    for (y, row) in rows.iter().enumerate() {
        let row = array(row);
        assert_eq!(row.len(), section.width, "{what} width");
        for (x, value) in row.iter().enumerate() {
            let actual = section.values[y * section.width + x];
            match as_opt_f64(value) {
                None => assert!(actual.is_nan(), "{what} ({x}, {y}): {actual} where none"),
                Some(reference) => assert_close(
                    f64::from(actual),
                    reference,
                    2e-4,
                    &format!("{what} ({x}, {y})"),
                ),
            }
        }
    }
}
