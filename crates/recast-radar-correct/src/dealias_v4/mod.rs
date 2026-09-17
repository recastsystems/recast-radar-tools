//! Dealias v4 — one volume-joint branch solve, per-tilt outputs, everything
//! deterministic (`docs/dealias-v4-spec.md` is the governing spec).
//!
//! The v1 region core (Jing & Wiener 1993, *JTECH* 10, 798–808) segments each
//! velocity tilt and resolves RELATIVE folds; what boundary evidence cannot
//! know is each connected group's ABSOLUTE branch (failures F1/F3/F5 in the
//! spec).  v4 decides all branches at once by minimizing one discrete energy
//! over per-super-region branch shifts k ∈ {−2…+2} across every velocity
//! tilt of the volume, fed by:
//!
//! - an optional caller-supplied [`EnvironmentalWindProfile`] (4DD sounding
//!   initialization, James & Houze 2001; ORPG environmental winds, Eilts &
//!   Smith 1990) — the only non-circular evidence in deep widespread
//!   aliasing;
//! - the previous volume's solution (temporal prior), weighted by its own
//!   confidence output so a persistent misbranch cannot reproduce itself
//!   (the F6 GIGO break);
//! - vertical co-location between adjacent tilts (the 3-D evidence);
//! - the v1 vote graph's weak edges as soft terms (the F3 fix).
//!
//! A post-solve repair gauntlet (UNRAVEL, Louf et al. 2020; R2D2 couplet
//! protection, Feldmann et al. 2020; despeckle, Holleman & Beekhuis 2003)
//! closes the long tail under strict do-no-harm rules.
//!
//! INVARIANTS
//! - recast_radar_correct never fetches: the profile and the previous volume are caller
//!   values.  With neither, output degrades to v1-class behavior (pinned by
//!   test) — never worse.
//! - Whole ±2N moves only, per gate, everywhere (solver and gauntlet).
//! - Nyquist-less feeds (JMA staggered PRT) stay pass-through by design —
//!   same `dealias_skipped_no_nyquist` contract as v1.
//! - Same input volume (+ same priors) ⇒ byte-identical grids and
//!   confidence, across runs and thread counts.
//! - v1/v2/v3 remain untouched; v4 is additive and selectable.
//!
//! DEVIATION FROM SPEC §6 (documented): `TemporalPrior::Volume` solves the
//! bare previous volume with THIS solver (no temporal prior, same
//! environment) instead of a plain v1 pass.  A v1-only prior would re-inject
//! exactly the branch errors the temporal term is meant to correct (the
//! hybrid solved its prior with an upper-tilt reference for the same
//! reason); confidence propagation still applies.  Callers that keep the
//! previous [`V4VolumeSolution`] avoid the extra solve entirely.

mod confidence;
mod env_profile;
mod graph;
mod merge;
mod repair;
mod solve;
mod super_regions;

pub use confidence::ConfidenceGrid;
pub use env_profile::{EnvWindLevel, EnvironmentalWindProfile, project_environmental_winds_onto};

use chrono::{DateTime, Utc};
use rayon::prelude::*;
use recast_radar_core::{Field, GateMapping, Quantity, Volume};

use crate::region_core::{self, RegionSolve};
use graph::MappedPrior;
use repair::{RepairContext, RepairDiagnostics};
use super_regions::{SUPERREGION_MIN_GATES, TiltSuperRegions, build_super_regions};

/// Temporal priors older than this are more likely to represent a different
/// storm structure than a useful continuity constraint (same constant as the
/// retired hybrid engine, removed at v0.29.0).
const TEMPORAL_MAX_AGE_SECONDS: i64 = 15 * 60;
/// Match the same nominal elevation between consecutive volumes.
const TEMPORAL_ELEVATION_TOLERANCE_DEG: f32 = 0.40;
/// Reject physically implausible reference samples (same as the retired
/// hybrid engine).
const MAX_REFERENCE_ABS_VELOCITY_MPS: f32 = 160.0;
/// Temporal reference gates below this confidence (≈ 64/255, the
/// "interior-consistent only" level) fall through to vertical/environmental
/// evidence in the repair reference stack.  `confidence::REPAIR_CHANGED`
/// sits BELOW this floor by design — see its doc for the measured
/// repair-echo failure this breaks.
const REPAIR_TEMPORAL_MIN_CONFIDENCE: f32 = 0.25;
/// Velocity-interval width (× Nyquist) for the segmentation hygiene split
/// (Helmus & Collis 2016; spec §4f/§5.1).  N/2 matches `REGION_JOIN_FRAC`,
/// so no pair that could directly join a region ever spans more than two
/// intervals.  Cuts F2 mega-region noise chains; smooth seams weld back as
/// strong vote edges.
const V4_INTERVAL_SPLIT_FRAC: f32 = 0.5;

/// The previous volume's evidence, in order of preference.  Prefer passing
/// the previous *solution* so confidence propagates (F6); a bare previous
/// volume is accepted and solved internally as a fallback (see module doc).
pub enum TemporalPrior<'a> {
    Solution(&'a V4VolumeSolution),
    Volume(&'a Volume),
}

/// Solver + gauntlet diagnostics; the eval battery regression-gates these.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct V4Diagnostics {
    pub velocity_tilts: usize,
    pub nodes: usize,
    pub graph_edges: usize,
    pub components: usize,
    pub enumerated_components: usize,
    /// Nonzero means the forest-DP + ICM heuristic missed an optimum that
    /// exhaustive enumeration found — watch it in the battery.
    pub enumeration_beat_heuristic: usize,
    pub energy: f64,
    pub env_profile_used: bool,
    pub temporal_prior_used: bool,
    pub couplet_masked: usize,
    pub speck_snapped: usize,
    pub patch_changed: usize,
    pub ring_closed: usize,
    /// Patch components reverted by the boundary-inflation audit (gates).
    pub patch_reverted: usize,
    pub box_moved: usize,
    pub plane_moved: usize,
    pub repair_aborts: u32,
}

/// One solved velocity tilt.
pub struct V4TiltSolution {
    sweep_index: usize,
    elevation_deg: f32,
    azimuths: Vec<f32>,
    /// Centre of native gate 0 and native spacing of `field`, metres.
    first_gate_m: f64,
    gate_spacing_m: f64,
    field: Field,
    confidence: ConfidenceGrid,
}

/// The whole-volume solve result.  The app's render worker caches one of
/// these per volume; every velocity tilt is served from it in O(1).
pub struct V4VolumeSolution {
    site_id: String,
    volume_time: DateTime<Utc>,
    /// Indexed by sweep index (None for sweeps without velocity).
    tilts: Vec<Option<V4TiltSolution>>,
    diagnostics: V4Diagnostics,
}

impl V4VolumeSolution {
    /// Dealiased velocity field (`VRADDH`) for a sweep, if it carried
    /// velocity.  O(1).
    pub fn tilt_field(&self, sweep_index: usize) -> Option<&Field> {
        self.tilts
            .get(sweep_index)?
            .as_ref()
            .map(|tilt| &tilt.field)
    }

    /// Per-gate branch confidence for a sweep (spec §8).  O(1).
    pub fn tilt_confidence(&self, sweep_index: usize) -> Option<&ConfidenceGrid> {
        self.tilts
            .get(sweep_index)?
            .as_ref()
            .map(|tilt| &tilt.confidence)
    }

    pub fn diagnostics(&self) -> &V4Diagnostics {
        &self.diagnostics
    }

    /// Consume the solution, extracting one tilt's field without a clone.
    pub fn into_tilt_field(mut self, sweep_index: usize) -> Option<Field> {
        self.tilts
            .get_mut(sweep_index)?
            .take()
            .map(|tilt| tilt.field)
    }
}

/// Solve the whole volume once (spec §6 public surface).
///
/// `previous` enables the temporal prior; `environment` the absolute branch
/// anchor.  Both are optional and independently validated (site, age,
/// staleness) — an unusable input degrades gracefully, never errors.
///
/// Velocity is each sweep's [`Quantity::RadialVelocity`] field
/// ([`recast_radar_core::Sweep::find`]); sweeps whose velocity field provides
/// no rows or gates are skipped.  The volume time used for the temporal and
/// environmental staleness checks is the first ray time
/// (`time_coverage.start`), else the time reference.
pub fn dealias_volume(
    volume: &Volume,
    previous: Option<TemporalPrior<'_>>,
    environment: Option<&EnvironmentalWindProfile>,
) -> V4VolumeSolution {
    let volume_time = crate::volume_start_time(volume);
    let environment = environment.filter(|profile| profile.usable_for(volume_time));

    // ---- per-tilt fields + v1 region solves (parallel, order-stable) ----
    let velocity_sweeps: Vec<(usize, &Field, (f64, f64))> = volume
        .sweeps
        .iter()
        .enumerate()
        .filter_map(|(sweep_index, sweep)| {
            let field = sweep
                .find(Quantity::RadialVelocity)
                .filter(|field| crate::provided_rows(field) > 0 && field.ngates > 0)?;
            let geometry = field.native_geometry(&sweep.range)?;
            Some((sweep_index, field, geometry))
        })
        .collect();
    let mut tilts: Vec<TiltField> = velocity_sweeps
        .par_iter()
        .map(|&(sweep_index, field, geometry)| {
            build_tilt_field(volume, sweep_index, field, geometry)
        })
        .collect();

    // ---- global node assignment (deterministic: tilt order, super order) --
    let mut node_count = 0usize;
    for tilt in &mut tilts {
        tilt.node_of_super = tilt
            .supers
            .super_gates
            .iter()
            .map(|&gate_count| {
                if gate_count >= SUPERREGION_MIN_GATES {
                    let node = node_count;
                    node_count += 1;
                    node
                } else {
                    usize::MAX
                }
            })
            .collect();
    }

    // ---- temporal prior mapping ----
    let temporal = resolve_temporal(volume, &tilts, previous, environment);
    let temporal_prior_used = temporal.iter().any(Option::is_some);

    // ---- one volume energy, one solve ----
    let tables = graph::build_evidence(&tilts, node_count, environment, &temporal);
    let outcome = solve::solve_labels(&tables);

    // ---- apply labels: per-gate folds + confidence ----
    let mut per_tilt: Vec<(Vec<i32>, Vec<u8>)> = tilts
        .iter()
        .map(|tilt| apply_labels(tilt, &tables, &outcome))
        .collect();

    // ---- repair references from the PRE-repair solved fields ----
    // Built before any gauntlet runs so tilt repair order cannot matter.
    let folds_snapshot: Vec<&[i32]> = per_tilt.iter().map(|(folds, _)| folds.as_slice()).collect();
    let references: Vec<Option<Vec<f32>>> = (0..tilts.len())
        .into_par_iter()
        .map(|index| build_repair_reference(&tilts, index, &folds_snapshot, &temporal, environment))
        .collect();

    // ---- repair gauntlet per tilt (parallel; each tilt owns its arrays) ---
    let repair_results: Vec<RepairDiagnostics> = per_tilt
        .par_iter_mut()
        .zip(tilts.par_iter())
        .zip(references.par_iter())
        .map(|(((folds, confidence), tilt), reference)| {
            let ctx = RepairContext {
                observed: &tilt.observed,
                nyq: &tilt.nyq,
                rows: tilt.rows,
                gates: tilt.gates,
                wraps: tilt.wraps,
                reference: reference.as_deref(),
            };
            repair::run_gauntlet(&ctx, folds, confidence)
        })
        .collect();
    let mut repair_totals = RepairDiagnostics::default();
    for result in &repair_results {
        repair_totals.accumulate(result);
    }

    // ---- encode ----
    let mut solutions: Vec<Option<V4TiltSolution>> =
        (0..volume.sweeps.len()).map(|_| None).collect();
    for (tilt, (folds, confidence_values)) in tilts.into_iter().zip(per_tilt) {
        let field = encode_tilt(&tilt, &folds);
        let confidence = ConfidenceGrid::new(tilt.rows, tilt.gates, confidence_values);
        solutions[tilt.sweep_index] = Some(V4TiltSolution {
            sweep_index: tilt.sweep_index,
            elevation_deg: tilt.elevation_deg,
            azimuths: tilt.azimuths,
            first_gate_m: tilt.first_gate_m,
            gate_spacing_m: tilt.gate_spacing_m,
            field,
            confidence,
        });
    }

    let diagnostics = V4Diagnostics {
        velocity_tilts: velocity_sweeps.len(),
        nodes: node_count,
        graph_edges: tables.edges.len(),
        components: outcome.components,
        enumerated_components: outcome.enumerated_components,
        enumeration_beat_heuristic: outcome.enumeration_beat_heuristic,
        energy: outcome.energy,
        env_profile_used: environment.is_some(),
        temporal_prior_used,
        couplet_masked: repair_totals.couplet_masked,
        speck_snapped: repair_totals.speck_snapped,
        patch_changed: repair_totals.patch_changed,
        ring_closed: repair_totals.ring_closed,
        patch_reverted: repair_totals.patch_reverted,
        box_moved: repair_totals.box_moved,
        plane_moved: repair_totals.plane_moved,
        repair_aborts: repair_totals.aborted_modules,
    };

    V4VolumeSolution {
        site_id: volume.attrs.instrument_name.clone(),
        volume_time,
        tilts: solutions,
        diagnostics,
    }
}

/// Per-sweep convenience mirroring the retired
/// `dealias_velocity_grid_hybrid` signature (removed at v0.29.0).
/// Runs the volume solve internally — callers that need more than one tilt
/// should hold a [`V4VolumeSolution`] instead (one solve serves all tilts).
pub fn dealias_velocity_v4(
    volume: &Volume,
    sweep_index: usize,
    previous: Option<&Volume>,
    environment: Option<&EnvironmentalWindProfile>,
) -> Option<Field> {
    volume
        .sweeps
        .get(sweep_index)?
        .find(Quantity::RadialVelocity)?;
    let solution = dealias_volume(volume, previous.map(TemporalPrior::Volume), environment);
    solution.into_tilt_field(sweep_index)
}

/// Everything the solver knows about one velocity tilt.
pub(crate) struct TiltField {
    pub(crate) sweep_index: usize,
    pub(crate) elevation_deg: f32,
    pub(crate) rows: usize,
    pub(crate) gates: usize,
    /// Centre of native gate 0 and native spacing, metres.
    pub(crate) first_gate_m: f64,
    pub(crate) gate_spacing_m: f64,
    /// The source field's mapping onto its sweep range and absent rows, which
    /// the encoded output keeps.
    pub(crate) source_gates: GateMapping,
    pub(crate) source_absent_rows: Vec<u32>,
    pub(crate) wraps: bool,
    pub(crate) azimuths: Vec<f32>,
    /// Per-row Nyquist (NaN where unknown — those rows pass through).
    pub(crate) nyq: Vec<f32>,
    /// Observed velocity, NaN for no-data / range-folded gates.
    pub(crate) observed: Vec<f32>,
    pub(crate) solve: RegionSolve,
    pub(crate) supers: TiltSuperRegions,
    /// Global node index per super-region (`usize::MAX` below the gate
    /// floor).
    pub(crate) node_of_super: Vec<usize>,
}

fn build_tilt_field(
    volume: &Volume,
    sweep_index: usize,
    field: &Field,
    (first_gate_m, gate_spacing_m): (f64, f64),
) -> TiltField {
    let sweep = &volume.sweeps[sweep_index];
    let (rows, gates) = field.shape();
    let total = rows.saturating_mul(gates);

    let azimuths = crate::radial_azimuths(sweep, field);
    let fallback_nyquist = crate::median_nyquist_mps(sweep, field);
    let mut nyq = vec![f32::NAN; rows.max(1)];
    for (row, slot) in nyq.iter_mut().enumerate().take(rows) {
        *slot = crate::row_nyquist_mps(sweep, row)
            .or(fallback_nyquist)
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(f32::NAN);
    }

    let mut observed = vec![f32::NAN; total];
    if total > 0 {
        let mut row_buffer = vec![f32::NAN; gates];
        for row in 0..rows {
            crate::copy_scaled_velocity_row(field, row, &mut row_buffer);
            observed[row * gates..(row + 1) * gates].copy_from_slice(&row_buffer);
        }
    }

    let solve = merge::solve_region_folds_merge(
        &observed,
        &nyq,
        rows,
        gates,
        &azimuths,
        V4_INTERVAL_SPLIT_FRAC,
    );
    let supers = build_super_regions(&solve);
    let wraps = crate::sweep_wraps(&azimuths);

    TiltField {
        sweep_index,
        elevation_deg: sweep.tilt_elevation_deg(volume.provenance.source_format),
        rows,
        gates,
        first_gate_m,
        gate_spacing_m,
        source_gates: field.gates,
        source_absent_rows: field.absent_rows.clone(),
        wraps,
        azimuths,
        nyq,
        observed,
        solve,
        supers,
        node_of_super: Vec::new(),
    }
}

/// Per-gate folds and confidence from the label solve (pre-repair).
fn apply_labels(
    tilt: &TiltField,
    tables: &graph::EvidenceTables,
    outcome: &solve::SolveOutcome,
) -> (Vec<i32>, Vec<u8>) {
    let total = tilt.rows.saturating_mul(tilt.gates);
    let mut folds = vec![0i32; total];
    let mut confidence_values = vec![0u8; total];
    for idx in 0..total {
        if !tilt.observed[idx].is_finite() {
            continue;
        }
        let rid = tilt.solve.region_of[idx];
        if rid == u32::MAX {
            continue;
        }
        let base = tilt.solve.region_fold[rid as usize];
        let sid = tilt.supers.super_of_region[rid as usize] as usize;
        let node = tilt.node_of_super[sid];
        let (fold, gate_confidence) = if node == usize::MAX {
            (base, confidence::INTERIOR_ONLY)
        } else {
            let shifted = (base + outcome.labels[node])
                .clamp(-region_core::REGION_MAX_FOLD, region_core::REGION_MAX_FOLD);
            let gate_confidence = if tables.has_external[node] && !outcome.renormalized[node] {
                confidence::margin_to_confidence(outcome.margins[node])
            } else {
                // No external evidence, or an absolutely-degenerate
                // (re-anchored) component: interior-consistent only.
                confidence::INTERIOR_ONLY
            };
            (shifted, gate_confidence)
        };
        folds[idx] = fold;
        confidence_values[idx] = gate_confidence;
    }
    (folds, confidence_values)
}

/// Resolve the temporal prior into per-tilt mapped fields.
fn resolve_temporal(
    volume: &Volume,
    tilts: &[TiltField],
    previous: Option<TemporalPrior<'_>>,
    environment: Option<&EnvironmentalWindProfile>,
) -> Vec<Option<MappedPrior>> {
    let none = || (0..tilts.len()).map(|_| None).collect::<Vec<_>>();
    match previous {
        None => none(),
        Some(TemporalPrior::Solution(solution)) => {
            if !temporal_usable(volume, &solution.site_id, solution.volume_time) {
                return none();
            }
            map_solution(tilts, solution)
        }
        Some(TemporalPrior::Volume(previous_volume)) => {
            if !temporal_usable(
                volume,
                &previous_volume.attrs.instrument_name,
                crate::volume_start_time(previous_volume),
            ) {
                return none();
            }
            // See module doc: the bare volume is solved with this engine
            // (no temporal recursion) so the prior does not re-inject the
            // exact branch errors the temporal term exists to correct.
            let prior_solution = dealias_volume(previous_volume, None, environment);
            map_solution(tilts, &prior_solution)
        }
    }
}

fn temporal_usable(current: &Volume, prior_site: &str, prior_time: DateTime<Utc>) -> bool {
    if !current
        .attrs
        .instrument_name
        .eq_ignore_ascii_case(prior_site)
    {
        return false;
    }
    let age_seconds = crate::volume_start_time(current)
        .signed_duration_since(prior_time)
        .num_seconds();
    age_seconds > 0 && age_seconds <= TEMPORAL_MAX_AGE_SECONDS
}

fn map_solution(tilts: &[TiltField], solution: &V4VolumeSolution) -> Vec<Option<MappedPrior>> {
    let sources: Vec<&V4TiltSolution> = solution.tilts.iter().flatten().collect();
    tilts
        .iter()
        .map(|tilt| {
            let source = sources
                .iter()
                .filter_map(|source| {
                    let delta = (source.elevation_deg - tilt.elevation_deg).abs();
                    (delta <= TEMPORAL_ELEVATION_TOLERANCE_DEG).then_some((source, delta))
                })
                .min_by(|left, right| {
                    left.1
                        .total_cmp(&right.1)
                        .then_with(|| left.0.sweep_index.cmp(&right.0.sweep_index))
                })?
                .0;
            map_solution_tilt(tilt, source)
        })
        .collect()
}

fn map_solution_tilt(tilt: &TiltField, source: &V4TiltSolution) -> Option<MappedPrior> {
    let total = tilt.rows.saturating_mul(tilt.gates);
    if total == 0 {
        return None;
    }
    let row_map = graph::nearest_row_map(&tilt.azimuths, &source.azimuths);
    let gate_map = graph::nearest_gate_map(
        tilt.first_gate_m,
        tilt.gate_spacing_m,
        tilt.gates,
        source.first_gate_m,
        source.gate_spacing_m,
        source.field.ngates as usize,
    );
    let mut values = vec![f32::NAN; total];
    let mut confidence = vec![0.0f32; total];
    let mut any = false;
    for (row, mapped_row) in row_map.iter().enumerate() {
        let Some(source_row) = *mapped_row else {
            continue;
        };
        for (gate, mapped_gate) in gate_map.iter().enumerate() {
            let Some(source_gate) = *mapped_gate else {
                continue;
            };
            let Some(value) = source
                .field
                .value(source_row, source_gate)
                .filter(|value| value.is_finite() && value.abs() <= MAX_REFERENCE_ABS_VELOCITY_MPS)
            else {
                continue;
            };
            let idx = row * tilt.gates + gate;
            values[idx] = value;
            confidence[idx] = f32::from(
                source
                    .confidence
                    .value(source_row, source_gate)
                    .unwrap_or(0),
            ) / 255.0;
            any = true;
        }
    }
    any.then_some(MappedPrior { values, confidence })
}

/// The gauntlet's covering reference: temporal (confidence-gated), then the
/// nearest vertical neighbor's solved field, then the environmental
/// projection (spec §7 R2 priority order).
fn build_repair_reference(
    tilts: &[TiltField],
    index: usize,
    folds: &[&[i32]],
    temporal: &[Option<MappedPrior>],
    environment: Option<&EnvironmentalWindProfile>,
) -> Option<Vec<f32>> {
    let tilt = &tilts[index];
    let total = tilt.rows.saturating_mul(tilt.gates);
    if total == 0 {
        return None;
    }
    let mut reference = vec![f32::NAN; total];
    let mut any = false;

    if let Some(prior) = &temporal[index] {
        for ((slot, value), gate_confidence) in reference
            .iter_mut()
            .zip(&prior.values)
            .zip(&prior.confidence)
        {
            if value.is_finite() && *gate_confidence >= REPAIR_TEMPORAL_MIN_CONFIDENCE {
                *slot = *value;
                any = true;
            }
        }
    }

    // Nearest vertical neighbor: prefer above (usually cleaner), else below.
    let neighbor = (0..tilts.len())
        .filter(|&other| other != index)
        .filter_map(|other| {
            let signed = tilts[other].elevation_deg - tilt.elevation_deg;
            let delta = signed.abs();
            (graph::SIBLING_ELEVATION_DELTA_DEG..=graph::VERTICAL_MAX_ELEVATION_DELTA_DEG)
                .contains(&delta)
                .then_some((other, signed < 0.0, delta))
        })
        .min_by(|left, right| {
            left.1
                .cmp(&right.1) // false (above) sorts before true (below)
                .then_with(|| left.2.total_cmp(&right.2))
        })
        .map(|(other, _, _)| other);
    if let Some(other) = neighbor {
        let source = &tilts[other];
        let source_folds = folds[other];
        let row_map = graph::nearest_row_map(&tilt.azimuths, &source.azimuths);
        let gate_map = graph::nearest_gate_map(
            tilt.first_gate_m,
            tilt.gate_spacing_m,
            tilt.gates,
            source.first_gate_m,
            source.gate_spacing_m,
            source.gates,
        );
        for (row, mapped_row) in row_map.iter().enumerate() {
            let Some(source_row) = *mapped_row else {
                continue;
            };
            let source_n = source.nyq[source_row];
            for (gate, mapped_gate) in gate_map.iter().enumerate() {
                let idx = row * tilt.gates + gate;
                if reference[idx].is_finite() {
                    continue;
                }
                let Some(source_gate) = *mapped_gate else {
                    continue;
                };
                let source_idx = source_row * source.gates + source_gate;
                let observed = source.observed[source_idx];
                if !observed.is_finite() {
                    continue;
                }
                let value = if source_n.is_finite() {
                    observed + 2.0 * source_n * source_folds[source_idx] as f32
                } else {
                    observed
                };
                if value.abs() <= MAX_REFERENCE_ABS_VELOCITY_MPS {
                    reference[idx] = value;
                    any = true;
                }
            }
        }
    }

    if let Some(profile) = environment {
        let env_field = env_profile::project_profile_to_tilt(
            profile,
            tilt.elevation_deg,
            &tilt.azimuths,
            tilt.first_gate_m,
            tilt.gate_spacing_m,
            tilt.gates,
        );
        for idx in 0..total {
            if !reference[idx].is_finite() && env_field[idx].is_finite() {
                reference[idx] = env_field[idx];
                any = true;
            }
        }
    }

    any.then_some(reference)
}

/// Encode the final folds into the shared dealiased-velocity field format
/// (same scale/offset contract as v1/v2/v3 so every consumer downstream is
/// untouched).
fn encode_tilt(tilt: &TiltField, folds: &[i32]) -> Field {
    let total = tilt.rows.saturating_mul(tilt.gates);
    let mut corrected = vec![crate::DEALIASED_VELOCITY_NODATA; total];
    for row in 0..tilt.rows {
        let n = tilt.nyq[row];
        for gate in 0..tilt.gates {
            let idx = row * tilt.gates + gate;
            let value = tilt.observed[idx];
            if !value.is_finite() {
                continue;
            }
            let unfolded = if n.is_finite() {
                value + 2.0 * n * folds[idx] as f32
            } else {
                value
            };
            corrected[idx] = crate::encode_dealiased_velocity(unfolded);
        }
    }
    crate::dealiased_velocity_field_parts(
        tilt.source_gates,
        tilt.rows,
        tilt.gates,
        tilt.source_absent_rows.clone(),
        corrected,
    )
}

#[cfg(test)]
mod tests {
    //! v4 tests on real Level II volumes. Relative folds come from Py-ART
    //! 2.2.5 `dealias_region_based` on the same sweeps; the absolute branch is
    //! Py-ART's output shifted by the golden's `env_offset`, the whole-sweep
    //! branch closest to the model wind fixture (`tools/correct_golden.py`).
    //! Previous volumes are the real KLIX volumes before the Ida volume.

    use super::*;
    use crate::real_data::{
        self, PyartGolden, VelocitySweep, corpus_volume, echo_components, enclosed_patches,
        environment_fixture, fold_agreement, golden_volume, grid_folds,
    };
    use crate::{dealias_skipped_no_nyquist, dealias_velocity, volume_start_time};
    use chrono::Duration;
    use recast_radar_core::Sweep;

    const IDA: &str = "l2-klix-20210829-180425";
    const IDA_PREVIOUS: &str = "l2-klix-20210829-175748";
    const IDA_33_MIN_EARLIER: &str = "l2-klix-20210829-173117";

    /// `volume` with only sweep `index` left (a real tilt with no vertical
    /// neighbours).
    fn only_cut(volume: &Volume, index: usize) -> Volume {
        let mut single = volume.clone();
        let mut keep = single.sweeps.swap_remove(index);
        keep.sweep_number = 0;
        single.sweeps = vec![keep];
        single
    }

    /// Gates of `grid` on Py-ART's absolute branch (golden `env_offset`).
    fn absolute_agreement(golden: &PyartGolden, cut: &Sweep, grid: &Field) -> usize {
        let sweep = VelocitySweep::of_sweep(cut);
        let pyart = golden.aligned_folds(cut, &sweep);
        let offset = golden.env.as_ref().map_or(0, |env| env.offset);
        grid_folds(&sweep, grid)
            .iter()
            .zip(&pyart)
            .filter(|(engine, pyart)| matches!((engine, pyart), (Some(e), Some(p)) if *e == *p + offset))
            .count()
    }

    /// Ida's 1.80 deg tilt (Nyquist 23.2 m/s) solved alone has no vertical
    /// evidence; the previous volume (17:57:48Z, 6 min 37 s earlier) supplies
    /// the branch through the temporal prior.
    #[test]
    fn v4_temporal_reference_recovers_a_topmost_aliased_high_tilt() {
        let Some((current, golden)) = golden_volume("klix_20210829_s9") else {
            return;
        };
        let Some(previous) = corpus_volume(IDA_PREVIOUS) else {
            return;
        };
        let alone = only_cut(&current, golden.sweep);
        let without = dealias_volume(&alone, None, None);
        let with = dealias_volume(&alone, Some(TemporalPrior::Volume(&previous)), None);
        assert!(with.diagnostics().temporal_prior_used);
        assert!(!without.diagnostics().temporal_prior_used);
        let cut = &alone.sweeps[0];
        let before = absolute_agreement(&golden, cut, without.tilt_field(0).expect("tilt"));
        let after = absolute_agreement(&golden, cut, with.tilt_field(0).expect("tilt"));
        eprintln!("temporal: {before} -> {after} of {}", golden.valid_gates);
        assert!(
            after as f64 >= 0.994 * golden.valid_gates as f64,
            "with the prior {after} of {} gates on Py-ART's branch",
            golden.valid_gates
        );
        assert!(
            after >= before + 300,
            "the prior must move gates onto the right branch: {before} -> {after}"
        );
    }

    /// The same 1.80 deg tilt solved with the lower Ida tilts (0.48, 0.88
    /// and 1.32 deg, 720 radials each, Nyquist up to 32.1 m/s) below it:
    /// vertical evidence from the lower tilts fixes branches the tilt cannot
    /// decide alone.
    #[test]
    fn v4_lower_current_tilt_can_reference_a_folded_higher_tilt() {
        let Some((volume, golden)) = golden_volume("klix_20210829_s9") else {
            return;
        };
        let mut stack = volume.as_ref().clone();
        stack.sweeps.truncate(golden.sweep + 1);
        let alone = only_cut(&volume, golden.sweep);
        let stacked = dealias_volume(&stack, None, None);
        let single = dealias_volume(&alone, None, None);
        assert_eq!(stacked.diagnostics().velocity_tilts, 7);
        let cut = &volume.sweeps[golden.sweep];
        let with_lower = absolute_agreement(
            &golden,
            cut,
            stacked.tilt_field(golden.sweep).expect("tilt"),
        );
        let without_lower = absolute_agreement(&golden, cut, single.tilt_field(0).expect("tilt"));
        eprintln!(
            "vertical: {without_lower} -> {with_lower} of {}",
            golden.valid_gates
        );
        assert!(with_lower as f64 >= 0.994 * golden.valid_gates as f64);
        assert!(
            with_lower >= without_lower + 300,
            "{without_lower} -> {with_lower}"
        );
    }

    /// Isolated echo regions (4-connected components of 64 to 1,000 valid
    /// gates) on Ida's 1.80 and 2.42 deg tilts: with the whole volume the
    /// lower tilts decide their branch; the tilt alone gets fewer right. The
    /// solve must not invent gates outside the tilt's own coverage.
    #[test]
    fn v4_current_lower_tilt_fixes_an_isolated_high_tilt_branch() {
        let Some(volume) = corpus_volume(IDA) else {
            return;
        };
        let full = dealias_volume(&volume, None, None);
        for (case, min_share, min_gain) in [
            ("klix_20210829_s9", 0.94, 0.15),
            ("klix_20210829_s13", 0.90, 0.05),
        ] {
            let golden = PyartGolden::load(case);
            let cut = &volume.sweeps[golden.sweep];
            let sweep = VelocitySweep::of_sweep(cut);
            let pyart = golden.aligned_folds(cut, &sweep);
            let single = dealias_volume(&only_cut(&volume, golden.sweep), None, None);
            let full_grid = full.tilt_field(golden.sweep).expect("tilt");
            let volume_folds = grid_folds(&sweep, full_grid);
            let single_folds = grid_folds(&sweep, single.tilt_field(0).expect("tilt"));

            let offset = golden.env.as_ref().map_or(0, |env| env.offset);
            let label = echo_components(sweep.rows, sweep.gates, golden.rays_wrap_around, |idx| {
                sweep.observed[idx].is_finite()
            });
            let mut members: std::collections::HashMap<u32, usize> =
                std::collections::HashMap::new();
            for component in label.iter().flatten() {
                *members.entry(*component).or_default() += 1;
            }
            let isolated = |size: usize| (64..=1000).contains(&size);
            let regions = members.values().filter(|size| isolated(**size)).count();
            let (mut gates, mut volume_ok, mut single_ok) = (0usize, 0usize, 0usize);
            for (idx, component) in label.iter().enumerate() {
                let Some(component) = component else { continue };
                if !isolated(members[component]) {
                    continue;
                }
                let truth = pyart[idx].map(|fold| fold + offset);
                gates += 1;
                volume_ok += usize::from(volume_folds[idx] == truth);
                single_ok += usize::from(single_folds[idx] == truth);
            }
            eprintln!(
                "{case}: {regions} regions, {gates} gates, volume {volume_ok}, alone {single_ok}"
            );
            assert!(regions >= 8, "{case}: isolated regions {regions}");
            assert!(
                volume_ok as f64 >= min_share * gates as f64,
                "{case}: {volume_ok} of {gates}"
            );
            assert!(
                volume_ok as f64 >= single_ok as f64 + min_gain * gates as f64,
                "{case}: volume {volume_ok} vs alone {single_ok} of {gates}"
            );
            let invented = (0..sweep.rows * sweep.gates)
                .filter(|&idx| {
                    !sweep.observed[idx].is_finite()
                        && full_grid
                            .value(idx / sweep.gates, idx % sweep.gates)
                            .is_some()
                })
                .count();
            assert_eq!(
                invented, 0,
                "{case}: gates invented outside native coverage"
            );
        }
    }

    /// The KDVN derecho's 0.48 deg Doppler cut (Nyquist 21.0 m/s): folded
    /// patches inside inbound flow. The volume solve and its repair
    /// gauntlet must agree with Py-ART far better than the region engine,
    /// including on the enclosed patches (Py-ART folds of 20+ gates ringed by
    /// dominant-branch gates, all inside inbound flow here).
    #[test]
    fn v4_repairs_folded_patches_inside_inbound_flow() {
        let Some((volume, golden)) = golden_volume("kdvn_20200810_s1") else {
            return;
        };
        let cut = &volume.sweeps[golden.sweep];
        let sweep = VelocitySweep::of_sweep(cut);
        let pyart = golden.aligned_folds(cut, &sweep);
        let solution = dealias_volume(&volume, None, None);
        assert!(solution.diagnostics().patch_changed > 0);
        let v4 = grid_folds(&sweep, solution.tilt_field(golden.sweep).expect("tilt"));
        let region = grid_folds(&sweep, &dealias_velocity(cut, real_data::velocity(cut)));
        let v4_agreement = fold_agreement(&sweep, &v4, &pyart, golden.rays_wrap_around);
        let region_agreement = fold_agreement(&sweep, &region, &pyart, golden.rays_wrap_around);
        let per_echo =
            |a: &real_data::FoldAgreement| a.component_agreeing as f64 / a.compared as f64;
        eprintln!(
            "v4 {:.5}/{:.5} region {:.5}/{:.5}",
            v4_agreement.fraction(),
            per_echo(&v4_agreement),
            region_agreement.fraction(),
            per_echo(&region_agreement)
        );
        assert!(v4_agreement.fraction() >= 0.935);
        assert!(per_echo(&v4_agreement) >= 0.985);
        assert!(v4_agreement.fraction() >= region_agreement.fraction() + 0.08);

        let patches = enclosed_patches(&sweep, &pyart, 20);
        let (mut patch_gates, mut inbound_patches, mut agreeing) = (0, 0, 0);
        for patch in &patches {
            let ring_mean = patch
                .ring
                .iter()
                .map(|&idx| sweep.unfolded(idx, pyart[idx].expect("ring gate")))
                .sum::<f32>()
                / patch.ring.len() as f32;
            if ring_mean >= 0.0 {
                continue;
            }
            inbound_patches += 1;
            patch_gates += patch.gates.len();
            agreeing += patch
                .gates
                .iter()
                .filter(|&&idx| v4[idx] == pyart[idx].map(|fold| fold + v4_agreement.offset))
                .count();
        }
        eprintln!("inbound patches {inbound_patches}, gates {patch_gates}, v4 {agreeing}");
        assert!(inbound_patches >= 4);
        assert!(
            agreeing as f64 >= 0.7 * patch_gates as f64,
            "{agreeing} of {patch_gates}"
        );
    }

    /// A previous volume older than the 15 min temporal limit (Ida 17:31Z,
    /// 33 min before) must be ignored: output byte-identical to no prior. The
    /// 17:57Z volume, inside the limit, is used.
    #[test]
    fn v4_stale_temporal_volume_is_ignored() {
        let Some((current, golden)) = golden_volume("klix_20210829_s9") else {
            return;
        };
        let (Some(stale), Some(fresh)) = (
            corpus_volume(IDA_33_MIN_EARLIER),
            corpus_volume(IDA_PREVIOUS),
        ) else {
            return;
        };
        // The staleness check compares volume start times (first rays): the
        // 17:31Z volume is about 33 min older.
        let age = volume_start_time(&current) - volume_start_time(&stale);
        assert!(
            age > Duration::minutes(15) && age < Duration::minutes(40),
            "{age}"
        );
        let alone = only_cut(&current, golden.sweep);
        let with_stale = dealias_volume(&alone, Some(TemporalPrior::Volume(&stale)), None);
        let without = dealias_volume(&alone, None, None);
        assert!(!with_stale.diagnostics().temporal_prior_used);
        assert_eq!(
            with_stale.tilt_field(0).map(|g| &g.data),
            without.tilt_field(0).map(|g| &g.data)
        );
        assert_eq!(
            with_stale.tilt_confidence(0).map(ConfidenceGrid::values),
            without.tilt_confidence(0).map(ConfidenceGrid::values)
        );
        let with_fresh = dealias_volume(&alone, Some(TemporalPrior::Volume(&fresh)), None);
        assert!(with_fresh.diagnostics().temporal_prior_used);
        assert_ne!(
            with_fresh.tilt_field(0).map(|g| &g.data),
            without.tilt_field(0).map(|g| &g.data)
        );
    }

    /// Moore 2013 with the RAP 20Z analysis at KTLX (16 min before the
    /// volume): the gates whose branch the profile changes are rebranched onto
    /// Py-ART's absolute branch. Without the profile the engine reports no
    /// external evidence anywhere (confidence never above interior-only).
    #[test]
    fn v4_rebranches_onto_the_absolute_branch_only_with_environmental_evidence() {
        let Some((volume, _)) = golden_volume("ktlx_20130520_s1") else {
            return;
        };
        let env = environment_fixture("env_ktlx.json");
        let without = dealias_volume(&volume, None, None);
        let with = dealias_volume(&volume, None, Some(&env));
        assert!(!without.diagnostics().env_profile_used);
        assert!(with.diagnostics().env_profile_used);
        let (mut changed, mut with_ok, mut without_ok) = (0, 0, 0);
        for case in ["ktlx_20130520_s1", "ktlx_20130520_s3"] {
            let golden = PyartGolden::load(case);
            let cut = &volume.sweeps[golden.sweep];
            let sweep = VelocitySweep::of_sweep(cut);
            let pyart = golden.aligned_folds(cut, &sweep);
            let offset = golden.env.as_ref().expect("RAP offset").offset;
            let before = grid_folds(&sweep, without.tilt_field(golden.sweep).expect("tilt"));
            let after = grid_folds(&sweep, with.tilt_field(golden.sweep).expect("tilt"));
            for idx in 0..pyart.len() {
                let (Some(b), Some(a), Some(p)) = (before[idx], after[idx], pyart[idx]) else {
                    continue;
                };
                if a != b {
                    changed += 1;
                    with_ok += usize::from(a == p + offset);
                    without_ok += usize::from(b == p + offset);
                }
            }
            let confidence = without.tilt_confidence(golden.sweep).expect("confidence");
            assert!(
                confidence
                    .values()
                    .iter()
                    .all(|value| *value <= confidence::INTERIOR_ONLY)
            );
        }
        eprintln!("rebranched {changed}: with env {with_ok}, without {without_ok}");
        assert!(changed >= 40, "rebranched gates {changed}");
        assert!(with_ok as f64 >= 0.85 * changed as f64);
        assert!(without_ok as f64 <= 0.15 * changed as f64);
    }

    /// With the RAP profile both lowest Doppler tilts of the Moore volume land
    /// on Py-ART's absolute branch (the branch closest to the profile) almost
    /// everywhere.
    #[test]
    fn v4_environment_decides_the_absolute_branch_on_both_tilts() {
        let Some((volume, _)) = golden_volume("ktlx_20130520_s1") else {
            return;
        };
        let env = environment_fixture("env_ktlx.json");
        assert!(env.usable_for(volume_start_time(&volume)));
        let solution = dealias_volume(&volume, None, Some(&env));
        for (case, min_share) in [("ktlx_20130520_s1", 0.998), ("ktlx_20130520_s3", 0.997)] {
            let golden = PyartGolden::load(case);
            let golden_env = golden.env.as_ref().expect("RAP offset");
            assert_eq!(golden_env.fixture, "env_ktlx.json");
            assert!(
                golden_env.within_nyquist as f64 >= 0.99 * golden.valid_gates as f64,
                "{case}: Py-ART's branch lies within one Nyquist of the RAP projection on {} gates",
                golden_env.within_nyquist
            );
            let cut = &volume.sweeps[golden.sweep];
            let agreeing = absolute_agreement(
                &golden,
                cut,
                solution.tilt_field(golden.sweep).expect("tilt"),
            );
            eprintln!("{case}: {agreeing} of {}", golden.valid_gates);
            assert!(
                agreeing as f64 >= min_share * golden.valid_gates as f64,
                "{case}: {agreeing}"
            );
        }
    }

    /// The HRRR profile for Ida, with its valid time moved 4 h earlier (6 h
    /// before the volume, beyond the 3 h limit), must behave exactly like no
    /// profile; unedited (2 h before) it is used.
    #[test]
    fn v4_stale_environment_profile_is_ignored() {
        let Some((volume, golden)) = golden_volume("klix_20210829_trim_s1") else {
            return;
        };
        let mut stale = environment_fixture("env_klix_hrrr.json");
        assert!(stale.usable_for(volume_start_time(&volume)));
        let fresh = dealias_volume(&volume, None, Some(&stale));
        assert!(fresh.diagnostics().env_profile_used);
        stale.valid_time -= Duration::hours(4);
        let with_stale = dealias_volume(&volume, None, Some(&stale));
        let without = dealias_volume(&volume, None, None);
        assert!(!with_stale.diagnostics().env_profile_used);
        assert_eq!(
            with_stale.tilt_field(golden.sweep).map(|g| &g.data),
            without.tilt_field(golden.sweep).map(|g| &g.data)
        );
        assert_eq!(
            with_stale
                .tilt_confidence(golden.sweep)
                .map(ConfidenceGrid::values),
            without
                .tilt_confidence(golden.sweep)
                .map(ConfidenceGrid::values)
        );
        assert_ne!(
            fresh
                .tilt_confidence(golden.sweep)
                .map(ConfidenceGrid::values),
            without
                .tilt_confidence(golden.sweep)
                .map(ConfidenceGrid::values)
        );
    }

    /// Determinism pin (spec §5.4): the whole Ida volume with the HRRR
    /// profile solved twice gives byte-identical grids and confidence on all
    /// 19 velocity tilts.
    #[test]
    fn v4_solve_is_deterministic_across_runs() {
        let Some((volume, golden)) = golden_volume("klix_20210829_s1") else {
            return;
        };
        let env = environment_fixture("env_klix_hrrr.json");
        let first = dealias_volume(&volume, None, Some(&env));
        let second = dealias_volume(&volume, None, Some(&env));
        assert_eq!(
            first.diagnostics().velocity_tilts,
            golden.file_velocity_sweeps
        );
        assert_eq!(first.diagnostics(), second.diagnostics());
        for sweep_index in 0..volume.sweeps.len() {
            assert_eq!(
                first.tilt_field(sweep_index).map(|field| &field.data),
                second.tilt_field(sweep_index).map(|field| &field.data),
                "grids must be byte-identical (sweep {sweep_index})"
            );
            assert_eq!(
                first
                    .tilt_confidence(sweep_index)
                    .map(ConfidenceGrid::values),
                second
                    .tilt_confidence(sweep_index)
                    .map(ConfidenceGrid::values),
                "confidence must be byte-identical (sweep {sweep_index})"
            );
        }
    }

    /// Confidence on Ida with the HRRR profile: gates the solver rates above
    /// interior-only are on Py-ART's absolute branch almost without exception
    /// (the margin is meaningful), most gates get such a rating, and the
    /// diagnostics report the profile and all 19 velocity tilts.
    #[test]
    fn v4_confidence_grid_reflects_decision_margins() {
        let Some((volume, first_golden)) = golden_volume("klix_20210829_s1") else {
            return;
        };
        let env = environment_fixture("env_klix_hrrr.json");
        let solution = dealias_volume(&volume, None, Some(&env));
        assert_eq!(
            solution.diagnostics().velocity_tilts,
            first_golden.file_velocity_sweeps
        );
        assert!(solution.diagnostics().env_profile_used);
        for case in [
            "klix_20210829_s1",
            "klix_20210829_s2",
            "klix_20210829_s9",
            "klix_20210829_s13",
        ] {
            let golden = PyartGolden::load(case);
            let cut = &volume.sweeps[golden.sweep];
            let sweep = VelocitySweep::of_sweep(cut);
            let pyart = golden.aligned_folds(cut, &sweep);
            let offset = golden.env.as_ref().expect("HRRR offset").offset;
            let folds = grid_folds(&sweep, solution.tilt_field(golden.sweep).expect("tilt"));
            let confidence = solution.tilt_confidence(golden.sweep).expect("confidence");
            let (mut confident, mut confident_ok, mut rest, mut rest_ok) = (0, 0, 0, 0);
            for idx in 0..pyart.len() {
                let Some(p) = pyart[idx] else { continue };
                let ok = folds[idx] == Some(p + offset);
                let value = confidence
                    .value(idx / sweep.gates, idx % sweep.gates)
                    .expect("gate");
                if value > confidence::INTERIOR_ONLY {
                    confident += 1;
                    confident_ok += usize::from(ok);
                } else {
                    rest += 1;
                    rest_ok += usize::from(ok);
                }
            }
            eprintln!("{case}: confident {confident_ok}/{confident}, rest {rest_ok}/{rest}");
            assert!(
                confident as f64 >= 0.9 * golden.valid_gates as f64,
                "{case}: {confident}"
            );
            assert!(
                confident_ok as f64 >= 0.995 * confident as f64,
                "{case}: {confident_ok} of {confident}"
            );
            assert!(
                (confident_ok as f64 / confident as f64) > (rest_ok as f64 / rest.max(1) as f64),
                "{case}: confident gates must be right more often than the rest"
            );
        }
    }

    /// Nyquist-less feeds stay pass-through in v4 as in the region engine: the
    /// TDWR Doppler cut (Nyquist 0 on every radial).
    #[test]
    fn v4_passes_nyquist_less_tdwr_through() {
        let Some((volume, golden)) = golden_volume("tstl_20230331_trim_s1") else {
            return;
        };
        let cut = &volume.sweeps[golden.sweep];
        assert!(dealias_skipped_no_nyquist(cut, real_data::velocity(cut)));
        let sweep = VelocitySweep::of_sweep(cut);
        golden.aligned_folds(cut, &sweep);
        let solution = dealias_volume(&volume, None, None);
        let grid = solution.tilt_field(golden.sweep).expect("tilt");
        for (idx, observed) in sweep.observed.iter().enumerate() {
            let value = grid.value(idx / sweep.gates, idx % sweep.gates);
            match value {
                Some(value) => assert!((value - observed).abs() <= 0.05, "gate {idx}"),
                None => assert!(!observed.is_finite(), "gate {idx} dropped"),
            }
        }
    }
}
