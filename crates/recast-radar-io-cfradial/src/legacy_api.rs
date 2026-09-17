//! Pre-FM301 signature of the CfRadial decoder, kept until the legacy model
//! is removed at the end of the FM301 migration (F.3;
//! `docs/design/fm301-model.md` section 13.3). Only this module names legacy
//! model items.
//!
//! The wrapper runs the native decoder with its legacy log and folds the
//! result onto the legacy model exactly as the pre-migration decoder built
//! it: field names collapse onto the canonical moments through
//! `canonical_moment` (first match in name order wins), packed and float64
//! fields expand to physical `f32` inside `legacy::legacy_from_volume`, the
//! gate range is the rounded start-of-first-gate form, ray times are the
//! file's `time` values truncated to milliseconds, non-positive Nyquist
//! velocities are dropped, and cuts (with their scan legs) are sorted by
//! elevation.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use recast_radar_core::bounded_read::DecodeBudget;
use recast_radar_core::legacy::{LegacyConvention, legacy_from_volume};
use recast_radar_core::model::{FieldName, RangeCoord, Sweep};
use recast_radar_core::{
    GateRange, RadarVolume, RayInstrumentMetadata, ScanMode, canonical_moment,
};

use crate::Result;
use crate::cfradial::{Decoded, LegacyScanMode, decode, invalid};

/// Decode a CfRadial 1.x byte buffer into the legacy radar model.
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_cfradial1_volume")
)]
pub fn decode_cfradial1_volume(bytes: &[u8]) -> Result<RadarVolume> {
    decode_cfradial1_volume_within(bytes, DecodeBudget::volume())
}

/// [`decode_cfradial1_volume`] with an explicit output budget.
pub(crate) fn decode_cfradial1_volume_within(
    bytes: &[u8],
    budget: DecodeBudget,
) -> Result<RadarVolume> {
    let Decoded { mut volume, legacy } = decode(bytes, budget, true)?;
    let log = legacy.ok_or_else(|| invalid("legacy log missing"))?;
    let gate_range = volume
        .sweeps
        .first()
        .map(|sweep| legacy_gate_range(&sweep.range))
        .unwrap_or(GateRange {
            first_gate_m: 0,
            gate_spacing_m: 1,
            gate_count: 0,
        });
    let per_sweep: Vec<SweepValues> = volume.sweeps.iter().map(SweepValues::of).collect();
    for sweep in &mut volume.sweeps {
        fold_legacy_moments(sweep);
        // The legacy model has one uniform gate range per volume; the
        // legacy decoder derived it from the first two centres.
        if let RangeCoord::Explicit { centers_m } = &sweep.range {
            sweep.range = RangeCoord::Uniform {
                first_center_m: f64::from(gate_range.first_gate_m)
                    + f64::from(gate_range.gate_spacing_m) / 2.0,
                spacing_m: f64::from(gate_range.gate_spacing_m),
                ngates: centers_m.len() as u32,
            };
        }
    }
    let mut legacy = legacy_from_volume(volume, None, LegacyConvention::CfRadial)
        .map_err(|err| invalid(format!("legacy conversion: {err}")))?;

    legacy.volume_time = log.volume_time.unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    legacy.metadata.scan_mode = combined_scan_mode(&log.sweep_modes);
    legacy.metadata.pulse_width_us = log.pulse_width_us;
    legacy.metadata.unambiguous_range_km = log.unambiguous_range_km;
    for (index, cut) in legacy.cuts.iter_mut().enumerate() {
        let values = &per_sweep[index];
        cut.elevation_number = Some(values.source_index.min(255) as u8);
        for (ray, radial) in cut.radials.iter_mut().enumerate() {
            radial.time_offset_ms = values.time_offset_ms[ray];
            radial.nyquist_velocity_mps = radial.nyquist_velocity_mps.filter(|v| *v > 0.0);
            radial.gate_range = gate_range.clone();
        }
        for grid in cut.moments.values_mut() {
            grid.gate_range = gate_range.clone();
        }
        cut.ray_instrument_metadata = log
            .ray_instruments
            .get(index)
            .map(|rays| {
                rays.iter()
                    .map(|ray| RayInstrumentMetadata {
                        prt_s: ray.prt_s,
                        unambiguous_range_km: ray.unambiguous_range_km,
                        pulse_count: ray.pulse_count,
                        independent_samples: ray.independent_samples,
                    })
                    .collect()
            })
            .unwrap_or_default();
    }

    // Cuts (with their scan legs) sorted by elevation, as the legacy decoder
    // returned them.
    let mut order: Vec<usize> = (0..legacy.cuts.len()).collect();
    order.sort_by(|a, b| {
        legacy.cuts[*a]
            .elevation_deg
            .total_cmp(&legacy.cuts[*b].elevation_deg)
    });
    let mut cuts: Vec<Option<_>> = legacy.cuts.into_iter().map(Some).collect();
    legacy.cuts = order.iter().filter_map(|i| cuts[*i].take()).collect();
    if !legacy.metadata.scan_legs.is_empty() {
        let mut legs: Vec<Option<_>> = legacy.metadata.scan_legs.into_iter().map(Some).collect();
        legacy.metadata.scan_legs = order.iter().filter_map(|i| legs[*i].take()).collect();
    }
    Ok(legacy)
}

/// Per-sweep values the legacy radials carried, read before the fold.
struct SweepValues {
    source_index: usize,
    time_offset_ms: Vec<i32>,
}

impl SweepValues {
    fn of(sweep: &Sweep) -> Self {
        Self {
            source_index: sweep.elevation_number.map_or(0, usize::from),
            time_offset_ms: sweep
                .rays
                .time_s
                .iter()
                .map(|seconds| (seconds * 1000.0) as i32)
                .collect(),
        }
    }
}

/// The legacy gate range: rounded spacing (at least 1 m) and the rounded
/// range to the start of the first gate.
fn legacy_gate_range(range: &RangeCoord) -> GateRange {
    let (first, spacing) = match range {
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
    let spacing = spacing.round().max(1.0);
    GateRange {
        first_gate_m: (first - spacing / 2.0).round() as i32,
        gate_spacing_m: spacing as i32,
        gate_count: range.ngates(),
    }
}

/// Rename, in name order, the first field of each canonical moment to that
/// moment's FM301 name so the conversion maps it back; every other field
/// becomes `MomentType::Unknown(name)` (a later field whose name is itself
/// an FM301 spelling is held as a verbatim `Other` so it does not map to the
/// canonical moment too).
fn fold_legacy_moments(sweep: &mut Sweep) {
    let mut names: Vec<(String, usize)> = sweep
        .fields
        .iter()
        .enumerate()
        .map(|(index, field)| (field.name.as_str().to_owned(), index))
        .collect();
    names.sort();
    let mut taken = BTreeSet::new();
    for (name, index) in names {
        let field = &mut sweep.fields[index];
        match canonical_moment(&name) {
            Some(moment) if taken.insert(moment.clone()) => {
                field.name = moment.to_field_name(LegacyConvention::CfRadial);
            }
            _ => {
                if !matches!(field.name, FieldName::Other(_)) {
                    field.name = FieldName::Other(name.into());
                }
            }
        }
    }
}

/// One volume-level mode when every sweep agrees; mixed scans report Other.
fn combined_scan_mode(modes: &[Option<LegacyScanMode>]) -> Option<ScanMode> {
    let mut all = modes.iter().flatten();
    let first = *all.next()?;
    Some(if all.all(|mode| *mode == first) {
        legacy_mode(first)
    } else {
        ScanMode::Other
    })
}

fn legacy_mode(mode: LegacyScanMode) -> ScanMode {
    match mode {
        LegacyScanMode::Ppi => ScanMode::Ppi,
        LegacyScanMode::Rhi => ScanMode::Rhi,
        LegacyScanMode::VerticalPointing => ScanMode::VerticalPointing,
        LegacyScanMode::Other => ScanMode::Other,
    }
}
