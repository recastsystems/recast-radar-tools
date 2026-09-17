//! VAD wind profiles on real volumes.
//!
//! Expected values come from `testdata/golden/retrieve/vwp.json`
//! (`tools/retrieve_golden.py vwp`): the documented VAD (one median per radial
//! over the annulus, one sample per azimuth degree, first-harmonic fit, robust
//! trim, refit and QC) implemented in numpy on Py-ART's region-based dealiased
//! velocity, plus Py-ART's own `vad_browning` wind on the same tilt, and the
//! DORADE walker's geometry for the sector scan.

mod common;

use common::{
    array, as_f64, as_str, as_usize, assert_close, cfradial, dorade, golden, level2, map_cells,
};
use recast_radar_core::{MomentGrid, MomentType, RadarVolume, ScanMode};
use recast_radar_retrieve::{
    VwpConfig, VwpError, VwpLevelOutcome, VwpProfile, VwpQuality, VwpRejectedLevel,
    VwpRejectionReason, VwpWindLevel, compute_vwp,
};
use recast_radar_testdata::require_file;
use serde_json::Value;

/// Dealias every velocity cut with the crate's own region engine, as the display does.
fn dealiased_grids(volume: &RadarVolume) -> Vec<Option<MomentGrid>> {
    volume
        .cuts
        .iter()
        .map(|cut| {
            cut.moments
                .get(&MomentType::Velocity)
                .map(|velocity| recast_radar_correct::dealias_velocity_grid(cut, velocity))
        })
        .collect()
}

fn borrowed(grids: &[Option<MomentGrid>]) -> Vec<Option<&MomentGrid>> {
    grids.iter().map(Option::as_ref).collect()
}

fn config(case: &Value, min_m: f32, max_m: f32, step_m: f32) -> VwpConfig {
    let c = &case["config"];
    VwpConfig {
        min_height_m_agl: min_m,
        max_height_m_agl: max_m,
        height_step_m: step_m,
        min_slant_range_m: as_f64(&c["min_slant_m"]) as f32,
        max_slant_range_m: as_f64(&c["max_slant_m"]) as f32,
        annulus_half_width_m: as_f64(&c["annulus_m"]) as f32,
        max_height_mismatch_m: as_f64(&c["max_mismatch_m"]) as f32,
    }
}

fn retrieved(profile: &VwpProfile, index: usize) -> &VwpWindLevel {
    match &profile.levels[index].outcome {
        VwpLevelOutcome::Retrieved(wind) => wind,
        other => panic!("level {index}: expected a retrieved wind, got {other:?}"),
    }
}

/// Compare a profile with its golden case, level by level: same outcome, and for a
/// retrieved level on the same tilt as the reference, u/v within `wind_tolerance` of the
/// numpy reference. Py-ART's `vad_browning` (a per-ring VAD without the robust trim,
/// averaged over neighbouring rings) is held to a profile-wide statistic instead: the
/// median |delta u|, |delta v| over the levels at most `pyart_median` and the largest at
/// most `pyart_max`. Returns the number of levels retrieved on the reference tilt.
fn assert_profile_matches(
    profile: &VwpProfile,
    case: &Value,
    wind_tolerance: f64,
    pyart_median: f64,
    pyart_max: f64,
) -> usize {
    let levels = array(&case["levels"]);
    assert_eq!(profile.levels.len(), levels.len());
    let mut same_cut = 0usize;
    let mut pyart_deltas = Vec::new();
    for (index, expected) in levels.iter().enumerate() {
        let target = as_f64(&expected["target_m"]);
        let level = &profile.levels[index];
        assert_close(
            f64::from(level.target_height_m_agl),
            target,
            0.01,
            "target height",
        );
        let what = format!("{} m", target);
        match as_str(&expected["outcome"]) {
            "retrieved" => {
                let wind = retrieved(profile, index);
                let diagnostics = &expected["diagnostics"];
                let reference = &expected["wind"];
                // The two dealiasers can pick different tilts at a level; where they
                // agree, the heights and winds must agree closely.
                if wind.diagnostics.cut_index == as_usize(&diagnostics["cut_index"]) {
                    same_cut += 1;
                    assert_close(
                        f64::from(wind.height_m_agl),
                        as_f64(&diagnostics["height_m_agl"]),
                        0.01,
                        &format!("{what}: beam height"),
                    );
                    assert_close(
                        f64::from(wind.diagnostics.slant_range_m),
                        as_f64(&diagnostics["slant_range_m"]),
                        0.01,
                        &format!("{what}: slant range"),
                    );
                    assert_close(
                        f64::from(wind.u_mps),
                        as_f64(&reference["u"]),
                        wind_tolerance,
                        &format!("{what}: u"),
                    );
                    assert_close(
                        f64::from(wind.v_mps),
                        as_f64(&reference["v"]),
                        wind_tolerance,
                        &format!("{what}: v"),
                    );
                    assert_close(
                        f64::from(wind.speed_mps),
                        as_f64(&reference["speed"]),
                        wind_tolerance,
                        &format!("{what}: speed"),
                    );
                    let direction = f64::from(wind.direction_deg);
                    let delta =
                        (direction - as_f64(&reference["direction"]) + 540.0) % 360.0 - 180.0;
                    assert!(
                        delta.abs() <= 5.0 || f64::from(wind.speed_mps) < 5.0,
                        "{what}: direction {direction} vs {}",
                        reference["direction"]
                    );
                }
                let pyart = &expected["pyart_vad_browning"];
                pyart_deltas.push((f64::from(wind.u_mps) - as_f64(&pyart["u"])).abs());
                pyart_deltas.push((f64::from(wind.v_mps) - as_f64(&pyart["v"])).abs());
                assert_eq!(
                    wind.height_m_msl,
                    Some(wind.height_m_agl + as_f64(&case["radar_altitude_m"]) as f32)
                );
            }
            "rejected" => {
                let VwpLevelOutcome::Rejected(rejected) = &level.outcome else {
                    panic!("{what}: expected a rejection, got {:?}", level.outcome);
                };
                assert_eq!(
                    format!("{:?}", rejected.reason),
                    as_str(&expected["rejection"])
                );
            }
            "no_coverage" => assert_eq!(
                level.outcome,
                VwpLevelOutcome::Rejected(VwpRejectedLevel {
                    reason: VwpRejectionReason::NoBeamCoverage,
                    best_candidate: None,
                })
            ),
            other => panic!("unknown golden outcome {other}"),
        }
    }
    pyart_deltas.sort_by(f64::total_cmp);
    let median = pyart_deltas[pyart_deltas.len() / 2];
    let largest = *pyart_deltas.last().expect("retrieved levels");
    assert!(
        median <= pyart_median && largest <= pyart_max,
        "vs Py-ART vad_browning: median |delta| {median:.2}, largest {largest:.2} m/s"
    );
    same_cut
}

#[test]
fn blizzard_profile_recovers_the_reference_wind() {
    let path = require_file!("l2-kbox-20220129-150537");
    let volume = level2(&path);
    let golden = golden("retrieve/vwp.json");
    let case = &array(&golden["cases"])[0];
    assert_eq!(as_str(&case["id"]), "l2-kbox-20220129-150537");
    let grids = dealiased_grids(&volume);
    let profile = compute_vwp(
        &volume,
        &borrowed(&grids),
        config(case, 500.0, 4000.0, 500.0),
    )
    .expect("profile");
    assert_eq!(profile.site_id, "KBOX");
    assert_eq!(
        profile.velocity_cut_count,
        array(&case["velocity_sweeps"]).len()
    );
    let same_cut = assert_profile_matches(&profile, case, 1.0, 2.5, 6.0);
    assert!(same_cut >= 6, "same tilt chosen at {same_cut} of 8 levels");
    // Blizzard: a northeasterly (from-direction 190-260 deg, i.e. the wind blows toward
    // the NW quadrant here) 25-30 m/s low-level jet.
    let low = retrieved(&profile, 1);
    assert!(low.speed_mps > 25.0, "1 km wind {} m/s", low.speed_mps);
    assert_eq!(low.quality, VwpQuality::Good);
}

#[test]
fn stratiform_levels_sit_at_four_thirds_earth_beam_height() {
    let path = require_file!("l2-pahg-20250909-212549");
    let volume = level2(&path);
    let golden = golden("retrieve/vwp.json");
    let case = &array(&golden["cases"])[1];
    assert_eq!(as_str(&case["id"]), "l2-pahg-20250909-212549");
    let grids = dealiased_grids(&volume);
    let profile = compute_vwp(
        &volume,
        &borrowed(&grids),
        config(case, 500.0, 6000.0, 500.0),
    )
    .expect("profile");
    let same_cut = assert_profile_matches(&profile, case, 1.0, 2.5, 6.0);
    assert!(
        same_cut >= 10,
        "same tilt chosen at {same_cut} of 12 levels"
    );
    for (index, expected) in array(&case["levels"]).iter().enumerate() {
        let wind = retrieved(&profile, index);
        // The level height is the 4/3-Earth beam-centre height of the annulus centre
        // gate on the chosen tilt (numpy's Doviak and Zrnic eq. 2.28b), within the
        // configured mismatch of the requested height.
        let geometric = recast_radar_core::beam_height_above_radar_m(
            f64::from(wind.diagnostics.slant_range_m),
            f64::from(wind.diagnostics.elevation_deg),
        );
        assert_close(f64::from(wind.height_m_agl), geometric, 0.01, "beam height");
        assert!((f64::from(wind.height_m_agl) - as_f64(&expected["target_m"])).abs() <= 300.0);
        if wind.diagnostics.cut_index == as_usize(&expected["diagnostics"]["cut_index"]) {
            assert_close(
                f64::from(wind.diagnostics.elevation_deg),
                as_f64(&expected["diagnostics"]["elevation_deg"]),
                0.01,
                "tilt elevation",
            );
        }
    }
    // Every retrieved level of this stratiform volume carries a Nyquist velocity.
    assert!(profile.levels.iter().all(|level| matches!(
        &level.outcome,
        VwpLevelOutcome::Retrieved(w) if w.diagnostics.nyquist_sample_fraction == 1.0
    )));
}

#[test]
fn robust_refit_removes_convective_outliers() {
    // Natural case: dense convection at KILX, where 14-22% of the azimuth samples are
    // trimmed at 1.5-2.75 km and the refit still matches the reference and Py-ART.
    let path = require_file!("l2-kilx-20260418-013553");
    let volume = level2(&path);
    let golden = golden("retrieve/vwp.json");
    let case = &array(&golden["cases"])[2];
    assert_eq!(as_str(&case["id"]), "l2-kilx-20260418-013553");
    let grids = dealiased_grids(&volume);
    let profile = compute_vwp(
        &volume,
        &borrowed(&grids),
        config(case, 500.0, 6000.0, 250.0),
    )
    .expect("profile");
    let same_cut = assert_profile_matches(&profile, case, 1.5, 2.5, 6.0);
    assert!(
        same_cut >= 15,
        "same tilt chosen at {same_cut} of 23 levels"
    );
    let trimmed = profile
        .levels
        .iter()
        .filter(|level| {
            matches!(
                &level.outcome,
                VwpLevelOutcome::Retrieved(w) if w.diagnostics.outlier_fraction > 0.10
            )
        })
        .count();
    assert!(
        trimmed >= 4,
        "{trimmed} levels trimmed more than 10% of their samples"
    );

    // Controlled case: the KBOX blizzard tilt with +30 m/s added to every seventh
    // radial's dealiased velocity. The trim must drop those radials (about 1/7 of the
    // samples) and leave the wind within 0.5 m/s of the unperturbed retrieval.
    let path = require_file!("l2-kbox-20220129-150537");
    let volume = level2(&path);
    let case = &array(&golden["cases"])[0];
    let grids = dealiased_grids(&volume);
    let config = config(case, 1000.0, 1000.0, 500.0);
    let clean = compute_vwp(&volume, &borrowed(&grids), config).expect("profile");
    let clean = retrieved(&clean, 0).clone();
    let mut perturbed = grids;
    let mut spiked = 0usize;
    for (cut, grid) in volume.cuts.iter().zip(perturbed.iter_mut()) {
        let Some(grid) = grid else { continue };
        let rows: Vec<usize> = grid
            .radial_indices
            .iter()
            .enumerate()
            .filter(|(_, radial)| {
                (cut.radials[**radial].azimuth_deg.rem_euclid(360.0) as usize).is_multiple_of(7)
            })
            .map(|(row, _)| row)
            .collect();
        map_cells(grid, |row, _, value| {
            if rows.binary_search(&row).is_ok() {
                value + 30.0
            } else {
                value
            }
        });
        spiked += rows.len();
    }
    assert!(spiked > 0);
    let profile = compute_vwp(&volume, &borrowed(&perturbed), config).expect("profile");
    let wind = retrieved(&profile, 0);
    assert!(
        wind.diagnostics.outlier_fraction > 0.10,
        "outlier fraction {}",
        wind.diagnostics.outlier_fraction
    );
    assert_close(
        f64::from(wind.u_mps),
        f64::from(clean.u_mps),
        0.5,
        "u after trimming",
    );
    assert_close(
        f64::from(wind.v_mps),
        f64::from(clean.v_mps),
        0.5,
        "v after trimming",
    );
    assert!(wind.diagnostics.rms_mps.expect("rms") < 2.0);
}

#[test]
fn sector_scan_is_explicitly_rejected_for_azimuth_coverage() {
    let golden = golden("retrieve/vwp.json");
    let case = &golden["sector"];
    let path = require_file!(as_str(&case["id"]));
    let volume = dorade(&path);
    assert_eq!(volume.metadata.scan_mode, Some(ScanMode::Ppi));
    let cut = &volume.cuts[0];
    assert_eq!(cut.radials.len(), as_usize(&case["rays"]));
    let span = (as_f64(&case["azimuth_max_deg"]) - as_f64(&case["azimuth_min_deg"])).abs();
    assert!(span < 110.0, "sector spans {span} deg");
    let grids = dealiased_grids(&volume);
    let profile = compute_vwp(
        &volume,
        &borrowed(&grids),
        VwpConfig {
            min_height_m_agl: 250.0,
            max_height_m_agl: 1000.0,
            height_step_m: 250.0,
            min_slant_range_m: 5_000.0,
            max_slant_range_m: 150_000.0,
            annulus_half_width_m: 2_000.0,
            max_height_mismatch_m: 300.0,
        },
    )
    .expect("profile");
    let mut rejected_for_coverage = 0usize;
    for level in &profile.levels {
        let VwpLevelOutcome::Rejected(rejection) = &level.outcome else {
            panic!("a 100-degree sector must not yield a trusted vector: {level:?}");
        };
        if rejection.reason == VwpRejectionReason::InsufficientAzimuthCoverage {
            let diagnostics = rejection.best_candidate.as_ref().expect("candidate");
            assert!(diagnostics.azimuth_sectors < 8, "{diagnostics:?}");
            assert!(diagnostics.max_azimuth_gap_deg > 120.0, "{diagnostics:?}");
            rejected_for_coverage += 1;
        }
    }
    assert!(rejected_for_coverage >= 2, "{:?}", profile.levels);
}

#[test]
fn unresolved_second_harmonic_is_rejected_by_residual_qc() {
    // The KBOX blizzard tilt with a 10 m/s sin(2 az) harmonic added to its dealiased
    // velocity: the first-harmonic model cannot absorb it, the trim (clamped at 12 m/s)
    // keeps most samples, and the refit residual exceeds the 5.2 m/s ceiling.
    let path = require_file!("l2-kbox-20220129-150537");
    let volume = level2(&path);
    let golden = golden("retrieve/vwp.json");
    let case = &array(&golden["cases"])[0];
    let mut grids = dealiased_grids(&volume);
    let config = config(case, 1000.0, 1000.0, 500.0);
    let clean = compute_vwp(&volume, &borrowed(&grids), config).expect("profile");
    let clean_rms = retrieved(&clean, 0).diagnostics.rms_mps.expect("rms");
    assert!(clean_rms < 2.0);
    for (cut, grid) in volume.cuts.iter().zip(grids.iter_mut()) {
        let Some(grid) = grid else { continue };
        let azimuths: Vec<f32> = grid
            .radial_indices
            .iter()
            .map(|&radial| cut.radials[radial].azimuth_deg)
            .collect();
        map_cells(grid, |row, _, value| {
            value + 10.0 * (2.0 * azimuths[row].to_radians()).sin()
        });
    }
    let profile = compute_vwp(&volume, &borrowed(&grids), config).expect("profile");
    let VwpLevelOutcome::Rejected(rejected) = &profile.levels[0].outcome else {
        panic!(
            "large non-wind harmonic must be rejected: {:?}",
            profile.levels[0]
        );
    };
    assert_eq!(rejected.reason, VwpRejectionReason::ResidualTooLarge);
    let diagnostics = rejected.best_candidate.as_ref().expect("candidate");
    assert!(diagnostics.rms_mps.expect("rms") > 5.2, "{diagnostics:?}");
}

#[test]
fn missing_height_coverage_is_a_level_rejection_not_a_profile_error() {
    let golden = golden("retrieve/vwp.json");
    let case = &golden["single_tilt"];
    let path = require_file!(as_str(&case["id"]));
    let volume = level2(&path);
    let grids = dealiased_grids(&volume);
    assert_eq!(grids.iter().filter(|g| g.is_some()).count(), 1);
    // The trimmed file keeps one Doppler tilt at 0.48 deg, whose beam is 2.6 km above
    // the radar at the 150 km range limit: a 20 km level has no candidate at all.
    assert!(as_f64(&case["max_beam_height_m_at_150km"]) < 3_000.0);
    let profile = compute_vwp(
        &volume,
        &borrowed(&grids),
        VwpConfig {
            min_height_m_agl: 1_000.0,
            max_height_m_agl: 20_000.0,
            height_step_m: 19_000.0,
            max_height_mismatch_m: 300.0,
            ..VwpConfig::default()
        },
    )
    .expect("a profile, not an error");
    assert_eq!(profile.levels.len(), 2);
    // The 1 km level has a candidate on the kept tilt (480 radials, storms 225 km
    // away): the reference rejects it for its sample count, with diagnostics.
    let expected = &array(&case["levels"])[0];
    assert_eq!(as_str(&expected["outcome"]), "rejected");
    let VwpLevelOutcome::Rejected(low) = &profile.levels[0].outcome else {
        panic!("{:?}", profile.levels[0]);
    };
    assert_eq!(format!("{:?}", low.reason), as_str(&expected["rejection"]));
    let candidate = low
        .best_candidate
        .as_ref()
        .expect("a candidate on the kept tilt");
    let diagnostics = &expected["diagnostics"];
    assert_eq!(candidate.cut_index, as_usize(&diagnostics["cut_index"]));
    assert_eq!(candidate.cut_index, as_usize(&case["doppler_sweep"]));
    assert_eq!(
        candidate.samples_total,
        as_usize(&diagnostics["samples_total"])
    );
    assert_close(
        f64::from(candidate.height_m_agl),
        as_f64(&diagnostics["height_m_agl"]),
        0.01,
        "candidate height",
    );
    assert_eq!(as_str(&array(&case["levels"])[1]["outcome"]), "no_coverage");
    assert_eq!(
        profile.levels[1].outcome,
        VwpLevelOutcome::Rejected(VwpRejectedLevel {
            reason: VwpRejectionReason::NoBeamCoverage,
            best_candidate: None,
        })
    );
}

#[test]
fn input_contract_and_scan_mode_fail_loudly() {
    // A real RHI (DOW8, CfRadial classic): not a PPI volume scan.
    let path = require_file!("cfrad1-dow8-20211011-223602-rhi-trim3-classic");
    let rhi = cfradial(&path);
    assert_eq!(rhi.metadata.scan_mode, Some(ScanMode::Rhi));
    let grids = dealiased_grids(&rhi);
    assert!(grids.iter().any(Option::is_some));
    assert_eq!(
        compute_vwp(&rhi, &borrowed(&grids), VwpConfig::default()),
        Err(VwpError::UnsupportedScanMode(ScanMode::Rhi))
    );

    // A real PPI volume with the wrong number of grids, and an invalid configuration.
    let path = require_file!("l2-ktlx-20240315-000217-trim");
    let volume = level2(&path);
    let grids = dealiased_grids(&volume);
    assert_eq!(
        compute_vwp(&volume, &[], VwpConfig::default()),
        Err(VwpError::GridCountMismatch {
            expected: volume.cuts.len(),
            actual: 0,
        })
    );
    assert_eq!(
        compute_vwp(&volume, &borrowed(&grids[..1]), VwpConfig::default()),
        Err(VwpError::GridCountMismatch {
            expected: volume.cuts.len(),
            actual: 1,
        })
    );
    let too_many_levels = VwpConfig {
        min_height_m_agl: 0.0,
        max_height_m_agl: 20_000.0,
        height_step_m: 1.0,
        ..VwpConfig::default()
    };
    assert_eq!(
        compute_vwp(&volume, &borrowed(&grids), too_many_levels),
        Err(VwpError::InvalidConfig("too many requested height levels"))
    );
    // No velocity grid supplied for any cut.
    let none: Vec<Option<&MomentGrid>> = vec![None; volume.cuts.len()];
    assert_eq!(
        compute_vwp(&volume, &none, VwpConfig::default()),
        Err(VwpError::NoVelocityGrids)
    );
}
