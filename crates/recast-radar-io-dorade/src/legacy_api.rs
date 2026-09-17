//! Pre-FM301 signatures of the DORADE decoders, kept until the legacy model
//! is removed at the end of the FM301 migration (F.3;
//! `docs/design/fm301-model.md` section 13.3). Only this module names legacy
//! model items.
//!
//! The wrappers run the native decoder and fold the result onto the legacy
//! model exactly as the pre-migration decoder built it: parameter names
//! collapse onto the canonical moments through `canonical_moment` (first
//! match in PARM order wins), 8/16-bit fields shift into unsigned storage
//! (`+128` / `+32768`, the legacy `(raw - offset) / scale` form), the gate
//! range is the rounded centre-of-first-cell form, ray times are
//! milliseconds since the sweep start, and cuts are sorted by elevation.

use std::collections::BTreeSet;
use std::fmt::Display;
use std::path::Path;

use chrono::{DateTime, Utc};
use recast_radar_core::legacy::{
    FieldResidue, LegacyConvention, LegacyResidue, SweepResidue, legacy_from_volume,
};
use recast_radar_core::model::{
    FieldData, FieldName, IntCoding, LinearTransform, RangeCoord, Sweep, SweepMode,
};
use recast_radar_core::{GateRange, RadarVolume, ScanMode, canonical_moment};

use crate::dorade::{DoradeVolumeBuilder, SweepLog, invalid};
use crate::mobile_archive::{self, MobileDecode, MobileVolume};
use crate::{DoradeError, Result};

/// Decode one sweepfile into a fresh single-cut legacy volume.
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_dorade_sweep_volume")
)]
pub fn decode_dorade_sweep_volume(bytes: &[u8]) -> Result<RadarVolume> {
    let mut builder = DoradeVolumeBuilder::new();
    builder.append(bytes)?;
    legacy_volume(builder)
}

/// Decode a set of sweepfiles forming one volume scan (legacy model).
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_dorade_volume_from_slices")
)]
pub fn decode_dorade_volume_from_slices<S: AsRef<[u8]>>(sweeps: &[S]) -> Result<RadarVolume> {
    if sweeps.is_empty() {
        return Err(invalid(0, "no DORADE sweeps to decode"));
    }
    let mut builder = DoradeVolumeBuilder::new();
    for sweep in sweeps {
        builder.append(sweep.as_ref())?;
    }
    legacy_volume(builder)
}

/// Decode a set of sweepfile paths forming one volume scan (legacy model).
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_dorade_volume_from_paths")
)]
pub fn decode_dorade_volume_from_paths<P: AsRef<Path>>(paths: &[P]) -> Result<RadarVolume> {
    if paths.is_empty() {
        return Err(invalid(0, "no DORADE sweep paths to decode"));
    }
    let mut builder = DoradeVolumeBuilder::new();
    for path in paths {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|source| DoradeError::Io {
            path: path.display().to_string(),
            source,
        })?;
        builder.append(&bytes)?;
    }
    let mut volume = legacy_volume(builder)?;
    volume.metadata.source_path = Some(paths[0].as_ref().display().to_string());
    Ok(volume)
}

/// Decode one sweepfile and append it as a cut on a legacy volume.
///
/// The first appended sweep populates the site, volume time, and metadata;
/// later sweeps must come from the same instrument.
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use DoradeVolumeBuilder::append")
)]
pub fn append_dorade_sweep(bytes: &[u8], volume: &mut RadarVolume) -> Result<()> {
    let mut builder = DoradeVolumeBuilder::new();
    builder.append(bytes)?;
    let mut decoded = legacy_volume(builder)?;
    let Some(cut) = decoded.cuts.pop() else {
        return Err(invalid(0, "DORADE sweep contains no rays"));
    };
    if volume.site.id.is_empty() {
        volume.site = decoded.site;
        volume.metadata.archive_version = decoded.metadata.archive_version;
        volume.metadata.compression = decoded.metadata.compression;
        volume.metadata.scan_mode = decoded.metadata.scan_mode;
        volume.metadata.radar_frequency_mhz = decoded.metadata.radar_frequency_mhz;
    } else if volume.site.id != decoded.site.id {
        return Err(invalid(
            0,
            format!(
                "DORADE sweep instrument '{}' does not match volume '{}'",
                decoded.site.id, volume.site.id
            ),
        ));
    }
    if decoded.metadata.decoded_radial_count > 0
        && (volume.cuts.is_empty() || decoded.volume_time < volume.volume_time)
    {
        volume.volume_time = decoded.volume_time;
    }
    volume.cuts.push(cut);
    volume.metadata.message_count += decoded.metadata.message_count;
    volume.metadata.skipped_message_count += decoded.metadata.skipped_message_count;
    Ok(())
}

/// Sort cuts by elevation and refresh volume-level bookkeeping. Called once
/// after the last [`append_dorade_sweep`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use DoradeVolumeBuilder::finish")
)]
pub fn finalize_dorade_volume(volume: &mut RadarVolume) {
    sort_cuts(volume);
}

/// One decoded legacy volume scan plus where it came from inside the
/// archive.
#[derive(Clone, Debug)]
pub struct MobileRadarVolume {
    pub volume: RadarVolume,
    /// Display label: first member name of the group (`swp....` or `*.msg31`).
    pub member_label: String,
    /// Number of archive members merged into this volume.
    pub member_count: usize,
}

/// Decode every radar volume in a zip archive into the legacy model, sorted
/// by scan time. See [`mobile_archive::decode_mobile_archive_from_path`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use mobile_archive::decode_mobile_archive_from_path")
)]
pub fn decode_mobile_archive_from_path<F, E>(
    path: &Path,
    decode_level2: F,
) -> Result<Vec<MobileRadarVolume>>
where
    F: Fn(&[u8]) -> std::result::Result<RadarVolume, E> + Sync,
    E: Display,
{
    Ok(
        mobile_archive::decode_mobile_archive_as::<RadarVolume, F, E>(path, decode_level2)?
            .into_iter()
            .map(mobile_legacy)
            .collect(),
    )
}

/// Decode every radar volume under a deployment folder into the legacy
/// model. See [`mobile_archive::decode_mobile_dir_from_path`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use mobile_archive::decode_mobile_dir_from_path")
)]
pub fn decode_mobile_dir_from_path<F, E>(
    dir: &Path,
    decode_level2: F,
) -> Result<Vec<MobileRadarVolume>>
where
    F: Fn(&[u8]) -> std::result::Result<RadarVolume, E> + Sync,
    E: Display,
{
    Ok(
        mobile_archive::decode_mobile_dir_as::<RadarVolume, F, E>(dir, decode_level2)?
            .into_iter()
            .map(mobile_legacy)
            .collect(),
    )
}

/// Decode the volume run a sweepfile belongs to, into the legacy model. See
/// [`mobile_archive::read_dorade_volume_for_path`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use mobile_archive::read_dorade_volume_for_path")
)]
pub fn decode_dorade_volume_for_path(path: &Path) -> Result<RadarVolume> {
    mobile_archive::decode_dorade_volume_for_path_as::<RadarVolume>(path)
}

fn mobile_legacy(volume: MobileVolume<RadarVolume>) -> MobileRadarVolume {
    MobileRadarVolume {
        volume: volume.volume,
        member_label: volume.member_label,
        member_count: volume.member_count,
    }
}

impl MobileDecode for RadarVolume {
    fn decode_dorade_run(sweeps: &[&[u8]]) -> Result<Self> {
        let mut builder = DoradeVolumeBuilder::new();
        for sweep in sweeps {
            builder.append(sweep)?;
        }
        legacy_volume(builder)
    }

    fn retained_bytes(&self) -> usize {
        let radials: usize = self.cuts.iter().map(|cut| cut.radials.len()).sum();
        recast_radar_core::bounded_read::volume_moment_capacity_bytes(self)
            .saturating_add(radials.saturating_mul(size_of::<recast_radar_core::Radial>()))
    }

    fn set_source_path(&mut self, path: String) {
        self.metadata.source_path = Some(path);
    }

    fn scan_time(&self) -> DateTime<Utc> {
        self.volume_time
    }
}

/// Convert a built volume to the legacy model.
fn legacy_volume(builder: DoradeVolumeBuilder) -> Result<RadarVolume> {
    let (mut volume, log) = builder.finish_with_log()?;
    let reference = volume.time_reference;
    let scan_mode = volume
        .sweeps
        .first()
        .map(|sweep| legacy_scan_mode(&sweep.sweep_mode));
    let mut per_sweep: Vec<SweepValues> = Vec::with_capacity(volume.sweeps.len());
    let mut residues = Vec::with_capacity(volume.sweeps.len());
    for (sweep, entry) in volume.sweeps.iter_mut().zip(&log) {
        let values = SweepValues::of(sweep, reference, entry);
        residues.push(fold_legacy_fields(sweep));
        // The legacy model has one uniform gate range per sweep; the legacy
        // decoder used the lead spacing of a non-uniform cell table.
        if let RangeCoord::Explicit { centers_m } = &sweep.range {
            sweep.range = RangeCoord::Uniform {
                first_center_m: f64::from(values.gate_range.first_gate_m),
                spacing_m: f64::from(values.gate_range.gate_spacing_m),
                ngates: centers_m.len() as u32,
            };
        }
        per_sweep.push(values);
    }
    let residue = LegacyResidue {
        volume_time: reference,
        scan_mode: None,
        pulse_width_us: None,
        unambiguous_range_km: None,
        sweeps: residues,
    };
    let mut legacy = legacy_from_volume(volume, Some(&residue), LegacyConvention::Dorade)
        .map_err(|err| invalid(0, format!("legacy conversion: {err}")))?;
    legacy.site.name = Some(format!("{} (mobile)", legacy.site.id));
    legacy.metadata.scan_mode = scan_mode;
    legacy.metadata.prt_s = None;
    for (cut, values) in legacy.cuts.iter_mut().zip(&per_sweep) {
        cut.elevation_number = values.elevation_number;
        cut.ray_instrument_metadata.clear();
        for (ray, radial) in cut.radials.iter_mut().enumerate() {
            radial.time_offset_ms = values.time_offset_ms[ray];
            radial.gate_range = values.gate_range.clone();
        }
        // A grid keeps its own (possibly widened) gate count.
        for grid in cut.moments.values_mut() {
            grid.gate_range = GateRange {
                gate_count: grid.gate_range.gate_count,
                ..values.gate_range.clone()
            };
        }
    }
    sort_cuts(&mut legacy);
    Ok(legacy)
}

/// Stable sort: same-elevation cuts (single-tilt COW2 sequences) keep their
/// scan-time order.
fn sort_cuts(volume: &mut RadarVolume) {
    volume
        .cuts
        .sort_by(|left, right| left.elevation_deg.total_cmp(&right.elevation_deg));
    volume.metadata.decoded_radial_count = volume.cuts.iter().map(|cut| cut.radials.len()).sum();
}

/// Per-sweep values the legacy cut carried, read before the fold.
struct SweepValues {
    elevation_number: Option<u8>,
    time_offset_ms: Vec<i32>,
    gate_range: GateRange,
}

impl SweepValues {
    fn of(sweep: &Sweep, reference: DateTime<Utc>, log: &SweepLog) -> Self {
        let start_s = log
            .start
            .map(|start| (start - reference).num_milliseconds() as f64 / 1000.0);
        Self {
            elevation_number: sweep.elevation_number.map(|number| number.min(255) as u8),
            time_offset_ms: sweep
                .rays
                .time_s
                .iter()
                .map(|time| match start_s {
                    Some(start) if time.is_finite() => ((time - start) * 1000.0)
                        .round()
                        .clamp(f64::from(i32::MIN), f64::from(i32::MAX))
                        as i32,
                    _ => 0,
                })
                .collect(),
            gate_range: legacy_gate_range(&sweep.range, log.gate_count),
        }
    }
}

/// The legacy gate range: rounded centre of the first cell, rounded (at
/// least 1 m) lead spacing and the descriptor gate count.
fn legacy_gate_range(range: &RangeCoord, gate_count: usize) -> GateRange {
    let (first, spacing) = match range {
        RangeCoord::Uniform {
            first_center_m,
            spacing_m,
            ngates,
        } => (*first_center_m, if *ngates >= 2 { *spacing_m } else { 0.0 }),
        RangeCoord::Explicit { centers_m } => (
            centers_m.first().map_or(0.0, |c| f64::from(*c)),
            match centers_m.as_slice() {
                [first, second, ..] => f64::from(*second) - f64::from(*first),
                _ => 0.0,
            },
        ),
    };
    GateRange {
        first_gate_m: first.round() as i32,
        gate_spacing_m: spacing.round().max(1.0) as i32,
        gate_count,
    }
}

/// Fold a sweep's fields onto the legacy moments: rename the first field of
/// each canonical moment (PARM order) to that moment's FM301 name, hold every
/// later canonical-name field as a verbatim `Other`, shift 8/16-bit storage
/// into the unsigned legacy form, and record the row residues of fields some
/// rays lack.
fn fold_legacy_fields(sweep: &mut Sweep) -> SweepResidue {
    let mut taken = BTreeSet::new();
    let mut fields = Vec::new();
    for field in &mut sweep.fields {
        let name = field.name.as_str().to_owned();
        match canonical_moment(&name) {
            Some(moment) if taken.insert(moment.clone()) => {
                field.name = moment.to_field_name(LegacyConvention::Dorade);
            }
            _ => {
                if !matches!(field.name, FieldName::Other(_)) {
                    field.name = FieldName::Other(name.into());
                }
            }
        }
        shift_unsigned(field);
        if !field.absent_rows.is_empty() {
            fields.push(FieldResidue {
                moment: field.name.to_legacy_moment(LegacyConvention::Dorade),
                name: field.name.clone(),
                gate_range: GateRange {
                    first_gate_m: 0,
                    gate_spacing_m: 0,
                    gate_count: 0,
                },
                nodata: legacy_nodata(field),
                range_folded: None,
                radial_indices: Some(
                    (0..field.nrays as usize)
                        .filter(|ray| !field.is_absent(*ray))
                        .collect(),
                ),
                float_scale_offset: None,
            });
        }
    }
    SweepResidue {
        fields,
        ..SweepResidue::default()
    }
}

fn legacy_nodata(field: &recast_radar_core::model::Field) -> Option<u16> {
    match &field.data {
        FieldData::U8 { coding, .. } => coding.fill_value.map(u16::from),
        FieldData::U16 { coding, .. } => coding.fill_value,
        _ => None,
    }
}

/// `i8` -> `u8` (+128) and `i16` -> `u16` (+32768) storage with the offset
/// shifted the same way, so `(raw - offset) / scale` is unchanged.
fn shift_unsigned(field: &mut recast_radar_core::model::Field) {
    let shifted = match &field.data {
        FieldData::I8 { values, coding } => {
            let LinearTransform::IcdScaleOffset { scale, offset } = coding.transform else {
                return;
            };
            FieldData::U8 {
                values: values.iter().map(|v| (i16::from(*v) + 128) as u8).collect(),
                coding: IntCoding {
                    transform: LinearTransform::IcdScaleOffset {
                        scale,
                        offset: offset + 128.0,
                    },
                    fill_value: coding.fill_value.map(|v| (i16::from(v) + 128) as u8),
                    undetect: None,
                    range_folded: None,
                    valid_range: None,
                },
            }
        }
        FieldData::I16 { values, coding } => {
            let LinearTransform::IcdScaleOffset { scale, offset } = coding.transform else {
                return;
            };
            FieldData::U16 {
                values: values
                    .iter()
                    .map(|v| (i32::from(*v) + 32768) as u16)
                    .collect(),
                coding: IntCoding {
                    transform: LinearTransform::IcdScaleOffset {
                        scale,
                        offset: offset + 32768.0,
                    },
                    fill_value: coding.fill_value.map(|v| (i32::from(v) + 32768) as u16),
                    undetect: None,
                    range_folded: None,
                    valid_range: None,
                },
            }
        }
        _ => return,
    };
    field.data = shifted;
}

/// The legacy volume-level scan mode of a RADD code's sweep mode.
fn legacy_scan_mode(mode: &SweepMode) -> ScanMode {
    match mode {
        SweepMode::AzimuthSurveillance | SweepMode::Sector => ScanMode::Ppi,
        SweepMode::Rhi => ScanMode::Rhi,
        SweepMode::VerticalPointing => ScanMode::VerticalPointing,
        _ => ScanMode::Other,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use recast_radar_core::{MomentStorage, MomentType};

    use crate::dorade::Endian;
    use crate::dorade::tests::{Synth, find_block, put_f32, put_i16, synth_rays};

    #[test]
    fn legacy_wrapper_reproduces_the_pre_fm301_cut() {
        let bytes = Synth {
            endian: Endian::Big,
            compressed: false,
        }
        .build(&synth_rays());
        let volume = decode_dorade_sweep_volume(&bytes).expect("decode");
        assert_eq!(volume.site.id, "TST1");
        assert_eq!(volume.site.name.as_deref(), Some("TST1 (mobile)"));
        // RADD scan mode 8 (SUR) maps to the shared PPI mode.
        assert_eq!(volume.metadata.scan_mode, Some(ScanMode::Ppi));
        assert_eq!(volume.metadata.radar_frequency_mhz, Some(5450));
        assert_eq!(volume.site.latitude_deg, Some(39.74));
        assert_eq!(
            volume.volume_time,
            Utc.with_ymd_and_hms(2026, 5, 21, 22, 55, 14).unwrap()
        );
        let cut = &volume.cuts[0];
        assert_eq!(cut.elevation_deg, 1.0);
        assert_eq!(cut.elevation_number, Some(6));
        assert_eq!(cut.radials.len(), 2);
        assert_eq!(cut.radials[0].nyquist_velocity_mps, Some(68.76));
        assert_eq!(cut.radials[0].gate_range.first_gate_m, 50);
        assert_eq!(cut.radials[0].gate_range.gate_spacing_m, 100);
        assert_eq!(cut.radials[0].gate_range.gate_count, 4);
        // RYIB time 22:55:15.250 - SSWB start 22:55:14 = 1250 ms.
        assert_eq!(cut.radials[0].time_offset_ms, 1250);
        assert!(cut.ray_instrument_metadata.is_empty());

        // 16-bit fields shift into unsigned storage with the legacy offset.
        let grid = cut.moments.get(&MomentType::Reflectivity).expect("DBZ");
        assert!(matches!(grid.storage, MomentStorage::U16(_)));
        assert_eq!(grid.scale, 100.0);
        assert_eq!(grid.offset, 32768.0);
        assert_eq!(grid.nodata, Some(0));
        assert_eq!(grid.radial_count(), 2);
        assert_eq!(grid.scaled_value(0, 0), Some(10.0));
        assert_eq!(grid.scaled_value(0, 2), None);
        assert_eq!(grid.scaled_value(1, 2), Some(7.0));
    }

    #[test]
    fn multi_sweep_legacy_volume_sorts_cuts_by_elevation() {
        let synth = Synth {
            endian: Endian::Big,
            compressed: false,
        };
        let high = {
            let mut rays = synth_rays();
            for ray in &mut rays {
                ray.1 = 2.4;
            }
            let mut bytes = synth.build(&rays);
            let swib_pos = find_block(&bytes, b"SWIB");
            put_f32(&mut bytes[swib_pos..], 32, 2.4, Endian::Big);
            bytes
        };
        let low = synth.build(&synth_rays());
        let volume = decode_dorade_volume_from_slices(&[high, low]).expect("decode");
        assert_eq!(volume.cuts.len(), 2);
        assert!(volume.cuts[0].elevation_deg < volume.cuts[1].elevation_deg);
        assert_eq!(volume.metadata.decoded_radial_count, 4);

        // The incremental legacy API agrees.
        let mut incremental = RadarVolume::default();
        append_dorade_sweep(&synth.build(&synth_rays()), &mut incremental).unwrap();
        finalize_dorade_volume(&mut incremental);
        assert_eq!(incremental.cuts.len(), 1);
        assert_eq!(incremental.site.id, "TST1");
    }

    #[test]
    fn rhi_scan_mode_maps_to_legacy_rhi() {
        let rays: Vec<(f32, f32, i32, &[i16])> = vec![
            (271.0, 0.5, 0, &[1000, 2000, 1500, 500][..]),
            (271.0, 1.5, 0, &[1500, 1200, 700, 800][..]),
        ];
        let mut bytes = Synth {
            endian: Endian::Big,
            compressed: false,
        }
        .build(&rays);
        let radd_pos = find_block(&bytes, b"RADD");
        put_i16(&mut bytes[radd_pos..], 50, 3, Endian::Big);
        let volume = decode_dorade_sweep_volume(&bytes).expect("decode");
        assert_eq!(volume.metadata.scan_mode, Some(ScanMode::Rhi));
    }

    #[test]
    fn duplicate_canonical_names_keep_original_field() {
        // DOW7 carries DZ (raw) and DCZ/VC (corrected); first match wins and
        // later candidates stay addressable under their DORADE names.
        let mut taken = BTreeSet::new();
        let mut resolved = Vec::new();
        for name in ["DZ", "DCZ", "VE", "VC"] {
            let canonical = canonical_moment(name);
            let moment = match canonical {
                Some(moment) if !taken.contains(&moment) => {
                    taken.insert(moment.clone());
                    moment
                }
                _ => MomentType::Unknown(name.to_owned()),
            };
            resolved.push(moment);
        }
        assert_eq!(resolved[0], MomentType::Reflectivity);
        assert_eq!(resolved[1], MomentType::Unknown("DCZ".to_owned()));
        assert_eq!(resolved[2], MomentType::Velocity);
        assert_eq!(resolved[3], MomentType::Unknown("VC".to_owned()));
    }
}
