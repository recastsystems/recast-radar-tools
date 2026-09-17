//! Resource limits against real Level II volumes mutated to claim more data
//! than the documented caps allow (crate docs, `# Limits`). Every mutation
//! starts from real bytes: the committed KIWA volume 307 Start chunk and its
//! first two intermediate chunks, or a cached archive volume.

use recast_radar_core::bounded_read::{MAX_GATES_PER_RADIAL, MAX_SWEEPS_PER_VOLUME};
use recast_radar_io_nexrad::{
    ArchiveCompression, NexradError, decode_normalized_volume_bytes, decode_volume_from_bytes,
    normalize_archive_bytes,
};

const VOLUME_HEADER_LEN: usize = 24;
const CONTROL_WORD_LEN: usize = 12;
const MESSAGE_HEADER_LEN: usize = 16;
const RECORD_BYTES: usize = 2432;
const METADATA_RECORDS: usize = 134;
/// Message 31 header: radial status byte, then the ten data block pointers.
const RADIAL_STATUS_OFFSET: usize = 21;
const BLOCK_POINTERS_OFFSET: usize = 32;
/// Generic data moment block: gate count (u16) offset.
const GATE_COUNT_OFFSET: usize = 8;

fn read_testdata(id: &str) -> Vec<u8> {
    let path = recast_radar_testdata::path(id).unwrap_or_else(|e| panic!("{e}"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Start chunk plus the first two intermediate chunks of KIWA volume 307,
/// concatenated in sequence order and decompressed: the real metadata
/// record followed by 240 real Message 31 radials.
fn kiwa_normalized() -> Vec<u8> {
    let mut raw = Vec::new();
    for id in [
        "l2chunk-kiwa-307-20260917-003629-001-s",
        "l2chunk-kiwa-307-20260917-003629-002-i",
        "l2chunk-kiwa-307-20260917-003629-003-i",
    ] {
        raw.extend(read_testdata(id));
    }
    let (bytes, compression) =
        normalize_archive_bytes(&raw).unwrap_or_else(|e| panic!("real chunks decompress: {e}"));
    assert_eq!(compression, ArchiveCompression::Bzip2Blocks);
    bytes
}

/// Offsets of every Message 31 body (just past its 16-byte message header),
/// following the Archive II record framing: 134 fixed 2,432-byte metadata
/// records, then variable-length Message 31 records.
fn message31_bodies(bytes: &[u8]) -> Vec<usize> {
    let mut bodies = Vec::new();
    let mut cursor = VOLUME_HEADER_LEN;
    let mut record = 0usize;
    while cursor + CONTROL_WORD_LEN + MESSAGE_HEADER_LEN <= bytes.len() {
        let header = cursor + CONTROL_WORD_LEN;
        let size = usize::from(u16::from_be_bytes([bytes[header], bytes[header + 1]])) * 2;
        let message_type = bytes[header + 3];
        if size == 0 && record >= METADATA_RECORDS {
            break;
        }
        if message_type == 31 {
            bodies.push(header + MESSAGE_HEADER_LEN);
        }
        cursor += if message_type == 31 && record >= METADATA_RECORDS {
            size + CONTROL_WORD_LEN
        } else {
            RECORD_BYTES
        };
        record += 1;
    }
    bodies
}

fn be_u32(bytes: &[u8], offset: usize) -> usize {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]) as usize
}

#[test]
fn committed_chunk_prefix_decodes_within_limits() {
    let bytes = kiwa_normalized();
    assert_eq!(message31_bodies(&bytes).len(), 240);
    let volume = decode_normalized_volume_bytes(&bytes, ArchiveCompression::Bzip2Blocks)
        .expect("unmodified real chunks decode");
    assert_eq!(volume.metadata.decoded_radial_count, 240);
}

#[test]
fn moment_block_claiming_more_gates_than_the_limit_is_rejected() {
    let mut bytes = kiwa_normalized();
    let body = message31_bodies(&bytes)[0];
    let gate_count_at = (0..10)
        .map(|index| be_u32(&bytes, body + BLOCK_POINTERS_OFFSET + index * 4))
        .find(|&pointer| pointer != 0 && bytes[body + pointer] == b'D')
        .map(|pointer| body + pointer + GATE_COUNT_OFFSET)
        .expect("first radial has a data moment block");
    let real_gates = usize::from(u16::from_be_bytes([
        bytes[gate_count_at],
        bytes[gate_count_at + 1],
    ]));
    assert!((1..=MAX_GATES_PER_RADIAL).contains(&real_gates));

    bytes[gate_count_at..gate_count_at + 2].copy_from_slice(&u16::MAX.to_be_bytes());
    for decoded in [
        decode_normalized_volume_bytes(&bytes, ArchiveCompression::Bzip2Blocks),
        decode_volume_from_bytes(&bytes),
    ] {
        let error = decoded.expect_err("65,535 gates exceed the per-radial limit");
        assert!(
            matches!(&error, NexradError::LimitExceeded(reason) if reason.contains("limit")),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn volume_with_more_cuts_than_the_sweep_limit_is_rejected() {
    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217");
    let raw = std::fs::read(&path).expect("read cached volume");
    let (mut bytes, _) = normalize_archive_bytes(&raw).expect("real volume decompresses");
    let bodies = message31_bodies(&bytes);
    assert_eq!(bodies.len(), 11_520);
    // Radial status 0 (start of elevation) on every radial opens a new cut
    // per radial. Unlinking the data moment blocks keeps those cuts free of
    // moment grids, so only the cut count can trip a limit.
    for body in bodies {
        bytes[body + RADIAL_STATUS_OFFSET] = 0;
        for index in 0..10 {
            let at = body + BLOCK_POINTERS_OFFSET + index * 4;
            let pointer = be_u32(&bytes, at);
            if pointer != 0 && bytes[body + pointer] == b'D' {
                bytes[at..at + 4].fill(0);
            }
        }
    }
    let error = decode_normalized_volume_bytes(&bytes, ArchiveCompression::Bzip2Blocks)
        .expect_err("11,520 cuts exceed the sweep limit");
    assert!(
        matches!(
            &error,
            NexradError::LimitExceeded(reason)
                if reason.contains(&format!("more than {MAX_SWEEPS_PER_VOLUME} elevation cuts"))
        ),
        "unexpected error: {error}"
    );
}
