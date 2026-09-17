//! Storm cell tracking across real consecutive volumes, checked against the
//! operational SCIT tracks of the Level III Storm Tracking Information product.
//!
//! Input: the four consecutive KDVN volumes of the 2020-08-10 Iowa derecho
//! (17:57:18, 18:04:01, 18:10:43 and 18:17:24Z; tag `sequence:kdvn-20200810`)
//! and their STI products (`l3-kdvn-20200810-*-nst`, committed). Expected
//! values: `testdata/golden/track/tracking.json`, written by
//! `tools/track_golden.py` (section `tracking`) with MetPy: per volume the SCIT
//! storm ids, positions (km east/north of the radar), maximum reflectivity and
//! forecast movement, plus each storm's distance to the nearest storm of the
//! previous volume.
//!
//! The tracker's cells come from `identify_storm_cells` (enhanced watershed),
//! which does not segment the derecho exactly like SCIT does, so every check
//! first pairs a SCIT storm with the tracker's cell within a few kilometres and
//! only then asks the tracker's identities and lineage to agree with SCIT.

// Test code panics on purpose: the workspace's unwrap/expect lints guard library code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use chrono::{DateTime, Utc};
use common::{array, as_f64, as_str, distance_km, golden, level2_with_time, timestamp};
use recast_radar_testdata::require_file;
use recast_radar_track::{StormCell, StormTrack, StormTracker, TIME_GATE_S, identify_storm_cells};
use serde_json::Value;

/// One volume of the sequence: its cells, and the tracker's tracks before and
/// after associating them.
struct Frame {
    time: DateTime<Utc>,
    cells: Vec<StormCell>,
    before: Vec<StormTrack>,
    after: Vec<StormTrack>,
}

/// Decode the four volumes, identify cells and run the tracker; `None` when a
/// volume is not available offline.
fn run_sequence(expected: &Value) -> Option<Vec<Frame>> {
    let mut tracker = StormTracker::default();
    let mut frames = Vec::new();
    for volume in array(&expected["volumes"]) {
        let path = match recast_radar_testdata::path(as_str(&volume["level2"])) {
            Ok(path) => path,
            Err(error) if error.is_offline() => {
                eprintln!("skipping: {error}");
                return None;
            }
            Err(error) => panic!("{error}"),
        };
        let (decoded, volume_time) = level2_with_time(&path);
        assert_eq!(volume_time, timestamp(&volume["level2_volume_time"]));
        let cells = identify_storm_cells(&decoded);
        assert!(
            cells.len() >= 20,
            "derecho volume with {} cells",
            cells.len()
        );
        let before = tracker.tracks.clone();
        tracker.associate(volume_time, &cells, None);
        frames.push(Frame {
            time: volume_time,
            cells,
            before,
            after: tracker.tracks.clone(),
        });
    }
    Some(frames)
}

fn position(storm: &Value) -> (f64, f64) {
    (as_f64(&storm["east_km"]), as_f64(&storm["north_km"]))
}

/// The cell nearest to a point, with its distance.
fn nearest_cell(cells: &[StormCell], point: (f64, f64)) -> (&StormCell, f64) {
    cells
        .iter()
        .map(|cell| (cell, distance_km((cell.east_km, cell.north_km), point)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .expect("cells")
}

/// The SCIT storm nearest to a point at one volume, with its distance.
fn nearest_storm(volume: &Value, point: (f64, f64)) -> (&Value, f64) {
    array(&volume["storms"])
        .iter()
        .map(|storm| (storm, distance_km(position(storm), point)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .expect("storms")
}

/// The track (in a snapshot) holding a fix at `time` for `cell`.
fn track_of<'a>(
    tracks: &'a [StormTrack],
    time: DateTime<Utc>,
    cell: &StormCell,
) -> Option<&'a StormTrack> {
    tracks.iter().find(|track| {
        track
            .history
            .iter()
            .any(|&(t, e, n)| t == time && e == cell.east_km && n == cell.north_km)
    })
}

fn fix_at(track: &StormTrack, time: DateTime<Utc>) -> Option<(f64, f64)> {
    track
        .history
        .iter()
        .find(|&&(t, ..)| t == time)
        .map(|&(_, e, n)| (e, n))
}

/// SCIT forecast movement (direction from, knots) as east/north m/s.
fn sti_motion(storm: &Value) -> Option<(f64, f64)> {
    let from = storm["forecast_from_deg"].as_f64()?;
    let knots = storm["forecast_kt"].as_f64()?;
    let toward = (from + 180.0).to_radians();
    let speed = knots * 0.514444;
    Some((speed * toward.sin(), speed * toward.cos()))
}

/// SCIT storms present in all four volumes that the watershed also identifies
/// within 3 km at every volume keep ONE tracker id from 17:57 to 18:17Z
/// (neighbours 12-30 km apart along the line moving 15-20 m/s: the QLCS regime),
/// distinct storms keep distinct ids, and the fitted motion after four fixes
/// agrees with SCIT's forecast movement.
#[test]
fn co_identified_storms_keep_one_track_id_and_scit_motion() {
    let golden = golden("tracking.json");
    let expected = &golden["kdvn"];
    let Some(frames) = run_sequence(expected) else {
        return;
    };
    let volumes = array(&expected["volumes"]);
    let persistent: Vec<&str> = array(&expected["persistent_ids"])
        .iter()
        .map(as_str)
        .collect();
    assert!(
        persistent.len() >= 8,
        "{} persistent SCIT storms",
        persistent.len()
    );
    let last = frames.last().expect("four frames");

    let mut matched: Vec<(&str, u32)> = Vec::new();
    for storm_id in &persistent {
        let mut ids = Vec::new();
        let mut positions = Vec::new();
        for (frame, volume) in frames.iter().zip(volumes) {
            let storm = array(&volume["storms"])
                .iter()
                .find(|s| as_str(&s["id"]) == *storm_id)
                .expect("persistent id present");
            let (cell, distance) = nearest_cell(&frame.cells, position(storm));
            if distance > 3.0 {
                ids.clear();
                break;
            }
            let track = track_of(&frame.after, frame.time, cell)
                .unwrap_or_else(|| panic!("cell {cell:?} at {} is on no track", frame.time));
            ids.push(track.id);
            positions.push((cell.east_km, cell.north_km));
        }
        if ids.is_empty() {
            continue;
        }
        assert!(
            ids.iter().all(|id| *id == ids[0]),
            "SCIT storm {storm_id} at {positions:?} changed tracker id: {ids:?}"
        );
        assert!(
            !matched.iter().any(|(_, id)| *id == ids[0]),
            "tracker id {} serves two SCIT storms ({storm_id} and {:?})",
            ids[0],
            matched.iter().find(|(_, id)| *id == ids[0])
        );
        matched.push((storm_id, ids[0]));

        let track = last
            .after
            .iter()
            .find(|t| t.id == ids[0])
            .expect("alive after the last volume");
        assert_eq!(
            track.history.len(),
            frames.len(),
            "{storm_id}: one fix per volume"
        );
        assert_eq!(track.missed, 0);
        assert!(track.merged_into.is_none());
        let (ve, vn) = track
            .fitted_motion
            .unwrap_or_else(|| panic!("{storm_id}: no fitted motion"));
        let storm = array(&volumes[volumes.len() - 1]["storms"])
            .iter()
            .find(|s| as_str(&s["id"]) == *storm_id)
            .expect("present");
        let (se, sn) = sti_motion(storm).expect("SCIT forecast movement");
        let error = (ve - se).hypot(vn - sn);
        assert!(
            error <= 6.0 && ve * se + vn * sn > 0.0,
            "{storm_id}: fitted motion ({ve:.1}, {vn:.1}) m/s vs SCIT ({se:.1}, {sn:.1}) m/s"
        );
    }
    assert!(
        matched.len() >= 2,
        "fewer than two SCIT storms co-identified at every volume: {matched:?}"
    );
    // The mean fitted motion of the live tracks is a plausible storm motion for
    // the derecho (eastward, tens of m/s at most).
    let mean = last
        .after
        .iter()
        .filter(|t| t.merged_into.is_none())
        .filter_map(|t| t.fitted_motion);
    let (count, sum_e) = mean.fold((0usize, 0.0), |(n, s), (e, _)| (n + 1, s + e));
    assert!(
        count > 0 && sum_e / count as f64 > 0.0,
        "mean motion should be eastward"
    );
    let mut tracker = StormTracker::default();
    assert!(tracker.mean_fitted_motion().is_none());
    tracker.clear();
    assert!(tracker.tracks.is_empty());
}

/// A track whose prediction lands inside a matched cell terminates into that
/// cell's track with a `merged_into` link. Every merge of the sequence joins two
/// tracker fragments of ONE SCIT storm: at the volume before the merge both
/// tracks' fixes lie within 8 km of the same SCIT storm, and at the merge volume
/// SCIT has exactly one storm within 8 km of the survivor.
#[test]
fn merge_terminates_the_loser_with_a_link() {
    let golden = golden("tracking.json");
    let expected = &golden["kdvn"];
    let Some(frames) = run_sequence(expected) else {
        return;
    };
    let volumes = array(&expected["volumes"]);
    let mut merges = 0usize;
    for k in 1..frames.len() {
        let frame = &frames[k];
        for loser in frame.after.iter().filter(|t| t.merged_into.is_some()) {
            merges += 1;
            let winner_id = loser.merged_into.expect("merged");
            let winner = frame
                .after
                .iter()
                .find(|t| t.id == winner_id)
                .unwrap_or_else(|| {
                    panic!("track {} merged into unknown track {winner_id}", loser.id)
                });
            assert!(winner.merged_into.is_none(), "winner is itself a tombstone");
            let survivor = fix_at(winner, frame.time).expect("winner matched at the merge volume");
            assert!(
                fix_at(loser, frame.time).is_none(),
                "loser gained a fix while merging"
            );
            let (_, previous_e, previous_n) = *loser.history.back().expect("history");
            let previous_time = loser.history.back().expect("history").0;
            assert_eq!(
                previous_time,
                frames[k - 1].time,
                "loser was matched at the previous volume"
            );
            let winner_previous =
                fix_at(winner, previous_time).expect("winner matched before the merge");
            // Both fragments belonged to one SCIT storm before the merge...
            let (storm_loser, d_loser) = nearest_storm(&volumes[k - 1], (previous_e, previous_n));
            let (storm_winner, d_winner) = nearest_storm(&volumes[k - 1], winner_previous);
            assert!(
                d_loser <= 8.0
                    && d_winner <= 8.0
                    && as_str(&storm_loser["id"]) == as_str(&storm_winner["id"]),
                "merge of track {} into {}: SCIT storms {} ({d_loser:.1} km) and {} ({d_winner:.1} km)",
                loser.id,
                winner.id,
                as_str(&storm_loser["id"]),
                as_str(&storm_winner["id"])
            );
            // ...and SCIT sees one storm where the survivor is.
            let near = array(&volumes[k]["storms"])
                .iter()
                .filter(|s| distance_km(position(s), survivor) <= 8.0)
                .count();
            assert_eq!(
                near, 1,
                "SCIT storms within 8 km of the survivor at {}",
                frame.time
            );
        }
        // Tombstones are dropped at the next association.
        if k + 1 < frames.len() {
            for loser in frame.after.iter().filter(|t| t.merged_into.is_some()) {
                assert!(!frames[k + 1].after.iter().any(|t| t.id == loser.id));
            }
        }
    }
    assert!(merges >= 1, "no merge in the derecho sequence");
}

/// An unmatched cell inside a track's forecast circle starts a child track that
/// records its parent and inherits the parent's motion. Every split of the
/// sequence buds from the parent's own SCIT storm: the child's first fix and the
/// parent's fix at that volume are within 8 km of the same SCIT storm.
#[test]
fn split_links_children_to_parent() {
    let golden = golden("tracking.json");
    let expected = &golden["kdvn"];
    let Some(frames) = run_sequence(expected) else {
        return;
    };
    let volumes = array(&expected["volumes"]);
    let mut splits = 0usize;
    for (k, frame) in frames.iter().enumerate() {
        for child in frame.after.iter().filter(|t| {
            t.parent_id.is_some() && t.history.len() == 1 && t.history[0].0 == frame.time
        }) {
            splits += 1;
            let parent_id = child.parent_id.expect("parent");
            let parent = frame
                .after
                .iter()
                .find(|t| t.id == parent_id)
                .unwrap_or_else(|| panic!("child {} names unknown parent {parent_id}", child.id));
            assert!(parent.merged_into.is_none());
            assert!(
                child.assumed_motion.is_some(),
                "child inherits a first-guess motion"
            );
            if parent.motion().is_some() {
                assert_eq!(
                    child.assumed_motion,
                    parent.motion(),
                    "child inherits the parent's motion"
                );
            }
            let (_, child_e, child_n) = child.history[0];
            let (_, parent_e, parent_n) = *parent.history.back().expect("parent history");
            let (storm_child, d_child) = nearest_storm(&volumes[k], (child_e, child_n));
            let (storm_parent, d_parent) = nearest_storm(&volumes[k], (parent_e, parent_n));
            assert!(
                d_child <= 8.0
                    && d_parent <= 8.0
                    && as_str(&storm_child["id"]) == as_str(&storm_parent["id"]),
                "split {} from {}: SCIT storms {} ({d_child:.1} km) and {} ({d_parent:.1} km)",
                child.id,
                parent.id,
                as_str(&storm_child["id"]),
                as_str(&storm_parent["id"])
            );
        }
    }
    assert!(splits >= 1, "no split in the derecho sequence");
    // First-volume tracks have no lineage.
    assert!(
        frames[0]
            .after
            .iter()
            .all(|t| t.parent_id.is_none() && t.merged_into.is_none())
    );
}

/// SCIT storms that are new in a volume and more than 20 km from every storm of
/// the previous volume (reaching them would take over 50 m/s) are also more than
/// 20 km from every tracker fix: the tracker starts a fresh track for such a
/// cell instead of letting a track jump to it (the speed gate), so the cell's
/// track has one fix and no parent.
#[test]
fn distant_new_storms_start_fresh_tracks() {
    let golden = golden("tracking.json");
    let expected = &golden["kdvn"];
    let Some(frames) = run_sequence(expected) else {
        return;
    };
    let volumes = array(&expected["volumes"]);
    let mut cases = 0usize;
    for k in 1..frames.len() {
        let frame = &frames[k];
        for storm in array(&volumes[k]["storms"]) {
            if !storm["new"].as_bool().unwrap_or(false)
                || as_f64(&storm["nearest_previous_km"]) < 20.0
            {
                continue;
            }
            let (cell, distance) = nearest_cell(&frame.cells, position(storm));
            if distance > 5.0 {
                continue;
            }
            let nearest_track = frame
                .before
                .iter()
                .filter_map(|t| t.last_fix())
                .map(|(_, e, n)| distance_km((e, n), (cell.east_km, cell.north_km)))
                .fold(f64::INFINITY, f64::min);
            if nearest_track < 20.0 {
                continue;
            }
            cases += 1;
            let track = track_of(&frame.after, frame.time, cell).expect("cell on a track");
            assert_eq!(
                track.history.len(),
                1,
                "SCIT storm {} ({distance:.1} km from cell {cell:?}, {nearest_track:.1} km from any track) continued track {}",
                as_str(&storm["id"]),
                track.id
            );
            assert!(track.parent_id.is_none() && track.fitted_motion.is_none());
            let elapsed = (frame.time - frames[k - 1].time).num_seconds() as f64;
            assert!(
                nearest_track / elapsed * 1000.0 > 30.0,
                "would need more than 30 m/s"
            );
        }
    }
    assert!(cases >= 1, "no distant new SCIT storm with a tracker cell");
}

/// A track that goes unmatched for a volume coasts (no fabricated fix) and is
/// reacquired under the same id. The sequence has such tracks, and at least one
/// is a SCIT storm that was present the whole time: its id at the volume before
/// the gap and at the reacquisition volume is the same.
#[test]
fn coast_and_reacquire_keeps_the_id() {
    let golden = golden("tracking.json");
    let expected = &golden["kdvn"];
    let Some(frames) = run_sequence(expected) else {
        return;
    };
    let volumes = array(&expected["volumes"]);
    let times: Vec<DateTime<Utc>> = frames.iter().map(|f| f.time).collect();
    let mut gaps = 0usize;
    let mut corroborated = Vec::new();
    for track in &frames.last().expect("frames").after {
        let fixes: Vec<usize> = track
            .history
            .iter()
            .map(|&(t, ..)| {
                times
                    .iter()
                    .position(|&x| x == t)
                    .expect("fix at a volume time")
            })
            .collect();
        for pair in fixes.windows(2) {
            let (before, after) = (pair[0], pair[1]);
            if after - before < 2 {
                continue;
            }
            gaps += 1;
            assert!(after - before - 1 <= 2, "coasted longer than two volumes");
            // No fix was fabricated for the gap volumes.
            for gap in before + 1..after {
                assert!(fix_at(track, times[gap]).is_none());
                let coasting = frames[gap]
                    .after
                    .iter()
                    .find(|t| t.id == track.id)
                    .expect("track alive while coasting");
                assert_eq!(coasting.missed, (gap - before) as u32);
                assert_eq!(
                    coasting.history.len(),
                    fixes.iter().filter(|&&f| f <= before).count(),
                    "history grew while coasting"
                );
            }
            let reacquired = frames[after]
                .after
                .iter()
                .find(|t| t.id == track.id)
                .expect("track alive at reacquisition");
            assert_eq!(reacquired.missed, 0, "missed count resets on reacquisition");
            let pre = fix_at(track, times[before]).expect("fix before the gap");
            let post = fix_at(track, times[after]).expect("fix after the gap");
            let (storm_pre, d_pre) = nearest_storm(&volumes[before], pre);
            let (storm_post, d_post) = nearest_storm(&volumes[after], post);
            if d_pre <= 5.0
                && d_post <= 5.0
                && as_str(&storm_pre["id"]) == as_str(&storm_post["id"])
            {
                corroborated.push((track.id, as_str(&storm_pre["id"]).to_owned(), before, after));
            }
        }
    }
    assert!(gaps >= 1, "no track coasted in the derecho sequence");
    assert!(
        !corroborated.is_empty(),
        "no coasted track matches a continuous SCIT storm"
    );
}

/// KTLX 2013-05-20 20:16Z followed by KTLX 2024-03-15 00:02Z (same radar, eleven
/// years apart, far beyond the 20 min outage gate): every track of the first
/// volume is dropped and every cell of the second starts a new one-fix track.
/// A repeated or out-of-order volume time is ignored.
#[test]
fn time_gate_resets_everything() {
    let golden = golden("tracking.json");
    let expected = &golden["time_gate"];
    let first = require_file!(as_str(&expected["first"]["entry"]));
    let second = require_file!(as_str(&expected["second"]["entry"]));
    let (first, first_time) = level2_with_time(&first);
    let (second, second_time) = level2_with_time(&second);
    assert_eq!(first_time, timestamp(&expected["first"]["volume_time"]));
    assert_eq!(second_time, timestamp(&expected["second"]["volume_time"]));
    assert_eq!(first.attrs.instrument_name, second.attrs.instrument_name);
    let gap = (second_time - first_time).num_seconds() as f64;
    assert!(gap > TIME_GATE_S);

    let cells_first = identify_storm_cells(&first);
    let cells_second = identify_storm_cells(&second);
    assert!(cells_first.len() >= 10 && cells_second.len() >= 10);
    let mut tracker = StormTracker::default();
    tracker.associate(first_time, &cells_first, None);
    assert_eq!(tracker.tracks.len(), cells_first.len());
    let old_ids: Vec<u32> = tracker.tracks.iter().map(|t| t.id).collect();

    // Same volume again: ignored.
    tracker.associate(first_time, &cells_first, None);
    assert_eq!(tracker.tracks.len(), cells_first.len());
    assert!(tracker.tracks.iter().all(|t| t.history.len() == 1));

    tracker.associate(second_time, &cells_second, None);
    assert_eq!(tracker.tracks.len(), cells_second.len());
    for track in &tracker.tracks {
        assert!(
            !old_ids.contains(&track.id),
            "old track {} survived the gap",
            track.id
        );
        assert_eq!(track.history.len(), 1);
        assert_eq!(track.history[0].0, second_time);
        assert!(track.fitted_motion.is_none() && track.parent_id.is_none());
    }
    // Out of order: ignored.
    let ids: Vec<u32> = tracker.tracks.iter().map(|t| t.id).collect();
    tracker.associate(first_time, &cells_first, None);
    assert_eq!(tracker.tracks.iter().map(|t| t.id).collect::<Vec<_>>(), ids);
}
