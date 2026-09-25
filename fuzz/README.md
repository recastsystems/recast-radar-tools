# Fuzzing the recast-radar decoders and the bzip2 encoder

cargo-fuzz (libFuzzer) targets for every byte-level decoder entry point. The
`fuzz/` directory is its own Cargo workspace, not a member of the main
workspace: the targets need a nightly toolchain and libFuzzer, which build on
Linux. Run them there, or in the `nexbench` container (see below).

## Targets

| Target | Crate | Entry points |
|---|---|---|
| `level2_volume` | `recast-radar-io-nexrad` | `read_volume_from_bytes`, `read_gzip_volume_from_bytes_with_preview`, `read_volume_from_bytes_with_bzip_preview`, `read_gzip_preview_from_bytes`, `read_bzip_block_preview_from_bytes` (one per input, by length mod 4; the preview threshold is the last byte) |
| `level2_writer` | `recast-radar-io-nexrad` | `read_volume_with_metadata`, then the writer: `write_volume_with_source` without the source metadata (LDM bzip2), with it (uncompressed), with it gzip-wrapped, or `write_realtime_chunks_with_source` with it (mode = input length mod 4; the last two drop gates before the radar and write site KTLX, so Message 1 volumes are written too); the written bytes must decode again with the reported sweeps and radials, the source's rays in their order (a ray left out only when no written moment has data on it), every radial's time (milliseconds since 1970) and angles, and every written moment's codes, gates and absent rays (a refused write is fine) |
| `level2_writer_router` | `recast-radar-io`, `recast-radar-io-nexrad` | `read_supported_volume_bytes` (ODIM_H5, CfRadial, DORADE, JMA, Level II), then the writer: `write_volume_with_source` (mode = input length mod 8: the Precise (0 and 3), Compatible (1) or Standard (2) policy by length mod 4; lengths with `(len / 4) % 2 == 1` also drop gates before the radar, accept any range rounding, supply a Nyquist velocity and unambiguous range where the source has none and go through `write_realtime_chunks`); the written bytes must decode again with the reported sweeps and radials, each radial the source ray `WriteSummary::written_rays` names (every ray at most once, a ray left out only when no written moment has data on it) with its time (to the millisecond) and angles (bit for bit), and every written moment's gate count, first gate and spacing (to the metre), absent rays and values (each within the reported `max_abs_error`, sentinels as sentinels) |
| `io_router` | `recast-radar-io` | `sniff_supported_volume_format`, `read_supported_volume_bytes` (zip/gzip unwrapping, then Level II, ODIM, CfRadial, DORADE or JMA) |
| `odim` | `recast-radar-io-odim` | `looks_like_hdf5_bytes`, `read_odim_h5_volume`, `decode_odim_h5_cartesian_max` |
| `cfradial` | `recast-radar-io-cfradial` | `looks_like_netcdf3_bytes`, `read_cfradial1_volume` |
| `dorade` | `recast-radar-io-dorade` | `looks_like_dorade_bytes`, `peek_dorade_sweep`, then `read_dorade_sweep_volume` (even lengths) or `read_dorade_volume_from_slices` with the input twice (odd lengths) |
| `jma` | `recast-radar-io-jma` | `looks_like_jma_tar_bytes`, then by length mod 3: `read_jma_tar_volumes(None)`, `read_jma_tar_first_station`, or `jma_tar_station_headers` plus a site-filtered `read_jma_tar_volumes` |
| `bzip2` | `recast-radar-bzip2` | `Decoder::decode_stream_into` on the input, then `Decoder::decode_two_into` on the input paired with its own first half, with a 64 MiB output limit |
| `bzip2_encode` | `recast-radar-bzip2` | Differential: `Encoder::encode_into` at level 1 + (length mod 9) on the input, then on its first quarter appended to the same output; each stream must equal the `bzip2` crate's (libbz2-rs-sys, a port of libbzip2 1.0.8) except in the `origPtr` of a periodic block, and must decode to what was compressed with our decoder, and with the reference decoder when it differs from the reference's stream. The encoders (one per level) and the decoder are reused across inputs, so every stream is written over buffers that earlier calls filled |

The harness bodies live in `src/lib.rs`; each `fuzz_targets/<target>.rs` is a
one-line libFuzzer wrapper around the function with the same name. A harness
fails only by panicking, aborting, hanging or exhausting memory; decode errors
are the expected result for most inputs. `bzip2_encode` panics on purpose
when a round trip or the comparison with the reference fails.

Not covered yet: Level III products (a target belongs with
`recast-radar-io-level3` once that crate merges), and the mobile-radar zip
archive and directory readers, which take paths instead of bytes.

## Layout

| Path | Committed | Contents |
|---|---|---|
| `src/lib.rs`, `fuzz_targets/` | yes | harnesses and libFuzzer wrappers |
| `tools/` | yes | `fuzz-tools`: builds seed corpora and replays inputs on stable Rust |
| `run.sh` | yes | runs targets in parallel for a time budget |
| `seeds/<target>/` | no | seed corpora written by `fuzz-tools seeds` |
| `corpus/<target>/` | no | corpus that libFuzzer grows during runs |
| `artifacts/<target>/` | no | crash, timeout and OOM inputs found by runs |
| `logs/` | no | `run.sh` logs |

## Seed corpora

The seeds are real test files. `fuzz-tools seeds` looks them up by id in the
`testdata/**/manifest.toml` manifests through `recast-radar-testdata`.
Committed fixtures are read in place. Larger files are downloaded on first use,
checked against their sha256, and cached (`RECAST_RADAR_TESTDATA` overrides the
cache location). The seeds are not committed, so the corpus files are not
stored twice. The tool writes each file as-is, or cuts it down with one of
these derivations:

- `.l2-sparse`: the Level II volume decompressed to its uncompressed record
  stream, keeping the volume header, the metadata records except empty,
  type-0 and clutter-map records (messages 13 and 15), the first radial
  messages (64 KiB), and the last radial messages (16 KiB, including the
  end-of-volume radial). Whole records only.
- `.l2-head`: the decompressed volume header, every record before the first
  radial, and the first radials (32 KiB). This is the prefix a partial
  download gives.
- `.l2-block-head`: an LDM block-bzip2 file as published, cut after its
  leading whole bzip2 blocks (256 KiB budget).
- `.plus-N-chunks`: a real-time start chunk followed by the next chunk, the
  way a real-time client assembles a volume.
- `.headN`: a DORADE sweepfile cut at the block boundary after its first `N`
  ray groups, the same head-trim used for the committed DORADE fixtures.
- `.ldm-recordN`: the bzip2 stream of LDM record `N` of a block-bzip2
  Level II file or real-time chunk, without its control word.
- `.ldm-payloadN`: the decompressed contents of LDM record `N`, the bytes a
  Level II writer compresses (the `bzip2_encode` seeds).

No seed byte is synthesized. The seed list, with the reason for each file, is
the `SEEDS` table in `tools/src/main.rs`.

```bash
cargo run --release --manifest-path fuzz/tools/Cargo.toml -- seeds
```

To check a seed directory, run `fuzz-tools replay <target> fuzz/seeds/<target>`.
It prints `decoded` when an entry point returned `Ok` for the input's mode, and
`rejected` otherwise. Some seeds are rejected on purpose:

- The netCDF-4 CfRadial file tests the netCDF-3 reader's rejection path
  (`cfradial`, `odim` and `io_router`).
- Level II seeds whose length is 3 mod 4 go through the preview-only mode.
  That mode reports `rejected` unless a preview cut is completed.
- A real-time intermediate chunk on its own (`...-002-i`) has no volume
  header.
- `io_router` rejects the ODIM Cartesian composite, which only the `odim`
  target decodes.
- `level2_writer` rejects the real-time start chunk alone: it holds the
  metadata record but no radial, so there is no volume to write.
- `level2_writer_router` rejects the DOW6 RHI head: Level II cannot hold RHI
  sweeps, and the typed refusal is the path it covers.

`l2-kvwx-20080415-235337` (AR2V0001 header with Message 31 radials) was
rejected until the Level II decoder accepted a blank Message 31 radar
identifier (`09c8d1e`). It now decodes and stays as a seed for that edge
case.

## Smoke runs on stable Rust

`fuzz-tools smoke <target> <n> [SEED_DIR]` mutates every seed of a target
`n` times with a fixed pseudo-random sequence (bit flips, boundary bytes,
overwritten, copied, inserted and deleted spans, truncation) and runs each
mutant through the harness, on any platform and without libFuzzer. The
mutants are the same on every run, so a result can be repeated; a panicking
mutant is saved as `artifacts/<target>/smoke-<seed>-<mutant>` for `replay`.
Length-changing mutations reach every length-selected mode. It is no
substitute for a coverage-guided campaign, but it checks a harness and its
seeds before one, and on Windows.

```bash
cargo run --release --manifest-path fuzz/tools/Cargo.toml -- smoke level2_writer_router 1000
```

## Running

Prerequisites: Linux, `rustup toolchain install nightly`,
`cargo install cargo-fuzz`, and a C++ compiler for libFuzzer.

```bash
# All ten targets in parallel for 10 minutes each, one libFuzzer worker per target:
fuzz/run.sh 600
# A subset:
fuzz/run.sh 120 level2_volume dorade
# One target interactively:
cd fuzz && cargo +nightly fuzz run -s none level2_volume corpus/level2_volume seeds/level2_volume -- -max_len=400000
```

`run.sh` builds with cargo-fuzz defaults (optimized, with debug assertions
and overflow checks, so arithmetic overflow panics) and no sanitizer. The
decoder crates contain no unsafe code, and leaving out AddressSanitizer about
doubles the execution rate. To check the unsafe code in dependencies (bzip2,
zlib-rs, zip), build with the default `-s address` instead (`recast-radar-bzip2`
has none, so the `bzip2` target gains nothing from a sanitizer). `run.sh` also:

- sets `RAYON_NUM_THREADS=1`, so each target uses about one core
- sets `-max_len` to the largest seed for the target
- sets `-timeout=10` (seconds per input) and `-rss_limit_mb=2048`
- runs libFuzzer in fork mode with `-ignore_crashes/-ignore_timeouts/-ignore_ooms`,
  so a run keeps going after the first finding and saves each one under
  `artifacts/<target>/`

`bzip2_encode` seeds are whole LDM record contents (up to 1.2 MB), which
the target compresses one and a quarter times and checks against the
reference encoder, so at the default `-max_len` it runs about 8 inputs a
second on one core of the nexbench host; with `-max_len=8192` about 70
(15-minute campaigns, 2026-09-25; see `docs/perf/bzip2-encoder.md`).

### In the nexbench container

```bash
cargo run --release --manifest-path fuzz/tools/Cargo.toml -- seeds     # on the host
tar -cf - --exclude=target Cargo.toml Cargo.lock crates fuzz testdata \
  | MSYS_NO_PATHCONV=1 docker exec -i nexbench bash -c 'mkdir -p /build/fuzz && tar -xf - -C /build/fuzz'
MSYS_NO_PATHCONV=1 docker exec nexbench bash -c 'source /root/.cargo/env; /build/fuzz/fuzz/run.sh 600'
```

`testdata/` carries the manifests and the committed regression inputs. The
container shares its CPUs with other work: `run.sh` starts one CPU-heavy
process per target, so pass fewer targets when cores are scarce.

## Crashes and regressions

1. Reproduce: `target/x86_64-unknown-linux-gnu/release/<target> artifacts/<target>/crash-<sha1>`.
2. Minimize: `cargo +nightly fuzz tmin -s none <target> artifacts/<target>/crash-<sha1>`.
   Timeouts and OOMs need `-timeout=` or `-rss_limit_mb=` passed after `--`.
   libFuzzer checks RSS only once per second, so an allocation spike that
   finishes quickly can slip past a 2048 MB limit. Minimize and replay OOM
   inputs with a limit well below their peak (for example
   `-rss_limit_mb=512`). Byte-deletion minimization barely shrinks netCDF and
   HDF5 inputs, because their data sections sit at absolute offsets.
3. Commit the minimized input as real test data: the file under
   `testdata/files/fuzz/<target>/<kind>-<short description>`, and an entry in
   `testdata/fuzz/manifest.toml` with `derived_from` set to the seed id it
   mutates, a `derivation` naming the artifact and what went wrong, and the
   tags `fuzz-regression`, `fuzz-target:<target>` and
   `fuzz-finding:<crash|oom|timeout>`. Regenerate the corpus index with
   `RECAST_RADAR_TESTDATA_BLESS=1 cargo test -p recast-radar-testdata --test corpus_doc`.
   To find the seed, compare the artifact with the seeds of its target; most
   mutations keep the seed's bytes at their offsets.
4. Fix the decoder, and add a regression test in the affected crate
   (`tests/fuzz_regressions.rs`) that feeds the input by manifest id and
   checks the error. Then this must print no `PANIC` and exit 0:

   ```bash
   cargo run --release --manifest-path fuzz/tools/Cargo.toml -- regressions
   ```

   It replays every manifest entry tagged `fuzz-regression` through its
   target and through `io_router`. The fuzz release profile turns on
   `debug-assertions` and `overflow-checks`, so the stable replay panics
   wherever the fuzz build did. Under libFuzzer, which also catches aborts,
   OOMs and timeouts that the stable replay cannot:

   ```bash
   cd fuzz && cargo +nightly fuzz run -s none <target> ../testdata/files/fuzz/<target> -- -runs=0 -rss_limit_mb=512 -timeout=10
   ```

## Regression inputs

| Manifest id | Target | Finding | Fixed in |
|---|---|---|---|
| `fuzz-odim-hdf5-local-heap-name-offset-overflow` | `odim` | panic: u64 overflow adding a local heap's data address and a link-name offset (`hdf5lite` `heap_string`) | D.2: checked add, error "HDF5 local heap name offset overflow" |
| `fuzz-cfradial-overlapping-sweep-ray-ranges` | `cfradial` | OOM (2.44 GB peak from 868 KB): a header claiming 6,146 sweeps read garbage ray indices, and every overlapping sweep copied the whole field | D.2: sweep cap (1,024) and decode budget; D.4 fix: overlapping sweep ray ranges are an error, and ray indices that are not non-negative integers skip the sweep |

Regression inputs are libFuzzer mutations of the real seeds for their target,
so they count as real-file-derived.
