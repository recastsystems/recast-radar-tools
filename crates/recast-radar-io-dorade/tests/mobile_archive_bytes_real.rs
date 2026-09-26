//! `read_mobile_archive_from_bytes` on the committed deployment zip (three
//! head-trimmed NOXP sweepfiles of one volume run and three text members,
//! `dorade-noxp-20090610-003210-heads-zip`): the archive decoded from memory
//! gives the volumes `read_mobile_archive_from_path` gives for the file.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use recast_radar_io_dorade::{read_mobile_archive_from_bytes, read_mobile_archive_from_path};

#[test]
fn a_mobile_archive_in_memory_decodes_like_the_file() {
    let id = "dorade-noxp-20090610-003210-heads-zip";
    let path = recast_radar_testdata::path(id).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let decode_level2 = recast_radar_io_nexrad::read_volume_from_bytes;
    let from_path = read_mobile_archive_from_path(&path, decode_level2).unwrap();
    let from_bytes =
        read_mobile_archive_from_bytes(&bytes, "deployment.zip", decode_level2).unwrap();
    assert_eq!(from_bytes.len(), from_path.len());
    assert!(!from_bytes.is_empty());
    for (memory, file) in from_bytes.iter().zip(&from_path) {
        assert_eq!(memory.member_label, file.member_label);
        assert_eq!(memory.member_count, file.member_count);
        let mut expected = file.volume.clone();
        expected.provenance.source_path = Some(format!("deployment.zip::{}", file.member_label));
        assert!(
            memory.volume == expected,
            "{}: volumes differ",
            file.member_label
        );
    }
    // Not a zip archive: a typed error, not a panic.
    let sweep = recast_radar_testdata::bytes("dorade-noxp-20090610-003210-ppi-head6").unwrap();
    assert!(read_mobile_archive_from_bytes(&sweep, "sweep", decode_level2).is_err());
}
