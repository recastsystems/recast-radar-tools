# Recorded real-time chunk traffic

Cassettes replayed by `tests/iterator.rs`. Each was recorded by `capture.sh`, which runs the real
`ChunkIterator` with its HTTPS transport against the public `unidata-nexrad-level2-chunks` bucket.
Every S3 response is stored verbatim: listing XML inline in the `.jsonl` file, chunk bytes in
`chunks/`. The file also holds the iterator configuration, every event with its wall-clock time and
the final counters. Nothing in these files was edited after capture.

The first five captures were made on 2026-09-17 between 01:50 and 01:54 UTC, the last two between
02:44 and 02:49 UTC.

| cassette | site | what it records | requests |
|---|---|---|---|
| `tlas-999-wrap` | TLAS (TDWR) | `Volume(999)` join. Volume 999 (start 01:34:43Z, 49 chunks S..E) was already complete. The next listing is volume **1** (01:40:42Z): the id after 999 is 1, and no id 0 exists in the bucket. | 2 |
| `tmco-710-abandoned` | TMCO (TDWR) | `Volume(710)` join. 710 (00:49:59Z) has 14 chunks and no End chunk. 711 holds one chunk keyed `19700101-000000-001-S`, so its volume time is not newer and it is skipped. 712 (00:57:31Z, 70 chunks) is newer: 710 is abandoned. | 4 |
| `phkm-live-join` | PHKM (WSR-88D) | Live `CurrentVolume` join. The id listing (226 ids: 1..=34 and 674..=865) selects 34 (01:48:36Z), which had 26 of its 55 chunks listed. The capture follows it live to the End chunk (LastModified 01:53:06Z), rolls over to 35 (01:53:10Z) and takes three chunks there. | 37 |
| `kmxx-offline` | KMXX (WSR-88D) | Live `CurrentVolume` join at a radar that stopped sending data on 2026-09-15. Volume 15 (14:46:55Z) has 25 chunks and no End chunk. The capture idles, probes ids 16 and 17 and relists the ids (still 9..15). Stall settings are shortened. | 16 |
| `tlas-next-volume-bytes` | TLAS (TDWR) | Live `NextVolume` join with downloads. The id listing (498 ids: 1, 2 and 504..999) selects 2 (01:46:42Z), which is skipped up to its End chunk. The capture then downloads the first three chunks of volume 3. | 22 |
| `pabc-rollover-leftover` | PABC (WSR-88D) | Live `Volume(42)` follow. PABC restarts its volume numbering every few hours, so ids keep volumes of earlier cycles. Id 42 listed volumes from 2026-09-15, 2026-09-16 and the one in progress (02:42:19Z), which is followed to its End chunk. Id 43 then held only a complete volume from 2026-09-15 21:39:55Z, which is not newer and is skipped for two polls, until the new volume 43 (02:46:54Z) appears beside it; its first three chunks are taken. | 31 |
| `tlas-chunk-too-large` | TLAS (TDWR) | `Volume(998)` walk with downloads and `max_chunk_bytes` = 4096. The Start chunks (263 and 265 bytes) download; each first Intermediate chunk (7546 bytes and up) is refused by the HTTPS transport from its Content-Length, and the volume is abandoned: 998 -> 999 -> 1. | 6 |

Chunk files (downloaded by `tlas-next-volume-bytes` and `tlas-chunk-too-large`):

| file | bytes | sha256 |
|---|---|---|
| `TLAS-998-20260917-012843-001-S` | 263 | `4c738b8c3f9bc723fc7190053687e31441b881114a777b2c50bbb9da1aa18371` |
| `TLAS-999-20260917-013443-001-S` | 265 | `dfeec3fd45f3e6aac91a448432539f9fc463ade0db4d0a4d59d67f80f00dfa66` |
| `TLAS-3-20260917-015242-001-S` | 262 | `e9e1587d18515a1374698f090652f9222bd3d6702ba685b4ddecb25233a9830b` |
| `TLAS-3-20260917-015242-002-I` | 7363 | `a17bcffe02bcb8bf88752551437411f88d134f87beeb990340637598d8f56a33` |
| `TLAS-3-20260917-015242-003-I` | 24018 | `60156285869bcdf98e2b804ba1c305c44adf893cfe1a11c0446aa46b2d4affba` |

A separate Python reader (not the crate) checked the three chunks. The Start chunk begins with
`AR2V0008.003` and ICAO `TLAS`, and its header time is 2026-09-17 01:52:42Z. It holds a metadata
record with Messages 2 and 5. Each Intermediate chunk holds one LDM bzip2 record with 120 Message 31
radials. The same reader parsed the recorded listing XML to derive the scenario expectations in the
tests: listed keys, the newest volume id and chunk order.

## Re-capturing

Volume ids repeat every 999 volumes, and the bucket purges volumes after about two days. The
historical joins (`volume:N`) therefore only work while those volumes are kept, and each live capture
records different volumes. Replay fails at the first request that differs from the recording. If the
iterator's request plan changes, re-capture with `capture.sh [scenario ...]`, choose sites and ids
that show the same situations, and update the scenario expectations in `tests/iterator.rs`.
