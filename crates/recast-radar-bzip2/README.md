# recast-radar-bzip2

A bzip2 decompressor in Rust with `#![forbid(unsafe_code)]` and no
dependencies, written for NEXRAD Level II LDM records (one bzip2 stream per
record) as the replacement for the `bzip2` crate in
`recast-radar-io-nexrad`'s record path.

```rust
use recast_radar_bzip2::{Decoder, Error};

fn decode_record(record: &[u8]) -> Result<Vec<u8>, Error> {
    let mut decoder = Decoder::new();
    decoder.set_max_output(16 << 20); // at most 16 MiB per stream
    let mut out = Vec::new();
    decoder.decode_stream_into(record, &mut out)?;
    Ok(out)
}
```

## API

| Item | Purpose |
|---|---|
| `Decoder::new()` | A decoder with no output limit. The ~7 MiB of block work buffers are allocated on the first decode and reused, so keep one decoder per thread. |
| `Decoder::set_max_output(limit)` | Bound the bytes one call may append. Checked against a block's exact decoded size before that block's output is allocated. |
| `Decoder::decode_stream_into(input, out)` | Decode one stream (`BZh1`..`BZh9` header, any number of blocks, end-of-stream marker) from the start of `input`, appending to `out`. Bytes after the marker are ignored. On `Err`, `out` is back at its original length. |
| `Decoder::decode_two_into(a, out_a, b, out_b)` | Two independent streams decoded in lockstep on one thread, with their inverse-BWT chases interleaved so the cache misses overlap. Same results as two single calls. |
| `Error` | Why a decode failed: header, block magic, block header, Huffman tables, block data, truncation, block CRC, stream CRC, output limit. |

What is accepted and what is rejected follows libbzip2 1.0.8: a stream
decodes to exactly the bytes libbzip2 produces, or returns an error.
Randomised blocks (bzip2 0.9.0 and earlier) decode, and selector counts above
18,002 are clamped as libbzip2 clamps them. Corrupt or truncated input never
panics.

## Performance

On the `KTLX20240315_000217_V06` bench volume (97 LDM records, 10.8 MB
compressed, 84 MB decoded), callgrind instruction counts for decoding every
record with a reused decoder, from the single-core decode study that led to
this crate (Rust 1.94, x86-64, release with fat LTO, Linux):

| Decoder | Instructions | Relative |
|---|---:|---:|
| `recast-radar-bzip2` | 1.16 G | 1.00 |
| lbzip2 0.5.12 (block API, unsafe) | 2.71 G | 2.32 |
| C libbzip2 1.0.8 (`bzip2-sys`) | 4.51 G | 3.87 |
| `libbz2-rs-sys` 0.2.5 (`bzip2` crate default) | 5.70 G | 4.89 |

The KILX20260418 volume (106 records, 87 MB decoded) gives the same
ordering (2.29 G against 10.95 G). Pinned wall-clock runs put the decoder at
roughly half the time of `libbz2-rs-sys`; the memory-latency-bound
inverse-BWT chase dominates, which is what `decode_two_into` overlaps.

## Design

Per block, four stages: (A) a 64-bit bit window with table-driven Huffman
decoding (11-bit primary tables per coding group and a reference-semantics
walk for longer codes), move-to-front through 16-byte window moves, and
run-length symbols expanded as they are decoded; (B) a write-only
inverse-BWT vector build with recorded long runs written as arithmetic
sequences; (C) a dense pre-RLE1 byte chase; (D) a SWAR run search that gives
the exact output size, expansion with short fixed-size copies and bulk fills,
and the block CRC as slice-by-16 over contiguous output slices with long
fills advanced 16 or 64 bytes per table step. The crate docs describe each
stage.

The block randomisation table is data from libbzip2's `randtable.c`
(BSD-style licence, notice kept in `src/rand.rs`). Everything else was
written from the bzip2 format; libbzip2's `decompress.c` was read for the
exact acceptance rules.

## Tests

Every test input is real radar data resolved through `recast-radar-testdata`
by manifest id, or real bytes recompressed by the reference encoder. The
reference decoder is the `bzip2` crate's pure-Rust backend (`libbz2-rs-sys`,
a port of libbzip2 1.0.8). Tests that need a downloadable volume skip with a
note when it is unavailable offline; the committed KIWA real-time chunks
always run.

- `tests/real_records.rs`: every LDM record of four archive volumes
  (KTLX 2024-03-15, KILX 2026-04-18, the TDWR TSTL volume and the TBWI
  status-only stub, 276 records) and the two committed KIWA chunks decode to
  the reference bytes, through the single API and through `decode_two_into`
  with every record on each side of a pair; the two bench volumes also
  match the per-record SHA-256 lists recorded from C libbzip2 in
  `tests/golden/` and the whole-volume digests of the decoder shootout;
  multi-block streams (real bytes recompressed at levels 1 and 9, 57 and 7
  blocks) singly, paired and concatenated; the output limit exactly at the
  decoded size and one byte under, with and without a prefix, single and
  paired, and on a multi-block stream; randomised blocks (no real NEXRAD
  record has the randomised bit, so real records get the bit set and their
  CRCs patched, then the reference must accept them); headers, restoration
  and appending, level digits, padding bits, trailing bytes and paired calls
  with invalid or empty sides.
- `tests/corruption.rs`: seeded truncations (1,056 cases), single bit flips
  and byte bursts (924 cases) and paired calls with one corrupt side on
  records of all four volumes. No case may panic, accept what the reference
  rejects, or produce different output; with CRC checks off, the bytes
  emitted before a failure and the block where decoding stops must also
  match the reference.
- Unit tests: the CRC against a bitwise reference for every length, and the
  Huffman fast table plus slow walk against a transcription of libbzip2's
  decode walk on 3,000 random code-length sets (including over-full and
  incomplete codes) and exhaustively over all 14-bit prefixes for small
  alphabets.

`examples/differential_fuzz.rs` runs the same differential check on random
mutations of real records for a time budget on the stable toolchain;
`fuzz/` has the `bzip2` cargo-fuzz target.

Run the tests with `cargo test -p recast-radar-bzip2` (about 90 s
unoptimised) or `--release` (about 20 s).
