//! `--dealias` mode: the dealias-engine eval battery (dealias-v4 spec §10).
//!
//! Runs every requested engine (region / v4 / v4-noenv) on one target volume
//! (+ optional temporal-prior volume + optional environmental-wind fixture)
//! and prints the §10.2 metric set per engine. The `cascade` and `hybrid`
//! battery arms were removed at v0.29.0 with the engines themselves
//! (dealias-v4 spec §16); `docs/dealias-v4-baselines.json` keeps their
//! historical rows for the record:
//!
//! 1. residual fold-boundary pairs (lowest velocity tilt + volume total) —
//!    adjacent finite 4-neighbor pairs, azimuth wrap seam included, with
//!    |Δv| > 1.2·min(N_row) — NEVER read alone (a consistently wrong field
//!    scores ~0);
//! 2. reference RMS vs the environmental profile and vs a Browning & Wexler
//!    (1968) per-range-band harmonic fit of the engine's own output;
//! 3. % gates branch-modified vs raw;
//! 4. isolated speck count;
//! 5. branch spot-check probes (5×5-gate means at az/range);
//! 6. runtime (best-of-N): whole volume, worst super-res tilt (per-cut
//!    engines) or amortized per tilt (v4's single volume solve);
//! 7. determinism: two runs must be byte-identical or the process exits
//!    nonzero, same discipline as the pixel-checksum bench.
//!
//! `--rewrap N` runs the synthetic low-Nyquist Case E instead: the lowest
//! velocity tilt's accepted v4 output becomes exact truth, is re-wrapped to
//! ±N m/s, presented as a single-tilt cold-start volume, and each engine is
//! scored on % correct-branch gates (|v − truth| < 6 m/s).
//!
//! The harness never fetches: download volumes and fixtures first (see
//! README).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{DateTime, Utc};
use recast_radar_core::{
    Field, FieldData, FieldName, FloatCoding, GateMapping, Quantity, Sweep, Volume,
};
use recast_radar_correct::{
    EnvWindLevel, EnvironmentalWindProfile, TemporalPrior, dealias_velocity,
    dealias_velocity_pyart_region, dealias_volume, project_environmental_winds_onto,
    range_band_reference,
};

pub const DEALIAS_USAGE: &str = "usage: recast-radar-bench --dealias --target <vol> [options]

  --target <file>        Level-II target volume (required)
  --prior <file>         previous volume for the temporal prior
  --env <file.json>      EnvironmentalWindProfile fixture
  --engines a,b,c        subset of region,region-pyart,v4,v4-noenv
  --probe az,km[,label]  5x5-gate mean spot check (repeatable)
  --rewrap <N>           synthetic low-Nyquist Case E at Nyquist N m/s
  --iters <K>            timing iterations per engine (default 3, best-of)
  --case <name>          case label echoed in the output
  --json                 machine-readable output
  --dump-fields <dir>    write decoded fields (f32-LE .bin + meta .json) per
                         engine and velocity cut, plus raw / env-projection /
                         rewrap-truth fields, for external metric-parity
                         validation (crates/recast-radar-bench/py)

Exits nonzero if any engine's output differs between two identical runs.";

const CORRECT_BRANCH_TOLERANCE_MPS: f32 = 6.0;
const BOUNDARY_NYQUIST_FRAC: f32 = 1.2;
const DEFAULT_ITERS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Region,
    RegionPyart,
    V4,
    V4NoEnv,
}

impl Engine {
    fn name(self) -> &'static str {
        match self {
            Engine::Region => "region",
            Engine::RegionPyart => "region-pyart",
            Engine::V4 => "v4",
            Engine::V4NoEnv => "v4-noenv",
        }
    }

    fn parse(name: &str) -> Result<Self, String> {
        match name {
            "region" => Ok(Engine::Region),
            "region-pyart" | "pyart-rs" | "pyart-region-rs" => Ok(Engine::RegionPyart),
            "v4" => Ok(Engine::V4),
            "v4-noenv" => Ok(Engine::V4NoEnv),
            "cascade" | "hybrid" => Err(format!(
                "engine {name:?} was removed at v0.29.0 (dealias-v4 spec §16); \
                 docs/dealias-v4-baselines.json keeps its historical rows"
            )),
            other => Err(format!("unknown engine {other:?}")),
        }
    }
}

const ALL_ENGINES: [Engine; 3] = [Engine::Region, Engine::V4, Engine::V4NoEnv];

#[derive(Clone, Debug)]
pub struct Probe {
    azimuth_deg: f32,
    range_km: f32,
    label: String,
}

#[derive(Debug)]
pub struct DealiasArgs {
    target: PathBuf,
    prior: Option<PathBuf>,
    env: Option<PathBuf>,
    engines: Vec<Engine>,
    probes: Vec<Probe>,
    rewrap: Option<f32>,
    iters: usize,
    case: String,
    json: bool,
    dump_fields: Option<PathBuf>,
}

pub fn parse_dealias_args(args: &[String]) -> Result<DealiasArgs, String> {
    let mut target = None;
    let mut prior = None;
    let mut env = None;
    let mut engines: Vec<Engine> = ALL_ENGINES.to_vec();
    let mut probes = Vec::new();
    let mut rewrap = None;
    let mut iters = DEFAULT_ITERS;
    let mut case = String::from("unnamed");
    let mut json = false;
    let mut dump_fields = None;

    let mut index = 0;
    let value_of = |index: &mut usize, flag: &str| -> Result<String, String> {
        *index += 1;
        args.get(*index)
            .cloned()
            .ok_or_else(|| format!("{flag} requires a value"))
    };
    while index < args.len() {
        match args[index].as_str() {
            "--dealias" => {}
            "--target" => target = Some(PathBuf::from(value_of(&mut index, "--target")?)),
            "--prior" => prior = Some(PathBuf::from(value_of(&mut index, "--prior")?)),
            "--env" => env = Some(PathBuf::from(value_of(&mut index, "--env")?)),
            "--engines" => {
                engines = value_of(&mut index, "--engines")?
                    .split(',')
                    .map(Engine::parse)
                    .collect::<Result<_, _>>()?;
            }
            "--probe" => probes.push(parse_probe(&value_of(&mut index, "--probe")?)?),
            "--rewrap" => {
                let value = value_of(&mut index, "--rewrap")?;
                rewrap = Some(
                    value
                        .parse::<f32>()
                        .ok()
                        .filter(|nyquist| nyquist.is_finite() && *nyquist > 0.0)
                        .ok_or_else(|| {
                            format!("--rewrap expects a positive Nyquist, got {value:?}")
                        })?,
                );
            }
            "--iters" => {
                let value = value_of(&mut index, "--iters")?;
                iters = value
                    .parse::<usize>()
                    .ok()
                    .filter(|parsed| *parsed > 0)
                    .ok_or_else(|| format!("--iters expects a positive integer, got {value:?}"))?;
            }
            "--case" => case = value_of(&mut index, "--case")?,
            "--json" => json = true,
            "--dump-fields" => {
                dump_fields = Some(PathBuf::from(value_of(&mut index, "--dump-fields")?));
            }
            other => return Err(format!("unknown --dealias option {other}")),
        }
        index += 1;
    }
    Ok(DealiasArgs {
        target: target.ok_or("--target is required")?,
        prior,
        env,
        engines,
        probes,
        rewrap,
        iters,
        case,
        json,
        dump_fields,
    })
}

fn parse_probe(spec: &str) -> Result<Probe, String> {
    let parts: Vec<&str> = spec.split(',').collect();
    if parts.len() < 2 {
        return Err(format!("--probe expects az,km[,label], got {spec:?}"));
    }
    let azimuth_deg = parts[0]
        .parse::<f32>()
        .map_err(|_| format!("bad probe azimuth {:?}", parts[0]))?;
    let range_km = parts[1]
        .parse::<f32>()
        .map_err(|_| format!("bad probe range {:?}", parts[1]))?;
    let label = parts
        .get(2)
        .map(|label| (*label).to_owned())
        .unwrap_or_else(|| format!("az{azimuth_deg}/r{range_km}km"));
    Ok(Probe {
        azimuth_deg,
        range_km,
        label,
    })
}

// ---- environmental fixture ----

#[derive(serde::Deserialize)]
struct EnvFixture {
    /// Provenance note (model, cycle, extraction method) — echoed in output.
    source: String,
    site: String,
    valid_time: DateTime<Utc>,
    levels: Vec<EnvFixtureLevel>,
}

#[derive(serde::Deserialize)]
struct EnvFixtureLevel {
    height_m_arl: f32,
    u_mps: f32,
    v_mps: f32,
}

fn load_env_fixture(path: &PathBuf) -> Result<(EnvironmentalWindProfile, String, String), String> {
    let text = fs::read_to_string(path).map_err(|err| format!("read {}: {err}", path.display()))?;
    let fixture: EnvFixture =
        serde_json::from_str(&text).map_err(|err| format!("parse {}: {err}", path.display()))?;
    let profile = EnvironmentalWindProfile {
        levels: fixture
            .levels
            .iter()
            .map(|level| EnvWindLevel {
                height_m_arl: level.height_m_arl,
                u_mps: level.u_mps,
                v_mps: level.v_mps,
            })
            .collect(),
        valid_time: fixture.valid_time,
    };
    Ok((profile, fixture.site, fixture.source))
}

// ---- decoded per-tilt field ----

struct Plane {
    rows: usize,
    gates: usize,
    wraps: bool,
    /// Decoded velocity (NaN = missing), row-major.
    values: Vec<f32>,
    /// Per-row Nyquist (NaN unknown).
    nyq: Vec<f32>,
}

/// Centre of native gate 0 and native gate spacing of `field` on `sweep`,
/// in metres.
fn geometry(sweep: &Sweep, field: &Field) -> (f64, f64) {
    field.native_geometry(&sweep.range).unwrap_or((0.0, 1.0))
}

fn decode_field(sweep: &Sweep, field: &Field) -> Plane {
    let (rows, gates) = field.shape();
    let mut values = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            if let Some(value) = field.value(row, gate).filter(|value| value.is_finite()) {
                values[row * gates + gate] = value;
            }
        }
    }
    let mut per_row: Vec<f32> = (0..rows)
        .map(|row| {
            sweep
                .ray_vars
                .nyquist_velocity_mps
                .as_ref()
                .and_then(|values| values.get(row).copied())
                .filter(|nyquist| nyquist.is_finite() && *nyquist > 0.0)
                .unwrap_or(f32::NAN)
        })
        .collect();
    let mut finite: Vec<f32> = per_row.iter().copied().filter(|n| n.is_finite()).collect();
    if !finite.is_empty() {
        let middle = finite.len() / 2;
        finite.select_nth_unstable_by(middle, |left, right| left.total_cmp(right));
        let fallback = finite[middle];
        for slot in per_row.iter_mut() {
            if !slot.is_finite() {
                *slot = fallback;
            }
        }
    }
    let azimuths: Vec<f32> = (0..rows)
        .map(|row| {
            sweep
                .rays
                .azimuth_deg
                .get(row)
                .map(|azimuth| azimuth.rem_euclid(360.0))
                .unwrap_or(f32::NAN)
        })
        .collect();
    let wraps = sweep_wraps(&azimuths);
    Plane {
        rows,
        gates,
        wraps,
        values,
        nyq: per_row,
    }
}

/// Same rule as the engine: the sweep closes 360° when first/last azimuths
/// are within 3 typical spacings.
fn sweep_wraps(azimuths: &[f32]) -> bool {
    let rows = azimuths.len();
    if rows < 8 {
        return false;
    }
    let (Some(first), Some(last)) = (azimuths.first(), azimuths.last()) else {
        return false;
    };
    if !first.is_finite() || !last.is_finite() {
        return false;
    }
    let gap = (first - last)
        .rem_euclid(360.0)
        .min((last - first).rem_euclid(360.0));
    gap <= 3.0 * (360.0 / rows as f32)
}

// ---- metrics ----

fn boundary_pairs(field: &Plane) -> usize {
    let mut boundaries = 0;
    let (rows, gates) = (field.rows, field.gates);
    let mut check = |a: usize, b: usize| {
        let (va, vb) = (field.values[a], field.values[b]);
        if !va.is_finite() || !vb.is_finite() {
            return;
        }
        let threshold = BOUNDARY_NYQUIST_FRAC * field.nyq[a / gates].min(field.nyq[b / gates]);
        if threshold.is_finite() && (va - vb).abs() > threshold {
            boundaries += 1;
        }
    };
    for row in 0..rows {
        for gate in 0..gates {
            let idx = row * gates + gate;
            if gate + 1 < gates {
                check(idx, idx + 1);
            }
            if row + 1 < rows {
                check(idx, idx + gates);
            }
        }
    }
    if field.wraps && rows > 1 {
        for gate in 0..gates {
            check((rows - 1) * gates + gate, gate);
        }
    }
    boundaries
}

fn rms_against(field: &Plane, reference: &[f32]) -> Option<f64> {
    let mut sum = 0.0f64;
    let mut count = 0u64;
    for (value, predicted) in field.values.iter().zip(reference) {
        if value.is_finite() && predicted.is_finite() {
            let delta = f64::from(value - predicted);
            sum += delta * delta;
            count += 1;
        }
    }
    (count > 0).then(|| (sum / count as f64).sqrt())
}

fn harmonic_rms(sweep: &Sweep, source: &Field, field: &Plane) -> Option<f64> {
    let fit = range_band_reference(sweep, source);
    let mut sum = 0.0f64;
    let mut count = 0u64;
    for row in 0..field.rows {
        let azimuth = sweep.rays.azimuth_deg.get(row).copied()?;
        let (sin_az, cos_az) = azimuth.to_radians().sin_cos();
        for gate in 0..field.gates {
            let value = field.values[row * field.gates + gate];
            if !value.is_finite() {
                continue;
            }
            let Some(Some((a, b))) = fit.fits.get(gate / fit.band_gates.max(1)) else {
                continue;
            };
            let predicted = a * cos_az + b * sin_az;
            let delta = f64::from(value - predicted);
            sum += delta * delta;
            count += 1;
        }
    }
    (count > 0).then(|| (sum / count as f64).sqrt())
}

fn percent_modified(output: &Plane, raw: &Plane) -> f64 {
    let mut modified = 0u64;
    let mut finite = 0u64;
    for idx in 0..output.values.len().min(raw.values.len()) {
        let (out, source) = (output.values[idx], raw.values[idx]);
        if !out.is_finite() || !source.is_finite() {
            continue;
        }
        finite += 1;
        let nyquist = raw.nyq[idx / raw.gates];
        if nyquist.is_finite() && (out - source).abs() > nyquist {
            modified += 1;
        }
    }
    if finite == 0 {
        0.0
    } else {
        100.0 * modified as f64 / finite as f64
    }
}

/// Isolated specks: 4-connected components (≤ 3 gates) of gates more than a
/// Nyquist off their finite 8-neighborhood median.
fn speck_count(field: &Plane) -> usize {
    let (rows, gates) = (field.rows, field.gates);
    let total = rows * gates;
    let mut flagged = vec![false; total];
    for row in 0..rows {
        let nyquist = field.nyq[row];
        if !nyquist.is_finite() {
            continue;
        }
        for gate in 0..gates {
            let idx = row * gates + gate;
            let value = field.values[idx];
            if !value.is_finite() {
                continue;
            }
            let mut neighborhood: Vec<f32> = Vec::with_capacity(8);
            for delta_row in -1i64..=1 {
                let mut sample_row = row as i64 + delta_row;
                if field.wraps {
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
                    let sample = field.values[sample_row as usize * gates + sample_gate as usize];
                    if sample.is_finite() {
                        neighborhood.push(sample);
                    }
                }
            }
            if neighborhood.len() < 4 {
                continue;
            }
            let middle = neighborhood.len() / 2;
            neighborhood.select_nth_unstable_by(middle, |left, right| left.total_cmp(right));
            if (value - neighborhood[middle]).abs() > nyquist {
                flagged[idx] = true;
            }
        }
    }
    // 4-connected components of flagged gates.
    let mut seen = vec![false; total];
    let mut specks = 0;
    for start in 0..total {
        if !flagged[start] || seen[start] {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let mut size = 0usize;
        while let Some(idx) = stack.pop() {
            size += 1;
            let (row, gate) = (idx / gates, idx % gates);
            let mut push = |neighbor: usize, stack: &mut Vec<usize>| {
                if flagged[neighbor] && !seen[neighbor] {
                    seen[neighbor] = true;
                    stack.push(neighbor);
                }
            };
            if row > 0 {
                push(idx - gates, &mut stack);
            } else if field.wraps && rows > 1 {
                push((rows - 1) * gates + gate, &mut stack);
            }
            if row + 1 < rows {
                push(idx + gates, &mut stack);
            } else if field.wraps && rows > 1 {
                push(gate, &mut stack);
            }
            if gate > 0 {
                push(idx - 1, &mut stack);
            }
            if gate + 1 < gates {
                push(idx + 1, &mut stack);
            }
        }
        if size <= 3 {
            specks += 1;
        }
    }
    specks
}

/// Strongest azimuthal gate-to-gate ΔV in the 15–40 km annulus (Case B
/// couplet-preservation check: repair must not smooth the mesocyclone).
fn couplet_max_delta(sweep: &Sweep, source: &Field, field: &Plane) -> Option<f64> {
    let (first, spacing) = geometry(sweep, source);
    let spacing = spacing.max(1.0);
    let gate_lo = (((15_000.0 - first) / spacing).ceil().max(0.0)) as usize;
    let gate_hi = ((40_000.0 - first) / spacing).floor() as usize;
    let mut strongest: Option<f64> = None;
    for row in 0..field.rows {
        let next_row = if row + 1 < field.rows {
            row + 1
        } else if field.wraps {
            0
        } else {
            continue;
        };
        for gate in gate_lo..=gate_hi.min(field.gates.saturating_sub(1)) {
            let a = field.values[row * field.gates + gate];
            let b = field.values[next_row * field.gates + gate];
            if a.is_finite() && b.is_finite() {
                let delta = f64::from((a - b).abs());
                if strongest.is_none_or(|current| delta > current) {
                    strongest = Some(delta);
                }
            }
        }
    }
    strongest
}

/// Max inbound (most negative) velocity on the tilt (Case C: the eyewall
/// must not be under-unfolded).
fn max_inbound(field: &Plane) -> Option<f32> {
    field
        .values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .min_by(|left, right| left.total_cmp(right))
}

/// Multi-fold structure (Case C): gates moved by |fold| ≥ 2 (|out − raw| >
/// 3·N) must form coherent regions, not speckle.  Returns (gate count,
/// number of 4-connected components smaller than 32 gates).
fn multifold_structure(output: &Plane, raw: &Plane) -> (usize, usize) {
    let (rows, gates) = (output.rows, output.gates);
    let total = rows * gates;
    let mut flagged = vec![false; total];
    let mut count = 0usize;
    for (idx, flag) in flagged
        .iter_mut()
        .enumerate()
        .take(total.min(raw.values.len()))
    {
        let (out, source) = (output.values[idx], raw.values[idx]);
        let nyquist = raw.nyq[idx / gates];
        if out.is_finite()
            && source.is_finite()
            && nyquist.is_finite()
            && (out - source).abs() > 3.0 * nyquist
        {
            *flag = true;
            count += 1;
        }
    }
    let mut seen = vec![false; total];
    let mut speckle = 0usize;
    for start in 0..total {
        if !flagged[start] || seen[start] {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let mut size = 0usize;
        while let Some(idx) = stack.pop() {
            size += 1;
            let (row, gate) = (idx / gates, idx % gates);
            let mut push = |neighbor: usize, stack: &mut Vec<usize>| {
                if flagged[neighbor] && !seen[neighbor] {
                    seen[neighbor] = true;
                    stack.push(neighbor);
                }
            };
            if row > 0 {
                push(idx - gates, &mut stack);
            } else if output.wraps && rows > 1 {
                push((rows - 1) * gates + gate, &mut stack);
            }
            if row + 1 < rows {
                push(idx + gates, &mut stack);
            } else if output.wraps && rows > 1 {
                push(gate, &mut stack);
            }
            if gate > 0 {
                push(idx - 1, &mut stack);
            }
            if gate + 1 < gates {
                push(idx + 1, &mut stack);
            }
        }
        if size < 32 {
            speckle += 1;
        }
    }
    (count, speckle)
}

/// 5×5-gate mean around the nearest (azimuth, range) gate.
fn probe_mean(sweep: &Sweep, source: &Field, field: &Plane, probe: &Probe) -> Option<f32> {
    let target_azimuth = probe.azimuth_deg.rem_euclid(360.0);
    let row = (0..field.rows)
        .filter_map(|row| {
            let azimuth = sweep.rays.azimuth_deg.get(row)?.rem_euclid(360.0);
            let distance = ((azimuth - target_azimuth + 180.0).rem_euclid(360.0) - 180.0).abs();
            Some((row, distance))
        })
        .min_by(|left, right| left.1.total_cmp(&right.1))?
        .0;
    let range_m = probe.range_km * 1000.0;
    let (first, spacing) = geometry(sweep, source);
    let gate = ((range_m - first as f32) / spacing.max(1.0) as f32).round();
    if gate < 0.0 || gate >= field.gates as f32 {
        return None;
    }
    let gate = gate as usize;
    let mut sum = 0.0f64;
    let mut count = 0u64;
    for delta_row in -2i64..=2 {
        let mut sample_row = row as i64 + delta_row;
        if field.wraps {
            sample_row = sample_row.rem_euclid(field.rows as i64);
        } else if sample_row < 0 || sample_row >= field.rows as i64 {
            continue;
        }
        for delta_gate in -2i64..=2 {
            let sample_gate = gate as i64 + delta_gate;
            if sample_gate < 0 || sample_gate >= field.gates as i64 {
                continue;
            }
            let value = field.values[sample_row as usize * field.gates + sample_gate as usize];
            if value.is_finite() {
                sum += f64::from(value);
                count += 1;
            }
        }
    }
    (count > 0).then(|| (sum / count as f64) as f32)
}

// ---- field dumps (external metric-parity validation) ----

/// Raw per-row azimuths (`rays.azimuth_deg`, no wrapping) — the exact
/// accessor the harmonic fit and env projection read.
fn row_azimuths(sweep: &Sweep, field: &Field) -> Vec<f32> {
    (0..field.nrays as usize)
        .map(|row| sweep.rays.azimuth_deg.get(row).copied().unwrap_or(f32::NAN))
        .collect()
}

/// Write `values` as little-endian f32, row-major — the layout
/// `numpy.fromfile(dtype="<f4")` reads back directly.
fn write_field_bin(path: &Path, values: &[f32]) -> Result<(), String> {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    fs::write(path, bytes).map_err(|err| format!("write {}: {err}", path.display()))
}

/// Dump one decoded field (`<label>_cutNN.bin` + `.json`).  JSON has no NaN
/// literal, so non-finite azimuth/Nyquist entries become `null`.
fn dump_field(
    dir: &Path,
    label: &str,
    volume: &Volume,
    cut_index: usize,
    source: &Field,
    field: &Plane,
    lowest: bool,
) -> Result<(), String> {
    let sweep = &volume.sweeps[cut_index];
    let (first_gate_m, gate_spacing_m) = geometry(sweep, source);
    let stem = format!("{label}_cut{cut_index:02}");
    write_field_bin(&dir.join(format!("{stem}.bin")), &field.values)?;
    let finite_or_null = |value: f32| {
        if value.is_finite() {
            serde_json::json!(value)
        } else {
            serde_json::Value::Null
        }
    };
    let meta = serde_json::json!({
        "label": label,
        "cut_index": cut_index,
        "elevation_deg": sweep.tilt_elevation_deg(volume.provenance.source_format),
        "rows": field.rows,
        "gates": field.gates,
        "wraps": field.wraps,
        "first_gate_m": first_gate_m,
        "gate_spacing_m": gate_spacing_m,
        "lowest": lowest,
        "nyquist_mps": field.nyq.iter().copied().map(finite_or_null).collect::<Vec<_>>(),
        "azimuth_deg": row_azimuths(sweep, source)
            .iter()
            .copied()
            .map(finite_or_null)
            .collect::<Vec<_>>(),
    });
    let path = dir.join(format!("{stem}.json"));
    fs::write(
        &path,
        serde_json::to_string(&meta).expect("serializable meta"),
    )
    .map_err(|err| format!("write {}: {err}", path.display()))
}

// ---- engine drivers ----

/// The radial velocity field of a sweep.
fn velocity_field(sweep: &Sweep) -> &Field {
    sweep
        .find(Quantity::RadialVelocity)
        .expect("velocity sweep")
}

fn velocity_cuts(volume: &Volume) -> Vec<usize> {
    (0..volume.sweeps.len())
        .filter(|&index| {
            volume.sweeps[index]
                .find(Quantity::RadialVelocity)
                .is_some_and(|field| {
                    field.nrays as usize > field.absent_rows.len() && field.ngates > 0
                })
        })
        .collect()
}

/// The velocity cut with the smallest tilt elevation
/// ([`Sweep::tilt_elevation_deg`]), the first on ties.
fn lowest_velocity_cut(volume: &Volume) -> Option<usize> {
    let tilt =
        |index: usize| volume.sweeps[index].tilt_elevation_deg(volume.provenance.source_format);
    velocity_cuts(volume)
        .into_iter()
        .min_by(|&a, &b| tilt(a).total_cmp(&tilt(b)).then_with(|| a.cmp(&b)))
}

struct EngineRun {
    /// Output field per velocity cut (aligned with `velocity_cuts`).
    grids: Vec<Option<Field>>,
    volume_ms: f64,
    worst_tilt_ms: f64,
    /// True when `worst_tilt_ms` is amortized (single volume solve).
    amortized: bool,
    diagnostics: Option<recast_radar_correct::V4Diagnostics>,
}

fn run_engine(
    engine: Engine,
    volume: &Volume,
    priors: &PriorSolutions,
    environment: Option<&EnvironmentalWindProfile>,
) -> EngineRun {
    let cuts = velocity_cuts(volume);
    match engine {
        Engine::V4 | Engine::V4NoEnv => {
            // v4-noenv must not smuggle the profile in through an
            // env-solved prior: each variant gets a matching prior chain.
            let (environment, prior_solution) = if engine == Engine::V4 {
                (environment, priors.with_env.as_ref())
            } else {
                (None, priors.without_env.as_ref())
            };
            let started = Instant::now();
            let solution = dealias_volume(
                volume,
                prior_solution.map(TemporalPrior::Solution),
                environment,
            );
            let volume_ms = started.elapsed().as_secs_f64() * 1000.0;
            let grids = cuts
                .iter()
                .map(|&cut| solution.tilt_field(cut).cloned())
                .collect();
            EngineRun {
                grids,
                volume_ms,
                worst_tilt_ms: volume_ms / cuts.len().max(1) as f64,
                amortized: true,
                diagnostics: Some(solution.diagnostics().clone()),
            }
        }
        Engine::Region => {
            let mut grids = Vec::with_capacity(cuts.len());
            let mut volume_ms = 0.0;
            let mut worst_tilt_ms = 0.0f64;
            let mut worst_is_superres = false;
            for &cut_index in &cuts {
                let sweep = &volume.sweeps[cut_index];
                let source = velocity_field(sweep);
                let started = Instant::now();
                let output = Some(dealias_velocity(sweep, source));
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                volume_ms += elapsed;
                let superres = source.nrays >= 600;
                // Prefer the worst SUPER-RES tilt; fall back to any tilt.
                if (superres && (!worst_is_superres || elapsed > worst_tilt_ms))
                    || (!worst_is_superres && elapsed > worst_tilt_ms)
                {
                    worst_tilt_ms = elapsed;
                    worst_is_superres = superres;
                }
                grids.push(output);
            }
            EngineRun {
                grids,
                volume_ms,
                worst_tilt_ms,
                amortized: false,
                diagnostics: None,
            }
        }
        Engine::RegionPyart => {
            let mut grids = Vec::with_capacity(cuts.len());
            let mut volume_ms = 0.0;
            let mut worst_tilt_ms = 0.0f64;
            let mut worst_is_superres = false;
            for &cut_index in &cuts {
                let sweep = &volume.sweeps[cut_index];
                let source = velocity_field(sweep);
                let started = Instant::now();
                let output = Some(dealias_velocity_pyart_region(sweep, source));
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                volume_ms += elapsed;
                let superres = source.nrays >= 600;
                // Prefer the worst SUPER-RES tilt; fall back to any tilt.
                if (superres && (!worst_is_superres || elapsed > worst_tilt_ms))
                    || (!worst_is_superres && elapsed > worst_tilt_ms)
                {
                    worst_tilt_ms = elapsed;
                    worst_is_superres = superres;
                }
                grids.push(output);
            }
            EngineRun {
                grids,
                volume_ms,
                worst_tilt_ms,
                amortized: false,
                diagnostics: None,
            }
        }
    }
}

#[derive(Default)]
struct EngineReport {
    boundaries_lowest: usize,
    boundaries_volume: usize,
    rms_env: Option<f64>,
    rms_harmonic: Option<f64>,
    percent_modified: f64,
    specks_lowest: usize,
    couplet_max_dv: Option<f64>,
    max_inbound: Option<f32>,
    multifold_gates: usize,
    multifold_speckle: usize,
    probes: Vec<(String, Option<f32>)>,
    volume_ms: f64,
    worst_tilt_ms: f64,
    amortized: bool,
    deterministic: bool,
    correct_branch_percent: Option<f64>,
}

/// Prior-volume solutions per env variant (the temporal chain must match
/// the engine's own env setting).
#[derive(Default)]
struct PriorSolutions {
    with_env: Option<recast_radar_correct::V4VolumeSolution>,
    without_env: Option<recast_radar_correct::V4VolumeSolution>,
}

#[allow(clippy::too_many_arguments)]
fn evaluate_engine(
    engine: Engine,
    volume: &Volume,
    priors: &PriorSolutions,
    environment: Option<&EnvironmentalWindProfile>,
    probes: &[Probe],
    iters: usize,
    truth_lowest: Option<&[f32]>,
) -> EngineReport {
    let cuts = velocity_cuts(volume);
    let lowest = lowest_velocity_cut(volume);

    // Timing: best-of-N.
    let mut best: Option<EngineRun> = None;
    for _ in 0..iters.max(1) {
        let run = run_engine(engine, volume, priors, environment);
        if best
            .as_ref()
            .is_none_or(|current| run.volume_ms < current.volume_ms)
        {
            best = Some(run);
        }
    }
    let timed = best.expect("at least one run");
    // Determinism: one more run, byte-compare all grids.
    let second = run_engine(engine, volume, priors, environment);
    let deterministic =
        timed
            .grids
            .iter()
            .zip(&second.grids)
            .all(|(left, right)| match (left, right) {
                (Some(left), Some(right)) => left.data == right.data,
                (None, None) => true,
                _ => false,
            });

    if let Some(diagnostics) = &timed.diagnostics {
        eprintln!(
            "[{}] nodes {} edges {} components {} enum {} (beat-heuristic {}) | \
             couplet-mask {} speck {} patch {} ring {} reverted {} box {} plane {} aborts {}",
            engine.name(),
            diagnostics.nodes,
            diagnostics.graph_edges,
            diagnostics.components,
            diagnostics.enumerated_components,
            diagnostics.enumeration_beat_heuristic,
            diagnostics.couplet_masked,
            diagnostics.speck_snapped,
            diagnostics.patch_changed,
            diagnostics.ring_closed,
            diagnostics.patch_reverted,
            diagnostics.box_moved,
            diagnostics.plane_moved,
            diagnostics.repair_aborts,
        );
    }
    let mut report = EngineReport {
        volume_ms: timed.volume_ms,
        worst_tilt_ms: timed.worst_tilt_ms,
        amortized: timed.amortized,
        deterministic,
        ..EngineReport::default()
    };

    for (slot, &cut_index) in cuts.iter().enumerate() {
        let Some(grid) = timed.grids[slot].as_ref() else {
            continue;
        };
        let sweep = &volume.sweeps[cut_index];
        let field = decode_field(sweep, grid);
        let boundaries = boundary_pairs(&field);
        report.boundaries_volume += boundaries;
        if Some(cut_index) == lowest {
            report.boundaries_lowest = boundaries;
            report.specks_lowest = speck_count(&field);
            let raw_grid = velocity_field(sweep);
            let raw = decode_field(sweep, raw_grid);
            report.percent_modified = percent_modified(&field, &raw);
            report.couplet_max_dv = couplet_max_delta(sweep, grid, &field);
            report.max_inbound = max_inbound(&field);
            let (multifold_gates, multifold_speckle) = multifold_structure(&field, &raw);
            report.multifold_gates = multifold_gates;
            report.multifold_speckle = multifold_speckle;
            if let Some(profile) = environment {
                let projected =
                    project_environmental_winds_onto(profile, volume, cut_index, raw_grid);
                report.rms_env = rms_against(&field, &projected);
            }
            report.rms_harmonic = harmonic_rms(sweep, grid, &field);
            for probe in probes {
                report
                    .probes
                    .push((probe.label.clone(), probe_mean(sweep, grid, &field, probe)));
            }
            if let Some(truth) = truth_lowest {
                let mut correct = 0u64;
                let mut counted = 0u64;
                for (value, truth_value) in field.values.iter().zip(truth) {
                    if value.is_finite() && truth_value.is_finite() {
                        counted += 1;
                        if (value - truth_value).abs() < CORRECT_BRANCH_TOLERANCE_MPS {
                            correct += 1;
                        }
                    }
                }
                report.correct_branch_percent =
                    (counted > 0).then(|| 100.0 * correct as f64 / counted as f64);
            }
        }
    }
    report
}

// ---- Case E rewrap ----

/// Build the synthetic low-Nyquist cold-start volume: the lowest velocity
/// tilt's `truth` re-wrapped into ±nyquist, single tilt, no priors.
fn build_rewrapped_volume(volume: &Volume, lowest: usize, truth: &[f32], nyquist: f32) -> Volume {
    let source_sweep = &volume.sweeps[lowest];
    let source = velocity_field(source_sweep);
    let mut sweep = source_sweep.clone();
    sweep.sweep_number = 0;
    sweep.fields.clear();
    sweep.ray_vars.nyquist_velocity_mps = Some(vec![nyquist; sweep.nrays()]);
    let wrapped: Vec<f32> = truth
        .iter()
        .map(|&value| {
            if value.is_finite() {
                (value + nyquist).rem_euclid(2.0 * nyquist) - nyquist
            } else {
                f32::NAN
            }
        })
        .collect();
    let mut field = Field::new(
        FieldName::Vradh,
        GateMapping {
            start: source.gates.start,
            stride: source.gates.stride,
        },
        source.ngates,
        FieldData::F32 {
            values: wrapped,
            coding: FloatCoding::default(),
        },
    );
    field.nrays = source.nrays;
    field.absent_rows = source.absent_rows.clone();
    sweep.add_field(field).expect("one velocity field");
    let mut synthetic = Volume::new(volume.attrs.instrument_name.clone(), volume.time_reference);
    synthetic.time_coverage = volume.time_coverage;
    synthetic.location = volume.location;
    synthetic.provenance.source_format = volume.provenance.source_format;
    synthetic.sweeps = vec![sweep];
    synthetic.seal().expect("rewrapped volume seals");
    synthetic
}

// ---- output ----

fn format_option_f64(value: Option<f64>, precision: usize) -> String {
    value.map_or_else(|| "-".to_owned(), |value| format!("{value:.precision$}"))
}

fn format_option_f32(value: Option<f32>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| format!("{value:+.1}"))
}

pub fn run_dealias(args: &DealiasArgs) -> Result<bool, String> {
    let raw =
        fs::read(&args.target).map_err(|err| format!("read {}: {err}", args.target.display()))?;
    let volume = recast_radar_io::read_supported_volume_bytes(raw.as_slice())
        .map_err(|err| err.to_string())?;
    let prior_volume = match &args.prior {
        Some(path) => {
            let bytes = fs::read(path).map_err(|err| format!("read {}: {err}", path.display()))?;
            Some(
                recast_radar_io::read_supported_volume_bytes(bytes.as_slice())
                    .map_err(|err| err.to_string())?,
            )
        }
        None => None,
    };
    let environment = match &args.env {
        Some(path) => {
            let (profile, site, source) = load_env_fixture(path)?;
            if !site.eq_ignore_ascii_case(&volume.attrs.instrument_name) {
                return Err(format!(
                    "env fixture is for {site}, volume is {}",
                    volume.attrs.instrument_name
                ));
            }
            Some((profile, source))
        }
        None => None,
    };
    let profile = environment.as_ref().map(|(profile, _)| profile);

    // Pre-solve the temporal prior ONCE per env variant with v4 (the app
    // flow caches the previous solution).
    let priors = PriorSolutions {
        with_env: prior_volume
            .as_ref()
            .map(|prior| dealias_volume(prior, None, profile)),
        without_env: prior_volume
            .as_ref()
            .map(|prior| dealias_volume(prior, None, None)),
    };

    let lowest = lowest_velocity_cut(&volume).ok_or("volume has no velocity cut")?;

    // Case E: replace the working volume with the rewrapped single tilt.
    let (volume, priors, truth): (Volume, PriorSolutions, Option<Vec<f32>>) =
        if let Some(nyquist) = args.rewrap {
            let truth_solution = dealias_volume(
                &volume,
                priors.with_env.as_ref().map(TemporalPrior::Solution),
                profile,
            );
            let truth_grid = truth_solution
                .tilt_field(lowest)
                .ok_or("v4 produced no field for the lowest velocity cut")?;
            let truth_field = decode_field(&volume.sweeps[lowest], truth_grid);
            let synthetic = build_rewrapped_volume(&volume, lowest, &truth_field.values, nyquist);
            (
                synthetic,
                PriorSolutions::default(),
                Some(truth_field.values),
            )
        } else {
            (volume, priors, None)
        };

    // Field dumps for the external (Python) metric-parity harness.  A
    // dedicated pass: every engine here is deterministic (enforced below),
    // so re-running produces the grids the metrics were scored on.
    if let Some(dir) = &args.dump_fields {
        fs::create_dir_all(dir).map_err(|err| format!("create {}: {err}", dir.display()))?;
        let cuts = velocity_cuts(&volume);
        let lowest_cut = lowest_velocity_cut(&volume);
        for &cut_index in &cuts {
            let sweep = &volume.sweeps[cut_index];
            let grid = velocity_field(sweep);
            let field = decode_field(sweep, grid);
            let lowest = Some(cut_index) == lowest_cut;
            dump_field(dir, "raw", &volume, cut_index, grid, &field, lowest)?;
            if lowest {
                if let Some(profile) = profile {
                    let projected =
                        project_environmental_winds_onto(profile, &volume, cut_index, grid);
                    write_field_bin(&dir.join(format!("env_cut{cut_index:02}.bin")), &projected)?;
                }
                if let Some(truth_values) = &truth {
                    write_field_bin(
                        &dir.join(format!("truth_cut{cut_index:02}.bin")),
                        truth_values,
                    )?;
                }
            }
        }
        for &engine in &args.engines {
            let run = run_engine(engine, &volume, &priors, profile);
            for (slot, &cut_index) in cuts.iter().enumerate() {
                let Some(grid) = run.grids[slot].as_ref() else {
                    continue;
                };
                let sweep = &volume.sweeps[cut_index];
                let field = decode_field(sweep, grid);
                dump_field(
                    dir,
                    engine.name(),
                    &volume,
                    cut_index,
                    grid,
                    &field,
                    Some(cut_index) == lowest_cut,
                )?;
            }
        }
    }

    let mut lines = Vec::new();
    let mut json_engines = Vec::new();
    let mut all_deterministic = true;
    for &engine in &args.engines {
        let report = evaluate_engine(
            engine,
            &volume,
            &priors,
            profile,
            &args.probes,
            args.iters,
            truth.as_deref(),
        );
        all_deterministic &= report.deterministic;
        if args.json {
            let probes: Vec<String> = report
                .probes
                .iter()
                .map(|(label, value)| {
                    format!(
                        "{{\"label\":{},\"mean\":{}}}",
                        serde_json::to_string(label).expect("string"),
                        value.map_or_else(|| "null".to_owned(), |value| format!("{value:.2}"))
                    )
                })
                .collect();
            json_engines.push(format!(
                "{{\"engine\":\"{}\",\"boundaries_lowest\":{},\"boundaries_volume\":{},\
                 \"rms_env\":{},\"rms_harmonic\":{},\"percent_modified\":{:.3},\
                 \"specks_lowest\":{},\"couplet_max_dv\":{},\"max_inbound\":{},\
                 \"multifold_gates\":{},\"multifold_speckle\":{},\
                 \"volume_ms\":{:.1},\"worst_tilt_ms\":{:.1},\
                 \"amortized\":{},\"deterministic\":{},\"correct_branch_percent\":{},\
                 \"probes\":[{}]}}",
                engine.name(),
                report.boundaries_lowest,
                report.boundaries_volume,
                report
                    .rms_env
                    .map_or_else(|| "null".to_owned(), |value| format!("{value:.2}")),
                report
                    .rms_harmonic
                    .map_or_else(|| "null".to_owned(), |value| format!("{value:.2}")),
                report.percent_modified,
                report.specks_lowest,
                report
                    .couplet_max_dv
                    .map_or_else(|| "null".to_owned(), |value| format!("{value:.2}")),
                report
                    .max_inbound
                    .map_or_else(|| "null".to_owned(), |value| format!("{value:.2}")),
                report.multifold_gates,
                report.multifold_speckle,
                report.volume_ms,
                report.worst_tilt_ms,
                report.amortized,
                report.deterministic,
                report
                    .correct_branch_percent
                    .map_or_else(|| "null".to_owned(), |value| format!("{value:.2}")),
                probes.join(",")
            ));
        } else {
            lines.push(format!(
                "{:<9} bnd_low {:>6}  bnd_vol {:>7}  rmsE {:>6}  rmsH {:>6}  mod% {:>6.2}  \
                 spk {:>4}  cplt {:>6}  inb {:>6}  mf {:>6}/{:<4}  vol {:>8.1}ms  \
                 tilt {:>7.1}ms{}  det {}{}",
                engine.name(),
                report.boundaries_lowest,
                report.boundaries_volume,
                format_option_f64(report.rms_env, 2),
                format_option_f64(report.rms_harmonic, 2),
                report.percent_modified,
                report.specks_lowest,
                format_option_f64(report.couplet_max_dv, 1),
                format_option_f32(report.max_inbound),
                report.multifold_gates,
                report.multifold_speckle,
                report.volume_ms,
                report.worst_tilt_ms,
                if report.amortized { "*" } else { " " },
                if report.deterministic { "yes" } else { "NO" },
                report
                    .correct_branch_percent
                    .map_or_else(String::new, |value| format!("  correct% {value:.2}"))
            ));
            for (label, value) in &report.probes {
                lines.push(format!(
                    "          probe {label}: {}",
                    format_option_f32(*value)
                ));
            }
        }
    }

    if args.json {
        println!(
            "{{\"case\":{},\"target\":{},\"env_source\":{},\"rewrap\":{},\"engines\":[{}],\"deterministic\":{}}}",
            serde_json::to_string(&args.case).expect("string"),
            serde_json::to_string(&args.target.display().to_string()).expect("string"),
            environment.as_ref().map_or_else(
                || "null".to_owned(),
                |(_, source)| serde_json::to_string(source).expect("string")
            ),
            args.rewrap
                .map_or_else(|| "null".to_owned(), |nyquist| format!("{nyquist}")),
            json_engines.join(","),
            all_deterministic
        );
    } else {
        println!(
            "case {}  site {}  volume {}  lowest vel cut {} ({:.2} deg)",
            args.case,
            volume.attrs.instrument_name,
            volume
                .time_coverage
                .map_or(volume.time_reference, |coverage| coverage.start)
                .to_rfc3339(),
            lowest_velocity_cut(&volume).unwrap_or(0),
            volume
                .tilt_elevation_deg(lowest_velocity_cut(&volume).unwrap_or(0))
                .unwrap_or(f32::NAN),
        );
        if let Some((_, source)) = &environment {
            println!("env  {source}");
        }
        if let Some(nyquist) = args.rewrap {
            println!("MODE Case-E rewrap to Nyquist {nyquist} m/s (single tilt, no priors)");
        }
        for line in &lines {
            println!("{line}");
        }
        println!("(* = amortized: one volume solve serves every tilt)");
    }
    Ok(all_deterministic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// `testdata/golden/bench/dealias_eval.json`, written by
    /// `tools/render_bench_golden.py bench`: Py-ART 2.2.5 raw codes and
    /// `dealias_region_based`, MetPy 1.7.1 values, and the metrics recomputed
    /// in numpy from the documented rules (spec §10.2).
    fn golden_case(id: &str) -> Value {
        let path = recast_radar_testdata::testdata_dir()
            .join("golden")
            .join("bench")
            .join("dealias_eval.json");
        let text =
            fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let golden: Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        golden["cases"]
            .as_array()
            .expect("cases")
            .iter()
            .find(|case| case["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no golden case for {id}"))
    }

    fn as_usize(value: &Value) -> usize {
        value
            .as_u64()
            .unwrap_or_else(|| panic!("expected an unsigned integer, got {value}")) as usize
    }

    fn as_f64(value: &Value) -> f64 {
        value
            .as_f64()
            .unwrap_or_else(|| panic!("expected a number, got {value}"))
    }

    /// Decode a corpus Level II file through the bench's own byte router and
    /// return the velocity tilt the golden case describes, checked against the
    /// golden geometry (rows, gates, azimuth span, Nyquist, finite gates).
    fn golden_plane(path: &Path, case: &Value) -> Plane {
        let raw = fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let volume = recast_radar_io::read_supported_volume_bytes(&raw)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let sweep = &volume.sweeps[as_usize(&case["sweep"])];
        let velocity = velocity_field(sweep);
        assert!(velocity.absent_rows.is_empty(), "every ray has velocity");
        let field = decode_field(sweep, velocity);
        assert_eq!(field.rows, as_usize(&case["rows"]));
        assert_eq!(field.gates, as_usize(&case["gates"]));
        assert_eq!(field.wraps, case["wraps"].as_bool().expect("wraps"));
        let first = sweep.rays.azimuth_deg[0];
        let last = sweep.rays.azimuth_deg[field.rows - 1];
        assert_eq!(first, as_f64(&case["first_azimuth_deg"]) as f32);
        assert_eq!(last, as_f64(&case["last_azimuth_deg"]) as f32);
        let nyquist: Vec<f32> = case["nyquist_mps"]
            .as_array()
            .expect("nyquist_mps")
            .iter()
            .map(|value| as_f64(value) as f32)
            .collect();
        assert!(
            field.nyq.iter().all(|nyq| nyquist.contains(nyq)),
            "per-row Nyquist velocities {:?} are not the file's {nyquist:?}",
            field.nyq
        );
        let finite = field
            .values
            .iter()
            .filter(|value| value.is_finite())
            .count();
        assert_eq!(finite, as_usize(&case["finite_gates"]));
        field
    }

    /// Apply Py-ART's region-based fold numbers (run-length encoded per row
    /// in the golden file) to a raw field: the dealiased output is
    /// `raw + 2·N·k` gate by gate, exactly as `dealias_region_based` forms it.
    fn unfold_like_pyart(field: &mut Plane, region: &Value) {
        let runs = region["unwrap_runs"].as_array().expect("unwrap_runs");
        assert_eq!(runs.len(), field.rows);
        for (row, row_runs) in runs.iter().enumerate() {
            let interval = 2.0 * field.nyq[row];
            let mut gate = 0usize;
            for run in row_runs.as_array().expect("row runs") {
                let fold = run[0].as_i64().expect("fold") as f32;
                let length = as_usize(&run[1]);
                if fold != 0.0 {
                    for value in &mut field.values[row * field.gates + gate..][..length] {
                        if value.is_finite() {
                            *value += interval * fold;
                        }
                    }
                }
                gate += length;
            }
            assert_eq!(gate, field.gates, "row {row} runs cover the row");
        }
    }

    /// Fold-boundary pairs of a real aliased sweep and of Py-ART's unfolding
    /// of it: the KDVN 2020-08-10 derecho Doppler cut (Nyquist 21.03 m/s,
    /// 84,964 finite gates) carries 7,486 boundary pairs raw; the region-based
    /// output (38,656 gates moved by whole folds) keeps 409.
    #[test]
    fn boundary_metric_counts_real_fold_boundaries_before_and_after_unfolding() {
        let case = golden_case("l2-kdvn-20200810-180401-trim");
        let path = recast_radar_testdata::require_file!("l2-kdvn-20200810-180401-trim");
        let raw = golden_plane(&path, &case);
        assert!(!raw.wraps);
        assert_eq!(boundary_pairs(&raw), as_usize(&case["boundary_pairs"]));

        let region = &case["pyart_region"];
        let mut unfolded = golden_plane(&path, &case);
        unfold_like_pyart(&mut unfolded, region);
        let unfolded_pairs = boundary_pairs(&unfolded);
        assert_eq!(unfolded_pairs, as_usize(&region["boundary_pairs"]));
        assert!(unfolded_pairs * 10 < boundary_pairs(&raw));
    }

    /// The azimuth wrap seam counts when the sweep closes the circle: the
    /// full KDVN 2020-08-10 Doppler cut (720 super-resolution radials, 286.2
    /// deg round to 285.7 deg) starts inside the aliased derecho, so 31 of its
    /// fold-boundary pairs sit between the last and the first radial and only
    /// the wrapped count includes them.
    #[test]
    fn boundary_metric_counts_the_wrap_seam() {
        let case = golden_case("l2-kdvn-20200810-180401");
        let path = recast_radar_testdata::require_file!("l2-kdvn-20200810-180401");
        let mut field = golden_plane(&path, &case);
        assert!(field.wraps);
        let with_seam = as_usize(&case["boundary_pairs"]);
        let without_seam = as_usize(&case["boundary_pairs_without_seam"]);
        assert!(with_seam > without_seam);
        assert_eq!(boundary_pairs(&field), with_seam);
        assert_eq!(speck_count(&field), as_usize(&case["speck_count"]));
        field.wraps = false;
        assert_eq!(boundary_pairs(&field), without_seam);
        assert_eq!(
            speck_count(&field),
            as_usize(&case["speck_count_without_seam"])
        );
    }

    /// Percent modified counts the gates Py-ART moved by a whole fold (2·N
    /// or 4·N here) and nothing else: 38,656 of 84,964 finite KDVN gates.
    #[test]
    fn percent_modified_counts_whole_fold_moves_only() {
        let case = golden_case("l2-kdvn-20200810-180401-trim");
        let path = recast_radar_testdata::require_file!("l2-kdvn-20200810-180401-trim");
        let raw = golden_plane(&path, &case);
        assert_eq!(percent_modified(&raw, &raw), 0.0);

        let region = &case["pyart_region"];
        let mut unfolded = golden_plane(&path, &case);
        unfold_like_pyart(&mut unfolded, region);
        let expected = 100.0 * as_f64(&region["unfolded_gates"]) / as_f64(&region["finite_gates"]);
        assert!((expected - as_f64(&region["percent_modified"])).abs() < 1e-9);
        assert!((percent_modified(&unfolded, &raw) - expected).abs() < 1e-9);
        assert!(region["max_abs_fold"].as_i64().expect("max_abs_fold") >= 1);
    }

    /// Isolated specks on real sweeps: the PGUA 2023-05-24 Typhoon Mawar
    /// Doppler cut (Nyquist 35.55 m/s) and the KDVN derecho cut, where
    /// Py-ART's unfolding removes most of the raw specks (1,212 to 164).
    #[test]
    fn speck_count_finds_isolated_outliers_only() {
        let case = golden_case("l2-pgua-20230524-030945-trim");
        let path = recast_radar_testdata::require_file!("l2-pgua-20230524-030945-trim");
        let field = golden_plane(&path, &case);
        assert_eq!(speck_count(&field), as_usize(&case["speck_count"]));

        let case = golden_case("l2-kdvn-20200810-180401-trim");
        let path = recast_radar_testdata::require_file!("l2-kdvn-20200810-180401-trim");
        let raw = golden_plane(&path, &case);
        assert_eq!(speck_count(&raw), as_usize(&case["speck_count"]));
        let region = &case["pyart_region"];
        let mut unfolded = golden_plane(&path, &case);
        unfold_like_pyart(&mut unfolded, region);
        let unfolded_specks = speck_count(&unfolded);
        assert_eq!(unfolded_specks, as_usize(&region["speck_count"]));
        assert!(unfolded_specks * 5 < speck_count(&raw));
    }

    #[test]
    fn dealias_args_parse_round_trip() {
        let args: Vec<String> = [
            "--dealias",
            "--target",
            "KEAX.V06",
            "--prior",
            "KEAX_prev.V06",
            "--engines",
            "region,v4",
            "--probe",
            "339,20,blob",
            "--rewrap",
            "12",
            "--iters",
            "2",
            "--case",
            "A",
            "--json",
            "--dump-fields",
            "dump-dir",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let parsed = parse_dealias_args(&args).expect("parse");
        assert_eq!(parsed.target, PathBuf::from("KEAX.V06"));
        assert_eq!(parsed.engines, vec![Engine::Region, Engine::V4]);
        assert_eq!(parsed.probes.len(), 1);
        assert_eq!(parsed.probes[0].label, "blob");
        assert_eq!(parsed.rewrap, Some(12.0));
        assert_eq!(parsed.iters, 2);
        assert!(parsed.json);
        assert_eq!(parsed.dump_fields, Some(PathBuf::from("dump-dir")));
        assert!(parse_dealias_args(&["--dealias".to_owned()]).is_err());
        assert!(
            parse_dealias_args(&["--target".to_owned(), "x".to_owned(), "--bogus".to_owned()])
                .is_err()
        );
    }
}
