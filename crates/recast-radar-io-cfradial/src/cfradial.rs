//! CfRadial 1.x decoder (classic-netCDF radar moments).
//!
//! Format reference: M. Dixon and W.-C. Lee, "CfRadial Data File Format —
//! CF-compliant netCDF Format for Moments Data for RADAR and LIDAR",
//! NCAR/EOL, version 1.4 (2016) (versions 1.1–1.4 share the layout read
//! here). CfRadial 1 files are classic netCDF (`CDF\x01`/`CDF\x02`) with:
//! - dimensions `time` (rays, usually unlimited) and `range` (gates),
//! - per-ray `azimuth(time)`, `elevation(time)`, `time(time)` and the
//!   instrument variables `nyquist_velocity`, `unambiguous_range`, `prt`,
//!   `prt_ratio`, `n_samples`, `pulse_width`, `scan_rate`,
//!   `antenna_transition`, `r_calib_index` (all optional),
//! - per-sweep `fixed_angle(sweep)`, `sweep_start_ray_index(sweep)`,
//!   `sweep_end_ray_index(sweep)`, `sweep_mode(sweep, string_length)` and
//!   the other Table 301-8a string variables,
//! - scalar `latitude`/`longitude`/`altitude` (per ray for moving
//!   platforms), `time_coverage_start`, `volume_number`, `platform_type`,
//! - field variables dimensioned `(time, range)`, optionally packed with
//!   `scale_factor`/`add_offset` and flagged with `_FillValue`
//!   (CF packing: physical = raw * scale_factor + add_offset).
//!
//! CfRadial 2 is netCDF-4 (HDF5 container) and is rejected by the routing
//! layer with an explicit message — it never reaches this module.
//!
//! [`read_cfradial1_volume`] builds the FM301 model ([`Volume`]; design note
//! `docs/design/fm301-model.md` sections 7.3, 9 and 11) the way xradar's
//! `open_cfradial1_datatree` and Py-ART's `read_cfradial` read the same
//! file: one sweep per `sweep` index in file order, field variables under
//! their names verbatim in file order, packed `byte`/`short`/`int` fields kept
//! as `i8`/`i16`/`i32` with the file's `scale_factor`, `add_offset` and `_FillValue`
//! (attribute width preserved), float fields verbatim with their fill, the
//! `range` coordinate as the file's gate centres, `time(time)` as seconds
//! since the `time.units` reference (else `time_coverage_start`), and every
//! per-ray, root and sweep variable without a typed slot kept verbatim in
//! `extra_vars`. The BowEcho export attributes (`vcp_*`, `polarization`,
//! `calibration`, `forward_operator`, ...) fill `ScanStrategy`, `Provenance`
//! and `SimulationProvenance`; sweeps whose `vcp_moment_coverage_code`
//! restricts their moments get only those fields. For RHI sweeps the fixed
//! angle is the AZIMUTH (CfRadial §5.8).

use std::borrow::Cow;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use recast_radar_core::bounded_read::{DecodeBudget, check_gate_count, check_sweep_count};
use recast_radar_core::model::{
    ArrayBuf, AttrValue, ExtraVariable, Field, FieldData, FieldName, FloatCoding, FloatWidth,
    FollowMode, GateMapping, InstrumentType, IntCoding, LinearTransform, PlatformTrack,
    PlatformType, PolarizationMode, PrimaryAxis, PrtMode, Quantity, RadarCalibration, RangeCoord,
    Scalar, ScanDefinition, ScanLeg, SimulationProvenance, SourceFormat, Sweep, SweepMode, Volume,
};

pub use crate::netcdf3::looks_like_netcdf3_bytes;
use crate::netcdf3::{Nc3File, NcArray, NcValue, NcVar};
use crate::{CfRadialError, Result};

/// Decode a CfRadial 1.x byte buffer into the FM301 model.
pub fn read_cfradial1_volume(bytes: &[u8]) -> Result<Volume> {
    decode(bytes, DecodeBudget::volume())
}

/// [`read_cfradial1_volume`] with an explicit output budget.
#[cfg(test)]
fn read_cfradial1_volume_within(bytes: &[u8], budget: DecodeBudget) -> Result<Volume> {
    decode(bytes, budget)
}

/// Global attributes with a typed slot in the model; every other global
/// attribute goes to `GlobalAttrs::other` verbatim.
const SLOTTED_GLOBAL_ATTRS: &[&str] = &[
    "Conventions",
    "version",
    "title",
    "institution",
    "references",
    "source",
    "history",
    "comment",
    "instrument_name",
    "site_name",
    "scan_name",
    "scan_id",
    "platform_is_mobile",
    "ray_times_increase",
    "simulated",
    "vcp_pattern",
    "vcp_source_document",
    "vcp_source_revision",
    "vcp_source_rda_build",
    "vcp_source_figure",
    "vcp_pulse_length",
    "vcp_adaptations",
    "polarization",
    "calibration",
    "forward_operator",
    "forward_operator_config",
    "source_model",
    "microphysics_scheme",
    "scattering_model",
    "time_coverage_start",
    "time_coverage_end",
];

/// Per-ray variables with a typed slot; the rest of the `(time)` variables
/// go to `Sweep::extra_vars`.
const SLOTTED_RAY_VARS: &[&str] = &[
    "time",
    "azimuth",
    "elevation",
    "nyquist_velocity",
    "unambiguous_range",
    "prt",
    "prt_ratio",
    "n_samples",
    "pulse_count",
    "pulse_width",
    "scan_rate",
    "antenna_transition",
    "r_calib_index",
    "independent_samples",
    "latitude",
    "longitude",
    "altitude",
    "altitude_agl",
    "heading",
    "roll",
    "pitch",
    "drift",
    "rotation",
    "tilt",
    "measured_transmit_power_h",
    "measured_transmit_power_v",
];

/// Root variables with a typed slot; other scalar root variables go to
/// `Volume::extra_vars`.
const SLOTTED_ROOT_VARS: &[&str] = &[
    "volume_number",
    "platform_type",
    "instrument_type",
    "primary_axis",
    "status_str",
    "time_coverage_start",
    "time_coverage_end",
    "latitude",
    "longitude",
    "altitude",
    "altitude_agl",
    "frequency",
    "radar_antenna_gain_h",
    "radar_antenna_gain_v",
    "radar_beam_width_h",
    "radar_beam_width_v",
    "radar_rx_bandwidth",
    "radar_receiver_bandwidth",
];

pub(crate) fn decode(bytes: &[u8], mut budget: DecodeBudget) -> Result<Volume> {
    let file = Nc3File::open(bytes)?;
    let dim = |name: &str| file.dims.iter().position(|(dim_name, _)| dim_name == name);
    let (Some(time_dim), Some(range_dim)) = (dim("time"), dim("range")) else {
        return Err(invalid(
            "netCDF file lacks time/range dimensions — not CfRadial 1.x",
        ));
    };
    let n_rays = file.dims[time_dim].1;
    let n_gates = file.dims[range_dim].1;
    if n_rays == 0 || n_gates == 0 {
        return Err(invalid("CfRadial volume has no rays or gates"));
    }
    check_gate_count(n_gates, "CfRadial range dimension").map_err(CfRadialError::LimitExceeded)?;
    let ngates = u32::try_from(n_gates).map_err(|_| invalid("CfRadial range overflow"))?;

    let azimuth = read_f64s(&file, "azimuth", &mut budget)?;
    let elevation = read_f64s(&file, "elevation", &mut budget)?;
    if azimuth.len() < n_rays || elevation.len() < n_rays {
        return Err(invalid("azimuth/elevation shorter than the time dimension"));
    }
    let nyquist = aligned_time_values(&file, "nyquist_velocity", time_dim, n_rays, &mut budget)?;
    let unambiguous_range_scale = range_units_to_m_scale(units_of(&file, "unambiguous_range"));
    let unambiguous_range =
        aligned_time_values(&file, "unambiguous_range", time_dim, n_rays, &mut budget)?;
    let prt_scale = time_units_scale(units_of(&file, "prt"));
    let prt = aligned_time_values(&file, "prt", time_dim, n_rays, &mut budget)?;
    let prt_ratio = aligned_time_values(&file, "prt_ratio", time_dim, n_rays, &mut budget)?;
    let n_samples = aligned_time_values(&file, "n_samples", time_dim, n_rays, &mut budget)?;
    let pulse_count = aligned_time_values(&file, "pulse_count", time_dim, n_rays, &mut budget)?;
    let pulse_width_scale = time_units_scale(units_of(&file, "pulse_width"));
    let pulse_width = aligned_time_values(&file, "pulse_width", time_dim, n_rays, &mut budget)?;
    let scan_rate = aligned_time_values(&file, "scan_rate", time_dim, n_rays, &mut budget)?;
    let antenna_transition =
        aligned_time_values(&file, "antenna_transition", time_dim, n_rays, &mut budget)?;
    let calib_index = aligned_time_values(&file, "r_calib_index", time_dim, n_rays, &mut budget)?;
    let independent_samples =
        aligned_time_values(&file, "independent_samples", time_dim, n_rays, &mut budget)?;
    let transmit_power_h = aligned_time_values(
        &file,
        "measured_transmit_power_h",
        time_dim,
        n_rays,
        &mut budget,
    )?;
    let transmit_power_v = aligned_time_values(
        &file,
        "measured_transmit_power_v",
        time_dim,
        n_rays,
        &mut budget,
    )?;
    let platform = read_platform_track(&file, time_dim, n_rays, &mut budget)?;

    // Gate geometry: range(range) gate centres in metres (spec §5.5).
    let range = read_f64s(&file, "range", &mut budget)?;
    if range.len() < 2 {
        return Err(invalid("range coordinate needs at least two gates"));
    }
    let range_coord = range_coordinate(&range[..n_gates.min(range.len())], ngates);

    // Sweep index ranges; a missing sweep dimension means one sweep.
    let fixed_angles = optional_f64s(&file, "fixed_angle", &mut budget)?.unwrap_or_default();
    check_sweep_count(fixed_angles.len(), "CfRadial fixed_angle")
        .map_err(CfRadialError::LimitExceeded)?;
    let sweep_starts =
        optional_f64s(&file, "sweep_start_ray_index", &mut budget)?.unwrap_or_default();
    let sweep_ends = optional_f64s(&file, "sweep_end_ray_index", &mut budget)?.unwrap_or_default();
    let sweep_count = fixed_angles.len().max(1);
    let sweep_modes = read_sweep_strings(&file, "sweep_mode", sweep_count);
    let follow_modes = read_sweep_strings(&file, "follow_mode", sweep_count);
    let prt_modes = read_sweep_strings(&file, "prt_mode", sweep_count);
    let polarization_modes = read_sweep_strings(&file, "polarization_mode", sweep_count);
    let target_scan_rates = optional_f64s(&file, "target_scan_rate", &mut budget)?;
    let rays_are_indexed = read_sweep_strings(&file, "rays_are_indexed", sweep_count);
    let ray_angle_res = optional_f64s(&file, "ray_angle_res", &mut budget)?;

    // Time: seconds since the `time.units` reference, else since
    // `time_coverage_start`.
    let coverage_start = parse_time_var_or_attr(&file, "time_coverage_start");
    let coverage_end = parse_time_var_or_attr(&file, "time_coverage_end");
    let time_reference = time_units_reference(&file)
        .or(coverage_start)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let ray_seconds = optional_f64s(&file, "time", &mut budget)?;

    let instrument_name = file
        .gattr_str("instrument_name")
        .or_else(|| file.gattr_str("site_name"))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("CFRAD")
        .to_owned();
    let mut volume = Volume::new(instrument_name, time_reference);
    volume.attrs.site_name = file.gattr_str("site_name").map(str::to_owned);
    volume.attrs.title = metadata_text(&file, "title");
    volume.attrs.institution = metadata_text(&file, "institution");
    volume.attrs.references = metadata_text(&file, "references");
    volume.attrs.source = metadata_text(&file, "source");
    volume.attrs.history = metadata_text(&file, "history");
    volume.attrs.comment = metadata_text(&file, "comment");
    volume.attrs.platform_is_mobile = gattr_bool(&file, "platform_is_mobile").unwrap_or(false);
    volume.attrs.ray_times_increase = gattr_bool(&file, "ray_times_increase");
    volume.attrs.simulated = gattr_bool(&file, "simulated").unwrap_or(false);
    volume.attrs.other = file
        .gattrs
        .iter()
        .filter(|(name, _)| !SLOTTED_GLOBAL_ATTRS.contains(&name.as_str()))
        .map(|(name, value)| (name.as_str().into(), attr_value(value)))
        .collect();
    volume.time_coverage = match (coverage_start, coverage_end) {
        (Some(start), Some(end)) => Some(recast_radar_core::model::TimeCoverage { start, end }),
        _ => None,
    };
    volume.volume_number = file
        .read_var("volume_number")
        .ok()
        .and_then(|array| array.get_f64(0))
        .filter(|value| value.is_finite() && value.fract() == 0.0)
        .and_then(|value| i32::try_from(value as i64).ok());
    volume.platform_type = char_var_text(&file, "platform_type")
        .and_then(|text| PlatformType::parse(&text))
        .unwrap_or(PlatformType::Fixed);
    volume.instrument_type = char_var_text(&file, "instrument_type")
        .and_then(|text| InstrumentType::parse(&text))
        .unwrap_or(InstrumentType::Radar);
    volume.primary_axis =
        char_var_text(&file, "primary_axis").and_then(|text| PrimaryAxis::parse(&text));
    volume.status_str = char_var_text(&file, "status_str").filter(|text| !text.is_empty());
    let scalar = |name: &str| -> Option<f64> {
        let var = file.vars.get(name)?;
        if !var.dim_ids.is_empty() && var.dim_ids.as_slice() != [time_dim] {
            return None;
        }
        file.read_var(name).ok().and_then(|array| array.get_f64(0))
    };
    // A moving platform's root location is its first ray's.
    volume.location.latitude_deg = scalar("latitude");
    volume.location.longitude_deg = scalar("longitude");
    volume.location.altitude_m = scalar("altitude");
    volume.location.altitude_agl_m = scalar("altitude_agl");
    volume.provenance.source_format = SourceFormat::CfRadial1;
    volume.provenance.source_version = Some(
        file.gattr_str("version")
            .map(str::to_owned)
            .unwrap_or_else(|| "CfRadial-1".to_owned()),
    );
    volume.provenance.source_conventions = file.gattr_str("Conventions").map(str::to_owned);
    volume.provenance.compression = Some("cfradial1-netcdf3".to_owned());
    volume.provenance.polarization_note = metadata_text(&file, "polarization");
    volume.provenance.calibration_note = metadata_text(&file, "calibration");
    let simulation = SimulationProvenance {
        forward_operator: metadata_text(&file, "forward_operator"),
        forward_operator_config: metadata_text(&file, "forward_operator_config"),
        source_model: metadata_text(&file, "source_model"),
        microphysics_scheme: metadata_text(&file, "microphysics_scheme"),
        scattering_model: metadata_text(&file, "scattering_model"),
    };
    if simulation != SimulationProvenance::default() {
        volume.attrs.simulated = true;
        volume.simulation = Some(Box::new(simulation));
    }

    // Radar parameters.
    volume.radar_parameters.frequency_hz = cfradial_frequency_hz(&file);
    volume.radar_parameters.beam_width_h_deg =
        cfradial_beam_width_deg(&file, &["radar_beam_width_h", "radar_beam_width_h_deg"]);
    volume.radar_parameters.beam_width_v_deg =
        cfradial_beam_width_deg(&file, &["radar_beam_width_v", "radar_beam_width_v_deg"]);
    volume.radar_parameters.antenna_gain_h_db =
        numeric_var_first(&file, "radar_antenna_gain_h").map(|v| v as f32);
    volume.radar_parameters.antenna_gain_v_db =
        numeric_var_first(&file, "radar_antenna_gain_v").map(|v| v as f32);
    volume.radar_parameters.receiver_bandwidth_hz = numeric_var_first(&file, "radar_rx_bandwidth")
        .or_else(|| numeric_var_first(&file, "radar_receiver_bandwidth"))
        .map(|v| v as f32);
    volume.radar_parameters.pulse_width_s = cfradial_pulse_width_us(&file).map(|us| us * 1e-6);
    volume.radar_parameters.prt_s = cfradial_prt_s(&file);
    volume.radar_parameters.unambiguous_range_m =
        cfradial_unambiguous_range_km(&file).map(|km| km * 1000.0);
    volume.radar_calibration = read_radar_calibration(&file, time_reference);

    // Scan strategy (BowEcho export attributes).
    volume.scan.name = metadata_text(&file, "scan_name");
    let scan_id_text = metadata_text(&file, "scan_id").or_else(|| {
        file.gattr_f64("scan_id")
            .filter(|value| value.is_finite())
            .map(|value| {
                if value.fract() == 0.0 {
                    format!("{value:.0}")
                } else {
                    value.to_string()
                }
            })
    });
    volume.scan.id = scan_id_text
        .as_deref()
        .and_then(|text| text.parse::<i64>().ok());
    volume.scan.vcp_pattern = file
        .gattr_f64("vcp_pattern")
        .filter(|value| value.is_finite() && value.fract() == 0.0)
        .and_then(|value| u16::try_from(value as i64).ok())
        .filter(|pattern| *pattern > 0);
    let mut definition = ScanDefinition {
        source_document: metadata_text(&file, "vcp_source_document"),
        source_revision: metadata_text(&file, "vcp_source_revision"),
        source_rda_build: metadata_text(&file, "vcp_source_rda_build"),
        source_figure: metadata_text(&file, "vcp_source_figure"),
        pulse_length: metadata_text(&file, "vcp_pulse_length"),
        adaptations: metadata_text(&file, "vcp_adaptations"),
        scan_id_text: scan_id_text
            .filter(|text| volume.scan.id.is_none_or(|id| id.to_string() != *text)),
        legs: Vec::new(),
    };

    // Scan legs (BowEcho catalog-backed synthetic volumes).
    let source_row_indices = optional_f64s(&file, "vcp_source_row_index", &mut budget)?;
    let vcp_azimuth_rates = optional_f64s(&file, "vcp_azimuth_rate", &mut budget)?;
    let vcp_source_periods = optional_f64s(&file, "vcp_source_period", &mut budget)?;
    let vcp_waveform_codes = optional_f64s(&file, "vcp_waveform_code", &mut budget)?;
    let vcp_moment_coverage_codes = optional_f64s(&file, "vcp_moment_coverage_code", &mut budget)?;
    let surveillance_prf_codes = optional_f64s(&file, "vcp_surveillance_prf_code", &mut budget)?;
    let surveillance_pulse_counts =
        optional_f64s(&file, "vcp_surveillance_pulse_count", &mut budget)?;
    let doppler_prf_codes = optional_f64s(&file, "vcp_doppler_prf_code", &mut budget)?;
    let doppler_pulse_counts = optional_f64s(&file, "vcp_doppler_pulse_count", &mut budget)?;
    let has_scan_leg_metadata = source_row_indices.is_some()
        || vcp_azimuth_rates.is_some()
        || vcp_source_periods.is_some()
        || vcp_waveform_codes.is_some()
        || vcp_moment_coverage_codes.is_some()
        || surveillance_prf_codes.is_some()
        || surveillance_pulse_counts.is_some()
        || doppler_prf_codes.is_some()
        || doppler_pulse_counts.is_some();

    // Field variables: anything shaped (time, range), in file order.
    let mut fields: Vec<&NcVar> = file
        .vars
        .values()
        .filter(|var| var.dim_ids.as_slice() == [time_dim, range_dim])
        .collect();
    fields.sort_by_key(|var| var.index);
    if fields.is_empty() {
        return Err(invalid("CfRadial volume has no (time, range) fields"));
    }
    // Per-ray variables without a typed slot, in file order.
    let mut extra_ray_vars: Vec<&NcVar> = file
        .vars
        .values()
        .filter(|var| {
            var.dim_ids.first() == Some(&time_dim)
                && var.dim_ids.len() <= 2
                && var.dim_ids.as_slice() != [time_dim, range_dim]
                && !SLOTTED_RAY_VARS.contains(&var.name.as_str())
        })
        .collect();
    extra_ray_vars.sort_by_key(|var| var.index);
    // Root variables without a typed slot: scalars and char strings.
    let mut extra_root_vars: Vec<&NcVar> = file
        .vars
        .values()
        .filter(|var| {
            (var.dim_ids.is_empty()
                || (var.dim_ids.len() == 1
                    && var.dim_ids[0] != time_dim
                    && var.dim_ids[0] != range_dim
                    && var.nc_type() == 2))
                && !SLOTTED_ROOT_VARS.contains(&var.name.as_str())
        })
        .collect();
    extra_root_vars.sort_by_key(|var| var.index);
    for var in extra_root_vars {
        if let Some(extra) = extra_variable(&file, var, None) {
            volume.extra_vars.push(extra);
        }
    }

    // Validate every sweep's ray range before building anything. Sweeps that
    // share rays would each copy the same field rows, so a header claiming
    // many overlapping sweeps multiplied the decoded size (fuzz regression
    // `fuzz-cfradial-overlapping-sweep-ray-ranges`).
    let ray_ranges: Vec<Option<(usize, usize)>> = (0..sweep_count)
        .map(|sweep| sweep_ray_range(&sweep_starts, &sweep_ends, sweep, n_rays))
        .collect();
    check_disjoint_sweeps(&ray_ranges)?;

    // Build sweep geometry first, then read each full (time, range) field
    // once and distribute its rows across every sweep.
    let mut sweeps: Vec<SweepBuild> = Vec::with_capacity(sweep_count);
    for (index, ray_range) in ray_ranges.into_iter().enumerate() {
        let Some((start_ray, end_ray)) = ray_range else {
            volume.provenance.decode.skipped_message_count += 1;
            continue;
        };
        let mode = sweep_modes
            .get(index)
            .cloned()
            .flatten()
            .map(|text| SweepMode::parse(&text))
            .unwrap_or(SweepMode::AzimuthSurveillance);
        let fixed = fixed_angles.get(index).copied().unwrap_or_else(|| {
            fallback_fixed_angle(
                matches!(mode, SweepMode::Rhi | SweepMode::ManualRhi),
                &azimuth[start_ray..=end_ray],
                &elevation[start_ray..=end_ray],
            )
        }) as f32;
        let sweep_rays = end_ray - start_ray + 1;
        budget
            .charge(sweep_rays, 16 * size_of::<f64>(), "CfRadial sweep rays")
            .map_err(CfRadialError::LimitExceeded)?;
        let mut sweep = Sweep::new(sweeps.len() as u32, mode, fixed);
        sweep.elevation_number = u16::try_from(index).ok();
        sweep.follow_mode = follow_modes
            .get(index)
            .cloned()
            .flatten()
            .map(|text| FollowMode::parse(&text));
        sweep.prt_mode = prt_modes
            .get(index)
            .cloned()
            .flatten()
            .map(|text| PrtMode::parse(&text));
        sweep.polarization_mode = polarization_modes
            .get(index)
            .cloned()
            .flatten()
            .map(|text| PolarizationMode::parse(&text));
        sweep.target_scan_rate_deg_per_s = numeric_f32_at(&target_scan_rates, index);
        sweep.rays_are_indexed = rays_are_indexed
            .get(index)
            .cloned()
            .flatten()
            .and_then(|text| parse_bool(&text));
        sweep.rays_angle_resolution_deg = numeric_f32_at(&ray_angle_res, index);
        sweep.range = range_coord.clone();
        sweep.reserve_rays(sweep_rays);
        let rays = start_ray..=end_ray;
        for ray in rays.clone() {
            let time_s = ray_seconds
                .as_ref()
                .and_then(|seconds| seconds.get(ray))
                .copied()
                .unwrap_or(0.0);
            sweep.push_ray(
                time_s,
                (azimuth[ray] as f32).rem_euclid(360.0),
                elevation[ray] as f32,
            );
        }
        let slice_f32 = |values: &Option<Vec<f64>>, scale: f64| -> Option<Vec<f32>> {
            values.as_ref().map(|values| {
                values[rays.clone()]
                    .iter()
                    .map(|value| {
                        if value.is_finite() {
                            (value * scale) as f32
                        } else {
                            f32::NAN
                        }
                    })
                    .collect()
            })
        };
        sweep.ray_vars.nyquist_velocity_mps = slice_f32(&nyquist, 1.0);
        sweep.ray_vars.unambiguous_range_m = slice_f32(&unambiguous_range, unambiguous_range_scale);
        sweep.ray_vars.prt_s = slice_f32(&prt, prt_scale);
        sweep.ray_vars.prt_ratio = slice_f32(&prt_ratio, 1.0);
        sweep.ray_vars.pulse_width_s = slice_f32(&pulse_width, pulse_width_scale);
        sweep.ray_vars.scan_rate_deg_per_s = slice_f32(&scan_rate, 1.0);
        sweep.ray_vars.independent_samples = slice_f32(&independent_samples, 1.0);
        let slice_i32 = |values: &Option<Vec<f64>>| -> Option<Vec<i32>> {
            values.as_ref().map(|values| {
                values[rays.clone()]
                    .iter()
                    .map(|value| {
                        if value.is_finite()
                            && value.fract() == 0.0
                            && (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(value)
                        {
                            *value as i32
                        } else {
                            -9999
                        }
                    })
                    .collect()
            })
        };
        sweep.ray_vars.n_samples = slice_i32(&n_samples).or_else(|| slice_i32(&pulse_count));
        sweep.ray_vars.calib_index = slice_i32(&calib_index);
        sweep.ray_vars.antenna_transition = antenna_transition.as_ref().map(|values| {
            values[rays.clone()]
                .iter()
                .map(|value| u8::from(*value != 0.0 && value.is_finite()))
                .collect()
        });
        if let Some(platform) = &platform {
            sweep.platform_track = Some(Box::new(platform.slice(rays.clone())));
        }
        if transmit_power_h.is_some() || transmit_power_v.is_some() {
            sweep.monitoring = Some(Box::new(recast_radar_core::model::Monitoring {
                radar_measured_transmit_power_h_dbm: slice_f32(&transmit_power_h, 1.0),
                radar_measured_transmit_power_v_dbm: slice_f32(&transmit_power_v, 1.0),
                ..Default::default()
            }));
        }
        for var in &extra_ray_vars {
            if let Some(extra) = extra_variable(&file, var, Some(rays.clone())) {
                sweep.extra_vars.push(extra);
            }
        }
        let leg = ScanLeg {
            source_row_index: numeric_u16_at(&source_row_indices, index),
            elevation_deg: has_scan_leg_metadata.then_some(fixed),
            azimuth_rate_deg_per_second: numeric_f32_at(&vcp_azimuth_rates, index),
            source_period_seconds: numeric_f32_at(&vcp_source_periods, index),
            waveform: numeric_u8_at(&vcp_waveform_codes, index)
                .and_then(waveform_from_code)
                .map(str::to_owned),
            moment_coverage: numeric_u8_at(&vcp_moment_coverage_codes, index)
                .and_then(moment_coverage_from_code)
                .map(str::to_owned),
            surveillance_prf_code: numeric_u8_at(&surveillance_prf_codes, index),
            surveillance_pulse_count: numeric_u16_at(&surveillance_pulse_counts, index),
            doppler_prf_code: numeric_u8_at(&doppler_prf_codes, index),
            doppler_pulse_count: numeric_u16_at(&doppler_pulse_counts, index),
        };
        sweeps.push(SweepBuild {
            start_ray,
            end_ray,
            sweep,
            leg,
        });
    }
    if sweeps.is_empty() {
        return Err(invalid("CfRadial volume decoded no sweeps"));
    }

    let expected_values = n_rays
        .checked_mul(n_gates)
        .ok_or_else(|| invalid("CfRadial field dimensions overflow addressable memory"))?;
    for var in fields {
        let name = FieldName::parse(&var.name);
        let (quantity, polarization) = Quantity::classify(&var.name, var.attr_str("standard_name"));
        let raw = file.read_var(&var.name)?;
        if raw.len() < expected_values {
            return Err(invalid(format!(
                "CfRadial field '{}' has {} values; expected at least {expected_values}",
                var.name,
                raw.len()
            )));
        }
        let word_bytes = match &raw {
            NcArray::I8(_) | NcArray::Char(_) => 1,
            NcArray::I16(_) => 2,
            NcArray::I32(_) | NcArray::F32(_) => 4,
            NcArray::F64(_) => 8,
        };
        let coding = FieldCoding::of(var);
        for build in &mut sweeps {
            if !scan_leg_allows_quantity(&build.leg, quantity) {
                continue;
            }
            let sweep_rays = build.end_ray - build.start_ray + 1;
            budget
                .charge(sweep_rays, n_gates * word_bytes, "CfRadial field")
                .map_err(CfRadialError::LimitExceeded)?;
            let rows = build.start_ray * n_gates..(build.end_ray + 1) * n_gates;
            let data = match &raw {
                NcArray::I8(values) => FieldData::I8 {
                    values: values[rows].to_vec(),
                    coding: coding.int(),
                },
                NcArray::Char(values) => FieldData::U8 {
                    values: values[rows].to_vec(),
                    coding: coding.int(),
                },
                NcArray::I16(values) => FieldData::I16 {
                    values: values[rows].to_vec(),
                    coding: coding.int(),
                },
                NcArray::I32(values) => FieldData::I32 {
                    values: values[rows].to_vec(),
                    coding: coding.int(),
                },
                NcArray::F32(values) => FieldData::F32 {
                    values: values[rows].to_vec(),
                    coding: FloatCoding {
                        transform: coding.float_transform(),
                        fill_value: coding.fill.map(|fill| fill as f32),
                        undetect: None,
                    },
                },
                NcArray::F64(values) => FieldData::F64 {
                    values: values[rows].to_vec(),
                    coding: FloatCoding {
                        transform: coding.float_transform(),
                        fill_value: coding.fill,
                        undetect: None,
                    },
                },
            };
            let mut field = Field::new(name.clone(), GateMapping::IDENTITY, ngates, data);
            field.quantity = quantity;
            field.polarization = polarization;
            field.attrs.standard_name = var
                .attr_str("standard_name")
                .map(|s| Cow::Owned(s.to_owned()));
            field.attrs.long_name = var.attr_str("long_name").map(|s| Cow::Owned(s.to_owned()));
            field.attrs.units = var.attr_str("units").map(|s| Cow::Owned(s.to_owned()));
            field.attrs.sampling_ratio = var.attr_f64("sampling_ratio").map(|v| v as f32);
            field.attrs.other = var
                .attrs
                .iter()
                .filter(|(attr, _)| {
                    !matches!(
                        attr.as_str(),
                        "standard_name"
                            | "long_name"
                            | "units"
                            | "sampling_ratio"
                            | "scale_factor"
                            | "add_offset"
                            | "_FillValue"
                            | "missing_value"
                    )
                })
                .map(|(attr, value)| (attr.as_str().into(), attr_value(value)))
                .collect();
            build
                .sweep
                .add_field(field)
                .map_err(|err| invalid(format!("CfRadial field '{}': {err}", var.name)))?;
        }
    }

    let legs: Vec<ScanLeg> = sweeps.iter().map(|build| build.leg.clone()).collect();
    if legs.iter().any(|leg| *leg != ScanLeg::default()) {
        definition.legs = legs;
    }
    if definition != ScanDefinition::default() {
        volume.scan.definition = Some(Box::new(definition));
    }
    volume.provenance.decode.message_count = sweep_count;
    volume.sweeps = sweeps.into_iter().map(|build| build.sweep).collect();
    volume.provenance.decode.decoded_ray_count = volume.sweeps.iter().map(Sweep::nrays).sum();
    volume.seal().map_err(|err| invalid(err.to_string()))?;
    if volume.time_coverage.is_none() {
        volume.time_coverage = volume.ray_time_extent();
    }
    Ok(volume)
}

struct SweepBuild {
    start_ray: usize,
    end_ray: usize,
    sweep: Sweep,
    leg: ScanLeg,
}

/// A field's CF packing as the file states it.
struct FieldCoding {
    scale: f64,
    offset: f64,
    attr_width: FloatWidth,
    fill: Option<f64>,
}

impl FieldCoding {
    fn of(var: &NcVar) -> Self {
        let attr_width = match (var.attrs.get("scale_factor"), var.attrs.get("add_offset")) {
            (Some(NcValue::Floats(_)), _) | (None, Some(NcValue::Floats(_))) => FloatWidth::F32,
            _ => FloatWidth::F64,
        };
        Self {
            scale: var.attr_f64("scale_factor").unwrap_or(1.0),
            offset: var.attr_f64("add_offset").unwrap_or(0.0),
            attr_width,
            fill: var
                .attr_f64("_FillValue")
                .or_else(|| var.attr_f64("missing_value")),
        }
    }

    fn transform(&self) -> LinearTransform {
        LinearTransform::CfScaleOffset {
            scale_factor: self.scale,
            add_offset: self.offset,
            attr_width: self.attr_width,
        }
    }

    /// The transform of a float field: `None` when the values are physical.
    fn float_transform(&self) -> Option<LinearTransform> {
        (self.scale != 1.0 || self.offset != 0.0).then(|| self.transform())
    }

    fn int<T: recast_radar_core::model::PackedInt>(&self) -> IntCoding<T> {
        IntCoding {
            transform: self.transform(),
            fill_value: self
                .fill
                .filter(|fill| fill.is_finite() && fill.fract() == 0.0)
                .and_then(|fill| T::from_i64(fill as i64)),
            undetect: None,
            range_folded: None,
            valid_range: None,
        }
    }
}

/// The `range` coordinate: uniform when every centre sits on the line
/// through the first and last centres within 1% of a gate (float32 files
/// carry rounding of that order), else the explicit centres.
fn range_coordinate(range: &[f64], ngates: u32) -> RangeCoord {
    let first = range[0];
    let spacing = (range[range.len() - 1] - first) / (range.len() - 1) as f64;
    let uniform = spacing > 0.0
        && spacing.is_finite()
        && range.iter().enumerate().all(|(gate, center)| {
            (center - (first + gate as f64 * spacing)).abs() <= 1e-2 * spacing
        });
    if uniform {
        RangeCoord::Uniform {
            first_center_m: first,
            spacing_m: spacing,
            ngates,
        }
    } else {
        RangeCoord::Explicit {
            centers_m: range.iter().map(|value| *value as f32).collect(),
        }
    }
}

/// The rays `start..=end` of `sweep` from `sweep_start_ray_index` and
/// `sweep_end_ray_index`; a missing value means the first or last ray.
/// `None` (the sweep is skipped) when an index is not a non-negative integer
/// (fill values, garbage) or the sweep starts after it ends. An end past the
/// last ray is clamped to it, which keeps the rays a truncated `time`
/// dimension still holds.
fn sweep_ray_range(
    starts: &[f64],
    ends: &[f64],
    sweep: usize,
    n_rays: usize,
) -> Option<(usize, usize)> {
    let last_ray = n_rays.checked_sub(1)?;
    let index = |values: &[f64], missing: usize| match values.get(sweep) {
        None => Some(missing),
        // `as` saturates, and a start past the last ray fails `start <= end`.
        Some(value) => {
            (value.is_finite() && *value >= 0.0 && value.fract() == 0.0).then_some(*value as usize)
        }
    };
    let start = index(starts, 0)?;
    let end = index(ends, last_ray)?.min(last_ray);
    (start <= end).then_some((start, end))
}

/// CfRadial sweeps partition the rays: reject two sweeps whose ray ranges
/// overlap instead of duplicating the shared rows into both.
fn check_disjoint_sweeps(ray_ranges: &[Option<(usize, usize)>]) -> Result<()> {
    let mut by_start: Vec<(usize, usize, usize)> = ray_ranges
        .iter()
        .enumerate()
        .filter_map(|(sweep, range)| range.map(|(start, end)| (start, end, sweep)))
        .collect();
    by_start.sort_unstable();
    // Sorted by start, any overlap shows up between neighbours.
    for pair in by_start.windows(2) {
        if let [(_, previous_end, previous), (start, end, sweep)] = pair
            && start <= previous_end
        {
            return Err(invalid(format!(
                "CfRadial sweeps {} and {} overlap: sweep_start_ray_index/sweep_end_ray_index \
                 put rays {start}..={} in both",
                previous.min(sweep),
                previous.max(sweep),
                end.min(previous_end)
            )));
        }
    }
    Ok(())
}

fn numeric_at(values: &Option<Vec<f64>>, index: usize) -> Option<f64> {
    values
        .as_ref()?
        .get(index)
        .copied()
        .filter(|value| value.is_finite() && *value != -9999.0)
}

fn numeric_f32_at(values: &Option<Vec<f64>>, index: usize) -> Option<f32> {
    numeric_at(values, index)
        .filter(|value| value.abs() <= f32::MAX as f64)
        .map(|value| value as f32)
}

fn numeric_u8_at(values: &Option<Vec<f64>>, index: usize) -> Option<u8> {
    let value = numeric_at(values, index)?;
    (value.fract() == 0.0)
        .then(|| u8::try_from(value as i64).ok())
        .flatten()
}

fn numeric_u16_at(values: &Option<Vec<f64>>, index: usize) -> Option<u16> {
    let value = numeric_at(values, index)?;
    (value.fract() == 0.0)
        .then(|| u16::try_from(value as i64).ok())
        .flatten()
}

fn waveform_from_code(code: u8) -> Option<&'static str> {
    Some(match code {
        1 => "CS",
        2 => "CD/W",
        3 => "B",
        4 => "CD/WO",
        5 => "SZCS",
        6 => "SZCD",
        _ => return None,
    })
}

fn moment_coverage_from_code(code: u8) -> Option<&'static str> {
    Some(match code {
        1 => "surveillance",
        2 => "doppler",
        3 => "all",
        _ => return None,
    })
}

fn scan_leg_allows_quantity(leg: &ScanLeg, quantity: Quantity) -> bool {
    let doppler = matches!(
        quantity,
        Quantity::RadialVelocity | Quantity::DealiasedRadialVelocity | Quantity::SpectrumWidth
    );
    match leg.moment_coverage.as_deref() {
        Some("surveillance") => !doppler,
        Some("doppler") => doppler,
        _ => true,
    }
}

/// CfRadial's fixed angle is elevation for PPI sweeps and azimuth for RHI
/// sweeps. Azimuth needs a circular mean so a 359-degree/1-degree RHI points
/// north, rather than being mislabeled as 180 degrees when `fixed_angle` is
/// absent.
fn fallback_fixed_angle(rhi: bool, azimuth: &[f64], elevation: &[f64]) -> f64 {
    if rhi {
        circular_mean_degrees(azimuth)
            .or_else(|| azimuth.iter().copied().find(|value| value.is_finite()))
            .map(|value| value.rem_euclid(360.0))
            .unwrap_or(0.0)
    } else {
        arithmetic_mean(elevation).unwrap_or(0.0)
    }
}

fn circular_mean_degrees(values: &[f64]) -> Option<f64> {
    let mut sin_sum = 0.0;
    let mut cos_sum = 0.0;
    let mut count = 0usize;
    for value in values.iter().copied().filter(|value| value.is_finite()) {
        let radians = value.to_radians();
        sin_sum += radians.sin();
        cos_sum += radians.cos();
        count += 1;
    }
    if count == 0 || sin_sum.hypot(cos_sum) <= f64::EPSILON {
        return None;
    }
    Some(sin_sum.atan2(cos_sum).to_degrees().rem_euclid(360.0))
}

fn arithmetic_mean(values: &[f64]) -> Option<f64> {
    let mut sum = 0.0;
    let mut count = 0usize;
    for value in values.iter().copied().filter(|value| value.is_finite()) {
        sum += value;
        count += 1;
    }
    if count == 0 {
        None
    } else {
        Some(sum / count as f64)
    }
}

/// Read a numeric variable as f64, charging the widened array to `budget`
/// (an 8-bit variable grows eightfold).
fn read_f64s(file: &Nc3File<'_>, name: &str, budget: &mut DecodeBudget) -> Result<Vec<f64>> {
    let raw = file.read_var(name)?;
    if matches!(raw, NcArray::Char(_)) {
        return Err(invalid(format!("variable '{name}' is not numeric")));
    }
    let count = raw.len();
    budget
        .charge(count, size_of::<f64>(), "CfRadial numeric variable")
        .map_err(CfRadialError::LimitExceeded)?;
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        out.push(
            raw.get_f64(index)
                .ok_or_else(|| invalid(format!("variable '{name}' is not numeric")))?,
        );
    }
    Ok(out)
}

/// [`read_f64s`] for an optional variable: a missing or unusable variable is
/// `None`, but exceeding a resource limit is still an error.
fn optional_f64s(
    file: &Nc3File<'_>,
    name: &str,
    budget: &mut DecodeBudget,
) -> Result<Option<Vec<f64>>> {
    match read_f64s(file, name, budget) {
        Ok(values) => Ok(Some(values)),
        Err(error @ CfRadialError::LimitExceeded(_)) => Err(error),
        Err(_) => Ok(None),
    }
}

/// Read a numeric variable only when it is exactly aligned to the CfRadial
/// `time` dimension. A scalar, sweep-level value, or malformed short array is
/// not silently broadcast across rays.
fn aligned_time_values(
    file: &Nc3File<'_>,
    name: &str,
    time_dim: usize,
    n_rays: usize,
    budget: &mut DecodeBudget,
) -> Result<Option<Vec<f64>>> {
    let Some(var) = file.vars.get(name) else {
        return Ok(None);
    };
    if var.dim_ids.as_slice() != [time_dim] {
        return Ok(None);
    }
    Ok(optional_f64s(file, name, budget)?.filter(|values| values.len() == n_rays))
}

/// Per-ray platform position and attitude of a moving platform
/// (`latitude(time)`, ...). `None` for a fixed platform.
fn read_platform_track(
    file: &Nc3File<'_>,
    time_dim: usize,
    n_rays: usize,
    budget: &mut DecodeBudget,
) -> Result<Option<PlatformTrack>> {
    let (Some(latitude), Some(longitude), Some(altitude)) = (
        aligned_time_values(file, "latitude", time_dim, n_rays, budget)?,
        aligned_time_values(file, "longitude", time_dim, n_rays, budget)?,
        aligned_time_values(file, "altitude", time_dim, n_rays, budget)?,
    ) else {
        return Ok(None);
    };
    let angles = |name: &str, budget: &mut DecodeBudget| -> Result<Option<Vec<f32>>> {
        Ok(aligned_time_values(file, name, time_dim, n_rays, budget)?
            .map(|values| values.into_iter().map(|v| v as f32).collect()))
    };
    Ok(Some(PlatformTrack {
        latitude_deg: latitude,
        longitude_deg: longitude,
        altitude_m: altitude,
        altitude_agl_m: aligned_time_values(file, "altitude_agl", time_dim, n_rays, budget)?,
        heading_deg: angles("heading", budget)?,
        roll_deg: angles("roll", budget)?,
        pitch_deg: angles("pitch", budget)?,
        drift_deg: angles("drift", budget)?,
        rotation_deg: angles("rotation", budget)?,
        tilt_deg: angles("tilt", budget)?,
    }))
}

trait SliceRays {
    fn slice(&self, rays: std::ops::RangeInclusive<usize>) -> Self;
}

impl SliceRays for PlatformTrack {
    fn slice(&self, rays: std::ops::RangeInclusive<usize>) -> Self {
        let take_f64 = |values: &Vec<f64>| values[rays.clone()].to_vec();
        let take_f32 =
            |values: &Option<Vec<f32>>| values.as_ref().map(|v| v[rays.clone()].to_vec());
        Self {
            latitude_deg: take_f64(&self.latitude_deg),
            longitude_deg: take_f64(&self.longitude_deg),
            altitude_m: take_f64(&self.altitude_m),
            altitude_agl_m: self
                .altitude_agl_m
                .as_ref()
                .map(|v| v[rays.clone()].to_vec()),
            heading_deg: take_f32(&self.heading_deg),
            roll_deg: take_f32(&self.roll_deg),
            pitch_deg: take_f32(&self.pitch_deg),
            drift_deg: take_f32(&self.drift_deg),
            rotation_deg: take_f32(&self.rotation_deg),
            tilt_deg: take_f32(&self.tilt_deg),
        }
    }
}

/// A variable without a typed slot, verbatim: the whole array (root
/// variables) or the rows of `rays` (per-ray variables, first dimension
/// `time`).
fn extra_variable(
    file: &Nc3File<'_>,
    var: &NcVar,
    rays: Option<std::ops::RangeInclusive<usize>>,
) -> Option<ExtraVariable> {
    let dims = file.var_dims(var);
    let array = file.read_var(&var.name).ok()?;
    let (dim_names, shape, values): (Vec<Box<str>>, Vec<u32>, ArrayBuf) = match rays {
        Some(rays) => {
            let row = dims.iter().skip(1).product::<usize>().max(1);
            let range = *rays.start() * row..(*rays.end() + 1) * row;
            if range.end > array.len() {
                return None;
            }
            let mut dim_names: Vec<Box<str>> = vec!["time".into()];
            let mut shape = vec![u32::try_from(rays.end() - rays.start() + 1).ok()?];
            for (index, len) in dims.iter().enumerate().skip(1) {
                dim_names.push(file.dims[var.dim_ids[index]].0.as_str().into());
                shape.push(u32::try_from(*len).ok()?);
            }
            let values = match array {
                NcArray::I8(v) => ArrayBuf::I8(v[range].to_vec()),
                NcArray::Char(v) => {
                    // One string per ray.
                    ArrayBuf::Text(
                        v[range]
                            .chunks(row)
                            .map(|chars| {
                                let text =
                                    chars.split(|byte| *byte == 0).next().unwrap_or_default();
                                String::from_utf8_lossy(text).into()
                            })
                            .collect(),
                    )
                }
                NcArray::I16(v) => ArrayBuf::I16(v[range].to_vec()),
                NcArray::I32(v) => ArrayBuf::I32(v[range].to_vec()),
                NcArray::F32(v) => ArrayBuf::F32(v[range].to_vec()),
                NcArray::F64(v) => ArrayBuf::F64(v[range].to_vec()),
            };
            if let ArrayBuf::Text(_) = values {
                dim_names.truncate(1);
                shape.truncate(1);
            }
            (dim_names, shape, values)
        }
        None => {
            let values = match array {
                NcArray::Char(v) => {
                    let text = v.split(|byte| *byte == 0).next().unwrap_or_default();
                    ArrayBuf::Text(vec![String::from_utf8_lossy(text).into()])
                }
                NcArray::I8(v) => ArrayBuf::I8(v),
                NcArray::I16(v) => ArrayBuf::I16(v),
                NcArray::I32(v) => ArrayBuf::I32(v),
                NcArray::F32(v) => ArrayBuf::F32(v),
                NcArray::F64(v) => ArrayBuf::F64(v),
            };
            (Vec::new(), Vec::new(), values)
        }
    };
    Some(ExtraVariable {
        name: var.name.as_str().into(),
        dims: dim_names,
        shape,
        values,
        attrs: var
            .attrs
            .iter()
            .map(|(name, value)| (name.as_str().into(), attr_value(value)))
            .collect(),
    })
}

fn attr_value(value: &NcValue) -> AttrValue {
    match value {
        NcValue::Str(text) => AttrValue::Text(text.as_str().into()),
        NcValue::Floats(values) => match values.as_slice() {
            [single] => AttrValue::Scalar(Scalar::F32(*single)),
            _ => AttrValue::Array(ArrayBuf::F32(values.clone())),
        },
        NcValue::Doubles(values) => match values.as_slice() {
            [single] => AttrValue::Scalar(Scalar::F64(*single)),
            _ => AttrValue::Array(ArrayBuf::F64(values.clone())),
        },
        NcValue::Ints(values) => match values.as_slice() {
            [single] => AttrValue::Scalar(Scalar::I64(*single)),
            _ => AttrValue::Array(ArrayBuf::I64(values.clone())),
        },
    }
}

/// `name(sweep, string_length)` char matrix → per-sweep strings.
fn read_sweep_strings(file: &Nc3File<'_>, name: &str, sweep_count: usize) -> Vec<Option<String>> {
    let Some(var) = file.vars.get(name) else {
        return vec![None; sweep_count];
    };
    let dims = file.var_dims(var);
    let (rows, width) = match dims.as_slice() {
        [rows, width] => (*rows, *width),
        _ => return vec![None; sweep_count],
    };
    let Ok(NcArray::Char(chars)) = file.read_var(name) else {
        return vec![None; sweep_count];
    };
    (0..sweep_count)
        .map(|sweep| {
            if sweep >= rows || (sweep + 1) * width > chars.len() {
                return None;
            }
            let raw = &chars[sweep * width..(sweep + 1) * width];
            let text = raw.split(|byte| *byte == 0).next().unwrap_or_default();
            Some(String::from_utf8_lossy(text).trim().to_owned())
        })
        .collect()
}

/// A scalar char variable as text.
fn char_var_text(file: &Nc3File<'_>, name: &str) -> Option<String> {
    match file.read_var(name).ok()? {
        NcArray::Char(chars) => {
            let text = chars.split(|byte| *byte == 0).next().unwrap_or_default();
            Some(String::from_utf8_lossy(text).trim().to_owned())
        }
        _ => None,
    }
}

fn parse_bool(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

fn gattr_bool(file: &Nc3File<'_>, name: &str) -> Option<bool> {
    match file.gattrs.get(name)? {
        NcValue::Str(text) => parse_bool(text),
        other => other.as_f64().map(|value| value != 0.0),
    }
}

fn units_of<'f>(file: &'f Nc3File<'_>, name: &str) -> Option<&'f str> {
    file.vars.get(name).and_then(|var| var.attr_str("units"))
}

/// The reference instant of `time.units` ("seconds since <ISO 8601>").
fn time_units_reference(file: &Nc3File<'_>) -> Option<DateTime<Utc>> {
    let units = units_of(file, "time")?;
    let (_, rest) = units.trim().split_once("since")?;
    parse_iso_instant(rest)
}

fn parse_time_var_or_attr(file: &Nc3File<'_>, name: &str) -> Option<DateTime<Utc>> {
    // Either a char variable or a global attribute, ISO8601 "...Z".
    let text = match file.read_var(name) {
        Ok(NcArray::Char(chars)) => {
            let bytes: Vec<u8> = chars.into_iter().take_while(|byte| *byte != 0).collect();
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => file.gattr_str(name)?.to_owned(),
    };
    parse_iso_instant(&text)
}

fn parse_iso_instant(text: &str) -> Option<DateTime<Utc>> {
    let trimmed = text.trim().trim_end_matches('Z').trim();
    let naive = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S"))
        .or_else(|_| NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S%.f"))
        .or_else(|_| NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S%.f"))
        .ok()?;
    Some(Utc.from_utc_datetime(&naive))
}

/// Operating frequencies in Hz: the `frequency` coordinate variable (values
/// above 1 MHz verbatim; MHz or GHz spellings normalized), else the
/// historical global-attribute spellings.
fn cfradial_frequency_hz(file: &Nc3File<'_>) -> Vec<f64> {
    if let Ok(values) = file.read_var("frequency") {
        let hz: Vec<f64> = (0..values.len())
            .filter_map(|index| values.get_f64(index))
            .filter(|value| value.is_finite() && *value > 0.0)
            .filter_map(|value| {
                if value > 1.0e6 {
                    Some(value)
                } else {
                    normalize_frequency_mhz(value).map(|mhz| f64::from(mhz) * 1e6)
                }
            })
            .collect();
        if !hz.is_empty() {
            return hz;
        }
    }
    for name in [
        "radar_frequency",
        "frequency",
        "frequency_ghz",
        "instrument_frequency",
    ] {
        if let Some(value) = file.gattr_f64(name)
            && let Some(mhz) = normalize_frequency_mhz(value)
        {
            return vec![f64::from(mhz) * 1e6];
        }
    }
    for name in ["radar_wavelength", "radar_wavelength_cm", "wavelength"] {
        if let Some(value) = file.gattr_f64(name)
            && let Some(mhz) = frequency_mhz_from_wavelength(value)
        {
            return vec![f64::from(mhz) * 1e6];
        }
    }
    Vec::new()
}

fn cfradial_beam_width_deg(file: &Nc3File<'_>, names: &[&str]) -> Option<f32> {
    for name in names {
        if let Some(value) = numeric_var_first(file, name).or_else(|| file.gattr_f64(name)) {
            let units = file
                .vars
                .get(*name)
                .and_then(|var| var.attr_str("units"))
                .unwrap_or("degrees")
                .to_ascii_lowercase();
            let degrees = if units.contains("rad") {
                value.to_degrees()
            } else {
                value
            };
            if degrees.is_finite() && degrees > 0.0 && degrees <= 180.0 {
                return Some(degrees as f32);
            }
        }
    }
    None
}

fn cfradial_pulse_width_us(file: &Nc3File<'_>) -> Option<f32> {
    if let Some(seconds) = time_var_seconds(file, "pulse_width") {
        return positive_f32(seconds * 1.0e6);
    }
    file.gattr_f64("pulse_width_us")
        .and_then(positive_f32)
        .or_else(|| {
            file.gattr_f64("pulse_width")
                .and_then(|seconds| positive_f32(seconds * 1.0e6))
        })
}

fn cfradial_prt_s(file: &Nc3File<'_>) -> Option<f32> {
    // A time-aligned `prt(time)` belongs to each ray, not to the volume.
    // Only a true scalar variable participates in this volume-level
    // fallback.
    if let Some(value) = numeric_scalar_var_first(file, "prt") {
        let scale = time_units_scale(file.vars.get("prt").and_then(|var| var.attr_str("units")));
        return positive_f32(value * scale);
    }
    file.gattr_f64("prt_s")
        .or_else(|| file.gattr_f64("prt"))
        .and_then(positive_f32)
}

fn cfradial_unambiguous_range_km(file: &Nc3File<'_>) -> Option<f32> {
    // As with PRT, do not collapse varying per-ray values to the first ray.
    if let Some(value) = numeric_scalar_var_first(file, "unambiguous_range") {
        let scale = range_units_to_km_scale(
            file.vars
                .get("unambiguous_range")
                .and_then(|var| var.attr_str("units")),
        );
        return positive_f32(value * scale);
    }
    file.gattr_f64("unambiguous_range_km")
        .and_then(positive_f32)
        .or_else(|| {
            file.gattr_f64("unambiguous_range")
                .and_then(|meters| positive_f32(meters / 1000.0))
        })
}

/// The `r_calib_*(r_calib)` variables as one [`RadarCalibration`] per
/// `r_calib` entry (design note 12.4: `r_calib_<name>` is Table 301-14a
/// `<name>`, `r_calib_base_dbz_1km_*` is `base_1km_*`; every other name goes
/// to `RadarCalibration::extra` verbatim). A value equal to the variable's
/// `_FillValue` (or not finite) is left unset. `r_calib_time` (ISO text or
/// seconds) becomes seconds since the volume time reference. `r_calib_index`
/// is a per-ray variable and is read with the ray variables. Empty when the
/// file has no `r_calib` dimension.
fn read_radar_calibration(
    file: &Nc3File<'_>,
    time_reference: DateTime<Utc>,
) -> Vec<RadarCalibration> {
    let Some(&(_, count)) = file.dims.iter().find(|(name, _)| name == "r_calib") else {
        return Vec::new();
    };
    if count == 0 || count > 4096 {
        return Vec::new();
    }
    let mut entries = vec![RadarCalibration::default(); count];
    for (name, var) in &file.vars {
        let Some(suffix) = name.strip_prefix("r_calib_") else {
            continue;
        };
        if suffix == "index"
            || var.dim_ids.first().map(|dim| file.dims[*dim].0.as_str()) != Some("r_calib")
        {
            continue;
        }
        let Ok(array) = file.read_var(name) else {
            continue;
        };
        if suffix == "time" {
            match &array {
                NcArray::Char(chars) => {
                    let width = chars.len() / count;
                    for (entry, text) in entries.iter_mut().zip(chars.chunks(width.max(1))) {
                        let text = text.split(|byte| *byte == 0).next().unwrap_or_default();
                        entry.time_s = parse_iso_instant(String::from_utf8_lossy(text).trim()).map(
                            |instant| (instant - time_reference).num_milliseconds() as f64 / 1000.0,
                        );
                    }
                }
                _ => {
                    let scale = time_units_scale(var.attr_str("units"));
                    for (index, entry) in entries.iter_mut().enumerate() {
                        entry.time_s = array
                            .get_f64(index)
                            .filter(|value| value.is_finite())
                            .map(|value| value * scale);
                    }
                }
            }
            continue;
        }
        let fill = var.attr_f64("_FillValue");
        let table_name = match suffix {
            "base_dbz_1km_hc" => "base_1km_hc",
            "base_dbz_1km_vc" => "base_1km_vc",
            "base_dbz_1km_hx" => "base_1km_hx",
            "base_dbz_1km_vx" => "base_1km_vx",
            other => other,
        };
        for (index, entry) in entries.iter_mut().enumerate() {
            let value = array
                .get_f64(index)
                .filter(|value| value.is_finite() && Some(*value) != fill);
            let Some(value) = value else {
                continue;
            };
            if !entry.set_float_entry(table_name, Some(value as f32)) {
                entry
                    .extra
                    .push((suffix.into(), AttrValue::Scalar(Scalar::F32(value as f32))));
            }
        }
    }
    entries
}

fn numeric_var_first(file: &Nc3File<'_>, name: &str) -> Option<f64> {
    let values = file.read_var(name).ok()?;
    (0..values.len()).find_map(|index| {
        values
            .get_f64(index)
            .filter(|value| value.is_finite() && *value > 0.0)
    })
}

fn numeric_scalar_var_first(file: &Nc3File<'_>, name: &str) -> Option<f64> {
    file.vars.get(name)?.dim_ids.is_empty().then_some(())?;
    numeric_var_first(file, name)
}

fn time_var_seconds(file: &Nc3File<'_>, name: &str) -> Option<f64> {
    let value = numeric_var_first(file, name)?;
    let units = file
        .vars
        .get(name)
        .and_then(|var| var.attr_str("units"))
        .unwrap_or("seconds")
        .trim()
        .to_ascii_lowercase();
    let seconds = if units.contains("microsecond") || matches!(units.as_str(), "us" | "µs") {
        value * 1.0e-6
    } else if units.contains("millisecond") || units == "ms" {
        value * 1.0e-3
    } else {
        value
    };
    seconds.is_finite().then_some(seconds)
}

fn time_units_scale(units: Option<&str>) -> f64 {
    let units = units.unwrap_or("seconds").trim().to_ascii_lowercase();
    if units.contains("microsecond") || matches!(units.as_str(), "us" | "µs") {
        1.0e-6
    } else if units.contains("millisecond") || units == "ms" {
        1.0e-3
    } else {
        1.0
    }
}

fn range_units_to_km_scale(units: Option<&str>) -> f64 {
    let units = units.unwrap_or("meters").trim().to_ascii_lowercase();
    if units.contains("kilometer") || units.contains("kilometre") || units == "km" {
        1.0
    } else {
        1.0e-3
    }
}

fn range_units_to_m_scale(units: Option<&str>) -> f64 {
    range_units_to_km_scale(units) * 1000.0
}

fn positive_f32(value: f64) -> Option<f32> {
    (value.is_finite() && value > 0.0 && value <= f32::MAX as f64).then_some(value as f32)
}

fn metadata_text(file: &Nc3File<'_>, name: &str) -> Option<String> {
    file.gattr_str(name)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn normalize_frequency_mhz(value: f64) -> Option<u32> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let mhz = if value > 1.0e6 {
        value / 1.0e6
    } else if value > 1000.0 {
        value
    } else {
        value * 1000.0
    };
    (1000.0..=12_000.0)
        .contains(&mhz)
        .then_some(mhz.round() as u32)
}

fn frequency_mhz_from_wavelength(value: f64) -> Option<u32> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let meters = if value > 1.0 { value / 100.0 } else { value };
    let mhz = 299.792_458 / meters;
    (1000.0..=12_000.0)
        .contains(&mhz)
        .then_some(mhz.round() as u32)
}

pub(crate) fn invalid(reason: impl Into<String>) -> CfRadialError {
    CfRadialError::InvalidMessage {
        offset: 0,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_volume_exceeding_the_output_budget_is_rejected() {
        let path = recast_radar_testdata::path("cfrad1-irene-sr2-20110827-120420-sur-sweeps01")
            .unwrap_or_else(|e| panic!("{e}"));
        let bytes = std::fs::read(path).expect("read committed CfRadial file");
        read_cfradial1_volume_within(&bytes, DecodeBudget::volume())
            .expect("real file fits the default budget");
        // The int8 fields alone (719 rays x 1,107 gates x two fields) take
        // 1.5 MiB on top of the widened coordinates; a 1 MiB budget must fail
        // cleanly.
        let Err(error) = read_cfradial1_volume_within(&bytes, DecodeBudget::new(1 << 20)) else {
            panic!("1 MiB budget must fail");
        };
        assert!(
            matches!(&error, CfRadialError::LimitExceeded(reason) if reason.contains("limit")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rhi_fixed_angle_fallback_uses_wrap_aware_azimuth_mean() {
        let fixed = fallback_fixed_angle(true, &[359.0, 0.0, 1.0], &[10.0, 20.0, 30.0]);
        assert!(!(0.01..=359.99).contains(&fixed), "fixed angle was {fixed}");
    }

    #[test]
    fn ppi_fixed_angle_fallback_still_uses_mean_elevation() {
        let fixed = fallback_fixed_angle(false, &[80.0, 90.0, 100.0], &[0.4, 0.5, 0.6]);
        assert!((fixed - 0.5).abs() < 1.0e-9);
    }

    #[test]
    fn range_coordinate_detects_uniform_spacing() {
        assert_eq!(
            range_coordinate(&[0.0, 75.0, 150.0, 225.0], 4),
            RangeCoord::Uniform {
                first_center_m: 0.0,
                spacing_m: 75.0,
                ngates: 4
            }
        );
        // float32 centres of a 124.913025 m spacing, as DOW8 stores them.
        let dow8: Vec<f64> = (0..950)
            .map(|gate| f64::from((62.456512_f64 + f64::from(gate) * 124.913025) as f32))
            .collect();
        let RangeCoord::Uniform { spacing_m, .. } = range_coordinate(&dow8, 950) else {
            panic!("float32 rounding must still read as uniform");
        };
        assert!((spacing_m - 124.913025).abs() < 1e-4);
        assert!(matches!(
            range_coordinate(&[0.0, 75.0, 200.0], 3),
            RangeCoord::Explicit { .. }
        ));
    }

    #[test]
    fn time_units_reference_parses_cf_epoch() {
        assert_eq!(
            parse_iso_instant(" 2011-08-27T12:04:20Z"),
            Some(Utc.with_ymd_and_hms(2011, 8, 27, 12, 4, 20).unwrap())
        );
    }
}
