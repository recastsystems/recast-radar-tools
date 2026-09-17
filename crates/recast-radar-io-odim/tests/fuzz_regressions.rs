//! Fuzz regression inputs for the ODIM_H5 decoder (`fuzz/`, target `odim`).
//!
//! Each input is a libFuzzer mutation of a real ODIM_H5 seed, minimized and
//! registered in `testdata/fuzz/manifest.toml` (tag `fuzz-target:odim`) with
//! the seed it derives from. The decoder must reject it with an error, not
//! panic.

use recast_radar_io_odim::{OdimError, decode_odim_h5_cartesian_max, read_odim_h5_volume};

fn fuzz_input(id: &str) -> Vec<u8> {
    let path = recast_radar_testdata::path(id).unwrap_or_else(|e| panic!("{e}"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Minimized from a mutation of the IMGW Ramza KDP max composite: the root
/// symbol table's local heap puts its data segment near `u64::MAX`, so the
/// data address plus a link-name offset overflowed in `heap_string`
/// ("attempt to add with overflow" with overflow checks on).
#[test]
fn local_heap_name_offset_overflow_is_an_error() {
    let entry = recast_radar_testdata::entry("fuzz-odim-hdf5-local-heap-name-offset-overflow")
        .unwrap_or_else(|| panic!("manifest entry missing"));
    assert_eq!(
        entry.derived_from.as_deref(),
        Some("odim-imgw-ram-20260711-0015-kdp-max")
    );
    let bytes = fuzz_input(&entry.id);
    let polar = read_odim_h5_volume(&bytes);
    let cartesian = decode_odim_h5_cartesian_max(&bytes);
    for (entry_point, result) in [("polar", polar.err()), ("cartesian", cartesian.err())] {
        match result {
            Some(OdimError::InvalidMessage { reason, .. }) => {
                assert_eq!(
                    reason, "HDF5 local heap name offset overflow",
                    "{entry_point}: unexpected rejection"
                );
            }
            Some(other) => panic!("{entry_point}: unexpected error: {other}"),
            None => panic!("{entry_point}: malformed file decoded"),
        }
    }
}
