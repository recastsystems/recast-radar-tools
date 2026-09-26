//! Operations across already co-registered fields.
//!
//! These functions intentionally require identical polar geometry: the same
//! shape, the same mapping onto the sweep range and the same absent rows
//! (the caller guarantees the sweeps' range coordinates agree). Motion
//! compensation, Cartesian mosaicking, and multi-radar blending belong in a
//! separate geospatial layer; silently combining mismatched polar gates would
//! produce plausible-looking but incorrect products.

use recast_radar_core::{Field, FieldName};

use crate::physical_field_like;

/// `newer - older`, gate by gate, named `output`; `None` when the geometries differ.
pub fn difference(newer: &Field, older: &Field, output: FieldName) -> Option<Field> {
    binary_field(newer, older, output, |new, old| new - old)
}

/// Difference normalized to units per hour.
pub fn trend(
    newer: &Field,
    older: &Field,
    elapsed_seconds: f64,
    output: FieldName,
) -> Option<Field> {
    if !elapsed_seconds.is_finite() || elapsed_seconds <= 0.0 {
        return None;
    }
    let hours = (elapsed_seconds / 3600.0) as f32;
    binary_field(newer, older, output, |new, old| (new - old) / hours)
}

/// The largest value of each gate over `fields`, named `output`; `None` when the geometries differ.
pub fn maximum_swath(fields: &[&Field], output: FieldName) -> Option<Field> {
    aggregate_field(fields, output, Aggregate::Maximum)
}

/// The smallest value of each gate over `fields`, named `output`; `None` when the geometries differ.
pub fn minimum_swath(fields: &[&Field], output: FieldName) -> Option<Field> {
    aggregate_field(fields, output, Aggregate::Minimum)
}

/// The mean of each gate over `fields`, named `output`; `None` when the geometries differ.
pub fn mean(fields: &[&Field], output: FieldName) -> Option<Field> {
    aggregate_field(fields, output, Aggregate::Mean)
}

/// Integrate rate fields (for example mm/h) using trapezoids between frame
/// timestamps. Timestamps are arbitrary monotonically increasing seconds.
pub fn accumulate_rates(frames: &[(&Field, f64)], output: FieldName) -> Option<Field> {
    let (first, _) = *frames.first()?;
    if frames.len() < 2
        || frames
            .iter()
            .any(|(grid, _)| !geometry_matches(first, grid))
    {
        return None;
    }
    if frames.windows(2).any(|window| {
        !window[0].1.is_finite() || !window[1].1.is_finite() || window[1].1 <= window[0].1
    }) {
        return None;
    }

    let len = value_len(first);
    let mut accumulated = vec![0.0f32; len];
    let mut seen = vec![false; len];
    for window in frames.windows(2) {
        let (left, left_time) = window[0];
        let (right, right_time) = window[1];
        let elapsed_hours = ((right_time - left_time) / 3600.0) as f32;
        for index in 0..len {
            let Some(left_rate) = flat_value(left, index) else {
                continue;
            };
            let Some(right_rate) = flat_value(right, index) else {
                continue;
            };
            if left_rate.is_finite() && right_rate.is_finite() {
                accumulated[index] +=
                    0.5 * (left_rate.max(0.0) + right_rate.max(0.0)) * elapsed_hours;
                seen[index] = true;
            }
        }
    }
    for (value, was_seen) in accumulated.iter_mut().zip(seen) {
        if !was_seen {
            *value = f32::NAN;
        }
    }
    Some(physical_field_like(first, output, accumulated))
}

/// Time above a threshold, in minutes, using linear occupancy between frames.
pub fn exceedance_duration(
    frames: &[(&Field, f64)],
    threshold: f32,
    output: FieldName,
) -> Option<Field> {
    let (first, _) = *frames.first()?;
    if frames.len() < 2
        || frames
            .iter()
            .any(|(grid, _)| !geometry_matches(first, grid))
    {
        return None;
    }
    if frames.windows(2).any(|window| {
        !window[0].1.is_finite() || !window[1].1.is_finite() || window[1].1 <= window[0].1
    }) {
        return None;
    }

    let len = value_len(first);
    let mut minutes = vec![0.0f32; len];
    let mut seen = vec![false; len];
    for window in frames.windows(2) {
        let (left, left_time) = window[0];
        let (right, right_time) = window[1];
        let elapsed_minutes = ((right_time - left_time) / 60.0) as f32;
        for index in 0..len {
            let Some(left_value) = flat_value(left, index) else {
                continue;
            };
            let Some(right_value) = flat_value(right, index) else {
                continue;
            };
            if !left_value.is_finite() || !right_value.is_finite() {
                continue;
            }
            let occupancy = match (left_value >= threshold, right_value >= threshold) {
                (true, true) => 1.0,
                (false, false) => 0.0,
                _ => 0.5,
            };
            minutes[index] += occupancy * elapsed_minutes;
            seen[index] = true;
        }
    }
    for (value, was_seen) in minutes.iter_mut().zip(seen) {
        if !was_seen {
            *value = f32::NAN;
        }
    }
    Some(physical_field_like(first, output, minutes))
}

/// Fraction of fields meeting a threshold, expressed as 0-100 percent.
pub fn exceedance_probability(
    fields: &[&Field],
    threshold: f32,
    output: FieldName,
) -> Option<Field> {
    let first = *fields.first()?;
    if fields.iter().any(|field| !geometry_matches(first, field)) {
        return None;
    }
    let len = value_len(first);
    let mut out = vec![f32::NAN; len];
    for (index, cell) in out.iter_mut().enumerate() {
        let mut valid = 0usize;
        let mut exceeded = 0usize;
        for field in fields {
            if let Some(value) = flat_value(field, index)
                && value.is_finite()
            {
                valid += 1;
                exceeded += usize::from(value >= threshold);
            }
        }
        if valid > 0 {
            *cell = 100.0 * exceeded as f32 / valid as f32;
        }
    }
    Some(physical_field_like(first, output, out))
}

enum Aggregate {
    Maximum,
    Minimum,
    Mean,
}

fn aggregate_field(fields: &[&Field], output: FieldName, aggregate: Aggregate) -> Option<Field> {
    let first = *fields.first()?;
    if fields.iter().any(|field| !geometry_matches(first, field)) {
        return None;
    }
    let len = value_len(first);
    let mut out = vec![f32::NAN; len];
    for (index, cell) in out.iter_mut().enumerate() {
        let values = fields
            .iter()
            .filter_map(|field| flat_value(field, index))
            .filter(|value| value.is_finite())
            .collect::<Vec<_>>();
        if values.is_empty() {
            continue;
        }
        *cell = match aggregate {
            Aggregate::Maximum => values.into_iter().fold(f32::NEG_INFINITY, f32::max),
            Aggregate::Minimum => values.into_iter().fold(f32::INFINITY, f32::min),
            Aggregate::Mean => values.iter().sum::<f32>() / values.len() as f32,
        };
    }
    Some(physical_field_like(first, output, out))
}

fn binary_field(
    left: &Field,
    right: &Field,
    output: FieldName,
    operation: impl Fn(f32, f32) -> f32,
) -> Option<Field> {
    if !geometry_matches(left, right) {
        return None;
    }
    let len = value_len(left);
    let mut out = vec![f32::NAN; len];
    for (index, cell) in out.iter_mut().enumerate() {
        let Some(left_value) = flat_value(left, index) else {
            continue;
        };
        let Some(right_value) = flat_value(right, index) else {
            continue;
        };
        if left_value.is_finite() && right_value.is_finite() {
            *cell = operation(left_value, right_value);
        }
    }
    Some(physical_field_like(left, output, out))
}

fn geometry_matches(left: &Field, right: &Field) -> bool {
    left.shape() == right.shape()
        && left.gates == right.gates
        && left.absent_rows == right.absent_rows
}

fn value_len(field: &Field) -> usize {
    field.nrays as usize * field.ngates as usize
}

fn flat_value(field: &Field, index: usize) -> Option<f32> {
    let gates = field.ngates as usize;
    (gates > 0).then_some(())?;
    field.value(index / gates, index % gates)
}
