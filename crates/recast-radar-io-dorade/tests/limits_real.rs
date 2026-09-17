//! Resource limits against real DORADE sweepfiles mutated to claim more data
//! than the documented caps allow (crate docs, `# Limits`). Every mutation
//! starts from committed real bytes: the big-endian, run-length-encoded CSWR
//! COW2 sweep (24 rays) and the little-endian, uncompressed NOAA NOXP sweep.

use recast_radar_core::bounded_read::{MAX_GATES_PER_RADIAL, MAX_SWEEPS_PER_VOLUME};
use recast_radar_io_dorade::DoradeError;
use recast_radar_io_dorade::dorade::{
    decode_dorade_sweep_volume, decode_dorade_volume_from_slices,
};

const COW2: &str = "dorade-cow2-20260521-225514-sur-head24";
const NOXP: &str = "dorade-noxp-20090501-190244-ppi";

#[derive(Clone, Copy)]
enum Endian {
    Big,
    Little,
}

fn read_testdata(id: &str) -> Vec<u8> {
    let path = recast_radar_testdata::path(id).unwrap_or_else(|e| panic!("{e}"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn read_i32(bytes: &[u8], at: usize, endian: Endian) -> i32 {
    let word = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
    match endian {
        Endian::Big => i32::from_be_bytes(word),
        Endian::Little => i32::from_le_bytes(word),
    }
}

fn write_i32(bytes: &mut [u8], at: usize, value: i32, endian: Endian) {
    let word = match endian {
        Endian::Big => value.to_be_bytes(),
        Endian::Little => value.to_le_bytes(),
    };
    bytes[at..at + 4].copy_from_slice(&word);
}

/// Offset of the first descriptor block with `id`, walking block lengths.
fn find_block(bytes: &[u8], id: &[u8; 4], endian: Endian) -> usize {
    let mut at = 0;
    while at + 8 <= bytes.len() {
        if &bytes[at..at + 4] == id {
            return at;
        }
        let len = read_i32(bytes, at + 4, endian);
        assert!(len >= 8, "walked off the block chain at {at}");
        at += len as usize;
    }
    panic!("no {} block", String::from_utf8_lossy(id));
}

fn assert_limit_error<T>(result: Result<T, DoradeError>, what: &str) {
    match result {
        Err(DoradeError::LimitExceeded(reason)) => {
            assert!(reason.contains("limit"), "{what}: {reason}");
        }
        Err(other) => panic!("{what}: expected a limit error, got {other}"),
        Ok(_) => panic!("{what}: mutated input decoded"),
    }
}

#[test]
fn unmodified_real_sweeps_decode_within_limits() {
    for id in [COW2, NOXP] {
        let volume =
            decode_dorade_sweep_volume(&read_testdata(id)).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(volume.cuts.len(), 1);
    }
}

#[test]
fn cell_spacing_descriptor_claiming_too_many_cells_is_rejected() {
    let mut bytes = read_testdata(COW2);
    let csfd = find_block(&bytes, b"CSFD", Endian::Big);
    // num_cells[0] (i16 at +48): the real 375 becomes 32,767.
    assert_eq!(
        i16::from_be_bytes([bytes[csfd + 48], bytes[csfd + 49]]),
        375
    );
    bytes[csfd + 48..csfd + 50].copy_from_slice(&i16::MAX.to_be_bytes());
    assert_limit_error(decode_dorade_sweep_volume(&bytes), "32,767-cell CSFD");
}

#[test]
fn parameter_descriptor_claiming_too_many_cells_is_rejected() {
    let mut bytes = read_testdata(NOXP);
    let parm = find_block(&bytes, b"PARM", Endian::Little);
    // Extended PARM number_cells (i32 at +200): the real 1,001 becomes 1e6.
    assert_eq!(read_i32(&bytes, parm + 200, Endian::Little), 1001);
    write_i32(&mut bytes, parm + 200, 1_000_000, Endian::Little);
    assert_limit_error(decode_dorade_sweep_volume(&bytes), "one-million-cell PARM");
}

#[test]
fn uncompressed_ray_longer_than_the_gate_limit_is_rejected() {
    let mut bytes = read_testdata(NOXP);
    let rdat = find_block(&bytes, b"RDAT", Endian::Little);
    // Stretch the first 16-bit field block over the rest of the file: its
    // payload now claims far more than the gate limit in one ray.
    let stretched = bytes.len() - rdat;
    assert!((stretched - 16) / 2 > MAX_GATES_PER_RADIAL);
    write_i32(&mut bytes, rdat + 4, stretched as i32, Endian::Little);
    assert_limit_error(decode_dorade_sweep_volume(&bytes), "stretched RDAT block");
}

#[test]
fn volume_with_more_sweeps_than_the_limit_is_rejected() {
    let sweep = read_testdata(COW2);
    let sweeps = vec![sweep.as_slice(); MAX_SWEEPS_PER_VOLUME + 1];
    assert_limit_error(
        decode_dorade_volume_from_slices(&sweeps),
        "1,025 sweeps in one volume",
    );
}
