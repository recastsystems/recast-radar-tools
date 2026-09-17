//! Product availability on real sweeps.
//!
//! Expected moment lists and radial counts come from
//! `testdata/golden/retrieve/availability.json` (`tools/retrieve_golden.py
//! availability`): MetPy's `Level2File` data-block names per sweep.

mod common;

use common::{array, as_str, as_usize, find_case, golden, level2};
use recast_radar_core::{MomentType, RadarVolume};
use recast_radar_retrieve::{
    DerivedSweepProduct, MIN_DISPLAYABLE_RADIALS, advanced_derived_product_for_moment,
    cut_can_materialize_moment, cut_has_advanced_product_sources, cut_has_moment_source,
    displayable_radial_threshold, volume_has_advanced_product_sources,
};
use recast_radar_testdata::require_file;
use serde_json::Value;

const NATIVE: [MomentType; 7] = [
    MomentType::Reflectivity,
    MomentType::Velocity,
    MomentType::SpectrumWidth,
    MomentType::DifferentialReflectivity,
    MomentType::CorrelationCoefficient,
    MomentType::DifferentialPhase,
    MomentType::SpecificDifferentialPhase,
];

fn sweep(case: &Value, index: usize) -> &Value {
    let sweeps = array(&case["sweeps"]);
    sweeps
        .iter()
        .find(|sweep| as_usize(&sweep["index"]) == index)
        .expect("sweep in the golden")
}

fn metpy_moments(sweep: &Value) -> Vec<MomentType> {
    sweep["moments"]
        .as_object()
        .expect("moments")
        .keys()
        .map(|name| MomentType::from_nexrad_name(name))
        .collect()
}

fn split_cut() -> Option<(RadarVolume, Value)> {
    let golden = golden("retrieve/availability.json");
    let case = find_case(&golden["files"], &[("id", "l2-ktlx-20240315-000217-trim")]).clone();
    let path = match recast_radar_testdata::path(as_str(&case["id"])) {
        Ok(path) => path,
        Err(error) if error.is_offline() => return None,
        Err(error) => panic!("{error}"),
    };
    let volume = level2(&path);
    for sweep in array(&case["sweeps"]) {
        let cut = &volume.cuts[as_usize(&sweep["index"])];
        assert_eq!(cut.radials.len(), as_usize(&sweep["radials"]));
        for (name, rows) in sweep["moments"].as_object().expect("moments") {
            let moment = MomentType::from_nexrad_name(name);
            let grid = cut
                .moments
                .get(&moment)
                .unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(grid.radial_count(), as_usize(rows), "{name} rows");
        }
        assert_eq!(
            cut.moments.len(),
            sweep["moments"].as_object().expect("moments").len()
        );
    }
    Some((volume, case))
}

#[test]
fn derive_on_demand_admits_a_dual_pol_sweep_the_presence_gate_rejects() {
    // The surveillance sweep carries REF, ZDR, PHI, RHO and CFP (MetPy), no REFC.
    let Some((volume, case)) = split_cut() else {
        return;
    };
    let cut = &volume.cuts[0];
    let native = metpy_moments(sweep(&case, 0));
    assert!(native.contains(&MomentType::Reflectivity));
    assert!(native.contains(&MomentType::DifferentialPhase));
    assert!(native.contains(&MomentType::CorrelationCoefficient));
    assert!(!native.contains(&MomentType::Velocity));
    let refc = DerivedSweepProduct::CorrectedReflectivity.moment_type();
    assert_eq!(refc, MomentType::Unknown("REFC".to_owned()));
    assert!(!cut_has_moment_source(cut, &refc));
    assert!(cut_can_materialize_moment(cut, &refc));
    assert!(cut_has_advanced_product_sources(
        cut,
        DerivedSweepProduct::CorrectedReflectivity
    ));
    // Every product the sweep's moments can feed is admitted; velocity products are not.
    for product in DerivedSweepProduct::ALL {
        let expected = !matches!(
            product,
            DerivedSweepProduct::VelocityTexture
                | DerivedSweepProduct::VelocityRangeGradient
                | DerivedSweepProduct::SpectrumWidthTexture
                | DerivedSweepProduct::TurbulenceProxy
        );
        assert_eq!(
            cut_has_advanced_product_sources(cut, *product),
            expected,
            "{product:?} on the surveillance sweep"
        );
        assert!(
            volume_has_advanced_product_sources(&volume, *product),
            "{product:?} somewhere in the split cut"
        );
    }
}

#[test]
fn derive_on_demand_never_admits_kdp() {
    // PHI is present on the surveillance sweep, so KDP could be computed, but selecting
    // "KDP" needs a real KDP field: none in a Level II file.
    let Some((volume, case)) = split_cut() else {
        return;
    };
    let cut = &volume.cuts[0];
    assert!(metpy_moments(sweep(&case, 0)).contains(&MomentType::DifferentialPhase));
    assert!(!metpy_moments(sweep(&case, 0)).contains(&MomentType::SpecificDifferentialPhase));
    assert_eq!(
        DerivedSweepProduct::Kdp.moment_type(),
        MomentType::SpecificDifferentialPhase
    );
    assert!(advanced_derived_product_for_moment(&MomentType::SpecificDifferentialPhase).is_none());
    assert!(cut_has_advanced_product_sources(
        cut,
        DerivedSweepProduct::Kdp
    ));
    assert!(!cut_can_materialize_moment(
        cut,
        &MomentType::SpecificDifferentialPhase
    ));
}

#[test]
fn native_moments_route_straight_through_the_presence_gate() {
    let Some((volume, case)) = split_cut() else {
        return;
    };
    for index in [0usize, 1] {
        let cut = &volume.cuts[index];
        let native = metpy_moments(sweep(&case, index));
        for moment in NATIVE {
            assert_eq!(
                cut_has_moment_source(cut, &moment),
                native.contains(&moment),
                "sweep {index}: {moment} presence vs MetPy"
            );
            assert_eq!(
                cut_can_materialize_moment(cut, &moment),
                cut_has_moment_source(cut, &moment),
                "sweep {index}: {moment} must not take the derive-on-demand arm"
            );
        }
    }
    // The Doppler sweep: REF, VEL and SW.
    let doppler = metpy_moments(sweep(&case, 1));
    assert!(doppler.contains(&MomentType::Velocity));
    assert!(!doppler.contains(&MomentType::DifferentialPhase));
}

#[test]
fn a_partial_sweep_carries_no_sources() {
    // A real truncated file: 68 radials of the first cut (MetPy and Py-ART read the
    // same 68). The threshold relaxes to half the cut's radials, so the cut itself is
    // displayable...
    let golden = golden("retrieve/availability.json");
    let case = find_case(&golden["files"], &[("id", "l2-ktlx-19990503-230052")]);
    let path = require_file!(as_str(&case["id"]));
    let volume = level2(&path);
    assert_eq!(volume.cuts.len(), 1);
    let cut = &volume.cuts[0];
    let radials = as_usize(&sweep(case, 0)["radials"]);
    assert_eq!(cut.radials.len(), radials);
    assert!(radials < MIN_DISPLAYABLE_RADIALS);
    assert_eq!(displayable_radial_threshold(radials), radials / 2);
    assert!(cut_has_moment_source(cut, &MomentType::Reflectivity));
    assert!(cut_has_advanced_product_sources(
        cut,
        DerivedSweepProduct::HailSignature
    ));

    // ... but a grid that filled only 10 of the declared radials is not: keep the first
    // 10 rows of the real reflectivity grid.
    let mut partial = volume.clone();
    let cut = &mut partial.cuts[0];
    let grid = cut.moments.get_mut(&MomentType::Reflectivity).expect("REF");
    let gates = grid.gate_range.gate_count;
    grid.radial_indices.truncate(10);
    if let recast_radar_core::MomentStorage::U8(values) = &mut grid.storage {
        values.truncate(10 * gates);
    } else {
        panic!("legacy reflectivity is 8-bit");
    }
    assert_eq!(grid.radial_count(), 10);
    let cut = &partial.cuts[0];
    assert_eq!(cut.radials.len(), radials);
    assert!(!cut_has_moment_source(cut, &MomentType::Reflectivity));
    assert!(!cut_has_advanced_product_sources(
        cut,
        DerivedSweepProduct::HailSignature
    ));
    assert!(!cut_can_materialize_moment(
        cut,
        &DerivedSweepProduct::HailSignature.moment_type()
    ));
}

#[test]
fn unknown_names_that_match_nothing_are_not_derivable() {
    let Some((volume, _)) = split_cut() else {
        return;
    };
    for cut in &volume.cuts {
        let bogus = MomentType::Unknown("NOT_A_PRODUCT".to_owned());
        assert!(advanced_derived_product_for_moment(&bogus).is_none());
        assert!(!cut_can_materialize_moment(cut, &bogus));
        assert!(!cut_has_moment_source(cut, &bogus));
    }
    // Real unknown names decode too: CFP is carried as an unknown moment on the
    // surveillance sweep, present but not a derived product.
    let cfp = MomentType::Unknown("CFP".to_owned());
    assert!(cut_has_moment_source(&volume.cuts[0], &cfp));
    assert!(advanced_derived_product_for_moment(&cfp).is_none());
    assert!(cut_can_materialize_moment(&volume.cuts[0], &cfp));
    assert!(!cut_can_materialize_moment(&volume.cuts[1], &cfp));
}
