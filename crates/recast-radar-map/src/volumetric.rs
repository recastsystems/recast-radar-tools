//! Volume-derived radar products via a shared column-walk.
//!
//! All three products resample every elevation cut onto the lowest tilt's
//! (azimuth, range) grid by GROUND location and walk the resulting vertical
//! column using 4/3-Earth beam geometry (Doviak & Zrnić 1993, eqs. 2.28b/c):
//!
//! * **Composite reflectivity** — column-max reflectivity (NWS NCR concept).
//! * **Echo tops** — height of the highest tilt with Z ≥ threshold (NWS ET
//!   uses 18.3 dBZ).
//! * **VIL / VIL density** — Vertically Integrated Liquid (Greene & Clark 1972,
//!   *A Vertically Integrated Liquid Water Content Profile from Radar Data*,
//!   JAM 11(8); the operational discretization in Witt et al. 1998, WAF 13(2),
//!   with the 56 dBZ hail cap). VIL density = VIL / echo-top height.
//!
//! Output fields reuse the base sweep's rays and native gate geometry (with
//! product ids as names: `CREF`, `ET`, `VIL`, `VILD`, `SHI`, `MESH`, `POSH`,
//! `POH`), so the existing renderer/azimuth lookup draws them unchanged.
//! Reflectivity is each sweep's preferred reflectivity field
//! ([`Sweep::find`] of [`Quantity::Reflectivity`]); other inputs are
//! selected by dataset variable name.

use std::borrow::Cow;

use rayon::prelude::*;
use recast_radar_core::{
    Field, FieldAttrs, FieldData, FieldName, FloatCoding, Polarization, Quantity, Sweep, Volume,
    beam_ground_range_m, beam_height_above_radar_m,
};
use recast_radar_correct::dealias_velocity;
pub use recast_radar_filters::InterpPolicy;

/// NWS echo-top reflectivity threshold (dBZ).
pub const ECHO_TOP_THRESHOLD_DBZ: f32 = 18.3;
/// Hail cap applied to reflectivity before VIL integration (dBZ).
const VIL_HAIL_CAP_DBZ: f32 = 56.0;

/// A single sweep's field resampled for column walking: a ground-range table
/// and beam-height table per gate, plus an azimuth→row index.
struct CutColumn<'a> {
    elevation_deg: f32,
    field: &'a Field,
    az_rows: Vec<(f32, usize)>, // (azimuth_deg, row) sorted by azimuth
    ground_range_m: Vec<f64>,   // per gate
    height_m: Vec<f64>,         // beam-center height above radar per gate
}

impl<'a> CutColumn<'a> {
    fn new(sweep: &'a Sweep, field: &'a Field) -> Option<Self> {
        let gates = field.ngates as usize;
        if gates == 0 {
            return None;
        }
        let (first_m, spacing_m) = field.native_geometry(&sweep.range)?;
        let mut az_rows: Vec<(f32, usize)> = (0..field.nrays as usize)
            .filter_map(|row| {
                let az = sweep.rays.azimuth_deg.get(row)?.rem_euclid(360.0);
                Some((az, row))
            })
            .collect();
        if az_rows.is_empty() {
            return None;
        }
        az_rows.sort_by(|a, b| a.0.total_cmp(&b.0));

        let elevation_deg = sweep.fixed_angle_deg;
        let (ground_range_m, height_m) = (0..gates)
            .map(|g| {
                let r = first_m + g as f64 * spacing_m;
                (
                    beam_ground_range_m(r, elevation_deg as f64),
                    beam_height_above_radar_m(r, elevation_deg as f64),
                )
            })
            .unzip();

        Some(Self {
            elevation_deg,
            field,
            az_rows,
            ground_range_m,
            height_m,
        })
    }

    fn nearest_row(&self, az: f32) -> usize {
        match self.az_rows.binary_search_by(|p| p.0.total_cmp(&az)) {
            Ok(i) => self.az_rows[i].1,
            Err(i) => {
                let lo = if i == 0 {
                    self.az_rows.len() - 1
                } else {
                    i - 1
                };
                let hi = if i >= self.az_rows.len() { 0 } else { i };
                let dl = ang_dist(self.az_rows[lo].0, az);
                let dh = ang_dist(self.az_rows[hi].0, az);
                if dl <= dh {
                    self.az_rows[lo].1
                } else {
                    self.az_rows[hi].1
                }
            }
        }
    }

    /// Gate index whose ground range is closest to `s` (monotonic table).
    fn gate_for_ground_range(&self, s: f64) -> Option<usize> {
        let n = self.ground_range_m.len();
        if n == 0 {
            return None;
        }
        // No sample beyond the farthest gate, and none BELOW the first gate's
        // ground range (within half a gate). The latter matters for high tilts:
        // their beam only reaches the surface ground range `ground_range_m[0]`,
        // so clamping shorter ranges to gate 0 would smear elevated reflectivity
        // into the radar's cone of silence (false-high CREF/ET/VIL over the site).
        let half_gate = if n >= 2 {
            0.5 * (self.ground_range_m[1] - self.ground_range_m[0])
        } else {
            0.0
        };
        if s > self.ground_range_m[n - 1] || s < self.ground_range_m[0] - half_gate {
            return None;
        }
        match self.ground_range_m.binary_search_by(|g| g.total_cmp(&s)) {
            Ok(i) => Some(i),
            Err(i) => {
                if i == 0 {
                    Some(0)
                } else if i >= n {
                    Some(n - 1)
                } else if (self.ground_range_m[i] - s) < (s - self.ground_range_m[i - 1]) {
                    Some(i)
                } else {
                    Some(i - 1)
                }
            }
        }
    }

    /// Reflectivity (dBZ) and beam height (m) at ground range `s`, azimuth `az`.
    fn sample(&self, az: f32, s: f64) -> Option<(f32, f64)> {
        let gate = self.gate_for_ground_range(s)?;
        let row = self.nearest_row(az);
        let value = self.field.value(row, gate)?;
        if !value.is_finite() {
            return None;
        }
        Some((value, self.height_m[gate]))
    }

    /// Inverse of `az_rows`: per-row azimuth (NaN for rows with no radial), so
    /// the column walk avoids an O(rows) scan per output row.
    fn row_azimuths(&self, rows: usize) -> Vec<f32> {
        let mut v = vec![f32::NAN; rows];
        for &(az, r) in &self.az_rows {
            if r < rows {
                v[r] = az;
            }
        }
        v
    }
}

fn ang_dist(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// Lowest-elevation sweep that carries reflectivity, and its field.
fn base_reflectivity_sweep(volume: &Volume) -> Option<(&Sweep, &Field)> {
    volume
        .sweeps
        .iter()
        .filter_map(|s| s.find(Quantity::Reflectivity).map(|f| (s, f)))
        .min_by(|a, b| a.0.fixed_angle_deg.total_cmp(&b.0.fixed_angle_deg))
}

/// All reflectivity-bearing sweeps as column samplers, sorted by elevation.
fn reflectivity_columns(volume: &Volume) -> Vec<CutColumn<'_>> {
    let mut cols: Vec<CutColumn<'_>> = volume
        .sweeps
        .iter()
        .filter_map(|s| CutColumn::new(s, s.find(Quantity::Reflectivity)?))
        .collect();
    cols.sort_by(|a, b| a.elevation_deg.total_cmp(&b.elevation_deg));
    cols
}

fn field_columns<'a>(volume: &'a Volume, name: &FieldName) -> Vec<CutColumn<'a>> {
    let mut cols: Vec<CutColumn<'_>> = volume
        .sweeps
        .iter()
        .filter_map(|s| CutColumn::new(s, s.field(name)?))
        .collect();
    cols.sort_by(|a, b| a.elevation_deg.total_cmp(&b.elevation_deg));
    cols
}

/// A physical `F32` field named `id` on the base field's rays, native gates
/// and absent rows (NaN = no data).
pub(crate) fn f32_field_like(
    base: &Field,
    id: &str,
    quantity: Quantity,
    units: &'static str,
    values: Vec<f32>,
) -> Field {
    debug_assert_eq!(values.len(), base.nrays as usize * base.ngates as usize);
    Field {
        name: FieldName::parse(id),
        quantity,
        polarization: Polarization::Unspecified,
        attrs: FieldAttrs {
            units: Some(Cow::Borrowed(units)),
            ..FieldAttrs::default()
        },
        nrays: base.nrays,
        ngates: base.ngates,
        gates: base.gates,
        data: FieldData::F32 {
            values,
            coding: FloatCoding::default(),
        },
        absent_rows: base.absent_rows.clone(),
    }
}

/// 4/3-effective-earth radius, m (Doviak & Zrnic 1993 Eq. 2.28 model).
const AE_M: f64 = 4.0 / 3.0 * 6_371_000.0;
/// WSR-88D half-power half-beamwidth, rad (0.95 deg aperture / 2).
const HALF_BW_RAD: f64 = 0.475 * std::f64::consts::PI / 180.0;

/// One cross-section profile sample: beam-center height, tilt elevation,
/// recovered slant range (for the beamwidth rules), value.
#[derive(Clone, Copy)]
struct ProfileSample {
    h: f64,
    theta_deg: f64,
    r_m: f64,
    v: f32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CrossSectionSmoothing {
    Native,
    Smoothed,
}

/// Slant range + elevation angle at (ground distance s, height h) — the
/// exact closed-form inverse of the 4/3-earth height equation (law of
/// cosines on the effective sphere; unit-tested to round-trip).
fn invert_beam(s: f64, h: f64) -> (f64, f64) {
    let sigma = s / AE_M;
    let r = (AE_M * AE_M + (AE_M + h) * (AE_M + h) - 2.0 * AE_M * (AE_M + h) * sigma.cos())
        .max(0.0)
        .sqrt();
    if r < 1.0 {
        return (0.0, 90.0);
    }
    let sin_theta =
        (((AE_M + h) * (AE_M + h) - AE_M * AE_M - r * r) / (2.0 * AE_M * r)).clamp(-1.0, 1.0);
    (r, sin_theta.asin().to_degrees())
}

/// Cross-section column: rich samples across all cuts, ascending height.
fn column_profile_xs(cols: &[CutColumn<'_>], az: f32, s: f64) -> Vec<ProfileSample> {
    let mut prof: Vec<ProfileSample> = cols
        .iter()
        .filter_map(|c| {
            c.sample(az, s).map(|(v, h)| {
                let (r_m, _) = invert_beam(s, h);
                ProfileSample {
                    h,
                    theta_deg: f64::from(c.elevation_deg),
                    r_m,
                    v,
                }
            })
        })
        .collect();
    prof.sort_by(|a, b| a.h.total_cmp(&b.h));
    prof
}

/// MRMS-style vertical interpolation at height z, ground distance s
/// (Zhang, Howard & Gourley 2005, Eqs. 5-7): linear IN ELEVATION ANGLE
/// between the bracketing tilts — not in height. Edge rule (Zhang et al.
/// 2011): values extend past the top/bottom tilt only within half a
/// beamwidth (range-dependent), never further; below the lowest beam we
/// keep a 300 m display floor so near-radar sections still reach ground
/// (operational RHI convention, documented divergence).
fn interp_profile_xs(prof: &[ProfileSample], z: f64, s: f64, policy: InterpPolicy) -> Option<f32> {
    let first = prof.first()?;
    let last = prof[prof.len() - 1];
    if z <= first.h {
        let extend = (first.r_m * HALF_BW_RAD).max(300.0);
        return (first.h - z <= extend).then_some(first.v);
    }
    if z >= last.h {
        let extend = last.r_m * HALF_BW_RAD;
        return (z - last.h <= extend).then_some(last.v);
    }
    let (_, theta_i) = invert_beam(s, z);
    for w in prof.windows(2) {
        let (lo, hi) = (w[0], w[1]);
        if z >= lo.h && z <= hi.h {
            let nearest = if (z - lo.h) <= (hi.h - z) { lo.v } else { hi.v };
            match policy {
                InterpPolicy::CcGuard if lo.v.min(hi.v) < 0.97 => return Some(nearest),
                InterpPolicy::VelocityGuard if (hi.v - lo.v).abs() > 30.0 => {
                    return Some(nearest);
                }
                _ => {}
            }
            let span = hi.theta_deg - lo.theta_deg;
            if span.abs() < 1e-6 {
                return Some(lo.v);
            }
            let w2 = ((theta_i - lo.theta_deg) / span).clamp(0.0, 1.0) as f32;
            return Some(lo.v + (hi.v - lo.v) * w2);
        }
    }
    Some(last.v)
}

/// Per-output-gate column at ground range `s`, azimuth `az`: (height_m, dbz)
/// pairs across all cuts, sorted ascending by height. Reused by all products.
fn column_profile(cols: &[CutColumn<'_>], az: f32, s: f64) -> Vec<(f64, f32)> {
    let mut prof: Vec<(f64, f32)> = cols
        .iter()
        .filter_map(|c| c.sample(az, s).map(|(dbz, h)| (h, dbz)))
        .collect();
    prof.sort_by(|a, b| a.0.total_cmp(&b.0));
    prof
}

/// Composite (column-max) reflectivity (`CREF`, dBZ), on the base sweep's
/// geometry.
pub fn composite_reflectivity(volume: &Volume) -> Option<Field> {
    let (base_sweep, base_field) = base_reflectivity_sweep(volume)?;
    let base = CutColumn::new(base_sweep, base_field)?;
    let cols = reflectivity_columns(volume);
    let (rows, gates) = base_field.shape();
    let mut out = vec![f32::NAN; rows * gates];
    let row_az = base.row_azimuths(rows);
    // Parallel row/gate column walk (rows are independent).
    out.par_chunks_mut(gates)
        .enumerate()
        .for_each(|(row, out_row)| {
            let az = row_az[row];
            if !az.is_finite() {
                return;
            }
            for (gate, cell) in out_row.iter_mut().enumerate() {
                let s = base.ground_range_m[gate];
                let mut max_dbz = f32::NEG_INFINITY;
                for (_, dbz) in column_profile(&cols, az, s) {
                    if dbz > max_dbz {
                        max_dbz = dbz;
                    }
                }
                if max_dbz.is_finite() {
                    *cell = max_dbz;
                }
            }
        });
    Some(f32_field_like(
        base_field,
        "CREF",
        Quantity::Reflectivity,
        "dBZ",
        out,
    ))
}

/// Echo-top height (`ET`, metres above radar) of the highest tilt with
/// Z ≥ threshold.
pub fn echo_top(volume: &Volume, threshold_dbz: f32) -> Option<Field> {
    let (base_sweep, base_field) = base_reflectivity_sweep(volume)?;
    let base = CutColumn::new(base_sweep, base_field)?;
    let cols = reflectivity_columns(volume);
    let (rows, gates) = base_field.shape();
    let mut out = vec![f32::NAN; rows * gates];
    let row_az = base.row_azimuths(rows);
    out.par_chunks_mut(gates)
        .enumerate()
        .for_each(|(row, out_row)| {
            let az = row_az[row];
            if !az.is_finite() {
                return;
            }
            for (gate, cell) in out_row.iter_mut().enumerate() {
                let s = base.ground_range_m[gate];
                let prof = column_profile(&cols, az, s);
                // highest height whose dbz >= threshold
                let top = prof
                    .iter()
                    .filter(|(_, dbz)| *dbz >= threshold_dbz)
                    .map(|(h, _)| *h)
                    .fold(f64::NEG_INFINITY, f64::max);
                if top.is_finite() {
                    *cell = top as f32;
                }
            }
        });
    Some(f32_field_like(base_field, "ET", Quantity::Other, "m", out))
}

/// Convert reflectivity factor in dBZ to linear (mm^6 m^-3).
#[inline]
fn dbz_to_z(dbz: f32) -> f64 {
    10f64.powf(dbz as f64 / 10.0)
}

/// Vertically Integrated Liquid (`VIL`, kg m^-2), Greene & Clark (1972) with
/// the 56 dBZ hail cap (Witt et al. 1998).
pub fn vil(volume: &Volume) -> Option<Field> {
    let (base_sweep, base_field) = base_reflectivity_sweep(volume)?;
    let base = CutColumn::new(base_sweep, base_field)?;
    let cols = reflectivity_columns(volume);
    let (rows, gates) = base_field.shape();
    let mut out = vec![f32::NAN; rows * gates];
    let row_az = base.row_azimuths(rows);
    // VIL = Σ 3.44e-6 * Zbar^(4/7) * Δh ; cap reflectivity at hail cap.
    let vil_inc = |z_lin: f64, dh: f64| 3.44e-6 * z_lin.powf(4.0 / 7.0) * dh;
    out.par_chunks_mut(gates)
        .enumerate()
        .for_each(|(row, out_row)| {
            let az = row_az[row];
            if !az.is_finite() {
                return;
            }
            for (gate, cell) in out_row.iter_mut().enumerate() {
                let s = base.ground_range_m[gate];
                let prof = column_profile(&cols, az, s);
                if prof.is_empty() {
                    continue;
                }
                let mut vil = 0.0f64;
                // Surface layer: the lowest beam represents the column down to the
                // ground (operational convention; Greene & Clark 1972, Witt 1998),
                // so a single deep tilt still contributes rather than reporting 0.
                let (h0, z0) = prof[0];
                if h0 > 0.0 {
                    vil += vil_inc(dbz_to_z(z0.min(VIL_HAIL_CAP_DBZ)), h0);
                }
                for w in prof.windows(2) {
                    let (ha, za) = w[0];
                    let (hb, zb) = w[1];
                    let dh = (hb - ha).max(0.0);
                    let za_c = dbz_to_z(za.min(VIL_HAIL_CAP_DBZ));
                    let zb_c = dbz_to_z(zb.min(VIL_HAIL_CAP_DBZ));
                    vil += vil_inc(0.5 * (za_c + zb_c), dh);
                }
                if vil > 0.0 {
                    *cell = vil as f32;
                }
            }
        });
    Some(f32_field_like(
        base_field,
        "VIL",
        Quantity::Other,
        "kg m-2",
        out,
    ))
}

/// Maximum Expected Hail Size (mm) from the Severe Hail Index — the WSR-88D
/// Hail Detection Algorithm of Witt et al. 1998 (WAF 13(2), 286-303):
/// hail kinetic-energy flux  E = 5e-6 * 10^(0.084*Z) * W(Z)  with W(Z) a
/// linear ramp over 40-50 dBZ, height-weighted by a thermal ramp W_T(H)
/// between the melting level H0 and the -20C level, integrated upward:
/// SHI = 0.1 * sum W_T(H) * E * dH ;  MEHS = 2.54 * sqrt(SHI) (mm).
/// `freezing_level_m` / `minus20c_level_m` are heights above the RADAR (set
/// them from a sounding for best results; mid-latitude warm-season defaults
/// are roughly 3200 m / 6400 m).
/// MESH calibration: which SHI->size fit to apply.
///
/// References (constants adversarially verified against the corrigendum and
/// the pyhail reference implementation — see docs/hail-wind-algo-spec.md):
/// - Witt et al. 1998, Wea. Forecasting 13, 286-303
///   (doi:10.1175/1520-0434(1998)013<0286:AEHDAF>2.0.CO;2): MESH = 2.54*SHI^0.5.
///   Still what operational MRMS ships (Smith et al. 2016, BAMS 97).
/// - Murillo & Homeyer 2019, J. Appl. Meteor. Climatol. 58, 947-970
///   (doi:10.1175/JAMC-D-18-0247.1) refit on ~5,954 reports — WITH THE 2021
///   CORRIGENDUM COEFFICIENTS (doi:10.1175/JAMC-D-20-0271.1; the 2019 paper
///   text printed wrong values): P75 = 15.096*SHI^0.206, P95 = 22.157*SHI^0.212.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MeshCalibration {
    Witt1998,
    MurilloHomeyer2019P75,
    MurilloHomeyer2019P95,
}

impl MeshCalibration {
    #[inline]
    pub fn mesh_mm(self, shi: f64) -> f64 {
        match self {
            Self::Witt1998 => 2.54 * shi.sqrt(),
            Self::MurilloHomeyer2019P75 => 15.096 * shi.powf(0.206),
            Self::MurilloHomeyer2019P95 => 22.157 * shi.powf(0.212),
        }
    }
}

/// SHI + MESH + POSH in one column walk (Witt et al. 1998 Hail Detection
/// Algorithm). [`mehs`] remains as the Witt-calibrated MESH wrapper.
pub struct HailFields {
    /// Severe Hail Index (`SHI`), J m^-1 s^-1.
    pub shi: Field,
    /// Maximum Estimated Size of Hail (`MESH`), mm (per `MeshCalibration`).
    pub mesh_mm: Field,
    /// Probability of Severe Hail (`POSH`), percent (continuous 0-100;
    /// Witt's warning threshold WT = max(57.5*H0_km - 121, 20), POSH =
    /// 29*ln(SHI/WT) + 50 — SHI == WT gives exactly 50%).
    pub posh_pct: Field,
}

pub fn hail(
    volume: &Volume,
    freezing_level_m: f32,
    minus20c_level_m: f32,
    calibration: MeshCalibration,
) -> Option<HailFields> {
    let (base_sweep, base_field) = base_reflectivity_sweep(volume)?;
    let base = CutColumn::new(base_sweep, base_field)?;
    let cols = reflectivity_columns(volume);
    let (rows, gates) = base_field.shape();
    let mut shi_out = vec![f32::NAN; rows * gates];
    let mut mesh_out = vec![f32::NAN; rows * gates];
    let mut posh_out = vec![f32::NAN; rows * gates];
    let row_az = base.row_azimuths(rows);
    let h0 = freezing_level_m.max(0.0) as f64;
    let hm20 = (minus20c_level_m.max(freezing_level_m + 1.0)) as f64;
    // POSH warning threshold (Witt 1998; floor of 20 per the operational
    // WSR-88D documentation).
    let wt_thresh = (57.5 * h0 / 1000.0 - 121.0).max(20.0);
    let ke_flux = |dbz: f64| -> f64 {
        let w = ((dbz - 40.0) / 10.0).clamp(0.0, 1.0);
        if w <= 0.0 {
            0.0
        } else {
            5.0e-6 * 10f64.powf(0.084 * dbz) * w
        }
    };
    let wt = |h: f64| ((h - h0) / (hm20 - h0)).clamp(0.0, 1.0);
    shi_out
        .par_chunks_mut(gates)
        .zip(mesh_out.par_chunks_mut(gates))
        .zip(posh_out.par_chunks_mut(gates))
        .enumerate()
        .for_each(|(row, ((shi_row, mesh_row), posh_row))| {
            let az = row_az[row];
            if !az.is_finite() {
                return;
            }
            for gate in 0..gates {
                let s = base.ground_range_m[gate];
                let prof = column_profile(&cols, az, s);
                if prof.len() < 2 {
                    continue;
                }
                let mut shi = 0.0f64;
                for w in prof.windows(2) {
                    let (ha, za) = w[0];
                    let (hb, zb) = w[1];
                    let dh = (hb - ha).max(0.0);
                    if hb <= h0 || dh <= 0.0 {
                        continue;
                    }
                    let mid_h = 0.5 * (ha + hb);
                    let mid_e = 0.5 * (ke_flux(za as f64) + ke_flux(zb as f64));
                    shi += wt(mid_h) * mid_e * dh;
                }
                shi *= 0.1;
                if shi > 1.0 {
                    shi_row[gate] = shi as f32;
                    mesh_row[gate] = calibration.mesh_mm(shi) as f32;
                    let posh = (29.0 * (shi / wt_thresh).ln() + 50.0).clamp(0.0, 100.0);
                    if posh > 0.0 {
                        posh_row[gate] = posh as f32;
                    }
                }
            }
        });
    Some(HailFields {
        shi: f32_field_like(base_field, "SHI", Quantity::Other, "J m-1 s-1", shi_out),
        mesh_mm: f32_field_like(base_field, "MESH", Quantity::Other, "mm", mesh_out),
        posh_pct: f32_field_like(base_field, "POSH", Quantity::Other, "percent", posh_out),
    })
}

/// POH — Probability of Hail (any size): the Waldvogel, Federer & Grimm
/// (1979, J. Appl. Meteor. 18, 1521-1525) hailpad-validated curve on the
/// height of the 45 dBZ echo top above the melting level. Linear
/// interpolation between the published table rows.
pub fn poh(volume: &Volume, freezing_level_m: f32) -> Option<Field> {
    const TABLE: [(f64, f64); 11] = [
        (1.65, 0.0),
        (1.80, 10.0),
        (1.97, 20.0),
        (2.17, 30.0),
        (2.40, 40.0),
        (2.70, 50.0),
        (3.07, 60.0),
        (3.55, 70.0),
        (4.20, 80.0),
        (5.00, 90.0),
        (5.80, 100.0),
    ];
    let et45 = echo_top(volume, 45.0)?;
    let (rows, gates) = et45.shape();
    let mut out = vec![f32::NAN; rows * gates];
    let h0_km = freezing_level_m.max(0.0) as f64 / 1000.0;
    for row in 0..rows {
        for gate in 0..gates {
            let cell = &mut out[row * gates + gate];
            let Some(top_m) = et45.value(row, gate) else {
                continue;
            };
            if !top_m.is_finite() {
                continue;
            }
            let delta_km = top_m as f64 / 1000.0 - h0_km;
            if delta_km <= TABLE[0].0 {
                continue;
            }
            let poh = if delta_km >= TABLE[10].0 {
                100.0
            } else {
                let mut value = 0.0;
                for pair in TABLE.windows(2) {
                    let (d0, p0) = pair[0];
                    let (d1, p1) = pair[1];
                    if delta_km >= d0 && delta_km <= d1 {
                        value = p0 + (p1 - p0) * (delta_km - d0) / (d1 - d0);
                        break;
                    }
                }
                value
            };
            if poh > 0.0 {
                *cell = poh as f32;
            }
        }
    }
    Some(f32_field_like(
        &et45,
        "POH",
        Quantity::Other,
        "percent",
        out,
    ))
}

/// Witt-calibrated MESH (`MESH`, mm).
pub fn mehs(volume: &Volume, freezing_level_m: f32, minus20c_level_m: f32) -> Option<Field> {
    let (base_sweep, base_field) = base_reflectivity_sweep(volume)?;
    let base = CutColumn::new(base_sweep, base_field)?;
    let cols = reflectivity_columns(volume);
    let (rows, gates) = base_field.shape();
    let mut out = vec![f32::NAN; rows * gates];
    let row_az = base.row_azimuths(rows);
    let h0 = freezing_level_m.max(0.0) as f64;
    let hm20 = (minus20c_level_m.max(freezing_level_m + 1.0)) as f64;
    // Hail KE flux with the 40-50 dBZ reflectivity ramp (Witt eq. 4-5).
    let ke_flux = |dbz: f64| -> f64 {
        let w = ((dbz - 40.0) / 10.0).clamp(0.0, 1.0);
        if w <= 0.0 {
            0.0
        } else {
            5.0e-6 * 10f64.powf(0.084 * dbz) * w
        }
    };
    // Thermal weight between the melting level and -20C (Witt eq. 7).
    let wt = |h: f64| ((h - h0) / (hm20 - h0)).clamp(0.0, 1.0);
    out.par_chunks_mut(gates)
        .enumerate()
        .for_each(|(row, out_row)| {
            let az = row_az[row];
            if !az.is_finite() {
                return;
            }
            for (gate, cell) in out_row.iter_mut().enumerate() {
                let s = base.ground_range_m[gate];
                let prof = column_profile(&cols, az, s);
                if prof.len() < 2 {
                    continue;
                }
                let mut shi = 0.0f64;
                for w in prof.windows(2) {
                    let (ha, za) = w[0];
                    let (hb, zb) = w[1];
                    let dh = (hb - ha).max(0.0);
                    if hb <= h0 || dh <= 0.0 {
                        continue;
                    }
                    let mid_h = 0.5 * (ha + hb);
                    let mid_e = 0.5 * (ke_flux(za as f64) + ke_flux(zb as f64));
                    shi += wt(mid_h) * mid_e * dh;
                }
                shi *= 0.1;
                if shi > 1.0 {
                    // MEHS in mm (Witt eq. 11).
                    *cell = (2.54 * shi.sqrt()) as f32;
                }
            }
        });
    Some(f32_field_like(
        base_field,
        "MESH",
        Quantity::Other,
        "mm",
        out,
    ))
}

/// VIL Density (`VILD`, g m^-3) = VIL / echo-top depth — a depth-normalized
/// large-hail discriminator (values ≳ 3.5 g/m³ flag large hail far better
/// than raw VIL; Amburn & Wolf 1997, WAF 12(3)). Reuses the VIL and echo-top
/// fields (same base geometry); only computed where the echo top is
/// meaningfully deep.
pub fn vil_density(volume: &Volume) -> Option<Field> {
    let vil = self::vil(volume)?;
    let echo = echo_top(volume, ECHO_TOP_THRESHOLD_DBZ)?;
    let (rows, gates) = vil.shape();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            let (Some(v), Some(h)) = (vil.value(row, gate), echo.value(row, gate)) else {
                continue;
            };
            // Need a meaningful echo depth (>1.5 km) to avoid blow-ups.
            if v.is_finite() && h.is_finite() && h > 1_500.0 {
                out[row * gates + gate] = 1000.0 * v / h; // kg/m² ÷ m → g/m³
            }
        }
    }
    Some(f32_field_like(&vil, "VILD", Quantity::Other, "g m-3", out))
}

/// A reconstructed vertical cross-section: `values[y * width + x]` in dBZ
/// (NaN = no data), with `y = 0` at `top_m` and `x = 0` at the start point.
pub struct CrossSection {
    pub width: usize,
    pub height: usize,
    pub top_m: f32,
    pub length_m: f32,
    pub values: Vec<f32>,
}

/// Reflectivity vertical cross-section between two ground points given as
/// (east_km, north_km) from the radar. Resamples every reflectivity tilt along
/// the path with 4/3-Earth beam geometry (Doviak & Zrnić 1993) and linearly
/// interpolates in height between tilt samples — the standard RHI-from-volume
/// reconstruction used to see BWER/vault, overhang and descending cores.
pub fn reflectivity_section(
    volume: &Volume,
    start_km: (f32, f32),
    end_km: (f32, f32),
    width: usize,
    height: usize,
    top_m: f32,
) -> Option<CrossSection> {
    reflectivity_section_with_smoothing(
        volume,
        start_km,
        end_km,
        width,
        height,
        top_m,
        CrossSectionSmoothing::Smoothed,
    )
}

pub fn reflectivity_section_with_smoothing(
    volume: &Volume,
    start_km: (f32, f32),
    end_km: (f32, f32),
    width: usize,
    height: usize,
    top_m: f32,
    smoothing: CrossSectionSmoothing,
) -> Option<CrossSection> {
    let cols = reflectivity_columns(volume);
    cross_section_from_columns(
        &cols,
        start_km,
        end_km,
        (width, height),
        top_m,
        InterpPolicy::LinearAngle,
        smoothing,
    )
}

/// Cartesian box resample of the reflectivity volume for 3D direct
/// volume rendering: `n` cells per horizontal side over `±half_km` about
/// (center_east_km, center_north_km), `nz` levels 0..top_m. Returns
/// row-major \[z]\[y]\[x] values (NaN = no data), same MRMS-style
/// per-column reconstruction as the cross-sections.
pub fn box_resample(
    volume: &Volume,
    center_east_km: f32,
    center_north_km: f32,
    half_km: f32,
    n: usize,
    nz: usize,
    top_m: f32,
) -> Option<Vec<f32>> {
    box_resample_columns(
        reflectivity_columns(volume),
        InterpPolicy::LinearAngle,
        center_east_km,
        center_north_km,
        half_km,
        n,
        nz,
        top_m,
    )
}

/// [`box_resample`] of the field named `name` with `policy`.
#[allow(clippy::too_many_arguments)] // box geometry is explicit by design
pub fn box_resample_field(
    volume: &Volume,
    name: &FieldName,
    policy: InterpPolicy,
    center_east_km: f32,
    center_north_km: f32,
    half_km: f32,
    n: usize,
    nz: usize,
    top_m: f32,
) -> Option<Vec<f32>> {
    box_resample_columns(
        field_columns(volume, name),
        policy,
        center_east_km,
        center_north_km,
        half_km,
        n,
        nz,
        top_m,
    )
}

#[allow(clippy::too_many_arguments)] // box geometry is explicit by design
fn box_resample_columns(
    cols: Vec<CutColumn<'_>>,
    policy: InterpPolicy,
    center_east_km: f32,
    center_north_km: f32,
    half_km: f32,
    n: usize,
    nz: usize,
    top_m: f32,
) -> Option<Vec<f32>> {
    if n < 8 || nz < 4 || half_km <= 1.0 {
        return None;
    }
    if cols.is_empty() {
        return None;
    }
    let mut out = vec![f32::NAN; n * n * nz];
    let slabs: Vec<Vec<f32>> = (0..n)
        .into_par_iter()
        .map(|yi| {
            let mut slab = vec![f32::NAN; n * nz];
            let north = center_north_km - half_km + 2.0 * half_km * yi as f32 / (n - 1) as f32;
            for xi in 0..n {
                let east = center_east_km - half_km + 2.0 * half_km * xi as f32 / (n - 1) as f32;
                let s = f64::from(east.hypot(north)) * 1000.0;
                let az = east.atan2(north).to_degrees().rem_euclid(360.0);
                let prof = column_profile_xs(&cols, az, s);
                if prof.is_empty() {
                    continue;
                }
                for zi in 0..nz {
                    let z = f64::from(top_m) * zi as f64 / (nz - 1) as f64;
                    if let Some(v) = interp_profile_xs(&prof, z, s, policy) {
                        slab[zi * n + xi] = v;
                    }
                }
            }
            slab
        })
        .collect();
    for (yi, slab) in slabs.iter().enumerate() {
        for zi in 0..nz {
            for xi in 0..n {
                out[zi * n * n + yi * n + xi] = slab[zi * n + xi];
            }
        }
    }
    Some(out)
}

/// Generic single-field cross-section (CC, ZDR, …) of the field named
/// `name` in every sweep: same MRMS-style reconstruction with the field's
/// interpolation policy.
#[allow(clippy::too_many_arguments)] // section geometry is irreducibly 6 values
pub fn field_section(
    volume: &Volume,
    name: &FieldName,
    policy: InterpPolicy,
    start_km: (f32, f32),
    end_km: (f32, f32),
    width: usize,
    height: usize,
    top_m: f32,
) -> Option<CrossSection> {
    field_section_with_smoothing(
        volume,
        name,
        policy,
        start_km,
        end_km,
        width,
        height,
        top_m,
        CrossSectionSmoothing::Smoothed,
    )
}

#[allow(clippy::too_many_arguments)] // section geometry is irreducibly 6 values
pub fn field_section_with_smoothing(
    volume: &Volume,
    name: &FieldName,
    policy: InterpPolicy,
    start_km: (f32, f32),
    end_km: (f32, f32),
    width: usize,
    height: usize,
    top_m: f32,
    smoothing: CrossSectionSmoothing,
) -> Option<CrossSection> {
    let cols = field_columns(volume, name);
    cross_section_from_columns(
        &cols,
        start_km,
        end_km,
        (width, height),
        top_m,
        policy,
        smoothing,
    )
}

/// Dealiased-velocity vertical cross-section (m/s) — shows the RIJ descent /
/// downdraft and inflow/outflow vertical structure. Same RHI reconstruction as
/// the reflectivity section, but the columns sample each tilt's dealiased
/// velocity. NaN = no data.
pub fn velocity_section(
    volume: &Volume,
    start_km: (f32, f32),
    end_km: (f32, f32),
    width: usize,
    height: usize,
    top_m: f32,
) -> Option<CrossSection> {
    let mut cache = VolumeDealiasCache::new();
    velocity_section_cached(volume, &mut cache, start_km, end_km, width, height, top_m)
}

/// Per-volume memo of every tilt's dealiased velocity. Dealiasing all tilts
/// costs ~100+ ms; an interactive endpoint drag recomputes the section every
/// frame, so the dealias must be paid ONCE per volume, not per frame.
pub struct VolumeDealiasCache {
    volume_ptr: usize,
    fields: Vec<(usize, Field)>,
}

impl VolumeDealiasCache {
    pub fn new() -> Self {
        Self {
            volume_ptr: 0,
            fields: Vec::new(),
        }
    }

    fn ensure(&mut self, volume: &Volume) {
        let ptr = volume as *const Volume as usize;
        if ptr == self.volume_ptr && !self.fields.is_empty() {
            return;
        }
        self.volume_ptr = ptr;
        self.fields = volume
            .sweeps
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let v = s.find(Quantity::RadialVelocity)?;
                Some((i, dealias_velocity(s, v)))
            })
            .collect();
    }
}

impl Default for VolumeDealiasCache {
    fn default() -> Self {
        Self::new()
    }
}

/// [`velocity_section`] with a caller-held dealias memo — the fast path for
/// interactive section drags.
pub fn velocity_section_cached(
    volume: &Volume,
    cache: &mut VolumeDealiasCache,
    start_km: (f32, f32),
    end_km: (f32, f32),
    width: usize,
    height: usize,
    top_m: f32,
) -> Option<CrossSection> {
    velocity_section_cached_with_smoothing(
        volume,
        cache,
        start_km,
        end_km,
        width,
        height,
        top_m,
        CrossSectionSmoothing::Smoothed,
    )
}

#[allow(clippy::too_many_arguments)] // cache plus section geometry keeps this call site explicit
pub fn velocity_section_cached_with_smoothing(
    volume: &Volume,
    cache: &mut VolumeDealiasCache,
    start_km: (f32, f32),
    end_km: (f32, f32),
    width: usize,
    height: usize,
    top_m: f32,
    smoothing: CrossSectionSmoothing,
) -> Option<CrossSection> {
    cache.ensure(volume);
    let mut cols: Vec<CutColumn<'_>> = cache
        .fields
        .iter()
        .filter_map(|(i, f)| CutColumn::new(volume.sweeps.get(*i)?, f))
        .collect();
    cols.sort_by(|a, b| a.elevation_deg.total_cmp(&b.elevation_deg));
    cross_section_from_columns(
        &cols,
        start_km,
        end_km,
        (width, height),
        top_m,
        InterpPolicy::VelocityGuard,
        smoothing,
    )
}

/// Shared RHI reconstruction: walk along the ground path, sample each column,
/// and interpolate in height. Works for any moment's columns.
fn cross_section_from_columns(
    cols: &[CutColumn<'_>],
    start_km: (f32, f32),
    end_km: (f32, f32),
    dims: (usize, usize),
    top_m: f32,
    policy: InterpPolicy,
    smoothing: CrossSectionSmoothing,
) -> Option<CrossSection> {
    let (width, height) = dims;
    if width < 2 || height < 2 || top_m <= 0.0 || cols.is_empty() {
        return None;
    }
    let length_m = ((end_km.0 - start_km.0).hypot(end_km.1 - start_km.1) * 1000.0).max(0.0);
    // Columns are independent — compute them in parallel (keeps endpoint
    // drags fluid), then transpose into the row-major grid.
    let columns: Vec<Vec<f32>> = (0..width)
        .into_par_iter()
        .map(|x| {
            let f = x as f32 / (width - 1) as f32;
            let east = start_km.0 + (end_km.0 - start_km.0) * f;
            let north = start_km.1 + (end_km.1 - start_km.1) * f;
            let s = east.hypot(north) as f64 * 1000.0;
            let az = east.atan2(north).to_degrees().rem_euclid(360.0);
            let prof = column_profile_xs(cols, az, s);
            let mut column = vec![f32::NAN; height];
            if prof.is_empty() {
                return column;
            }
            for (y, cell) in column.iter_mut().enumerate() {
                let z = top_m * (1.0 - y as f32 / (height - 1) as f32);
                if let Some(v) = interp_profile_xs(&prof, z as f64, s, policy) {
                    *cell = v;
                }
            }
            column
        })
        .collect();
    let mut values = vec![f32::NAN; width * height];
    for (x, column) in columns.iter().enumerate() {
        for (y, v) in column.iter().enumerate() {
            values[y * width + x] = *v;
        }
    }
    if smoothing == CrossSectionSmoothing::Native {
        return Some(CrossSection {
            width,
            height,
            top_m,
            length_m,
            values,
        });
    }
    // Path-sampling cleanup. Each column samples ONE nearest radial/gate, so
    // (a) some columns miss entirely (azimuth/gate gaps -> NaN stripes) and
    // (b) adjacent columns can disagree gate-to-gate ("barcode"). Two
    // NaN-aware passes fix both without touching heights: fill short gaps
    // (<= 2 columns) from horizontal neighbors, then a 3-tap blend — the same
    // smoothing every operational RHI display applies.
    let mut filled = values.clone();
    for y in 0..height {
        let row = y * width;
        for x in 0..width {
            if values[row + x].is_finite() {
                continue;
            }
            let mut sum = 0.0f32;
            let mut n = 0.0f32;
            for dx in [-2isize, -1, 1, 2] {
                let xi = x as isize + dx;
                if xi < 0 || xi >= width as isize {
                    continue;
                }
                let v = values[row + xi as usize];
                if v.is_finite() {
                    sum += v;
                    n += 1.0;
                }
            }
            if n >= 2.0 {
                filled[row + x] = sum / n;
            }
        }
    }
    let mut smoothed = filled.clone();
    for y in 0..height {
        let row = y * width;
        for x in 0..width {
            if !filled[row + x].is_finite() {
                continue;
            }
            let mut sum = 0.0f32;
            let mut n = 0.0f32;
            for dx in [-1isize, 0, 1] {
                let xi = x as isize + dx;
                if xi < 0 || xi >= width as isize {
                    continue;
                }
                let v = filled[row + xi as usize];
                if v.is_finite() {
                    sum += v;
                    n += 1.0;
                }
            }
            if n > 0.0 {
                smoothed[row + x] = sum / n;
            }
        }
    }
    Some(CrossSection {
        width,
        height,
        top_m,
        length_m,
        values: smoothed,
    })
}
