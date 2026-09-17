#!/bin/sh
# Records the real-time chunk traffic that tests/iterator.rs replays.
#
# Each capture runs the real ChunkIterator (ReqwestTransport, the crate's
# HTTPS client) against the live unidata-nexrad-level2-chunks bucket and writes
# tests/fixtures/listings/<name>.jsonl:
#   - a header with the site, iterator configuration and stop condition,
#   - one line per request: time, URL, kind, and the verbatim response (listing
#     XML inline; chunk bytes written to chunks/<key with / as ->),
#   - one line per event the iterator produced (with wall-clock time),
#   - an end line with the counters.
# The capture sleeps for real on Idle/Retry, so live scenarios take minutes.
# A capture refuses to write more than 3 chunk files. Stop conditions
# (RECAST_CAPTURE_STOP, comma-separated key=value): events, chunks, completed,
# idles, chunks_after_rollover, abandoned, errors.
#
# Real-time volume ids cycle 1..=999 and volumes are purged after about two
# days, so the historical scenarios (volume:N joins) only work while those
# volumes are retained, and every live scenario records different volumes.
# After re-capturing, update the expectations in tests/iterator.rs.
#
# Usage (from anywhere): capture.sh [scenario ...]   (default: all)
set -eu

crate=$(cd "$(dirname "$0")/../../.." && pwd)

capture() {
    name=$1
    shift
    echo "== $name" >&2
    (cd "$crate" && env "$@" RECAST_CAPTURE_OUT="tests/fixtures/listings/$name.jsonl" \
        cargo test -p recast-radar-data --test iterator capture_cassette -- \
        --ignored --exact --nocapture)
}

scenario() {
    case $1 in
    # Walk TLAS (TDWR Las Vegas) from volume 999 across the id wrap to the
    # Start chunk of volume 1 (captured 2026-09-17 ~01:50Z; historical).
    tlas-999-wrap)
        capture "$1" RECAST_CAPTURE_SITE=TLAS RECAST_CAPTURE_JOIN=volume:999 \
            RECAST_CAPTURE_DOWNLOAD=0 RECAST_CAPTURE_STOP=chunks_after_rollover=1 \
            RECAST_CAPTURE_NOTE="historical walk across the 999 -> 1 volume id wrap"
        ;;
    # TMCO volume 710 has no End chunk; 711 holds a single chunk whose volume
    # time is 1970-01-01; 712 is the real successor (historical).
    tmco-710-abandoned)
        capture "$1" RECAST_CAPTURE_SITE=TMCO RECAST_CAPTURE_JOIN=volume:710 \
            RECAST_CAPTURE_DOWNLOAD=0 RECAST_CAPTURE_STALL_POLLS=1 RECAST_CAPTURE_PROBE_AHEAD=2 \
            RECAST_CAPTURE_STOP=chunks_after_rollover=1 \
            RECAST_CAPTURE_NOTE="abandoned volume without End; bogus 1970 volume time in the next id"
        ;;
    # Live join at PHKM (Kohala, Hawaii) while a volume is being collected;
    # follow it through its End chunk into the next volume.
    phkm-live-join)
        capture "$1" RECAST_CAPTURE_SITE=PHKM RECAST_CAPTURE_JOIN=current \
            RECAST_CAPTURE_DOWNLOAD=0 RECAST_CAPTURE_POLL_MS=5000 \
            RECAST_CAPTURE_STOP=chunks_after_rollover=3 RECAST_CAPTURE_MINUTES=15 \
            RECAST_CAPTURE_NOTE="live mid-volume join through one rollover"
        ;;
    # KMXX stopped sending data on 2026-09-15; the newest volume (15) has no
    # End chunk and nothing follows it. Short stall/rediscovery settings.
    kmxx-offline)
        capture "$1" RECAST_CAPTURE_SITE=KMXX RECAST_CAPTURE_JOIN=current \
            RECAST_CAPTURE_DOWNLOAD=0 RECAST_CAPTURE_POLL_MS=5000 RECAST_CAPTURE_STALL_POLLS=2 \
            RECAST_CAPTURE_PROBE_AHEAD=2 RECAST_CAPTURE_REDISCOVER_EVERY=2 \
            RECAST_CAPTURE_STOP=idles=8 \
            RECAST_CAPTURE_NOTE="offline radar: idle, probe, rediscover"
        ;;
    # Live NextVolume join at TLAS with downloads: skip the volume in progress
    # and download the first three chunks of the next one (TDWR chunks are
    # small: Start ~260 B, then ~8 KB and ~23 KB).
    tlas-next-volume-bytes)
        capture "$1" RECAST_CAPTURE_SITE=TLAS RECAST_CAPTURE_JOIN=next \
            RECAST_CAPTURE_DOWNLOAD=1 RECAST_CAPTURE_POLL_MS=10000 \
            RECAST_CAPTURE_STOP=chunks=3 RECAST_CAPTURE_MINUTES=15 \
            RECAST_CAPTURE_NOTE="live NextVolume join downloading three chunks"
        ;;
    # Live Volume(N) follow at PABC (Bethel, Alaska), which restarts its volume
    # numbering every few hours: N and N+1 still hold volumes of earlier
    # cycles. Pick N as the volume in progress whose successor id holds only
    # an older volume (captured 2026-09-17 02:44Z with N = 42).
    pabc-rollover-leftover)
        capture "$1" RECAST_CAPTURE_SITE=PABC RECAST_CAPTURE_JOIN=volume:42             RECAST_CAPTURE_DOWNLOAD=0 RECAST_CAPTURE_POLL_MS=5000             RECAST_CAPTURE_STOP=chunks_after_rollover=3 RECAST_CAPTURE_MINUTES=15             RECAST_CAPTURE_NOTE="live follow into a volume id that still holds an older cycle's volume"
        ;;
    # Historical TLAS walk with downloads and max_chunk_bytes below every
    # Intermediate chunk: each volume delivers its Start chunk and is then
    # abandoned (captured 2026-09-17 02:49Z; historical).
    tlas-chunk-too-large)
        capture "$1" RECAST_CAPTURE_SITE=TLAS RECAST_CAPTURE_JOIN=volume:998             RECAST_CAPTURE_DOWNLOAD=1 RECAST_CAPTURE_MAX_CHUNK_BYTES=4096             RECAST_CAPTURE_STOP=abandoned=2 RECAST_CAPTURE_MINUTES=5             RECAST_CAPTURE_NOTE="historical walk with max_chunk_bytes below the Intermediate chunk size: each volume is abandoned after its Start chunk"
        ;;
    *)
        echo "unknown scenario $1" >&2
        exit 2
        ;;
    esac
}

if [ $# -eq 0 ]; then
    set -- tlas-999-wrap tmco-710-abandoned phkm-live-join kmxx-offline tlas-next-volume-bytes         pabc-rollover-leftover tlas-chunk-too-large
fi
for name in "$@"; do
    scenario "$name"
done
