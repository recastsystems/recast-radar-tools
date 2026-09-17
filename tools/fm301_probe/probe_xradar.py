"""Dump the real structure xradar produces for a radar file.

usage: probe_xradar.py <nexrad|odim|cfradial1> <path> [max_detail_sweeps]
"""
import sys
import warnings

import numpy as np
import xradar as xd

kind, path = sys.argv[1], sys.argv[2]
detail = int(sys.argv[3]) if len(sys.argv) > 3 else 2
opener = {
    "nexrad": xd.io.open_nexradlevel2_datatree,
    "odim": xd.io.open_odim_datatree,
    "cfradial1": xd.io.open_cfradial1_datatree,
}[kind]

with warnings.catch_warnings(record=True) as caught:
    warnings.simplefilter("always")
    dt = opener(path)
for w in caught:
    print("WARNING:", str(w.message)[:300])

ENC_KEYS = ("dtype", "scale_factor", "add_offset", "_FillValue", "missing_value", "_Undetect", "units", "calendar")


def fmt_attrs(attrs):
    return {k: (v.item() if hasattr(v, "item") and np.ndim(v) == 0 else v) for k, v in attrs.items()}


def show_var(name, v):
    enc = {k: v.encoding[k] for k in ENC_KEYS if k in v.encoding}
    print(f"   {name} dims={v.dims} dtype={v.dtype} attrs={fmt_attrs(v.attrs)} encoding={enc}")
    if v.ndim == 0 and v.size:
        print(f"       value={v.values!r}")
    elif v.ndim == 1 and v.size and v.dtype.kind in "fiubM":
        vals = v.values
        print(f"       n={vals.size} first={vals[:3]} last={vals[-2:]}")


def show_ds(title, ds):
    print(f"\n--- {title} ---")
    print("dims:", dict(ds.sizes))
    print("coords:")
    for c in ds.coords:
        show_var(c, ds.coords[c])
    print("data_vars:")
    for d in ds.data_vars:
        show_var(d, ds[d])
    print("attrs:", {k: str(val)[:160] for k, val in ds.attrs.items()})


print("=== GROUPS ===")
for node in dt.subtree:
    ds = node.to_dataset()
    print(f"{node.path}: dims={dict(ds.sizes)} n_vars={len(ds.data_vars)} n_coords={len(ds.coords)}")

show_ds("/", dt.to_dataset())
sweeps = [c for c in dt.children if c.startswith("sweep_")]
for c in dt.children:
    if not c.startswith("sweep_"):
        show_ds("/" + c, dt[c].to_dataset())

print("\n=== SWEEP SUMMARY ===")
for c in sweeps:
    ds = dt[c].to_dataset()
    rng = ds["range"].values
    fields = [d for d in ds.data_vars if ds[d].ndim == 2]
    fa = float(ds["sweep_fixed_angle"].values) if "sweep_fixed_angle" in ds else None
    mode = str(ds["sweep_mode"].values) if "sweep_mode" in ds else None
    print(
        f"{c}: dims={dict(ds.sizes)} fixed_angle={fa} mode={mode} "
        f"range[0]={rng[0]} dr={rng[1]-rng[0] if rng.size > 1 else None} range[-1]={rng[-1]} fields={fields}"
    )

for c in sweeps[:detail]:
    show_ds("/" + c, dt[c].to_dataset())

print("\n=== PER-FIELD VALID EXTENT (last gate index with any finite value + 1) ===")
for c in sweeps:
    ds = dt[c].to_dataset()
    for d in ds.data_vars:
        v = ds[d]
        if v.ndim != 2:
            continue
        arr = v.values
        if arr.dtype.kind != "f":
            print(f"  {c}/{d}: dtype={arr.dtype} (not float)")
            continue
        finite = np.isfinite(arr)
        cols = np.nonzero(finite.any(axis=0))[0]
        last = int(cols[-1]) + 1 if cols.size else 0
        fin = arr[finite]
        print(
            f"  {c}/{d}: shape={arr.shape} last_finite_gate={last} nan_frac={1 - finite.mean():.4f} "
            f"min={fin.min() if fin.size else None} max={fin.max() if fin.size else None}"
        )
