//! Gate-by-gate comparison of a written-and-read-back volume with the
//! volume it was written from, and how each writer's output relates to its
//! source.
//!
//! Shared by the writer tests (`tests/write_real.rs`), the survey tool
//! (`examples/write_readback.rs`) and the `writers` fuzz harness
//! (`fuzz/src/lib.rs`), which include this file by path. It needs only
//! `recast-radar-core` and `recast-radar-io-cfradial`.

#![allow(dead_code)]

use recast_radar_core::model::{Field, FieldData, Gate, RangeCoord, SourceFormat, Sweep, Volume};
use recast_radar_io_cfradial::write::netcdf3::sanitize_name;
use recast_radar_io_cfradial::{Cfradial1Options, RangeLayout};

/// The source sweep of each written sweep and each one's ray order, when a
/// writer reorders sweeps (`None`: source order).
pub type VolumeOrder = fn(&Volume) -> Option<(Vec<usize>, Vec<Vec<usize>>)>;

/// How the read-back volume relates to its source.
pub struct Expect {
    /// Label for messages.
    pub what: String,
    /// Read-back rays of a source sweep, as source rows in order.
    pub row_order: fn(&Sweep) -> Vec<usize>,
    /// The source sweep of each read-back sweep and its ray order (overrides
    /// `row_order`), when the writer reorders sweeps.
    pub volume_order: Option<VolumeOrder>,
    /// The read-back field of a source field (`index` is its position in
    /// the source sweep).
    pub field: fn(&Sweep, &Field, usize) -> Option<usize>,
    /// Gate centres of equal grids agree to this many metres.
    pub range_tolerance_m: f64,
    /// A range-folded source gate reads back as missing (ODIM).
    pub folded_is_missing: bool,
    /// Compare the gates of every `ray_step`-th ray of each sweep (1: every
    /// ray; the fuzz harness bounds its work on large volumes). Ray angles
    /// and times are compared for every ray either way.
    pub ray_step: usize,
}

/// Azimuth order (stable).
pub fn azimuth_order(sweep: &Sweep) -> Vec<usize> {
    let mut order: Vec<usize> = (0..sweep.nrays()).collect();
    let azimuth = &sweep.rays.azimuth_deg;
    order.sort_by(|a, b| azimuth[*a].total_cmp(&azimuth[*b]));
    order
}

/// Storage order.
pub fn identity_order(sweep: &Sweep) -> Vec<usize> {
    (0..sweep.nrays()).collect()
}

/// Time order (stable).
pub fn time_order(sweep: &Sweep) -> Vec<usize> {
    let mut order: Vec<usize> = (0..sweep.nrays()).collect();
    let time = &sweep.rays.time_s;
    order.sort_by(|a, b| time[*a].total_cmp(&time[*b]));
    order
}

/// The field of the same position.
pub fn by_index(_: &Sweep, _: &Field, index: usize) -> Option<usize> {
    Some(index)
}

/// The field of the same name.
pub fn by_name(sweep: &Sweep, field: &Field, _: usize) -> Option<usize> {
    sweep.field_index(&field.name)
}

/// The field of the name the netCDF writers give it: its own, made a valid
/// netCDF name (`sanitize_name`: an empty name becomes `_`, a `/` becomes
/// `_`, ...).
pub fn by_netcdf_name(sweep: &Sweep, field: &Field, _: usize) -> Option<usize> {
    let name = sanitize_name(field.name.as_str());
    sweep
        .fields
        .iter()
        .position(|read| read.name.as_str() == name)
}

/// The field of the same name, else the field of the same position: the
/// ODIM writer numbers planes once per volume in order of first appearance
/// (a sweep whose fields come in another order reads back reordered) and
/// renames other formats' fields to ODIM quantities (in sweep order).
pub fn by_name_or_index(sweep: &Sweep, field: &Field, index: usize) -> Option<usize> {
    sweep.field_index(&field.name).or(Some(index))
}

/// The CfRadial 1 writer's sweep and ray order with the default options.
pub fn cf1_order(volume: &Volume) -> Option<(Vec<usize>, Vec<Vec<usize>>)> {
    recast_radar_io_cfradial::write::time_order(volume, &Cfradial1Options::default())
}

/// The same with `RangeLayout::PerSweep`.
pub fn cf1_order_per_sweep(volume: &Volume) -> Option<(Vec<usize>, Vec<Vec<usize>>)> {
    recast_radar_io_cfradial::write::time_order(
        volume,
        &Cfradial1Options::default().with_range_layout(RangeLayout::PerSweep),
    )
}

/// The same with `RangeLayout::PerRay`.
pub fn cf1_order_per_ray(volume: &Volume) -> Option<(Vec<usize>, Vec<Vec<usize>>)> {
    recast_radar_io_cfradial::write::time_order(
        volume,
        &Cfradial1Options::default().with_range_layout(RangeLayout::PerRay),
    )
}

/// A CfRadial 1 file written with `layout` (the other options default):
/// sweeps and rays in the writer's time order, fields by netCDF name, gate
/// centres through float32.
pub fn cfradial1_expect(what: String, layout: RangeLayout) -> Expect {
    Expect {
        what,
        row_order: identity_order,
        volume_order: Some(match layout {
            RangeLayout::PerSweep => cf1_order_per_sweep,
            RangeLayout::PerRay => cf1_order_per_ray,
            _ => cf1_order,
        }),
        field: by_netcdf_name,
        range_tolerance_m: 0.1,
        folded_is_missing: false,
        ray_step: 1,
    }
}

/// A CfRadial 2 / FM301 file: each sweep's rays in time order, fields by
/// netCDF name.
pub fn cfradial2_expect(what: String) -> Expect {
    Expect {
        what,
        row_order: time_order,
        volume_order: None,
        field: by_netcdf_name,
        range_tolerance_m: 0.1,
        folded_is_missing: false,
        ray_step: 1,
    }
}

/// An ODIM_H5 file written with the default options: rays in azimuth order
/// (in storage order for a volume read from ODIM), fields by name or
/// position ([`by_name_or_index`]), the first gate's start in whole metres,
/// range-folded gates as `nodata`.
pub fn odim_expect(what: String, source: &Volume) -> Expect {
    Expect {
        what,
        row_order: if source.provenance.source_format == SourceFormat::OdimH5 {
            identity_order
        } else {
            azimuth_order
        },
        volume_order: None,
        field: by_name_or_index,
        range_tolerance_m: 0.51,
        folded_is_missing: true,
        ray_step: 1,
    }
}

/// The source gate of sweep range gate `gate` of `field`: `None` for
/// padding.
fn native_gate(field: &Field, range_gate: usize) -> Option<usize> {
    let start = field.gates.start as usize;
    let stride = field.gates.stride.max(1) as usize;
    let native = range_gate.checked_sub(start)? / stride;
    (native < field.ngates as usize).then_some(native)
}

fn spacing(range: &RangeCoord) -> Option<f64> {
    match range {
        RangeCoord::Uniform { spacing_m, .. } => Some(*spacing_m),
        RangeCoord::Explicit { .. } => None,
    }
}

/// The source range gate at `center`: the gate whose centre is `center`
/// (within `tolerance`) when the grids are equal, else the gate that
/// contains it (a coarser source repeated on a finer grid).
fn source_gate(range: &RangeCoord, center: f64, tolerance: f64, same_grid: bool) -> Option<usize> {
    match range {
        RangeCoord::Uniform {
            first_center_m,
            spacing_m,
            ngates,
        } => {
            let index = ((center - first_center_m) / spacing_m).round();
            if index < 0.0 || index >= f64::from(*ngates) {
                return None;
            }
            let expected = first_center_m + index * spacing_m;
            let limit = if same_grid {
                tolerance
            } else {
                spacing_m / 2.0 + tolerance
            };
            ((expected - center).abs() <= limit).then_some(index as usize)
        }
        RangeCoord::Explicit { centers_m } => centers_m
            .iter()
            .position(|c| (f64::from(*c) - center).abs() <= tolerance),
    }
}

fn close(a: f32, b: f32) -> bool {
    a == b || (a - b).abs() <= 1e-5 * a.abs().max(b.abs()).max(1.0)
}

/// Equal within `tolerance`, or both NaN.
fn same_angle(a: f64, b: f64, tolerance: f64) -> bool {
    a == b || (a - b).abs() < tolerance || (a.is_nan() && b.is_nan())
}

/// The same direction within `tolerance`: readers wrap azimuths into
/// [0, 360).
fn same_azimuth(a: f64, b: f64, tolerance: f64) -> bool {
    // Each wrapped first: the remainder is exact at any magnitude, a
    // difference of two large angles is not.
    let turn = (a.rem_euclid(360.0) - b.rem_euclid(360.0)).abs();
    same_angle(a, b, tolerance) || turn < tolerance || (360.0 - turn).abs() < tolerance
}

/// Gate centre tolerance at `center`: `tolerance`, or the precision of
/// float32 there (CfRadial stores `range` as float), whichever is larger.
fn centre_tolerance(tolerance: f64, center: f64) -> f64 {
    tolerance.max(center.abs() * f64::from(f32::EPSILON))
}

/// `true` when both grids have the same gates, centre for centre.
fn same_centres(a: &RangeCoord, b: &RangeCoord, tolerance: f64) -> bool {
    a.ngates() == b.ngates()
        && (0..a.ngates()).all(|gate| match (a.center_m(gate), b.center_m(gate)) {
            (Some(x), Some(y)) => {
                x == y
                    || (x - y).abs() <= centre_tolerance(tolerance, x)
                    || (x.is_nan() && y.is_nan())
            }
            (x, y) => x.is_none() && y.is_none(),
        })
}

/// Counts of gates compared, by class.
#[derive(Default, Debug)]
pub struct Tally {
    pub values: usize,
    pub missing: usize,
    pub undetect: usize,
}

impl Tally {
    pub fn add(&mut self, other: &Tally) {
        self.values += other.values;
        self.missing += other.missing;
        self.undetect += other.undetect;
    }
}

/// `true` when the field's fill code is also its undetect code (NEXRAD raw
/// 0): a CF file cannot tell padding from undetect gates of such a field.
fn fill_is_undetect(field: &Field) -> bool {
    fn same<T: PartialEq>(fill: Option<T>, undetect: Option<T>) -> bool {
        fill.is_some() && fill == undetect
    }
    match &field.data {
        FieldData::U8 { coding, .. } => same(coding.fill_value, coding.undetect),
        FieldData::U16 { coding, .. } => same(coding.fill_value, coding.undetect),
        FieldData::I8 { coding, .. } => same(coding.fill_value, coding.undetect),
        FieldData::I16 { coding, .. } => same(coding.fill_value, coding.undetect),
        FieldData::I32 { coding, .. } => same(coding.fill_value, coding.undetect),
        FieldData::F32 { coding, .. } => coding
            .fill_value
            .is_some_and(|f| coding.undetect.is_some_and(|u| u.to_bits() == f.to_bits())),
        FieldData::F64 { coding, .. } => coding
            .fill_value
            .is_some_and(|f| coding.undetect.is_some_and(|u| u.to_bits() == f.to_bits())),
    }
}

fn is_integer(field: &Field) -> bool {
    !matches!(field.data, FieldData::F32 { .. } | FieldData::F64 { .. })
}

/// Compare every sweep, ray and gate of `read` with `source`: the gate
/// tally, or the first difference.
pub fn compare_volumes(source: &Volume, read: &Volume, expect: &Expect) -> Result<Tally, String> {
    let what = &expect.what;
    if source.sweeps.len() != read.sweeps.len() {
        return Err(format!(
            "{what}: {} sweeps read back as {}",
            source.sweeps.len(),
            read.sweeps.len()
        ));
    }
    let mut tally = Tally::default();
    let reordered = expect.volume_order.and_then(|order| order(source));
    for (index, r) in read.sweeps.iter().enumerate() {
        let (s, order) = match &reordered {
            Some((sweeps, rays)) => (&source.sweeps[sweeps[index]], rays[index].clone()),
            None => (
                &source.sweeps[index],
                (expect.row_order)(&source.sweeps[index]),
            ),
        };
        if order.len() != r.nrays() {
            return Err(format!(
                "{what}: sweep {index}: {} rays read back as {}",
                order.len(),
                r.nrays()
            ));
        }
        if !same_angle(
            f64::from(s.fixed_angle_deg),
            f64::from(r.fixed_angle_deg),
            f64::MIN_POSITIVE,
        ) {
            return Err(format!(
                "{what}: sweep {index} fixed angle {} != {}",
                s.fixed_angle_deg, r.fixed_angle_deg
            ));
        }
        for (out, row) in order.iter().enumerate() {
            let (sa, ra) = (s.rays.azimuth_deg[*row], r.rays.azimuth_deg[out]);
            if !same_azimuth(f64::from(sa), f64::from(ra), 1e-4) {
                return Err(format!(
                    "{what}: sweep {index} ray {out} azimuth {sa} != {ra}"
                ));
            }
            let (se, re) = (s.rays.elevation_deg[*row], r.rays.elevation_deg[out]);
            if !same_angle(f64::from(se), f64::from(re), 1e-4) {
                return Err(format!(
                    "{what}: sweep {index} ray {out} elevation {se} != {re}"
                ));
            }
            let st = source.time_reference.timestamp() as f64 + s.rays.time_s[*row];
            let rt = read.time_reference.timestamp() as f64 + r.rays.time_s[out];
            if !(st == rt || (st - rt).abs() <= 1e-6 || (st.is_nan() && rt.is_nan())) {
                return Err(format!("{what}: sweep {index} ray {out} time {st} != {rt}"));
            }
        }
        let same_grid = match (spacing(&s.range), spacing(&r.range)) {
            (Some(a), Some(b)) => (a - b).abs() <= 1e-6 * a.max(b),
            _ => true,
        };
        // Grids that agree gate for gate pair gates by index (a grid whose
        // centres repeat has no other pairing).
        let identical = same_centres(&s.range, &r.range, expect.range_tolerance_m);
        if same_grid && !identical {
            // Every source gate centre is on the read-back grid.
            for gate in 0..s.range.ngates() {
                let center = s.range.center_m(gate).unwrap_or(f64::NAN);
                let tolerance = centre_tolerance(expect.range_tolerance_m, center);
                if !(0..r.range.ngates()).any(|g| {
                    r.range
                        .center_m(g)
                        .is_some_and(|c| (c - center).abs() <= tolerance)
                }) {
                    return Err(format!(
                        "{what}: sweep {index} gate {gate} centre {center} m not on the read-back grid"
                    ));
                }
            }
        }
        // The source range gate of each read-back gate (the same for every
        // field and ray).
        let source_gates: Vec<Option<usize>> = (0..r.range.ngates())
            .map(|gate| {
                if identical {
                    return Some(gate);
                }
                let center = r.range.center_m(gate).unwrap_or(f64::NAN);
                let tolerance = centre_tolerance(expect.range_tolerance_m, center);
                source_gate(&s.range, center, tolerance, same_grid)
            })
            .collect();
        for (field_index, field) in s.fields.iter().enumerate() {
            let Some(read_index) = (expect.field)(r, field, field_index)
                .filter(|read_index| *read_index < r.fields.len())
            else {
                return Err(format!(
                    "{what}: sweep {index} field {} not read back",
                    field.name
                ));
            };
            let read_field = &r.fields[read_index];
            // A field written as physical floats (codings that differ
            // between sweeps) keeps values only.
            let floats_only = is_integer(field) && !is_integer(read_field);
            // Padding and absent rows of such a field hold the fill code,
            // which reads back as undetect.
            let padding_is_undetect = fill_is_undetect(field) && !expect.folded_is_missing;
            for (out, row) in order.iter().enumerate().step_by(expect.ray_step.max(1)) {
                for (gate, source_range_gate) in source_gates.iter().enumerate() {
                    let expected = source_range_gate
                        .and_then(|range_gate| native_gate(field, range_gate))
                        .and_then(|native| field.gate(*row, native))
                        .unwrap_or(Gate::Missing);
                    let expected = match expected {
                        Gate::RangeFolded if expect.folded_is_missing || floats_only => {
                            Gate::Missing
                        }
                        Gate::Undetect if floats_only => Gate::Missing,
                        Gate::Value(v) if v.is_nan() => Gate::Missing,
                        other => other,
                    };
                    let actual = native_gate(read_field, gate)
                        .and_then(|native| read_field.gate(out, native))
                        .unwrap_or(Gate::Missing);
                    let actual = match actual {
                        Gate::Value(v) if v.is_nan() => Gate::Missing,
                        other => other,
                    };
                    let same = match (expected, actual) {
                        (Gate::Value(a), Gate::Value(b)) => {
                            tally.values += 1;
                            close(a, b)
                        }
                        (Gate::Missing, Gate::Missing) => {
                            tally.missing += 1;
                            true
                        }
                        (Gate::Undetect, Gate::Undetect) => {
                            tally.undetect += 1;
                            true
                        }
                        (Gate::Missing, Gate::Undetect) if padding_is_undetect => {
                            tally.missing += 1;
                            true
                        }
                        (a, b) => a == b,
                    };
                    if !same {
                        return Err(format!(
                            "{what}: sweep {index} field {} ray {out} (source row {row}) gate \
                             {gate}: {expected:?} != {actual:?}",
                            field.name
                        ));
                    }
                }
            }
        }
    }
    Ok(tally)
}

/// [`compare_volumes`], panicking on the first difference.
pub fn assert_matches(source: &Volume, read: &Volume, expect: &Expect) -> Tally {
    compare_volumes(source, read, expect).unwrap_or_else(|difference| panic!("{difference}"))
}
