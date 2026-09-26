# Cross-library decode benchmark and the first performance pass (perf-p1)

Branch `perf-p1` (base `main` d69f902). This page has three parts: a reproducible
benchmark of every format recast-radar-tools reads against the other libraries that read it
(what is measured, how, and every number), the optimisations of this pass with their
before/after measurements, and the hotspots found in the crates this pass was not allowed to
change, for the pass after the other streams merge.

Every number below was produced on one host on 2026-09-24/25. That host was shared with other
agents' builds, tests and fuzzers the whole time (Windows CPU at 53-100%, median 98%, in the
second fix pass's 10 s log; about 17 of 32 logical CPUs busy with other builds; `nexbench` load
average 10-15 from fuzz targets and builds). `nexbench` is also a container with a CPU quota of
12 CPUs (cgroup `cpu.max` 1200000/100000), and it was throttled whenever the work inside it
used more: its `cpu.stat` counted 3,608 throttled 100 ms periods (1,805 s) at the start of the
second fix pass and 7,049 (3,021 s) at its end, and a throttled period stalls every process in
the container, pinned or not. So **wall-clock times are 1.7-3.7x the ones of the quiet-host
study in `docs/perf/single-core.md`** (KTLX 2024 pinned: 1,036 ms on Linux and 423 ms on
Windows here, 281 and 243 ms there), and a ratio between two libraries is only as good as its
spread over rounds: two libraries whose round medians overlap are not ranked by this page
(every table's *apart* column says which pairs separate, and the Summary cites wall-clock
ratios only for pairs that do). Instruction counts (callgrind, independent of load) and peak
RSS at one thread are the reliable numbers; peak RSS with a thread pool depends on load too
(section "Level II peak RSS against the nexrad crate"). `docs/perf/single-core.md` has the
quiet-host absolute Level II numbers.

## Summary

**Which recast row a ratio divides by.** Every wall-clock ratio below is a library's median of
round medians over that of the `recast` row: recast's decode into packed `u8`/`u16` fields in
the source's encoding, like-for-like with the nexrad crate, RSL, go-nexrad, LROSE, h5py,
wradlib's ODIM reader and MetPy's Level III reader, which return stored values too. Py-ART,
xradar, radrs, MetPy's Level II reader, and netCDF4 and wradlib on CfRadial return float arrays
(scale and offset applied); for them the like-for-like row is `recast-f32` (the same decode plus
float32 physical values of every field, the Method table's "work Py-ART and xradar do"), which
took 1.23-1.9x `recast`'s time on Level II, 1.21-1.48x on ODIM_H5 and 1.5-2.1x on CfRadial 1
(Windows, pinned, the files where the two rows' rounds lie apart; the Linux rounds of the two
rows overlap on most files). Ratios over `recast` overstate those readers' gap by that factor,
so each of their figures below is followed by its ratio over `recast-f32` in square brackets,
again counting only pairs whose round medians lie apart (`tools/xlib-bench/run.py ratios` prints
both ratios for every case; the commands are in `docs/perf/data/perf-p1/README.md`).

- **Level II, one core.** recast-radar-tools executes the fewest instructions of the decoders
  that decode the whole volume, on every Level II input (callgrind, one decode, one thread;
  independent of load): the nexrad crate executes 4.7-4.9x recast's instructions on the LDM
  bzip2 volumes and 1.36x on the KTLX 2013 gzip volume, RSL 1.6-3.0x except on KILX 2026
  (0.96x, where it keeps 16 of the 23 sweeps and 45 of the 67 million gates), LROSE 14-23x. In
  wall clock, pinned, median of the round medians relative to recast, counting only libraries
  whose round medians lie apart from recast's on that file (files where a library returned no
  sweep or one radial left out). Windows: the nexrad crate 1.43x on KILX 2026, 2.49x on KTLX
  2024 and 2.66x on the KTLX 2013 gzip volume; radrs 1.8-3.5x [2.1-2.9x], MetPy 2.2-21x
  [2.7-11x], Py-ART 2.6-34x [3.2-28x], xradar 6.4-15x [7.7-12x]. Linux: the nexrad crate 2.88x
  on the KTLX 2013 gzip volume; radrs 2.0x on KTLX 2024 [1.7x]; RSL 4.5-7.3x (the two gzip
  volumes); go-nexrad 2.1-6.0x; MetPy 3.5-29x [3.0-23x]; Py-ART 2.0-29x [2.5-32x]; LROSE
  3.1-6.3x; xradar 5.0-7.2x [6.1-6.5x]. On the two large LDM volumes the Linux
  rounds do not separate recast from the nexrad crate (median ratios 1.55 on KTLX 2024 and 1.08
  on KILX 2026), from RSL (2.38 and 1.85), or on KILX 2026 from radrs (1.20) and MetPy (2.09),
  so the page does not rank those pairs in wall clock; the instruction counts above are the
  evidence there (the nexrad crate 4.87x and 4.74x, RSL 1.57x and 0.96x).
- **Level II, default thread pools.** With every library's default threading, recast's round
  medians lie apart from and below radrs's (2.4-2.9x on Linux, 3.5-3.8x on Windows [2.3-2.6x on
  Linux; 1.9x on Windows, where the KILX 2026 rounds overlap]), go-nexrad's, RSL's, LROSE's
  and the Python readers' (17-64x on Windows [9.9-42x]; on the whole-file bzip2 volume, which
  recast decodes on one thread, MetPy and Py-ART take 3.3x [2.9x]) on every file they decode,
  and below the nexrad crate's on
  the KTLX 2013 gzip volume (2.17x on Linux, 2.05x on Windows). On the LDM bzip2 volumes (KTLX
  2024, KILX 2026) the default-pool rounds of recast and the nexrad crate overlap on both
  hosts (median ratios 1.21-1.54 on Windows; 0.74-1.20 on Linux, where both 12-thread pools
  shared the container's CPU quota with fuzz targets and builds, and recast's KILX 2026 round
  medians range from 135 to 443 ms), so the page does not rank the two there.
- **Level II files other readers cannot decode.** KLIX 2005 (Message 1): the nexrad crate
  returns no sweeps, MetPy and xradar fail. xradar and radrs fail on the gzip volumes and LROSE does not open them.
  RSL keeps one sweep per elevation, so it decodes 11 of KTLX 2024's 20 sweeps. A whole-file
  bzip2 volume (KTLX 2013 recompressed; fix pass): only recast, Py-ART and MetPy decode it.
- **Level III.** Py-ART takes 2.2-8.9x recast's time (2.5-6.1x on Windows) [2.5-9.2x; 3.0-4.5x
  on Windows], MetPy 2.8-5.7x (2.3-3.4x; MetPy's Level III reader returns the stored codes, so
  `recast` is its like-for-like row), rounds apart on every product except DPR on Linux, where
  Py-ART's overlap recast's; MetPy returns no radials for the DPR product; LROSE does not open
  Level III products. On a raster, a digital precipitation array, two graphic and one tabular
  product (fourth fix pass, section "Cases added in the fourth fix pass") MetPy takes 26-101x
  recast's time on Linux and 13-23x on Windows; Py-ART reads none of them.
- **ODIM_H5.** h5py reading every dataset without interpreting it (the container floor) takes
  2.8-4.3x recast's time (2.0-3.5x), wradlib 7.0-9.4x (3.5-5.5x; `read_opera_hdf5` returns the
  stored arrays), Py-ART 15-21x (8.2-19x) [14-18x; 6.2-16x on Windows], LROSE 16-21x, xradar
  38-87x (25-32x) [34-86x; 17-24x on Windows]. On a `SCAN` object (DWD, one sweep per file;
  fourth fix pass) h5py 4.5x, LROSE 5.8x, wradlib 8.3x, Py-ART 11.8x and xradar 28x on Linux
  (Windows: h5py 2.5x, wradlib 3.7x, Py-ART 6.4x, xradar 13.5x).
- **ODIM_H5 Cartesian** (fix pass). recast decodes a 500 x 500 IMGW `MAX` product in 0.2-0.4
  ms; h5py reading every dataset takes 10-12x that and wradlib 26-37x on Linux
  (3.5-5.3x and 7.0-9.8x on Windows); nothing else in the reference set reads the files.
- **CfRadial 1 (classic netCDF).** netCDF4 reading every variable takes 4.0x recast's time on
  S-Pol (2.1x on Windows) and 22.5-23.6x on the two small files [6.8-13x on the two small files;
  on S-Pol its rounds overlap `recast-f32`'s]; Py-ART 8-74x [5.3-33x], LROSE 12-32x, xradar
  14-129x [9.0-58x], wradlib 15-102x [9.7-58x] (Linux; on Windows [3.3-12x], [4.5-20x] and
  [6.5-18x]).
- **CfRadial 2.** This branch has no reader yet (G4). netCDF4 reads the S-Pol CfRadial 2 file
  in 127 ms pinned and xradar in 3.2 s (one decode per process, a single sample: xradar
  crashes when it opens the file a second time in the same process, on Linux and Windows).
  Neither Py-ART nor LROSE reads it (third fix pass): Py-ART's `read_cfradial` expects the
  CfRadial 1 layout (`KeyError: 'time'`; Py-ART's CfRadial 2 route is its xradar wrapper
  `pyart.xradar.Xradar`, which is xradar's decode), and LROSE Radx (2025-08 build) hands the
  file to its Leosphere CfRadial 2 reader, which rejects the sweep variable `prt_mode` for
  having a dimension (`LeoCf2RadxFile::_readSweepMeta`: "Bad dimCount, should be 0, found:
  1"), in `lrose_bench` and in `RadxPrint` alike.
- **DORADE.** LROSE takes 16-23x recast's time. RSL crashes (SIGSEGV) on both NOXP sweep files
  and returns no sweep from the DOW6 file. On the airborne N42RF sweep (fourth fix pass) LROSE
  takes 24x recast's time and RSL returns no sweep.
- **JMA GRIB2.** No other library in the reference set reads it: ecCodes 2.49.0 stops at
  JMA's local grid template ("Unable to find template gridDefinitionSection from
  grib2/local/rjtd/template.3.50120.def"). recast decodes one station (26 sweeps, 7.5 million
  gates) in 5.6 ms and the 20-station tar (520 sweeps, 150.5 million gates) in 159 ms, pinned
  on Linux.
- **Level II real-time chunks.** On KIWA volume 307 as a real-time client holds it (the Start
  chunk and 2 or 34 intermediate chunks: 240 and 4,080 radials) recast's pinned rounds lie apart
  from and below every other reader's, Windows (Linux): the nexrad crate 1.63x and 1.93x (2.30x
  on the 35-chunk file; on the three-chunk file the Linux rounds overlap, median ratio 2.25),
  radrs 2.6x (2.4-3.0x) [2.0-2.6x], MetPy 2.9-3.7x (2.4-2.8x) [2.8-3.0x], Py-ART 2.4-4.0x
  (2.6-2.9x) [2.5-3.1x], xradar 2.8-8.0x (2.7-5.9x) [2.9-6.1x], RSL, go-nexrad and LROSE
  2.4-4.3x on Linux (the bracketed figures, over `recast-f32`, are Windows'; on Linux only
  Py-ART's and xradar's rounds on the 35-chunk file lie apart from `recast-f32`'s, 3.0x and
  5.9x). xradar returns no sweep from the three-chunk file and drops the unfinished cut of the
  other, LROSE does not open the three-chunk file.
- **Router.** `recast_radar_io::read_supported_volume_bytes` costs nothing measurable over the
  format reader for AR2V, real-time chunks, ODIM, CfRadial 1, DORADE and JMA input (pinned
  0.95-1.05x on Windows), but a gzip Level II volume through it takes 1.37x the time pinned
  (Windows, rounds apart) and peaks at 96.7 against 54.9 MiB in one decode (Windows), because
  the router inflates the whole file to sniff it; and it does not dispatch Level III on this
  branch.
- **Peak RSS, one decode on one thread** (Linux figures, GNU time; see the correction under
  Modes). Level II: recast peaks lowest of the libraries that decode the whole volume on KTLX
  2024 (95.2 MiB against the nexrad crate's 101.2), KTLX 2013 gzip (53.8 against 134.1) and
  KLIX 2005; on KILX 2026 RSL peaks at 110.2 against recast's 111.0 but keeps 16 of the 23
  sweeps. It is not the lowest on the partial real-time volumes, where the nexrad crate peaks at 9.1 and 40.5 MiB against recast's
  10.5 and 40.8 and RSL at 5.8 and 36.2 with fewer gates. (Earlier versions of this page put
  that down to recast reserving the unfinished cut's buffers for its full radial count. It is
  not: capacity that is never written is not resident, and shrinking every field to its length
  at the end of the decode, an experiment of the fourth fix pass, took the three-chunk volume's
  field capacity from 6.9 to 2.3 MB and left its one-thread peak where it was, 11.8-12.3 against
  11.9-12.4 MiB on Windows, `fix4-rss-shrink-win.jsonl.gz`.) With the default pool (12 threads
  in `nexbench`, 32 on Windows; ranges over five runs), the third fix pass took 42-50 MiB off
  recast's peaks on the large LDM volumes (a decoded record's buffer is now freed as the parser
  finishes it instead of staying in a 64-buffer pool; no time cost measurable): on Linux recast
  now peaks at the nexrad crate's level on KTLX 2024 (128.5-135.7 against 128.2-131.4 MiB),
  and above it on KILX 2026 (162.0-176.5 against
  145.8-150.6); on Windows it is still above on every LDM volume (KTLX 2024 154.1-169.3 against
  126.9-133.2, KILX 2026 222.4-232.6 against 155.7-167.0), because each of its 31 decoding
  threads keeps a bzip2 workspace while the volume completes. Two levers remain, and each costs
  time, so which one (if any) is an owner decision (section "Level II peak RSS against the
  nexrad crate"): capping the decoding threads (at 8 threads recast is at the nexrad crate's
  default-pool peak on KTLX 2024 on both hosts and still faster; on KILX 2026 it is 1.06-1.40x
  slower than with the full pool), or freeing each worker's workspace when it runs out of
  records (measured in the fourth fix pass, not merged: at or below the nexrad crate on KTLX
  2024 and the 35-chunk volume on both hosts, Windows KTLX 2024 118.3-129.6 against
  126.6-131.4 MiB, KILX 2026 still above, 189.8-201.3 against 151.3-165.8; but every later
  decode in the same process on the default pool takes about 1.2x the time, because it allocates
  and faults the workspaces again). For every other format recast's one-decode peak is the
  lowest of the libraries that decode the file (4.0-46.3 MiB outside JMA; the Python libraries'
  peaks include 40-230 MiB of interpreter and imports).
- **This pass's optimisations** (interleaved before/after, one pinned core, output identical):
  JMA decode 0.36-0.37x the time; decode plus float32 conversion of every field 0.45-0.72x; the
  xradar-default FM301 view materialized 0.77x; gzip Level II 0.81-0.86x with peak RSS 95.5 to
  53.8 MiB on KTLX 2013; uncompressed Level II 0.43x; DORADE 0.79-0.84x; the render raster stage
  0.82-0.84x; the metadata record of a whole-file bzip2 volume 0.55x the instructions; a
  whole-file bzip2 volume parsed while it is decoded block by block, peak RSS 98.3 to 59.0 MiB
  on KTLX 2013 for 4.1% more instructions; opt-in in-place ray ordering that makes 76 of KTLX
  2024's 104 fields zero-copy under the xradar default view (0 before); a decoded LDM record's
  buffer freed as the parser finishes it (third fix pass), 42-50 MiB off the default-pool peaks
  of the large LDM volumes. The three bench pixel checksums are unchanged.
- **paired-bzip2 stays off by default**: with the default pool it was slower in every run that
  measured it (1.20x over the 19 LDM volumes of the corpus, geometric mean, 1.23x in their
  fastest samples; 1.14-1.22x on three of four volumes in the fix-pass review). On one thread
  the runs disagree, because none of the wide ones was quiet (geometric means 0.980, 1.071 and,
  in the fourth fix pass's nine rounds in `nexbench`, 0.970; fastest samples 0.997, 0.982 and
  0.955; 12-14% faster on KILX 2026 in the two quietest runs, not in the fourth): paired
  decoding for one-thread decodes is an owner decision that needs a quiet-host run.

## Method

### Harnesses

Every harness decodes one file `warmup + iters` times in one process and prints one JSON line
(`tools/xlib-bench`). A timed sample is **path to decoded arrays in memory, file read
included**, for every library. Nothing is written to disk.

| library | version | harness | what one sample does |
|---|---|---|---|
| recast-radar-tools | this branch | `decode_bench --from-path` (`crates/recast-radar-bench/src/bin/decode_bench.rs`) | `std::fs::read` + the format's reader into the FM301 model: packed `u8`/`u16` fields, the source's encoding |
| recast-radar-tools, float | this branch | `decode_bench --from-path --physical` | the same plus `Field::to_physical` of every field (float32 physical values, NaN for sentinels): the work Py-ART and xradar do |
| recast-radar-tools, paired bzip2 | this branch, `--features paired-bzip2` | `decode_bench_paired` | the same decode with paired LDM record decoding |
| recast-radar-tools, router | this branch | `decode_bench --format auto` | the same bytes through `recast_radar_io::read_supported_volume_bytes` (sniff, unwrap a gzip or zip wrapper, dispatch); router cases only |
| danielway/nexrad | 1591b64 (`nexrad-data`, `parallel`) | `tools/xlib-bench/nexrad-crate` | `fs::read` + `File::new` + `decompress()` + `scan()` |
| go-nexrad | bwiggs/go-nexrad 78ea27b, Go 1.22.2 | `tools/xlib-bench/go-nexrad` | `os.ReadFile` + `archive2.Extract` |
| RSL | 1.50 | `tools/xlib-bench/rsl_bench.c` | `RSL_wsr88d_to_radar` / `RSL_dorade_to_radar` (RSL inflates gzip through an external `gzip` process, by design) |
| LROSE Radx | lrose-core 2025-08 build | `tools/xlib-bench/lrose_bench.cc` | `RadxFile::readFromPath` + `RadxVol::loadFieldsFromRays` |
| radrs | 0.4.0 (wraps the nexrad crate) | `py_bench.py --lib radrs` | `radrs.xradar.open_datatree(path).load()` |
| Py-ART | arm_pyart 2.3.0 | `py_bench.py --lib pyart` | `read_nexrad_archive`, `read_nexrad_level3`, `aux_io.read_odim_h5`, `read_cfradial` (eager: every field a masked float array; also tried on CfRadial 2) |
| MetPy | 1.7.1 | `py_bench.py --lib metpy` | `Level2File`, `Level3File` (eager) |
| xradar | 0.12.0 | `py_bench.py --lib xradar` | `open_*_datatree(path).load()` (the backends are lazy; `load()` reads every variable) |
| wradlib | 2.9.6 | `py_bench.py --lib wradlib` | `read_opera_hdf5`, `read_generic_netcdf` |
| h5py / netCDF4 | 3.16.0 / 1.7.4 | `py_bench.py --lib h5py/netcdf4` | every dataset / variable read in full: the container floor |

Python 3.12 (nexbench) and 3.13 (Windows) with NumPy 2.5.3 and xarray 2026.7.0; Rust 1.94.0
with the workspace release profile (`lto = "fat"`, `codegen-units = 1`). The recast binaries
of the cross-library timing rounds were built from b7ebb14: every optimisation of this pass
except e70bf43, which changes field capacity only (+0.06% instructions). The memory rows
(`rss`, `pinned-mem`, `multi-mem`), the real-time chunk cases and the router cases were run
afterwards with every harness rebuilt from 639dd22 (b7ebb14 plus e70bf43 and the harness
changes; ac71f48 changes only whole-file bzip2 input, which none of those cases uses). The
second fix pass added a whole-file bzip2 Level II case and two ODIM_H5 Cartesian cases (section
"Cases added in the second fix pass"), run with `decode_bench` built from the fix pass and the
other harnesses from 639dd22.

Readers looked for and not found: nothing in the reference set reads DORADE besides RSL and
LROSE (Py-ART, xradar and wradlib have no DORADE sweep-file reader), and nothing reads JMA
radar GRIB2 (ecCodes 2.49.0, installed into the nexbench environment for this, fails on the
local grid template 3.50120). LROSE Radx does not recognise Level III products or
gzip-wrapped Level II volumes.

### Modes

`tools/xlib-bench/run.py bench` runs every harness on every case in rounds (3 rounds; the
order of files reverses and the order of libraries rotates every round):

- **pinned**: one CPU (`taskset -c 26` in nexbench; on Windows `SetProcessAffinityMask` to one
  logical CPU plus `HIGH_PRIORITY_CLASS`, set after the process started and before it reads
  its go line; the Python harness sets both on itself, because a Windows virtual
  environment's `python.exe` is a launcher), one thread: `--threads 1` (an inline one-thread
  rayon pool), `RAYON_NUM_THREADS=1`, `GOMAXPROCS=1`, `OMP/OPENBLAS/MKL/NUMEXPR_NUM_THREADS=1`.
  Rust/C/Go: 1 warmup + 10 samples (200 for Level III); Python: 1 warmup + 3 samples for Level II
  and CfRadial with Py-ART, MetPy and xradar, 5 otherwise, 30 for Level III.
- **multi**: no affinity, every library's default threading (recast and the nexrad crate:
  rayon with one thread per CPU the process may use: 32 on Windows, and 12 in `nexbench`,
  because Rust's `available_parallelism`, which rayon's default pool uses, counts the
  container's 12-CPU quota; go-nexrad's Go 1.22 runtime ignores the quota and runs 32).
- **rss**: one decode per process (warmup 0, iters 1), pinned (`rss-1thread`), and for recast
  and the nexrad crate also with the default pool (`rss-default`). Peak RSS is the process's
  own: on Linux GNU time's `%M` (the harness's `ru_maxrss`; GNU time forks the harness from
  its own process, so the floor is GNU time's 1.0-1.25 MiB), on Windows
  `GetProcessMemoryInfo` `PeakWorkingSetSize`. Every Linux harness also prints its own
  `VmHWM` from `/proc/self/status` (`self_hwm_kb`) as a cross-check. It includes the
  interpreter and imports for Python (`rss_before_kb`, the resident set just before the first
  decode, is in the JSON: 40 MiB for h5py, 140-230 MiB for xradar, MetPy, wradlib and Py-ART).
  **Use these rows to compare memory**: in the pinned and multi rows the Rust harnesses keep
  the previous decode alive while the next one runs (the drop is outside the timed region),
  so their peaks hold two volumes, while the other harnesses free each decode first.
- **pinned-mem, multi-mem**: the pinned and multi runs once more, for the peak RSS column of
  the Linux pinned and multi tables only (`run.py table` takes that column from them).

**Linux peak RSS correction.** The first version of this page read Linux peak RSS with
`wait4` on the process the Python driver started. That is not the harness's own peak: at
`exec` the kernel records the old address space's high-water mark in the new process's
`maxrss`, and a `subprocess.Popen` child execs from the driver's address space, so every peak
below the driver's own size read as the driver's size (in nexbench `/bin/true` read 11,264
KiB that way, and 318,464 KiB after the driver touched 300 MB). The 14.2, 17.2 and 21.9 MiB
figures of the small files were the driver's. Every Linux peak on this page now comes from
GNU time: the `rss` rows were run again, and the pinned and multi tables take their peak
column from the `pinned-mem` and `multi-mem` rounds. GNU time and the harnesses' own `VmHWM`
agree on all 490 rows of that run that report both: identical on 400, `VmHWM` at most 2.3 MiB
lower on the others. The Windows figures, the timing columns and the `/usr/bin/time` tables
further down were not affected.

The Windows run lost its third round to process-start failures (`STATUS_DLL_INIT_FAILED`)
when the host was under memory pressure; that round was run again with `run.py bench
--first-round 2`, and `run.py table` keeps one result per round (the later successful one).

Instruction counts: `tools/xlib-bench/cg_compare.sh` (callgrind, whole process, one decode,
one thread) for the compiled Level II decoders, and `valgrind --tool=callgrind
--toggle-collect='*timed_work*'` around `decode_bench`'s timed region for this branch's
before/after numbers.

Before/after of this pass: `tools/xlib-bench/ab.py`, two builds (`c3be188`, the harness commit
on top of `main`, against this branch) run on the same files in alternating order, 5 rounds,
pinned to one logical CPU at high priority on Windows. The ratio is the median over rounds of
the per-round ratio, so load drift between rounds cancels.

### Host

AMD Ryzen 9 9950X3D (16 cores, 32 threads), 96 GB, Windows 11; `nexbench` is Ubuntu 24.04 in
Docker on WSL2 (kernel 6.6.87, glibc 2.39, 32 vCPUs, 62 GB, a CPU quota of 12 CPUs). WSL2 does
not expose hardware performance counters to the container (`perf stat -e cycles`: no
permission), so cycle counts were not available; callgrind instruction counts stand in.

### Corpus

Real files only (`run.py stage` copies them from the testdata cache, the committed fixtures and
`~/radar-corpus`). The DWD sweep and the N42RF sweep of
the fourth fix pass are pinned by SHA-256 (`PINNED_SHA256`) until their testdata entries
from other streams merge:

| case | format | source |
|---|---|---|
| KTLX20240315_000217_V06 | Level II, LDM bzip2, SAILS | `l2-ktlx-20240315-000217` |
| KILX20260418_013553_V06 | Level II, LDM bzip2, 23 cuts | `l2-kilx-20260418-013553` |
| KTLX20130520_201643_V06.gz | Level II, whole-file gzip (44.9 MB inside) | `l2-ktlx-20130520-201643` |
| KLIX20050829_130035.gz | Level II, gzip, Message 1 | `l2-klix-20050829-130035` |
| KIWA307_chunks001-003, KIWA307_chunks001-035 | Level II real-time chunks: KIWA volume 307's Start chunk and its first 2 and 34 intermediate chunks, concatenated in sequence order as a real-time client holds them (240 radials of the first cut; 5.5 cuts, 4,080 radials) | `l2chunk-kiwa-307-20260917-003629-001-s` .. `-035-i` (`run.py stage`, source `chunks:`) |
| TLX N0B / N0Q / N0U / DPR | Level III | committed `testdata/files/level3` |
| iesha, dkrom, bejab | ODIM_H5 PVOL | committed `testdata/files/other/odim` |
| S-Pol 2008-06-04 | CfRadial 1 | `cfrad1-spol-20080604-002217-sur` (netCDF-4) rewritten as classic netCDF by `run.py stage` (this branch reads classic netCDF only) |
| Irene CPOL sweeps 0-1, DOW8 RHI | CfRadial 1 classic | committed |
| S-Pol 2008-06-04 CfRadial 2 | CfRadial 2 (netCDF-4) | `cfrad2-spol-20080604-002217-sur`; this branch has no CfRadial 2 reader yet (G4) |
| NOXP PPI, NOXP sector, DOW6 RHI | DORADE sweep files | committed |
| JMA N5 2019-10-12 09:00 | JMA GRIB2 tar, 20 stations | `jma-n5-20191012-090000` |
| JMA N5 RS47773 | JMA GRIB2 tar, one station | committed |
| KTLX20130520_201643_V06.bz2 | Level II, whole-file bzip2 (fix pass) | `l2-ktlx-20130520-201643` inflated and recompressed at level 9 by `run.py stage` (Python's `bz2`, libbzip2; byte-identical to `bzip2 -9`, SHA-256 `8238ffed...cac4`): no published Level II file in the corpus is whole-file bzip2 |
| imgw.ram.KDP.max.h5, imgw.ram.RhoHV.max.h5 | ODIM_H5 Cartesian `MAX` products (fix pass) | committed `odim-imgw-ram-20260711-0015-kdp-max`, `-rhohv-max` (IMGW POLRAD Ramza, 500 x 500 cells) |
| TLX NCR / DPA | Level III raster (code 37) and digital precipitation array (code 81) (fourth fix pass) | committed `testdata/files/level3` (`l3-tlx-ncr-20260622-080623`, `l3-tlx-dpa-20260629-173638`) |
| TLX NST / NMD / NSS | Level III graphic (storm tracking 58, mesocyclone 141) and tabular (storm structure 62) products (fourth fix pass) | committed `testdata/files/level3` (`l3-tlx-nst-20260622-080623`, `l3-tlx-nmd-20260622-080623`, `l3-tlx-nss-20220503-005231`) |
| deboo.scan.20260924T2130.th00.hd5 | ODIM_H5 `SCAN`, DWD Boostedt sweep 00, TH (fourth fix pass) | `radar-corpus/feeds/dwd/boo/ras07-vol5minng01_sweeph5onem_th_00-2026092421305800-boo-10132-hd5` from the rolling opendata.dwd.de directory, SHA-256 `491252444c433f40940cc649201d5a9bd2b6bc0a385df79a137fa62195ffaea9` (committed by the hdf5-netcdf stream as `odim-deboo-20260924-2130-sweep-th-00`) |
| swp.N42RF-TM_20181010_123925_AIR | DORADE airborne sweep, NOAA P-3 N42RF tail radar (fourth fix pass) | testdata cache `dorade-n42rf-tm-20181010-123925-air` (GitHub Alex-DesRosiers/radarqc_scans at 0dc45a2; the metadata-complete stream's manifest entry), SHA-256 `7766bc200ab520660071157f23233927055bb4e67653e27dfe2d837872cf5748` |

## Results

### Reading the tables

One table per format and mode. *round medians* are the per-round medians of the samples, *MoRM*
their median, *min* the fastest sample of all rounds, */ recast* the MoRM over recast's MoRM,
*apart* whether the library's round medians lie apart from recast's (`yes`: all of them above,
or all below, recast's; `no`: the two ranges overlap, and the ratio does not rank the two;
empty for a single round), and *peak RSS* the largest of the rounds (on Linux the pinned and
multi rows take it from the `pinned-mem` and `multi-mem` round; see the rss note above for what
those rows hold). *decoded* is what the library returned: sweeps and gate values (the nexrad
crate harness counts radials). The counts differ because the libraries shape their output
differently: Py-ART pads every sweep to the volume's longest ray, xradar and radrs pad within a
sweep, LROSE gives every field on every ray the longest gate count, RSL keeps one sweep per
elevation, and h5py, netCDF4 and wradlib return datasets, not sweeps (0 sw). A row whose
*decoded* column shows much less than recast's (the nexrad crate on KLIX 2005, MetPy on DPR,
RSL on DOW6) did not decode the file and its time is
not a comparison. A *fails* row failed in every round; it shows the last line of the library's
error, or its exit code when it printed none (-11: SIGSEGV on Linux; 3221225477: an access
violation on Windows). The real-time chunk files are partial volumes: xradar returns no sweep
from the three-chunk file and drops the unfinished cut of the 35-chunk file (5 of 6 sweeps),
RSL returns fewer gates (one sweep per elevation, 3 of the 6 on the 35-chunk file), and LROSE
does not open the three-chunk file.

### Level II instructions (load-independent)

Callgrind Ir of one decode on one thread (`tools/xlib-bench/cg_compare.sh`, nexbench): recast
is `decode_bench`'s timed region (file read + decode), the others the whole process (startup
is under 5 M instructions for each). go-nexrad's runtime crashes valgrind 3.22, so it has no
row.

| file | recast | nexrad crate | / recast | RSL | / recast | LROSE | / recast |
|---|---:|---:|---:|---:|---:|---:|---:|
| KTLX20240315 (LDM) | 1,211,342,724 | 5,899,387,657 | 4.87 | 1,906,204,073 | 1.57 | 27,498,899,558 | 22.7 |
| KILX20260418 (LDM) | 2,345,584,234 | 11,121,701,948 | 4.74 | 2,251,971,921 | 0.96 | 32,632,689,875 | 13.9 |
| KTLX20130520 (gzip) | 433,356,787 | 587,813,997 | 1.36 | 1,278,774,373 | 2.95 | does not open .gz | |
| KLIX20050829 (gzip, Message 1) | 244,604,687 | 320,691,221 (no sweeps) | | 543,643,980 | 2.22 | does not open .gz | |

RSL does less work than the others on the SAILS/MRLE volumes: it keeps one sweep per
elevation (11 of KTLX 2024's 20 cuts, 32 million gates against 64.6 million; 16 of KILX
2026's 23), and its bzip2 is C libbzip2, instruction-efficient and latency-bound (its pinned
median of round medians is 2.4x recast's on KTLX 2024 for 1.57x the instructions, from rounds
that overlap recast's). The nexrad crate's
KLIX 2005 count is not a decode: it returns no sweep from the Message 1 volume.

### Linux (`nexbench`)

#### Level II, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20240315_000217_V06 | recast | 1036.0 / 1719.1 / 871.1 | 1036.0 | 670.8 | 172.1 | 1.00 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | recast-paired | 1230.5 / 1355.6 / 998.1 | 1230.5 | 855.0 | 176.6 | 1.19 | no | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | recast-f32 | 1246.2 / 1238.4 / 774.8 | 1238.4 | 601.2 | 172.1 | 1.20 | no | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | nexrad-crate | 1469.6 / 1610.7 / 1768.2 | 1610.7 | 1098.9 | 184.7 | 1.55 | no | 20 sw, 11,520 rays |
| KTLX20240315_000217_V06 | radrs | 2089.5 / 2534.3 / 1753.3 | 2089.5 | 1386.2 | 654.8 | 2.02 | yes | 20 sw, 73,517,869 gates |
| KTLX20240315_000217_V06 | rsl | 2468.2 / 1589.9 / 2551.3 | 2468.2 | 1430.7 | 105.3 | 2.38 | no | 11 sw, 32,091,840 gates |
| KTLX20240315_000217_V06 | go-nexrad | 3172.4 / 2509.0 / 3150.2 | 3150.2 | 2149.0 | 240.3 | 3.04 | yes | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | metpy | 4865.5 / 3577.8 / 3649.5 | 3649.5 | 3243.9 | 815.4 | 3.52 | yes | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | pyart | 4900.2 / 6051.2 / 4140.6 | 4900.2 | 3769.7 | 1159.2 | 4.73 | yes | 20 sw, 147,732,480 gates |
| KTLX20240315_000217_V06 | lrose | 7215.0 / 6498.5 / 5249.9 | 6498.5 | 4823.7 | 393.6 | 6.27 | yes | 14 sw, 84,257,280 gates |
| KTLX20240315_000217_V06 | xradar | 9781.7 / 7502.0 / 6910.2 | 7502.0 | 6821.7 | 1713.9 | 7.24 | yes | 20 sw, 73,517,865 gates |
| KILX20260418_013553_V06 | recast-paired | 2719.7 / 2152.7 / 2068.5 | 2152.7 | 1399.2 | 195.9 | 0.76 | no | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | recast-f32 | 3562.1 / 2191.1 / 1367.0 | 2191.1 | 1127.0 | 190.4 | 0.78 | no | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | recast | 2824.2 / 3693.9 / 1448.6 | 2824.2 | 1166.2 | 190.3 | 1.00 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | nexrad-crate | 3668.7 / 3040.1 / 2302.7 | 3040.1 | 2005.5 | 203.6 | 1.08 | no | 23 sw, 12,600 rays |
| KILX20260418_013553_V06 | radrs | 3399.7 / 3761.1 / 2195.5 | 3399.7 | 2065.6 | 765.8 | 1.20 | no | 23 sw, 75,464,764 gates |
| KILX20260418_013553_V06 | rsl | 5217.5 / 5360.7 / 2976.9 | 5217.5 | 1694.1 | 110.2 | 1.85 | no | 16 sw, 45,256,320 gates |
| KILX20260418_013553_V06 | pyart | 5518.4 / 6836.4 / 4242.4 | 5518.4 | 4063.0 | 1223.0 | 1.95 | yes | 23 sw, 161,582,400 gates |
| KILX20260418_013553_V06 | metpy | 6594.2 / 5893.6 / 3317.7 | 5893.6 | 3273.8 | 841.1 | 2.09 | no | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | go-nexrad | 5993.8 / 6759.9 / 4831.0 | 5993.8 | 3819.3 | 273.8 | 2.12 | yes | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | lrose | 8732.3 / 10061.2 / 5489.4 | 8732.3 | 4770.0 | 409.3 | 3.09 | yes | 17 sw, 86,284,800 gates |
| KILX20260418_013553_V06 | xradar | 16184.3 / 14213.1 / 8672.1 | 14213.1 | 8175.3 | 1798.1 | 5.03 | yes | 23 sw, 75,464,760 gates |
| KTLX20130520_201643_V06.gz | recast | 80.7 / 68.5 / 46.4 | 68.5 | 36.7 | 94.4 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | recast-f32 | 137.4 / 87.4 / 50.3 | 87.4 | 46.7 | 94.6 | 1.28 | no | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | nexrad-crate | 251.4 / 197.6 / 135.1 | 197.6 | 117.1 | 184.6 | 2.88 | yes | 17 sw, 8,280 rays |
| KTLX20130520_201643_V06.gz | go-nexrad | 473.8 / 412.6 / 244.5 | 412.6 | 214.7 | 205.1 | 6.02 | yes | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | rsl | 518.6 / 502.3 / 319.7 | 502.3 | 309.0 | 75.8 | 7.33 | yes | 14 sw, 34,050,240 gates |
| KTLX20130520_201643_V06.gz | pyart | 1469.4 / 1579.4 / 911.8 | 1469.4 | 693.8 | 783.8 | 21.45 | yes | 17 sw, 91,013,760 gates |
| KTLX20130520_201643_V06.gz | metpy | 2114.3 / 1983.6 / 730.5 | 1983.6 | 719.6 | 555.7 | 28.95 | yes | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | lrose | fails: File format not recognized: data/KTLX20130520_201643_V06.gz | | | | | | |
| KTLX20130520_201643_V06.gz | radrs | fails: ValueError: NEXRAD data error: gzip-compressed volume file must be decompressed before accessing records | | | | | | |
| KTLX20130520_201643_V06.gz | xradar | fails: TypeError: unsupported operand type(s) for +: 'NoneType' and 'int' | | | | | | |
| KLIX20050829_130035.gz | recast-f32 | 46.0 / 36.3 / 23.1 | 36.3 | 21.4 | 32.5 | 0.90 | no | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | recast | 40.5 / 46.5 / 22.2 | 40.5 | 19.9 | 32.3 | 1.00 |  | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | nexrad-crate | 75.5 / 56.9 / 42.2 | 56.9 | 38.8 | 41.6 | 1.41 | no | 0 sw, 0 rays |
| KLIX20050829_130035.gz | rsl | 246.1 / 183.6 / 110.1 | 183.6 | 107.2 | 26.5 | 4.54 | yes | 9 sw, 9,310,724 gates |
| KLIX20050829_130035.gz | pyart | 1645.4 / 697.6 | 1171.5 | 688.6 | 518.5 | 28.94 | yes | 20 sw, 40,031,040 gates |
| KLIX20050829_130035.gz | lrose | fails: File format not recognized: data/KLIX20050829_130035.gz | | | | | | |
| KLIX20050829_130035.gz | metpy | fails: IndexError: tuple index out of range | | | | | | |
| KLIX20050829_130035.gz | xradar | fails: TypeError: unsupported operand type(s) for +: 'NoneType' and 'int' | | | | | | |

#### Level II, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20240315_000217_V06 | recast | 207.1 / 218.1 / 170.3 | 207.1 | 147.8 | 414.4 | 1.00 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | recast-f32 | 234.2 / 434.8 / 228.2 | 234.2 | 138.6 | 416.1 | 1.13 | yes | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | nexrad-crate | 248.3 / 335.8 / 198.3 | 248.3 | 167.1 | 380.2 | 1.20 | no | 20 sw, 11,520 rays |
| KTLX20240315_000217_V06 | recast-paired | 263.7 / 290.7 / 161.1 | 263.7 | 132.8 | 497.8 | 1.27 | no | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | radrs | 733.3 / 607.1 / 441.3 | 607.1 | 430.3 | 869.5 | 2.93 | yes | 20 sw, 73,517,869 gates |
| KTLX20240315_000217_V06 | rsl | 2517.1 / 3088.1 / 1700.6 | 2517.1 | 1393.3 | 104.2 | 12.15 | yes | 11 sw, 32,091,840 gates |
| KTLX20240315_000217_V06 | go-nexrad | 3692.8 / 3675.8 / 2040.4 | 3675.8 | 1871.5 | 218.2 | 17.75 | yes | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | pyart | 4730.8 / 5467.0 / 3648.6 | 4730.8 | 3534.4 | 1139.9 | 22.84 | yes | 20 sw, 147,732,480 gates |
| KTLX20240315_000217_V06 | metpy | 5441.7 / 4863.7 / 3334.6 | 4863.7 | 3176.9 | 815.6 | 23.48 | yes | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | lrose | 7594.3 / 6701.1 / 4190.5 | 6701.1 | 3690.2 | 393.2 | 32.36 | yes | 14 sw, 84,257,280 gates |
| KTLX20240315_000217_V06 | xradar | 10428.7 / 7848.9 / 5753.2 | 7848.9 | 5619.7 | 1713.6 | 37.90 | yes | 20 sw, 73,517,865 gates |
| KILX20260418_013553_V06 | recast-paired | 403.9 / 125.5 / 184.1 | 184.1 | 93.3 | 612.7 | 0.55 | no | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | nexrad-crate | 419.0 / 244.8 / 212.8 | 244.8 | 117.3 | 405.2 | 0.74 | no | 23 sw, 12,600 rays |
| KILX20260418_013553_V06 | recast | 332.1 / 442.7 / 135.4 | 332.1 | 116.6 | 485.6 | 1.00 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | recast-f32 | 351.3 / 451.3 / 187.7 | 351.3 | 166.4 | 484.1 | 1.06 | no | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | radrs | 1105.2 / 804.8 / 454.3 | 804.8 | 426.1 | 963.1 | 2.42 | yes | 23 sw, 75,464,764 gates |
| KILX20260418_013553_V06 | rsl | 6686.2 / 4450.1 / 2704.5 | 4450.1 | 2514.9 | 110.5 | 13.40 | yes | 16 sw, 45,256,320 gates |
| KILX20260418_013553_V06 | go-nexrad | 6747.6 / 5416.5 / 2925.8 | 5416.5 | 2743.1 | 265.8 | 16.31 | yes | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | metpy | 5680.4 / 6844.9 / 3452.7 | 5680.4 | 3433.2 | 841.6 | 17.10 | yes | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | pyart | 8293.6 / 7236.0 / 5068.2 | 7236.0 | 4848.6 | 1223.8 | 21.79 | yes | 23 sw, 161,582,400 gates |
| KILX20260418_013553_V06 | lrose | 12867.2 / 8016.1 / 5155.2 | 8016.1 | 4799.4 | 409.6 | 24.14 | yes | 17 sw, 86,284,800 gates |
| KILX20260418_013553_V06 | xradar | 13903.6 / 13831.3 / 7935.4 | 13831.3 | 7878.3 | 1797.1 | 41.64 | yes | 23 sw, 75,464,760 gates |
| KTLX20130520_201643_V06.gz | recast-f32 | 200.6 / 75.0 / 49.2 | 75.0 | 46.9 | 94.6 | 0.95 | no | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | recast | 95.5 / 78.8 / 38.7 | 78.8 | 37.0 | 94.3 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | nexrad-crate | 192.3 / 170.6 / 111.0 | 170.6 | 97.0 | 184.9 | 2.17 | yes | 17 sw, 8,280 rays |
| KTLX20130520_201643_V06.gz | go-nexrad | 501.8 / 435.5 / 223.1 | 435.5 | 206.0 | 214.7 | 5.53 | yes | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | rsl | 505.8 / 463.9 / 204.8 | 463.9 | 200.9 | 75.5 | 5.89 | yes | 14 sw, 34,050,240 gates |
| KTLX20130520_201643_V06.gz | metpy | 1569.3 / 789.7 | 1179.5 | 774.7 | 555.7 | 14.97 | yes | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | pyart | 1418.6 / 1208.5 / 692.1 | 1208.5 | 655.1 | 799.1 | 15.34 | yes | 17 sw, 91,013,760 gates |
| KTLX20130520_201643_V06.gz | lrose | fails: File format not recognized: data/KTLX20130520_201643_V06.gz | | | | | | |
| KTLX20130520_201643_V06.gz | radrs | fails: ValueError: NEXRAD data error: gzip-compressed volume file must be decompressed before accessing records | | | | | | |
| KTLX20130520_201643_V06.gz | xradar | fails: TypeError: unsupported operand type(s) for +: 'NoneType' and 'int' | | | | | | |
| KLIX20050829_130035.gz | recast-f32 | 55.0 / 41.9 / 24.6 | 41.9 | 22.3 | 31.9 | 0.85 | no | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | recast | 49.2 / 57.3 / 23.3 | 49.2 | 20.3 | 31.9 | 1.00 |  | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | nexrad-crate | 155.3 / 59.5 / 43.7 | 59.5 | 39.3 | 53.4 | 1.21 | no | 0 sw, 0 rays |
| KLIX20050829_130035.gz | rsl | 287.6 / 270.7 / 115.0 | 270.7 | 113.0 | 26.2 | 5.50 | yes | 9 sw, 9,310,724 gates |
| KLIX20050829_130035.gz | pyart | 1203.7 / 704.9 | 954.3 | 693.0 | 518.9 | 19.38 | yes | 20 sw, 40,031,040 gates |
| KLIX20050829_130035.gz | lrose | fails: File format not recognized: data/KLIX20050829_130035.gz | | | | | | |
| KLIX20050829_130035.gz | metpy | fails: IndexError: tuple index out of range | | | | | | |
| KLIX20050829_130035.gz | xradar | fails: TypeError: unsupported operand type(s) for +: 'NoneType' and 'int' | | | | | | |

#### Level II, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20240315_000217_V06 | recast | 679.5 | 679.5 | 679.5 | 95.2 | 1.00 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | recast-f32 | 790.4 | 790.4 | 790.4 | 95.5 | 1.16 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | recast-paired | 1209.8 | 1209.8 | 1209.8 | 100.0 | 1.78 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | nexrad-crate | 1785.2 | 1785.2 | 1785.2 | 101.2 | 2.63 |  | 20 sw, 11,520 rays |
| KTLX20240315_000217_V06 | rsl | 2324.1 | 2324.1 | 2324.1 | 105.2 | 3.42 |  | 11 sw, 32,091,840 gates |
| KTLX20240315_000217_V06 | radrs | 2559.8 | 2559.8 | 2559.8 | 551.7 | 3.77 |  | 20 sw, 73,517,869 gates |
| KTLX20240315_000217_V06 | go-nexrad | 2775.4 | 2775.4 | 2775.4 | 182.0 | 4.08 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | metpy | 3132.9 | 3132.9 | 3132.9 | 815.2 | 4.61 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | lrose | 7079.6 | 7079.6 | 7079.6 | 381.1 | 10.42 |  | 14 sw, 84,257,280 gates |
| KTLX20240315_000217_V06 | xradar | 8287.5 | 8287.5 | 8287.5 | 988.7 | 12.20 |  | 20 sw, 73,517,865 gates |
| KTLX20240315_000217_V06 | pyart | 8695.4 | 8695.4 | 8695.4 | 1139.3 | 12.80 |  | 20 sw, 147,732,480 gates |
| KILX20260418_013553_V06 | recast-paired | 1660.4 | 1660.4 | 1660.4 | 116.5 | 0.43 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | recast-f32 | 3049.4 | 3049.4 | 3049.4 | 111.0 | 0.80 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | nexrad-crate | 3522.2 | 3522.2 | 3522.2 | 116.6 | 0.92 |  | 23 sw, 12,600 rays |
| KILX20260418_013553_V06 | recast | 3818.3 | 3818.3 | 3818.3 | 111.0 | 1.00 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | radrs | 5323.0 | 5323.0 | 5323.0 | 576.5 | 1.39 |  | 23 sw, 75,464,764 gates |
| KILX20260418_013553_V06 | rsl | 5375.4 | 5375.4 | 5375.4 | 110.2 | 1.41 |  | 16 sw, 45,256,320 gates |
| KILX20260418_013553_V06 | go-nexrad | 5589.4 | 5589.4 | 5589.4 | 217.5 | 1.46 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | pyart | 8243.2 | 8243.2 | 8243.2 | 1223.5 | 2.16 |  | 23 sw, 161,582,400 gates |
| KILX20260418_013553_V06 | metpy | 9581.0 | 9581.0 | 9581.0 | 841.3 | 2.51 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | lrose | 10649.8 | 10649.8 | 10649.8 | 394.2 | 2.79 |  | 17 sw, 86,284,800 gates |
| KILX20260418_013553_V06 | xradar | 20329.1 | 20329.1 | 20329.1 | 1042.7 | 5.32 |  | 23 sw, 75,464,760 gates |
| KTLX20130520_201643_V06.gz | recast | 103.5 | 103.5 | 103.5 | 53.8 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | recast-f32 | 139.2 | 139.2 | 139.2 | 53.8 | 1.34 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | nexrad-crate | 272.8 | 272.8 | 272.8 | 134.1 | 2.63 |  | 17 sw, 8,280 rays |
| KTLX20130520_201643_V06.gz | go-nexrad | 359.4 | 359.4 | 359.4 | 137.1 | 3.47 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | rsl | 633.2 | 633.2 | 633.2 | 75.8 | 6.12 |  | 14 sw, 34,050,240 gates |
| KTLX20130520_201643_V06.gz | pyart | 1718.6 | 1718.6 | 1718.6 | 784.3 | 16.60 |  | 17 sw, 91,013,760 gates |
| KTLX20130520_201643_V06.gz | metpy | 1923.1 | 1923.1 | 1923.1 | 555.6 | 18.58 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | lrose | fails: File format not recognized: data/KTLX20130520_201643_V06.gz | | | | | | |
| KTLX20130520_201643_V06.gz | radrs | fails: ValueError: NEXRAD data error: gzip-compressed volume file must be decompressed before accessing records | | | | | | |
| KTLX20130520_201643_V06.gz | xradar | fails: TypeError: unsupported operand type(s) for +: 'NoneType' and 'int' | | | | | | |
| KLIX20050829_130035.gz | recast | 40.2 | 40.2 | 40.2 | 18.8 | 1.00 |  | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | recast-f32 | 91.2 | 91.2 | 91.2 | 18.8 | 2.27 |  | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | nexrad-crate | 98.5 | 98.5 | 98.5 | 38.4 | 2.45 |  | 0 sw, 0 rays |
| KLIX20050829_130035.gz | rsl | 402.7 | 402.7 | 402.7 | 26.5 | 10.01 |  | 9 sw, 9,310,724 gates |
| KLIX20050829_130035.gz | pyart | 1483.7 | 1483.7 | 1483.7 | 467.4 | 36.87 |  | 20 sw, 40,031,040 gates |
| KLIX20050829_130035.gz | lrose | fails: File format not recognized: data/KLIX20050829_130035.gz | | | | | | |
| KLIX20050829_130035.gz | metpy | fails: IndexError: tuple index out of range | | | | | | |
| KLIX20050829_130035.gz | xradar | fails: TypeError: unsupported operand type(s) for +: 'NoneType' and 'int' | | | | | | |

#### Level II, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20240315_000217_V06 | recast | 308.5 | 308.5 | 308.5 | 202.5 | 1.00 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | recast-paired | 445.3 | 445.3 | 445.3 | 230.8 | 1.44 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | nexrad-crate | 475.2 | 475.2 | 475.2 | 178.7 | 1.54 |  | 20 sw, 11,520 rays |
| KTLX20240315_000217_V06 | recast-f32 | 546.4 | 546.4 | 546.4 | 202.5 | 1.77 |  | 20 sw, 64,604,160 gates |
| KILX20260418_013553_V06 | recast-paired | 304.8 | 304.8 | 304.8 | 333.3 | 0.82 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | recast | 370.1 | 370.1 | 370.1 | 271.5 | 1.00 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | nexrad-crate | 416.5 | 416.5 | 416.5 | 210.0 | 1.13 |  | 23 sw, 12,600 rays |
| KILX20260418_013553_V06 | recast-f32 | 494.9 | 494.9 | 494.9 | 269.8 | 1.34 |  | 23 sw, 66,800,160 gates |
| KTLX20130520_201643_V06.gz | recast | 105.6 | 105.6 | 105.6 | 53.8 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | recast-f32 | 139.1 | 139.1 | 139.1 | 53.8 | 1.32 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | nexrad-crate | 323.8 | 323.8 | 323.8 | 134.6 | 3.07 |  | 17 sw, 8,280 rays |
| KLIX20050829_130035.gz | recast | 54.6 | 54.6 | 54.6 | 19.0 | 1.00 |  | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | recast-f32 | 62.7 | 62.7 | 62.7 | 18.8 | 1.15 |  | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | nexrad-crate | 100.1 | 100.1 | 100.1 | 38.8 | 1.83 |  | 0 sw, 0 rays |

#### Level II real-time chunks, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KIWA307_chunks001-003 | recast | 55.2 / 31.4 / 27.3 | 31.4 | 15.1 | 12.7 | 1.00 |  | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | recast-f32 | 79.7 / 61.6 / 34.5 | 61.6 | 17.6 | 16.0 | 1.96 | no | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | nexrad-crate | 80.0 / 70.6 / 51.0 | 70.6 | 33.0 | 12.9 | 2.25 | no | 1 sw, 240 rays |
| KIWA307_chunks001-003 | pyart | 81.3 / 73.5 / 99.7 | 81.3 | 72.1 | 248.6 | 2.59 | yes | 1 sw, 2,198,400 gates |
| KIWA307_chunks001-003 | xradar | 103.6 / 85.7 / 73.5 | 85.7 | 58.0 | 154.2 | 2.73 | yes | 0 sw, 0 rays |
| KIWA307_chunks001-003 | metpy | 115.8 / 88.3 / 67.9 | 88.3 | 63.6 | 204.9 | 2.81 | yes | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | radrs | 94.7 / 92.6 / 71.9 | 92.6 | 68.7 | 153.5 | 2.95 | yes | 1 sw, 2,198,414 gates |
| KIWA307_chunks001-003 | rsl | 151.2 / 118.7 / 114.8 | 118.7 | 96.4 | 5.8 | 3.78 | yes | 1 sw, 1,297,920 gates |
| KIWA307_chunks001-003 | go-nexrad | 144.8 / 115.1 / 126.9 | 126.9 | 70.3 | 18.8 | 4.04 | yes | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | lrose | fails: File format not recognized: data/KIWA307_chunks001-003 | | | | | | |
| KIWA307_chunks001-035 | recast-f32 | 483.7 / 1191.3 / 464.8 | 483.7 | 314.5 | 70.2 | 0.99 | no | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | recast | 489.4 / 433.1 / 499.3 | 489.4 | 226.7 | 69.9 | 1.00 |  | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | nexrad-crate | 1705.3 / 1124.1 / 512.2 | 1124.1 | 466.6 | 72.6 | 2.30 | yes | 6 sw, 4,080 rays |
| KIWA307_chunks001-035 | rsl | 1157.8 / 1320.7 / 994.6 | 1157.8 | 797.7 | 36.5 | 2.37 | yes | 3 sw, 16,172,160 gates |
| KIWA307_chunks001-035 | metpy | 1430.7 / 1161.8 / 1013.8 | 1161.8 | 926.3 | 403.2 | 2.37 | yes | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | radrs | 1447.6 / 1186.4 / 886.0 | 1186.4 | 689.9 | 335.4 | 2.42 | yes | 6 sw, 26,219,559 gates |
| KIWA307_chunks001-035 | pyart | 2319.6 / 1430.9 / 1276.5 | 1430.9 | 1200.1 | 562.5 | 2.92 | yes | 6 sw, 52,321,920 gates |
| KIWA307_chunks001-035 | go-nexrad | 1575.5 / 2035.7 / 1175.4 | 1575.5 | 1037.3 | 94.9 | 3.22 | yes | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | lrose | 2567.8 / 2096.6 / 1903.2 | 2096.6 | 1735.3 | 166.8 | 4.28 | yes | 3 sw, 30,965,760 gates |
| KIWA307_chunks001-035 | xradar | 4390.8 / 2874.7 / 2558.9 | 2874.7 | 2322.6 | 709.0 | 5.87 | yes | 5 sw, 24,503,070 gates |

#### Level II real-time chunks, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KIWA307_chunks001-003 | recast-f32 | 20.7 / 20.2 / 28.5 | 20.7 | 14.5 | 33.2 | 0.74 | no | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | recast | 16.0 / 44.5 / 28.2 | 28.2 | 13.9 | 26.4 | 1.00 |  | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | nexrad-crate | 38.3 / 47.1 / 29.9 | 38.3 | 23.9 | 33.1 | 1.36 | no | 1 sw, 240 rays |
| KIWA307_chunks001-003 | pyart | 80.4 / 94.1 / 79.4 | 80.4 | 74.0 | 247.9 | 2.85 | yes | 1 sw, 2,198,400 gates |
| KIWA307_chunks001-003 | xradar | 82.4 / 86.1 / 69.8 | 82.4 | 66.3 | 154.0 | 2.92 | yes | 0 sw, 0 rays |
| KIWA307_chunks001-003 | radrs | 62.6 / 84.4 / 108.8 | 84.4 | 57.1 | 248.4 | 2.99 | yes | 1 sw, 2,198,414 gates |
| KIWA307_chunks001-003 | go-nexrad | 98.8 / 96.5 / 169.1 | 98.8 | 64.8 | 22.2 | 3.51 | yes | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | metpy | 83.9 / 102.8 / 129.8 | 102.8 | 70.6 | 205.1 | 3.65 | yes | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | rsl | 136.1 / 133.1 / 164.9 | 136.1 | 92.9 | 5.6 | 4.83 | yes | 1 sw, 1,297,920 gates |
| KIWA307_chunks001-003 | lrose | fails: File format not recognized: data/KIWA307_chunks001-003 | | | | | | |
| KIWA307_chunks001-035 | recast-f32 | 189.9 / 83.4 / 71.7 | 83.4 | 46.0 | 207.0 | 0.75 | no | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | nexrad-crate | 166.8 / 102.6 / 108.1 | 108.1 | 72.7 | 215.9 | 0.97 | no | 6 sw, 4,080 rays |
| KIWA307_chunks001-035 | recast | 186.4 / 111.4 / 54.3 | 111.4 | 46.2 | 207.8 | 1.00 |  | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | radrs | 521.8 / 296.2 / 167.2 | 296.2 | 148.4 | 579.4 | 2.66 | no | 6 sw, 26,219,559 gates |
| KIWA307_chunks001-035 | rsl | 1076.6 / 923.8 / 701.7 | 923.8 | 627.4 | 36.5 | 8.29 | yes | 3 sw, 16,172,160 gates |
| KIWA307_chunks001-035 | go-nexrad | 1457.6 / 1135.1 / 846.3 | 1135.1 | 717.4 | 93.6 | 10.19 | yes | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | metpy | 3091.2 / 1613.3 / 1118.1 | 1613.3 | 1110.0 | 403.1 | 14.48 | yes | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | pyart | 2135.1 / 1693.1 / 1079.9 | 1693.1 | 1041.0 | 561.9 | 15.19 | yes | 6 sw, 52,321,920 gates |
| KIWA307_chunks001-035 | lrose | 1987.8 / 2056.2 / 1383.0 | 1987.8 | 1324.0 | 166.9 | 17.84 | yes | 3 sw, 30,965,760 gates |
| KIWA307_chunks001-035 | xradar | 4741.8 / 3206.6 / 2211.5 | 3206.6 | 2094.1 | 716.5 | 28.77 | yes | 5 sw, 24,503,070 gates |

#### Level II real-time chunks, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KIWA307_chunks001-003 | recast | 31.1 | 31.1 | 31.1 | 10.5 | 1.00 |  | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | recast-f32 | 49.7 | 49.7 | 49.7 | 11.5 | 1.60 |  | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | nexrad-crate | 63.3 | 63.3 | 63.3 | 9.1 | 2.04 |  | 1 sw, 240 rays |
| KIWA307_chunks001-003 | rsl | 114.8 | 114.8 | 114.8 | 5.8 | 3.69 |  | 1 sw, 1,297,920 gates |
| KIWA307_chunks001-003 | go-nexrad | 127.8 | 127.8 | 127.8 | 15.2 | 4.11 |  | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | pyart | 135.6 | 135.6 | 135.6 | 247.1 | 4.36 |  | 1 sw, 2,198,400 gates |
| KIWA307_chunks001-003 | metpy | 158.1 | 158.1 | 158.1 | 204.5 | 5.09 |  | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | xradar | 158.8 | 158.8 | 158.8 | 150.2 | 5.11 |  | 0 sw, 0 rays |
| KIWA307_chunks001-003 | radrs | 808.7 | 808.7 | 808.7 | 147.9 | 26.02 |  | 1 sw, 2,198,414 gates |
| KIWA307_chunks001-003 | lrose | fails: File format not recognized: data/KIWA307_chunks001-003 | | | | | | |
| KIWA307_chunks001-035 | recast-f32 | 400.8 | 400.8 | 400.8 | 41.0 | 0.70 |  | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | recast | 571.7 | 571.7 | 571.7 | 40.8 | 1.00 |  | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | nexrad-crate | 963.7 | 963.7 | 963.7 | 40.5 | 1.69 |  | 6 sw, 4,080 rays |
| KIWA307_chunks001-035 | rsl | 1447.6 | 1447.6 | 1447.6 | 36.2 | 2.53 |  | 3 sw, 16,172,160 gates |
| KIWA307_chunks001-035 | go-nexrad | 1863.3 | 1863.3 | 1863.3 | 74.5 | 3.26 |  | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | radrs | 2065.2 | 2065.2 | 2065.2 | 277.7 | 3.61 |  | 6 sw, 26,219,559 gates |
| KIWA307_chunks001-035 | metpy | 2849.2 | 2849.2 | 2849.2 | 403.2 | 4.98 |  | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | pyart | 2889.9 | 2889.9 | 2889.9 | 562.8 | 5.06 |  | 6 sw, 52,321,920 gates |
| KIWA307_chunks001-035 | lrose | 2977.9 | 2977.9 | 2977.9 | 147.7 | 5.21 |  | 3 sw, 30,965,760 gates |
| KIWA307_chunks001-035 | xradar | 3214.6 | 3214.6 | 3214.6 | 449.0 | 5.62 |  | 5 sw, 24,503,070 gates |

#### Level II real-time chunks, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KIWA307_chunks001-003 | recast | 20.7 | 20.7 | 20.7 | 14.0 | 1.00 |  | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | recast-f32 | 26.8 | 26.8 | 26.8 | 15.1 | 1.29 |  | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | nexrad-crate | 62.5 | 62.5 | 62.5 | 10.9 | 3.02 |  | 1 sw, 240 rays |
| KIWA307_chunks001-035 | recast | 173.2 | 173.2 | 173.2 | 121.8 | 1.00 |  | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | recast-f32 | 191.9 | 191.9 | 191.9 | 123.8 | 1.11 |  | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | nexrad-crate | 286.1 | 286.1 | 286.1 | 91.1 | 1.65 |  | 6 sw, 4,080 rays |

#### Level III, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_N0B_20260622_0806 | recast-f32 | 21.6 / 18.1 / 8.2 | 18.1 | 7.1 | 15.9 | 0.90 | no | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | recast | 20.2 / 25.2 / 10.3 | 20.2 | 8.0 | 13.3 | 1.00 |  | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | pyart | 57.3 / 45.3 / 25.2 | 45.3 | 23.4 | 251.8 | 2.24 | yes | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | metpy | 56.2 / 72.5 / 27.6 | 56.2 | 23.8 | 194.0 | 2.78 | yes | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | lrose | fails: File format not recognized: data/TLX_N0B_20260622_0806 | | | | | | |
| TLX_N0Q_20130520_2016 | recast-f32 | 0.5 / 0.5 / 0.4 | 0.5 | 0.4 | 4.8 | 0.97 | no | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | recast | 0.6 / 0.6 / 0.4 | 0.6 | 0.4 | 4.4 | 1.00 |  | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | metpy | 3.1 / 3.3 / 2.3 | 3.1 | 1.9 | 189.2 | 5.72 | yes | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | pyart | 5.0 / 4.9 / 3.2 | 4.9 | 2.7 | 234.7 | 8.93 | yes | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | lrose | fails: File format not recognized: data/TLX_N0Q_20130520_2016 | | | | | | |
| TLX_N0U_20220503_0052 | recast-f32 | 1.9 / 1.9 / 1.4 | 1.9 | 1.3 | 7.3 | 0.91 | no | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | recast | 2.3 / 2.1 / 1.5 | 2.1 | 1.3 | 6.3 | 1.00 |  | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | metpy | 11.2 / 9.3 / 6.8 | 9.3 | 6.1 | 189.3 | 4.36 | yes | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | pyart | 13.6 / 13.1 / 7.1 | 13.1 | 6.5 | 239.0 | 6.16 | yes | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | lrose | fails: File format not recognized: data/TLX_N0U_20220503_0052 | | | | | | |
| TLX_DPR_20260622_0806 | recast | 95.2 / 31.5 / 16.1 | 31.5 | 10.4 | 13.2 | 1.00 |  | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | recast-f32 | 126.2 / 45.8 / 15.7 | 45.8 | 10.5 | 12.9 | 1.45 | no | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | metpy | 140.7 / 66.4 / 31.1 | 66.4 | 27.8 | 197.7 | 2.11 | no | 1 sw, 0 rays |
| TLX_DPR_20260622_0806 | pyart | 215.4 / 162.2 / 66.1 | 162.2 | 62.0 | 241.2 | 5.15 | no | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | lrose | fails: File format not recognized: data/TLX_DPR_20260622_0806 | | | | | | |

#### Level III, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_N0B_20260622_0806 | recast-f32 | 20.6 / 14.0 / 8.2 | 14.0 | 7.1 | 16.1 | 0.96 | no | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | recast | 23.1 / 14.5 / 8.9 | 14.5 | 7.3 | 12.1 | 1.00 |  | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | pyart | 43.0 / 42.4 / 26.0 | 42.4 | 22.1 | 250.2 | 2.92 | yes | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | metpy | 79.6 / 56.4 / 25.9 | 56.4 | 22.2 | 194.5 | 3.88 | yes | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | lrose | fails: File format not recognized: data/TLX_N0B_20260622_0806 | | | | | | |
| TLX_N0Q_20130520_2016 | recast-f32 | 0.6 / 0.5 / 0.4 | 0.5 | 0.4 | 4.8 | 0.97 | no | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | recast | 0.5 / 0.6 / 0.4 | 0.5 | 0.4 | 4.5 | 1.00 |  | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | metpy | 3.8 / 2.9 / 2.4 | 2.9 | 2.1 | 189.2 | 5.34 | yes | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | pyart | 5.7 / 4.2 / 3.3 | 4.2 | 2.7 | 234.3 | 7.88 | yes | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | lrose | fails: File format not recognized: data/TLX_N0Q_20130520_2016 | | | | | | |
| TLX_N0U_20220503_0052 | recast-f32 | 1.9 / 1.8 / 1.4 | 1.8 | 1.3 | 7.3 | 0.94 | no | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | recast | 1.9 / 1.9 / 1.4 | 1.9 | 1.3 | 5.9 | 1.00 |  | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | metpy | 8.9 / 8.6 / 6.7 | 8.6 | 6.0 | 189.7 | 4.53 | yes | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | pyart | 15.2 / 17.9 / 7.1 | 15.2 | 6.6 | 237.5 | 7.99 | yes | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | lrose | fails: File format not recognized: data/TLX_N0U_20220503_0052 | | | | | | |
| TLX_DPR_20260622_0806 | recast-f32 | 18.5 / 20.3 / 13.6 | 18.5 | 10.4 | 13.3 | 0.80 | no | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | recast | 65.2 / 23.1 / 14.7 | 23.1 | 10.1 | 12.7 | 1.00 |  | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | metpy | 252.7 / 67.4 / 35.0 | 67.4 | 28.4 | 197.5 | 2.92 | no | 1 sw, 0 rays |
| TLX_DPR_20260622_0806 | pyart | 176.9 / 141.6 / 74.0 | 141.6 | 68.0 | 241.2 | 6.13 | yes | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | lrose | fails: File format not recognized: data/TLX_DPR_20260622_0806 | | | | | | |

#### Level III, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_N0B_20260622_0806 | recast | 27.7 | 27.7 | 27.7 | 8.6 | 1.00 |  | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | metpy | 60.8 | 60.8 | 60.8 | 192.5 | 2.19 |  | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | recast-f32 | 78.6 | 78.6 | 78.6 | 13.6 | 2.83 |  | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | pyart | 147.1 | 147.1 | 147.1 | 251.3 | 5.30 |  | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | lrose | fails: File format not recognized: data/TLX_N0B_20260622_0806 | | | | | | |
| TLX_N0Q_20130520_2016 | recast | 1.1 | 1.1 | 1.1 | 4.0 | 1.00 |  | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | recast-f32 | 1.3 | 1.3 | 1.3 | 4.5 | 1.20 |  | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | metpy | 3.2 | 3.2 | 3.2 | 189.4 | 2.96 |  | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | pyart | 8.6 | 8.6 | 8.6 | 233.8 | 7.87 |  | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | lrose | fails: File format not recognized: data/TLX_N0Q_20130520_2016 | | | | | | |
| TLX_N0U_20220503_0052 | recast | 2.7 | 2.7 | 2.7 | 4.6 | 1.00 |  | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | recast-f32 | 4.0 | 4.0 | 4.0 | 6.4 | 1.46 |  | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | metpy | 17.0 | 17.0 | 17.0 | 191.0 | 6.25 |  | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | pyart | 20.6 | 20.6 | 20.6 | 238.2 | 7.56 |  | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | lrose | fails: File format not recognized: data/TLX_N0U_20220503_0052 | | | | | | |
| TLX_DPR_20260622_0806 | recast-f32 | 28.9 | 28.9 | 28.9 | 10.1 | 0.59 |  | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | recast | 49.2 | 49.2 | 49.2 | 9.6 | 1.00 |  | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | metpy | 78.1 | 78.1 | 78.1 | 197.4 | 1.59 |  | 1 sw, 0 rays |
| TLX_DPR_20260622_0806 | pyart | 143.0 | 143.0 | 143.0 | 240.9 | 2.91 |  | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | lrose | fails: File format not recognized: data/TLX_DPR_20260622_0806 | | | | | | |

#### Level III, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_N0B_20260622_0806 | recast-f32 | 27.5 | 27.5 | 27.5 | 13.6 | 0.75 |  | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | recast | 36.8 | 36.8 | 36.8 | 8.5 | 1.00 |  | 1 sw, 1,324,800 gates |
| TLX_N0Q_20130520_2016 | recast | 1.1 | 1.1 | 1.1 | 4.0 | 1.00 |  | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | recast-f32 | 1.4 | 1.4 | 1.4 | 4.8 | 1.22 |  | 1 sw, 165,600 gates |
| TLX_N0U_20220503_0052 | recast | 2.6 | 2.6 | 2.6 | 4.6 | 1.00 |  | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | recast-f32 | 4.6 | 4.6 | 4.6 | 6.4 | 1.74 |  | 1 sw, 432,000 gates |
| TLX_DPR_20260622_0806 | recast | 23.0 | 23.0 | 23.0 | 9.5 | 1.00 |  | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | recast-f32 | 30.1 | 30.1 | 30.1 | 10.4 | 1.31 |  | 1 sw, 331,200 gates |

#### ODIM_H5, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| iesha.pvol.20260305T0115.h5 | recast | 13.8 / 8.0 / 6.4 | 8.0 | 5.5 | 14.1 | 1.00 |  | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | recast-f32 | 11.0 / 8.6 / 8.1 | 8.6 | 6.1 | 14.3 | 1.08 | no | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | h5py | 22.8 / 24.2 / 15.1 | 22.8 | 14.3 | 44.5 | 2.84 | yes | 0 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | wradlib | 84.3 / 68.3 / 37.1 | 68.3 | 34.1 | 222.7 | 8.54 | yes | 0 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | pyart | 124.9 / 125.9 / 64.6 | 124.9 | 52.8 | 266.5 | 15.60 | yes | 10 sw, 5,367,600 gates |
| iesha.pvol.20260305T0115.h5 | lrose | 211.9 / 171.0 / 97.1 | 171.0 | 87.1 | 62.5 | 21.36 | yes | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | xradar | 398.7 / 301.8 / 195.1 | 301.8 | 188.3 | 289.4 | 37.71 | yes | 10 sw, 4,343,845 gates |
| dkrom.pvol.20260820T1130.h5 | recast | 26.1 / 17.0 / 9.4 | 17.0 | 9.1 | 32.6 | 1.00 |  | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | recast-f32 | 36.5 / 19.4 / 12.7 | 19.4 | 12.3 | 32.3 | 1.14 | no | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | h5py | 110.3 / 72.8 / 47.0 | 72.8 | 44.4 | 46.8 | 4.28 | yes | 0 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | wradlib | 283.0 / 119.5 / 71.4 | 119.5 | 65.7 | 234.7 | 7.03 | yes | 0 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | pyart | 528.1 / 352.7 / 158.6 | 352.7 | 151.3 | 308.6 | 20.74 | yes | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | lrose | 899.1 / 360.2 / 248.8 | 360.2 | 228.1 | 96.1 | 21.18 | yes | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | xradar | 896.3 / 659.2 / 322.8 | 659.2 | 306.8 | 365.7 | 38.77 | yes | 10 sw, 13,651,285 gates |
| bejab.pvol.hdf | recast | 3.6 / 3.8 / 2.3 | 3.6 | 2.2 | 8.1 | 1.00 |  | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | recast-f32 | 3.7 / 3.6 / 2.6 | 3.6 | 2.5 | 8.8 | 1.01 | no | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | h5py | 12.6 / 11.4 / 6.9 | 11.4 | 6.4 | 43.6 | 3.19 | yes | 0 sw, 1,831,680 gates |
| bejab.pvol.hdf | wradlib | 33.8 / 33.9 / 13.9 | 33.8 | 13.5 | 218.6 | 9.44 | yes | 0 sw, 1,831,680 gates |
| bejab.pvol.hdf | pyart | 55.0 / 52.3 / 26.6 | 52.3 | 23.2 | 253.0 | 14.62 | yes | 11 sw, 2,368,080 gates |
| bejab.pvol.hdf | lrose | 77.0 / 57.3 / 34.7 | 57.3 | 31.4 | 37.6 | 16.00 | yes | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | xradar | 310.0 / 371.2 / 131.3 | 310.0 | 127.1 | 269.0 | 86.57 | yes | 11 sw, 1,831,773 gates |

#### ODIM_H5, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| iesha.pvol.20260305T0115.h5 | recast | 11.1 / 6.7 / 5.6 | 6.7 | 5.2 | 14.0 | 1.00 |  | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | recast-f32 | 14.7 / 10.1 / 6.9 | 10.1 | 6.3 | 14.0 | 1.50 | no | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | h5py | 24.7 / 37.8 / 14.4 | 24.7 | 13.2 | 44.6 | 3.66 | yes | 0 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | wradlib | 62.4 / 64.0 / 35.4 | 62.4 | 34.1 | 223.7 | 9.26 | yes | 0 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | pyart | 179.5 / 130.5 / 61.2 | 130.5 | 58.7 | 267.1 | 19.37 | yes | 10 sw, 5,367,600 gates |
| iesha.pvol.20260305T0115.h5 | lrose | 173.6 / 179.7 / 92.2 | 173.6 | 79.6 | 62.6 | 25.76 | yes | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | xradar | 453.7 / 326.0 / 207.7 | 326.0 | 190.0 | 289.3 | 48.39 | yes | 10 sw, 4,343,845 gates |
| dkrom.pvol.20260820T1130.h5 | recast | 23.2 / 13.5 / 9.4 | 13.5 | 9.0 | 32.4 | 1.00 |  | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | recast-f32 | 26.3 / 17.8 / 13.1 | 17.8 | 12.6 | 32.3 | 1.32 | no | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | h5py | 121.7 / 73.4 / 50.5 | 73.4 | 46.2 | 46.7 | 5.43 | yes | 0 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | wradlib | 177.2 / 159.9 / 70.5 | 159.9 | 66.5 | 233.9 | 11.82 | yes | 0 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | pyart | 393.9 / 342.3 / 169.8 | 342.3 | 153.3 | 312.3 | 25.30 | yes | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | lrose | 565.3 / 545.9 / 260.0 | 545.9 | 231.2 | 96.3 | 40.36 | yes | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | xradar | 976.6 / 613.0 / 341.0 | 613.0 | 319.9 | 365.3 | 45.32 | yes | 10 sw, 13,651,285 gates |
| bejab.pvol.hdf | recast | 3.7 / 3.4 / 2.6 | 3.4 | 2.3 | 7.8 | 1.00 |  | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | recast-f32 | 3.9 / 4.8 / 3.1 | 3.9 | 2.7 | 8.8 | 1.16 | no | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | h5py | 11.6 / 11.5 / 7.6 | 11.5 | 6.1 | 43.7 | 3.40 | yes | 0 sw, 1,831,680 gates |
| bejab.pvol.hdf | wradlib | 31.2 / 22.6 / 13.5 | 22.6 | 13.0 | 218.9 | 6.69 | yes | 0 sw, 1,831,680 gates |
| bejab.pvol.hdf | pyart | 44.1 / 40.1 / 24.2 | 40.1 | 22.7 | 252.5 | 11.88 | yes | 11 sw, 2,368,080 gates |
| bejab.pvol.hdf | lrose | 62.0 / 66.6 / 35.0 | 62.0 | 29.7 | 37.6 | 18.38 | yes | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | xradar | 289.1 / 287.3 / 139.4 | 287.3 | 125.0 | 268.6 | 85.09 | yes | 11 sw, 1,831,773 gates |

#### ODIM_H5, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| iesha.pvol.20260305T0115.h5 | recast-f32 | 19.7 | 19.7 | 19.7 | 9.5 | 0.42 |  | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | recast | 47.3 | 47.3 | 47.3 | 9.5 | 1.00 |  | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | h5py | 49.9 | 49.9 | 49.9 | 43.2 | 1.05 |  | 0 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | wradlib | 98.2 | 98.2 | 98.2 | 222.9 | 2.07 |  | 0 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | pyart | 313.6 | 313.6 | 313.6 | 265.2 | 6.63 |  | 10 sw, 5,367,600 gates |
| iesha.pvol.20260305T0115.h5 | lrose | 338.5 | 338.5 | 338.5 | 59.0 | 7.15 |  | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | xradar | 3896.6 | 3896.6 | 3896.6 | 287.8 | 82.36 |  | 10 sw, 4,343,845 gates |
| dkrom.pvol.20260820T1130.h5 | recast-f32 | 44.8 | 44.8 | 44.8 | 18.8 | 0.87 |  | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | recast | 51.4 | 51.4 | 51.4 | 19.0 | 1.00 |  | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | h5py | 96.8 | 96.8 | 96.8 | 46.1 | 1.88 |  | 0 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | wradlib | 179.3 | 179.3 | 179.3 | 234.2 | 3.49 |  | 0 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | pyart | 361.1 | 361.1 | 361.1 | 308.2 | 7.03 |  | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | lrose | 536.3 | 536.3 | 536.3 | 95.6 | 10.44 |  | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | xradar | 3485.7 | 3485.7 | 3485.7 | 363.2 | 67.84 |  | 10 sw, 13,651,285 gates |
| bejab.pvol.hdf | recast-f32 | 6.4 | 6.4 | 6.4 | 6.3 | 0.56 |  | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | recast | 11.4 | 11.4 | 11.4 | 6.0 | 1.00 |  | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | h5py | 12.3 | 12.3 | 12.3 | 43.0 | 1.08 |  | 0 sw, 1,831,680 gates |
| bejab.pvol.hdf | lrose | 74.6 | 74.6 | 74.6 | 36.5 | 6.53 |  | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | wradlib | 84.8 | 84.8 | 84.8 | 219.0 | 7.42 |  | 0 sw, 1,831,680 gates |
| bejab.pvol.hdf | pyart | 138.7 | 138.7 | 138.7 | 249.7 | 12.14 |  | 11 sw, 2,368,080 gates |
| bejab.pvol.hdf | xradar | 2450.2 | 2450.2 | 2450.2 | 267.7 | 214.61 |  | 11 sw, 1,831,773 gates |

#### ODIM_H5, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| iesha.pvol.20260305T0115.h5 | recast-f32 | 12.3 | 12.3 | 12.3 | 9.5 | 0.56 |  | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | recast | 21.9 | 21.9 | 21.9 | 9.5 | 1.00 |  | 10 sw, 4,343,760 gates |
| dkrom.pvol.20260820T1130.h5 | recast-f32 | 42.3 | 42.3 | 42.3 | 18.8 | 0.55 |  | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | recast | 76.7 | 76.7 | 76.7 | 18.8 | 1.00 |  | 10 sw, 13,651,200 gates |
| bejab.pvol.hdf | recast | 4.8 | 4.8 | 4.8 | 6.0 | 1.00 |  | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | recast-f32 | 5.9 | 5.9 | 5.9 | 6.4 | 1.21 |  | 11 sw, 1,831,680 gates |

#### CfRadial 1, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| cfrad.SPOL_20080604_002217.classic.nc | recast | 11.6 / 23.4 / 11.6 | 11.6 | 9.0 | 73.0 | 1.00 |  | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | recast-f32 | 17.5 / 50.7 / 16.8 | 17.5 | 11.5 | 72.5 | 1.51 | no | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | netcdf4 | 47.0 / 63.4 / 29.2 | 47.0 | 26.2 | 92.5 | 4.04 | yes | 0 sw, 8,714,513 gates |
| cfrad.SPOL_20080604_002217.classic.nc | pyart | 93.6 / 130.7 / 77.8 | 93.6 | 71.7 | 310.8 | 8.05 | yes | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | lrose | 138.9 / 295.1 / 105.3 | 138.9 | 91.8 | 89.8 | 11.94 | yes | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | xradar | 158.7 / 319.7 / 117.5 | 158.7 | 115.1 | 205.7 | 13.65 | yes | 9 sw, 8,699,099 gates |
| cfrad.SPOL_20080604_002217.classic.nc | wradlib | 170.5 / 313.7 / 152.1 | 170.5 | 146.1 | 292.5 | 14.66 | yes | 0 sw, 8,713,211 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast | 1.0 / 0.4 / 0.4 | 0.4 | 0.2 | 9.9 | 1.00 |  | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast-f32 | 1.4 / 2.0 / 0.6 | 1.4 | 0.6 | 10.1 | 3.45 | no | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | netcdf4 | 11.0 / 9.9 / 9.7 | 9.9 | 4.8 | 51.8 | 23.55 | yes | 0 sw, 1,605,011 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | lrose | 13.3 / 16.1 / 11.7 | 13.3 | 7.6 | 32.0 | 31.73 | yes | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | pyart | 31.2 / 38.5 / 17.9 | 31.2 | 17.3 | 245.0 | 74.33 | yes | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | wradlib | 38.2 / 50.8 / 42.7 | 42.7 | 28.6 | 226.5 | 101.55 | yes | 0 sw, 1,604,563 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | xradar | 54.1 / 67.6 / 34.9 | 54.1 | 34.6 | 174.7 | 128.82 | yes | 2 sw, 1,601,235 gates |
| cfrad.DOW8_RHI.trim3.nc | recast | 0.4 / 0.4 / 0.3 | 0.4 | 0.2 | 6.5 | 1.00 |  | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | recast-f32 | 0.7 / 0.6 / 0.5 | 0.6 | 0.4 | 6.5 | 1.72 | yes | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | lrose | 13.3 / 7.5 / 4.1 | 7.5 | 3.9 | 29.0 | 20.27 | yes | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | netcdf4 | 13.0 / 8.3 / 6.4 | 8.3 | 4.9 | 46.9 | 22.51 | yes | 0 sw, 426,699 gates |
| cfrad.DOW8_RHI.trim3.nc | pyart | 27.2 / 20.8 / 12.7 | 20.8 | 12.6 | 236.5 | 56.39 | yes | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | xradar | 82.2 / 36.6 / 23.6 | 36.6 | 22.7 | 171.0 | 99.23 | yes | 1 sw, 424,626 gates |
| cfrad.DOW8_RHI.trim3.nc | wradlib | 55.6 / 36.8 / 21.7 | 36.8 | 20.9 | 220.6 | 99.70 | yes | 0 sw, 426,382 gates |

#### CfRadial 1, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| cfrad.SPOL_20080604_002217.classic.nc | recast | 13.0 / 29.7 / 11.7 | 13.0 | 7.7 | 73.2 | 1.00 |  | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | recast-f32 | 26.3 / 36.1 / 14.6 | 26.3 | 12.6 | 73.0 | 2.03 | no | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | netcdf4 | 51.9 / 65.5 / 41.5 | 51.9 | 33.1 | 92.9 | 4.01 | yes | 0 sw, 8,714,513 gates |
| cfrad.SPOL_20080604_002217.classic.nc | pyart | 93.7 / 144.4 / 79.6 | 93.7 | 75.2 | 310.8 | 7.23 | yes | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | lrose | 180.1 / 187.0 / 97.3 | 180.1 | 91.0 | 89.4 | 13.90 | yes | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | wradlib | 194.7 / 240.9 / 144.7 | 194.7 | 143.5 | 292.1 | 15.02 | yes | 0 sw, 8,713,211 gates |
| cfrad.SPOL_20080604_002217.classic.nc | xradar | 210.1 / 259.4 / 127.4 | 210.1 | 126.3 | 205.3 | 16.21 | yes | 9 sw, 8,699,099 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast-f32 | 1.4 / 1.0 / 0.7 | 1.0 | 0.6 | 9.8 | 0.98 | no | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast | 1.1 / 1.2 / 0.2 | 1.1 | 0.2 | 9.8 | 1.00 |  | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | lrose | 21.4 / 20.4 / 7.1 | 20.4 | 6.9 | 31.8 | 19.09 | yes | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | netcdf4 | 30.8 / 32.7 / 5.4 | 30.8 | 4.7 | 50.9 | 28.85 | yes | 0 sw, 1,605,011 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | pyart | 31.9 / 31.1 / 18.3 | 31.1 | 18.0 | 244.6 | 29.15 | yes | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | xradar | 55.4 / 51.9 / 34.9 | 51.9 | 30.7 | 174.9 | 48.68 | yes | 2 sw, 1,601,235 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | wradlib | 71.3 / 70.1 / 32.0 | 70.1 | 30.3 | 226.7 | 65.70 | yes | 0 sw, 1,604,563 gates |
| cfrad.DOW8_RHI.trim3.nc | recast | 0.2 / 0.3 / 0.2 | 0.2 | 0.1 | 6.2 | 1.00 |  | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | recast-f32 | 0.9 / 0.6 / 0.5 | 0.6 | 0.4 | 6.2 | 2.44 | yes | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | netcdf4 | 18.8 / 7.9 / 6.1 | 7.9 | 4.7 | 46.7 | 31.99 | yes | 0 sw, 426,699 gates |
| cfrad.DOW8_RHI.trim3.nc | lrose | 16.8 / 9.0 / 4.7 | 9.0 | 4.1 | 28.8 | 36.40 | yes | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | pyart | 42.2 / 23.7 / 12.8 | 23.7 | 12.5 | 237.0 | 96.22 | yes | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | xradar | 95.4 / 40.0 / 23.4 | 40.0 | 23.0 | 171.0 | 162.49 | yes | 1 sw, 424,626 gates |
| cfrad.DOW8_RHI.trim3.nc | wradlib | 77.6 / 43.6 / 23.2 | 43.6 | 22.7 | 220.5 | 177.06 | yes | 0 sw, 426,382 gates |

#### CfRadial 1, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| cfrad.SPOL_20080604_002217.classic.nc | netcdf4 | 93.7 | 93.7 | 93.7 | 83.9 | 0.14 |  | 0 sw, 8,714,513 gates |
| cfrad.SPOL_20080604_002217.classic.nc | recast-f32 | 176.4 | 176.4 | 176.4 | 46.3 | 0.26 |  | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | pyart | 229.7 | 229.7 | 229.7 | 306.8 | 0.34 |  | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | lrose | 416.7 | 416.7 | 416.7 | 89.8 | 0.62 |  | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | wradlib | 423.2 | 423.2 | 423.2 | 292.7 | 0.63 |  | 0 sw, 8,713,211 gates |
| cfrad.SPOL_20080604_002217.classic.nc | recast | 671.4 | 671.4 | 671.4 | 46.3 | 1.00 |  | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | xradar | 885.8 | 885.8 | 885.8 | 203.5 | 1.32 |  | 9 sw, 8,699,099 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast-f32 | 4.6 | 4.6 | 4.6 | 7.9 | 0.20 |  | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | netcdf4 | 16.9 | 16.9 | 16.9 | 50.6 | 0.72 |  | 0 sw, 1,605,011 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast | 23.5 | 23.5 | 23.5 | 7.5 | 1.00 |  | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | lrose | 33.7 | 33.7 | 33.7 | 31.0 | 1.44 |  | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | pyart | 55.5 | 55.5 | 55.5 | 244.3 | 2.37 |  | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | wradlib | 65.3 | 65.3 | 65.3 | 226.3 | 2.78 |  | 0 sw, 1,604,563 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | xradar | 667.7 | 667.7 | 667.7 | 174.8 | 28.46 |  | 2 sw, 1,601,235 gates |
| cfrad.DOW8_RHI.trim3.nc | recast-f32 | 4.3 | 4.3 | 4.3 | 5.5 | 0.18 |  | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | netcdf4 | 15.8 | 15.8 | 15.8 | 45.4 | 0.65 |  | 0 sw, 426,699 gates |
| cfrad.DOW8_RHI.trim3.nc | lrose | 18.6 | 18.6 | 18.6 | 27.8 | 0.76 |  | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | recast | 24.4 | 24.4 | 24.4 | 5.5 | 1.00 |  | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | pyart | 33.8 | 33.8 | 33.8 | 235.6 | 1.38 |  | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | wradlib | 83.0 | 83.0 | 83.0 | 219.9 | 3.40 |  | 0 sw, 426,382 gates |
| cfrad.DOW8_RHI.trim3.nc | xradar | 328.4 | 328.4 | 328.4 | 169.9 | 13.44 |  | 1 sw, 424,626 gates |

#### CfRadial 1, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| cfrad.SPOL_20080604_002217.classic.nc | recast | 107.8 | 107.8 | 107.8 | 46.6 | 1.00 |  | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | recast-f32 | 130.4 | 130.4 | 130.4 | 46.5 | 1.21 |  | 9 sw, 8,651,256 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast | 3.9 | 3.9 | 3.9 | 7.8 | 1.00 |  | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast-f32 | 4.0 | 4.0 | 4.0 | 7.6 | 1.03 |  | 2 sw, 1,591,866 gates |
| cfrad.DOW8_RHI.trim3.nc | recast | 2.6 | 2.6 | 2.6 | 5.5 | 1.00 |  | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | recast-f32 | 3.3 | 3.3 | 3.3 | 5.7 | 1.27 |  | 1 sw, 421,800 gates |

#### CfRadial 2, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| cfrad2.SPOL_20080604_002217.nc | netcdf4 | 136.9 / 117.8 | 127.3 | 102.5 | 91.1 |  |  | 0 sw, 8,674,132 gates |
| cfrad2.SPOL_20080604_002217.nc | xradar | fails: exit code -11 | | | | | | |
| cfrad2.SPOL_20080604_002217.nc | pyart | fails: KeyError: 'time' | | | | | | |
| cfrad2.SPOL_20080604_002217.nc | lrose | fails: Loading sweep info | | | | | | |

#### CfRadial 2, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| cfrad2.SPOL_20080604_002217.nc | netcdf4 | 145.8 / 93.1 | 119.5 | 91.0 | 91.2 |  |  | 0 sw, 8,674,132 gates |
| cfrad2.SPOL_20080604_002217.nc | xradar | fails: exit code -11 | | | | | | |
| cfrad2.SPOL_20080604_002217.nc | pyart | fails: KeyError: 'time' | | | | | | |
| cfrad2.SPOL_20080604_002217.nc | lrose | fails: Loading sweep info | | | | | | |

#### CfRadial 2, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| cfrad2.SPOL_20080604_002217.nc | netcdf4 | 238.7 | 238.7 | 238.7 | 87.9 |  |  | 0 sw, 8,674,132 gates |
| cfrad2.SPOL_20080604_002217.nc | xradar | 3217.5 | 3217.5 | 3217.5 | 329.6 |  |  | 9 sw, 8,651,326 gates |
| cfrad2.SPOL_20080604_002217.nc | pyart | fails: KeyError: 'time' | | | | | | |
| cfrad2.SPOL_20080604_002217.nc | lrose | fails: Loading sweep info | | | | | | |

#### DORADE, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.NOXP_20090501_190244_PPI | recast | 0.6 / 0.5 / 0.3 | 0.5 | 0.2 | 7.3 | 1.00 |  | 1 sw, 459,459 gates |
| swp.NOXP_20090501_190244_PPI | recast-f32 | 1.0 / 1.0 / 0.6 | 1.0 | 0.6 | 7.3 | 2.17 | no | 1 sw, 459,459 gates |
| swp.NOXP_20090501_190244_PPI | lrose | 8.7 / 10.2 / 4.8 | 8.7 | 4.4 | 28.0 | 18.58 | yes | 1 sw, 459,459 gates |
| swp.NOXP_20090501_190244_PPI | rsl | fails: exit code -11 | | | | | | |
| swp.NOXP_20090525_203211_SEC | recast | 1.0 / 0.9 / 0.5 | 0.9 | 0.4 | 9.6 | 1.00 |  | 1 sw, 800,800 gates |
| swp.NOXP_20090525_203211_SEC | recast-f32 | 1.7 / 2.1 / 1.1 | 1.7 | 1.0 | 10.1 | 1.99 | yes | 1 sw, 800,800 gates |
| swp.NOXP_20090525_203211_SEC | lrose | 13.7 / 22.1 / 7.8 | 13.7 | 7.5 | 30.8 | 15.83 | yes | 1 sw, 800,800 gates |
| swp.NOXP_20090525_203211_SEC | rsl | fails: exit code -11 | | | | | | |
| swp.DOW6_20211230_RHI.head41 | recast | 1.3 / 1.2 / 0.8 | 1.2 | 0.7 | 12.0 | 1.00 |  | 1 sw, 1,120,000 gates |
| swp.DOW6_20211230_RHI.head41 | recast-f32 | 2.1 / 3.8 / 1.7 | 2.1 | 1.6 | 11.7 | 1.80 | yes | 1 sw, 1,120,000 gates |
| swp.DOW6_20211230_RHI.head41 | rsl | 11.9 / 18.7 / 9.9 | 11.9 | 6.8 | 19.2 | 10.22 | yes | 0 sw, 0 rays |
| swp.DOW6_20211230_RHI.head41 | lrose | 26.5 / 42.4 / 18.9 | 26.5 | 17.3 | 32.0 | 22.75 | yes | 1 sw, 1,312,000 gates |

#### DORADE, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.NOXP_20090501_190244_PPI | recast | 0.4 / 0.4 / 0.3 | 0.4 | 0.2 | 7.0 | 1.00 |  | 1 sw, 459,459 gates |
| swp.NOXP_20090501_190244_PPI | recast-f32 | 0.7 / 0.8 / 0.6 | 0.7 | 0.5 | 7.0 | 1.84 | yes | 1 sw, 459,459 gates |
| swp.NOXP_20090501_190244_PPI | lrose | 8.2 / 9.9 / 6.8 | 8.2 | 4.3 | 28.5 | 23.13 | yes | 1 sw, 459,459 gates |
| swp.NOXP_20090501_190244_PPI | rsl | fails: exit code -11 | | | | | | |
| swp.NOXP_20090525_203211_SEC | recast | 1.0 / 1.3 / 0.6 | 1.0 | 0.4 | 9.6 | 1.00 |  | 1 sw, 800,800 gates |
| swp.NOXP_20090525_203211_SEC | recast-f32 | 1.1 / 1.5 / 1.3 | 1.3 | 1.0 | 9.9 | 1.37 | no | 1 sw, 800,800 gates |
| swp.NOXP_20090525_203211_SEC | lrose | 12.2 / 18.3 / 8.0 | 12.2 | 7.4 | 30.8 | 12.62 | yes | 1 sw, 800,800 gates |
| swp.NOXP_20090525_203211_SEC | rsl | fails: exit code -11 | | | | | | |
| swp.DOW6_20211230_RHI.head41 | recast | 1.2 / 1.4 / 1.6 | 1.4 | 0.9 | 11.7 | 1.00 |  | 1 sw, 1,120,000 gates |
| swp.DOW6_20211230_RHI.head41 | recast-f32 | 2.6 / 4.1 / 1.9 | 2.6 | 1.6 | 11.7 | 1.93 | yes | 1 sw, 1,120,000 gates |
| swp.DOW6_20211230_RHI.head41 | rsl | 11.8 / 19.1 / 8.2 | 11.8 | 7.5 | 18.2 | 8.72 | yes | 0 sw, 0 rays |
| swp.DOW6_20211230_RHI.head41 | lrose | 28.0 / 38.3 / 18.0 | 28.0 | 17.3 | 32.3 | 20.65 | yes | 1 sw, 1,312,000 gates |

#### DORADE, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.NOXP_20090501_190244_PPI | recast-f32 | 4.8 | 4.8 | 4.8 | 5.8 | 0.31 |  | 1 sw, 459,459 gates |
| swp.NOXP_20090501_190244_PPI | recast | 15.4 | 15.4 | 15.4 | 6.0 | 1.00 |  | 1 sw, 459,459 gates |
| swp.NOXP_20090501_190244_PPI | lrose | 35.7 | 35.7 | 35.7 | 27.3 | 2.32 |  | 1 sw, 459,459 gates |
| swp.NOXP_20090501_190244_PPI | rsl | fails: exit code 139 | | | | | | |
| swp.NOXP_20090525_203211_SEC | recast-f32 | 14.8 | 14.8 | 14.8 | 8.0 | 0.60 |  | 1 sw, 800,800 gates |
| swp.NOXP_20090525_203211_SEC | recast | 24.6 | 24.6 | 24.6 | 8.0 | 1.00 |  | 1 sw, 800,800 gates |
| swp.NOXP_20090525_203211_SEC | lrose | 30.5 | 30.5 | 30.5 | 28.8 | 1.24 |  | 1 sw, 800,800 gates |
| swp.NOXP_20090525_203211_SEC | rsl | fails: exit code 139 | | | | | | |
| swp.DOW6_20211230_RHI.head41 | recast-f32 | 11.3 | 11.3 | 11.3 | 9.5 | 0.28 |  | 1 sw, 1,120,000 gates |
| swp.DOW6_20211230_RHI.head41 | recast | 40.8 | 40.8 | 40.8 | 9.5 | 1.00 |  | 1 sw, 1,120,000 gates |
| swp.DOW6_20211230_RHI.head41 | lrose | 46.2 | 46.2 | 46.2 | 30.7 | 1.13 |  | 1 sw, 1,312,000 gates |
| swp.DOW6_20211230_RHI.head41 | rsl | 64.0 | 64.0 | 64.0 | 4.5 | 1.57 |  | 0 sw, 0 rays |

#### DORADE, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.NOXP_20090501_190244_PPI | recast-f32 | 2.8 | 2.8 | 2.8 | 6.0 | 0.59 |  | 1 sw, 459,459 gates |
| swp.NOXP_20090501_190244_PPI | recast | 4.7 | 4.7 | 4.7 | 6.0 | 1.00 |  | 1 sw, 459,459 gates |
| swp.NOXP_20090525_203211_SEC | recast | 5.5 | 5.5 | 5.5 | 8.0 | 1.00 |  | 1 sw, 800,800 gates |
| swp.NOXP_20090525_203211_SEC | recast-f32 | 6.4 | 6.4 | 6.4 | 8.0 | 1.16 |  | 1 sw, 800,800 gates |
| swp.DOW6_20211230_RHI.head41 | recast-f32 | 9.2 | 9.2 | 9.2 | 9.5 | 0.47 |  | 1 sw, 1,120,000 gates |
| swp.DOW6_20211230_RHI.head41 | recast | 19.7 | 19.7 | 19.7 | 9.5 | 1.00 |  | 1 sw, 1,120,000 gates |

#### JMA GRIB2 (one station), pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.RS47773.tar | recast | 5.6 / 6.4 / 4.5 | 5.6 | 4.2 | 62.8 | 1.00 |  | 26 sw, 7,526,400 gates |
| JMA_N5_20191012_0900.RS47773.tar | recast-f32 | 11.3 / 9.9 / 9.1 | 9.9 | 8.5 | 62.8 | 1.76 | yes | 26 sw, 7,526,400 gates |

#### JMA GRIB2 (one station), multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.RS47773.tar | recast | 6.2 / 6.6 / 4.5 | 6.2 | 4.4 | 62.8 | 1.00 |  | 26 sw, 7,526,400 gates |
| JMA_N5_20191012_0900.RS47773.tar | recast-f32 | 11.8 / 9.4 / 9.0 | 9.4 | 8.4 | 62.8 | 1.50 | yes | 26 sw, 7,526,400 gates |

#### JMA GRIB2 (one station), rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.RS47773.tar | recast | 39.6 | 39.6 | 39.6 | 33.8 | 1.00 |  | 26 sw, 7,526,400 gates |
| JMA_N5_20191012_0900.RS47773.tar | recast-f32 | 42.8 | 42.8 | 42.8 | 33.8 | 1.08 |  | 26 sw, 7,526,400 gates |

#### JMA GRIB2 (one station), rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.RS47773.tar | recast | 21.1 | 21.1 | 21.1 | 33.8 | 1.00 |  | 26 sw, 7,526,400 gates |
| JMA_N5_20191012_0900.RS47773.tar | recast-f32 | 28.4 | 28.4 | 28.4 | 33.8 | 1.34 |  | 26 sw, 7,526,400 gates |

#### JMA GRIB2 (20 stations), pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.tar | recast | 158.7 / 255.6 / 115.6 | 158.7 | 102.3 | 1203.7 | 1.00 |  | 520 sw, 150,528,000 gates |
| JMA_N5_20191012_0900.tar | recast-f32 | 292.6 / 301.0 / 197.7 | 292.6 | 178.9 | 1201.7 | 1.84 | no | 520 sw, 150,528,000 gates |

#### JMA GRIB2 (20 stations), multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.tar | recast | 159.8 / 310.2 / 109.2 | 159.8 | 89.4 | 1203.5 | 1.00 |  | 520 sw, 150,528,000 gates |
| JMA_N5_20191012_0900.tar | recast-f32 | 225.4 / 278.3 / 193.9 | 225.4 | 179.2 | 1201.9 | 1.41 | no | 520 sw, 150,528,000 gates |

#### JMA GRIB2 (20 stations), rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.tar | recast-f32 | 1088.4 | 1088.4 | 1088.4 | 622.0 | 0.91 |  | 520 sw, 150,528,000 gates |
| JMA_N5_20191012_0900.tar | recast | 1193.9 | 1193.9 | 1193.9 | 622.0 | 1.00 |  | 520 sw, 150,528,000 gates |

#### JMA GRIB2 (20 stations), rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.tar | recast | 739.9 | 739.9 | 739.9 | 622.0 | 1.00 |  | 520 sw, 150,528,000 gates |
| JMA_N5_20191012_0900.tar | recast-f32 | 991.5 | 991.5 | 991.5 | 622.0 | 1.34 |  | 520 sw, 150,528,000 gates |

### Windows

#### Level II, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20240315_000217_V06 | recast | 400.6 / 531.4 / 422.8 | 422.8 | 377.3 | 173.9 | 1.00 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | recast-paired | 466.9 / 580.8 / 483.8 | 483.8 | 399.4 | 179.0 | 1.14 | no | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | recast-f32 | 518.0 / 558.9 / 520.2 | 520.2 | 462.1 | 173.9 | 1.23 | no | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | nexrad-crate | 930.4 / 1085.2 / 1052.1 | 1052.1 | 899.6 | 197.3 | 2.49 | yes | 20 sw, 11,520 rays |
| KTLX20240315_000217_V06 | radrs | 1240.3 / 1751.7 / 1486.5 | 1486.5 | 1230.7 | 490.7 | 3.52 | yes | 20 sw, 73,517,869 gates |
| KTLX20240315_000217_V06 | metpy | 2509.9 / 2840.6 / 2635.1 | 2635.1 | 2474.3 | 826.1 | 6.23 | yes | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | pyart | 2857.4 / 3344.5 / 3247.2 | 3247.2 | 2783.8 | 1219.0 | 7.68 | yes | 20 sw, 147,732,480 gates |
| KTLX20240315_000217_V06 | xradar | 6064.9 / 6407.8 / 6256.5 | 6256.5 | 6016.3 | 3286.9 | 14.80 | yes | 20 sw, 73,517,865 gates |
| KILX20260418_013553_V06 | recast-paired | 1383.9 / 1475.7 / 1279.3 | 1383.9 | 926.5 | 198.1 | 0.80 | no | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | recast-f32 | 1149.0 / 1422.9 / 2435.3 | 1422.9 | 1011.3 | 192.1 | 0.82 | no | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | recast | 1306.5 / 1886.7 / 1726.2 | 1726.2 | 785.0 | 192.6 | 1.00 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | nexrad-crate | 2624.8 / 2122.2 / 2476.1 | 2476.1 | 1831.5 | 216.9 | 1.43 | yes | 23 sw, 12,600 rays |
| KILX20260418_013553_V06 | radrs | 3032.1 / 2990.2 / 3939.3 | 3032.1 | 2683.9 | 502.2 | 1.76 | yes | 23 sw, 75,464,764 gates |
| KILX20260418_013553_V06 | metpy | 3475.7 / 4524.2 / 3812.3 | 3812.3 | 3439.8 | 851.5 | 2.21 | yes | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | pyart | 4561.7 / 5470.8 / 4428.3 | 4561.7 | 4129.8 | 1290.3 | 2.64 | yes | 23 sw, 161,582,400 gates |
| KILX20260418_013553_V06 | xradar | 8157.6 / 11565.4 / 11004.9 | 11004.9 | 8151.3 | 3380.5 | 6.38 | yes | 23 sw, 75,464,760 gates |
| KTLX20130520_201643_V06.gz | recast | 54.0 / 57.2 / 54.8 | 54.8 | 51.6 | 95.8 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | recast-f32 | 105.1 / 99.8 / 104.0 | 104.0 | 89.4 | 95.9 | 1.90 | yes | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | nexrad-crate | 125.1 / 151.4 / 145.6 | 145.6 | 116.2 | 189.0 | 2.66 | yes | 17 sw, 8,280 rays |
| KTLX20130520_201643_V06.gz | metpy | 1107.1 / 1156.6 / 1268.5 | 1156.6 | 1085.0 | 558.8 | 21.09 | yes | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | pyart | 1174.5 / 1171.0 / 1216.4 | 1174.5 | 1051.8 | 820.7 | 21.42 | yes | 17 sw, 91,013,760 gates |
| KTLX20130520_201643_V06.gz | radrs | fails: ValueError: NEXRAD data error: gzip-compressed volume file must be decompressed before accessing records | | | | | | |
| KTLX20130520_201643_V06.gz | xradar | fails: TypeError: unsupported operand type(s) for +: 'NoneType' and 'int' | | | | | | |
| KLIX20050829_130035.gz | recast | 26.6 / 29.1 / 29.4 | 29.1 | 25.9 | 36.4 | 1.00 |  | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | recast-f32 | 34.3 / 46.8 / 35.8 | 35.8 | 32.0 | 36.0 | 1.23 | yes | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | nexrad-crate | 53.2 / 76.7 / 63.5 | 63.5 | 51.0 | 42.4 | 2.18 | yes | 0 sw, 0 rays |
| KLIX20050829_130035.gz | pyart | 895.1 / 998.3 / 1236.0 | 998.3 | 860.8 | 495.3 | 34.30 | yes | 20 sw, 40,031,040 gates |
| KLIX20050829_130035.gz | metpy | fails: IndexError: tuple index out of range | | | | | | |
| KLIX20050829_130035.gz | xradar | fails: TypeError: unsupported operand type(s) for +: 'NoneType' and 'int' | | | | | | |

#### Level II, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20240315_000217_V06 | recast | 95.2 / 134.0 / 109.0 | 109.0 | 59.4 | 337.7 | 1.00 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | nexrad-crate | 132.0 / 110.7 / 151.5 | 132.0 | 97.4 | 249.0 | 1.21 | no | 20 sw, 11,520 rays |
| KTLX20240315_000217_V06 | recast-paired | 148.8 / 119.7 / 136.4 | 136.4 | 73.2 | 426.3 | 1.25 | no | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | recast-f32 | 273.1 / 216.3 / 182.6 | 216.3 | 142.3 | 339.2 | 1.98 | yes | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | radrs | 413.2 / 384.5 / 618.6 | 413.2 | 346.4 | 504.6 | 3.79 | yes | 20 sw, 73,517,869 gates |
| KTLX20240315_000217_V06 | metpy | 2698.2 / 3267.0 / 3981.8 | 3267.0 | 2685.0 | 829.1 | 29.97 | yes | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | pyart | 3192.5 / 3348.3 / 4258.9 | 3348.3 | 2881.5 | 1202.8 | 30.72 | yes | 20 sw, 147,732,480 gates |
| KTLX20240315_000217_V06 | xradar | 5012.0 / 7534.2 / 6952.9 | 6952.9 | 4641.7 | 3290.4 | 63.79 | yes | 20 sw, 73,517,865 gates |
| KILX20260418_013553_V06 | recast | 144.4 / 183.3 / 176.3 | 176.3 | 91.6 | 396.7 | 1.00 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | recast-paired | 201.0 / 340.0 / 179.6 | 201.0 | 113.6 | 528.7 | 1.14 | no | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | nexrad-crate | 156.0 / 371.1 / 271.8 | 271.8 | 123.9 | 305.8 | 1.54 | no | 23 sw, 12,600 rays |
| KILX20260418_013553_V06 | recast-f32 | 266.1 / 501.0 / 286.2 | 286.2 | 202.6 | 391.7 | 1.62 | yes | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | radrs | 457.8 / 806.1 / 617.4 | 617.4 | 400.8 | 515.1 | 3.50 | yes | 23 sw, 75,464,764 gates |
| KILX20260418_013553_V06 | metpy | 3938.5 / 4690.5 / 4875.1 | 4690.5 | 3913.2 | 855.0 | 26.61 | yes | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | pyart | 5121.1 / 7402.5 / 5838.1 | 5838.1 | 5056.7 | 1315.8 | 33.12 | yes | 23 sw, 161,582,400 gates |
| KILX20260418_013553_V06 | xradar | 7765.5 / 11151.1 / 11842.3 | 11151.1 | 7124.1 | 3383.5 | 63.26 | yes | 23 sw, 75,464,760 gates |
| KTLX20130520_201643_V06.gz | recast | 54.4 / 69.4 / 70.1 | 69.4 | 49.8 | 95.8 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | recast-f32 | 96.0 / 120.5 / 161.9 | 120.5 | 88.5 | 95.8 | 1.74 | yes | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | nexrad-crate | 137.2 / 142.3 / 170.8 | 142.3 | 120.2 | 190.1 | 2.05 | yes | 17 sw, 8,280 rays |
| KTLX20130520_201643_V06.gz | pyart | 983.7 / 1190.6 / 1451.4 | 1190.6 | 936.4 | 823.9 | 17.16 | yes | 17 sw, 91,013,760 gates |
| KTLX20130520_201643_V06.gz | metpy | 1200.9 / 1301.1 / 1436.6 | 1301.1 | 1168.0 | 561.6 | 18.75 | yes | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | radrs | fails: ValueError: NEXRAD data error: gzip-compressed volume file must be decompressed before accessing records | | | | | | |
| KTLX20130520_201643_V06.gz | xradar | fails: TypeError: unsupported operand type(s) for +: 'NoneType' and 'int' | | | | | | |
| KLIX20050829_130035.gz | recast | 30.6 / 37.8 / 47.7 | 37.8 | 26.8 | 35.7 | 1.00 |  | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | recast-f32 | 33.1 / 42.1 / 46.7 | 42.1 | 32.2 | 36.2 | 1.11 | no | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | nexrad-crate | 54.2 / 106.4 / 159.4 | 106.4 | 52.0 | 43.4 | 2.81 | yes | 0 sw, 0 rays |
| KLIX20050829_130035.gz | pyart | 884.1 / 1753.6 / 2393.3 | 1753.6 | 842.9 | 498.4 | 46.39 | yes | 20 sw, 40,031,040 gates |
| KLIX20050829_130035.gz | metpy | fails: IndexError: tuple index out of range | | | | | | |
| KLIX20050829_130035.gz | xradar | fails: TypeError: unsupported operand type(s) for +: 'NoneType' and 'int' | | | | | | |

#### Level II, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20240315_000217_V06 | recast | 404.7 | 404.7 | 404.7 | 96.4 | 1.00 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | recast-f32 | 468.0 | 468.0 | 468.0 | 96.4 | 1.16 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | recast-paired | 529.0 | 529.0 | 529.0 | 101.0 | 1.31 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | nexrad-crate | 900.0 | 900.0 | 900.0 | 107.2 | 2.22 |  | 20 sw, 11,520 rays |
| KTLX20240315_000217_V06 | radrs | 1882.0 | 1882.0 | 1882.0 | 482.7 | 4.65 |  | 20 sw, 73,517,869 gates |
| KTLX20240315_000217_V06 | metpy | 2529.5 | 2529.5 | 2529.5 | 823.2 | 6.25 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | pyart | 2633.1 | 2633.1 | 2633.1 | 1198.4 | 6.51 |  | 20 sw, 147,732,480 gates |
| KTLX20240315_000217_V06 | xradar | 6234.7 | 6234.7 | 6234.7 | 2439.6 | 15.41 |  | 20 sw, 73,517,865 gates |
| KILX20260418_013553_V06 | recast-f32 | 1267.3 | 1267.3 | 1267.3 | 112.0 | 0.98 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | recast | 1298.4 | 1298.4 | 1298.4 | 111.9 | 1.00 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | recast-paired | 1320.8 | 1320.8 | 1320.8 | 117.8 | 1.02 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | nexrad-crate | 2050.4 | 2050.4 | 2050.4 | 122.3 | 1.58 |  | 23 sw, 12,600 rays |
| KILX20260418_013553_V06 | radrs | 3263.5 | 3263.5 | 3263.5 | 493.6 | 2.51 |  | 23 sw, 75,464,764 gates |
| KILX20260418_013553_V06 | metpy | 3668.1 | 3668.1 | 3668.1 | 849.8 | 2.83 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | pyart | 4736.4 | 4736.4 | 4736.4 | 1289.4 | 3.65 |  | 23 sw, 161,582,400 gates |
| KILX20260418_013553_V06 | xradar | 8389.5 | 8389.5 | 8389.5 | 2477.5 | 6.46 |  | 23 sw, 75,464,760 gates |
| KTLX20130520_201643_V06.gz | recast | 49.8 | 49.8 | 49.8 | 54.9 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | recast-f32 | 99.9 | 99.9 | 99.9 | 54.9 | 2.01 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | nexrad-crate | 117.7 | 117.7 | 117.7 | 139.3 | 2.36 |  | 17 sw, 8,280 rays |
| KTLX20130520_201643_V06.gz | pyart | 976.3 | 976.3 | 976.3 | 820.3 | 19.60 |  | 17 sw, 91,013,760 gates |
| KTLX20130520_201643_V06.gz | metpy | 1122.8 | 1122.8 | 1122.8 | 557.8 | 22.54 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | radrs | fails: ValueError: NEXRAD data error: gzip-compressed volume file must be decompressed before accessing records | | | | | | |
| KTLX20130520_201643_V06.gz | xradar | fails: TypeError: unsupported operand type(s) for +: 'NoneType' and 'int' | | | | | | |
| KLIX20050829_130035.gz | recast | 32.0 | 32.0 | 32.0 | 22.0 | 1.00 |  | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | recast-f32 | 40.2 | 40.2 | 40.2 | 21.9 | 1.26 |  | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | nexrad-crate | 75.9 | 75.9 | 75.9 | 41.5 | 2.37 |  | 0 sw, 0 rays |
| KLIX20050829_130035.gz | pyart | 1153.0 | 1153.0 | 1153.0 | 494.6 | 36.05 |  | 20 sw, 40,031,040 gates |
| KLIX20050829_130035.gz | metpy | fails: IndexError: tuple index out of range | | | | | | |
| KLIX20050829_130035.gz | xradar | fails: TypeError: unsupported operand type(s) for +: 'NoneType' and 'int' | | | | | | |

#### Level II, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20240315_000217_V06 | recast | 86.6 | 86.6 | 86.6 | 203.4 | 1.00 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | recast-paired | 101.8 | 101.8 | 101.8 | 214.1 | 1.18 |  | 20 sw, 64,604,160 gates |
| KTLX20240315_000217_V06 | nexrad-crate | 124.5 | 124.5 | 124.5 | 120.6 | 1.44 |  | 20 sw, 11,520 rays |
| KTLX20240315_000217_V06 | recast-f32 | 161.6 | 161.6 | 161.6 | 194.8 | 1.87 |  | 20 sw, 64,604,160 gates |
| KILX20260418_013553_V06 | recast | 141.9 | 141.9 | 141.9 | 229.8 | 1.00 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | recast-paired | 151.9 | 151.9 | 151.9 | 342.6 | 1.07 |  | 23 sw, 66,800,160 gates |
| KILX20260418_013553_V06 | nexrad-crate | 198.3 | 198.3 | 198.3 | 151.9 | 1.40 |  | 23 sw, 12,600 rays |
| KILX20260418_013553_V06 | recast-f32 | 213.7 | 213.7 | 213.7 | 275.6 | 1.51 |  | 23 sw, 66,800,160 gates |
| KTLX20130520_201643_V06.gz | recast | 54.7 | 54.7 | 54.7 | 54.9 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | recast-f32 | 85.3 | 85.3 | 85.3 | 54.9 | 1.56 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.gz | nexrad-crate | 127.7 | 127.7 | 127.7 | 140.3 | 2.34 |  | 17 sw, 8,280 rays |
| KLIX20050829_130035.gz | recast | 32.3 | 32.3 | 32.3 | 21.9 | 1.00 |  | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | recast-f32 | 42.9 | 42.9 | 42.9 | 22.0 | 1.33 |  | 20 sw, 9,808,200 gates |
| KLIX20050829_130035.gz | nexrad-crate | 102.6 | 102.6 | 102.6 | 41.8 | 3.18 |  | 0 sw, 0 rays |

#### Level II real-time chunks, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KIWA307_chunks001-003 | recast-f32 | 34.2 / 24.3 / 25.6 | 25.6 | 21.5 | 15.5 | 0.97 | no | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | recast | 35.6 / 26.3 / 24.1 | 26.3 | 19.0 | 14.6 | 1.00 |  | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | nexrad-crate | 42.8 / 44.6 / 39.6 | 42.8 | 38.1 | 15.8 | 1.63 | yes | 1 sw, 240 rays |
| KIWA307_chunks001-003 | pyart | 69.1 / 64.3 / 64.2 | 64.3 | 60.9 | 225.7 | 2.44 | yes | 1 sw, 2,198,400 gates |
| KIWA307_chunks001-003 | radrs | 75.5 / 62.3 / 67.7 | 67.7 | 58.3 | 129.1 | 2.57 | yes | 1 sw, 2,198,414 gates |
| KIWA307_chunks001-003 | xradar | 73.7 / 87.3 / 66.2 | 73.7 | 60.7 | 140.2 | 2.80 | yes | 0 sw, 0 rays |
| KIWA307_chunks001-003 | metpy | 78.0 / 72.9 / 76.4 | 76.4 | 69.1 | 199.0 | 2.90 | yes | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-035 | recast | 250.5 / 278.3 / 354.3 | 278.3 | 234.8 | 68.8 | 1.00 |  | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | recast-f32 | 284.7 / 362.1 / 420.6 | 362.1 | 273.0 | 69.0 | 1.30 | no | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | nexrad-crate | 526.9 / 555.4 / 537.7 | 537.7 | 502.2 | 75.8 | 1.93 | yes | 6 sw, 4,080 rays |
| KIWA307_chunks001-035 | radrs | 649.3 / 723.6 / 787.0 | 723.6 | 630.0 | 249.7 | 2.60 | yes | 6 sw, 26,219,559 gates |
| KIWA307_chunks001-035 | metpy | 991.2 / 1020.9 / 1104.5 | 1020.9 | 946.4 | 401.5 | 3.67 | yes | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | pyart | 1043.4 / 1107.4 / 1295.9 | 1107.4 | 1020.7 | 560.7 | 3.98 | yes | 6 sw, 52,321,920 gates |
| KIWA307_chunks001-035 | xradar | 2217.1 / 2084.3 / 2371.5 | 2217.1 | 2057.4 | 679.2 | 7.97 | yes | 5 sw, 24,503,070 gates |

#### Level II real-time chunks, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KIWA307_chunks001-003 | recast | 20.3 / 12.9 / 19.8 | 19.8 | 11.4 | 38.6 | 1.00 |  | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | recast-f32 | 23.3 / 20.2 / 20.1 | 20.2 | 16.5 | 30.3 | 1.02 | no | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | nexrad-crate | 35.3 / 27.0 / 45.1 | 35.3 | 25.0 | 17.3 | 1.79 | yes | 1 sw, 240 rays |
| KIWA307_chunks001-003 | radrs | 76.4 / 66.2 / 63.1 | 66.2 | 51.0 | 131.9 | 3.35 | yes | 1 sw, 2,198,414 gates |
| KIWA307_chunks001-003 | xradar | 140.4 / 66.3 / 65.6 | 66.3 | 64.6 | 143.5 | 3.35 | yes | 0 sw, 0 rays |
| KIWA307_chunks001-003 | pyart | 78.4 / 72.0 / 82.8 | 78.4 | 68.5 | 229.6 | 3.96 | yes | 1 sw, 2,198,400 gates |
| KIWA307_chunks001-003 | metpy | 94.2 / 73.1 / 82.6 | 82.6 | 69.0 | 201.9 | 4.18 | yes | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-035 | recast | 53.6 / 43.6 / 92.4 | 53.6 | 32.8 | 215.2 | 1.00 |  | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | recast-f32 | 83.2 / 121.6 / 113.7 | 113.7 | 65.5 | 216.2 | 2.12 | no | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | nexrad-crate | 112.6 / 125.3 / 152.5 | 125.3 | 68.6 | 123.4 | 2.34 | yes | 6 sw, 4,080 rays |
| KIWA307_chunks001-035 | radrs | 226.9 / 335.3 / 199.8 | 226.9 | 176.7 | 260.6 | 4.24 | yes | 6 sw, 26,219,559 gates |
| KIWA307_chunks001-035 | pyart | 1259.1 / 1437.5 / 1216.7 | 1259.1 | 1176.7 | 557.3 | 23.51 | yes | 6 sw, 52,321,920 gates |
| KIWA307_chunks001-035 | metpy | 1313.0 / 1171.6 / 1261.9 | 1261.9 | 1130.4 | 405.4 | 23.57 | yes | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | xradar | 2506.8 / 2805.3 / 2516.8 | 2516.8 | 2292.9 | 684.6 | 47.00 | yes | 5 sw, 24,503,070 gates |

#### Level II real-time chunks, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KIWA307_chunks001-003 | nexrad-crate | 41.6 | 41.6 | 41.6 | 10.5 | 0.86 |  | 1 sw, 240 rays |
| KIWA307_chunks001-003 | recast | 48.1 | 48.1 | 48.1 | 11.9 | 1.00 |  | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | pyart | 64.4 | 64.4 | 64.4 | 224.7 | 1.34 |  | 1 sw, 2,198,400 gates |
| KIWA307_chunks001-003 | metpy | 74.0 | 74.0 | 74.0 | 198.1 | 1.54 |  | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | xradar | 76.8 | 76.8 | 76.8 | 138.2 | 1.60 |  | 0 sw, 0 rays |
| KIWA307_chunks001-003 | recast-f32 | 80.9 | 80.9 | 80.9 | 13.6 | 1.68 |  | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | radrs | 752.3 | 752.3 | 752.3 | 126.7 | 15.63 |  | 1 sw, 2,198,414 gates |
| KIWA307_chunks001-035 | recast | 252.0 | 252.0 | 252.0 | 42.4 | 1.00 |  | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | recast-f32 | 298.3 | 298.3 | 298.3 | 42.3 | 1.18 |  | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | nexrad-crate | 513.1 | 513.1 | 513.1 | 43.5 | 2.04 |  | 6 sw, 4,080 rays |
| KIWA307_chunks001-035 | metpy | 1006.9 | 1006.9 | 1006.9 | 400.5 | 4.00 |  | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | pyart | 1108.3 | 1108.3 | 1108.3 | 560.9 | 4.40 |  | 6 sw, 52,321,920 gates |
| KIWA307_chunks001-035 | radrs | 1333.0 | 1333.0 | 1333.0 | 245.7 | 5.29 |  | 6 sw, 26,219,559 gates |
| KIWA307_chunks001-035 | xradar | 2595.6 | 2595.6 | 2595.6 | 430.8 | 10.30 |  | 5 sw, 24,503,070 gates |

#### Level II real-time chunks, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KIWA307_chunks001-003 | nexrad-crate | 33.3 | 33.3 | 33.3 | 11.3 | 0.39 |  | 1 sw, 240 rays |
| KIWA307_chunks001-003 | recast | 84.9 | 84.9 | 84.9 | 12.6 | 1.00 |  | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-003 | recast-f32 | 85.6 | 85.6 | 85.6 | 16.6 | 1.01 |  | 1 sw, 1,737,600 gates |
| KIWA307_chunks001-035 | recast | 77.4 | 77.4 | 77.4 | 112.1 | 1.00 |  | 6 sw, 22,331,520 gates |
| KIWA307_chunks001-035 | nexrad-crate | 88.1 | 88.1 | 88.1 | 59.6 | 1.14 |  | 6 sw, 4,080 rays |
| KIWA307_chunks001-035 | recast-f32 | 110.0 | 110.0 | 110.0 | 120.5 | 1.42 |  | 6 sw, 22,331,520 gates |

#### Level III, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_N0B_20260622_0806 | recast | 11.2 / 12.0 / 12.1 | 12.0 | 9.4 | 11.2 | 1.00 |  | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | recast-f32 | 12.7 / 17.2 / 14.6 | 14.6 | 11.0 | 15.0 | 1.22 | yes | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | metpy | 26.5 / 27.5 / 27.4 | 27.4 | 23.9 | 186.2 | 2.29 | yes | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | pyart | 36.7 / 43.4 / 45.7 | 43.4 | 32.0 | 229.6 | 3.63 | yes | 1 sw, 1,324,800 gates |
| TLX_N0Q_20130520_2016 | recast | 0.8 / 0.8 / 0.8 | 0.8 | 0.6 | 5.4 | 1.00 |  | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | recast-f32 | 0.9 / 1.1 / 1.1 | 1.1 | 0.7 | 6.1 | 1.34 | yes | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | metpy | 2.6 / 2.7 / 2.7 | 2.7 | 2.4 | 180.9 | 3.43 | yes | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | pyart | 4.8 / 5.1 / 4.8 | 4.8 | 4.0 | 211.1 | 6.11 | yes | 1 sw, 165,600 gates |
| TLX_N0U_20220503_0052 | recast | 2.1 / 2.3 / 2.3 | 2.3 | 1.9 | 7.2 | 1.00 |  | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | recast-f32 | 2.7 / 2.8 / 3.2 | 2.8 | 2.3 | 8.5 | 1.24 | yes | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | metpy | 6.2 / 6.2 / 6.9 | 6.2 | 5.6 | 182.8 | 2.77 | yes | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | pyart | 10.3 / 10.0 / 11.6 | 10.3 | 9.0 | 215.4 | 4.59 | yes | 1 sw, 432,000 gates |
| TLX_DPR_20260622_0806 | recast-f32 | 25.9 / 15.9 / 41.3 | 25.9 | 11.4 | 13.3 | 0.57 | no | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | metpy | 38.8 / 34.7 / 43.8 | 38.8 | 31.1 | 191.4 | 0.85 | no | 1 sw, 0 rays |
| TLX_DPR_20260622_0806 | recast | 24.0 / 47.2 / 45.5 | 45.5 | 12.3 | 13.0 | 1.00 |  | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | pyart | 115.4 / 110.9 / 124.1 | 115.4 | 99.8 | 220.2 | 2.54 | yes | 1 sw, 331,200 gates |

#### Level III, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_N0B_20260622_0806 | recast | 10.5 / 14.2 / 12.6 | 12.6 | 8.9 | 11.1 | 1.00 |  | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | recast-f32 | 14.5 / 15.1 / 17.0 | 15.1 | 10.5 | 14.9 | 1.20 | yes | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | metpy | 26.4 / 31.4 / 30.5 | 30.5 | 20.9 | 190.3 | 2.42 | yes | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | pyart | 34.8 / 47.4 / 47.2 | 47.2 | 28.5 | 233.6 | 3.76 | yes | 1 sw, 1,324,800 gates |
| TLX_N0Q_20130520_2016 | recast | 0.8 / 0.9 / 0.7 | 0.8 | 0.6 | 5.4 | 1.00 |  | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | recast-f32 | 1.0 / 1.0 / 1.0 | 1.0 | 0.8 | 6.1 | 1.29 | yes | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | metpy | 2.6 / 2.9 / 2.9 | 2.9 | 2.3 | 184.5 | 3.72 | yes | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | pyart | 4.7 / 5.6 / 5.8 | 5.6 | 4.1 | 214.5 | 7.08 | yes | 1 sw, 165,600 gates |
| TLX_N0U_20220503_0052 | recast | 2.2 / 2.4 / 2.7 | 2.4 | 1.9 | 7.3 | 1.00 |  | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | recast-f32 | 2.6 / 3.2 / 3.8 | 3.2 | 1.9 | 8.9 | 1.33 | no | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | metpy | 6.7 / 6.7 / 8.2 | 6.7 | 5.6 | 186.1 | 2.81 | yes | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | pyart | 10.7 / 11.5 / 15.1 | 11.5 | 8.9 | 219.2 | 4.81 | yes | 1 sw, 432,000 gates |
| TLX_DPR_20260622_0806 | recast-f32 | 29.7 / 35.2 / 19.5 | 29.7 | 12.5 | 13.3 | 0.95 | no | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | recast | 15.4 / 31.3 / 59.7 | 31.3 | 11.0 | 13.1 | 1.00 |  | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | metpy | 39.4 / 56.7 / 49.6 | 49.6 | 32.2 | 195.1 | 1.58 | no | 1 sw, 0 rays |
| TLX_DPR_20260622_0806 | pyart | 136.3 / 145.9 / 151.1 | 145.9 | 107.8 | 223.7 | 4.66 | yes | 1 sw, 331,200 gates |

#### Level III, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_N0B_20260622_0806 | recast | 12.1 | 12.1 | 12.1 | 9.8 | 1.00 |  | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | recast-f32 | 13.7 | 13.7 | 13.7 | 13.6 | 1.12 |  | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | metpy | 34.4 | 34.4 | 34.4 | 185.0 | 2.83 |  | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | pyart | 38.3 | 38.3 | 38.3 | 229.5 | 3.16 |  | 1 sw, 1,324,800 gates |
| TLX_N0Q_20130520_2016 | recast | 1.2 | 1.2 | 1.2 | 5.1 | 1.00 |  | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | recast-f32 | 1.3 | 1.3 | 1.3 | 5.4 | 1.09 |  | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | metpy | 3.5 | 3.5 | 3.5 | 180.9 | 2.93 |  | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | pyart | 7.3 | 7.3 | 7.3 | 209.8 | 6.06 |  | 1 sw, 165,600 gates |
| TLX_N0U_20220503_0052 | recast | 2.3 | 2.3 | 2.3 | 5.9 | 1.00 |  | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | recast-f32 | 3.3 | 3.3 | 3.3 | 7.1 | 1.39 |  | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | metpy | 6.6 | 6.6 | 6.6 | 181.9 | 2.82 |  | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | pyart | 12.8 | 12.8 | 12.8 | 214.7 | 5.47 |  | 1 sw, 432,000 gates |
| TLX_DPR_20260622_0806 | metpy | 38.6 | 38.6 | 38.6 | 190.3 | 0.60 |  | 1 sw, 0 rays |
| TLX_DPR_20260622_0806 | recast-f32 | 42.5 | 42.5 | 42.5 | 11.0 | 0.66 |  | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | recast | 64.2 | 64.2 | 64.2 | 11.1 | 1.00 |  | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | pyart | 148.0 | 148.0 | 148.0 | 216.2 | 2.31 |  | 1 sw, 331,200 gates |

#### Level III, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_N0B_20260622_0806 | recast | 13.9 | 13.9 | 13.9 | 9.8 | 1.00 |  | 1 sw, 1,324,800 gates |
| TLX_N0B_20260622_0806 | recast-f32 | 16.0 | 16.0 | 16.0 | 13.6 | 1.15 |  | 1 sw, 1,324,800 gates |
| TLX_N0Q_20130520_2016 | recast-f32 | 1.3 | 1.3 | 1.3 | 5.4 | 0.92 |  | 1 sw, 165,600 gates |
| TLX_N0Q_20130520_2016 | recast | 1.4 | 1.4 | 1.4 | 5.0 | 1.00 |  | 1 sw, 165,600 gates |
| TLX_N0U_20220503_0052 | recast | 2.8 | 2.8 | 2.8 | 5.9 | 1.00 |  | 1 sw, 432,000 gates |
| TLX_N0U_20220503_0052 | recast-f32 | 3.1 | 3.1 | 3.1 | 7.2 | 1.08 |  | 1 sw, 432,000 gates |
| TLX_DPR_20260622_0806 | recast-f32 | 18.7 | 18.7 | 18.7 | 11.1 | 0.52 |  | 1 sw, 331,200 gates |
| TLX_DPR_20260622_0806 | recast | 35.7 | 35.7 | 35.7 | 11.1 | 1.00 |  | 1 sw, 331,200 gates |

#### ODIM_H5, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| iesha.pvol.20260305T0115.h5 | recast | 9.6 / 8.8 / 9.5 | 9.5 | 7.9 | 15.6 | 1.00 |  | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | recast-f32 | 15.3 / 14.1 / 13.7 | 14.1 | 10.3 | 15.7 | 1.48 | yes | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | h5py | 19.5 / 18.1 / 20.2 | 19.5 | 16.8 | 44.6 | 2.05 | yes | 0 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | wradlib | 39.3 / 40.0 / 42.9 | 40.0 | 38.5 | 199.9 | 4.20 | yes | 0 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | pyart | 89.8 / 77.9 / 109.7 | 89.8 | 73.3 | 241.4 | 9.43 | yes | 10 sw, 5,367,600 gates |
| iesha.pvol.20260305T0115.h5 | xradar | 240.2 / 220.2 / 247.6 | 240.2 | 214.8 | 262.6 | 25.22 | yes | 10 sw, 4,343,845 gates |
| dkrom.pvol.20260820T1130.h5 | recast | 18.2 / 17.9 / 16.7 | 17.9 | 16.1 | 34.1 | 1.00 |  | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | recast-f32 | 21.8 / 21.6 / 21.3 | 21.6 | 20.3 | 34.1 | 1.21 | yes | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | h5py | 65.9 / 57.9 / 62.2 | 62.2 | 54.9 | 47.8 | 3.48 | yes | 0 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | wradlib | 98.7 / 87.8 / 115.3 | 98.7 | 85.9 | 212.5 | 5.51 | yes | 0 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | pyart | 348.1 / 312.9 / 350.2 | 348.1 | 306.0 | 283.5 | 19.45 | yes | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | xradar | 456.7 / 451.8 / 535.1 | 456.7 | 426.9 | 339.6 | 25.52 | yes | 10 sw, 13,651,285 gates |
| bejab.pvol.hdf | recast | 5.6 / 5.1 / 5.1 | 5.1 | 4.7 | 9.4 | 1.00 |  | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | recast-f32 | 6.5 / 6.7 / 7.3 | 6.7 | 5.4 | 9.6 | 1.32 | yes | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | h5py | 10.2 / 9.5 / 10.2 | 10.2 | 9.2 | 43.2 | 2.01 | yes | 0 sw, 1,831,680 gates |
| bejab.pvol.hdf | wradlib | 18.0 / 16.7 / 17.5 | 17.5 | 15.6 | 196.3 | 3.45 | yes | 0 sw, 1,831,680 gates |
| bejab.pvol.hdf | pyart | 40.8 / 41.9 / 43.8 | 41.9 | 38.9 | 224.8 | 8.24 | yes | 11 sw, 2,368,080 gates |
| bejab.pvol.hdf | xradar | 174.2 / 161.6 / 162.8 | 162.8 | 153.4 | 241.1 | 32.05 | yes | 11 sw, 1,831,773 gates |

#### ODIM_H5, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| iesha.pvol.20260305T0115.h5 | recast | 10.8 / 10.2 / 9.3 | 10.2 | 8.6 | 15.6 | 1.00 |  | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | recast-f32 | 17.8 / 15.9 / 13.2 | 15.9 | 9.7 | 15.7 | 1.56 | yes | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | h5py | 18.9 / 23.1 / 22.6 | 22.6 | 16.1 | 46.4 | 2.22 | yes | 0 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | wradlib | 47.1 / 60.4 / 52.1 | 52.1 | 41.1 | 203.5 | 5.12 | yes | 0 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | pyart | 103.7 / 112.9 / 120.9 | 112.9 | 89.6 | 243.9 | 11.10 | yes | 10 sw, 5,367,600 gates |
| iesha.pvol.20260305T0115.h5 | xradar | 259.1 / 265.2 / 289.9 | 265.2 | 235.9 | 266.3 | 26.06 | yes | 10 sw, 4,343,845 gates |
| dkrom.pvol.20260820T1130.h5 | recast | 17.6 / 18.8 / 19.0 | 18.8 | 16.0 | 34.1 | 1.00 |  | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | recast-f32 | 24.5 / 22.8 / 22.7 | 22.8 | 20.7 | 33.9 | 1.21 | yes | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | h5py | 63.4 / 70.6 / 73.4 | 70.6 | 56.9 | 49.9 | 3.75 | yes | 0 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | wradlib | 113.6 / 138.1 / 183.4 | 138.1 | 110.3 | 216.0 | 7.33 | yes | 0 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | pyart | 351.7 / 361.0 / 373.3 | 361.0 | 285.9 | 287.6 | 19.17 | yes | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | xradar | 615.0 / 627.2 / 705.7 | 627.2 | 557.0 | 343.0 | 33.30 | yes | 10 sw, 13,651,285 gates |
| bejab.pvol.hdf | recast | 6.2 / 6.3 / 6.0 | 6.2 | 4.7 | 9.4 | 1.00 |  | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | recast-f32 | 6.0 / 8.2 / 9.0 | 8.2 | 5.3 | 9.6 | 1.32 | no | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | h5py | 10.2 / 10.4 / 10.9 | 10.4 | 8.8 | 44.9 | 1.67 | yes | 0 sw, 1,831,680 gates |
| bejab.pvol.hdf | wradlib | 26.2 / 26.2 / 26.5 | 26.2 | 17.6 | 199.5 | 4.21 | yes | 0 sw, 1,831,680 gates |
| bejab.pvol.hdf | pyart | 49.7 / 48.7 / 61.7 | 49.7 | 37.3 | 228.7 | 7.99 | yes | 11 sw, 2,368,080 gates |
| bejab.pvol.hdf | xradar | 182.6 / 182.2 / 182.4 | 182.4 | 160.6 | 245.0 | 29.35 | yes | 11 sw, 1,831,773 gates |

#### ODIM_H5, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| iesha.pvol.20260305T0115.h5 | recast | 15.6 | 15.6 | 15.6 | 10.6 | 1.00 |  | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | recast-f32 | 16.9 | 16.9 | 16.9 | 10.7 | 1.09 |  | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | h5py | 22.8 | 22.8 | 22.8 | 43.5 | 1.47 |  | 0 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | wradlib | 53.6 | 53.6 | 53.6 | 199.4 | 3.45 |  | 0 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | pyart | 104.4 | 104.4 | 104.4 | 239.8 | 6.71 |  | 10 sw, 5,367,600 gates |
| iesha.pvol.20260305T0115.h5 | xradar | 2035.2 | 2035.2 | 2035.2 | 260.3 | 130.82 |  | 10 sw, 4,343,845 gates |
| dkrom.pvol.20260820T1130.h5 | recast | 21.8 | 21.8 | 21.8 | 19.8 | 1.00 |  | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | recast-f32 | 26.0 | 26.0 | 26.0 | 19.7 | 1.19 |  | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | h5py | 63.9 | 63.9 | 63.9 | 46.4 | 2.93 |  | 0 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | wradlib | 109.9 | 109.9 | 109.9 | 211.5 | 5.03 |  | 0 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | pyart | 356.6 | 356.6 | 356.6 | 282.3 | 16.32 |  | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | xradar | 2455.6 | 2455.6 | 2455.6 | 336.9 | 112.42 |  | 10 sw, 13,651,285 gates |
| bejab.pvol.hdf | recast | 6.1 | 6.1 | 6.1 | 7.1 | 1.00 |  | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | recast-f32 | 7.1 | 7.1 | 7.1 | 7.6 | 1.17 |  | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | h5py | 10.4 | 10.4 | 10.4 | 42.6 | 1.70 |  | 0 sw, 1,831,680 gates |
| bejab.pvol.hdf | wradlib | 20.0 | 20.0 | 20.0 | 196.2 | 3.28 |  | 0 sw, 1,831,680 gates |
| bejab.pvol.hdf | pyart | 42.6 | 42.6 | 42.6 | 224.8 | 6.97 |  | 11 sw, 2,368,080 gates |
| bejab.pvol.hdf | xradar | 1892.0 | 1892.0 | 1892.0 | 239.6 | 309.86 |  | 11 sw, 1,831,773 gates |

#### ODIM_H5, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| iesha.pvol.20260305T0115.h5 | recast | 12.1 | 12.1 | 12.1 | 10.7 | 1.00 |  | 10 sw, 4,343,760 gates |
| iesha.pvol.20260305T0115.h5 | recast-f32 | 17.4 | 17.4 | 17.4 | 10.7 | 1.44 |  | 10 sw, 4,343,760 gates |
| dkrom.pvol.20260820T1130.h5 | recast | 22.9 | 22.9 | 22.9 | 19.7 | 1.00 |  | 10 sw, 13,651,200 gates |
| dkrom.pvol.20260820T1130.h5 | recast-f32 | 30.5 | 30.5 | 30.5 | 19.8 | 1.33 |  | 10 sw, 13,651,200 gates |
| bejab.pvol.hdf | recast-f32 | 6.4 | 6.4 | 6.4 | 7.6 | 0.98 |  | 11 sw, 1,831,680 gates |
| bejab.pvol.hdf | recast | 6.6 | 6.6 | 6.6 | 7.1 | 1.00 |  | 11 sw, 1,831,680 gates |

#### CfRadial 1, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| cfrad.SPOL_20080604_002217.classic.nc | recast | 29.1 / 31.6 / 26.3 | 29.1 | 22.4 | 64.6 | 1.00 |  | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | recast-f32 | 29.8 / 45.9 / 40.5 | 40.5 | 28.1 | 63.8 | 1.39 | no | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | netcdf4 | 47.1 / 61.5 / 61.3 | 61.3 | 46.5 | 79.3 | 2.10 | yes | 0 sw, 8,714,513 gates |
| cfrad.SPOL_20080604_002217.classic.nc | pyart | 138.0 / 135.2 / 121.3 | 135.2 | 117.4 | 282.9 | 4.64 | yes | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | xradar | 179.6 / 181.1 / 199.2 | 181.1 | 177.6 | 186.8 | 6.22 | yes | 9 sw, 8,699,099 gates |
| cfrad.SPOL_20080604_002217.classic.nc | wradlib | 243.0 / 277.2 / 264.1 | 264.1 | 229.8 | 267.3 | 9.07 | yes | 0 sw, 8,713,211 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast | 2.0 / 2.3 / 2.5 | 2.3 | 1.8 | 10.9 | 1.00 |  | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast-f32 | 3.9 / 4.8 / 4.9 | 4.8 | 3.1 | 10.9 | 2.09 | yes | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | netcdf4 | 13.4 / 15.9 / 15.4 | 15.4 | 12.8 | 44.8 | 6.69 | yes | 0 sw, 1,605,011 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | pyart | 30.2 / 29.0 / 33.6 | 30.2 | 25.8 | 220.1 | 13.13 | yes | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | wradlib | 49.5 / 46.6 / 54.2 | 49.5 | 44.9 | 205.4 | 21.55 | yes | 0 sw, 1,604,563 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | xradar | 51.1 / 47.0 / 53.6 | 51.1 | 46.9 | 156.8 | 22.23 | yes | 2 sw, 1,601,235 gates |
| cfrad.DOW8_RHI.trim3.nc | recast | 1.3 / 1.4 / 1.3 | 1.3 | 1.0 | 7.4 | 1.00 |  | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | recast-f32 | 2.0 / 2.5 / 2.0 | 2.0 | 1.9 | 7.9 | 1.52 | yes | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | netcdf4 | 11.7 / 10.6 / 12.0 | 11.7 | 10.5 | 42.9 | 8.68 | yes | 0 sw, 426,699 gates |
| cfrad.DOW8_RHI.trim3.nc | pyart | 22.5 / 24.4 / 25.2 | 24.4 | 21.9 | 215.7 | 18.13 | yes | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | wradlib | 36.8 / 37.1 / 41.7 | 37.1 | 35.3 | 199.9 | 27.52 | yes | 0 sw, 426,382 gates |
| cfrad.DOW8_RHI.trim3.nc | xradar | 37.9 / 41.6 / 43.9 | 41.6 | 37.6 | 150.4 | 30.89 | yes | 1 sw, 424,626 gates |

#### CfRadial 1, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| cfrad.SPOL_20080604_002217.classic.nc | recast | 22.2 / 34.4 / 34.0 | 34.0 | 19.8 | 64.6 | 1.00 |  | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | recast-f32 | 35.9 / 41.9 / 57.7 | 41.9 | 32.0 | 63.8 | 1.23 | yes | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | netcdf4 | 67.3 / 66.4 / 63.7 | 66.4 | 54.1 | 81.0 | 1.95 | yes | 0 sw, 8,714,513 gates |
| cfrad.SPOL_20080604_002217.classic.nc | pyart | 133.3 / 132.7 / 164.9 | 133.3 | 119.9 | 286.1 | 3.92 | yes | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | xradar | 226.8 / 182.3 / 195.6 | 195.6 | 181.9 | 189.8 | 5.75 | yes | 9 sw, 8,699,099 gates |
| cfrad.SPOL_20080604_002217.classic.nc | wradlib | 275.2 / 291.3 / 335.7 | 291.3 | 241.9 | 270.4 | 8.57 | yes | 0 sw, 8,713,211 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast | 1.9 / 3.3 / 2.6 | 2.6 | 1.6 | 10.9 | 1.00 |  | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast-f32 | 4.8 / 3.9 / 5.3 | 4.8 | 3.4 | 10.9 | 1.83 | yes | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | netcdf4 | 15.8 / 17.7 / 19.8 | 17.7 | 14.1 | 46.5 | 6.69 | yes | 0 sw, 1,605,011 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | pyart | 31.6 / 28.4 / 33.1 | 31.6 | 27.5 | 224.1 | 11.93 | yes | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | wradlib | 48.4 / 51.6 / 74.4 | 51.6 | 42.5 | 209.0 | 19.49 | yes | 0 sw, 1,604,563 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | xradar | 58.0 / 63.0 / 57.1 | 58.0 | 47.1 | 159.3 | 21.88 | yes | 2 sw, 1,601,235 gates |
| cfrad.DOW8_RHI.trim3.nc | recast | 1.1 / 1.6 / 1.5 | 1.5 | 1.0 | 7.4 | 1.00 |  | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | recast-f32 | 2.0 / 2.2 / 2.6 | 2.2 | 1.7 | 7.4 | 1.45 | yes | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | netcdf4 | 10.5 / 12.4 / 14.2 | 12.4 | 8.9 | 44.0 | 8.17 | yes | 0 sw, 426,699 gates |
| cfrad.DOW8_RHI.trim3.nc | pyart | 26.4 / 30.5 / 34.1 | 30.5 | 23.1 | 218.8 | 20.06 | yes | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | xradar | 41.3 / 50.0 / 52.0 | 50.0 | 38.4 | 153.5 | 32.83 | yes | 1 sw, 424,626 gates |
| cfrad.DOW8_RHI.trim3.nc | wradlib | 43.3 / 58.9 / 51.1 | 51.1 | 37.8 | 203.8 | 33.60 | yes | 0 sw, 426,382 gates |

#### CfRadial 1, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| cfrad.SPOL_20080604_002217.classic.nc | recast | 26.1 | 26.1 | 26.1 | 46.7 | 1.00 |  | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | recast-f32 | 41.6 | 41.6 | 41.6 | 46.7 | 1.59 |  | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | netcdf4 | 63.9 | 63.9 | 63.9 | 79.2 | 2.45 |  | 0 sw, 8,714,513 gates |
| cfrad.SPOL_20080604_002217.classic.nc | pyart | 133.1 | 133.1 | 133.1 | 281.9 | 5.09 |  | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | wradlib | 237.8 | 237.8 | 237.8 | 267.1 | 9.10 |  | 0 sw, 8,713,211 gates |
| cfrad.SPOL_20080604_002217.classic.nc | xradar | 547.3 | 547.3 | 547.3 | 184.1 | 20.94 |  | 9 sw, 8,699,099 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast | 2.0 | 2.0 | 2.0 | 8.5 | 1.00 |  | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast-f32 | 4.5 | 4.5 | 4.5 | 8.5 | 2.28 |  | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | netcdf4 | 14.2 | 14.2 | 14.2 | 44.8 | 7.19 |  | 0 sw, 1,605,011 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | pyart | 28.6 | 28.6 | 28.6 | 219.8 | 14.51 |  | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | wradlib | 53.7 | 53.7 | 53.7 | 204.7 | 27.29 |  | 0 sw, 1,604,563 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | xradar | 407.8 | 407.8 | 407.8 | 155.0 | 207.10 |  | 2 sw, 1,601,235 gates |
| cfrad.DOW8_RHI.trim3.nc | recast | 1.6 | 1.6 | 1.6 | 6.4 | 1.00 |  | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | recast-f32 | 2.0 | 2.0 | 2.0 | 6.5 | 1.20 |  | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | netcdf4 | 10.5 | 10.5 | 10.5 | 41.8 | 6.45 |  | 0 sw, 426,699 gates |
| cfrad.DOW8_RHI.trim3.nc | pyart | 27.7 | 27.7 | 27.7 | 212.0 | 17.02 |  | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | wradlib | 51.9 | 51.9 | 51.9 | 196.4 | 31.86 |  | 0 sw, 426,382 gates |
| cfrad.DOW8_RHI.trim3.nc | xradar | 392.3 | 392.3 | 392.3 | 149.2 | 240.85 |  | 1 sw, 424,626 gates |

#### CfRadial 1, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| cfrad.SPOL_20080604_002217.classic.nc | recast | 26.2 | 26.2 | 26.2 | 46.7 | 1.00 |  | 9 sw, 8,651,256 gates |
| cfrad.SPOL_20080604_002217.classic.nc | recast-f32 | 42.7 | 42.7 | 42.7 | 46.7 | 1.63 |  | 9 sw, 8,651,256 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast | 2.4 | 2.4 | 2.4 | 8.5 | 1.00 |  | 2 sw, 1,591,866 gates |
| cfrad.IRENE_CPOL_sweeps0-1.nc | recast-f32 | 4.2 | 4.2 | 4.2 | 8.5 | 1.70 |  | 2 sw, 1,591,866 gates |
| cfrad.DOW8_RHI.trim3.nc | recast | 1.3 | 1.3 | 1.3 | 6.4 | 1.00 |  | 1 sw, 421,800 gates |
| cfrad.DOW8_RHI.trim3.nc | recast-f32 | 2.2 | 2.2 | 2.2 | 6.5 | 1.71 |  | 1 sw, 421,800 gates |

#### CfRadial 2, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| cfrad2.SPOL_20080604_002217.nc | netcdf4 | 157.4 / 183.1 / 172.4 | 172.4 | 153.5 | 69.4 |  |  | 0 sw, 8,674,132 gates |
| cfrad2.SPOL_20080604_002217.nc | xradar | fails: exit code 3221225477 | | | | | | |
| cfrad2.SPOL_20080604_002217.nc | pyart | fails: KeyError: 'time' | | | | | | |

#### CfRadial 2, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| cfrad2.SPOL_20080604_002217.nc | netcdf4 | 187.8 / 218.7 / 165.1 | 187.8 | 159.6 | 70.8 |  |  | 0 sw, 8,674,132 gates |
| cfrad2.SPOL_20080604_002217.nc | xradar | fails: exit code 3221225477 | | | | | | |
| cfrad2.SPOL_20080604_002217.nc | pyart | fails: KeyError: 'time' | | | | | | |

#### CfRadial 2, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| cfrad2.SPOL_20080604_002217.nc | netcdf4 | 180.7 | 180.7 | 180.7 | 69.1 |  |  | 0 sw, 8,674,132 gates |
| cfrad2.SPOL_20080604_002217.nc | xradar | 12264.2 | 12264.2 | 12264.2 | 289.5 |  |  | 9 sw, 8,651,326 gates |
| cfrad2.SPOL_20080604_002217.nc | pyart | fails: KeyError: 'time' | | | | | | |

#### DORADE, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.NOXP_20090501_190244_PPI | recast | 1.2 / 1.3 / 1.3 | 1.3 | 1.1 | 7.8 | 1.00 |  | 1 sw, 459,459 gates |
| swp.NOXP_20090501_190244_PPI | recast-f32 | 2.8 / 3.0 / 2.8 | 2.8 | 1.8 | 7.8 | 2.24 | yes | 1 sw, 459,459 gates |
| swp.NOXP_20090525_203211_SEC | recast | 2.7 / 2.2 / 2.1 | 2.2 | 1.8 | 10.4 | 1.00 |  | 1 sw, 800,800 gates |
| swp.NOXP_20090525_203211_SEC | recast-f32 | 5.4 / 4.9 / 4.4 | 4.9 | 2.8 | 10.5 | 2.19 | yes | 1 sw, 800,800 gates |
| swp.DOW6_20211230_RHI.head41 | recast | 4.3 / 3.8 / 4.1 | 4.1 | 3.5 | 12.2 | 1.00 |  | 1 sw, 1,120,000 gates |
| swp.DOW6_20211230_RHI.head41 | recast-f32 | 5.5 / 4.6 / 5.2 | 5.2 | 4.4 | 12.3 | 1.26 | yes | 1 sw, 1,120,000 gates |

#### DORADE, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.NOXP_20090501_190244_PPI | recast | 1.6 / 1.3 / 1.2 | 1.3 | 1.1 | 7.9 | 1.00 |  | 1 sw, 459,459 gates |
| swp.NOXP_20090501_190244_PPI | recast-f32 | 2.9 / 2.8 / 3.1 | 2.9 | 1.9 | 8.0 | 2.18 | yes | 1 sw, 459,459 gates |
| swp.NOXP_20090525_203211_SEC | recast | 3.2 / 2.6 / 2.1 | 2.6 | 1.8 | 10.4 | 1.00 |  | 1 sw, 800,800 gates |
| swp.NOXP_20090525_203211_SEC | recast-f32 | 5.4 / 4.5 / 4.6 | 4.6 | 3.3 | 10.4 | 1.77 | yes | 1 sw, 800,800 gates |
| swp.DOW6_20211230_RHI.head41 | recast | 5.0 / 4.0 / 4.2 | 4.2 | 3.0 | 12.2 | 1.00 |  | 1 sw, 1,120,000 gates |
| swp.DOW6_20211230_RHI.head41 | recast-f32 | 9.4 / 6.6 / 5.0 | 6.6 | 4.3 | 12.2 | 1.57 | yes | 1 sw, 1,120,000 gates |

#### DORADE, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.NOXP_20090501_190244_PPI | recast | 1.7 | 1.7 | 1.7 | 6.5 | 1.00 |  | 1 sw, 459,459 gates |
| swp.NOXP_20090501_190244_PPI | recast-f32 | 3.3 | 3.3 | 3.3 | 6.4 | 1.91 |  | 1 sw, 459,459 gates |
| swp.NOXP_20090525_203211_SEC | recast | 3.7 | 3.7 | 3.7 | 8.2 | 1.00 |  | 1 sw, 800,800 gates |
| swp.NOXP_20090525_203211_SEC | recast-f32 | 4.4 | 4.4 | 4.4 | 8.2 | 1.21 |  | 1 sw, 800,800 gates |
| swp.DOW6_20211230_RHI.head41 | recast | 4.7 | 4.7 | 4.7 | 9.7 | 1.00 |  | 1 sw, 1,120,000 gates |
| swp.DOW6_20211230_RHI.head41 | recast-f32 | 6.1 | 6.1 | 6.1 | 9.7 | 1.28 |  | 1 sw, 1,120,000 gates |

#### DORADE, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.NOXP_20090501_190244_PPI | recast | 1.7 | 1.7 | 1.7 | 6.5 | 1.00 |  | 1 sw, 459,459 gates |
| swp.NOXP_20090501_190244_PPI | recast-f32 | 2.5 | 2.5 | 2.5 | 6.4 | 1.47 |  | 1 sw, 459,459 gates |
| swp.NOXP_20090525_203211_SEC | recast | 3.9 | 3.9 | 3.9 | 8.2 | 1.00 |  | 1 sw, 800,800 gates |
| swp.NOXP_20090525_203211_SEC | recast-f32 | 4.1 | 4.1 | 4.1 | 8.2 | 1.05 |  | 1 sw, 800,800 gates |
| swp.DOW6_20211230_RHI.head41 | recast | 5.1 | 5.1 | 5.1 | 9.7 | 1.00 |  | 1 sw, 1,120,000 gates |
| swp.DOW6_20211230_RHI.head41 | recast-f32 | 13.7 | 13.7 | 13.7 | 9.7 | 2.66 |  | 1 sw, 1,120,000 gates |

#### JMA GRIB2 (one station), pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.RS47773.tar | recast | 13.6 / 13.9 / 10.6 | 13.6 | 10.2 | 64.5 | 1.00 |  | 26 sw, 7,526,400 gates |
| JMA_N5_20191012_0900.RS47773.tar | recast-f32 | 23.5 / 25.8 / 19.1 | 23.5 | 17.7 | 65.4 | 1.74 | yes | 26 sw, 7,526,400 gates |

#### JMA GRIB2 (one station), multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.RS47773.tar | recast | 19.5 / 19.0 / 13.6 | 19.0 | 9.8 | 64.4 | 1.00 |  | 26 sw, 7,526,400 gates |
| JMA_N5_20191012_0900.RS47773.tar | recast-f32 | 29.6 / 73.9 / 24.3 | 29.6 | 20.5 | 65.4 | 1.56 | yes | 26 sw, 7,526,400 gates |

#### JMA GRIB2 (one station), rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.RS47773.tar | recast | 14.8 | 14.8 | 14.8 | 35.0 | 1.00 |  | 26 sw, 7,526,400 gates |
| JMA_N5_20191012_0900.RS47773.tar | recast-f32 | 24.5 | 24.5 | 24.5 | 35.9 | 1.66 |  | 26 sw, 7,526,400 gates |

#### JMA GRIB2 (one station), rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.RS47773.tar | recast | 36.7 | 36.7 | 36.7 | 35.0 | 1.00 |  | 26 sw, 7,526,400 gates |
| JMA_N5_20191012_0900.RS47773.tar | recast-f32 | 41.3 | 41.3 | 41.3 | 35.4 | 1.13 |  | 26 sw, 7,526,400 gates |

#### JMA GRIB2 (20 stations), pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.tar | recast | 266.4 / 323.4 / 296.2 | 296.2 | 260.2 | 1204.9 | 1.00 |  | 520 sw, 150,528,000 gates |
| JMA_N5_20191012_0900.tar | recast-f32 | 446.5 / 495.5 / 465.2 | 465.2 | 399.9 | 1205.0 | 1.57 | yes | 520 sw, 150,528,000 gates |

#### JMA GRIB2 (20 stations), multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.tar | recast | 454.4 / 400.1 / 291.0 | 400.1 | 225.1 | 1204.9 | 1.00 |  | 520 sw, 150,528,000 gates |
| JMA_N5_20191012_0900.tar | recast-f32 | 589.4 / 646.7 / 474.5 | 589.4 | 429.1 | 1204.8 | 1.47 | yes | 520 sw, 150,528,000 gates |

#### JMA GRIB2 (20 stations), rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.tar | recast | 306.6 | 306.6 | 306.6 | 622.8 | 1.00 |  | 520 sw, 150,528,000 gates |
| JMA_N5_20191012_0900.tar | recast-f32 | 515.4 | 515.4 | 515.4 | 622.8 | 1.68 |  | 520 sw, 150,528,000 gates |

#### JMA GRIB2 (20 stations), rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| JMA_N5_20191012_0900.tar | recast | 406.9 | 406.9 | 406.9 | 622.7 | 1.00 |  | 520 sw, 150,528,000 gates |
| JMA_N5_20191012_0900.tar | recast-f32 | 737.8 | 737.8 | 737.8 | 622.7 | 1.81 |  | 520 sw, 150,528,000 gates |

### Router path

`recast_radar_io::read_supported_volume_bytes` (`decode_bench --format auto`, library
`recast-auto`) against the format's own reader (`recast`) on one file per format it routes,
3 rounds each (`run.py compare`); both run in the same rounds, so load affects them alike.
The peak RSS columns of the pinned rows hold two volumes (see Modes).

Linux (`nexbench`):

| file | mode | recast MoRM ms | recast-auto MoRM ms | recast-auto / recast | recast peak RSS MiB | recast-auto peak RSS MiB |
|---|---|---:|---:|---:|---:|---:|
| KTLX20240315_000217_V06 | pinned | 438.0 | 503.5 | 1.15 | 172.4 | 172.4 |
| KTLX20240315_000217_V06 | rss-1thread | 746.6 | 807.4 | 1.08 | 95.5 | 95.5 |
| KTLX20130520_201643_V06.gz | pinned | 64.6 | 95.1 | 1.47 | 94.4 | 136.3 |
| KTLX20130520_201643_V06.gz | rss-1thread | 79.5 | 139.1 | 1.75 | 54.0 | 95.8 |
| TLX_N0B_20260622_0806 | pinned | 16.5 |  | recast-auto fails: decode_bench: no Archive II volume header: the input starts with `SDUS54 K`, not AR2V or ARCHIVE2 (model-data  | | |
| TLX_N0B_20260622_0806 | rss-1thread | 16.3 |  | recast-auto fails: decode_bench: no Archive II volume header: the input starts with `SDUS54 K`, not AR2V or ARCHIVE2 (model-data  | | |
| iesha.pvol.20260305T0115.h5 | pinned | 7.3 | 7.1 | 0.97 | 14.3 | 14.1 |
| iesha.pvol.20260305T0115.h5 | rss-1thread | 10.6 | 10.6 | 1.00 | 9.8 | 9.8 |
| cfrad.IRENE_CPOL_sweeps0-1.nc | pinned | 0.6 | 0.5 | 0.87 | 10.1 | 10.1 |
| cfrad.IRENE_CPOL_sweeps0-1.nc | rss-1thread | 4.5 | 4.4 | 0.98 | 7.5 | 7.5 |
| swp.NOXP_20090501_190244_PPI | pinned | 0.4 | 0.4 | 1.03 | 7.3 | 7.3 |
| swp.NOXP_20090501_190244_PPI | rss-1thread | 1.7 | 1.9 | 1.10 | 6.0 | 5.8 |
| JMA_N5_20191012_0900.RS47773.tar | pinned | 6.6 | 6.2 | 0.94 | 63.1 | 63.1 |
| JMA_N5_20191012_0900.RS47773.tar | rss-1thread | 21.4 | 19.7 | 0.92 | 33.8 | 33.8 |
| KIWA307_chunks001-003 | pinned | 28.0 | 28.6 | 1.02 | 12.7 | 12.7 |
| KIWA307_chunks001-003 | rss-1thread | 18.3 | 19.3 | 1.05 | 10.5 | 10.5 |

Windows:

| file | mode | recast MoRM ms | recast-auto MoRM ms | recast-auto / recast | recast peak RSS MiB | recast-auto peak RSS MiB |
|---|---|---:|---:|---:|---:|---:|
| KTLX20240315_000217_V06 | pinned | 400.9 | 388.8 | 0.97 | 173.7 | 173.9 |
| KTLX20240315_000217_V06 | rss-1thread | 677.6 | 512.0 | 0.76 | 96.4 | 96.4 |
| KTLX20130520_201643_V06.gz | pinned | 58.0 | 79.2 | 1.37 | 95.9 | 137.6 |
| KTLX20130520_201643_V06.gz | rss-1thread | 58.7 | 82.3 | 1.40 | 54.9 | 96.7 |
| TLX_N0B_20260622_0806 | pinned | 12.2 |  | recast-auto fails: decode_bench: no Archive II volume header: the input starts with `SDUS54 K`, not AR2V or ARCHIVE2 (model-data  | | |
| TLX_N0B_20260622_0806 | rss-1thread | 15.9 |  | recast-auto fails: decode_bench: no Archive II volume header: the input starts with `SDUS54 K`, not AR2V or ARCHIVE2 (model-data  | | |
| iesha.pvol.20260305T0115.h5 | pinned | 9.1 | 9.0 | 1.00 | 15.7 | 15.5 |
| iesha.pvol.20260305T0115.h5 | rss-1thread | 12.7 | 12.4 | 0.98 | 10.6 | 10.6 |
| cfrad.IRENE_CPOL_sweeps0-1.nc | pinned | 2.5 | 2.4 | 0.96 | 10.9 | 10.9 |
| cfrad.IRENE_CPOL_sweeps0-1.nc | rss-1thread | 2.9 | 2.8 | 0.97 | 8.5 | 8.5 |
| swp.NOXP_20090501_190244_PPI | pinned | 1.3 | 1.4 | 1.05 | 7.8 | 7.6 |
| swp.NOXP_20090501_190244_PPI | rss-1thread | 1.9 | 1.8 | 0.92 | 6.4 | 6.4 |
| JMA_N5_20191012_0900.RS47773.tar | pinned | 13.9 | 14.7 | 1.05 | 64.5 | 64.5 |
| JMA_N5_20191012_0900.RS47773.tar | rss-1thread | 15.1 | 14.2 | 0.94 | 35.0 | 35.0 |
| KIWA307_chunks001-003 | pinned | 19.5 | 19.8 | 1.01 | 14.2 | 14.3 |
| KIWA307_chunks001-003 | rss-1thread | 24.7 | 22.6 | 0.92 | 12.0 | 11.9 |

Routing costs nothing measurable for AR2V, real-time chunks, ODIM, CfRadial 1, DORADE and JMA
input: the pinned ratios are 0.95-1.05 on Windows and 0.87-1.15 on the more loaded Linux side,
and the one-decode rows scatter more (a single sample each). Two exceptions, both post-merge
items for `recast-radar-io` (Hotspots): a gzip Level II volume goes through a whole inflate in
the router, which then parses the expanded buffer, so KTLX 2013 takes 1.37x (Windows, rounds
apart) and 1.47x (Linux, rounds overlapping) the time pinned and its one-decode peak goes from
54.9 to 96.7 MiB (Windows) and 54.0 to 95.8 MiB (Linux), the expanded 44.9 MB on top; and the
router does not dispatch Level III on this branch (G6): the product falls through to the Level
II decoder, which rejects it for its missing volume header.

### Cases added in the second fix pass

The first version of this page claimed every format this branch reads but had no whole-file
bzip2 Level II case and no ODIM_H5 Cartesian case (`decode_odim_h5_cartesian_max`). Both were
added (`run.py` cases, `decode_bench --format odim-cart`, `py_bench.py` for h5py and wradlib
on Cartesian products) and run the same way as the rest, 3 rounds, `decode_bench` from the fix
pass. xradar, Py-ART and LROSE read polar ODIM only, and no reader in the reference set other
than h5py and wradlib opens the IMGW Cartesian files. What they show:

- **Whole-file bzip2 Level II** (KTLX 2013 recompressed): recast is the only compiled reader
  that decodes it. Py-ART takes 2.5-3.4x recast's time and MetPy 2.6-3.9x on Linux (3.1-5.8x and 3.3-4.6x on Windows);
  the nexrad crate reads the stream as LDM records and returns `TruncatedRecord { expected:
  1753323641, actual: 7649586 }` (its harness stops on the error, so the tables show the panic
  note), go-nexrad stops at "unsupported compression bz2", RSL and LROSE do not open it,
  radrs fails the same way as the nexrad crate, and xradar raises an `IndexError`. recast
  decodes such a volume block by block on one thread (ac71f48), so the default pool does not
  help (Linux multi MoRM 1078 ms against 860 ms pinned); its one-decode peak is 59.0-59.3 MiB.
- **ODIM_H5 Cartesian**: recast decodes a 500 x 500 product in 0.2-0.4 ms; h5py reading every
  dataset takes 10-12x that and wradlib 26-37x on Linux (3.5-5.3x and 7.0-9.8x on Windows); one-decode peaks 4.2-4.8
  MiB against h5py's 42.5-42.7 and wradlib's 216.5-216.9 on Linux (5.5-5.6, 42.3-42.5 and
  193.8-194.1 on Windows; the Python peaks include the interpreter and imports).

#### Linux (`nexbench`)

##### Level II, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20130520_201643_V06.bz2 | recast | 742.8 / 860.1 / 2158.0 | 860.1 | 626.1 | 100.5 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | recast-f32 | 700.7 / 945.2 / 1841.1 | 945.2 | 432.3 | 101.9 | 1.10 | no | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | pyart | 2928.3 / 2671.9 / 4615.7 | 2928.3 | 2376.0 | 801.7 | 3.40 | yes | 17 sw, 91,013,760 gates |
| KTLX20130520_201643_V06.bz2 | metpy | 3313.5 / 3127.0 / 4757.7 | 3313.5 | 2895.0 | 555.5 | 3.85 | yes | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | nexrad-crate | fails: note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace | | | | | | |
| KTLX20130520_201643_V06.bz2 | go-nexrad | fails: time="2026-09-25T08:06:15Z" level=fatal msg="unsupported compression bz2" | | | | | | |
| KTLX20130520_201643_V06.bz2 | rsl | fails: rsl_bench: RSL could not read /build/xlib/data/KTLX20130520_201643_V06.bz2 | | | | | | |
| KTLX20130520_201643_V06.bz2 | lrose | fails: File format not recognized: /build/xlib/data/KTLX20130520_201643_V06.bz2 | | | | | | |
| KTLX20130520_201643_V06.bz2 | radrs | fails: ValueError: NEXRAD data error: truncated record: expected 1753323641 bytes, got 7649586 | | | | | | |
| KTLX20130520_201643_V06.bz2 | xradar | fails: IndexError: index 0 is out of bounds for axis 0 with size 0 | | | | | | |

##### Level II, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20130520_201643_V06.bz2 | recast-f32 | 908.9 / 1038.4 / 1571.1 | 1038.4 | 505.0 | 103.1 | 0.96 | no | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | recast | 1078.0 / 780.6 / 1492.7 | 1078.0 | 431.6 | 102.9 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | pyart | 2659.7 / 3911.4 / 2572.7 | 2659.7 | 2483.6 | 787.9 | 2.47 | yes | 17 sw, 91,013,760 gates |
| KTLX20130520_201643_V06.bz2 | metpy | 3744.0 / 4199.9 / 3828.3 | 3828.3 | 2432.4 | 555.8 | 3.55 | yes | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | nexrad-crate | fails: note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace | | | | | | |
| KTLX20130520_201643_V06.bz2 | go-nexrad | fails: time="2026-09-25T08:07:44Z" level=fatal msg="unsupported compression bz2" | | | | | | |
| KTLX20130520_201643_V06.bz2 | rsl | fails: rsl_bench: RSL could not read /build/xlib/data/KTLX20130520_201643_V06.bz2 | | | | | | |
| KTLX20130520_201643_V06.bz2 | lrose | fails: File format not recognized: /build/xlib/data/KTLX20130520_201643_V06.bz2 | | | | | | |
| KTLX20130520_201643_V06.bz2 | radrs | fails: ValueError: NEXRAD data error: truncated record: expected 1753323641 bytes, got 7649586 | | | | | | |
| KTLX20130520_201643_V06.bz2 | xradar | fails: IndexError: index 0 is out of bounds for axis 0 with size 0 | | | | | | |

##### Level II, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20130520_201643_V06.bz2 | recast | 1369.8 | 1369.8 | 1369.8 | 59.3 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | recast-f32 | 1472.4 | 1472.4 | 1472.4 | 59.0 | 1.07 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | pyart | 3573.3 | 3573.3 | 3573.3 | 787.6 | 2.61 |  | 17 sw, 91,013,760 gates |
| KTLX20130520_201643_V06.bz2 | metpy | 3619.0 | 3619.0 | 3619.0 | 555.6 | 2.64 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | nexrad-crate | fails: note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace | | | | | | |
| KTLX20130520_201643_V06.bz2 | go-nexrad | fails: time="2026-09-25T08:02:24Z" level=fatal msg="unsupported compression bz2" | | | | | | |
| KTLX20130520_201643_V06.bz2 | rsl | fails: rsl_bench: RSL could not read /build/xlib/data/KTLX20130520_201643_V06.bz2 | | | | | | |
| KTLX20130520_201643_V06.bz2 | lrose | fails: File format not recognized: /build/xlib/data/KTLX20130520_201643_V06.bz2 | | | | | | |
| KTLX20130520_201643_V06.bz2 | radrs | fails: ValueError: NEXRAD data error: truncated record: expected 1753323641 bytes, got 7649586 | | | | | | |
| KTLX20130520_201643_V06.bz2 | xradar | fails: IndexError: index 0 is out of bounds for axis 0 with size 0 | | | | | | |

##### Level II, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20130520_201643_V06.bz2 | recast-f32 | 624.8 | 624.8 | 624.8 | 59.3 | 0.92 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | recast | 678.2 | 678.2 | 678.2 | 59.0 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | nexrad-crate | fails: note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace | | | | | | |

##### ODIM_H5 Cartesian, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| imgw.ram.KDP.max.h5 | recast | 0.2 / 0.3 / 0.2 | 0.2 | 0.2 | 6.6 | 1.00 |  | 0 sw, 250,000 gates |
| imgw.ram.KDP.max.h5 | h5py | 2.7 / 3.2 / 2.9 | 2.9 | 1.3 | 43.2 | 11.76 | yes | 0 sw, 350,000 gates |
| imgw.ram.KDP.max.h5 | wradlib | 9.9 / 7.5 / 9.3 | 9.3 | 5.6 | 217.4 | 37.25 | yes | 0 sw, 350,000 gates |
| imgw.ram.RhoHV.max.h5 | recast | 0.3 / 0.3 / 0.4 | 0.3 | 0.2 | 6.9 | 1.00 |  | 0 sw, 250,000 gates |
| imgw.ram.RhoHV.max.h5 | h5py | 3.7 / 2.8 / 3.5 | 3.5 | 1.3 | 43.3 | 10.30 | yes | 0 sw, 350,000 gates |
| imgw.ram.RhoHV.max.h5 | wradlib | 7.5 / 9.2 / 9.3 | 9.2 | 5.5 | 217.7 | 27.17 | yes | 0 sw, 350,000 gates |

##### ODIM_H5 Cartesian, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| imgw.ram.KDP.max.h5 | recast | 0.3 / 0.2 / 0.3 | 0.3 | 0.2 | 6.8 | 1.00 |  | 0 sw, 250,000 gates |
| imgw.ram.KDP.max.h5 | h5py | 4.3 / 2.8 / 3.0 | 3.0 | 1.5 | 43.1 | 11.48 | yes | 0 sw, 350,000 gates |
| imgw.ram.KDP.max.h5 | wradlib | 13.0 / 9.1 / 8.0 | 9.1 | 3.8 | 217.3 | 35.52 | yes | 0 sw, 350,000 gates |
| imgw.ram.RhoHV.max.h5 | recast | 0.3 / 0.3 / 0.4 | 0.3 | 0.2 | 6.9 | 1.00 |  | 0 sw, 250,000 gates |
| imgw.ram.RhoHV.max.h5 | h5py | 7.0 / 3.5 / 2.5 | 3.5 | 1.5 | 43.5 | 10.72 | yes | 0 sw, 350,000 gates |
| imgw.ram.RhoHV.max.h5 | wradlib | 8.5 / 7.9 / 10.9 | 8.5 | 4.8 | 217.5 | 26.35 | yes | 0 sw, 350,000 gates |

##### ODIM_H5 Cartesian, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| imgw.ram.KDP.max.h5 | recast | 1.3 | 1.3 | 1.3 | 4.5 | 1.00 |  | 0 sw, 250,000 gates |
| imgw.ram.KDP.max.h5 | h5py | 5.9 | 5.9 | 5.9 | 42.5 | 4.56 |  | 0 sw, 350,000 gates |
| imgw.ram.KDP.max.h5 | wradlib | 18.9 | 18.9 | 18.9 | 216.9 | 14.70 |  | 0 sw, 350,000 gates |
| imgw.ram.RhoHV.max.h5 | recast | 1.2 | 1.2 | 1.2 | 4.8 | 1.00 |  | 0 sw, 250,000 gates |
| imgw.ram.RhoHV.max.h5 | h5py | 4.9 | 4.9 | 4.9 | 42.7 | 4.13 |  | 0 sw, 350,000 gates |
| imgw.ram.RhoHV.max.h5 | wradlib | 17.3 | 17.3 | 17.3 | 216.5 | 14.63 |  | 0 sw, 350,000 gates |

##### ODIM_H5 Cartesian, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| imgw.ram.KDP.max.h5 | recast | 1.2 | 1.2 | 1.2 | 4.2 | 1.00 |  | 0 sw, 250,000 gates |
| imgw.ram.RhoHV.max.h5 | recast | 1.3 | 1.3 | 1.3 | 4.8 | 1.00 |  | 0 sw, 250,000 gates |

#### Windows

##### Level II, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20130520_201643_V06.bz2 | recast-f32 | 650.0 / 325.9 / 386.2 | 386.2 | 261.6 | 101.6 | 0.82 | no | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | recast | 658.2 / 342.0 / 470.5 | 470.5 | 267.2 | 101.9 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | pyart | 2707.1 / 1219.8 / 1441.4 | 1441.4 | 1192.4 | 838.0 | 3.06 | yes | 17 sw, 91,013,760 gates |
| KTLX20130520_201643_V06.bz2 | metpy | 1345.7 / 1581.3 / 1832.2 | 1581.3 | 1161.3 | 558.6 | 3.36 | yes | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | nexrad-crate | fails: note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace | | | | | | |
| KTLX20130520_201643_V06.bz2 | radrs | fails: ValueError: NEXRAD data error: truncated record: expected 1753323641 bytes, got 7649586 | | | | | | |
| KTLX20130520_201643_V06.bz2 | xradar | fails: IndexError: index 0 is out of bounds for axis 0 with size 0 | | | | | | |

##### Level II, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20130520_201643_V06.bz2 | recast | 487.5 / 466.5 / 541.3 | 487.5 | 361.3 | 101.5 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | recast-f32 | 553.4 / 414.8 / 599.3 | 553.4 | 380.8 | 101.4 | 1.14 | no | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | pyart | 1420.6 / 1699.5 / 1582.5 | 1582.5 | 1375.0 | 841.9 | 3.25 | yes | 17 sw, 91,013,760 gates |
| KTLX20130520_201643_V06.bz2 | metpy | 1446.1 / 2058.8 / 1600.0 | 1600.0 | 1424.6 | 562.5 | 3.28 | yes | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | nexrad-crate | fails: note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace | | | | | | |
| KTLX20130520_201643_V06.bz2 | radrs | fails: ValueError: NEXRAD data error: truncated record: expected 1753323641 bytes, got 7649586 | | | | | | |
| KTLX20130520_201643_V06.bz2 | xradar | fails: IndexError: index 0 is out of bounds for axis 0 with size 0 | | | | | | |

##### Level II, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20130520_201643_V06.bz2 | recast | 286.3 | 286.3 | 286.3 | 60.5 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | recast-f32 | 298.3 | 298.3 | 298.3 | 60.4 | 1.04 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | metpy | 1320.2 | 1320.2 | 1320.2 | 557.6 | 4.61 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | pyart | 1647.7 | 1647.7 | 1647.7 | 822.7 | 5.76 |  | 17 sw, 91,013,760 gates |
| KTLX20130520_201643_V06.bz2 | nexrad-crate | fails: note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace | | | | | | |
| KTLX20130520_201643_V06.bz2 | radrs | fails: ValueError: NEXRAD data error: truncated record: expected 1753323641 bytes, got 7649586 | | | | | | |
| KTLX20130520_201643_V06.bz2 | xradar | fails: IndexError: index 0 is out of bounds for axis 0 with size 0 | | | | | | |

##### Level II, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| KTLX20130520_201643_V06.bz2 | recast | 337.2 | 337.2 | 337.2 | 60.4 | 1.00 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | recast-f32 | 363.7 | 363.7 | 363.7 | 60.5 | 1.08 |  | 17 sw, 36,624,960 gates |
| KTLX20130520_201643_V06.bz2 | nexrad-crate | fails: note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace | | | | | | |

##### ODIM_H5 Cartesian, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| imgw.ram.KDP.max.h5 | recast | 0.7 / 0.7 / 0.6 | 0.7 | 0.3 | 7.4 | 1.00 |  | 0 sw, 250,000 gates |
| imgw.ram.KDP.max.h5 | h5py | 2.1 / 3.3 / 2.9 | 2.9 | 1.8 | 42.8 | 4.37 | yes | 0 sw, 350,000 gates |
| imgw.ram.KDP.max.h5 | wradlib | 4.6 / 5.4 / 5.3 | 5.3 | 3.3 | 195.0 | 8.12 | yes | 0 sw, 350,000 gates |
| imgw.ram.RhoHV.max.h5 | recast | 0.5 / 0.5 / 0.9 | 0.5 | 0.3 | 7.4 | 1.00 |  | 0 sw, 250,000 gates |
| imgw.ram.RhoHV.max.h5 | h5py | 2.9 / 1.8 / 3.5 | 2.9 | 1.6 | 42.4 | 5.30 | yes | 0 sw, 350,000 gates |
| imgw.ram.RhoHV.max.h5 | wradlib | 5.9 / 5.4 / 5.1 | 5.4 | 3.6 | 194.4 | 9.83 | yes | 0 sw, 350,000 gates |

##### ODIM_H5 Cartesian, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| imgw.ram.KDP.max.h5 | recast | 0.6 / 0.8 / 0.7 | 0.7 | 0.2 | 6.8 | 1.00 |  | 0 sw, 250,000 gates |
| imgw.ram.KDP.max.h5 | h5py | 2.2 / 4.3 / 2.4 | 2.4 | 1.4 | 44.6 | 3.57 | yes | 0 sw, 350,000 gates |
| imgw.ram.KDP.max.h5 | wradlib | 5.1 / 7.3 / 5.4 | 5.4 | 3.3 | 197.7 | 8.11 | yes | 0 sw, 350,000 gates |
| imgw.ram.RhoHV.max.h5 | recast | 0.7 / 0.6 / 0.8 | 0.7 | 0.3 | 6.9 | 1.00 |  | 0 sw, 250,000 gates |
| imgw.ram.RhoHV.max.h5 | h5py | 2.7 / 2.1 / 2.4 | 2.4 | 1.4 | 44.2 | 3.46 | yes | 0 sw, 350,000 gates |
| imgw.ram.RhoHV.max.h5 | wradlib | 5.5 / 4.9 / 4.9 | 4.9 | 3.1 | 197.8 | 6.99 | yes | 0 sw, 350,000 gates |

##### ODIM_H5 Cartesian, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| imgw.ram.KDP.max.h5 | recast | 0.9 | 0.9 | 0.9 | 5.5 | 1.00 |  | 0 sw, 250,000 gates |
| imgw.ram.KDP.max.h5 | h5py | 2.7 | 2.7 | 2.7 | 42.3 | 2.83 |  | 0 sw, 350,000 gates |
| imgw.ram.KDP.max.h5 | wradlib | 7.3 | 7.3 | 7.3 | 193.8 | 7.83 |  | 0 sw, 350,000 gates |
| imgw.ram.RhoHV.max.h5 | recast | 1.4 | 1.4 | 1.4 | 5.6 | 1.00 |  | 0 sw, 250,000 gates |
| imgw.ram.RhoHV.max.h5 | h5py | 3.6 | 3.6 | 3.6 | 42.5 | 2.56 |  | 0 sw, 350,000 gates |
| imgw.ram.RhoHV.max.h5 | wradlib | 7.6 | 7.6 | 7.6 | 194.1 | 5.36 |  | 0 sw, 350,000 gates |

##### ODIM_H5 Cartesian, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| imgw.ram.KDP.max.h5 | recast | 1.4 | 1.4 | 1.4 | 5.5 | 1.00 |  | 0 sw, 250,000 gates |
| imgw.ram.RhoHV.max.h5 | recast | 1.4 | 1.4 | 1.4 | 5.6 | 1.00 |  | 0 sw, 250,000 gates |

### Cases added in the fourth fix pass

The review of the third fix pass found Level III benchmarked on radial products only, no ODIM_H5
SCAN object and no airborne DORADE. The fourth fix pass added (`run.py` cases, 3 rounds,
pinned / multi / rss, `decode_bench` of 2a3365f): a raster product (TLX composite reflectivity,
code 37, 464 x 464 cells) and a digital precipitation array (code 81, packet 17) through
`read_level3_volume`; three products that have no data array for a volume, storm tracking
(58) and mesocyclone (141) graphic products and the storm structure (62) tabular product,
through `decode_bench --format l3-product` (`decode_message`: every block and packet, as
MetPy's `Level3File` parses them); DWD's Boostedt sweep 00 as published, an ODIM_H5 `SCAN`
object (one sweep per file); and the NOAA P-3 N42RF tail radar's airborne sweep in Hurricane
Michael (scan mode AIR, 17 fields, 3.8 million gates). The DWD and N42RF files are not in this
branch's testdata manifest, so `run.py stage` pins them by SHA-256 (the hdf5-netcdf stream
commits the DWD sweep and the metadata-complete stream registers the N42RF sweep). The Linux
rounds overlapped 53 throttled 100 ms periods of the container (`cpu.stat` before and after).
What they show (pinned, median of the round medians over recast's, every pair's rounds apart):

- **Level III raster and digital array products**: MetPy takes 26x recast's time on the raster
  product and about 100x on the digital precipitation array on Linux (12.8x and 15.2x on
  Windows), whose decode takes recast under 0.1 ms; MetPy's `Level3File` returns the stored
  codes, so `recast` is its like-for-like row. Py-ART reads neither (`NotImplementedError:
  Level3 product with code 37 is not supported`, and code 81), nor any of the graphic and
  tabular products.
- **Level III graphic and tabular products**: recast decodes each in under 0.1 ms; MetPy takes
  46-81x that on Linux and 17-23x on Windows.
- **ODIM_H5 SCAN**: h5py reading every dataset takes 4.5x recast's time, LROSE 5.8x, wradlib
  8.3x, Py-ART 11.8x and xradar 28x on Linux (h5py 2.5x, wradlib 3.7x, Py-ART 6.4x, xradar 13.5x
  on Windows). `recast-f32` took 1.18x `recast`'s time on Linux (rounds overlapping) and 1.43x
  on Windows. One-decode peak 4.8 MiB (Linux) against LROSE's 26.0 and the Python readers'
  42-254.
- **Airborne DORADE**: recast decodes the N42RF sweep in 3.7 ms pinned on Linux (10.4 ms on
  Windows); LROSE takes 24x that and peaks at 45.5 MiB against recast's 21.2 in one decode; RSL
  returns no sweep from it. Nothing else in the reference set reads DORADE.

#### Linux (`nexbench`)

##### Level III, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NCR_20260622_0806 | recast | 0.2 / 0.2 / 0.2 | 0.2 | 0.2 | 6.2 | 1.00 |  | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | recast-f32 | 0.4 / 0.4 / 0.4 | 0.4 | 0.3 | 6.9 | 2.07 | yes | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | metpy | 5.6 / 5.2 / 5.3 | 5.3 | 4.5 | 190.1 | 26.27 | yes | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | pyart | fails: NotImplementedError: Level3 product with code 37 is not supported | | | | | | |
| TLX_DPA_20260629_1736 | recast | 0.0 / 0.0 / 0.0 | 0.0 | 0.0 | 3.2 | 1.00 |  | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | recast-f32 | 0.0 / 0.0 / 0.0 | 0.0 | 0.0 | 3.5 | 1.57 | yes | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | metpy | 0.7 / 0.8 / 0.7 | 0.7 | 0.5 | 188.3 | 101.00 | yes | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | pyart | fails: NotImplementedError: Level3 product with code 81 is not supported | | | | | | |

##### Level III, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NCR_20260622_0806 | recast | 0.2 / 0.2 / 0.3 | 0.2 | 0.2 | 6.1 | 1.00 |  | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | recast-f32 | 0.5 / 0.5 / 0.5 | 0.5 | 0.3 | 7.0 | 2.14 | yes | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | metpy | 6.1 / 5.9 / 5.8 | 5.9 | 4.8 | 190.1 | 27.63 | yes | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | pyart | fails: NotImplementedError: Level3 product with code 37 is not supported | | | | | | |
| TLX_DPA_20260629_1736 | recast | 0.0 / 0.0 / 0.0 | 0.0 | 0.0 | 3.5 | 1.00 |  | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | recast-f32 | 0.0 / 0.0 / 0.0 | 0.0 | 0.0 | 3.5 | 1.57 | yes | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | metpy | 0.8 / 0.7 / 0.7 | 0.7 | 0.5 | 188.2 | 106.29 | yes | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | pyart | fails: NotImplementedError: Level3 product with code 81 is not supported | | | | | | |

##### Level III, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NCR_20260622_0806 | recast | 0.7 | 0.7 | 0.7 | 4.2 | 1.00 |  | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | recast-f32 | 1.3 | 1.3 | 1.3 | 5.1 | 1.88 |  | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | metpy | 5.1 | 5.1 | 5.1 | 190.0 | 7.22 |  | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | pyart | fails: NotImplementedError: Level3 product with code 37 is not supported | | | | | | |
| TLX_DPA_20260629_1736 | recast | 0.2 | 0.2 | 0.2 | 3.0 | 1.00 |  | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | recast-f32 | 0.4 | 0.4 | 0.4 | 3.5 | 1.61 |  | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | metpy | 1.5 | 1.5 | 1.5 | 188.3 | 6.25 |  | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | pyart | fails: NotImplementedError: Level3 product with code 81 is not supported | | | | | | |

##### Level III, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NCR_20260622_0806 | recast | 0.7 | 0.7 | 0.7 | 4.5 | 1.00 |  | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | recast-f32 | 1.3 | 1.3 | 1.3 | 4.8 | 1.78 |  | 1 sw, 215,296 gates |
| TLX_DPA_20260629_1736 | recast | 0.1 | 0.1 | 0.1 | 3.0 | 1.00 |  | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | recast-f32 | 0.2 | 0.2 | 0.2 | 3.5 | 1.28 |  | 1 sw, 17,161 gates |

##### Level III graphic and tabular products, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NST_20260622_0806 | recast | 0.0 / 0.1 / 0.1 | 0.1 | 0.0 | 3.5 | 1.00 |  | 0 sw, 0 rays |
| TLX_NST_20260622_0806 | metpy | 2.3 / 2.1 / 2.5 | 2.3 | 1.8 | 188.8 | 46.08 | yes | 1 sw, 0 rays |
| TLX_NMD_20260622_0806 | recast | 0.0 / 0.0 / 0.0 | 0.0 | 0.0 | 3.2 | 1.00 |  | 0 sw, 0 rays |
| TLX_NMD_20260622_0806 | metpy | 1.0 / 1.0 / 1.2 | 1.0 | 0.8 | 188.2 | 74.92 | yes | 1 sw, 0 rays |
| TLX_NSS_20220503_0052 | recast | 0.0 / 0.0 / 0.0 | 0.0 | 0.0 | 3.2 | 1.00 |  | 0 sw, 0 rays |
| TLX_NSS_20220503_0052 | metpy | 1.1 / 1.2 / 1.1 | 1.1 | 0.9 | 188.2 | 80.79 | yes | 1 sw, 0 rays |

##### Level III graphic and tabular products, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NST_20260622_0806 | recast | 0.0 / 0.0 / 0.0 | 0.0 | 0.0 | 3.8 | 1.00 |  | 0 sw, 0 rays |
| TLX_NST_20260622_0806 | metpy | 2.4 / 2.1 / 2.3 | 2.3 | 1.9 | 188.9 | 55.71 | yes | 1 sw, 0 rays |
| TLX_NMD_20260622_0806 | recast | 0.0 / 0.0 / 0.0 | 0.0 | 0.0 | 3.2 | 1.00 |  | 0 sw, 0 rays |
| TLX_NMD_20260622_0806 | metpy | 1.1 / 1.0 / 0.9 | 1.0 | 0.8 | 188.3 | 74.54 | yes | 1 sw, 0 rays |
| TLX_NSS_20220503_0052 | recast | 0.0 / 0.0 / 0.0 | 0.0 | 0.0 | 3.0 | 1.00 |  | 0 sw, 0 rays |
| TLX_NSS_20220503_0052 | metpy | 1.2 / 1.1 / 1.0 | 1.1 | 0.9 | 188.0 | 80.21 | yes | 1 sw, 0 rays |

##### Level III graphic and tabular products, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NST_20260622_0806 | recast | 0.2 | 0.2 | 0.2 | 3.0 | 1.00 |  | 0 sw, 0 rays |
| TLX_NST_20260622_0806 | metpy | 2.1 | 2.1 | 2.1 | 188.9 | 11.56 |  | 1 sw, 0 rays |
| TLX_NMD_20260622_0806 | recast | 0.1 | 0.1 | 0.1 | 3.2 | 1.00 |  | 0 sw, 0 rays |
| TLX_NMD_20260622_0806 | metpy | 0.8 | 0.8 | 0.8 | 188.0 | 5.78 |  | 1 sw, 0 rays |
| TLX_NSS_20220503_0052 | recast | 0.1 | 0.1 | 0.1 | 3.2 | 1.00 |  | 0 sw, 0 rays |
| TLX_NSS_20220503_0052 | metpy | 1.4 | 1.4 | 1.4 | 188.0 | 16.01 |  | 1 sw, 0 rays |

##### Level III graphic and tabular products, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NST_20260622_0806 | recast | 0.3 | 0.3 | 0.3 | 3.2 | 1.00 |  | 0 sw, 0 rays |
| TLX_NMD_20260622_0806 | recast | 0.1 | 0.1 | 0.1 | 3.2 | 1.00 |  | 0 sw, 0 rays |
| TLX_NSS_20220503_0052 | recast | 0.1 | 0.1 | 0.1 | 3.0 | 1.00 |  | 0 sw, 0 rays |

##### ODIM_H5, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| deboo.scan.20260924T2130.th00.hd5 | recast | 0.6 / 0.9 / 0.6 | 0.6 | 0.4 | 5.9 | 1.00 |  | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | recast-f32 | 0.7 / 0.8 / 0.8 | 0.8 | 0.5 | 6.9 | 1.18 | no | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | h5py | 2.9 / 3.0 / 1.6 | 2.9 | 1.5 | 42.8 | 4.50 | yes | 0 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | lrose | 3.8 / 3.7 / 4.0 | 3.8 | 3.4 | 25.3 | 5.84 | yes | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | wradlib | 5.9 / 5.3 / 5.3 | 5.3 | 4.2 | 217.4 | 8.30 | yes | 0 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | pyart | 7.6 / 7.0 / 11.2 | 7.6 | 6.2 | 239.5 | 11.82 | yes | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | xradar | 18.1 / 19.9 / 16.6 | 18.1 | 14.6 | 255.9 | 28.09 | yes | 1 sw, 259,213 gates |

##### ODIM_H5, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| deboo.scan.20260924T2130.th00.hd5 | recast | 0.6 / 0.8 / 0.5 | 0.6 | 0.5 | 6.2 | 1.00 |  | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | recast-f32 | 0.6 / 0.7 / 0.7 | 0.7 | 0.5 | 6.6 | 1.19 | no | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | h5py | 2.5 / 2.8 / 1.8 | 2.5 | 1.6 | 42.7 | 4.47 | yes | 0 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | lrose | 4.5 / 4.4 / 4.4 | 4.4 | 3.6 | 25.8 | 7.97 | yes | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | wradlib | 5.6 / 4.5 / 4.1 | 4.5 | 3.8 | 217.5 | 8.14 | yes | 0 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | pyart | 6.8 / 8.6 / 6.7 | 6.8 | 5.8 | 239.5 | 12.23 | yes | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | xradar | 16.7 / 17.4 / 14.0 | 16.7 | 12.9 | 255.7 | 30.08 | yes | 1 sw, 259,213 gates |

##### ODIM_H5, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| deboo.scan.20260924T2130.th00.hd5 | recast | 1.1 | 1.1 | 1.1 | 4.8 | 1.00 |  | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | recast-f32 | 1.7 | 1.7 | 1.7 | 5.4 | 1.50 |  | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | h5py | 2.6 | 2.6 | 2.6 | 42.2 | 2.30 |  | 0 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | wradlib | 6.4 | 6.4 | 6.4 | 216.8 | 5.67 |  | 0 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | pyart | 10.6 | 10.6 | 10.6 | 237.8 | 9.42 |  | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | lrose | 11.4 | 11.4 | 11.4 | 26.0 | 10.16 |  | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | xradar | 1180.7 | 1180.7 | 1180.7 | 254.3 | 1048.54 |  | 1 sw, 259,213 gates |

##### ODIM_H5, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| deboo.scan.20260924T2130.th00.hd5 | recast | 1.2 | 1.2 | 1.2 | 4.8 | 1.00 |  | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | recast-f32 | 1.6 | 1.6 | 1.6 | 5.4 | 1.38 |  | 1 sw, 259,200 gates |

##### DORADE, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.N42RF-TM_20181010_123925_AIR | recast | 3.6 / 3.7 / 5.1 | 3.7 | 2.0 | 29.2 | 1.00 |  | 1 sw, 3,837,240 gates |
| swp.N42RF-TM_20181010_123925_AIR | recast-f32 | 4.5 / 10.4 / 5.8 | 5.8 | 3.6 | 29.4 | 1.59 | no | 1 sw, 3,837,240 gates |
| swp.N42RF-TM_20181010_123925_AIR | rsl | 14.8 / 13.5 / 9.7 | 13.5 | 8.3 | 36.0 | 3.67 | yes | 0 sw, 0 rays |
| swp.N42RF-TM_20181010_123925_AIR | lrose | 97.7 / 89.6 / 68.7 | 89.6 | 63.3 | 48.0 | 24.42 | yes | 1 sw, 3,837,240 gates |

##### DORADE, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.N42RF-TM_20181010_123925_AIR | recast | 5.7 / 3.9 / 3.8 | 3.9 | 2.2 | 29.4 | 1.00 |  | 1 sw, 3,837,240 gates |
| swp.N42RF-TM_20181010_123925_AIR | recast-f32 | 11.1 / 8.5 / 6.4 | 8.5 | 5.5 | 29.4 | 2.18 | yes | 1 sw, 3,837,240 gates |
| swp.N42RF-TM_20181010_123925_AIR | rsl | 18.3 / 13.6 / 10.7 | 13.6 | 9.9 | 36.0 | 3.48 | yes | 0 sw, 0 rays |
| swp.N42RF-TM_20181010_123925_AIR | lrose | 116.3 / 90.8 / 68.5 | 90.8 | 64.5 | 48.0 | 23.25 | yes | 1 sw, 3,837,240 gates |

##### DORADE, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.N42RF-TM_20181010_123925_AIR | recast-f32 | 11.0 | 11.0 | 11.0 | 21.2 | 0.66 |  | 1 sw, 3,837,240 gates |
| swp.N42RF-TM_20181010_123925_AIR | rsl | 13.2 | 13.2 | 13.2 | 6.2 | 0.79 |  | 0 sw, 0 rays |
| swp.N42RF-TM_20181010_123925_AIR | recast | 16.7 | 16.7 | 16.7 | 21.2 | 1.00 |  | 1 sw, 3,837,240 gates |
| swp.N42RF-TM_20181010_123925_AIR | lrose | 77.0 | 77.0 | 77.0 | 45.5 | 4.60 |  | 1 sw, 3,837,240 gates |

##### DORADE, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.N42RF-TM_20181010_123925_AIR | recast-f32 | 13.6 | 13.6 | 13.6 | 21.2 | 0.62 |  | 1 sw, 3,837,240 gates |
| swp.N42RF-TM_20181010_123925_AIR | recast | 22.1 | 22.1 | 22.1 | 21.5 | 1.00 |  | 1 sw, 3,837,240 gates |

#### Windows

##### Level III, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NCR_20260622_0806 | recast | 0.7 / 0.8 / 0.7 | 0.7 | 0.4 | 7.1 | 1.00 |  | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | recast-f32 | 1.0 / 0.9 / 0.9 | 0.9 | 0.7 | 7.1 | 1.26 | yes | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | metpy | 8.8 / 9.4 / 8.6 | 8.8 | 7.4 | 181.9 | 12.83 | yes | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | pyart | fails: NotImplementedError: Level3 product with code 37 is not supported | | | | | | |
| TLX_DPA_20260629_1736 | recast | 0.1 / 0.1 / 0.1 | 0.1 | 0.0 | 4.9 | 1.00 |  | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | recast-f32 | 0.1 / 0.1 / 0.1 | 0.1 | 0.1 | 4.9 | 1.52 | yes | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | metpy | 1.1 / 1.1 / 1.0 | 1.1 | 0.7 | 180.4 | 15.21 | yes | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | pyart | fails: NotImplementedError: Level3 product with code 81 is not supported | | | | | | |

##### Level III, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NCR_20260622_0806 | recast | 0.8 / 0.7 / 0.6 | 0.7 | 0.3 | 6.2 | 1.00 |  | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | recast-f32 | 1.2 / 1.2 / 0.9 | 1.2 | 0.5 | 7.1 | 1.66 | yes | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | metpy | 10.9 / 9.2 / 7.8 | 9.2 | 5.1 | 185.3 | 13.23 | yes | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | pyart | fails: NotImplementedError: Level3 product with code 37 is not supported | | | | | | |
| TLX_DPA_20260629_1736 | recast | 0.1 / 0.1 / 0.1 | 0.1 | 0.0 | 4.8 | 1.00 |  | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | recast-f32 | 0.1 / 0.1 / 0.1 | 0.1 | 0.1 | 4.9 | 2.27 | yes | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | metpy | 1.0 / 1.0 / 1.1 | 1.0 | 0.7 | 183.2 | 17.14 | yes | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | pyart | fails: NotImplementedError: Level3 product with code 81 is not supported | | | | | | |

##### Level III, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NCR_20260622_0806 | recast | 1.1 | 1.1 | 1.1 | 5.3 | 1.00 |  | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | recast-f32 | 1.3 | 1.3 | 1.3 | 6.0 | 1.23 |  | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | metpy | 11.1 | 11.1 | 11.1 | 181.9 | 10.55 |  | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | pyart | fails: NotImplementedError: Level3 product with code 37 is not supported | | | | | | |
| TLX_DPA_20260629_1736 | recast | 0.3 | 0.3 | 0.3 | 4.8 | 1.00 |  | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | recast-f32 | 0.4 | 0.4 | 0.4 | 4.8 | 1.33 |  | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | metpy | 1.6 | 1.6 | 1.6 | 179.8 | 5.53 |  | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | pyart | fails: NotImplementedError: Level3 product with code 81 is not supported | | | | | | |

##### Level III, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NCR_20260622_0806 | recast | 1.0 | 1.0 | 1.0 | 5.3 | 1.00 |  | 1 sw, 215,296 gates |
| TLX_NCR_20260622_0806 | recast-f32 | 1.2 | 1.2 | 1.2 | 6.0 | 1.18 |  | 1 sw, 215,296 gates |
| TLX_DPA_20260629_1736 | recast | 0.3 | 0.3 | 0.3 | 4.8 | 1.00 |  | 1 sw, 17,161 gates |
| TLX_DPA_20260629_1736 | recast-f32 | 0.3 | 0.3 | 0.3 | 4.8 | 1.03 |  | 1 sw, 17,161 gates |

##### Level III graphic and tabular products, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NST_20260622_0806 | recast | 0.2 / 0.1 / 0.1 | 0.1 | 0.1 | 5.1 | 1.00 |  | 0 sw, 0 rays |
| TLX_NST_20260622_0806 | metpy | 3.3 / 3.1 / 3.4 | 3.3 | 2.7 | 180.4 | 22.63 | yes | 1 sw, 0 rays |
| TLX_NMD_20260622_0806 | recast | 0.1 / 0.1 / 0.1 | 0.1 | 0.0 | 4.9 | 1.00 |  | 0 sw, 0 rays |
| TLX_NMD_20260622_0806 | metpy | 1.4 / 1.4 / 1.3 | 1.4 | 1.0 | 180.4 | 16.98 | yes | 1 sw, 0 rays |
| TLX_NSS_20220503_0052 | recast | 0.1 / 0.1 / 0.1 | 0.1 | 0.1 | 4.9 | 1.00 |  | 0 sw, 0 rays |
| TLX_NSS_20220503_0052 | metpy | 1.6 / 1.6 / 1.6 | 1.6 | 1.4 | 179.9 | 22.79 | yes | 1 sw, 0 rays |

##### Level III graphic and tabular products, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NST_20260622_0806 | recast | 0.2 / 0.2 / 0.1 | 0.2 | 0.1 | 5.1 | 1.00 |  | 0 sw, 0 rays |
| TLX_NST_20260622_0806 | metpy | 3.3 / 2.9 / 2.0 | 2.9 | 1.8 | 183.0 | 17.65 | yes | 1 sw, 0 rays |
| TLX_NMD_20260622_0806 | recast | 0.1 / 0.1 / 0.0 | 0.1 | 0.0 | 4.9 | 1.00 |  | 0 sw, 0 rays |
| TLX_NMD_20260622_0806 | metpy | 1.6 / 1.4 / 1.2 | 1.4 | 0.9 | 183.6 | 17.58 | yes | 1 sw, 0 rays |
| TLX_NSS_20220503_0052 | recast | 0.1 / 0.1 / 0.1 | 0.1 | 0.0 | 4.9 | 1.00 |  | 0 sw, 0 rays |
| TLX_NSS_20220503_0052 | metpy | 1.7 / 1.7 / 1.6 | 1.7 | 1.0 | 183.0 | 16.59 | yes | 1 sw, 0 rays |

##### Level III graphic and tabular products, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NST_20260622_0806 | recast | 0.5 | 0.5 | 0.5 | 4.8 | 1.00 |  | 0 sw, 0 rays |
| TLX_NST_20260622_0806 | metpy | 3.8 | 3.8 | 3.8 | 180.2 | 7.60 |  | 1 sw, 0 rays |
| TLX_NMD_20260622_0806 | recast | 0.3 | 0.3 | 0.3 | 4.8 | 1.00 |  | 0 sw, 0 rays |
| TLX_NMD_20260622_0806 | metpy | 1.5 | 1.5 | 1.5 | 179.9 | 4.96 |  | 1 sw, 0 rays |
| TLX_NSS_20220503_0052 | recast | 0.4 | 0.4 | 0.4 | 4.8 | 1.00 |  | 0 sw, 0 rays |
| TLX_NSS_20220503_0052 | metpy | 1.7 | 1.7 | 1.7 | 179.5 | 4.11 |  | 1 sw, 0 rays |

##### Level III graphic and tabular products, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| TLX_NST_20260622_0806 | recast | 1.0 | 1.0 | 1.0 | 4.8 | 1.00 |  | 0 sw, 0 rays |
| TLX_NMD_20260622_0806 | recast | 0.3 | 0.3 | 0.3 | 4.8 | 1.00 |  | 0 sw, 0 rays |
| TLX_NSS_20220503_0052 | recast | 0.4 | 0.4 | 0.4 | 4.8 | 1.00 |  | 0 sw, 0 rays |

##### ODIM_H5, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| deboo.scan.20260924T2130.th00.hd5 | recast | 1.3 / 1.3 / 1.6 | 1.3 | 1.0 | 6.9 | 1.00 |  | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | recast-f32 | 1.8 / 1.9 / 2.0 | 1.9 | 1.8 | 6.8 | 1.43 | yes | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | h5py | 3.2 / 3.3 / 3.6 | 3.3 | 3.0 | 43.6 | 2.46 | yes | 0 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | wradlib | 4.8 / 6.0 / 4.9 | 4.9 | 4.0 | 195.8 | 3.68 | yes | 0 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | pyart | 7.9 / 10.4 / 8.6 | 8.6 | 7.4 | 216.2 | 6.37 | yes | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | xradar | 19.4 / 18.2 / 17.1 | 18.2 | 14.8 | 230.1 | 13.50 | yes | 1 sw, 259,213 gates |

##### ODIM_H5, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| deboo.scan.20260924T2130.th00.hd5 | recast | 1.2 / 1.9 / 1.2 | 1.2 | 0.8 | 7.2 | 1.00 |  | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | recast-f32 | 1.6 / 4.4 / 1.4 | 1.6 | 1.2 | 7.7 | 1.29 | no | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | h5py | 3.0 / 3.3 / 2.2 | 3.0 | 1.7 | 45.4 | 2.41 | yes | 0 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | wradlib | 5.7 / 7.1 / 4.5 | 5.7 | 3.1 | 199.4 | 4.60 | yes | 0 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | pyart | 10.4 / 10.6 / 6.0 | 10.4 | 4.8 | 219.7 | 8.41 | yes | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | xradar | 22.4 / 24.0 / 16.0 | 22.4 | 12.6 | 233.5 | 18.10 | yes | 1 sw, 259,213 gates |

##### ODIM_H5, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| deboo.scan.20260924T2130.th00.hd5 | recast | 1.8 | 1.8 | 1.8 | 6.2 | 1.00 |  | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | recast-f32 | 2.0 | 2.0 | 2.0 | 6.3 | 1.16 |  | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | h5py | 3.6 | 3.6 | 3.6 | 43.5 | 2.05 |  | 0 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | wradlib | 8.4 | 8.4 | 8.4 | 195.0 | 4.72 |  | 0 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | pyart | 10.8 | 10.8 | 10.8 | 215.8 | 6.12 |  | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | xradar | 2023.9 | 2023.9 | 2023.9 | 229.6 | 1142.16 |  | 1 sw, 259,213 gates |

##### ODIM_H5, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| deboo.scan.20260924T2130.th00.hd5 | recast | 1.8 | 1.8 | 1.8 | 6.2 | 1.00 |  | 1 sw, 259,200 gates |
| deboo.scan.20260924T2130.th00.hd5 | recast-f32 | 2.5 | 2.5 | 2.5 | 6.3 | 1.36 |  | 1 sw, 259,200 gates |

##### DORADE, pinned

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.N42RF-TM_20181010_123925_AIR | recast | 10.4 / 11.3 / 10.3 | 10.4 | 9.5 | 25.9 | 1.00 |  | 1 sw, 3,837,240 gates |
| swp.N42RF-TM_20181010_123925_AIR | recast-f32 | 12.3 / 16.5 / 16.2 | 16.2 | 11.9 | 25.8 | 1.55 | yes | 1 sw, 3,837,240 gates |

##### DORADE, multi

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.N42RF-TM_20181010_123925_AIR | recast | 10.1 / 12.6 / 10.6 | 10.6 | 7.2 | 26.0 | 1.00 |  | 1 sw, 3,837,240 gates |
| swp.N42RF-TM_20181010_123925_AIR | recast-f32 | 17.6 / 20.9 / 13.7 | 17.6 | 12.2 | 25.9 | 1.66 | yes | 1 sw, 3,837,240 gates |

##### DORADE, rss-1thread

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.N42RF-TM_20181010_123925_AIR | recast | 11.1 | 11.1 | 11.1 | 16.8 | 1.00 |  | 1 sw, 3,837,240 gates |
| swp.N42RF-TM_20181010_123925_AIR | recast-f32 | 17.1 | 17.1 | 17.1 | 16.9 | 1.54 |  | 1 sw, 3,837,240 gates |

##### DORADE, rss-default

| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |
|---|---|---|---:|---:|---:|---:|---|---|
| swp.N42RF-TM_20181010_123925_AIR | recast | 10.4 | 10.4 | 10.4 | 17.1 | 1.00 |  | 1 sw, 3,837,240 gates |
| swp.N42RF-TM_20181010_123925_AIR | recast-f32 | 18.0 | 18.0 | 18.0 | 16.9 | 1.74 |  | 1 sw, 3,837,240 gates |

## This pass's optimisations

Every change keeps the output: decode hashes (`decode_bench`: every field's values, names and
shapes and the azimuths), the `Debug` text of every volume (`--debug-hash`, which covers
provenance and decode statistics too), the float and view outputs (`derived_hash`) and the bench
pixel checksums were compared before and after on every affected corpus file; the counts are in
each commit message. Instruction counts are callgrind Ir of `decode_bench`'s timed region, one
thread, on `nexbench`. The four 0c8129b rows compare release builds (the workspace profile) of
one harness source, the current `decode_bench.rs` without `--order-rays`, built against
`c3be188`'s crates and against ac71f48; the first version of this table took its before column
from a profiling build in which the old `to_physical` was inlined into `timed_work`, which
overstated these gains (KTLX 2024 float: 4,392.8 M before; dkrom float: 930.3 M; KTLX 2013
float: 2,342.2 M). The instruction ratios are 0.410, 0.310, 0.456 and 0.299; the wall-clock
ratios were measured with release builds and stand. Wall clock is `ab.py`: Windows, one logical
CPU at high priority, `c3be188` against this branch, 5 interleaved rounds, the ratio is the
median of the per-round ratios (range in brackets).

| commit | change | case | instructions before | after | wall before | after | ratio |
|---|---|---|---:|---:|---:|---:|---|
| 32e7026 | JMA: levels mapped while the DRT 5.200 run-length stream expands (no `u16` level plane), byte-wide streams without the bit reader, 64-bit bit reader otherwise | N5 tar, 20 stations | 9,078,230,306 | 1,255,770,989 | 636.9 ms | 234.9 ms | 0.369 (0.348-0.431) |
| | | N5 TAKA (RS47773) | 421,904,502 | 57,452,754 | 29.8 ms | 10.6 ms | 0.355 (0.300-0.428) |
| 0c8129b | core: `Field::to_physical` per code (8-bit and large 16-bit fields through the coding's own decode table) | KTLX 2024, decode + every field to float32 | 3,632,809,797 | 1,488,865,382 | 686.8 ms | 482.1 ms | 0.723 (0.676-0.799) |
| | | KTLX 2013 .gz, the same (with d5d7341 and e70bf43) | 1,902,045,768 | 588,886,372 | 202.0 ms | 96.1 ms | 0.477 (0.417-0.490) |
| | | dkrom ODIM, the same | 774,887,071 | 353,015,703 | 49.5 ms | 22.2 ms | 0.447 (0.426-0.483) |
| 0c8129b | core: FM301 `Values::materialize` writes each output value once per row (block copy at stride 1) instead of fill + one slice fill per gate | KTLX 2024, decode + xradar-default view materialized | 4,121,081,970 | 1,231,178,840 | 594.6 ms | 456.3 ms | 0.767 (0.729-0.772) |
| d5d7341 | Level II: gzip parsed while inflating through a reused 1 MiB window; uncompressed input parsed in place | KTLX 2013 .gz (45 MB inside) | 475,222,423 | 433,380,462 | 57.2 ms | 46.8 ms | 0.810 (0.723-0.826) |
| | | KLIX 2005 .gz (Message 1) | 260,143,732 | 244,624,983 | 29.5 ms | 25.9 ms | 0.859 (0.802-0.881) |
| 16c88de | Level II: metadata record of a whole-file bzip2 volume from its first blocks only | KTLX 2013 recompressed `bzip2 -9`, `read_volume_with_metadata` | 1,528,204,990 | 847,447,163 | | | |
| 10cdd57 | DORADE: 16-bit words converted per byte order; gate-count check without formatting its error context | NOXP 2009 sector | 12,667,812 | 7,174,676 | 1.9 ms | 1.5 ms | 0.791 (0.753-0.822) |
| | | DOW6 RHI | 16,630,376 | 10,202,081 | 3.6 ms | 3.0 ms | 0.840 (0.751-0.853) |
| 0c4d826 | render: exact fast `mul_add`, `round` and azimuth bin for every viewport and raster pixel | recast-radar-bench KTLX 2024, warmup + 1 iteration (decode + 6 viewports x 2 fields) | 11,984,375,591 | 8,697,484,846 | | | |
| | | its raster thread | 9,084,652,943 | 5,797,721,810 | | | |
| | | reflectivity raster stage (3 viewports), `RAYON_NUM_THREADS=1`, KTLX 2024 | | | 254.5 ms | 210.3 ms | 0.827 (0.788-0.844) |
| | | the same, KILX 2026 | | | 248.6 ms | 207.3 ms | 0.819 (0.803-0.836) |
| | | the same, KTLX 2013 | | | 237.7 ms | 199.7 ms | 0.835 (0.811-0.887) |
| b7ebb14 | core: `fm301::order_rays_for_view` (opt-in, in place) | KTLX 2024: zero-copy fields under the xradar default | 0 of 104 | 76 of 104 | | +29.9 M instructions | |
| e70bf43 | Level II: fields outgrowing their reservation grow by an eighth, not double | KLIX 2005 field capacity | 19.5 MB | 11.0 MB | | | |
| ac71f48 | Level II: whole-file bzip2 parsed while it is decoded block by block (the `bzip2_prefix` block split, now with a four-byte-stride magic scan and a one-pass block copy) | KTLX 2013 as `bzip2 -9`, `read_volume_from_bytes` | 776,926,395 | 808,460,850 | 537.1 ms | 484.3 ms | 0.955 (0.682-1.299); A is 639dd22, 10 rounds |
| | | the same, `read_volume_with_metadata` | 847,483,734 | 862,200,151 | 667.6 ms | 679.6 ms | 1.037 (0.663-1.691); A is 639dd22, 10 rounds |

Peak RSS of one decode (`nexbench`, GNU time `%M`, `decode_bench --from-path --threads 1`,
`run.py rss-threads`, 3 runs each; the range when they differ), a `c3be188` build against a
build of this branch with the fix pass and the bounded look-ahead experiment (which does not
change a one-thread decode), MiB (KiB):

| file | before | after | minor faults before | after |
|---|---:|---:|---:|---:|
| KTLX20130520 .gz | 95.5 (97,792) | 53.8 (55,040) | 23,801 | 13,112 |
| KLIX20050829 .gz | 34.8 (35,584) | 18.8 (19,200) | 8,256 | 4,148 |
| KTLX20240315 (LDM bzip2) | 95.0-95.2 (97,280-97,536) | 95.2-95.5 (97,536-97,792) | 23,699 | 23,700 |

The first version of this table gave thousands of KiB labelled MB (97.5 "MB" was 97,5xx KiB);
the reductions it reported are the same.

Whole-file bzip2 (ac71f48 against 639dd22, the same way, 3 runs each, within 8 KiB of each
other): KTLX 2013 as `bzip2 -9` (7.6 MB, 44.9 MB inside), 100,608 KiB to 60,440 KiB (98.3 to
59.0 MiB), minor faults 24,561 to 14,677; with `read_volume_with_metadata` 100,864 to 61,008
KiB. The block-wise decode costs 4.1% more instructions (1.7% with the metadata reader, whose
metadata record already came from the first blocks), and the wall-clock ratios are within
this host's noise.

Second fix pass. The record sources of the Level II parser (`SliceRecordBytes`,
`GzipRecordBytes`, `Bzip2RecordBytes`) return an error instead of panicking when asked for bytes
outside what they hold (bytes already released, or past the end), which the parser never does
but nothing enforced. Callgrind, `decode_bench`'s timed region, one thread, 75f5db6 against the
fix pass: KTLX 2013 gzip 433,394,321 to 433,511,351 (+0.03%), KLIX 2005 gzip 244,754,113 to
244,856,351 (+0.04%), KTLX 2013 as whole-file bzip2 808,461,967 to 808,570,382 (+0.01%), KTLX 2024 (LDM, which does not use them) 1,211,396,280 to
1,211,407,811 (`cg_fix2.txt`). Output: `decode_bench` built from 639dd22 and from the fix pass
on 243 files (34 cached Level II downloads, 16 committed Level II fixtures, the 8 Level II
cases of the cross-library run, KTLX 2013 and KLIX 2005 recompressed as whole-file bzip2, and
183 converted Level II files that are no longer in the corpus), each with `--debug-hash` on the default
pool and on 2 threads and with `--physical --view`: 729 runs per build, 729 identical, of which 12 identical errors (a model-data `_MDM`
file and three non-radar files). The three bench pixel checksums are
unchanged (0xc04a5e2dfecc4c1f / 0xd5080047ae5dfeb5 / 0x19e3735f42cdca4b, Windows, fat LTO).

Third fix pass. (1) The decoded-record buffer pool keeps at most 8 buffers instead of 64
(section "Level II peak RSS against the nexrad crate": 42-50 MiB off the default-pool peaks of
the large LDM volumes; `ab.py --threads 0` ratios 0.99-1.08, every range spanning 1). (2) The
metadata record of a whole-file gzip or bzip2 wrapper around LDM records is now the first LDM
record taken from the wrapper's first blocks, where the second fix pass decoded those blocks
and then the whole wrapper again (the review's finding): no block is decoded twice, and damage
or truncation past the first record no longer fails `metadata_record`, as it already did not
for raw records; damage inside the first blocks fails as before, with the metadata record's
context (`whole-file compressed Level II metadata record: bzip2: ...`). Of 144 wrapped, damaged
and truncated variants of six Level II files (gzip, bzip2 -1 and -9; damage at 64 bytes and 2,
18, 78 and 95% of the stream; truncation at 18 and 60%), 52 changed from an error to the
undamaged file's record, and the other 92 are identical. One of those 92 returns a record that
differs from the undamaged one on both builds: an uncompressed Level II file as gzip damaged at 2%,
because the prefix of a deflate stream carries no checksum before the gzip trailer, so damage
inside it goes undetected (as on `main`; a bzip2 prefix is CRC-checked per block). Output:
`decode_bench` built from dc3fdf7 and from the fix pass on 244 files (the 243 of the second fix
pass and one more converted file), each with `--debug-hash` on the default pool and on 2
threads and with `--physical --view`: 732 runs per build, 732 identical (12 identical errors).

Fourth fix pass (the review of a9a3ff6). (1) A gzip or whole-file bzip2 copy of an
LDM-compressed Level II file decoded to garbage without an error, on `main` as on this branch:
the wrapper was expanded and the LDM control words and bzip2 bytes parsed as messages (KTLX 2024
gzipped or bzip2'd whole: 5 sweeps, 7 radials and 12,605 gates instead of 20 sweeps, 11,520
radials and 64.6 million gates, through `read_volume_from_bytes`, `read_volume_with_metadata`
and the router). Every entry point now recognises LDM records after the volume header (a byte
count, then `BZh`, where uncompressed records have their 12-byte control word), expands the
wrapper whole (about the size of the file inside, 10.8 MB for KTLX 2024) and decodes the records
as the unwrapped file's are, with the wrapper as the volume's `provenance.compression`:
`read_volume_from_bytes`, `read_volume_with_metadata`, `read_gzip_volume_from_reader`, the gzip
preview decoders, `normalize_archive_bytes` and `read_normalized_volume_bytes`, which the router
calls with its gzip expansion (0af97c2). Py-ART 2.3.0 and MetPy read both wrapped copies as 20
sweeps and 11,520 radials too. An LDM file cut short inside a record fell back to the same
misparse (KTLX 2024 cut at 50% or 90% of its bytes: the same 5 garbage sweeps; cut inside its
first record: an empty volume). It now decodes to the records before the cut, as a damaged
record does (7 sweeps and 4,800 radials at 50%, 15 sweeps and 9,240 radials at 90%, what Py-ART
2.3.0's `read_nexrad_archive` returns from the same files; MetPy 1.7.1 fails on both), and a cut
inside the first record is a `NexradError::Truncated` error; the metadata record frames only the
first LDM record, so a truncated file keeps its metadata. The tests wrap and cut the committed
KTLX 2024 trim (`ldm_records_inside_a_whole_file_wrapper_decode_like_the_unwrapped_file`,
`ldm_file_cut_short_decodes_the_records_before_the_cut`). Wall clock of the paths the fix
touches without changing their output, `ab.py --threads 1`, a9a3ff6 against 0af97c2, 6 rounds of
5 decodes, pinned at high priority on Windows, median of the per-round ratios (range): KTLX 2013
gzip 1.003 (0.952-1.090), KLIX 2005 gzip 1.001 (0.840-1.039), KTLX 2024 LDM 0.983 (0.949-1.071),
and through the router KTLX 2013 gzip 0.991 (0.944-1.077). KTLX 2013 as whole-file bzip2 -9 read 1.115 (1.045-1.154) in that
run and 0.983 (0.703-1.807) in a 10-round rerun of the two whole-file bzip2 inputs (bzip2 -1:
0.996, 0.952-1.012; fastest samples 0.990 and 1.008), so no cost is measurable
(`fix4-ab-ldmfix-*.jsonl.gz`). (2) `fm301::order_rays_for_view` checks every sweep before it
moves any rows (24baac7): a sweep with a per-ray item of the wrong length used to fail after the
sweeps before it were reordered, and now leaves the whole volume in storage order
(`fm301_view::ordering_rays_for_the_view_changes_nothing_when_a_sweep_fails`, which fails on the
previous version). Output: `decode_bench` built from a9a3ff6 and from 2a3365f (the fast release
profile for both) on 108 files: 71 Level II inputs (35 cached Level II downloads and the first
nine real-time chunks of KIWA volume 307, the committed Level II fixtures, the three Level II
perf-corpus volumes, KTLX 2013 as whole-file bzip2 at levels 1 and 9, the two KIWA chunk
sequences, and the five new inputs: KTLX 2024
gzipped whole, bzip2'd whole, and cut at 50%, 90% and 1,028 bytes), each through `l2` with
`--debug-hash` on the default pool and on 2 threads, `l2-meta` and the router with
`--debug-hash`, and `l2` with `--physical --order-rays --view`; and the 37 committed ODIM,
CfRadial, DORADE, JMA and Level III fixtures with `--debug-hash` and with `--physical
--order-rays --view`: 429 runs per build, 404 identical (69 of them identical errors on 21
files that are not volumes this branch decodes: intermediate real-time chunks without a volume
header, a model-data file, ODIM `IMAGE` products and a saved HTTP response, a netCDF-4
CfRadial file, a zipped DORADE archive and graphic Level III products), and the 25 others are
exactly the five new inputs in their five modes. The two wrapped copies now give the unwrapped
file's gate hash (`8cc57ef6f26a6bb4`) and its float and ordered-view hash (`15ca00b3920e7ab9`);
their whole-model hashes differ from it only by `provenance.compression`. The three bench pixel
checksums are unchanged (Windows, fat LTO).

LDM bzip2 decoding, 96% of a Level II decode's instructions (`recast-radar-bzip2`), is another
stream's crate; the Level II parse around it is 4% (KTLX 2024: `parse_message_31` 49 M of
1,211 M instructions, 0.55 instructions per gate for the row copies), so this pass left it.

## paired-bzip2: off by default; one thread is an open question

Decision: the `paired-bzip2` feature (two LDM records decoded in lockstep per thread) stays
off by default, because a default decode runs on the rayon pool, where paired decoding is
slower, and because it doubles every decoding thread's bzip2 workspace. Whether decodes on a
one-thread pool should decode paired (a switch on the pool size at run time rather than a
cargo feature) is not decided: the one-thread measurements disagree, and none of the wide ones
was made on a quiet host.

The 19 LDM bzip2 volumes of the corpus (every downloaded `AR2V` volume),
`ab.py`, Windows, release builds of this branch without and with the feature, 5 interleaved
rounds; one thread: `--threads 1`, one logical CPU, high priority, 5 decodes per process;
default pool: 32 threads, no affinity, 10 decodes per process. Every run decoded the same
fields. The ratio is paired over off.

| volume | 1 thread: off ms | paired ms | ratio (range) | fastest samples | default pool: off ms | paired ms | ratio (range) | fastest samples |
|---|---:|---:|---|---:|---:|---:|---|---:|
| l2-kbox-20220129-150537 | 484.6 | 645.3 | 1.280 (1.002-1.680) | 1.010 | 88.3 | 158.4 | 1.705 (1.005-1.956) | 1.625 |
| l2-kdgx-20230325-010651 | 533.1 | 550.6 | 1.014 (0.976-1.089) | 0.968 | 133.3 | 144.8 | 1.102 (0.738-1.505) | 1.085 |
| l2-kdvn-20200810-175718 | 518.7 | 564.3 | 1.079 (0.938-1.221) | 0.978 | 103.1 | 96.9 | 0.896 (0.773-1.149) | 0.949 |
| l2-kdvn-20200810-180401 | 515.1 | 536.2 | 1.021 (0.998-1.122) | 1.015 | 90.6 | 122.5 | 1.328 (0.997-2.000) | 1.258 |
| l2-kdvn-20200810-181043 | 559.4 | 616.6 | 1.136 (1.026-1.218) | 1.050 | 86.8 | 119.3 | 1.401 (0.671-1.553) | 1.015 |
| l2-kdvn-20200810-181724 | 545.5 | 627.1 | 1.082 (1.046-1.197) | 0.973 | 101.1 | 128.5 | 1.279 (1.193-1.442) | 1.353 |
| l2-kilx-20260418-013553 | 1097.5 | 1580.3 | 1.276 (1.036-1.560) | 1.099 | 188.3 | 207.8 | 1.220 (0.781-1.583) | 1.246 |
| l2-kiwa-20260917-003629 | 497.8 | 504.4 | 1.013 (0.967-1.278) | 0.980 | 104.0 | 151.0 | 1.855 (1.141-2.011) | 1.929 |
| l2-klix-20210829-173117 | 819.3 | 916.6 | 1.052 (1.019-1.157) | 0.955 | 135.7 | 221.4 | 1.463 (1.161-1.857) | 1.340 |
| l2-klix-20210829-175748 | 923.3 | 1086.0 | 1.112 (0.799-1.565) | 0.879 | 163.4 | 160.9 | 1.046 (0.690-1.084) | 1.262 |
| l2-klix-20210829-180425 | 828.2 | 989.5 | 1.129 (0.995-1.328) | 0.980 | 159.7 | 177.7 | 1.112 (0.685-1.557) | 0.814 |
| l2-kmaf-20230331-230843 | 98.0 | 98.7 | 1.004 (0.933-1.069) | 0.973 | 37.2 | 35.6 | 0.947 (0.403-1.401) | 1.066 |
| l2-kmtx-20240301-212827 | 329.6 | 334.6 | 0.983 (0.966-1.093) | 0.977 | 71.8 | 81.1 | 1.244 (0.818-1.624) | 1.150 |
| l2-ktlx-20240315-000217 | 419.4 | 447.7 | 1.035 (0.956-1.192) | 0.959 | 88.3 | 96.8 | 1.024 (0.746-1.096) | 0.966 |
| l2-ktlx-20240515-000014 | 147.2 | 148.2 | 0.997 (0.960-1.039) | 1.011 | 44.0 | 60.1 | 1.145 (0.853-1.885) | 1.121 |
| l2-pahg-20250909-212549 | 469.9 | 468.7 | 0.980 (0.900-1.083) | 0.925 | 89.3 | 116.2 | 1.162 (1.019-1.554) | 1.272 |
| l2-pgua-20230524-030945 | 844.9 | 1374.1 | 1.370 (0.928-1.799) | 0.865 | 153.2 | 186.5 | 1.553 (1.018-1.753) | 1.557 |
| l2-tjua-20220918-190621 | 977.9 | 1025.9 | 0.890 (0.828-1.499) | 1.124 | 138.3 | 182.6 | 1.219 (1.098-1.871) | 1.682 |
| l2-tstl-20230331-230314 | 96.0 | 97.0 | 1.001 (0.962-1.078) | 0.976 | 21.9 | 16.7 | 0.727 (0.663-1.326) | 1.179 |

Across the 19 volumes the per-file ratio has a median of 1.035 and a geometric mean of 1.071
on one thread, and 1.219 and 1.204 with the default pool. The first version of this section
read the one-thread figures as "no gain on one core, a small loss". They measure the load
instead: the per-round ratios of most files straddle 1, and the ratio of the two builds'
fastest samples over all rounds (`ab.py --summarize`, the figure least affected by preemption,
which only adds time) has a geometric mean of 0.982 on one thread. With the default pool the
loss holds in the fastest samples too (1.228).

| run | host | one thread, paired / off | default pool, paired / off |
|---|---|---|---|
| quiet-host study (`docs/perf/single-core.md`) | quiet | KILX 2026 0.86, KTLX 2024 no change | not measured |
| the 19 volumes above, Windows | CPU 90-100% | per-file medians 0.89-1.37 (geometric mean 1.071); fastest samples 0.982 | 0.73-1.86 (1.204); fastest samples 1.228 |
| fix-pass review, `nexbench`, 5 rounds of 5 decodes (numbers from the review; its rounds are not in this repository) | container load about 1 | KILX 2026 0.878, KTLX 2024 1.010, KBOX 0.988, PGUA 0.960 | 1.139, 1.163, 1.216, 0.999 |
| fix pass, `nexbench`, the 19 volumes, 6 of 9 rounds (stopped: the host was not quiet; `paired_1t_loaded.jsonl.gz`) | host CPU 54-100% (median 95%), container load 0.3-15, throttled | per-file medians 0.63-1.29 (geometric mean 0.980); fastest samples 0.997 | not run |
| fourth fix pass, `nexbench`, the 19 volumes, 9 rounds of 5 decodes, builds of 24baac7 (`fix4-paired-1t-linux.jsonl.gz`) | host CPU 97-98% in a sample during the run (other agents' builds), container load 1.3-4.1, no throttled period | per-file medians 0.856-1.097 (median 0.974, geometric mean 0.970); fastest samples 0.955 (KILX 2026 1.097 and 1.003) | not run |

So with the default pool paired decoding is slower in every run that measured it (1.14-1.22 on
three of the review's four volumes, 1.20-1.23 over the 19), and on one thread it is neutral to a
small gain: 12-14% on KILX 2026 in the two quietest runs, no significant change elsewhere, and
3-5% over the 19 volumes in the fourth fix pass's run (geometric means 0.970 of the round
medians and 0.955 of the fastest samples), where KILX 2026 did not gain (1.097, fastest 1.003).
Paired decoding overlaps the memory-latency-bound inverse-BWT chases of two records; with many
threads claiming records two at a time it halves the scheduling granularity (the last records
finish later) and doubles the resident workspaces. In the cross-library runs (3 rounds, pinned,
as loaded as the rest of this page) it was 0.76x on KILX 2026 and 1.19x on KTLX 2024 on Linux,
and 0.80x and 1.14x on Windows.

**Owner decision:** whether one-thread decodes should decode paired. It needs the 19-volume
one-thread run on a quiet host (`ab.py --threads 1` with both builds; `ab.py --summarize`
reports each file's load range from the log). None of the fix passes could make it: the host CPU
was at 54-100% (median 95%) during the second (logged every 10 s, `host-cpu-fix2.log`), whose
run was stopped after six rounds, and at 97-98% when sampled during the fourth's, which ran all
nine rounds in an unthrottled container and leans towards paired (3-5%) without deciding it: its
per-round ratios span 1 on every file.

## Level II peak RSS against the nexrad crate

The open question from the wave 2 perf study. Peak RSS of one decode per process, the file
bytes included (`run.py rss-threads`: recast with `--threads N`, the nexrad crate with
`RAYON_NUM_THREADS=N`, no affinity; the libraries and thread counts in alternating order).
Every cell is the range over the runs, in MiB. The nexrad crate harness is `fs::read` +
`File::new` + `decompress()` + `scan()`. The first version of this section gave one sample per
cell, and its thread-pool figures did not reproduce: a decode's peak on a thread pool depends on
the host's load (below). In `nexbench` the default pool is 12 threads (the container's CPU
quota, see Modes), on Windows 32.

### Where a pool decode's memory went, and the fix (third fix pass)

The parser takes the decoded LDM records in order on one thread while the other pool threads
decode records (`io-nexrad`'s `BlockSlots`). From about a dozen threads on, the parser is the
slower side: an instrumented build (`pool-experiments.patch`, `RECAST_EXP_PRINT`;
`parser-instrumentation-win.log`, Windows, 4 decodes per row) shows the parser's own work at
16-37 ms per KTLX 2024 volume at every thread count (its time minus the records it decoded
itself and its waits), while up to 63-74 of the volume's 97 decoded records waited for it at 12
threads and 82-89 at 32 (KILX 2026: 50-100 and 98-101 of 106). A record's buffer used to go
back to a process-wide pool of up to 64 decoded-record buffers (64 MiB) when the parser was done
with it, so as the parser drained that backlog the buffers stayed resident beside the growing
volume, and the backlog's high-water mark was still held at the end of the decode, when the
volume is complete. The pool now keeps at most 8 buffers (16 MiB) and frees the rest as the
parser finishes them, so a decoded record's memory turns into the volume's instead of adding to
it. One-thread decodes cycle two or three buffers and are unchanged. The output is the same
(the identity check under "This pass's optimisations").

Peak RSS, this branch (`recast`), the same decode before the change (`before`, pool of 64:
the second fix pass's build) and the nexrad crate, 5 runs per cell for KTLX 2024 and KILX 2026
and 3 for the others (`fix3-rss-threads-linux.jsonl.gz`, `fix3-rss-threads-win.jsonl.gz`):

| file | threads | Linux recast | before | nexrad crate | Windows recast | before | nexrad crate |
|---|---:|---:|---:|---:|---:|---:|---:|
| KTLX20240315_000217_V06 | 1 | 95.2-95.5 | 95.2-95.5 | 100.9-101.0 | 96.4-96.8 | 96.4-96.9 | 107.0-107.4 |
| KTLX20240315_000217_V06 | 2 | 107.2-110.7 | 108.2-117.2 | 107.2-108.9 | 108.8-112.3 | 110.6-117.2 | 106.9-107.7 |
| KTLX20240315_000217_V06 | 4 | 115.6-131.1 | 127.8-140.2 | 110.0-115.6 | 117.2-119.7 | 131.8-134.9 | 108.1-109.5 |
| KTLX20240315_000217_V06 | 8 | 123.0-137.4 | 169.5-174.3 | 117.9-126.6 | 126.7-135.2 | 150.8-180.1 | 110.9-112.7 |
| KTLX20240315_000217_V06 | 12 (Linux default) | 128.5-135.7 | 178.2-183.8 | 128.2-131.4 | | | |
| KTLX20240315_000217_V06 | 16 | 137.1-140.1 | 184.5-189.1 | 138.5-144.9 | 141.7-151.1 | 192.7-197.4 | 119.4-123.4 |
| KTLX20240315_000217_V06 | 32 (Windows default) | 160.4-163.3 | 204.5-206.5 | 181.0-205.1 | 154.1-169.3 | 201.5-215.1 | 126.9-133.2 |
| KILX20260418_013553_V06 | 1 | 110.8-111.0 | 110.5-111.0 | 116.6-116.9 | 112.0-112.0 | 112.0-112.1 | 122.7-123.1 |
| KILX20260418_013553_V06 | 2 | 123.9-125.2 | 129.0-132.8 | 124.5-125.6 | 124.4-126.4 | 124.2-130.9 | 122.7-123.4 |
| KILX20260418_013553_V06 | 4 | 137.5-147.4 | 148.2-158.0 | 128.8-132.5 | 135.8-137.8 | 142.8-155.7 | 124.1-125.1 |
| KILX20260418_013553_V06 | 8 | 152.4-161.6 | 195.0-201.8 | 134.8-141.1 | 155.3-162.9 | 182.6-193.0 | 128.5-135.4 |
| KILX20260418_013553_V06 | 12 (Linux default) | 162.0-176.5 | 212.8-217.8 | 145.8-150.6 | | | |
| KILX20260418_013553_V06 | 16 | 177.2-183.1 | 224.5-229.5 | 160.5-166.7 | 183.3-193.4 | 232.8-237.1 | 138.0-150.9 |
| KILX20260418_013553_V06 | 32 (Windows default) | 225.4-229.8 | 273.5-278.3 | 215.0-229.9 | 222.4-232.6 | 260.0-282.6 | 155.7-167.0 |
| KIWA307_chunks001-035 | 1 | 40.8-41.0 | 41.0 | 40.5 | 42.4 | 42.4-42.4 | 43.2 |
| KIWA307_chunks001-035 | default (Linux 12, Windows 32) | 80.7-83.9 | 98.0-99.0 | 78.2-80.9 | 108.8-116.3 | 121.9-134.7 | 67.4-68.8 |
| KIWA307_chunks001-003 | 1 | 10.2-10.5 | 10.5 | 9.1 | 11.8-11.9 | 11.9-11.9 | 10.5 |
| KIWA307_chunks001-003 | default (Linux 12, Windows 32) | 13.0-13.5 | 13.2-13.8 | 10.5-10.8 | 15.9-16.0 | 15.9-15.9 | 12.1 |
| KTLX20130520_201643_V06.gz | 1 | 53.8 | 53.8 | 133.9 | 54.9 | 54.9 | 139.3-139.4 |
| KTLX20130520_201643_V06.gz | default (Linux 12, Windows 32) | 53.8 | 53.8 | 134.1-134.4 | 54.9 | 54.9 | 140.3-140.4 |
| KLIX20050829_130035.gz | 1 | 18.8 | 18.8-19.0 | 38.4 | 20.6-20.8 | 20.8 | 41.5-41.6 |
| KLIX20050829_130035.gz | default (Linux 12, Windows 32) | 18.5-18.8 | 18.8 | 38.7 | 20.6 | 20.6 | 41.9 |

With its default pool, recast now peaks at the nexrad crate's level on KTLX 2024 in `nexbench`
(128.5-135.7 against 128.2-131.4 MiB),
level on the 35-chunk real-time volume (80.7-83.9 against 78.2-80.9) and below it on every
one-thread decode of a complete volume, the gzip, uncompressed and whole-file bzip2 inputs on
any pool, and KTLX 2024 at 32 threads on Linux (160.4-163.3 against 181.0-205.1; KILX 2026 is
level there, 225.4-229.8 against 215.0-229.9). It is still above the nexrad crate on KILX 2026
at 12 threads (162.0-176.5 against 145.8-150.6) and on every LDM volume with Windows' 32-thread
default pool (KTLX 2024 154.1-169.3 against 126.9-133.2, KILX 2026 222.4-232.6 against
155.7-167.0, KIWA 35 chunks 108.8-116.3 against 67.4-68.8), where the nexrad crate's peaks are
lower than on Linux because the Windows heap returns its per-record libbzip2 buffers to the
system. The change took 42-50 MiB (medians of the runs) off the default-pool peaks of the two
large volumes on both hosts.

Wall clock of the change, `ab.py --threads 0` (the default pool), the build before against this
branch, 10 rounds of 10 decodes per file, the median of the per-round ratios (range) and the
ratio of the fastest samples (`fix3-ab-pool-mt-win.jsonl.gz`, `fix3-ab-pool-mt-linux.jsonl.gz`):

| host, pool | KTLX 2024 | KILX 2026 | KIWA307_chunks001-035 | geometric mean |
|---|---|---|---|---|
| Windows, 32 threads | 1.024 (0.711-1.492); 1.078 | 1.041 (0.840-1.196); 0.931 | 1.047 (0.656-1.245); 0.866 | 1.038; fastest 0.954 |
| Linux, 12 threads | 1.002 (0.797-1.798); 0.900 | 1.080 (0.841-1.548); 0.969 | 0.985 (0.696-1.456); 1.182 | 1.022; fastest 1.010 |

Every range spans 1: no cost is measurable on this host. What the smaller pool can cost is
page faults of fresh buffers in the decoding threads when one process decodes volume after
volume on many threads (the first decode in a process allocates them either way). A study of
the pool size by environment variable in the instrumented build (8, 16, 33 and 64 buffers,
`fix3-pool-cap-win.jsonl.gz`, `fix3-pool-cap-linux.jsonl.gz`, 4-10 interleaved rounds) found
the same: 8 and 16 buffers peak within 8 MiB of each other and 30-55 MiB below 64, and no pool
size was consistently faster, in the round medians or in the fastest samples, on either host.

### What remains: one bzip2 workspace per decoding thread

The rest of the difference is structural. Each decoding thread keeps a
`recast_radar_bzip2::Decoder` in a thread-local (`BZIP2_DECODER`) whose block workspace (about
7 MiB of address space: a 4 MiB inverse-BWT vector, a 1 MiB byte vector and run tables) is
resident as far as the largest record touched it, about 2.5-4.5 MiB for these volumes, and all
of them are alive while the last records decode and the volume is nearly complete. The nexrad
crate decompresses every record first and parses afterwards, so its libbzip2 state (about 3.6
MB per record, allocated and freed per record) is gone before its volume is complete; its peak
holds the whole expanded volume instead. Recreating the decoder per record, as libbzip2 does,
would fault its pages in again for every record (the cost behind the nexrad crate's one-thread
times below); the workspace size is `recast-radar-bzip2`'s.

One-decode wall clock of the same runs (median of the 5 runs, one decode per process, ms):

| file | threads | Linux recast | nexrad crate | Windows recast | nexrad crate |
|---|---:|---:|---:|---:|---:|
| KTLX20240315_000217_V06 | 1 | 669 | 1149 | 370 | 792 |
| KTLX20240315_000217_V06 | 2 | 325 | 555 | 219 | 428 |
| KTLX20240315_000217_V06 | 4 | 206 | 312 | 124 | 232 |
| KTLX20240315_000217_V06 | 8 | 131 | 190 | 75 | 131 |
| KTLX20240315_000217_V06 | 12 (Linux default) | 120 | 175 | | |
| KTLX20240315_000217_V06 | 16 | 119 | 177 | 63 | 95 |
| KTLX20240315_000217_V06 | 32 (Windows default) | 124 | 172 | 68 | 84 |
| KILX20260418_013553_V06 | 1 | 1483 | 2678 | 992 | 1808 |
| KILX20260418_013553_V06 | 2 | 922 | 1386 | 647 | 1007 |
| KILX20260418_013553_V06 | 4 | 697 | 886 | 253 | 582 |
| KILX20260418_013553_V06 | 8 | 317 | 418 | 181 | 291 |
| KILX20260418_013553_V06 | 12 (Linux default) | 298 | 404 | | |
| KILX20260418_013553_V06 | 16 | 246 | 327 | 126 | 190 |
| KILX20260418_013553_V06 | 32 (Windows default) | 273 | 494 | 129 | 164 |

**Owner decision: how many threads decode.** Past the parser's rate, more decoding threads add
workspaces and no speed: on this host KTLX 2024 decodes in about the same time with 16 threads
as with 32 (63 against 68 ms one-decode on Windows; in warm processes,
`fix3-thread-sweep-win.jsonl.gz`, 74.6 against 70.6 ms, fastest samples 40.0 against 35.5), for
142-151 against 154-169 MiB. A cap on the decoding threads, whatever the pool size, is the
remaining lever: at 8 threads recast is at the nexrad crate's default-pool peak on KTLX 2024 on
both hosts (123.0-137.4 and 126.7-135.2 MiB) and still decodes it faster (131 against 175 ms on
Linux, 75 against 84 ms on Windows), but on KILX 2026 8 threads take 1.06x (Linux) to 1.40x
(Windows) the one-decode time of the default pool. Which cap (none, 16, or a smaller one) is
worth its time on a quiet many-core host is the owner's call; a caller can already cap it per
call by running the decode inside a smaller rayon pool (`ThreadPool::install`), which is what
`decode_bench --threads N` does.

### A second lever: freeing each worker's workspace when it runs out of records (fourth fix pass)

The fourth fix pass measured the other way to take the workspaces out of the peak: a pool worker
that finds no record left to claim replaces its thread-local decoder with a new one (which
allocates nothing until it decodes), so its workspace is freed while the parser is still taking
the decoded records and the volume is still growing (`fix4-relws.patch`: a call at the end of
`BlockSlots::run_worker` and a ten-line helper; not merged; the builds of both sides have the
same `io-nexrad` sources otherwise). Peak RSS of one decode per process, `run.py rss-threads`,
the patched build (`relws`) against this branch (`keep`) and the nexrad crate, 5 runs per cell
for KTLX 2024 and KILX 2026 and 3 for the others, MiB (`fix4-rss-relws-linux.jsonl.gz`,
`fix4-rss-relws-win.jsonl.gz`):

| file | threads | Linux keep | relws | nexrad crate | Windows keep | relws | nexrad crate |
|---|---:|---:|---:|---:|---:|---:|---:|
| KTLX20240315_000217_V06 | 1 | 95.2-95.5 | 95.5 | 100.7-100.9 | 96.4-96.9 | 96.4-96.9 | 107.1-107.6 |
| KTLX20240315_000217_V06 | 4 | 117.1-122.5 | 110.1-112.5 | 113.6-115.8 | 117.4-120.1 | 110.9-115.0 | 108.8-109.4 |
| KTLX20240315_000217_V06 | 8 | 131.2-134.3 | 118.5-122.3 | 120.9-124.7 | 127.4-130.2 | 117.1-129.0 | 110.5-111.8 |
| KTLX20240315_000217_V06 | default (Linux 12, Windows 32) | 130.1-133.8 | 122.0-126.5 | 124.7-132.3 | 165.4-170.3 | 118.3-129.6 | 126.6-131.4 |
| KILX20260418_013553_V06 | 1 | 110.8-111.0 | 110.8-111.0 | 116.6-116.9 | 111.9-112.0 | 112.0-112.1 | 122.7-123.0 |
| KILX20260418_013553_V06 | 4 | 134.8-138.6 | 133.2-135.7 | 129.9-132.6 | 133.7-137.1 | 128.2-136.7 | 124.0-125.0 |
| KILX20260418_013553_V06 | 8 | 155.1-168.0 | 146.8-154.8 | 135.3-142.7 | 154.2-163.9 | 145.2-159.7 | 127.9-133.4 |
| KILX20260418_013553_V06 | default (Linux 12, Windows 32) | 162.9-178.8 | 153.8-171.3 | 142.9-145.8 | 219.6-234.0 | 189.8-201.3 | 151.3-165.8 |
| KIWA307_chunks001-035 | default (Linux 12, Windows 32) | 81.0-81.5 | 63.8-67.2 | 79.2-81.4 | 112.7-115.8 | 62.4-72.1 | 63.9-69.5 |

With the default pool that puts recast at or below the nexrad crate's peak on KTLX 2024
and the 35-chunk real-time volume on both hosts (medians of the runs 17-49 MiB below `keep` on
Windows, 6-16 MiB on Linux); KILX 2026 stays above it (Linux 153.8-171.3 against 142.9-145.8,
Windows 189.8-201.3 against 151.3-165.8). One-thread decodes do not change. The price is paid by
the next decode in the same process, which allocates and faults every workspace again: `ab.py`,
`keep` (A) against `relws` (B), 10 decodes per process after one warmup, median of the per-round
ratios (range) and the ratio of the fastest samples (`fix4-ab-relws-mt-win.jsonl.gz`,
`fix4-ab-relws-4t-win.jsonl.gz`, `fix4-ab-relws-mt-linux.jsonl.gz`):

| host, pool | KTLX 2024 | KILX 2026 | KIWA307_chunks001-035 | geometric mean |
|---|---|---|---|---|
| Windows, 32 threads, 10 rounds | 1.138 (0.836-1.361); 1.108 | 1.253 (0.901-1.618); 1.178 | 1.107 (0.822-1.786); 1.316 | 1.164; fastest 1.198 |
| Windows, 4 threads, 6 rounds | 1.076 (0.918-1.308); 1.008 | 1.024 (0.861-1.137); 1.058 | 1.050 (0.976-1.177); 1.030 | 1.050; fastest 1.032 |
| Linux, 12 threads, 10 rounds (container load 4.2-7.6; 42 throttled 100 ms periods over the peak RSS and A/B runs, `fix4-linux-driver.log`) | 1.154 (0.964-1.307); 1.076 | 1.053 (0.961-1.218); 1.004 | 1.125 (0.760-1.345); 1.186 | 1.110; fastest 1.086 |

A process that decodes one volume and exits (a command-line conversion) pays little for it: the
same runs time that one decode, and the two builds' times overlap on every file at the default
pool. A
process that decodes volume after volume on the default pool (a server, a viewer following a
radar) pays about 1.2x the decode time. Freeing only some of the workspaces (keeping the first
few in a shared pool) would sit between the two, and a caller can already get the thread-cap
lever's memory per call (`ThreadPool::install`).

**Owner decision (restated with both levers).** The Level II default-pool peak is above the
nexrad crate's on KILX 2026 on both hosts and, without one of the levers, on every LDM volume on
Windows' 32-thread default pool. Closing it costs time either way: a cap on the decoding threads
costs one-decode time on the largest volumes (KILX 2026 at 8 threads 1.06-1.40x), while 16
threads decoded KTLX 2024 as fast as 32 on this host; freeing the workspaces as the workers
finish costs about 1.2x on every warm decode on the default pool and little for one decode per
process; both together (8 threads, `relws`) peak at 145.2-159.7 MiB on KILX 2026 on Windows,
level with the nexrad crate's default-pool 151.3-165.8. Which one, if any, belongs in the
default (or behind a caller's switch, such as a process-wide "release decoder workspaces"
setting for one-shot tools) is the owner's call; this branch keeps the workspaces and the full
pool.

The parser also decodes a record itself whenever the one it needs is not ready, which costs it
up to 19 ms (KTLX 2024) and 40-61 ms (KILX 2026) of its own time per volume even at 32 threads
(`parser-instrumentation-win.log`). Two
other rules were measured against it (`fix3-help-policy-win.jsonl.gz`, 6 interleaved rounds,
median per-round ratio): helping only when no decoded record waits was 0.93-0.96x at 16 and 32
threads but 1.05-1.08x at 4 and 8, and never helping while workers run 1.19-1.23x at 4 threads;
no rule was better at every pool size, so the shipped one stays.

**The bounded look-ahead experiment** (second fix pass, `bounded-lookahead.patch`: a worker
starts a record only while it is fewer than workers + 2 records past the one the parser takes)
is superseded. It bounded the same backlog, but held the workers back whenever one record's
decode was slow, so pool decodes were 1.07x slower on Windows and 1.45x in the throttled
`nexbench` container (`ab.py --threads 0`, bounded over the build before it, median of the
per-round ratios (range) and the ratio of the fastest samples; its peak RSS per thread count is
in `rss-window-linux.jsonl.gz` and `rss-window-win.jsonl.gz`):

| host, pool | KTLX 2024 | KILX 2026 | KIWA307_chunks001-035 | KBOX 2022 | PGUA 2023 | geometric mean |
|---|---|---|---|---|---|---|
| Linux, 12 threads, 10 rounds of 10 decodes | 1.592 (1.418-1.904); 1.860 | 1.460 (1.168-1.908); 1.647 | 1.452 (0.986-2.079); 1.305 | 1.428 (1.236-1.905); 1.455 | 1.335 (0.949-2.396); 1.603 | 1.451; fastest 1.563 |
| Windows, 32 threads, 6 rounds of 10 decodes | 1.039 (0.885-1.437); 1.053 | 1.171 (1.014-1.453); 1.121 | 1.013 (0.676-1.321); 1.048 | | | 1.072; fastest 1.073 |
With the smaller buffer pool the backlog no longer adds to the peak, and the bound on top of it
(`window-pool8` in `fix3-window-pool8-win.jsonl.gz`, 4 rounds on Windows) lowered the peak by
at most 7 MiB (KILX 2026 at 12 threads), raised it at 32 threads (KTLX 2024 175.6-177.9 against
133.6-167.6 MiB), and made decodes 1.25-2.0x slower (medians of the round medians). A wider
window (2 x workers + 4) and a speculative decode of the record the parser waits for were
slower than the unbounded default as well (`fix3-window-variants-win.jsonl.gz`, an earlier
build of the same experiment; the speculative decode is not in the patch).

## Hotspots left for the post-merge pass

Other streams are rewriting these crates, so this pass measured them and changed nothing.
Callgrind, one thread, `decode_bench`'s timed region, on the files of the tables above
(instructions, share of the decode):

**`recast-radar-io-odim` (hdf5lite).** dkrom (1,280 chunked datasets): 301 M instructions,
of which 160 M (53%) is `memset`. 84 M comes from `H5File::read_chunked` zero-filling each
output buffer (`vec![0u8; total]`) before inflating into it, 55 M from allocator-zeroed
memory and 18 M from a new zlib inflater (`Inflate::new`) for every chunk, whose window and
state are zeroed each time; `inflate_table` (16.5 M, 5.5%) is rebuilt per chunk as well.
Reusing one `flate2::Decompress` with `reset` across chunks and inflating into a reused buffer
would remove most of it. iesha (fewer, larger chunks): 86.6 M, of which inflate is 57% (zlib-rs
at its speed), `memcpy` 16% and `memset` 9%. recast is already 2.0-4.3x faster than h5py
reading the same datasets raw, so the remaining margin is in these copies and fills.

**`recast-radar-io-cfradial` (netcdf3).** S-Pol classic: 31.2 M instructions, 62% `memcpy`
(values copied through intermediate buffers in `Nc3File::read_var`) and 18% the per-value
big-endian conversion in `read_var`. Converting straight into the typed field buffer would
halve it. netCDF4 reading the same variables takes 2.1-4.0x recast's time on S-Pol (CfRadial 1
tables).

**CfRadial 2 / netCDF-4 (G4).** No reader on this branch. The targets for the reader the
hdf5-netcdf stream is writing: netCDF4 reads every variable of the S-Pol CfRadial 2 file in
127 ms pinned on Linux (239 ms as one decode per process), xradar in 3.2 s (one decode per
process; a single sample on a loaded host). Py-ART and LROSE do not read the file (Summary), so
xradar is the only reference reader that decodes it into sweeps.

**`recast-radar-io-level3`.** N0B 28.8 M, DPR 27.5 M, N0U 8.6 M, N0Q 2.8 M instructions;
the bzip2 decoder's own functions are 96.3%, 82.9%, 85.9% and 76.8% of them (callgrind self
cost of `recast_radar_bzip2::*` in `timed_work`, ac71f48 build; N0B: `decode_symbols` 57.0%,
`build_tt` 16.9%, `expand` 8.0%, `chase` 6.5%, `find_runs` 5.8%, `decode_stream_into` 1.4%).
The product's own work is small: DPR's `to_volume` 8.2% and `decode_packets` 3.8%; N0Q's
largest non-bzip2 cost is `memcpy`, 14.7%.
A first decode in a fresh process also pays for the thread-local bzip2 workspace, sized for
900 kB blocks whatever the product's block size (the `rss-1thread` rows of the Level III
tables: 27.7 ms for one N0B decode in a fresh process against a pinned median of 20.2 ms
warm; single samples on a loaded host).

**`recast-radar-io` (router).** A gzip wrapper is inflated whole to sniff the inner format
(`unwrap_containers`), and a Level II payload is then parsed from that buffer
(`read_normalized_volume_bytes`), so a routed `.gz` Level II volume still holds the full
expansion (45 MB for KTLX 2013) that `read_volume_from_bytes` no longer holds (d5d7341).
Sniffing from an inflated prefix and passing the compressed bytes to the Level II decoder
would give the router the same peak. The router has no Level III dispatch on this branch
(G6): a Level III product falls through to the Level II decoder and fails with "no Archive
II volume header" (the Router path section).

**`recast-radar-bzip2`.** 96% of an LDM Level II decode. It decodes whole streams only.
`io-nexrad` now decodes a whole-file bzip2 volume block by block itself (ac71f48: its
`bzip2_prefix` finds the block boundaries and rewraps each block as a one-block stream), but
on one thread (KTLX 2013 as `bzip2 -9`: 808 M instructions, against 433 M for the same volume
as gzip); handing those one-block streams to the LDM worker pool is an `io-nexrad` change
for the next pass. A block-level decoder API (decode one block from a bit offset and report
where it ended) would make the magic scan (23 M instructions of the 808 M) and the block
copy unnecessary. The workspace size (about 7 MiB of address space per decoder) is this
crate's; how many decoders are alive at once, and for how long, is `io-nexrad`'s (previous
section).

**JMA model storage.** JMA fields are float32 values from a level table; the 20-station N5
tar decodes to 150 million gates, 600 MB of `f32` (the 622 MiB one-decode peak). Storing the
level codes (`u8`) with a coding would cut that by four if the level tables are linear, which
is a model decision (G4's raw-storage rule) rather than a decoder one.

## Reproduction

```
# nexbench (Linux): build every harness, stage the corpus, run
bash tools/xlib-bench/build_linux.sh SRC OUT
python3 tools/xlib-bench/run.py stage --out DATA          # on the host, then copy DATA in
python tools/xlib-bench/run.py bench --data DATA --bin OUT/bin --python /opt/xlib-venv/bin/python \
    --out results-linux.jsonl --rounds 3 --modes pinned,multi,rss --cpu 26
python tools/xlib-bench/run.py table results-linux.jsonl
bash tools/xlib-bench/cg_compare.sh CGX DATA OUT/bin

# Windows
python tools/xlib-bench/run.py bench --data DATA --bin BIN --python <venv python> \
    --out results-win.jsonl --rounds 3 --modes pinned,multi,rss --cpu 22 --high-priority \
    --libs recast recast-f32 recast-paired nexrad-crate pyart metpy xradar radrs wradlib h5py netcdf4
# a lost round: the same command with --first-round N --rounds 1 --modes pinned,multi
# Python environments: tools/xlib-bench/requirements-linux.txt (nexbench) and
# requirements-windows.txt (pip freeze of each benchmark environment)

# the fix pass (docs/perf/data/perf-p1/README.md): Linux memory under GNU time, the
# real-time chunk cases and the router path, harnesses rebuilt from 639dd22
python tools/xlib-bench/run.py bench ... --out results-linux-mem.jsonl --rounds 1 \
    --modes rss,pinned-mem,multi-mem --libs recast recast-f32 recast-paired nexrad-crate \
    go-nexrad rsl lrose radrs pyart metpy xradar wradlib h5py netcdf4
python tools/xlib-bench/run.py bench ... --out results-linux-chunks.jsonl --rounds 3 \
    --modes pinned,multi --files KIWA307 --libs <the same>
python tools/xlib-bench/run.py bench ... --out results-linux-router.jsonl --rounds 3 \
    --modes pinned,multi,rss --libs recast recast-auto --files <the router cases>
python tools/xlib-bench/run.py table docs/perf/data/perf-p1/results-linux.jsonl.gz \
    docs/perf/data/perf-p1/results-linux-rssfix.jsonl.gz \
    docs/perf/data/perf-p1/results-linux-mem.jsonl.gz docs/perf/data/perf-p1/results-linux-chunks.jsonl.gz \
    docs/perf/data/perf-p1/results-linux-cfrad2.jsonl.gz
python tools/xlib-bench/run.py table docs/perf/data/perf-p1/results-win.jsonl.gz \
    docs/perf/data/perf-p1/results-win-r2.jsonl.gz docs/perf/data/perf-p1/results-win-chunks.jsonl.gz \
    docs/perf/data/perf-p1/results-win-cfrad2.jsonl.gz
python tools/xlib-bench/run.py compare docs/perf/data/perf-p1/results-linux-router.jsonl.gz \
    --a recast --b recast-auto
python tools/xlib-bench/ab.py --a BEFORE --b AFTER --threads 1 --cpu 20 --high-priority FILE...

# second fix pass: the cases it added, the per-thread peak RSS study, the bounded look-ahead
# experiment (git apply docs/perf/data/perf-p1/bounded-lookahead.patch for its build), and the
# paired-bzip2 logs summarized with their fastest samples and load ranges
python tools/xlib-bench/run.py bench ... --out results-linux-newcases.jsonl --rounds 3 \
    --modes pinned,multi,rss --files KTLX20130520_201643_V06.bz2 imgw.ram
python tools/xlib-bench/run.py rss-threads --data DATA --bin BIN --out rss-threads-linux.jsonl \
    --reps 5 --threads 1,2,4,8,16,32,0 --files KTLX20240315_000217_V06 KILX20260418_013553_V06
python tools/xlib-bench/run.py rss-threads --data DATA --bin HEAD_BIN --bin-b BOUNDED_BIN \
    --libs recast recast-b nexrad-crate --reps 5 --out rss-window-win.jsonl ...
python tools/xlib-bench/run.py rss-table docs/perf/data/perf-p1/rss-window-win.jsonl.gz
python tools/xlib-bench/ab.py --a HEAD_BIN/decode_bench --b BOUNDED_BIN/decode_bench --threads 0 \
    --rounds 10 --iters 10 --out ab-window-mt-linux.jsonl FILE...
python tools/xlib-bench/ab.py --summarize docs/perf/data/perf-p1/paired_1t.jsonl.gz

# third fix pass: peak RSS per thread count against the build before it and the nexrad crate,
# the A/B of the buffer pool bound, the variant studies (their build: this branch with
# `git apply docs/perf/data/perf-p1/pool-experiments.patch`; the data README lists the knobs),
# and CfRadial 2 with Py-ART and LROSE
python tools/xlib-bench/run.py rss-threads --data DATA --bin NEW_BIN --bin-b BEFORE_BIN \
    --libs recast recast-b nexrad-crate --reps 5 --threads 1,2,4,8,16,32,0 \
    --files KTLX20240315_000217_V06 KILX20260418_013553_V06 --out fix3-rss-threads-linux.jsonl
python tools/xlib-bench/ab.py --a BEFORE_BIN/decode_bench --b NEW_BIN/decode_bench --threads 0 \
    --rounds 10 --iters 10 --out fix3-ab-pool-mt-linux.jsonl FILE...
python tools/xlib-bench/variants.py --bin EXP_BIN/decode_bench \
    --variants "pool8=RECAST_EXP_POOLMAX:8|pool64=RECAST_EXP_POOLMAX:64" \
    --files FILE... --threads 12,0 --rounds 4 --out fix3-pool-cap-win.jsonl
python tools/xlib-bench/variants.py --summarize docs/perf/data/perf-p1/fix3-pool-cap-win.jsonl.gz
python tools/xlib-bench/run.py bench ... --out results-linux-cfrad2.jsonl --rounds 3 \
    --modes pinned,multi,rss --files cfrad2.SPOL_20080604_002217.nc --libs pyart lrose

# fourth fix pass: the ratios over recast and recast-f32 of the Summary, the cases it added,
# the paired-bzip2 one-thread run, and the two memory experiments (their builds: this branch
# with `git apply docs/perf/data/perf-p1/fix4-relws.patch` or `fix4-shrink.patch`)
python tools/xlib-bench/run.py ratios docs/perf/data/perf-p1/results-win.jsonl.gz ...
python tools/xlib-bench/run.py stage --out DATA              # stages the new cases too
python tools/xlib-bench/run.py bench ... --out results-linux-fix4cases.jsonl --rounds 3 \
    --modes pinned,multi,rss --files TLX_NCR_20260622_0806 TLX_DPA_20260629_1736 \
    TLX_NST_20260622_0806 TLX_NMD_20260622_0806 TLX_NSS_20220503_0052 \
    deboo.scan.20260924T2130.th00.hd5 swp.N42RF-TM_20181010_123925_AIR
python3 tools/xlib-bench/ab.py --a OFF/decode_bench --b PAIRED/decode_bench --threads 1 --cpu 26 \
    --rounds 9 --iters 5 --out fix4-paired-1t-linux.jsonl DATA20/*
python tools/xlib-bench/run.py rss-threads --data DATA --bin RELWS_BIN --bin-b KEEP_BIN \
    --libs recast recast-b nexrad-crate --reps 5 --threads 1,4,8,0 \
    --files KTLX20240315_000217_V06 KILX20260418_013553_V06 --out fix4-rss-relws-win.jsonl
python tools/xlib-bench/ab.py --a KEEP_BIN/decode_bench --b RELWS_BIN/decode_bench --threads 0 \
    --rounds 10 --iters 10 --out fix4-ab-relws-mt-win.jsonl FILE...
```
