//! LLSD azimuthal shear and radial divergence on real Level II sweeps.
//!
//! Expected values come from `testdata/golden/retrieve/shear.json`
//! (`tools/retrieve_golden.py shear`): a numpy least-squares derivative
//! (Smith and Elmore 2004) on the MetPy-read raw velocity of the same sweep,
//! and on Py-ART's region-based dealiased velocity.

mod common;

use common::{
    array, as_f64, as_i64, as_str, as_usize, assert_close, assert_grid_matches, cell, find_case,
    golden, jma, level2, lowest_cut_with, map_values, moment,
};
use recast_radar_core::{Field, FieldData, FieldName};
use recast_radar_retrieve::{
    azimuthal_shear, azimuthal_shear_from_dealiased, radial_divergence,
    radial_divergence_from_dealiased,
};
use recast_radar_testdata::require_file;

/// Derivative tolerance in 1e-3/s: gate values are exact half-integers in both readers,
/// so only f32 rounding of the arc lengths separates the two implementations.
const TOLERANCE: f64 = 0.05;

/// Fraction of finite gates that must agree between the Rust internal dealiaser and
/// Py-ART's region-based dealiaser on the same sweep (the two engines differ on a few
/// gates at region boundaries).
const MIN_DEALIASED_AGREEMENT: f64 = 0.97;

fn agreement(grid: &Field, summary: &serde_json::Value) -> (f64, usize) {
    let mut agree = 0usize;
    let mut compared = 0usize;
    for entry in array(&summary["cells"]) {
        let entry = array(entry);
        let (row, gate) = (as_usize(&entry[0]), as_usize(&entry[1]));
        let expected = entry[2].as_f64();
        let actual = cell(grid, row, gate).map(f64::from);
        if let (Some(a), Some(e)) = (actual, expected) {
            compared += 1;
            if (a - e).abs() <= 0.5 {
                agree += 1;
            }
        }
    }
    (agree as f64 / compared.max(1) as f64, compared)
}

#[test]
fn moore_couplet_azimuthal_shear_matches_the_llsd_reference() {
    let path = require_file!("l2-ktlx-20130520-201643-trim");
    let volume = level2(&path);
    let golden = golden("retrieve/shear.json");
    let case = find_case(
        &golden["cases"],
        &[
            ("id", "l2-ktlx-20130520-201643-trim"),
            ("axis", "azimuthal"),
        ],
    );
    let cut = &volume.sweeps[as_usize(&case["sweep"])];
    assert_close(
        f64::from(cut.fixed_angle_deg),
        as_f64(&case["elevation_deg"]),
        1e-3,
        "sweep elevation",
    );
    let velocity = moment(cut, &FieldName::Vradh);
    let (first_gate_m, spacing_m) = velocity.native_geometry(&cut.range).expect("geometry");
    assert_eq!(first_gate_m, as_i64(&case["first_gate_m"]) as f64);
    assert_eq!(spacing_m, as_i64(&case["gate_spacing_m"]) as f64);

    // The explicit entry point differentiates exactly the grid it is given: the raw
    // velocity here, gate for gate against numpy on MetPy's raw velocity.
    let shear = azimuthal_shear_from_dealiased(cut, velocity);
    assert_grid_matches(&shear, &case["raw"], TOLERANCE, "azimuthal shear (raw)");
    for (key, sign) in [("raw_max", 1.0), ("raw_min", -1.0)] {
        let extreme = &case[key];
        let value = cell(
            &shear,
            as_usize(&extreme["row"]),
            as_usize(&extreme["gate"]),
        )
        .expect("extreme cell finite");
        assert_close(f64::from(value), as_f64(&extreme["value"]), TOLERANCE, key);
        assert!(sign * f64::from(value) > 100.0, "{key}: {value}");
    }

    // The internal-dealias entry point agrees with numpy on Py-ART's dealiased field
    // wherever the two dealiasers agree (262 of 108,077 gates were unfolded by Py-ART).
    let dealiased = azimuthal_shear(cut, velocity);
    assert_eq!(dealiased.shape().0, shear.shape().0);
    let (fraction, compared) = agreement(&dealiased, &case["dealiased"]);
    assert!(compared >= 300, "only {compared} sampled cells compared");
    assert!(
        fraction >= MIN_DEALIASED_AGREEMENT,
        "dealiased azimuthal shear agrees on {fraction:.3} of {compared} sampled cells"
    );
}

#[test]
fn derecho_radial_divergence_matches_the_llsd_reference() {
    let path = require_file!("l2-kdvn-20200810-180401-trim");
    let volume = level2(&path);
    let golden = golden("retrieve/shear.json");
    let case = find_case(
        &golden["cases"],
        &[("id", "l2-kdvn-20200810-180401-trim"), ("axis", "radial")],
    );
    let cut = &volume.sweeps[as_usize(&case["sweep"])];
    let velocity = moment(cut, &FieldName::Vradh);
    assert_close(
        f64::from(cut.ray_vars.nyquist_velocity_mps.as_ref().expect("Nyquist")[0]),
        as_f64(&case["nyquist_mps"]),
        0.01,
        "Nyquist velocity",
    );

    let divergence = radial_divergence_from_dealiased(cut, velocity);
    assert_grid_matches(
        &divergence,
        &case["raw"],
        TOLERANCE,
        "radial divergence (raw)",
    );
    for key in ["raw_max", "raw_min"] {
        let extreme = &case[key];
        let value = cell(
            &divergence,
            as_usize(&extreme["row"]),
            as_usize(&extreme["gate"]),
        )
        .expect("extreme cell finite");
        assert_close(f64::from(value), as_f64(&extreme["value"]), TOLERANCE, key);
    }
    // Rows and gates of the output follow the velocity grid.
    assert_eq!(divergence.shape(), velocity.shape());
    assert_eq!(divergence.gates, velocity.gates);
    assert_eq!(divergence.absent_rows, velocity.absent_rows);
}

#[test]
fn explicit_derivative_entry_points_never_run_a_second_dealias_pass() {
    // Py-ART unfolds 38,656 of this sweep's 84,964 velocity gates (Nyquist 21 m/s): a
    // hidden dealias pass would move the derivative away from the raw-velocity reference
    // at every fold boundary. The explicit entry points reproduce the raw reference
    // exactly; the internal-dealias entry points do not.
    let path = require_file!("l2-kdvn-20200810-180401-trim");
    let volume = level2(&path);
    let golden = golden("retrieve/shear.json");
    let cut = &volume.sweeps[1];
    let velocity = moment(cut, &FieldName::Vradh);
    for (axis, explicit, internal) in [
        (
            "radial",
            radial_divergence_from_dealiased(cut, velocity),
            radial_divergence(cut, velocity),
        ),
        (
            "azimuthal",
            azimuthal_shear_from_dealiased(cut, velocity),
            azimuthal_shear(cut, velocity),
        ),
    ] {
        let case = find_case(
            &golden["cases"],
            &[("id", "l2-kdvn-20200810-180401-trim"), ("axis", axis)],
        );
        assert!(as_usize(&case["pyart_unfolded_gates"]) > 30_000);
        assert_grid_matches(
            &explicit,
            &case["raw"],
            TOLERANCE,
            &format!("{axis} explicit"),
        );
        let (raw_agreement, _) = agreement(&internal, &case["raw"]);
        let (dealiased_agreement, compared) = agreement(&internal, &case["dealiased"]);
        assert!(compared >= 300);
        assert!(
            dealiased_agreement > raw_agreement,
            "{axis}: internal dealias agrees with the dealiased reference on \
             {dealiased_agreement:.3} and with the raw reference on {raw_agreement:.3}"
        );
        assert!(
            dealiased_agreement >= 0.9,
            "{axis}: internal dealias agrees with the Py-ART reference on {dealiased_agreement:.3}"
        );
    }
}

#[test]
fn degraded_velocity_yields_no_data_without_panicking() {
    // A real velocity sweep without any Nyquist velocity (JMA N6, staggered PRF): the
    // derivatives are still defined wherever velocity is.
    let golden = golden("retrieve/shear.json");
    let jma_path = require_file!(as_str(&golden["no_nyquist"]["id"]));
    let jma_volume = jma(&jma_path);
    assert_eq!(
        jma_volume.sweeps.len(),
        as_usize(&golden["no_nyquist"]["sweeps"])
    );
    let cut_index = lowest_cut_with(&jma_volume, &FieldName::Vradh);
    let cut = &jma_volume.sweeps[cut_index];
    assert!(cut.ray_vars.nyquist_velocity_mps.is_none());
    let velocity = moment(cut, &FieldName::Vradh);
    let shear = azimuthal_shear(cut, velocity);
    let finite = (0..shear.shape().0)
        .flat_map(|row| (0..shear.shape().1).map(move |gate| (row, gate)))
        .filter(|&(row, gate)| cell(&shear, row, gate).is_some())
        .count();
    assert!(finite > 1_000, "JMA shear has {finite} finite cells");

    // The same sweep with every velocity gate blanked: all-NaN output, no panic.
    let path = require_file!("l2-ktlx-20130520-201643-trim");
    let volume = level2(&path);
    let cut = &volume.sweeps[1];
    let mut blank = moment(cut, &FieldName::Vradh).clone();
    map_values(&mut blank, |_| f32::NAN);
    for grid in [azimuthal_shear(cut, &blank), radial_divergence(cut, &blank)] {
        assert_eq!(grid.shape().0, blank.shape().0);
        for row in 0..grid.shape().0 {
            for gate in 0..grid.shape().1 {
                assert!(cell(&grid, row, gate).is_none());
            }
        }
    }

    // And with the gate axis cut to zero length: empty output, no panic.
    let mut empty = moment(cut, &FieldName::Vradh).clone();
    empty.ngates = 0;
    let FieldData::U8 { values, .. } = &mut empty.data else {
        panic!("Level II velocity is 8-bit");
    };
    values.clear();
    assert_eq!(azimuthal_shear(cut, &empty).shape().1, 0);
    assert_eq!(radial_divergence(cut, &empty).shape().1, 0);
}
