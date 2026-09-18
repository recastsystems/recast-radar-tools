//! Assembly of decoded Level II radials into the FM301 model
//! (`docs/design/fm301-model.md` sections 3, 6, 7.1 and 9).
//!
//! The builder owns the [`Volume`] under construction plus the per-sweep state
//! the record parsers need (last radial status for previews, the block-name to
//! field index cache, rejected field geometries). Rows are written once into
//! each field's native buffer; nothing is padded, resampled or expanded.

use chrono::{DateTime, NaiveTime, Utc};
use recast_radar_core::bounded_read::{DecodeBudget, MAX_SWEEPS_PER_VOLUME};
use recast_radar_core::model::{
    AttrValue, Field, FieldData, FieldName, FollowMode, GateMapping, IntCoding, PolarizationMode,
    Quantity, Scalar, SourceFormat, Sweep, SweepMode, Volume, floor_to_second,
};

use crate::messages::adaptation;
use crate::messages::rda_status::RdaSystem;
use crate::messages::vcp::VolumeCoveragePattern;
use crate::{ArchiveCompression, MessageHeader, NexradError, RadialStatus, Result};

/// Two cut elevations closer than this (degrees) are the same tilt.
const CUT_ELEVATION_MATCH_TOLERANCE_DEG: f32 = 0.05;
pub(crate) const DAY_MS: i64 = 86_400_000;

/// Gate geometry of one moment block as the file states it: centre of the
/// first gate and spacing in whole metres, and the gate count.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct BlockGates {
    pub first_gate_m: i32,
    pub gate_spacing_m: i32,
    pub gate_count: usize,
}

/// One moment of a radial, borrowed from the record.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MomentBlock<'a> {
    /// Data block name (`REF`, `VEL`, `CFP`, ...): the three bytes after the
    /// block type.
    pub name: [u8; 3],
    pub gates: BlockGates,
    pub scale: f32,
    pub offset: f32,
    pub row: MomentPayload<'a>,
    /// Message 31 moment-header values with no FM301 slot, carried into
    /// [`FieldAttrs::other`] when the field is created. `None` for Message 1,
    /// whose legacy moment header has no equivalent.
    pub extras: Option<MomentHeaderExtras>,
}

/// Message 31 data-moment header values the FM301 model has no field for
/// (ICD 2620002 Table XVII-B bytes 14-18).
#[derive(Clone, Copy, Debug)]
pub(crate) struct MomentHeaderExtras {
    /// Bytes 14-15, 0.1 dB steps.
    pub tover_raw: u16,
    /// Bytes 16-17, 0.125 dB steps (signed).
    pub snr_threshold_raw: i16,
    /// Byte 18: the recombination code.
    pub control_flags: u8,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum MomentPayload<'a> {
    U8(&'a [u8]),
    /// Big-endian 16-bit words.
    U16(&'a [u8]),
}

/// Per-sweep decode state that is not part of the model.
#[derive(Clone, Debug, Default)]
pub(crate) struct SweepState {
    /// Elevation angle of the radial that opened the sweep: the value later
    /// radials are matched against.
    pub first_elevation_deg: f32,
    /// Status of the radial that opened the sweep (checked by the golden
    /// tests; the model has no slot for it).
    #[cfg_attr(not(test), allow(dead_code))]
    pub first_status: Option<RadialStatus>,
    /// Status of the last radial appended to the sweep.
    pub last_status: Option<RadialStatus>,
    /// Block name to field index; `usize::MAX` marks a moment whose gates do
    /// not align with the sweep range (it is dropped).
    blocks: Vec<([u8; 3], usize)>,
}

/// A Level II volume under construction.
#[derive(Clone, Debug)]
pub(crate) struct VolumeBuilder {
    pub volume: Volume,
    pub sweeps: Vec<SweepState>,
    /// The volume header time, replaced by the first Message 1 radial's
    /// collection time, or by the first Message 31 collection time when the
    /// header date is the epoch.
    pub header_time: DateTime<Utc>,
    /// `true` once `Volume::time_reference` follows the first radial.
    reference_from_radial: bool,
    reference_ms: i64,
    /// Elevation angle of each VCP cut (Message 5, 1-based cut numbers), the
    /// `fixed_angle` xradar and Py-ART report.
    vcp_cut_angles_deg: Vec<f32>,
    /// `true` once a message 18 set `Volume::radar_parameters`.
    adaptation_seen: bool,
    pub budget: DecodeBudget,
}

impl VolumeBuilder {
    pub fn new(
        icao: String,
        archive_version: String,
        header_time: DateTime<Utc>,
        compression: ArchiveCompression,
        budget: DecodeBudget,
    ) -> Self {
        let mut volume = Volume::new(icao, header_time);
        volume.attrs.source = Some("NEXRAD Level II".to_owned());
        volume.provenance.source_format = SourceFormat::NexradLevel2;
        volume.provenance.source_version = Some(archive_version);
        volume.provenance.compression = Some(compression.as_str().to_owned());
        let reference_ms = volume.time_reference.timestamp_millis();
        Self {
            volume,
            sweeps: Vec::new(),
            header_time,
            reference_from_radial: false,
            reference_ms,
            vcp_cut_angles_deg: Vec::new(),
            adaptation_seen: false,
            budget,
        }
    }

    pub fn decoded_radials(&self) -> usize {
        self.volume.provenance.decode.decoded_ray_count
    }

    pub fn count_message(&mut self) {
        self.volume.provenance.decode.message_count += 1;
    }

    pub fn count_skipped(&mut self) {
        self.volume.provenance.decode.skipped_message_count += 1;
    }

    pub fn count_radial(&mut self) {
        self.volume.provenance.decode.decoded_ray_count += 1;
    }

    /// `true` while the station location or VCP is still unknown, so Message
    /// 31 volume constant blocks are parsed (the legacy rule).
    pub fn needs_volume_constants(&self) -> bool {
        let location = &self.volume.location;
        location.latitude_deg.is_none()
            || location.longitude_deg.is_none()
            || location.altitude_m.is_none()
            || self.volume.scan.vcp_pattern.is_none()
    }

    pub fn set_vcp(&mut self, pattern: u16) {
        if pattern != 0 {
            self.volume.scan.vcp_pattern = Some(pattern);
        }
    }

    /// Record a Message 5 body: the VCP number (as the legacy decoder did)
    /// and, when the message decodes, each cut's elevation angle for
    /// `Sweep::fixed_angle_deg`.
    pub fn set_vcp_message(&mut self, body: &[u8]) {
        if body.len() >= 6 {
            self.set_vcp(u16::from_be_bytes([body[4], body[5]]));
        }
        if let Ok(vcp) = VolumeCoveragePattern::decode(body) {
            self.vcp_cut_angles_deg = vcp.cuts.iter().map(|cut| cut.elevation_angle_deg).collect();
        }
    }

    /// Record a metadata-record message the volume decode reads: message 5
    /// (see [`Self::set_vcp_message`]) or the first segment of message 18.
    /// `body` is the message's first frame body.
    pub fn set_metadata_message(&mut self, header: &MessageHeader, body: &[u8]) {
        match header.message_type {
            5 => self.set_vcp_message(body),
            18 => self.set_adaptation_segment(header, body),
            _ => {}
        }
    }

    /// The transmitter frequency, antenna gain and (before Build 18) beam
    /// width of the first Open RDA message 18 (RDA Adaptation Data), from its
    /// first segment, as `Volume::radar_parameters`. WSR-88D transmits both
    /// polarizations through one antenna, so the gain and beam width are also
    /// the vertical ones. Legacy RDA bodies have another layout and are
    /// ignored.
    ///
    /// Every message 18 frame still counts as skipped: the model keeps two of
    /// its values and nothing else.
    fn set_adaptation_segment(&mut self, header: &MessageHeader, body: &[u8]) {
        self.count_skipped();
        if self.adaptation_seen
            || header.segment_number > 1
            || RdaSystem::from_channels(header.channels) != RdaSystem::Orda
        {
            return;
        }
        let Some(site) = adaptation::site_constants(body) else {
            return;
        };
        self.adaptation_seen = true;
        let parameters = &mut self.volume.radar_parameters;
        parameters.frequency_hz = site.frequency_hz.into_iter().collect();
        parameters.antenna_gain_h_db = site.antenna_gain_db;
        parameters.antenna_gain_v_db = site.antenna_gain_db;
        parameters.beam_width_h_deg = site.beam_width_deg;
        parameters.beam_width_v_deg = site.beam_width_deg;
    }

    /// Replace the legacy volume time by a radial's collection time (Message
    /// 1 first radial; Message 31 first radial when the header date is the
    /// epoch).
    pub fn set_header_time(&mut self, time: DateTime<Utc>) {
        self.header_time = time;
    }

    /// Milliseconds since the Unix epoch of a radial's collection time. A zero
    /// date takes the day of the header time.
    fn ray_instant_ms(&self, collect_date: u16, collect_ms: u32) -> i64 {
        if collect_date > 0 {
            (i64::from(collect_date) - 1) * DAY_MS + i64::from(collect_ms)
        } else {
            let midnight = self
                .header_time
                .date_naive()
                .and_time(NaiveTime::MIN)
                .and_utc();
            midnight.timestamp_millis() + i64::from(collect_ms)
        }
    }

    /// Index of the sweep a radial belongs to: a new sweep when the radial
    /// starts an elevation after the last sweep already holds rays, the last
    /// sweep when it matches, and otherwise the most recent matching sweep
    /// (created if none). Errors rather than creating a sweep beyond
    /// [`MAX_SWEEPS_PER_VOLUME`].
    pub fn sweep_for_radial(
        &mut self,
        status: RadialStatus,
        elevation_angle: f32,
        elevation_number: u8,
    ) -> Result<usize> {
        let number = Some(u16::from(elevation_number));
        let matches = |(sweep, state): (&Sweep, &SweepState)| {
            sweep.elevation_number == number
                || (state.first_elevation_deg - elevation_angle).abs()
                    <= CUT_ELEVATION_MATCH_TOLERANCE_DEG
        };
        let starts_elevation = matches!(
            status,
            RadialStatus::StartElevation
                | RadialStatus::StartVolume
                | RadialStatus::StartElevationLastCut
        );
        let sweeps = &self.volume.sweeps;
        let last_has_rays = sweeps.last().is_some_and(|sweep| sweep.nrays() > 0);
        let push_new = starts_elevation && last_has_rays;
        if !push_new
            && let Some((sweep, state)) = sweeps.last().zip(self.sweeps.last())
            && matches((sweep, state))
        {
            return Ok(sweeps.len() - 1);
        }
        if !push_new
            && let Some(index) = sweeps
                .iter()
                .zip(&self.sweeps)
                .rposition(|(sweep, state)| matches((sweep, state)))
        {
            return Ok(index);
        }
        if sweeps.len() >= MAX_SWEEPS_PER_VOLUME {
            return Err(NexradError::LimitExceeded(format!(
                "Level II volume starts more than {MAX_SWEEPS_PER_VOLUME} elevation cuts"
            )));
        }
        let index = sweeps.len();
        let mut sweep = Sweep::new(
            index as u32,
            SweepMode::AzimuthSurveillance,
            elevation_angle,
        );
        sweep.elevation_number = number;
        sweep.follow_mode = Some(FollowMode::None);
        self.volume.sweeps.push(sweep);
        self.sweeps.push(SweepState {
            first_elevation_deg: elevation_angle,
            first_status: Some(status),
            ..SweepState::default()
        });
        Ok(index)
    }

    /// Append a ray to sweep `sweep` and return its index.
    #[allow(clippy::too_many_arguments)]
    pub fn push_ray(
        &mut self,
        sweep: usize,
        collect_date: u16,
        collect_ms: u32,
        azimuth_deg: f32,
        elevation_deg: f32,
        nyquist_velocity_mps: Option<f32>,
        unambiguous_range_m: Option<f32>,
        status: RadialStatus,
        expected_rays: usize,
    ) -> usize {
        let instant_ms = self.ray_instant_ms(collect_date, collect_ms);
        if !self.reference_from_radial {
            self.reference_from_radial = true;
            if let Some(instant) = DateTime::<Utc>::from_timestamp_millis(instant_ms) {
                self.volume.time_reference = floor_to_second(instant);
                self.reference_ms = self.volume.time_reference.timestamp_millis();
            }
        }
        let time_s = (instant_ms - self.reference_ms) as f64 / 1000.0;
        let model = &mut self.volume.sweeps[sweep];
        if model.nrays() == 0 {
            model.reserve_rays(expected_rays);
        }
        let ray = model.push_ray(time_s, azimuth_deg, elevation_deg);
        let nyquist = model
            .ray_vars
            .nyquist_velocity_mps
            .get_or_insert_with(|| Vec::with_capacity(expected_rays));
        nyquist.push(nyquist_velocity_mps.unwrap_or(f32::NAN));
        let unambiguous = model
            .ray_vars
            .unambiguous_range_m
            .get_or_insert_with(|| Vec::with_capacity(expected_rays));
        unambiguous.push(unambiguous_range_m.unwrap_or(f32::NAN));
        let state = &mut self.sweeps[sweep];
        state.last_status = Some(status);
        ray
    }

    /// Write one moment row of ray `ray` into its field, creating the field on
    /// first sight. A moment whose gates cannot share the sweep range is
    /// dropped for the whole sweep.
    pub fn push_moment(
        &mut self,
        sweep: usize,
        ray: usize,
        block: &MomentBlock<'_>,
        expected_rays: usize,
    ) -> Result<()> {
        let field_index = match self.sweeps[sweep]
            .blocks
            .iter()
            .find(|(name, _)| *name == block.name)
        {
            Some((_, index)) => *index,
            None => self.register_field(sweep, block, expected_rays)?,
        };
        if field_index == usize::MAX {
            return Ok(());
        }
        let field = &mut self.volume.sweeps[sweep].fields[field_index];
        if field.nrays as usize > ray {
            // A second block of the same moment in one radial.
            return Ok(());
        }
        push_row(field, ray, block.row, &mut self.budget)
    }

    fn register_field(
        &mut self,
        sweep: usize,
        block: &MomentBlock<'_>,
        expected_rays: usize,
    ) -> Result<usize> {
        let name = FieldName::from_nexrad_block(&block.name);
        let model = &mut self.volume.sweeps[sweep];
        let index = match model.field_index(&name) {
            Some(index) => index,
            None => {
                let gates = block.gates;
                let ngates = u32::try_from(gates.gate_count).map_err(|_| {
                    NexradError::LimitExceeded("moment gate count exceeds u32".to_owned())
                })?;
                match model.attach_geometry(
                    f64::from(gates.first_gate_m),
                    f64::from(gates.gate_spacing_m),
                    ngates,
                ) {
                    Ok(mapping) => {
                        let mut field = new_field(name, mapping, ngates, block);
                        push_header_extras(&mut field, block);
                        reserve_new_field(&mut field, expected_rays, &mut self.budget)?;
                        model.fields.push(field);
                        model.fields.len() - 1
                    }
                    // Gates that cannot share the sweep range (only seen in
                    // garbage radials of misframed files): the moment is
                    // dropped for this sweep.
                    Err(_) => usize::MAX,
                }
            }
        };
        self.sweeps[sweep].blocks.push((block.name, index));
        Ok(index)
    }

    /// `true` when some sweep has at least `min_rays` rays, a field with at
    /// least `min_rays` provided rows, and has ended (end-of-elevation or
    /// end-of-volume status on its last ray, or a later sweep exists).
    pub fn has_complete_displayable_sweep(&self, min_rays: usize) -> bool {
        let count = self.volume.sweeps.len();
        self.volume
            .sweeps
            .iter()
            .zip(&self.sweeps)
            .enumerate()
            .any(|(index, (sweep, state))| {
                if sweep.nrays() < min_rays {
                    return false;
                }
                let displayable = sweep.fields.iter().any(|field| {
                    (field.nrays as usize).saturating_sub(field.absent_rows.len()) >= min_rays
                });
                if !displayable {
                    return false;
                }
                let ended = matches!(
                    state.last_status,
                    Some(RadialStatus::EndElevation | RadialStatus::EndVolume)
                );
                ended || index + 1 < count
            })
    }

    /// Seal every sweep and fill the volume-level items derived from the rays.
    pub fn finish(mut self) -> Result<(Volume, Vec<SweepState>)> {
        finalize(&mut self.volume, &self.vcp_cut_angles_deg)?;
        Ok((self.volume, self.sweeps))
    }

    /// A sealed copy of the volume so far (preview callbacks).
    pub fn snapshot(&self) -> Result<Volume> {
        let mut volume = self.volume.clone();
        finalize(&mut volume, &self.vcp_cut_angles_deg)?;
        Ok(volume)
    }
}

/// Seal sweeps, drop all-missing Nyquist vectors, set the polarization mode,
/// the VCP fixed angles, scan name and time coverage.
fn finalize(volume: &mut Volume, vcp_cut_angles_deg: &[f32]) -> Result<()> {
    for sweep in &mut volume.sweeps {
        for values in [
            &mut sweep.ray_vars.nyquist_velocity_mps,
            &mut sweep.ray_vars.unambiguous_range_m,
        ] {
            if values
                .as_ref()
                .is_some_and(|values| values.iter().all(|value| value.is_nan()))
            {
                *values = None;
            }
        }
        let dual_pol = sweep.fields.iter().any(|field| {
            matches!(
                field.quantity,
                Quantity::DifferentialReflectivity
                    | Quantity::DifferentialPhase
                    | Quantity::CorrelationCoefficient
            )
        });
        sweep.polarization_mode = Some(if dual_pol {
            PolarizationMode::HvSim
        } else {
            PolarizationMode::Horizontal
        });
        if let Some(angle) = sweep
            .elevation_number
            .and_then(|number| vcp_cut_angles_deg.get(usize::from(number).checked_sub(1)?))
        {
            sweep.fixed_angle_deg = *angle;
        }
    }
    volume.seal().map_err(|err| NexradError::InvalidMessage {
        offset: 0,
        reason: format!("decoded volume violates the model invariants: {err}"),
    })?;
    if let Some(pattern) = volume.scan.vcp_pattern {
        volume.scan.id = Some(i64::from(pattern));
        volume.scan.name = Some(format!("VCP-{pattern}"));
    }
    volume.time_coverage = volume.ray_time_extent();
    let mut previous = f64::NEG_INFINITY;
    let mut increasing = true;
    for time in volume
        .sweeps
        .iter()
        .flat_map(|sweep| sweep.rays.time_s.iter())
    {
        if *time < previous {
            increasing = false;
            break;
        }
        previous = *time;
    }
    volume.attrs.ray_times_increase = Some(increasing);
    Ok(())
}

fn new_field(name: FieldName, mapping: GateMapping, ngates: u32, block: &MomentBlock<'_>) -> Field {
    let data = match block.row {
        MomentPayload::U8(_) => FieldData::U8 {
            values: Vec::new(),
            coding: IntCoding::nexrad(block.scale, block.offset),
        },
        MomentPayload::U16(_) => FieldData::U16 {
            values: Vec::new(),
            coding: IntCoding::nexrad(block.scale, block.offset),
        },
    };
    Field::new(name, mapping, ngates, data)
}

/// Carry the Message 31 moment-header values that have no FM301 field into
/// the field's source attributes, with the RDA's own units: TOVER and the SNR
/// threshold in dB, the recombination as its Table XVII-B code. They are
/// written once, from the radial that creates the field.
fn push_header_extras(field: &mut Field, block: &MomentBlock<'_>) {
    let Some(extras) = block.extras else {
        return;
    };
    field.attrs.other.push((
        "nexrad_tover_db".into(),
        AttrValue::Scalar(Scalar::F32(f32::from(extras.tover_raw) * 0.1)),
    ));
    field.attrs.other.push((
        "nexrad_snr_threshold_db".into(),
        AttrValue::Scalar(Scalar::F32(f32::from(extras.snr_threshold_raw) * 0.125)),
    ));
    field.attrs.other.push((
        "nexrad_recombination".into(),
        AttrValue::Scalar(Scalar::U8(extras.control_flags)),
    ));
}

/// Bytes allocated by a field's value buffer.
pub(crate) fn field_capacity_bytes(field: &Field) -> usize {
    match &field.data {
        FieldData::U8 { values, .. } => values.capacity(),
        FieldData::U16 { values, .. } => values.capacity().saturating_mul(2),
        FieldData::I8 { values, .. } => values.capacity(),
        FieldData::I16 { values, .. } => values.capacity().saturating_mul(2),
        FieldData::I32 { values, .. } => values.capacity().saturating_mul(4),
        FieldData::F32 { values, .. } => values.capacity().saturating_mul(4),
        FieldData::F64 { values, .. } => values.capacity().saturating_mul(8),
    }
}

fn word_bytes(field: &Field) -> usize {
    match &field.data {
        FieldData::U8 { .. } | FieldData::I8 { .. } => 1,
        FieldData::U16 { .. } | FieldData::I16 { .. } => 2,
        FieldData::I32 { .. } | FieldData::F32 { .. } => 4,
        FieldData::F64 { .. } => 8,
    }
}

/// Reserve `expected_rays` rows for a new field, checking the reservation
/// against `budget` before allocating and charging it after.
fn reserve_new_field(
    field: &mut Field,
    expected_rays: usize,
    budget: &mut DecodeBudget,
) -> Result<()> {
    let reservation = expected_rays
        .saturating_mul(field.ngates as usize)
        .saturating_mul(word_bytes(field));
    budget
        .check(reservation, "Level II moment grid")
        .map_err(NexradError::LimitExceeded)?;
    field.reserve_rows(expected_rays);
    budget
        .charge(field_capacity_bytes(field), 1, "Level II moment grid")
        .map_err(NexradError::LimitExceeded)
}

/// Append one row, checking the growth it needs against `budget` before the
/// push and recording the field's new allocated size after it.
fn push_row(
    field: &mut Field,
    ray: usize,
    row: MomentPayload<'_>,
    budget: &mut DecodeBudget,
) -> Result<()> {
    let before = field_capacity_bytes(field);
    let (row_gates, word) = match row {
        MomentPayload::U8(bytes) => (bytes.len(), 1),
        MomentPayload::U16(bytes) => (bytes.len() / 2, 2),
    };
    let rows = ray.saturating_add(1);
    let needed = rows
        .saturating_mul((field.ngates as usize).max(row_gates))
        .saturating_mul(word);
    if needed > before {
        budget
            .check(needed - before, "Level II moment grid")
            .map_err(NexradError::LimitExceeded)?;
    }
    let pushed = match row {
        MomentPayload::U8(bytes) => field.push_row_u8(ray, bytes),
        MomentPayload::U16(bytes) => field.push_row_u16_be(ray, bytes),
    };
    pushed.map_err(|err| NexradError::InvalidMessage {
        offset: 0,
        reason: format!("moment {}: {err}", field.name),
    })?;
    let after = field_capacity_bytes(field);
    if after != before {
        budget
            .update(before, after, "Level II moment grid")
            .map_err(NexradError::LimitExceeded)?;
    }
    Ok(())
}
