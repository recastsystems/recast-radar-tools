//! Merge-based fold resolution for the v4 tilt baseline: the dynamic
//! network reduction of Py-ART's `dealias_region_based` (Helmus & Collis
//! 2016, *J. Open Res. Softw.*, 4(1):e25, doi:10.5334/jors.119; region
//! lineage Jing & Wiener 1993, *JTECH* 10, 798–808), grafted ON TOP of v1's
//! vote-graph solve rather than replacing it.
//!
//! WHY (measured, 2026-07-02, `docs/dealias-external-baselines.md`): Py-ART's
//! region engine beat v4 on residual fold boundaries on every real battery
//! case and took Case E at 99.07% with no environmental input.  Dissecting
//! its source against the battery showed the advantage is NOT the documented
//! `centered` global-mean recentering (running Py-ART with `centered=False`
//! reproduces 99.07/96.86 on E exactly, and swapping the anchor rule in this
//! port moves no battery number at all — the dominant group's mean fold
//! already rounds to zero).  The load-bearing mechanism is **gap-bridged
//! region edges** (`skip_between_rays`/`skip_along_ray` = 100): when the
//! adjacent gate is invalid, the nearest finite gate within ~100 gates still
//! votes.  Removing only this from Py-ART collapses E-ktlx12 from 96.86% to
//! 77.60% and E-keax12 from 99.07% to 95.69% — statistically our own no-env
//! numbers (77.01/95.25).  Bridges are the ONLY relative evidence connecting
//! isolated echo islands to the main field (failure F9), and on the KMBX
//! no-env chain they are what dissolved the D = 3,808-boundary catastrophe
//! (quality-filtered swaths leave whole storm sectors touching nothing).
//!
//! DIVISION OF LABOR (measured, not aesthetic):
//!
//! 1. **v1's vote graph keeps every relation it already resolves.**  All
//!    resolved vote edges are applied FIRST, in v1's exact
//!    strongest-boundary order with cycle edges skipped (phase 1 below) —
//!    reproducing v1's in-group relative folds.  A pure port that let
//!    Py-ART's weight-ordered aggregation re-decide those seams
//!    under-unfolded a 15k-gate RIJ sector on the KEAX 05:44 batch cut
//!    (intervening noise regions merge first and dilute the seam mean
//!    toward zero), and the error then propagated across tilts through the
//!    repair gauntlet's vertical reference.
//! 2. **The network reduction decides what the vote graph CANNOT**: the
//!    relative fold of every group pair connected only by bridged gap
//!    votes.  Phase 2 is Py-ART's reduction: pop the heaviest live
//!    relation, unfold the smaller node by `round(mean Δv / 2N)` over the
//!    *current* unfolded state of the *aggregated* relation, merge, combine
//!    parallels, repeat (Py-ART `_EdgeTracker`/`_RegionTracker`).
//! 3. **Bridges are evidence with two extra obligations** Py-ART does not
//!    impose (both measured necessary at full Nyquist, both no-ops in the
//!    low-Nyquist regime the mechanism was stolen for):
//!    residual — a bridge-dominated relation may unfold only when the
//!    unfolded state lands within [`BRIDGE_MAX_RESIDUAL_MPS`] of the mean
//!    (cross-gap shear is Nyquist-independent m/s physics, so a fold-unit
//!    ambiguity test is wrong at one end of the Nyquist range or the other);
//!    corroboration — at least [`BRIDGE_UNWRAP_MIN_SUPPORT`] agreeing pairs
//!    (a lone speck's handful of bridge pairs can read REAL near-2N shear
//!    across a gap as a decisive fold; measured: 4.9k near-zero KEAX gates
//!    bridged into the upper jet and unfolded to +46 m/s, |v−env| 5.2 →
//!    46.3, and 39 wrong-branch speck components on the Moore lowest tilt).
//!    An untrusted bridge relation still welds — it just never unwraps.
//! 4. **`centered` anchoring**: each final connected group lands so its
//!    gate-weighted mean unfold count rounds to zero (Py-ART applies this
//!    per sweep).  Over a full PPI the radial projection of a quasi-uniform
//!    wind has near-zero azimuthal mean (the zeroth VAD harmonic, Browning &
//!    Wexler 1968).  Measured indistinguishable from v1's "largest region =
//!    fold 0" on every battery case, kept as the honest no-evidence anchor.
//! 5. **Couplet freeze**: regions that ARE compact azimuthal shear-couplet
//!    lobes keep their v1 vote-graph folds (rebased onto the merge frame) —
//!    the R0 do-no-harm mask extended to the baseline solve (Feldmann et
//!    al. 2020 R2D2, *JTECH* 37, 2341–2356; spec §7 / failure F11).
//!
//! Bridged votes are deliberately NOT fed to the super-region strong/weak
//! graph — a 100-gate bridge is how the F3 spurious-contact class arises, so
//! bridges inform the baseline but never rigidly weld super-regions and
//! never outvote external evidence in the volume energy.
//!
//! DETERMINISM: pair statistics accumulate in row-major order; phase 1
//! follows the resolved-edge order (strongest boundary first, then region
//! pair); the merge queue breaks weight ties by (lo node, hi node, edge
//! id); every map is drained through sorted keys.  Same input ⇒
//! byte-identical folds.

use std::cmp::Reverse;

use crate::region_core::{REGION_MAX_FOLD, RegionSolve, solve_region_folds_split};

/// Py-ART `dealias_region_based` gap-bridging defaults (`skip_along_ray`,
/// `skip_between_rays` = 100 gates/rays; Helmus & Collis 2016).  Validated
/// against the battery rather than re-tuned: shorter reach (≤ 20 gates)
/// resurrects the KMBX no-env catastrophe the bridges fix.
pub(crate) const BRIDGE_MAX_GAP_GATES: usize = 100;
pub(crate) const BRIDGE_MAX_GAP_ROWS: usize = 100;
/// Reject a boundary/bridge pair whose implied jump exceeds this many folds
/// (the v1 vote pass applies the same sanity cap).
const PAIR_MAX_FOLDS: i32 = 2 * REGION_MAX_FOLD;
/// A bridge-dominated relation may unwrap a node only when the unfolded
/// state lands within this residual of the relation mean, in m/s — the same
/// tolerance class as the repair gauntlet's `PATCH_MAX_REFERENCE_ERROR_MPS`.
/// Rationale: velocity is only ASSUMED smooth across an echo-free gap; real
/// cross-gap shear is Nyquist-independent (m/s, not folds).  At N = 24.1
/// this is a 0.373-fold margin (rejects the measured KEAX jet-bridge
/// poison); at Case E's N = 12 it passes 0.75 folds — the low-Nyquist
/// regime where the whole signal lives near the half-fold mark and rounding
/// is still right (external evidence: Py-ART E-ktlx12 96.9%).
const BRIDGE_MAX_RESIDUAL_MPS: f64 = 18.0;
/// Minimum aggregate pair support before a bridge-dominated relation may
/// unwrap a node — the same corroboration floor as
/// [`super::super_regions::STRONG_EDGE_MIN_SUPPORT`].
const BRIDGE_UNWRAP_MIN_SUPPORT: u32 = super::super_regions::STRONG_EDGE_MIN_SUPPORT;

/// [`solve_region_folds_split`] with the fold resolution extended per the
/// module doc.  Segmentation, region ids, and the resolved vote-edge list
/// (consumed by `super_regions.rs` for strong/weak classification) are
/// IDENTICAL to the v1 path; only `region_fold` / `region_offset` /
/// `region_group` come from the merge.
pub(crate) fn solve_region_folds_merge(
    observed: &[f32],
    nyq: &[f32],
    rows: usize,
    gates: usize,
    azimuths: &[f32],
    interval_frac: f32,
) -> RegionSolve {
    let mut solve = solve_region_folds_split(observed, nyq, rows, gates, azimuths, interval_frac);
    let region_count = solve.region_size.len();
    if region_count == 0 {
        return solve;
    }

    let pairs = accumulate_pair_stats(observed, nyq, rows, gates, azimuths, &solve.region_of);

    // Phase-1 constraints: every relation v1's vote graph resolved, in its
    // deterministic strongest-boundary order (see module doc item 1).
    let constraints: Vec<(u32, u32, i32)> = solve
        .edges
        .iter()
        .map(|edge| (edge.lo, edge.hi, edge.fold))
        .collect();

    // Median finite Nyquist interval of the tilt, for the bridge residual
    // gate (deterministic: sort over a fixed row array).
    let mut finite_nyq: Vec<f32> = nyq
        .iter()
        .copied()
        .filter(|n| n.is_finite() && *n > 0.0)
        .collect();
    finite_nyq.sort_unstable_by(f32::total_cmp);
    let typical_interval_mps = finite_nyq
        .get(finite_nyq.len() / 2)
        .map_or(0.0, |n| 2.0 * f64::from(*n));

    let outcome = reduce_network(
        region_count,
        &solve.region_size,
        &pairs,
        &constraints,
        typical_interval_mps,
    );
    let frozen = frozen_couplet_regions(observed, nyq, rows, gates, azimuths, &solve);

    // Anchor each connected group with Py-ART's `centered` criterion: the
    // gate-weighted mean unfold count of the group rounds to zero.
    let mut group_folds = vec![0.0f64; outcome.group_count];
    let mut group_gates = vec![0u64; outcome.group_count];
    for rid in 0..region_count {
        let group = outcome.region_group[rid] as usize;
        let size = u64::from(solve.region_size[rid]);
        group_folds[group] += f64::from(outcome.unwrap[rid]) * size as f64;
        group_gates[group] += size;
    }
    let group_anchor: Vec<i32> = group_folds
        .iter()
        .zip(&group_gates)
        .map(|(folds, gates)| {
            if *gates == 0 {
                0
            } else {
                // Half-to-even mirrors Py-ART's `np.round` semantics: an
                // exactly split group must not be dragged to either branch.
                (folds / *gates as f64).round_ties_even() as i32
            }
        })
        .collect();

    // v1 anchor region per v1 vote-graph group (largest region, then the
    // smallest id — v1's exact anchor pick), for rebasing frozen regions.
    let mut v1_group_anchor = vec![u32::MAX; solve.group_count];
    for rid in 0..region_count {
        let group = solve.region_group[rid] as usize;
        let current = v1_group_anchor[group];
        if current == u32::MAX || solve.region_size[rid] > solve.region_size[current as usize] {
            v1_group_anchor[group] = rid as u32;
        }
    }
    let merge_fold =
        |rid: usize| outcome.unwrap[rid] - group_anchor[outcome.region_group[rid] as usize];

    let v1_fold = std::mem::take(&mut solve.region_fold);
    let mut region_fold = vec![0i32; region_count];
    let mut region_offset = vec![0i32; region_count];
    for rid in 0..region_count {
        let offset = if frozen[rid] {
            // Couplet-frozen region: keep its v1 vote-graph fold, expressed
            // relative to its v1 group's anchor region and rebased onto
            // wherever the merge put that anchor (see module doc).
            let anchor = v1_group_anchor[solve.region_group[rid] as usize];
            v1_fold[rid] + merge_fold(anchor as usize)
        } else {
            merge_fold(rid)
        };
        region_fold[rid] = offset.clamp(-REGION_MAX_FOLD, REGION_MAX_FOLD);
        region_offset[rid] = offset;
    }
    solve.region_fold = region_fold;
    solve.region_offset = region_offset;
    solve.region_group = outcome.region_group;
    solve.group_count = outcome.group_count;
    solve
}

/// Regions that ARE the two lobes of a compact azimuthal shear couplet
/// (candidate mesocyclone / TVS) are exempt from the merge's aggregate
/// re-branching and keep their v1 vote-graph folds — the R0 do-no-harm mask
/// (Feldmann et al. 2020 R2D2, *JTECH* 37, 2341–2356; failure F11) extended
/// from the repair gauntlet to the baseline solve.  MEASURED NECESSITY
/// (Moore 2013): the TVS inbound core is an 11-gate region whose boundary
/// means to every neighbor are 0.36–0.81 folds — rounding coin flips.
/// Boundary statistics of any kind (per-pair majority or aggregate mean)
/// prefer the smooth branch that ERASES the tornado signature; the couplet
/// criterion is the only guard, exactly as spec §7 argues for repair.
fn frozen_couplet_regions(
    observed: &[f32],
    nyq: &[f32],
    rows: usize,
    gates: usize,
    azimuths: &[f32],
    solve: &RegionSolve,
) -> Vec<bool> {
    let region_count = solve.region_size.len();
    let mask = raw_couplet_mask(observed, nyq, rows, gates, crate::sweep_wraps(azimuths));
    let mut masked_gates = vec![0u32; region_count];
    for (idx, flagged) in mask.iter().enumerate() {
        if *flagged {
            let rid = solve.region_of[idx];
            if rid != u32::MAX {
                masked_gates[rid as usize] += 1;
            }
        }
    }
    // Frozen = at least half the region's gates sit inside the (dilated)
    // couplet mask.  Giant background regions never qualify; the compact
    // lobes always do.
    masked_gates
        .iter()
        .zip(&solve.region_size)
        .map(|(masked, size)| 2 * masked >= *size)
        .collect()
}

/// The R0 couplet criterion evaluated on the RAW field: opposite-sign gate
/// pairs within [`super::repair::COUPLET_MAX_AZIMUTH_GATES`] rows at the
/// same range gate with |Δv| ≥ 1.4·N, both sides carrying a coherent
/// same-sign lobe (≥ 2 same-sign finite 8-neighbors), dilated by
/// [`super::repair::COUPLET_DILATE_GATES`].  The repair gauntlet applies the
/// same test to the solved field; here the raw field is the only one that
/// exists yet.  An ALIASED couplet side can evade the raw-field test (its
/// wrapped values may not read opposite-sign), in which case the merge
/// treats it like any region — the battery's Case B pins that the composed
/// engine still preserves the Moore couplet end-to-end.
fn raw_couplet_mask(
    observed: &[f32],
    nyq: &[f32],
    rows: usize,
    gates: usize,
    wraps: bool,
) -> Vec<bool> {
    use super::repair::{
        COUPLET_DILATE_GATES, COUPLET_MAX_AZIMUTH_GATES, COUPLET_MIN_DELTA_NYQUIST_FRAC,
    };
    let total = rows.saturating_mul(gates);
    let mut mask = vec![false; total];
    let mut seeds: Vec<usize> = Vec::new();
    let row_above = |row: usize| -> Option<usize> {
        if row + 1 < rows {
            Some(row + 1)
        } else if wraps {
            Some(0)
        } else {
            None
        }
    };
    let has_same_sign_lobe = |idx: usize| -> bool {
        let (row, gate) = (idx / gates, idx % gates);
        let value = observed[idx];
        let mut support = 0;
        for delta_row in -1i64..=1 {
            let mut sample_row = row as i64 + delta_row;
            if wraps {
                sample_row = sample_row.rem_euclid(rows as i64);
            } else if sample_row < 0 || sample_row >= rows as i64 {
                continue;
            }
            for delta_gate in -1i64..=1 {
                if delta_row == 0 && delta_gate == 0 {
                    continue;
                }
                let sample_gate = gate as i64 + delta_gate;
                if sample_gate < 0 || sample_gate >= gates as i64 {
                    continue;
                }
                let sample = sample_row as usize * gates + sample_gate as usize;
                if observed[sample].is_finite() && observed[sample] * value > 0.0 {
                    support += 1;
                    if support >= 2 {
                        return true;
                    }
                }
            }
        }
        false
    };
    for row in 0..rows {
        let n = nyq[row];
        if !n.is_finite() || n <= 0.0 {
            continue;
        }
        for gate in 0..gates {
            let idx = row * gates + gate;
            let value = observed[idx];
            if !value.is_finite() {
                continue;
            }
            let mut other_row = row;
            for _ in 0..COUPLET_MAX_AZIMUTH_GATES {
                let Some(next) = row_above(other_row) else {
                    break;
                };
                other_row = next;
                if other_row == row {
                    break; // tiny wrapped sweeps
                }
                let other_idx = other_row * gates + gate;
                let other_value = observed[other_idx];
                if !other_value.is_finite() {
                    continue;
                }
                let other_n = nyq[other_row];
                if !other_n.is_finite() || other_n <= 0.0 {
                    continue;
                }
                let threshold = COUPLET_MIN_DELTA_NYQUIST_FRAC * n.min(other_n);
                if value * other_value < 0.0
                    && (value - other_value).abs() >= threshold
                    && has_same_sign_lobe(idx)
                    && has_same_sign_lobe(other_idx)
                {
                    if !mask[idx] {
                        mask[idx] = true;
                        seeds.push(idx);
                    }
                    if !mask[other_idx] {
                        mask[other_idx] = true;
                        seeds.push(other_idx);
                    }
                }
            }
        }
    }
    let mut dilated = mask.clone();
    for idx in seeds {
        let (row, gate) = (idx / gates, idx % gates);
        for delta_row in -(COUPLET_DILATE_GATES as i64)..=(COUPLET_DILATE_GATES as i64) {
            let mut neighbor_row = row as i64 + delta_row;
            if wraps {
                neighbor_row = neighbor_row.rem_euclid(rows as i64);
            } else if neighbor_row < 0 || neighbor_row >= rows as i64 {
                continue;
            }
            for delta_gate in -(COUPLET_DILATE_GATES as i64)..=(COUPLET_DILATE_GATES as i64) {
                let neighbor_gate = gate as i64 + delta_gate;
                if neighbor_gate < 0 || neighbor_gate >= gates as i64 {
                    continue;
                }
                dilated[neighbor_row as usize * gates + neighbor_gate as usize] = true;
            }
        }
    }
    dilated
}

/// Aggregated statistics for one undirected region pair `(lo, hi)`, oriented
/// lo→hi: `sum_folds` accumulates `(v_lo − v_hi) / 2N` per gate pair, so
/// `round(sum_folds / weight)` is the fold by which `hi` trails `lo`
/// (Py-ART `_EdgeTracker.sum_diff`).
struct PairStats {
    weight: u32,
    sum_folds: f64,
    /// How many of the `weight` gate pairs are gap bridges (not touching).
    bridge_weight: u32,
}

fn accumulate_pair_stats(
    observed: &[f32],
    nyq: &[f32],
    rows: usize,
    gates: usize,
    azimuths: &[f32],
    region_of: &[u32],
) -> Vec<((u32, u32), PairStats)> {
    let mut pairs: std::collections::HashMap<(u32, u32), PairStats> =
        std::collections::HashMap::new();
    let mut add_pair = |ra: u32, va: f32, rb: u32, vb: f32, n: f32, bridged: bool| {
        if ra == rb || !n.is_finite() || n <= 0.0 {
            return;
        }
        let jump = f64::from(va - vb) / f64::from(2.0 * n);
        if (jump.round() as i32).abs() > PAIR_MAX_FOLDS {
            return; // same sanity rejection as the v1 vote pass
        }
        let (lo, hi, oriented) = if ra < rb {
            (ra, rb, jump)
        } else {
            (rb, ra, -jump)
        };
        let entry = pairs.entry((lo, hi)).or_insert(PairStats {
            weight: 0,
            sum_folds: 0.0,
            bridge_weight: 0,
        });
        entry.weight += 1;
        entry.sum_folds += oriented;
        entry.bridge_weight += u32::from(bridged);
    };

    let wrap = crate::sweep_wraps(azimuths);

    // ---- touching pairs (the v1 vote pass's exact neighbor set) ----
    for row in 0..rows {
        let row_n = nyq[row];
        for gate in 0..gates {
            let idx = row * gates + gate;
            let ra = region_of[idx];
            if ra == u32::MAX {
                continue;
            }
            if gate + 1 < gates {
                let rb = region_of[idx + 1];
                if rb != u32::MAX {
                    add_pair(ra, observed[idx], rb, observed[idx + 1], row_n, false);
                }
            }
            if row + 1 < rows {
                let down = (row + 1) * gates + gate;
                let rb = region_of[down];
                if rb != u32::MAX {
                    add_pair(
                        ra,
                        observed[idx],
                        rb,
                        observed[down],
                        row_n.min(nyq[row + 1]),
                        false,
                    );
                }
            }
        }
    }
    if wrap && rows > 1 {
        let n = nyq[rows - 1].min(nyq[0]);
        for gate in 0..gates {
            let a = (rows - 1) * gates + gate;
            let (ra, rb) = (region_of[a], region_of[gate]);
            if ra != u32::MAX && rb != u32::MAX {
                add_pair(ra, observed[a], rb, observed[gate], n, false);
            }
        }
    }

    // ---- bridged pairs along each ray (nearest finite gate across an
    // invalid gap of 1..=BRIDGE_MAX_GAP_GATES gates) ----
    for row in 0..rows {
        let row_n = nyq[row];
        let mut last: Option<(usize, u32)> = None;
        for gate in 0..gates {
            let idx = row * gates + gate;
            let region = region_of[idx];
            if region == u32::MAX {
                continue;
            }
            if let Some((last_gate, last_region)) = last {
                let gap = gate - last_gate - 1;
                if (1..=BRIDGE_MAX_GAP_GATES).contains(&gap) {
                    add_pair(
                        last_region,
                        observed[row * gates + last_gate],
                        region,
                        observed[idx],
                        row_n,
                        true,
                    );
                }
            }
            last = Some((gate, region));
        }
    }

    // ---- bridged pairs across rays at fixed range (including the azimuth
    // wrap seam) ----
    for gate in 0..gates {
        let mut first: Option<usize> = None;
        let mut last: Option<usize> = None;
        for row in 0..rows {
            let idx = row * gates + gate;
            let region = region_of[idx];
            if region == u32::MAX {
                continue;
            }
            if let Some(last_row) = last {
                let gap = row - last_row - 1;
                if (1..=BRIDGE_MAX_GAP_ROWS).contains(&gap) {
                    let prev = last_row * gates + gate;
                    add_pair(
                        region_of[prev],
                        observed[prev],
                        region,
                        observed[idx],
                        nyq[last_row].min(nyq[row]),
                        true,
                    );
                }
            }
            if first.is_none() {
                first = Some(row);
            }
            last = Some(row);
        }
        // Wrap seam: bridge from the last finite row back to the first, but
        // only across a genuine gap (gap 0 is the touching wrap pair above).
        if wrap
            && rows > 1
            && let (Some(first_row), Some(last_row)) = (first, last)
            && first_row != last_row
        {
            let gap = (rows - 1 - last_row) + first_row;
            if (1..=BRIDGE_MAX_GAP_ROWS).contains(&gap) {
                let a = last_row * gates + gate;
                let b = first_row * gates + gate;
                add_pair(
                    region_of[a],
                    observed[a],
                    region_of[b],
                    observed[b],
                    nyq[last_row].min(nyq[first_row]),
                    true,
                );
            }
        }
    }

    // Drain through sorted keys so edge ids are deterministic.
    let mut keys: Vec<(u32, u32)> = pairs.keys().copied().collect();
    keys.sort_unstable();
    keys.into_iter()
        .map(|key| (key, pairs.remove(&key).expect("key from map")))
        .collect()
}

struct MergeEdge {
    a: u32,
    b: u32,
    weight: u32,
    /// Σ (v_a − v_b)/2N over the relation's gate pairs, in the CURRENT
    /// unfolded state (updated as nodes unwrap, Py-ART
    /// `_EdgeTracker.unwrap_node`).
    sum_folds: f64,
    /// How many of the `weight` pairs are gap bridges.
    bridge_weight: u32,
    alive: bool,
}

struct ReduceOutcome {
    /// Final unfold count per region, in the merge frame.
    unwrap: Vec<i32>,
    /// Compact connected-group id per region (region-id order).
    region_group: Vec<u32>,
    group_count: usize,
}

/// Phase 1: apply the trusted vote-graph relations (`k[hi] = k[lo] + fold`)
/// in their given deterministic order, skipping cycles — v1's exact union
/// semantics.  Phase 2: the dynamic network reduction (Py-ART
/// `_combine_regions` loop) over everything that remains: pop the heaviest
/// live relation, unwrap the smaller node by the rounded aggregate mean
/// difference (bridge-dominated relations must also pass the residual and
/// corroboration gates), merge it into the larger, combine parallel
/// relations, repeat until none remain.
fn reduce_network(
    region_count: usize,
    region_size: &[u32],
    pairs: &[((u32, u32), PairStats)],
    constraints: &[(u32, u32, i32)],
    typical_interval_mps: f64,
) -> ReduceOutcome {
    let edges: Vec<MergeEdge> = pairs
        .iter()
        .map(|((lo, hi), stats)| MergeEdge {
            a: *lo,
            b: *hi,
            weight: stats.weight,
            sum_folds: stats.sum_folds,
            bridge_weight: stats.bridge_weight,
            alive: true,
        })
        .collect();
    let mut adjacency: Vec<Vec<u32>> = vec![Vec::new(); region_count];
    for (edge_index, edge) in edges.iter().enumerate() {
        adjacency[edge.a as usize].push(edge_index as u32);
        adjacency[edge.b as usize].push(edge_index as u32);
    }

    // The bridge residual cap expressed in folds for this tilt (see the
    // constant's doc); a degenerate Nyquist disables the gate.
    let bridge_max_residual_folds = if typical_interval_mps > 0.0 {
        BRIDGE_MAX_RESIDUAL_MPS / typical_interval_mps
    } else {
        f64::INFINITY
    };

    let mut reducer = Reducer {
        edges,
        adjacency,
        node_gates: region_size.iter().map(|&size| u64::from(size)).collect(),
        regions_in_node: (0..region_count as u32).map(|rid| vec![rid]).collect(),
        node_of: (0..region_count as u32).collect(),
        unwrap: vec![0i32; region_count],
        heap: std::collections::BinaryHeap::new(),
        base_cache: None,
    };
    reducer.heap = reducer
        .edges
        .iter()
        .enumerate()
        .map(|(edge_index, edge)| {
            (
                edge.weight,
                Reverse(edge.a),
                Reverse(edge.b),
                Reverse(edge_index as u32),
            )
        })
        .collect();

    // ---- phase 1: trusted relations, in their deterministic given order ---
    for &(lo, hi, fold) in constraints {
        let node_lo = reducer.node_of[lo as usize];
        let node_hi = reducer.node_of[hi as usize];
        if node_lo == node_hi {
            // A cycle among trusted relations: first-processed wins, exactly
            // the v1 union semantics for its strongest-first edge walk.
            continue;
        }
        let needed = fold - (reducer.unwrap[hi as usize] - reducer.unwrap[lo as usize]);
        // Larger node stays (same rule as the aggregate loop below).
        let keep_lo = reducer.node_gates[node_lo as usize] > reducer.node_gates[node_hi as usize]
            || (reducer.node_gates[node_lo as usize] == reducer.node_gates[node_hi as usize]
                && node_lo < node_hi);
        let (base, merge, rdiff) = if keep_lo {
            (node_lo, node_hi, needed)
        } else {
            (node_hi, node_lo, -needed)
        };
        reducer.merge_nodes(base, merge, rdiff);
    }

    // ---- phase 2: weight-ordered aggregate reduction (Py-ART) ----
    while let Some((weight, Reverse(lo), Reverse(hi), Reverse(edge_index))) = reducer.heap.pop() {
        let edge = &reducer.edges[edge_index as usize];
        if !edge.alive
            || edge.weight != weight
            || (edge.a.min(edge.b), edge.a.max(edge.b)) != (lo, hi)
        {
            continue; // stale heap entry
        }
        let (a, b) = (edge.a, edge.b);
        let mean = edge.sum_folds / f64::from(edge.weight);
        // Half-to-even mirrors Py-ART (`int(np.round(diff))`): a relation
        // whose aggregate mean sits exactly on a half fold is a coin flip
        // and must not unwrap.
        let mut rdiff = (mean.round_ties_even() as i32).clamp(-PAIR_MAX_FOLDS, PAIR_MAX_FOLDS);
        // Bridge-dominated relations unwrap only when (a) the unfolded
        // state lands close to the mean in m/s and (b) enough parallel
        // pairs corroborate the relation — see the module doc and the
        // constants for the measured failure classes each gate blocks.
        // The do-no-harm answer for an uncorroborated or shear-ambiguous
        // bridge is to weld without unwrapping.
        if rdiff != 0
            && edge.bridge_weight * 2 >= edge.weight
            && ((mean - mean.round()).abs() > bridge_max_residual_folds
                || edge.weight < BRIDGE_UNWRAP_MIN_SUPPORT)
        {
            rdiff = 0;
        }

        // Larger node (by gates) stays; ties keep the smaller id (Py-ART
        // keeps node2 on ties — any fixed rule works, this one is stable
        // under our id assignment).
        let (base, merge) = if reducer.node_gates[a as usize] > reducer.node_gates[b as usize]
            || (reducer.node_gates[a as usize] == reducer.node_gates[b as usize] && a < b)
        {
            (a, b)
        } else {
            (b, a)
        };
        // sum_folds is oriented a→b (`k[b] = k[a] + round(mean)`), so the
        // merge node's unwrap is +rdiff when it is `b`, −rdiff when it is
        // `a` (Py-ART negates rdiff when the merge direction flips).
        if merge == a {
            rdiff = -rdiff;
        }

        reducer.edges[edge_index as usize].alive = false;
        reducer.merge_nodes(base, merge, rdiff);
    }

    // Compact group ids in region-id order (deterministic).
    let mut node_of_region = vec![u32::MAX; region_count];
    for (node, regions) in reducer.regions_in_node.iter().enumerate() {
        for &rid in regions {
            node_of_region[rid as usize] = node as u32;
        }
    }
    let mut region_group = vec![0u32; region_count];
    let mut node_to_group: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for rid in 0..region_count {
        let node = node_of_region[rid];
        let next_group = node_to_group.len() as u32;
        region_group[rid] = *node_to_group.entry(node).or_insert(next_group);
    }
    let group_count = node_to_group.len();

    ReduceOutcome {
        unwrap: reducer.unwrap,
        region_group,
        group_count,
    }
}

/// One heap entry: `(weight, lo node, hi node, edge id)` with the node and
/// edge components reversed so the max-heap breaks weight ties toward the
/// SMALLEST identifiers (deterministic pop order).
type QueueEntry = (u32, Reverse<u32>, Reverse<u32>, Reverse<u32>);

/// Mutable state of the dynamic network reduction.
struct Reducer {
    edges: Vec<MergeEdge>,
    adjacency: Vec<Vec<u32>>,
    node_gates: Vec<u64>,
    regions_in_node: Vec<Vec<u32>>,
    /// Current node id per region.
    node_of: Vec<u32>,
    unwrap: Vec<i32>,
    heap: std::collections::BinaryHeap<QueueEntry>,
    /// Neighbor → edge map for the current base node (Py-ART's
    /// `_common_finder` with the `_last_base_node` reuse).
    base_cache: Option<(u32, std::collections::HashMap<u32, u32>)>,
}

impl Reducer {
    /// Unwrap `merge` by `rdiff` and merge it into `base`: update the region
    /// unwraps, shift every aggregate sum touching `merge`, re-point its
    /// live edges to `base` combining parallels, and move its regions.
    fn merge_nodes(&mut self, base: u32, merge: u32, rdiff: i32) {
        if rdiff != 0 {
            for &rid in &self.regions_in_node[merge as usize] {
                self.unwrap[rid as usize] += rdiff;
            }
            for &other_index in &self.adjacency[merge as usize] {
                let other = &mut self.edges[other_index as usize];
                if !other.alive {
                    continue;
                }
                // v_merge moved by rdiff intervals.
                if other.a == merge {
                    other.sum_folds += f64::from(other.weight) * f64::from(rdiff);
                } else {
                    other.sum_folds -= f64::from(other.weight) * f64::from(rdiff);
                }
            }
        }

        // Rebuild the base neighbor map only when the base node changes
        // (taken out of the cache so edge mutations below don't conflict).
        let mut neighbor_map = match self.base_cache.take() {
            Some((cached_base, map)) if cached_base == base => map,
            _ => {
                let mut map = std::collections::HashMap::new();
                for &base_edge in &self.adjacency[base as usize] {
                    let candidate = &self.edges[base_edge as usize];
                    if !candidate.alive {
                        continue;
                    }
                    let neighbor = if candidate.a == base {
                        candidate.b
                    } else {
                        candidate.a
                    };
                    map.insert(neighbor, base_edge);
                }
                map
            }
        };

        // Re-point the merge node's live edges to the base, combining
        // parallel edges (weights and sums add; orientation reconciled).
        let merge_edges = std::mem::take(&mut self.adjacency[merge as usize]);
        for moved_index in merge_edges {
            let moved = &self.edges[moved_index as usize];
            if !moved.alive {
                continue;
            }
            let neighbor = if moved.a == merge { moved.b } else { moved.a };
            if neighbor == base {
                // The consumed base↔merge relation itself: never self-loop.
                self.edges[moved_index as usize].alive = false;
                continue;
            }
            if let Some(&kept_index) = neighbor_map.get(&neighbor) {
                let moved_weight = self.edges[moved_index as usize].weight;
                let moved_bridge_weight = self.edges[moved_index as usize].bridge_weight;
                let moved_oriented = if self.edges[moved_index as usize].a == merge {
                    // moved was merge→neighbor; kept orientation decides.
                    self.edges[moved_index as usize].sum_folds
                } else {
                    -self.edges[moved_index as usize].sum_folds
                };
                let kept_sign = if self.edges[kept_index as usize].a == base {
                    1.0
                } else {
                    -1.0
                };
                let kept = &mut self.edges[kept_index as usize];
                kept.weight += moved_weight;
                kept.sum_folds += kept_sign * moved_oriented;
                kept.bridge_weight += moved_bridge_weight;
                let (kept_weight, kept_a, kept_b) = (kept.weight, kept.a, kept.b);
                self.edges[moved_index as usize].alive = false;
                self.heap.push((
                    kept_weight,
                    Reverse(kept_a.min(kept_b)),
                    Reverse(kept_a.max(kept_b)),
                    Reverse(kept_index),
                ));
            } else {
                let moved = &mut self.edges[moved_index as usize];
                if moved.a == merge {
                    moved.a = base;
                } else {
                    moved.b = base;
                }
                let (moved_weight, moved_a, moved_b) = (moved.weight, moved.a, moved.b);
                neighbor_map.insert(neighbor, moved_index);
                self.adjacency[base as usize].push(moved_index);
                self.heap.push((
                    moved_weight,
                    Reverse(moved_a.min(moved_b)),
                    Reverse(moved_a.max(moved_b)),
                    Reverse(moved_index),
                ));
            }
        }
        self.base_cache = Some((base, neighbor_map));

        let moved_regions = std::mem::take(&mut self.regions_in_node[merge as usize]);
        for &rid in &moved_regions {
            self.node_of[rid as usize] = base;
        }
        self.regions_in_node[base as usize].extend(moved_regions);
        self.node_gates[base as usize] += self.node_gates[merge as usize];
        self.node_gates[merge as usize] = 0;
    }
}

#[cfg(test)]
mod tests {
    //! Merge-solve tests on real Level II sweeps and windows cut from them;
    //! expected folds from Py-ART 2.2.5 `dealias_region_based` on the same
    //! files (`tools/correct_golden.py`).

    use super::*;
    use crate::real_data::{
        VelocitySweep, echo_components, fold_agreement, golden_volume, strongest_azimuthal_shear,
    };
    use crate::region_core::solve_region_folds;

    const SPLIT: f32 = 0.5;

    /// Per-gate fold of a region solve (`None` outside every region).
    fn solve_folds(solve: &RegionSolve) -> Vec<Option<i32>> {
        solve
            .region_of
            .iter()
            .map(|&rid| (rid != u32::MAX).then(|| solve.region_fold[rid as usize]))
            .collect()
    }

    fn merge_solve(sweep: &VelocitySweep) -> RegionSolve {
        solve_region_folds_merge(
            &sweep.observed,
            &sweep.nyq,
            sweep.rows,
            sweep.gates,
            &sweep.azimuths,
            SPLIT,
        )
    }

    fn majority(folds: impl Iterator<Item = i32>) -> i32 {
        let mut counts = std::collections::HashMap::new();
        for fold in folds {
            *counts.entry(fold).or_insert(0usize) += 1;
        }
        counts
            .into_iter()
            .max_by_key(|(fold, count)| (*count, -fold.abs()))
            .map_or(0, |(fold, _)| fold)
    }

    /// Echo islands (valid-gate components of 20 to 5,000 gates) separated
    /// by no-data gaps from the main echo, on a branch Py-ART puts apart from
    /// the main echo's: the KDVN derecho's 0.48 deg cut (18 islands) and Ida's
    /// 0.48 deg cut at Nyquist 23.2 m/s (8 islands). Nothing touches them, so
    /// only bridged pairs across the gap can resolve their fold: the merge
    /// solve must put them on Py-ART's branch, which the plain vote graph (no
    /// bridges) cannot.
    #[test]
    fn bridged_pairs_resolve_an_isolated_island_across_a_gap() {
        for (case, min_islands) in [("kdvn_20200810_s1", 15), ("klix_20210829_s2", 6)] {
            let Some((volume, golden)) = golden_volume(case) else {
                return;
            };
            let cut = &volume.cuts[golden.sweep];
            let sweep = VelocitySweep::of_cut(cut);
            let pyart = golden.aligned_folds(cut, &sweep);
            let members = echo_members(&sweep, golden.rays_wrap_around);
            let main = members
                .iter()
                .max_by_key(|gates| gates.len())
                .expect("echo");
            let main_fold = majority(main.iter().filter_map(|&idx| pyart[idx]));

            let merge = solve_folds(&merge_solve(&sweep));
            let plain = solve_folds(&solve_region_folds(
                &sweep.observed,
                &sweep.nyq,
                sweep.rows,
                sweep.gates,
                &sweep.azimuths,
            ));
            let merge_offset =
                fold_agreement(&sweep, &merge, &pyart, golden.rays_wrap_around).offset;
            let plain_offset =
                fold_agreement(&sweep, &plain, &pyart, golden.rays_wrap_around).offset;
            let (mut islands, mut gates, mut merge_ok, mut plain_ok) = (0, 0, 0, 0);
            for island in &members {
                if !(20..=5000).contains(&island.len())
                    || majority(island.iter().filter_map(|&idx| pyart[idx])) == main_fold
                {
                    continue;
                }
                islands += 1;
                gates += island.len();
                for &idx in island {
                    let truth = pyart[idx].expect("valid gate");
                    merge_ok += usize::from(merge[idx] == Some(truth + merge_offset));
                    plain_ok += usize::from(plain[idx] == Some(truth + plain_offset));
                }
            }
            eprintln!(
                "{case}: aliased islands {islands}, gates {gates}, merge {merge_ok}, plain {plain_ok}"
            );
            assert!(islands >= min_islands, "{case}: aliased islands {islands}");
            assert!(
                merge_ok as f64 >= 0.9 * gates as f64,
                "{case}: merge {merge_ok} of {gates}"
            );
            assert!(
                plain_ok as f64 <= 0.2 * gates as f64,
                "{case}: without bridges {plain_ok} of {gates} (the islands must need them)"
            );
        }
    }

    /// Valid-gate components (4-neighbour) of a sweep.
    fn echo_members(sweep: &VelocitySweep, wraps: bool) -> Vec<Vec<usize>> {
        let label = echo_components(sweep.rows, sweep.gates, wraps, |idx| {
            sweep.observed[idx].is_finite()
        });
        let mut members: std::collections::BTreeMap<u32, Vec<usize>> = Default::default();
        for (idx, component) in label.iter().enumerate() {
            if let Some(component) = component {
                members.entry(*component).or_default().push(idx);
            }
        }
        members.into_values().collect()
    }

    /// The 20 May 2013 Moore tornado couplet on KTLX's 0.48 deg Doppler cut
    /// (Nyquist 26.1 m/s): in Py-ART's output the strongest opposite-sign
    /// azimuthal gate-to-gate shear 10-40 km from the radar is 98.0 m/s, at
    /// 266 deg and 22.6 km. Genuine shear near 2N must not be averaged away by
    /// the merge's aggregate re-branching: the merge solve keeps a couplet at
    /// least that strong at the same place.
    #[test]
    fn aggregation_preserves_an_embedded_shear_couplet() {
        let Some((volume, golden)) = golden_volume("ktlx_20130520_trim_s1") else {
            return;
        };
        let cut = &volume.cuts[golden.sweep];
        let sweep = VelocitySweep::of_cut(cut);
        let pyart = golden.aligned_folds(cut, &sweep);
        let (row, gate, shear) = strongest_azimuthal_shear(&sweep, &pyart, 10_000, 40_000);
        eprintln!(
            "Py-ART couplet row {row} gate {gate} azimuth {} shear {shear}",
            sweep.azimuths[row]
        );
        assert!(
            shear > 1.4 * golden.nyquist_mps,
            "Py-ART couplet shear {shear}"
        );

        let merge = solve_folds(&merge_solve(&sweep));
        let (merge_row, merge_gate, merge_shear) =
            strongest_azimuthal_shear(&sweep, &merge, 10_000, 40_000);
        eprintln!("merge couplet row {merge_row} gate {merge_gate} shear {merge_shear}");
        assert!(
            merge_shear >= shear,
            "couplet shear {shear} -> {merge_shear}"
        );
        assert!(
            merge_row.abs_diff(row) <= 4 && merge_gate.abs_diff(gate) <= 8,
            "couplet moved from ({row}, {gate}) to ({merge_row}, {merge_gate})"
        );
    }

    /// Specks of one or two gates (at most 8 bridge pairs, below the 12-pair
    /// corroboration floor) whose nearest echo in most bridge directions
    /// differs by more than the Nyquist velocity. Py-ART, which has no floor,
    /// unwraps most of them toward those neighbours; the merge must weld them
    /// without unwrapping, keeping each on the branch of a bridge neighbour.
    #[test]
    fn uncorroborated_bridge_welds_without_unwrapping() {
        let Some((volume, golden)) = golden_volume("kdvn_20200810_s1") else {
            return;
        };
        let cut = &volume.cuts[golden.sweep];
        let sweep = VelocitySweep::of_cut(cut);
        let pyart = golden.aligned_folds(cut, &sweep);
        let wraps = golden.rays_wrap_around;
        let label = echo_components(sweep.rows, sweep.gates, wraps, |idx| {
            sweep.observed[idx].is_finite()
        });
        let merge = solve_folds(&merge_solve(&sweep));
        let (mut specks, mut merge_kept, mut pyart_unwrapped) = (0, 0, 0);
        for speck in echo_members(&sweep, wraps)
            .iter()
            .filter(|gates| gates.len() <= 2)
        {
            let idx = speck[0];
            let (row, gate) = (idx / sweep.gates, idx % sweep.gates);
            let outside =
                |other: usize| sweep.observed[other].is_finite() && label[other] != label[idx];
            let along = |steps: &mut dyn Iterator<Item = usize>| {
                steps.map(|g| row * sweep.gates + g).find(|&o| outside(o))
            };
            let across = |forward: bool| {
                (1..=BRIDGE_MAX_GAP_ROWS)
                    .map_while(|step| {
                        let r = if forward {
                            row + step
                        } else {
                            row.wrapping_sub(step)
                        };
                        if r < sweep.rows {
                            Some(r)
                        } else if wraps {
                            Some(if forward {
                                r - sweep.rows
                            } else {
                                r.wrapping_add(sweep.rows)
                            })
                        } else {
                            None
                        }
                    })
                    .map(|r| r * sweep.gates + gate)
                    .find(|&o| outside(o))
            };
            let partners: Vec<usize> = [
                along(&mut (gate + 1..(gate + 1 + BRIDGE_MAX_GAP_GATES).min(sweep.gates))),
                along(&mut (gate.saturating_sub(BRIDGE_MAX_GAP_GATES)..gate).rev()),
                across(true),
                across(false),
            ]
            .into_iter()
            .flatten()
            .collect();
            let n = sweep.nyq[row];
            let shearing = partners
                .iter()
                .filter(|&&other| (sweep.observed[idx] - sweep.observed[other]).abs() > n)
                .count();
            if partners.is_empty() || 2 * shearing <= partners.len() {
                continue;
            }
            specks += 1;
            merge_kept += usize::from(
                partners
                    .iter()
                    .any(|&other| merge[other].is_some() && merge[other] == merge[idx]),
            );
            let pyart_same = partners
                .iter()
                .filter(|&&other| pyart[other] == pyart[idx])
                .count();
            pyart_unwrapped += usize::from(2 * pyart_same < partners.len());
        }
        eprintln!("specks {specks}, merge kept {merge_kept}, Py-ART unwrapped {pyart_unwrapped}");
        assert!(specks >= 30, "sheared specks {specks}");
        assert!(
            merge_kept as f64 >= 0.95 * specks as f64,
            "merge kept {merge_kept} of {specks}"
        );
        assert!(
            pyart_unwrapped as f64 >= 0.75 * specks as f64,
            "Py-ART unwrapped {pyart_unwrapped} of {specks}"
        );
    }

    /// Byte-determinism of the merge path over the derecho sector.
    #[test]
    fn merge_solve_is_deterministic() {
        let Some((volume, golden)) = golden_volume("kdvn_20200810_trim_s1") else {
            return;
        };
        let sweep = VelocitySweep::of_cut(&volume.cuts[golden.sweep]);
        let first = merge_solve(&sweep);
        let second = merge_solve(&sweep);
        assert!(
            first.region_size.len() > 1_000,
            "regions {}",
            first.region_size.len()
        );
        assert_eq!(first.region_of, second.region_of);
        assert_eq!(first.region_fold, second.region_fold);
        assert_eq!(first.region_offset, second.region_offset);
        assert_eq!(first.region_group, second.region_group);
    }
}
