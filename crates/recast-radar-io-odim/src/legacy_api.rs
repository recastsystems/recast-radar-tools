//! Pre-FM301 signature of the ODIM_H5 decoder, kept until the legacy model
//! is removed at the end of the FM301 migration (F.3;
//! `docs/design/fm301-model.md` section 13.3). Only this module names legacy
//! model items.
//!
//! The wrapper runs [`read_odim_h5_volume`] and folds the result onto the
//! legacy model exactly as the pre-migration decoder built it:
//!
//! - quantities collapse onto the seven canonical moments by
//!   [`canonical_quantity`] and [`canonical_quantity_priority`] (`DBZH`
//!   over `TH`, filtered dual-pol spellings over unfiltered ones), other
//!   quantities become `MomentType::Unknown` (first plane of a name wins);
//! - gate buffers move (no copy); `undetect` codes are remapped onto the
//!   `nodata` sentinel and float planes expand to physical `f32` inside
//!   `legacy::legacy_from_volume`;
//! - radials get the synthesized centre azimuths `(i + 0.5) * 360 / nrays`,
//!   the sweep elevation and zero time offsets, and cuts are sorted by
//!   elevation.

use recast_radar_core::legacy::{LegacyConvention, legacy_from_volume};
use recast_radar_core::model::Sweep;
use recast_radar_core::{MomentType, RadarVolume};

use crate::Result;
use crate::odim::{
    CanonicalMoment, canonical_quantity, canonical_quantity_priority, invalid, read_odim_h5_volume,
};

/// Decode an ODIM_H5 PVOL/SCAN byte buffer into the legacy radar model.
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use read_odim_h5_volume")
)]
pub fn decode_odim_h5_volume(bytes: &[u8]) -> Result<RadarVolume> {
    let mut volume = read_odim_h5_volume(bytes)?;
    for sweep in &mut volume.sweeps {
        fold_legacy_moments(sweep);
    }
    let mut legacy = legacy_from_volume(volume, None, LegacyConvention::Odim)
        .map_err(|err| invalid(format!("legacy conversion: {err}")))?;
    for cut in &mut legacy.cuts {
        let nrays = cut.radials.len();
        for (ray, radial) in cut.radials.iter_mut().enumerate() {
            radial.azimuth_deg = ((ray as f32 + 0.5) * 360.0 / nrays as f32).rem_euclid(360.0);
            radial.elevation_deg = cut.elevation_deg;
            radial.time_offset_ms = 0;
        }
    }
    legacy
        .cuts
        .sort_by(|left, right| left.elevation_deg.total_cmp(&right.elevation_deg));
    Ok(legacy)
}

/// Keep, per canonical moment, the plane the legacy decoder chose (renamed
/// to the moment's FM301 name so the conversion maps it back), plus the
/// first plane of every other quantity name; drop the rest.
fn fold_legacy_moments(sweep: &mut Sweep) {
    let mut chosen: Vec<(CanonicalMoment, u8, usize)> = Vec::new();
    let mut keep = vec![false; sweep.fields.len()];
    for (index, field) in sweep.fields.iter().enumerate() {
        let quantity = field.name.as_str();
        match canonical_quantity(quantity) {
            Some(moment) => {
                let priority = canonical_quantity_priority(quantity);
                match chosen.iter_mut().find(|(m, _, _)| *m == moment) {
                    Some(entry) if priority <= entry.1 => {}
                    Some(entry) => *entry = (moment, priority, index),
                    None => chosen.push((moment, priority, index)),
                }
            }
            // Distinct verbatim names are unique already (the decoder
            // skips a second plane of the same quantity).
            None => keep[index] = true,
        }
    }
    for (moment, _, index) in chosen {
        keep[index] = true;
        sweep.fields[index].name = legacy_moment(moment).to_field_name(LegacyConvention::Odim);
    }
    let mut index = 0;
    sweep.fields.retain(|_| {
        let kept = keep[index];
        index += 1;
        kept
    });
}

fn legacy_moment(moment: CanonicalMoment) -> MomentType {
    match moment {
        CanonicalMoment::Reflectivity => MomentType::Reflectivity,
        CanonicalMoment::Velocity => MomentType::Velocity,
        CanonicalMoment::SpectrumWidth => MomentType::SpectrumWidth,
        CanonicalMoment::DifferentialReflectivity => MomentType::DifferentialReflectivity,
        CanonicalMoment::CorrelationCoefficient => MomentType::CorrelationCoefficient,
        CanonicalMoment::DifferentialPhase => MomentType::DifferentialPhase,
        CanonicalMoment::SpecificDifferentialPhase => MomentType::SpecificDifferentialPhase,
    }
}
