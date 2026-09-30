//! Rust port of Py-ART's `dealias_region_based` per-sweep core: the same
//! regions, edge sums, merge order and floating-point arithmetic, so the
//! folds equal Py-ART's gate for gate; only the edge tracker's data
//! structures differ (see `EdgeTracker`).
//!
//! This module is intentionally separate from v4.  It mirrors the Py-ART
//! region algorithm defaults for one sweep: 3 interval splits, invalid-gate
//! filter only, skip gaps of 100 rays/gates, dynamic network reduction, and
//! centered sweep offset.  It does not apply v4 volume evidence, environmental
//! winds, temporal priors, couplet freeze, or repair passes.
//!
//! Derived from Py-ART (Copyright (c) 2013, UChicago Argonne, LLC; BSD
//! 3-Clause); modified from the original. The license is in
//! `THIRD_PARTY_NOTICES.md` at the repository root.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::hash::{BuildHasherDefault, Hasher};

use recast_radar_core::{Field, Sweep};

use crate::{
    DEALIASED_VELOCITY_NODATA, copy_scaled_velocity_row, dealiased_velocity_field,
    encode_dealiased_velocity, median_nyquist_mps, radial_azimuths, row_nyquist_mps, sweep_wraps,
};

const INTERVAL_SPLITS: usize = 3;
const SKIP_BETWEEN_RAYS: usize = 100;
const SKIP_ALONG_RAY: usize = 100;

/// Dealias the velocity field `source` of `sweep` with the Py-ART region
/// algorithm port. Returns a `VRADDH` field on the same rays and native gates.
pub fn dealias_velocity_pyart_region(sweep: &Sweep, source: &Field) -> Field {
    let (rows, gates) = source.shape();
    let total = rows.saturating_mul(gates);
    let fallback_nyquist = median_nyquist_mps(sweep, source);

    let mut nyq = vec![f32::NAN; rows.max(1)];
    for (row, slot) in nyq.iter_mut().enumerate().take(rows) {
        *slot = row_nyquist_mps(sweep, row)
            .or(fallback_nyquist)
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(f32::NAN);
    }

    let mut observed = vec![f32::NAN; total];
    if total > 0 {
        let mut row_buf = vec![f32::NAN; gates];
        for row in 0..rows {
            copy_scaled_velocity_row(source, row, &mut row_buf);
            observed[row * gates..(row + 1) * gates].copy_from_slice(&row_buf);
        }
    }

    let azimuths = radial_azimuths(sweep, source);
    let folds = pyart_region_folds(&observed, &nyq, rows, gates, sweep_wraps(&azimuths));

    let mut corrected = vec![DEALIASED_VELOCITY_NODATA; total];
    for (row, &n) in nyq.iter().enumerate().take(rows) {
        for gate in 0..gates {
            let idx = row * gates + gate;
            let value = observed[idx];
            if !value.is_finite() {
                continue;
            }
            let unfolded = if n.is_finite() && n > 0.0 {
                value + 2.0 * n * folds[idx] as f32
            } else {
                value
            };
            corrected[idx] = encode_dealiased_velocity(unfolded);
        }
    }

    dealiased_velocity_field(source, corrected)
}

fn pyart_region_folds(
    observed: &[f32],
    nyq: &[f32],
    rows: usize,
    gates: usize,
    wraps: bool,
) -> Vec<i32> {
    let total = rows.saturating_mul(gates);
    let mut folds = vec![0i32; total];
    if rows == 0 || gates == 0 || observed.len() != total {
        return folds;
    }

    let nvel = sweep_nyquist(nyq).unwrap_or(f32::NAN);
    if !nvel.is_finite() || nvel <= 0.0 {
        return folds;
    }
    let nyquist_interval = 2.0 * nvel;
    let limits = interval_limits(nvel, observed);
    let (labels, region_sizes) = find_regions(observed, rows, gates, &limits);
    let nfeatures = region_sizes.len().saturating_sub(1);
    if nfeatures < 2 {
        return folds;
    }

    let edges = edge_sum_and_count(&labels, observed, rows, gates, wraps, nyquist_interval);
    if edges.is_empty() {
        return folds;
    }

    let mut regions = RegionTracker::new(region_sizes);
    let mut edge_tracker = EdgeTracker::new(edges, nfeatures + 1);
    while let Some((node1, node2, diff, edge_number)) = edge_tracker.pop_edge() {
        let mut rdiff = round_ties_even_to_i32(diff);
        let node1_size = regions.get_node_size(node1);
        let node2_size = regions.get_node_size(node2);
        let (base_node, merge_node) = if node1_size > node2_size {
            (node1, node2)
        } else {
            rdiff = -rdiff;
            (node2, node1)
        };
        if rdiff != 0 {
            regions.unwrap_node(merge_node, rdiff);
            edge_tracker.unwrap_node(merge_node, rdiff);
        }
        regions.merge_nodes(base_node, merge_node);
        edge_tracker.merge_nodes(base_node, merge_node, edge_number);
    }

    let gates_dealiased: u64 = regions
        .node_size
        .iter()
        .skip(1)
        .map(|&value| value as u64)
        .sum();
    if gates_dealiased > 0 {
        let total_folds: i64 = regions
            .original_region_sizes
            .iter()
            .enumerate()
            .skip(1)
            .map(|(region, &size)| size as i64 * regions.unwrap_number[region] as i64)
            .sum();
        let sweep_offset = round_ties_even_to_i32(total_folds as f64 / gates_dealiased as f64);
        if sweep_offset != 0 {
            for unwrap in &mut regions.unwrap_number {
                *unwrap -= sweep_offset;
            }
        }
    }

    for idx in 0..total {
        let label = labels[idx] as usize;
        if label != 0 {
            folds[idx] = regions.unwrap_number[label];
        }
    }
    folds
}

fn sweep_nyquist(nyq: &[f32]) -> Option<f32> {
    nyq.iter()
        .copied()
        .find(|value| value.is_finite() && *value > 0.0)
}

fn interval_limits(nyquist: f32, observed: &[f32]) -> Vec<f32> {
    let interval = (2.0 * nyquist) / INTERVAL_SPLITS as f32;
    let mut add_start = 0i32;
    let mut add_end = 0i32;
    let mut min_value = f32::INFINITY;
    let mut max_value = f32::NEG_INFINITY;
    let mut any = false;
    for value in observed.iter().copied().filter(|value| value.is_finite()) {
        any = true;
        min_value = min_value.min(value);
        max_value = max_value.max(value);
    }
    if any && (max_value > nyquist || min_value < -nyquist) {
        add_start = ((max_value - nyquist) / interval).ceil() as i32;
        add_end = (-(min_value + nyquist) / interval).ceil() as i32;
    }
    let start = -nyquist - add_start as f32 * interval;
    let end = nyquist + add_end as f32 * interval;
    let count = INTERVAL_SPLITS as i32 + 1 + add_start + add_end;
    if count <= 1 {
        return vec![start, end];
    }
    (0..count)
        .map(|i| start + (end - start) * i as f32 / (count - 1) as f32)
        .collect()
}

fn find_regions(
    observed: &[f32],
    rows: usize,
    gates: usize,
    limits: &[f32],
) -> (Vec<i32>, Vec<u32>) {
    let mut labels = vec![0i32; rows * gates];
    let mut region_sizes = vec![0u32];
    let mut next_label = 1i32;
    let mut stack = Vec::new();

    for pair in limits.windows(2) {
        let lmin = pair[0];
        let lmax = pair[1];
        for row in 0..rows {
            for gate in 0..gates {
                let idx = row * gates + gate;
                if labels[idx] != 0 || !in_interval(observed[idx], lmin, lmax) {
                    continue;
                }
                let label = next_label;
                next_label += 1;
                region_sizes.push(0);
                labels[idx] = label;
                stack.push((row, gate));
                while let Some((r, g)) = stack.pop() {
                    region_sizes[label as usize] += 1;
                    if r > 0 {
                        try_label_neighbor(
                            observed,
                            &mut labels,
                            rows,
                            gates,
                            r - 1,
                            g,
                            lmin,
                            lmax,
                            label,
                            &mut stack,
                        );
                    }
                    if r + 1 < rows {
                        try_label_neighbor(
                            observed,
                            &mut labels,
                            rows,
                            gates,
                            r + 1,
                            g,
                            lmin,
                            lmax,
                            label,
                            &mut stack,
                        );
                    }
                    if g > 0 {
                        try_label_neighbor(
                            observed,
                            &mut labels,
                            rows,
                            gates,
                            r,
                            g - 1,
                            lmin,
                            lmax,
                            label,
                            &mut stack,
                        );
                    }
                    if g + 1 < gates {
                        try_label_neighbor(
                            observed,
                            &mut labels,
                            rows,
                            gates,
                            r,
                            g + 1,
                            lmin,
                            lmax,
                            label,
                            &mut stack,
                        );
                    }
                }
            }
        }
    }
    (labels, region_sizes)
}

fn in_interval(value: f32, lmin: f32, lmax: f32) -> bool {
    value.is_finite() && lmin <= value && value < lmax
}

#[allow(clippy::too_many_arguments)]
fn try_label_neighbor(
    observed: &[f32],
    labels: &mut [i32],
    rows: usize,
    gates: usize,
    row: usize,
    gate: usize,
    lmin: f32,
    lmax: f32,
    label: i32,
    stack: &mut Vec<(usize, usize)>,
) {
    let idx = row * gates + gate;
    if labels[idx] == 0 && in_interval(observed[idx], lmin, lmax) {
        debug_assert!(row < rows && gate < gates);
        labels[idx] = label;
        stack.push((row, gate));
    }
}

#[derive(Clone, Copy)]
struct EdgeAccum {
    label: u32,
    neighbor: u32,
    count: u32,
    vel_sum: f64,
    nvel_sum: f64,
}

#[derive(Clone)]
struct EdgeState {
    alpha: usize,
    beta: usize,
    sum_diff: f64,
    weight: i32,
}

/// splitmix64 finaliser over a `u64` key (a packed node or label pair).
#[derive(Default, Clone, Copy)]
struct PairHasher(u64);

impl Hasher for PairHasher {
    fn finish(&self) -> u64 {
        let mut z = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(byte);
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

type PairMap<V> = HashMap<u64, V, BuildHasherDefault<PairHasher>>;

fn pair_key(a: usize, b: usize) -> u64 {
    ((a as u64) << 32) | b as u64
}

/// Py-ART's `_edge_sum_and_count`: for every pair of adjacent regions (gaps
/// of up to 100 rays or gates skipped), the number of boundary gate pairs and
/// the summed velocity difference in Nyquist intervals. Edges come out in
/// Py-ART's order: by lower label, then by higher label. Sums accumulate in
/// gate scan order per directed pair, as Py-ART's do.
fn edge_sum_and_count(
    labels: &[i32],
    observed: &[f32],
    rows: usize,
    gates: usize,
    wraps: bool,
    nyquist_interval: f32,
) -> Vec<EdgeState> {
    let mut index: PairMap<usize> = PairMap::default();
    let mut acc: Vec<EdgeAccum> = Vec::new();
    let mut add = |label: i32, neighbor: i32, vel: f32, nvel: f32| {
        if neighbor == label || neighbor <= 0 || label <= 0 {
            return;
        }
        let (label, neighbor) = (label as u32, neighbor as u32);
        let slot = *index
            .entry(pair_key(label as usize, neighbor as usize))
            .or_insert_with(|| {
                acc.push(EdgeAccum {
                    label,
                    neighbor,
                    count: 0,
                    vel_sum: 0.0,
                    nvel_sum: 0.0,
                });
                acc.len() - 1
            });
        let entry = &mut acc[slot];
        entry.count += 1;
        entry.vel_sum += f64::from(vel);
        entry.nvel_sum += f64::from(nvel);
    };
    for row in 0..rows {
        for gate in 0..gates {
            let idx = row * gates + gate;
            let label = labels[idx];
            if label == 0 {
                continue;
            }
            let vel = observed[idx];
            for step in [-1, 1] {
                if let Some(nrow) = scan_row_gap(labels, rows, gates, row, gate, step, wraps) {
                    let other = nrow * gates + gate;
                    add(label, labels[other], vel, observed[other]);
                }
            }
            for step in [-1, 1] {
                if let Some(ngate) = scan_gate_gap(labels, rows, gates, row, gate, step) {
                    let other = row * gates + ngate;
                    add(label, labels[other], vel, observed[other]);
                }
            }
        }
    }

    let mut edges: Vec<EdgeState> = acc
        .iter()
        .filter(|entry| entry.label > entry.neighbor && entry.count > 0)
        .map(|entry| EdgeState {
            alpha: entry.label as usize,
            beta: entry.neighbor as usize,
            sum_diff: (entry.vel_sum - entry.nvel_sum) / f64::from(nyquist_interval),
            weight: entry.count as i32,
        })
        .collect();
    edges.sort_unstable_by_key(|edge| (edge.beta, edge.alpha));
    edges
}

fn scan_row_gap(
    labels: &[i32],
    rows: usize,
    gates: usize,
    row: usize,
    gate: usize,
    step: isize,
    wraps: bool,
) -> Option<usize> {
    let mut check = row as isize + step;
    if check < 0 {
        if wraps {
            check = rows as isize - 1;
        } else {
            return None;
        }
    } else if check == rows as isize {
        if wraps {
            check = 0;
        } else {
            return None;
        }
    }
    if labels[check as usize * gates + gate] != 0 {
        return Some(check as usize);
    }
    for _ in 0..SKIP_BETWEEN_RAYS {
        check += step;
        if check < 0 {
            if wraps {
                check = rows as isize - 1;
            } else {
                break;
            }
        } else if check == rows as isize {
            if wraps {
                check = 0;
            } else {
                break;
            }
        }
        if labels[check as usize * gates + gate] != 0 {
            return Some(check as usize);
        }
    }
    None
}

fn scan_gate_gap(
    labels: &[i32],
    _rows: usize,
    gates: usize,
    row: usize,
    gate: usize,
    step: isize,
) -> Option<usize> {
    let mut check = gate as isize + step;
    if check < 0 || check == gates as isize {
        return None;
    }
    if labels[row * gates + check as usize] != 0 {
        return Some(check as usize);
    }
    for _ in 0..SKIP_ALONG_RAY {
        check += step;
        if check < 0 || check == gates as isize {
            break;
        }
        if labels[row * gates + check as usize] != 0 {
            return Some(check as usize);
        }
    }
    None
}

struct RegionTracker {
    node_size: Vec<u32>,
    original_region_sizes: Vec<u32>,
    regions_in_node: Vec<Vec<usize>>,
    unwrap_number: Vec<i32>,
}

impl RegionTracker {
    fn new(region_sizes: Vec<u32>) -> Self {
        let nregions = region_sizes.len();
        Self {
            node_size: region_sizes.clone(),
            original_region_sizes: region_sizes,
            regions_in_node: (0..nregions).map(|region| vec![region]).collect(),
            unwrap_number: vec![0; nregions],
        }
    }

    fn merge_nodes(&mut self, node_a: usize, node_b: usize) {
        let regions_to_merge = std::mem::take(&mut self.regions_in_node[node_b]);
        self.regions_in_node[node_a].extend(regions_to_merge);
        self.node_size[node_a] += self.node_size[node_b];
        self.node_size[node_b] = 0;
    }

    fn unwrap_node(&mut self, node: usize, nwrap: i32) {
        if nwrap == 0 {
            return;
        }
        for &region in &self.regions_in_node[node] {
            self.unwrap_number[region] += nwrap;
        }
    }

    fn get_node_size(&self, node: usize) -> u32 {
        self.node_size[node]
    }
}

/// Py-ART's `_EdgeTracker`, with the same edge selection, merge order and
/// floating-point sums in O(E log E):
///
/// - the strongest live edge comes from a max-heap ordered by weight, then
///   lowest index, as `np.argmax` over the weight array picks it;
/// - the base node's edge to a neighbour is found in a map keyed by the node
///   pair, where Py-ART marks every neighbour of a new base node;
/// - Py-ART turns every edge of a new base node to point away from it; here
///   that turn is recorded as the node's scan time and applied when an edge
///   is next read (an edge points away from whichever endpoint was scanned
///   last after the edge was last written);
/// - removed edges stay in the per-node lists and are skipped.
struct EdgeTracker {
    edges: Vec<TrackedEdge>,
    edges_in_node: Vec<Vec<usize>>,
    by_pair: PairMap<usize>,
    scan_time: Vec<u64>,
    clock: u64,
    last_base_node: Option<usize>,
    heap: BinaryHeap<(i32, Reverse<usize>)>,
}

/// An edge as stored: `sum_diff` is from `alpha` to `beta` as of `written`.
#[derive(Clone)]
struct TrackedEdge {
    alpha: usize,
    beta: usize,
    sum_diff: f64,
    weight: i32,
    written: u64,
}

/// Weight of a removed edge (Py-ART's marker).
const DEAD_EDGE: i32 = -999;

impl EdgeTracker {
    fn new(edges: Vec<EdgeState>, nnodes: usize) -> Self {
        let mut edges_in_node = vec![Vec::new(); nnodes];
        let mut by_pair = PairMap::default();
        by_pair.reserve(edges.len());
        for (edge_index, edge) in edges.iter().enumerate() {
            edges_in_node[edge.alpha].push(edge_index);
            edges_in_node[edge.beta].push(edge_index);
            by_pair.insert(unordered_key(edge.alpha, edge.beta), edge_index);
        }
        let heap = edges
            .iter()
            .enumerate()
            .map(|(index, edge)| (edge.weight, Reverse(index)))
            .collect();
        Self {
            edges: edges
                .into_iter()
                .map(|edge| TrackedEdge {
                    alpha: edge.alpha,
                    beta: edge.beta,
                    sum_diff: edge.sum_diff,
                    weight: edge.weight,
                    written: 0,
                })
                .collect(),
            edges_in_node,
            by_pair,
            scan_time: vec![0; nnodes],
            clock: 0,
            last_base_node: None,
            heap,
        }
    }

    /// The edge as Py-ART holds it now: (alpha, beta, sum_diff).
    fn oriented(&self, edge: usize) -> (usize, usize, f64) {
        let stored = &self.edges[edge];
        let (alpha_scan, beta_scan) = (self.scan_time[stored.alpha], self.scan_time[stored.beta]);
        let turned =
            (alpha_scan > stored.written || beta_scan > stored.written) && beta_scan > alpha_scan;
        if turned {
            (stored.beta, stored.alpha, -stored.sum_diff)
        } else {
            (stored.alpha, stored.beta, stored.sum_diff)
        }
    }

    fn write(&mut self, edge: usize, alpha: usize, beta: usize, sum_diff: f64) {
        let clock = self.clock;
        let stored = &mut self.edges[edge];
        stored.alpha = alpha;
        stored.beta = beta;
        stored.sum_diff = sum_diff;
        stored.written = clock;
    }

    fn pop_edge(&mut self) -> Option<(usize, usize, f64, usize)> {
        while let Some((weight, Reverse(index))) = self.heap.pop() {
            if self.edges[index].weight != weight || weight < 0 {
                continue; // superseded by a later push, or removed
            }
            let (alpha, beta, sum_diff) = self.oriented(index);
            return Some((alpha, beta, sum_diff / f64::from(weight), index));
        }
        None
    }

    fn merge_nodes(&mut self, base_node: usize, merge_node: usize, foo_edge: usize) {
        self.edges[foo_edge].weight = DEAD_EDGE;
        self.by_pair.remove(&unordered_key(base_node, merge_node));
        if self.last_base_node != Some(base_node) {
            self.clock += 1;
            self.scan_time[base_node] = self.clock;
        }

        let edges_in_merge = std::mem::take(&mut self.edges_in_node[merge_node]);
        for edge_num in edges_in_merge {
            if self.edges[edge_num].weight < 0 {
                continue;
            }
            let (alpha, beta, sum_diff) = self.oriented(edge_num);
            let (neighbor, sum_diff) = if alpha == merge_node {
                (beta, sum_diff)
            } else {
                debug_assert_eq!(beta, merge_node);
                (alpha, -sum_diff)
            };
            self.by_pair.remove(&unordered_key(merge_node, neighbor));
            let key = unordered_key(base_node, neighbor);
            if let Some(&base_edge) = self.by_pair.get(&key) {
                // Py-ART's combine: the base node's edge takes the merged one.
                let (base_alpha, base_beta, base_sum) = self.oriented(base_edge);
                debug_assert_eq!(base_alpha, base_node);
                let weight = self.edges[base_edge].weight + self.edges[edge_num].weight;
                self.edges[edge_num].weight = DEAD_EDGE;
                self.write(base_edge, base_alpha, base_beta, base_sum + sum_diff);
                self.edges[base_edge].weight = weight;
                self.heap.push((weight, Reverse(base_edge)));
            } else {
                self.write(edge_num, base_node, neighbor, sum_diff);
                self.by_pair.insert(key, edge_num);
                self.edges_in_node[base_node].push(edge_num);
            }
        }
        self.last_base_node = Some(base_node);
    }

    fn unwrap_node(&mut self, node: usize, nwrap: i32) {
        if nwrap == 0 {
            return;
        }
        let edges = std::mem::take(&mut self.edges_in_node[node]);
        for &edge_index in &edges {
            let weight = self.edges[edge_index].weight;
            if weight < 0 {
                continue;
            }
            let delta = f64::from(weight) * f64::from(nwrap);
            let (alpha, beta, sum_diff) = self.oriented(edge_index);
            let sum_diff = if node == alpha {
                sum_diff + delta
            } else {
                debug_assert_eq!(node, beta);
                sum_diff - delta
            };
            self.write(edge_index, alpha, beta, sum_diff);
        }
        self.edges_in_node[node] = edges;
    }
}

fn unordered_key(a: usize, b: usize) -> u64 {
    pair_key(a.min(b), a.max(b))
}

fn round_ties_even_to_i32(value: f64) -> i32 {
    value.round_ties_even() as i32
}

#[cfg(test)]
mod tests {
    //! The port against Py-ART's own output on real Level II sweeps: the
    //! goldens in `tests/golden/` are `pyart.correct.dealias_region_based`
    //! folds written by `tools/correct_golden.py`.

    use super::*;
    use crate::dealias_velocity;
    use crate::real_data::{self, VelocitySweep, fold_agreement, golden_volume, grid_folds};

    /// Every golden sweep with a Nyquist velocity.
    const PYART_GOLDEN_SWEEPS: [&str; 15] = [
        "kbox_20220129_trim_s1",
        "kdvn_20200810_s1",
        "kdvn_20200810_trim_s1",
        "kilx_20260418_s1",
        "klix_20050829_trim_s1",
        "klix_20210829_s1",
        "klix_20210829_s13",
        "klix_20210829_s2",
        "klix_20210829_s9",
        "klix_20210829_trim_s1",
        "ktlx_20130520_s1",
        "ktlx_20130520_s3",
        "ktlx_20130520_trim_s1",
        "ktlx_20240315_s9",
        "pahg_20250909_s1",
    ];

    /// On every golden sweep the port's fold equals Py-ART's at every gate,
    /// with no global offset (both centre the sweep on its mean fold). The
    /// crate's default region engine ([`dealias_velocity`]) is printed
    /// alongside for comparison.
    #[test]
    fn port_matches_pyart_at_every_gate() {
        for case in PYART_GOLDEN_SWEEPS {
            let Some((volume, golden)) = golden_volume(case) else {
                continue;
            };
            let cut = &volume.sweeps[golden.sweep];
            let sweep = VelocitySweep::of_sweep(cut);
            let pyart = golden.aligned_folds(cut, &sweep);
            let port = grid_folds(
                &sweep,
                &dealias_velocity_pyart_region(cut, real_data::velocity(cut)),
            );
            let agreement = fold_agreement(&sweep, &port, &pyart, golden.rays_wrap_around);
            let region = grid_folds(&sweep, &dealias_velocity(cut, real_data::velocity(cut)));
            let region = fold_agreement(&sweep, &region, &pyart, golden.rays_wrap_around);
            eprintln!(
                "{case}: {} gates; default region engine differs at {} ({} per echo)",
                agreement.compared,
                region.compared - region.agreeing,
                region.compared - region.component_agreeing
            );
            assert_eq!(agreement.compared, golden.valid_gates, "{case}");
            assert_eq!(agreement.offset, 0, "{case}");
            assert_eq!(agreement.agreeing, agreement.compared, "{case}");
            assert_eq!(agreement.engine_breaks, 0, "{case}");
        }
    }

    /// The Moore EF5 couplet on KTLX's 0.48 deg cut, 22.4-22.9 km out: the
    /// gates Py-ART unfolds to outbound north of the vortex centre (az
    /// 266.6-269.4 deg) are outbound in the port's output, and the inbound
    /// side (az 264.0-265.9 deg) stays inbound, so the couplet keeps its
    /// cyclonic sign (measured: 18 of 18 outbound, 12 of 12 inbound). The
    /// default region engine's count is printed but not asserted: it keeps 4
    /// of the 18 outbound, because its half-Nyquist join chains both sides
    /// into one region through the small folded steps of the core (see
    /// `docs/design/retrievals-validation.md`).
    #[test]
    fn moore_couplet_keeps_its_outbound_side() {
        let Some((volume, golden)) = golden_volume("ktlx_20130520_s1") else {
            return;
        };
        let cut = &volume.sweeps[golden.sweep];
        let sweep = VelocitySweep::of_sweep(cut);
        let pyart = golden.aligned_folds(cut, &sweep);
        let port = dealias_velocity_pyart_region(cut, real_data::velocity(cut));
        let region = dealias_velocity(cut, real_data::velocity(cut));
        let gate_of =
            |range_m: f64| ((range_m - sweep.first_gate_m) / sweep.gate_spacing_m).round() as usize;
        let gates = gate_of(22_375.0)..=gate_of(22_875.0);
        let rows_between = |low: f32, high: f32| -> Vec<usize> {
            (0..sweep.rows)
                .filter(|&row| (low..=high).contains(&sweep.azimuths[row]))
                .collect()
        };
        // (Py-ART outbound, kept outbound, Py-ART inbound, kept inbound)
        let side = |rows: &[usize], field: &Field| -> (usize, usize, usize, usize) {
            let mut counts = (0, 0, 0, 0);
            for &row in rows {
                for gate in gates.clone() {
                    let idx = row * sweep.gates + gate;
                    let (Some(fold), Some(value)) = (pyart[idx], field.value(row, gate)) else {
                        continue;
                    };
                    let truth = sweep.unfolded(idx, fold);
                    counts.0 += usize::from(truth > 0.0);
                    counts.1 += usize::from(truth > 0.0 && value > 0.0);
                    counts.2 += usize::from(truth < 0.0);
                    counts.3 += usize::from(truth < 0.0 && value < 0.0);
                }
            }
            counts
        };
        let north = rows_between(266.6, 269.4);
        let south = rows_between(264.0, 265.9);
        let (north_out, port_out, _, _) = side(&north, &port);
        let (_, _, south_in, port_in) = side(&south, &port);
        let (_, region_out, _, _) = side(&north, &region);
        eprintln!(
            "north: {north_out} gates outbound in Py-ART, port keeps {port_out}, \
             default region engine {region_out}; south: {south_in} inbound, port keeps {port_in}"
        );
        assert!(north_out >= 15, "{north_out}");
        assert_eq!(port_out, north_out);
        assert!(south_in >= 10, "{south_in}");
        assert_eq!(port_in, south_in);
    }
}
