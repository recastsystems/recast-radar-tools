#!/usr/bin/env bash
# Build every harness of tools/xlib-bench inside the `nexbench` container
# (Ubuntu 24.04 with RSL 1.50 under /usr/local, LROSE under /usr/local/lrose,
# go-nexrad cloned at /build/go-nexrad, danielway/nexrad at /build/nexrad).
# The clones must be at the revisions the page was measured with; the
# Python libraries are pinned in requirements-linux.txt (pip freeze of the
# benchmark virtual environment).
#
#   build_linux.sh SRC OUT
#
# SRC is a checkout of this repository, OUT receives bin/ and the build dirs.
set -euo pipefail
SRC=$(realpath "$1")
OUT=$(realpath -m "$2")
HERE="$SRC/tools/xlib-bench"
mkdir -p "$OUT/bin"
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-4}

# The revisions of docs/perf/cross-library.md.
require_rev() {
  local clone=$1 rev=$2 head
  head=$(git -C "$clone" rev-parse HEAD)
  if [[ $head != "$rev"* ]]; then
    echo "$clone is at $head, not $rev: git -C $clone checkout $rev" >&2
    exit 1
  fi
}
require_rev /build/nexrad 1591b64
require_rev /build/go-nexrad 78ea27b

# recast-radar-tools, with and without paired bzip2 record decode.
( cd "$SRC" && CARGO_TARGET_DIR="$OUT/target" cargo build --release -q -p recast-radar-bench --bin decode_bench )
cp "$OUT/target/release/decode_bench" "$OUT/bin/decode_bench"
( cd "$SRC" && CARGO_TARGET_DIR="$OUT/target-paired" cargo build --release -q -p recast-radar-bench --bin decode_bench --features paired-bzip2 )
cp "$OUT/target-paired/release/decode_bench" "$OUT/bin/decode_bench_paired"

# RSL.
gcc -O2 -o "$OUT/bin/rsl_bench" "$HERE/rsl_bench.c" -I/usr/local/include -L/usr/local/lib -lrsl -ltirpc -lbz2 -lz -lm -Wl,-rpath,/usr/local/lib

# LROSE Radx.
g++ -O2 -std=c++17 -o "$OUT/bin/lrose_bench" "$HERE/lrose_bench.cc" -I"$HERE" \
  -I/usr/local/lrose/include -L/usr/local/lrose/lib \
  -lRadx -lNcxx -ltoolsa -ldataport -lnetcdf -lhdf5_serial_cpp -lhdf5_serial -lbz2 -lz -lpthread \
  -Wl,-rpath,/usr/local/lrose/lib

# go-nexrad.
rm -rf "$OUT/go-nexrad" && cp -r "$HERE/go-nexrad" "$OUT/go-nexrad"
( cd "$OUT/go-nexrad" \
  && go mod edit -require=github.com/bwiggs/go-nexrad@v0.0.0 -replace=github.com/bwiggs/go-nexrad=/build/go-nexrad \
  && go mod tidy >/dev/null 2>&1 \
  && go build -o "$OUT/bin/go_nexrad_bench" . )

# danielway/nexrad: the harness's git dependency replaced by the local clone.
rm -rf "$OUT/nexrad-crate" && cp -r "$HERE/nexrad-crate" "$OUT/nexrad-crate"
sed -i 's#{ git = "https://github.com/danielway/nexrad", rev = "1591b64",#{ path = "/build/nexrad/nexrad-data",#' "$OUT/nexrad-crate/Cargo.toml"
( cd "$OUT/nexrad-crate" && cargo build --release -q )
cp "$OUT/nexrad-crate/target/release/nexrad-crate-bench" "$OUT/bin/nexrad_crate_bench"

ls -la "$OUT/bin"
