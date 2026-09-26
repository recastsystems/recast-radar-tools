//! Product availability on real sweeps.
//!
//! Expected moment lists and radial counts come from
//! `testdata/golden/retrieve/availability.json` (`tools/retrieve_golden.py
//! availability`): MetPy's `Level2File` data-block names per sweep.

// Test code panics on purpose: the workspace's unwrap/expect lints guard library code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{array, as_str, as_usize, find_case, golden, level2};
use recast_radar_core::{Field, FieldData, FieldName, Volume};
use recast_radar_retrieve::{
    DerivedSweepProduct, MIN_DISPLAYABLE_RADIALS, advanced_derived_product_for_name,
    displayable_radial_threshold, sweep_can_materialize_field, sweep_has_advanced_product_sources,
    sweep_has_field_source, volume_has_advanced_product_sources,
};
use recast_radar_testdata::require_file;
use serde_json::Value;

const NATIVE: [FieldName; 7] = [
    FieldName::Dbzh,
    FieldName::Vradh,
    FieldName::Wradh,
    FieldName::Zdr,
    FieldName::Rhohv,
    FieldName::Phidp,
    FieldName::Kdp,
];

fn sweep(case: &Value, index: usize) -> &Value {
    let sweeps = array(&case["sweeps"]);
    sweeps
        .iter()
        .find(|sweep| as_usize(&sweep["index"]) == index)
        .expect("sweep in the golden")
}

/// MetPy's data-block names of a sweep, as the FM301 field names the decoder
/// gives them.
fn metpy_moments(sweep: &Value) -> Vec<FieldName> {
    sweep["moments"]
        .as_object()
        .expect("moments")
        .keys()
        .map(|name| FieldName::from_nexrad_block(name.as_bytes()))
        .collect()
}

/// Rays a field provides.
fn provided_rows(field: &Field) -> usize {
    field.nrays as usize - field.absent_rows.len()
}

fn split_cut() -> Option<(Volume, Value)> {
    let golden = golden("retrieve/availability.json");
    let case = find_case(&golden["files"], &[("id", "l2-ktlx-20240315-000217-trim")]).clone();
    let path = match recast_radar_testdata::path(as_str(&case["id"])) {
        Ok(path) => path,
        Err(error) if error.is_offline() => return None,
        Err(error) => panic!("{error}"),
    };
    let volume = level2(&path);
    for sweep in array(&case["sweeps"]) {
        let cut = &volume.sweeps[as_usize(&sweep["index"])];
        assert_eq!(cut.nrays(), as_usize(&sweep["radials"]));
        for (name, rows) in sweep["moments"].as_object().expect("moments") {
            let field = cut
                .field(&FieldName::from_nexrad_block(name.as_bytes()))
                .unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(provided_rows(field), as_usize(rows), "{name} rows");
        }
        assert_eq!(
            cut.fields.len(),
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
    let cut = &volume.sweeps[0];
    let native = metpy_moments(sweep(&case, 0));
    assert!(native.contains(&FieldName::Dbzh));
    assert!(native.contains(&FieldName::Phidp));
    assert!(native.contains(&FieldName::Rhohv));
    assert!(!native.contains(&FieldName::Vradh));
    let refc = DerivedSweepProduct::CorrectedReflectivity.field_name_in(cut);
    assert_eq!(refc, FieldName::parse("DBZH_CORR"));
    assert!(!sweep_has_field_source(cut, &refc));
    assert!(sweep_can_materialize_field(cut, &refc));
    assert!(sweep_has_advanced_product_sources(
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
            sweep_has_advanced_product_sources(cut, *product),
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
    let cut = &volume.sweeps[0];
    assert!(metpy_moments(sweep(&case, 0)).contains(&FieldName::Phidp));
    assert!(!metpy_moments(sweep(&case, 0)).contains(&FieldName::Kdp));
    assert_eq!(DerivedSweepProduct::Kdp.field_name_in(cut), FieldName::Kdp);
    assert!(advanced_derived_product_for_name(&FieldName::Kdp).is_none());
    assert!(sweep_has_advanced_product_sources(
        cut,
        DerivedSweepProduct::Kdp
    ));
    assert!(!sweep_can_materialize_field(cut, &FieldName::Kdp));
}

#[test]
fn native_moments_route_straight_through_the_presence_gate() {
    let Some((volume, case)) = split_cut() else {
        return;
    };
    for index in [0usize, 1] {
        let cut = &volume.sweeps[index];
        let native = metpy_moments(sweep(&case, index));
        for name in NATIVE {
            assert_eq!(
                sweep_has_field_source(cut, &name),
                native.contains(&name),
                "sweep {index}: {name} presence vs MetPy"
            );
            assert_eq!(
                sweep_can_materialize_field(cut, &name),
                sweep_has_field_source(cut, &name),
                "sweep {index}: {name} must not take the derive-on-demand arm"
            );
        }
    }
    // The Doppler sweep: REF, VEL and SW.
    let doppler = metpy_moments(sweep(&case, 1));
    assert!(doppler.contains(&FieldName::Vradh));
    assert!(!doppler.contains(&FieldName::Phidp));
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
    assert_eq!(volume.sweeps.len(), 1);
    let cut = &volume.sweeps[0];
    let radials = as_usize(&sweep(case, 0)["radials"]);
    assert_eq!(cut.nrays(), radials);
    assert!(radials < MIN_DISPLAYABLE_RADIALS);
    assert_eq!(displayable_radial_threshold(radials), radials / 2);
    assert!(sweep_has_field_source(cut, &FieldName::Dbzh));
    assert!(sweep_has_advanced_product_sources(
        cut,
        DerivedSweepProduct::HailSignature
    ));

    // ... but a field that filled only 10 of the sweep's rays is not: keep the first
    // 10 rows of the real reflectivity field and mark the others absent.
    let mut partial = volume.clone();
    let cut = &mut partial.sweeps[0];
    let index = cut.field_index(&FieldName::Dbzh).expect("DBZH");
    let field = &mut cut.fields[index];
    let (rows, gates) = field.shape();
    let FieldData::U8 { values, coding } = &mut field.data else {
        panic!("legacy reflectivity is 8-bit");
    };
    values[10 * gates..].fill(coding.fill_value.expect("fill code"));
    field.absent_rows = (10..rows as u32).collect();
    cut.seal().expect("sealed edit");
    let cut = &partial.sweeps[0];
    assert_eq!(provided_rows(cut.field(&FieldName::Dbzh).unwrap()), 10);
    assert_eq!(cut.nrays(), radials);
    assert!(!sweep_has_field_source(cut, &FieldName::Dbzh));
    assert!(!sweep_has_advanced_product_sources(
        cut,
        DerivedSweepProduct::HailSignature
    ));
    assert!(!sweep_can_materialize_field(
        cut,
        &DerivedSweepProduct::HailSignature.field_name_in(cut)
    ));
}

#[test]
fn unknown_names_that_match_nothing_are_not_derivable() {
    let Some((volume, _)) = split_cut() else {
        return;
    };
    for cut in &volume.sweeps {
        let bogus = FieldName::parse("NOT_A_PRODUCT");
        assert!(advanced_derived_product_for_name(&bogus).is_none());
        assert!(!sweep_can_materialize_field(cut, &bogus));
        assert!(!sweep_has_field_source(cut, &bogus));
    }
    // Other native names decode too: CFP is carried as CCORH on the
    // surveillance sweep, present but not a derived product.
    let cfp = FieldName::Ccorh;
    assert!(sweep_has_field_source(&volume.sweeps[0], &cfp));
    assert!(advanced_derived_product_for_name(&cfp).is_none());
    assert!(sweep_can_materialize_field(&volume.sweeps[0], &cfp));
    assert!(!sweep_can_materialize_field(&volume.sweeps[1], &cfp));
}
