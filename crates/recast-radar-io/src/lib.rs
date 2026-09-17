//! Format-sniffing router over the `recast-radar-io-*` decoders.
//!
//! [`decode_supported_volume_bytes`] takes a byte buffer of unknown
//! provenance, unwraps a single-member ZIP local record or a whole-file gzip
//! wrapper, sniffs the container by magic bytes, and hands it to the
//! matching decoder crate. [`decode_mobile_archive_from_path`] and
//! [`decode_mobile_dir_from_path`] wire the NEXRAD Level II decoder into the
//! DORADE crate's mobile-radar archive ingest.
//!
//! # Limits
//!
//! The router expands a whole-file gzip wrapper and a single-member ZIP
//! local record (declared and actual size) to at most
//! `MAX_DECODED_RADAR_BYTES` (512 MiB) each; a gzip stream inside a ZIP
//! record holds both buffers. The expanded bytes then meet the limits of the
//! decoder they route to, whose limit errors pass through unchanged inside
//! [`IoError`] (`NexradError::LimitExceeded`, `OdimError::LimitExceeded`, and
//! so on). The mobile archive wrappers inherit the DORADE crate's archive
//! limits.

use std::path::Path;

use flate2::read::{DeflateDecoder, GzDecoder};
use recast_radar_core::RadarVolume;
use recast_radar_core::bounded_read::{
    MAX_DECODED_RADAR_BYTES, copy_bytes_limited, read_to_end_limited,
};
use recast_radar_io_cfradial::CfRadialError;
use recast_radar_io_dorade::mobile_archive::{self, MobileVolume};
use recast_radar_io_dorade::{DoradeError, dorade};
use recast_radar_io_jma::JmaError;
use recast_radar_io_nexrad::{ArchiveCompression, NexradError};
use recast_radar_io_odim::{OdimError, hdf5lite, odim};
use thiserror::Error;

const ZIP_LOCAL_FILE_HEADER_LEN: usize = 30;

/// Error from [`decode_supported_volume_bytes`] and the mobile-archive
/// wrappers.
///
/// Decoder errors are transparent: `to_string()` yields exactly the
/// dispatched decoder's message.
#[derive(Debug, Error)]
pub enum IoError {
    /// NEXRAD Archive II / Level II decode failure.
    #[error(transparent)]
    Nexrad(#[from] NexradError),
    /// ODIM_H5 / HDF5 decode failure (including netCDF-4 CfRadial, which is
    /// an HDF5 container and routes here).
    #[error(transparent)]
    Odim(#[from] OdimError),
    /// CfRadial 1.x / classic netCDF decode failure.
    #[error(transparent)]
    CfRadial(#[from] CfRadialError),
    /// DORADE sweepfile or mobile-archive decode failure.
    #[error(transparent)]
    Dorade(#[from] DoradeError),
    /// JMA radar GRIB2 tar decode failure.
    #[error(transparent)]
    Jma(#[from] JmaError),
    /// The outer ZIP local record or gzip wrapper could not be expanded.
    #[error("unsupported or corrupt compression wrapper: {0}")]
    Compression(String),
}

/// A radar container format [`decode_supported_volume_bytes`] can decode
/// from a single byte buffer, in magic-byte sniff precedence order.
///
/// The variants and their order are the one shared routing contract used by
/// local file open, URL polling, and international providers — keep
/// [`sniff_supported_volume_format`] and [`decode_supported_volume_bytes`]
/// in lockstep with it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupportedVolumeFormat {
    /// DORADE sweepfile (solo/Radx descriptor blocks: `COMM`/`SSWB`/`VOLD`/`RADD`).
    Dorade,
    /// HDF5 container, decoded as ODIM_H5 PVOL/SCAN (EUMETNET OPERA Data
    /// Information Model; Michelson et al., OPERA WP 2.1/2.2, v2.2-2.3).
    OdimH5,
    /// Classic netCDF (`CDF\x01`/`CDF\x02`, plus CDF-5 sniffed for a useful
    /// rejection), decoded as CfRadial 1.x.
    CfRadial,
    /// JMA polar-coordinate radar GRIB2 tar (`Z__C_RJTD_*_RDR_JMAGPV*.tar`;
    /// ustar magic at byte 257, JMA GRIB2 templates 3.50120/4.51022/5.200
    /// per the JMA technical format documentation).
    JmaGrib2Tar,
    /// Everything else: NEXRAD Archive II / Level II (AR2V, gzip, bzip2,
    /// LDM block-bzip, GR2-style msg31 exports).
    NexradLevel2,
}

/// Sniff which decoder [`decode_supported_volume_bytes`] would route to.
///
/// Most magic signatures live in the first 8 bytes, but the JMA tar check
/// reads the first 512-byte tar header block (ustar magic at byte 257), and
/// the DORADE check also validates the first descriptor block length against
/// the buffer length — pass the full buffer when you have it; a short head
/// prefix may sniff DORADE or a JMA tar as Level II. Anything unrecognized
/// falls through to [`SupportedVolumeFormat::NexradLevel2`] so the error
/// surfaces from the Archive II decoder, matching the historical routing
/// chains.
pub fn sniff_supported_volume_format(head: &[u8]) -> SupportedVolumeFormat {
    if dorade::looks_like_dorade_bytes(head) {
        SupportedVolumeFormat::Dorade
    } else if hdf5lite::looks_like_hdf5_bytes(head) {
        SupportedVolumeFormat::OdimH5
    } else if recast_radar_io_cfradial::looks_like_netcdf3_bytes(head) {
        SupportedVolumeFormat::CfRadial
    } else if recast_radar_io_jma::looks_like_jma_tar_bytes(head) {
        SupportedVolumeFormat::JmaGrib2Tar
    } else {
        SupportedVolumeFormat::NexradLevel2
    }
}

/// Decode any supported single-buffer radar container by magic bytes:
/// DORADE → ODIM_H5 (HDF5) → CfRadial 1.x (classic netCDF) → JMA GRIB2 tar
/// → NEXRAD Archive II fallback.
///
/// This is the one shared router for bytes of unknown provenance (local
/// file open, custom URL polling, international feed downloads). Errors are
/// the dispatched decoder's error, displayed unchanged — callers add their
/// own source context (file name, URL). Never panics on malformed input.
///
/// JMA tars are multi-station archives (one GRIB2 member per radar of the
/// national network); this router decodes the FIRST station only, because
/// its contract is one volume per buffer. Providers that need a specific
/// station call [`recast_radar_io_jma::decode_jma_tar_volumes`] with a
/// `site_filter` directly instead of going through the router.
pub fn decode_supported_volume_bytes(raw: &[u8]) -> Result<RadarVolume, IoError> {
    let decoded_zip = if mobile_archive::looks_like_zip_bytes(raw) {
        Some(decompress_zip_local_member_bytes(raw)?)
    } else {
        None
    };
    let raw = decoded_zip.as_deref().unwrap_or(raw);
    let decoded_gzip = if raw.starts_with(&[0x1f, 0x8b]) {
        Some(decompress_gzip_bytes(raw)?)
    } else {
        None
    };
    let sniff_bytes = decoded_gzip.as_deref().unwrap_or(raw);
    match sniff_supported_volume_format(sniff_bytes) {
        SupportedVolumeFormat::Dorade => Ok(dorade::decode_dorade_sweep_volume(sniff_bytes)?),
        SupportedVolumeFormat::OdimH5 => Ok(odim::decode_odim_h5_volume(sniff_bytes)?),
        SupportedVolumeFormat::CfRadial => Ok(recast_radar_io_cfradial::decode_cfradial1_volume(
            sniff_bytes,
        )?),
        SupportedVolumeFormat::JmaGrib2Tar => Ok(
            recast_radar_io_jma::decode_jma_tar_first_station(sniff_bytes)?,
        ),
        SupportedVolumeFormat::NexradLevel2 => {
            if decoded_gzip.is_some() {
                // The router already expanded gzip to inspect its inner
                // format. Parse those normalized bytes directly instead of
                // retaining them while inflating the same payload again.
                Ok(recast_radar_io_nexrad::decode_normalized_volume_bytes(
                    sniff_bytes,
                    ArchiveCompression::Gzip,
                )?)
            } else {
                Ok(recast_radar_io_nexrad::decode_volume_from_bytes(raw)?)
            }
        }
    }
}

/// Decode every radar volume in a mobile-radar zip archive, with `.msg31`
/// and `AR2V` members decoded by the NEXRAD Level II decoder. See
/// [`recast_radar_io_dorade::mobile_archive::decode_mobile_archive_from_path`].
pub fn decode_mobile_archive_from_path(path: &Path) -> Result<Vec<MobileVolume>, IoError> {
    Ok(mobile_archive::decode_mobile_archive_from_path(
        path,
        recast_radar_io_nexrad::decode_volume_from_bytes,
    )?)
}

/// Decode every radar volume under a mobile-radar deployment folder, with
/// Level II members decoded by the NEXRAD Level II decoder. See
/// [`recast_radar_io_dorade::mobile_archive::decode_mobile_dir_from_path`].
pub fn decode_mobile_dir_from_path(dir: &Path) -> Result<Vec<MobileVolume>, IoError> {
    Ok(mobile_archive::decode_mobile_dir_from_path(
        dir,
        recast_radar_io_nexrad::decode_volume_from_bytes,
    )?)
}

fn decompress_gzip_bytes(raw: &[u8]) -> Result<Vec<u8>, IoError> {
    read_to_end_limited(
        GzDecoder::new(raw),
        MAX_DECODED_RADAR_BYTES,
        "gzip radar payload",
    )
    .map_err(IoError::Compression)
}

/// Decode a single ZIP local-file record.
///
/// NCI THREDDS can serve one member inside a huge daily radar ZIP directly,
/// but the response body is the member's ZIP local record rather than a
/// complete central-directory ZIP archive. The regular `zip` crate quite
/// reasonably rejects that stream; this small parser handles only the local
/// record shape needed by single-member HTTP responses.
fn decompress_zip_local_member_bytes(raw: &[u8]) -> Result<Vec<u8>, IoError> {
    if raw.len() < ZIP_LOCAL_FILE_HEADER_LEN || &raw[..4] != b"PK\x03\x04" {
        return Err(IoError::Compression(
            "not a ZIP local-file record".to_owned(),
        ));
    }
    let flags = u16::from_le_bytes([raw[6], raw[7]]);
    if flags & 0x0008 != 0 {
        return Err(IoError::Compression(
            "ZIP local-file record uses a trailing data descriptor".to_owned(),
        ));
    }
    if flags & 0x0001 != 0 {
        return Err(IoError::Compression(
            "encrypted ZIP local-file record is unsupported".to_owned(),
        ));
    }
    let method = u16::from_le_bytes([raw[8], raw[9]]);
    let compressed_size = u32::from_le_bytes([raw[18], raw[19], raw[20], raw[21]]) as usize;
    let uncompressed_size = u32::from_le_bytes([raw[22], raw[23], raw[24], raw[25]]) as usize;
    if uncompressed_size > MAX_DECODED_RADAR_BYTES {
        return Err(IoError::Compression(format!(
            "ZIP local-file member declares {uncompressed_size} expanded bytes (limit {MAX_DECODED_RADAR_BYTES})"
        )));
    }
    let name_len = u16::from_le_bytes([raw[26], raw[27]]) as usize;
    let extra_len = u16::from_le_bytes([raw[28], raw[29]]) as usize;
    let data_start = ZIP_LOCAL_FILE_HEADER_LEN
        .checked_add(name_len)
        .and_then(|value| value.checked_add(extra_len))
        .ok_or_else(|| IoError::Compression("ZIP local-file header overflow".to_owned()))?;
    let data_end = data_start.checked_add(compressed_size).ok_or_else(|| {
        IoError::Compression("ZIP local-file compressed size overflow".to_owned())
    })?;
    if data_end > raw.len() {
        return Err(IoError::Compression(format!(
            "ZIP local-file data truncated: need {data_end} bytes, have {}",
            raw.len()
        )));
    }
    let compressed = &raw[data_start..data_end];
    match method {
        0 => copy_bytes_limited(compressed, MAX_DECODED_RADAR_BYTES, "ZIP stored member")
            .map_err(IoError::Compression),
        8 => {
            let decoded = read_to_end_limited(
                DeflateDecoder::new(compressed),
                MAX_DECODED_RADAR_BYTES,
                "ZIP deflate member",
            )
            .map_err(IoError::Compression)?;
            if uncompressed_size != 0 && decoded.len() != uncompressed_size {
                return Err(IoError::Compression(format!(
                    "ZIP deflate member decoded to {} bytes, expected {uncompressed_size}",
                    decoded.len()
                )));
            }
            Ok(decoded)
        }
        other => Err(IoError::Compression(format!(
            "unsupported ZIP compression method {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    use std::io::Write;

    #[test]
    fn sniffs_supported_volume_formats_in_router_order() {
        // DORADE descriptor blocks win over everything (8-byte head form:
        // 4-byte name + a block length plausible in at least one byte order).
        let mut dorade_head = b"VOLD".to_vec();
        dorade_head.extend_from_slice(&8u32.to_le_bytes());
        assert_eq!(
            sniff_supported_volume_format(&dorade_head),
            SupportedVolumeFormat::Dorade
        );
        assert_eq!(
            sniff_supported_volume_format(b"\x89HDF\r\n\x1a\nrest"),
            SupportedVolumeFormat::OdimH5
        );
        assert_eq!(
            sniff_supported_volume_format(b"CDF\x01...."),
            SupportedVolumeFormat::CfRadial
        );
        // JMA radar tar: ustar magic at byte 257 + Z__C_RJTD member name.
        let mut jma_tar = vec![0u8; 1024];
        let jma_name =
            b"Z__C_RJTD_20260612064000_RDR_JMAGPV_RS47937_Gar0p5km0p7deg_Pze_ANAL_grib2.bin";
        jma_tar[..jma_name.len()].copy_from_slice(jma_name);
        jma_tar[257..262].copy_from_slice(b"ustar");
        assert_eq!(
            sniff_supported_volume_format(&jma_tar),
            SupportedVolumeFormat::JmaGrib2Tar
        );
        // A generic (non-JMA) tar is not claimed; it falls through.
        let mut plain_tar = vec![0u8; 1024];
        plain_tar[..9].copy_from_slice(b"notes.txt");
        plain_tar[257..262].copy_from_slice(b"ustar");
        assert_eq!(
            sniff_supported_volume_format(&plain_tar),
            SupportedVolumeFormat::NexradLevel2
        );
        // Archive II, compressed wrappers, and unknown garbage all fall
        // through to the Level II decoder so its errors surface.
        assert_eq!(
            sniff_supported_volume_format(b"AR2V0006."),
            SupportedVolumeFormat::NexradLevel2
        );
        assert_eq!(
            sniff_supported_volume_format(b"\x1f\x8b\x08\0\0\0\0\0"),
            SupportedVolumeFormat::NexradLevel2
        );
        assert_eq!(
            sniff_supported_volume_format(b""),
            SupportedVolumeFormat::NexradLevel2
        );
        // CDF-3 does not exist; netCDF-3 sniff accepts 1/2/5 only.
        assert_eq!(
            sniff_supported_volume_format(b"CDF\x03...."),
            SupportedVolumeFormat::NexradLevel2
        );
    }

    #[test]
    fn router_stringifies_level2_error_for_unrecognized_bytes() {
        // Short unrecognized bytes fail the Archive II volume-header check;
        // the router must surface that exact decoder message.
        let direct_err = recast_radar_io_nexrad::decode_volume_from_bytes(b"not radar")
            .expect_err("short garbage must not decode")
            .to_string();
        let routed_err = decode_supported_volume_bytes(b"not radar")
            .expect_err("short garbage must not decode")
            .to_string();
        assert_eq!(routed_err, direct_err);
        assert!(
            routed_err.contains("too short for an Archive II volume header"),
            "unexpected error text: {routed_err}"
        );
    }

    #[test]
    fn unwraps_zip_local_member_stream_without_central_directory() {
        let payload = b"\x89HDF\r\n\x1a\nfake odim bytes";
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(payload).unwrap();
        let compressed = encoder.finish().unwrap();
        let name = b"2_20260624_235500.pvol.h5";
        let mut zip = Vec::new();
        zip.extend_from_slice(b"PK\x03\x04");
        zip.extend_from_slice(&20u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&8u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u32.to_le_bytes());
        zip.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(name.len() as u16).to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(name);
        zip.extend_from_slice(&compressed);

        let decoded = decompress_zip_local_member_bytes(&zip).unwrap();
        assert_eq!(decoded, payload);
    }
}
