//! Resource limits against a real CfRadial 1.3 file mutated to claim more
//! data than the documented caps allow (crate docs, `# Limits`). Every
//! mutation starts from the committed SMART-R2 Hurricane Irene volume
//! (dimensions `time` = 719, `range` = 1107, `sweep` = 2).

use recast_radar_core::bounded_read::{MAX_GATES_PER_RADIAL, MAX_SWEEPS_PER_VOLUME};
use recast_radar_io_cfradial::{CfRadialError, decode_cfradial1_volume};

fn irene() -> Vec<u8> {
    let path = recast_radar_testdata::path("cfrad1-irene-sr2-20110827-120420-sur-sweeps01")
        .unwrap_or_else(|e| panic!("{e}"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The Irene file with the length of the named dimension overwritten in the
/// classic netCDF header (`CDF\x01`, numrecs, dimension list tag and count,
/// then per dimension: name length, name padded to 4 bytes, length).
fn irene_with_dimension_len(wanted: &str, len: u32) -> Vec<u8> {
    let mut bytes = irene();
    let be_u32 = |bytes: &[u8], at: usize| {
        u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
    };
    let count = be_u32(&bytes, 12);
    let mut at = 16;
    for _ in 0..count {
        let name_len = be_u32(&bytes, at);
        let name = &bytes[at + 4..at + 4 + name_len];
        let len_at = at + 4 + name_len.div_ceil(4) * 4;
        if name == wanted.as_bytes() {
            bytes[len_at..len_at + 4].copy_from_slice(&len.to_be_bytes());
            return bytes;
        }
        at = len_at + 4;
    }
    panic!("dimension {wanted} not found");
}

fn assert_limit_error(bytes: &[u8], what: &str) {
    match decode_cfradial1_volume(bytes) {
        Err(CfRadialError::LimitExceeded(reason)) => {
            assert!(reason.contains("limit"), "{what}: {reason}");
        }
        Err(other) => panic!("{what}: expected a limit error, got {other}"),
        Ok(_) => panic!("{what}: mutated file decoded"),
    }
}

#[test]
fn unmodified_real_volume_decodes_within_limits() {
    let bytes = irene_with_dimension_len("range", 1107); // identity edit
    assert_eq!(bytes, irene());
    let volume = decode_cfradial1_volume(&bytes).expect("real Irene volume decodes");
    assert_eq!(volume.cuts.len(), 2);
}

#[test]
fn range_dimension_beyond_the_gate_limit_is_rejected() {
    let bytes = irene_with_dimension_len("range", (MAX_GATES_PER_RADIAL + 1) as u32);
    assert_limit_error(&bytes, "16,385 gates per ray");
}

#[test]
fn dimension_longer_than_the_netcdf_limit_is_rejected() {
    let bytes = irene_with_dimension_len("range", 200_000_000);
    assert_limit_error(&bytes, "200 million gates");
}

#[test]
fn time_dimension_claiming_a_variable_beyond_the_array_limit_is_rejected() {
    // 100 million rays: each float per-ray variable becomes a 400 MiB slab.
    let bytes = irene_with_dimension_len("time", 100_000_000);
    assert_limit_error(&bytes, "100 million rays");
}

#[test]
fn sweep_dimension_beyond_the_sweep_limit_is_rejected() {
    let bytes = irene_with_dimension_len("sweep", (MAX_SWEEPS_PER_VOLUME + 1) as u32);
    assert_limit_error(&bytes, "1,025 sweeps");
}

/// The ARM X-SAPR classic copy stores `time` as the record (unlimited)
/// dimension, so its length is the header's `numrecs` (40).
fn xsapr_with_numrecs(numrecs: u32) -> Vec<u8> {
    let path = recast_radar_testdata::path("cfrad1-xsapr-sgp-20110520-ppi-classic")
        .unwrap_or_else(|e| panic!("{e}"));
    let mut bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert_eq!(
        u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        40
    );
    bytes[4..8].copy_from_slice(&numrecs.to_be_bytes());
    bytes
}

#[test]
fn record_count_beyond_the_netcdf_limit_is_rejected() {
    assert_limit_error(&xsapr_with_numrecs(200_000_000), "200 million records");
}

#[test]
fn record_count_the_file_cannot_back_fails_before_reserving() {
    // 50 million records of the float azimuth variable fit the 256 MiB array
    // cap, but the 13.6 KiB file holds 40. The reader checks the whole record
    // range against the file before reserving the 200 MiB array, so the
    // decode fails as truncated.
    match decode_cfradial1_volume(&xsapr_with_numrecs(50_000_000)) {
        Err(CfRadialError::Truncated { .. }) => {}
        Err(other) => panic!("expected a truncation error, got {other}"),
        Ok(_) => panic!("50 million claimed records decoded"),
    }
}
