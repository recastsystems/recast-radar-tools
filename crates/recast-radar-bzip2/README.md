# recast-radar-bzip2

A bzip2 compressor and decompressor in Rust with `#![forbid(unsafe_code)]`
and no dependencies (rayon only with the `rayon` feature), written for NEXRAD
Level II LDM records, which are one bzip2 stream each. The decoder replaces
the `bzip2` crate in `recast-radar-io-nexrad`'s record path; the encoder
writes the records of the Level II writer.

```rust
use recast_radar_bzip2::{Decoder, Encoder, Error, Level};

fn encode_record(contents: &[u8]) -> Vec<u8> {
    let mut encoder = Encoder::new(Level::BEST); // keep one per thread
    let mut record = Vec::new();
    encoder.encode_into(contents, &mut record);
    record
}

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
| `Level` | Block size, `Level::new(1..=9)`, `Level::FASTEST` (1), `Level::BEST` (9, the default; what NEXRAD records use). |
| `Encoder::new(level)` | An encoder for one block size. Its work buffers are allocated on the first call and reused, so keep one encoder per thread. |
| `Encoder::encode_into(input, out)` | Compress `input` into one complete bzip2 stream (header, blocks, end-of-stream marker, combined CRC, padding) appended to `out`. Cannot fail; an empty input gives the 14-byte empty stream. |
| `encode_many(level, inputs)` | (feature `rayon`) Every input compressed into its own stream on the current rayon pool; `result[i]` is what `encode_into` writes for `inputs[i]`. The shape of a Level II volume's LDM records. Runs on a fresh `EncoderPool`: at most one encoder per worker thread, each reused for every input it compresses, freed on return. |
| `EncoderPool::new(level)`, `pool.encode_many(inputs)` | (feature `rayon`) The same, with the encoders kept between calls (at most one per thread that ran a call; `pool.idle_encoders()` counts them), for a writer that compresses volume after volume or real-time chunks as they fill. |
| `Decoder::new()` | A decoder with no output limit. The ~7 MiB of block work buffers are allocated on the first decode and reused, so keep one decoder per thread. |
| `Decoder::set_max_output(limit)` | Bound the bytes one call may append. Checked against a block's exact decoded size before that block's output is allocated. |
| `Decoder::decode_stream_into(input, out)` | Decode one stream (`BZh1`..`BZh9` header, any number of blocks, end-of-stream marker) from the start of `input`, appending to `out`. Bytes after the marker are ignored. On `Err`, `out` is back at its original length. |
| `Decoder::decode_two_into(a, out_a, b, out_b)` | Two independent streams decoded in lockstep on one thread, with their inverse-BWT chases interleaved so the cache misses overlap. Same results as two single calls. |
| `Error` | Why a decode failed: header, block magic, block header, Huffman tables, block data, truncation, block CRC, stream CRC, output limit. |

### What the encoder writes

The stream libbzip2 1.0.8 writes for the same input and level with
`BZ2_bzBuffToBuffCompress`, byte for byte: the same block boundaries,
Burrows-Wheeler transform, Huffman tables and selectors, so the compression
ratio is libbzip2's exactly. The one exception is a block that is an exact
repetition of a shorter string: several of its rows are identical, any of
them is a valid `origPtr`, and the one chosen here can differ from
libbzip2's (the stream decodes to the same bytes either way). NOAA writes
its LDM records with libbzip2, so re-encoding a record's contents gives back
the published record: all 1,850 records of the Level II test corpus do.

Block boundaries follow one `BZ_FINISH` call over the whole input, as
`BZ2_bzBuffToBuffCompress` makes it. Feeding libbzip2 through `BZ_RUN` first
(the `bzip2` crate's `write::BzEncoder`) differs only when the input ends
exactly where a block fills: there the final one-byte run starts a second
block instead of joining the first. The published records do not show which
of the two their writers used: all 1,850 records of the test corpus
re-encode the same way under both, as no record's contents end exactly where
a block fills. The choice rests on `BZ2_bzBuffToBuffCompress` being the
one-shot call, and is tested against the reference at that boundary.

### What the decoder accepts

What is accepted and what is rejected follows libbzip2 1.0.8: a stream
decodes to exactly the bytes libbzip2 produces, or returns an error.
Randomised blocks (bzip2 0.9.0 and earlier) decode, and selector counts above
18,002 are clamped as libbzip2 clamps them. Corrupt or truncated input never
panics.

## Memory

An encoder allocates about 22 bytes of zero-initialised address space per
byte of block capacity (about 20 MB at level 9) on its first call, of which
a block touches about 13 bytes per byte left after the initial run-length
stage. Memory does not depend on the input: a call allocates nothing else
but its output. A decoder allocates about 7 MiB and touches about five bytes
per block symbol.

## Performance

Encoding, on one core, against C libbzip2 1.0.8 (`bzip2-sys`) and
`libbz2-rs-sys` 0.2.5 (the `bzip2` crate's default backend). Inputs are the
decoded contents of every LDM record of the two bench volumes, each
compressed at level 9 as a Level II writer does. Instructions are callgrind
counts for encoding only (2026-09-25); time is on-CPU time of a pinned
thread, the minimum over five interleaved rounds on a shared host
(2026-09-24; a re-run on 2026-09-25 under heavier load had ours fastest in
every round). Rust 1.94, fat LTO, x86-64 Linux:

| Input | Instructions: ours / C / rs | Time: ours / C / rs |
|---|---|---|
| KTLX 2024-03-15, 97 records, 84 MB | 10.11 G / 12.88 G / 14.84 G | 1.20 s / 2.01 s / 1.81 s |
| KILX 2026-04-18, 106 records, 87 MB | 20.49 G / 21.30 G / 24.76 G | 2.04 s / 2.62 s / 2.83 s |
| a published real-time chunk (336 kB of bzip2 data) | 0.18 G / 0.35 G / 0.34 G | 11.1 ms / 18.8 ms / 15.1 ms |

Small inputs cost less than with C libbzip2 at every size measured, from
16 bytes (1.8 us per call against 2.1 us for C's one-shot compression)
through 1,000 bytes (68 against 94 us) to whole records. Repetitive input
stays linear: one byte value repeated takes 0.64 ns per input byte (C 5.4,
rs 5.2), a real 1,000-byte stretch repeated 28.7 ns (C 220, rs 186). On 16
threads, `encode_many` compresses the KTLX records at about 390 MB/s and a
kept `EncoderPool` at about 430 MB/s. Method, the small-input,
degenerate-input and thread tables and a cycle breakdown are in the
repository's `docs/perf/bzip2-encoder.md`; the harness is `bench/` (not part
of the crate: it links C libbzip2).

Decoding, on the `KTLX20240315_000217_V06` bench volume (97 LDM records,
10.8 MB compressed, 84 MB decoded), callgrind instruction counts for
decoding every record with a reused decoder, from the single-core decode
study that led to this crate (Rust 1.94, x86-64, release with fat LTO,
Linux):

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
The whole-volume effect inside `recast-radar-io-nexrad` (instructions,
pinned wall clock, multi-core, page faults, and the Windows bench) is in
the repository's `docs/perf/single-core.md`.

## Design

Encoding, per block: RLE1 with libbzip2's block boundaries (eight bytes at a
time where no two neighbours are equal) and the block CRC; the
Burrows-Wheeler transform by SA-IS in linear time on a rotation of the block
whose suffix order equals its rotation order (ending with a byte that occurs
once, or else the least rotation, found among starts of runs of the
smallest byte), with stage 1 as a direct sort of the LMS substrings
(counting sort on their first symbols, then packed integer keys) at every
level and branch-free induction passes that write the last column; move to
front over runs of the last column on a list of 32 words with a table of
the word that holds each byte; and libbzip2's Huffman table selection with
the six tables' costs of a symbol packed into one word. The crate docs
describe each step.

Decoding, per block, four stages: (A) a 64-bit bit window with table-driven
Huffman decoding (11-bit primary tables per coding group and a
reference-semantics walk for longer codes), move-to-front through 16-byte
window moves, and run-length symbols expanded as they are decoded; (B) a
write-only inverse-BWT vector build with recorded long runs written as
arithmetic sequences; (C) a dense pre-RLE1 byte chase; (D) a SWAR run search
that gives the exact output size, expansion with short fixed-size copies and
bulk fills, and the block CRC as slice-by-16 over contiguous output slices
with long fills advanced 16 or 64 bytes per table step.

The block randomisation table is data from libbzip2's `randtable.c`
(BSD-style licence, notice kept in `src/rand.rs`). Everything else was
written from the bzip2 format; libbzip2's `decompress.c` was read for the
exact acceptance rules, and its `compress.c`, `huffman.c` and `bzlib.c`
(through the libbz2-rs-sys port) for the block boundaries and table
selection that make the encoder's output identical.

## Tests

Every test input is real radar data resolved through `recast-radar-testdata`
by manifest id, or real bytes recompressed by the reference encoder or cut
and repeated from real records. The reference decoder and encoder are the
`bzip2` crate's pure-Rust backend (`libbz2-rs-sys`, a port of libbzip2
1.0.8). Tests that need a downloadable volume skip with a note when it is
unavailable offline; the committed KIWA real-time chunks always run.

- `tests/encode.rs`: every LDM record re-encodes to its published bytes (in
  release builds every record of every committed or cached Level II file
  and chunk, 1,850 records of 112 files; otherwise the four corpus volumes
  and the KIWA chunks); multi-block streams of real bytes at every level
  match the reference byte for byte and decode with both decoders; every
  prefix length around a level-1 block boundary, including the length at
  which the last input byte is the one that fills the block; empty and
  short inputs; appending and buffer reuse; periodic blocks (real stretches
  of 1 to 20,000 bytes repeated), which may differ from the reference only
  in a periodic block's `origPtr`; `encode_many` against `encode_into`, and
  an `EncoderPool` on three threads creating at most three encoders over
  two calls of 64 inputs; and, in release builds, every test corpus file
  (399 files, 513 MB) at level 9.
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
- Unit tests: the block sort and the least-rotation search against brute
  force on every string over three letters up to length 7 or 8 and over two
  letters up to 12 or 14 (including the periodicity flag), SA-IS against a
  sorted suffix list, both with one-byte and with two-byte stage-1 buckets;
  the CRC against a bitwise reference for every length; the Huffman fast
  table plus slow walk against a transcription of libbzip2's decode walk on
  3,000 random code-length sets (including over-full and incomplete codes)
  and exhaustively over all 14-bit prefixes for small alphabets.

`fuzz/` has the `bzip2` (decoder) and `bzip2_encode` (differential encoder:
each input and then its first quarter compressed by encoders reused across
inputs; each stream must match the reference encoder's and decode with our
decoder) cargo-fuzz targets. Two 15-minute `bzip2_encode` campaigns from
decompressed LDM records (7,728 inputs of up to 1.2 MB, 66,861 of up to
8 KiB) found nothing.
`examples/differential_fuzz.rs` runs the decoder check on random mutations
of real records for a time budget on the stable toolchain.

Run the tests with `cargo test -p recast-radar-bzip2 --features rayon`
(about 3 minutes unoptimised, the every-file test skipped) or `--release`
(about 30 s on 8 threads, with the whole corpus).
