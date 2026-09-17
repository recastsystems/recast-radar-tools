# Single-core Level II decode: the ported library measured

Branch `perf` at 0eec9b9 (on 261aaf8, which adds `recast-radar-bzip2`) against
`main` at 2b72af7. The branch replaces the `bzip2` crate (libbz2-rs-sys) with
`recast-radar-bzip2` on every Level II bzip2 path, recycles decoded LDM block
buffers through a bounded pool, spawns no decompression worker on a one-thread
rayon pool, inflates in-memory `.gz` volumes in one zlib-rs pass, and trims a
few message 31 parse allocations. `main` 2b72af7 differs from the branch base
(c690263) only in parse-path details (message framing, radar identifier), so
it is the "before" everywhere below. Every number here was produced on one
host on 2026-09-17; the study that chose the decoder is summarised in the
`recast-radar-bzip2` README.

## Method

Host: AMD Ryzen 9 9950X3D (16 cores, 32 threads), Windows 11. The Linux
numbers come from a WSL2/Docker container on the same machine (Ubuntu 24.04,
kernel 6.6.87, glibc 2.39, valgrind 3.22.0). rustc 1.94.0 on both sides.

Corpus (real volumes, `recast-radar-testdata` ids): `KTLX20240315_000217_V06`
(10.8 MB, 97 LDM bzip2 records, 20 cuts / 11,520 radials),
`KILX20260418_013553_V06` (23.3 MB, 106 records, 23 cuts / 12,600 radials) and
`KTLX20130520_201643_V06.gz` (9.5 MB gzip of a 44.9 MB uncompressed volume,
17 cuts / 8,280 radials).

Decode harness (Linux): a 150-line binary that calls
`recast_radar_io_nexrad::decode_volume_from_bytes` (`read_volume_from_bytes`
since the FM301 migration) on the file bytes read
before any timing, built with `lto = "fat"`, `codegen-units = 1`, release,
`debug = true`. Modes `time` (1 warmup + N timed decodes, the volume dropped
after each), `once` (one decode, for callgrind) and `verify` (cut/radial
counts and FNV-64 hashes of every moment grid and of the volume's Debug
output). Unless stated otherwise the global rayon pool is built with
`ThreadPoolBuilder::num_threads(1).use_current_thread()`, so the calling thread
is the only rayon worker and the whole decode runs on it. The same harness
source was used for the round 1 baselines (`bowecho-rust-bz2`: the BowEcho
`nexrad_io` crate with libbz2-rs-sys; `nexrad-crate`: danielway/nexrad
1591b64 `nexrad-data`, which is the decoder behind `radrs`), so those binaries
are compared directly.

1. Instructions: `valgrind --tool=callgrind --toggle-collect='*decode_once*'`,
   `RAYON_NUM_THREADS=1`, inline pool. Ir is deterministic and was collected
   with the runs in parallel.
2. Pinned wall clock: strictly serial, one process at a time, `taskset` to the
   least busy physical core over a 10 s window at start (cpu26, sibling cpu27
   idle: at most 13% busy during any run), `RAYON_NUM_THREADS=1`, inline
   pool, 1 warmup + 15 timed decodes per process. Three rounds; within a
   round the file order and the candidate order rotate (reversed on round 2).
   Before each round the script polls `/proc/loadavg` every 30 s and starts
   only under a 1-minute load of 6 (it never had to wait: load 1.1-3.7
   throughout, no other benchmark running). "MoRM" is the median of the three
   round medians; "ratio" is the median over rounds of the ratio of round
   medians (same round, same file), so drift between rounds cancels.
3. Multi-core: the default global rayon pool (`HARNESS_POOL=default`), not
   pinned, with the default thread count (32) and with
   `RAYON_NUM_THREADS=4`; 1 warmup + 15 decodes, three interleaved rounds.
4. Page faults and RSS: `/usr/bin/time -f "%R %M ..."` around `once` and
   around the 10-decode `time` loop, with default glibc and with
   `GLIBC_TUNABLES=glibc.malloc.trim_threshold=4294967296:glibc.malloc.mmap_threshold=33554432`.
5. Windows: `recast-radar-bench` (decode + 3 reflectivity rasters + 3
   dealiased velocity rasters per iteration, `--release`, `--iters 10` after
   1 warmup) built from `main` 2b72af7 and from the branch. Three interleaved
   rounds with the build order alternating, one process at a time, with
   `RAYON_NUM_THREADS=1` and with the default thread count (the bench uses the
   default global pool, as the app does). A further pinned pass sets the
   process affinity to one logical CPU 5 ms after launch, inside the untimed
   warmup. The bench reports mean/min per stage, not medians; the table gives
   the median of the three round means and the overall min.

Pool caveat that matters for reading the Windows `RAYON_NUM_THREADS=1` rows:
on `main` a one-thread default pool still spawns one decompression worker and
the parsing thread steals decompression work, so that configuration decodes on
two OS threads. On the branch a one-thread pool spawns nothing, so the same
setting is a true single-thread decode. The pinned rows are the like-for-like
single-core comparison.

## Correctness

Both builds give the round 1 hashes on all three files at 1, 4 and 16 threads
on both pool kinds (36 runs): moment_hash 9681efba4c14cafc / e9c583be742f5e55
/ 5486ac518666055e, debug_hash bb0ac718e4bd847c / 8c91ba3f80c84fb8 /
b78c871fe2911c07. The Windows bench's pixel checksums are
0xc04a5e2dfecc4c1f / 0xd5080047ae5dfeb5 / 0x19e3735f42cdca4b for every run of
both builds, equal to `docs/baselines/import-checksums.txt`, and deterministic
across iterations.

## Instructions (callgrind Ir, decode only, one thread)

| file | r1 bowecho-rust-bz2 | r1 nexrad-crate | main 2b72af7 | **perf 0eec9b9** | vs main | vs nexrad-crate |
|---|---:|---:|---:|---:|---:|---:|
| KTLX20240315_000217_V06 | 5,791,098,444 | 5,893,419,231 | 5,795,776,037 | **1,207,071,978** | -79.17% | -79.52% |
| KILX20260418_013553_V06 | 10,912,264,831 | 11,111,886,466 | 10,917,266,383 | **2,336,729,969** | -78.60% | -78.97% |
| KTLX20130520_201643_V06.gz | 258,543,976 (libdeflate, C) | 578,977,637 | 515,455,827 | **474,213,997** | -8.00% | -18.09% |

The standalone `safe_bz2_b` prototype from the decoder study measured
1,209,802,538 / 2,346,612,247 on the two bzip2 files, so the port costs
nothing over it (-0.23% / -0.42%). Cross-checks on KTLX20240315: with the
default pool and `RAYON_NUM_THREADS=1` the toggled count is 1,207,082,085, so
the whole decode stays on the calling thread; the full program without the
toggle is 1,207,443,406 (371 K Ir outside the decode).

## Pinned single-core wall clock (Linux, 15 decodes per process, 3 rounds)

| file | candidate | round medians ms | MoRM ms | min ms | ratio vs nexrad-crate | vs bowecho-rust-bz2 | vs main |
|---|---|---|---:|---:|---|---|---|
| KTLX20240315 | **perf 0eec9b9** | 275.4 / 291.8 / 280.7 | **280.7** | **271.6** | **0.478** (0.459-0.480, 3/3) | 0.455 | 0.460 |
| KTLX20240315 | main 2b72af7 | 598.2 / 669.9 / 594.0 | 598.2 | 578.6 | 1.011 | 0.964 | 1 |
| KTLX20240315 | r1 nexrad-crate | 599.7 / 607.7 / 587.2 | 599.7 | 567.7 | 1 | 0.958 | 0.989 |
| KTLX20240315 | r1 bowecho-rust-bz2 | 625.8 / 625.3 / 616.4 | 625.3 | 596.2 | 1.044 | 1 | 1.038 |
| KILX20260418 | **perf 0eec9b9** | 662.7 / 663.7 / 626.5 | **662.7** | **597.4** | **0.553** (0.540-0.573, 3/3) | 0.524 | 0.527 |
| KILX20260418 | r1 nexrad-crate | 1157.1 / 1199.8 / 1160.0 | 1160.0 | 1120.5 | 1 | 0.970 | 0.960 |
| KILX20260418 | r1 bowecho-rust-bz2 | 1182.2 / 1291.3 / 1195.5 | 1195.5 | 1141.8 | 1.031 | 1 | 1.005 |
| KILX20260418 | main 2b72af7 | 1205.0 / 1267.0 / 1189.5 | 1205.0 | 1130.1 | 1.041 | 0.995 | 1 |
| KTLX20130520.gz | r1 bowecho-rust-bz2 (libdeflate, C) | 52.5 / 54.9 / 52.5 | 52.5 | 49.7 | 0.608 | 1 | 0.850 |
| KTLX20130520.gz | **perf 0eec9b9** | 60.5 / 62.2 / 57.4 | **60.5** | **56.1** | **0.697** (0.655-0.702, 3/3) | 1.133 | 0.929 |
| KTLX20130520.gz | main 2b72af7 | 65.5 / 64.5 / 61.7 | 64.5 | 57.6 | 0.723 | 1.176 | 1 |
| KTLX20130520.gz | r1 nexrad-crate | 86.3 / 89.2 / 87.6 | 87.6 | 84.1 | 1 | 1.644 | 1.384 |

The study's target was at most 0.75x the nexrad crate on KTLX20240315; the
port is at 0.478x, faster in every round on every file. The round spread of
the branch's medians is 5.6-8.0%, of `main` 5.9-12.7%. The `.gz` volume has
no bzip2 data; its gain is the one-shot inflate, and the C libdeflate build of
round 1 stays 13% ahead of the pure-Rust zlib-rs path.

## Multi-core (Linux, default rayon pool, not pinned)

| file | threads | main round medians ms | main MoRM | main min | perf round medians ms | perf MoRM | perf min | perf/main (per round, median) |
|---|---|---|---:|---:|---|---:|---:|---|
| KTLX20240315 | 32 (default) | 74.6 / 78.6 / 75.8 | 75.8 | 67.5 | 46.9 / 48.5 / 44.8 | 46.9 | 41.5 | 0.628 / 0.618 / 0.592 (0.618) |
| KTLX20240315 | 4 | 173.7 / 172.3 / 163.2 | 172.3 | 158.0 | 87.8 / 87.9 / 83.0 | 87.8 | 79.5 | 0.506 / 0.510 / 0.509 (0.509) |
| KILX20260418 | 32 (default) | 104.7 / 106.3 / 105.2 | 105.2 | 94.1 | 73.2 / 71.9 / 75.0 | 73.2 | 64.6 | 0.699 / 0.677 / 0.713 (0.699) |
| KILX20260418 | 4 | 313.6 / 316.8 / 310.9 | 313.6 | 300.7 | 168.9 / 172.9 / 166.0 | 168.9 | 155.8 | 0.539 / 0.546 / 0.534 (0.539) |
| KTLX20130520.gz | 32 (default) | 62.4 / 62.0 / 63.7 | 62.4 | 58.0 | 55.6 / 56.9 / 58.1 | 56.9 | 54.0 | 0.892 / 0.917 / 0.913 (0.913) |
| KTLX20130520.gz | 4 | 61.0 / 61.2 / 60.8 | 61.0 | 57.5 | 57.5 / 57.8 / 56.1 | 57.5 | 53.8 | 0.942 / 0.944 / 0.922 (0.942) |

No regression at any thread count: the branch is faster in all 18 pairs. At 32
threads the bzip2 files are bounded by the serial parse and the longest
records, so the gain is smaller than on one core (0.62-0.70x); at 4 threads it
is 0.51-0.54x. The gzip path is serial and gains the same 6-9% as pinned.

## Page faults and resident memory (Linux, `/usr/bin/time`)

| file | measurement | main 2b72af7 | perf 0eec9b9 |
|---|---|---:|---:|
| KTLX20240315 | once, 1 thread: minor faults / max RSS | 44,977 / 181.5 MB | 23,799 / 97.0 MB |
| KTLX20240315 | 10-decode loop, 1 thread: minor faults | 233,080 | 200,622 |
| KTLX20240315 | 10-decode loop with trim/mmap tunables | 53,386 | 23,794 |
| KTLX20240315 | once, default pool, 32 threads: faults / max RSS | 71,230 / 268.2 MB | 58,897 / 213.2 MB |
| KILX20260418 | once, 1 thread | 49,900 / 201.2 MB | 27,820 / 113.2 MB |
| KILX20260418 | 10-decode loop, 1 thread | 139,746 | 210,937 |
| KILX20260418 | 10-decode loop with tunables | 60,463 | 27,811 |
| KILX20260418 | once, default pool, 32 threads | 85,158 / 312.9 MB | 76,682 / 284.2 MB |
| KTLX20130520.gz | once, 1 thread | 23,897 / 97.3 MB | 23,863 / 97.3 MB |
| KTLX20130520.gz | 10-decode loop, 1 thread | 186,775 | 186,738 |

A single decode of a bzip2 volume faults about half as many pages and peaks
at about half the RSS: the decoded blocks no longer stay alive until the end
of the parse, and the decoder's workspace replaces a zeroed 3.6 MB `tt`
allocation per record. Over a loop of decodes the branch takes no faults at
all once glibc keeps freed memory (23,794 for 10 decodes equals one decode),
while `main` still faults about 3 K per decode. With default glibc settings
the loop counts are allocator policy on caller-owned output: each decode's
moment storage (about 77 MB; `MomentGrid` before the FM301 migration) is
freed by the harness, trimmed by glibc
and re-faulted by the next decode. `main` re-faults less on KILX in that
configuration because its per-record 3.6 MB callocs raise glibc's dynamic
mmap threshold, so its freed memory is not returned; the application keeps
decoded volumes in a cache and does not see this loop.

## Windows `recast-radar-bench` (release, decode + 6 rasters per iteration)

Unpinned, as requested (`RAYON_NUM_THREADS=1` rows on `main` use two OS
threads, see the method note):

| file | threads | build | decode mean ms per round | decode MoRM | decode min | total MoRM | total min | decode ratio perf/main (median) | total ratio |
|---|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20240315 | 1 | main | 340.7 / 355.1 / 341.2 | 341.2 | 299.8 | 719.6 | 597.8 | | |
| KTLX20240315 | 1 | perf | 266.1 / 261.4 / 266.7 | 266.1 | 237.1 | 639.5 | 535.2 | 0.781 | 0.883 |
| KTLX20240315 | default | main | 56.4 / 56.3 / 76.6 | 56.4 | 49.7 | 89.5 | 81.5 | | |
| KTLX20240315 | default | perf | 33.1 / 29.3 / 44.6 | 33.1 | 27.3 | 66.3 | 58.3 | 0.582 | 0.705 |
| KILX20260418 | 1 | main | 679.9 / 684.5 / 686.3 | 684.5 | 589.5 | 1109.8 | 889.5 | | |
| KILX20260418 | 1 | perf | 606.6 / 616.1 / 599.3 | 606.6 | 541.3 | 1009.9 | 847.6 | 0.892 | 0.913 |
| KILX20260418 | default | main | 87.8 / 88.5 / 89.5 | 88.5 | 76.7 | 137.5 | 123.0 | | |
| KILX20260418 | default | perf | 54.8 / 54.0 / 53.1 | 54.0 | 46.3 | 102.0 | 92.3 | 0.609 | 0.742 |
| KTLX20130520.gz | 1 | main | 53.6 / 52.8 / 52.9 | 52.9 | 50.0 | 379.0 | 336.7 | | |
| KTLX20130520.gz | 1 | perf | 42.3 / 40.9 / 40.9 | 40.9 | 39.4 | 358.1 | 329.5 | 0.775 | 0.959 |
| KTLX20130520.gz | default | main | 52.8 / 53.3 / 53.7 | 53.3 | 50.2 | 83.5 | 79.0 | | |
| KTLX20130520.gz | default | perf | 41.6 / 40.7 / 44.7 | 41.6 | 38.9 | 72.5 | 66.8 | 0.787 | 0.871 |

Pinned to one logical CPU (`RAYON_NUM_THREADS=1`, affinity mask 0x100000):

| file | build | decode mean ms per round | decode MoRM | decode min | total MoRM | total min | decode ratio perf/main | total ratio |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20240315 | main | 688.7 / 642.8 / 611.8 | 642.8 | 598.4 | 948.1 | 893.5 | | |
| KTLX20240315 | **perf** | 239.4 / 244.4 / 242.9 | **242.9** | **232.5** | 543.1 | 532.7 | **0.380** (0.348-0.397) | 0.582 |
| KILX20260418 | main | 1213.0 / 1337.0 / 1283.7 | 1283.7 | 1168.4 | 1593.1 | 1474.8 | | |
| KILX20260418 | **perf** | 541.4 / 542.3 / 537.8 | **541.4** | **526.6** | 854.1 | 831.7 | **0.419** (0.406-0.446) | 0.530 |
| KTLX20130520.gz | main | 51.7 / 50.6 / 51.0 | 51.0 | 48.6 | 340.5 | 333.5 | | |
| KTLX20130520.gz | **perf** | 39.9 / 40.3 / 40.2 | **40.2** | **38.6** | 332.4 | 325.1 | **0.787** (0.771-0.796) | 0.976 |

The pinned Windows decode agrees with the pinned Linux harness (`main` 643 vs
598 ms, branch 243 vs 281 ms on KTLX). The branch's single-thread decode is
faster pinned than free-running (243 vs 266 ms), which is consistent with the
scheduler moving the thread between the two core complexes of this CPU. The
raster stages are unchanged by the branch, so the per-iteration total moves
less than the decode stage.

## Reproduction

Container: `/build/perf/r3-port/{cg.sh,wall.sh,multi.sh,faults.sh}` with the
harness crates `harness-port` (branch) and `harness-port-main` (main), logs in
`logs/` and callgrind outputs in `cg/`; mirrored on the host under the perf
scratchpad as `r3-port/`. Windows: `bench-win.sh` (unpinned) and
`bench-win-pinned.ps1`, summarised by `bench-win-summ.py`, binaries copied to
`bin-win/`. The Level II decoder test suite and the bench checksum test are the
gate for any future change to these paths.
