#!/usr/bin/env python3
"""Check the recast-radar-hdf5 writer demos with independent readers.

The two examples of `recast-radar-hdf5` write one file each that uses every
structure the writers emit:

    cargo run --release -p recast-radar-hdf5 --example h5_write_demo -- demo.h5
    cargo run --release -p recast-radar-hdf5 --example nc4_write_demo -- demo.nc

This script reads them with h5py (the HDF Group's C library), netCDF4-python
(netCDF-C) and xarray (the `netcdf4` and `h5netcdf` engines), and checks every
attribute, dataset, layout, filter, reference, dimension scale and value
against what the examples write (the expectations below mirror their
source):

    python tools/hdf5_writer_check.py hdf5 demo.h5
    python tools/hdf5_writer_check.py netcdf4 demo.nc

Exit status 1 when any check fails. Run with the reference venv (h5py,
netCDF4, xarray, h5netcdf).
"""

import sys

import h5py
import numpy as np

FAILURES = []


def check(condition, what):
    if condition:
        print(f"ok    {what}")
    else:
        print(f"FAIL  {what}")
        FAILURES.append(what)


def text(value):
    if isinstance(value, bytes):
        return value.decode("utf-8")
    if isinstance(value, np.ndarray) and value.shape == ():
        return text(value[()])
    return value


# h5_write_demo: a 300 x 97 grid of u16 (i * 7 mod 65521) in three layouts.
ROWS, COLS = 300, 97
GRID = ((np.arange(ROWS * COLS, dtype=np.uint64) * 7) % 65_521).astype(np.uint16).reshape(ROWS, COLS)


def check_hdf5(path):
    f = h5py.File(path, "r")
    check(f.id.get_create_plist().get_version()[0] == 2, "superblock version 2")
    attrs = f.attrs
    check(text(attrs["Conventions"]) == "ODIM_H5/V2_4", "root Conventions (NUL-terminated string)")
    check(attrs.get_id("Conventions").get_type().get_strpad() == h5py.h5t.STR_NULLTERM,
          "Conventions is NUL-terminated")
    check(text(attrs["title"]) == "writer demo", "root title")
    # An empty string attribute has a null dataspace (as netCDF-C writes one).
    empty = attrs["empty"]
    check(isinstance(empty, h5py.Empty) or text(empty) == "", "root empty string")
    check(attrs["gain"].dtype == np.float64 and float(attrs["gain"]) == 0.5, "root gain f64 0.5")
    check(attrs["flags"].dtype == np.int8 and list(attrs["flags"]) == [-1, 2, 3], "root flags i8")
    check(attrs["count"].dtype == np.uint64 and int(attrs["count"][0]) == 2**64 - 1, "root count u64 max")
    names = [text(n) for n in attrs["names"]]
    check(names == ["alpha", "", "γ"], "root names: variable-length UTF-8 strings")

    group = f["group"]
    for name, layout in [("contiguous", h5py.h5d.CONTIGUOUS), ("chunked", h5py.h5d.CHUNKED),
                         ("many_chunks", h5py.h5d.CHUNKED)]:
        ds = group[name]
        check(ds.dtype == np.uint16 and ds.shape == (ROWS, COLS), f"{name}: u16 {ROWS} x {COLS}")
        check(ds.id.get_create_plist().get_layout() == layout, f"{name}: layout")
        check(np.array_equal(ds[()], GRID), f"{name}: every value")
    chunked = group["chunked"]
    check(chunked.chunks == (45, 40), "chunked: chunk shape 45 x 40")
    check(chunked.shuffle and chunked.compression == "gzip" and chunked.compression_opts == 6,
          "chunked: shuffle + deflate 6")
    check(chunked.fillvalue == 65_535, "chunked: fill value 65535")
    check(chunked.id.get_num_chunks() == 7 * 3, "chunked: 21 chunks stored (partial edge chunks)")
    many = group["many_chunks"]
    check(many.chunks == (1, COLS) and many.compression == "gzip" and many.compression_opts == 1,
          "many_chunks: one row a chunk, deflate 1")
    check(many.id.get_num_chunks() == ROWS, "many_chunks: 300 chunks")
    compact = group["compact"]
    check(compact.id.get_create_plist().get_layout() == h5py.h5d.COMPACT, "compact: compact layout")
    check(list(compact[()]) == [1.5, -2.0], "compact: values")
    check(group["scalar"].shape == () and int(group["scalar"][()]) == -7, "scalar: i32 -7")
    strings = [text(s) for s in group["strings"][()]]
    check(strings == ["azimuth_surveillance", "rhi"], "strings: variable-length")
    check(text(group["scalar_string"][()]) == "fixed", "scalar_string")
    unallocated = group["unallocated"]
    check(unallocated.shape == (5,) and unallocated.id.get_storage_size() == 0,
          "unallocated: 5 elements, no storage")
    check(np.array_equal(unallocated[()], np.zeros(5, dtype=np.float32)), "unallocated: reads the fill (0)")

    time, field = f["time"], f["field"]
    check(list(time[()]) == [0.0, 1.0], "time values")
    check(np.array_equal(field[()], np.array([[1, 2], [3, 4]], dtype=np.int16)), "field values")
    check(h5py.h5ds.is_scale(time.id), "time is a dimension scale")
    check(text(time.attrs["NAME"]) == "time", "time NAME")
    check(len(field.dims[0]) == 1 and field.dims[0][0] == time, "field dimension 0 attached to time")
    check(len(field.dims[1]) == 0, "field dimension 1 has no scale")
    refs = time.attrs["REFERENCE_LIST"]
    check(len(refs) == 1 and f[refs[0]["dataset"]] == field and int(refs[0]["dimension"]) == 0,
          "time REFERENCE_LIST names (field, 0)")
    check(f[attrs["target"]] == field, "root target: an object reference to /field")

    order = []
    f.id.links.iterate(lambda name: order.append(name.decode()), idx_type=h5py.h5.INDEX_CRT_ORDER)
    want = ["group", "time", "field"] + [f"sweep_{i}" for i in range(19, -1, -1)]
    check(order == want, "root links in creation order")
    f.close()


def check_netcdf4(path):
    import netCDF4
    import xarray as xr

    nc = netCDF4.Dataset(path)
    check(nc.data_model == "NETCDF4", "netCDF-C reads it as NETCDF4")
    check(len(nc.dimensions["sweep"]) == 2, "root dimension sweep = 2")
    check(nc.getncattr("Conventions") == "CF-1.8", "Conventions (char)")
    check(nc.getncattr("empty") == "", "empty char attribute")
    check(list(nc.getncattr("strings")) == ["a", "bc"], "string attribute array")
    centre = nc.getncattr("centre")
    check(np.asarray(centre).dtype == np.uint16 and int(np.asarray(centre).reshape(-1)[0]) == 7,
          "centre u16 attribute")
    check(list(nc["sweep_group_name"][:]) == ["sweep_0", "sweep_1"], "sweep_group_name strings")
    check(float(nc["latitude"][...]) == 35.333, "latitude scalar")
    for index in range(2):
        group = nc.groups[f"sweep_{index}"]
        check(len(group.dimensions["time"]) == 3 and len(group.dimensions["range"]) == 4,
              f"sweep_{index} dimensions")
        check(list(group["time"][:]) == [0.0, 1.0, 2.0], f"sweep_{index} time")
        check(list(group["range"][:]) == [125.0, 375.0, 625.0, 875.0], f"sweep_{index} range")
        dbzh = group["DBZH"]
        check(dbzh.dimensions == ("time", "range"), f"sweep_{index} DBZH dimensions")
        # Shuffling one-byte elements changes nothing: the writer leaves the
        # filter out for them.
        check(dbzh.chunking() == [3, 4] and dbzh.filters()["zlib"]
              and dbzh.filters()["complevel"] == 4 and not dbzh.filters()["shuffle"],
              f"sweep_{index} DBZH chunked 3 x 4, deflate 4 (no shuffle filter on bytes)")
        dbzh.set_auto_maskandscale(False)
        codes = dbzh[:]
        check(codes.dtype == np.uint8 and np.array_equal(codes, np.arange(12, dtype=np.uint8).reshape(3, 4)),
              f"sweep_{index} DBZH raw codes")
        dbzh.set_auto_maskandscale(True)
        values = dbzh[:]
        want = np.ma.masked_equal(np.arange(12).reshape(3, 4), 0) * 0.5 - 33.0
        check(bool(values.mask[0, 0]) and np.allclose(values.compressed(), want.compressed()),
              f"sweep_{index} DBZH unpacked, fill masked")
        check(np.asarray(dbzh.flag_values).reshape(-1).tolist() == [1]
              and dbzh.flag_meanings == "range_folded",
              f"sweep_{index} DBZH flags")
        check(group["sweep_mode"][...] == "azimuth_surveillance" or
              group["sweep_mode"][0] == "azimuth_surveillance", f"sweep_{index} sweep_mode string")
        monitoring = group.groups["monitoring"]
        check(np.allclose(monitoring["zdr_offset"][:], [0.1, 0.2, 0.3]), f"sweep_{index} monitoring")
    calibration = nc.groups["radar_calibration"]
    check(len(calibration.dimensions["calib"]) == 1 and list(calibration["time"][:]) == [0.0],
          "radar_calibration: a variable named like another group's dimension")
    nc.close()

    for engine in ("netcdf4", "h5netcdf"):
        tree = xr.open_datatree(path, engine=engine)
        sweep = tree["sweep_1"].to_dataset()
        check(sweep["DBZH"].dims == ("time", "range") and np.isnan(float(sweep["DBZH"][0, 0]))
              and float(sweep["DBZH"][2, 3]) == 11 * 0.5 - 33.0, f"xarray {engine}: sweep_1 DBZH")
        check(list(sweep["range"].values) == [125.0, 375.0, 625.0, 875.0], f"xarray {engine}: range")
        check(list(tree.to_dataset()["sweep_group_name"].values) == ["sweep_0", "sweep_1"],
              f"xarray {engine}: sweep_group_name")
        tree.close()

    # h5py: the dimension scales under the netCDF-4 layer.
    f = h5py.File(path, "r")
    check(h5py.h5ds.is_scale(f["sweep"].id), "h5py: /sweep is a dimension scale without a variable")
    dbzh = f["sweep_0/DBZH"]
    check(dbzh.dims[0][0] == f["sweep_0/time"] and dbzh.dims[1][0] == f["sweep_0/range"],
          "h5py: DBZH dimensions attached to time and range")
    f.close()


def main():
    if len(sys.argv) != 3 or sys.argv[1] not in ("hdf5", "netcdf4"):
        print(__doc__)
        return 2
    if sys.argv[1] == "hdf5":
        check_hdf5(sys.argv[2])
    else:
        check_netcdf4(sys.argv[2])
    print(f"{len(FAILURES)} failure(s)")
    return 1 if FAILURES else 0


if __name__ == "__main__":
    sys.exit(main())
