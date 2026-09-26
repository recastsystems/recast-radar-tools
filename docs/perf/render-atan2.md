# Render: the per-pixel azimuth

`recast-radar-render` computes one azimuth per drawn pixel (`azimuth_from_xy`
in `crates/recast-radar-render/src/lib.rs`) and picks the ray from it. Three
versions of that angle have existed:

| version | angle | same bits on every platform |
|---|---|---|
| `main` (d69f902) | `f32::atan2`: the C library's `atan2f` | no: the MSVC CRT and glibc differ in the last bit, which moved display-upsampled KLIX pixels across azimuth bins on Linux and failed the CI job `test (stable)` (run 35298333232) |
| `usability` d497a4b to 444875a | the C library's f64 `atan2`, rounded to f32 | almost: the f32 roundings differ only when the f64 value lies within the libraries' error of an f32 rounding boundary |
| `usability` now | `trig::atan2` (`src/trig.rs`): this crate's `atan2` in plain f64 arithmetic, rounded to f32 | yes: only IEEE 754 `+ - * /`, which Rust evaluates the same way on every target |

The review of 444875a measured the f64 C-library `atan2` at 12-17% of
single-core raster time on KTLX 2024 and 8-11% on KILX, with nothing in the
documentation saying so. This page is the measurement of all three and the
reason the third is kept.

## What `trig::atan2` computes

For f32 arguments it returns the f64 angle within 2 ulp of the exact value.
The method (a three-way argument reduction to `|u| <= 0.2061` in which every
product and sum is exact, one division, a degree-7 polynomial in `u^2` fitted
with mpmath, correctly rounded constants for each branch and quadrant) is
described in the module documentation. Accuracy, measured:

- against mpmath's `atan2` at 300 bits on 400,000 f32 pairs (pixel-scale
  offsets, every binade, ratios within 1e-5 of the three branch points):
  largest error 1.82 ulp of the f64 result;
- against the MSVC CRT's f64 `atan2` on 106,290,565 pairs (the bench's
  6,681,600 viewport offsets, 50 million random bit patterns, 50 million
  pixel-scale offsets; NaN results excluded): at most 2 ulp apart (48,973
  pairs at 2 ulp), and the f32 roundings agree on every pair;
- zeros, infinities and NaN: the C99 Annex F values, bit for bit.

The unit tests in `src/trig.rs` pin 36 mpmath reference values (within
2 ulp; the branch points and their f32 neighbours among them), the Annex F
table, and one million random pairs within 3 ulp of the platform's f64
`atan2`.

## Output

`recast-radar-bench --iters 1` pixel checksums of the three baseline volumes
(KTLX 2024, KILX, KTLX 2013). Windows builds use the release profile; Linux
builds (nexbench container, Ubuntu 24.04, glibc 2.39, and
x86_64-unknown-linux-musl) use thin LTO, which does not change pixels.

| build | Windows (MSVC CRT) | Linux glibc | Linux musl |
|---|---|---|---|
| `main` d69f902 (`atan2f`) | 0xc04a5e2dfecc4c1f, 0xd5080047ae5dfeb5, 0x19e3735f42cdca4b | 0x873d9370fecc4c1f, 0x4a51bbfdae5dfeb5, 0x62e7ca24c87c43ce | same as glibc |
| 444875a (f64 `atan2`) | baseline | baseline | baseline |
| `trig::atan2` | baseline | baseline | baseline |

"baseline" is the Windows row of `main`, the values in
`docs/baselines/import-checksums.txt`. `main`'s Linux values are also what
wasm32 printed (`docs/design/wasm.md`): glibc's, musl's and Rust's `libm`
`atan2f` agree with each other and differ from MSVC's in the last bit on
pixels at azimuth bin edges. `real_render_parity`, whose KLIX fingerprints
failed CI run 35298333232 on Linux, passes with `trig::atan2` on Windows,
glibc and musl, as do the `trig` unit tests.

## Cost of the angle alone

A microbenchmark sums the azimuth of each of the bench's 6,681,600 viewport
offsets (1280x720, 1920x1080 and 2560x1440 at 0.25 km/px, rotated 0.02 rad):
fat LTO, one process pinned to one logical CPU at High priority, 15 rounds per
function, three processes.

| function | ns per call (min over rounds, three processes) |
|---|---:|
| `f32::atan2` (MSVC CRT `atan2f`) | 3.69-3.72 |
| `f64::atan2` rounded to f32 (MSVC CRT) | 5.70-5.75 |
| `trig::atan2` rounded to f32 | 3.47-3.51 |
| two-way reduction, degree-10 polynomial (an earlier form) | 3.89-3.92 |

The earlier form reduced only at `tan(pi/8)` and needed a degree-10
polynomial for the same accuracy; written with one division per branch it
compiled to both divisions and a select, at 4.23 ns. The three-way reduction
with an exact middle point (13/32) keeps one division and every product
exact and shortens the polynomial to degree 7.

## Cost in the raster stages

`recast-radar-bench` (decode + three reflectivity and three dealiased
velocity viewport rasters per iteration), release profile (fat LTO), rustc
1.94.0, Windows 11, AMD Ryzen 9 9950X3D. Four builds: `main` d69f902,
444875a (f64 C-library `atan2`), 444875a with that one line put back to
`f32::atan2` (the review's control), and `trig::atan2`. Rounds interleave the
four builds; each process is pinned to logical CPU 14 at High priority with
`RAYON_NUM_THREADS=1` and runs `--iters 8` after its warmup. Per run the
statistic is a stage's minimum over the 8 iterations; the table gives the
median of that over the rounds and, for the ratios, the median and range
over the rounds of the ratio of two builds in the same round. The host is
shared with other agents' builds. These rounds ran when it was quieter (34%
of all CPUs busy at the start, logical CPUs 14 and 15 idle), and a round
varies by a few percent; runs at 60-70% load gave the same order with rounds
varying by up to a third.

Median of per-run minimum, ms (8 rounds on KTLX 2024, 6 on KILX):

| file | stage | `main` d69f902 | 444875a (f64 `atan2`) | 444875a with `atan2f` | `trig::atan2` |
|---|---|---:|---:|---:|---:|
| KTLX20240315_000217_V06 | reflectivity raster x3 | 153.4 | 174.7 | 154.6 | **140.2** |
| KTLX20240315_000217_V06 | velocity raster x3 | 152.0 | 170.6 | 153.5 | **139.7** |
| KILX20260418_013553_V06 | reflectivity raster x3 | 148.4 | 167.2 | 150.8 | **134.1** |
| KILX20260418_013553_V06 | velocity raster x3 | 171.3 | 187.3 | 172.5 | **157.0** |

Ratios within a round, median (range):

| file | stage | 444875a / `main` | `trig::atan2` / `main` | `trig::atan2` / `atan2f` control |
|---|---|---|---|---|
| KTLX 2024 | reflectivity | 1.133 (1.112-1.148) | **0.913** (0.888-0.949) | 0.901 (0.889-0.928) |
| KTLX 2024 | velocity | 1.122 (1.101-1.143) | **0.915** (0.893-0.984) | 0.907 (0.894-0.966) |
| KILX | reflectivity | 1.133 (1.078-1.141) | **0.896** (0.869-0.946) | 0.899 (0.879-0.911) |
| KILX | velocity | 1.113 (1.065-1.128) | **0.916** (0.889-0.990) | 0.929 (0.887-0.949) |

The decode stage, which none of these versions change, is the control: its
ratios to `main` are 1.00-1.01 for every build. The f64 C-library `atan2`
cost 11-13% of raster time, as the review found; `trig::atan2` makes the
raster stages 8-10% faster than `main` and than the `atan2f` control. In the
raster loop the inlined arithmetic gains more than its 6% in isolation,
presumably because the loop no longer calls out per pixel. All four builds
print the same pixel checksums in every run.

## Decision

`trig::atan2` stays. It removes the platform dependence of the per-pixel
angle by construction rather than by making it unlikely, it is more accurate
than `atan2f` (within 2 ulp of the f64 angle), and it is faster than both
C-library functions: 6% less time a call than `atan2f` and 39% less than the
f64 `atan2` in isolation, 8-10% less raster time than `main`.

Alternatives not taken:

- A port of musl's `atan2f`: also deterministic, but an f32 algorithm with
  up to about 1 ulp of f32 error, so its roundings differ from the correctly
  rounded angle on many more pixels than `trig::atan2`'s, and pixels near bin
  boundaries would move relative to the pinned images.
- The `perf-p1` branch's `exact::azimuth_bin` computes the bin from an f64
  series and falls back to the plain angle within 0.02 bins of a rounding
  boundary. It is complementary: when the branches merge, its fallback should
  call `azimuth_from_xy` (and so `trig::atan2`), which keeps the fallback
  pixels platform-independent.

The remaining C-library calls in drawing paths are the f64 `sin`/`cos` of the
viewport rotation and storm motion in `recast-radar-render` (once per frame
or per ray, not measurable in these stages) and the f64 `hypot`/`atan2` of
the cross-section and box-grid cells in `recast-radar-map`. Both are rounded
to f32, which makes a platform difference unlikely, not impossible.
