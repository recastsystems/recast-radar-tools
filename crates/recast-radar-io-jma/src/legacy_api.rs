//! Pre-FM301 signatures of the JMA decoder, kept until the legacy model is
//! removed at the end of the FM301 migration (F.3;
//! `docs/design/fm301-model.md` section 13.3). Only this module names legacy
//! model items.
//!
//! The wrappers run the native decoder and convert through
//! `legacy::legacy_from_volume` (the physical `f32` planes move; nothing is
//! copied), then restore the legacy radial values the FM301 model derives
//! differently: zero time offsets, the rounded gate range and the
//! one-based cut numbers.

use recast_radar_core::RadarVolume;
use recast_radar_core::legacy::{LegacyConvention, legacy_from_volume};
use recast_radar_core::model::{RangeCoord, Volume};

use crate::{JmaError, read_jma_tar_first_station, read_jma_tar_volumes};

/// Decode a JMA radar GRIB2 tar into one legacy `RadarVolume` per station.
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_jma_tar_volumes")
)]
pub fn decode_jma_tar_volumes(
    bytes: &[u8],
    site_filter: Option<&str>,
) -> Result<Vec<RadarVolume>, JmaError> {
    read_jma_tar_volumes(bytes, site_filter)?
        .into_iter()
        .map(legacy_volume)
        .collect()
}

/// Decode only the FIRST station of a JMA tar into the legacy model.
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_jma_tar_first_station")
)]
pub fn decode_jma_tar_first_station(bytes: &[u8]) -> Result<RadarVolume, JmaError> {
    legacy_volume(read_jma_tar_first_station(bytes)?)
}

fn legacy_volume(volume: Volume) -> Result<RadarVolume, JmaError> {
    let gate_ranges: Vec<recast_radar_core::GateRange> = volume
        .sweeps
        .iter()
        .map(|sweep| {
            let (first, spacing) = match &sweep.range {
                RangeCoord::Uniform {
                    first_center_m,
                    spacing_m,
                    ..
                } => (*first_center_m, *spacing_m),
                RangeCoord::Explicit { centers_m } => (
                    centers_m.first().map_or(0.0, |c| f64::from(*c)),
                    match centers_m.as_slice() {
                        [first, second, ..] => f64::from(*second) - f64::from(*first),
                        _ => 0.0,
                    },
                ),
            };
            recast_radar_core::GateRange {
                first_gate_m: first.round() as i32,
                gate_spacing_m: (spacing.round() as i32).max(1),
                gate_count: sweep.range.ngates(),
            }
        })
        .collect();
    let mut legacy = legacy_from_volume(volume, None, LegacyConvention::Jma)
        .map_err(|err| JmaError::Decode(format!("legacy conversion: {err}")))?;
    for (index, (cut, gate_range)) in legacy.cuts.iter_mut().zip(gate_ranges).enumerate() {
        cut.elevation_number = u8::try_from(index + 1).ok();
        for radial in &mut cut.radials {
            radial.time_offset_ms = 0;
            radial.gate_range = gate_range.clone();
        }
        for grid in cut.moments.values_mut() {
            grid.gate_range = gate_range.clone();
        }
    }
    Ok(legacy)
}
