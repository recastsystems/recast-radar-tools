#!/usr/bin/env python3
"""Derive the CfRadial 2 (netCDF-4 group layout) fixtures from real files.

No operational feed publishes CfRadial 2. The two writers that produce it
are LROSE Radx (`RadxConvert -cf2`, class Cf2RadxFile) and xradar
(`xradar.io.to_cfradial2`), so the fixtures are real radar files written by
each of them. Nothing in the data is changed except where stated.

Radx (run inside the `nexbench` container, LROSE release 2025-08 build):

    cfrad2-radx-irene-sr2-20110827-120420-sur-r30km
        RadxConvert -f <IRENE classic CfRadial 1.3> -cf2 -max_range 30
        (source: corpus entry cfrad1-irene-sr2-20110827-120420-sur-sweeps01;
        gates beyond 30 km dropped by Radx, 400 of 1107 kept)

    cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32
        step 1 (this script, `iesha-subset`): h5py copies /, /what, /where,
        /how and datasets 7-10 (8.5, 10.1, 20 and 90 deg; 350/350/240/100
        gates) of corpus entry odim-iesha-20260305-0115-pvol into a new file
        as dataset1-4, every attribute and data plane as stored;
        step 2: RadxConvert -f <subset> -cf2 -to_int32
        (Radx rescales every field to int32 with a per-sweep scale_factor
        and add_offset)

xradar 0.12.0 (this script, `xradar`):

    cfrad2-xradar-xsapr-sgp-20110520-ppi
        open_cfradial1_datatree(<xsapr classic>) then to_cfradial2
        (source: cfrad1-xsapr-sgp-20110520-ppi-classic)

    cfrad2-xradar-dow8-20211011-223602-rhi-r300
        open_cfradial1_datatree(<DOW8 3-field classic>), every sweep group
        cut to its first 300 of 950 range gates with isel, then
        to_cfradial2 (source: cfrad1-dow8-20211011-223602-rhi-trim3-classic)

Radx writes its run time into `created`/`history`, so its files are not
byte-reproducible; the xradar files are.

Usage (reference venv):

    python tools/derive_cfradial2.py iesha-subset <iesha.h5> <subset.h5>
    python tools/derive_cfradial2.py xradar <xsapr_classic.nc> <out.nc>
    python tools/derive_cfradial2.py xradar-trim <dow8_classic.nc> <out.nc> 300
"""

import sys

import h5py


def iesha_subset(source, target):
    src = h5py.File(source, "r")
    dst = h5py.File(target, "w")
    for key, value in src.attrs.items():
        dst.attrs[key] = value
    for group in ("what", "where", "how"):
        src.copy(src[group], dst, group)
    for new, old in enumerate(range(7, 11), start=1):
        src.copy(src[f"dataset{old}"], dst, f"dataset{new}")
    dst.close()


def xradar_cf2(source, target, gates=None):
    import xradar as xd

    tree = xd.io.open_cfradial1_datatree(source)
    if gates is not None:
        for node in tree.subtree:
            if "range" in node.dims:
                node.dataset = node.to_dataset().isel(range=slice(0, gates))
    xd.io.to_cfradial2(tree, target)


def main():
    mode = sys.argv[1]
    if mode == "iesha-subset":
        iesha_subset(sys.argv[2], sys.argv[3])
    elif mode == "xradar":
        xradar_cf2(sys.argv[2], sys.argv[3])
    elif mode == "xradar-trim":
        xradar_cf2(sys.argv[2], sys.argv[3], int(sys.argv[4]))
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()
