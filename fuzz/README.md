# Fuzzing the recast-radar decoders

cargo-fuzz (libFuzzer) targets for every byte-level decoder entry point. The
`fuzz/` directory is its own Cargo workspace, not a member of the main
workspace: the targets need a nightly toolchain and libFuzzer, which build on
Linux. Run them there, or in the `nexbench` container (see below).

## Targets

| Target | Crate | Entry points |
|---|---|---|
| `level2_volume` | `recast-radar-io-nexrad` | `decode_volume_from_bytes`, `decode_gzip_volume_from_bytes_with_preview`, `decode_volume_from_bytes_with_bzip_preview`, `decode_gzip_preview_from_bytes`, `decode_bzip_block_preview_from_bytes` (one per input, by length mod 4; the preview threshold is the last byte) |
| `io_router` | `recast-radar-io` | `sniff_supported_volume_format`, `decode_supported_volume_bytes` (zip/gzip unwrapping, then Level II, ODIM, CfRadial, DORADE or JMA) |
| `odim` | `recast-radar-io-odim` | `looks_like_hdf5_bytes`, `decode_odim_h5_volume`, `decode_odim_h5_cartesian_max` |
| `cfradial` | `recast-radar-io-cfradial` | `looks_like_netcdf3_bytes`, `decode_cfradial1_volume` |
| `dorade` | `recast-radar-io-dorade` | `looks_like_dorade_bytes`, `peek_dorade_sweep`, then `decode_dorade_sweep_volume` (even lengths) or `decode_dorade_volume_from_slices` with the input twice (odd lengths) |
| `jma` | `recast-radar-io-jma` | `looks_like_jma_tar_bytes`, then by length mod 3: `decode_jma_tar_volumes(None)`, `decode_jma_tar_first_station`, or `jma_tar_station_headers` plus a site-filtered `decode_jma_tar_volumes` |

The harness bodies live in `src/lib.rs`; each `fuzz_targets/<target>.rs` is a
one-line libFuzzer wrapper around the function with the same name. A harness
fails only by panicking, aborting, hanging or exhausting memory; decode errors
are the expected result for most inputs.

Not covered yet: Level III products (a target belongs with
`recast-radar-io-level3` once that crate merges), and the mobile-radar zip
archive and directory readers, which take paths instead of bytes.

## Layout

| Path | Committed | Contents |
|---|---|---|
| `src/lib.rs`, `fuzz_targets/` | yes | harnesses and libFuzzer wrappers |
| `tools/` | yes | `fuzz-tools`: builds seed corpora and replays inputs on stable Rust |
| `regressions/<target>/` | yes | minimized inputs that crashed a target; must replay cleanly |
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

No seed byte is synthesized. The seed list, with the reason for each file, is
the `SEEDS` table in `tools/src/main.rs`.

```bash
cargo run --release --manifest-path fuzz/tools/Cargo.toml -- seeds
```

To check a seed directory, run `fuzz-tools replay <target> fuzz/seeds/<target>`.
It prints `decoded` when an entry point returned `Ok` for the input's mode, and
`rejected` otherwise. Some seeds are rejected on purpose:

- The netCDF-4 CfRadial file tests the netCDF-3 reader's rejection path.
- Level II seeds whose length is 3 mod 4 go through the preview-only mode.
  That mode reports `rejected` unless a preview cut is completed.
- `l2-kvwx-20080415-235337` (AR2V0001 header with Message 31 radials) is
  rejected by the current Level II decoder. It stays as a seed for that edge
  case.

## Running

Prerequisites: Linux, `rustup toolchain install nightly`,
`cargo install cargo-fuzz`, and a C++ compiler for libFuzzer.

```bash
# All six targets in parallel for 10 minutes each, one libFuzzer worker per target:
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
zlib-rs, zip), build with the default `-s address` instead. `run.sh` also:

- sets `RAYON_NUM_THREADS=1`, so each target uses about one core
- sets `-max_len` to the largest seed for the target
- sets `-timeout=10` (seconds per input) and `-rss_limit_mb=2048`
- runs libFuzzer in fork mode with `-ignore_crashes/-ignore_timeouts/-ignore_ooms`,
  so a run keeps going after the first finding and saves each one under
  `artifacts/<target>/`

### In the nexbench container

```bash
cargo run --release --manifest-path fuzz/tools/Cargo.toml -- seeds     # on the host
tar -cf - --exclude=target Cargo.toml Cargo.lock crates fuzz \
  | MSYS_NO_PATHCONV=1 docker exec -i nexbench bash -c 'mkdir -p /build/fuzz && tar -xf - -C /build/fuzz'
MSYS_NO_PATHCONV=1 docker exec nexbench bash -c 'source /root/.cargo/env; /build/fuzz/fuzz/run.sh 600'
```

## Crashes and regressions

1. Reproduce: `target/x86_64-unknown-linux-gnu/release/<target> artifacts/<target>/crash-<sha1>`.
2. Minimize: `cargo +nightly fuzz tmin -s none <target> artifacts/<target>/crash-<sha1>`.
   Timeouts and OOMs need `-timeout=` or `-rss_limit_mb=` passed after `--`.
   libFuzzer checks RSS only once per second, so an allocation spike that
   finishes quickly can slip past a 2048 MB limit. Minimize and replay OOM
   inputs with a limit well below their peak (for example
   `-rss_limit_mb=512`). Byte-deletion minimization barely shrinks netCDF and
   HDF5 inputs, because their data sections sit at absolute offsets.
3. Commit the minimized input as `regressions/<target>/<kind>-<short description>`.
   Record where it fails and why in the commit message.
4. Fix the decoder. Then this must print no `PANIC` and exit 0:

   ```bash
   cargo run --release --manifest-path fuzz/tools/Cargo.toml -- regressions
   ```

   The workspace release profile turns on `debug-assertions` and
   `overflow-checks`, so the stable replay panics wherever the fuzz build
   did. `cargo +nightly fuzz run -s none <target> regressions/<target> -- -runs=0`
   replays the same inputs under libFuzzer, which also catches aborts, OOMs
   and timeouts that the stable replay cannot.

Regression inputs are libFuzzer mutations of the real seeds for their target,
so they count as real-file-derived.
