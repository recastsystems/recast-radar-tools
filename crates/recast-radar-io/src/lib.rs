//! Format-sniffing router over the `recast-radar-io-*` decoders.
//!
//! [`read_supported_volume_bytes`] takes a byte buffer of unknown
//! provenance, unwraps a single-member ZIP local record or a whole-file gzip
//! wrapper, sniffs the container by magic bytes (an HDF5 container by
//! content: ODIM_H5, CfRadial 1 in netCDF-4, or CfRadial 2; NEXRAD Level III
//! by its NOAAPort/WMO framing or Message Header Block), and hands it to the
//! matching decoder crate, returning the FM301
//! [`recast_radar_core::model::Volume`] with the format's typed metadata
//! beside it ([`Decoded`], [`FormatMetadata`]; design note
//! `docs/design/fm301-model.md` section 2). [`read_mobile_archive_from_path`]
//! and [`read_mobile_dir_from_path`] wire the NEXRAD Level II decoder into
//! the DORADE crate's mobile-radar archive ingest.
//!
//! # Limits
//!
//! The router expands a whole-file gzip wrapper (every member of a
//! multi-member file) and a single-member ZIP local record (declared and
//! actual size) to at most `MAX_DECODED_RADAR_BYTES` (512 MiB) each; a gzip
//! stream inside a ZIP record holds both buffers. The expanded bytes then
//! meet the limits of the decoder they route to, whose limit errors pass
//! through unchanged inside [`IoError`] (`NexradError::LimitExceeded`,
//! `OdimError::LimitExceeded`, and so on). The mobile archive wrappers
//! inherit the DORADE crate's archive limits.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::path::Path;

use flate2::read::DeflateDecoder;
use recast_radar_core::bounded_read::{
    MAX_DECODED_RADAR_BYTES, copy_bytes_limited, read_to_end_limited,
};
use recast_radar_core::model::Volume;
use recast_radar_io_cfradial::CfRadialError;
use recast_radar_io_dorade::mobile_archive::{self, MobileVolume};
use recast_radar_io_dorade::{DoradeError, dorade};
use recast_radar_io_jma::JmaError;
use recast_radar_io_level3::{Level3Error, Level3Message, Level3Product};
use recast_radar_io_nexrad::{ArchiveCompression, NexradError, NexradMetadata};
use recast_radar_io_odim::{OdimError, hdf5, odim};
use thiserror::Error;

const ZIP_LOCAL_FILE_HEADER_LEN: usize = 30;

/// The error [`recast_radar_io_level3::read_level3_volume`] gives for a
/// decoded message that is not a volume, for the message of
/// [`IoError::Level3WithoutVolume`].
fn without_volume(message: &Level3Message) -> String {
    match message {
        Level3Message::Product(product) => Level3Error::NoDataArray {
            code: product.description.product_code,
        }
        .to_string(),
        Level3Message::GeneralStatus(status) => Level3Error::NotAProduct {
            code: status.message_header.code,
        }
        .to_string(),
        Level3Message::Text(text) => Level3Error::TextOnly {
            heading: text.text_header.wmo_heading.clone(),
        }
        .to_string(),
        _ => "NEXRAD Level III message without a data array".to_owned(),
    }
}

/// Error from [`read_supported_volume_bytes`] and the mobile-archive
/// wrappers.
///
/// Decoder errors are transparent: `to_string()` yields exactly the
/// dispatched decoder's message.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum IoError {
    /// NEXRAD Archive II / Level II decode failure.
    #[error(transparent)]
    Nexrad(#[from] NexradError),
    /// ODIM_H5 / HDF5 decode failure (an HDF5 container that is not
    /// netCDF-4, or one that cannot be opened, routes here).
    #[error(transparent)]
    Odim(#[from] OdimError),
    /// CfRadial 1.x / 2 decode failure (classic netCDF or netCDF-4).
    #[error(transparent)]
    CfRadial(#[from] CfRadialError),
    /// DORADE sweepfile or mobile-archive decode failure.
    #[error(transparent)]
    Dorade(#[from] DoradeError),
    /// JMA radar GRIB2 tar decode failure.
    #[error(transparent)]
    Jma(#[from] JmaError),
    /// NEXRAD / TDWR Level III decode failure.
    #[error(transparent)]
    Level3(#[from] Level3Error),
    /// A NEXRAD / TDWR Level III file that decoded but has no radial,
    /// raster or generic data array to make a volume of: a graphic or
    /// tabular product (storm tracking, VAD wind profile, melting layer),
    /// a General Status Message or a plain-text message. The decoded
    /// message is attached ([`recast_radar_io_level3::decode_message`]),
    /// every block and packet typed; the error message is the one
    /// [`recast_radar_io_level3::read_level3_volume`] gives
    /// ([`Level3Error::NoDataArray`], [`Level3Error::NotAProduct`] or
    /// [`Level3Error::TextOnly`]).
    #[error("{}", without_volume(.0))]
    Level3WithoutVolume(Box<Level3Message>),
    /// The outer ZIP local record or gzip wrapper could not be expanded.
    #[error("unsupported or corrupt compression wrapper: {0}")]
    Compression(String),
}

/// A radar container format [`read_supported_volume_bytes`] can decode
/// from a single byte buffer, in magic-byte sniff precedence order.
///
/// The variants and their order are the one shared routing contract used by
/// local file open, URL polling, and international providers — keep
/// [`sniff_supported_volume_format`] and [`read_supported_volume_bytes`]
/// in lockstep with it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SupportedVolumeFormat {
    /// DORADE sweepfile (solo/Radx descriptor blocks: `COMM`/`SSWB`/`VOLD`/`RADD`).
    Dorade,
    /// HDF5 container, decoded as ODIM_H5 PVOL/SCAN (EUMETNET OPERA Data
    /// Information Model; Michelson et al., OPERA WP 2.1/2.2, v2.2-2.3):
    /// every HDF5 file that is not netCDF-4 CfRadial, and one that cannot be
    /// opened (the ODIM decoder reports why).
    OdimH5,
    /// Classic netCDF (`CDF\x01`/`CDF\x02`, plus CDF-5 sniffed for a useful
    /// rejection), decoded as CfRadial 1.x.
    CfRadial,
    /// netCDF-4 (HDF5) with the CfRadial 1.x layout (`time` and `range`
    /// dimensions in the root group), or any other netCDF-4 file without
    /// the CfRadial 2 layout (the CfRadial decoder reports why it is not
    /// CfRadial, or why its dimensions or variables cannot be rebuilt).
    CfRadialNetcdf4,
    /// netCDF-4 (HDF5) with the CfRadial 2 / FM301 layout: one group per
    /// sweep.
    CfRadial2,
    /// JMA polar-coordinate radar GRIB2 tar (`Z__C_RJTD_*_RDR_JMAGPV*.tar`;
    /// ustar magic at byte 257, JMA GRIB2 templates 3.50120/4.51022/5.200
    /// per the JMA technical format documentation).
    JmaGrib2Tar,
    /// NEXRAD / TDWR Level III product: NOAAPort or WMO/AWIPS framing, or a
    /// bare message (Message Header Block with the block divider at halfword
    /// 10 and the product code repeated at halfword 16), per
    /// [`recast_radar_io_level3::looks_like_level3`].
    NexradLevel3,
    /// Everything else: NEXRAD Archive II / Level II (AR2V, gzip, bzip2,
    /// LDM block-bzip, GR2-style msg31 exports).
    NexradLevel2,
}

/// Sniff which decoder [`read_supported_volume_bytes`] would route to.
///
/// Most magic signatures live in the first 8 bytes, but the JMA tar check
/// reads the first 512-byte tar header block (ustar magic at byte 257), and
/// the DORADE check also validates the first descriptor block length against
/// the buffer length — pass the full buffer when you have it; a short head
/// prefix may sniff DORADE or a JMA tar as Level II. An HDF5 container
/// (signature at 0, or after a 512, 1024, ... byte user block) is told apart
/// by content, which needs the whole file: it is opened (walking its groups
/// and attributes), an ODIM `/what` group or a file without netCDF-4
/// markers is [`SupportedVolumeFormat::OdimH5`], and a netCDF-4 file is
/// [`SupportedVolumeFormat::CfRadial2`] with that layout
/// ([`recast_radar_io_cfradial::cfradial_layout`]), else
/// [`SupportedVolumeFormat::CfRadialNetcdf4`] (a netCDF-4 file with neither
/// CfRadial layout included: the CfRadial decoder says so). A head too short
/// to open sniffs as `OdimH5`.
/// Anything unrecognized falls through to
/// [`SupportedVolumeFormat::NexradLevel2`] so the error surfaces from the
/// Archive II decoder, matching the historical routing chains.
pub fn sniff_supported_volume_format(head: &[u8]) -> SupportedVolumeFormat {
    if dorade::looks_like_dorade_bytes(head) {
        SupportedVolumeFormat::Dorade
    } else if hdf5::looks_like_hdf5_bytes(head) {
        match hdf5::H5File::open(head) {
            Ok(file) => match classify_hdf5(file) {
                Hdf5Route::Odim(_) => SupportedVolumeFormat::OdimH5,
                Hdf5Route::CfRadial(file) => match recast_radar_io_cfradial::cfradial_layout(&file)
                {
                    Some(recast_radar_io_cfradial::CfRadialLayout::CfRadial2) => {
                        SupportedVolumeFormat::CfRadial2
                    }
                    _ => SupportedVolumeFormat::CfRadialNetcdf4,
                },
                Hdf5Route::Netcdf4Error(_) => SupportedVolumeFormat::CfRadialNetcdf4,
            },
            Err(_) => SupportedVolumeFormat::OdimH5,
        }
    } else if recast_radar_io_cfradial::looks_like_netcdf3_bytes(head) {
        SupportedVolumeFormat::CfRadial
    } else if recast_radar_io_jma::looks_like_jma_tar_bytes(head) {
        SupportedVolumeFormat::JmaGrib2Tar
    } else if recast_radar_io_level3::looks_like_level3(head) {
        SupportedVolumeFormat::NexradLevel3
    } else {
        SupportedVolumeFormat::NexradLevel2
    }
}

/// Where an opened HDF5 file goes.
enum Hdf5Route<'a> {
    Odim(hdf5::H5File<'a>),
    CfRadial(Box<recast_radar_io_cfradial::Netcdf4File<'a>>),
    Netcdf4Error(CfRadialError),
}

/// ODIM_H5 when the file has an ODIM `/what` group or no netCDF-4 markers;
/// CfRadial otherwise, whatever its layout: a netCDF-4 file that is neither
/// CfRadial 1 nor 2 (a gridded product, say) gets the CfRadial decoder's
/// "neither CfRadial 1 nor CfRadial 2" error rather than the ODIM decoder's
/// "no /what group". The file is opened once and handed on.
fn classify_hdf5(file: hdf5::H5File<'_>) -> Hdf5Route<'_> {
    if file.has_object("/what") || !hdf5::netcdf4::is_netcdf4(&file) {
        return Hdf5Route::Odim(file);
    }
    match recast_radar_io_cfradial::Netcdf4File::from_hdf5(file) {
        Ok(netcdf) => Hdf5Route::CfRadial(Box::new(netcdf)),
        Err(err) => Hdf5Route::Netcdf4Error(err.into()),
    }
}

/// Decode an HDF5 container by content (see [`classify_hdf5`]).
fn decode_hdf5(bytes: &[u8]) -> Result<Volume, IoError> {
    let file = hdf5::H5File::open(bytes).map_err(OdimError::from)?;
    Ok(match classify_hdf5(file) {
        Hdf5Route::Odim(file) => odim::read_odim_hdf5_volume(file)?,
        Hdf5Route::CfRadial(file) => recast_radar_io_cfradial::read_cfradial_netcdf4(&file)?,
        Hdf5Route::Netcdf4Error(err) => return Err(IoError::CfRadial(err)),
    })
}

/// Format-specific metadata decoded beside a [`Volume`]: typed structs the
/// FM301 model has no slot for (design note section 2).
#[derive(Clone, Debug, PartialEq, Default)]
#[non_exhaustive]
pub enum FormatMetadata {
    /// The source format keeps nothing beside the volume.
    #[default]
    None,
    /// NEXRAD Level II metadata messages and per-sweep constant blocks.
    Nexrad(Box<NexradMetadata>),
    /// The decoded NEXRAD / TDWR Level III product the volume was converted
    /// from: every block and display packet, including the ones the FM301
    /// model has no slot for (symbols, text, graphic and tabular pages).
    Level3(Box<Level3Product>),
}

/// A routed decode: the FM301 volume plus its format metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    /// The decoded volume.
    pub volume: Volume,
    /// The format's metadata beside it.
    pub metadata: FormatMetadata,
}

/// Decode any supported single-buffer radar container by magic bytes:
/// DORADE → HDF5 (ODIM_H5, or netCDF-4 CfRadial 1.x or 2 by content) →
/// CfRadial 1.x (classic netCDF) → JMA GRIB2 tar → NEXRAD Level III → NEXRAD
/// Archive II fallback.
///
/// This is the one shared router for bytes of unknown provenance (local
/// file open, custom URL polling, international feed downloads). Errors are
/// the dispatched decoder's error, displayed unchanged — callers add their
/// own source context (file name, URL). Never panics on malformed input.
///
/// JMA tars are multi-station archives (one GRIB2 member per radar of the
/// national network); this router decodes the FIRST station only, because
/// its contract is one volume per buffer. Providers that need a specific
/// station call [`recast_radar_io_jma::read_jma_tar_volumes`] with a
/// `site_filter` directly instead of going through the router.
///
/// Only the volume is decoded; [`read_supported_volume_with_metadata`] also
/// reads the format's metadata.
pub fn read_supported_volume_bytes(raw: &[u8]) -> Result<Volume, IoError> {
    route(raw, false).map(|decoded| decoded.volume)
}

/// [`read_supported_volume_bytes`] plus the format's typed metadata (NEXRAD
/// Level II metadata messages, the decoded Level III product; nothing for
/// the other formats yet).
pub fn read_supported_volume_with_metadata(raw: &[u8]) -> Result<Decoded, IoError> {
    route(raw, true)
}

/// The outer containers of a buffer, expanded: a single-member ZIP local
/// record and a whole-file gzip wrapper.
pub(crate) struct Unwrapped {
    zip: Option<Vec<u8>>,
    gzip: Option<Vec<u8>>,
}

impl Unwrapped {
    /// The bytes after the ZIP record (the decoder input for Archive II,
    /// whose own gzip handling stays intact).
    pub(crate) fn raw<'a>(&'a self, original: &'a [u8]) -> &'a [u8] {
        self.zip.as_deref().unwrap_or(original)
    }

    /// The bytes after the gzip wrapper too (what the format sniff sees).
    pub(crate) fn sniff<'a>(&'a self, original: &'a [u8]) -> &'a [u8] {
        self.gzip.as_deref().unwrap_or(self.raw(original))
    }

    pub(crate) fn gzip_expanded(&self) -> bool {
        self.gzip.is_some()
    }
}

pub(crate) fn unwrap_containers(raw: &[u8]) -> Result<Unwrapped, IoError> {
    let zip = if mobile_archive::looks_like_zip_bytes(raw) {
        Some(decompress_zip_local_member_bytes(raw)?)
    } else {
        None
    };
    let inner = zip.as_deref().unwrap_or(raw);
    let gzip = if inner.starts_with(&[0x1f, 0x8b]) {
        Some(decompress_gzip_bytes(inner)?)
    } else {
        None
    };
    Ok(Unwrapped { zip, gzip })
}

fn route(original: &[u8], with_metadata: bool) -> Result<Decoded, IoError> {
    let unwrapped = unwrap_containers(original)?;
    let raw = unwrapped.raw(original);
    let sniff_bytes = unwrapped.sniff(original);
    // HDF5 is classified by content once the file is open; sniffing first
    // would open it twice.
    let format = if !dorade::looks_like_dorade_bytes(sniff_bytes)
        && hdf5::looks_like_hdf5_bytes(sniff_bytes)
    {
        SupportedVolumeFormat::OdimH5
    } else {
        sniff_supported_volume_format(sniff_bytes)
    };
    let volume = match format {
        SupportedVolumeFormat::Dorade => dorade::read_dorade_sweep_volume(sniff_bytes)?,
        SupportedVolumeFormat::OdimH5
        | SupportedVolumeFormat::CfRadialNetcdf4
        | SupportedVolumeFormat::CfRadial2 => decode_hdf5(sniff_bytes)?,
        SupportedVolumeFormat::CfRadial => {
            recast_radar_io_cfradial::read_cfradial1_volume(sniff_bytes)?
        }
        SupportedVolumeFormat::JmaGrib2Tar => {
            recast_radar_io_jma::read_jma_tar_first_station(sniff_bytes)?
        }
        SupportedVolumeFormat::NexradLevel3 => {
            let product = match recast_radar_io_level3::decode_message(sniff_bytes)? {
                Level3Message::Product(product) => product,
                other => return Err(IoError::Level3WithoutVolume(Box::new(other))),
            };
            let volume = match product.to_volume() {
                Ok(volume) => volume,
                Err(Level3Error::NoDataArray { .. }) => {
                    return Err(IoError::Level3WithoutVolume(Box::new(
                        Level3Message::Product(product),
                    )));
                }
                Err(err) => return Err(err.into()),
            };
            let metadata = if with_metadata {
                FormatMetadata::Level3(product)
            } else {
                FormatMetadata::None
            };
            return Ok(Decoded { volume, metadata });
        }
        SupportedVolumeFormat::NexradLevel2 => {
            if with_metadata {
                let decoded = recast_radar_io_nexrad::read_volume_with_metadata(raw)?;
                return Ok(Decoded {
                    volume: decoded.volume,
                    metadata: FormatMetadata::Nexrad(Box::new(decoded.metadata)),
                });
            }
            if unwrapped.gzip_expanded() {
                // The router already expanded gzip to inspect its inner
                // format. Parse those normalized bytes directly instead of
                // retaining them while inflating the same payload again.
                recast_radar_io_nexrad::read_normalized_volume_bytes(
                    sniff_bytes,
                    ArchiveCompression::Gzip,
                )?
            } else {
                recast_radar_io_nexrad::read_volume_from_bytes(raw)?
            }
        }
    };
    Ok(Decoded {
        volume,
        metadata: FormatMetadata::None,
    })
}

/// Decode every radar volume in a mobile-radar zip archive, with `.msg31`
/// and `AR2V` members decoded by the NEXRAD Level II decoder. See
/// [`recast_radar_io_dorade::mobile_archive::read_mobile_archive_from_path`].
pub fn read_mobile_archive_from_path(path: &Path) -> Result<Vec<MobileVolume>, IoError> {
    Ok(mobile_archive::read_mobile_archive_from_path(
        path,
        recast_radar_io_nexrad::read_volume_from_bytes,
    )?)
}

/// Decode every radar volume under a mobile-radar deployment folder, with
/// Level II members decoded by the NEXRAD Level II decoder. See
/// [`recast_radar_io_dorade::mobile_archive::read_mobile_dir_from_path`].
pub fn read_mobile_dir_from_path(dir: &Path) -> Result<Vec<MobileVolume>, IoError> {
    Ok(mobile_archive::read_mobile_dir_from_path(
        dir,
        recast_radar_io_nexrad::read_volume_from_bytes,
    )?)
}

/// Inflate a whole-file gzip wrapper, every member, in one pass: the same
/// path the Level II decoder takes for `.gz` volumes, so a multi-member file
/// decodes the same whichever way it is opened.
fn decompress_gzip_bytes(raw: &[u8]) -> Result<Vec<u8>, IoError> {
    recast_radar_io_nexrad::gzip::inflate_gzip_members_limited(
        raw,
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
    use flate2::read::GzDecoder;
    use std::io::Read;

    fn corpus(id: &str) -> Vec<u8> {
        recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{err}"))
    }

    /// Leading bytes of committed corpus files in each format, sniffed in
    /// router order; the expected route is each manifest entry's `format`.
    #[test]
    fn sniffs_supported_volume_formats_in_router_order() {
        for (id, expected) in [
            // format dorade (big-endian COW2 and little-endian DOW6 / NOXP)
            (
                "dorade-cow2-20260521-225514-sur-head24",
                SupportedVolumeFormat::Dorade,
            ),
            (
                "dorade-dow6-20211230-222139-rhi-head41",
                SupportedVolumeFormat::Dorade,
            ),
            (
                "dorade-noxp-20090501-190244-ppi",
                SupportedVolumeFormat::Dorade,
            ),
            // format odim-h5 (HDF5 without netCDF-4 markers; also behind a
            // 512-byte user block)
            (
                "odim-bejab-20190606-0000-pvol",
                SupportedVolumeFormat::OdimH5,
            ),
            (
                "odim-dkrom-20260820-1130-pvol-h5latest-trim",
                SupportedVolumeFormat::OdimH5,
            ),
            // HDF5 containers told apart by content: netCDF-4 CfRadial 1
            // (root time/range dimensions) and CfRadial 2 (sweep groups)
            (
                "cfrad1-xsapr-sgp-20110520-ppi-netcdf4",
                SupportedVolumeFormat::CfRadialNetcdf4,
            ),
            (
                "cfrad2-radx-irene-sr2-20110827-120420-sur-r30km",
                SupportedVolumeFormat::CfRadial2,
            ),
            (
                "cfrad2-xradar-xsapr-sgp-20110520-ppi",
                SupportedVolumeFormat::CfRadial2,
            ),
            // format cfradial1 (classic netCDF)
            (
                "cfrad1-xsapr-sgp-20110520-ppi-classic",
                SupportedVolumeFormat::CfRadial,
            ),
            (
                "cfrad1-irene-sr2-20110827-120420-sur-sweeps01",
                SupportedVolumeFormat::CfRadial,
            ),
            // format jma-grib2-tar
            (
                "jma-n5-20191012-090000-rs47773",
                SupportedVolumeFormat::JmaGrib2Tar,
            ),
            // format nexrad-level2 (AR2V0006 and ARCHIVE2 headers)
            (
                "l2-ktlx-20240315-000217-trim",
                SupportedVolumeFormat::NexradLevel2,
            ),
            (
                "l2-ktlx-19990504-002218-trim",
                SupportedVolumeFormat::NexradLevel2,
            ),
            // format zip-local-member: the router unwraps it before sniffing,
            // so the raw record itself falls through.
            (
                "odim-au24-20260610-000300-nci-zip-member",
                SupportedVolumeFormat::NexradLevel2,
            ),
        ] {
            // The COW2 and JMA files are not redistributed: each is checked
            // only when cached.
            let Some(bytes) = recast_radar_testdata::bytes_if_available(id) else {
                continue;
            };
            assert_eq!(sniff_supported_volume_format(&bytes), expected, "{id}");
        }

        // Telling HDF5 containers apart needs the whole file: a netCDF-4
        // head too short to open sniffs as ODIM (whose decoder reports it).
        let netcdf4 = corpus("cfrad1-xsapr-sgp-20110520-ppi-netcdf4");
        assert_eq!(
            sniff_supported_volume_format(&netcdf4[..4096]),
            SupportedVolumeFormat::OdimH5
        );
        // A JMA tar needs its first 512-byte header block.
        if let Some(jma) =
            recast_radar_testdata::bytes_if_available("jma-n5-20191012-090000-rs47773")
        {
            assert_eq!(
                sniff_supported_volume_format(&jma[..511]),
                SupportedVolumeFormat::NexradLevel2
            );
            // The same real ustar header with the member name no longer a
            // Z__C_RJTD_*_RDR_JMAGPV name is a generic tar and falls through.
            let mut renamed = jma.clone();
            assert_eq!(&renamed[..10], b"Z__C_RJTD_");
            renamed[..2].copy_from_slice(b"X_");
            assert_eq!(
                sniff_supported_volume_format(&renamed),
                SupportedVolumeFormat::NexradLevel2
            );
        }
        // CDF-3 does not exist: the classic header relabelled as version 3.
        let mut cdf3 = corpus("cfrad1-xsapr-sgp-20110520-ppi-classic");
        cdf3[3] = 3;
        assert_eq!(
            sniff_supported_volume_format(&cdf3),
            SupportedVolumeFormat::NexradLevel2
        );
        assert_eq!(
            sniff_supported_volume_format(b""),
            SupportedVolumeFormat::NexradLevel2
        );
    }

    /// Downloaded corpus files: a whole-file gzip Archive II object and a
    /// generic (non-JMA) ustar archive both fall through to Level II.
    #[test]
    fn sniffs_gzip_archive_and_generic_tar_as_level2_fallthrough() {
        let kvwx = std::fs::read(recast_radar_testdata::require_file!(
            "l2-kvwx-20080415-235337"
        ))
        .expect("read kvwx");
        assert_eq!(kvwx[..2], [0x1f, 0x8b], "gzip member header");
        assert_eq!(
            sniff_supported_volume_format(&kvwx),
            SupportedVolumeFormat::NexradLevel2
        );

        // First tar header of the NOXP archive (a ustar directory entry).
        let tgz = std::fs::read(recast_radar_testdata::require_file!(
            "dorade-noxp-20090501-sweeps-tgz"
        ))
        .expect("read NOXP archive");
        let mut head = vec![0u8; 1024];
        GzDecoder::new(tgz.as_slice())
            .read_exact(&mut head)
            .expect("inflate tar head");
        assert_eq!(&head[257..262], b"ustar");
        assert!(head.starts_with(b"2009/NOX/sweep/0501"));
        assert_eq!(
            sniff_supported_volume_format(&head),
            SupportedVolumeFormat::NexradLevel2
        );
    }

    #[test]
    fn router_stringifies_level2_error_for_unrecognized_bytes() {
        // Short unrecognized bytes fail the Archive II volume-header check;
        // the router must surface that exact decoder message.
        let direct_err = recast_radar_io_nexrad::read_volume_from_bytes(b"not radar")
            .expect_err("short garbage must not decode")
            .to_string();
        let routed_err = read_supported_volume_bytes(b"not radar")
            .expect_err("short garbage must not decode")
            .to_string();
        assert_eq!(routed_err, direct_err);
        assert!(
            routed_err.contains("too short for an Archive II volume header"),
            "unexpected error text: {routed_err}"
        );
    }

    /// Input: corpus entry `odim-au24-20260610-000300-nci-zip-member`, the
    /// unmodified NCI THREDDS response for a member of a daily
    /// `24_20260610.pvol.zip`: one ZIP local-file record (no central
    /// directory) followed by the start of the next record.
    ///
    /// Expected values: tools/golden_io_formats.py, section `router`, key
    /// `nci_zip_member` (PKWARE APPNOTE local header read with `struct`,
    /// member inflated with `zlib`, CRC-32 checked, opened with h5py).
    #[test]
    fn unwraps_zip_local_member_stream_without_central_directory() {
        let response = corpus("odim-au24-20260610-000300-nci-zip-member");
        assert!(mobile_archive::looks_like_zip_bytes(&response));
        // flags 0, method 8 (deflate), 85154 compressed bytes from offset 84;
        // 269511 trailing bytes start with the next local-header signature.
        assert_eq!(u16::from_le_bytes([response[8], response[9]]), 8);
        assert_eq!(&response[84 + 85_154..84 + 85_154 + 4], b"PK\x03\x04");

        let member = decompress_zip_local_member_bytes(&response).expect("unwrap member");
        assert_eq!(member.len(), 354_749);
        assert_eq!(
            recast_radar_testdata::sha256_hex(&member),
            "3d5bac474eaefed70d82a6567c26ab93d992e1118c7c5a50888e36e952c2a75a"
        );
        assert!(hdf5::looks_like_hdf5_bytes(&member));

        // The router decodes the unwrapped ODIM PVOL exactly like the direct
        // decoder. h5py: source RAD:AU24,PLC:Bowen; 10 sweeps, stored from
        // 32 deg down to 0.8 deg (kept in file order); the lowest has 360
        // rays x 958 bins.
        let direct = odim::read_odim_h5_volume(&member).expect("direct ODIM decode");
        let routed = read_supported_volume_bytes(&response).expect("routed decode");
        assert_eq!(routed.attrs.instrument_name, "AU24");
        assert_eq!(routed.attrs.site_name.as_deref(), Some("Bowen"));
        let elevations: Vec<f32> = routed
            .sweeps
            .iter()
            .map(|sweep| sweep.fixed_angle_deg)
            .collect();
        assert_eq!(
            elevations,
            [32.0, 22.0, 16.0, 11.5, 8.0, 5.6, 3.6, 2.4, 1.6, 0.8]
        );
        assert_eq!(routed.sweeps[9].nrays(), 360);
        assert_eq!(routed.sweeps[9].range.ngates(), 958);
        assert_eq!(routed, direct);
    }

    /// A netCDF-4 file with neither CfRadial layout (a gridded product, say)
    /// goes to the CfRadial decoder, whose error says what the file is not.
    /// It went to the ODIM decoder, whose "no /what group" error told the
    /// caller to use the router the call had come through.
    #[test]
    fn a_netcdf4_file_without_a_cfradial_layout_gets_the_cfradial_error() {
        use hdf5::write::Data;
        use hdf5::write::netcdf4::{NcAttr, NcStorage, NcVariable, NcWriter};

        let mut nc = NcWriter::new();
        let root = nc.root();
        nc.add_dim(root, "x", 3).expect("dimension");
        nc.add_variable(
            root,
            NcVariable {
                name: "x".into(),
                dims: vec!["x".into()],
                data: Data::F32(vec![-1000.0, 0.0, 1000.0]),
                attrs: vec![("units".into(), NcAttr::Text("m".into()))],
                storage: NcStorage::Contiguous,
            },
        )
        .expect("variable");
        let bytes = nc.finish().expect("netCDF-4 bytes");
        assert_eq!(
            sniff_supported_volume_format(&bytes),
            SupportedVolumeFormat::CfRadialNetcdf4
        );
        let err = read_supported_volume_bytes(&bytes).expect_err("not a radar volume");
        assert!(matches!(err, IoError::CfRadial(_)), "{err}");
        assert!(
            err.to_string()
                .contains("neither CfRadial 1 (root time/range dimensions) nor CfRadial 2"),
            "{err}"
        );
    }
}
