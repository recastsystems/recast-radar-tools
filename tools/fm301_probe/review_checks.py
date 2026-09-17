"""Checks behind the FM301 design note's review resolutions (docs/design/fm301-model.md, 16).

usage: review_checks.py <KTLX20240315_000217_V06 path> <testdata/files/other dir> <scratch dir>

Prints:
  1. Py-ART config names for PIDA and LDR, and Py-ART's NEXRAD time epoch rule.
  2. xradar NEXRAD root and sweep attribute Python types.
  3. xarray CF decoding of NEXRAD sentinels, and whether packed-unit attributes moved into
     .encoding survive to_netcdf (netcdf4 and h5netcdf engines).
  4. CfRadial 1 file contents (netCDF4) and what xradar keeps (open_cfradial1_datatree).
"""
import glob
import inspect
import os
import sys
import warnings

warnings.simplefilter("ignore")

import netCDF4  # noqa: E402
import numpy as np  # noqa: E402
import xarray as xr  # noqa: E402
import xradar as xd  # noqa: E402
import pyart  # noqa: E402
from pyart.io import nexrad_level2  # noqa: E402

nexrad, other, scratch = sys.argv[1], sys.argv[2], sys.argv[3]
cfdir = os.path.join(other, "cfradial")

print("== 1. Py-ART names and epoch")
cfg = pyart.config
print("PIDA:", repr(cfg.get_field_name("path_integrated_differential_attenuation")))
print("LDR:", repr(cfg.get_field_name("linear_depolarization_ratio")))
src = inspect.getsource(nexrad_level2.NEXRADLevel2File.get_times)
print("get_times floors first radial:", "seconds=int(secs[0])" in src)
radar = pyart.io.read_nexrad_archive(nexrad)
print("time units:", radar.time["units"], "first:", radar.time["data"][:2])

print("== 2. xradar NEXRAD attribute types")
dt = xd.io.open_nexradlevel2_datatree(nexrad, sweep=[0])
print("root:", {k: type(v).__name__ for k, v in dt["/"].attrs.items()})
print("sweep_0:", {k: type(v).__name__ for k, v in dt["sweep_0"].attrs.items()})

print("== 3. CF decoding of NEXRAD sentinels")
raw = np.array([[0, 1, 2, 66, 200, 255]], dtype=np.uint8)
attrs = dict(
    scale_factor=0.5,
    add_offset=-33.0,
    _FillValue=np.uint8(0),
    _Undetect=np.uint8(0),
    valid_range=np.array([2, 255], dtype=np.uint8),
    flag_values=np.array([1], dtype=np.uint8),
    flag_meanings="range_folded",
)
decoded = xr.decode_cf(xr.Dataset({"DBZH": (("time", "range"), raw, attrs)}))
v = decoded.DBZH
print("values:", v.dtype, v.values)
print("attrs:", dict(v.attrs))
print("encoding:", dict(v.encoding))
for key in ["_Undetect", "valid_range", "flag_values", "flag_meanings"]:
    v.encoding[key] = v.attrs.pop(key)
for engine in ["netcdf4", "h5netcdf"]:
    out = os.path.join(scratch, f"packed_attrs_{engine}.nc")
    decoded.to_netcdf(out, engine=engine)
    with xr.open_dataset(out, mask_and_scale=False, engine=engine) as back:
        print(engine, "written attrs:", sorted(back.DBZH.attrs))

print("== 4. CfRadial 1")
for path in sorted(glob.glob(os.path.join(cfdir, "*.nc"))):
    nc = netCDF4.Dataset(path)
    print("--", os.path.basename(path))
    print(" global attrs:", nc.ncattrs())
    print(" n_gates_vary:", repr(getattr(nc, "n_gates_vary", None)), "ray_n_gates:", "ray_n_gates" in nc.variables)
    print(" time units:", nc.variables["time"].units)
    for name in ("ray_start_range", "ray_gate_spacing"):
        if name in nc.variables:
            a = np.asarray(nc.variables[name][:])
            print(f" {name}: min {a.min()} max {a.max()}")
    for name, var in nc.variables.items():
        if var.dimensions[:1] == ("time",) and len(var.dimensions) == 2 and var.dimensions[1] == "range":
            print(f"  field {name} {var.dtype} standard_name={getattr(var, 'standard_name', None)}")
        else:
            print(f"  {name}{var.dimensions} {var.dtype}")
    nc.close()
dow8 = os.path.join(cfdir, "cfrad.20211011_223602_DOW8_RHI.trim3.nc")
tree = xd.io.open_cfradial1_datatree(dow8, optional_groups=True)
print("xradar DOW8 root attrs:", {k: type(v).__name__ for k, v in tree.attrs.items()})
print("xradar DOW8 sweep_0 vars:", {k: (v.dims, str(v.dtype)) for k, v in tree["sweep_0"].to_dataset().data_vars.items()})
print("xradar DOW8 radar_calibration:", list(tree["radar_calibration"].to_dataset().data_vars))
print("xradar DOW8 radar_parameters:", list(tree["radar_parameters"].to_dataset().data_vars))
