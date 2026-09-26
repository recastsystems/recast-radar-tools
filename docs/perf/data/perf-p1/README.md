# perf-p1 raw benchmark results

The raw output behind `docs/perf/cross-library.md`, kept so the tables can be regenerated and
re-checked. The `.jsonl.gz` files are gzip-compressed JSON lines, one line per harness process
(`tools/xlib-bench/run.py bench`) or per build and round (`tools/xlib-bench/ab.py`).
`run.py table` reads them compressed.

| file | what | rows | SHA-256 of the uncompressed file |
|---|---|---:|---|
| `results-linux.jsonl.gz` | cross-library run in `nexbench`, 3 rounds, pinned / multi / rss, binaries from b7ebb14 | 884 | `531563e8bdabdc3e77de1bbdff7d173f3e0264fdcacc7f58c085a75772c37d48` |
| `results-linux-rssfix.jsonl.gz` | eight `rss-1thread` rows run again (Python libraries on the two gzip Level II volumes, netCDF4 on CfRadial 2) | 8 | `0766757297c59964ce476081a519be8ccd933939aede237008e479f3f4d90357` |
| `results-win.jsonl.gz` | the same run on Windows | 695 | `f920a3fb45509a1f9a2e854546b30583ec0718edef63dbc87485f9b2077b65a0` |
| `results-win-r2.jsonl.gz` | Windows round 2 run again (`--first-round 2`) | 186 | `904f392d070dbdd8658c2dae7598bdd285faab356aa1579031305681f99505c0` |
| `results-linux-mem.jsonl.gz` | fix pass, `nexbench`: `rss`, `pinned-mem` and `multi-mem` for every case, one round, under GNU time, harnesses from 639dd22 | 470 | `d05fc7c440d48f14e9260e0de0f863a2d16df8b2502b9190d3d8cf4297e1fd21` |
| `results-linux-chunks.jsonl.gz` | fix pass: the real-time chunk cases, pinned and multi, 3 rounds | 120 | `8efc02106543bc4bcd8d84a62265a82dbe24e363ee4b65992f4e4364b615b2c5` |
| `results-linux-router.jsonl.gz` | fix pass: `recast` and `recast-auto` on the router cases, pinned, multi and rss, 3 rounds | 128 | `48c64ff7c3e48a513afc20b8a7083a7126e263cd15d11e9af77374b6580fdbcb` |
| `results-win-chunks.jsonl.gz` | fix pass, Windows: the real-time chunk cases, pinned, multi and rss, 3 rounds | 104 | `3844d4fe47233b204da328417a4ef06e3964ec72edb30176249ba7de889baf27` |
| `results-win-router.jsonl.gz` | fix pass, Windows: the router cases | 128 | `55eb6e854912b3c1e10e2f7ea62ccecbd6775d6594309d27429f9a1fea6fb66d` |
| `run-linux-fix.log`, `run-linux-fix2.log`, `run-win-fix.log` | the fix pass's driver logs (the first carries the GNU time floor, and the discarded first attempt at the chunk and router rounds after its memory rows) | | |
| `cg_fix.txt` | callgrind totals of the fix pass: the 0c8129b before/after rows, whole-file bzip2 before/after, the Level III bzip2 share | | |
| `ab_bz2.md`, `ab_bz2meta.md` | `ab.py` summaries for ac71f48 (639dd22 against ac71f48, 10 rounds) | | |
| `paired_1t.jsonl.gz`, `paired_mt.jsonl.gz` | `ab.py` rounds of the paired-bzip2 decision, one thread and default pool; `.md` their summaries | | |
| `ab_*.md` | `ab.py` summaries of the before/after table of this pass | | |
| `cg_compare.log` | the `Collected` totals of the `tools/xlib-bench/cg_compare.sh` run. That run used an earlier version of the script without `--toggle-collect` (whole process), so the page's recast column is `callgrind_annotate --inclusive=yes` of `decode_bench::timed_work` in the same run's output files (1,211,342,763 against a 1,508,842,809 total for KTLX 2024). The committed script collects the timed region itself: in the fix pass its recast command line gave 1,211,396,280 for KTLX 2024 (a 75f5db6 build; `cg_fix2.txt`) | | |

Second fix pass (the review of 75f5db6). Linux rows ran in `nexbench` with a 12-CPU quota
while other agents' builds and fuzzers ran on the host (Windows CPU 53-100%, median 98%, `host-cpu-fix2.log`, 00:02-01:44 local time);
see the top of `docs/perf/cross-library.md`.

| file | what | rows | SHA-256 of the uncompressed file |
|---|---|---:|---|
| `rss-threads-linux.jsonl.gz` | `run.py rss-threads`, `nexbench`: recast (639dd22 build) and the nexrad crate, one decode per process, threads 1-32 and the default pool on KTLX 2024 and KILX 2026, 1 and default on the other six Level II cases, 5 repetitions | 220 | `8a45ecf58b65c019622c070f0a5f720c3769de2d3c2f1d7b6038bcc92d561eb6` |
| `rss-window-linux.jsonl.gz` | the same for a 75f5db6 build (`recast`) against the bounded look-ahead experiment (`recast-b`, `bounded-lookahead.patch`) | 220 | `44d77ef240b1ea2fa88a362a123bb910c3dfb9faab062a727e49b195fe49b399` |
| `rss-window-win.jsonl.gz` | the same on Windows: a 639dd22 build (`recast`, the same LDM path as 75f5db6), the bounded experiment (`recast-b`) and the nexrad crate; threads 1-16 and the default pool (32) | 300 | `c8c13e133307d7147ac3d32b0e0b5f8a4862d989321f1246a06fa6d422f1db99` |
| `rss-d3-linux.jsonl.gz` | one-thread peaks, a `c3be188` build (`recast`) against a build of the fix pass with the bounded experiment (`recast-b`; one thread takes the same path with and without it), 3 repetitions: the d5d7341 table | 18 | `1274457627ffed5d99e35dbc9aeefe2dbefc3ac17b73f5d72c7a64498e7bd346` |
| `ab-window-mt-linux.jsonl.gz`, `ab-window-mt-win.jsonl.gz` | `ab.py --threads 0` rounds, 75f5db6 (Windows: 639dd22) against the bounded experiment, default pools | 100, 36 | `99131596c17a0f5963d05ea5361bbc27a0b44bfd9ba3cec0accb7f2b24a088dc`, `05f55f53a04bf77a69d3f371ba0280798ee082f8191613dd8d3d83ef383c67f4` |
| `ab-window-1t-linux.jsonl.gz` | `ab.py --threads 1` rounds, 75f5db6 against the fix-pass build with the bounded experiment (one thread: the fallible `RecordBytes::get` on gzip and uncompressed input) | 24 | `01c2fc5705398cdb70c8cf669ab91ca290e627f3a2dd68521f57f78e3ba74c9b` |
| `paired_1t_loaded.jsonl.gz` | the fix pass's one-thread paired-bzip2 run on the 19 volumes in `nexbench`, stopped after 6 of 9 rounds because the host was loaded (rows carry `loadavg1`) | 227 | `4cfb1e1f76b4712a36ee06dda2c9346478427b9c097e3ba9523dadbab879fec3` |
| `results-linux-newcases.jsonl.gz`, `results-win-newcases.jsonl.gz` | `run.py bench`, the cases added in the fix pass: whole-file bzip2 Level II (KTLX 2013 recompressed) and two ODIM_H5 Cartesian products, 3 rounds, pinned / multi / rss | 117, 96 | `625ae18b4718b8dd987c8bec1fd196996438475cee0804ef636f41181ae6108c`, `58e3923822e3dc8037e86ed65207f762010b7f61885f6b97330daf719be9419c` |
| `cg_fix2.txt` | callgrind totals of `decode_bench`'s timed region, a 75f5db6 build against the final fix-pass build (the fallible `RecordBytes::get`) | | |
| `host-cpu-fix2.log` | Windows total CPU utilisation every 10 s during the fix pass (`Get-Counter`) | | |
| `bounded-lookahead.patch` | the bounded look-ahead experiment, which applies to this branch (not merged: pool decodes 1.07-1.45x slower; superseded by the third fix pass's buffer pool bound) | | |

Later files of the same run come after earlier ones on the `table` command line: a row of a
later file replaces the row of an earlier one for the same case, mode, library and round.

```
python tools/xlib-bench/run.py table docs/perf/data/perf-p1/results-linux.jsonl.gz \
    docs/perf/data/perf-p1/results-linux-rssfix.jsonl.gz
python tools/xlib-bench/run.py table docs/perf/data/perf-p1/results-win.jsonl.gz \
    docs/perf/data/perf-p1/results-win-r2.jsonl.gz
```

The Linux chunk and router timing rounds were run twice: the first attempt overlapped a
workspace build on the same host and was discarded; the committed files are the second
(`run-linux-fix2.log`). The fix pass's table commands:

```
python tools/xlib-bench/run.py table docs/perf/data/perf-p1/results-linux.jsonl.gz \
    docs/perf/data/perf-p1/results-linux-rssfix.jsonl.gz \
    docs/perf/data/perf-p1/results-linux-mem.jsonl.gz \
    docs/perf/data/perf-p1/results-linux-chunks.jsonl.gz docs/perf/data/perf-p1/results-linux-cfrad2.jsonl.gz
python tools/xlib-bench/run.py table docs/perf/data/perf-p1/results-win.jsonl.gz \
    docs/perf/data/perf-p1/results-win-r2.jsonl.gz docs/perf/data/perf-p1/results-win-chunks.jsonl.gz \
    docs/perf/data/perf-p1/results-win-cfrad2.jsonl.gz
python tools/xlib-bench/run.py compare docs/perf/data/perf-p1/results-linux-router.jsonl.gz --a recast --b recast-auto
```

**Linux peak RSS in the first two files is not the harness's own.** The driver then read
`wait4` `ru_maxrss` of a process it started with `subprocess.Popen`, and the kernel records the
old address space's high-water mark at exec, so every peak below the driver's own size (11-22
MiB) read as the driver's size. `run.py table` prints `n/m` for the peak RSS of every Linux row
without `"rss_method": "gnu-time"`, and takes the pinned and multi tables' peak column from the
`pinned-mem` and `multi-mem` rows; the Linux memory figures of the page all come from the fix
pass's files, measured under GNU time. The timing columns
of these files are unaffected, and so are the Windows files (`PeakWorkingSetSize`).

Third fix pass (the review of dc3fdf7). Windows rows ran on the host while other agents'
builds kept it at or near 100% CPU; Linux rows in `nexbench` as before. `before` builds are
dc3fdf7 (`git archive`), `after` builds the fix pass. `variants.py` rows come from builds with
the experiment's knobs (`pool-experiments.patch`, which applies to this branch), whose
`RECAST_EXP_*` variables select the variant: `RECAST_EXP_POOLMAX` the buffer pool size;
`RECAST_EXP_AHEAD` `none` or `a,b` for a look-ahead window of a x workers + b records;
`RECAST_EXP_MAXDEC` a cap on the decoding threads; `RECAST_EXP_INIT` and `RECAST_EXP_SPAWN`
workers started up front and on demand; `RECAST_EXP_HELP` `1` (the parser decodes a record
itself whenever the one it needs is not ready, the shipped rule), `0` (never while workers
run) or `backlog` (only when no decoded record waits); `RECAST_EXP_PRINT` the parser's time,
its own decodes, its waits and the largest backlog on stderr. The studies ran on such builds of
dc3fdf7, whose pool default was 64 buffers: their variant `pool64` sets no `RECAST_EXP_POOLMAX`,
which on this branch needs `RECAST_EXP_POOLMAX=64`.

| file | what | rows | SHA-256 of the uncompressed file |
|---|---|---:|---|
| `fix3-rss-threads-linux.jsonl.gz`, `fix3-rss-threads-win.jsonl.gz` | `run.py rss-threads`: this fix pass (`recast`), dc3fdf7 (`recast-b`) and the nexrad crate; KTLX 2024 and KILX 2026 at 1-32 threads and the default pool, 5 repetitions, the six other Level II cases at 1 and the default pool, 3 | 282, 252 | `a91f9d606106c7a6ef390744491d7e5a362309d48c614eba79d703de307164ca`, `4e9a7a78c0e2c225157401bba0b4cb45f62896dd14626875db0e80e2c5a6af2c` |
| `fix3-ab-pool-mt-linux.jsonl.gz`, `fix3-ab-pool-mt-win.jsonl.gz` | `ab.py --threads 0`, dc3fdf7 (A) against the fix pass (B), 10 rounds of 10 decodes, four LDM cases | 60, 60 | `b527adbbb10dc61b775dc72515c8535d93bc909f0229277ebf2c7ba9b7826f59`, `cf4c9eb6241ea09542611e6067513ec9f25ac870bb42a8bfc2fb3975220eeb2b` |
| `fix3-pool-cap-linux.jsonl.gz`, `fix3-pool-cap-win.jsonl.gz` | `variants.py`: buffer pool of 8, 16, 33 (Windows) and 64 buffers, and on Linux the look-ahead window, at 1 and 4 threads and the default pool (and 12 on Windows); peak RSS rows and 10-decode timing rows | 252, 424 | `1bca7b0d85da73752b6ca5965d9370a8ee85db315331e1a1450fc193ec82f917`, `d62a65312d5b0398ca7a42d25c08129ffaff605aa9e65696192ab9c79bc5e0fd` |
| `fix3-thread-sweep-win.jsonl.gz` | `variants.py`: the fix pass at 4, 8, 12, 16 threads and the default pool, 6 rounds | 120 | `73101f98665f78dee841b32e731716fddd3522e23cbe0e39c93bd1b8dd2fc42b` |
| `fix3-window-pool8-win.jsonl.gz` | `variants.py`: the pool of 8 with and without the workers + 2 look-ahead window, and the pool of 64, at 12 threads and the default pool, 4 rounds | 96 | `17b335f6fb999a07a8f2a5942645ae8230bde4a311f8c145b04c5bdc3c5c5902` |
| `fix3-window-variants-win.jsonl.gz` | an earlier build of the experiment: unbounded, workers + 2 and 2 x workers + 4 windows, each with and without a speculative decode of the awaited record after 3 ms, at 12 threads and the default pool, 3 rounds | 120 | `d88e5e87f0d0e4a3d7ff7a32e2e2b89f5441134381be53be5ceaa749c1fbc6d6` |
| `fix3-help-policy-win.jsonl.gz` | `variants.py`: `RECAST_EXP_HELP` `1`, `backlog` and `0` with the pool of 8, at 4, 8, 16 threads and the default pool, 6 rounds of 10 decodes | 144 | `236a604db2009a54f8f14bacb2ed7cce36315b08d963b3b4a597aea8970578e2` |
| `parser-instrumentation-win.log` | `RECAST_EXP_PRINT` lines, KTLX 2024 and KILX 2026, 1-16 threads and the default pool, 4 decodes each | | |
| `pool-experiments.patch` | the experiment's knobs, for this branch | | |
| `results-linux-cfrad2.jsonl.gz`, `results-win-cfrad2.jsonl.gz` | `run.py bench`, CfRadial 2 with Py-ART and (Linux) LROSE, 3 rounds, pinned / multi / rss; both fail on the file | 14, 7 | `3b1be3ecb80a988c3a1b455821f0402c70474fed1b6b0fe5fedba72eea0327cf`, `bd00797af64f1989e0412ef8c1f71215da2e85b1e5f18df846ad105f750f401e` |

```
python tools/xlib-bench/run.py rss-table docs/perf/data/perf-p1/fix3-rss-threads-linux.jsonl.gz
python tools/xlib-bench/ab.py --summarize docs/perf/data/perf-p1/fix3-ab-pool-mt-win.jsonl.gz
python tools/xlib-bench/variants.py --summarize docs/perf/data/perf-p1/fix3-pool-cap-win.jsonl.gz
```

**Commit hashes.** The branch's commits were reworded after these runs (messages only: every
commit's tree is unchanged), so the raw logs (`cg_fix.txt`, `cg_fix2.txt` and the driver
logs) name the hashes the commits had when they ran. The prose above and
`docs/perf/cross-library.md` use the current ones:

| hash in the raw logs | current commit |
|---|---|
| d3257fc | c3be188 |
| 2b577ba | 32e7026 |
| f6290b3 | 0c8129b |
| d8db647 | d5d7341 |
| 2fff376 | 16c88de |
| 0b563de | 10cdd57 |
| b5025f2 | 0c4d826 |
| 449895e | b7ebb14 |
| 965a453 | 782eedf |
| 27a82f6 | e70bf43 |
| 7fa0166 | 63f8672 |
| dd95e0f | c923ba4 |
| 8ba43df | f5bb757 |
| aa6797d | 171ce18 |
| daee1d0 | 8901af1 |
| bb0ec98 | b7b38f7 |
| 50e6629 | 639dd22 |
| 6786aae | ac71f48 |
| 79b932d | 75f5db6 |
| 84bb3a6 | 4bddf19 |
| b4cd309 | 15d9f16 |
| ba71816 | 7033d78 |
| 8a010a8 | 6f67c3f |
| db0015d | dc3fdf7 |

Fourth fix pass (the review of a9a3ff6). Windows rows ran on the host while other agents'
builds kept it busy (97-98% CPU when sampled); Linux rows in `nexbench` as before, with the
container's load and `cpu.stat` throttling before and after each run in
`fix4-linux-driver.log`. `keep` builds are this branch (Windows 0af97c2 for the A/B, 2a3365f
for the peak RSS; Linux 56d29c5: the same `io-nexrad` sources), `relws` builds the same with
`git apply docs/perf/data/perf-p1/fix4-relws.patch`.

| file | what | rows | SHA-256 of the uncompressed file |
|---|---|---:|---|
| `fix4-ab-ldmfix-1t-win.jsonl.gz`, `fix4-ab-ldmfix-auto-1t-win.jsonl.gz`, `fix4-ab-ldmfix-bz-1t-win.jsonl.gz` | `ab.py --threads 1`, a9a3ff6 (A) against 0af97c2 (B, the wrapped-LDM fix): gzip, uncompressed, LDM and whole-file bzip2 Level II through `l2` (6 rounds) and the router (`auto`, 6 rounds), and the two whole-file bzip2 inputs again (10 rounds) | 48, 12, 40 | `d8fe49067037477b2590fcfa0e0d1554ba6eba3f13370354f035d459ebe2b67f`, `8fba12bb8d6121ae6615884b332f68b9a6d79212b4e0fdae3cd29ab51f45d10c`, `0844404d9d713d81edc8b4dda93d06582cd3695728fb2d69288fb988f234729b` |
| `fix4-rss-relws-linux.jsonl.gz`, `fix4-rss-relws-win.jsonl.gz` | `run.py rss-threads`: `relws` (`recast`), `keep` (`recast-b`) and the nexrad crate, KTLX 2024 and KILX 2026 at 1, 4, 8 threads and the default pool (5 repetitions), KIWA 35 chunks at the default pool (3) | 129, 129 | `8bdd449686f909d3af3f4e24442ad5cdea01480d205aa166064b9e6af3d122e3`, `36f93eeb942bb66c82776c0427c690068e9d7f63f47f9288033da361c30904e3` |
| `fix4-ab-relws-mt-win.jsonl.gz`, `fix4-ab-relws-4t-win.jsonl.gz`, `fix4-ab-relws-mt-linux.jsonl.gz` | `ab.py`, `keep` (A) against `relws` (B), 10 decodes per process: default pool (10 rounds), 4 threads (6 rounds, Windows) | 60, 36, 60 | `a1602bafa994b1700a3045a68daac03431fd216eca6ed3fb4231fbff8ecdc0cd`, `13101b5a6c087360cc925db256c1337f9836019ab7e6b4c3abd4a8ea29ef5784`, `9961aeb28f9dd0830eccb5215f900473fbf8be5a18739f630fcb64f280c1cc97` |
| `fix4-relws.patch` | the experiment: free a pool worker's bzip2 workspace when it runs out of records (not merged; section "A second lever") | | |
| `fix4-rss-shrink-win.jsonl.gz` | `run.py rss-threads --threads 1`, KIWA 3 and 35 chunks: fields shrunk to their length at the end of the decode (`recast`, `fix4-shrink.patch`, not merged) against 2a3365f (`recast-b`) and the nexrad crate, 5 repetitions, fast release profile for both recast builds | 30 | `781be3f4452b282f31ef652bbca75c6b90d735b553ac230b6f69070ea1b82a40` |
| `fix4-shrink.patch` | that experiment | | |
| `fix4-paired-1t-linux.jsonl.gz` | `ab.py --threads 1 --cpu 26`, the 19 LDM volumes, 24baac7 without (A) and with (B) `paired-bzip2`, 9 rounds of 5 decodes (rows carry `loadavg1`) | 342 | `34120a9d396c2823cc35275810243468d88e9f89a0fac9043721e20af50c0a96` |
| `results-linux-fix4cases.jsonl.gz`, `results-win-fix4cases.jsonl.gz` | `run.py bench`, the cases the fourth fix pass added (Level III raster, digital array, graphic and tabular products; an ODIM_H5 SCAN; airborne DORADE), 3 rounds, pinned / multi / rss, `decode_bench` of 2a3365f | 186, 165 | `2a1adc59292542cfda26517adb6542ceeceeed7b425ae7c43627a2f1f673f2db`, `7f71ca42628f65382776d476bae329dc4fe82e27d893708303b87afb2738e882` |
| `fix4-identity-win.jsonl.gz` | the output comparison of `decode_bench` built from a9a3ff6 (`a`) and 2a3365f (`b`), fast release profile: one row per file, format and option set, both builds' counts and hashes or error (section "This pass's optimisations", fourth fix pass) | 429 | `1331dfbbadc2619dccfaaab6cfb51502996cdc821340a51a0e1c281325328912` |
| `fix4-linux-driver.log` | the Linux runs' start and end times, load averages and `cpu.stat` throttling counters | | |

```
python tools/xlib-bench/run.py ratios docs/perf/data/perf-p1/results-win.jsonl.gz \
    docs/perf/data/perf-p1/results-win-r2.jsonl.gz docs/perf/data/perf-p1/results-win-chunks.jsonl.gz \
    docs/perf/data/perf-p1/results-win-newcases.jsonl.gz
python tools/xlib-bench/run.py ratios docs/perf/data/perf-p1/results-linux.jsonl.gz \
    docs/perf/data/perf-p1/results-linux-rssfix.jsonl.gz docs/perf/data/perf-p1/results-linux-chunks.jsonl.gz \
    docs/perf/data/perf-p1/results-linux-newcases.jsonl.gz
python tools/xlib-bench/run.py ratios ... --bases recast     # recast-f32 over recast
python tools/xlib-bench/run.py rss-table docs/perf/data/perf-p1/fix4-rss-relws-win.jsonl.gz
python tools/xlib-bench/ab.py --summarize docs/perf/data/perf-p1/fix4-paired-1t-linux.jsonl.gz
python tools/xlib-bench/run.py table docs/perf/data/perf-p1/results-linux-fix4cases.jsonl.gz
```
