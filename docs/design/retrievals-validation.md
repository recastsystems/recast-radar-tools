# Retrievals checked against Py-ART: KDP, Z-PHI, gridding, rotation

This note records how the wave 3 algorithm items were validated and which
defaults were chosen. Every comparison runs on real Level II files; the
expected values come from Py-ART 2.3.0 and numpy through the golden scripts
named below, never from this workspace's output.

## KDP estimators

`KdpConfig::method` ([`KdpMethod`](../../crates/recast-radar-retrieve/src/kdp.rs))
selects the estimator. All three share the phase front end of
`derive_phase_products`: RHOHV (0.80) and reflectivity (-10 dBZ) gating,
360 degree unwrapping across gaps of at most 2 gates, linear fill of those
gaps and a 7-gate Hampel filter. They report KDP only at gates that passed
the gating (unless `emit_interpolated_gates`).

The method sets KDP, `PHIF`, whether `KDP_UNC` exists, and the products
computed from KDP (the KDP and hybrid rain rates, KDP texture). It does not
touch the attenuation products: PHIDP-linear and Z-PHI alike always take the
windowed-regression phase and KDP (or a source KDP), and with another method
the pass runs the regression as well when an attenuation product is
requested. Before this, attenuation used the method's `PHIF`, and the method
moved it a long way. On the Moore cut, against the regression: PHIDP-linear
PIA by up to 19.1 dB (Vulpiani) and 14.6 dB (Maesaka, whose forward phase
left PIA defined at 28,707 gates instead of 78,039), and Z-PHI PIA by up to
10 and 10.6 dB. These figures come from the stream review, not from a rerun
here. `attenuation_does_not_depend_on_the_kdp_method` in
`attenuation_real.rs` now checks all six attenuation products bit for bit
against the regression's on the three cuts below, for both attenuation
methods and both other KDP methods, and checks Z-PHI against the Py-ART
golden with each method. It fails on the old wiring.

| Method | What it computes | Other products |
|---|---|---|
| `WindowedRegression` (default) | Huber-weighted linear fit over 3 km; KDP = half the slope, inside the band bounds | `PHIF` = fit intercept, `KDP_UNC` = slope standard error |
| `Vulpiani(VulpianiKdp)` | Vulpiani et al. (2012) as Py-ART `kdp_vulpiani`: finite-difference first guess over 10 gates, band bounds, a 5 deg/km texture test, 10 integrate-and-differentiate passes | `PHIF` = first filtered phase + reconstructed phase; no `KDP_UNC` |
| `Maesaka(MaesakaKdp)` | Maesaka et al. (2012) as Py-ART `kdp_maesaka` formulates it: KDP = k^2 / (2 dr), k minimising forward and reverse phase misfits plus a radial smoothness penalty (Py-ART's cost, gradient, boundary conditions with n = 20 and the outlier check, first guess 0.01), minimised per ray with L-BFGS and a strong-Wolfe line search | `PHIF` = forward propagation phase; no `KDP_UNC` |

### Agreement with Py-ART

`tools/retrieve_golden.py kdp_methods` runs the same front end in numpy on
MetPy-read moments of three S-band split cuts (Moore 2013-05-20 hail core,
480 radials; Hurricane Ida 2021-08-29 rain bands, 120; the 2020-08-10 Iowa
derecho, 120), hands the filtered phase to Py-ART, and samples 1,500 gates per
method and case (`crates/recast-radar-retrieve/tests/kdp_methods_real.rs`):

- Vulpiani matches `pyart.retrieve.kdp_vulpiani(band="S", windsize=10,
  n_iter=10)` to at most 2.4e-7 deg/km at every sampled gate, with the same
  gate count. The profile runs to the end of the sweep's range axis, as a
  Py-ART ray runs to the radar's last gate; stopping at the end of the phase
  field instead changed KDP within about 50 gates of the field's end (ten
  iterations of a 10-gate window) by up to 2.5e-4 deg/km.
- Maesaka is compared with Py-ART's own cost function minimised ray by ray
  with scipy L-BFGS-B to convergence (pgtol 1e-9): sampled differences have a
  median of at most 1.2e-11, a 99th percentile of at most 2.4e-4 and a maximum
  of 0.038 deg/km (gates where k passes near zero and the cost is flat). Gate
  counts match, so the boundary conditions match Py-ART's.
- Py-ART's `kdp_maesaka` with its default 50 conjugate-gradient iterations
  over all rays at once is not converged: against the converged minimum its
  95th-percentile difference is 1.7 (Moore), 0.09 (Ida) and 1.2 (derecho)
  deg/km, and its maximum 200 deg/km. The crate therefore compares with the
  converged problem, not with Py-ART's default output.

So only Vulpiani is compared with the output of a public, unmodified Py-ART
function. "Maesaka as Py-ART" means the minimum of Py-ART's cost function
(`_cost_maesaka`, `_jac_maesaka`), not what `pyart.retrieve.kdp_maesaka`
returns with its defaults; against that output the crate differs as the
converged minimum does (95th percentile 0.09-1.7 deg/km, maximum 200 deg/km).

### Which estimator is the default

Measured on the same three cuts, over gates where all three report and the
echo is meteorological (Z >= 20 dBZ, RHOHV >= 0.9):

| | Moore | Ida | Derecho |
|---|---|---|---|
| Phase closure, 2 sum(KDP) dr / measured phase change, median (p10-p90); regression | 1.15 (0.85-1.67) | no ray with >= 20 deg | 1.03 (0.94-1.19) |
| Vulpiani | 1.27 (0.90-1.78) | | 1.33 (1.09-1.62) |
| Maesaka | 0.78 (0.45-1.03) | | 0.90 (0.71-0.98) |
| Mean KDP in light echo (20-30 dBZ, true KDP a few hundredths of a deg/km); regression | 0.28 | 0.037 | 0.053 |
| Vulpiani | 0.48 | 0.149 | 0.142 |
| Maesaka | 0.085 | 0.013 | 0.036 |
| Standard deviation there; regression | 1.27 | 0.76 | 0.69 |
| Vulpiani | 0.47 | 0.22 | 0.21 |
| Maesaka | 0.84 | 0.18 | 0.27 |
| 99.9th percentile in echo; regression | 10.7 | 4.5 | 4.9 |
| Vulpiani | 3.8 | 1.2 | 3.7 |
| Maesaka | 21.4 | 3.2 | 10.1 |

Time for one sweep (release build, 8 threads, loaded machine): regression
24 ms, Vulpiani 20 ms, Maesaka 658 ms on the 480-radial Moore cut; 42, 31
and 1,602 ms on a 720-radial KEWX cut. One thread: 101, 59 and 9,678 ms.
Py-ART took 1.1 s (Vulpiani) and 21 s (Maesaka, unconverged) on Moore.

Maesaka's work per ray is capped by `MaesakaKdp::max_cost_evaluations`
(default 10,000 cost evaluations, line-search trials included). Before, only
the 4,000-iteration limit applied, and each iteration could spend 40
line-search evaluations, so a ray could take 160,000 evaluations. On the
three cuts the most any ray used was 4,674 (mean per ray: 701 on Moore, 539 on
Ida and 3,276 on the derecho), on rays that stopped at the iteration limit,
so the cap does not bind there and the output is unchanged. One evaluation
is linear in the gate count. On the three 1,192-gate cuts the whole
single-threaded test run took about 26 s for 794,000 evaluations, about
33 us each including the L-BFGS update. At that rate a 720-ray sweep of
1,192-gate rays in which every ray exhausts the budget would take about
4 minutes on one core, or about 30 s on 8 threads. Longer rays cost
proportionally more. `maesaka_evaluation_budget_stops_the_solver_early` checks that the
budget binds: with 50 evaluations per ray, 34,489 of the derecho's 117,241
gates move by more than 0.1 deg/km.

**Decision: the windowed regression stays the default.** It closes the phase
budget best (median 1.03-1.15 against 1.27-1.33 for Vulpiani and 0.78-0.90
for Maesaka), its light-echo bias is between the other two, it is the only
method with an uncertainty product, and keeping it leaves every existing
product and golden unchanged. Its cost is noise: the gate-level standard
deviation in light echo is 3-6 times Vulpiani's. Vulpiani is offered for
Py-ART-compatible, smooth KDP (it matches Py-ART to 1e-7 deg/km) with a
positive bias of 0.14-0.48 deg/km in weak echo, from zeroing first-guess
values at or below -2 deg/km while keeping positive noise. Maesaka is
offered for non-negative KDP in rain below the melting layer: it is the least
biased in weak echo but concentrates the phase rise into spikes, and costs
about 30 times more. Like Py-ART's, it applies no band bounds, so the spikes
reach callers: the largest KDP it reports is 333 deg/km on the Moore cut, 86
on Ida and 47 on the derecho (`kdp_methods_real.rs` checks these against the
reference within 1 %; over the echo gates of the table the Moore maximum is
120 deg/km, in the hail core where its monotone-phase assumption does not
hold). The KDP and hybrid rain rates and KDP texture computed with Maesaka
receive those values; the attenuation products do not (see above). The owner may prefer
Vulpiani as the default for Py-ART parity; that is a one-line change in
`KdpConfig::for_band` and moves the KDP-derived goldens. Whether to clip
Maesaka to the band bounds (which would part from Py-ART) is also the
owner's call.

## Z-PHI attenuation correction

`AttenuationConfig::method` selects `PhiLinear` (default, unchanged) or
`ZPhi(ZPhiAttenuation)`, a port of Py-ART's `calculate_attenuation_zphi`
(Testud et al. 2000, the base-10 form of Gu et al. 2011) with Py-ART's band
coefficients, a fixed freezing level (default 4 km above the radar antenna)
and 15 excluded end gates. Py-ART's default `fzl = 4000` m is an altitude
(its `fzl_index` adds the radar altitude), so the two defaults agree only for
a radar at sea level; the goldens pass Py-ART the radar altitude plus
4,000 m. The phase is the windowed regression's filtered phase less its
near-range median (the phase excess the PHIDP-linear method already uses),
whatever `KdpConfig::method` is, made monotone as Py-ART does. Output: `AH`, `PIA`, the corrected reflectivity, `ADP`, `PIDA` and
the corrected ZDR.

`tools/retrieve_golden.py attenuation` runs Py-ART on the same three cuts with
that phase excess (`crates/recast-radar-retrieve/tests/attenuation_real.rs`).
Without reflectivity smoothing, compared with unmodified Py-ART, every
sampled gate, the path-integrated maxima and the sums agree within 2e-3 dB
and 2e-3 relative. The default 5-gate smoothing is compared with a patched
Py-ART, not the shipped one: Py-ART's
`smooth_masked` builds its window with numpy's `as_strided`, which drops the
mask, so its mean also averages the underlying data of masked gates (-33 dBZ
for NEXRAD code 0, the fill value for other readers). Against a mask-aware
mean that moves Py-ART's specific attenuation by a median of 0.6 % (Moore) and
1 % (Ida), and by 70-88 % at the 95th percentile, at gates next to missing
reflectivity where -33 dBZ enters the window. The crate averages the
unmasked gates only, as the function's documentation describes, and the golden
patches Py-ART's `smooth_masked` the same way. The crate matches that patched
Py-ART. Against shipped Py-ART (the same phase excess, full grids, recorded
under `pyart_shipped_smoothing_vs_mask_aware` in `attenuation.json`), the
crate's default Z-PHI therefore differs as follows. Each value is the absolute
per-gate difference between the shipped and the mask-aware result:

| | Moore | Ida | Derecho |
|---|---|---|---|
| PIA and corrected Z, largest per-gate difference (dB) | 2.57 | 0.92 | 0.10 |
| PIA, 99th percentile of the per-gate difference (dB) | 0.68 | 0.072 | 0.0079 |
| AH, 99th percentile of the difference relative to the mask-aware value | 0.96 | 0.92 | 0.69 |
| AH, the same relative to the shipped value | 23 | 12 | 2.2 |

Both runs define the same gates. The corrected reflectivity is Z + PIA in
both, so its difference is the PIA difference. The AH ratios are large
where shipped Py-ART averaged -33 dBZ into the window and produced a small
AH.

PHIDP-linear stays the default: Z-PHI needs a freezing level the crate does not know, and at S band the
correction is small (Z-PHI PIA at most 10.6 dB in the Moore hail core, 7.7-7.8
dB elsewhere, mostly from noisy phase).

## Cartesian gridding

`recast_radar_map::grid_from_volumes` ports Py-ART's `grid_from_radars`
(`map_gates_to_grid`): Barnes2, Barnes, Cressman or nearest weighting, the
constant, `dist` and `dist_beam` radii, one or more volumes, Py-ART's
azimuthal equidistant transforms, f32 arithmetic in Py-ART's accumulation
order. `crates/recast-radar-map/tests/grid_real.rs` compares eight cases with
Py-ART (`tools/filters_map_golden.py grid`), with float64 gate positions given
to Py-ART because Py-ART computes gate heights in f32:

- one radar: Barnes2 with `dist_beam` on the Moore split cut (reflectivity
  and ZDR) and on the 19-tilt KEWX volume, Cressman with a constant radius,
  and an explicit origin 20 km from the radar with `dist`;
- two radars 161 km apart (KPAH and KVWX, 2008-04-15 23:50-23:53Z), origin
  at KPAH: Barnes2 with `dist_beam` and Barnes with `dist`, where the radius
  is the minimum over both radar offsets;
- two KDVN volumes 7 minutes apart gridded together (26,157 points take gates
  from both);
- nearest weighting, with Py-ART given a gate filter that excludes masked
  reflectivity: `GridWeighting::Nearest` ignores gates without a value, where
  Py-ART otherwise lets a masked nearest gate blank the point.

Defined-point counts match exactly, sampled values within 1.1e-5 dBZ
(Cressman 7e-15, nearest exact) and sampled radii within 1e-6 relative.

Those goldens give Py-ART float64 gate positions, so they measure the port,
not its distance from stock Py-ART. The golden script also grids each case
with unmodified `grid_from_radars` (Py-ART's own gate positions, heights in
float32) and records the comparison under `stock_pyart_positions` in
`grid.json`. The float32 heights are up to about 1 m off at 230 km. That
moves which gates reach a point near the edge of a radius, and the weights
of gates near the edge:

| Case | Defined (float64 / stock) | Points defined in one only | Points differing by > 0.01 | Largest difference |
|---|---|---|---|---|
| Moore, Barnes2, DBZH | 9,704 / 9,702 | 6 | 403 | 2.1 dBZ |
| Moore, Barnes2, ZDR | 9,072 / 9,070 | 6 | 130 | 2.7 dB |
| Moore, Cressman, constant radius | 10,748 / 10,748 | 0 | 186 | 0.16 dBZ |
| Moore, origin 20 km away, `dist` | 3,347 / 3,345 | 2 | 178 | 5.0 dBZ |
| KEWX 19 tilts, Barnes2 | 71,678 / 71,676 | 8 | 1,002 | 3.8 dBZ |
| KPAH + KVWX, Barnes2 | 1,015 / 1,013 | 2 | 5 | 0.037 dBZ |
| KPAH + KVWX, Barnes, `dist` | 831 / 829 | 2 | 2 | 1.4 dBZ |
| KDVN two volumes, Barnes2 | 30,537 / 30,534 | 5 | 439 | 3.7 dBZ |
| Moore, nearest | 4,621 / 4,619 | 2 | 4 | 9.5 dBZ |

The crate keeps the float64 positions, so against stock Py-ART it differs by
those amounts. Differences over 0.01 affect 0.1-5.3 % of the defined points;
the most is 5.3 %, on Moore with the `dist` radius.

Rays are turned into gate positions in parallel. Each z level is filled in
bands of y rows, so a grid with fewer levels than threads still uses them
all. Every task walks the gates in Py-ART's order, so the output is bit for
bit what the sequential mapper gives: all eight cases hash identically
before and after the change. Three alternating rounds, best of 5 each, on a
loaded 8-thread run: the 19-tilt KEWX volume onto 16 x 101 x 101 points went
from 194-325 ms to 115-141 ms, the two KDVN volumes onto 6 x 81 x 81 from
1,166-1,412 ms to 256-298 ms, and the Moore Barnes2 case from 21-32 ms to
10-15 ms. On one thread the times were about the same (KEWX 787 -> 608 ms,
KDVN 1,675 -> 1,837 ms). Py-ART took about 3 s and 24 s on the KEWX and KDVN
cases, as measured in the stream review. The field values are written
straight into the output (there is no per-level copy).
`MAX_GRID_CELLS` (2^28) now counts the radius field too, so the output at the
limit is 1 GiB.

Py-ART reads only the gridded fields (`include_fields`). Read with the 250 m
Doppler moments, `read_nexrad_archive` interpolates legacy 1 km reflectivity
onto 250 m gates and Py-ART grids four gates per recorded one: on KPAH and
KVWX that gave 1,456 defined points where the crate, gridding the recorded
gates, gives 1,015. The module documentation states this.

## Rotation detection: the Moore 2013-05-20 20:16Z miss

`detect_rotation_sites` found no site within 10 km of the Moore EF5 at 21 km.
Every velocity tilt carried a strong 2D feature at the couplet (rank 10-22,
gate-to-gate dV 101 m/s at 0.5 deg) and vertical association built the
column, but its 3D rank was 0: the rank core must be 3 km deep with its base
below 5 km (Stumpf et al. 1998), and the detector used only the lowest 8
velocity tilts up to 10 deg, whose top (5.1 deg) samples 1.9 km above the
radar at 21 km. The detector now considers up to 16 velocity tilts up to
20 deg; the Moore site is reported at az 268.8 deg, 22.1 km (TVS, rank 15,
14 tilts), 2.1 km from the SPC path position at the ray time.

The extra tilts cost detection time on VCP 12/212 volumes. Best of 9 runs in
6 alternating rounds on a loaded machine: Moore 117 -> 142 ms (+22 %),
Rolling Fork 216 -> 243 ms (+13 %); the median of the round medians moved
+29 % and +21 %. On one thread the best runs moved 422 -> 532 ms and 644 ->
759 ms (+26 %, +18 %). An independent run of the same comparison measured
+10 % and +11 % by the minimum. A later review ran 3 rounds of 9 alternating
base and head runs and measured Moore +11-19 %, Rolling Fork (KDGX) +10-36 %
(noisy) and Stanton 2014 (KOAX) about 0 %, all by the minimum. The 30-40 %
first recorded here was the upper end of that noise, and the stream's summary
repeated it. The owner should decide from the figures in this section.

Two effects the backlog blamed are real but not the cause. The region
dealiaser unfolds the northern half of the debris-region couplet on the
0.5 deg tilt by the wrong fold (-62 to -84 m/s where Py-ART's
`dealias_region_based`, and the crate's literal port of it, give +20 to
+61 m/s); the site is therefore placed on that misbranched patch, about 1 km
north of the couplet centre Py-ART's velocities show. And LLSD shear in the
core exceeds the 150 m/s/km plausibility cap (145-230 m/s/km on the 0.5 and
0.9 deg tilts even after Py-ART dealiasing); the feature survives on the gates
around the core, so the cap was left as it is. Both are open.

With the detector's sweeps dealiased by the Py-ART port instead
(`detect_rotation_sites_from_dealiased`), the Moore site moves to az 266.3
deg, 22.4 km (TVS, rank 12, 14 tilts), 1.8 km from the SPC position and
about 1 km south of the default engine's site, where the analysis above puts
the couplet centre of Py-ART's velocities. Rolling Fork keeps its position
(rank 12 -> 10); the Stanton, NE 2014 site moves from 2.9 to 3.8 km from its
SPC position (Vrot 43 -> 35 m/s, 5 -> 4 tilts). Whether to switch is part of
the dealiaser decision below.

## Dealiasing: the Py-ART port and the default engine

`dealias_velocity` is the crate's vote-graph region engine;
`dealias_velocity_pyart_region` ports Py-ART's `dealias_region_based`. The
port now equals Py-ART 2.3.0 at every gate of all 15 golden sweeps with a
Nyquist velocity (`dealias_pyart::tests::port_matches_pyart_at_every_gate`,
goldens from `tools/correct_golden.py`, two of them the velocity sweeps the
bench renders). Its edge tracker was O(E^2) (a full scan for the strongest
edge on every merge); a heap, a pair map and lazily applied edge turns make
it O(E log E) with identical output on all 434 velocity sweeps of the 32
cached Level II volumes, and cut their total time from 26.9 s to 8.1 s.

The default engine, measured against the same goldens, after the best
global fold offset (per connected echo in brackets): 313 of 189,629 gates
differ on KBOX (278), 57,608 of 337,846 on the KDVN derecho (55,780),
41,586 of 84,964 on its trimmed sector (22,431), 202,203 of 438,781 on KILX
2026-04-18 (111,677), 233 of 218,886 on KTLX 2024-03-15 (155), 104-632 on
the Moore sweeps, 10-1,775 on the Ida sweeps, 110 on Katrina and 322 on
PAHG. On the Moore couplet it keeps 4 of the 18 gates Py-ART unfolds to
outbound; the port keeps all 18.
Over the 434 sweeps the default engine took 6.3-7.1 s and the port 7.3-8.9 s
(two runs; worst sweep 78-97 ms against 151-320 ms).

**Owner decision: which engine `dealias_velocity` runs.** The port matches
Py-ART at every tested gate and fixes the Moore couplet; the vote-graph
engine is the one the viewer and the bench have used, is somewhat faster,
and supports a wind reference. Switching changes every dealiased-velocity
render: an earlier trial of the switch moved the three bench checksums to
0x9ee54544d6fe03d1, 0xfbedba82c6e9b723 and 0xa7c428b188e201f6 (measured in
review, not re-run here) and needs the render fingerprints re-recorded. The
switch was not made.

## Render: sample cache against the direct render

The viewport sample cache resolved each pixel to the first radial whose code
was not blank, while the direct render draws the first radial whose value the
colour table shows. Under the default reflectivity table, which hides low dBZ,
the two differed wherever overlapping radials disagreed in visibility. Sample
caches now resolve with the building cache's colour table and remember it; a
field render through a cache built under another table is refused. The bench
renders through the direct path, so its checksums are unchanged.
