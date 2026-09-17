//! `merge_radar_volumes` on real per-product, per-sweep and per-chunk parts.
//!
//! Expected outcomes: `testdata/golden/core/model.json` (`merges`), written by
//! `tools/core_golden.py`: a reference implementation of the documented merge
//! rules over sweep metadata from h5py (ODIM parts), MetPy (Level II) and a
//! GRIB2 section walker (JMA), never from this workspace's readers.

mod common;

use std::collections::BTreeSet;

use common::{
    array, as_f64, as_str, as_usize, assert_close, cfradial, golden, grids_identical, jma,
    kiwa_chunk_bytes, level2, level2_bytes, moment, odim, odim_physical, part_with_only, time,
};
use recast_radar_core::{
    CUT_ELEVATION_MATCH_TOLERANCE_DEG, MergeReport, MomentType, RadarVolume, merge_radar_volumes,
};
use serde_json::Value;

fn report(value: &Value) -> MergeReport {
    MergeReport {
        merged_moments: as_usize(&value["merged_moments"]),
        skipped_geometry: as_usize(&value["skipped_geometry"]),
        moment_collisions: as_usize(&value["moment_collisions"]),
    }
}

/// The merged volume against the reference merge: site, earliest time, the
/// report counters, and every cut's elevation, number, radial count and
/// moment set.
fn assert_merge_matches(merged: &RadarVolume, actual: MergeReport, expected: &Value, what: &str) {
    assert_eq!(merged.site.id, as_str(&expected["site"]), "{what}: site");
    assert_eq!(
        merged.volume_time,
        time(as_str(&expected["time"])),
        "{what}: volume time"
    );
    assert_eq!(actual, report(&expected["report"]), "{what}: report");
    let cuts = array(&expected["cuts"]);
    assert_eq!(merged.cuts.len(), cuts.len(), "{what}: cut count");
    for (index, (cut, want)) in merged.cuts.iter().zip(cuts).enumerate() {
        assert_close(
            f64::from(cut.elevation_deg),
            as_f64(&want["elevation_deg"]),
            1e-6,
            &format!("{what}: cut {index} elevation"),
        );
        assert_eq!(
            cut.elevation_number,
            Some(as_usize(&want["elevation_number"]) as u8),
            "{what}: cut {index} number"
        );
        assert_eq!(
            cut.radials.len(),
            as_usize(&want["rays"]),
            "{what}: cut {index} radials"
        );
        let moments: BTreeSet<MomentType> = want["moments"]
            .as_object()
            .expect("moments")
            .keys()
            .map(|name| moment(name))
            .collect();
        assert_eq!(
            cut.moments_available(),
            moments,
            "{what}: cut {index} moments"
        );
        for grid in cut.moments.values() {
            assert_eq!(
                grid.radial_count(),
                cut.radials.len(),
                "{what}: cut {index} grid rows"
            );
        }
    }
}

/// A KIWA real-time part: the start chunk plus one intermediate chunk.
fn kiwa_part(number: &str) -> Option<RadarVolume> {
    let start = recast_radar_testdata::path("l2chunk-kiwa-307-20260917-003629-001-s").ok()?;
    let chunk =
        recast_radar_testdata::path(&format!("l2chunk-kiwa-307-20260917-003629-{number}-i"))
            .ok()?;
    let start = std::fs::read(start).ok()?;
    Some(level2_bytes(&kiwa_chunk_bytes(&start, &chunk)))
}

#[test]
fn merge_rejects_empty_input() {
    let err = merge_radar_volumes(Vec::new()).unwrap_err();
    assert!(err.contains("no radar volumes"), "unexpected error: {err}");
}

/// One part (KTLX 2024-03-15 split cut: the 0.58 deg surveillance sweep
/// before the 0.48 deg Doppler sweep) merges to itself with the cuts sorted
/// by elevation and renumbered.
#[test]
fn merge_single_part_is_identity_with_sorted_cuts() {
    let expected = golden();
    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
    let part = level2(&path);
    assert!(part.cuts[0].elevation_deg > part.cuts[1].elevation_deg);
    let (merged, report) = merge_radar_volumes(vec![part.clone()]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["single_part_ktlx_2024"],
        "KTLX 2024",
    );
    assert_eq!(merged.site, part.site);
    assert_eq!(merged.vcp, part.vcp);
    assert_eq!(merged.metadata, part.metadata);
    let mut sorted = part.clone();
    sorted.cuts.swap(0, 1);
    sorted.cuts[0].elevation_number = Some(1);
    sorted.cuts[1].elevation_number = Some(2);
    assert_eq!(merged, sorted, "identity up to the documented sort");
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
    let err = merge_radar_volumes(vec![kiwa.clone(), ktlx.clone()]).unwrap_err();
    assert!(
        err.contains(&kiwa.site.id) && err.contains(&ktlx.site.id),
        "error must name both sites: {err}"
    );
    let parts = array(&expected["merges"]["kiwa_002_vs_ktlx_2024"]["parts"]);
    assert_eq!(kiwa.site.id, as_str(&parts[0]["site"]));
    assert_eq!(ktlx.site.id, as_str(&parts[1]["site"]));
    assert!(expected["merges"]["kiwa_002_vs_ktlx_2024"]["error"].is_string());
}

/// Hurum's 14:45 scan as three ORD files: the VRADH part is stamped 14:46:48
/// (its last sweep is a later vertical scan), the DBZH and TH parts 14:45:13.
/// Whatever the part order, the merged volume takes the earliest time.
#[test]
fn merge_keeps_earliest_volume_time() {
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
    assert_eq!(vradh.volume_time, time(as_str(&parts[0]["time"])));
    assert_eq!(th.volume_time, time(as_str(&parts[1]["time"])));
    assert_eq!(dbzh.volume_time, time(as_str(&parts[2]["time"])));
    assert!(vradh.volume_time > dbzh.volume_time);

    let (merged, report) =
        merge_radar_volumes(vec![vradh.clone(), th.clone(), dbzh.clone()]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["nohur_vradh_th_dbzh"],
        "VRADH first",
    );
    assert_eq!(merged.volume_time, dbzh.volume_time);
    let (merged, report) = merge_radar_volumes(vec![dbzh.clone(), vradh, th]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["nohur_dbzh_vradh_th"],
        "DBZH first",
    );
    assert_eq!(merged.volume_time, dbzh.volume_time);
}

/// Jabbeke's Doppler task as one PVOL per quantity (SHMU/RMI style): the
/// VRAD part's 9 sweeps have the same elevations and 360-ray geometry as the
/// DBZH part's, so every cut ends up with REF and VEL.
#[test]
fn merge_unions_moments_of_elevation_matched_cuts() {
    let expected = golden();
    let dbzh = odim(&recast_radar_testdata::require_file!(
        "odim-bejab-20260612-1450-dbzh"
    ));
    let vrad = odim(&recast_radar_testdata::require_file!(
        "odim-bejab-20260612-1450-vrad"
    ));
    for part in [&dbzh, &vrad] {
        assert_eq!(part.cuts.len(), 9);
        assert!(part.cuts.iter().all(|cut| cut.moments.len() == 1));
    }
    let (merged, report) = merge_radar_volumes(vec![dbzh.clone(), vrad.clone()]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["bejab_dbzh_vrad"],
        "bejab",
    );
    for (index, cut) in merged.cuts.iter().enumerate() {
        assert_eq!(
            cut.elevation_deg, dbzh.cuts[index].elevation_deg,
            "first part's cut is the base"
        );
        assert_eq!(
            cut.moments[&MomentType::Reflectivity],
            dbzh.cuts[index].moments[&MomentType::Reflectivity]
        );
        assert_eq!(
            cut.moments[&MomentType::Velocity],
            vrad.cuts[index].moments[&MomentType::Velocity]
        );
    }
    assert_eq!(
        report,
        MergeReport {
            merged_moments: 9,
            skipped_geometry: 0,
            moment_collisions: 0,
        }
    );
}

/// Irene's two sweeps split into a DBZ part and a VEL part (both real): the
/// per-ray `prt` and `unambiguous_range` values from netCDF4 survive the
/// merge; a value the first part lacks is filled from the second, and a
/// value the first part has is not overwritten by a second-part edit.
#[test]
fn merge_fills_aligned_ray_instrument_metadata_without_overwriting_source_values() {
    let expected = golden();
    let irene = &expected["irene"];
    let path =
        recast_radar_testdata::require_file!("cfrad1-irene-sr2-20110827-120420-sur-sweeps01");
    let volume = cfradial(&path);
    let mut dbz = part_with_only(&volume, &[MomentType::Reflectivity]);
    let mut vel = part_with_only(&volume, &[MomentType::Velocity]);
    let prt_s = as_f64(&irene["sweeps"][0]["prt_s_unique"][0]) as f32;
    let range_km = as_f64(&irene["sweeps"][0]["unambiguous_range_km_unique"][0]) as f32;
    assert_eq!(dbz.cuts[0].ray_instrument_metadata[0].prt_s, Some(prt_s));
    assert_eq!(
        dbz.cuts[0].ray_instrument_metadata[0].unambiguous_range_km,
        Some(range_km)
    );

    // Edits of the real sidecars: the DBZ part loses its unambiguous range on
    // every ray of sweep 1 (to be filled from the VEL part); the VEL part's
    // prt on ray 0 of sweep 1 is doubled (must not overwrite the DBZ value).
    for entry in &mut dbz.cuts[0].ray_instrument_metadata {
        entry.unambiguous_range_km = None;
    }
    vel.cuts[0].ray_instrument_metadata[0].prt_s = Some(prt_s * 2.0);

    let (merged, report) = merge_radar_volumes(vec![dbz, vel]).unwrap();
    assert_eq!(report.merged_moments, 2);
    assert_eq!(report.skipped_geometry, 0);
    let metadata = merged.cuts[0]
        .aligned_ray_instrument_metadata()
        .unwrap()
        .unwrap();
    assert_eq!(metadata.len(), as_usize(&irene["sweeps"][0]["rays"]));
    assert_eq!(metadata[0].prt_s, Some(prt_s), "first source wins");
    for (ray, entry) in metadata.iter().enumerate() {
        assert_eq!(entry.prt_s, Some(prt_s), "ray {ray}");
        assert_eq!(
            entry.unambiguous_range_km,
            Some(range_km),
            "ray {ray}: filled from VEL part"
        );
        assert_eq!(entry.pulse_count, None);
        assert_eq!(entry.independent_samples, None);
    }
    // Sweep 2 was not edited: both parts agree and the values are the file's.
    for entry in merged.cuts[1]
        .aligned_ray_instrument_metadata()
        .unwrap()
        .unwrap()
    {
        assert_eq!(entry.prt_s, Some(prt_s));
        assert_eq!(entry.unambiguous_range_km, Some(range_km));
    }
}

/// A VEL part whose sidecar is one entry short (edit of the real sidecar) is
/// ignored: the merged cut keeps the DBZ part's aligned sidecar. The other
/// way round, a malformed first-part sidecar is replaced by the valid one.
#[test]
fn merge_ignores_malformed_incoming_ray_instrument_metadata() {
    let path =
        recast_radar_testdata::require_file!("cfrad1-irene-sr2-20110827-120420-sur-sweeps01");
    let volume = cfradial(&path);
    let dbz = part_with_only(&volume, &[MomentType::Reflectivity]);
    let mut malformed = part_with_only(&volume, &[MomentType::Velocity]);
    malformed.cuts[0].ray_instrument_metadata.pop();
    assert!(malformed.cuts[0].aligned_ray_instrument_metadata().is_err());

    let (merged, report) = merge_radar_volumes(vec![dbz.clone(), malformed.clone()]).unwrap();
    assert_eq!(report.merged_moments, 2);
    assert_eq!(
        merged.cuts[0].ray_instrument_metadata,
        dbz.cuts[0].ray_instrument_metadata
    );
    assert_eq!(
        merged.cuts[1].ray_instrument_metadata,
        dbz.cuts[1].ray_instrument_metadata
    );

    let (merged, _) = merge_radar_volumes(vec![malformed, dbz.clone()]).unwrap();
    assert_eq!(
        merged.cuts[0].ray_instrument_metadata, dbz.cuts[0].ray_instrument_metadata,
        "a valid incoming sidecar replaces a malformed existing one"
    );
    assert!(merged.cuts[0].aligned_ray_instrument_metadata().is_ok());
}

/// Hurum's DBZH and TH parts both decode to the reflectivity moment: every
/// cut collides and the first part's (filtered DBZH) grid is kept; the raw
/// planes differ where h5py shows TH echo that DBZH filtered out.
#[test]
fn merge_collision_keeps_first_part_grid() {
    let expected = golden();
    let dbzh = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1445-dbzh"
    ));
    let th = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1445-th"
    ));
    let (merged, report) = merge_radar_volumes(vec![dbzh.clone(), th.clone()]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["nohur_dbzh_th"],
        "DBZH + TH",
    );
    assert_eq!(report.moment_collisions, 10);
    assert_eq!(report.merged_moments, 0);
    let mut differing = 0;
    for (index, cut) in merged.cuts.iter().enumerate() {
        let kept = &cut.moments[&MomentType::Reflectivity];
        assert_eq!(kept, &dbzh.cuts[index].moments[&MomentType::Reflectivity]);
        if kept != &th.cuts[index].moments[&MomentType::Reflectivity] {
            differing += 1;
        }
    }
    assert!(differing > 0, "the TH planes differ from the DBZH planes");

    // h5py raw probes of both parts on every sweep, as physical values
    // (gain * raw + offset, nodata/undetect as no data).
    for (index, cut) in merged.cuts.iter().enumerate() {
        let dbzh_q = &expected["odim"]["nohur_dbzh"]["sweeps"][index]["quantities"]["DBZH"];
        let th_q = &expected["odim"]["nohur_th"]["sweeps"][index]["quantities"]["TH"];
        let kept = &cut.moments[&MomentType::Reflectivity];
        let th_grid = &th.cuts[index].moments[&MomentType::Reflectivity];
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
                    th_grid,
                    odim_physical(th_q, th_probe["raw"].as_u64().unwrap()),
                    "TH",
                ),
            ];
            for (grid, want, name) in cases {
                let got = grid.scaled_value(row, gate).map(f64::from);
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

/// Three KIWA chunk parts from different sweeps (1.33, 0.27 and 1.01 deg
/// first-radial elevations): nothing matches, so the cuts are unioned,
/// sorted by elevation and renumbered.
#[test]
fn merge_unions_unmatched_cuts_sorted_by_elevation() {
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
        assert_eq!(part.cuts.len(), 1);
        assert_eq!(part.cuts[0].radials.len(), 120);
    }
    let (merged, report) = merge_radar_volumes(vec![c026, c002, c014]).unwrap();
    assert_merge_matches(&merged, report, want, "KIWA 026+002+014");
    assert!(
        merged
            .cuts
            .windows(2)
            .all(|pair| pair[0].elevation_deg < pair[1].elevation_deg)
    );
    assert_eq!(report.merged_moments, 0);
    assert_eq!(report.skipped_geometry, 0);
}

/// The KIWA 0.53 deg sweep as one chunk (120 radials) and as two chunks (240
/// radials): same site and first-radial elevation, different radial counts,
/// so the incoming cut is skipped and counted.
#[test]
fn merge_skips_matched_cut_with_different_radial_count() {
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
    assert_eq!(one.cuts[0].radials.len(), 120);
    assert_eq!(two.cuts[0].radials.len(), 240);
    assert_eq!(one.cuts[0].elevation_deg, two.cuts[0].elevation_deg);

    let (merged, report) = merge_radar_volumes(vec![one.clone(), two]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["kiwa_002_vs_002_003"],
        "KIWA 120 vs 240",
    );
    assert_eq!(merged.cuts.len(), 1);
    assert_eq!(merged.cuts[0].radials.len(), 120);
    assert_eq!(report.skipped_geometry, 1);
    assert_eq!(report.merged_moments, 0);
}

/// KTLX 2013 and 2024 split cuts: same site, elevations within the 0.05 deg
/// tolerance (0.60/0.58 and 0.53/0.48 deg), 480 radials each, but the
/// azimuth grids start 44 and 58 deg apart, so both incoming cuts are
/// skipped.
#[test]
fn merge_skips_matched_cut_with_shifted_azimuths() {
    let expected = golden();
    let a = level2(&recast_radar_testdata::require_file!(
        "l2-ktlx-20130520-201643-trim"
    ));
    let b = level2(&recast_radar_testdata::require_file!(
        "l2-ktlx-20240315-000217-trim"
    ));
    for (cut_a, cut_b) in [(&a.cuts[0], &b.cuts[0]), (&a.cuts[1], &b.cuts[1])] {
        assert!(
            (cut_a.elevation_deg - cut_b.elevation_deg).abs() <= CUT_ELEVATION_MATCH_TOLERANCE_DEG
        );
        assert_eq!(cut_a.radials.len(), cut_b.radials.len());
        assert!((cut_a.radials[0].azimuth_deg - cut_b.radials[0].azimuth_deg).abs() > 40.0);
    }
    let (merged, report) = merge_radar_volumes(vec![a.clone(), b]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["ktlx_2013_2024"],
        "KTLX 2013 + 2024",
    );
    assert_eq!(report.skipped_geometry, 2);
    assert_eq!(report.merged_moments, 0);
    for cut in &merged.cuts {
        assert!(!cut.moments.is_empty());
    }
}

/// Jabbeke's parts with radial 0 (the bin centred 0.5 deg east of north)
/// written on the two sides of the wrap, 359.99 deg in the DBZH part and
/// 0.01 deg in the VRAD part (edits of the real values; the other 359
/// radials keep the file values): the 0.02 deg difference across 0/360 is
/// within tolerance and the cuts still merge.
#[test]
fn merge_accepts_azimuths_equal_across_the_north_wrap() {
    let expected = golden();
    let mut dbzh = odim(&recast_radar_testdata::require_file!(
        "odim-bejab-20260612-1450-dbzh"
    ));
    let mut vrad = odim(&recast_radar_testdata::require_file!(
        "odim-bejab-20260612-1450-vrad"
    ));
    assert_eq!(dbzh.cuts[0].radials[0].azimuth_deg, 0.5);
    assert_eq!(vrad.cuts[0].radials[0].azimuth_deg, 0.5);
    dbzh.cuts[0].radials[0].azimuth_deg = 359.99;
    vrad.cuts[0].radials[0].azimuth_deg = 0.01;

    let (merged, report) = merge_radar_volumes(vec![dbzh, vrad]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["bejab_dbzh_vrad"],
        "bejab wrapped",
    );
    assert!(merged.cuts[0].moments.contains_key(&MomentType::Velocity));
    assert_eq!(report.merged_moments, 9);
    assert_eq!(report.skipped_geometry, 0);
    assert_eq!(
        merged.cuts[0].radials[0].azimuth_deg, 359.99,
        "base radial keeps its azimuth"
    );
}

/// KTLX 1999-05-04 sweep 5 (2.46 deg, Message 1): REF at 1 km gates and
/// VEL/SW at 250 m gates on the same 367 radials. Split into a REF part and
/// a VEL/SW part, the merge accepts the cut and each grid keeps its own gate
/// range. On the KTLX 2024 Doppler cut, a REF part whose radial Nyquist
/// velocities were cleared gets them back from the VEL part (the Message 31
/// RAD block value).
#[test]
fn merge_accepts_matched_cut_with_different_gate_layout() {
    let expected = golden();
    let sweep = &expected["ktlx_1999_sweep4"];
    let path = recast_radar_testdata::require_file!("l2-ktlx-19990504-002218");
    let volume = level2(&path);
    let index = as_usize(&sweep["sweep_index"]);
    let mut single = volume.clone();
    single.cuts = vec![volume.cuts[index].clone()];
    let cut = &single.cuts[0];
    assert_close(
        f64::from(cut.elevation_deg),
        as_f64(&sweep["elevation_deg"]),
        1e-6,
        "elevation",
    );
    assert_eq!(cut.radials.len(), as_usize(&sweep["rays"]));
    let ref_grid = &cut.moments[&MomentType::Reflectivity];
    let vel_grid = &cut.moments[&MomentType::Velocity];
    assert_eq!(
        ref_grid.gate_range.gate_count,
        as_usize(&sweep["ref"]["gates"])
    );
    assert_eq!(
        ref_grid.gate_range.gate_spacing_m,
        as_f64(&sweep["ref"]["gate_spacing_m"]) as i32
    );
    assert_eq!(
        vel_grid.gate_range.gate_count,
        as_usize(&sweep["vel"]["gates"])
    );
    assert_eq!(
        vel_grid.gate_range.gate_spacing_m,
        as_f64(&sweep["vel"]["gate_spacing_m"]) as i32
    );
    assert_eq!(
        vel_grid.gate_range.first_gate_m,
        as_f64(&sweep["vel"]["first_gate_m"]) as i32
    );
    assert_ne!(ref_grid.gate_range, vel_grid.gate_range);

    let ref_part = part_with_only(&single, &[MomentType::Reflectivity]);
    let doppler_part = part_with_only(&single, &[MomentType::Velocity, MomentType::SpectrumWidth]);
    let (merged, report) = merge_radar_volumes(vec![ref_part, doppler_part]).unwrap();
    assert_eq!(
        report,
        MergeReport {
            merged_moments: 2,
            skipped_geometry: 0,
            moment_collisions: 0,
        }
    );
    assert_eq!(merged.cuts.len(), 1);
    let cut = &merged.cuts[0];
    assert_eq!(
        cut.moments[&MomentType::Reflectivity].gate_range,
        ref_grid.gate_range
    );
    assert_eq!(
        cut.moments[&MomentType::Velocity].gate_range,
        vel_grid.gate_range,
        "moment grid keeps its own range; radial range is only azimuth metadata"
    );
    assert_eq!(cut.moments[&MomentType::Velocity], *vel_grid);

    // Nyquist fill on the KTLX 2024 Doppler cut (Message 31 RAD block).
    let volume = level2(&recast_radar_testdata::require_file!(
        "l2-ktlx-20240315-000217-trim"
    ));
    let mut doppler = volume.clone();
    doppler.cuts = vec![volume.cuts[1].clone()];
    let nyquist: Vec<Option<f32>> = doppler.cuts[0]
        .radials
        .iter()
        .map(|r| r.nyquist_velocity_mps)
        .collect();
    assert!(nyquist.iter().all(|n| n.is_some_and(|v| v > 0.0)));
    let mut ref_part = part_with_only(&doppler, &[MomentType::Reflectivity]);
    for radial in &mut ref_part.cuts[0].radials {
        radial.nyquist_velocity_mps = None;
    }
    let vel_part = part_with_only(&doppler, &[MomentType::Velocity, MomentType::SpectrumWidth]);
    let (merged, report) = merge_radar_volumes(vec![ref_part, vel_part]).unwrap();
    assert_eq!(report.merged_moments, 2);
    let filled: Vec<Option<f32>> = merged.cuts[0]
        .radials
        .iter()
        .map(|r| r.nyquist_velocity_mps)
        .collect();
    assert_eq!(
        filled, nyquist,
        "cleared Nyquist velocities are filled from the VEL part"
    );
}

/// Hurum's scan as three per-quantity files (DWD/CHMI-style assembly): the
/// VRADH part adds velocity to the 8 cuts it covers, the TH part's planes
/// collide with the filtered DBZH and are dropped, and the volume time is
/// the earliest part time.
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
    assert_eq!(dbzh.cuts.len(), 10);
    assert_eq!(vradh.cuts.len(), 8);
    assert_eq!(th.cuts.len(), 10);
    let (merged, report) = merge_radar_volumes(vec![dbzh.clone(), vradh.clone(), th]).unwrap();
    assert_merge_matches(
        &merged,
        report,
        &expected["merges"]["nohur_dbzh_vradh_th"],
        "nohur",
    );
    assert_eq!(
        report,
        MergeReport {
            merged_moments: 8,
            skipped_geometry: 0,
            moment_collisions: 10,
        }
    );
    assert_eq!(merged.volume_time, dbzh.volume_time);
    let with_velocity = merged
        .cuts
        .iter()
        .filter(|cut| cut.moments.contains_key(&MomentType::Velocity))
        .count();
    assert_eq!(with_velocity, 8);
    assert!(
        merged.cuts[..2].iter().all(|cut| cut.moments.len() == 1),
        "0.5 and 1.0 deg have no velocity part"
    );
    for (cut, vradh_cut) in merged.cuts[2..].iter().zip(&vradh.cuts) {
        assert_eq!(
            cut.moments[&MomentType::Velocity],
            vradh_cut.moments[&MomentType::Velocity]
        );
    }
}

/// JMA Osaka: the N5 (reflectivity) member's four ladders repeat the low
/// tilts and the N6 (velocity) member's two ladders repeat them too, each
/// member numbering its own sweeps from 1. Merged, each N6 sweep lands on
/// the N5 repetition whose azimuth grid it shares (the GRIB2 start azimuth),
/// the two 0.3 deg velocity sweeps whose start azimuths match no
/// reflectivity sweep are skipped, and the ladder is renumbered 1..=26.
/// Merging the N6 member twice makes every velocity sweep collide.
#[test]
fn merge_jma_repeated_tilts_keep_repetition_velocity_and_renumber() {
    let expected = golden();
    let n5 = jma(&recast_radar_testdata::require_file!(
        "jma-n5-20191012-090000-rs47773"
    ));
    let n6 = jma(&recast_radar_testdata::require_file!(
        "jma-n6-20191012-090000-rs47773"
    ));
    assert_eq!(n5.cuts.len(), 26);
    assert_eq!(n6.cuts.len(), 13);
    assert_eq!(
        n6.cuts
            .iter()
            .map(|c| c.elevation_number)
            .collect::<Vec<_>>(),
        (1..=13).map(Some).collect::<Vec<_>>()
    );

    let (merged, report) = merge_radar_volumes(vec![n5.clone(), n6.clone()]).unwrap();
    assert_merge_matches(&merged, report, &expected["merges"]["jma_n5_n6"], "N5 + N6");
    assert_eq!(
        report,
        MergeReport {
            merged_moments: 11,
            skipped_geometry: 2,
            moment_collisions: 0,
        }
    );
    // Every velocity grid landed on the reflectivity cut with the same
    // azimuth grid, and repetition-2 velocity is on the repetition-2 cut.
    for n6_cut in &n6.cuts {
        let targets: Vec<&recast_radar_core::ElevationCut> = merged
            .cuts
            .iter()
            .filter(|cut| {
                cut.moments.get(&MomentType::Velocity).is_some_and(|grid| {
                    grids_identical(grid, &n6_cut.moments[&MomentType::Velocity])
                })
            })
            .collect();
        if targets.is_empty() {
            assert_eq!(
                n6_cut.elevation_deg, 0.3,
                "only the 0.3 deg velocity sweeps are skipped"
            );
            continue;
        }
        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0].radials[0].azimuth_deg,
            n6_cut.radials[0].azimuth_deg
        );
    }
    let five_degree: Vec<&recast_radar_core::ElevationCut> = merged
        .cuts
        .iter()
        .filter(|cut| cut.elevation_deg == 5.0)
        .collect();
    assert_eq!(five_degree.len(), 3);
    assert!(five_degree[0].moments.contains_key(&MomentType::Velocity));
    assert!(five_degree[1].moments.contains_key(&MomentType::Velocity));
    assert!(
        !five_degree[2].moments.contains_key(&MomentType::Velocity),
        "the long-range 800-gate 5.0 deg sweep has no Doppler twin"
    );
    assert_ne!(
        five_degree[0].radials[0].azimuth_deg,
        five_degree[1].radials[0].azimuth_deg
    );

    let (merged_twice, report) = merge_radar_volumes(vec![n5, n6.clone(), n6]).unwrap();
    assert_merge_matches(
        &merged_twice,
        report,
        &expected["merges"]["jma_n5_n6_n6"],
        "N5 + N6 + N6",
    );
    assert_eq!(
        report.moment_collisions, 11,
        "the repeated member collides on every landed sweep"
    );
    assert_eq!(report.skipped_geometry, 4);
    assert_eq!(merged_twice.cuts.len(), merged.cuts.len());
    for (twice, once) in merged_twice.cuts.iter().zip(&merged.cuts) {
        assert_eq!(twice.moments_available(), once.moments_available());
        for (name, grid) in &once.moments {
            assert!(
                grids_identical(&twice.moments[name], grid),
                "first part wins every collision"
            );
        }
    }
}
