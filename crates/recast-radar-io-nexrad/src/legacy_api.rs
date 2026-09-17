//! Pre-FM301 signatures of the Level II decoder, kept until the legacy model
//! is removed at the end of the FM301 migration (F.3;
//! `docs/design/fm301-model.md` section 13.3). Only this module names legacy
//! model items.
//!
//! Every wrapper runs the native decoder with a per-ray log of the values the
//! FM301 model does not keep (radial status, each radial's first gate range,
//! the record's millisecond-of-day time) and converts with
//! `legacy::legacy_from_volume`. The result is identical to the pre-migration
//! decoder's output: gate buffers move, nothing is re-decoded.

use std::io::Read;
use std::path::Path;

use chrono::{DateTime, Utc};
use recast_radar_core::bounded_read::DecodeBudget;
use recast_radar_core::legacy::{
    self, FieldResidue, LegacyConvention, LegacyResidue, SweepResidue,
};
use recast_radar_core::model::Volume;
use recast_radar_core::{GateRange, RadarVolume};

use crate::builder::{SweepState, VolumeBuilder};
use crate::{
    ArchiveCompression, NexradError, RadialStatus, Result, builder_bzip_block_preview,
    builder_from_bytes, builder_from_gzip_bytes_with_preview, builder_from_gzip_reader,
    builder_from_normalized, builder_gzip_preview, builder_with_bzip_preview, read_file,
};

/// Decode a local Archive II / Level II file into the legacy model.
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_volume_from_path")
)]
pub fn decode_volume_from_path(path: &Path) -> Result<RadarVolume> {
    let mut volume = decode_volume_from_bytes(&read_file(path)?)?;
    volume.metadata.source_path = Some(path.display().to_string());
    Ok(volume)
}

/// Decode a byte slice into the legacy model.
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_volume_from_bytes")
)]
pub fn decode_volume_from_bytes(bytes: &[u8]) -> Result<RadarVolume> {
    legacy_volume(builder_from_bytes(bytes, true)?)
}

#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_gzip_volume_from_reader")
)]
pub fn decode_gzip_volume_from_reader(reader: impl Read) -> Result<RadarVolume> {
    legacy_volume(builder_from_gzip_reader(reader, true)?)
}

#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_gzip_volume_from_bytes_with_preview")
)]
pub fn decode_gzip_volume_from_bytes_with_preview<F>(
    raw: &[u8],
    min_displayable_radials: usize,
    mut on_preview: F,
) -> Result<RadarVolume>
where
    F: FnMut(RadarVolume),
{
    legacy_volume(builder_from_gzip_bytes_with_preview(
        raw,
        min_displayable_radials,
        true,
        |builder| {
            on_preview(legacy_snapshot(builder)?);
            Ok(())
        },
    )?)
}

#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_gzip_preview_from_bytes")
)]
pub fn decode_gzip_preview_from_bytes(
    raw: &[u8],
    min_displayable_radials: usize,
) -> Result<Option<RadarVolume>> {
    builder_gzip_preview(raw, min_displayable_radials, true)?
        .map(legacy_volume)
        .transpose()
}

/// Decode a completed first displayable cut from NEXRAD block-bzip Level II
/// bytes into the legacy model.
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_bzip_block_preview_from_bytes")
)]
pub fn decode_bzip_block_preview_from_bytes(
    raw: &[u8],
    min_displayable_radials: usize,
) -> Result<Option<RadarVolume>> {
    builder_bzip_block_preview(raw, min_displayable_radials, true)?
        .map(legacy_volume)
        .transpose()
}

/// Decode a full volume into the legacy model while optionally emitting an
/// early completed first-cut preview.
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_volume_from_bytes_with_bzip_preview")
)]
pub fn decode_volume_from_bytes_with_bzip_preview<F>(
    raw: &[u8],
    min_displayable_radials: usize,
    mut on_preview: F,
) -> Result<RadarVolume>
where
    F: FnMut(RadarVolume),
{
    legacy_volume(builder_with_bzip_preview(
        raw,
        min_displayable_radials,
        true,
        |builder| {
            on_preview(legacy_snapshot(builder)?);
            Ok(())
        },
    )?)
}

/// Parse already-normalized Archive II bytes into the legacy model.
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_normalized_volume_bytes")
)]
pub fn decode_normalized_volume_bytes(
    bytes: &[u8],
    compression: ArchiveCompression,
) -> Result<RadarVolume> {
    legacy_volume(builder_from_normalized(
        bytes,
        compression,
        DecodeBudget::volume(),
        true,
    )?)
}

fn legacy_volume(builder: VolumeBuilder) -> Result<RadarVolume> {
    let volume_time = builder.header_time;
    let (volume, states) = builder.finish()?;
    to_legacy(volume, &states, volume_time)
}

fn legacy_snapshot(builder: &VolumeBuilder) -> Result<RadarVolume> {
    to_legacy(builder.snapshot()?, &builder.sweeps, builder.header_time)
}

/// `volume` (sealed) and its per-sweep logs to the legacy model.
fn to_legacy(
    mut volume: Volume,
    states: &[SweepState],
    volume_time: DateTime<Utc>,
) -> Result<RadarVolume> {
    // The legacy decoder left the scan name and id unset for NEXRAD, and
    // used the opening radial's elevation as the cut elevation.
    volume.scan.name = None;
    volume.scan.id = None;
    let mut sweeps = Vec::with_capacity(volume.sweeps.len());
    for (sweep, state) in volume.sweeps.iter_mut().zip(states) {
        let Some(log) = state.legacy.as_ref() else {
            return Err(internal("legacy ray log missing"));
        };
        if log.times_ms.len() != sweep.nrays() {
            return Err(internal("legacy ray log does not match the rays"));
        }
        sweep.fixed_angle_deg = state.first_elevation_deg;
        // A moment missing from some radials: the legacy grid held only the
        // provided rows.
        let fields = sweep
            .fields
            .iter()
            .filter(|field| !field.absent_rows.is_empty())
            .map(|field| {
                let (center, spacing) = field.native_geometry(&sweep.range).unwrap_or_default();
                FieldResidue {
                    moment: field.name.to_legacy_moment(LegacyConvention::Nexrad),
                    name: field.name.clone(),
                    gate_range: GateRange {
                        first_gate_m: center.round() as i32,
                        gate_spacing_m: spacing.round() as i32,
                        gate_count: field.ngates as usize,
                    },
                    nodata: Some(0),
                    range_folded: Some(1),
                    radial_indices: Some(
                        (0..field.nrays as usize)
                            .filter(|ray| !field.is_absent(*ray))
                            .collect(),
                    ),
                    float_scale_offset: None,
                }
            })
            .collect();
        sweeps.push(SweepResidue {
            fields,
            ..SweepResidue::default()
        });
    }
    let residue = LegacyResidue {
        volume_time,
        scan_mode: None,
        pulse_width_us: None,
        unambiguous_range_km: None,
        sweeps,
    };
    let mut legacy = legacy::legacy_from_volume(volume, Some(&residue), LegacyConvention::Nexrad)
        .map_err(|err| internal(&err.to_string()))?;
    // Per-radial values the FM301 model does not keep, from the log.
    legacy.volume_time = volume_time;
    for (cut, state) in legacy.cuts.iter_mut().zip(states) {
        let Some(log) = state.legacy.as_ref() else {
            continue;
        };
        for (index, radial) in cut.radials.iter_mut().enumerate() {
            radial.time_offset_ms = log.times_ms[index];
            radial.radial_status = Some(legacy_status(log.statuses[index]));
            let gates = log.gates[index];
            radial.gate_range = GateRange {
                first_gate_m: gates.first_gate_m,
                gate_spacing_m: gates.gate_spacing_m,
                gate_count: gates.gate_count,
            };
        }
    }
    Ok(legacy)
}

fn legacy_status(status: RadialStatus) -> recast_radar_core::RadialStatus {
    use recast_radar_core::RadialStatus as Legacy;
    match status {
        RadialStatus::StartElevation => Legacy::StartElevation,
        RadialStatus::Intermediate => Legacy::Intermediate,
        RadialStatus::EndElevation => Legacy::EndElevation,
        RadialStatus::StartVolume => Legacy::StartVolume,
        RadialStatus::EndVolume => Legacy::EndVolume,
        RadialStatus::StartElevationLastCut => Legacy::StartElevationLastCut,
        RadialStatus::Unknown(code) => Legacy::Unknown(code),
    }
}

fn internal(reason: &str) -> NexradError {
    NexradError::InvalidMessage {
        offset: 0,
        reason: format!("legacy conversion: {reason}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use recast_radar_core::MomentType;
    use recast_radar_core::model::FieldName;

    /// The legacy wrapper reproduces the pre-migration decoder's per-radial
    /// values (opening-radial cut elevation, millisecond-of-day times, radial
    /// status, unset scan name) and the same gate values as the native model.
    #[test]
    fn legacy_wrapper_matches_native_decode_of_real_chunks() {
        let bytes = crate::tests::kiwa_chunk_prefix_normalized();
        let volume =
            crate::read_normalized_volume_bytes(&bytes, ArchiveCompression::Bzip2Blocks).unwrap();
        let sweep = &volume.sweeps[0];
        let reflectivity = sweep.field(&FieldName::Dbzh).unwrap();

        let legacy =
            decode_normalized_volume_bytes(&bytes, ArchiveCompression::Bzip2Blocks).unwrap();
        assert_eq!(legacy.cuts.len(), 1);
        assert_eq!(legacy.cuts[0].elevation_deg, sweep.rays.elevation_deg[0]);
        assert_ne!(legacy.cuts[0].elevation_deg, sweep.fixed_angle_deg);
        assert_eq!(legacy.cuts[0].radials.len(), 240);
        assert_eq!(legacy.metadata.scan_name, None);
        assert_eq!(legacy.vcp.map(|vcp| vcp.pattern), volume.scan.vcp_pattern);
        let grid = legacy.cuts[0]
            .moments
            .get(&MomentType::Reflectivity)
            .unwrap();
        assert_eq!(grid.radial_indices.len(), 240);
        for ray in [0usize, 119, 239] {
            let radial = &legacy.cuts[0].radials[ray];
            assert_eq!(radial.azimuth_deg, sweep.rays.azimuth_deg[ray]);
            let instant = volume.ray_time(0, ray).unwrap();
            let midnight = instant
                .date_naive()
                .and_time(chrono::NaiveTime::MIN)
                .and_utc();
            assert_eq!(
                i64::from(radial.time_offset_ms),
                (instant - midnight).num_milliseconds()
            );
            assert!(radial.radial_status.is_some());
            for gate in [0usize, 100, 1000] {
                assert_eq!(grid.scaled_value(ray, gate), reflectivity.value(ray, gate));
            }
        }
    }
}
