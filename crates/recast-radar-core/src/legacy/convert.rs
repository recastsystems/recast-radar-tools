//! Conversions between the legacy model and the FM301 model
//! (`docs/design/fm301-model.md` sections 5 and 13.2).
//!
//! Gate buffers move in both directions; nothing is copied except where a
//! value has no representation on the other side (non-identity
//! `radial_indices`, and `i8` / `i16` / `f64` / sentinel-bearing float fields,
//! which the legacy model only holds as physical `f32`).
//!
//! Exactness: [`volume_from_legacy`] returns every value the FM301 model cannot
//! hold exactly in a [`LegacyResidue`], and [`legacy_from_volume`] uses a
//! residue value only while the model still holds that value's forward image,
//! so `legacy_from_volume(volume_from_legacy(v))` is bit-identical to `v`
//! (checked with [`super::bit_identical`]) while a migrated algorithm's changes
//! to the model win over a stale residue.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, NaiveTime, Utc};
use thiserror::Error;

use super::{
    ElevationCut, GateRange, MomentGrid, MomentStorage, MomentType, RadarSite, RadarVolume, Radial,
    RadialStatus, RayInstrumentMetadata, ScanLegMetadata, ScanMode, VcpInfo, VolumeMetadata,
};
use crate::model::{
    DecodeStats, Field, FieldAttrs, FieldData, FieldName, FloatCoding, GlobalAttrs, IntCoding,
    LinearTransform, Location, Polarization, Provenance, Quantity, RadarParameters, RangeCoord,
    RayVariables, Rays, ScanDefinition, ScanLeg, ScanStrategy, SimulationProvenance, SourceFormat,
    Sweep, SweepMode, Volume, floor_to_second,
};

/// Everything [`volume_from_legacy`] could not represent exactly (design note
/// 5.5).
#[derive(Clone, Debug, PartialEq)]
pub struct LegacyResidue {
    pub volume_time: DateTime<Utc>,
    pub scan_mode: Option<ScanMode>,
    pub pulse_width_us: Option<f32>,
    pub unambiguous_range_km: Option<f32>,
    /// One per sweep, in sweep order.
    pub sweeps: Vec<SweepResidue>,
}

/// Per-sweep part of the residue.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SweepResidue {
    /// `Radial::time_offset_ms` as written: ms of day (NEXRAD), ms since volume
    /// start (CfRadial), ms since sweep start (DORADE), 0 (ODIM, JMA).
    pub time_offset_ms: Vec<i32>,
    pub radial_gate_ranges: Vec<GateRange>,
    pub radial_status: Vec<Option<RadialStatus>>,
    pub nyquist_velocity_mps: Vec<Option<f32>>,
    /// Verbatim (covers km values, `Some(NaN)`, pulse counts above `i32::MAX`,
    /// all-`None` and misaligned vectors).
    pub ray_instrument_metadata: Vec<RayInstrumentMetadata>,
    /// One per field, in `Sweep::fields` order.
    pub fields: Vec<FieldResidue>,
}

/// Per-field part of the residue.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldResidue {
    /// The legacy key, so the reverse is exact even after a naming collision
    /// (5.4).
    pub moment: MomentType,
    /// The name the conversion gave the field. The residue applies to the field
    /// of this name.
    pub name: FieldName,
    /// Preserves each decoder's start-vs-centre convention and metre rounding
    /// (6.6).
    pub gate_range: GateRange,
    pub nodata: Option<u16>,
    pub range_folded: Option<u16>,
    /// `radial_indices` when they were not exactly `0..nrays` (the rows were
    /// scattered or padded, 5.3).
    pub radial_indices: Option<Vec<usize>>,
    /// `scale`, `offset` of an `F32` grid when not exactly `1.0`, `0.0` (the
    /// float coding has no slot for values the legacy model ignores).
    pub float_scale_offset: Option<(f32, f32)>,
}

/// Legacy conventions of the decoder that produced a volume: gate-range meaning
/// (6.6), `time_offset_ms` meaning (5.2), naming (5.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegacyConvention {
    Nexrad,
    Odim,
    CfRadial,
    Dorade,
    Jma,
    Generic,
}

impl From<SourceFormat> for LegacyConvention {
    fn from(format: SourceFormat) -> Self {
        match format {
            SourceFormat::NexradLevel2 | SourceFormat::NexradLevel3 => Self::Nexrad,
            SourceFormat::OdimH5 => Self::Odim,
            SourceFormat::CfRadial1 | SourceFormat::CfRadial2 => Self::CfRadial,
            SourceFormat::Dorade => Self::Dorade,
            SourceFormat::JmaGrib2 => Self::Jma,
            _ => Self::Generic,
        }
    }
}

impl LegacyConvention {
    /// The convention of a legacy volume, from its provenance markers.
    pub fn of_metadata(metadata: &VolumeMetadata) -> Self {
        SourceFormat::infer_from_markers(
            metadata.archive_version.as_deref(),
            metadata.compression.as_deref(),
        )
        .into()
    }

    /// `true` when legacy `GateRange::first_gate_m` is the start of the first
    /// bin (ODIM `rstart`, CfRadial `range[0] - spacing/2`), not its centre.
    fn first_gate_is_start(self) -> bool {
        matches!(self, Self::Odim | Self::CfRadial)
    }
}

#[derive(Debug, Error)]
pub enum LegacyConversionError {
    #[error("sweep {sweep}: field {field} gates do not align with the sweep range")]
    UnalignedGates { sweep: usize, field: String },
    #[error("sweep {sweep}: explicit (non-uniform) range has no legacy form")]
    ExplicitRange { sweep: usize },
    #[error("sweep {sweep}: {detail}")]
    Shape { sweep: usize, detail: String },
}

/// How `time_offset_ms` relates to `Rays::time_s`.
#[derive(Clone, Copy, Debug)]
enum TimeBase {
    /// No volume time known (borrowed cut conversions): `time_s = ms / 1000`.
    Relative,
    /// `time_s` is seconds since `reference`; `volume_time` anchors the
    /// decoder's `time_offset_ms` (5.2).
    Volume {
        volume_time: DateTime<Utc>,
        reference: DateTime<Utc>,
    },
}

const DAY_MS: i64 = 86_400_000;

impl TimeBase {
    fn forward(self, ms: i32, convention: LegacyConvention) -> f64 {
        match self {
            Self::Relative => f64::from(ms) / 1000.0,
            Self::Volume {
                volume_time,
                reference,
            } => {
                if convention == LegacyConvention::Nexrad {
                    let midnight = volume_time.date_naive().and_time(NaiveTime::MIN).and_utc();
                    let mut diff =
                        midnight.timestamp_millis() + i64::from(ms) - reference.timestamp_millis();
                    if diff < -DAY_MS / 2 {
                        diff += DAY_MS;
                    }
                    diff as f64 / 1000.0
                } else {
                    f64::from(ms) / 1000.0 + seconds_between(reference, volume_time)
                }
            }
        }
    }

    fn reverse(self, time_s: f64, convention: LegacyConvention) -> i32 {
        let ms = match self {
            Self::Relative => (time_s * 1000.0).round(),
            Self::Volume {
                volume_time,
                reference,
            } => {
                if convention == LegacyConvention::Nexrad {
                    let total = reference.timestamp_millis() as f64 + (time_s * 1000.0).round();
                    return (total as i64).rem_euclid(DAY_MS) as i32;
                }
                ((time_s - seconds_between(reference, volume_time)) * 1000.0).round()
            }
        };
        clamp_i32(ms)
    }
}

/// `later - earlier` in seconds, with sub-second precision.
fn seconds_between(earlier: DateTime<Utc>, later: DateTime<Utc>) -> f64 {
    let delta = later - earlier;
    match delta.num_nanoseconds() {
        Some(nanos) => nanos as f64 / 1e9,
        None => delta.num_milliseconds() as f64 / 1000.0,
    }
}

fn clamp_i32(value: f64) -> i32 {
    if value.is_nan() {
        0
    } else {
        value.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    }
}

fn round_i32(value: f64) -> i32 {
    clamp_i32(value.round())
}

fn bits_eq_f32(a: f32, b: f32) -> bool {
    a.to_bits() == b.to_bits()
}

fn opt_bits_eq_f32(a: Option<f32>, b: Option<f32>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => bits_eq_f32(a, b),
        (None, None) => true,
        _ => false,
    }
}

fn vec_bits_eq_f32(a: &Option<Vec<f32>>, b: &Option<Vec<f32>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| bits_eq_f32(*x, *y))
        }
        (None, None) => true,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Names (5.4)
// ---------------------------------------------------------------------------

impl MomentType {
    /// The FM301 name of a legacy moment (design note 5.4): the seven canonical
    /// variants to DBZH, VRADH, WRADH, ZDR, RHOHV, PHIDP, KDP; NEXRAD `CFP` to
    /// CCORH; any other `Unknown(s)` through [`FieldName::parse`].
    pub fn to_field_name(&self, convention: LegacyConvention) -> FieldName {
        match self {
            Self::Reflectivity => FieldName::Dbzh,
            Self::Velocity => FieldName::Vradh,
            Self::SpectrumWidth => FieldName::Wradh,
            Self::DifferentialReflectivity => FieldName::Zdr,
            Self::CorrelationCoefficient => FieldName::Rhohv,
            Self::DifferentialPhase => FieldName::Phidp,
            Self::SpecificDifferentialPhase => FieldName::Kdp,
            Self::Unknown(name) if convention == LegacyConvention::Nexrad && name == "CFP" => {
                FieldName::Ccorh
            }
            Self::Unknown(name) => FieldName::parse(name),
        }
    }

    /// Quantity and polarization the conversion records for this moment.
    fn quantity(&self, convention: LegacyConvention) -> (Quantity, Polarization) {
        match self {
            Self::Unknown(name) => {
                let field_name = self.to_field_name(convention);
                match field_name.info() {
                    Some(info) => (info.quantity, info.polarization),
                    None => Quantity::classify(name, None),
                }
            }
            canonical => {
                let info = canonical.to_field_name(convention).info();
                info.map_or((Quantity::Other, Polarization::Unspecified), |info| {
                    (info.quantity, info.polarization)
                })
            }
        }
    }

    /// Base text for collision renaming: the variant name for canonical
    /// moments, the source spelling for `Unknown`.
    fn collision_base(&self) -> String {
        match self {
            Self::Unknown(name) => name.clone(),
            other => format!("{other:?}"),
        }
    }
}

impl FieldName {
    /// The legacy moment of an FM301 name: the exact inverse of
    /// [`MomentType::to_field_name`] for the seven canonical names, CCORH to
    /// `Unknown("CFP")` for NEXRAD, and `Unknown(name)` otherwise.
    pub fn to_legacy_moment(&self, convention: LegacyConvention) -> MomentType {
        match self {
            Self::Dbzh => MomentType::Reflectivity,
            Self::Vradh => MomentType::Velocity,
            Self::Wradh => MomentType::SpectrumWidth,
            Self::Zdr => MomentType::DifferentialReflectivity,
            Self::Rhohv => MomentType::CorrelationCoefficient,
            Self::Phidp => MomentType::DifferentialPhase,
            Self::Kdp => MomentType::SpecificDifferentialPhase,
            Self::Ccorh if convention == LegacyConvention::Nexrad => {
                MomentType::Unknown("CFP".to_owned())
            }
            other => MomentType::Unknown(other.as_str().to_owned()),
        }
    }
}

/// `preferred` unless its spelling is taken; otherwise `Other(base)`,
/// `Other(base_2)`, ... (never a known spelling, never taken).
fn unique_name(preferred: FieldName, base: &str, taken: &HashSet<String>) -> FieldName {
    if !taken.contains(preferred.as_str()) {
        return preferred;
    }
    let mut suffix = 1usize;
    loop {
        let text = if suffix == 1 {
            base.to_owned()
        } else {
            format!("{base}_{suffix}")
        };
        let candidate = FieldName::parse(&text);
        if matches!(candidate, FieldName::Other(_)) && !taken.contains(&text) {
            return candidate;
        }
        suffix += 1;
    }
}

// ---------------------------------------------------------------------------
// Scan mode (10)
// ---------------------------------------------------------------------------

fn sweep_mode_of(scan_mode: Option<ScanMode>) -> SweepMode {
    match scan_mode {
        None | Some(ScanMode::Ppi) => SweepMode::AzimuthSurveillance,
        Some(ScanMode::Rhi) => SweepMode::Rhi,
        Some(ScanMode::VerticalPointing) => SweepMode::VerticalPointing,
        Some(ScanMode::Other) => SweepMode::Other("other".into()),
    }
}

fn scan_mode_of(sweep_mode: &SweepMode) -> ScanMode {
    match sweep_mode {
        SweepMode::AzimuthSurveillance | SweepMode::Sector | SweepMode::ManualPpi => ScanMode::Ppi,
        SweepMode::Rhi | SweepMode::ManualRhi => ScanMode::Rhi,
        SweepMode::VerticalPointing => ScanMode::VerticalPointing,
        _ => ScanMode::Other,
    }
}

/// The legacy volume scan mode: the mode every sweep shares, else `Other`;
/// `None` without sweeps.
fn combined_scan_mode(sweeps: &[Sweep]) -> Option<ScanMode> {
    let first = scan_mode_of(&sweeps.first()?.sweep_mode);
    Some(
        if sweeps
            .iter()
            .all(|sweep| scan_mode_of(&sweep.sweep_mode) == first)
        {
            first
        } else {
            ScanMode::Other
        },
    )
}

// ---------------------------------------------------------------------------
// Gate geometry (6.6)
// ---------------------------------------------------------------------------

/// Legacy gate range to (centre of first gate, spacing) in metres.
fn legacy_geometry(convention: LegacyConvention, gate_range: &GateRange) -> (f64, f64) {
    let first = f64::from(gate_range.first_gate_m);
    let spacing = f64::from(gate_range.gate_spacing_m);
    if convention.first_gate_is_start() {
        (first + spacing / 2.0, spacing)
    } else {
        (first, spacing)
    }
}

/// (centre, spacing) back to the legacy gate range of `convention`.
fn legacy_gate_range(
    convention: LegacyConvention,
    (center, spacing): (f64, f64),
    gate_count: usize,
) -> GateRange {
    let first = if convention.first_gate_is_start() {
        center - spacing / 2.0
    } else {
        center
    };
    GateRange {
        first_gate_m: round_i32(first),
        gate_spacing_m: round_i32(spacing),
        gate_count,
    }
}

// ---------------------------------------------------------------------------
// Per-ray instrument variables (9)
// ---------------------------------------------------------------------------

fn forward_nyquist(values: &[Option<f32>]) -> Option<Vec<f32>> {
    values
        .iter()
        .any(Option::is_some)
        .then(|| values.iter().map(|v| v.unwrap_or(f32::NAN)).collect())
}

fn reverse_nyquist(values: &Option<Vec<f32>>, ray: usize) -> Option<f32> {
    values
        .as_ref()
        .and_then(|values| values.get(ray).copied())
        .filter(|value| !value.is_nan())
}

/// The four `RayVariables` vectors legacy `RayInstrumentMetadata` maps to.
#[derive(Clone, Debug, Default, PartialEq)]
struct InstrumentVectors {
    prt_s: Option<Vec<f32>>,
    unambiguous_range_m: Option<Vec<f32>>,
    n_samples: Option<Vec<i32>>,
    independent_samples: Option<Vec<f32>>,
}

impl InstrumentVectors {
    fn forward(metadata: &[RayInstrumentMetadata], nrays: usize) -> Self {
        if metadata.is_empty() || metadata.len() != nrays {
            return Self::default();
        }
        let floats = |get: &dyn Fn(&RayInstrumentMetadata) -> Option<f32>| {
            metadata.iter().any(|m| get(m).is_some()).then(|| {
                metadata
                    .iter()
                    .map(|m| get(m).unwrap_or(f32::NAN))
                    .collect()
            })
        };
        Self {
            prt_s: floats(&|m| m.prt_s),
            unambiguous_range_m: floats(&|m| m.unambiguous_range_km.map(|km| km * 1000.0)),
            n_samples: metadata.iter().any(|m| m.pulse_count.is_some()).then(|| {
                metadata
                    .iter()
                    .map(|m| {
                        m.pulse_count
                            .and_then(|count| i32::try_from(count).ok())
                            .unwrap_or(-9999)
                    })
                    .collect()
            }),
            independent_samples: floats(&|m| m.independent_samples),
        }
    }

    fn of(vars: &RayVariables) -> Self {
        Self {
            prt_s: vars.prt_s.clone(),
            unambiguous_range_m: vars.unambiguous_range_m.clone(),
            n_samples: vars.n_samples.clone(),
            independent_samples: vars.independent_samples.clone(),
        }
    }

    fn bits_eq(&self, other: &Self) -> bool {
        vec_bits_eq_f32(&self.prt_s, &other.prt_s)
            && vec_bits_eq_f32(&self.unambiguous_range_m, &other.unambiguous_range_m)
            && self.n_samples == other.n_samples
            && vec_bits_eq_f32(&self.independent_samples, &other.independent_samples)
    }

    fn is_empty(&self) -> bool {
        self.prt_s.is_none()
            && self.unambiguous_range_m.is_none()
            && self.n_samples.is_none()
            && self.independent_samples.is_none()
    }

    fn reverse(&self, nrays: usize) -> Vec<RayInstrumentMetadata> {
        if self.is_empty() {
            return Vec::new();
        }
        let float = |values: &Option<Vec<f32>>, ray: usize| {
            values
                .as_ref()
                .and_then(|values| values.get(ray).copied())
                .filter(|value| !value.is_nan())
        };
        (0..nrays)
            .map(|ray| RayInstrumentMetadata {
                prt_s: float(&self.prt_s, ray),
                unambiguous_range_km: float(&self.unambiguous_range_m, ray).map(|m| m / 1000.0),
                pulse_count: self
                    .n_samples
                    .as_ref()
                    .and_then(|values| values.get(ray).copied())
                    .filter(|count| *count != -9999)
                    .and_then(|count| u32::try_from(count).ok()),
                independent_samples: float(&self.independent_samples, ray),
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Legacy -> FM301
// ---------------------------------------------------------------------------

/// Convert a legacy volume. Every `U8` / `U16` / `F32` buffer moves; no gate
/// data is copied unless a grid's `radial_indices` are not `0..nrays`.
pub fn volume_from_legacy(
    v: RadarVolume,
) -> Result<(Volume, LegacyResidue), LegacyConversionError> {
    let RadarVolume {
        site,
        volume_time,
        vcp,
        cuts,
        metadata,
    } = v;
    let source_format = SourceFormat::infer_from_markers(
        metadata.archive_version.as_deref(),
        metadata.compression.as_deref(),
    );
    let convention = LegacyConvention::from(source_format);
    let reference = floor_to_second(volume_time);
    let base = TimeBase::Volume {
        volume_time,
        reference,
    };
    let sweep_mode = sweep_mode_of(metadata.scan_mode);

    let mut sweeps = Vec::with_capacity(cuts.len());
    let mut sweep_residues = Vec::with_capacity(cuts.len());
    for (index, cut) in cuts.into_iter().enumerate() {
        let (sweep, residue) =
            sweep_from_cut_owned(cut, index, sweep_mode.clone(), convention, base)?;
        sweeps.push(sweep);
        sweep_residues.push(residue);
    }

    let VolumeMetadata {
        source_path,
        archive_version,
        compression,
        message_count,
        decoded_radial_count,
        skipped_message_count,
        scan_mode,
        radar_frequency_mhz,
        beam_width_h_deg,
        beam_width_v_deg,
        pulse_width_us,
        prt_s,
        unambiguous_range_km,
        scan_name,
        scan_id,
        vcp_source_document,
        vcp_source_revision,
        vcp_source_rda_build,
        vcp_source_figure,
        vcp_pulse_length,
        vcp_adaptations,
        scan_legs,
        polarization,
        calibration,
        forward_operator,
        forward_operator_config,
        source_model,
        microphysics_scheme,
        scattering_model,
    } = metadata;

    let id = scan_id.as_deref().and_then(|text| text.parse::<i64>().ok());
    let scan_id_text = scan_id.filter(|text| id.map(|id| id.to_string()).as_deref() != Some(text));
    let definition = ScanDefinition {
        source_document: vcp_source_document,
        source_revision: vcp_source_revision,
        source_rda_build: vcp_source_rda_build,
        source_figure: vcp_source_figure,
        pulse_length: vcp_pulse_length,
        adaptations: vcp_adaptations,
        scan_id_text,
        legs: scan_legs.into_iter().map(scan_leg_from_legacy).collect(),
    };
    let simulation = SimulationProvenance {
        forward_operator,
        forward_operator_config,
        source_model,
        microphysics_scheme,
        scattering_model,
    };
    let simulated = simulation != SimulationProvenance::default();

    let mut volume = Volume {
        attrs: GlobalAttrs {
            instrument_name: site.id,
            site_name: site.name,
            simulated,
            ..GlobalAttrs::default()
        },
        location: Location {
            latitude_deg: site.latitude_deg.map(f64::from),
            longitude_deg: site.longitude_deg.map(f64::from),
            altitude_m: site.elevation_m.map(f64::from),
            altitude_agl_m: None,
        },
        scan: ScanStrategy {
            name: scan_name,
            id,
            vcp_pattern: vcp.map(|vcp| vcp.pattern),
            definition: (definition != ScanDefinition::default()).then(|| Box::new(definition)),
        },
        radar_parameters: RadarParameters {
            frequency_hz: radar_frequency_mhz
                .map(|mhz| vec![f64::from(mhz) * 1e6])
                .unwrap_or_default(),
            beam_width_h_deg,
            beam_width_v_deg,
            pulse_width_s: pulse_width_us.map(|us| us * 1e-6),
            prt_s,
            unambiguous_range_m: unambiguous_range_km.map(|km| km * 1000.0),
            ..RadarParameters::default()
        },
        provenance: Provenance {
            source_format,
            source_path,
            source_version: archive_version,
            source_conventions: None,
            compression,
            decode: DecodeStats {
                message_count,
                decoded_ray_count: decoded_radial_count,
                skipped_message_count,
            },
            polarization_note: polarization,
            calibration_note: calibration,
        },
        simulation: simulated.then(|| Box::new(simulation)),
        sweeps,
        ..Volume::new(String::new(), reference)
    };
    volume.time_coverage = volume.ray_time_extent();
    let residue = LegacyResidue {
        volume_time,
        scan_mode,
        pulse_width_us,
        unambiguous_range_km,
        sweeps: sweep_residues,
    };
    Ok((volume, residue))
}

fn scan_leg_from_legacy(leg: ScanLegMetadata) -> ScanLeg {
    let ScanLegMetadata {
        source_row_index,
        elevation_deg,
        azimuth_rate_deg_per_second,
        source_period_seconds,
        waveform,
        moment_coverage,
        surveillance_prf_code,
        surveillance_pulse_count,
        doppler_prf_code,
        doppler_pulse_count,
    } = leg;
    ScanLeg {
        source_row_index,
        elevation_deg,
        azimuth_rate_deg_per_second,
        source_period_seconds,
        waveform,
        moment_coverage,
        surveillance_prf_code,
        surveillance_pulse_count,
        doppler_prf_code,
        doppler_pulse_count,
    }
}

fn scan_leg_to_legacy(leg: ScanLeg) -> ScanLegMetadata {
    let ScanLeg {
        source_row_index,
        elevation_deg,
        azimuth_rate_deg_per_second,
        source_period_seconds,
        waveform,
        moment_coverage,
        surveillance_prf_code,
        surveillance_pulse_count,
        doppler_prf_code,
        doppler_pulse_count,
    } = leg;
    ScanLegMetadata {
        source_row_index,
        elevation_deg,
        azimuth_rate_deg_per_second,
        source_period_seconds,
        waveform,
        moment_coverage,
        surveillance_prf_code,
        surveillance_pulse_count,
        doppler_prf_code,
        doppler_pulse_count,
    }
}

/// Convert one cut (borrowed; this clones the cut). Ray times are relative:
/// `time_s = time_offset_ms / 1000`, since a cut carries no volume time. The
/// convention comes from `meta`'s provenance markers, the sweep mode from
/// `meta.scan_mode`.
pub fn sweep_from_cut(
    cut: &ElevationCut,
    meta: &VolumeMetadata,
    number: u32,
) -> Result<(Sweep, SweepResidue), LegacyConversionError> {
    sweep_from_cut_owned(
        cut.clone(),
        number as usize,
        sweep_mode_of(meta.scan_mode),
        LegacyConvention::of_metadata(meta),
        TimeBase::Relative,
    )
}

fn sweep_from_cut_owned(
    cut: ElevationCut,
    index: usize,
    sweep_mode: SweepMode,
    convention: LegacyConvention,
    base: TimeBase,
) -> Result<(Sweep, SweepResidue), LegacyConversionError> {
    let ElevationCut {
        elevation_deg,
        elevation_number,
        radials,
        ray_instrument_metadata,
        moments,
    } = cut;
    let number = u32::try_from(index).map_err(|_| LegacyConversionError::Shape {
        sweep: index,
        detail: "sweep index exceeds u32".to_owned(),
    })?;
    let mut sweep = Sweep::new(number, sweep_mode, elevation_deg);
    sweep.elevation_number = elevation_number.map(u16::from);
    let nrays = radials.len();

    let mut residue = SweepResidue {
        time_offset_ms: Vec::with_capacity(nrays),
        radial_gate_ranges: Vec::with_capacity(nrays),
        radial_status: Vec::with_capacity(nrays),
        nyquist_velocity_mps: Vec::with_capacity(nrays),
        ray_instrument_metadata: Vec::new(),
        fields: Vec::with_capacity(moments.len()),
    };
    let mut rays = Rays {
        time_s: Vec::with_capacity(nrays),
        azimuth_deg: Vec::with_capacity(nrays),
        elevation_deg: Vec::with_capacity(nrays),
    };
    for radial in radials {
        let Radial {
            azimuth_deg,
            elevation_deg,
            time_offset_ms,
            gate_range,
            nyquist_velocity_mps,
            radial_status,
        } = radial;
        rays.time_s.push(base.forward(time_offset_ms, convention));
        rays.azimuth_deg.push(azimuth_deg);
        rays.elevation_deg.push(elevation_deg);
        residue.time_offset_ms.push(time_offset_ms);
        residue.radial_gate_ranges.push(gate_range);
        residue.radial_status.push(radial_status);
        residue.nyquist_velocity_mps.push(nyquist_velocity_mps);
    }
    sweep.rays = rays;
    sweep.ray_vars.nyquist_velocity_mps = forward_nyquist(&residue.nyquist_velocity_mps);
    let instruments = InstrumentVectors::forward(&ray_instrument_metadata, nrays);
    sweep.ray_vars.prt_s = instruments.prt_s;
    sweep.ray_vars.unambiguous_range_m = instruments.unambiguous_range_m;
    sweep.ray_vars.n_samples = instruments.n_samples;
    sweep.ray_vars.independent_samples = instruments.independent_samples;
    residue.ray_instrument_metadata = ray_instrument_metadata;

    // Names in two passes (5.4): source spellings first, canonical defaults
    // second, so a canonical variant yields to a verbatim source name.
    let entries: Vec<(MomentType, MomentGrid)> = moments.into_iter().collect();
    let mut names: Vec<Option<FieldName>> = vec![None; entries.len()];
    let mut taken = HashSet::with_capacity(entries.len());
    for pass_unknown in [true, false] {
        for (slot, (moment, _)) in names.iter_mut().zip(&entries) {
            if matches!(moment, MomentType::Unknown(_)) != pass_unknown {
                continue;
            }
            let name = unique_name(
                moment.to_field_name(convention),
                &moment.collision_base(),
                &taken,
            );
            taken.insert(name.as_str().to_owned());
            *slot = Some(name);
        }
    }
    for ((moment, grid), name) in entries.into_iter().zip(names) {
        let name = name.unwrap_or_else(|| moment.to_field_name(convention));
        let (field, field_residue) =
            field_from_grid_owned(grid, &mut sweep, index, convention, name, &moment)?;
        sweep.fields.push(field);
        residue.fields.push(field_residue);
    }
    Ok((sweep, residue))
}

/// Add a grid (borrowed; this clones the grid) as a field of `sweep`, attaching
/// its geometry under `convention`, and return the field's index. The name is
/// the moment's FM301 name, or `Other(..)` if the sweep already has it.
pub fn field_from_grid(
    grid: &MomentGrid,
    sweep: &mut Sweep,
    convention: LegacyConvention,
) -> Result<(usize, FieldResidue), LegacyConversionError> {
    let taken: HashSet<String> = sweep
        .fields
        .iter()
        .map(|field| field.name.as_str().to_owned())
        .collect();
    let name = unique_name(
        grid.moment.to_field_name(convention),
        &grid.moment.collision_base(),
        &taken,
    );
    let moment = grid.moment.clone();
    let index = sweep.sweep_number as usize;
    let (field, residue) =
        field_from_grid_owned(grid.clone(), sweep, index, convention, name, &moment)?;
    sweep.fields.push(field);
    Ok((sweep.fields.len() - 1, residue))
}

fn field_from_grid_owned(
    grid: MomentGrid,
    sweep: &mut Sweep,
    sweep_index: usize,
    convention: LegacyConvention,
    name: FieldName,
    moment: &MomentType,
) -> Result<(Field, FieldResidue), LegacyConversionError> {
    let MomentGrid {
        moment: _,
        gate_range,
        scale,
        offset,
        nodata,
        range_folded,
        radial_indices,
        storage,
    } = grid;
    let shape = |detail: String| LegacyConversionError::Shape {
        sweep: sweep_index,
        detail: format!("{}: {detail}", name.as_str()),
    };
    let nrays = sweep.nrays();
    let gate_count = gate_range.gate_count;
    let ngates = u32::try_from(gate_count).map_err(|_| shape("gate count exceeds u32".into()))?;
    let rows = radial_indices.len();
    if rows.checked_mul(gate_count) != Some(storage.len()) {
        return Err(shape(format!(
            "{} values for {rows} rows of {gate_count} gates",
            storage.len()
        )));
    }
    let (center, spacing) = legacy_geometry(convention, &gate_range);
    let gates = sweep
        .attach_geometry(center, spacing, ngates)
        .map_err(|_| LegacyConversionError::UnalignedGates {
            sweep: sweep_index,
            field: name.as_str().to_owned(),
        })?;

    let identity_prefix = radial_indices.iter().enumerate().all(|(i, r)| i == *r);
    if identity_prefix && rows > nrays {
        return Err(shape(format!("{rows} rows for {nrays} radials")));
    }
    let scatter = if identity_prefix {
        None
    } else {
        let mut seen = vec![false; nrays];
        for &ray in &radial_indices {
            match seen.get_mut(ray) {
                Some(slot) if !*slot => *slot = true,
                _ => {
                    return Err(shape(format!(
                        "radial index {ray} out of range or repeated"
                    )));
                }
            }
        }
        Some(seen)
    };

    let (quantity, polarization) = moment.quantity(convention);
    let nexrad_undetect = convention == LegacyConvention::Nexrad && nodata == Some(0);
    let is_f32 = matches!(storage, MomentStorage::F32(_));
    let data = match storage {
        MomentStorage::U8(values) => FieldData::U8 {
            values: arrange_rows(values, &radial_indices, scatter.is_some(), gate_count, 0),
            coding: IntCoding {
                transform: LinearTransform::IcdScaleOffset { scale, offset },
                fill_value: nodata.and_then(|code| u8::try_from(code).ok()),
                undetect: nexrad_undetect.then_some(0),
                range_folded: range_folded.and_then(|code| u8::try_from(code).ok()),
                valid_range: None,
            },
        },
        MomentStorage::U16(values) => FieldData::U16 {
            values: arrange_rows(values, &radial_indices, scatter.is_some(), gate_count, 0),
            coding: IntCoding {
                transform: LinearTransform::IcdScaleOffset { scale, offset },
                fill_value: nodata,
                undetect: nexrad_undetect.then_some(0),
                range_folded,
                valid_range: None,
            },
        },
        MomentStorage::F32(values) => FieldData::F32 {
            values: arrange_rows(
                values,
                &radial_indices,
                scatter.is_some(),
                gate_count,
                f32::NAN,
            ),
            coding: FloatCoding::default(),
        },
    };
    // `arrange_rows` scattered with the placeholder fill; re-fill with the
    // coding's code below via the absent-row path.
    let mut field = Field {
        name: name.clone(),
        quantity,
        polarization,
        attrs: FieldAttrs::default(),
        nrays: 0,
        ngates,
        gates,
        data,
        absent_rows: Vec::new(),
    };
    match scatter {
        None => {
            field.nrays = rows as u32;
            field
                .push_absent_rows_to(nrays)
                .map_err(|err| shape(err.to_string()))?;
        }
        Some(seen) => {
            field.nrays = nrays as u32;
            field.absent_rows = seen
                .iter()
                .enumerate()
                .filter(|(_, present)| !**present)
                .map(|(ray, _)| ray as u32)
                .collect();
            refill_absent_rows(&mut field);
        }
    }

    let float_scale_offset = (is_f32 && !(bits_eq_f32(scale, 1.0) && bits_eq_f32(offset, 0.0)))
        .then_some((scale, offset));
    let residue = FieldResidue {
        moment: moment.clone(),
        name,
        gate_range,
        nodata,
        range_folded,
        radial_indices: (!(identity_prefix && rows == nrays)).then_some(radial_indices),
        float_scale_offset,
    };
    Ok((field, residue))
}

/// Rows in ray order. An identity prefix moves the buffer; otherwise rows are
/// scattered to `radial_indices` positions in a `nrays`-row buffer (absent
/// rows hold `fill` until [`refill_absent_rows`]).
fn arrange_rows<T: Copy>(
    values: Vec<T>,
    radial_indices: &[usize],
    scatter: bool,
    gate_count: usize,
    fill: T,
) -> Vec<T> {
    if !scatter || gate_count == 0 {
        return values;
    }
    let nrays = radial_indices
        .iter()
        .copied()
        .max()
        .map_or(0, |max| max + 1);
    let mut out = vec![fill; nrays * gate_count];
    for (row, &ray) in radial_indices.iter().enumerate() {
        out[ray * gate_count..(ray + 1) * gate_count]
            .copy_from_slice(&values[row * gate_count..(row + 1) * gate_count]);
    }
    out
}

/// Size a scattered buffer to `nrays` rows and write the coding's fill code into
/// every absent row.
fn refill_absent_rows(field: &mut Field) {
    let nrays = field.nrays as usize;
    let ngates = field.ngates as usize;
    let absent = field.absent_rows.clone();
    macro_rules! refill {
        ($values:expr, $fill:expr) => {{
            let values = $values;
            let fill = $fill;
            values.resize(nrays * ngates, fill);
            for ray in absent {
                let start = ray as usize * ngates;
                values[start..start + ngates].fill(fill);
            }
        }};
    }
    match &mut field.data {
        FieldData::U8 { values, coding } => refill!(values, coding.fill_code()),
        FieldData::U16 { values, coding } => refill!(values, coding.fill_code()),
        FieldData::I8 { values, coding } => refill!(values, coding.fill_code()),
        FieldData::I16 { values, coding } => refill!(values, coding.fill_code()),
        FieldData::F32 { values, coding } => refill!(values, coding.fill_code()),
        FieldData::F64 { values, coding } => refill!(values, coding.fill_code()),
    }
}

impl TryFrom<RadarVolume> for Volume {
    type Error = LegacyConversionError;

    /// [`volume_from_legacy`], residue dropped.
    fn try_from(volume: RadarVolume) -> Result<Self, Self::Error> {
        volume_from_legacy(volume).map(|(volume, _)| volume)
    }
}

// ---------------------------------------------------------------------------
// FM301 -> legacy
// ---------------------------------------------------------------------------

/// Convert back to the legacy model. With a residue: the exact inverse of
/// [`volume_from_legacy`] for every value whose forward image the model still
/// holds. Without one (natively decoded volumes): legacy values are derived
/// under `convention` (13.3). `I8` / `I16` / `F64` fields and float fields with
/// sentinels or a transform become physical legacy `F32` grids; that is the
/// only copy.
pub fn legacy_from_volume(
    v: Volume,
    residue: Option<&LegacyResidue>,
    convention: LegacyConvention,
) -> Result<RadarVolume, LegacyConversionError> {
    let Volume {
        attrs,
        time_reference,
        location,
        scan,
        radar_parameters,
        provenance,
        simulation,
        sweeps,
        ..
    } = v;
    let volume_time = residue
        .map(|residue| residue.volume_time)
        .filter(|time| floor_to_second(*time) == time_reference)
        .unwrap_or(time_reference);
    let base = TimeBase::Volume {
        volume_time,
        reference: time_reference,
    };

    let scan_mode = match residue {
        Some(residue)
            if sweeps
                .iter()
                .all(|sweep| sweep.sweep_mode == sweep_mode_of(residue.scan_mode)) =>
        {
            residue.scan_mode
        }
        _ => combined_scan_mode(&sweeps),
    };
    let pulse_width_us = match residue {
        Some(residue)
            if opt_bits_eq_f32(
                residue.pulse_width_us.map(|us| us * 1e-6),
                radar_parameters.pulse_width_s,
            ) =>
        {
            residue.pulse_width_us
        }
        _ => radar_parameters.pulse_width_s.map(|s| s * 1e6),
    };
    let unambiguous_range_km = match residue {
        Some(residue)
            if opt_bits_eq_f32(
                residue.unambiguous_range_km.map(|km| km * 1000.0),
                radar_parameters.unambiguous_range_m,
            ) =>
        {
            residue.unambiguous_range_km
        }
        _ => radar_parameters.unambiguous_range_m.map(|m| m / 1000.0),
    };

    let definition = scan
        .definition
        .map(|definition| *definition)
        .unwrap_or_default();
    let scan_id = definition
        .scan_id_text
        .or_else(|| scan.id.map(|id| id.to_string()));
    let simulation = simulation.map(|s| *s).unwrap_or_default();

    let mut cuts = Vec::with_capacity(sweeps.len());
    for (index, sweep) in sweeps.into_iter().enumerate() {
        let sweep_residue = residue.and_then(|residue| residue.sweeps.get(index));
        cuts.push(cut_from_sweep_owned(
            sweep,
            index,
            sweep_residue,
            convention,
            base,
        )?);
    }

    Ok(RadarVolume {
        site: RadarSite {
            id: attrs.instrument_name,
            name: attrs.site_name,
            latitude_deg: location.latitude_deg.map(|v| v as f32),
            longitude_deg: location.longitude_deg.map(|v| v as f32),
            elevation_m: location.altitude_m.map(|v| v as f32),
        },
        volume_time,
        vcp: scan.vcp_pattern.map(|pattern| VcpInfo { pattern }),
        cuts,
        metadata: VolumeMetadata {
            source_path: provenance.source_path,
            archive_version: provenance.source_version,
            compression: provenance.compression,
            message_count: provenance.decode.message_count,
            decoded_radial_count: provenance.decode.decoded_ray_count,
            skipped_message_count: provenance.decode.skipped_message_count,
            scan_mode,
            radar_frequency_mhz: radar_parameters
                .frequency_hz
                .first()
                .map(|hz| (hz / 1e6).round() as u32),
            beam_width_h_deg: radar_parameters.beam_width_h_deg,
            beam_width_v_deg: radar_parameters.beam_width_v_deg,
            pulse_width_us,
            prt_s: radar_parameters.prt_s,
            unambiguous_range_km,
            scan_name: scan.name,
            scan_id,
            vcp_source_document: definition.source_document,
            vcp_source_revision: definition.source_revision,
            vcp_source_rda_build: definition.source_rda_build,
            vcp_source_figure: definition.source_figure,
            vcp_pulse_length: definition.pulse_length,
            vcp_adaptations: definition.adaptations,
            scan_legs: definition
                .legs
                .into_iter()
                .map(scan_leg_to_legacy)
                .collect(),
            polarization: provenance.polarization_note,
            calibration: provenance.calibration_note,
            forward_operator: simulation.forward_operator,
            forward_operator_config: simulation.forward_operator_config,
            source_model: simulation.source_model,
            microphysics_scheme: simulation.microphysics_scheme,
            scattering_model: simulation.scattering_model,
        },
    })
}

/// Convert one sweep back to a legacy cut (borrowed; this clones the sweep).
/// Ray times are relative (`time_offset_ms = time_s × 1000`), the counterpart of
/// [`sweep_from_cut`].
pub fn cut_from_sweep(
    sweep: &Sweep,
    residue: Option<&SweepResidue>,
    convention: LegacyConvention,
) -> Result<ElevationCut, LegacyConversionError> {
    cut_from_sweep_owned(
        sweep.clone(),
        sweep.sweep_number as usize,
        residue,
        convention,
        TimeBase::Relative,
    )
}

fn cut_from_sweep_owned(
    sweep: Sweep,
    index: usize,
    residue: Option<&SweepResidue>,
    convention: LegacyConvention,
    base: TimeBase,
) -> Result<ElevationCut, LegacyConversionError> {
    if matches!(sweep.range, RangeCoord::Explicit { .. }) {
        return Err(LegacyConversionError::ExplicitRange { sweep: index });
    }
    let Sweep {
        fixed_angle_deg,
        elevation_number,
        rays,
        range,
        ray_vars,
        fields,
        ..
    } = sweep;
    let nrays = rays.len();
    if rays.time_s.len() != nrays || rays.elevation_deg.len() != nrays {
        return Err(LegacyConversionError::Shape {
            sweep: index,
            detail: "ray coordinate vectors differ in length".to_owned(),
        });
    }

    let times = residue.map(|r| r.time_offset_ms.as_slice()).unwrap_or(&[]);
    let gate_ranges = residue
        .map(|r| r.radial_gate_ranges.as_slice())
        .filter(|ranges| ranges.len() == nrays);
    let statuses = residue
        .map(|r| r.radial_status.as_slice())
        .filter(|statuses| statuses.len() == nrays);
    let nyquist_residue = residue
        .map(|r| r.nyquist_velocity_mps.as_slice())
        .filter(|values| {
            values.len() == nrays
                && vec_bits_eq_f32(&forward_nyquist(values), &ray_vars.nyquist_velocity_mps)
        });
    let derived_gate_range = fields
        .first()
        .and_then(|field| {
            field
                .native_geometry(&range)
                .map(|geometry| legacy_gate_range(convention, geometry, field.ngates as usize))
        })
        .unwrap_or_else(|| {
            let spacing = range.spacing_m().unwrap_or(0.0);
            legacy_gate_range(
                convention,
                (range.center_m(0).unwrap_or(0.0), spacing),
                range.ngates(),
            )
        });

    let mut radials = Vec::with_capacity(nrays);
    for ray in 0..nrays {
        let time_s = rays.time_s[ray];
        let time_offset_ms = match times.get(ray) {
            Some(ms) if base.forward(*ms, convention).to_bits() == time_s.to_bits() => *ms,
            _ => base.reverse(time_s, convention),
        };
        radials.push(Radial {
            azimuth_deg: rays.azimuth_deg[ray],
            elevation_deg: rays.elevation_deg[ray],
            time_offset_ms,
            gate_range: gate_ranges
                .map(|ranges| ranges[ray].clone())
                .unwrap_or_else(|| derived_gate_range.clone()),
            nyquist_velocity_mps: match nyquist_residue {
                Some(values) => values[ray],
                None => reverse_nyquist(&ray_vars.nyquist_velocity_mps, ray),
            },
            radial_status: statuses.and_then(|statuses| statuses[ray]),
        });
    }

    let model_instruments = InstrumentVectors::of(&ray_vars);
    let ray_instrument_metadata = match residue {
        Some(residue)
            if InstrumentVectors::forward(&residue.ray_instrument_metadata, nrays)
                .bits_eq(&model_instruments) =>
        {
            residue.ray_instrument_metadata.clone()
        }
        _ => model_instruments.reverse(nrays),
    };

    let mut moments = BTreeMap::new();
    for field in fields {
        let field_residue = residue.and_then(|residue| {
            residue
                .fields
                .iter()
                .find(|candidate| candidate.name == field.name)
        });
        let grid = grid_from_field_owned(field, &range, field_residue, convention);
        moments.entry(grid.moment.clone()).or_insert(grid);
    }

    Ok(ElevationCut {
        elevation_deg: fixed_angle_deg,
        elevation_number: elevation_number.and_then(|number| u8::try_from(number).ok()),
        radials,
        ray_instrument_metadata,
        moments,
    })
}

/// Convert one field back to a legacy grid (borrowed; this clones the field).
/// On an explicit range the legacy gate range is approximated from the first
/// two centres.
pub fn grid_from_field(
    field: &Field,
    range: &RangeCoord,
    residue: Option<&FieldResidue>,
    convention: LegacyConvention,
) -> MomentGrid {
    grid_from_field_owned(field.clone(), range, residue, convention)
}

fn grid_from_field_owned(
    mut field: Field,
    range: &RangeCoord,
    residue: Option<&FieldResidue>,
    convention: LegacyConvention,
) -> MomentGrid {
    let residue = residue.filter(|residue| residue.name == field.name);
    let nrays = field.nrays as usize;
    let ngates = field.ngates as usize;
    let native = field.native_geometry(range);

    let moment = residue.map_or_else(
        || field.name.to_legacy_moment(convention),
        |residue| residue.moment.clone(),
    );
    let gate_range = match (residue, native) {
        (Some(residue), Some(native))
            if residue.gate_range.gate_count == ngates
                && legacy_geometry(convention, &residue.gate_range) == native =>
        {
            residue.gate_range.clone()
        }
        (_, Some(native)) => legacy_gate_range(convention, native, ngates),
        (_, None) => legacy_gate_range(
            convention,
            (
                range.center_m(0).unwrap_or(0.0),
                range.spacing_m().unwrap_or(0.0),
            ),
            ngates,
        ),
    };

    // Rows: the residue's scattered order while the absent rows still match it.
    let gather = residue
        .and_then(|residue| residue.radial_indices.as_ref())
        .filter(|indices| {
            let mut present = vec![false; nrays];
            for &ray in indices.iter() {
                match present.get_mut(ray) {
                    Some(slot) if !*slot => *slot = true,
                    _ => return false,
                }
            }
            present
                .iter()
                .enumerate()
                .filter(|(_, present)| !**present)
                .map(|(ray, _)| ray as u32)
                .eq(field.absent_rows.iter().copied())
        })
        .cloned();

    // ODIM without a residue: remap undetect onto nodata in place, as the
    // legacy decoder did.
    if residue.is_none() && convention == LegacyConvention::Odim {
        remap_undetect_onto_fill(&mut field);
    }

    let physical = match &field.data {
        FieldData::U8 { .. } | FieldData::U16 { .. } => None,
        FieldData::F32 { coding, .. } if *coding == FloatCoding::default() => None,
        _ => Some(field.to_physical()),
    };

    let (scale, offset, nodata, range_folded, storage) = match (field.data, physical) {
        (FieldData::U8 { values, coding }, _) => {
            let (scale, offset) = legacy_scale_offset(coding.transform);
            let (nodata, range_folded) = legacy_codes(
                residue,
                convention,
                coding.fill_value.map(u16::from),
                coding.undetect.map(u16::from),
                coding.range_folded.map(u16::from),
                |code| u8::try_from(code).ok().map(u16::from),
            );
            (
                scale,
                offset,
                nodata,
                range_folded,
                MomentStorage::U8(values),
            )
        }
        (FieldData::U16 { values, coding }, _) => {
            let (scale, offset) = legacy_scale_offset(coding.transform);
            let (nodata, range_folded) = legacy_codes(
                residue,
                convention,
                coding.fill_value,
                coding.undetect,
                coding.range_folded,
                Some,
            );
            (
                scale,
                offset,
                nodata,
                range_folded,
                MomentStorage::U16(values),
            )
        }
        (FieldData::F32 { values, .. }, None) => {
            let (scale, offset) = residue
                .and_then(|residue| residue.float_scale_offset)
                .unwrap_or((1.0, 0.0));
            let (nodata, range_folded) = residue.map_or((None, None), |residue| {
                (residue.nodata, residue.range_folded)
            });
            (
                scale,
                offset,
                nodata,
                range_folded,
                MomentStorage::F32(values),
            )
        }
        (_, physical) => (
            1.0,
            0.0,
            None,
            None,
            MomentStorage::F32(physical.unwrap_or_default()),
        ),
    };

    let (radial_indices, storage) = match gather {
        Some(indices) => {
            let storage = gather_rows(storage, &indices, ngates);
            (indices, storage)
        }
        None => ((0..nrays).collect(), storage),
    };

    MomentGrid {
        moment,
        gate_range,
        scale,
        offset,
        nodata,
        range_folded,
        radial_indices,
        storage,
    }
}

fn legacy_scale_offset(transform: LinearTransform) -> (f32, f32) {
    match transform {
        LinearTransform::IcdScaleOffset { scale, offset } => (scale, offset),
        LinearTransform::CfScaleOffset {
            scale_factor,
            add_offset,
            ..
        } => (
            (1.0 / scale_factor) as f32,
            (-add_offset / scale_factor) as f32,
        ),
    }
}

/// Legacy `nodata` / `range_folded`: the residue's codes while the coding still
/// holds their forward image, else the coding's codes.
fn legacy_codes(
    residue: Option<&FieldResidue>,
    convention: LegacyConvention,
    fill_value: Option<u16>,
    undetect: Option<u16>,
    range_folded: Option<u16>,
    narrow: impl Fn(u16) -> Option<u16>,
) -> (Option<u16>, Option<u16>) {
    if let Some(residue) = residue {
        let forward_fill = residue.nodata.and_then(&narrow);
        let forward_undetect =
            (convention == LegacyConvention::Nexrad && residue.nodata == Some(0)).then_some(0);
        let forward_folded = residue.range_folded.and_then(&narrow);
        if forward_fill == fill_value
            && forward_undetect == undetect
            && forward_folded == range_folded
        {
            return (residue.nodata, residue.range_folded);
        }
    }
    (fill_value, range_folded)
}

fn remap_undetect_onto_fill(field: &mut Field) {
    fn remap<T: Copy + PartialEq>(values: &mut [T], undetect: Option<T>, fill: Option<T>) {
        if let (Some(undetect), Some(fill)) = (undetect, fill)
            && undetect != fill
        {
            values
                .iter_mut()
                .filter(|value| **value == undetect)
                .for_each(|value| *value = fill);
        }
    }
    match &mut field.data {
        FieldData::U8 { values, coding } => {
            remap(values, coding.undetect, coding.fill_value);
            coding.undetect = None;
        }
        FieldData::U16 { values, coding } => {
            remap(values, coding.undetect, coding.fill_value);
            coding.undetect = None;
        }
        _ => {}
    }
}

fn gather_rows(storage: MomentStorage, indices: &[usize], ngates: usize) -> MomentStorage {
    fn gather<T: Copy>(values: &[T], indices: &[usize], ngates: usize) -> Vec<T> {
        let mut out = Vec::with_capacity(indices.len() * ngates);
        for &ray in indices {
            if let Some(row) = values.get(ray * ngates..(ray + 1) * ngates) {
                out.extend_from_slice(row);
            }
        }
        out
    }
    match storage {
        MomentStorage::U8(values) => MomentStorage::U8(gather(&values, indices, ngates)),
        MomentStorage::U16(values) => MomentStorage::U16(gather(&values, indices, ngates)),
        MomentStorage::F32(values) => MomentStorage::F32(gather(&values, indices, ngates)),
    }
}

impl TryFrom<Volume> for RadarVolume {
    type Error = LegacyConversionError;

    /// [`legacy_from_volume`] without a residue, under the convention of the
    /// volume's source format.
    fn try_from(volume: Volume) -> Result<Self, Self::Error> {
        let convention = LegacyConvention::from(volume.provenance.source_format);
        legacy_from_volume(volume, None, convention)
    }
}
