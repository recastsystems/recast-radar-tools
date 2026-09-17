//! Super-regions: strong/weak edge classification over the v1 vote graph.
//!
//! The v1 solver welds every resolved vote edge into a rigid body, so one
//! spurious low-support contact can misbranch an internally-consistent
//! subgraph (failure F3, `docs/dealias-fold-branch-analysis.md` root-cause 4:
//! KMBX region 600, 826 gates, +1 fold → +29.1 m/s outbound, wrong).  v4
//! keeps only STRONG edges as rigid unions; everything else becomes a soft
//! pairwise term in the volume energy (spec §5.1), so external evidence can
//! pull a misbranched subgraph back independently.
//!
//! INVARIANT: super-region ids are assigned in region-id order and the weak
//! edge list preserves the deterministic order of `RegionSolve::edges`
//! (strongest boundary first, then region-pair) — the volume solve is
//! byte-reproducible.

use crate::region_core::RegionSolve;

/// A vote edge is STRONG when its majority fold has this much boundary
/// support...
pub(crate) const STRONG_EDGE_MIN_SUPPORT: u32 = 12;
/// ...and this share of all votes on the edge.  Everything else is weak —
/// including every edge class the F3 post-mortem showed can silently
/// misbranch a subgraph.
pub(crate) const STRONG_EDGE_MIN_SHARE: f64 = 0.80;
/// Super-regions smaller than this skip the volume graph: they keep their
/// local v1 solve and are handled by the repair gauntlet (too few gates to
/// accumulate meaningful unary evidence).
pub(crate) const SUPERREGION_MIN_GATES: u64 = 64;
/// A seam is only unambiguous when its mean |Δv| sits within this many
/// folds of a whole fold count.  Near the half-fold mark every boundary
/// pair rounds the same (possibly wrong) way, so vote share alone reads as
/// unanimous — the measured KMBX weld class (mean jump ≈ 0.7 folds,
/// `docs/dealias-fold-branch-analysis.md` root-cause 4).  Such seams stay
/// soft no matter how much support they carry.
pub(crate) const STRONG_EDGE_MAX_FOLD_AMBIGUITY: f32 = 0.30;

/// A weak vote edge lifted to super-region granularity.  The energy charges
/// `λ_wk · share · min(|residual + k_hi − k_lo|, 2)` (spec §5.2): zero when
/// the final labels honor the observed boundary fold.
pub(crate) struct WeakEdge {
    pub(crate) lo_super: u32,
    pub(crate) hi_super: u32,
    /// `(v1 fold of hi region) − (v1 fold of lo region) − edge fold`: the
    /// boundary-fold violation already present in the v1 baseline, so the
    /// label term only charges *additional* violation.
    pub(crate) residual: i32,
    /// Winning-vote share of the edge, in (0, 1].
    pub(crate) share: f64,
}

/// Per-tilt super-region decomposition.
pub(crate) struct TiltSuperRegions {
    /// Super-region id per v1 region (compact, assigned in region-id order).
    pub(crate) super_of_region: Vec<u32>,
    /// Total gate count per super-region.
    pub(crate) super_gates: Vec<u64>,
    /// Weak edges whose endpoints landed in different super-regions.
    pub(crate) weak_edges: Vec<WeakEdge>,
}

impl TiltSuperRegions {
    pub(crate) fn super_count(&self) -> usize {
        self.super_gates.len()
    }
}

/// Build super-regions for one tilt from the v1 solve (spec §5.1): connected
/// components of regions over strong edges only, weak edges kept as soft
/// terms.  Interval-based region splitting (Helmus & Collis 2016) was tested
/// against F2 on the real KMBX volume and did not fix the branch (the bad
/// branch survives the vote graph — analysis doc root-cause 3), so v4 relies
/// on weak-edge softening + external unaries instead.
pub(crate) fn build_super_regions(solve: &RegionSolve) -> TiltSuperRegions {
    let region_count = solve.region_size.len();
    let mut union = crate::region_core::UnionFind::new(region_count);
    for edge in &solve.edges {
        if edge_is_strong(edge) {
            union.union(edge.lo, edge.hi);
        }
    }

    // Compact super ids in region-id order (deterministic).
    let mut super_of_region = vec![u32::MAX; region_count];
    let mut super_gates: Vec<u64> = Vec::new();
    let mut root_to_super: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for rid in 0..region_count as u32 {
        let root = union.find(rid);
        let next = super_gates.len() as u32;
        let sid = *root_to_super.entry(root).or_insert_with(|| {
            super_gates.push(0);
            next
        });
        super_of_region[rid as usize] = sid;
        super_gates[sid as usize] += u64::from(solve.region_size[rid as usize]);
    }

    // Weak edges between distinct super-regions become soft pairwise terms.
    // Seams whose mean jump is a rounding coin-flip carry NO branch
    // information and are dropped outright: their "residual repair" pressure
    // otherwise mass-shifts split subtrees toward whichever way the noise
    // rounded (measured on KEAX after interval splitting: 264 → 3090
    // boundary pairs without an environmental profile).
    let mut weak_edges = Vec::new();
    for edge in &solve.edges {
        if edge_is_strong(edge) {
            continue;
        }
        if (edge.mean_jump_folds - edge.mean_jump_folds.round()).abs()
            > STRONG_EDGE_MAX_FOLD_AMBIGUITY
        {
            continue;
        }
        let lo_super = super_of_region[edge.lo as usize];
        let hi_super = super_of_region[edge.hi as usize];
        if lo_super == hi_super {
            continue; // intra-super violation is constant under the labels
        }
        let residual =
            solve.region_fold[edge.hi as usize] - solve.region_fold[edge.lo as usize] - edge.fold;
        let share = if edge.total_votes > 0 {
            f64::from(edge.winning_votes) / f64::from(edge.total_votes)
        } else {
            0.0
        };
        weak_edges.push(WeakEdge {
            lo_super,
            hi_super,
            residual,
            share,
        });
    }

    TiltSuperRegions {
        super_of_region,
        super_gates,
        weak_edges,
    }
}

/// Strong = high support, high vote share, AND an unambiguous jump (see the
/// constants above): only such seams may rigidly weld two regions.
fn edge_is_strong(edge: &crate::region_core::RegionEdge) -> bool {
    let unambiguous = (edge.mean_jump_folds - edge.mean_jump_folds.round()).abs()
        <= STRONG_EDGE_MAX_FOLD_AMBIGUITY;
    edge.winning_votes >= STRONG_EDGE_MIN_SUPPORT
        && f64::from(edge.winning_votes) >= STRONG_EDGE_MIN_SHARE * f64::from(edge.total_votes)
        && unambiguous
}

#[cfg(test)]
mod tests {
    //! Super-region tests on the region solve of real Level II sweeps.
    //! Fold votes are checked against Py-ART 2.2.5 `dealias_region_based`
    //! on the same sweep (`tools/correct_golden.py`); the partition is checked
    //! against an independent union-find over the documented edge rule.

    use super::*;
    use crate::real_data::{VelocitySweep, golden_volume};
    use crate::region_core::{RegionEdge, solve_region_folds};

    fn sweep_solve(case: &str) -> Option<(VelocitySweep, RegionSolve, Vec<Option<i32>>)> {
        let (volume, golden) = golden_volume(case)?;
        let cut = &volume.sweeps[golden.sweep];
        let sweep = VelocitySweep::of_sweep(cut);
        let pyart = golden.aligned_folds(cut, &sweep);
        let solve = solve_region_folds(
            &sweep.observed,
            &sweep.nyq,
            sweep.rows,
            sweep.gates,
            &sweep.azimuths,
        );
        Some((sweep, solve, pyart))
    }

    /// The documented strong-edge rule (support >= 12, share >= 0.8, mean
    /// jump within 0.3 of a whole fold), written out independently.
    fn documented_strong(edge: &RegionEdge) -> bool {
        edge.winning_votes >= 12
            && 5 * edge.winning_votes >= 4 * edge.total_votes
            && (edge.mean_jump_folds - edge.mean_jump_folds.round()).abs() <= 0.30
    }

    /// Super-region partition from a plain union-find over strong edges,
    /// as a canonical label per region (smallest region id in its set).
    fn reference_partition(solve: &RegionSolve) -> Vec<u32> {
        let mut parent: Vec<u32> = (0..solve.region_size.len() as u32).collect();
        fn root(parent: &mut [u32], mut x: u32) -> u32 {
            while parent[x as usize] != x {
                x = parent[x as usize];
            }
            x
        }
        for edge in solve.edges.iter().filter(|edge| documented_strong(edge)) {
            let (a, b) = (root(&mut parent, edge.lo), root(&mut parent, edge.hi));
            let (keep, drop) = (a.min(b), a.max(b));
            parent[drop as usize] = keep;
        }
        (0..parent.len() as u32)
            .map(|x| root(&mut parent, x))
            .collect()
    }

    /// Majority Py-ART fold of every region.
    fn region_pyart_folds(solve: &RegionSolve, pyart: &[Option<i32>]) -> Vec<Option<i32>> {
        let mut votes: Vec<std::collections::BTreeMap<i32, usize>> =
            vec![Default::default(); solve.region_size.len()];
        for (idx, &rid) in solve.region_of.iter().enumerate() {
            if let (true, Some(fold)) = (rid != u32::MAX, pyart[idx]) {
                *votes[rid as usize].entry(fold).or_default() += 1;
            }
        }
        votes
            .into_iter()
            .map(|counts| {
                counts
                    .into_iter()
                    .max_by_key(|(_, count)| *count)
                    .map(|(fold, _)| fold)
            })
            .collect()
    }

    /// The derecho sector's region graph has many seams touching along a
    /// single gate pair. Such an edge (support 1 < 12) is weak: it never welds
    /// its regions, and when they end up in different super-regions and its
    /// jump is unambiguous it survives only as a soft weak edge. The whole
    /// partition must equal the union-find over strong edges.
    #[test]
    fn single_contact_edge_is_weak_and_splits_super_regions() {
        let Some((_, solve, _)) = sweep_solve("kdvn_20200810_trim_s1") else {
            return;
        };
        let supers = build_super_regions(&solve);
        let reference = reference_partition(&solve);
        for (region, &representative) in reference.iter().enumerate() {
            assert_eq!(
                supers.super_of_region[region], supers.super_of_region[representative as usize],
                "region {region} split from its strong set"
            );
        }
        let distinct_reference: std::collections::BTreeSet<u32> =
            reference.iter().copied().collect();
        assert_eq!(
            supers.super_count(),
            distinct_reference.len(),
            "super-regions vs strong components"
        );

        let single_contacts: Vec<&RegionEdge> = solve
            .edges
            .iter()
            .filter(|edge| edge.total_votes == 1)
            .collect();
        let split = single_contacts
            .iter()
            .filter(|edge| {
                supers.super_of_region[edge.lo as usize] != supers.super_of_region[edge.hi as usize]
            })
            .count();
        let expected_weak: Vec<(u32, u32)> = solve
            .edges
            .iter()
            .filter(|edge| !documented_strong(edge))
            .filter(|edge| (edge.mean_jump_folds - edge.mean_jump_folds.round()).abs() <= 0.30)
            .map(|edge| {
                (
                    supers.super_of_region[edge.lo as usize],
                    supers.super_of_region[edge.hi as usize],
                )
            })
            .filter(|(lo, hi)| lo != hi)
            .collect();
        let weak: Vec<(u32, u32)> = supers
            .weak_edges
            .iter()
            .map(|edge| (edge.lo_super, edge.hi_super))
            .collect();
        let single_weak = supers
            .weak_edges
            .iter()
            .filter(|edge| (edge.share - 1.0).abs() < 1e-12)
            .count();
        eprintln!(
            "regions {}, supers {}, edges {}, single contacts {} ({split} across super-regions), weak edges {} ({single_weak} unanimous)",
            solve.region_size.len(),
            supers.super_count(),
            solve.edges.len(),
            single_contacts.len(),
            weak.len()
        );
        assert!(
            single_contacts.len() >= 1_000,
            "single-contact edges {}",
            single_contacts.len()
        );
        assert!(
            split >= 1_000,
            "single contacts across super-regions {split}"
        );
        assert!(single_contacts.iter().all(|edge| !documented_strong(edge)));
        assert_eq!(
            weak, expected_weak,
            "weak edges are the unambiguous weak seams across super-regions, in edge order"
        );
        assert!(supers.super_count() < solve.region_size.len());
    }

    /// The KBOX blizzard sector: seams with long unanimous boundaries weld
    /// their regions into one super-region, and their fold vote is the fold
    /// Py-ART finds between the same two regions.
    #[test]
    fn high_support_edge_welds_a_super_region() {
        let Some((_, solve, pyart)) = sweep_solve("kbox_20220129_trim_s1") else {
            return;
        };
        let supers = build_super_regions(&solve);
        let region_folds = region_pyart_folds(&solve, &pyart);
        let strong: Vec<&RegionEdge> = solve
            .edges
            .iter()
            .filter(|edge| documented_strong(edge))
            .collect();
        let (mut folding, mut folding_agree, mut agree) = (0, 0, 0);
        for edge in &strong {
            assert_eq!(
                supers.super_of_region[edge.lo as usize], supers.super_of_region[edge.hi as usize],
                "strong edge {}-{} must weld",
                edge.lo, edge.hi
            );
            let pyart_fold = region_folds[edge.hi as usize]
                .zip(region_folds[edge.lo as usize])
                .map(|(hi, lo)| hi - lo);
            agree += usize::from(pyart_fold == Some(edge.fold));
            if edge.fold != 0 {
                folding += 1;
                folding_agree += usize::from(pyart_fold == Some(edge.fold));
            }
        }
        let welded_across_folds = supers
            .weak_edges
            .iter()
            .filter(|edge| edge.lo_super == edge.hi_super)
            .count();
        eprintln!(
            "strong edges {}, fold vote = Py-ART on {agree}; folding strong edges {folding}, Py-ART agrees on {folding_agree}",
            strong.len()
        );
        assert_eq!(
            welded_across_folds, 0,
            "no weak edge inside one super-region"
        );
        assert!(strong.len() >= 100, "strong edges {}", strong.len());
        assert!(folding >= 100, "strong edges carrying a fold {folding}");
        assert!(
            agree as f64 >= 0.99 * strong.len() as f64,
            "fold votes matching Py-ART {agree}"
        );
        assert!(
            folding_agree as f64 >= 0.99 * folding as f64,
            "folding votes matching Py-ART {folding_agree}"
        );
    }
}
