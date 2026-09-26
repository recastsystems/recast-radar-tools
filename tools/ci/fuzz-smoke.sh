#!/usr/bin/env bash
# Short libFuzzer run of every fuzz target (a smoke test, not a campaign).
# Used by .github/workflows/ci.yml; run locally on Linux with
#
#     bash tools/ci/fuzz-smoke.sh [SECONDS]      default: 60 s per target
#
# Needs the nightly toolchain, cargo-fuzz and a C++ compiler (fuzz/README.md).
#
# 1. Replays every committed regression input (testdata tagged
#    `fuzz-regression`) through its target and `io-router` on stable, with
#    debug assertions and overflow checks (`fuzz-tools regressions`).
# 2. Writes the seed corpora from the real testdata manifest
#    (`fuzz-tools seeds`; files that are not committed are downloaded into
#    the testdata cache).
# 3. Runs all seven targets in parallel for SECONDS each (`fuzz/run.sh`).
# 4. Fails when any target saved a crash, timeout or out-of-memory input
#    under fuzz/artifacts/, and prints the inputs' names.
set -euo pipefail

seconds=${1:-60}
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

cargo run --locked --release --manifest-path fuzz/tools/Cargo.toml -- regressions
cargo run --locked --release --manifest-path fuzz/tools/Cargo.toml -- seeds

rm -rf fuzz/artifacts
bash fuzz/run.sh "$seconds"

findings=$(find fuzz/artifacts -type f 2>/dev/null | sort)
if [[ -n "$findings" ]]; then
    echo "fuzz findings (reproduce and minimize as fuzz/README.md describes):"
    echo "$findings"
    for target_log in fuzz/logs/*.log; do
        echo "== $target_log"
        grep -E "ERROR|panicked|deadly signal|Timeout|out-of-memory" "$target_log" | head -20 || true
    done
    exit 1
fi
echo "no fuzz findings"
