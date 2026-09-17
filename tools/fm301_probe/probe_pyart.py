"""Dump the real structure Py-ART produces for a radar file.

usage: probe_pyart.py <nexrad|odim|cfradial1> <path>
"""
import sys
import warnings

import numpy as np

kind, path = sys.argv[1], sys.argv[2]
with warnings.catch_warnings(record=True) as caught:
    warnings.simplefilter("always")
    import pyart

    if kind == "nexrad":
        radar = pyart.io.read_nexrad_archive(path)
    elif kind == "odim":
        radar = pyart.aux_io.read_odim_h5(path)
    elif kind == "cfradial1":
        radar = pyart.io.read_cfradial(path)
    else:
        raise SystemExit(kind)
seen = set()
for w in caught:
    msg = str(w.message)[:300]
    if msg not in seen and "Py-ART" not in msg[:20]:
        seen.add(msg)
        print("WARNING:", msg)


def meta(d):
    return {k: (v if not isinstance(v, np.ndarray) else f"ndarray{v.shape}") for k, v in d.items() if k != "data"}


def arr(d):
    a = d["data"]
    if isinstance(a, np.ndarray) and a.size:
        return f"{type(a).__name__} dtype={a.dtype} shape={a.shape} first={np.ravel(a)[:3]} last={np.ravel(a)[-2:]}"
    return repr(a)


print("scan_type:", radar.scan_type)
print("nrays:", radar.nrays, "ngates:", radar.ngates, "nsweeps:", radar.nsweeps)
for name in (
    "time", "range", "latitude", "longitude", "altitude", "sweep_number", "sweep_mode", "fixed_angle",
    "sweep_start_ray_index", "sweep_end_ray_index", "azimuth", "elevation", "target_scan_rate",
    "rays_are_indexed", "ray_angle_res", "scan_rate", "antenna_transition", "altitude_agl",
):
    d = getattr(radar, name, None)
    if d is None:
        print(f"{name}: None")
        continue
    print(f"{name}: meta={meta(d)}")
    print(f"     data: {arr(d)}")
print("metadata:", {k: str(v)[:160] for k, v in radar.metadata.items()})
print("instrument_parameters:")
for k, d in (radar.instrument_parameters or {}).items():
    print(f"   {k}: meta={meta(d)} data: {arr(d)}")
print("radar_calibration:", None if radar.radar_calibration is None else list(radar.radar_calibration))
print("fields:")
for k, d in radar.fields.items():
    a = d["data"]
    masked = np.ma.getmaskarray(a)
    vals = np.ma.compressed(a)
    print(f"   {k}: meta={meta(d)}")
    print(
        f"       data type={type(a).__name__} dtype={a.dtype} shape={a.shape} masked_frac={masked.mean():.4f} "
        f"min={vals.min() if vals.size else None} max={vals.max() if vals.size else None}"
    )

print("\nper-sweep: rays, fixed_angle, unmasked gate extent per field")
for s in range(radar.nsweeps):
    start = int(radar.sweep_start_ray_index["data"][s])
    end = int(radar.sweep_end_ray_index["data"][s])
    parts = []
    for k, d in radar.fields.items():
        a = d["data"][start : end + 1]
        m = ~np.ma.getmaskarray(a)
        cols = np.nonzero(m.any(axis=0))[0]
        parts.append(f"{k}:{int(cols[-1]) + 1 if cols.size else 0}")
    print(
        f"  sweep {s}: rays={end - start + 1} fixed_angle={radar.fixed_angle['data'][s]:.3f} "
        f"mode={radar.sweep_mode['data'][s]} {' '.join(parts)}"
    )
