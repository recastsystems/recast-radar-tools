//! Resource limits against a real ODIM_H5 polar volume mutated to claim more
//! data than the documented caps allow (crate docs, `# Limits`). Every
//! mutation starts from the committed RMI Belgium Jabbeke PVOL bytes.

use recast_radar_core::bounded_read::MAX_GATES_PER_RADIAL;
use recast_radar_io_odim::{OdimError, decode_odim_h5_volume};

/// Superblock v0 with 8-byte offsets: the root group symbol-table entry
/// starts after the 24 fixed bytes and four addresses; its object header
/// address is the entry's second field.
const ROOT_ENTRY: usize = 24 + 4 * 8;

fn bejab() -> Vec<u8> {
    let path = recast_radar_testdata::path("odim-bejab-20190606-0000-pvol")
        .unwrap_or_else(|e| panic!("{e}"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn le_u64(bytes: &[u8], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(word)
}

/// Offsets of the two dimension sizes in every version-1, rank-2 dataspace
/// message that describes a 360-ray x 598-bin data plane.
fn plane_dataspace_dims(bytes: &[u8]) -> Vec<usize> {
    let mut dims = Vec::new();
    for at in 8..bytes.len().saturating_sub(16) {
        let header = &bytes[at - 8..at];
        if header[0] == 1
            && header[1] == 2
            && header[3..] == [0; 5]
            && le_u64(bytes, at) == 360
            && le_u64(bytes, at + 8) == 598
        {
            dims.push(at);
        }
    }
    dims
}

/// The bejab file with every 360 x 598 data plane claiming `rays` x `bins`.
fn bejab_with_plane_dims(rays: u64, bins: u64) -> Vec<u8> {
    let mut bytes = bejab();
    let dims = plane_dataspace_dims(&bytes);
    assert_eq!(dims.len(), 6, "six 360 x 598 sweeps in the real file");
    for at in dims {
        bytes[at..at + 8].copy_from_slice(&rays.to_le_bytes());
        bytes[at + 8..at + 16].copy_from_slice(&bins.to_le_bytes());
    }
    bytes
}

fn assert_limit_error(result: Result<recast_radar_core::RadarVolume, OdimError>, what: &str) {
    match result {
        Err(OdimError::LimitExceeded(reason)) => {
            assert!(reason.contains("limit"), "{what}: {reason}");
        }
        Err(other) => panic!("{what}: expected a limit error, got {other}"),
        Ok(_) => panic!("{what}: mutated file decoded"),
    }
}

#[test]
fn unmodified_real_volume_decodes_within_limits() {
    let volume = decode_odim_h5_volume(&bejab()).expect("real bejab decodes");
    assert_eq!(volume.cuts.len(), 11);
}

#[test]
fn root_object_header_claiming_too_many_messages_is_rejected() {
    let mut bytes = bejab();
    let root = le_u64(&bytes, ROOT_ENTRY + 8) as usize;
    assert_eq!(bytes[root], 1, "version-1 root object header");
    bytes[root + 2..root + 4].copy_from_slice(&5000u16.to_le_bytes());
    assert_limit_error(decode_odim_h5_volume(&bytes), "5,000 header messages");
}

#[test]
fn root_object_header_claiming_a_huge_message_block_is_rejected() {
    let mut bytes = bejab();
    let root = le_u64(&bytes, ROOT_ENTRY + 8) as usize;
    bytes[root + 8..root + 12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_limit_error(decode_odim_h5_volume(&bytes), "4 GiB header block");
}

#[test]
fn dataspace_dimension_beyond_the_limit_is_rejected() {
    let bytes = bejab_with_plane_dims(360, u64::MAX / 2);
    assert_limit_error(decode_odim_h5_volume(&bytes), "absurd bin count");
}

#[test]
fn dataset_claiming_more_bytes_than_the_limit_is_rejected() {
    // Each dimension is individually plausible; the product (36 GiB) is not.
    let bytes = bejab_with_plane_dims(360, 100_000_000);
    assert_limit_error(decode_odim_h5_volume(&bytes), "36 GiB data plane");
}

#[test]
fn sweep_claiming_more_bins_than_the_gate_limit_is_rejected() {
    let bytes = bejab_with_plane_dims(360, (MAX_GATES_PER_RADIAL + 1) as u64);
    assert_limit_error(decode_odim_h5_volume(&bytes), "16,385 bins per ray");
}

#[test]
fn sweep_claiming_more_rays_than_the_output_budget_is_rejected() {
    // 30 MiB of (mostly unwritten, zero-filled) plane data is within the
    // per-dataset cap, but 30 million radials exceed the volume budget.
    let bytes = bejab_with_plane_dims(30_000_000, 1);
    assert_limit_error(decode_odim_h5_volume(&bytes), "30 million rays");
}
