# bzip2 encoder: recast-radar-bzip2 against libbzip2

The compressor half of `recast-radar-bzip2` (branch `bzip2-enc`) measured
against C libbzip2 1.0.8 (the `bzip2-sys` build of the reference
implementation) and `libbz2-rs-sys` 0.2.5 (its Rust port, the `bzip2`
crate's default backend). Every number here was produced on one host with
the harness in `crates/recast-radar-bzip2/bench/`: on 2026-09-24, and again
on 2026-09-25 (instructions, single-core time, small inputs, `encode_many`
and fuzzing) after the changes for small inputs and `EncoderPool`.

## Output

The encoder writes the stream libbzip2 writes for the same input and block
size with `BZ2_bzBuffToBuffCompress`, byte for byte, so the compression
ratio is libbzip2's exactly (not merely within 1%). The only possible
difference is the 24-bit `origPtr` of a block that is an exact repetition of
a shorter string, where several rows are identical and each is valid.
Checked by:

- every LDM record of every committed or cached Level II file and real-time
  chunk of the test corpus, 1,850 records of 112 files: 1,662 records of 21
  NOAA volumes from 2020 to 2026 (WSR-88D and TDWR) and 75 real-time chunks,
  as published, plus 113 records of 16 trimmed fixtures cut from 1991 to 2026
  volumes, which the repository's fixture tool recompressed at level 9 (the
  1991 to 2016 volumes themselves are published gzip-wrapped and give no
  records). Each re-encodes, at the level in its header, to its published
  bytes (`tests/encode.rs`);
- every test corpus file (399 files, 513 MB) at level 9, multi-block streams
  at every level, prefixes around a block boundary, empty and short inputs,
  and periodic blocks: identical to the reference encoder (up to the
  `origPtr` exception), decoded to the input by both decoders;
- the `bzip2-encode` fuzz target (see `fuzz/README.md` and *Fuzzing*
  below).

Block boundaries follow one `BZ_FINISH` call over the whole input
(`BZ2_bzBuffToBuffCompress`). Feeding libbzip2 with `BZ_RUN` first gives a
different stream only for an input that ends exactly where a block fills.
The published records do not show which of the two their writers used: all
1,850 corpus records re-encode to the same bytes both ways (checked once
with the reference encoder, 2026-09-25). `tests/encode.rs` checks the
boundary itself against the reference, prefix by prefix.

## Method

Host: AMD Ryzen 9 9950X3D, Windows 11, Linux measurements in WSL2/Docker
containers (Ubuntu 24.04, kernel 6.6.87, glibc 2.39). rustc 1.94.0,
`lto = "fat"`, `codegen-units = 1`; C libbzip2 built by `bzip2-sys` 0.1.13
(gcc 13, `-O3`). The machine was shared with other builds throughout, so the
timing tables use the minimum over interleaved rounds and only compare
encoders measured in the same round.

Inputs: the LDM records of `KTLX20240315_000217_V06` (97 records, 84.0 MB
decoded, 22.4 M bytes after RLE1) and `KILX20260418_013553_V06` (106
records, 87.3 MB decoded, 46.5 M bytes after RLE1), each record's decoded
contents compressed at its own level (9), as a Level II writer does; the
KTLX records joined into one 84 MB input (level 9, 900 kB blocks); and a
real-time chunk as published (`l2chunk-kiwa-307-20260917-003629-046-i`,
336 kB of bzip2 data: already compressed, the flat-histogram worst case).

1. Instructions: `valgrind --tool=callgrind --toggle-collect='*encode_all*'`
   around one pass over all inputs (`bzip2-enc-bench once`), so input
   decoding is excluded. Deterministic.
2. Time: `bzip2-enc-bench time` in a privileged container, pinned with
   `taskset` to one CPU, 1 warm-up pass then 3 timed passes (30 for the
   chunk); on-CPU time of the thread (`/proc/thread-self/schedstat`), which
   excludes time the thread waits for the CPU. Five rounds, each running
   ours, C and libbz2-rs-sys in turn (`bench/compare.sh`); the table gives
   each encoder's minimum.
3. Degenerate inputs (`bzip2-enc-bench patterns`): 8 MiB inputs built from
   the KTLX records (one byte value; every byte repeated 64 times; a real
   stretch of 7, 1,000 and 100,000 bytes repeated; the compressed volume
   itself), level 9, best of 4 runs, wall time per input byte.
4. Many records: `bzip2-enc-bench par`, the KTLX records on a rayon pool
   of 1 to 16 threads, with the process pinned to CPUs 0-15. Three ways to
   compress them, interleaved pass by pass after a warm-up pass: `encode_many`
   (a fresh `EncoderPool` per call), one `EncoderPool` kept across passes,
   and a new encoder per rayon job (rayon's `map_init` calls its init once
   per job, not once per thread; this is how `encode_many` worked at first).
   Median of 15 passes (wall time).
5. Small inputs: `bzip2-enc-bench small`, prefixes of 16 bytes to 300 kB of
   the contents of KTLX record 1 at level 9. Ours reuses one encoder; the
   reference compresses one-shot (`BZ2_bzCompressInit`, one `BZ_FINISH`
   call, `BZ2_bzCompressEnd`), which is how each library is meant to be
   called for one record. Calls alternate between the two; 400 calls each,
   pinned to one CPU, three rounds. Instructions for 2,001 calls on 16 and
   100 bytes with callgrind (`bzip2-enc-bench time`).

Build the harness with `cargo build --release` (reference libbz2-rs-sys) or
`cargo build --release --features c` (reference C libbzip2) in
`crates/recast-radar-bzip2/bench/`; see its `main.rs` for the modes.

## Results

Instructions (callgrind Ir, encoding only; 2026-09-25):

| Input | recast-radar-bzip2 | C libbzip2 | libbz2-rs-sys | C / ours | rs / ours |
|---|---:|---:|---:|---:|---:|
| KTLX, 97 records | 10.11 G | 12.88 G | 14.84 G | 1.27 | 1.47 |
| KILX, 106 records | 20.49 G | 21.30 G | 24.76 G | 1.04 | 1.21 |
| KIWA chunk (compressed bytes) | 0.178 G | 0.350 G | 0.344 G | 1.96 | 1.93 |

The references' counts are the same as on 2026-09-24 to the instruction.
Ours were 10.146 G, 20.541 G and 0.179 G before the small-input changes,
which touch no large-block loop.

On-CPU time, one core (minimum over five interleaved rounds):

| Input | recast-radar-bzip2 | C libbzip2 | libbz2-rs-sys | C / ours | rs / ours |
|---|---:|---:|---:|---:|---:|
| KTLX, 97 records | 1,195 ms | 2,014 ms | 1,813 ms | 1.69 | 1.52 |
| KILX, 106 records | 2,038 ms | 2,619 ms | 2,827 ms | 1.29 | 1.39 |
| KTLX records joined, 84 MB | 1,122 ms | 1,743 ms | 1,573 ms | 1.55 | 1.40 |
| KIWA chunk (compressed bytes) | 11.1 ms | 18.8 ms | 15.1 ms | 1.69 | 1.36 |

Re-run on 2026-09-25 after the small-input changes, under heavier load
(load average up to 21 on 32 CPUs), the minima were, ours / C / rs: KTLX
1,145 / 1,938 / 2,091 ms, KILX 2,358 / 3,600 / 3,716 ms, KTLX joined
1,231 / 1,993 / 1,753 ms and the KIWA chunk 15.0 / 26.2 / 26.2 ms; ours was
the fastest in every round of every input.

The KILX blocks are twice as large after RLE1 (its moments compress less),
so their suffix sorting leaves the core's L2 cache; libbzip2's comparison
sort needs relatively fewer instructions on that less repetitive data,
which is where its instruction count comes closest.

Degenerate inputs (level 9, ns per input byte; output sizes are identical
for all three):

| Input (8 MiB) | recast-radar-bzip2 | C libbzip2 | libbz2-rs-sys |
|---|---:|---:|---:|
| real records (first 8 MiB of KTLX) | 13.3 | 26.1 | 23.3 |
| one byte value | 0.64 | 5.40 | 5.18 |
| long runs (each byte ×64) | 0.66 | 5.50 | 6.07 |
| period 7 | 23.7 | 121.9 | 107.5 |
| period 1,000 | 28.7 | 220.3 | 185.6 |
| period 100,000 | 2.80 | 21.07 | 15.94 |
| compressed bytes | 62.7 | 93.6 | 86.5 |

libbzip2 falls back from its main sort to a slower doubling sort on
repetitive blocks; SA-IS is linear on every input, and the periodic cases
cost about as much per byte as real records.

Small inputs (2026-09-25; microseconds per call, the median of three
rounds' medians; "before" is the encoder as first committed, with
two-byte stage-1 buckets at every size and the Huffman work arrays zeroed
on every call):

| Input | before | recast-radar-bzip2 | C libbzip2 | C / ours |
|---:|---:|---:|---:|---:|
| 16 B | 42.5 | 1.8 | 2.1 | 1.17 |
| 100 B | 50.2 | 8.2 | 9.8 | 1.20 |
| 1,000 B | 124.2 | 68.3 | 93.8 | 1.37 |
| 2,432 B | 296.8 | 229.1 | 299.2 | 1.31 |
| 10 kB | 720.0 | 641.6 | 893.3 | 1.39 |
| 30 kB | 1,300.7 | 1,252.3 | 1,787.9 | 1.43 |
| 100 kB | 2,833.4 | 2,712.8 | 3,902.1 | 1.44 |
| 300 kB | 6,287.5 | 5,938.9 | 9,381.3 | 1.58 |

The fixed cost of a call was the top level's two-byte bucket array (65,536
counters filled, summed and scanned for every block) and about 6 KB of
Huffman work arrays zeroed on each of up to 24 code-length builds per block.
Blocks with fewer than 4,096 LMS positions now bucket on one byte, and the
work arrays live with the encoder. In instructions, 16 bytes now take 24,600
per call against C's 27,700, and 100 bytes 155,300 against 156,200 (2,001
calls each under callgrind); at that size most of both goes to the Huffman
code-length builds.

Many records (2026-09-25; the KTLX records, median wall time of 15 passes,
CPUs 0-15 of a shared host):

| Threads | `encode_many` | `EncoderPool` kept | new encoder per job |
|---:|---:|---:|---:|
| 1 | 1,437 ms | 1,411 ms | 1,411 ms |
| 4 | 391 ms | 403 ms | 445 ms |
| 8 | 252 ms (334 MB/s) | 222 ms (378 MB/s) | 292 ms |
| 16 | 214 ms (393 MB/s) | 196 ms (429 MB/s) | 281 ms |

A kept pool held exactly one encoder per thread. A new encoder per rayon
job is what `encode_many` did at first: rayon's `map_init` calls its init
once per job, not once per thread, and each new encoder allocates and first
touches its own work buffers (about 20 MB at level 9). With an encoder
created per job, the test that now guards this
(`encode_many_pool_reuses_encoders`: 64 inputs, 3 threads) saw 23 encoders
created in one call, where the pool may create at most 3. Two other series
on the same host, under heavier load, gave the same order at 8 and 16
threads with times up to 1.4 times higher; at 1 to 4 threads the three are
within the noise of each other. The 2026-09-24 table (530.7 MB/s at 16
threads) was measured under different load and is not comparable.

## Fuzzing

`bzip2-encode` (see `fuzz/README.md`) compresses each input, then its first
quarter with the same encoder, and checks both streams against the
reference encoder and decoders; the encoders and the decoder are reused
across inputs. Four libFuzzer campaigns ran at the same time on 2026-09-25
in the nexbench container, 15 minutes each on one pinned CPU, from the six
seeds of `fuzz-tools seeds` (decompressed LDM records and one LDM record),
without a sanitizer, with the `run.sh` options: this harness and the first
version (a fresh encoder and decoder per input, one stream per input), each
with `-max_len` at the largest seed (1,194,720 bytes) and at 8,192 bytes.

| `-max_len` | this harness | first version |
|---:|---:|---:|
| 1,194,720 | 7,728 inputs | 2,607 inputs |
| 8,192 | 66,861 inputs | 60,087 inputs |

None of the four found a crash, timeout or out-of-memory input. Replayed
over one fixed corpus (the 960 inputs an earlier campaign of the first
version kept), this harness took 42 to 45 s against 47 s, while it also
compresses every input's first quarter. In an earlier set of campaigns,
both first-version runs left one timeout input each; they replay in 12 to
22 ms, so they were stalls of the shared host.

## Where the time goes

Hardware counters (perf, cycles sampled in a privileged container) on the
KILX records put the encoder's cycles at: move-to-front 18%, stage 1 of
SA-IS (the direct LMS-substring sort and naming, all levels) 22%, the final
BWT induction passes 17%, the recursion's induction passes 10%, Huffman
table selection and coding 10%, RLE1 and the block CRC 7%, the rest (least
rotation, LMS placement, bitmaps) 13%. Move-to-front runs at about 19
cycles per move and the final S pass at about 6.5 cycles per slot on KTLX:
both are bound by memory latency (the list state, random text reads), not
by instruction count.

What moved the numbers, in order (each step verified against the reference
on every corpus record):

1. SA-IS on the block's least rotation instead of libbzip2's comparison
   sort, with the last column written by the final induction pass.
2. Branch-free induction passes (a dummy slot instead of a branch for
   skipped entries) and a pad symbol before the text: about half of the
   slots are skipped in no predictable pattern on radar data.
3. Rotation search only among starts of runs of the smallest byte, or none
   when a byte occurs once in the block.
4. Move-to-front on runs of the last column, with the list as 32 whole
   words plus the word of each byte value (a partial-word store followed
   by a wider load cost a store-forwarding stall per move).
5. Stage 1 of SA-IS as a direct sort of the LMS substrings (counting sort on
   the first symbols, packed integer keys) at every level, instead of two
   induction passes over the whole text: 12% fewer encoder instructions on
   KILX, and the lead over both references there grew from about 1.1-1.2
   to 1.3-1.8 times in same-round timings.
6. For small inputs: one-byte stage-1 buckets below 4,096 LMS positions
   (the bucket choice is a constant per compiled copy, so large blocks keep
   their fixed shifts), the Huffman code-length work arrays kept with the
   encoder instead of zeroed per call, with masked indices and depths
   computed from the root down: 16-byte inputs went from 20 times C's time
   to 0.86 of it, and large inputs lost 0.2-0.4% of their instructions.
