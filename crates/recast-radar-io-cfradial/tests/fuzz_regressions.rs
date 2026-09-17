//! Fuzz regression inputs for the CfRadial decoder (`fuzz/`, target
//! `cfradial`), and the bug behind them reproduced on the real seed.
//!
//! `fuzz-cfradial-overlapping-sweep-ray-ranges` (testdata/fuzz/manifest.toml)
//! is a minimized libFuzzer mutation of the committed SMART-R2 Hurricane
//! Irene volume (`time` = 719, `range` = 1107, `sweep` = 2) whose header
//! claims 6,146 sweeps. `fixed_angle`, `sweep_start_ray_index` and
//! `sweep_end_ray_index` then read neighbouring header bytes as ray indices;
//! the decoder cast them with `as usize`, clamped the ends to the last ray,
//! and copied the full field into every one of the resulting overlapping
//! sweeps (2.44 GB peak from an 868 KB input). The other inputs here are the
//! same mutation kept within `MAX_SWEEPS_PER_VOLUME`, or single index edits
//! of the real Irene file.

use recast_radar_core::bounded_read::MAX_SWEEPS_PER_VOLUME;
use recast_radar_io_cfradial::{CfRadialError, read_cfradial1_volume};

const FUZZ_INPUT: &str = "fuzz-cfradial-overlapping-sweep-ray-ranges";
const IRENE: &str = "cfrad1-irene-sr2-20110827-120420-sur-sweeps01";

/// Data offsets of `sweep_start_ray_index(sweep)` and
/// `sweep_end_ray_index(sweep)` (big-endian `int`) in the Irene file, from
/// its netCDF header's `begin` fields.
const SWEEP_START_RAY_INDEX_AT: usize = 21_124;
const SWEEP_END_RAY_INDEX_AT: usize = 21_132;
/// netCDF default fill value for `int` (`NC_FILL_INT`).
const NC_FILL_INT: i32 = -2_147_483_647;

fn testdata_bytes(id: &str) -> Vec<u8> {
    let path = recast_radar_testdata::path(id).unwrap_or_else(|e| panic!("{e}"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn be_u32(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    u32::from_be_bytes(word)
}

/// Offset of the named dimension's length in the classic netCDF header
/// (`CDF\x01`, numrecs, dimension list tag and count, then per dimension:
/// name length, name padded to 4 bytes, length).
fn dimension_len_at(bytes: &[u8], wanted: &str) -> usize {
    let count = be_u32(bytes, 12) as usize;
    let mut at = 16;
    for _ in 0..count {
        let name_len = be_u32(bytes, at) as usize;
        let name = &bytes[at + 4..at + 4 + name_len];
        let len_at = at + 4 + name_len.div_ceil(4) * 4;
        if name == wanted.as_bytes() {
            return len_at;
        }
        at = len_at + 4;
    }
    panic!("dimension {wanted} not found");
}

fn set_sweep_dimension(bytes: &mut [u8], sweeps: usize) {
    let at = dimension_len_at(bytes, "sweep");
    let sweeps = u32::try_from(sweeps).unwrap_or_else(|e| panic!("{e}"));
    bytes[at..at + 4].copy_from_slice(&sweeps.to_be_bytes());
}

/// The Irene file with `sweep_start_ray_index[1]` replaced, after checking
/// the real sweep ranges (0..=359 and 360..=718) are where they should be.
fn irene_with_second_sweep_start(start: i32) -> Vec<u8> {
    let mut bytes = testdata_bytes(IRENE);
    let int_at = |bytes: &[u8], at: usize| be_u32(bytes, at) as i32;
    assert_eq!(
        [
            int_at(&bytes, SWEEP_START_RAY_INDEX_AT),
            int_at(&bytes, SWEEP_START_RAY_INDEX_AT + 4),
            int_at(&bytes, SWEEP_END_RAY_INDEX_AT),
            int_at(&bytes, SWEEP_END_RAY_INDEX_AT + 4),
        ],
        [0, 360, 359, 718]
    );
    let at = SWEEP_START_RAY_INDEX_AT + 4;
    bytes[at..at + 4].copy_from_slice(&start.to_be_bytes());
    bytes
}

fn assert_overlap_error(bytes: &[u8], what: &str) {
    match read_cfradial1_volume(bytes) {
        Err(CfRadialError::InvalidMessage { reason, .. }) => {
            assert!(reason.contains("overlap"), "{what}: {reason}");
        }
        Err(other) => panic!("{what}: expected an overlapping-sweeps error, got {other}"),
        Ok(volume) => panic!("{what}: decoded {} cuts", volume.sweeps.len()),
    }
}

#[test]
fn fuzz_input_is_rejected_by_the_sweep_limit() {
    let entry = recast_radar_testdata::entry(FUZZ_INPUT)
        .unwrap_or_else(|| panic!("manifest entry {FUZZ_INPUT} missing"));
    assert_eq!(entry.derived_from.as_deref(), Some(IRENE));
    let bytes = testdata_bytes(FUZZ_INPUT);
    assert_eq!(be_u32(&bytes, dimension_len_at(&bytes, "sweep")), 6146);
    match read_cfradial1_volume(&bytes) {
        Err(CfRadialError::LimitExceeded(reason)) => {
            assert!(reason.contains("6146 sweeps"), "{reason}");
        }
        Err(other) => panic!("expected the sweep limit error, got {other}"),
        Ok(_) => panic!("fuzz input decoded"),
    }
}

#[test]
fn fuzz_input_within_the_sweep_limit_is_rejected_as_overlapping() {
    // The same garbage ray indices, read for 1,024 sweeps: sweeps 1 and 16
    // both cover every ray.
    let mut bytes = testdata_bytes(FUZZ_INPUT);
    set_sweep_dimension(&mut bytes, MAX_SWEEPS_PER_VOLUME);
    assert_overlap_error(&bytes, "fuzz input with 1,024 sweeps");
}

#[test]
fn real_volume_claiming_the_maximum_sweep_count_is_rejected_as_overlapping() {
    // The fuzz finding's mechanism on the intact seed: with `sweep` = 1,024
    // the sweep variables run into the following header variables. The
    // decoder used to return 292 cuts, several repeating all 719 rays.
    let mut bytes = testdata_bytes(IRENE);
    set_sweep_dimension(&mut bytes, MAX_SWEEPS_PER_VOLUME);
    assert_overlap_error(&bytes, "Irene with 1,024 sweeps");
}

#[test]
fn real_sweeps_sharing_one_ray_are_rejected() {
    // Sweep 1 starting at ray 359 shares it with sweep 0 (0..=359).
    assert_overlap_error(&irene_with_second_sweep_start(359), "sweep 1 from ray 359");
}

#[test]
fn sweep_with_a_fill_value_ray_index_is_skipped() {
    // A fill value used to become ray 0 (`as usize`), so sweep 1 repeated
    // all 719 rays including sweep 0's. Now only sweep 1 is dropped.
    let volume = read_cfradial1_volume(&irene_with_second_sweep_start(NC_FILL_INT))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(volume.provenance.decode.skipped_message_count, 1);
    let rays: Vec<usize> = volume.sweeps.iter().map(|sweep| sweep.nrays()).collect();
    assert_eq!(rays, [360]);

    let unmodified = read_cfradial1_volume(&irene_with_second_sweep_start(360))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(unmodified.provenance.decode.skipped_message_count, 0);
    let rays: Vec<usize> = unmodified
        .sweeps
        .iter()
        .map(|sweep| sweep.nrays())
        .collect();
    assert_eq!(rays, [360, 359]);
}
