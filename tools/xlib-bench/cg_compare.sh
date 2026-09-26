#!/usr/bin/env bash
# Instruction counts (callgrind Ir, whole process, one decode, one thread) of
# the compiled Level II decoders on the corpus: recast-radar-tools, the
# danielway/nexrad crate, RSL and LROSE (go-nexrad's runtime crashes
# valgrind 3.22). Unlike wall clock, Ir does not depend on machine load.
# Run inside nexbench after build_linux.sh:
#
#   cg_compare.sh OUT DATA BIN
set -u
OUT=$1; DATA=$2; BIN=$3
mkdir -p "$OUT"
FILES="KTLX20240315_000217_V06 KILX20260418_013553_V06 KTLX20130520_201643_V06.gz KLIX20050829_130035.gz"
run() {
  local name=$1; shift
  valgrind --tool=callgrind $TOGGLE --callgrind-out-file="$OUT/$name.out" "$@" > "$OUT/$name.json" 2> "$OUT/$name.log" < <(echo go)
  echo "$name $(grep -o 'Collected : [0-9]*' "$OUT/$name.log" | cut -d' ' -f3)"
}
for f in $FILES; do
  # decode_bench's timed region only (file read + decode): its summary hashes
  # every gate after the decode, which the other harnesses do not do.
  TOGGLE="--toggle-collect=*timed_work*" run "recast-$f" "$BIN/decode_bench" --format l2 "$DATA/$f" --iters 1 --warmup 0 --threads 1 --from-path
  TOGGLE=
  RAYON_NUM_THREADS=1 run "nexrad-crate-$f" "$BIN/nexrad_crate_bench" l2 "$DATA/$f" 1 0
  run "rsl-$f" "$BIN/rsl_bench" l2 "$DATA/$f" 1 0
  run "lrose-$f" "$BIN/lrose_bench" l2 "$DATA/$f" 1 0
done
