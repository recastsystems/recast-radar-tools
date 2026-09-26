//! Cartesian gridding of one or more radar volumes by distance-weighted
//! gate-to-grid mapping, the algorithm of Py-ART's `pyart.map.grid_from_radars`
//! (`map_gates_to_grid`, its default gridding algorithm).
//!
//! Every gate of every requested field scatters its value to the grid points
//! within its radius of influence (ROI), weighted by distance:
//!
//! - [`GridWeighting::Barnes2`] (Py-ART's default): `exp(-d^2 / (R^2 / 4)) + 1e-5`,
//!   the Barnes weight of Pauley and Wu (1990);
//! - [`GridWeighting::Barnes`]: `exp(-d^2 / (2 R^2)) + 1e-5`;
//! - [`GridWeighting::Cressman`]: `(R^2 - d^2) / (R^2 + d^2)` (Cressman 1959);
//! - [`GridWeighting::Nearest`]: the nearest gate within R.
//!
//! with `d` the (optionally scaled) distance from the gate to the grid point
//! and `R` the ROI evaluated at the gate. A grid point's value is the weighted
//! mean of the gates that reached it; points no gate reached are missing
//! (NaN).
//!
//! Geometry follows Py-ART: gate positions from the 4/3-Earth model
//! (Doviak and Zrnic 1993, eqs. 2.28b-c) on each ray's azimuth and
//! elevation; with one volume and no explicit origin the grid is centred on
//! the radar, otherwise gates go through geographic coordinates onto an
//! azimuthal equidistant projection about the grid origin (Snyder 1987) with
//! Py-ART's sphere radius. The arithmetic runs in f32 as Py-ART's Cython
//! mapper does, and gates are applied in ray-then-gate order, so the sums at
//! each grid point accumulate in Py-ART's order. Positions are computed in
//! f64 before the f32 cast; Py-ART computes gate heights in f32 (up to about
//! 1 m off at 230 km), which this port does not reproduce. Against
//! unmodified Py-ART that changes which gates reach points at the edge of a
//! radius: on the real cases of `docs/design/retrievals-validation.md`,
//! 0.1-5.3 % of the defined points differ by more than 0.01, single points
//! by up to 5 dBZ (9.5 dBZ with nearest weighting), and 0-8 points are
//! defined in one grid only.
//!
//! The work is parallel over rays (gate positions) and over bands of y rows
//! within each z level (the weighted sums), and the result does not depend
//! on the thread count.
//!
//! Each field is gridded on its own recorded gates. Py-ART's
//! `read_nexrad_archive` puts every moment on one range axis, so a legacy
//! Level II file read with its 250 m Doppler moments carries the 1 km
//! reflectivity interpolated onto 250 m gates, and Py-ART grids four gates
//! per recorded one (on KPAH and KVWX 2008-04-15: 1,456 defined points
//! instead of 1,015). Read with the gridded fields alone, Py-ART keeps the
//! recorded gates and the grids agree.

use rayon::prelude::*;
use recast_radar_core::{Field, FieldName, Volume};

/// Effective Earth radius of the 4/3 model, m (Py-ART `antenna_to_cartesian`).
const EFFECTIVE_EARTH_RADIUS_M: f64 = 6371.0 * 1000.0 * 4.0 / 3.0;
/// Sphere radius of Py-ART's azimuthal equidistant projection, m.
const AEQD_EARTH_RADIUS_M: f64 = 6_370_997.0;
/// Py-ART's single-precision pi (`cdef float PI`).
const PI_F32: f32 = std::f32::consts::PI;
/// Largest grid this module allocates, in points times (fields + 1): the
/// gridded fields and the radius-of-influence field, 4 bytes a value, so the
/// output of a grid at the limit is 1 GiB. Building it also holds one weight
/// sum per point of the level bands being filled (a second one for
/// [`GridWeighting::Nearest`]) and 20 bytes per gate whose radius reaches the
/// grid.
pub const MAX_GRID_CELLS: usize = 1 << 28;

/// Geographic origin of a grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridOrigin {
    /// Latitude, degrees north.
    pub latitude_deg: f64,
    /// Longitude, degrees east.
    pub longitude_deg: f64,
    /// Altitude of the grid's z = 0, m above mean sea level.
    pub altitude_m: f64,
}

/// Grid points: `shape` = (nz, ny, nx) points spanning the inclusive limits
/// (Py-ART `grid_shape`, `grid_limits`), in metres from the origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridSpec {
    /// Points along z, y and x.
    pub shape: (usize, usize, usize),
    /// First and last z level, m above the origin altitude.
    pub z_limits_m: (f64, f64),
    /// First and last y (north) coordinate, m.
    pub y_limits_m: (f64, f64),
    /// First and last x (east) coordinate, m.
    pub x_limits_m: (f64, f64),
    /// Grid origin; `None` uses the first volume's radar position (and, with
    /// a single volume, grids in radar-relative coordinates directly).
    pub origin: Option<GridOrigin>,
}

/// Distance weighting of a gate at a grid point (see the module docs).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GridWeighting {
    /// `exp(-d^2 / (R^2 / 4)) + 1e-5` (Py-ART `Barnes2`, its default).
    Barnes2,
    /// `exp(-d^2 / (2 R^2)) + 1e-5` (Py-ART's deprecated `Barnes`).
    Barnes,
    /// `(R^2 - d^2) / (R^2 + d^2)`.
    Cressman,
    /// The nearest gate within the radius of influence. Only gates with a
    /// value take part; Py-ART also lets a masked gate win and blank the
    /// point.
    Nearest,
}

/// Radius of influence of a gate, evaluated at the gate's position.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum RadiusOfInfluence {
    /// A fixed radius, m (Py-ART `constant`).
    Constant {
        /// Radius, m.
        radius_m: f32,
    },
    /// `z_factor * dz + xy_factor * horizontal distance` from the nearest
    /// radar, at least `min_radius_m` (Py-ART `dist`).
    Distance {
        /// Growth per metre of height above the radar.
        z_factor: f32,
        /// Growth per metre of horizontal distance from the radar.
        xy_factor: f32,
        /// Smallest radius, m.
        min_radius_m: f32,
    },
    /// Distance from the nearest radar (components scaled by `h_factor`,
    /// z, y, x) times `tan(beams * beam_spacing_deg)`, at least
    /// `min_radius_m` (Py-ART `dist_beam`, its default).
    DistanceBeam {
        /// Scale of the z, y and x offsets.
        h_factor: [f32; 3],
        /// Number of beam widths the radius spans (Py-ART `nb`).
        beams: f32,
        /// Beam spacing, degrees (Py-ART `bsp`).
        beam_spacing_deg: f32,
        /// Smallest radius, m.
        min_radius_m: f32,
    },
}

/// Gridding settings. [`Default`] is Py-ART's `grid_from_radars` default:
/// Barnes2 weighting, the `dist_beam` radius (`h_factor` 1, `nb` 1, `bsp` 1,
/// `min_radius` 250 m) and unscaled distances.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridOptions {
    /// Distance weighting.
    pub weighting: GridWeighting,
    /// Radius of influence.
    pub roi: RadiusOfInfluence,
    /// Scale of the squared z, y and x differences in the gate-to-point
    /// distance (Py-ART `dist_factor`; `[0, 1, 1]` ignores height).
    pub distance_factor: [f32; 3],
}

impl Default for GridOptions {
    fn default() -> Self {
        Self {
            weighting: GridWeighting::Barnes2,
            roi: RadiusOfInfluence::DistanceBeam {
                h_factor: [1.0, 1.0, 1.0],
                beams: 1.0,
                beam_spacing_deg: 1.0,
                min_radius_m: 250.0,
            },
            distance_factor: [1.0, 1.0, 1.0],
        }
    }
}

/// One gridded field.
#[derive(Clone, Debug, PartialEq)]
pub struct GridField {
    /// The source field name.
    pub name: FieldName,
    /// Values in (z, y, x) order, x fastest; NaN where no gate contributed.
    pub values: Vec<f32>,
}

/// A Cartesian grid of radar fields.
#[derive(Clone, Debug, PartialEq)]
pub struct CartesianGrid {
    /// Points along z, y and x.
    pub shape: (usize, usize, usize),
    /// z coordinates, m above the origin altitude.
    pub z_m: Vec<f64>,
    /// y (north) coordinates, m.
    pub y_m: Vec<f64>,
    /// x (east) coordinates, m.
    pub x_m: Vec<f64>,
    /// The grid origin.
    pub origin: GridOrigin,
    /// Gridded fields, in request order.
    pub fields: Vec<GridField>,
    /// The radius of influence at each grid point, m (Py-ART's `ROI` field).
    pub roi_m: Vec<f32>,
}

impl CartesianGrid {
    /// The gridded field `name`.
    pub fn field(&self, name: &FieldName) -> Option<&GridField> {
        self.fields.iter().find(|field| &field.name == name)
    }

    /// Flat index of point (z, y, x).
    pub fn index(&self, z: usize, y: usize, x: usize) -> usize {
        (z * self.shape.1 + y) * self.shape.2 + x
    }
}

/// Why a grid could not be built.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum GridError {
    /// No volume was given.
    #[error("no radar volume to grid")]
    NoVolumes,
    /// A grid dimension is zero.
    #[error("grid shape {0:?} has an empty dimension")]
    EmptyShape((usize, usize, usize)),
    /// The grid would exceed [`MAX_GRID_CELLS`].
    #[error("grid of {cells} cells exceeds the {MAX_GRID_CELLS}-cell limit")]
    TooLarge {
        /// Points times (fields requested + 1 for the radius field).
        cells: usize,
    },
    /// A limit or origin coordinate is not finite.
    #[error("grid limits and origin must be finite")]
    NonFiniteSpec,
    /// A volume has no radar position, and the grid needs one (several
    /// volumes, or an explicit origin, or no origin to default to).
    #[error("volume {0} has no radar latitude, longitude and altitude")]
    MissingLocation(usize),
}

/// A gate position in grid coordinates and its ROI.
#[derive(Clone, Copy)]
struct GatePoint {
    z: f32,
    y: f32,
    x: f32,
    roi: f32,
}

/// Grid `fields` of `volumes` onto `spec` (see the module docs).
pub fn grid_from_volumes(
    volumes: &[&Volume],
    fields: &[FieldName],
    spec: &GridSpec,
    options: &GridOptions,
) -> Result<CartesianGrid, GridError> {
    let first = volumes.first().ok_or(GridError::NoVolumes)?;
    let (nz, ny, nx) = spec.shape;
    if nz == 0 || ny == 0 || nx == 0 {
        return Err(GridError::EmptyShape(spec.shape));
    }
    let points = nz
        .checked_mul(ny)
        .and_then(|value| value.checked_mul(nx))
        .ok_or(GridError::TooLarge { cells: usize::MAX })?;
    let cells = points.saturating_mul(fields.len() + 1);
    if cells > MAX_GRID_CELLS {
        return Err(GridError::TooLarge { cells });
    }
    let limits = [spec.z_limits_m, spec.y_limits_m, spec.x_limits_m];
    if limits.iter().any(|(a, b)| !a.is_finite() || !b.is_finite()) {
        return Err(GridError::NonFiniteSpec);
    }

    let radar_position = |index: usize, volume: &Volume| -> Result<GridOrigin, GridError> {
        let location = volume.location;
        match (
            location.latitude_deg,
            location.longitude_deg,
            location.altitude_m,
        ) {
            (Some(latitude_deg), Some(longitude_deg), Some(altitude_m)) => Ok(GridOrigin {
                latitude_deg,
                longitude_deg,
                altitude_m,
            }),
            _ => Err(GridError::MissingLocation(index)),
        }
    };
    // Py-ART skips the geographic transform for one radar without an origin.
    let skip_transform = volumes.len() == 1 && spec.origin.is_none();
    let origin = match spec.origin {
        Some(origin) => origin,
        None if skip_transform => radar_position(0, first).unwrap_or(GridOrigin {
            latitude_deg: f64::NAN,
            longitude_deg: f64::NAN,
            altitude_m: first.location.altitude_m.unwrap_or(0.0),
        }),
        None => radar_position(0, first)?,
    };
    if !(origin.altitude_m.is_finite()
        && (skip_transform
            || (origin.latitude_deg.is_finite() && origin.longitude_deg.is_finite())))
    {
        return Err(GridError::NonFiniteSpec);
    }

    // Radar offsets from the origin (z, y, x) for the ROI functions.
    let mut offsets = Vec::with_capacity(volumes.len());
    for (index, volume) in volumes.iter().enumerate() {
        if skip_transform {
            offsets.push([0.0f32; 3]);
            continue;
        }
        let radar = radar_position(index, volume)?;
        let (x, y) = geographic_to_aeqd(
            radar.longitude_deg,
            radar.latitude_deg,
            origin.longitude_deg,
            origin.latitude_deg,
        );
        offsets.push([
            (radar.altitude_m - origin.altitude_m) as f32,
            y as f32,
            x as f32,
        ]);
    }
    let roi_fn = RoiFunction::new(options.roi, &offsets);

    let step = |(start, stop): (f64, f64), n: usize| -> f32 {
        if n == 1 {
            0.0
        } else {
            ((stop - start) / (n as f64 - 1.0)) as f32
        }
    };
    let grid = GridGeometry {
        shape: spec.shape,
        start: [
            spec.z_limits_m.0 as f32,
            spec.y_limits_m.0 as f32,
            spec.x_limits_m.0 as f32,
        ],
        step: [
            step(spec.z_limits_m, nz),
            step(spec.y_limits_m, ny),
            step(spec.x_limits_m, nx),
        ],
    };

    let mut out_fields = Vec::with_capacity(fields.len());
    for name in fields {
        let values = grid_field(
            volumes,
            name,
            &grid,
            &origin,
            skip_transform,
            &roi_fn,
            options,
        )?;
        out_fields.push(GridField {
            name: name.clone(),
            values,
        });
    }

    let coordinates = |start: f32, step: f32, n: usize| -> Vec<f64> {
        (0..n).map(|i| f64::from(start + step * i as f32)).collect()
    };
    let mut roi_m = vec![0.0f32; points];
    roi_m
        .par_chunks_mut(nx)
        .enumerate()
        .for_each(|(line, roi_line)| {
            let (z, y) = (line / ny, line % ny);
            let pz = grid.start[0] + grid.step[0] * z as f32;
            let py = grid.start[1] + grid.step[1] * y as f32;
            for (x, roi) in roi_line.iter_mut().enumerate() {
                let px = grid.start[2] + grid.step[2] * x as f32;
                *roi = roi_fn.at(pz, py, px);
            }
        });
    Ok(CartesianGrid {
        shape: spec.shape,
        z_m: coordinates(grid.start[0], grid.step[0], nz),
        y_m: coordinates(grid.start[1], grid.step[1], ny),
        x_m: coordinates(grid.start[2], grid.step[2], nx),
        origin,
        fields: out_fields,
        roi_m,
    })
}

struct GridGeometry {
    shape: (usize, usize, usize),
    /// z, y, x of the first point.
    start: [f32; 3],
    /// z, y, x spacing (0 for a single point).
    step: [f32; 3],
}

/// The ROI functions of Py-ART's `_gate_to_grid_map`, in f32.
struct RoiFunction {
    kind: RadiusOfInfluence,
    offsets: Vec<[f32; 3]>,
    beam_factor: f32,
}

impl RoiFunction {
    fn new(kind: RadiusOfInfluence, offsets: &[[f32; 3]]) -> Self {
        let beam_factor = match kind {
            RadiusOfInfluence::DistanceBeam {
                beams,
                beam_spacing_deg,
                ..
            } => (f64::from(beams * beam_spacing_deg * PI_F32) / 180.0).tan() as f32,
            _ => 0.0,
        };
        Self {
            kind,
            offsets: offsets.to_vec(),
            beam_factor,
        }
    }

    fn at(&self, z: f32, y: f32, x: f32) -> f32 {
        match self.kind {
            RadiusOfInfluence::Constant { radius_m } => radius_m,
            RadiusOfInfluence::Distance {
                z_factor,
                xy_factor,
                min_radius_m,
            } => self.offsets.iter().fold(999_999_999.0f32, |best, offset| {
                // Py-ART: the float terms go through libm's double sqrt.
                let horizontal = f64::from(
                    (x - offset[2]) * (x - offset[2]) + (y - offset[1]) * (y - offset[1]),
                )
                .sqrt();
                let roi = (f64::from(z_factor * (z - offset[0]))
                    + f64::from(xy_factor) * horizontal) as f32;
                best.min(roi.max(min_radius_m))
            }),
            RadiusOfInfluence::DistanceBeam {
                h_factor,
                min_radius_m,
                ..
            } => self.offsets.iter().fold(999_999_999.0f32, |best, offset| {
                let dz = h_factor[0] * (z - offset[0]);
                let dy = h_factor[1] * (y - offset[1]);
                let dx = h_factor[2] * (x - offset[2]);
                let roi = (f64::from(dz * dz + dy * dy + dx * dx).sqrt()
                    * f64::from(self.beam_factor)) as f32;
                best.min(roi.max(min_radius_m))
            }),
        }
    }
}

/// One ray of one field, ready to be turned into gate points.
struct RayTask<'a> {
    field: &'a Field,
    row: usize,
    /// Gate centre range of gate 0 and the gate spacing, m.
    first_m: f64,
    spacing_m: f64,
    azimuth_deg: f32,
    elevation_deg: f32,
    /// The radar's latitude, longitude and altitude when gates go through
    /// geographic coordinates.
    radar: Option<(f64, f64, f64)>,
    /// Radar altitude, m (the direct path).
    radar_altitude_m: f64,
}

/// One field of every volume onto the grid; NaN where nothing contributed.
fn grid_field(
    volumes: &[&Volume],
    name: &FieldName,
    grid: &GridGeometry,
    origin: &GridOrigin,
    skip_transform: bool,
    roi_fn: &RoiFunction,
    options: &GridOptions,
) -> Result<Vec<f32>, GridError> {
    // Every ray carrying the field, in Py-ART's order: volumes, sweeps, rays.
    let mut rays = Vec::new();
    for (index, volume) in volumes.iter().enumerate() {
        let radar = if skip_transform {
            None
        } else {
            let location = volume.location;
            match (
                location.latitude_deg,
                location.longitude_deg,
                location.altitude_m,
            ) {
                (Some(lat), Some(lon), Some(alt)) => Some((lat, lon, alt)),
                _ => return Err(GridError::MissingLocation(index)),
            }
        };
        let radar_altitude_m = volume.location.altitude_m.unwrap_or(origin.altitude_m);
        for sweep in &volume.sweeps {
            let Some(field) = sweep.field(name) else {
                continue;
            };
            let Some((first_m, spacing_m)) = field.native_geometry(&sweep.range) else {
                continue;
            };
            for row in 0..field.shape().0 {
                if field.is_absent(row) {
                    continue;
                }
                let (Some(&azimuth_deg), Some(&elevation_deg)) = (
                    sweep.rays.azimuth_deg.get(row),
                    sweep.rays.elevation_deg.get(row),
                ) else {
                    continue;
                };
                rays.push(RayTask {
                    field,
                    row,
                    first_m,
                    spacing_m,
                    azimuth_deg,
                    elevation_deg,
                    radar,
                    radar_altitude_m,
                });
            }
        }
    }
    // Every gate carrying a value whose radius reaches the grid, per ray in
    // ray order (an indexed collect keeps it), gates in gate order.
    let gates: Vec<Vec<(GatePoint, f32)>> = rays
        .par_iter()
        .map(|ray| ray_gate_points(ray, grid, origin, roi_fn))
        .collect();

    // The grid is filled in tasks of one z level and a band of y rows. Each
    // task walks every gate in order, so the sums at a point accumulate in
    // the sequential order whatever the split; bands add parallelism when
    // there are fewer levels than threads.
    let (nz, ny, nx) = grid.shape;
    let plane = ny * nx;
    let bands = (2 * rayon::current_num_threads()).div_ceil(nz).clamp(1, ny);
    let band_rows = ny.div_ceil(bands);
    let mut values = vec![0.0f32; nz * plane];
    let mut tasks = Vec::with_capacity(nz * bands);
    for (zi, level) in values.chunks_mut(plane).enumerate() {
        for (band, rows) in level.chunks_mut(band_rows * nx).enumerate() {
            tasks.push((zi, band * band_rows, rows));
        }
    }
    tasks.into_par_iter().for_each(|(zi, y_first, sum)| {
        let mut weight_sum = vec![0.0f32; sum.len()];
        let mut nearest = match options.weighting {
            GridWeighting::Nearest => vec![f32::INFINITY; sum.len()],
            _ => Vec::new(),
        };
        let band = LevelBand {
            zi,
            y_first,
            y_last: y_first + sum.len() / nx - 1,
        };
        for (gate, value) in gates.iter().flatten() {
            map_gate(
                grid,
                &band,
                gate,
                *value,
                options,
                sum,
                &mut weight_sum,
                &mut nearest,
            );
        }
        for (s, w) in sum.iter_mut().zip(&weight_sum) {
            *s = if *w == 0.0 { f32::NAN } else { *s / *w };
        }
    });
    Ok(values)
}

/// The gates of one ray that carry a value and whose radius reaches the
/// grid, in gate order.
fn ray_gate_points(
    ray: &RayTask<'_>,
    grid: &GridGeometry,
    origin: &GridOrigin,
    roi_fn: &RoiFunction,
) -> Vec<(GatePoint, f32)> {
    let theta_a = f64::from(ray.azimuth_deg).to_radians();
    let theta_e = f64::from(ray.elevation_deg).to_radians();
    let (sin_e, cos_e) = theta_e.sin_cos();
    let (sin_a, cos_a) = theta_a.sin_cos();
    let (nz, ny, nx) = grid.shape;
    let mut out = Vec::new();
    for gate in 0..ray.field.shape().1 {
        let Some(value) = ray
            .field
            .value(ray.row, gate)
            .filter(|value| value.is_finite())
        else {
            continue;
        };
        let r = ray.first_m + gate as f64 * ray.spacing_m;
        let ae = EFFECTIVE_EARTH_RADIUS_M;
        let height = (r * r + ae * ae + 2.0 * r * ae * sin_e).sqrt() - ae;
        let arc = ae * (r * cos_e / (ae + height)).asin();
        let (x_radar, y_radar) = (arc * sin_a, arc * cos_a);
        let (x, y, z) = match ray.radar {
            None => (
                x_radar,
                y_radar,
                ray.radar_altitude_m + height - origin.altitude_m,
            ),
            Some((lat, lon, alt)) => {
                let (gate_lon, gate_lat) = aeqd_to_geographic(x_radar, y_radar, lon, lat);
                let (x, y) = geographic_to_aeqd(
                    gate_lon,
                    gate_lat,
                    origin.longitude_deg,
                    origin.latitude_deg,
                );
                (x, y, alt + height - origin.altitude_m)
            }
        };
        let (z, y, x) = (z as f32, y as f32, x as f32);
        let roi = roi_fn.at(z, y, x);
        // Gates whose radius misses the grid contribute nothing.
        if index_range(x - grid.start[2], roi, grid.step[2], nx).is_none()
            || index_range(y - grid.start[1], roi, grid.step[1], ny).is_none()
            || index_range(z - grid.start[0], roi, grid.step[0], nz).is_none()
        {
            continue;
        }
        out.push((GatePoint { z, y, x, roi }, value));
    }
    out.shrink_to_fit();
    out
}

/// The part of the grid one mapping task fills: z level `zi`, y rows
/// `y_first..=y_last`.
struct LevelBand {
    zi: usize,
    y_first: usize,
    y_last: usize,
}

/// Py-ART `GateToGridMapper.map_gate` restricted to one level band; the
/// accumulators hold the band's rows.
#[allow(clippy::too_many_arguments)]
fn map_gate(
    grid: &GridGeometry,
    band: &LevelBand,
    gate: &GatePoint,
    value: f32,
    options: &GridOptions,
    sum: &mut [f32],
    weight_sum: &mut [f32],
    nearest: &mut [f32],
) {
    let (nz, ny, nx) = grid.shape;
    let zi = band.zi;
    let x = gate.x - grid.start[2];
    let y = gate.y - grid.start[1];
    let z = gate.z - grid.start[0];
    let roi = gate.roi;
    let Some((z_min, z_max)) = index_range(z, roi, grid.step[0], nz) else {
        return;
    };
    if (zi as i64) < z_min || (zi as i64) > z_max {
        return;
    }
    let Some((y_min, y_max)) = index_range(y, roi, grid.step[1], ny) else {
        return;
    };
    let (y_min, y_max) = (
        y_min.max(band.y_first as i64),
        y_max.min(band.y_last as i64),
    );
    if y_min > y_max {
        return;
    }
    let Some((x_min, x_max)) = index_range(x, roi, grid.step[2], nx) else {
        return;
    };
    let roi2 = roi * roi;
    let factor = options.distance_factor;
    let zg = grid.step[0] * zi as f32;
    let dz2 = factor[0] * (zg - z) * (zg - z);
    for xi in x_min..=x_max {
        let xg = grid.step[2] * xi as f32;
        let dx2 = factor[2] * (xg - x) * (xg - x);
        for yi in y_min..=y_max {
            let yg = grid.step[1] * yi as f32;
            let dist2 = dx2 + factor[1] * (yg - y) * (yg - y) + dz2;
            let index = (yi as usize - band.y_first) * nx + xi as usize;
            match options.weighting {
                GridWeighting::Nearest => {
                    if dist2 >= roi2 {
                        continue;
                    }
                    if dist2 < nearest[index] {
                        nearest[index] = dist2;
                        weight_sum[index] = 1.0;
                        sum[index] = value;
                    }
                }
                weighting => {
                    if dist2 > roi2 {
                        continue;
                    }
                    let weight = match weighting {
                        GridWeighting::Barnes => {
                            (f64::from(-dist2 / (2.0 * roi2)).exp() + 1e-5) as f32
                        }
                        GridWeighting::Barnes2 => {
                            (f64::from(-dist2 / (roi2 / 4.0)).exp() + 1e-5) as f32
                        }
                        _ => (roi2 - dist2) / (roi2 + dist2),
                    };
                    sum[index] += weight * value;
                    weight_sum[index] += weight;
                }
            }
        }
    }
}

/// Py-ART `find_min` / `find_max`: the grid indices within `roi` of `a`
/// (grid-relative), or `None` when the span misses the grid.
fn index_range(a: f32, roi: f32, step: f32, n: usize) -> Option<(i64, i64)> {
    if step == 0.0 {
        return Some((0, 0));
    }
    let low = (((a - roi) / step).ceil() as i64).max(0);
    if low > n as i64 - 1 {
        return None;
    }
    let high = (((a + roi) / step).floor() as i64).min(n as i64 - 1);
    if high < 0 {
        return None;
    }
    Some((low, high))
}

/// Py-ART `geographic_to_cartesian_aeqd` (Snyder 1987) on a sphere of
/// 6,370,997 m: (x, y) in metres of (lon, lat) about (lon_0, lat_0).
fn geographic_to_aeqd(lon: f64, lat: f64, lon_0: f64, lat_0: f64) -> (f64, f64) {
    let (lat_r, lat_0_r) = (lat.to_radians(), lat_0.to_radians());
    let dlon = lon.to_radians() - lon_0.to_radians();
    let cos_c =
        (lat_0_r.sin() * lat_r.sin() + lat_0_r.cos() * lat_r.cos() * dlon.cos()).clamp(-1.0, 1.0);
    let c = cos_c.acos();
    let k = if c == 0.0 { 1.0 } else { c / c.sin() };
    let x = AEQD_EARTH_RADIUS_M * k * lat_r.cos() * dlon.sin();
    let y = AEQD_EARTH_RADIUS_M
        * k
        * (lat_0_r.cos() * lat_r.sin() - lat_0_r.sin() * lat_r.cos() * dlon.cos());
    (x, y)
}

/// Py-ART `cartesian_to_geographic_aeqd`: (lon, lat) of (x, y) metres about
/// (lon_0, lat_0).
fn aeqd_to_geographic(x: f64, y: f64, lon_0: f64, lat_0: f64) -> (f64, f64) {
    let lat_0_r = lat_0.to_radians();
    let rho = (x * x + y * y).sqrt();
    if rho == 0.0 {
        return (lon_0, lat_0);
    }
    let c = rho / AEQD_EARTH_RADIUS_M;
    let lat = (c.cos() * lat_0_r.sin() + y * c.sin() * lat_0_r.cos() / rho)
        .asin()
        .to_degrees();
    let x1 = x * c.sin();
    let x2 = rho * lat_0_r.cos() * c.cos() - y * lat_0_r.sin() * c.sin();
    let mut lon = (lon_0.to_radians() + x1.atan2(x2)).to_degrees();
    if lon > 180.0 {
        lon -= 360.0;
    }
    if lon < -180.0 {
        lon += 360.0;
    }
    (lon, lat)
}
