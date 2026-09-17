//! The format router surfaces every decoder's resource-limit error unchanged
//! (crate docs, `# Limits`). Each case mutates committed real bytes to claim
//! more than a documented cap and routes them through
//! `decode_supported_volume_bytes`.

use std::io::Write;

use flate2::Compression;
use flate2::write::GzEncoder;
use recast_radar_io::{IoError, decode_supported_volume_bytes};
use recast_radar_io_cfradial::CfRadialError;
use recast_radar_io_dorade::DoradeError;
use recast_radar_io_jma::JmaError;
use recast_radar_io_nexrad::{NexradError, normalize_archive_bytes};
use recast_radar_io_odim::OdimError;

fn read_testdata(id: &str) -> Vec<u8> {
    let path = recast_radar_testdata::path(id).unwrap_or_else(|e| panic!("{e}"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn be_u32(bytes: &[u8], at: usize) -> usize {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
}

/// Bejab PVOL whose version-1 root object header (address in the v0
/// superblock's root symbol-table entry) claims 5,000 messages.
fn odim_claiming_too_many_messages() -> Vec<u8> {
    let mut bytes = read_testdata("odim-bejab-20190606-0000-pvol");
    let mut address = [0u8; 8];
    address.copy_from_slice(&bytes[64..72]);
    let root = u64::from_le_bytes(address) as usize;
    bytes[root + 2..root + 4].copy_from_slice(&5000u16.to_le_bytes());
    bytes
}

fn assert_limit(result: Result<recast_radar_core::RadarVolume, IoError>, what: &str) {
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("{what}: mutated input decoded"),
    };
    let limited = matches!(
        &error,
        IoError::Nexrad(NexradError::LimitExceeded(_))
            | IoError::Odim(OdimError::LimitExceeded(_))
            | IoError::CfRadial(CfRadialError::LimitExceeded(_))
            | IoError::Dorade(DoradeError::LimitExceeded(_))
            | IoError::Jma(JmaError::LimitExceeded(_))
    );
    assert!(limited, "{what}: expected a limit error, got {error}");
    assert!(error.to_string().contains("limit"), "{what}: {error}");
}

#[test]
fn level2_gate_limit_error_is_routed() {
    let mut raw = Vec::new();
    for id in [
        "l2chunk-kiwa-307-20260917-003629-001-s",
        "l2chunk-kiwa-307-20260917-003629-002-i",
    ] {
        raw.extend(read_testdata(id));
    }
    let (mut bytes, _) = normalize_archive_bytes(&raw).expect("real chunks decompress");
    // The first Message 31 record follows the volume header and the 134
    // fixed 2,432-byte metadata records; its body starts after a 12-byte
    // control word and the 16-byte message header.
    let body = 24 + 134 * 2432 + 12 + 16;
    assert_eq!(bytes[body - 13], 31, "first radial record is Message 31");
    let pointer = be_u32(&bytes, body + 32 + 3 * 4); // fourth block: REF
    assert_eq!(&bytes[body + pointer..body + pointer + 4], b"DREF");
    let gate_count = body + pointer + 8;
    bytes[gate_count..gate_count + 2].copy_from_slice(&u16::MAX.to_be_bytes());
    assert_limit(
        decode_supported_volume_bytes(&bytes),
        "Level II 65,535 gates",
    );
}

#[test]
fn odim_header_limit_error_is_routed_plain_and_gzip_wrapped() {
    let bytes = odim_claiming_too_many_messages();
    assert_limit(decode_supported_volume_bytes(&bytes), "ODIM root header");
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&bytes).expect("gzip in memory");
    let gzipped = encoder.finish().expect("gzip in memory");
    assert_limit(decode_supported_volume_bytes(&gzipped), "gzip-wrapped ODIM");
}

#[test]
fn cfradial_gate_limit_error_is_routed() {
    let mut bytes = read_testdata("cfrad1-irene-sr2-20110827-120420-sur-sweeps01");
    // Classic netCDF header: dimension 1 is `range` (name length 5, padded
    // name "range\0\0\0"), its u32 length at byte 40.
    assert_eq!(&bytes[32..37], b"range");
    assert_eq!(be_u32(&bytes, 40), 1107);
    bytes[40..44].copy_from_slice(&20_000u32.to_be_bytes());
    assert_limit(
        decode_supported_volume_bytes(&bytes),
        "CfRadial 20,000 gates",
    );
}

#[test]
fn dorade_gate_limit_error_is_routed() {
    let mut bytes = read_testdata("dorade-cow2-20260521-225514-sur-head24");
    // Big-endian COW2 sweep: CSFD descriptor at byte 1944, num_cells[0] at +48.
    assert_eq!(&bytes[1944..1948], b"CSFD");
    bytes[1944 + 48..1944 + 50].copy_from_slice(&i16::MAX.to_be_bytes());
    assert_limit(decode_supported_volume_bytes(&bytes), "DORADE 32,767 cells");
}

#[test]
fn jma_member_limit_error_is_routed() {
    let mut bytes = read_testdata("jma-n5-20191012-090000-rs47773");
    // ustar size field of the first member: 32 MiB + 1 byte, octal.
    bytes[124..136].copy_from_slice(b"00200000001\0");
    assert_limit(
        decode_supported_volume_bytes(&bytes),
        "JMA 32 MiB + 1 member",
    );
}
