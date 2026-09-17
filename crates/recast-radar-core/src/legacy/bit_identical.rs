//! Bitwise structural comparison of legacy volumes (design note 13.4).
//!
//! `PartialEq` cannot check a round trip: `NaN != NaN`, so a volume with NaN
//! gates is not even equal to its own clone.

use super::{
    ElevationCut, GateRange, MomentGrid, MomentStorage, RadarVolume, Radial, RayInstrumentMetadata,
    VolumeMetadata,
};

/// Structural equality with every `f32` / `f64` compared by `to_bits` (NaN
/// payloads included). `Err` names the first difference, for example
/// `cuts[3].moments[Velocity].storage[1042]`.
#[doc(hidden)]
pub fn bit_identical(a: &RadarVolume, b: &RadarVolume) -> Result<(), String> {
    eq("site.id", &a.site.id, &b.site.id)?;
    eq("site.name", &a.site.name, &b.site.name)?;
    opt_f32(
        "site.latitude_deg",
        a.site.latitude_deg,
        b.site.latitude_deg,
    )?;
    opt_f32(
        "site.longitude_deg",
        a.site.longitude_deg,
        b.site.longitude_deg,
    )?;
    opt_f32("site.elevation_m", a.site.elevation_m, b.site.elevation_m)?;
    eq("volume_time", &a.volume_time, &b.volume_time)?;
    eq("vcp", &a.vcp, &b.vcp)?;
    metadata(&a.metadata, &b.metadata)?;
    eq("cuts.len()", &a.cuts.len(), &b.cuts.len())?;
    for (index, (x, y)) in a.cuts.iter().zip(&b.cuts).enumerate() {
        cut(&format!("cuts[{index}]"), x, y)?;
    }
    Ok(())
}

fn eq<T: PartialEq + std::fmt::Debug>(path: &str, a: &T, b: &T) -> Result<(), String> {
    if a == b {
        Ok(())
    } else {
        Err(format!("{path}: {a:?} != {b:?}"))
    }
}

fn f32_bits(path: &str, a: f32, b: f32) -> Result<(), String> {
    if a.to_bits() == b.to_bits() {
        Ok(())
    } else {
        Err(format!(
            "{path}: {a:?} ({:#x}) != {b:?} ({:#x})",
            a.to_bits(),
            b.to_bits()
        ))
    }
}

fn opt_f32(path: &str, a: Option<f32>, b: Option<f32>) -> Result<(), String> {
    match (a, b) {
        (Some(a), Some(b)) => f32_bits(path, a, b),
        (None, None) => Ok(()),
        _ => Err(format!("{path}: {a:?} != {b:?}")),
    }
}

fn metadata(a: &VolumeMetadata, b: &VolumeMetadata) -> Result<(), String> {
    eq("metadata.source_path", &a.source_path, &b.source_path)?;
    eq(
        "metadata.archive_version",
        &a.archive_version,
        &b.archive_version,
    )?;
    eq("metadata.compression", &a.compression, &b.compression)?;
    eq("metadata.message_count", &a.message_count, &b.message_count)?;
    eq(
        "metadata.decoded_radial_count",
        &a.decoded_radial_count,
        &b.decoded_radial_count,
    )?;
    eq(
        "metadata.skipped_message_count",
        &a.skipped_message_count,
        &b.skipped_message_count,
    )?;
    eq("metadata.scan_mode", &a.scan_mode, &b.scan_mode)?;
    eq(
        "metadata.radar_frequency_mhz",
        &a.radar_frequency_mhz,
        &b.radar_frequency_mhz,
    )?;
    opt_f32(
        "metadata.beam_width_h_deg",
        a.beam_width_h_deg,
        b.beam_width_h_deg,
    )?;
    opt_f32(
        "metadata.beam_width_v_deg",
        a.beam_width_v_deg,
        b.beam_width_v_deg,
    )?;
    opt_f32(
        "metadata.pulse_width_us",
        a.pulse_width_us,
        b.pulse_width_us,
    )?;
    opt_f32("metadata.prt_s", a.prt_s, b.prt_s)?;
    opt_f32(
        "metadata.unambiguous_range_km",
        a.unambiguous_range_km,
        b.unambiguous_range_km,
    )?;
    eq("metadata.scan_name", &a.scan_name, &b.scan_name)?;
    eq("metadata.scan_id", &a.scan_id, &b.scan_id)?;
    eq(
        "metadata.vcp_source_document",
        &a.vcp_source_document,
        &b.vcp_source_document,
    )?;
    eq(
        "metadata.vcp_source_revision",
        &a.vcp_source_revision,
        &b.vcp_source_revision,
    )?;
    eq(
        "metadata.vcp_source_rda_build",
        &a.vcp_source_rda_build,
        &b.vcp_source_rda_build,
    )?;
    eq(
        "metadata.vcp_source_figure",
        &a.vcp_source_figure,
        &b.vcp_source_figure,
    )?;
    eq(
        "metadata.vcp_pulse_length",
        &a.vcp_pulse_length,
        &b.vcp_pulse_length,
    )?;
    eq(
        "metadata.vcp_adaptations",
        &a.vcp_adaptations,
        &b.vcp_adaptations,
    )?;
    eq(
        "metadata.scan_legs.len()",
        &a.scan_legs.len(),
        &b.scan_legs.len(),
    )?;
    for (index, (x, y)) in a.scan_legs.iter().zip(&b.scan_legs).enumerate() {
        let path = format!("metadata.scan_legs[{index}]");
        eq(
            &format!("{path}.source_row_index"),
            &x.source_row_index,
            &y.source_row_index,
        )?;
        opt_f32(
            &format!("{path}.elevation_deg"),
            x.elevation_deg,
            y.elevation_deg,
        )?;
        opt_f32(
            &format!("{path}.azimuth_rate_deg_per_second"),
            x.azimuth_rate_deg_per_second,
            y.azimuth_rate_deg_per_second,
        )?;
        opt_f32(
            &format!("{path}.source_period_seconds"),
            x.source_period_seconds,
            y.source_period_seconds,
        )?;
        eq(&format!("{path}.waveform"), &x.waveform, &y.waveform)?;
        eq(
            &format!("{path}.moment_coverage"),
            &x.moment_coverage,
            &y.moment_coverage,
        )?;
        eq(
            &format!("{path}.surveillance_prf_code"),
            &x.surveillance_prf_code,
            &y.surveillance_prf_code,
        )?;
        eq(
            &format!("{path}.surveillance_pulse_count"),
            &x.surveillance_pulse_count,
            &y.surveillance_pulse_count,
        )?;
        eq(
            &format!("{path}.doppler_prf_code"),
            &x.doppler_prf_code,
            &y.doppler_prf_code,
        )?;
        eq(
            &format!("{path}.doppler_pulse_count"),
            &x.doppler_pulse_count,
            &y.doppler_pulse_count,
        )?;
    }
    eq("metadata.polarization", &a.polarization, &b.polarization)?;
    eq("metadata.calibration", &a.calibration, &b.calibration)?;
    eq(
        "metadata.forward_operator",
        &a.forward_operator,
        &b.forward_operator,
    )?;
    eq(
        "metadata.forward_operator_config",
        &a.forward_operator_config,
        &b.forward_operator_config,
    )?;
    eq("metadata.source_model", &a.source_model, &b.source_model)?;
    eq(
        "metadata.microphysics_scheme",
        &a.microphysics_scheme,
        &b.microphysics_scheme,
    )?;
    eq(
        "metadata.scattering_model",
        &a.scattering_model,
        &b.scattering_model,
    )
}

fn cut(path: &str, a: &ElevationCut, b: &ElevationCut) -> Result<(), String> {
    f32_bits(
        &format!("{path}.elevation_deg"),
        a.elevation_deg,
        b.elevation_deg,
    )?;
    eq(
        &format!("{path}.elevation_number"),
        &a.elevation_number,
        &b.elevation_number,
    )?;
    eq(
        &format!("{path}.radials.len()"),
        &a.radials.len(),
        &b.radials.len(),
    )?;
    for (index, (x, y)) in a.radials.iter().zip(&b.radials).enumerate() {
        radial(&format!("{path}.radials[{index}]"), x, y)?;
    }
    eq(
        &format!("{path}.ray_instrument_metadata.len()"),
        &a.ray_instrument_metadata.len(),
        &b.ray_instrument_metadata.len(),
    )?;
    for (index, (x, y)) in a
        .ray_instrument_metadata
        .iter()
        .zip(&b.ray_instrument_metadata)
        .enumerate()
    {
        instrument(&format!("{path}.ray_instrument_metadata[{index}]"), x, y)?;
    }
    let keys_a: Vec<_> = a.moments.keys().collect();
    let keys_b: Vec<_> = b.moments.keys().collect();
    eq(&format!("{path}.moments.keys()"), &keys_a, &keys_b)?;
    for ((key, x), y) in a.moments.iter().zip(b.moments.values()) {
        grid(&format!("{path}.moments[{key:?}]"), x, y)?;
    }
    Ok(())
}

fn radial(path: &str, a: &Radial, b: &Radial) -> Result<(), String> {
    f32_bits(&format!("{path}.azimuth_deg"), a.azimuth_deg, b.azimuth_deg)?;
    f32_bits(
        &format!("{path}.elevation_deg"),
        a.elevation_deg,
        b.elevation_deg,
    )?;
    eq(
        &format!("{path}.time_offset_ms"),
        &a.time_offset_ms,
        &b.time_offset_ms,
    )?;
    gate_range(&format!("{path}.gate_range"), &a.gate_range, &b.gate_range)?;
    opt_f32(
        &format!("{path}.nyquist_velocity_mps"),
        a.nyquist_velocity_mps,
        b.nyquist_velocity_mps,
    )?;
    eq(
        &format!("{path}.radial_status"),
        &a.radial_status,
        &b.radial_status,
    )
}

fn gate_range(path: &str, a: &GateRange, b: &GateRange) -> Result<(), String> {
    eq(path, a, b)
}

fn instrument(
    path: &str,
    a: &RayInstrumentMetadata,
    b: &RayInstrumentMetadata,
) -> Result<(), String> {
    opt_f32(&format!("{path}.prt_s"), a.prt_s, b.prt_s)?;
    opt_f32(
        &format!("{path}.unambiguous_range_km"),
        a.unambiguous_range_km,
        b.unambiguous_range_km,
    )?;
    eq(
        &format!("{path}.pulse_count"),
        &a.pulse_count,
        &b.pulse_count,
    )?;
    opt_f32(
        &format!("{path}.independent_samples"),
        a.independent_samples,
        b.independent_samples,
    )
}

fn grid(path: &str, a: &MomentGrid, b: &MomentGrid) -> Result<(), String> {
    eq(&format!("{path}.moment"), &a.moment, &b.moment)?;
    gate_range(&format!("{path}.gate_range"), &a.gate_range, &b.gate_range)?;
    f32_bits(&format!("{path}.scale"), a.scale, b.scale)?;
    f32_bits(&format!("{path}.offset"), a.offset, b.offset)?;
    eq(&format!("{path}.nodata"), &a.nodata, &b.nodata)?;
    eq(
        &format!("{path}.range_folded"),
        &a.range_folded,
        &b.range_folded,
    )?;
    eq(
        &format!("{path}.radial_indices"),
        &a.radial_indices,
        &b.radial_indices,
    )?;
    match (&a.storage, &b.storage) {
        (MomentStorage::U8(x), MomentStorage::U8(y)) => values(path, x, y, |v| u32::from(*v)),
        (MomentStorage::U16(x), MomentStorage::U16(y)) => values(path, x, y, |v| u32::from(*v)),
        (MomentStorage::F32(x), MomentStorage::F32(y)) => values(path, x, y, |v| v.to_bits()),
        (x, y) => Err(format!(
            "{path}.storage: {}-bit != {}-bit",
            x.word_size_bits(),
            y.word_size_bits()
        )),
    }
}

fn values<T>(path: &str, a: &[T], b: &[T], bits: impl Fn(&T) -> u32) -> Result<(), String> {
    eq(&format!("{path}.storage.len()"), &a.len(), &b.len())?;
    match a.iter().zip(b).position(|(x, y)| bits(x) != bits(y)) {
        None => Ok(()),
        Some(index) => Err(format!(
            "{path}.storage[{index}]: {:#x} != {:#x}",
            bits(&a[index]),
            bits(&b[index])
        )),
    }
}
