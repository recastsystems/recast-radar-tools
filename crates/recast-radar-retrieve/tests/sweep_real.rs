//! Sweep-local product derivation (KDP, filtered phase, range gradient, CDR) on
//! real dual-pol sweeps.
//!
//! Expected values come from `testdata/golden/retrieve/sweep.json`
//! (`tools/retrieve_golden.py sweep`): a numpy implementation of the documented
//! phase bundle and range gradient on MetPy-read moments, Py-ART's `compute_cdr`
//! and `kdp_vulpiani`, and a DORADE block walker for the native-KDP file.

mod common;

use common::{
    array, as_f64, as_i64, as_str, as_usize, assert_close, assert_grid_matches, assert_same_grid,
    cell, dorade, golden, level2, moment,
};
use recast_radar_core::{MomentStorage, MomentType};
use recast_radar_retrieve::{
    DerivationConfig, DerivedSweepProduct, RadarBand, derive_cut_in_place, derive_product,
};
use recast_radar_testdata::require_file;

/// KDP (deg/km) and filtered phase (deg) tolerance: the reference repeats the f32 Hampel
/// arithmetic, so only the f64 fit summation order separates the two.
const PHASE_TOLERANCE: f64 = 0.01;

fn moore_surveillance_cut() -> Option<recast_radar_core::ElevationCut> {
    let path = match recast_radar_testdata::path("l2-ktlx-20130520-201643-trim") {
        Ok(path) => path,
        Err(error) if error.is_offline() => return None,
        Err(error) => panic!("{error}"),
    };
    let volume = level2(&path);
    Some(volume.cuts[0].clone())
}

fn phase_config(products: impl IntoIterator<Item = DerivedSweepProduct>) -> DerivationConfig {
    DerivationConfig::with_products(RadarBand::S, products)
}

#[test]
fn moore_core_kdp_and_filtered_phase_match_the_reference() {
    let Some(mut cut) = moore_surveillance_cut() else {
        return;
    };
    let golden = golden("retrieve/sweep.json");
    let case = &golden["kdp"];
    assert_close(
        f64::from(cut.elevation_deg),
        as_f64(&case["elevation_deg"]),
        1e-3,
        "elevation",
    );
    let phi = moment(&cut, &MomentType::DifferentialPhase);
    assert_eq!(
        phi.gate_range.gate_spacing_m,
        as_i64(&case["gate_spacing_m"]) as i32
    );
    assert_close(
        f64::from(phi.scale),
        as_f64(&case["phi_scale"]),
        1e-6,
        "PHI scale",
    );
    assert_close(
        f64::from(phi.offset),
        as_f64(&case["phi_offset"]),
        1e-6,
        "PHI offset",
    );

    let report = derive_cut_in_place(
        &mut cut,
        &phase_config([
            DerivedSweepProduct::Kdp,
            DerivedSweepProduct::FilteredDifferentialPhase,
        ]),
    );
    assert_eq!(report.inserted, vec!["KDP", "PHIF"]);
    assert!(report.unavailable.is_empty());
    let kdp = moment(&cut, &MomentType::SpecificDifferentialPhase);
    let phif = moment(
        &cut,
        &DerivedSweepProduct::FilteredDifferentialPhase.moment_type(),
    );
    assert_grid_matches(kdp, &case["kdp"], PHASE_TOLERANCE, "KDP");
    assert_grid_matches(phif, &case["phif"], PHASE_TOLERANCE, "PHIF");

    let peak = &case["kdp_max"];
    let value = cell(kdp, as_usize(&peak["row"]), as_usize(&peak["gate"])).expect("peak KDP");
    assert_close(
        f64::from(value),
        as_f64(&peak["value"]),
        PHASE_TOLERANCE,
        "peak KDP",
    );

    // Physical check on the hail core (reflectivity >= 50 dBZ within 40 km on the rays
    // through the Moore supercell): KDP of order 1 deg/km and more, as expected for
    // heavy precipitation at S band, and of the same magnitude class as Py-ART's
    // (much smoother) Vulpiani estimate on the same sweep.
    let core = &case["moore_core"];
    let cells = array(&core["core_cells"]);
    let values: Vec<f64> = cells
        .iter()
        .map(|entry| {
            let entry = array(entry);
            f64::from(
                cell(kdp, as_usize(&entry[0]), as_usize(&entry[1]))
                    .expect("core cell has KDP in the reference"),
            )
        })
        .collect();
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let vulpiani = as_f64(&core["vulpiani_mean_kdp"]);
    assert!(values.len() >= 100, "only {} core cells", values.len());
    assert!(mean > 1.0, "core mean KDP {mean} deg/km");
    assert!(
        mean > vulpiani / 3.0 && mean < vulpiani * 3.0,
        "core mean KDP {mean} vs Py-ART Vulpiani {vulpiani}"
    );
}

#[test]
fn short_phidp_gaps_feed_the_fit_but_get_no_estimate() {
    let Some(mut cut) = moore_surveillance_cut() else {
        return;
    };
    let golden = golden("retrieve/sweep.json");
    let gaps = array(&golden["kdp"]["gaps"]);
    assert!(gaps.len() >= 40, "{} gaps in the golden", gaps.len());
    derive_cut_in_place(
        &mut cut,
        &phase_config([
            DerivedSweepProduct::Kdp,
            DerivedSweepProduct::FilteredDifferentialPhase,
        ]),
    );
    let kdp = moment(&cut, &MomentType::SpecificDifferentialPhase);
    let phif = moment(
        &cut,
        &DerivedSweepProduct::FilteredDifferentialPhase.moment_type(),
    );
    let mut fill_mattered = 0usize;
    for gap in gaps {
        let row = as_usize(&gap["row"]);
        let range = array(&gap["gates"]);
        let (first, last) = (as_usize(&range[0]), as_usize(&range[1]));
        for gate in first..=last {
            assert!(
                cell(kdp, row, gate).is_none(),
                "KDP emitted at gap ({row}, {gate})"
            );
            assert!(
                cell(phif, row, gate).is_none(),
                "PHIF emitted at gap ({row}, {gate})"
            );
        }
        for (gate, key, key_without) in [
            (first - 1, "left_kdp", "left_kdp_without_fill"),
            (last + 1, "right_kdp", "right_kdp_without_fill"),
        ] {
            let value = f64::from(cell(kdp, row, gate).expect("neighbour KDP"));
            assert_close(
                value,
                as_f64(&gap[key]),
                PHASE_TOLERANCE,
                &format!("({row}, {gate})"),
            );
            if (as_f64(&gap[key]) - as_f64(&gap[key_without])).abs() > 0.05 {
                fill_mattered += 1;
            }
        }
    }
    // The interpolated gap gates take part in the neighbours' fits: without the fill
    // the reference values differ at most gaps.
    assert!(
        fill_mattered >= gaps.len(),
        "gap filling changed {fill_mattered} neighbour fits"
    );
}

#[test]
fn native_kdp_is_preserved() {
    // A real sweep carrying a native KDP field: the NOXP sector, whose KDP holds only the
    // bad-data flag (the walker counts 0 finite of 100 x 1001 words) while its PHIDP is
    // an unknown-named field the phase bundle cannot use.
    let golden = golden("retrieve/sweep.json");
    let case = &golden["native_kdp"];
    let path = require_file!(as_str(&case["id"]));
    let mut volume = dorade(&path);
    let cut = &mut volume.cuts[0];
    assert_eq!(cut.radials.len(), as_usize(&case["rays"]));
    let native = moment(cut, &MomentType::SpecificDifferentialPhase).clone();
    assert!(native.gate_range.gate_count >= as_usize(&case["cells"]));
    let finite = (0..native.radial_count())
        .flat_map(|row| (0..native.gate_range.gate_count).map(move |gate| (row, gate)))
        .filter(|&(row, gate)| cell(&native, row, gate).is_some())
        .count();
    assert_eq!(finite, as_usize(&case["kdp_finite"]));
    let report = derive_cut_in_place(cut, &DerivationConfig::kdp_only());
    assert_eq!(report.skipped_existing, vec!["KDP"]);
    assert!(report.inserted.is_empty());
    assert_same_grid(
        moment(cut, &MomentType::SpecificDifferentialPhase),
        &native,
        "native KDP",
    );

    // A derived KDP is native to the next pass: preserved unless overwriting is asked for,
    // and `derive_product` recomputes the same values without touching the cut.
    let Some(mut cut) = moore_surveillance_cut() else {
        return;
    };
    let first = derive_cut_in_place(&mut cut, &DerivationConfig::kdp_only());
    assert_eq!(first.inserted, vec!["KDP"]);
    let derived = moment(&cut, &MomentType::SpecificDifferentialPhase).clone();
    let second = derive_cut_in_place(&mut cut, &DerivationConfig::kdp_only());
    assert_eq!(second.skipped_existing, vec!["KDP"]);
    assert_same_grid(
        moment(&cut, &MomentType::SpecificDifferentialPhase),
        &derived,
        "second pass",
    );
    let recomputed = derive_product(
        &cut,
        DerivedSweepProduct::Kdp,
        &DerivationConfig::kdp_only(),
    )
    .expect("KDP derivable");
    assert_same_grid(&recomputed, &derived, "derive_product");
    let mut overwrite = DerivationConfig::kdp_only();
    overwrite.overwrite_existing = true;
    let third = derive_cut_in_place(&mut cut, &overwrite);
    assert_eq!(third.inserted, vec!["KDP"]);
    assert_same_grid(
        moment(&cut, &MomentType::SpecificDifferentialPhase),
        &derived,
        "overwrite",
    );
}

#[test]
fn filtered_phase_survives_where_kdp_is_out_of_bounds() {
    let Some(mut cut) = moore_surveillance_cut() else {
        return;
    };
    let golden = golden("retrieve/sweep.json");
    let case = &golden["kdp"];
    let cells = array(&case["out_of_bounds_cells"]);
    assert!(as_usize(&case["out_of_bounds_count"]) > 1_000);
    assert_eq!(cells.len(), 100);
    derive_cut_in_place(
        &mut cut,
        &phase_config([
            DerivedSweepProduct::Kdp,
            DerivedSweepProduct::FilteredDifferentialPhase,
        ]),
    );
    let kdp = moment(&cut, &MomentType::SpecificDifferentialPhase);
    let phif = moment(
        &cut,
        &DerivedSweepProduct::FilteredDifferentialPhase.moment_type(),
    );
    for entry in cells {
        let entry = array(entry);
        let (row, gate) = (as_usize(&entry[0]), as_usize(&entry[1]));
        assert!(
            cell(kdp, row, gate).is_none(),
            "KDP kept at ({row}, {gate})"
        );
        assert!(
            cell(phif, row, gate).is_some(),
            "PHIF lost at ({row}, {gate})"
        );
    }
}

#[test]
fn velocity_range_gradient_uses_the_nyquist_wrapped_difference() {
    let path = require_file!("l2-kdvn-20200810-180401-trim");
    let volume = level2(&path);
    let golden = golden("retrieve/sweep.json");
    let case = &golden["velocity_range_gradient"];
    let mut cut = volume.cuts[as_usize(&case["sweep"])].clone();
    assert_close(
        f64::from(cut.radials[0].nyquist_velocity_mps.expect("Nyquist")),
        as_f64(&case["nyquist_mps"]),
        0.01,
        "Nyquist",
    );
    let report = derive_cut_in_place(
        &mut cut,
        &phase_config([DerivedSweepProduct::VelocityRangeGradient]),
    );
    assert_eq!(report.inserted, vec!["VEL_GRAD_R"]);
    let gradient = moment(
        &cut,
        &DerivedSweepProduct::VelocityRangeGradient.moment_type(),
    );
    assert_grid_matches(gradient, &case["grid"], 0.01, "VEL_GRAD_R");
    // At the fold boundaries (4,686 gates of this Nyquist 21 m/s sweep) the wrapped
    // difference is a few m/s per km where the plain difference would be ~80.
    let changed = array(&case["wrap_changed_cells"]);
    assert!(as_usize(&case["wrap_changed_gates"]) > 1_000);
    for entry in changed {
        let entry = array(entry);
        let (row, gate) = (as_usize(&entry[0]), as_usize(&entry[1]));
        let value = f64::from(cell(gradient, row, gate).expect("gradient at a fold"));
        assert_close(
            value,
            as_f64(&entry[2]),
            0.01,
            &format!("wrapped ({row}, {gate})"),
        );
        assert!(
            (value - as_f64(&entry[3])).abs() > 1.0,
            "({row}, {gate}): {value} equals the unwrapped difference"
        );
    }
}

#[test]
fn rho_gating_samples_by_physical_range() {
    // The correlation-coefficient gate of the phase bundle is sampled by range, not by
    // gate index: the same sweep with its RHO grid thinned to 500 m gates (every other
    // real gate kept) must gate PHIDP exactly as the reference does with that geometry.
    let Some(mut cut) = moore_surveillance_cut() else {
        return;
    };
    let golden = golden("retrieve/sweep.json");
    let case = &golden["kdp"]["rho_500m"];
    let mut full = cut.clone();
    derive_cut_in_place(&mut full, &DerivationConfig::kdp_only());
    let kdp_full = moment(&full, &MomentType::SpecificDifferentialPhase).clone();

    let rho = cut
        .moments
        .get_mut(&MomentType::CorrelationCoefficient)
        .expect("RHO");
    let gates = rho.gate_range.gate_count;
    let kept = gates.div_ceil(2);
    let MomentStorage::U8(values) = &rho.storage else {
        panic!("RHO is 8-bit in this file");
    };
    let thinned: Vec<u8> = values
        .chunks(gates)
        .flat_map(|row| row.iter().step_by(2).copied())
        .collect();
    rho.storage = MomentStorage::U8(thinned);
    rho.gate_range.gate_count = kept;
    rho.gate_range.gate_spacing_m *= 2;
    derive_cut_in_place(&mut cut, &DerivationConfig::kdp_only());
    let kdp = moment(&cut, &MomentType::SpecificDifferentialPhase);
    assert_grid_matches(kdp, &case["kdp"], PHASE_TOLERANCE, "KDP with 500 m RHO");
    assert!(as_usize(&case["qc_changed_gates"]) > 1_000);
    for entry in array(&case["changed_cells"]) {
        let entry = array(entry);
        let (row, gate) = (as_usize(&entry[0]), as_usize(&entry[1]));
        assert_ne!(
            cell(kdp, row, gate).is_some(),
            cell(&kdp_full, row, gate).is_some(),
            "({row}, {gate}): the thinned RHO did not change the gate"
        );
    }
}

#[test]
fn cdr_matches_pyart_compute_cdr() {
    let Some(cut) = moore_surveillance_cut() else {
        return;
    };
    let golden = golden("retrieve/sweep.json");
    let case = &golden["cdr"];
    let cdr = derive_product(
        &cut,
        DerivedSweepProduct::CircularDepolarizationRatio,
        &phase_config([DerivedSweepProduct::CircularDepolarizationRatio]),
    )
    .expect("CDR derivable from ZDR and RHO");
    assert!(as_f64(&case["pyart_vs_f32_max_abs_delta"]) < 0.02);
    assert_grid_matches(&cdr, &case["grid"], 0.02, "CDR");
}

#[test]
fn unknown_band_blocks_band_sensitive_products_but_keeps_phif() {
    let Some(mut cut) = moore_surveillance_cut() else {
        return;
    };
    let golden = golden("retrieve/sweep.json");
    let config = DerivationConfig::with_products(
        RadarBand::Unknown,
        [
            DerivedSweepProduct::Kdp,
            DerivedSweepProduct::FilteredDifferentialPhase,
            DerivedSweepProduct::RainRateKdp,
        ],
    );
    let report = derive_cut_in_place(&mut cut, &config);
    assert_eq!(report.inserted, vec!["PHIF"]);
    assert_eq!(report.unavailable, vec!["KDP", "RATE_KDP"]);
    assert!(
        !cut.moments
            .contains_key(&MomentType::SpecificDifferentialPhase)
    );
    let phif = moment(
        &cut,
        &DerivedSweepProduct::FilteredDifferentialPhase.moment_type(),
    );
    assert_grid_matches(
        phif,
        &golden["kdp"]["unknown_band"]["phif"],
        PHASE_TOLERANCE,
        "PHIF (unknown band)",
    );
}
