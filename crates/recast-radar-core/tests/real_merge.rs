//! `merge_volumes` on real per-product, per-sweep and per-chunk parts.
//!
//! Expected outcomes: `testdata/golden/core/model.json` (`merges`), written by
//! `tools/core_golden.py`: a reference implementation of the documented merge
//! rules over sweep metadata from h5py (ODIM parts), MetPy (Level II) and a
//! GRIB2 section walker (JMA), never from this workspace's readers.

// A panic is how a test fails (clippy.toml), in helpers too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;

use common::{
    array, as_f64, as_str, as_usize, assert_close, cfradial, data_identical, field, field_names,
    fields_identical, golden, jma, kiwa_chunk_bytes, level2, level2_bytes, odim, odim_physical,
    part_with_only, time,
};
use recast_radar_core::model::ANGLE_MATCH_TOLERANCE_DEG;
use recast_radar_core::{
    FieldData, FieldName, GateMapping, MergeError, MergeReport, Quantity, RangeCoord, Sweep,
    Volume, merge_volumes,
};
use serde_json::Value;

fn report(value: &Value) -> MergeReport {
    MergeReport {
        merged_fields: as_usize(&value["merged_fields"]),
        skipped_geometry: as_usize(&value["skipped_geometry"]),
        field_collisions: as_usize(&value["field_collisions"]),
    }
}

/// `volume` with the NaN of its per-ray instrument variables replaced, so
/// `PartialEq` treats two missing values as equal.
fn without_nan(volume: &Volume) -> Volume {
    let mut volume = volume.clone();
    for sweep in &mut volume.sweeps {
        let vars = &mut sweep.ray_vars;
        for values in [
            &mut vars.nyquist_velocity_mps,
            &mut vars.unambiguous_range_m,
            &mut vars.prt_s,
            &mut vars.prt_ratio,
            &mut vars.pulse_width_s,
            &mut vars.scan_rate_deg_per_s,
            &mut vars.rx_range_resolution_m,
            &mut vars.independent_samples,
        ]
        .into_iter()
        .flatten()
        {
            for value in values.iter_mut().filter(|value| value.is_nan()) {
                *value = f32::MAX;
            }
        }
    }
    volume
}

/// The merged volume against the reference merge: site, earliest time, the
/// report counters, and every sweep's fixed angle, numbers, ray count and
/// field names.
fn assert_merge_matches(merged: &Volume, actual: MergeReport, expected: &Value, what: &str) {
    assert_eq!(
        merged.attrs.instrument_name,
        as_str(&expected["site"]),
        "{what}: site"
    );
    assert_eq!(
        merged.time_reference,
        time(as_str(&expected["time"])),
        "{what}: time reference"
    );
    assert_eq!(actual, report(&expected["report"]), "{what}: report");
    let sweeps = array(&expected["sweeps"]);
    assert_eq!(merged.sweeps.len(), sweeps.len(), "{what}: sweep count");
    for (index, (sweep, want)) in merged.sweeps.iter().zip(sweeps).enumerate() {
        assert_close(
            f64::from(sweep.fixed_angle_deg),
            as_f64(&want["fixed_angle_deg"]),
            1e-6,
            &format!("{what}: sweep {index} fixed angle"),
        );
        assert_eq!(sweep.sweep_number, index as u32, "{what}: sweep {index}");
        assert_eq!(
            sweep.elevation_number,
            Some(as_usize(&want["elevation_number"]) as u16),
            "{what}: sweep {index} elevation number"
        );
        assert_eq!(
            sweep.nrays(),
            as_usize(&want["rays"]),
            "{what}: sweep {index} rays"
        );
        let names: BTreeSet<String> = want["fields"]
            .as_object()
            .expect("fields")
            .keys()
            .cloned()
            .collect();
        assert_eq!(field_names(sweep), names, "{what}: sweep {index} fields");
        for field in &sweep.fields {
            assert_eq!(
                field.nrays as usize,
                sweep.nrays(),
                "{what}: sweep {index} {} rows",
                field.name
            );
        }
        let mut sealed = sweep.clone();
        assert_eq!(sealed.seal(), Ok(()), "{what}: sweep {index} invariants");
    }
}

/// A KIWA real-time part: the start chunk plus one intermediate chunk.
fn kiwa_part(number: &str) -> Option<Volume> {
    let start = recast_radar_testdata::path("l2chunk-kiwa-307-20260917-003629-001-s").ok()?;
    let chunk =
        recast_radar_testdata::path(&format!("l2chunk-kiwa-307-20260917-003629-{number}-i"))
            .ok()?;
    let start = std::fs::read(start).ok()?;
    Some(level2_bytes(&kiwa_chunk_bytes(&start, &chunk)))
}

/// Absolute time of ray `ray` of `sweep` in `volume`, in seconds since the
/// epoch.
fn ray_epoch_s(volume: &Volume, sweep: &Sweep, ray: usize) -> f64 {
    volume.time_reference.timestamp() as f64 + sweep.rays.time_s[ray]
}

#[test]
fn merge_rejects_empty_input() {
    let err = merge_volumes(Vec::new()).unwrap_err();
    assert_eq!(err, MergeError::NoParts);
    assert!(
        err.to_string().contains("no radar volumes"),
        "unexpected error: {err}"
    );
}

/// One part (KTLX 2024-03-15 split cut: the surveillance sweep, first ray at
/// 0.58 deg, before the Doppler sweep, first ray at 0.48 deg; both are VCP cut
/// 0.48 deg) merges to itself: equal fixed angles keep their acquisition
/// order.
#[test]
fn merge_single_part_is_identity_with_stable_order() {
    let expected = golden();
    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
    let part = level2(&path);
    assert!(part.sweeps[0].rays.elevation_deg[0] > part.sweeps[1].rays.elevation_deg[0]);
    assert_eq!(
        part.sweeps[0].fixed_angle_deg,
        part.sweeps[1].fixed_angle_deg
    );
    let (merged, report) = merge_volumes(vec![part.clone()]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["single_part_ktlx_2024"],
        "KTLX 2024",
    );
    assert_eq!(merged.attrs, part.attrs);
    assert_eq!(merged.scan, part.scan);
    assert_eq!(merged.location, part.location);
    assert!(
        without_nan(&merged) == without_nan(&part),
        "a single part merges to itself"
    );
}

/// A KIWA chunk part and a KTLX volume: different sites, named in the error.
#[test]
fn merge_rejects_mismatched_site_ids() {
    let expected = golden();
    let Some(kiwa) = kiwa_part("002") else {
        eprintln!("KIWA chunks unavailable; skipping");
        return;
    };
    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
    let ktlx = level2(&path);
    let err = merge_volumes(vec![kiwa.clone(), ktlx.clone()]).unwrap_err();
    assert_eq!(
        err,
        MergeError::SiteMismatch {
            first: kiwa.attrs.instrument_name.clone(),
            other: ktlx.attrs.instrument_name.clone(),
        }
    );
    let parts = array(&expected["merges"]["kiwa_002_vs_ktlx_2024"]["parts"]);
    assert_eq!(kiwa.attrs.instrument_name, as_str(&parts[0]["site"]));
    assert_eq!(ktlx.attrs.instrument_name, as_str(&parts[1]["site"]));
    assert!(expected["merges"]["kiwa_002_vs_ktlx_2024"]["error"].is_string());
}

/// Two real volumes of different source formats, made to agree on the site
/// name: the merge is refused. `Sweep::tilt_elevation_deg` reads a Level II
/// sweep's first-ray elevation and every other format's fixed angle, and a
/// merged volume carries one `provenance.source_format`, so merging across
/// formats would evaluate every sweep under the first part's format (a
/// Level II part's rule applied to ODIM sweeps, or the reverse).
#[test]
fn merge_rejects_mismatched_source_formats() {
    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
    let ktlx = level2(&path);
    let mut nohur = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1445-dbzh"
    ));
    assert_ne!(
        ktlx.provenance.source_format,
        nohur.provenance.source_format
    );
    // Same site name, so only the format check can reject the pair.
    nohur.attrs.instrument_name = ktlx.attrs.instrument_name.clone();
    let err = merge_volumes(vec![ktlx.clone(), nohur.clone()]).unwrap_err();
    assert_eq!(
        err,
        MergeError::SourceMismatch {
            first: ktlx.provenance.source_format,
            other: nohur.provenance.source_format,
        }
    );
    // Either order, and the message names both formats.
    let reversed = merge_volumes(vec![nohur.clone(), ktlx.clone()]).unwrap_err();
    assert_eq!(
        reversed,
        MergeError::SourceMismatch {
            first: nohur.provenance.source_format,
            other: ktlx.provenance.source_format,
        }
    );
    let message = err.to_string();
    assert!(
        message.contains("NexradLevel2") && message.contains("OdimH5"),
        "{message}"
    );
    // Parts of one format still merge.
    assert!(merge_volumes(vec![nohur.clone(), nohur]).is_ok());
}

/// Hurum's 14:45 scan as three ORD files: the VRADH part is stamped 14:46:48
/// (its last sweep is a later vertical scan), the DBZH and TH parts 14:45:13.
/// Whatever the part order, the merged volume takes the earliest time, and ray
/// times are rebased onto it.
#[test]
fn merge_keeps_earliest_time_reference() {
    let expected = golden();
    let dbzh = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1445-dbzh"
    ));
    let th = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1445-th"
    ));
    let vradh = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1446-vradh"
    ));
    let parts = array(&expected["merges"]["nohur_vradh_th_dbzh"]["parts"]);
    assert_eq!(vradh.time_reference, time(as_str(&parts[0]["time"])));
    assert_eq!(th.time_reference, time(as_str(&parts[1]["time"])));
    assert_eq!(dbzh.time_reference, time(as_str(&parts[2]["time"])));
    assert!(vradh.time_reference > dbzh.time_reference);

    let (merged, report) = merge_volumes(vec![vradh.clone(), th.clone(), dbzh.clone()]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["nohur_vradh_th_dbzh"],
        "VRADH first",
    );
    assert_eq!(merged.time_reference, dbzh.time_reference);
    // The VRADH part was the base: its rays keep their absolute times.
    let base = &vradh.sweeps[0];
    let rebased = merged
        .sweeps
        .iter()
        .find(|sweep| sweep.field(&FieldName::Vradh).is_some())
        .expect("a VRADH sweep");
    assert_eq!(rebased.fixed_angle_deg, base.fixed_angle_deg);
    for ray in [0, base.nrays() - 1] {
        assert_eq!(
            ray_epoch_s(&merged, rebased, ray),
            ray_epoch_s(&vradh, base, ray),
            "ray {ray} time"
        );
    }

    let (merged, report) = merge_volumes(vec![dbzh.clone(), vradh, th]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["nohur_dbzh_vradh_th"],
        "DBZH first",
    );
    assert_eq!(merged.time_reference, dbzh.time_reference);
}

/// Jabbeke's Doppler task as one PVOL per quantity (SHMU/RMI style): the
/// VRAD part's 9 sweeps have the same elevations and 360-ray geometry as the
/// DBZH part's, so every sweep ends up with DBZH and VRAD.
#[test]
fn merge_unions_fields_of_angle_matched_sweeps() {
    let expected = golden();
    let dbzh = odim(&recast_radar_testdata::require_file!(
        "odim-bejab-20260612-1450-dbzh"
    ));
    let vrad = odim(&recast_radar_testdata::require_file!(
        "odim-bejab-20260612-1450-vrad"
    ));
    for part in [&dbzh, &vrad] {
        assert_eq!(part.sweeps.len(), 9);
        assert!(part.sweeps.iter().all(|sweep| sweep.fields.len() == 1));
    }
    let (merged, report) = merge_volumes(vec![dbzh.clone(), vrad.clone()]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["bejab_dbzh_vrad"],
        "bejab",
    );
    let vrad_name = FieldName::parse("VRAD");
    for (index, sweep) in merged.sweeps.iter().enumerate() {
        let base = &dbzh.sweeps[index];
        assert_eq!(
            sweep.fixed_angle_deg, base.fixed_angle_deg,
            "first part's sweep is the base"
        );
        assert_eq!(sweep.range, base.range);
        assert!(fields_identical(
            field(sweep, &FieldName::Dbzh),
            field(base, &FieldName::Dbzh)
        ));
        // The VRAD field keeps its values and its native geometry on the
        // DBZH sweep's range.
        let source = &vrad.sweeps[index];
        let moved = field(sweep, &vrad_name);
        let original = field(source, &vrad_name);
        assert!(data_identical(&moved.data, &original.data));
        assert_eq!(moved.shape(), original.shape());
        assert_eq!(
            moved.native_geometry(&sweep.range),
            original.native_geometry(&source.range),
            "sweep {index} VRAD geometry"
        );
    }
    assert_eq!(
        report,
        MergeReport {
            merged_fields: 9,
            skipped_geometry: 0,
            field_collisions: 0,
        }
    );
}

/// Irene's two sweeps split into a reflectivity part and a velocity part
/// (both real): the per-ray `prt` and `unambiguous_range` values from netCDF4
/// survive the merge; a value the first part lacks (NaN) is filled from the
/// second, and a value the first part has is not overwritten by a
/// second-part edit.
#[test]
fn merge_fills_missing_ray_variables_without_overwriting_source_values() {
    let expected = golden();
    let irene = &expected["irene"];
    let path =
        recast_radar_testdata::require_file!("cfrad1-irene-sr2-20110827-120420-sur-sweeps01");
    let volume = cfradial(&path);
    let mut dbz = part_with_only(&volume, &[Quantity::Reflectivity]);
    let mut vel = part_with_only(&volume, &[Quantity::RadialVelocity]);
    let velocity_fields: usize = vel.sweeps.iter().map(|sweep| sweep.fields.len()).sum();
    assert_eq!(velocity_fields, 2, "one velocity field per sweep");
    let prt_s = as_f64(&irene["sweeps"][0]["prt_s_unique"][0]) as f32;
    let range_km = as_f64(&irene["sweeps"][0]["unambiguous_range_km_unique"][0]);
    let range_m = volume.sweeps[0]
        .ray_vars
        .unambiguous_range_m
        .as_ref()
        .unwrap()[0];
    assert_close(f64::from(range_m) / 1000.0, range_km, 1e-4, "range");
    assert_eq!(dbz.sweeps[0].ray_vars.prt_s.as_ref().unwrap()[0], prt_s);

    // Edits of the real variables: the reflectivity part loses its
    // unambiguous range on every ray of sweep 1 (to be filled from the
    // velocity part); the velocity part's prt on ray 0 of sweep 1 is doubled
    // (must not overwrite the reflectivity part's value).
    for value in dbz.sweeps[0].ray_vars.unambiguous_range_m.as_mut().unwrap() {
        *value = f32::NAN;
    }
    vel.sweeps[0].ray_vars.prt_s.as_mut().unwrap()[0] = prt_s * 2.0;

    let (merged, report) = merge_volumes(vec![dbz, vel]).unwrap();
    assert_eq!(report.merged_fields, 2);
    assert_eq!(report.skipped_geometry, 0);
    let vars = &merged.sweeps[0].ray_vars;
    let rays = as_usize(&irene["sweeps"][0]["rays"]);
    let merged_prt = vars.prt_s.as_ref().unwrap();
    let merged_range = vars.unambiguous_range_m.as_ref().unwrap();
    assert_eq!(merged_prt.len(), rays);
    assert_eq!(merged_prt[0], prt_s, "first source wins");
    for ray in 0..rays {
        assert_eq!(merged_prt[ray], prt_s, "ray {ray}");
        assert_eq!(
            merged_range[ray], range_m,
            "ray {ray}: filled from the velocity part"
        );
    }
    assert_eq!(vars.independent_samples, None);
    // Sweep 2 was not edited: both parts agree and the values are the file's.
    let vars = &merged.sweeps[1].ray_vars;
    assert!(vars.prt_s.as_ref().unwrap().iter().all(|&v| v == prt_s));
    assert!(
        vars.unambiguous_range_m
            .as_ref()
            .unwrap()
            .iter()
            .all(|&v| v == range_m)
    );
}

/// A velocity part whose `prt` is one entry short (edit of the real
/// variable) does not supply values: the merged sweep keeps the reflectivity
/// part's. The other way round, a mis-sized first-part variable is replaced
/// by the incoming one, so the merged sweep seals.
#[test]
fn merge_ignores_mis_sized_incoming_ray_variables() {
    let path =
        recast_radar_testdata::require_file!("cfrad1-irene-sr2-20110827-120420-sur-sweeps01");
    let volume = cfradial(&path);
    let dbz = part_with_only(&volume, &[Quantity::Reflectivity]);
    let mut malformed = part_with_only(&volume, &[Quantity::RadialVelocity]);
    malformed.sweeps[0].ray_vars.prt_s.as_mut().unwrap().pop();
    assert!(malformed.sweeps[0].clone().seal().is_err());

    let (merged, report) = merge_volumes(vec![dbz.clone(), malformed.clone()]).unwrap();
    assert_eq!(report.merged_fields, 2);
    assert_eq!(merged.sweeps[0].ray_vars, dbz.sweeps[0].ray_vars);
    assert_eq!(merged.sweeps[1].ray_vars, dbz.sweeps[1].ray_vars);

    // The mis-sized part first: the well-sized incoming vector replaces its
    // `prt` whole.
    let (merged, _) = merge_volumes(vec![malformed, dbz.clone()]).unwrap();
    assert_eq!(
        merged.sweeps[0].ray_vars, dbz.sweeps[0].ray_vars,
        "a well-sized incoming variable replaces a mis-sized existing one"
    );
    assert_eq!(merged.sweeps[0].clone().seal(), Ok(()));
}

/// Hurum's DBZH and TH parts carry filtered reflectivity and total power:
/// different FM301 names, so every sweep gains TH and nothing collides (the
/// legacy model merged both as one reflectivity moment and dropped TH).
#[test]
fn merge_keeps_distinct_names_of_one_quantity() {
    let expected = golden();
    let dbzh = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1445-dbzh"
    ));
    let th = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1445-th"
    ));
    let th_name = FieldName::parse("TH");
    let (merged, report) = merge_volumes(vec![dbzh.clone(), th.clone()]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["nohur_dbzh_th"],
        "DBZH + TH",
    );
    assert_eq!(report.field_collisions, 0);
    assert_eq!(report.merged_fields, 10);
    for (index, sweep) in merged.sweeps.iter().enumerate() {
        let kept = field(sweep, &FieldName::Dbzh);
        assert!(fields_identical(
            kept,
            field(&dbzh.sweeps[index], &FieldName::Dbzh)
        ));
        let moved = field(sweep, &th_name);
        let original = field(&th.sweeps[index], &th_name);
        assert_eq!(moved.quantity, Quantity::TotalPower);
        assert_eq!(moved.data, original.data);
        assert_eq!(
            moved.native_geometry(&sweep.range),
            original.native_geometry(&th.sweeps[index].range)
        );
    }
}

/// Hurum's TH part renamed to DBZH (an edit of the real part: same name,
/// other planes) collides on every sweep and the first part's (filtered
/// DBZH) field is kept; the raw planes differ where h5py shows TH echo that
/// DBZH filtered out.
#[test]
fn merge_collision_keeps_first_part_field() {
    let expected = golden();
    let dbzh = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1445-dbzh"
    ));
    let th = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1445-th"
    ));
    let th_name = FieldName::parse("TH");
    let mut renamed = th.clone();
    for sweep in &mut renamed.sweeps {
        let index = sweep.field_index(&th_name).expect("TH");
        sweep.fields[index].name = FieldName::Dbzh;
    }
    let (merged, report) = merge_volumes(vec![dbzh.clone(), renamed]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["nohur_dbzh_th_as_dbzh"],
        "DBZH + TH as DBZH",
    );
    assert_eq!(report.field_collisions, 10);
    assert_eq!(report.merged_fields, 0);
    let mut differing = 0;
    for (index, sweep) in merged.sweeps.iter().enumerate() {
        let kept = field(sweep, &FieldName::Dbzh);
        assert!(fields_identical(
            kept,
            field(&dbzh.sweeps[index], &FieldName::Dbzh)
        ));
        if kept.data != field(&th.sweeps[index], &th_name).data {
            differing += 1;
        }
    }
    assert!(differing > 0, "the TH planes differ from the DBZH planes");

    // h5py raw probes of both parts on every sweep, as physical values
    // (gain * raw + offset, nodata/undetect as no data).
    for (index, sweep) in merged.sweeps.iter().enumerate() {
        let dbzh_q = &expected["odim"]["nohur_dbzh"]["sweeps"][index]["quantities"]["DBZH"];
        let th_q = &expected["odim"]["nohur_th"]["sweeps"][index]["quantities"]["TH"];
        let kept = field(sweep, &FieldName::Dbzh);
        let th_field = field(&th.sweeps[index], &th_name);
        for (probe, th_probe) in array(&dbzh_q["raw_probes"])
            .iter()
            .zip(array(&th_q["raw_probes"]))
        {
            let row = as_usize(&probe["ray"]);
            let gate = as_usize(&probe["gate"]);
            let cases = [
                (
                    kept,
                    odim_physical(dbzh_q, probe["raw"].as_u64().unwrap()),
                    "DBZH",
                ),
                (
                    th_field,
                    odim_physical(th_q, th_probe["raw"].as_u64().unwrap()),
                    "TH",
                ),
            ];
            for (field, want, name) in cases {
                let got = field.value(row, gate).map(f64::from);
                match (got, want) {
                    (Some(got), Some(want)) => assert_close(
                        got,
                        want,
                        1e-4,
                        &format!("{name} sweep {index} ray {row} gate {gate}"),
                    ),
                    (got, want) => {
                        assert_eq!(got, want, "{name} sweep {index} ray {row} gate {gate}")
                    }
                }
            }
        }
    }
}

/// Three KIWA chunk parts from different sweeps (VCP cuts 5, 1 and 3 at 1.27,
/// 0.48 and 0.88 deg): nothing matches, so the sweeps are unioned, sorted by
/// fixed angle and renumbered.
#[test]
fn merge_unions_unmatched_sweeps_sorted_by_fixed_angle() {
    let expected = golden();
    let (Some(c026), Some(c002), Some(c014)) =
        (kiwa_part("026"), kiwa_part("002"), kiwa_part("014"))
    else {
        eprintln!("KIWA chunks unavailable; skipping");
        return;
    };
    let Some(want) = expected["merges"].get("kiwa_026_002_014") else {
        panic!("golden merge kiwa_026_002_014 missing");
    };
    for part in [&c026, &c002, &c014] {
        assert_eq!(part.sweeps.len(), 1);
        assert_eq!(part.sweeps[0].nrays(), 120);
    }
    let (merged, report) = merge_volumes(vec![c026, c002, c014]).unwrap();
    assert_merge_matches(&merged, report, want, "KIWA 026+002+014");
    assert!(
        merged
            .sweeps
            .windows(2)
            .all(|pair| pair[0].fixed_angle_deg < pair[1].fixed_angle_deg)
    );
    assert_eq!(report.merged_fields, 0);
    assert_eq!(report.skipped_geometry, 0);
}

/// The KIWA 0.48 deg sweep as one chunk (120 rays) and as two chunks (240
/// rays): same site and fixed angle, different ray counts, so the incoming
/// sweep is skipped and counted.
#[test]
fn merge_skips_matched_sweep_with_different_ray_count() {
    let expected = golden();
    let Some(one) = kiwa_part("002") else {
        eprintln!("KIWA chunks unavailable; skipping");
        return;
    };
    let start = std::fs::read(recast_radar_testdata::require_file!(
        "l2chunk-kiwa-307-20260917-003629-001-s"
    ))
    .unwrap();
    let mut bytes = kiwa_chunk_bytes(
        &start,
        &recast_radar_testdata::require_file!("l2chunk-kiwa-307-20260917-003629-002-i"),
    );
    bytes.extend_from_slice(
        &std::fs::read(recast_radar_testdata::require_file!(
            "l2chunk-kiwa-307-20260917-003629-003-i"
        ))
        .unwrap(),
    );
    let two = level2_bytes(&bytes);
    assert_eq!(one.sweeps[0].nrays(), 120);
    assert_eq!(two.sweeps[0].nrays(), 240);
    assert_eq!(one.sweeps[0].fixed_angle_deg, two.sweeps[0].fixed_angle_deg);

    let (merged, report) = merge_volumes(vec![one.clone(), two]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["kiwa_002_vs_002_003"],
        "KIWA 120 vs 240",
    );
    assert_eq!(merged.sweeps.len(), 1);
    assert_eq!(merged.sweeps[0].nrays(), 120);
    assert_eq!(report.skipped_geometry, 1);
    assert_eq!(report.merged_fields, 0);
}

/// KTLX 2013 and 2024 split cuts: same site, the same VCP cut angle (0.48
/// deg) on all four sweeps, 480 rays each, but the azimuth grids start 44 and
/// 58 deg apart, so both incoming sweeps are skipped.
#[test]
fn merge_skips_matched_sweep_with_shifted_azimuths() {
    let expected = golden();
    let a = level2(&recast_radar_testdata::require_file!(
        "l2-ktlx-20130520-201643-trim"
    ));
    let b = level2(&recast_radar_testdata::require_file!(
        "l2-ktlx-20240315-000217-trim"
    ));
    for (sweep_a, sweep_b) in [(&a.sweeps[0], &b.sweeps[0]), (&a.sweeps[1], &b.sweeps[1])] {
        assert!(
            (sweep_a.fixed_angle_deg - sweep_b.fixed_angle_deg).abs() <= ANGLE_MATCH_TOLERANCE_DEG
        );
        assert_eq!(sweep_a.nrays(), sweep_b.nrays());
        assert!((sweep_a.rays.azimuth_deg[0] - sweep_b.rays.azimuth_deg[0]).abs() > 40.0);
    }
    let (merged, report) = merge_volumes(vec![a.clone(), b.clone()]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["ktlx_2013_2024"],
        "KTLX 2013 + 2024",
    );
    assert_eq!(report.skipped_geometry, 2);
    assert_eq!(report.merged_fields, 0);
    assert!(
        without_nan(&merged).sweeps == without_nan(&a).sweeps,
        "the 2013 sweeps are kept as they are"
    );
    // The time coverage is the union of the parts' coverage.
    let (first, second) = (a.time_coverage.unwrap(), b.time_coverage.unwrap());
    let coverage = merged.time_coverage.unwrap();
    assert_eq!(coverage.start, first.start.min(second.start));
    assert_eq!(coverage.end, first.end.max(second.end));
    assert_eq!(coverage.start, first.start);
    assert_eq!(coverage.end, second.end);
}

/// Jabbeke's parts with ray 0 (the bin centred 0.5 deg east of north) written
/// on the two sides of the wrap, 359.99 deg in the DBZH part and 0.01 deg in
/// the VRAD part (edits of the real values; the other 359 rays keep the file
/// values): the 0.02 deg difference across 0/360 is within tolerance and the
/// sweeps still merge.
#[test]
fn merge_accepts_azimuths_equal_across_the_north_wrap() {
    let expected = golden();
    let mut dbzh = odim(&recast_radar_testdata::require_file!(
        "odim-bejab-20260612-1450-dbzh"
    ));
    let mut vrad = odim(&recast_radar_testdata::require_file!(
        "odim-bejab-20260612-1450-vrad"
    ));
    assert_eq!(dbzh.sweeps[0].rays.azimuth_deg[0], 0.5);
    assert_eq!(vrad.sweeps[0].rays.azimuth_deg[0], 0.5);
    dbzh.sweeps[0].rays.azimuth_deg[0] = 359.99;
    vrad.sweeps[0].rays.azimuth_deg[0] = 0.01;

    let (merged, report) = merge_volumes(vec![dbzh, vrad]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["bejab_dbzh_vrad"],
        "bejab wrapped",
    );
    assert!(merged.sweeps[0].field(&FieldName::parse("VRAD")).is_some());
    assert_eq!(report.merged_fields, 9);
    assert_eq!(report.skipped_geometry, 0);
    assert_eq!(
        merged.sweeps[0].rays.azimuth_deg[0], 359.99,
        "base ray keeps its azimuth"
    );
}

/// KTLX 1999-05-04 sweep 5 (2.46 deg, Message 1): DBZH at 1 km gates and
/// VRADH/WRADH at 250 m gates on the same 367 rays. Split into a reflectivity
/// part and a Doppler part, the merge accepts the sweep and each field keeps
/// its own native geometry. On the KTLX 2024 Doppler sweep, a reflectivity
/// part whose Nyquist velocities were cleared gets them back from the
/// velocity part (the Message 31 RAD block value).
#[test]
fn merge_accepts_matched_sweep_with_different_gate_layout() {
    let expected = golden();
    let want = &expected["ktlx_1999_sweep4"];
    let path = recast_radar_testdata::require_file!("l2-ktlx-19990504-002218");
    let volume = level2(&path);
    let index = as_usize(&want["sweep_index"]);
    let mut single = volume.clone();
    single.sweeps = vec![volume.sweeps[index].clone()];
    let sweep = &single.sweeps[0];
    assert_close(
        f64::from(sweep.rays.elevation_deg[0]),
        as_f64(&want["elevation_deg"]),
        1e-6,
        "first ray elevation",
    );
    assert_eq!(sweep.nrays(), as_usize(&want["rays"]));
    let dbzh = field(sweep, &FieldName::Dbzh);
    let vradh = field(sweep, &FieldName::Vradh);
    let (_, dbzh_spacing) = dbzh.native_geometry(&sweep.range).unwrap();
    let (vradh_first, vradh_spacing) = vradh.native_geometry(&sweep.range).unwrap();
    assert_eq!(dbzh.shape().1, as_usize(&want["ref"]["gates"]));
    assert_eq!(dbzh_spacing, as_f64(&want["ref"]["gate_spacing_m"]));
    assert_eq!(vradh.shape().1, as_usize(&want["vel"]["gates"]));
    assert_eq!(vradh_spacing, as_f64(&want["vel"]["gate_spacing_m"]));
    assert_eq!(vradh_first, as_f64(&want["vel"]["first_gate_m"]));
    assert_ne!(dbzh.gates, vradh.gates);

    let ref_part = part_with_only(&single, &[Quantity::Reflectivity]);
    let doppler_part = part_with_only(
        &single,
        &[Quantity::RadialVelocity, Quantity::SpectrumWidth],
    );
    let (merged, report) = merge_volumes(vec![ref_part, doppler_part]).unwrap();
    assert_eq!(
        report,
        MergeReport {
            merged_fields: 2,
            skipped_geometry: 0,
            field_collisions: 0,
        }
    );
    assert_eq!(merged.sweeps.len(), 1);
    let merged_sweep = &merged.sweeps[0];
    assert_eq!(merged_sweep.range, sweep.range);
    assert_eq!(field(merged_sweep, &FieldName::Dbzh).gates, dbzh.gates);
    assert!(
        fields_identical(field(merged_sweep, &FieldName::Vradh), vradh),
        "a field keeps its own native gates on the shared range"
    );
    assert!(fields_identical(
        field(merged_sweep, &FieldName::Wradh),
        field(sweep, &FieldName::Wradh)
    ));

    // Nyquist fill on the KTLX 2024 Doppler sweep (Message 31 RAD block).
    let volume = level2(&recast_radar_testdata::require_file!(
        "l2-ktlx-20240315-000217-trim"
    ));
    let mut doppler = volume.clone();
    doppler.sweeps = vec![volume.sweeps[1].clone()];
    let nyquist = doppler.sweeps[0]
        .ray_vars
        .nyquist_velocity_mps
        .clone()
        .expect("Nyquist");
    assert!(nyquist.iter().all(|&v| v > 0.0));
    let mut ref_part = part_with_only(&doppler, &[Quantity::Reflectivity]);
    for value in ref_part.sweeps[0]
        .ray_vars
        .nyquist_velocity_mps
        .as_mut()
        .unwrap()
    {
        *value = f32::NAN;
    }
    let vel_part = part_with_only(
        &doppler,
        &[Quantity::RadialVelocity, Quantity::SpectrumWidth],
    );
    let (merged, report) = merge_volumes(vec![ref_part, vel_part]).unwrap();
    assert_eq!(report.merged_fields, 2);
    assert_eq!(
        merged.sweeps[0].ray_vars.nyquist_velocity_mps.as_ref(),
        Some(&nyquist),
        "cleared Nyquist velocities are filled from the velocity part"
    );
}

/// Hurum's scan as three per-quantity files (DWD/CHMI-style assembly): the
/// VRADH part adds velocity to the 8 sweeps it covers, the TH part adds TH to
/// all 10, and the time reference is the earliest part time.
#[test]
fn merge_three_product_parts_assembles_one_scan() {
    let expected = golden();
    let dbzh = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1445-dbzh"
    ));
    let vradh = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1446-vradh"
    ));
    let th = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1445-th"
    ));
    assert_eq!(dbzh.sweeps.len(), 10);
    assert_eq!(vradh.sweeps.len(), 8);
    assert_eq!(th.sweeps.len(), 10);
    let (merged, report) = merge_volumes(vec![dbzh.clone(), vradh.clone(), th]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["nohur_dbzh_vradh_th"],
        "nohur",
    );
    assert_eq!(
        report,
        MergeReport {
            merged_fields: 18,
            skipped_geometry: 0,
            field_collisions: 0,
        }
    );
    assert_eq!(merged.time_reference, dbzh.time_reference);
    let with_velocity = merged
        .sweeps
        .iter()
        .filter(|sweep| sweep.field(&FieldName::Vradh).is_some())
        .count();
    assert_eq!(with_velocity, 8);
    assert!(
        merged.sweeps[..2]
            .iter()
            .all(|sweep| sweep.field(&FieldName::Vradh).is_none()),
        "0.5 and 1.0 deg have no velocity part"
    );
    for (sweep, source) in merged.sweeps[2..].iter().zip(&vradh.sweeps) {
        let moved = field(sweep, &FieldName::Vradh);
        let original = field(source, &FieldName::Vradh);
        assert_eq!(moved.data, original.data);
        assert_eq!(
            moved.native_geometry(&sweep.range),
            original.native_geometry(&source.range)
        );
    }
}

/// JMA Osaka: the N5 (reflectivity) member's four ladders repeat the low
/// tilts and the N6 (velocity) member's two ladders repeat them too, each
/// member numbering its own sweeps from 1. Merged, each N6 sweep lands on
/// the N5 repetition whose azimuth grid it shares (the GRIB2 start azimuth),
/// the two 0.3 deg velocity sweeps whose start azimuths match no
/// reflectivity sweep are skipped, and the ladder is renumbered 1..=26.
/// Merging the N6 member twice makes every velocity field collide.
#[test]
fn merge_jma_repeated_tilts_keep_repetition_velocity_and_renumber() {
    let expected = golden();
    let n5 = jma(&recast_radar_testdata::require_file!(
        "jma-n5-20191012-090000-rs47773"
    ));
    let n6 = jma(&recast_radar_testdata::require_file!(
        "jma-n6-20191012-090000-rs47773"
    ));
    assert_eq!(n5.sweeps.len(), 26);
    assert_eq!(n6.sweeps.len(), 13);
    assert_eq!(
        n6.sweeps
            .iter()
            .map(|s| s.elevation_number)
            .collect::<Vec<_>>(),
        (1..=13).map(Some).collect::<Vec<_>>()
    );

    let (merged, report) = merge_volumes(vec![n5.clone(), n6.clone()]).unwrap();
    assert_merge_matches(&merged, report, &expected["merges"]["jma_n5_n6"], "N5 + N6");
    assert_eq!(
        report,
        MergeReport {
            merged_fields: 11,
            skipped_geometry: 2,
            field_collisions: 0,
        }
    );
    // Every velocity field landed on the reflectivity sweep with the same
    // azimuth grid, and repetition-2 velocity is on the repetition-2 sweep.
    for n6_sweep in &n6.sweeps {
        let velocity = field(n6_sweep, &FieldName::Vradh);
        let targets: Vec<&Sweep> = merged
            .sweeps
            .iter()
            .filter(|sweep| {
                sweep
                    .field(&FieldName::Vradh)
                    .is_some_and(|moved| data_identical(&moved.data, &velocity.data))
            })
            .collect();
        if targets.is_empty() {
            assert_eq!(
                n6_sweep.fixed_angle_deg, 0.3,
                "only the 0.3 deg velocity sweeps are skipped"
            );
            continue;
        }
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].rays.azimuth_deg[0], n6_sweep.rays.azimuth_deg[0]);
        let moved = field(targets[0], &FieldName::Vradh);
        assert_eq!(
            moved.native_geometry(&targets[0].range),
            velocity.native_geometry(&n6_sweep.range)
        );
    }
    let five_degree: Vec<&Sweep> = merged
        .sweeps
        .iter()
        .filter(|sweep| sweep.fixed_angle_deg == 5.0)
        .collect();
    assert_eq!(five_degree.len(), 3);
    assert!(five_degree[0].field(&FieldName::Vradh).is_some());
    assert!(five_degree[1].field(&FieldName::Vradh).is_some());
    assert!(
        five_degree[2].field(&FieldName::Vradh).is_none(),
        "the long-range 800-gate 5.0 deg sweep has no Doppler twin"
    );
    assert_ne!(
        five_degree[0].rays.azimuth_deg[0],
        five_degree[1].rays.azimuth_deg[0]
    );

    let (merged_twice, report) = merge_volumes(vec![n5, n6.clone(), n6]).unwrap();
    assert_merge_matches(
        &merged_twice,
        report,
        &expected["merges"]["jma_n5_n6_n6"],
        "N5 + N6 + N6",
    );
    assert_eq!(
        report.field_collisions, 11,
        "the repeated member collides on every landed sweep"
    );
    assert_eq!(report.skipped_geometry, 4);
    assert_eq!(merged_twice.sweeps.len(), merged.sweeps.len());
    for (twice, once) in merged_twice.sweeps.iter().zip(&merged.sweeps) {
        assert_eq!(field_names(twice), field_names(once));
        for field in &once.fields {
            assert!(
                fields_identical(common::field(twice, &field.name), field),
                "first part wins every collision"
            );
        }
    }
}

/// `part` with its range cut to the native extent of its fields (which must
/// share one geometry), as a product-per-file feed would write it.
fn with_native_range(mut part: Volume) -> Volume {
    for sweep in &mut part.sweeps {
        let (first, spacing) = sweep.fields[0]
            .native_geometry(&sweep.range)
            .expect("geometry");
        let mut ngates = 0;
        for field in &sweep.fields {
            assert_eq!(field.native_geometry(&sweep.range), Some((first, spacing)));
            ngates = ngates.max(field.ngates);
        }
        sweep.range = RangeCoord::Uniform {
            first_center_m: first,
            spacing_m: spacing,
            ngates,
        };
        for field in &mut sweep.fields {
            field.gates = GateMapping::IDENTITY;
        }
        assert_eq!(sweep.seal(), Ok(()));
    }
    part
}

/// `part` without the first `gates` native gates of every field: a product
/// whose first range gate is further out.
fn without_leading_gates(mut part: Volume, gates: u32) -> Volume {
    for sweep in &mut part.sweeps {
        let RangeCoord::Uniform {
            first_center_m,
            spacing_m,
            ngates,
        } = sweep.range.clone()
        else {
            panic!("Level II ranges are uniform");
        };
        for field in &mut sweep.fields {
            assert_eq!(field.gates, GateMapping::IDENTITY);
            let width = field.ngates as usize;
            let FieldData::U8 { values, .. } = &mut field.data else {
                panic!("KTLX 1999 fields are u8");
            };
            *values = values
                .chunks(width)
                .flat_map(|row| row[gates as usize..].iter().copied())
                .collect();
            field.ngates -= gates;
        }
        sweep.range = RangeCoord::Uniform {
            first_center_m: first_center_m + f64::from(gates) * spacing_m,
            spacing_m,
            ngates: ngates - gates,
        };
        assert_eq!(sweep.seal(), Ok(()));
    }
    part
}

/// KTLX 1999-05-04 sweep 5 as a reflectivity file (356 x 1000 m from centre
/// 0) and a Doppler file (920 x 250 m from centre -375), each with the range
/// of its own fields: merged in either order, the sweep gets the range and
/// gate mappings the reader builds from the whole file (the 250 m grid,
/// extended to 1424 gates for reflectivity, DBZH on stride 4). A Doppler
/// file that starts 4 gates later has its range extended back to the
/// reflectivity's first gate; one shifted by 100 m or on 300 m gates does
/// not align and its fields are skipped.
#[test]
fn merge_aligns_real_gate_layouts_of_separate_products() {
    let expected = golden();
    let want = &expected["ktlx_1999_sweep4"];
    let path = recast_radar_testdata::require_file!("l2-ktlx-19990504-002218");
    let volume = level2(&path);
    let mut single = volume.clone();
    single.sweeps = vec![volume.sweeps[as_usize(&want["sweep_index"])].clone()];
    let decoded = &single.sweeps[0];
    let reflectivity = with_native_range(part_with_only(&single, &[Quantity::Reflectivity]));
    let doppler = with_native_range(part_with_only(
        &single,
        &[Quantity::RadialVelocity, Quantity::SpectrumWidth],
    ));
    assert_eq!(
        reflectivity.sweeps[0].range,
        RangeCoord::Uniform {
            first_center_m: as_f64(&want["ref"]["first_gate_m"]),
            spacing_m: as_f64(&want["ref"]["gate_spacing_m"]),
            ngates: as_usize(&want["ref"]["gates"]) as u32,
        }
    );
    assert_eq!(
        doppler.sweeps[0].range,
        RangeCoord::Uniform {
            first_center_m: as_f64(&want["vel"]["first_gate_m"]),
            spacing_m: as_f64(&want["vel"]["gate_spacing_m"]),
            ngates: as_usize(&want["vel"]["gates"]) as u32,
        }
    );

    for (order, parts) in [
        ("coarse first", vec![reflectivity.clone(), doppler.clone()]),
        ("fine first", vec![doppler.clone(), reflectivity.clone()]),
    ] {
        let (merged, report) = merge_volumes(parts).unwrap();
        assert_eq!(
            report.merged_fields,
            if order == "coarse first" { 2 } else { 1 }
        );
        assert_eq!(report.skipped_geometry, 0, "{order}");
        let sweep = &merged.sweeps[0];
        assert_eq!(sweep.range, decoded.range, "{order}: range");
        assert_eq!(field_names(sweep), field_names(decoded), "{order}");
        for field in &sweep.fields {
            assert!(
                fields_identical(field, common::field(decoded, &field.name)),
                "{order}: {} mapping and values",
                field.name
            );
        }
    }

    // The Doppler product starting 4 gates (1 km) further out.
    let later = without_leading_gates(doppler.clone(), 4);
    let (merged, report) = merge_volumes(vec![later, reflectivity.clone()]).unwrap();
    assert_eq!(report.merged_fields, 1);
    let sweep = &merged.sweeps[0];
    assert_eq!(sweep.range, decoded.range, "extended back to -375 m");
    assert_eq!(
        field(sweep, &FieldName::Dbzh).gates,
        field(decoded, &FieldName::Dbzh).gates
    );
    for name in [FieldName::Vradh, FieldName::Wradh] {
        let moved = field(sweep, &name);
        assert_eq!(
            moved.gates,
            GateMapping {
                start: 4,
                stride: 1
            },
            "{name}"
        );
        for row in [0, 100] {
            for gate in [0, 10, moved.ngates as usize - 1] {
                assert_eq!(
                    moved.gate(row, gate),
                    field(decoded, &name).gate(row, gate + 4),
                    "{name} [{row}, {gate}]"
                );
            }
        }
    }

    // Misaligned Doppler products: every field is skipped.
    for (what, first, spacing) in [
        ("shifted 100 m", -275.0, 250.0),
        ("300 m gates", -375.0, 300.0),
    ] {
        let mut misaligned = doppler.clone();
        misaligned.sweeps[0].range = RangeCoord::Uniform {
            first_center_m: first,
            spacing_m: spacing,
            ngates: as_usize(&want["vel"]["gates"]) as u32,
        };
        let (merged, report) = merge_volumes(vec![reflectivity.clone(), misaligned]).unwrap();
        assert_eq!(
            report,
            MergeReport {
                merged_fields: 0,
                skipped_geometry: 2,
                field_collisions: 0,
            },
            "{what}"
        );
        assert_eq!(merged.sweeps[0].fields.len(), 1, "{what}");
        assert_eq!(
            merged.sweeps[0].range, reflectivity.sweeps[0].range,
            "{what}"
        );
    }
}
