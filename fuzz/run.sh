#!/usr/bin/env bash
# Run the fuzz targets in parallel, one libFuzzer worker per target, for a
# fixed time budget. Linux (or the nexbench container), nightly toolchain,
# cargo-fuzz installed; seeds/ generated first (see README.md).
#
#   fuzz/run.sh [SECONDS] [TARGET...]    default: 600 s, all sixteen targets
#
# Logs go to logs/<target>.log, new corpus entries to corpus/<target>/, and
# crash/timeout/OOM inputs to artifacts/<target>/. libFuzzer runs in fork mode
# with -ignore_crashes so one crash does not end the run before the budget.
set -euo pipefail
cd "$(dirname "$0")"

seconds=${1:-600}
shift || true
if [ "$#" -gt 0 ]; then
    targets=("$@")
else
    targets=(level2-volume level2-metadata level2-writer level2-writer-router io-router odim hdf5 cfradial dorade dorade-archive jma bzip2 bzip2-encode writers level3 polling-listing)
fi

cargo +nightly fuzz build -s none
mkdir -p logs
for target in "${targets[@]}"; do
    if [ ! -d "seeds/$target" ]; then
        echo "missing seeds/$target: run fuzz-tools seeds first" >&2
        exit 1
    fi
    mkdir -p "corpus/$target" "artifacts/$target"
    max_len=$(find "seeds/$target" -type f -printf '%s\n' | sort -n | tail -1)
    RAYON_NUM_THREADS=1 "target/x86_64-unknown-linux-gnu/release/$target" \
        -artifact_prefix="artifacts/$target/" \
        -max_total_time="$seconds" \
        -timeout=10 \
        -rss_limit_mb=2048 \
        -max_len="$max_len" \
        -fork=1 -ignore_crashes=1 -ignore_timeouts=1 -ignore_ooms=1 \
        -print_final_stats=1 \
        "corpus/$target" "seeds/$target" > "logs/$target.log" 2>&1 &
done
wait
for target in "${targets[@]}"; do
    echo "== $target: $(ls "artifacts/$target" | wc -l) artifact(s)"
    grep -E "^#[0-9]+:" "logs/$target.log" | tail -1
done
