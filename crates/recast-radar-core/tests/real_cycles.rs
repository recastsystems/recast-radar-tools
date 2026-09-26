//! `scan_cycles` and `split_scan_cycles` on real files.
//!
//! Expected cycles: `testdata/golden/core/model.json` (`scan_cycles`), written
//! by `tools/core_golden.py`: a reference implementation of the documented
//! cycle rules over the sweep times and geometry of h5py (ODIM) and a GRIB2
//! section walker (JMA), never from this workspace's readers.

// A panic is how a test fails (clippy.toml), in helpers too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{array, as_f64, as_str, as_usize, golden, jma, odim, time};
use recast_radar_core::{
    CycleBreak, CycleTracker, FieldName, ScanCycle, Volume, collection_order, floor_to_second,
    merge_volumes, scan_cycles, split_scan_cycles,
};
use serde_json::Value;

/// The cycles of `volume` against the reference: the sweeps of each cycle in
/// collection order, what begins it, and its first ray's time.
fn assert_cycles(volume: &Volume, cycles: &[ScanCycle], expected: &Value, what: &str) {
    let want = array(expected);
    assert_eq!(cycles.len(), want.len(), "{what}: cycle count");
    for (index, (cycle, want)) in cycles.iter().zip(want).enumerate() {
        let sweeps: Vec<usize> = array(&want["sweeps"]).iter().map(as_usize).collect();
        assert_eq!(cycle.sweeps, sweeps, "{what}: cycle {index} sweeps");
        let first = cycle
            .sweeps
            .iter()
            .flat_map(|sweep| volume.sweeps[*sweep].rays.time_s.iter().copied())
            .fold(f64::INFINITY, f64::min);
        // The reference's times are whole seconds (ODIM `starttime`, JMA
        // offsets); ODIM rays carry `how/startazT` to the microsecond.
        assert_eq!(
            floor_to_second(volume.instant(first).unwrap()),
            time(as_str(&want["start"])),
            "{what}: cycle {index} first ray"
        );
        let begins = &want["begins"];
        match (&cycle.starts_with, begins["kind"].as_str()) {
            (None, None) => assert!(begins.is_null(), "{what}: cycle {index}"),
            (
                Some(CycleBreak::RepeatedCut {
                    sweep,
                    earlier,
                    seconds_apart,
                }),
                Some("repeated_cut"),
            ) => {
                assert_eq!(*sweep, as_usize(&begins["sweep"]), "{what}: cycle {index}");
                assert_eq!(
                    *earlier,
                    as_usize(&begins["earlier"]),
                    "{what}: cycle {index}"
                );
                let seconds = seconds_apart.unwrap();
                assert!(
                    (seconds - as_f64(&begins["seconds_apart"])).abs() < 1.0,
                    "{what}: cycle {index}: {seconds} s apart"
                );
            }
            (
                Some(CycleBreak::Pause {
                    sweep,
                    previous,
                    seconds,
                }),
                Some("pause"),
            ) => {
                assert_eq!(*sweep, as_usize(&begins["sweep"]), "{what}: cycle {index}");
                assert_eq!(
                    *previous,
                    as_usize(&begins["previous"]),
                    "{what}: cycle {index}"
                );
                assert!(
                    (seconds - as_f64(&begins["seconds"])).abs() < 1.0,
                    "{what}: cycle {index}: {seconds} s pause"
                );
            }
            (got, want) => panic!("{what}: cycle {index} begins with {got:?}, want {want:?}"),
        }
    }
}

/// JMA's 10-minute tars hold two 5-minute cycles: each cut of the first
/// cycle (the same elevation, gates and moment) is collected again 281 s
/// (Takayasu 2019) or 284 s (Okinawa 2026) later, and the second cycle
/// begins with the first cut collected again. Within one cycle the low
/// angles are collected twice on other gates (a long-range surveillance cut
/// and a Doppler cut), which does not begin a cycle.
#[test]
fn jma_ten_minute_tars_hold_two_cycles() {
    let expected = golden();
    for (key, id) in [
        ("jma_taka_n5", "jma-n5-20191012-090000-rs47773"),
        ("jma_taka_n6", "jma-n6-20191012-090000-rs47773"),
        ("jma_itok_n5", "jma-n5-20260924-210000-rs47937"),
        ("jma_itok_n6", "jma-n6-20260924-210000-rs47937"),
    ] {
        let volume = jma(&recast_radar_testdata::require_file!(id));
        let cycles = scan_cycles(&volume);
        assert_cycles(&volume, &cycles, &expected["scan_cycles"][key], key);
        assert_eq!(cycles.len(), 2, "{key}");
        assert!(
            matches!(cycles[1].starts_with, Some(CycleBreak::RepeatedCut { .. })),
            "{key}"
        );
        let message = cycles[1].starts_with.as_ref().unwrap().to_string();
        assert!(message.contains("collects the cut of sweep"), "{message}");
    }
}

/// ODIM polar volumes of one scan are one cycle; Hurum's 14:46 velocity file
/// carries a 90 deg sweep collected at 14:38:53, 442 s before its other
/// sweeps began: a cycle of its own.
#[test]
fn odim_files_hold_one_cycle_except_a_sweep_of_the_scan_before() {
    let expected = golden();
    for (key, id) in [
        ("odim_bejab_dbzh", "odim-bejab-20260612-1450-dbzh"),
        ("odim_bejab_vrad", "odim-bejab-20260612-1450-vrad"),
        ("odim_nohur_dbzh", "odim-nohur-20260612-1445-dbzh"),
        ("odim_nohur_vradh", "odim-nohur-20260612-1446-vradh"),
    ] {
        let volume = odim(&recast_radar_testdata::require_file!(id));
        let cycles = scan_cycles(&volume);
        assert_cycles(&volume, &cycles, &expected["scan_cycles"][key], key);
    }
}

/// Hurum's three parts merged: the velocity file's 90 deg sweep of the scan
/// before is kept as a sweep of its own (`merge_volumes` matches sweeps by
/// collection time), and `scan_cycles` puts it in a cycle of its own, before
/// the scan the other ten sweeps hold.
#[test]
fn merged_parts_keep_the_sweep_of_the_scan_before_in_its_own_cycle() {
    let dbzh = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1445-dbzh"
    ));
    let vradh = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1446-vradh"
    ));
    let th = odim(&recast_radar_testdata::require_file!(
        "odim-nohur-20260612-1445-th"
    ));
    let (merged, _) = merge_volumes(vec![dbzh, vradh, th]).unwrap();
    let cycles = scan_cycles(&merged);
    assert_eq!(cycles.len(), 2);
    assert_eq!(cycles[0].sweeps, vec![10]);
    let earlier = &merged.sweeps[10];
    assert_eq!(earlier.fixed_angle_deg, 90.0);
    assert!(earlier.field(&FieldName::Vradh).is_some());
    assert!(earlier.field(&FieldName::Dbzh).is_none());
    assert_eq!(cycles[1].sweeps.len(), 10);
    assert!(matches!(
        cycles[1].starts_with,
        Some(CycleBreak::Pause {
            sweep: 0,
            previous: 10,
            ..
        })
    ));

    let parts = split_scan_cycles(merged);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].sweeps.len(), 1);
    assert_eq!(parts[1].sweeps.len(), 10);
    assert!(
        parts[1]
            .sweeps
            .iter()
            .all(|sweep| sweep.field(&FieldName::Dbzh).is_some())
    );
}

/// Okinawa's 2026 reflectivity tar split into its two cycles: every sweep in
/// exactly one, in the order collected, numbered from 0, each part sealing,
/// its time reference at its first ray and its coverage from its first to
/// its last ray; each part is one cycle.
#[test]
fn split_scan_cycles_makes_one_volume_per_cycle() {
    let expected = golden();
    let want = array(&expected["scan_cycles"]["jma_itok_n5"]);
    let volume = jma(&recast_radar_testdata::require_file!(
        "jma-n5-20260924-210000-rs47937"
    ));
    let absolute = |volume: &Volume, sweep: usize| volume.ray_time(sweep, 0).unwrap();
    let parts = split_scan_cycles(volume.clone());
    assert_eq!(parts.len(), 2);
    let mut seen = 0;
    for (index, (part, want)) in parts.iter().zip(want).enumerate() {
        let sweeps: Vec<usize> = array(&want["sweeps"]).iter().map(as_usize).collect();
        assert_eq!(part.sweeps.len(), sweeps.len());
        for (number, (sweep, source)) in part.sweeps.iter().zip(&sweeps).enumerate() {
            assert_eq!(sweep.sweep_number as usize, number);
            assert_eq!(sweep.fields, volume.sweeps[*source].fields, "part {index}");
            assert_eq!(absolute(part, number), absolute(&volume, *source));
            seen += 1;
        }
        let start = time(as_str(&want["start"]));
        assert_eq!(part.time_reference, start, "part {index}");
        let coverage = part.time_coverage.unwrap();
        assert_eq!(coverage.start, start);
        assert_eq!(Some(coverage), part.ray_time_extent());
        assert_eq!(
            collection_order(part),
            (0..part.sweeps.len()).collect::<Vec<_>>()
        );
        assert_eq!(scan_cycles(part).len(), 1, "part {index}");
        let mut sealed = part.clone();
        assert_eq!(sealed.seal(), Ok(()), "part {index}");
    }
    assert_eq!(seen, volume.sweeps.len());
}

/// A NEXRAD Level II volume with SAILS or MESO-SAILS rescans of its lowest
/// cut collects that cut more than once in one volume scan: still one
/// cycle. The same volume twice (its second copy an hour later) is two: the
/// second begins a volume scan (radial status 3).
#[test]
fn level2_supplemental_cuts_stay_in_one_cycle() {
    // KOAX 2014 and KEWX 2016: VCP 212 with one SAILS split cut; KDVN 2020:
    // MESO-SAILS, two (the manifest tags).
    for id in [
        "l2-koax-20140616-205305",
        "l2-kewx-20160413-022531",
        "l2-kdvn-20200810-175718",
        "l2-ktlx-20240315-000217-trim",
    ] {
        let Ok(path) = recast_radar_testdata::path(id) else {
            eprintln!("{id} unavailable; skipping");
            continue;
        };
        let volume = common::level2(&path);
        let cycles = scan_cycles(&volume);
        assert_eq!(cycles.len(), 1, "{id}: {:?}", cycles.get(1));
        assert_eq!(cycles[0].sweeps.len(), volume.sweeps.len(), "{id}");
    }

    let volume = common::level2(&recast_radar_testdata::require_file!(
        "l2-ktlx-20240315-000217-trim"
    ));
    let mut twice = volume.clone();
    for sweep in &volume.sweeps {
        let mut later = sweep.clone();
        later
            .rays
            .time_s
            .iter_mut()
            .for_each(|time| *time += 3600.0);
        later.sweep_number = u32::try_from(twice.sweeps.len()).unwrap();
        twice.sweeps.push(later);
    }
    let cycles = scan_cycles(&twice);
    assert_eq!(cycles.len(), 2);
    assert_eq!(
        cycles[1].starts_with,
        Some(CycleBreak::VolumeStart {
            sweep: volume.sweeps.len()
        })
    );

    // A tracker fed sweep by sweep agrees.
    let mut tracker = CycleTracker::new();
    for (index, _) in twice.sweeps.iter().enumerate() {
        let begins = tracker.check(&twice, index, index);
        assert_eq!(
            begins.is_some(),
            index == volume.sweeps.len(),
            "sweep {index}"
        );
        tracker.add(&twice, index, index);
    }
}
