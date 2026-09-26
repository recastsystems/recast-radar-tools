//! The three KDP estimators on real S-band split cuts against Py-ART.
//!
//! Expected values come from `testdata/golden/retrieve/kdp_methods.json`
//! (`tools/retrieve_golden.py kdp_methods`). The golden script runs the
//! documented phase front end (RHOHV/reflectivity gating, unwrapping, gap
//! fill, Hampel filter) in numpy on MetPy-read moments, hands the filtered
//! phase to Py-ART's `kdp_vulpiani` (band S, windsize 10, n_iter 10), and
//! minimises Py-ART's own Maesaka cost function (`_cost_maesaka`,
//! `_jac_maesaka`, `boundary_conditions_maesaka`) ray by ray with scipy's
//! L-BFGS-B to convergence.

mod common;

use common::{array, as_f64, as_str, as_usize, cell, golden, level2};
use recast_radar_core::{Field, Sweep};
use recast_radar_retrieve::{
    DerivationConfig, DerivedSweepProduct, KdpMethod, MaesakaKdp, RadarBand, VulpianiKdp,
    derive_product,
};
use serde_json::Value;

fn case_sweep(case: &Value) -> Option<Sweep> {
    let path = match recast_radar_testdata::path(as_str(&case["id"])) {
        Ok(path) => path,
        Err(error) if error.is_offline() => return None,
        Err(error) => panic!("{error}"),
    };
    Some(level2(&path).sweeps[as_usize(&case["sweep"])].clone())
}

fn kdp(sweep: &Sweep, method: KdpMethod) -> Field {
    let mut config = DerivationConfig::with_products(RadarBand::S, [DerivedSweepProduct::Kdp]);
    config.kdp.method = method;
    derive_product(sweep, DerivedSweepProduct::Kdp, &config)
        .unwrap_or_else(|| panic!("no KDP for {method:?}"))
}

fn finite_gates(field: &Field) -> usize {
    let (rows, gates) = field.shape();
    (0..rows)
        .map(|row| {
            (0..gates)
                .filter(|&gate| cell(field, row, gate).is_some())
                .count()
        })
        .sum()
}

/// Vulpiani is deterministic: the same filtered phase gives Py-ART's KDP up to the
/// f32 rounding of the filtered phase the two front ends share (the Rust Hampel and
/// unwrapping run in f32 like the reference) and the order of the texture sum.
#[test]
fn vulpiani_kdp_matches_pyart_kdp_vulpiani() {
    let golden = golden("retrieve/kdp_methods.json");
    for case in array(&golden["cases"]) {
        let name = as_str(&case["case"]);
        let Some(sweep) = case_sweep(case) else {
            return;
        };
        let field = kdp(&sweep, KdpMethod::Vulpiani(VulpianiKdp::default()));
        let expected = &case["vulpiani"];
        assert_eq!(
            finite_gates(&field),
            as_usize(&expected["summary"]["gates"]),
            "{name}: Vulpiani gates"
        );
        let mut worst = 0.0f64;
        let mut sum = 0.0f64;
        for entry in array(&expected["cells"]) {
            let entry = array(entry);
            let (row, gate) = (as_usize(&entry[0]), as_usize(&entry[1]));
            let actual = f64::from(cell(&field, row, gate).expect("Vulpiani KDP"));
            let expected = as_f64(&entry[2]);
            worst = worst.max((actual - expected).abs());
            sum += actual;
            assert!(
                (actual - expected).abs() <= 1e-5,
                "{name} ({row}, {gate}): {actual} != Py-ART {expected}"
            );
        }
        eprintln!("{name}: Vulpiani worst |diff| {worst:.2e} deg/km, sampled sum {sum:.2}");
    }
}

/// Maesaka is an optimisation: the crate's per-ray L-BFGS and scipy's L-BFGS-B stop
/// at slightly different points of the same minimum. Nearly every gate agrees to a
/// few hundredths of a deg/km; the tail is gates where k passes near zero, where the
/// cost is flat. Neither applies band bounds, so the largest reported KDP is the
/// reference's spike (measured 333.24, 85.71 and 47.33 deg/km against 333.24, 85.71
/// and 47.26).
#[test]
fn maesaka_kdp_matches_the_converged_pyart_cost_minimum() {
    let golden = golden("retrieve/kdp_methods.json");
    for case in array(&golden["cases"]) {
        let name = as_str(&case["case"]);
        let Some(sweep) = case_sweep(case) else {
            return;
        };
        let field = kdp(&sweep, KdpMethod::Maesaka(MaesakaKdp::default()));
        let expected = &case["maesaka"];
        assert_eq!(
            finite_gates(&field),
            as_usize(&expected["summary"]["gates"]),
            "{name}: Maesaka gates (Py-ART boundary conditions)"
        );
        // No band bounds: the largest reported KDP is the reference's spike.
        let (rows, gates) = field.shape();
        let largest = (0..rows)
            .flat_map(|row| (0..gates).map(move |gate| (row, gate)))
            .filter_map(|(row, gate)| cell(&field, row, gate))
            .map(f64::from)
            .fold(f64::NEG_INFINITY, f64::max);
        let reference_max = as_f64(&expected["summary"]["max"]);
        eprintln!("{name}: Maesaka max {largest:.3} deg/km, reference {reference_max:.3}");
        assert!(
            (largest - reference_max).abs() <= 0.01 * reference_max,
            "{name}: Maesaka max {largest} against the reference's {reference_max}"
        );
        let mut differences: Vec<f64> = array(&expected["cells"])
            .iter()
            .map(|entry| {
                let entry = array(entry);
                let (row, gate) = (as_usize(&entry[0]), as_usize(&entry[1]));
                let actual = f64::from(cell(&field, row, gate).expect("Maesaka KDP"));
                (actual - as_f64(&entry[2])).abs()
            })
            .collect();
        differences.sort_by(f64::total_cmp);
        let quantile = |q: f64| differences[((differences.len() - 1) as f64 * q) as usize];
        eprintln!(
            "{name}: Maesaka |diff| p50 {:.2e} p95 {:.2e} p99 {:.2e} max {:.2e}",
            quantile(0.5),
            quantile(0.95),
            quantile(0.99),
            quantile(1.0)
        );
        assert!(quantile(0.5) <= 1e-6, "{name}: median {}", quantile(0.5));
        assert!(quantile(0.95) <= 1e-3, "{name}: p95 {}", quantile(0.95));
        assert!(quantile(0.99) <= 5e-3, "{name}: p99 {}", quantile(0.99));
        assert!(quantile(1.0) <= 0.1, "{name}: max {}", quantile(1.0));
    }
}

/// The per-ray evaluation budget bounds the solver's work. On the derecho cut, whose
/// rays use about 3,300 cost evaluations each to converge, a budget of 50 stops every
/// ray early at the best point found: KDP is still reported at the gates the boundary
/// conditions allow, finite and non-negative, but it is not the converged minimum.
#[test]
fn maesaka_evaluation_budget_stops_the_solver_early() {
    let golden = golden("retrieve/kdp_methods.json");
    let Some(case) = array(&golden["cases"])
        .iter()
        .find(|case| as_str(&case["case"]) == "derecho")
    else {
        panic!("no derecho case");
    };
    let Some(sweep) = case_sweep(case) else {
        return;
    };
    let converged = kdp(&sweep, KdpMethod::Maesaka(MaesakaKdp::default()));
    let mut limited_config = MaesakaKdp::default();
    assert_eq!(limited_config.max_cost_evaluations, 10_000);
    limited_config.max_cost_evaluations = 50;
    let limited = kdp(&sweep, KdpMethod::Maesaka(limited_config));
    assert_eq!(finite_gates(&limited), finite_gates(&converged));
    let (rows, gates) = limited.shape();
    let mut moved = 0usize;
    for row in 0..rows {
        for gate in 0..gates {
            let Some(value) = cell(&limited, row, gate) else {
                continue;
            };
            assert!(value >= 0.0, "({row}, {gate}): {value}");
            let Some(reference) = cell(&converged, row, gate) else {
                panic!("({row}, {gate}): converged KDP missing");
            };
            if (value - reference).abs() > 0.1 {
                moved += 1;
            }
        }
    }
    eprintln!(
        "derecho: {moved} of {} gates move by more than 0.1 deg/km with 50 evaluations a ray",
        finite_gates(&limited)
    );
    assert!(moved > 0, "a budget of 50 evaluations did not bind");
}

/// The windowed regression stays the default: its product set includes the slope
/// standard error, which the other methods do not have.
#[test]
fn regression_is_the_default_and_the_only_method_with_an_uncertainty() {
    let config = DerivationConfig::analyst_defaults();
    assert_eq!(config.kdp.method, KdpMethod::WindowedRegression);
    let golden = golden("retrieve/kdp_methods.json");
    let Some(sweep) = case_sweep(&array(&golden["cases"])[0]) else {
        return;
    };
    let mut config = config.clone();
    assert!(derive_product(&sweep, DerivedSweepProduct::KdpUncertainty, &config).is_some());
    config.kdp.method = KdpMethod::Vulpiani(VulpianiKdp::default());
    assert!(derive_product(&sweep, DerivedSweepProduct::KdpUncertainty, &config).is_none());
    assert!(
        derive_product(
            &sweep,
            DerivedSweepProduct::FilteredDifferentialPhase,
            &config
        )
        .is_some()
    );
}
