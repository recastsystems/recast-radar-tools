//! Fuzz regression inputs for the HDF5 reader (`fuzz/`, target `hdf5`).
//!
//! Each input is a libFuzzer mutation of a real HDF5 seed, registered in
//! `testdata/fuzz/manifest.toml` (tag `fuzz-target:hdf5`) with the seed it
//! derives from. The reader must return an error or a value, not panic.

use recast_radar_hdf5::{Error, H5File, ObjectKind, OpenOptions};

fn fuzz_input(id: &str) -> Vec<u8> {
    let path = recast_radar_testdata::path(id).unwrap_or_else(|e| panic!("{e}"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// A mutation of the superblock v3 fixture (18 bytes differ, read without
/// metadata checksums, as the harness does): a v2 B-tree chunk record's
/// scaled offset times the chunk dimension overflowed u64 in
/// `chunk_locations` ("attempt to multiply with overflow" with overflow
/// checks on, a wrapped offset without).
#[test]
fn chunk_offset_overflow_is_an_error() {
    let entry = recast_radar_testdata::entry("fuzz-hdf5-chunk-offset-overflow")
        .unwrap_or_else(|| panic!("manifest entry missing"));
    assert_eq!(
        entry.derived_from.as_deref(),
        Some("odim-dkrom-20260820-1130-pvol-h5latest-trim")
    );
    let bytes = fuzz_input(&entry.id);
    let options = OpenOptions::default().with_metadata_checksums(false);
    let file = H5File::open_with(&bytes, options).unwrap_or_else(|err| panic!("{err}"));
    let mut overflows = 0;
    for (path, object) in file.objects() {
        if object.kind() != ObjectKind::Dataset {
            continue;
        }
        match file.chunk_locations(path) {
            Err(Error::Invalid { reason, .. }) if reason.contains("chunk offset overflows") => {
                overflows += 1;
            }
            _ => {}
        }
        let _ = file.dataset(path);
    }
    assert_eq!(overflows, 1);
    // With checksums verified the mutation is a checksum error.
    assert!(matches!(H5File::open(&bytes), Err(Error::Checksum { .. })));
}
