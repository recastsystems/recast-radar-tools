#!/usr/bin/env python3
"""Derive a netCDF-4 CfRadial 1 fixture with szip- and LZF-compressed planes.

recast-radar-hdf5 decodes deflate, shuffle and Fletcher-32 and refuses every
other HDF5 filter with a typed `Error::UnsupportedFilter`. No radar file in
the corpus uses another filter (every producer probed writes deflate or
nothing), so this script makes one from real data: it copies the Py-ART
X-SAPR netCDF-4 CfRadial 1 file (corpus entry
cfrad1-xsapr-sgp-20110520-ppi-netcdf4) and adds its own
`reflectivity_horizontal` values twice more:

- `reflectivity_szip(time, range)`: the same stored values and attributes,
  compressed with szip (HDF5 filter 4; nearest-neighbour coding, 8 pixels
  per block) by netCDF-C through netCDF4-python (libaec);
- `reflectivity_lzf(time, range)`: the same stored values, compressed with
  LZF (HDF5 filter 32000, registered by h5py) through h5py, attached to the
  `time` and `range` dimension scales so netCDF-C sees its dimensions.

Both read back unchanged through netCDF4-python and h5py (checked at the
end). Run with the reference venv (netCDF4 1.7.4 / netCDF-C 4.9.3 / HDF5
1.14.6, h5py 3.16.0 / HDF5 2.0.0 when the fixture was made):

    python tools/derive_hdf5_filters.py <source.nc> <output.nc>
"""

import shutil
import sys

import h5py
import netCDF4
import numpy as np


def main(source, output):
    shutil.copyfile(source, output)
    with netCDF4.Dataset(output, "a") as nc:
        src = nc["reflectivity_horizontal"]
        src.set_auto_maskandscale(False)
        raw = src[:]
        attrs = {name: src.getncattr(name) for name in src.ncattrs() if name != "_FillValue"}
        fill = src.getncattr("_FillValue") if "_FillValue" in src.ncattrs() else None
        szip = nc.createVariable(
            "reflectivity_szip",
            src.dtype,
            src.dimensions,
            compression="szip",
            szip_coding="nn",
            szip_pixels_per_block=8,
            chunksizes=raw.shape,
            fill_value=fill,
        )
        szip.set_auto_maskandscale(False)
        szip.setncatts(attrs)
        szip[:] = raw
    with h5py.File(output, "a") as h5:
        lzf = h5.create_dataset(
            "reflectivity_lzf", data=raw, chunks=raw.shape, compression="lzf"
        )
        lzf.dims[0].attach_scale(h5["time"])
        lzf.dims[1].attach_scale(h5["range"])
    # Both read back unchanged.
    with netCDF4.Dataset(output) as nc:
        nc["reflectivity_szip"].set_auto_maskandscale(False)
        assert np.array_equal(nc["reflectivity_szip"][:], raw)
        assert nc["reflectivity_szip"].filters()["szip"]
    with h5py.File(output, "r") as h5:
        assert np.array_equal(h5["reflectivity_lzf"][...], raw)
        assert h5["reflectivity_lzf"].compression == "lzf"
        plist = h5["reflectivity_szip"].id.get_create_plist()
        ids = [plist.get_filter(i)[0] for i in range(plist.get_nfilters())]
        assert 4 in ids, ids
    print(f"{output}: reflectivity_szip filters {ids}, reflectivity_lzf lzf, {raw.shape} {raw.dtype}")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
