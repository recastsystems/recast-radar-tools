#!/usr/bin/env bash
# Sorted list (path relative to the cache, size in bytes) of the files in the
# testdata download cache, $RECAST_RADAR_TESTDATA, creating it if missing.
# Hidden files (in-progress `.part` downloads) are left out. ci.yml compares
# the list before and after `cargo test` to decide whether to save a new
# cache entry.
set -euo pipefail

dir="${RECAST_RADAR_TESTDATA:?RECAST_RADAR_TESTDATA is not set}"
mkdir -p "$dir"
find "$dir" -type f ! -name '.*' -printf '%P\t%s\n' | LC_ALL=C sort
