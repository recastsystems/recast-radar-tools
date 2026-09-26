//! Fuzz regression inputs for the Level III decoder (`fuzz/`, target
//! `level3`).
//!
//! Each input is a libFuzzer mutation of a real Level III seed, minimized and
//! registered in `testdata/fuzz/manifest.toml` (tag `fuzz-target:level3`)
//! with the seed it derives from. The decoder must return an error or decode
//! it, not panic.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use recast_radar_io_level3::{RadarCodedMessage, decode_message};

fn fuzz_input(id: &str) -> Vec<u8> {
    let path = recast_radar_testdata::path(id).unwrap_or_else(|e| panic!("{e}"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Minimized from a mutation of the KFWS 1995 radar coded message: a Part A
/// centroid group whose motion field holds Latin-1 bytes above 0x7F was
/// sliced inside a character. The group is now kept as unparsed.
#[test]
fn rcm_centroid_with_non_ascii_motion_is_unparsed() {
    let entry = recast_radar_testdata::entry("fuzz-level3-rcm-centroid-non-ascii")
        .unwrap_or_else(|| panic!("manifest entry missing"));
    assert_eq!(
        entry.derived_from.as_deref(),
        Some("l3-fws-rcm-19950517-2310")
    );
    let bytes = fuzz_input(&entry.id);
    let text: String = bytes.iter().map(|&b| char::from(b)).collect();
    let rcm = RadarCodedMessage::parse(&text).unwrap();
    let a = rcm.part_a.expect("part A");
    assert!(a.centroids.is_empty());
    assert_eq!(rcm.unparsed.len(), 1, "{:?}", rcm.unparsed);
    // Not a product: the message decoder rejects it.
    assert!(decode_message(&bytes).is_err());
}
