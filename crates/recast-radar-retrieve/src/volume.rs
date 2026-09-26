//! Volume/column products built from multiple sweeps.
//!
//! BowEcho already has optimized implementations of CREF, echo tops, VIL,
//! VIL density, SHI/MESH/POSH/POH, and cross sections in `recast_radar_map`. This
//! module adds generic CAPPI/column statistics plus echo-base/depth and height
//! of maximum reflectivity without coupling `recast_radar_retrieve` to that
//! crate.
//!
//! Every product selects its input by dataset variable name ([`FieldName`])
//! across sweeps, and returns a physical `F32` field on the lowest such
//! sweep's rays and native gates.

use recast_radar_core::{
    Field, FieldName, Quantity, SourceFormat, Sweep, Volume, beam_ground_range_m,
    beam_height_above_radar_m,
};

use crate::sweep::physical_field;

const EFFECTIVE_EARTH_RADIUS_M: f64 = 4.0 / 3.0 * 6_371_000.0;
const HALF_BEAMWIDTH_RAD: f64 = 0.475 * std::f64::consts::PI / 180.0;

/// How a CAPPI samples between tilts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CappiInterpolation {
    /// The value of the tilt whose beam is nearest the height.
    Nearest,
    /// Interpolate between bracketing tilts in elevation-angle space.
    LinearElevation,
}

struct SweepSampler<'a> {
    elevation_deg: f32,
    field: &'a Field,
    first_gate_m: f64,
    gate_spacing_m: f64,
    azimuth_rows: Vec<(f32, usize)>,
    ground_range_m: Vec<f64>,
    height_m: Vec<f64>,
}

impl<'a> SweepSampler<'a> {
    /// `source` is the volume's source format, which decides the tilt
    /// elevation ([`Sweep::tilt_elevation_deg`]).
    fn new(source: SourceFormat, sweep: &'a Sweep, field: &'a Field) -> Option<Self> {
        let (first_gate_m, gate_spacing_m) = field.native_geometry(&sweep.range)?;
        let gates = field.ngates as usize;
        if gates == 0 || gate_spacing_m <= 0.0 {
            return None;
        }
        let mut azimuth_rows = (0..field.nrays as usize)
            .filter_map(|row| {
                sweep
                    .rays
                    .azimuth_deg
                    .get(row)
                    .map(|azimuth| (azimuth.rem_euclid(360.0), row))
            })
            .collect::<Vec<_>>();
        if azimuth_rows.is_empty() {
            return None;
        }
        azimuth_rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        let elevation_deg = sweep.tilt_elevation_deg(source);
        let mut ground_range_m = Vec::with_capacity(gates);
        let mut height_m = Vec::with_capacity(gates);
        for gate in 0..gates {
            let slant_range_m = first_gate_m + gate as f64 * gate_spacing_m;
            ground_range_m.push(beam_ground_range_m(slant_range_m, elevation_deg as f64));
            height_m.push(beam_height_above_radar_m(
                slant_range_m,
                elevation_deg as f64,
            ));
        }
        Some(Self {
            elevation_deg,
            field,
            first_gate_m,
            gate_spacing_m,
            azimuth_rows,
            ground_range_m,
            height_m,
        })
    }

    fn nearest_row(&self, azimuth_deg: f32) -> usize {
        match self
            .azimuth_rows
            .binary_search_by(|entry| entry.0.total_cmp(&azimuth_deg))
        {
            Ok(index) => self.azimuth_rows[index].1,
            Err(index) => {
                let lower = if index == 0 {
                    self.azimuth_rows.len() - 1
                } else {
                    index - 1
                };
                let upper = if index >= self.azimuth_rows.len() {
                    0
                } else {
                    index
                };
                if angular_distance(self.azimuth_rows[lower].0, azimuth_deg)
                    <= angular_distance(self.azimuth_rows[upper].0, azimuth_deg)
                {
                    self.azimuth_rows[lower].1
                } else {
                    self.azimuth_rows[upper].1
                }
            }
        }
    }

    fn gate_for_ground_range(&self, ground_range_m: f64) -> Option<usize> {
        let count = self.ground_range_m.len();
        if count == 0 {
            return None;
        }
        let half_gate = if count > 1 {
            0.5 * (self.ground_range_m[1] - self.ground_range_m[0]).abs()
        } else {
            0.0
        };
        if ground_range_m < self.ground_range_m[0] - half_gate
            || ground_range_m > self.ground_range_m[count - 1] + half_gate
        {
            return None;
        }
        match self
            .ground_range_m
            .binary_search_by(|value| value.total_cmp(&ground_range_m))
        {
            Ok(index) => Some(index),
            Err(0) => Some(0),
            Err(index) if index >= count => Some(count - 1),
            Err(index) => {
                let lower_distance = ground_range_m - self.ground_range_m[index - 1];
                let upper_distance = self.ground_range_m[index] - ground_range_m;
                Some(if lower_distance <= upper_distance {
                    index - 1
                } else {
                    index
                })
            }
        }
    }

    fn sample(&self, azimuth_deg: f32, ground_range_m: f64) -> Option<ColumnSample> {
        let gate = self.gate_for_ground_range(ground_range_m)?;
        let row = self.nearest_row(azimuth_deg);
        let value = self.field.value(row, gate)?;
        if !value.is_finite() {
            return None;
        }
        let slant_range_m = self.first_gate_m + gate as f64 * self.gate_spacing_m;
        Some(ColumnSample {
            height_m: self.height_m[gate],
            elevation_deg: self.elevation_deg as f64,
            slant_range_m,
            value,
        })
    }

    fn row_azimuths(&self) -> Vec<f32> {
        let mut out = vec![f32::NAN; self.field.nrays as usize];
        for &(azimuth, row) in &self.azimuth_rows {
            if row < out.len() {
                out[row] = azimuth;
            }
        }
        out
    }
}

#[derive(Clone, Copy)]
struct ColumnSample {
    height_m: f64,
    elevation_deg: f64,
    slant_range_m: f64,
    value: f32,
}

/// Constant-altitude PPI of the field named `name` at `height_m` above the
/// radar, on the lowest such sweep's geometry; named
/// `CAPPI_<NAME>_<height>KM`.
pub fn cappi(
    volume: &Volume,
    name: &FieldName,
    height_m: f32,
    interpolation: CappiInterpolation,
) -> Option<Field> {
    if !height_m.is_finite() || height_m < 0.0 {
        return None;
    }
    let (base_index, base_field) = base_sweep(volume, name)?;
    let base = SweepSampler::new(
        volume.provenance.source_format,
        &volume.sweeps[base_index],
        base_field,
    )?;
    let columns = field_columns(volume, name);
    let (rows, gates) = base_field.shape();
    let azimuths = base.row_azimuths();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        let azimuth = azimuths[row];
        if !azimuth.is_finite() {
            continue;
        }
        for gate in 0..gates {
            let ground_range = base.ground_range_m[gate];
            let samples = column_profile(&columns, azimuth, ground_range);
            if let Some(value) =
                interpolate_cappi(&samples, ground_range, height_m as f64, interpolation)
            {
                out[row * gates + gate] = value;
            }
        }
    }
    let id = format!("CAPPI_{}_{:.1}KM", name.as_str(), height_m / 1000.0);
    Some(product_field(
        base_field,
        &id,
        base_field.attrs.units.as_deref(),
        out,
    ))
}

/// Column maximum of the field named `name`; `CMAX_<NAME>`.
pub fn column_max(volume: &Volume, name: &FieldName) -> Option<Field> {
    column_stat(volume, name, ColumnStatistic::Maximum)
}

/// Column minimum of the field named `name`; `CMIN_<NAME>`.
pub fn column_min(volume: &Volume, name: &FieldName) -> Option<Field> {
    column_stat(volume, name, ColumnStatistic::Minimum)
}

/// Column mean of the field named `name`; `CMEAN_<NAME>`.
pub fn column_mean(volume: &Volume, name: &FieldName) -> Option<Field> {
    column_stat(volume, name, ColumnStatistic::Mean)
}

/// Column-maximum reflectivity below `maximum_height_m`; `LLCREF`.
pub fn low_level_composite_reflectivity(volume: &Volume, maximum_height_m: f32) -> Option<Field> {
    if !maximum_height_m.is_finite() || maximum_height_m <= 0.0 {
        return None;
    }
    let name = reflectivity_name(volume)?;
    let (base_index, base_field) = base_sweep(volume, &name)?;
    let base = SweepSampler::new(
        volume.provenance.source_format,
        &volume.sweeps[base_index],
        base_field,
    )?;
    let columns = field_columns(volume, &name);
    let (rows, gates) = base_field.shape();
    let azimuths = base.row_azimuths();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        let azimuth = azimuths[row];
        if !azimuth.is_finite() {
            continue;
        }
        for gate in 0..gates {
            let maximum = column_profile(&columns, azimuth, base.ground_range_m[gate])
                .into_iter()
                .filter(|sample| sample.height_m <= maximum_height_m as f64)
                .map(|sample| sample.value)
                .fold(f32::NEG_INFINITY, f32::max);
            if maximum.is_finite() {
                out[row * gates + gate] = maximum;
            }
        }
    }
    Some(product_field(base_field, "LLCREF", Some("dBZ"), out))
}

/// Lowest height (m above the radar) with reflectivity ≥ `threshold_dbz`;
/// `EBASE`.
pub fn echo_base(volume: &Volume, threshold_dbz: f32) -> Option<Field> {
    echo_boundary(volume, threshold_dbz, EchoBoundary::Base)
}

/// Highest height (m above the radar) with reflectivity ≥ `threshold_dbz`;
/// `ET`.
pub fn echo_top_height(volume: &Volume, threshold_dbz: f32) -> Option<Field> {
    echo_boundary(volume, threshold_dbz, EchoBoundary::Top)
}

/// Echo top minus echo base (m); `EDEPTH`.
pub fn echo_depth(volume: &Volume, threshold_dbz: f32) -> Option<Field> {
    let base = echo_base(volume, threshold_dbz)?;
    let top = echo_top_height(volume, threshold_dbz)?;
    if base.gates != top.gates || base.shape() != top.shape() {
        return None;
    }
    let (rows, gates) = base.shape();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            if let (Some(base_height), Some(top_height)) =
                (base.value(row, gate), top.value(row, gate))
                && base_height.is_finite()
                && top_height.is_finite()
            {
                out[row * gates + gate] = (top_height - base_height).max(0.0);
            }
        }
    }
    Some(product_field(&base, "EDEPTH", Some("m"), out))
}

/// Height (m above the radar) of the column's maximum reflectivity; `HMAX`.
pub fn height_of_max_reflectivity(volume: &Volume) -> Option<Field> {
    let name = reflectivity_name(volume)?;
    let (base_index, base_field) = base_sweep(volume, &name)?;
    let base = SweepSampler::new(
        volume.provenance.source_format,
        &volume.sweeps[base_index],
        base_field,
    )?;
    let columns = field_columns(volume, &name);
    let (rows, gates) = base_field.shape();
    let azimuths = base.row_azimuths();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        let azimuth = azimuths[row];
        if !azimuth.is_finite() {
            continue;
        }
        for gate in 0..gates {
            let best = column_profile(&columns, azimuth, base.ground_range_m[gate])
                .into_iter()
                .max_by(|left, right| left.value.total_cmp(&right.value));
            if let Some(best) = best {
                out[row * gates + gate] = best.height_m as f32;
            }
        }
    }
    Some(product_field(base_field, "HMAX", Some("m"), out))
}

#[derive(Clone, Copy)]
enum ColumnStatistic {
    Maximum,
    Minimum,
    Mean,
}

fn column_stat(volume: &Volume, name: &FieldName, statistic: ColumnStatistic) -> Option<Field> {
    let (base_index, base_field) = base_sweep(volume, name)?;
    let base = SweepSampler::new(
        volume.provenance.source_format,
        &volume.sweeps[base_index],
        base_field,
    )?;
    let columns = field_columns(volume, name);
    let (rows, gates) = base_field.shape();
    let azimuths = base.row_azimuths();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        let azimuth = azimuths[row];
        if !azimuth.is_finite() {
            continue;
        }
        for gate in 0..gates {
            let values = column_profile(&columns, azimuth, base.ground_range_m[gate])
                .into_iter()
                .map(|sample| sample.value)
                .collect::<Vec<_>>();
            if values.is_empty() {
                continue;
            }
            out[row * gates + gate] = match statistic {
                ColumnStatistic::Maximum => values.into_iter().fold(f32::NEG_INFINITY, f32::max),
                ColumnStatistic::Minimum => values.into_iter().fold(f32::INFINITY, f32::min),
                ColumnStatistic::Mean => values.iter().sum::<f32>() / values.len() as f32,
            };
        }
    }
    let prefix = match statistic {
        ColumnStatistic::Maximum => "CMAX",
        ColumnStatistic::Minimum => "CMIN",
        ColumnStatistic::Mean => "CMEAN",
    };
    Some(product_field(
        base_field,
        &format!("{prefix}_{}", name.as_str()),
        base_field.attrs.units.as_deref(),
        out,
    ))
}

#[derive(Clone, Copy)]
enum EchoBoundary {
    Base,
    Top,
}

fn echo_boundary(volume: &Volume, threshold_dbz: f32, boundary: EchoBoundary) -> Option<Field> {
    let name = reflectivity_name(volume)?;
    let (base_index, base_field) = base_sweep(volume, &name)?;
    let base = SweepSampler::new(
        volume.provenance.source_format,
        &volume.sweeps[base_index],
        base_field,
    )?;
    let columns = field_columns(volume, &name);
    let (rows, gates) = base_field.shape();
    let azimuths = base.row_azimuths();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        let azimuth = azimuths[row];
        if !azimuth.is_finite() {
            continue;
        }
        for gate in 0..gates {
            let heights = column_profile(&columns, azimuth, base.ground_range_m[gate])
                .into_iter()
                .filter(|sample| sample.value >= threshold_dbz)
                .map(|sample| sample.height_m)
                .collect::<Vec<_>>();
            if heights.is_empty() {
                continue;
            }
            let height = match boundary {
                EchoBoundary::Base => heights.into_iter().fold(f64::INFINITY, f64::min),
                EchoBoundary::Top => heights.into_iter().fold(f64::NEG_INFINITY, f64::max),
            };
            out[row * gates + gate] = height as f32;
        }
    }
    let id = match boundary {
        EchoBoundary::Base => "EBASE",
        EchoBoundary::Top => "ET",
    };
    Some(product_field(base_field, id, Some("m"), out))
}

/// The name of the volume's reflectivity: the preferred reflectivity field
/// ([`Sweep::find`]) of the lowest sweep ([`Sweep::tilt_elevation_deg`]) that
/// has one.
fn reflectivity_name(volume: &Volume) -> Option<FieldName> {
    let source = volume.provenance.source_format;
    volume
        .sweeps
        .iter()
        .filter_map(|sweep| {
            sweep
                .find(Quantity::Reflectivity)
                .map(|field| (sweep.tilt_elevation_deg(source), field))
        })
        .min_by(|left, right| left.0.total_cmp(&right.0))
        .map(|(_, field)| field.name.clone())
}

/// Lowest sweep ([`Sweep::tilt_elevation_deg`], the first on ties) carrying
/// a field named `name`, and that field.
fn base_sweep<'a>(volume: &'a Volume, name: &FieldName) -> Option<(usize, &'a Field)> {
    let source = volume.provenance.source_format;
    volume
        .sweeps
        .iter()
        .enumerate()
        .filter_map(|(index, sweep)| {
            sweep
                .field(name)
                .map(|field| (index, sweep.tilt_elevation_deg(source), field))
        })
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(index, _, field)| (index, field))
}

fn field_columns<'a>(volume: &'a Volume, name: &FieldName) -> Vec<SweepSampler<'a>> {
    let mut columns = volume
        .sweeps
        .iter()
        .filter_map(|sweep| {
            let field = sweep.field(name)?;
            SweepSampler::new(volume.provenance.source_format, sweep, field)
        })
        .collect::<Vec<_>>();
    columns.sort_by(|left, right| left.elevation_deg.total_cmp(&right.elevation_deg));
    columns
}

fn column_profile(
    columns: &[SweepSampler<'_>],
    azimuth_deg: f32,
    ground_range_m: f64,
) -> Vec<ColumnSample> {
    let mut samples = columns
        .iter()
        .filter_map(|column| column.sample(azimuth_deg, ground_range_m))
        .collect::<Vec<_>>();
    samples.sort_by(|left, right| left.height_m.total_cmp(&right.height_m));
    samples
}

fn interpolate_cappi(
    samples: &[ColumnSample],
    ground_range_m: f64,
    target_height_m: f64,
    interpolation: CappiInterpolation,
) -> Option<f32> {
    let first = *samples.first()?;
    let last = *samples.last()?;
    if samples.len() == 1 {
        let allowance = (first.slant_range_m * HALF_BEAMWIDTH_RAD).max(250.0);
        return ((first.height_m - target_height_m).abs() <= allowance).then_some(first.value);
    }
    if target_height_m <= first.height_m {
        let allowance = (first.slant_range_m * HALF_BEAMWIDTH_RAD).max(250.0);
        return (first.height_m - target_height_m <= allowance).then_some(first.value);
    }
    if target_height_m >= last.height_m {
        let allowance = last.slant_range_m * HALF_BEAMWIDTH_RAD;
        return (target_height_m - last.height_m <= allowance).then_some(last.value);
    }
    for pair in samples.windows(2) {
        let lower = pair[0];
        let upper = pair[1];
        if target_height_m < lower.height_m || target_height_m > upper.height_m {
            continue;
        }
        return match interpolation {
            CappiInterpolation::Nearest => {
                if target_height_m - lower.height_m <= upper.height_m - target_height_m {
                    Some(lower.value)
                } else {
                    Some(upper.value)
                }
            }
            CappiInterpolation::LinearElevation => {
                let (_, target_elevation_deg) = invert_beam(ground_range_m, target_height_m);
                let span = upper.elevation_deg - lower.elevation_deg;
                if span.abs() <= 1.0e-8 {
                    Some(lower.value)
                } else {
                    let weight = ((target_elevation_deg - lower.elevation_deg) / span)
                        .clamp(0.0, 1.0) as f32;
                    Some(lower.value + weight * (upper.value - lower.value))
                }
            }
        };
    }
    None
}

fn invert_beam(ground_range_m: f64, height_m: f64) -> (f64, f64) {
    let central_angle = ground_range_m / EFFECTIVE_EARTH_RADIUS_M;
    let slant_range = (EFFECTIVE_EARTH_RADIUS_M.powi(2)
        + (EFFECTIVE_EARTH_RADIUS_M + height_m).powi(2)
        - 2.0
            * EFFECTIVE_EARTH_RADIUS_M
            * (EFFECTIVE_EARTH_RADIUS_M + height_m)
            * central_angle.cos())
    .max(0.0)
    .sqrt();
    if slant_range < 1.0 {
        return (0.0, 90.0);
    }
    let sine = (((EFFECTIVE_EARTH_RADIUS_M + height_m).powi(2)
        - EFFECTIVE_EARTH_RADIUS_M.powi(2)
        - slant_range.powi(2))
        / (2.0 * EFFECTIVE_EARTH_RADIUS_M * slant_range))
        .clamp(-1.0, 1.0);
    (slant_range, sine.asin().to_degrees())
}

fn angular_distance(left: f32, right: f32) -> f32 {
    let difference = (left - right).abs().rem_euclid(360.0);
    difference.min(360.0 - difference)
}

/// An F32 output field named `id` on the base field's geometry (NaN = no
/// data).
fn product_field(base: &Field, id: &str, units: Option<&str>, values: Vec<f32>) -> Field {
    let mut field = physical_field(
        base,
        FieldName::parse(id),
        Quantity::Other,
        None,
        None,
        values,
    );
    field.attrs.units = units.map(|units| std::borrow::Cow::Owned(units.to_owned()));
    field
}
