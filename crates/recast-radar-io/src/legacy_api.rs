//! Pre-FM301 signatures of the router, kept until the legacy model is
//! removed at the end of the FM301 migration (F.3;
//! `docs/design/fm301-model.md` section 13.3). Only this module names legacy
//! model items.
//!
//! Each wrapper dispatches to the matching decoder crate's own legacy
//! wrapper, so the legacy output of the router equals the legacy output of
//! the decoder it routes to, as before the migration.

use std::path::Path;

use recast_radar_core::RadarVolume;
use recast_radar_io_dorade::mobile_archive::{self, MobileRadarVolume};
use recast_radar_io_nexrad::ArchiveCompression;
use recast_radar_io_odim::odim;

use crate::{IoError, SupportedVolumeFormat, sniff_supported_volume_format, unwrap_containers};

/// Decode any supported single-buffer radar container into the legacy
/// model. See [`crate::read_supported_volume_bytes`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_supported_volume_bytes")
)]
pub fn decode_supported_volume_bytes(original: &[u8]) -> Result<RadarVolume, IoError> {
    let unwrapped = unwrap_containers(original)?;
    let raw = unwrapped.raw(original);
    let sniff_bytes = unwrapped.sniff(original);
    match sniff_supported_volume_format(sniff_bytes) {
        SupportedVolumeFormat::Dorade => Ok(
            recast_radar_io_dorade::dorade::decode_dorade_sweep_volume(sniff_bytes)?,
        ),
        SupportedVolumeFormat::OdimH5 => Ok(odim::decode_odim_h5_volume(sniff_bytes)?),
        SupportedVolumeFormat::CfRadial => Ok(recast_radar_io_cfradial::decode_cfradial1_volume(
            sniff_bytes,
        )?),
        SupportedVolumeFormat::JmaGrib2Tar => Ok(
            recast_radar_io_jma::decode_jma_tar_first_station(sniff_bytes)?,
        ),
        SupportedVolumeFormat::NexradLevel2 => {
            if unwrapped.gzip_expanded() {
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

/// Decode every radar volume in a mobile-radar zip archive into the legacy
/// model. See [`crate::read_mobile_archive_from_path`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_mobile_archive_from_path")
)]
pub fn decode_mobile_archive_from_path(path: &Path) -> Result<Vec<MobileRadarVolume>, IoError> {
    Ok(mobile_archive::decode_mobile_archive_from_path(
        path,
        recast_radar_io_nexrad::decode_volume_from_bytes,
    )?)
}

/// Decode every radar volume under a mobile-radar deployment folder into
/// the legacy model. See [`crate::read_mobile_dir_from_path`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_mobile_dir_from_path")
)]
pub fn decode_mobile_dir_from_path(dir: &Path) -> Result<Vec<MobileRadarVolume>, IoError> {
    Ok(mobile_archive::decode_mobile_dir_from_path(
        dir,
        recast_radar_io_nexrad::decode_volume_from_bytes,
    )?)
}
