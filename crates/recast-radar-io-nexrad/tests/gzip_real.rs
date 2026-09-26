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
//!
//! A Level II file of LDM bzip2 records wrapped whole in one gzip member
//! (the committed KTLX 2024 trim, gzipped here) used to decode to an empty
//! volume with no error; every gzip entry point must now decode it to the
//! volume of its inflated bytes. Such a file cut short (plain or gzipped)
//! is an error when its first LDM record is cut, and otherwise the volume
//! of the records before the cut.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Read, Write};

use flate2::Compression;
use flate2::write::GzEncoder;
use recast_radar_core::model::Volume;
use recast_radar_io_nexrad::{
    ArchiveCompression, NexradError, messages, normalize_archive_bytes,
    read_gzip_preview_from_bytes, read_gzip_volume_from_bytes_with_preview,
    read_gzip_volume_from_reader, read_normalized_volume_bytes, read_volume_from_bytes,
    read_volume_with_metadata,
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

/// `volume` with NaN (a ray without a Nyquist velocity or unambiguous
/// range) replaced, so that equal volumes compare equal.
fn without_nan(volume: &Volume) -> Volume {
    let mut volume = volume.clone();
    for sweep in &mut volume.sweeps {
        for values in [
            &mut sweep.ray_vars.nyquist_velocity_mps,
            &mut sweep.ray_vars.unambiguous_range_m,
        ]
        .into_iter()
        .flatten()
        {
            for value in values.iter_mut().filter(|value| value.is_nan()) {
                *value = f32::MAX;
            }
        }
    }
    volume
}

fn assert_same_volume(actual: &Volume, expected: &Volume, what: &str) {
    assert_eq!(
        actual.sweeps.len(),
        expected.sweeps.len(),
        "{what}: sweep count"
    );
    assert_eq!(
        actual.provenance.decode.decoded_ray_count, expected.provenance.decode.decoded_ray_count,
        "{what}: decoded rays"
    );
    assert!(
        without_nan(actual) == without_nan(expected),
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
    let expected = read_volume_from_bytes(&original).expect("real gzip volume decodes");
    assert_eq!(expected.sweeps.len(), 17);
    assert_eq!(expected.provenance.decode.decoded_ray_count, 8280);

    // 4 MiB members: the volume header, the metadata record and every
    // sweep boundary land inside different members.
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
    let volume = read_volume_from_bytes(&multi).expect("multi-member gzip decodes");
    assert_same_volume(&volume, &expected, "read_volume_from_bytes");

    // Streaming reader.
    let volume = read_gzip_volume_from_reader(multi.as_slice()).expect("streamed decode");
    assert_same_volume(&volume, &expected, "read_gzip_volume_from_reader");

    // Preview paths: the preview stops inside the first sweep, well before
    // the first member boundary at 4 MiB, and the full decode continues
    // across every boundary.
    let preview = read_gzip_preview_from_bytes(&multi, 360)
        .expect("preview decode")
        .expect("a 360-radial cut completes");
    assert!(!preview.sweeps.is_empty());
    assert!(preview.sweeps[0].nrays() >= 360);
    let mut previews = 0;
    let volume = read_gzip_volume_from_bytes_with_preview(&multi, 360, |_| previews += 1)
        .expect("decode with preview");
    assert_eq!(previews, 1);
    assert_same_volume(
        &volume,
        &expected,
        "read_gzip_volume_from_bytes_with_preview",
    );

    // Metadata decode goes through the same normalization.
    let with_metadata = read_volume_with_metadata(&multi).expect("metadata decode");
    assert_same_volume(
        &with_metadata.volume,
        &expected,
        "read_volume_with_metadata",
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
    let expected = read_volume_from_bytes(&original).expect("real gzip volume decodes");
    let (payload, _) = normalize_archive_bytes(&original).expect("real gzip volume inflates");
    let (mut padded, _) = regzip_members(&payload, 16 << 20);
    padded.extend_from_slice(&[0u8; 4096]);

    let volume = read_volume_from_bytes(&padded).expect("padded gzip decodes");
    assert_same_volume(&volume, &expected, "read_volume_from_bytes");
    let volume = read_gzip_volume_from_reader(padded.as_slice()).expect("padded stream decodes");
    assert_same_volume(&volume, &expected, "read_gzip_volume_from_reader");
    let volume = read_gzip_volume_from_bytes_with_preview(&padded, 360, |_| {})
        .expect("padded decode with preview");
    assert_same_volume(
        &volume,
        &expected,
        "read_gzip_volume_from_bytes_with_preview",
    );
}

/// `volume` as decoded from the gzip file equals `expected`, decoded from
/// its inflated bytes, except for the recorded compression.
fn assert_same_but_gzip(volume: &Volume, expected: &Volume, what: &str) {
    assert_eq!(
        volume.provenance.compression.as_deref(),
        Some("gzip"),
        "{what}: compression"
    );
    let mut volume = volume.clone();
    volume.provenance.compression = expected.provenance.compression.clone();
    assert_same_volume(&volume, expected, what);
}

/// The committed KTLX 2024 trim: an AR2V header, then LDM records.
fn ldm_records() -> Vec<u8> {
    recast_radar_testdata::bytes("l2-ktlx-20240315-000217-trim").unwrap()
}

#[test]
fn gzip_around_ldm_records_decodes_on_every_gzip_path() {
    let inflated = ldm_records();
    // An AR2V file whose first record is an LDM record (byte count, then a
    // bzip2 stream), wrapped whole in gzip.
    assert_eq!(&inflated[..4], b"AR2V");
    assert_eq!(&inflated[28..31], b"BZh");
    let stored = gzipped(&inflated);
    let mut check = Vec::new();
    flate2::read::MultiGzDecoder::new(stored.as_slice())
        .read_to_end(&mut check)
        .unwrap();
    assert!(check == inflated, "the wrapper holds the file");
    let expected = read_volume_from_bytes(&inflated).unwrap();
    assert_eq!(
        expected.provenance.compression.as_deref(),
        Some("bzip2-blocks")
    );
    // The trim holds the two sweeps of the 0.48 deg split cut, 480 radials
    // each (its manifest entry, checked against Py-ART and MetPy).
    assert_eq!(
        expected
            .sweeps
            .iter()
            .map(|sweep| sweep.nrays())
            .collect::<Vec<_>>(),
        vec![480; 2]
    );

    let volume = read_volume_from_bytes(&stored).unwrap();
    assert_same_but_gzip(&volume, &expected, "read_volume_from_bytes");
    let volume = read_gzip_volume_from_reader(stored.as_slice()).unwrap();
    assert_same_but_gzip(&volume, &expected, "read_gzip_volume_from_reader");
    let mut previews = Vec::new();
    let volume = read_gzip_volume_from_bytes_with_preview(&stored, 360, |preview| {
        previews.push(preview.sweeps[0].nrays());
    })
    .unwrap();
    assert_eq!(previews, vec![480]);
    assert_same_but_gzip(
        &volume,
        &expected,
        "read_gzip_volume_from_bytes_with_preview",
    );
    let preview = read_gzip_preview_from_bytes(&stored, 360)
        .unwrap()
        .expect("the first 480-radial sweep completes");
    assert_eq!(preview.sweeps[0].nrays(), 480);
    let with_metadata = read_volume_with_metadata(&stored).unwrap();
    assert_same_but_gzip(
        &with_metadata.volume,
        &expected,
        "read_volume_with_metadata",
    );

    // normalize_archive_bytes decodes the records too, as it does for LDM
    // records without the wrapper.
    let (normalized, compression) = normalize_archive_bytes(&stored).unwrap();
    assert_eq!(compression, ArchiveCompression::Gzip);
    let (records, _) = normalize_archive_bytes(&inflated).unwrap();
    assert!(normalized == records, "normalized bytes differ");
    let volume = read_normalized_volume_bytes(&normalized, compression).unwrap();
    assert_same_but_gzip(&volume, &expected, "read_normalized_volume_bytes");
    // The format router inflates gzip itself and passes the inflated bytes,
    // records still compressed, as normalized gzip bytes.
    let volume = read_normalized_volume_bytes(&inflated, ArchiveCompression::Gzip).unwrap();
    assert_same_but_gzip(&volume, &expected, "router path");
}

/// The LDM record starts (the 4-byte byte count) of `bytes`, read here:
/// after the 24-byte volume header, each record is a signed big-endian byte
/// count and that many bytes.
fn record_starts(bytes: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut cursor = 24;
    while cursor + 4 <= bytes.len() {
        starts.push(cursor);
        let count = i32::from_be_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        cursor += 4 + count.unsigned_abs() as usize;
    }
    starts
}

fn gzipped(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// The KTLX 2024 trim cut short, as an interrupted download leaves it: cut
/// inside its first LDM record it is a truncation error; cut inside a later
/// one it is the volume of the whole records before the cut (every sweep a
/// prefix of the full volume's), plain and gzipped. Both used to decode to
/// an empty volume with no error.
#[test]
fn truncated_ldm_records_are_an_error_or_the_records_before_the_cut() {
    let inflated = ldm_records();
    let starts = record_starts(&inflated);
    assert_eq!(starts.len(), 9, "the metadata record and 8 radial records");
    let full = read_volume_from_bytes(&inflated).unwrap();

    let inside_first = &inflated[..starts[1] - 100];
    for (what, bytes) in [
        ("plain", inside_first.to_vec()),
        ("gzipped", gzipped(inside_first)),
    ] {
        match read_volume_from_bytes(&bytes) {
            Err(NexradError::Truncated { what: kind, .. }) => {
                assert_eq!(kind, "LDM compressed record", "{what}");
            }
            other => panic!("{what}: {other:?}"),
        }
    }

    // Inside the fourth record: the metadata record and two radial records
    // before it are whole.
    let cut = (starts[3] + starts[4]) / 2;
    let cut_record = starts.iter().rposition(|start| *start < cut).unwrap();
    assert!(cut_record == 3 && cut < inflated.len());
    let whole = &inflated[..starts[cut_record]];
    let expected = read_volume_from_bytes(whole).unwrap();
    assert!(expected.provenance.decode.decoded_ray_count > 0);
    for (what, bytes, compression) in [
        ("plain", inflated[..cut].to_vec(), "bzip2-blocks"),
        ("gzipped", gzipped(&inflated[..cut]), "gzip"),
    ] {
        let volume = read_volume_from_bytes(&bytes).unwrap();
        assert_eq!(
            volume.provenance.compression.as_deref(),
            Some(compression),
            "{what}"
        );
        assert_eq!(
            volume.provenance.decode.decoded_ray_count,
            expected.provenance.decode.decoded_ray_count,
            "{what}"
        );
        assert!(
            volume.provenance.decode.decoded_ray_count < full.provenance.decode.decoded_ray_count
        );
        assert!(volume.sweeps == expected.sweeps, "{what}: sweeps differ");
        for (index, (sweep, full_sweep)) in volume.sweeps.iter().zip(&full.sweeps).enumerate() {
            assert_eq!(
                sweep.rays.azimuth_deg,
                full_sweep.rays.azimuth_deg[..sweep.nrays()],
                "{what} sweep {index}"
            );
        }
    }
}

/// `read_volume_from_bytes` parses gzip input while inflating it, through a
/// window of about 1 MiB. It must give the volume the parser gives on the
/// whole inflated buffer, and fail exactly when, and as, the one-shot
/// inflate fails: a truncated file, a damaged CRC-32 trailer and a damaged
/// deflate stream all return the one-shot's compression error.
#[test]
fn windowed_gzip_decode_equals_the_whole_buffer_parse_and_fails_as_the_one_shot() {
    let Some(original) = real_gzip_volume() else {
        return;
    };
    let (payload, compression) =
        normalize_archive_bytes(&original).expect("real gzip volume inflates");
    let expected = recast_radar_io_nexrad::read_normalized_volume_bytes(&payload, compression)
        .expect("whole-buffer parse");
    let volume = read_volume_from_bytes(&original).expect("windowed decode");
    assert!(
        without_nan(&volume) == without_nan(&expected),
        "windowed decode differs from the whole-buffer parse"
    );

    let mut truncated = original.clone();
    truncated.truncate(original.len() / 2);
    let mut bad_crc = original.clone();
    let crc_offset = bad_crc.len() - 8;
    bad_crc[crc_offset] ^= 0x5a;
    let mut bad_deflate = original.clone();
    let middle = bad_deflate.len() / 3;
    for byte in &mut bad_deflate[middle..middle + 64] {
        *byte ^= 0xa5;
    }
    for (what, raw) in [
        ("truncated", truncated),
        ("bad CRC-32", bad_crc),
        ("bad deflate", bad_deflate),
    ] {
        let one_shot = normalize_archive_bytes(&raw).expect_err(what).to_string();
        let windowed = read_volume_from_bytes(&raw).expect_err(what).to_string();
        assert_eq!(windowed, one_shot, "{what}");
    }
}
