//! Test support: real Level II velocity sweeps from the corpus
//! (`recast-radar-testdata`) and the Py-ART region-based dealiasing goldens
//! written by `tools/correct_golden.py` into `tests/golden/`.
//!
//! Nothing here fabricates radar data. Volumes come from decoding corpus
//! files; windows are sub-rectangles of decoded sweeps; the expected folds
//! come from Py-ART 2.2.5 run on the same files.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use chrono::{DateTime, Utc};
use recast_radar_core::{ElevationCut, MomentGrid, MomentType, RadarVolume};

use crate::{EnvWindLevel, EnvironmentalWindProfile};

/// Decoded volume for a corpus id, decoded once per test process. `None`
/// (after printing why) when the file is not committed, not cached and cannot
/// be downloaded: the caller skips.
pub(crate) fn corpus_volume(id: &str) -> Option<Arc<RadarVolume>> {
    type DecodeOnce = Arc<OnceLock<Arc<RadarVolume>>>;
    static VOLUMES: OnceLock<Mutex<HashMap<String, DecodeOnce>>> = OnceLock::new();
    let path = match recast_radar_testdata::path(id) {
        Ok(path) => path,
        Err(error) if error.is_offline() => {
            eprintln!("skipping: {error}");
            return None;
        }
        Err(error) => panic!("{error}"),
    };
    let slot = VOLUMES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(id.to_owned())
        .or_default()
        .clone();
    Some(
        slot.get_or_init(|| {
            Arc::new(
                recast_radar_io_nexrad::decode_volume_from_path(&path)
                    .unwrap_or_else(|error| panic!("decode {id}: {error}")),
            )
        })
        .clone(),
    )
}

/// A real model wind profile from `crates/recast-radar-bench/fixtures/dealias/`
/// (HRRR / RAP analyses at the radar site, see each fixture's `source`).
pub(crate) fn environment_fixture(name: &str) -> EnvironmentalWindProfile {
    let path = format!(
        "{}/../recast-radar-bench/fixtures/dealias/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let json: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path}: {e}"));
    let number = |value: &serde_json::Value, key: &str| -> f32 {
        value[key]
            .as_f64()
            .unwrap_or_else(|| panic!("{path}: `{key}` is not a number")) as f32
    };
    let valid_time = json["valid_time"]
        .as_str()
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .unwrap_or_else(|| panic!("{path}: bad `valid_time`"))
        .with_timezone(&Utc);
    let levels = json["levels"]
        .as_array()
        .unwrap_or_else(|| panic!("{path}: no `levels`"))
        .iter()
        .map(|level| EnvWindLevel {
            height_m_arl: number(level, "height_m_arl"),
            u_mps: number(level, "u_mps"),
            v_mps: number(level, "v_mps"),
        })
        .collect();
    EnvironmentalWindProfile { levels, valid_time }
}

/// The velocity grid of a decoded cut.
pub(crate) fn velocity(cut: &ElevationCut) -> &MomentGrid {
    cut.moments
        .get(&MomentType::Velocity)
        .unwrap_or_else(|| panic!("cut at {:.2} deg has no velocity", cut.elevation_deg))
}

/// A decoded velocity sweep as the fold solvers see it: observed m/s (NaN for
/// no data), per-row Nyquist (NaN unknown), per-row azimuth.
pub(crate) struct VelocitySweep {
    pub(crate) rows: usize,
    pub(crate) gates: usize,
    /// Range of gate 0 and gate spacing (m).
    pub(crate) first_gate_m: i32,
    pub(crate) gate_spacing_m: i32,
    pub(crate) observed: Vec<f32>,
    pub(crate) nyq: Vec<f32>,
    pub(crate) azimuths: Vec<f32>,
}

impl VelocitySweep {
    /// Observed velocities and radial metadata of `cut`, read exactly as the
    /// engines read them (`copy_scaled_velocity_row`, per-row Nyquist with
    /// the sweep median as fallback).
    pub(crate) fn of_cut(cut: &ElevationCut) -> Self {
        let grid = velocity(cut);
        let rows = grid.radial_count();
        let gates = grid.gate_range.gate_count;
        let mut observed = Vec::with_capacity(rows * gates);
        let mut row_values: Vec<f32> = std::iter::repeat_n(f32::NAN, gates).collect();
        for row in 0..rows {
            crate::copy_scaled_velocity_row(grid, row, &mut row_values);
            observed.extend_from_slice(&row_values);
        }
        let fallback = crate::median_nyquist_mps(cut, grid);
        let nyq = (0..rows)
            .map(|row| {
                crate::row_nyquist_mps(cut, grid, row)
                    .or(fallback)
                    .filter(|n| n.is_finite() && *n > 0.0)
                    .unwrap_or(f32::NAN)
            })
            .collect();
        Self {
            rows,
            gates,
            first_gate_m: grid.gate_range.first_gate_m,
            gate_spacing_m: grid.gate_range.gate_spacing_m,
            observed,
            nyq,
            azimuths: crate::radial_azimuths(cut, grid),
        }
    }

    pub(crate) fn finite_gates(&self) -> usize {
        self.observed.iter().filter(|v| v.is_finite()).count()
    }

    /// Unfolded value of gate `idx` for integer fold `fold`.
    pub(crate) fn unfolded(&self, idx: usize, fold: i32) -> f32 {
        self.observed[idx] + 2.0 * self.nyq[idx / self.gates] * fold as f32
    }
}

/// Per-gate integer fold of a dealiased grid against the observed sweep:
/// `round((dealiased - observed) / 2N)`, `None` where either is missing.
pub(crate) fn grid_folds(sweep: &VelocitySweep, dealiased: &MomentGrid) -> Vec<Option<i32>> {
    assert_eq!(dealiased.radial_count(), sweep.rows, "dealiased rows");
    assert_eq!(
        dealiased.gate_range.gate_count, sweep.gates,
        "dealiased gates"
    );
    (0..sweep.rows * sweep.gates)
        .map(|idx| {
            let observed = sweep.observed[idx];
            let n = sweep.nyq[idx / sweep.gates];
            let value = dealiased.scaled_value(idx / sweep.gates, idx % sweep.gates)?;
            (observed.is_finite() && n.is_finite())
                .then(|| ((value - observed) / (2.0 * n)).round() as i32)
        })
        .collect()
}

/// One ray of a Py-ART golden.
pub(crate) struct GoldenRay {
    pub(crate) azimuth_deg: f32,
    pub(crate) nyquist_mps: f32,
    pub(crate) valid: usize,
    pub(crate) gate_index_sum: u64,
    pub(crate) raw_sum: f64,
    /// Py-ART fold of each valid gate, in gate order.
    pub(crate) folds: Vec<i32>,
}

/// `tests/golden/<case>.txt` (see `tools/correct_golden.py`).
pub(crate) struct PyartGolden {
    pub(crate) case: String,
    pub(crate) id: String,
    pub(crate) sweep: usize,
    pub(crate) first_gate_m: f32,
    pub(crate) gate_spacing_m: f32,
    pub(crate) rays_wrap_around: bool,
    pub(crate) nyquist_mps: f32,
    pub(crate) valid_gates: usize,
    pub(crate) unfolded_gates: usize,
    /// Sweeps in the file, and those with any valid velocity gate (Py-ART).
    pub(crate) file_sweeps: usize,
    pub(crate) file_velocity_sweeps: usize,
    /// For goldens with an environmental wind fixture: the global fold offset
    /// that puts Py-ART's output closest to the fixture's wind projected on the
    /// sweep, and how many valid gates then lie within one Nyquist of it.
    pub(crate) env: Option<GoldenEnvironment>,
    pub(crate) rays: Vec<GoldenRay>,
}

pub(crate) struct GoldenEnvironment {
    pub(crate) fixture: String,
    pub(crate) offset: i32,
    pub(crate) within_nyquist: usize,
}

impl PyartGolden {
    pub(crate) fn load(case: &str) -> Self {
        let path = format!("{}/tests/golden/{case}.txt", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let mut header: HashMap<&str, &str> = HashMap::new();
        let mut lines = text.lines().filter(|line| !line.starts_with('#'));
        for line in lines.by_ref() {
            if line == "rays:" {
                break;
            }
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            header.insert(key, value);
        }
        let field = |key: &str| -> &str {
            header
                .get(key)
                .copied()
                .unwrap_or_else(|| panic!("{path}: missing `{key}`"))
        };
        let number = |key: &str| -> f64 {
            field(key)
                .parse()
                .unwrap_or_else(|e| panic!("{path}: `{key}`: {e}"))
        };
        let rays: Vec<GoldenRay> = lines
            .map(|line| {
                let mut words = line.split(' ');
                let mut next = |what: &str| {
                    words
                        .next()
                        .unwrap_or_else(|| panic!("{path}: ray line without {what}: {line}"))
                };
                let _ray = next("index");
                let parse = |text: &str| -> f64 {
                    text.parse()
                        .unwrap_or_else(|e| panic!("{path}: `{text}`: {e}"))
                };
                let azimuth_deg = parse(next("azimuth")) as f32;
                let nyquist_mps = parse(next("nyquist")) as f32;
                let valid = parse(next("valid")) as usize;
                let gate_index_sum = parse(next("gate index sum")) as u64;
                let raw_sum = parse(next("raw sum"));
                let mut folds = Vec::with_capacity(valid);
                for run in words.filter(|w| !w.is_empty()) {
                    let (count, fold) = run
                        .split_once('*')
                        .unwrap_or_else(|| panic!("{path}: bad run `{run}`"));
                    let count = parse(count) as usize;
                    folds.extend(std::iter::repeat_n(parse(fold) as i32, count));
                }
                assert!(
                    folds.is_empty() || folds.len() == valid,
                    "{path}: ray runs cover {} of {valid} gates",
                    folds.len()
                );
                GoldenRay {
                    azimuth_deg,
                    nyquist_mps,
                    valid,
                    gate_index_sum,
                    raw_sum,
                    folds,
                }
            })
            .collect();
        let golden = Self {
            case: field("case").to_owned(),
            id: field("id").to_owned(),
            sweep: number("sweep") as usize,
            first_gate_m: number("first_gate_m") as f32,
            gate_spacing_m: number("gate_spacing_m") as f32,
            rays_wrap_around: field("rays_wrap_around") == "true",
            nyquist_mps: number("nyquist_mps") as f32,
            valid_gates: number("valid_gates") as usize,
            unfolded_gates: number("unfolded_gates") as usize,
            file_sweeps: number("file_sweeps") as usize,
            file_velocity_sweeps: number("file_velocity_sweeps") as usize,
            env: header
                .contains_key("env_fixture")
                .then(|| GoldenEnvironment {
                    fixture: field("env_fixture").to_owned(),
                    offset: number("env_offset") as i32,
                    within_nyquist: number("env_within_nyquist") as usize,
                }),
            rays,
        };
        assert_eq!(
            golden.rays.len(),
            number("rays") as usize,
            "{path}: ray count"
        );
        golden
    }

    /// Checks that `sweep` (decoded by the Rust reader from `self.id`) is the
    /// sweep Py-ART read, ray for ray and gate for gate: same ray count and
    /// azimuths, and in every ray the same number of valid gates at the same
    /// ranges (gate index sum) with the same raw velocities (sum). Returns the
    /// Py-ART folds on the Rust gate lattice.
    pub(crate) fn aligned_folds(
        &self,
        cut: &ElevationCut,
        sweep: &VelocitySweep,
    ) -> Vec<Option<i32>> {
        let grid = velocity(cut);
        assert_eq!(sweep.rows, self.rays.len(), "{}: ray count", self.case);
        let spacing = grid.gate_range.gate_spacing_m as f32;
        assert_eq!(spacing, self.gate_spacing_m, "{}: gate spacing", self.case);
        let offset = (grid.gate_range.first_gate_m as f32 - self.first_gate_m) / spacing;
        assert_eq!(offset.fract(), 0.0, "{}: gate lattices differ", self.case);
        let offset = offset as i64;
        let mut aligned = Vec::with_capacity(sweep.rows * sweep.gates);
        for (row, ray) in self.rays.iter().enumerate() {
            let azimuth_error = (sweep.azimuths[row] - ray.azimuth_deg.rem_euclid(360.0)).abs();
            assert!(
                azimuth_error.min(360.0 - azimuth_error) < 0.01,
                "{}: row {row} azimuth {} vs Py-ART {}",
                self.case,
                sweep.azimuths[row],
                ray.azimuth_deg
            );
            let n = sweep.nyq[row];
            assert!(
                (n.is_nan() && ray.nyquist_mps == 0.0) || (n - ray.nyquist_mps).abs() < 0.01,
                "{}: row {row} Nyquist {n} vs Py-ART {}",
                self.case,
                ray.nyquist_mps
            );
            let values = &sweep.observed[row * sweep.gates..(row + 1) * sweep.gates];
            let mut valid = 0usize;
            let mut index_sum = 0u64;
            let mut raw_sum = 0.0f64;
            for (gate, value) in values.iter().enumerate() {
                if value.is_finite() {
                    valid += 1;
                    index_sum += (gate as i64 + offset) as u64;
                    raw_sum += f64::from(*value);
                }
            }
            assert_eq!(valid, ray.valid, "{}: row {row} valid gates", self.case);
            assert_eq!(
                index_sum, ray.gate_index_sum,
                "{}: row {row} gate positions",
                self.case
            );
            assert!(
                (raw_sum - ray.raw_sum).abs() < 0.3,
                "{}: row {row} raw velocity sum {raw_sum} vs Py-ART {}",
                self.case,
                ray.raw_sum
            );
            let mut folds = ray.folds.iter().copied();
            aligned.extend(values.iter().map(|value| {
                if value.is_finite() {
                    folds.next()
                } else {
                    None
                }
            }));
        }
        aligned
    }
}

/// Agreement of an engine's per-gate folds with Py-ART's on the same sweep.
#[derive(Debug)]
pub(crate) struct FoldAgreement {
    /// Gates where both have a fold.
    pub(crate) compared: usize,
    /// The global fold offset (engine - Py-ART) shared by most gates; Py-ART
    /// anchors each sweep by its mean fold, the engines by their own rule.
    pub(crate) offset: i32,
    /// Gates whose fold equals Py-ART's plus `offset`.
    pub(crate) agreeing: usize,
    /// Gates whose fold equals Py-ART's plus the offset shared by most gates
    /// of their own connected echo (see [`echo_components`]).
    pub(crate) component_agreeing: usize,
    /// Adjacent gate pairs (along the ray and between neighbouring rays)
    /// that Py-ART leaves continuous (|dv| <= N after its unfolding).
    pub(crate) pyart_continuous_pairs: usize,
    /// ...of which the engine's unfolded field jumps by more than N.
    pub(crate) engine_breaks: usize,
}

impl FoldAgreement {
    pub(crate) fn fraction(&self) -> f64 {
        self.agreeing as f64 / self.compared.max(1) as f64
    }
}

pub(crate) fn fold_agreement(
    sweep: &VelocitySweep,
    engine: &[Option<i32>],
    pyart: &[Option<i32>],
    wraps: bool,
) -> FoldAgreement {
    let total = sweep.rows * sweep.gates;
    assert_eq!(engine.len(), total);
    assert_eq!(pyart.len(), total);
    let mut offsets: HashMap<i32, usize> = HashMap::new();
    let mut compared = 0;
    for (e, p) in engine.iter().zip(pyart) {
        if let (Some(e), Some(p)) = (e, p) {
            *offsets.entry(e - p).or_default() += 1;
            compared += 1;
        }
    }
    let (offset, agreeing) = offsets
        .into_iter()
        .max_by_key(|(offset, count)| (*count, -offset.abs()))
        .unwrap_or((0, 0));

    // Per connected echo (4-neighbour components of gates with both folds,
    // rows adjacent across the wrap when the sweep closes): the offset shared
    // by most of the component's gates.
    let label = echo_components(sweep.rows, sweep.gates, wraps, |idx| {
        engine[idx].is_some() && pyart[idx].is_some()
    });
    let mut per_component: HashMap<(u32, i32), usize> = HashMap::new();
    for idx in 0..total {
        if let (Some(component), Some(e), Some(p)) = (label[idx], engine[idx], pyart[idx]) {
            *per_component.entry((component, e - p)).or_default() += 1;
        }
    }
    let mut best: HashMap<u32, usize> = HashMap::new();
    for ((component, _), count) in per_component {
        let slot = best.entry(component).or_default();
        *slot = (*slot).max(count);
    }
    let component_agreeing = best.values().sum();

    let mut pyart_continuous_pairs = 0;
    let mut engine_breaks = 0;
    let mut pair = |a: usize, b: usize| {
        let (Some(pa), Some(pb), Some(ea), Some(eb)) = (pyart[a], pyart[b], engine[a], engine[b])
        else {
            return;
        };
        let n = sweep.nyq[a / sweep.gates].min(sweep.nyq[b / sweep.gates]);
        if (sweep.unfolded(a, pa) - sweep.unfolded(b, pb)).abs() > n {
            return;
        }
        pyart_continuous_pairs += 1;
        if (sweep.unfolded(a, ea) - sweep.unfolded(b, eb)).abs() > n {
            engine_breaks += 1;
        }
    };
    for row in 0..sweep.rows {
        for gate in 0..sweep.gates {
            let idx = row * sweep.gates + gate;
            if gate + 1 < sweep.gates {
                pair(idx, idx + 1);
            }
            if row + 1 < sweep.rows {
                pair(idx, idx + sweep.gates);
            } else if wraps {
                pair(idx, gate);
            }
        }
    }
    FoldAgreement {
        compared,
        offset,
        agreeing,
        component_agreeing,
        pyart_continuous_pairs,
        engine_breaks,
    }
}

/// 4-neighbour connected components of the gates where `member` holds (row
/// order wraps when `wraps`); `None` outside.
pub(crate) fn echo_components(
    rows: usize,
    gates: usize,
    wraps: bool,
    member: impl Fn(usize) -> bool,
) -> Vec<Option<u32>> {
    let total = rows * gates;
    let mut label: Vec<Option<u32>> = std::iter::repeat_n(None, total).collect();
    let mut next = 0u32;
    let mut stack = Vec::new();
    for start in 0..total {
        if label[start].is_some() || !member(start) {
            continue;
        }
        label[start] = Some(next);
        stack.push(start);
        while let Some(idx) = stack.pop() {
            let (row, gate) = (idx / gates, idx % gates);
            let mut neighbours = [None; 4];
            if gate > 0 {
                neighbours[0] = Some(idx - 1);
            }
            if gate + 1 < gates {
                neighbours[1] = Some(idx + 1);
            }
            if row > 0 {
                neighbours[2] = Some(idx - gates);
            } else if wraps && rows > 1 {
                neighbours[2] = Some((rows - 1) * gates + gate);
            }
            if row + 1 < rows {
                neighbours[3] = Some(idx + gates);
            } else if wraps && rows > 1 {
                neighbours[3] = Some(gate);
            }
            for neighbour in neighbours.into_iter().flatten() {
                if label[neighbour].is_none() && member(neighbour) {
                    label[neighbour] = Some(next);
                    stack.push(neighbour);
                }
            }
        }
        next += 1;
    }
    label
}

/// The Py-ART golden `case` with its decoded volume (`None` when offline).
pub(crate) fn golden_volume(case: &str) -> Option<(Arc<RadarVolume>, PyartGolden)> {
    let golden = PyartGolden::load(case);
    let volume = corpus_volume(&golden.id)?;
    assert_eq!(
        volume.cuts.len(),
        golden.file_sweeps,
        "{case}: decoded cuts vs Py-ART sweeps"
    );
    Some((volume, golden))
}

/// The Py-ART fold held by most gates of a sweep.
pub(crate) fn dominant_fold(pyart: &[Option<i32>]) -> i32 {
    let mut counts: HashMap<i32, usize> = HashMap::new();
    for fold in pyart.iter().flatten() {
        *counts.entry(*fold).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(fold, count)| (*count, -fold.abs()))
        .map_or(0, |(fold, _)| fold)
}

/// A connected set of gates that Py-ART moved to a different fold than the
/// sweep's dominant one, entirely enclosed by valid gates at the dominant
/// fold: `gates` are the patch, `ring` its 4-neighbours outside it.
pub(crate) struct FoldedPatch {
    pub(crate) gates: Vec<usize>,
    pub(crate) ring: Vec<usize>,
    pub(crate) rows: usize,
}

/// Every enclosed Py-ART patch of at least `min_gates` gates (see
/// [`FoldedPatch`]); selection uses the golden only.
pub(crate) fn enclosed_patches(
    sweep: &VelocitySweep,
    pyart: &[Option<i32>],
    min_gates: usize,
) -> Vec<FoldedPatch> {
    let dominant = dominant_fold(pyart);
    let (rows, gates) = (sweep.rows, sweep.gates);
    let label = echo_components(rows, gates, false, |idx| {
        pyart[idx].is_some_and(|fold| fold != dominant)
    });
    let mut members: HashMap<u32, Vec<usize>> = HashMap::new();
    for (idx, component) in label.iter().enumerate() {
        if let Some(component) = component {
            members.entry(*component).or_default().push(idx);
        }
    }
    let mut patches: Vec<FoldedPatch> = members
        .into_values()
        .filter(|patch| patch.len() >= min_gates)
        .filter_map(|patch| {
            let mut ring = Vec::new();
            for &idx in &patch {
                let (row, gate) = (idx / gates, idx % gates);
                if row == 0 || row + 1 == rows || gate == 0 || gate + 1 == gates {
                    return None;
                }
                for neighbour in [idx - 1, idx + 1, idx - gates, idx + gates] {
                    if label[neighbour] == label[idx] {
                        continue;
                    }
                    if pyart[neighbour] != Some(dominant) {
                        return None;
                    }
                    ring.push(neighbour);
                }
            }
            ring.sort_unstable();
            ring.dedup();
            let first_row = patch.iter().map(|idx| idx / gates).min()?;
            let last_row = patch.iter().map(|idx| idx / gates).max()?;
            Some(FoldedPatch {
                gates: patch,
                ring,
                rows: last_row - first_row + 1,
            })
        })
        .collect();
    patches.sort_by_key(|patch| patch.gates[0]);
    patches
}

/// Along-ray gate pairs whose raw velocities jump by more than the Nyquist
/// velocity and that Py-ART's unfolding makes continuous (|dv| <= N): how
/// many, and how many of them the engine's folds also make continuous.
pub(crate) fn radial_jumps_removed(
    sweep: &VelocitySweep,
    pyart: &[Option<i32>],
    engine: &[Option<i32>],
) -> (usize, usize) {
    let mut removed_by_pyart = 0;
    let mut removed_by_engine = 0;
    for row in 0..sweep.rows {
        let n = sweep.nyq[row];
        for gate in 1..sweep.gates {
            let (a, b) = (row * sweep.gates + gate - 1, row * sweep.gates + gate);
            let (Some(pa), Some(pb), Some(ea), Some(eb)) =
                (pyart[a], pyart[b], engine[a], engine[b])
            else {
                continue;
            };
            let raw_jump = (sweep.observed[a] - sweep.observed[b]).abs() > n;
            if raw_jump && (sweep.unfolded(a, pa) - sweep.unfolded(b, pb)).abs() <= n {
                removed_by_pyart += 1;
                if (sweep.unfolded(a, ea) - sweep.unfolded(b, eb)).abs() <= n {
                    removed_by_engine += 1;
                }
            }
        }
    }
    (removed_by_pyart, removed_by_engine)
}

/// The strongest azimuthal shear of an unfolded field: over gates between
/// `min_range_m` and `max_range_m`, the pair of adjacent rays with the largest
/// |dv| at the same gate, their values of opposite sign. `folds` gives the
/// field (Py-ART's golden or an engine's). Returns `(row, gate, dv)` with the
/// pair at rows `row` and `row + 1`.
pub(crate) fn strongest_azimuthal_shear(
    sweep: &VelocitySweep,
    folds: &[Option<i32>],
    min_range_m: i32,
    max_range_m: i32,
) -> (usize, usize, f32) {
    let mut best = (0, 0, 0.0f32);
    for row in 0..sweep.rows.saturating_sub(1) {
        for gate in 0..sweep.gates {
            let range = sweep.first_gate_m + gate as i32 * sweep.gate_spacing_m;
            if range < min_range_m || range > max_range_m {
                continue;
            }
            let (a, b) = (row * sweep.gates + gate, (row + 1) * sweep.gates + gate);
            let (Some(fa), Some(fb)) = (folds[a], folds[b]) else {
                continue;
            };
            let (va, vb) = (sweep.unfolded(a, fa), sweep.unfolded(b, fb));
            if va * vb < 0.0 && (va - vb).abs() > best.2 {
                best = (row, gate, (va - vb).abs());
            }
        }
    }
    best
}
