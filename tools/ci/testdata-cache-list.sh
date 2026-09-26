#!/usr/bin/env bash
# Sorted list (path relative to the cache, size in bytes) of the files in the
# testdata download cache, $RECAST_RADAR_TESTDATA, creating it if missing.
# Hidden files (in-progress `.part` downloads) are left out. ci.yml compares
# the list before and after `cargo test` to decide whether to save a new
# cache entry.
#
# The test job runs this on Linux, Windows (Git Bash) and macOS, so it uses
# only POSIX find, wc and awk: GNU find's `-printf` does not exist in the BSD
# find of macOS. `wc -c` prints "<size> <path>" per file (BSD wc pads the
# size with spaces) and a "<size> total" line per batch of several files;
# every path starts with "./", which tells the file lines from the totals.
set -euo pipefail

dir="${RECAST_RADAR_TESTDATA:?RECAST_RADAR_TESTDATA is not set}"
mkdir -p "$dir"
cd "$dir"
find . -type f ! -name '.*' -exec wc -c {} + |
    awk '{
        size = $1
        sub(/^[ \t]*[0-9]+[ \t]/, "")
        if (substr($0, 1, 2) == "./") printf "%s\t%s\n", substr($0, 3), size
    }' |
    LC_ALL=C sort
