# Recorded real-time chunk traffic

Cassettes replayed by `tests/iterator.rs`. Each was recorded by `capture.sh`, which runs the real
`ChunkIterator` with its HTTPS transport against the public `unidata-nexrad-level2-chunks` bucket.
Every S3 response is stored verbatim: listing XML inline in the `.jsonl` file, chunk bytes in
`chunks/`. The file also holds the iterator configuration, every event with its wall-clock time and
the final counters. Nothing in these files was edited after capture.

All captures were made on 2026-09-17 between 01:50 and 01:54 UTC.

| cassette | site | what it records | requests |
|---|---|---|---|
| `tlas-999-wrap` | TLAS (TDWR) | `Volume(999)` join. Volume 999 (start 01:34:43Z, 49 chunks S..E) was already complete. The next listing is volume **1** (01:40:42Z): the id after 999 is 1, and no id 0 exists in the bucket. | 2 |
| `tmco-710-abandoned` | TMCO (TDWR) | `Volume(710)` join. 710 (00:49:59Z) has 14 chunks and no End chunk. 711 holds one chunk keyed `19700101-000000-001-S`, so its volume time is not newer and it is skipped. 712 (00:57:31Z, 70 chunks) is newer: 710 is abandoned. | 4 |
| `phkm-live-join` | PHKM (WSR-88D) | Live `CurrentVolume` join. The id listing (226 ids: 1..=34 and 674..=865) selects 34 (01:48:36Z), which had 26 of its 55 chunks listed. The capture follows it live to the End chunk (LastModified 01:53:06Z), rolls over to 35 (01:53:10Z) and takes three chunks there. | 37 |
| `kmxx-offline` | KMXX (WSR-88D) | Live `CurrentVolume` join at a radar that stopped sending data on 2026-09-15. Volume 15 (14:46:55Z) has 25 chunks and no End chunk. The capture idles, probes ids 16 and 17 and relists the ids (still 9..15). Stall settings are shortened. | 16 |
| `tlas-next-volume-bytes` | TLAS (TDWR) | Live `NextVolume` join with downloads. The id listing (498 ids: 1, 2 and 504..999) selects 2 (01:46:42Z), which is skipped up to its End chunk. The capture then downloads the first three chunks of volume 3. | 22 |

Chunk files (downloaded by `tlas-next-volume-bytes`):

| file | bytes | sha256 |
|---|---|---|
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
