#!/bin/bash
# Interleaved single-core comparison of recast-radar-bzip2, C libbzip2 and
# libbz2-rs-sys (Linux: needs taskset and /proc/thread-self/schedstat).
#
#   compare.sh C_BUILD RS_BUILD ROUNDS ITERS CPU INPUT...
#
# C_BUILD is the harness built with `--features c`, RS_BUILD the default
# build; INPUT is as for `bzip2-enc-bench time` (a Level II file, raw:FILE or
# cat:FILE). Each round runs ours (from C_BUILD), C libbzip2 and
# libbz2-rs-sys in turn, pinned to CPU, one warm-up and ITERS timed passes
# each; prints every result, then each encoder's minimum on-CPU time.
set -euo pipefail
c_build=$1; rs_build=$2; rounds=$3; iters=$4; cpu=$5; shift 5
declare -A best
for r in $(seq "$rounds"); do
  for cand in "$c_build ours" "$c_build ref" "$rs_build ref"; do
    read -r bin which <<< "$cand"
    line=$(taskset -c "$cpu" "$bin" time "$which" "$iters" "$@")
    ms=$(sed -E 's/.*on-CPU median [0-9.]+ ms, min ([0-9.]+) ms.*/\1/' <<< "$line")
    name=${line%%:*}
    echo "round $r  $name  $ms ms"
    if [ -z "${best[$name]:-}" ] || awk -v a="$ms" -v b="${best[$name]}" 'BEGIN { exit !(a < b) }'; then
      best[$name]=$ms
    fi
  done
done
for name in "${!best[@]}"; do
  echo "best  $name  ${best[$name]} ms"
done
