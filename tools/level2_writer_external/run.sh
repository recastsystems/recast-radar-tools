#!/bin/sh
# Read Level II files with RSL and LROSE Radx for
# `tools/level2_writer_check.py --external OUT`: the written files, the NEXRAD
# sources, and the DORADE sources (RadxConvert's CfRadial of those is the
# check's independent reading of the source; RSL fails on them).
#
#     sh tools/level2_writer_external/run.sh OUT FILE...
#
# Needs RSL 1.50 (librsl, rsl.h, wsr88d_decode_ar2v on PATH, the site table
# wsr88d_locations.dat), libtirpc, gcc and LROSE RadxConvert, as installed in
# the nexbench container. For each FILE it writes
#
#   OUT/rsl/NAME.bin   rsl_dump's reading (see rsl_dump.c), NAME.log its stderr
#   OUT/radx/NAME.nc   RadxConvert's CfRadial with float32 fields, split cuts
#                      and long-range sweeps kept as in the file; NAME.log
#   OUT/index.tsv      NAME, the site given to RSL, RSL's and RadxConvert's
#                      exit status
#
# A gzip file is also read unwrapped, as NAME.unwrapped: RSL gunzips a file
# and then takes its records as uncompressed (it crashes on gzip-wrapped LDM
# records), and RadxConvert fails on them too.
#
# RSL looks the site up in its table and refuses a file whose ICAO is not
# there; such files are read as KTLX (RSL then reports KTLX's location).
set -u
here=$(dirname "$0")
out=$1
shift
mkdir -p "$out/rsl" "$out/radx" "$out/unwrapped" "$out/work"
gcc -O2 -Wall -o "$out/rsl_dump" "$here/rsl_dump.c" -lrsl -ltirpc -lm || exit 1
printf '%s\n' \
    'preserve_sweeps = TRUE;' \
    'remove_long_range_rays = FALSE;' \
    'set_output_encoding_for_all_fields = TRUE;' \
    'output_encoding = OUTPUT_ENCODING_FLOAT32;' >"$out/radx.params"
table=$(dirname "$(command -v wsr88d_decode_ar2v)")/../lib/wsr88d_locations.dat
radx=${RADXCONVERT:-RadxConvert}
: >"$out/index.tsv"

read_one() {
    path=$1
    name=$2
    site=$(head -c 24 "$path" | tail -c 4)
    if ! grep -q "	$site	" "$table" 2>/dev/null; then
        site=KTLX
    fi
    # RSL and RadxConvert gunzip a file named .gz in place (renaming it),
    # so each gets a copy.
    cp "$path" "$out/work/$name"
    "$out/rsl_dump" "$out/work/$name" "$site" "$out/rsl/$name.bin" >"$out/rsl/$name.log" 2>&1
    rsl=$?
    rm -f "$out/work/$name" "$out/work/${name%.gz}"
    rm -f "$out/radx/$name.nc"
    cp "$path" "$out/work/$name"
    "$radx" -params "$out/radx.params" -f "$out/work/$name" -outdir "$out/radx" -outname "$name.nc" \
        >"$out/radx/$name.log" 2>&1
    radx_status=$?
    rm -f "$out/work/$name" "$out/work/${name%.gz}"
    printf '%s\t%s\t%s\t%s\n' "$name" "$site" "$rsl" "$radx_status" >>"$out/index.tsv"
}

for path in "$@"; do
    name=$(basename "$path")
    read_one "$path" "$name"
    if [ "$(head -c 2 "$path" | od -An -tx1 | tr -d ' \n')" = "1f8b" ]; then
        gzip -dc "$path" >"$out/unwrapped/$name" && read_one "$out/unwrapped/$name" "$name.unwrapped"
    fi
done
