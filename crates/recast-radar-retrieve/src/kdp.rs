//! Specific differential phase (KDP) estimators on one ray of prefiltered
//! differential phase.
//!
//! [`KdpMethod`] selects how [`DerivedSweepProduct::Kdp`](crate::DerivedSweepProduct::Kdp)
//! is computed. Every method shares the phase front end of
//! `sweep::derive_phase_products` (RHOHV / reflectivity gating, 360 degree
//! unwrapping, linear fill of short gaps and a Hampel filter); they differ in
//! how the filtered phase becomes KDP:
//!
//! - [`KdpMethod::WindowedRegression`]: a Huber-weighted linear fit over a
//!   range window, KDP = half the slope (the crate's original estimator).
//! - [`KdpMethod::Vulpiani`]: Vulpiani et al. (2012), as Py-ART's
//!   `pyart.retrieve.kdp_vulpiani` computes it: a finite-difference range
//!   derivative, band bounds and a texture test, then repeated integration of
//!   KDP into phase and differentiation of that phase.
//! - [`KdpMethod::Maesaka`]: Maesaka et al. (2012), as Py-ART's
//!   `pyart.retrieve.kdp_maesaka` formulates it: KDP = k^2 / (2 dr), with k
//!   the minimiser of forward and reverse phase misfits plus a radial
//!   smoothness penalty on k, so KDP is never negative. The minimisation runs
//!   per ray to convergence (Py-ART minimises all rays of a radar jointly with
//!   50 conjugate-gradient iterations by default); the cost function, its
//!   boundary conditions and first guess are Py-ART's. The output is the
//!   converged minimum of Py-ART's cost, not what `kdp_maesaka` returns
//!   with its defaults (see `docs/design/retrievals-validation.md`). Like
//!   Py-ART's, it applies no band bounds.

use rayon::prelude::*;

/// Vulpiani et al. (2012) KDP settings (Py-ART `kdp_vulpiani` arguments).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VulpianiKdp {
    /// Range derivative window in gates (Py-ART `windsize`). Must be even and
    /// at least 2; an odd value is rounded up.
    pub window_gates: usize,
    /// Integrate-and-differentiate passes (Py-ART `n_iter`).
    pub iterations: usize,
    /// First-guess KDP gates whose local standard deviation over
    /// `window_gates + 1` gates exceeds this (deg/km) are zeroed before the
    /// iterations (Py-ART's fixed 5 deg/km).
    pub texture_threshold_deg_km: f64,
}

impl Default for VulpianiKdp {
    fn default() -> Self {
        Self {
            window_gates: 10,
            iterations: 10,
            texture_threshold_deg_km: 5.0,
        }
    }
}

/// Maesaka et al. (2012) variational KDP settings (Py-ART `kdp_maesaka`
/// arguments, plus the per-ray solver's stopping rule).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaesakaKdp {
    /// Low-pass (radial smoothness) constraint weight `Clpf`.
    pub low_pass_weight: f64,
    /// Length scale (m) that brings the smoothness term in line with the
    /// phase misfit; `None` uses the gate spacing (Py-ART's default).
    pub length_scale_m: Option<f64>,
    /// First guess of the control variable k at every gate.
    pub first_guess: f64,
    /// Contiguous valid gates needed at each end of a ray to set the near
    /// and far phase boundary conditions (Py-ART `n`).
    pub boundary_gates: usize,
    /// Replace near-range boundary conditions outside the system-phase
    /// histogram peak by the system-phase estimate (Py-ART
    /// `check_outliers`).
    pub check_outliers: bool,
    /// Solver iteration limit per ray.
    pub max_iterations: usize,
    /// Cost-function evaluations allowed per ray, line-search trials
    /// included; the solver stops at the best point found when it reaches
    /// this. One evaluation is linear in the ray's gate count, so a sweep
    /// costs at most rays x this x gates work whatever its phase holds
    /// (without it, `max_iterations` line searches of up to 40 evaluations
    /// each). The default, 10,000, is about twice the most any ray of the
    /// three validation cuts in `docs/design/retrievals-validation.md` used
    /// (4,674, on a ray that stopped at `max_iterations`), so it does not
    /// bind there.
    pub max_cost_evaluations: usize,
    /// Solver stops when the largest gradient component falls below this.
    pub gradient_tolerance: f64,
    /// Solver stops when one iteration lowers the cost by less than this
    /// fraction of its magnitude.
    pub relative_cost_tolerance: f64,
}

impl Default for MaesakaKdp {
    fn default() -> Self {
        Self {
            low_pass_weight: 1.0,
            length_scale_m: None,
            first_guess: 0.01,
            boundary_gates: 20,
            check_outliers: true,
            max_iterations: 4000,
            max_cost_evaluations: 10_000,
            gradient_tolerance: 1.0e-6,
            relative_cost_tolerance: 1.0e-13,
        }
    }
}

/// How KDP is estimated from the filtered differential phase.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub enum KdpMethod {
    /// Huber-weighted linear regression over
    /// [`KdpConfig::window_km`](crate::KdpConfig::window_km); KDP is half the
    /// slope and the product set includes its standard error
    /// ([`DerivedSweepProduct::KdpUncertainty`](crate::DerivedSweepProduct::KdpUncertainty)).
    #[default]
    WindowedRegression,
    /// Vulpiani et al. (2012), Py-ART `kdp_vulpiani`.
    Vulpiani(VulpianiKdp),
    /// Maesaka et al. (2012), Py-ART `kdp_maesaka`'s cost minimised per ray
    /// to convergence; non-negative KDP.
    ///
    /// No band bounds are applied (Py-ART applies none), so where the phase
    /// is not monotone the method concentrates the phase rise into spikes
    /// that no rain produces: the reported gates of the three real S-band
    /// cuts in `docs/design/retrievals-validation.md` reach 333 deg/km (Moore
    /// hail core), 86 deg/km (Hurricane Ida) and 47 deg/km (Iowa derecho).
    /// Those values flow into the KDP-derived products computed with this
    /// method (the KDP and hybrid rain rates, KDP texture); bound or mask
    /// them before such use. The attenuation products do not use them: they
    /// always take the windowed-regression phase and KDP.
    Maesaka(MaesakaKdp),
}

/// Py-ART `_kdp_vulpiani_profile` on one ray: `psidp` is the filtered phase
/// (deg, NaN = missing), `dr_km` the gate spacing. Returns KDP (deg/km, NaN
/// where `psidp` is missing) and the phase reconstructed from it (deg, zero
/// at the first gate, NaN where `psidp` is missing).
///
/// The bounds `(low, high)` zero first-guess and iterated KDP at or beyond
/// them (Py-ART's band thresholds: -2 and 14, 20 or 40 deg/km at S, C and X
/// band).
pub(crate) fn vulpiani_profile(
    psidp: &[f64],
    dr_km: f64,
    config: &VulpianiKdp,
    bounds: (f64, f64),
) -> (Vec<f64>, Vec<f64>) {
    let nn = psidp.len();
    let size = config.window_gates.max(2).next_multiple_of(2);
    let half = size / 2;
    let denominator = 2.0 * size as f64 * dr_km;
    let (low, high) = bounds;
    let mut kdp = vec![0.0f64; nn];
    if nn > size {
        for gate in half..nn - half {
            kdp[gate] = (psidp[gate + half] - psidp[gate - half]) / denominator;
        }
    }
    let clamp_edges_and_bounds = |kdp: &mut [f64]| {
        for value in kdp.iter_mut().take(half) {
            *value = 0.0;
        }
        for value in kdp.iter_mut().skip(nn.saturating_sub(half)) {
            *value = 0.0;
        }
        for value in kdp.iter_mut() {
            if *value <= low || *value >= high {
                *value = 0.0;
            }
        }
    };
    clamp_edges_and_bounds(&mut kdp);
    for value in &mut kdp {
        if value.is_nan() {
            *value = 0.0;
        }
    }

    // Local standard deviation over size + 1 gates, centred, for the gates
    // at least `half` from either end; the texture test zeroes gates above
    // the threshold after every deviation is computed.
    if nn > size {
        let window = size + 1;
        let mut rough = Vec::new();
        for gate in half..nn - half {
            let samples = &kdp[gate - half..gate - half + window];
            let mean = samples.iter().sum::<f64>() / window as f64;
            let variance = samples
                .iter()
                .map(|value| (value - mean) * (value - mean))
                .sum::<f64>()
                / window as f64;
            if variance.sqrt() > config.texture_threshold_deg_km {
                rough.push(gate);
            }
        }
        for gate in rough {
            kdp[gate] = 0.0;
        }
    }

    let mut phase = vec![0.0f64; nn];
    for _ in 0..config.iterations {
        cumulative_phase(&kdp, dr_km, &mut phase);
        if nn > size {
            for gate in half..nn - half {
                kdp[gate] = (phase[gate + half] - phase[gate - half]) / denominator;
            }
        }
        clamp_edges_and_bounds(&mut kdp);
    }

    // Censor KDP where the phase was missing; the final reconstruction sums
    // the censored KDP with missing gates as zero.
    for (value, input) in kdp.iter_mut().zip(psidp) {
        if !input.is_finite() {
            *value = 0.0;
        }
    }
    cumulative_phase(&kdp, dr_km, &mut phase);
    for ((value, rebuilt), input) in kdp.iter_mut().zip(phase.iter_mut()).zip(psidp) {
        if !input.is_finite() {
            *value = f64::NAN;
            *rebuilt = f64::NAN;
        }
    }
    (kdp, phase)
}

/// `cumsum(kdp) * 2 * dr_km`, in Py-ART's operation order.
fn cumulative_phase(kdp: &[f64], dr_km: f64, out: &mut [f64]) {
    let mut sum = 0.0f64;
    for (value, slot) in kdp.iter().zip(out.iter_mut()) {
        sum += value;
        *slot = sum * 2.0 * dr_km;
    }
}

/// Near and far propagation-phase boundary conditions of one ray (Py-ART
/// `boundary_conditions_maesaka`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MaesakaBounds {
    pub(crate) phi_near: f64,
    pub(crate) phi_far: f64,
    /// First gate of the near window; 0 when no window qualifies.
    pub(crate) idx_near: usize,
    /// One past the last gate of the far window (Py-ART keeps this gate
    /// too); 0 when no window qualifies.
    pub(crate) idx_far: usize,
}

/// Boundary conditions for every ray of a sweep (Py-ART
/// `boundary_conditions_maesaka` with the default outlier check). `rows`
/// holds the filtered phase of each ray (NaN = missing); `range_m` the gate
/// ranges used by the slope test.
pub(crate) fn maesaka_boundary_conditions(
    rows: &[Vec<f64>],
    range_m: &[f64],
    config: &MaesakaKdp,
) -> Vec<MaesakaBounds> {
    let n = config.boundary_gates.max(2);
    let mut bounds: Vec<MaesakaBounds> = rows
        .iter()
        .map(|row| {
            let regions = finite_runs(row);
            let mut out = MaesakaBounds {
                phi_near: 0.0,
                phi_far: 0.0,
                idx_near: 0,
                idx_far: 0,
            };
            if let Some(&(start, _)) = regions.iter().find(|(start, stop)| stop - start >= n) {
                let window = &row[start..start + n];
                out.idx_near = start;
                out.phi_near = if regression_slope(&range_m[start..start + n], window) > 0.0 {
                    window[0]
                } else {
                    median(window)
                };
            }
            if let Some(&(_, stop)) = regions.iter().rev().find(|(start, stop)| stop - start >= n) {
                let window = &row[stop - n..stop];
                out.idx_far = stop;
                out.phi_far = if regression_slope(&range_m[stop - n..stop], window) > 0.0 {
                    window[n - 1]
                } else {
                    median(window)
                };
            }
            out
        })
        .collect();

    // Py-ART's outlier check: a 5 degree histogram of the nonzero near
    // conditions over [-360, 360]; its peak bin and the nearest bins on
    // either side holding at most 5 values bracket the system phase. Rays
    // whose near condition falls outside the bracket take the median of the
    // nonzero near conditions (Py-ART selects the values it takes the median
    // of with a logical or of the two bracket tests, which keeps every
    // value).
    let valid: Vec<f64> = bounds
        .iter()
        .map(|b| b.phi_near)
        .filter(|value| *value != 0.0)
        .collect();
    if config.check_outliers
        && !valid.is_empty()
        && let Some((left_edge, right_edge)) = system_phase_bracket(&valid)
    {
        let offset = median(&valid);
        for bound in &mut bounds {
            if bound.phi_near < left_edge || bound.phi_near > right_edge {
                bound.phi_near = offset;
            }
        }
    }
    for bound in &mut bounds {
        if bound.phi_far - bound.phi_near < 0.0 {
            bound.phi_far = bound.phi_near;
        }
    }
    bounds
}

/// `[start, stop)` runs of finite values.
fn finite_runs(row: &[f64]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut start = None;
    for (index, value) in row.iter().enumerate() {
        match (value.is_finite(), start) {
            (true, None) => start = Some(index),
            (false, Some(begin)) => {
                runs.push((begin, index));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        runs.push((begin, row.len()));
    }
    runs
}

/// Least-squares slope of `ys` on `xs` (the sign is all the caller uses).
fn regression_slope(xs: &[f64], ys: &[f64]) -> f64 {
    let n = xs.len() as f64;
    let x_mean = xs.iter().sum::<f64>() / n;
    let y_mean = ys.iter().sum::<f64>() / n;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (x, y) in xs.iter().zip(ys) {
        sxy += (x - x_mean) * (y - y_mean);
        sxx += (x - x_mean) * (x - x_mean);
    }
    if sxx > 0.0 { sxy / sxx } else { 0.0 }
}

/// numpy `median` of a non-empty slice (mean of the middle pair for an even
/// count).
fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        0.5 * (sorted[middle - 1] + sorted[middle])
    } else {
        sorted[middle]
    }
}

/// Left and right edges of the system-phase peak in a 144-bin histogram of
/// `values` over [-360, 360] (Py-ART's outlier check); `None` when no bin
/// with at most 5 values lies on one side (Py-ART raises there).
fn system_phase_bracket(values: &[f64]) -> Option<(f64, f64)> {
    const BINS: usize = 144;
    let edge = |index: usize| -360.0 + 5.0 * index as f64;
    let mut counts = [0usize; BINS];
    for &value in values {
        if !(-360.0..=360.0).contains(&value) {
            continue;
        }
        let bin = (((value + 360.0) / 5.0).floor() as usize).min(BINS - 1);
        counts[bin] += 1;
    }
    let mut peak = 0;
    for (bin, count) in counts.iter().enumerate() {
        if *count > counts[peak] {
            peak = bin;
        }
    }
    let peak_left = edge(peak);
    let peak_right = edge(peak + 1);
    let left = (0..BINS).rfind(|&bin| edge(bin) < peak_left && counts[bin] <= 5)?;
    let right = (0..BINS).find(|&bin| edge(bin + 1) > peak_right && counts[bin] <= 5)?;
    Some((edge(left), edge(right + 1)))
}

/// Solution of the Maesaka problem on one ray.
pub(crate) struct MaesakaRay {
    /// KDP (deg/km) at every gate: k^2 / (2 dr).
    pub(crate) kdp: Vec<f64>,
    /// Forward propagation phase phi_near + sum of k^2 before each gate.
    pub(crate) phidp_forward: Vec<f64>,
    /// Gates that carried an observation in the cost.
    pub(crate) observed: Vec<bool>,
}

/// Minimise Py-ART's Maesaka cost on one ray: `psidp` is the filtered phase
/// (NaN = missing), `bounds` the ray's boundary conditions, `dr_m` the gate
/// spacing.
pub(crate) fn maesaka_ray(
    psidp: &[f64],
    bounds: MaesakaBounds,
    dr_m: f64,
    config: &MaesakaKdp,
) -> MaesakaRay {
    let ng = psidp.len();
    // Observation weights: finite phase at gates idx_near..=idx_far.
    let observed: Vec<bool> = psidp
        .iter()
        .enumerate()
        .map(|(gate, value)| value.is_finite() && gate >= bounds.idx_near && gate <= bounds.idx_far)
        .collect();
    let length_scale = config.length_scale_m.unwrap_or(dr_m);
    // Py-ART: Clpf * length_scale^4 * sum((d2k/dr2)^2) / 2 with d2k/dr2 the
    // second difference over dr^2.
    let smoothness = config.low_pass_weight * (length_scale / dr_m).powi(4);
    let problem = MaesakaProblem {
        forward_obs: psidp.iter().map(|value| value - bounds.phi_near).collect(),
        reverse_obs: psidp.iter().map(|value| bounds.phi_far - value).collect(),
        observed: &observed,
        smoothness,
    };
    let mut k = vec![config.first_guess; ng];
    if ng >= 3 {
        lbfgs_minimize(&problem, &mut k, config);
    }
    let kdp = k.iter().map(|k| k * k / (2.0 * dr_m) * 1000.0).collect();
    let mut phidp_forward = vec![bounds.phi_near; ng];
    let mut sum = 0.0;
    for gate in 1..ng {
        sum += k[gate - 1] * k[gate - 1];
        phidp_forward[gate] = bounds.phi_near + sum;
    }
    MaesakaRay {
        kdp,
        phidp_forward,
        observed,
    }
}

/// One ray's Maesaka cost J(k) = Jof + Jor + Jlpf (Py-ART `_cost_maesaka`).
struct MaesakaProblem<'a> {
    /// psidp - phi_near.
    forward_obs: Vec<f64>,
    /// phi_far - psidp.
    reverse_obs: Vec<f64>,
    observed: &'a [bool],
    /// Weight of 0.5 * sum((second difference of k)^2).
    smoothness: f64,
}

impl MaesakaProblem<'_> {
    /// Cost at `k`, gradient into `gradient`.
    fn evaluate(&self, k: &[f64], gradient: &mut [f64]) -> f64 {
        let ng = k.len();
        let mut cost = 0.0;
        gradient.fill(0.0);

        // Forward: phi_fa[g] = sum_{i<g} k_i^2; dJof/dk_i = 2 k_i
        // sum_{g>i} Cobs_g (phi_fa_g - phi_fo_g).
        let mut residual = vec![0.0f64; ng];
        let mut phi = 0.0;
        for gate in 0..ng {
            if gate > 0 {
                phi += k[gate - 1] * k[gate - 1];
            }
            if self.observed[gate] {
                let misfit = phi - self.forward_obs[gate];
                cost += 0.5 * misfit * misfit;
                residual[gate] = misfit;
            }
        }
        let mut tail = 0.0;
        for gate in (0..ng).rev() {
            gradient[gate] += 2.0 * k[gate] * tail;
            tail += residual[gate];
        }

        // Reverse: phi_ra[g] = sum_{i>g} k_i^2; dJor/dk_i = 2 k_i
        // sum_{g<i} Cobs_g (phi_ra_g - phi_ro_g).
        residual.fill(0.0);
        let mut phi = 0.0;
        for gate in (0..ng).rev() {
            if gate + 1 < ng {
                phi += k[gate + 1] * k[gate + 1];
            }
            if self.observed[gate] {
                let misfit = phi - self.reverse_obs[gate];
                cost += 0.5 * misfit * misfit;
                residual[gate] = misfit;
            }
        }
        let mut head = 0.0;
        for gate in 0..ng {
            gradient[gate] += 2.0 * k[gate] * head;
            head += residual[gate];
        }

        // Smoothness: second differences, one-sided at both ends as Py-ART's
        // low-order scheme (gate 0 uses k0, k1, k2; the last gate k[n-1],
        // k[n-2], k[n-3]).
        let weight = self.smoothness;
        let add = |stencil: [usize; 3], gradient: &mut [f64], cost: &mut f64| {
            let d2 = k[stencil[0]] - 2.0 * k[stencil[1]] + k[stencil[2]];
            *cost += 0.5 * weight * d2 * d2;
            gradient[stencil[0]] += weight * d2;
            gradient[stencil[1]] -= 2.0 * weight * d2;
            gradient[stencil[2]] += weight * d2;
        };
        add([0, 1, 2], gradient, &mut cost);
        for gate in 1..ng - 1 {
            add([gate + 1, gate, gate - 1], gradient, &mut cost);
        }
        add([ng - 1, ng - 2, ng - 3], gradient, &mut cost);
        cost
    }
}

/// Limited-memory BFGS (Nocedal and Wright 2006, algorithms 7.4 and 7.5)
/// with a strong-Wolfe line search (algorithms 3.5 and 3.6).
fn lbfgs_minimize(problem: &MaesakaProblem<'_>, x: &mut [f64], config: &MaesakaKdp) {
    const MEMORY: usize = 12;
    let n = x.len();
    let mut gradient = vec![0.0; n];
    let mut cost = problem.evaluate(x, &mut gradient);
    let mut evaluations = 1usize;
    let mut s_hist: Vec<Vec<f64>> = Vec::with_capacity(MEMORY);
    let mut y_hist: Vec<Vec<f64>> = Vec::with_capacity(MEMORY);
    let mut rho_hist: Vec<f64> = Vec::with_capacity(MEMORY);
    let mut direction = vec![0.0; n];
    let mut alpha = [0.0; MEMORY];
    let mut trial = vec![0.0; n];
    let mut trial_gradient = vec![0.0; n];

    for _ in 0..config.max_iterations {
        if max_abs(&gradient) <= config.gradient_tolerance
            || !cost.is_finite()
            || evaluations >= config.max_cost_evaluations
        {
            break;
        }
        // Two-loop recursion: direction = -H grad.
        for (d, g) in direction.iter_mut().zip(&gradient) {
            *d = -g;
        }
        for index in (0..s_hist.len()).rev() {
            alpha[index] = rho_hist[index] * dot(&s_hist[index], &direction);
            axpy(-alpha[index], &y_hist[index], &mut direction);
        }
        let gamma = match (s_hist.last(), y_hist.last()) {
            (Some(s), Some(y)) => dot(s, y) / dot(y, y),
            _ => 1.0 / max_abs(&gradient).max(f64::MIN_POSITIVE),
        };
        for d in &mut direction {
            *d *= gamma;
        }
        for index in 0..s_hist.len() {
            let beta = rho_hist[index] * dot(&y_hist[index], &direction);
            axpy(alpha[index] - beta, &s_hist[index], &mut direction);
        }
        let mut slope = dot(&gradient, &direction);
        if slope >= 0.0 || !slope.is_finite() {
            // Not a descent direction: restart from steepest descent.
            s_hist.clear();
            y_hist.clear();
            rho_hist.clear();
            let scale = 1.0 / max_abs(&gradient).max(f64::MIN_POSITIVE);
            for (d, g) in direction.iter_mut().zip(&gradient) {
                *d = -g * scale;
            }
            slope = dot(&gradient, &direction);
        }
        let mut spent = 0usize;
        let accepted = wolfe_line_search(
            problem,
            LineStart {
                x,
                cost,
                slope,
                direction: &direction,
            },
            &mut trial,
            &mut trial_gradient,
            config.max_cost_evaluations - evaluations,
            &mut spent,
        );
        evaluations += spent;
        let Some(new_cost) = accepted else {
            break;
        };
        // s = x_new - x, y = g_new - g.
        let mut s = vec![0.0; n];
        let mut y = vec![0.0; n];
        for index in 0..n {
            s[index] = trial[index] - x[index];
            y[index] = trial_gradient[index] - gradient[index];
        }
        x.copy_from_slice(&trial);
        gradient.copy_from_slice(&trial_gradient);
        let previous = cost;
        cost = new_cost;
        let sy = dot(&s, &y);
        if sy > f64::EPSILON * dot(&y, &y) {
            if s_hist.len() == MEMORY {
                s_hist.remove(0);
                y_hist.remove(0);
                rho_hist.remove(0);
            }
            s_hist.push(s);
            y_hist.push(y);
            rho_hist.push(1.0 / sy);
        }
        if previous - cost
            <= config.relative_cost_tolerance * previous.abs().max(cost.abs()).max(1.0)
        {
            break;
        }
    }
}

/// Where a line search starts: the point, its cost, the search direction
/// and the directional derivative along it (< 0).
#[derive(Clone, Copy)]
struct LineStart<'a> {
    x: &'a [f64],
    cost: f64,
    slope: f64,
    direction: &'a [f64],
}

/// Strong-Wolfe line search along `start.direction`, Nocedal and Wright
/// algorithms 3.5 (bracketing) and 3.6 (zoom, cubic interpolation), with at
/// most 40 cost evaluations and never more than `budget`; the evaluations
/// spent are added to `evaluations`. Returns the accepted cost, with `trial`
/// and `trial_gradient` holding the accepted point and its gradient, or
/// `None` when no step lowered the cost.
fn wolfe_line_search(
    problem: &MaesakaProblem<'_>,
    start: LineStart<'_>,
    trial: &mut [f64],
    trial_gradient: &mut [f64],
    budget: usize,
    evaluations: &mut usize,
) -> Option<f64> {
    const C1: f64 = 1.0e-4;
    const C2: f64 = 0.9;
    let LineStart {
        x,
        cost,
        slope,
        direction,
    } = start;
    // Keep one evaluation for re-evaluating the best point.
    let max_steps = 40usize.min(budget.saturating_sub(1));
    if max_steps == 0 {
        return None;
    }
    let mut steps = 0usize;
    let mut evaluate = |step: f64, trial: &mut [f64], trial_gradient: &mut [f64]| {
        *evaluations += 1;
        for ((t, x), d) in trial.iter_mut().zip(x).zip(direction) {
            *t = x + step * d;
        }
        let value = problem.evaluate(trial, trial_gradient);
        (value, dot(trial_gradient, direction))
    };
    let armijo = |step: f64, value: f64| value.is_finite() && value <= cost + C1 * step * slope;

    // Bracketing: grow the step until it overshoots or satisfies both
    // conditions. `lo` always satisfies sufficient decrease.
    let (mut lo, mut lo_cost, mut lo_slope) = (0.0, cost, slope);
    let (mut hi, mut hi_cost, mut hi_slope);
    let mut step = 1.0;
    loop {
        if steps >= max_steps {
            return best_point(lo, &mut evaluate, trial, trial_gradient);
        }
        steps += 1;
        let (value, derivative) = evaluate(step, trial, trial_gradient);
        if !armijo(step, value) || (lo > 0.0 && value >= lo_cost) {
            (hi, hi_cost, hi_slope) = (step, value, derivative);
            break;
        }
        if derivative.abs() <= -C2 * slope {
            return Some(value);
        }
        if derivative >= 0.0 {
            (hi, hi_cost, hi_slope) = (lo, lo_cost, lo_slope);
            (lo, lo_cost, lo_slope) = (step, value, derivative);
            break;
        }
        (lo, lo_cost, lo_slope) = (step, value, derivative);
        step *= 2.0;
    }

    // Zoom between lo (sufficient decrease, lowest cost so far) and hi.
    loop {
        if steps >= max_steps {
            return best_point(lo, &mut evaluate, trial, trial_gradient);
        }
        let (a, b) = if lo < hi { (lo, hi) } else { (hi, lo) };
        if b - a <= f64::EPSILON * b.abs().max(1.0) {
            return best_point(lo, &mut evaluate, trial, trial_gradient);
        }
        let margin = 0.1 * (b - a);
        let step = cubic_minimizer(lo, lo_cost, lo_slope, hi, hi_cost, hi_slope)
            .clamp(a + margin, b - margin);
        steps += 1;
        let (value, derivative) = evaluate(step, trial, trial_gradient);
        if !armijo(step, value) || value >= lo_cost {
            (hi, hi_cost, hi_slope) = (step, value, derivative);
            continue;
        }
        if derivative.abs() <= -C2 * slope {
            return Some(value);
        }
        if derivative * (hi - lo) >= 0.0 {
            (hi, hi_cost, hi_slope) = (lo, lo_cost, lo_slope);
        }
        (lo, lo_cost, lo_slope) = (step, value, derivative);
    }
}

/// The best sufficient-decrease step found (`lo`), re-evaluated into
/// `trial`; `None` when no step lowered the cost.
fn best_point(
    lo: f64,
    evaluate: &mut impl FnMut(f64, &mut [f64], &mut [f64]) -> (f64, f64),
    trial: &mut [f64],
    trial_gradient: &mut [f64],
) -> Option<f64> {
    if lo > 0.0 {
        let (value, _) = evaluate(lo, trial, trial_gradient);
        Some(value)
    } else {
        None
    }
}

/// Minimiser of the cubic through (a, fa, da) and (b, fb, db); the midpoint
/// when the cubic has none.
fn cubic_minimizer(a: f64, fa: f64, da: f64, b: f64, fb: f64, db: f64) -> f64 {
    if !fb.is_finite() || !db.is_finite() {
        return 0.5 * (a + b);
    }
    let d1 = da + db - 3.0 * (fa - fb) / (a - b);
    let radicand = d1 * d1 - da * db;
    if radicand < 0.0 {
        return 0.5 * (a + b);
    }
    let d2 = (b - a).signum() * radicand.sqrt();
    let value = b - (b - a) * (db + d2 - d1) / (db - da + 2.0 * d2);
    if value.is_finite() {
        value
    } else {
        0.5 * (a + b)
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}

fn axpy(scale: f64, x: &[f64], y: &mut [f64]) {
    for (y, x) in y.iter_mut().zip(x) {
        *y += scale * x;
    }
}

fn max_abs(values: &[f64]) -> f64 {
    values
        .iter()
        .fold(0.0f64, |acc, value| acc.max(value.abs()))
}

/// Solve every ray of a sweep in parallel.
pub(crate) fn maesaka_rays(
    rows: &[Vec<f64>],
    bounds: &[MaesakaBounds],
    dr_m: f64,
    config: &MaesakaKdp,
) -> Vec<MaesakaRay> {
    rows.par_iter()
        .zip(bounds.par_iter())
        .map(|(row, bound)| maesaka_ray(row, *bound, dr_m, config))
        .collect()
}
