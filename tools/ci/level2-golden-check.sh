#!/usr/bin/env bash
# Fails unless tools/level2_golden.py reproduces every committed golden file
# under testdata/level2/golden byte for byte (docs/level2/messages.md, "Golden
# files"). Run locally with
#
#     RECAST_RADAR_GOLDEN_PYTHON=/path/to/python bash tools/ci/level2-golden-check.sh
#
# The interpreter needs MetPy 1.7.1 and arm_pyart 2.2.5 (default: python3).
# The Rust test downloads every source file into the testdata cache
# ($RECAST_RADAR_TESTDATA when set) before it runs `level2_golden.py --check all`.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
export RECAST_RADAR_GOLDEN_PYTHON="${RECAST_RADAR_GOLDEN_PYTHON:-python3}"
cargo test --locked -p recast-radar-io-nexrad --test golden_script -- --ignored --nocapture
