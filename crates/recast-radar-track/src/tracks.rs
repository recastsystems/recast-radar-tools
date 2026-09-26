//! TOR TRACKS — rotation tracks and tornado-debris-signature (TDS) flags.
//!
//! **Rotation tracks** accumulate the per-cell MAXIMUM low-level cyclonic
//! azimuthal shear over a frame sequence on a fixed radar-centered Cartesian
//! grid — the swath a translating mesocyclone paints. This is the
//! single-radar analogue of the MRMS rotation-tracks product family:
//!
//! - Mahalik et al. 2019, *Estimates of Gradients in Radar Moments Using a
//!   Linear Least Squares Derivative Technique*, Wea. Forecasting 34,
//!   1423–1447, doi:10.1175/WAF-D-18-0165.1 — the operational LLSD
//!   azimuthal-shear formulation and the 0–2 km AGL "low-level" layer.
//! - Miller et al. 2013, *A multi-sensor severe weather nowcast system using
//!   rotation tracks*, 28th Conf. IIPS, AMS — the rotation-tracks
//!   time-accumulation (running max) and its reflectivity-based QC.
//! - Smith et al. 2016, *Multi-Radar Multi-Sensor (MRMS) Severe Weather and
//!   Aviation Products*, BAMS 97, 1617–1630, doi:10.1175/BAMS-D-14-00173.1 —
//!   the MRMS product lineage (0.005° ≈ 500 m grid; we default to 500 m).
//!
//! Sign convention: the LLSD azimuthal shear of
//! `recast_radar_retrieve::azimuthal_shear_grid` is positive for
//! Northern-Hemisphere cyclonic rotation. Rotation tracks accumulate
//! **cyclonic shear only** (positive values), matching the operational MDA /
//! MRMS rotation-track convention; anticyclonic shear (outflow flanks,
//! anticyclonic members of vortex couplets) is intentionally excluded.
//!
//! **TDS flags** are a deterministic dual-pol physics criterion — NOT a
//! probability: low co-polar correlation inside real echo, co-located with a
//! rank-significant detected circulation, at the lowest dual-pol tilt:
//!
//! - Ryzhkov et al. 2005, *Polarimetric Tornado Detection*, J. Appl. Meteor.
//!   44, 557–570 — the polarimetric TDS (lofted debris: low ρhv, high Z,
//!   co-located with the vortex).
//! - Van Den Broeke & Jauernic 2014, *Spatial and Temporal Characteristics of
//!   Polarimetric Tornadic Debris Signatures*, J. Appl. Meteor. Climatol. 53,
//!   2217–2231 — operational criteria bracket (ρhv ≲ 0.82, Z ≳ 30 dBZ near
//!   the circulation center).
//! - Snyder & Ryzhkov 2015, *Automated Detection of Polarimetric Tornadic
//!   Debris Signatures Using a Hydrometeor Classification Algorithm*,
//!   J. Appl. Meteor. Climatol. 54, 1861–1870 — debris as a deterministic
//!   class from the same predictors.

use std::collections::HashSet;

use recast_radar_core::{Field, Quantity, Sweep, Volume, beam_height_above_radar_m};

use recast_radar_correct::dealias_velocity;
use recast_radar_retrieve::{RotationSite, RotationStrength, azimuthal_shear_from_dealiased};

/// Top of the "low-level" layer (m above radar level, used as the AGL proxy —
/// the WSR-88D feedhorn sits only tens of meters above ground). Mahalik et
/// al. 2019 define the operational low-level azimuthal-shear product over
/// 0–2 km AGL; with a single radar the 0.5° beam exits this layer near
/// ~125 km range, which bounds the swath's range coverage.
const LOW_LEVEL_TOP_M: f64 = 2_000.0;
/// Tilts feeding the low-level composite: the lowest velocity-bearing
/// elevations (≤ 2.0°, at most 3 — e.g. 0.5/0.9/1.3 on VCP 12/212). Higher
/// tilts only contribute very close to the radar before they leave the
/// 0–2 km layer.
const LOW_LEVEL_MAX_TILT_DEG: f32 = 2.0;
const MAX_LOW_TILTS: usize = 3;
/// Range window. The lower bound excludes the clutter/sidelobe zone around
/// the radar (same engineering floor as the MDA port in
/// `recast_radar_retrieve`'s rotation detection):
/// near-field clutter makes spurious LLSD shear that a running max would
/// paint permanently.
const TRACKS_MIN_RANGE_M: f64 = 5_000.0;
/// Reflectivity QC floor (dBZ): azimuthal shear is only accumulated inside
/// real echo, per the MRMS rotation-track QC practice (Miller et al. 2013).
/// Clear-air boundary-layer shear noise must never paint the swath.
const TRACKS_REFLECTIVITY_FLOOR_DBZ: f32 = 20.0;
/// Implausible-shear cap (clutter residue), ×10⁻³ s⁻¹.
const MAX_PLAUSIBLE_SHEAR_E3: f32 = 150.0;
/// Impulse rejection: every accumulated gate is capped at its 3×3
/// neighborhood median (rotation-track QC practice, Miller et al. 2013; the
/// KMKX 2026-06-11 QLCS validation showed single-gate dealias residue
/// reaching 0.1+ s⁻¹ that a running max would keep forever, while a reject
/// threshold alone left heavy speckle). A couplet's LLSD ridge spans several
/// radials — the LLSD window itself smooths it — so the median cap preserves
/// real swaths and kills 1–2-gate spikes.
const MEDIAN_CAP_MIN_NEIGHBORS: usize = 5;
/// Azimuth lookup resolution (0.25°/bin covers super-res 0.5° radials).
const AZ_BINS: usize = 1440;

/// Display ramp window, ×10⁻³ s⁻¹: transparent below 0.003 s⁻¹, saturating
/// magenta at 0.02 s⁻¹ (brackets the strong-mesocyclone azimuthal-shear range
/// of the MRMS rotation-track display; Mahalik et al. 2019).
pub const TRACK_DISPLAY_FLOOR_E3: f32 = 3.0;
/// Top of the display ramp, ×10⁻³ s⁻¹ (0.02 s⁻¹).
pub const TRACK_DISPLAY_CEIL_E3: f32 = 20.0;

/// TDS criteria (Ryzhkov et al. 2005; Van Den Broeke & Jauernic 2014):
/// ρhv below 0.82 inside ≥ 30 dBZ echo, within 5 km of a detected
/// circulation of 3-D rank ≥ 3 (or TVS). The rank anchor is the co-located
/// "significant azimuthal shear" requirement — debris-like dual-pol values
/// without a vortex (hail cores, biota) must not flag.
pub const TDS_CC_MAX: f32 = 0.82;
/// TDS criterion: the gate's reflectivity is at least this, dBZ.
pub const TDS_MIN_DBZ: f32 = 30.0;
/// TDS criterion: the gate lies within this distance of a detected circulation, km.
pub const TDS_ANCHOR_RADIUS_KM: f64 = 5.0;
/// TDS criterion: that circulation has at least this 3-D rank.
pub const TDS_ANCHOR_MIN_RANK: u8 = 3;
/// Debris is a low-level signature (median TDS heights are well below
/// 1.5 km AGL; Van Den Broeke & Jauernic 2014) — gates whose beam center
/// sits above 3 km ARL never flag, whatever the anchor says.
const TDS_MAX_BEAM_HEIGHT_M: f64 = 3_000.0;
const MAX_TDS_GATES: usize = 4_096;

/// Geometry of the radar-centered accumulation grid: a square of
/// `size() × size()` cells in planar ENU km about the radar, row 0 at the
/// north edge (matches the radar raster's planar geometry, so the same
/// AEQD placement applies).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TracksGridSpec {
    /// Half the side of the square grid, km.
    pub half_extent_km: f32,
    /// Side of one grid cell, km.
    pub cell_km: f32,
}

impl Default for TracksGridSpec {
    fn default() -> Self {
        // ±150 km at 500 m ≈ the MRMS 0.005° rotation-track grid (Smith et
        // al. 2016); 150 km also brackets where the 0.5° beam has long left
        // the 0–2 km layer.
        Self {
            half_extent_km: 150.0,
            cell_km: 0.5,
        }
    }
}

impl TracksGridSpec {
    /// Cells per side.
    pub fn size(&self) -> usize {
        ((2.0 * self.half_extent_km / self.cell_km).round() as usize).max(1)
    }

    /// Total cell count (`size²`).
    pub fn cell_count(&self) -> usize {
        self.size() * self.size()
    }

    /// ENU km of a cell center; row 0 = north edge, column 0 = west edge.
    pub fn cell_center_km(&self, column: usize, row: usize) -> (f32, f32) {
        let east = (column as f32 + 0.5) * self.cell_km - self.half_extent_km;
        let north = self.half_extent_km - (row as f32 + 0.5) * self.cell_km;
        (east, north)
    }

    /// Flat cell index containing an ENU point, if inside the grid.
    pub fn cell_index(&self, east_km: f32, north_km: f32) -> Option<usize> {
        let size = self.size();
        let column = ((east_km + self.half_extent_km) / self.cell_km).floor();
        let row = ((self.half_extent_km - north_km) / self.cell_km).floor();
        if column < 0.0 || row < 0.0 {
            return None;
        }
        let (column, row) = (column as usize, row as usize);
        if column >= size || row >= size {
            return None;
        }
        Some(row * size + column)
    }
}

/// One tilt's QC'd LLSD azimuthal shear with the lookup tables needed for
/// pull-resampling onto the Cartesian grid.
struct TiltShearField {
    /// ×10⁻³ s⁻¹; NaN = no data / QC-rejected; 0 = data, no rotation signal.
    shear: Vec<f32>,
    gates: usize,
    first_gate_m: f64,
    spacing_m: f64,
    /// Azimuth bin (0.25°) → grid row, `usize::MAX` when empty.
    az_row: Vec<usize>,
    min_gate: usize,
    /// Last gate whose beam center is still inside the 0–2 km layer.
    max_gate: usize,
}

/// Ordered, bounded sweep set consumed by the low-level track composite:
/// ordered by tilt elevation ([`recast_radar_core::Sweep::tilt_elevation_deg`]),
/// ties in acquisition order.
pub fn low_level_azshear_sweep_indices(volume: &Volume) -> Vec<usize> {
    let source = volume.provenance.source_format;
    let mut velocity_sweeps: Vec<(usize, f32)> = volume
        .sweeps
        .iter()
        .enumerate()
        .map(|(index, sweep)| (index, sweep, sweep.tilt_elevation_deg(source)))
        .filter(|(_, sweep, elevation)| {
            sweep.find(Quantity::RadialVelocity).is_some() && *elevation <= LOW_LEVEL_MAX_TILT_DEG
        })
        .map(|(index, _, elevation)| (index, elevation))
        .collect();
    velocity_sweeps.sort_by(|left, right| left.1.total_cmp(&right.1));
    velocity_sweeps.truncate(MAX_LOW_TILTS);
    velocity_sweeps
        .into_iter()
        .map(|(index, _)| index)
        .collect()
}

/// Resample one volume's low-level (0–2 km ARL) cyclonic azimuthal shear onto
/// a radar-centered Cartesian grid (×10⁻³ s⁻¹; NaN = no coverage). Each cell
/// pull-samples its nearest gate on every contributing tilt and keeps the
/// maximum — one frame of the rotation-tracks accumulation.
pub fn low_level_azshear_cartesian(volume: &Volume, spec: &TracksGridSpec) -> Vec<f32> {
    let mut owned = vec![None; volume.sweeps.len()];
    for sweep_index in low_level_azshear_sweep_indices(volume) {
        let sweep = &volume.sweeps[sweep_index];
        owned[sweep_index] = sweep
            .find(Quantity::RadialVelocity)
            .map(|velocity| dealias_velocity(sweep, velocity));
    }
    let borrowed: Vec<Option<&Field>> = owned.iter().map(Option::as_ref).collect();
    low_level_azshear_cartesian_from_dealiased(volume, &borrowed, spec)
}

/// Resample low-level shear from caller-provided dealiased velocity fields.
/// The slice is indexed like `volume.sweeps`, each entry a field on that
/// sweep's rays and range; no dealias engine runs here.
pub fn low_level_azshear_cartesian_from_dealiased(
    volume: &Volume,
    dealiased_velocity: &[Option<&Field>],
    spec: &TracksGridSpec,
) -> Vec<f32> {
    let fields = low_level_tilt_fields_from_dealiased(volume, dealiased_velocity);
    let size = spec.size();
    let mut out = vec![f32::NAN; spec.cell_count()];
    if fields.is_empty() {
        return out;
    }
    for row in 0..size {
        for column in 0..size {
            let (east, north) = spec.cell_center_km(column, row);
            let range_m = (east as f64).hypot(north as f64) * 1000.0;
            if range_m < TRACKS_MIN_RANGE_M {
                continue;
            }
            let az_deg = (east as f64).atan2(north as f64).to_degrees();
            let bin = ((az_deg.rem_euclid(360.0)) * (AZ_BINS as f64 / 360.0)) as usize % AZ_BINS;
            let mut best = f32::NAN;
            for field in &fields {
                let grid_row = field.az_row[bin];
                if grid_row == usize::MAX {
                    continue;
                }
                let gate = ((range_m - field.first_gate_m) / field.spacing_m).round();
                if gate < field.min_gate as f64 || gate > field.max_gate as f64 {
                    continue;
                }
                let value = field.shear[grid_row * field.gates + gate as usize];
                if value.is_finite() && (best.is_nan() || value > best) {
                    best = value;
                }
            }
            out[row * size + column] = best;
        }
    }
    out
}

/// Running max-composite: fold one frame into the accumulator. NaN cells in
/// the frame leave the accumulator untouched; finite values replace NaN or
/// smaller accumulated values (the rotation-tracks accumulation operator;
/// Miller et al. 2013).
pub fn max_composite_into(accumulator: &mut [f32], frame: &[f32]) {
    debug_assert_eq!(accumulator.len(), frame.len());
    for (acc, &value) in accumulator.iter_mut().zip(frame.iter()) {
        if value.is_finite() && (acc.is_nan() || value > *acc) {
            *acc = value;
        }
    }
}

/// Rotation-tracks display ramp (RGBA, non-premultiplied): transparent below
/// [`TRACK_DISPLAY_FLOOR_E3`], blue → yellow → red → magenta saturating at
/// [`TRACK_DISPLAY_CEIL_E3`] (0.003–0.02 s⁻¹).
pub fn rotation_track_color(shear_e3: f32) -> [u8; 4] {
    if !shear_e3.is_finite() || shear_e3 < TRACK_DISPLAY_FLOOR_E3 {
        return [0, 0, 0, 0];
    }
    // (threshold ×10⁻³ s⁻¹, r, g, b, a)
    const STOPS: [(f32, f32, f32, f32, f32); 4] = [
        (TRACK_DISPLAY_FLOOR_E3, 35.0, 70.0, 220.0, 150.0),
        (8.0, 255.0, 230.0, 70.0, 205.0),
        (14.0, 235.0, 35.0, 35.0, 235.0),
        (TRACK_DISPLAY_CEIL_E3, 255.0, 40.0, 255.0, 255.0),
    ];
    let last = STOPS[STOPS.len() - 1];
    if shear_e3 >= last.0 {
        return [last.1 as u8, last.2 as u8, last.3 as u8, last.4 as u8];
    }
    for pair in STOPS.windows(2) {
        let (lo, hi) = (pair[0], pair[1]);
        if shear_e3 < hi.0 {
            let t = ((shear_e3 - lo.0) / (hi.0 - lo.0)).clamp(0.0, 1.0);
            let lerp = |a: f32, b: f32| (a + (b - a) * t).round() as u8;
            return [
                lerp(lo.1, hi.1),
                lerp(lo.2, hi.2),
                lerp(lo.3, hi.3),
                lerp(lo.4, hi.4),
            ];
        }
    }
    [last.1 as u8, last.2 as u8, last.3 as u8, last.4 as u8]
}

/// One TDS-flagged gate (planar ENU km about the radar).
#[derive(Clone, Copy, Debug)]
pub struct TdsGate {
    /// Km east of the radar.
    pub east_km: f32,
    /// Km north of the radar.
    pub north_km: f32,
    /// Correlation coefficient of the gate.
    pub cc: f32,
    /// Reflectivity of the gate, dBZ.
    pub dbz: f32,
}

/// The per-gate dual-pol debris criterion (Ryzhkov et al. 2005; Van Den
/// Broeke & Jauernic 2014): ρhv < 0.82 inside > 30 dBZ echo. Co-location
/// with a significant circulation is enforced separately in
/// [`detect_tds_gates`].
pub fn tds_gate_criteria(cc: f32, dbz: f32) -> bool {
    cc.is_finite() && dbz.is_finite() && cc < TDS_CC_MAX && dbz > TDS_MIN_DBZ
}

/// Whether a detected circulation can anchor TDS gates: 3-D rank ≥ 3
/// (moderate circulation) or TVS — the "co-located significant azimuthal
/// shear" requirement of the TDS literature, reusing the MDA/TDA port of
/// `recast_radar_retrieve`'s rotation detection.
pub fn tds_anchor(site: &RotationSite) -> bool {
    site.strength == RotationStrength::Tvs || site.rank >= TDS_ANCHOR_MIN_RANK
}

/// Per-gate TDS flags on the lowest dual-pol tilt: every gate within
/// [`TDS_ANCHOR_RADIUS_KM`] of an anchoring circulation that satisfies
/// [`tds_gate_criteria`]. Deterministic physics flag — never a probability.
pub fn detect_tds_gates(volume: &Volume, sites: &[RotationSite]) -> Vec<TdsGate> {
    let anchors: Vec<(f64, f64)> = sites
        .iter()
        .filter(|site| tds_anchor(site))
        .map(|site| {
            let az = (site.azimuth_deg as f64).to_radians();
            let range_km = site.ground_range_m / 1000.0;
            (range_km * az.sin(), range_km * az.cos())
        })
        .collect();
    if anchors.is_empty() {
        return Vec::new();
    }

    // Lowest tilt carrying both ρhv and Z (the split-cut surveillance tilt).
    let source = volume.provenance.source_format;
    let Some((sweep, tilt_elevation_deg)) = volume
        .sweeps
        .iter()
        .map(|sweep| (sweep, sweep.tilt_elevation_deg(source)))
        .filter(|(sweep, elevation)| {
            *elevation <= LOW_LEVEL_MAX_TILT_DEG
                && sweep.find(Quantity::CorrelationCoefficient).is_some()
                && sweep.find(Quantity::Reflectivity).is_some()
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
    else {
        return Vec::new();
    };
    let Some(cc_field) = sweep.find(Quantity::CorrelationCoefficient) else {
        return Vec::new();
    };
    let Some(ref_field) = sweep.find(Quantity::Reflectivity) else {
        return Vec::new();
    };
    let Some(ref_sampler) = RangeSampler::new(sweep, ref_field) else {
        return Vec::new();
    };
    let elevation = f64::from(tilt_elevation_deg);
    let Some((first_gate_m, spacing_m)) = cc_field.native_geometry(&sweep.range) else {
        return Vec::new();
    };
    let spacing_m = spacing_m.max(1.0);
    let gate_count = cc_field.ngates as usize;

    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut out = Vec::new();
    'rows: for row in 0..cc_field.nrays as usize {
        let Some(azimuth) = sweep.rays.azimuth_deg.get(row) else {
            continue;
        };
        let az = (*azimuth as f64).to_radians();
        let (ux, uy) = (az.sin(), az.cos());
        for &(anchor_east, anchor_north) in &anchors {
            // Closest approach of this ray to the anchor; the ray intersects
            // the 5-km disc over [t* − s, t* + s].
            let along_km = anchor_east * ux + anchor_north * uy;
            let perp_km = (anchor_east * uy - anchor_north * ux).abs();
            if along_km <= 0.0 || perp_km > TDS_ANCHOR_RADIUS_KM {
                continue;
            }
            let half_span_km =
                (TDS_ANCHOR_RADIUS_KM * TDS_ANCHOR_RADIUS_KM - perp_km * perp_km).sqrt();
            let range_lo_m = ((along_km - half_span_km) * 1000.0).max(TRACKS_MIN_RANGE_M);
            let range_hi_m = (along_km + half_span_km) * 1000.0;
            let gate_lo = ((range_lo_m - first_gate_m) / spacing_m).ceil().max(0.0) as usize;
            let gate_hi = ((range_hi_m - first_gate_m) / spacing_m).floor();
            if gate_hi < 0.0 {
                continue;
            }
            let gate_hi = (gate_hi as usize).min(gate_count.saturating_sub(1));
            for gate in gate_lo..=gate_hi {
                if seen.contains(&(row, gate)) {
                    continue;
                }
                let range_m = first_gate_m + gate as f64 * spacing_m;
                if beam_height_above_radar_m(range_m, elevation) > TDS_MAX_BEAM_HEIGHT_M {
                    break;
                }
                let Some(cc) = cc_field.value(row, gate).filter(|v| v.is_finite()) else {
                    continue;
                };
                let Some(dbz) = ref_sampler.sample(row, range_m) else {
                    continue;
                };
                if !tds_gate_criteria(cc, dbz) {
                    continue;
                }
                seen.insert((row, gate));
                out.push(TdsGate {
                    east_km: (range_m / 1000.0 * az.sin()) as f32,
                    north_km: (range_m / 1000.0 * az.cos()) as f32,
                    cc,
                    dbz,
                });
                if out.len() >= MAX_TDS_GATES {
                    break 'rows;
                }
            }
        }
    }
    out
}

/// Build the QC'd per-tilt shear fields feeding the Cartesian resample.
fn low_level_tilt_fields_from_dealiased(
    volume: &Volume,
    dealiased_velocity: &[Option<&Field>],
) -> Vec<TiltShearField> {
    let velocity_sweeps = low_level_azshear_sweep_indices(volume);

    let mut fields = Vec::new();
    for sweep_index in velocity_sweeps {
        let sweep = &volume.sweeps[sweep_index];
        let Some(velocity) = dealiased_velocity.get(sweep_index).copied().flatten() else {
            continue;
        };
        let shear = azimuthal_shear_from_dealiased(sweep, velocity);
        let (rows, gates) = shear.shape();
        if rows == 0 || gates == 0 {
            continue;
        }
        let Some((first_gate_m, spacing_m)) = shear.native_geometry(&sweep.range) else {
            continue;
        };
        let spacing_m = spacing_m.max(1.0);
        let elevation = f64::from(sweep.tilt_elevation_deg(volume.provenance.source_format));

        // Range window: clutter floor up to where the beam exits 0–2 km.
        let min_gate = ((TRACKS_MIN_RANGE_M - first_gate_m) / spacing_m)
            .ceil()
            .max(0.0) as usize;
        let mut max_gate = None;
        for gate in (min_gate..gates).rev() {
            let range_m = first_gate_m + gate as f64 * spacing_m;
            if beam_height_above_radar_m(range_m, elevation) <= LOW_LEVEL_TOP_M {
                max_gate = Some(gate);
                break;
            }
        }
        let Some(max_gate) = max_gate else {
            continue;
        };
        if min_gate > max_gate {
            continue;
        }

        let reflectivity = sweep
            .find(Quantity::Reflectivity)
            .and_then(|field| RangeSampler::new(sweep, field));

        let shear_at = |row: usize, gate: usize| -> Option<f32> {
            shear.value(row, gate).filter(|v| v.is_finite())
        };
        let mut qc = vec![f32::NAN; rows * gates];
        for row in 0..rows {
            for gate in min_gate..=max_gate {
                let Some(value) = shear_at(row, gate) else {
                    continue;
                };
                // Cyclonic only; clutter-residue cap.
                if value > MAX_PLAUSIBLE_SHEAR_E3 {
                    continue;
                }
                if value < 0.0 {
                    qc[row * gates + gate] = 0.0;
                    continue;
                }
                // Reflectivity QC: shear only accumulates inside real echo.
                let range_m = first_gate_m + gate as f64 * spacing_m;
                let dbz = reflectivity
                    .as_ref()
                    .and_then(|sampler| sampler.sample(row, range_m));
                if !dbz.is_some_and(|v| v >= TRACKS_REFLECTIVITY_FLOOR_DBZ) {
                    continue;
                }
                // Impulse rejection: cap at the 3×3 neighborhood median. A
                // real couplet's LLSD ridge is spatially coherent (the LLSD
                // window itself smooths it), so min(value, median) preserves
                // swaths while 1–2-gate dealias spikes collapse to their
                // quiet surroundings.
                let mut neighborhood = [0.0f32; 9];
                let mut count = 0usize;
                for dr in -1i64..=1 {
                    for dg in -1i64..=1 {
                        let r = ((row as i64 + dr).rem_euclid(rows as i64)) as usize;
                        let g = gate as i64 + dg;
                        if g < 0 || g >= gates as i64 {
                            continue;
                        }
                        if let Some(v) = shear_at(r, g as usize) {
                            neighborhood[count] = v;
                            count += 1;
                        }
                    }
                }
                if count < MEDIAN_CAP_MIN_NEIGHBORS {
                    continue;
                }
                neighborhood[..count].sort_by(f32::total_cmp);
                qc[row * gates + gate] = value.min(neighborhood[count / 2].max(0.0));
            }
        }

        // Azimuth → row lookup with nearest-fill (the detect.rs pattern).
        let mut az_row = vec![usize::MAX; AZ_BINS];
        for row in 0..rows {
            if let Some(azimuth) = sweep.rays.azimuth_deg.get(row) {
                let bin =
                    ((azimuth.rem_euclid(360.0)) * (AZ_BINS as f32 / 360.0)) as usize % AZ_BINS;
                az_row[bin] = row;
            }
        }
        let filled: Vec<usize> = (0..AZ_BINS)
            .map(|bin| {
                (0..4)
                    .flat_map(|step| [(bin + step) % AZ_BINS, (bin + AZ_BINS - step) % AZ_BINS])
                    .map(|b| az_row[b])
                    .find(|&row| row != usize::MAX)
                    .unwrap_or(usize::MAX)
            })
            .collect();

        fields.push(TiltShearField {
            shear: qc,
            gates,
            first_gate_m,
            spacing_m,
            az_row: filled,
            min_gate,
            max_gate,
        });
    }
    fields
}

/// Samples a field of a sweep by (ray, physical range) at the nearest native
/// gate. Every field of a sweep is on the sweep's rays, so row `r` of the
/// shear field and row `r` of the reflectivity are the same ray.
struct RangeSampler<'a> {
    field: &'a Field,
    first_gate_m: f64,
    spacing_m: f64,
}

impl<'a> RangeSampler<'a> {
    fn new(sweep: &Sweep, field: &'a Field) -> Option<Self> {
        let (first_gate_m, spacing_m) = field.native_geometry(&sweep.range)?;
        Some(Self {
            field,
            first_gate_m,
            spacing_m: spacing_m.max(1.0),
        })
    }

    fn sample(&self, row: usize, range_m: f64) -> Option<f32> {
        let gate = ((range_m - self.first_gate_m) / self.spacing_m).round();
        if gate < 0.0 || gate as usize >= self.field.ngates as usize {
            return None;
        }
        self.field
            .value(row, gate as usize)
            .filter(|v| v.is_finite())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_composite_keeps_running_maximum() {
        let mut acc = vec![f32::NAN, 2.0, 5.0, f32::NAN];
        max_composite_into(&mut acc, &[1.0, f32::NAN, 3.0, f32::NAN]);
        assert_eq!(acc[0], 1.0, "finite frame value replaces NaN");
        assert_eq!(acc[1], 2.0, "NaN frame cell leaves accumulator");
        assert_eq!(acc[2], 5.0, "smaller frame value never lowers the max");
        assert!(acc[3].is_nan(), "NaN over NaN stays NaN");
        max_composite_into(&mut acc, &[4.0, 9.0, 6.0, 0.0]);
        assert_eq!(acc, vec![4.0, 9.0, 6.0, 0.0]);
    }

    #[test]
    fn grid_spec_round_trips_cells() {
        let spec = TracksGridSpec {
            half_extent_km: 30.0,
            cell_km: 0.5,
        };
        assert_eq!(spec.size(), 120);
        for &(column, row) in &[(0usize, 0usize), (59, 60), (119, 119), (3, 117)] {
            let (east, north) = spec.cell_center_km(column, row);
            assert_eq!(
                spec.cell_index(east, north),
                Some(row * spec.size() + column),
                "cell ({column},{row}) center must map back to itself"
            );
        }
        // A gate at az 90°, 20 km lands in the cell containing (east 20, north 0).
        let (az, range_km) = (90.0f64.to_radians(), 20.0f64);
        let east = (range_km * az.sin()) as f32;
        let north = (range_km * az.cos()) as f32;
        let index = spec.cell_index(east, north).expect("inside grid");
        let (ce, cn) = spec.cell_center_km(index % spec.size(), index / spec.size());
        assert!((ce - east).abs() <= spec.cell_km * 0.5 + 1e-4);
        assert!((cn - north).abs() <= spec.cell_km * 0.5 + 1e-4);
        // Outside the square → None.
        assert_eq!(spec.cell_index(30.4, 0.0), None);
        assert_eq!(spec.cell_index(0.0, -30.4), None);
    }

    #[test]
    fn tds_criteria_thresholds() {
        assert!(tds_gate_criteria(0.70, 45.0), "classic debris values flag");
        assert!(!tds_gate_criteria(0.95, 45.0), "rain-grade ρhv never flags");
        assert!(!tds_gate_criteria(0.70, 12.0), "weak echo never flags");
        assert!(!tds_gate_criteria(f32::NAN, 45.0));
        assert!(!tds_gate_criteria(0.70, f32::NAN));
        assert!(tds_gate_criteria(TDS_CC_MAX - 0.001, TDS_MIN_DBZ + 0.1));
        assert!(!tds_gate_criteria(TDS_CC_MAX, TDS_MIN_DBZ + 0.1));
        assert!(!tds_gate_criteria(TDS_CC_MAX - 0.001, TDS_MIN_DBZ));
    }

    #[test]
    fn rotation_track_ramp_endpoints() {
        assert_eq!(rotation_track_color(f32::NAN)[3], 0);
        assert_eq!(rotation_track_color(0.0)[3], 0);
        assert_eq!(rotation_track_color(TRACK_DISPLAY_FLOOR_E3 - 0.01)[3], 0);
        let floor = rotation_track_color(TRACK_DISPLAY_FLOOR_E3);
        assert_eq!(floor, [35, 70, 220, 150], "floor is translucent blue");
        let ceil = rotation_track_color(TRACK_DISPLAY_CEIL_E3);
        assert_eq!(ceil, [255, 40, 255, 255], "ceiling is opaque magenta");
        assert_eq!(
            rotation_track_color(99.0),
            ceil,
            "values above the ceiling clamp"
        );
        // Monotone alpha across the ramp.
        let mut last_alpha = 0u8;
        for i in 0..=40 {
            let v = TRACK_DISPLAY_FLOOR_E3
                + (TRACK_DISPLAY_CEIL_E3 - TRACK_DISPLAY_FLOOR_E3) * (i as f32 / 40.0);
            let alpha = rotation_track_color(v)[3];
            assert!(
                alpha >= last_alpha,
                "alpha must not decrease along the ramp"
            );
            last_alpha = alpha;
        }
    }
}
