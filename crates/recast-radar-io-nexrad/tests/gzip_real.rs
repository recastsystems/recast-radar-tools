//! Multi-member gzip against a real `.gz` Level II volume: the KTLX
//! 2013-05-20 (Moore tornado) archive, 9.5 MB gzip, 44.9 MB inflated.
//!
//! `gzip -d` concatenates every member of a multi-member file, which is
//! what parallel or chunked compressors (`pigz`, `bgzip`, appended files)
//! write. The decoder used to inflate the first member only, so such a
//! file silently gave a partial volume. Every gzip entry point (in-memory
//! decode, streaming reader, the preview decoders, `normalize_archive_bytes`
//! and the `messages` helpers) must now decode a re-gzipped multi-member
//! copy of the real volume to the same volume as the original.

use std::io::Write;

use flate2::Compression;
use flate2::write::GzEncoder;
use recast_radar_core::RadarVolume;
use recast_radar_io_nexrad::{
    ArchiveCompression, decode_gzip_preview_from_bytes, decode_gzip_volume_from_bytes_with_preview,
    decode_gzip_volume_from_reader, decode_volume_from_bytes, decode_volume_with_metadata,
    messages, normalize_archive_bytes,
};

/// The real single-member archive from the corpus, or `None` when it is
/// not cached and cannot be downloaded right now.
fn real_gzip_volume() -> Option<Vec<u8>> {
    match recast_radar_testdata::bytes("l2-ktlx-20130520-201643") {
        Ok(bytes) => {
            assert!(bytes.starts_with(&[0x1f, 0x8b]), "the corpus entry is gzip");
            Some(bytes)
        }
        Err(e) if e.is_offline() => {
            eprintln!("skipping: {e}");
            None
        }
        Err(e) => panic!("{e}"),
    }
}

/// Re-compress `payload` as one gzip member per `member_len` bytes, back
/// to back, the way `gzip -d` expects a multi-member file.
fn regzip_members(payload: &[u8], member_len: usize) -> (Vec<u8>, usize) {
    let mut raw = Vec::new();
    let mut members = 0;
    for chunk in payload.chunks(member_len) {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        let member = encoder
            .write_all(chunk)
            .and_then(|()| encoder.finish())
            .unwrap_or_else(|e| panic!("gzip member: {e}"));
        raw.extend(member);
        members += 1;
    }
    (raw, members)
}

fn assert_same_volume(actual: &RadarVolume, expected: &RadarVolume, what: &str) {
    assert_eq!(actual.cuts.len(), expected.cuts.len(), "{what}: cut count");
    assert_eq!(
        actual.metadata.decoded_radial_count, expected.metadata.decoded_radial_count,
        "{what}: decoded radials"
    );
    assert!(
        actual == expected,
        "{what}: volume differs from the original"
    );
}

#[test]
fn multi_member_regzip_of_real_volume_decodes_identically_on_every_gzip_path() {
    let Some(original) = real_gzip_volume() else {
        return;
    };
    let (payload, compression) =
        normalize_archive_bytes(&original).expect("real gzip volume inflates");
    assert_eq!(compression, ArchiveCompression::Gzip);
    let expected = decode_volume_from_bytes(&original).expect("real gzip volume decodes");
    assert_eq!(expected.cuts.len(), 17);
    assert_eq!(expected.metadata.decoded_radial_count, 8280);

    // 4 MiB members: the volume header, the metadata record and every
    // elevation cut boundary land inside different members.
    let (multi, members) = regzip_members(&payload, 4 << 20);
    assert!(members > 10, "{members} members");
    // ISIZE of the trailer describes the last (short) member only, so the
    // one-shot buffer starts from a small hint and grows.
    let last_member_len = payload.len() % (4 << 20);
    assert_eq!(
        u32::from_le_bytes(multi[multi.len() - 4..].try_into().unwrap()),
        last_member_len as u32
    );

    // In-memory decode: the one-shot inflate.
    let (normalized, compression) =
        normalize_archive_bytes(&multi).expect("multi-member gzip normalizes");
    assert_eq!(compression, ArchiveCompression::Gzip);
    assert_eq!(normalized.len(), payload.len(), "normalized length");
    assert!(normalized == payload, "normalized bytes differ");
    let volume = decode_volume_from_bytes(&multi).expect("multi-member gzip decodes");
    assert_same_volume(&volume, &expected, "decode_volume_from_bytes");

    // Streaming reader.
    let volume = decode_gzip_volume_from_reader(multi.as_slice()).expect("streamed decode");
    assert_same_volume(&volume, &expected, "decode_gzip_volume_from_reader");

    // Preview paths: the preview stops inside the first cut, well before
    // the first member boundary at 4 MiB, and the full decode continues
    // across every boundary.
    let preview = decode_gzip_preview_from_bytes(&multi, 360)
        .expect("preview decode")
        .expect("a 360-radial cut completes");
    assert!(!preview.cuts.is_empty());
    assert!(preview.cuts[0].radials.len() >= 360);
    let mut previews = 0;
    let volume = decode_gzip_volume_from_bytes_with_preview(&multi, 360, |_| previews += 1)
        .expect("decode with preview");
    assert_eq!(previews, 1);
    assert_same_volume(
        &volume,
        &expected,
        "decode_gzip_volume_from_bytes_with_preview",
    );

    // Metadata decode goes through the same normalization.
    let with_metadata = decode_volume_with_metadata(&multi).expect("metadata decode");
    assert_same_volume(
        &with_metadata.volume,
        &expected,
        "decode_volume_with_metadata",
    );

    // The message walker's whole-file helpers.
    let records = messages::record_bytes(&multi).expect("record bytes");
    assert!(*records == payload[24..], "record_bytes differs");
    let metadata_record = messages::metadata_record(&multi).expect("metadata record");
    assert_eq!(metadata_record.len(), 134 * 2432);
    assert!(
        *metadata_record == payload[24..24 + 134 * 2432],
        "metadata_record differs"
    );
}

#[test]
fn trailing_padding_after_the_last_member_is_ignored_on_every_gzip_path() {
    let Some(original) = real_gzip_volume() else {
        return;
    };
    let expected = decode_volume_from_bytes(&original).expect("real gzip volume decodes");
    let (payload, _) = normalize_archive_bytes(&original).expect("real gzip volume inflates");
    let (mut padded, _) = regzip_members(&payload, 16 << 20);
    padded.extend_from_slice(&[0u8; 4096]);

    let volume = decode_volume_from_bytes(&padded).expect("padded gzip decodes");
    assert_same_volume(&volume, &expected, "decode_volume_from_bytes");
    let volume = decode_gzip_volume_from_reader(padded.as_slice()).expect("padded stream decodes");
    assert_same_volume(&volume, &expected, "decode_gzip_volume_from_reader");
    let volume = decode_gzip_volume_from_bytes_with_preview(&padded, 360, |_| {})
        .expect("padded decode with preview");
    assert_same_volume(
        &volume,
        &expected,
        "decode_gzip_volume_from_bytes_with_preview",
    );
}
