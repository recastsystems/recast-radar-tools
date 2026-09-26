//! Structural checks on real HDF5 bytes: the signature sniffer, cycle and
//! checksum rejection on mutated copies of real files, and no panic on any
//! truncation or byte flip of a real file.
//!
//! Byte offsets of the RMI Jabbeke PVOL (`odim-bejab-20190606-0000-pvol`,
//! superblock v0, 8-byte offsets): tools/golden_io_formats.py, section
//! `odim`, key `bejab_hdf5` (h5py `h5o.get_info` addresses and chunk info,
//! and a version-1 object header / B-tree reader written from the HDF5
//! specification).

use recast_radar_hdf5::{Error, H5File, OpenOptions, looks_like_hdf5_bytes};

const BEJAB: &str = "odim-bejab-20190606-0000-pvol";
const XSAPR_NETCDF4: &str = "cfrad1-xsapr-sgp-20110520-ppi-netcdf4";
const H5LATEST: &str = "odim-dkrom-20260820-1130-pvol-h5latest-trim";

fn corpus(id: &str) -> Vec<u8> {
    recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{err}"))
}

fn open_error(bytes: &[u8]) -> Error {
    match H5File::open(bytes) {
        Ok(_) => panic!("mutated file opened"),
        Err(err) => err,
    }
}

#[test]
fn magic_sniffer_matches_signature_only() {
    // HDF5 format specification: the signature is 89 48 44 46 0d 0a 1a 0a
    // (golden signatures.bejab_first8).
    let bejab = corpus(BEJAB);
    assert_eq!(bejab[..8], [0x89, 0x48, 0x44, 0x46, 0x0d, 0x0a, 0x1a, 0x0a]);
    assert!(looks_like_hdf5_bytes(&bejab));
    assert!(looks_like_hdf5_bytes(&bejab[..8]));
    assert!(!looks_like_hdf5_bytes(&bejab[..7]));
    assert!(looks_like_hdf5_bytes(&corpus(XSAPR_NETCDF4)));
    assert!(!looks_like_hdf5_bytes(&corpus(
        "cfrad1-xsapr-sgp-20110520-ppi-classic"
    )));
    assert!(!looks_like_hdf5_bytes(&corpus(
        "l2-ktlx-20240315-000217-trim"
    )));
    // A user block moves the signature (derivation of the h5latest fixture:
    // 512 bytes); the sniffer and `open` both look at 0, 512, 1024, ...
    let latest = corpus(H5LATEST);
    assert_ne!(latest[..8], bejab[..8]);
    assert_eq!(latest[512..520], bejab[..8]);
    assert!(looks_like_hdf5_bytes(&latest));
    assert!(looks_like_hdf5_bytes(&latest[512..]));
    assert!(!looks_like_hdf5_bytes(&latest[..519]));
    assert!(H5File::open(&latest).is_ok());
    // Only power-of-two offsets from 512 count: shifted by 256 bytes, the
    // same file is not HDF5.
    let mut shifted = vec![0u8; 256];
    shifted.extend_from_slice(&bejab);
    assert!(!looks_like_hdf5_bytes(&shifted));
}

/// Root group object header at 96 (h5py): its first message block starts at
/// 112 and holds the continuation message whose body (offset, length) sits
/// at 120 and points at 800.
#[test]
fn v1_object_header_rejects_continuation_cycle() {
    let mut bytes = corpus(BEJAB);
    assert_eq!((bytes[8], bytes[13], bytes[14]), (0, 8, 8));
    assert_eq!(u64::from_le_bytes(bytes[120..128].try_into().unwrap()), 800);
    bytes[120..128].copy_from_slice(&112u64.to_le_bytes());
    let err = open_error(&bytes);
    assert!(err.to_string().contains("cycle"), "{err}");
}

/// Root symbol-table B-tree node at 136 (TREE, type 0, level 0, 3
/// entries), first child pointer at 168; the chunk B-tree of
/// `dataset1/data1/data` at 3440 (type 1, level 0, 1 entry), first child
/// pointer at 3496 = h5py chunk byte offset 6112, stored size 103544.
#[test]
fn btree_walks_reject_self_references() {
    let original = corpus(BEJAB);
    let file = H5File::open(&original).expect("real file opens");
    assert_eq!(file.child_names("/").len(), 14, "h5py: 14 root children");
    let chunks = file
        .chunk_locations("/dataset1/data1/data")
        .expect("chunks");
    assert_eq!(chunks.len(), 1);
    assert_eq!(
        (chunks[0].file_offset, chunks[0].stored_size),
        (6112, 103_544)
    );

    // Group node: raise it to level 1 and point its first child at itself.
    let mut group = original.clone();
    assert_eq!(group[136..142], [b'T', b'R', b'E', b'E', 0, 0]);
    group[141] = 1;
    group[168..176].copy_from_slice(&136u64.to_le_bytes());
    let err = open_error(&group);
    assert!(err.to_string().contains("cycle"), "{err}");

    // Chunk node: the same edit on the dataset's chunk B-tree.
    let mut chunk = original;
    assert_eq!(chunk[3440..3446], [b'T', b'R', b'E', b'E', 1, 0]);
    chunk[3445] = 1;
    chunk[3496..3504].copy_from_slice(&3440u64.to_le_bytes());
    let file = H5File::open(&chunk).expect("groups still open");
    let err = file
        .dataset("/dataset1/data1/data")
        .expect_err("chunk B-tree cycle must fail");
    assert!(err.to_string().contains("cycle"), "{err}");
}

/// Version 2 object headers, fractal heaps and v2 B-trees carry lookup3
/// checksums: flipping one byte inside the netCDF-4 root group's header
/// (h5py address 48) fails with a checksum error, not a misread.
#[test]
fn v2_header_checksum_mismatch_is_rejected() {
    let mut bytes = corpus(XSAPR_NETCDF4);
    assert_eq!(bytes[8], 2, "superblock v2");
    assert_eq!(&bytes[48..52], b"OHDR");
    bytes[60] ^= 0x01;
    let err = open_error(&bytes);
    assert!(
        matches!(err, Error::Checksum { .. }),
        "expected a checksum error, got {err}"
    );
}

/// Every prefix of real files (cut at a stride that visits every structure
/// kind) is an error or a successful open, never a panic, and every dataset
/// read on a successful open returns without panicking.
#[test]
fn truncated_real_files_never_panic() {
    for id in [BEJAB, XSAPR_NETCDF4, H5LATEST] {
        let bytes = corpus(id);
        let stride = (bytes.len() / 400).max(1);
        let mut cut = 0;
        while cut < bytes.len() {
            if let Ok(file) = H5File::open(&bytes[..cut]) {
                let paths: Vec<String> = file.objects().map(|(path, _)| path.to_owned()).collect();
                for path in paths {
                    let _ = file.dataset(&path);
                }
            }
            cut += stride;
        }
    }
}

/// Flipping any single byte of the metadata region of real files is an
/// error or a (possibly different) successful read, never a panic.
#[test]
fn byte_flips_in_real_metadata_never_panic() {
    for (id, span) in [(BEJAB, 6112), (XSAPR_NETCDF4, 40_000), (H5LATEST, 30_000)] {
        let original = corpus(id);
        let span = span.min(original.len());
        let mut bytes = original.clone();
        for at in (0..span).step_by(29) {
            bytes[at] ^= 0xA5;
            if let Ok(file) = H5File::open(&bytes) {
                let paths: Vec<String> = file.objects().map(|(path, _)| path.to_owned()).collect();
                for path in paths.iter().take(4) {
                    let _ = file.dataset(path);
                }
            }
            bytes[at] = original[at];
        }
    }
}

/// The byte flips above mostly stop at a lookup3 checksum in files written
/// by HDF5 1.8 and later. With metadata checksums off, the same flips reach
/// the version 2 object header, v2 B-tree, fractal heap and chunk index
/// parsers behind them: every flipped byte of the metadata of the netCDF-4
/// file and of the superblock v3 fixtures (dense links and attributes,
/// huge heap objects, every chunk index, 4-byte addresses and lengths) is
/// an error or a successful read, never a panic.
#[test]
fn byte_flips_without_checksums_never_panic() {
    let unverified = OpenOptions::default().with_metadata_checksums(false);
    let mut opened = 0;
    for (id, span, step) in [
        (XSAPR_NETCDF4, 40_000, 13),
        (H5LATEST, 30_000, 17),
        ("odim-dkrom-20260820-1130-pvol-h5edge-len4", 60_000, 11),
        // 170,640 one-element chunks: a read takes about 50 ms.
        ("odim-dkrom-20260820-1130-pvol-h5edge-paged-ea", 30_000, 293),
    ] {
        let original = corpus(id);
        let span = span.min(original.len());
        let mut bytes = original.clone();
        for at in (0..span).step_by(step) {
            for flip in [0xA5, 0x01] {
                bytes[at] ^= flip;
                if let Ok(file) = H5File::open_with(&bytes, unverified) {
                    opened += 1;
                    let paths: Vec<String> =
                        file.objects().map(|(path, _)| path.to_owned()).collect();
                    for path in paths.iter().take(6) {
                        let _ = file.dataset_info(path);
                        let chunks = file.chunk_locations(path).map_or(0, |chunks| chunks.len());
                        if chunks <= 20_000 {
                            let _ = file.dataset(path);
                        }
                    }
                }
                bytes[at] = original[at];
            }
        }
    }
    assert!(opened > 1000, "{opened}");
}
