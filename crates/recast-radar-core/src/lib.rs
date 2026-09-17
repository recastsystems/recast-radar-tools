//! Core data model for the clean-room Rust radar analyst.
//!
//! The model is intentionally data-oriented: radial geometry lives beside compact
//! moment arrays so decoders, product algorithms, and GPU upload code can share a
//! stable contract without per-gate heap objects.
//!
//! # Limits
//!
//! [`bounded_read`] holds the resource limits every decoder crate shares
//! (expanded input size, decoded volume and batch budgets, gates per radial,
//! sweeps per volume) and the [`bounded_read::DecodeBudget`] that enforces
//! them. Each decoder crate documents its format-specific limits in its own
//! `# Limits` section.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

pub mod bounded_read;
mod field_names;
mod refractivity;

pub use field_names::canonical_moment;
pub use refractivity::{
    EARTH_DUCTING_GRADIENT_N_PER_KM, PropagationRegime, RefractedBeamError, RefractedBeamPoint,
    RefractedBeamTrace, RefractivityLevel, RefractivityProfile, RefractivityProfileError,
    STANDARD_REFRACTIVITY_GRADIENT_N_PER_KM, propagation_regime, radio_refractivity_n_units,
    trace_refracted_beam,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A NEXRAD, TDWR, or compatible radar site.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RadarSite {
    pub id: String,
    pub name: Option<String>,
    pub latitude_deg: Option<f32>,
    pub longitude_deg: Option<f32>,
    pub elevation_m: Option<f32>,
}

impl RadarSite {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            latitude_deg: None,
            longitude_deg: None,
            elevation_m: None,
        }
    }
}

/// Decoded radar volume with raw moments grouped by elevation cut.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RadarVolume {
    pub site: RadarSite,
    pub volume_time: DateTime<Utc>,
    pub vcp: Option<VcpInfo>,
    pub cuts: Vec<ElevationCut>,
    pub metadata: VolumeMetadata,
}

impl RadarVolume {
    pub fn new(site: RadarSite, volume_time: DateTime<Utc>) -> Self {
        Self {
            site,
            volume_time,
            vcp: None,
            cuts: Vec::new(),
            metadata: VolumeMetadata::default(),
        }
    }

    pub fn find_or_insert_cut(
        &mut self,
        elevation_deg: f32,
        elevation_number: Option<u8>,
    ) -> &mut ElevationCut {
        if let Some(index) = self.cuts.iter().rposition(|cut| {
            cut.elevation_number == elevation_number
                || (cut.elevation_deg - elevation_deg).abs() <= CUT_ELEVATION_MATCH_TOLERANCE_DEG
        }) {
            return &mut self.cuts[index];
        }

        self.push_cut(elevation_deg, elevation_number)
    }

    pub fn push_cut(
        &mut self,
        elevation_deg: f32,
        elevation_number: Option<u8>,
    ) -> &mut ElevationCut {
        let index = self.cuts.len();
        self.cuts
            .push(ElevationCut::new(elevation_deg, elevation_number));
        &mut self.cuts[index]
    }
}

impl Default for RadarVolume {
    fn default() -> Self {
        Self::new(RadarSite::new(""), DateTime::<Utc>::UNIX_EPOCH)
    }
}

/// Tolerance used to treat two cut elevations as the same tilt, both by
/// [`RadarVolume::find_or_insert_cut`] and by [`merge_radar_volumes`].
pub const CUT_ELEVATION_MATCH_TOLERANCE_DEG: f32 = 0.05;

/// Counters describing what [`merge_radar_volumes`] did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeReport {
    /// Moment grids moved from a later part into an elevation-matched cut.
    pub merged_moments: usize,
    /// Later-part cuts that matched an elevation but had different
    /// radial/gate geometry and were therefore dropped entirely.
    pub skipped_geometry: usize,
    /// Moment grids dropped because the matched cut already carried that
    /// moment type (first part wins).
    pub moment_collisions: usize,
}

/// Merge per-product / per-sweep partial volumes of one scan into a single
/// [`RadarVolume`].
///
/// Several international feeds split one physical volume scan across files:
/// ODIM_H5 feeds (EUMETNET OPERA Data Information Model; Michelson et al.,
/// OPERA WP 2.1/2.2, v2.2-2.3) like DWD/CHMI publish one file per product
/// and/or sweep, SHMU one full PVOL per product. This helper reassembles
/// them.
///
/// Semantics:
/// - All parts must share `site.id`, else `Err`. The first part supplies the
///   site record, VCP, and metadata.
/// - `volume_time` is the earliest part time (parts of one scan carry
///   per-product write times).
/// - Cuts are matched by `elevation_deg` within
///   [`CUT_ELEVATION_MATCH_TOLERANCE_DEG`]; an incoming cut merges into the
///   first tolerance-matched cut with compatible radial geometry that does
///   not already carry one of its moments (JMA 10-minute tars repeat a tilt
///   twice, so repetition-2 velocity must land on the repetition-2 cut, not
///   collide with repetition-1's), falling back to the first
///   geometry-compatible match when every candidate collides.
/// - A matched cut contributes its moment grids to the existing cut's
///   moment map. On a moment-type collision the first part wins and the
///   incoming grid is dropped (counted in
///   [`MergeReport::moment_collisions`]); otherwise the move is counted in
///   [`MergeReport::merged_moments`].
/// - An elevation-matched cut whose radial geometry (radial count or
///   per-radial azimuth beyond the same tolerance) differs from every
///   tolerance-matched candidate is NOT merged: moment-grid rows index
///   into the cut's radial list, so mixing azimuths would scramble the
///   display. Different gate ranges are allowed because every [`MomentGrid`]
///   carries its own [`GateRange`] and render/readout code uses the selected
///   grid's range. Rejected cuts are counted in [`MergeReport::skipped_geometry`].
/// - Unmatched cuts are unioned in, and the final cut list is sorted by
///   elevation (stable: equal elevations keep first-part-then-arrival
///   order) with `elevation_number` renumbered `1..=n` (parts carry
///   per-file sweep numbers, so a merged ladder would otherwise repeat
///   them).
///
/// Never panics: malformed combinations come back as `Err` or as skip
/// counters.
pub fn merge_radar_volumes(parts: Vec<RadarVolume>) -> Result<(RadarVolume, MergeReport), String> {
    let mut parts = parts.into_iter();
    let Some(mut base) = parts.next() else {
        return Err("no radar volumes to merge".to_owned());
    };
    let mut report = MergeReport::default();

    for part in parts {
        if part.site.id != base.site.id {
            return Err(format!(
                "cannot merge radar volumes from different sites: '{}' vs '{}'",
                base.site.id, part.site.id
            ));
        }
        if part.volume_time < base.volume_time {
            base.volume_time = part.volume_time;
        }
        for cut in part.cuts {
            // Scan elevation-matched base cuts in order: JMA 10-minute tars
            // carry two 5-minute repetitions of a tilt, so when the first
            // match already holds an incoming moment the next matching cut
            // is tried before declaring a collision (repetition-2 velocity
            // must land on the repetition-2 cut, not be silently dropped).
            let mut elevation_matched = false;
            let mut target = None;
            for (index, existing) in base.cuts.iter().enumerate() {
                if (existing.elevation_deg - cut.elevation_deg).abs()
                    > CUT_ELEVATION_MATCH_TOLERANCE_DEG
                {
                    continue;
                }
                elevation_matched = true;
                if !cut_radials_match(existing, &cut) {
                    continue;
                }
                if target.is_none() {
                    target = Some(index);
                }
                if cut
                    .moments
                    .keys()
                    .all(|moment| !existing.moments.contains_key(moment))
                {
                    target = Some(index);
                    break;
                }
            }
            let Some(index) = target else {
                if elevation_matched {
                    report.skipped_geometry += 1;
                } else {
                    base.cuts.push(cut);
                }
                continue;
            };
            let existing = &mut base.cuts[index];
            merge_radial_metadata(existing, &cut);
            for (moment, grid) in cut.moments {
                match existing.moments.entry(moment) {
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert(grid);
                        report.merged_moments += 1;
                    }
                    std::collections::btree_map::Entry::Occupied(_) => {
                        report.moment_collisions += 1;
                    }
                }
            }
        }
    }

    base.cuts
        .sort_by(|a, b| a.elevation_deg.total_cmp(&b.elevation_deg));
    // Parts carry per-file sweep numbers (JMA N5/N6 members each restart at
    // 1), so renumber the merged ladder to keep tilt numbers unique and
    // monotonic.
    for (index, cut) in base.cuts.iter_mut().enumerate() {
        cut.elevation_number = u8::try_from(index + 1).ok();
    }
    Ok((base, report))
}

/// `true` when two cuts describe the same radial azimuth geometry, so moment
/// grids whose rows index one cut's radials are valid against the other's.
fn cut_radials_match(a: &ElevationCut, b: &ElevationCut) -> bool {
    a.radials.len() == b.radials.len()
        && a.radials.iter().zip(&b.radials).all(|(ra, rb)| {
            azimuth_difference_deg(ra.azimuth_deg, rb.azimuth_deg)
                <= CUT_ELEVATION_MATCH_TOLERANCE_DEG
        })
}

fn merge_radial_metadata(existing: &mut ElevationCut, incoming: &ElevationCut) {
    for (base, other) in existing.radials.iter_mut().zip(&incoming.radials) {
        if base.nyquist_velocity_mps.is_none() {
            base.nyquist_velocity_mps = other.nyquist_velocity_mps;
        }
    }

    // Empty means absent. A malformed incoming sidecar is ignored rather
    // than indexed partially; a valid incoming sidecar replaces a malformed
    // existing one so bad alignment does not propagate into the merged cut.
    if incoming.ray_instrument_metadata.is_empty()
        || incoming.ray_instrument_metadata.len() != incoming.radials.len()
    {
        return;
    }
    if existing.ray_instrument_metadata.is_empty()
        || existing.ray_instrument_metadata.len() != existing.radials.len()
    {
        existing.ray_instrument_metadata = incoming.ray_instrument_metadata.clone();
        return;
    }
    for (base, other) in existing
        .ray_instrument_metadata
        .iter_mut()
        .zip(&incoming.ray_instrument_metadata)
    {
        if base.prt_s.is_none() {
            base.prt_s = other.prt_s;
        }
        if base.unambiguous_range_km.is_none() {
            base.unambiguous_range_km = other.unambiguous_range_km;
        }
        if base.pulse_count.is_none() {
            base.pulse_count = other.pulse_count;
        }
        if base.independent_samples.is_none() {
            base.independent_samples = other.independent_samples;
        }
    }
}

/// Smallest absolute angular difference, wrap-aware (359.99° vs 0.01° is
/// 0.02°, not 359.98°).
fn azimuth_difference_deg(a: f32, b: f32) -> f32 {
    let diff = (a - b).abs() % 360.0;
    diff.min(360.0 - diff)
}

/// One elevation sweep/cut in a volume scan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ElevationCut {
    pub elevation_deg: f32,
    pub elevation_number: Option<u8>,
    pub radials: Vec<Radial>,
    /// Physical instrument parameters aligned one-for-one with `radials`.
    ///
    /// An empty vector means the source did not supply ray-local metadata.
    /// A non-empty vector must have exactly `radials.len()` entries; use
    /// [`ElevationCut::aligned_ray_instrument_metadata`] before consuming it.
    #[serde(default)]
    pub ray_instrument_metadata: Vec<RayInstrumentMetadata>,
    pub moments: BTreeMap<MomentType, MomentGrid>,
}

impl ElevationCut {
    pub fn new(elevation_deg: f32, elevation_number: Option<u8>) -> Self {
        Self {
            elevation_deg,
            elevation_number,
            radials: Vec::new(),
            ray_instrument_metadata: Vec::new(),
            moments: BTreeMap::new(),
        }
    }

    pub fn moments_available(&self) -> BTreeSet<MomentType> {
        self.moments.keys().cloned().collect()
    }

    /// Return source-supplied ray instrument metadata after checking that it
    /// is exactly aligned with this cut's radial geometry.
    pub fn aligned_ray_instrument_metadata(
        &self,
    ) -> Result<Option<&[RayInstrumentMetadata]>, RayInstrumentMetadataAlignmentError> {
        if self.ray_instrument_metadata.is_empty() {
            return Ok(None);
        }
        if self.ray_instrument_metadata.len() != self.radials.len() {
            return Err(RayInstrumentMetadataAlignmentError {
                radial_count: self.radials.len(),
                metadata_count: self.ray_instrument_metadata.len(),
            });
        }
        Ok(Some(&self.ray_instrument_metadata))
    }
}

/// Physical instrument parameters supplied for one radar ray.
///
/// These quantities are deliberately separate from source-table PRF *codes*
/// in [`ScanLegMetadata`]. A numbered VCP code is not a frequency or PRT and
/// must never be converted into one of these physical values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RayInstrumentMetadata {
    /// Pulse repetition time in seconds.
    #[serde(default)]
    pub prt_s: Option<f32>,
    /// Instrument unambiguous range in kilometres.
    #[serde(default)]
    pub unambiguous_range_km: Option<f32>,
    /// Number of transmitted/received pulses contributing to the ray.
    #[serde(default)]
    pub pulse_count: Option<u32>,
    /// Effective number of statistically independent samples.
    #[serde(default)]
    pub independent_samples: Option<f32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RayInstrumentMetadataAlignmentError {
    pub radial_count: usize,
    pub metadata_count: usize,
}

impl fmt::Display for RayInstrumentMetadataAlignmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ray instrument metadata has {} entries for {} radials",
            self.metadata_count, self.radial_count
        )
    }
}

impl Error for RayInstrumentMetadataAlignmentError {}

/// Geometry and timing for one radial.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Radial {
    pub azimuth_deg: f32,
    pub elevation_deg: f32,
    pub time_offset_ms: i32,
    pub gate_range: GateRange,
    pub nyquist_velocity_mps: Option<f32>,
    pub radial_status: Option<RadialStatus>,
}

/// Gate layout for a radial or moment grid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GateRange {
    pub first_gate_m: i32,
    pub gate_spacing_m: i32,
    pub gate_count: usize,
}

/// NEXRAD radial status markers used to detect sweep and volume boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RadialStatus {
    StartElevation,
    Intermediate,
    EndElevation,
    StartVolume,
    EndVolume,
    StartElevationLastCut,
    Unknown(u8),
}

impl From<u8> for RadialStatus {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::StartElevation,
            1 => Self::Intermediate,
            2 => Self::EndElevation,
            3 => Self::StartVolume,
            4 => Self::EndVolume,
            5 => Self::StartElevationLastCut,
            other => Self::Unknown(other),
        }
    }
}

/// Base radar moment. Unknown names are preserved for forward compatibility.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum MomentType {
    Reflectivity,
    Velocity,
    SpectrumWidth,
    DifferentialReflectivity,
    CorrelationCoefficient,
    DifferentialPhase,
    SpecificDifferentialPhase,
    Unknown(String),
}

impl MomentType {
    pub fn from_nexrad_name(name: &str) -> Self {
        match name.trim() {
            "REF" => Self::Reflectivity,
            "VEL" => Self::Velocity,
            "SW" => Self::SpectrumWidth,
            "ZDR" => Self::DifferentialReflectivity,
            "RHO" => Self::CorrelationCoefficient,
            "PHI" => Self::DifferentialPhase,
            "KDP" => Self::SpecificDifferentialPhase,
            other => Self::Unknown(other.to_owned()),
        }
    }

    pub fn from_nexrad_bytes(name: &[u8]) -> Self {
        match name {
            b"REF" => return Self::Reflectivity,
            b"VEL" => return Self::Velocity,
            b"SW " | b"SW" => return Self::SpectrumWidth,
            b"ZDR" => return Self::DifferentialReflectivity,
            b"RHO" => return Self::CorrelationCoefficient,
            b"PHI" => return Self::DifferentialPhase,
            b"KDP" => return Self::SpecificDifferentialPhase,
            _ => {}
        }

        match trim_ascii_name(name) {
            b"REF" => Self::Reflectivity,
            b"VEL" => Self::Velocity,
            b"SW" => Self::SpectrumWidth,
            b"ZDR" => Self::DifferentialReflectivity,
            b"RHO" => Self::CorrelationCoefficient,
            b"PHI" => Self::DifferentialPhase,
            b"KDP" => Self::SpecificDifferentialPhase,
            other => Self::Unknown(String::from_utf8_lossy(other).into_owned()),
        }
    }

    pub fn short_name(&self) -> &str {
        match self {
            Self::Reflectivity => "REF",
            Self::Velocity => "VEL",
            Self::SpectrumWidth => "SW",
            Self::DifferentialReflectivity => "ZDR",
            Self::CorrelationCoefficient => "RHO",
            Self::DifferentialPhase => "PHI",
            Self::SpecificDifferentialPhase => "KDP",
            Self::Unknown(name) => name.as_str(),
        }
    }
}

fn trim_ascii_name(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.first(), Some(0 | b' ' | b'\t' | b'\r' | b'\n')) {
        bytes = &bytes[1..];
    }
    while matches!(bytes.last(), Some(0 | b' ' | b'\t' | b'\r' | b'\n')) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

impl fmt::Display for MomentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.short_name())
    }
}

/// Product identifier used by future base and derived-product registries.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct ProductId(pub String);

impl From<MomentType> for ProductId {
    fn from(moment: MomentType) -> Self {
        Self(moment.short_name().to_owned())
    }
}

/// Compact moment grid for one sweep. Rows are linked back to radial indices.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MomentGrid {
    pub moment: MomentType,
    pub gate_range: GateRange,
    pub scale: f32,
    pub offset: f32,
    pub nodata: Option<u16>,
    pub range_folded: Option<u16>,
    pub radial_indices: Vec<usize>,
    pub storage: MomentStorage,
}

impl MomentGrid {
    pub fn new_u8(
        moment: MomentType,
        gate_range: GateRange,
        scale: f32,
        offset: f32,
        nodata: Option<u8>,
        range_folded: Option<u8>,
    ) -> Self {
        Self {
            moment,
            gate_range,
            scale,
            offset,
            nodata: nodata.map(u16::from),
            range_folded: range_folded.map(u16::from),
            radial_indices: Vec::new(),
            storage: MomentStorage::U8(Vec::new()),
        }
    }

    pub fn new_u16(
        moment: MomentType,
        gate_range: GateRange,
        scale: f32,
        offset: f32,
        nodata: Option<u16>,
        range_folded: Option<u16>,
    ) -> Self {
        Self {
            moment,
            gate_range,
            scale,
            offset,
            nodata,
            range_folded,
            radial_indices: Vec::new(),
            storage: MomentStorage::U16(Vec::new()),
        }
    }

    pub fn radial_count(&self) -> usize {
        self.radial_indices.len()
    }

    pub fn reserve_rows(&mut self, additional_rows: usize) {
        self.radial_indices.reserve(additional_rows);
        let additional_values = additional_rows.saturating_mul(self.gate_range.gate_count);
        match &mut self.storage {
            MomentStorage::U8(values) => values.reserve(additional_values),
            MomentStorage::U16(values) => values.reserve(additional_values),
            MomentStorage::F32(values) => values.reserve(additional_values),
        }
    }

    pub fn push_row(&mut self, radial_index: usize, row: MomentRow) -> Result<(), MomentGridError> {
        if row.len() > self.gate_range.gate_count {
            self.expand_gate_count(row.len());
        }

        match (&mut self.storage, row) {
            (MomentStorage::U8(values), MomentRow::U8(mut row)) => {
                row.resize(self.gate_range.gate_count, self.nodata.unwrap_or(0) as u8);
                values.extend(row);
            }
            (MomentStorage::U16(values), MomentRow::U16(mut row)) => {
                row.resize(self.gate_range.gate_count, self.nodata.unwrap_or(0));
                values.extend(row);
            }
            (MomentStorage::F32(values), MomentRow::F32(mut row)) => {
                row.resize(self.gate_range.gate_count, f32::NAN);
                values.extend(row);
            }
            (storage, row) => {
                return Err(MomentGridError::StorageMismatch {
                    expected: storage.word_size_bits(),
                    actual: row.word_size_bits(),
                });
            }
        }
        self.radial_indices.push(radial_index);
        Ok(())
    }

    pub fn push_u8_row_slice(
        &mut self,
        radial_index: usize,
        row: &[u8],
    ) -> Result<(), MomentGridError> {
        if row.len() > self.gate_range.gate_count {
            self.expand_gate_count(row.len());
        }

        let MomentStorage::U8(values) = &mut self.storage else {
            return Err(MomentGridError::StorageMismatch {
                expected: self.storage.word_size_bits(),
                actual: 8,
            });
        };

        values.extend_from_slice(row);
        if row.len() < self.gate_range.gate_count {
            values.resize(
                values.len() + (self.gate_range.gate_count - row.len()),
                self.nodata.unwrap_or(0) as u8,
            );
        }
        self.radial_indices.push(radial_index);
        Ok(())
    }

    pub fn push_u16_be_row_bytes(
        &mut self,
        radial_index: usize,
        row: &[u8],
    ) -> Result<(), MomentGridError> {
        if !row.len().is_multiple_of(2) {
            return Err(MomentGridError::InvalidRowByteLength {
                word_size_bits: 16,
                byte_len: row.len(),
            });
        }

        let row_gate_count = row.len() / 2;
        if row_gate_count > self.gate_range.gate_count {
            self.expand_gate_count(row_gate_count);
        }

        let expected = self.storage.word_size_bits();
        let MomentStorage::U16(values) = &mut self.storage else {
            return Err(MomentGridError::StorageMismatch {
                expected,
                actual: 16,
            });
        };

        values.extend(
            row.chunks_exact(2)
                .map(|gate| u16::from_be_bytes([gate[0], gate[1]])),
        );
        if row_gate_count < self.gate_range.gate_count {
            values.resize(
                values.len() + (self.gate_range.gate_count - row_gate_count),
                self.nodata.unwrap_or(0),
            );
        }
        self.radial_indices.push(radial_index);
        Ok(())
    }

    pub fn scaled_value(&self, row_index: usize, gate_index: usize) -> Option<f32> {
        if gate_index >= self.gate_range.gate_count {
            return None;
        }

        let index = row_index
            .checked_mul(self.gate_range.gate_count)?
            .checked_add(gate_index)?;

        match &self.storage {
            MomentStorage::U8(values) => {
                let raw = u16::from(*values.get(index)?);
                self.scale_raw(raw)
            }
            MomentStorage::U16(values) => {
                let raw = *values.get(index)?;
                self.scale_raw(raw)
            }
            MomentStorage::F32(values) => values.get(index).copied(),
        }
    }

    fn scale_raw(&self, raw: u16) -> Option<f32> {
        if self.nodata == Some(raw) || self.range_folded == Some(raw) {
            return None;
        }
        Some((raw as f32 - self.offset) / self.scale)
    }

    fn expand_gate_count(&mut self, new_gate_count: usize) {
        let old_gate_count = self.gate_range.gate_count;
        if new_gate_count <= old_gate_count {
            return;
        }

        let rows = self.radial_indices.len();
        if rows == 0 {
            self.gate_range.gate_count = new_gate_count;
            return;
        }

        match &mut self.storage {
            MomentStorage::U8(values) => {
                let fill = self.nodata.unwrap_or(0) as u8;
                *values = expand_rows(values, rows, old_gate_count, new_gate_count, fill);
            }
            MomentStorage::U16(values) => {
                let fill = self.nodata.unwrap_or(0);
                *values = expand_rows(values, rows, old_gate_count, new_gate_count, fill);
            }
            MomentStorage::F32(values) => {
                *values = expand_rows(values, rows, old_gate_count, new_gate_count, f32::NAN);
            }
        }
        self.gate_range.gate_count = new_gate_count;
    }
}

fn expand_rows<T: Copy>(
    values: &[T],
    rows: usize,
    old_gate_count: usize,
    new_gate_count: usize,
    fill: T,
) -> Vec<T> {
    let mut expanded = Vec::with_capacity(rows * new_gate_count);
    for row in values.chunks(old_gate_count).take(rows) {
        expanded.extend_from_slice(row);
        expanded.resize(expanded.len() + (new_gate_count - old_gate_count), fill);
    }
    expanded
}

/// Backing storage for a moment grid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MomentStorage {
    U8(Vec<u8>),
    U16(Vec<u16>),
    F32(Vec<f32>),
}

impl MomentStorage {
    pub fn word_size_bits(&self) -> u8 {
        match self {
            Self::U8(_) => 8,
            Self::U16(_) => 16,
            Self::F32(_) => 32,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::U8(values) => values.len(),
            Self::U16(values) => values.len(),
            Self::F32(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One decoded row of moment values.
#[derive(Clone, Debug, PartialEq)]
pub enum MomentRow {
    U8(Vec<u8>),
    U16(Vec<u16>),
    F32(Vec<f32>),
}

impl MomentRow {
    pub fn len(&self) -> usize {
        match self {
            Self::U8(values) => values.len(),
            Self::U16(values) => values.len(),
            Self::F32(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn word_size_bits(&self) -> u8 {
        match self {
            Self::U8(_) => 8,
            Self::U16(_) => 16,
            Self::F32(_) => 32,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MomentGridError {
    GateCountMismatch { expected: usize, actual: usize },
    StorageMismatch { expected: u8, actual: u8 },
    InvalidRowByteLength { word_size_bits: u8, byte_len: usize },
}

impl fmt::Display for MomentGridError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GateCountMismatch { expected, actual } => {
                write!(f, "gate count mismatch: expected {expected}, got {actual}")
            }
            Self::StorageMismatch { expected, actual } => {
                write!(
                    f,
                    "moment storage mismatch: expected {expected}-bit, got {actual}-bit"
                )
            }
            Self::InvalidRowByteLength {
                word_size_bits,
                byte_len,
            } => {
                write!(
                    f,
                    "{word_size_bits}-bit moment row has invalid byte length {byte_len}"
                )
            }
        }
    }
}

impl Error for MomentGridError {}

/// Volume Coverage Pattern metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VcpInfo {
    pub pattern: u16,
}

/// Source-qualified provenance for one physical scan leg/sweep.
///
/// PRF values here are source-table *codes* and pulse counts. They are not
/// frequencies or pulse-repetition times; keeping them separately typed and
/// optional prevents downstream formats from inventing Hz/PRT metadata.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanLegMetadata {
    /// Zero-based row within the qualified source definition.
    #[serde(default)]
    pub source_row_index: Option<u16>,
    #[serde(default)]
    pub elevation_deg: Option<f32>,
    #[serde(default)]
    pub azimuth_rate_deg_per_second: Option<f32>,
    #[serde(default)]
    pub source_period_seconds: Option<f32>,
    /// Source waveform abbreviation (for example `CS`, `CD/W`, or `SZCD`).
    #[serde(default)]
    pub waveform: Option<String>,
    /// `surveillance`, `doppler`, or `all` for catalog-backed synthetic cuts.
    #[serde(default)]
    pub moment_coverage: Option<String>,
    #[serde(default)]
    pub surveillance_prf_code: Option<u8>,
    #[serde(default)]
    pub surveillance_pulse_count: Option<u16>,
    #[serde(default)]
    pub doppler_prf_code: Option<u8>,
    #[serde(default)]
    pub doppler_pulse_count: Option<u16>,
}

/// Antenna scanning strategy declared by the data source.
///
/// NEXRAD Archive II is always PPI surveillance, but mobile/research formats
/// (DORADE, CfRadial, ODIM_H5) carry explicit scan modes: RHI sweeps hold a
/// fixed azimuth and sweep the antenna in elevation, so a plan-view (PPI)
/// renderer shows them as a single spoke — displays must branch on this.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ScanMode {
    /// Plan-position indicator: fixed elevation, azimuth sweep (includes
    /// 360° surveillance and sector scans).
    Ppi,
    /// Range-height indicator: fixed azimuth, elevation sweep.
    Rhi,
    /// Vertically pointing (birdbath calibration / profiling).
    VerticalPointing,
    /// Declared by the source but not one of the modes above (coplane,
    /// manual, idle, calibration, airborne, ...).
    Other,
}

/// Provenance and decode statistics.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VolumeMetadata {
    pub source_path: Option<String>,
    pub archive_version: Option<String>,
    pub compression: Option<String>,
    pub message_count: usize,
    pub decoded_radial_count: usize,
    pub skipped_message_count: usize,
    /// Scan strategy declared by the source format, when it carries one
    /// (`None` for formats that do not declare it, e.g. Archive II).
    #[serde(default)]
    pub scan_mode: Option<ScanMode>,
    /// Transmit frequency rounded to MHz, when the source format declares it.
    /// Used by downstream polarimetric derivations to choose S/C/X-band
    /// coefficients without hard-coding every radar network.
    #[serde(default)]
    pub radar_frequency_mhz: Option<u32>,
    /// Horizontal one-way 3 dB antenna beam width in degrees.
    #[serde(default)]
    pub beam_width_h_deg: Option<f32>,
    /// Vertical one-way 3 dB antenna beam width in degrees.
    #[serde(default)]
    pub beam_width_v_deg: Option<f32>,
    /// Transmitted pulse duration in microseconds.
    #[serde(default)]
    pub pulse_width_us: Option<f32>,
    /// Pulse repetition time in seconds.
    #[serde(default)]
    pub prt_s: Option<f32>,
    /// Instrument unambiguous range in kilometres.
    #[serde(default)]
    pub unambiguous_range_km: Option<f32>,
    /// Human-readable scan/VCP name when supplied by the source.
    #[serde(default)]
    pub scan_name: Option<String>,
    /// Source-specific scan identifier.
    #[serde(default)]
    pub scan_id: Option<String>,
    /// Primary document that qualifies `RadarVolume::vcp` (if known). A VCP
    /// number alone is insufficient because patterns change between builds.
    #[serde(default)]
    pub vcp_source_document: Option<String>,
    #[serde(default)]
    pub vcp_source_revision: Option<String>,
    #[serde(default)]
    pub vcp_source_rda_build: Option<String>,
    #[serde(default)]
    pub vcp_source_figure: Option<String>,
    /// Source-table categorical pulse length (`short`/`long`), never inferred
    /// as a duration in microseconds.
    #[serde(default)]
    pub vcp_pulse_length: Option<String>,
    /// Explicit statement of adaptations present/absent from the base pattern.
    #[serde(default)]
    pub vcp_adaptations: Option<String>,
    /// Physical-row provenance aligned with `RadarVolume::cuts` when present.
    #[serde(default)]
    pub scan_legs: Vec<ScanLegMetadata>,
    /// Transmit/receive polarization description.
    #[serde(default)]
    pub polarization: Option<String>,
    /// Calibration provenance or configuration summary.
    #[serde(default)]
    pub calibration: Option<String>,
    /// Forward-operator implementation identifier.
    #[serde(default)]
    pub forward_operator: Option<String>,
    /// Serialized forward-operator configuration (normally JSON).
    #[serde(default)]
    pub forward_operator_config: Option<String>,
    /// Source model/run family (for example, WRF or HRRR).
    #[serde(default)]
    pub source_model: Option<String>,
    /// Model microphysics scheme used by a scattering operator.
    #[serde(default)]
    pub microphysics_scheme: Option<String>,
    /// Electromagnetic scattering model/LUT provenance.
    #[serde(default)]
    pub scattering_model: Option<String>,
}

/// Earth's mean radius (m).
pub const EARTH_RADIUS_M: f64 = 6_371_000.0;
/// Effective Earth radius under the standard "4/3 Earth" refraction model
/// (Bean & Dutton 1968; the standard-atmosphere refractivity gradient).
pub const EFFECTIVE_EARTH_RADIUS_M: f64 = EARTH_RADIUS_M * 4.0 / 3.0;

/// Center height of the radar beam **above the antenna**, in metres, under the
/// 4/3-Earth-radius effective-radius approximation for atmospheric refraction.
///
/// Doviak & Zrnić (1993), *Doppler Radar and Weather Observations* (2nd ed.),
/// eq. 2.28b: `h = sqrt(r² + aₑ² + 2·r·aₑ·sin θ) − aₑ`, with `aₑ = 4/3·a`.
///
/// `slant_range_m` is range along the beam; `elevation_deg` the antenna
/// elevation angle. Add the antenna's MSL altitude to get beam MSL height.
pub fn beam_height_above_radar_m(slant_range_m: f64, elevation_deg: f64) -> f64 {
    let ae = EFFECTIVE_EARTH_RADIUS_M;
    let r = slant_range_m;
    let theta = elevation_deg.to_radians();
    (r * r + ae * ae + 2.0 * r * ae * theta.sin()).sqrt() - ae
}

/// Great-circle (ground) distance from the radar to the gate, in metres, under
/// the same 4/3-Earth model. Doviak & Zrnić (1993) eq. 2.28c.
pub fn beam_ground_range_m(slant_range_m: f64, elevation_deg: f64) -> f64 {
    let ae = EFFECTIVE_EARTH_RADIUS_M;
    let r = slant_range_m;
    let theta = elevation_deg.to_radians();
    let h = beam_height_above_radar_m(r, elevation_deg);
    ae * ((r * theta.cos()) / (ae + h)).asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beam_height_matches_four_thirds_earth_reference() {
        // At 0° elevation, h ≈ r²/(2·aₑ): 100 km -> ~588 m.
        let h0 = beam_height_above_radar_m(100_000.0, 0.0);
        assert!((h0 - 588.6).abs() < 3.0, "0° 100km height was {h0}");

        // At 0.5° elevation, add ~r·sin(0.5°) ≈ 873 m -> ~1461 m.
        let h05 = beam_height_above_radar_m(100_000.0, 0.5);
        assert!((h05 - 1461.0).abs() < 5.0, "0.5° 100km height was {h05}");

        // Origin and monotonicity in range.
        assert!(beam_height_above_radar_m(0.0, 0.5).abs() < 1.0);
        assert!(
            beam_height_above_radar_m(200_000.0, 0.5) > beam_height_above_radar_m(100_000.0, 0.5)
        );
    }

    #[test]
    fn ground_range_close_to_slant_range_at_low_tilt() {
        // At low elevation the ground range is only slightly less than slant range.
        let s = beam_ground_range_m(100_000.0, 0.5);
        assert!(s > 99_000.0 && s < 100_000.0, "ground range was {s}");
    }

    #[test]
    fn moment_type_parses_padded_nexrad_bytes() {
        assert_eq!(
            MomentType::from_nexrad_bytes(b"SW "),
            MomentType::SpectrumWidth
        );
        assert_eq!(
            MomentType::from_nexrad_bytes(b"\0VEL"),
            MomentType::Velocity
        );
    }

    #[test]
    fn merge_rejects_empty_input() {
        let err = merge_radar_volumes(Vec::new()).unwrap_err();
        assert!(err.contains("no radar volumes"), "unexpected error: {err}");
    }
}
