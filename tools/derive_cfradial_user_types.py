#!/usr/bin/env python3
"""Derive a netCDF-4 CfRadial 1 fixture with netCDF-4 user-defined types.

No CfRadial producer seen writes user-defined types, but netCDF-4
allows them and the decoder must keep what it parses. This script copies the
Py-ART X-SAPR netCDF-4 CfRadial 1 file (corpus entry
cfrad1-xsapr-sgp-20110520-ppi-netcdf4) and adds, from its own values:

- a compound type `pair_t` {ray: int, angle: double} and a variable
  `pairs(sweep)` holding each sweep's `sweep_start_ray_index` and
  `fixed_angle`;
- an enumerated type `flag_t` (ubyte: none = 0, echo = 1) and a variable
  `echo_flag(sweep)`: 1 where the sweep has a reflectivity gate of 20 dBZ or
  more;
- a variable-length type `rays_t` (int) and a variable `echo_rays(sweep)`:
  the rays of each sweep (index within the sweep) with a reflectivity gate
  of 20 dBZ or more;
- (with h5py, on the committed types netCDF-C reads back) a global compound
  attribute `sweep_pair` = the first sweep's (ray count, fixed angle), a
  global enumerated attribute `echo_state` = the first sweep's flag, and an
  opaque type `angle_bytes_t` (4 bytes) with a variable
  `fixed_angle_bytes(sweep)`: each sweep's `fixed_angle` as its
  little-endian float32 bytes, and a global opaque attribute
  `first_fixed_angle_bytes` = the first sweep's.

netCDF4-python reads the first five back (checked at the end); it skips
opaque variables and attributes ("unsupported datatype"), so h5py checks
those here and netCDF-C's `ncdump` lists them (run by hand in the nexbench
container, recorded in the manifest). Run with the
reference venv (netCDF4 1.7, h5py 3.16 / HDF5 2.0 when the fixture was made):

    python tools/derive_cfradial_user_types.py <source.nc> <output.nc>
"""

import shutil
import sys

import h5py
import netCDF4
import numpy as np


def main(source, output):
    shutil.copyfile(source, output)
    nc = netCDF4.Dataset(output, "a")
    starts = nc["sweep_start_ray_index"][:]
    ends = nc["sweep_end_ray_index"][:]
    fixed = nc["fixed_angle"][:]
    reflectivity = nc["reflectivity_horizontal"][:]
    pair_t = nc.createCompoundType(np.dtype([("ray", "i4"), ("angle", "f8")]), "pair_t")
    flag_t = nc.createEnumType(np.uint8, "flag_t", {"none": 0, "echo": 1})
    pairs = np.empty(len(starts), dtype=pair_t.dtype)
    pairs["ray"] = starts
    pairs["angle"] = fixed
    nc.createVariable("pairs", pair_t, ("sweep",))[:] = pairs
    flags = np.array(
        [int((reflectivity[s:e + 1] >= 20.0).any()) for s, e in zip(starts, ends)], dtype=np.uint8
    )
    nc.createVariable("echo_flag", flag_t, ("sweep",))[:] = flags
    rays_t = nc.createVLType(np.int32, "rays_t")
    echo_rays = nc.createVariable("echo_rays", rays_t, ("sweep",))
    echo_lists = []
    for index, (s, e) in enumerate(zip(starts, ends)):
        rows = np.ma.filled(reflectivity[s:e + 1] >= 20.0, False).any(axis=1)
        echo_lists.append(np.flatnonzero(rows).astype(np.int32))
        echo_rays[index] = echo_lists[-1]
    nc.close()

    h5 = h5py.File(output, "a")
    first = np.array((int(ends[0] - starts[0] + 1), float(fixed[0])), dtype=h5["pair_t"].dtype)
    h5.attrs.create("sweep_pair", first, dtype=h5["pair_t"])
    h5.attrs.create("echo_state", np.array(flags[0], dtype=np.uint8), dtype=h5["flag_t"])
    h5["angle_bytes_t"] = np.dtype("V4")
    raw = np.asarray(fixed, dtype="<f4").view("V4")
    angle_bytes = h5.create_dataset("fixed_angle_bytes", data=raw, dtype=h5["angle_bytes_t"])
    angle_bytes.dims[0].attach_scale(h5["sweep"])
    h5.attrs.create("first_fixed_angle_bytes", raw[0], dtype=h5["angle_bytes_t"])
    h5.close()

    h5 = h5py.File(output, "r")
    assert h5["fixed_angle_bytes"][...].tobytes() == np.asarray(fixed, dtype="<f4").tobytes()
    assert h5.attrs["first_fixed_angle_bytes"].tobytes() == raw[0].tobytes()
    h5.close()

    check = netCDF4.Dataset(output)
    assert tuple(check.getncattr("sweep_pair")) == (int(ends[0] - starts[0] + 1), float(fixed[0]))
    assert int(check.getncattr("echo_state")) == int(flags[0])
    assert (check["pairs"][:]["ray"] == starts).all()
    assert (check["echo_flag"][:] == flags).all()
    for got, want in zip(check["echo_rays"][:], echo_lists):
        assert (np.asarray(got) == want).all()
    got_bytes = np.asarray(fixed, dtype="<f4").tobytes()
    print(f"wrote {output}: pairs {check['pairs'][:].tolist()}, echo_flag {flags.tolist()}, "
          f"sweep_pair {tuple(check.getncattr('sweep_pair'))}, echo_state {int(flags[0])}, "
          f"echo_rays lengths {[len(v) for v in echo_lists]} first {echo_lists[0][:5].tolist()}, "
          f"fixed_angle_bytes {got_bytes.hex()}")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
