//! Z-PHI attenuation correction on real S-band split cuts against Py-ART.
//!
//! Expected values come from `testdata/golden/retrieve/attenuation.json`
//! (`tools/retrieve_golden.py attenuation`): Py-ART's
//! `calculate_attenuation_zphi` (fixed freezing level 4 km above the radar,
//! 15 excluded end gates, S-band coefficients) run on Py-ART's reading of the
//! sweep, with the propagation phase set to the numpy phase-bundle phase excess
//! (the filtered phase less its near-range median), which is what the crate
//! feeds Z-PHI. Two runs per sweep: without reflectivity smoothing, and with
//! the default 5-gate mean after replacing Py-ART's `smooth_masked` by one that
//! averages unmasked gates only (Py-ART's rolling window drops the mask and
//! averages the underlying data of masked gates too, -33 dBZ for NEXRAD).

mod common;

use common::{array, as_f64, as_str, as_usize, cell, golden, level2};
use recast_radar_core::{Field, Sweep};
use recast_radar_retrieve::{
    AttenuationMethod, DerivationConfig, DerivedSweepProduct, KdpMethod, MaesakaKdp, RadarBand,
    VulpianiKdp, ZPhiAttenuation, derive_sweep_in_place,
};

/// The phase excess comes from the regression phase bundle, which the Rust and
/// numpy implementations reproduce to 0.01 deg (the f64 fit summation order); at
/// S band (0.02 dB/deg) that moves the path-integrated attenuation by well under
/// 1e-3 dB.
const PIA_TOLERANCE_DB: f64 = 2.0e-3;
/// Relative tolerance of the specific attenuations, whose normalising integral
/// feels the same phase difference.
const SPECIFIC_RELATIVE_TOLERANCE: f64 = 2.0e-3;

const PRODUCTS: [DerivedSweepProduct; 6] = [
    DerivedSweepProduct::SpecificAttenuation,
    DerivedSweepProduct::PathIntegratedAttenuation,
    DerivedSweepProduct::CorrectedReflectivity,
    DerivedSweepProduct::SpecificDifferentialAttenuation,
    DerivedSweepProduct::PathIntegratedDifferentialAttenuation,
    DerivedSweepProduct::CorrectedDifferentialReflectivity,
];

fn case_sweep(case: &serde_json::Value) -> Option<Sweep> {
    let id = as_str(&case["id"]);
    let path = match recast_radar_testdata::path(id) {
        Ok(path) => path,
        Err(error) if error.is_offline() => return None,
        Err(error) => panic!("{error}"),
    };
    Some(level2(&path).sweeps[as_usize(&case["sweep"])].clone())
}

fn zphi_config(smooth_window_gates: usize) -> DerivationConfig {
    let mut config = DerivationConfig::with_products(RadarBand::S, PRODUCTS);
    let Some(mut zphi) = ZPhiAttenuation::for_band(RadarBand::S) else {
        panic!("no S-band Z-PHI coefficients");
    };
    assert_eq!(zphi.freezing_level_above_radar_m, 4000.0);
    assert_eq!(zphi.excluded_end_gates, 15);
    assert_eq!(zphi.smooth_window_gates, 5);
    zphi.smooth_window_gates = smooth_window_gates;
    config.attenuation.method = AttenuationMethod::ZPhi(zphi);
    config
}

fn assert_relative(actual: f64, expected: f64, relative: f64, absolute: f64, what: &str) {
    let tolerance = absolute + relative * expected.abs();
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} != {expected} (tolerance {tolerance})"
    );
}

#[test]
fn zphi_matches_pyart_calculate_attenuation_zphi() {
    let golden = golden("retrieve/attenuation.json");
    for case in array(&golden["cases"]) {
        for (variant, window) in [("unsmoothed", 0), ("smoothed_5", 5)] {
            let Some(sweep) = case_sweep(case) else {
                return;
            };
            let name = format!("{} {variant}", as_str(&case["case"]));
            check_case(sweep, &case[variant], &zphi_config(window), &name, case);
        }
    }
}

fn check_case(
    mut sweep: Sweep,
    expected: &serde_json::Value,
    config: &DerivationConfig,
    name: &str,
    case: &serde_json::Value,
) {
    {
        let report = derive_sweep_in_place(&mut sweep, config);
        assert!(report.unavailable.is_empty(), "{name}: {report:?}");
        let field = |product: DerivedSweepProduct| {
            sweep
                .field(&product.field_name_in(&sweep))
                .unwrap_or_else(|| panic!("{name}: no {product:?}"))
        };
        let ah = field(DerivedSweepProduct::SpecificAttenuation);
        let pia = field(DerivedSweepProduct::PathIntegratedAttenuation);
        let adiff = field(DerivedSweepProduct::SpecificDifferentialAttenuation);
        let pida = field(DerivedSweepProduct::PathIntegratedDifferentialAttenuation);
        let corrected = field(DerivedSweepProduct::CorrectedReflectivity);
        let Some(reflectivity) = sweep.find(recast_radar_core::Quantity::Reflectivity) else {
            panic!("{name}: no reflectivity");
        };
        // Every product is defined wherever PIA is.
        let value_at = |field, row, gate, what: &str| {
            cell(field, row, gate)
                .map(f64::from)
                .unwrap_or_else(|| panic!("{name}: no {what} at ({row}, {gate})"))
        };

        // Outputs live on the reflectivity grid and are defined wherever it is.
        assert_eq!(
            pia.shape().1,
            as_usize(&case["reflectivity_gates"]),
            "{name}"
        );
        let (rows, gates) = pia.shape();
        let (mut defined, mut pia_sum, mut ah_sum, mut pida_sum) = (0usize, 0.0, 0.0, 0.0);
        let mut pia_max = 0.0f64;
        for row in 0..rows {
            for gate in 0..gates {
                let Some(value) = cell(pia, row, gate) else {
                    assert!(
                        cell(reflectivity, row, gate).is_none(),
                        "{name}: ({row}, {gate})"
                    );
                    continue;
                };
                defined += 1;
                pia_sum += f64::from(value);
                pia_max = pia_max.max(f64::from(value));
                ah_sum += value_at(ah, row, gate, "AH");
                pida_sum += value_at(pida, row, gate, "PIDA");
                let z = value_at(reflectivity, row, gate, "Z");
                let zc = value_at(corrected, row, gate, "corrected Z");
                assert!(
                    (zc - (z + f64::from(value))).abs() < 1e-3,
                    "{name}: Z + PIA"
                );
            }
        }
        assert_eq!(
            defined,
            as_usize(&expected["reflectivity_valid"]),
            "{name}: gates"
        );
        assert_relative(
            pia_max,
            as_f64(&expected["pia_max"]),
            1e-3,
            PIA_TOLERANCE_DB,
            name,
        );
        assert_relative(
            pia_sum,
            as_f64(&expected["pia_sum"]),
            1e-3,
            0.0,
            &format!("{name}: PIA sum"),
        );
        assert_relative(
            ah_sum,
            as_f64(&expected["ah_sum"]),
            1e-3,
            0.0,
            &format!("{name}: AH sum"),
        );
        assert_relative(
            pida_sum,
            as_f64(&expected["pida_sum"]),
            1e-3,
            0.0,
            &format!("{name}: PIDA sum"),
        );

        for entry in array(&expected["cells"]) {
            let entry = array(entry);
            let (row, gate) = (as_usize(&entry[0]), as_usize(&entry[1]));
            let at = |field| value_at(field, row, gate, "output");
            let what = format!("{name} ({row}, {gate})");
            assert_relative(
                at(ah),
                as_f64(&entry[2]),
                SPECIFIC_RELATIVE_TOLERANCE,
                1e-7,
                &format!("{what} AH"),
            );
            assert_relative(
                at(pia),
                as_f64(&entry[3]),
                1e-3,
                PIA_TOLERANCE_DB,
                &format!("{what} PIA"),
            );
            assert_relative(
                at(adiff),
                as_f64(&entry[4]),
                SPECIFIC_RELATIVE_TOLERANCE,
                1e-7,
                &format!("{what} ADP"),
            );
            assert_relative(
                at(pida),
                as_f64(&entry[5]),
                1e-3,
                PIA_TOLERANCE_DB,
                &format!("{what} PIDA"),
            );
        }
        // The most attenuated rays, along their whole path.
        for profile in array(&expected["profiles"]) {
            let row = as_usize(&profile["row"]);
            for ((gate, expected_pia), expected_ah) in array(&profile["gates"])
                .iter()
                .zip(array(&profile["pia"]))
                .zip(array(&profile["ah"]))
            {
                let gate = as_usize(gate);
                let what = format!("{name} profile ({row}, {gate})");
                let actual_pia = value_at(pia, row, gate, "PIA");
                assert_relative(
                    actual_pia,
                    as_f64(expected_pia),
                    1e-3,
                    PIA_TOLERANCE_DB,
                    &what,
                );
                let actual_ah = value_at(ah, row, gate, "AH");
                assert_relative(
                    actual_ah,
                    as_f64(expected_ah),
                    SPECIFIC_RELATIVE_TOLERANCE,
                    1e-7,
                    &what,
                );
            }
        }
        eprintln!("{name}: {defined} gates, PIA max {pia_max:.3} dB");
    }
}

/// The default attenuation method is still the PHIDP-linear one.
#[test]
fn phi_linear_stays_the_default_attenuation_method() {
    let config = DerivationConfig::analyst_defaults();
    assert_eq!(config.attenuation.method, AttenuationMethod::PhiLinear);
    assert!(ZPhiAttenuation::for_band(RadarBand::Unknown).is_none());
}

/// Every product in `products`, derived from `sweep` with `config`.
fn derive_fields(
    sweep: &Sweep,
    config: &DerivationConfig,
    products: &[DerivedSweepProduct],
    name: &str,
) -> Vec<Field> {
    let mut sweep = sweep.clone();
    let report = derive_sweep_in_place(&mut sweep, config);
    assert!(report.unavailable.is_empty(), "{name}: {report:?}");
    products
        .iter()
        .map(|product| {
            sweep
                .field(&product.field_name_in(&sweep))
                .cloned()
                .unwrap_or_else(|| panic!("{name}: no {product:?}"))
        })
        .collect()
}

/// Whether two fields hold the same values bit for bit (NaN included).
fn same_bits(a: &Field, b: &Field) -> bool {
    let (rows, gates) = a.shape();
    a.shape() == b.shape()
        && (0..rows).all(|row| {
            (0..gates).all(|gate| {
                a.value(row, gate).map(f32::to_bits) == b.value(row, gate).map(f32::to_bits)
            })
        })
}

/// The KDP method does not move the attenuation correction: with Vulpiani or
/// Maesaka KDP requested in the same pass, the six attenuation products of both
/// the PHIDP-linear and Z-PHI methods are the windowed regression's bit for bit,
/// and Z-PHI still matches Py-ART's `calculate_attenuation_zphi`. The KDP of the
/// same pass is the method's own, so the method did run.
#[test]
fn attenuation_does_not_depend_on_the_kdp_method() {
    let golden = golden("retrieve/attenuation.json");
    let methods = [
        ("Vulpiani", KdpMethod::Vulpiani(VulpianiKdp::default())),
        ("Maesaka", KdpMethod::Maesaka(MaesakaKdp::default())),
    ];
    let mut with_kdp = PRODUCTS.to_vec();
    with_kdp.push(DerivedSweepProduct::Kdp);
    for case in array(&golden["cases"]) {
        let Some(sweep) = case_sweep(case) else {
            return;
        };
        let case_name = as_str(&case["case"]);
        let mut linear = DerivationConfig::with_products(RadarBand::S, with_kdp.iter().copied());
        assert_eq!(linear.attenuation.method, AttenuationMethod::PhiLinear);
        let mut zphi = zphi_config(5);
        zphi.products.insert(DerivedSweepProduct::Kdp);
        for (attenuation, base) in [("PHIDP-linear", &mut linear), ("Z-PHI", &mut zphi)] {
            let reference = derive_fields(&sweep, base, &with_kdp, case_name);
            for (method_name, method) in methods {
                let mut config = base.clone();
                config.kdp.method = method;
                let name = format!("{case_name} {attenuation} {method_name}");
                let fields = derive_fields(&sweep, &config, &with_kdp, &name);
                for ((product, actual), expected) in PRODUCTS.iter().zip(&fields).zip(&reference) {
                    assert!(same_bits(actual, expected), "{name}: {product:?} moved");
                }
                assert!(
                    !same_bits(&fields[PRODUCTS.len()], &reference[PRODUCTS.len()]),
                    "{name}: KDP is the regression's"
                );
                if attenuation == "Z-PHI" {
                    check_case(sweep.clone(), &case["smoothed_5"], &config, &name, case);
                }
            }
        }
    }
}
