//! Resource limits against real JMA radar GRIB2 tars (crate docs,
//! `# Limits`): the full 20-station N5 tar must decode, and the committed
//! single-station Osaka/Takayasu (RS47773) N5 tar mutated to claim more data
//! than the caps allow must fail with a limit error.

use recast_radar_io_jma::{JmaError, read_jma_tar_volumes, volume_retained_bytes};

const TAR_BLOCK: usize = 512;

fn rs47773_n5() -> Vec<u8> {
    let path = recast_radar_testdata::path("jma-n5-20191012-090000-rs47773")
        .unwrap_or_else(|e| panic!("{e}"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn be_u32(bytes: &[u8], at: usize) -> usize {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
}

/// Offset of the first GRIB2 grid definition section (section 3) of the
/// tar's first member: the member starts after one 512-byte tar header, the
/// GRIB2 indicator section is 16 bytes, and every later section starts with
/// its u32 length and section number.
fn first_grid_section(tar: &[u8]) -> usize {
    assert_eq!(&tar[TAR_BLOCK..TAR_BLOCK + 4], b"GRIB");
    let mut at = TAR_BLOCK + 16;
    loop {
        let length = be_u32(tar, at);
        assert!(length >= 5, "walked off the section chain at {at}");
        if tar[at + 4] == 3 {
            return at;
        }
        at += length;
    }
}

/// Section 3 (template 3.50120): point count (u32 at +6), gate count (u32
/// at +14), radial count (u32 at +18).
fn set_grid(tar: &mut [u8], gates: u32, radials: u32) {
    let section = first_grid_section(tar);
    let points = gates * radials;
    tar[section + 6..section + 10].copy_from_slice(&points.to_be_bytes());
    tar[section + 14..section + 18].copy_from_slice(&gates.to_be_bytes());
    tar[section + 18..section + 22].copy_from_slice(&radials.to_be_bytes());
}

fn assert_limit_error(tar: &[u8], what: &str) {
    match read_jma_tar_volumes(tar, None) {
        Err(JmaError::LimitExceeded(reason)) => {
            assert!(reason.contains("limit"), "{what}: {reason}");
        }
        Err(other) => panic!("{what}: expected a limit error, got {other}"),
        Ok(_) => panic!("{what}: mutated tar decoded"),
    }
}

#[test]
fn full_national_reflectivity_tar_decodes_within_the_batch_limit() {
    let path = recast_radar_testdata::require_file!("jma-n5-20191012-090000");
    let tar = std::fs::read(&path).expect("read cached N5 tar");
    let volumes = read_jma_tar_volumes(&tar, None).expect("all 20 stations decode");
    assert_eq!(volumes.len(), 20);
    let decoded: usize = volumes.iter().map(volume_retained_bytes).sum();
    // 26 sweeps x 512 radials per station as f32 planes: well over the old
    // 64-million-point ceiling that rejected this real file.
    assert!(decoded > 64 * 1024 * 1024 * 4, "decoded {decoded} bytes");
}

#[test]
fn unmodified_single_station_tar_decodes_within_limits() {
    let tar = rs47773_n5();
    let section = first_grid_section(&tar);
    let (points, gates, radials) = (
        be_u32(&tar, section + 6),
        be_u32(&tar, section + 14),
        be_u32(&tar, section + 18),
    );
    assert_eq!((gates * radials, radials), (points, 512));
    let volumes = read_jma_tar_volumes(&tar, None).expect("real RS47773 tar decodes");
    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0].sweeps.len(), 26);
}

#[test]
fn tar_member_claiming_more_bytes_than_the_limit_is_rejected() {
    let mut tar = rs47773_n5();
    // ustar size field: 11 octal digits + NUL at offset 124. 32 MiB + 1.
    tar[124..136].copy_from_slice(b"00200000001\0");
    assert_limit_error(&tar, "32 MiB + 1 member");
}

#[test]
fn grid_claiming_more_gates_than_the_limit_is_rejected() {
    let mut tar = rs47773_n5();
    set_grid(&mut tar, 5000, 512);
    assert_limit_error(&tar, "5,000-gate grid");
}

#[test]
fn grid_claiming_more_points_than_the_limit_is_rejected() {
    let mut tar = rs47773_n5();
    // Both axes are at their individual ceilings; the product is not.
    set_grid(&mut tar, 4096, 2048);
    assert_limit_error(&tar, "8.4-million-point grid");
}
