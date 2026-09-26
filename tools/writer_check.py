"""Check the writers' output with independent readers.

Reads the files `cargo run --release -p recast-radar-io --example write_all`
writes (CfRadial 1, FM301 / CfRadial 2, ODIM_H5 per corpus volume) with
independent readers and compares every gate with what the source volume
holds (the example's `expect/` dump):

- ODIM_H5 (`odim.h5`, and `odim-every.h5` with a plane for every quantity
  in every dataset): h5py (raw planes decoded with what/gain, offset,
  nodata, undetect), wradlib `read_opera_hdf5`, xradar
  `open_odim_datatree`, Py-ART `aux_io.read_odim_h5`, LROSE Radx;
- CfRadial 1 (`cfradial1.nc`, and `cfradial1-perray.nc` with the gate
  geometry per ray): netCDF4-python, xradar `open_cfradial1_datatree`,
  Py-ART `read_cfradial`, LROSE Radx; the same written with `_Unsigned`
  (`cfradial1-unsigned.nc`): netCDF4-python, xradar, Py-ART;
- FM301 (`fm301.nc`): xarray `open_datatree` with the netcdf4 and h5netcdf
  engines, xradar `open_cfradial2_datatree`, netCDF4-python groups, h5py.

LROSE: `RadxConvert -cf_classic -preserve_sweeps -const_ngates` in the
`nexbench` Docker container reads each file with Radx's own ODIM_H5 or
CfRadial reader and writes it as CfRadial 1, which netCDF4-python then
compares. Radx takes an ODIM `where/rstart` as the centre of the first gate,
in km (ODIM_H5 defines the start of the first bin, in metres from v2.4): the
check verifies Radx's range is exactly that reading and compares gates on
the ODIM_H5 geometry. `--no-lrose` skips the container.

A value gate must decode to the source's physical value (float32 tolerance),
a missing gate must be masked or NaN. Undetect and range-folded gates are
reported: CF readers mask an undetect code that is the `_FillValue` (NEXRAD)
and xradar decodes an ODIM `undetect` code to a number; xarray does not apply
`valid_range`, so it decodes the NEXRAD range-folded code to a number.

Sweeps are paired with the source's by their ray times (the CfRadial 1
writer stores sweeps in time order), rays by (time, azimuth), gates by
range. Every plane of an ODIM file is paired with its source field by the
writer's numbering (a field keeps one `dataM` in every dataset, numbered in
order of first appearance; quality fields go to `dataM/qualityK` or the
dataset's `qualityK`), not by position, and every Py-ART field is checked
in every sweep: a dataset without that quantity must read as all missing.
A CfRadial 1 file in `n_gates_vary` storage is laid out per ray from
`ray_start_index` and `ray_n_gates`; each sweep's gates come from its row of
a two-dimensional `range`, or from `ray_start_range` / `ray_gate_spacing`
where they differ from `range(range)`.

A reader that fails in a way the check recognises from the file as a known
limitation of that reader (not of the file) is reported as LIMIT with the
reason. A reader that fails on a written file whose source is a file of the
same format (`source.h5` / `source.nc`, which `write_all` copies) is run on
the source too: when it fails there with the same error (the same
exception and message, the file names aside), the failure is the reader's
behaviour on that data, not a property of the written file, and is reported
as LIMIT ("fails the same way on the source file"). Anything else is FAIL.

Usage: python tools/writer_check.py <write_all out_dir> [--no-lrose] [-v] [id ...]
Exit status 1 when any check fails.
"""

import gc
import json
import os
import pathlib
import shutil
import subprocess
import sys
import traceback
import warnings

import numpy as np

warnings.filterwarnings("ignore")

RTOL = 1e-5
ATOL = 1e-5
CONTAINER = "nexbench"
LROSE_BIN = "/usr/local/lrose/bin"


class ReaderLimitation(Exception):
    """A reader fails in a known way that the file does not cause."""


def load_expect(case_dir):
    meta = json.loads((case_dir / "expect" / "meta.json").read_text())
    for sweep in meta["sweeps"]:
        nrays = len(sweep["azimuth"])
        ngates = len(sweep["range"])
        for field in sweep["fields"]:
            stem = case_dir / "expect" / field["stem"]
            field["values"] = np.fromfile(str(stem) + ".f32", dtype="<f4").reshape(nrays, ngates)
            field["classes"] = np.fromfile(str(stem) + ".u8", dtype="u1").reshape(nrays, ngates)
        sweep["azimuth"] = np.asarray(sweep["azimuth"], dtype="f8")
        sweep["elevation"] = np.asarray(sweep["elevation"], dtype="f8")
        sweep["time"] = np.asarray(sweep["time"], dtype="f8")
        sweep["range"] = np.asarray(sweep["range"], dtype="f8")
    return meta


def ray_keys(time, azimuth):
    return np.round(np.asarray(time, dtype="f8"), 3), np.round(np.mod(azimuth, 360.0), 3)


def pair_rays(exp_time, exp_az, got_time, got_az, time_tolerance=2e-3):
    """Indices of the expected ray for each read ray, by (time, azimuth)."""
    et, ea = ray_keys(exp_time, exp_az)
    gt, ga = ray_keys(got_time, got_az)
    exp_order = np.lexsort((ea, et))
    got_order = np.lexsort((ga, gt))
    if len(exp_order) != len(got_order):
        raise AssertionError(f"{len(got_order)} rays read, {len(exp_order)} expected")
    pairs = np.empty(len(got_order), dtype=int)
    pairs[got_order] = exp_order
    dt = np.abs(np.asarray(got_time, dtype="f8") - np.asarray(exp_time, dtype="f8")[pairs])
    da = np.abs(
        (np.asarray(got_az, dtype="f8") - np.asarray(exp_az, dtype="f8")[pairs] + 180.0) % 360.0
        - 180.0
    )
    if np.nanmax(dt, initial=0.0) > time_tolerance or np.nanmax(da, initial=0.0) > 1e-3:
        raise AssertionError(
            f"rays do not pair: max time difference {np.nanmax(dt):.6f} s, "
            f"azimuth {np.nanmax(da):.6f} deg"
        )
    return pairs


def pair_by_azimuth(exp_az, got_az, tolerance=1e-3):
    """Indices of the expected ray for each read ray, by azimuth alone (a
    reader that derives its own ray times)."""
    exp_az = np.mod(np.asarray(exp_az, dtype="f8"), 360.0)
    got_az = np.mod(np.asarray(got_az, dtype="f8"), 360.0)
    if len(exp_az) != len(got_az):
        raise AssertionError(f"{len(got_az)} rays read, {len(exp_az)} expected")
    diff = np.abs((got_az[:, None] - exp_az[None, :] + 180.0) % 360.0 - 180.0)
    pairs = np.argmin(diff, axis=1)
    worst = float(diff[np.arange(len(pairs)), pairs].max(initial=0.0))
    if len(set(pairs.tolist())) != len(pairs) or worst > tolerance:
        raise AssertionError(f"rays do not pair by azimuth (max difference {worst:.6f} deg)")
    return pairs


def find_sweep(meta, time, tolerance=2e-3):
    """The source sweep whose ray times are `time` (in any order)."""
    got = np.sort(np.asarray(time, dtype="f8"))
    best, best_error = None, np.inf
    for index, sweep in enumerate(meta["sweeps"]):
        expected = np.sort(sweep["time"])
        if len(expected) != len(got):
            continue
        error = float(np.nanmax(np.abs(expected - got), initial=0.0))
        if error < best_error:
            best, best_error = index, error
    if best is None or best_error > tolerance:
        raise AssertionError(
            f"no source sweep has these {len(got)} ray times (closest differs by {best_error} s)"
        )
    return best


def pair_gates(exp_range, got_range):
    """Index of the expected gate holding each read gate (-1 outside)."""
    exp_range = np.asarray(exp_range, dtype="f8")
    got_range = np.asarray(got_range, dtype="f8")
    if len(exp_range) == 1:
        spacing = 1.0
    else:
        spacing = (exp_range[-1] - exp_range[0]) / (len(exp_range) - 1)
    index = np.round((got_range - exp_range[0]) / spacing).astype(int)
    inside = (index >= 0) & (index < len(exp_range))
    index = np.where(inside, index, 0)
    ok = inside & (np.abs(exp_range[index] - got_range) <= spacing / 2 + 0.6)
    return np.where(ok, index, -1)


class Tally:
    def __init__(self):
        self.values = 0
        self.missing = 0
        self.undetect_masked = 0
        self.undetect_number = 0
        self.folded_masked = 0
        self.folded_number = 0
        self.notes = []
        # Gates counted under a note, summed over sweeps and fields.
        self.counts = {}

    def text(self):
        text = (
            f"values {self.values}, missing {self.missing}, undetect masked "
            f"{self.undetect_masked}/number {self.undetect_number}, folded masked "
            f"{self.folded_masked}/number {self.folded_number}"
        )
        for note in dict.fromkeys(self.notes):
            text += f"; {note}"
        for note, count in self.counts.items():
            text += f"; {count} {note}"
        return text


def compare_field(label, field, got, rays, gates, tally, missing_as=(), atol=0.0, maskable=None):
    """`got`: float array (rays read x gates read), NaN where masked.
    `missing_as`: values a reader that ignores a sentinel decodes missing
    gates to (counted in the notes, not failures). `atol`: an absolute
    tolerance beyond float32 precision. `maskable`: (boolean array like
    `got`, note): value gates the reader masks for a reason the file shows
    (counted in the notes, not failures)."""
    got = np.asarray(got, dtype="f8")
    exp_values = field["values"][rays]
    exp_classes = field["classes"][rays]
    inside = gates >= 0
    gate_index = np.where(inside, gates, 0)
    expected = exp_values[:, gate_index].astype("f8")
    classes = np.where(inside[None, :], exp_classes[:, gate_index], 1)
    masked = ~np.isfinite(got)
    value = classes == 0
    bad = value & (masked | ~np.isclose(got, expected, rtol=RTOL, atol=max(ATOL, atol)))
    if maskable is not None and maskable[0].shape == got.shape:
        excused = bad & masked & maskable[0]
        if excused.any():
            tally.counts[maskable[1]] = tally.counts.get(maskable[1], 0) + int(excused.sum())
        bad &= ~excused
    if atol > ATOL and (value & ~np.isclose(got, expected, rtol=RTOL, atol=ATOL)).any():
        tally.notes.append(f"values within the reader's re-packing precision ({atol:.2g})")
    if bad.any():
        r, g = np.argwhere(bad)[0]
        raise AssertionError(
            f"{label} {field['name']}: {bad.sum()} value gates differ; first at ray {r} gate "
            f"{g}: read {got[r, g]!r}, expected {expected[r, g]!r}"
        )
    missing = classes == 1
    decoded = np.zeros(got.shape, bool)
    for sentinel in missing_as:
        decoded |= np.isclose(got, sentinel, rtol=RTOL, atol=max(ATOL, atol))
    if (missing & ~masked & decoded).any():
        tally.notes.append(
            f"{int((missing & ~masked & decoded).sum())} missing gates read as the decoded "
            "nodata/undetect code"
        )
    bad = missing & ~masked & ~decoded
    if bad.any():
        r, g = np.argwhere(bad)[0]
        raise AssertionError(
            f"{label} {field['name']}: {bad.sum()} missing gates read as numbers; first at "
            f"ray {r} gate {g}: {got[r, g]!r}"
        )
    tally.values += int(value.sum())
    tally.missing += int(missing.sum())
    tally.undetect_masked += int(((classes == 2) & masked).sum())
    tally.undetect_number += int(((classes == 2) & ~masked).sum())
    tally.folded_masked += int(((classes == 3) & masked).sum())
    tally.folded_number += int(((classes == 3) & ~masked).sum())


def epoch(values, units):
    """Seconds since 1970 of CF time values."""
    import calendar

    import cftime

    dates = cftime.num2date(np.asarray(values, dtype="f8"), units, only_use_cftime_datetimes=False)
    return np.array(
        [calendar.timegm(d.timetuple()) + d.microsecond / 1e6 for d in np.atleast_1d(dates)]
    )


def datetimes_to_epoch(values):
    values = np.asarray(values)
    if np.issubdtype(values.dtype, np.datetime64):
        return values.astype("datetime64[ns]").astype("int64") / 1e9
    return values.astype("f8")


def to_float(array):
    array = np.ma.masked_invalid(np.ma.asarray(array, dtype="f8"))
    return array.filled(np.nan)


def text(value):
    return value.decode() if isinstance(value, bytes) else str(value)


def numbered(group, prefix):
    return sorted(
        (k for k in group if k.startswith(prefix) and k[len(prefix):].isdigit()),
        key=lambda k: int(k[len(prefix):]),
    )


def odim_placed(sweep):
    """The quality fields of a sweep the ODIM writer puts in quality groups
    (`<plane>_qualityK` qualifying one data plane, `qualityK` qualifying all
    of them), as write.rs `PlaneClasses` decides."""
    import re

    data = [f["name"] for f in sweep["fields"] if not f.get("quality")]
    placed = {}
    for field in sweep["fields"]:
        if not field.get("quality"):
            continue
        qualified = field.get("qualified", [])
        name = field["name"]
        one = re.fullmatch(r"(.*)_quality(\d+)", name)
        every = re.fullmatch(r"quality(\d+)", name)
        if len(qualified) == 1 and qualified[0] in data and one and one.group(1) == qualified[0]:
            placed[name] = (qualified[0], int(one.group(2)))
        elif qualified == data and every:
            placed[name] = (None, int(every.group(1)))
    return placed


def odim_paths(meta, index):
    """Per source field of sweep `index`: its plane in the ODIM file
    (`dataM`, `dataM/qualityK` or `qualityK`). Data planes are numbered once
    for the volume, in order of first appearance."""
    order = []
    for sweep in meta["sweeps"]:
        placed = odim_placed(sweep)
        for field in sweep["fields"]:
            if field["name"] not in placed and field["name"] not in order:
                order.append(field["name"])
    sweep = meta["sweeps"][index]
    placed = odim_placed(sweep)
    out = []
    for field in sweep["fields"]:
        name = field["name"]
        if name not in placed:
            out.append(f"data{order.index(name) + 1}")
        else:
            plane, k = placed[name]
            out.append(f"quality{k}" if plane is None else f"data{order.index(plane) + 1}/quality{k}")
    return out


def odim_quantities(path, meta, index):
    """`what/quantity` of each source field's plane in dataset `index`
    (`None` for a quality group)."""
    import h5py

    with h5py.File(path, "r") as h5:
        group = h5[f"dataset{index + 1}"]
        return [
            None if "quality" in plane else text(group[plane]["what"].attrs["quantity"])
            for plane in odim_paths(meta, index)
        ]


def overlapping_sweeps(meta):
    """Pairs of source sweeps whose ray time ranges overlap."""
    spans = [(np.nanmin(s["time"]), np.nanmax(s["time"])) for s in meta["sweeps"] if len(s["time"])]
    return [
        (i, j)
        for i in range(len(spans))
        for j in range(i + 1, len(spans))
        if spans[i][0] < spans[j][1] and spans[j][0] < spans[i][1]
    ]


# --------------------------------------------------------------------------
# ODIM_H5
# --------------------------------------------------------------------------


def rstart_scale(conventions):
    """Metres per `where/rstart` unit: ODIM_H5 v2.4 states metres, earlier
    versions km."""
    return 1.0 if text(conventions).strip() == "ODIM_H5/V2_4" else 1000.0


def odim_rays(how, nrays):
    """(time, azimuth) of an ODIM dataset's rays from its `how` arrays, or
    `None` when it has no `startazA`/`startazT` (the writer derives none for
    a volume read from ODIM_H5 that had none: its rows are the source's)."""
    if not all(name in how for name in ("startazA", "stopazA", "startazT", "stopazT")):
        return None
    start, stop = np.asarray(how["startazA"]), np.asarray(how["stopazA"])
    if len(start) != nrays:
        return None
    stop = np.where(stop < start, stop + 360.0, stop)
    azimuth = np.mod((start + stop) / 2.0, 360.0)
    time = (np.asarray(how["startazT"]) + np.asarray(how["stopazT"])) / 2.0
    return time, azimuth


def pair_odim_rows(sweep, how, nrays, tally):
    rays = odim_rays(how, nrays)
    if rays is None:
        if nrays != len(sweep["time"]):
            raise AssertionError(f"{nrays} rays, {len(sweep['time'])} expected")
        tally.notes.append("rows paired by position (no startazA/startazT, as in the source)")
        return np.arange(nrays)
    return pair_rays(sweep["time"], sweep["azimuth"], rays[0], rays[1])


def odim_sentinels(path, meta, index):
    """Per source field of dataset `index`: the physical values of its
    plane's `nodata` and `undetect` codes (what a reader ignoring them reads)."""
    import h5py

    out = []
    with h5py.File(path, "r") as h5:
        group = h5[f"dataset{index + 1}"]
        planes = [group[plane] for plane in odim_paths(meta, index)]
        for plane in planes:
            what = plane["what"].attrs
            gain, offset = float(what.get("gain", 1.0)), float(what.get("offset", 0.0))
            out.append(
                tuple(float(what[k]) * gain + offset for k in ("nodata", "undetect") if k in what)
            )
    return out


def odim_geometry(path):
    """Per dataset: gate centres (ODIM_H5 geometry) and the raw `rstart`."""
    import h5py

    out = []
    with h5py.File(path, "r") as h5:
        scale = rstart_scale(h5.attrs.get("Conventions", ""))
        for name in numbered(h5, "dataset"):
            where = h5[name]["where"].attrs
            nbins, rscale, rstart = int(where["nbins"]), float(where["rscale"]), float(where["rstart"])
            out.append((rstart * scale + rscale / 2 + np.arange(nbins) * rscale, rstart, rscale))
    return out


def check_odim_h5py(path, meta, tally):
    import h5py

    with h5py.File(path, "r") as h5:
        datasets = numbered(h5, "dataset")
        assert len(datasets) == len(meta["sweeps"]), "dataset count"
        geometry = odim_geometry(path)
        for index, (name, sweep) in enumerate(zip(datasets, meta["sweeps"])):
            group = h5[name]
            how = group["how"].attrs if "how" in group else {}
            nrays = int(group["where"].attrs["nrays"])
            rays = pair_odim_rows(sweep, how, nrays, tally)
            gates = pair_gates(sweep["range"], geometry[index][0])
            paths = odim_paths(meta, index)
            planes = []
            for plane in numbered(group, "data"):
                planes.append(plane)
                planes.extend(f"{plane}/{q}" for q in numbered(group[plane], "quality"))
            planes.extend(numbered(group, "quality"))
            # A plane of no source field (`every_quantity`) holds only nodata.
            for extra in sorted(set(planes) - set(paths)):
                what = group[extra]["what"].attrs
                raw = group[extra]["data"][...]
                if "nodata" not in what or not (raw == what["nodata"]).all():
                    raise AssertionError(f"{name}/{extra}: a plane of no source field holds data")
                tally.notes.append("planes of quantities a sweep lacks hold only nodata")
            if set(paths) - set(planes):
                raise AssertionError(f"{name}: no plane {sorted(set(paths) - set(planes))}")
            for path_in_group, field in zip(paths, sweep["fields"]):
                plane = group[path_in_group]
                what = plane["what"].attrs
                raw = plane["data"][...]
                values = raw.astype("f8") * float(what.get("gain", 1.0)) + float(
                    what.get("offset", 0.0)
                )
                mask = np.zeros(raw.shape, bool)
                for key in ("nodata", "undetect"):
                    if key in what:
                        code = what[key]
                        mask |= (raw == code) if np.isfinite(code) else np.isnan(raw)
                mask |= ~np.isfinite(values)
                values[mask] = np.nan
                compare_field(f"h5py {name}", field, values, rays, gates, tally)


def check_odim_wradlib(path, meta, tally):
    import wradlib

    content = wradlib.io.read_opera_hdf5(str(path))
    geometry = odim_geometry(path)
    for index, sweep in enumerate(meta["sweeps"]):
        name = f"dataset{index + 1}"
        how = content.get(f"{name}/how", {})
        nrays = int(content[f"{name}/where"]["nrays"])
        rays = pair_odim_rows(sweep, how, nrays, tally)
        gates = pair_gates(sweep["range"], geometry[index][0])
        for plane, field in zip(odim_paths(meta, index), sweep["fields"]):
            if f"{name}/{plane}/data" not in content:
                raise AssertionError(f"wradlib read no {name}/{plane}")
            what = content[f"{name}/{plane}/what"]
            raw = content[f"{name}/{plane}/data"]
            values = raw.astype("f8") * float(what["gain"]) + float(what["offset"])
            mask = np.zeros(raw.shape, bool)
            for key in ("nodata", "undetect"):
                if key in what:
                    code = what[key]
                    mask |= (raw == code) if np.isfinite(code) else np.isnan(raw)
            mask |= ~np.isfinite(values)
            values[mask] = np.nan
            compare_field(f"wradlib {name}/{plane}", field, values, rays, gates, tally)


def odim_has_quality_legend(path):
    import h5py

    found = []
    with h5py.File(path, "r") as h5:
        h5.visit(lambda name: found.append(name) if name.endswith("legend") and "quality" in name else None)
    return bool(found)


def check_odim_xradar(path, meta, tally):
    import xradar

    try:
        tree = xradar.io.open_odim_datatree(str(path))
    except ValueError as err:
        if "legend" in str(err) and odim_has_quality_legend(path):
            raise ReaderLimitation(
                "xradar reads a quality group's `legend` dataset as a data variable on the ray "
                f"dimension ({err}); it fails the same way on the source file"
            ) from err
        raise
    for index, sweep in enumerate(meta["sweeps"]):
        ds = tree[f"sweep_{index}"].to_dataset()
        time = datetimes_to_epoch(ds["time"].values)
        rays = pair_rays(sweep["time"], sweep["azimuth"], time, ds["azimuth"].values)
        gates = pair_gates(sweep["range"], ds["range"].values)
        for field, quantity in zip(sweep["fields"], odim_quantities(path, meta, index)):
            # xradar reads no quality groups.
            if quantity is None:
                continue
            compare_field(
                f"xradar sweep_{index} {quantity}",
                field,
                to_float(ds[quantity].values),
                rays,
                gates,
                tally,
            )


def check_odim_pyart(path, meta, tally):
    import h5py
    import pyart

    geometry = odim_geometry(path)
    with h5py.File(path, "r") as h5:
        v24 = rstart_scale(h5.attrs.get("Conventions", "")) == 1.0
    try:
        radar = pyart.aux_io.read_odim_h5(str(path), file_field_names=True)
    except ValueError as err:
        if "range scale changes" in str(err) and len({g[2] for g in geometry}) > 1:
            raise ReaderLimitation(
                "Py-ART's ODIM reader needs one `rscale` for every dataset "
                f"(this volume has {sorted({g[2] for g in geometry})} m; ODIM_H5 allows one per dataset)"
            ) from err
        if "range start changes" in str(err) and len({g[1] for g in geometry}) > 1:
            raise ReaderLimitation(
                "Py-ART's ODIM reader needs one `rstart` for every dataset "
                f"(this volume has {sorted({g[1] for g in geometry})}; ODIM_H5 allows one per dataset)"
            ) from err
        raise
    # Py-ART takes the plane names of dataset1 and reads the same name in
    # every dataset: a quantity no plane of dataset1 holds is not read.
    with h5py.File(path, "r") as h5:
        everywhere = {
            text(h5[d][p]["what"].attrs["quantity"])
            for d in numbered(h5, "dataset")
            for p in numbered(h5[d], "data")
        }
    unread = sorted(everywhere - set(radar.fields))
    if unread:
        tally.notes.append(
            f"Py-ART reads only the planes dataset1 has; {', '.join(unread)} not read"
        )
    for index, sweep in enumerate(meta["sweeps"]):
        s0 = radar.sweep_start_ray_index["data"][index]
        s1 = radar.sweep_end_ray_index["data"][index] + 1
        time = epoch(radar.time["data"][s0:s1], radar.time["units"])
        with h5py.File(path, "r") as h5:
            group = h5[f"dataset{index + 1}"]
            has_times = odim_rays(group["how"].attrs if "how" in group else {}, s1 - s0)
        if has_times is None:
            # Py-ART keeps the file's rows and derives its own ray times.
            rays = pair_odim_rows(sweep, {}, s1 - s0, tally)
        else:
            # Py-ART's ODIM reader writes its time units to the whole second
            # of the first ray but subtracts the fractional epoch: ray times
            # read up to a second early.
            rays = pair_rays(
                sweep["time"], sweep["azimuth"], time, radar.azimuth["data"][s0:s1], 1.0
            )
        got_range = np.asarray(radar.range["data"], dtype="f8")
        if v24:
            # Py-ART reads `rstart` as km whatever the version; a v2.4 file
            # states metres.
            rstart = geometry[0][1]
            got_range = got_range - rstart * 1000.0 + rstart
            tally.notes.append(f"Py-ART reads the ODIM_H5 v2.4 rstart {rstart} m as km")
        gates = pair_gates(sweep["range"], got_range)
        quantities = odim_quantities(path, meta, index)
        for field, quantity in zip(sweep["fields"], quantities):
            # Py-ART reads no quality groups; a quantity it has seen in an
            # earlier plane of the dataset keeps the first.
            if quantity is None or quantity not in radar.fields:
                continue
            compare_field(
                f"pyart sweep {index} {quantity}",
                field,
                to_float(radar.fields[quantity]["data"][s0:s1]),
                rays,
                gates,
                tally,
            )
        # A Py-ART field of a quantity this sweep lacks must be all missing
        # (not another plane's values under its name).
        for quantity in radar.fields:
            if quantity in quantities:
                continue
            got = to_float(radar.fields[quantity]["data"][s0:s1])
            if np.isfinite(got).any():
                raise AssertionError(
                    f"pyart sweep {index}: dataset{index + 1} has no {quantity} plane, but Py-ART "
                    f"reads {int(np.isfinite(got).sum())} {quantity} values there (another plane's)"
                )


# --------------------------------------------------------------------------
# CfRadial 1
# --------------------------------------------------------------------------


def repack_tolerance(var):
    """How far a reader that re-packs values into this integer variable can
    move them: its float32 `add_offset` and `scale_factor` round to their
    own precision (Radx writes both as float32)."""
    if var.dtype.kind not in "iu":
        return 0.0
    attrs = {k: var.getncattr(k) for k in var.ncattrs()}
    offset = abs(float(np.asarray(attrs.get("add_offset", 0.0)).ravel()[0]))
    scale = abs(float(np.asarray(attrs.get("scale_factor", 1.0)).ravel()[0]))
    largest = float(np.iinfo(var.dtype).max)
    return 2.0 * (float(np.spacing(np.float32(offset))) + largest * float(np.spacing(np.float32(scale))))


def cf1_sweep_gates(nc, s0, s1):
    """Gates of a sweep's rows: its longest `ray_n_gates` in `n_gates_vary`
    storage, else the `range` dimension."""
    if "ray_n_gates" in nc.variables and "n_points" in nc.dimensions:
        return int(np.max(nc["ray_n_gates"][s0:s1]))
    return len(nc.dimensions["range"])


def cf1_sweep_range(nc, index, s0, s1):
    """Gate centres of sweep `index` (rays `s0:s1`) as CfRadial 1.4 states
    them: its row of `range(sweep, range)` or `range(time, range)`, else
    `range(range)`, or `ray_start_range` / `ray_gate_spacing` where those
    differ from it."""
    ngates = cf1_sweep_gates(nc, s0, s1)
    var = nc["range"]
    if var.dimensions == ("sweep", "range"):
        return np.asarray(var[index], dtype="f8")[:ngates]
    if var.dimensions == ("time", "range"):
        return np.asarray(var[s0], dtype="f8")[:ngates]
    centres = np.asarray(var[:], dtype="f8")[:ngates]
    per_ray = ray_geometry(nc, s0)
    if per_ray is not None and len(centres) > 1:
        start, spacing = per_ray
        if abs(start - centres[0]) > 1e-3 or abs(spacing - (centres[1] - centres[0])) > 1e-3:
            return start + spacing * np.arange(ngates)
    return centres


def ray_geometry(nc, ray):
    """(`ray_start_range`, `ray_gate_spacing`) of a ray, when stated."""
    if "ray_start_range" not in nc.variables or "ray_gate_spacing" not in nc.variables:
        return None
    start = nc["ray_start_range"][ray]
    spacing = nc["ray_gate_spacing"][ray]
    if np.ma.is_masked(start) or np.ma.is_masked(spacing) or spacing <= 0:
        return None
    return float(start), float(spacing)


def cf1_sweeps_off_range(path):
    """Sweeps whose gate geometry is not the file's `range(range)` (stated
    per ray): readers that take every ray's gates from `range` misplace them."""
    import netCDF4

    with netCDF4.Dataset(path) as nc:
        if nc["range"].ndim != 1:
            return []
        centres = np.asarray(nc["range"][:], dtype="f8")
        out = []
        for index, (s0, s1) in enumerate(zip(nc["sweep_start_ray_index"][:], nc["sweep_end_ray_index"][:])):
            own = cf1_sweep_range(nc, index, int(s0), int(s1) + 1)
            if not np.allclose(own, centres[: len(own)], atol=1e-3):
                out.append(index)
        return out


class Cf1Rows:
    """A field's rows `s0:s1`, laid out from `n_points` storage when the file
    uses it (each ray padded after its `ray_n_gates` with NaN)."""

    def __init__(self, nc):
        self.nc = nc
        self.cache = {}

    def rows(self, name, s0, s1, ngates):
        var = self.nc[name]
        if var.dimensions != ("n_points",):
            return to_float(var[s0:s1])[:, :ngates]
        if name not in self.cache:
            self.cache[name] = to_float(var[:])
        data = self.cache[name]
        counts = np.asarray(self.nc["ray_n_gates"][s0:s1], dtype=int)
        starts = np.asarray(self.nc["ray_start_index"][s0:s1], dtype=int)
        out = np.full((s1 - s0, ngates), np.nan)
        for row, (start, count) in enumerate(zip(starts, counts)):
            out[row, :count] = data[start : start + count]
        return out


def compare_cfradial1_file(
    label,
    path,
    meta,
    tally,
    names=None,
    ranges=None,
    by_azimuth=False,
    missing_as=None,
    repacked=False,
    maskable=None,
):
    """A CfRadial 1 file read with netCDF4-python. `names(sweep_index,
    field)` gives the variable holding a source field (default: its name;
    `None` skips it); `ranges[sweep_index]` overrides the gate centres;
    `by_azimuth` pairs sweeps by position and rays by azimuth (a reader that
    derives its own ray times); `missing_as(sweep_index, field)` the values
    missing gates may decode to; `repacked`: the file was written by a
    reader that re-packed the values (see `repack_tolerance`);
    `maskable(name, s0, s1, ngates)`: the `compare_field` maskable pair for
    a variable's rows, or `None`."""
    import netCDF4

    with netCDF4.Dataset(path) as nc:
        starts = nc["sweep_start_ray_index"][:]
        ends = nc["sweep_end_ray_index"][:]
        time_all = epoch(nc["time"][:], nc["time"].units)
        rows = Cf1Rows(nc)
        seen = set()
        for index in range(len(starts)):
            s0, s1 = int(starts[index]), int(ends[index]) + 1
            source = index if by_azimuth else find_sweep(meta, time_all[s0:s1])
            seen.add(source)
            sweep = meta["sweeps"][source]
            if by_azimuth:
                rays = pair_by_azimuth(sweep["azimuth"], nc["azimuth"][s0:s1])
            else:
                rays = pair_rays(
                    sweep["time"], sweep["azimuth"], time_all[s0:s1], nc["azimuth"][s0:s1]
                )
            if ranges is not None:
                got_range = ranges[source][: cf1_sweep_gates(nc, s0, s1)]
            else:
                got_range = cf1_sweep_range(nc, index, s0, s1)
            gates = pair_gates(sweep["range"], got_range)
            for field in sweep["fields"]:
                name = field["name"] if names is None else names(source, field)
                if name is None:
                    continue
                values = rows.rows(name, s0, s1, len(got_range))
                sentinels = () if missing_as is None else missing_as(source, field)
                compare_field(
                    f"{label} sweep {index}",
                    field,
                    values,
                    rays,
                    gates,
                    tally,
                    sentinels,
                    repack_tolerance(nc[name]) if repacked else 0.0,
                    None if maskable is None else maskable(name, s0, s1, len(got_range)),
                )
        if len(seen) != len(meta["sweeps"]):
            raise AssertionError(f"{len(seen)} of {len(meta['sweeps'])} source sweeps read")


def check_cfradial1_netcdf4(path, meta, tally):
    compare_cfradial1_file("netCDF4", path, meta, tally)


def check_cfradial1_xradar(path, meta, tally):
    import xradar

    overlaps = overlapping_sweeps(meta)
    off_range = cf1_sweeps_off_range(path)
    try:
        tree = xradar.io.open_cfradial1_datatree(str(path))
        for index in range(len(meta["sweeps"])):
            if index in off_range:
                continue
            ds = tree[f"sweep_{index}"].to_dataset()
            time = datetimes_to_epoch(ds["time"].values)
            sweep = meta["sweeps"][find_sweep(meta, time)]
            rays = pair_rays(sweep["time"], sweep["azimuth"], time, ds["azimuth"].values)
            gates = pair_gates(sweep["range"], ds["range"].values)
            for field in sweep["fields"]:
                compare_field(
                    f"xradar sweep_{index}",
                    field,
                    to_float(ds[field["name"]].values),
                    rays,
                    gates,
                    tally,
                )
    except (AssertionError, KeyError, ValueError) as err:
        if overlaps:
            raise ReaderLimitation(
                "xradar's CfRadial 1 reader sorts every ray of the file by time before cutting "
                f"the sweeps out by their ray indices; source sweeps {overlaps} overlap in time "
                f"({err})"
            ) from err
        raise
    if off_range:
        raise ReaderLimitation(
            "xradar takes every ray's gates from range(range) and ignores ray_start_range / "
            f"ray_gate_spacing; sweeps {off_range} state their own geometry there (the other "
            "sweeps read correctly)"
        )


def check_cfradial1_pyart(path, meta, tally):
    import netCDF4
    import pyart

    with netCDF4.Dataset(path) as nc:
        range_dims = nc["range"].dimensions
    try:
        radar = pyart.io.read_cfradial(str(path))
    except ValueError as err:
        if len(range_dims) == 2:
            raise ReaderLimitation(
                f"Py-ART reads only a one-dimensional range; this file's range{range_dims} holds "
                f"each sweep's own gate geometry (CfRadial 1.4 section 4.4) ({err})"
            ) from err
        raise
    off_range = cf1_sweeps_off_range(path)
    for index in range(radar.nsweeps):
        if index in off_range:
            continue
        s0 = radar.sweep_start_ray_index["data"][index]
        s1 = radar.sweep_end_ray_index["data"][index] + 1
        time = epoch(radar.time["data"][s0:s1], radar.time["units"])
        sweep = meta["sweeps"][find_sweep(meta, time)]
        rays = pair_rays(sweep["time"], sweep["azimuth"], time, radar.azimuth["data"][s0:s1])
        gates = pair_gates(sweep["range"], radar.range["data"])
        for field in sweep["fields"]:
            compare_field(
                f"pyart sweep {index}",
                field,
                to_float(radar.fields[field["name"]]["data"][s0:s1]),
                rays,
                gates,
                tally,
            )
    if off_range:
        raise ReaderLimitation(
            "Py-ART takes every ray's gates from range(range) and ignores ray_start_range / "
            f"ray_gate_spacing; sweeps {off_range} state their own geometry there (the other "
            "sweeps read correctly)"
        )


# --------------------------------------------------------------------------
# LROSE Radx (RadxConvert in the nexbench container)
# --------------------------------------------------------------------------


def docker(*args, check=True, cwd=None):
    env = dict(os.environ, MSYS_NO_PATHCONV="1")
    return subprocess.run(
        ["docker", *args], check=check, cwd=cwd, env=env, capture_output=True, text=True
    )


def lrose_dir(case_dir, filename):
    """Where the Radx conversion of `filename` lands (short: Windows paths)."""
    return case_dir / "lrose" / filename.replace(".", "_")


def run_lrose(out, cases):
    """RadxConvert every `cfradial1.nc` and `odim.h5` of `cases`, and a
    source of those formats, to CfRadial 1 in the container; each result
    lands in `lrose_dir` with Radx's log."""
    staging = out / "_lrose_in"
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir()
    jobs = []
    for case in cases:
        source = source_file(out / case)
        extra = (source.name,) if source and file_kind(source) is not None else ()
        for filename in ("cfradial1.nc", "cfradial1-perray.nc", "odim.h5", "odim-every.h5", *extra):
            if (out / case / filename).exists():
                (staging / case).mkdir(exist_ok=True)
                shutil.copy(out / case / filename, staging / case / filename)
                jobs.append((case, filename))
    root = "/tmp/writer_check"
    # Radx writes <outdir>/<date>/cfrad.<times>_<name>.nc: move it up to a
    # short name.
    lines = []
    for case, filename in jobs:
        job = f"{root}/out/{case}/{filename.replace('.', '_')}"
        lines.append(
            f"mkdir -p {job}/radx && {LROSE_BIN}/RadxConvert -f {root}/in/{case}/{filename} "
            f"-cf_classic -preserve_sweeps -const_ngates -outdir {job}/radx > {job}/log.txt 2>&1; "
            f"find {job}/radx -name '*.nc' -exec mv {{}} {job}/radx.nc ';' ; rm -rf {job}/radx"
        )
    # One script file: the command line would exceed Windows' limit.
    (staging / "run.sh").write_text("\n".join(lines) + "\n", newline="\n")
    docker("exec", CONTAINER, "bash", "-c", f"rm -rf {root} && mkdir -p {root}/in {root}/out")
    docker("cp", "_lrose_in/.", f"{CONTAINER}:{root}/in", cwd=out)
    docker("exec", CONTAINER, "bash", f"{root}/in/run.sh", check=False)
    for case in cases:
        shutil.rmtree(out / case / "lrose", ignore_errors=True)
    staging_out = out / "_lrose_out"
    shutil.rmtree(staging_out, ignore_errors=True)
    docker("cp", f"{CONTAINER}:{root}/out", "_lrose_out", cwd=out)
    for case, filename in jobs:
        shutil.copytree(
            staging_out / case / filename.replace(".", "_"), lrose_dir(out / case, filename)
        )
    shutil.rmtree(staging, ignore_errors=True)
    shutil.rmtree(staging_out, ignore_errors=True)


def lrose_output(path):
    folder = lrose_dir(path.parent, path.name)
    files = sorted(folder.glob("*.nc")) if folder.exists() else []
    if len(files) != 1:
        log = (folder / "log.txt").read_text(errors="replace") if (folder / "log.txt").exists() else ""
        lines = [line for line in log.splitlines() if "ERROR" in line or "rror" in line]
        raise AssertionError(f"RadxConvert wrote {len(files)} files: {' | '.join(lines[:6])}")
    return files[0]


def radx_masks_int16_code(path):
    """Radx reads the int16 raw code -32767 of a CfRadial 1 file as missing
    (S-Pol's VR has 9,255 such gates, -26.8 m/s; Radx masks them in the
    source file too): the `maskable` rows of `path` (the file Radx read,
    rows in its order) where an int16 variable stores that code."""
    import netCDF4

    def maskable(name, s0, s1, ngates):
        with netCDF4.Dataset(path) as nc:
            if name not in nc.variables or nc[name].dtype != np.int16:
                return None
            var = nc[name]
            var.set_auto_maskandscale(False)
            if var.dimensions == ("n_points",):
                data = np.asarray(var[:])
                counts = np.asarray(nc["ray_n_gates"][s0:s1], dtype=int)
                starts = np.asarray(nc["ray_start_index"][s0:s1], dtype=int)
                raw = np.zeros((s1 - s0, ngates), dtype=np.int16)
                for row, (start, count) in enumerate(zip(starts, counts)):
                    raw[row, : min(count, ngates)] = data[start : start + min(count, ngates)]
            else:
                raw = np.asarray(var[s0:s1])[:, :ngates]
        return raw == -32767, "value gates stored as int16 -32767 read as missing (Radx masks that code, in the source file too)"

    return maskable


def check_cfradial1_lrose(path, meta, tally):
    import netCDF4

    with netCDF4.Dataset(path) as nc:
        range_dims = nc["range"].dimensions
    if range_dims == ("sweep", "range"):
        log = lrose_dir(path.parent, path.name) / "log.txt"
        if log.exists() and "Range has incorrect dimensions" in log.read_text(errors="replace"):
            raise ReaderLimitation(
                "Radx refuses the CfRadial 1.4 range(sweep, range) of a volume whose gate "
                "geometry varies by sweep ('Range has incorrect dimensions'); it reads the "
                "geometry per ray instead (RangeLayout::PerRay, cfradial1-perray.nc)"
            )
    converted = lrose_output(path)
    # Radx takes the range from `meters_to_center_of_first_gate` and
    # `meters_between_gates`. A file that keeps a source's attributes that
    # disagree with its `range` variable reads with Radx's range off: compare
    # gates by position then.
    ranges = None
    with netCDF4.Dataset(path) as nc:
        var = nc["range"]
        centres = np.asarray(var[:], dtype="f8")
        attrs = {k: var.getncattr(k) for k in var.ncattrs()}
        per_ray = "ray_gate_spacing" in nc.variables
    if centres.ndim == 1 and not per_ray and len(centres) > 1 and "meters_between_gates" in attrs:
        stated = float(np.asarray(attrs["meters_between_gates"]).ravel()[0])
        actual = (centres[-1] - centres[0]) / (len(centres) - 1)
        if not np.isclose(stated, actual, rtol=1e-4):
            ranges = {index: centres for index in range(len(meta["sweeps"]))}
            tally.notes.append(
                f"the file keeps its source's meters_between_gates {stated} m, which disagrees "
                f"with its range variable ({actual} m steps); Radx takes the attribute, so "
                "gates are paired by position"
            )
    compare_cfradial1_file(
        "Radx",
        converted,
        meta,
        tally,
        ranges=ranges,
        repacked=True,
        maskable=radx_masks_int16_code(path),
    )


def check_odim_lrose(path, meta, tally):
    import netCDF4

    import h5py

    geometry = odim_geometry(path)
    with h5py.File(path, "r") as h5:
        no_how = [d for d in numbered(h5, "dataset") if "how" not in h5[d]]
        gaps = [
            d for d in numbered(h5, "dataset")
            if [int(p[4:]) for p in numbered(h5[d], "data")] != list(range(1, len(numbered(h5[d], "data")) + 1))
        ]
    log = lrose_dir(path.parent, path.name) / "log.txt"
    if gaps and log.exists() and "Cannot open data grop" in log.read_text(errors="replace"):
        raise ReaderLimitation(
            f"Radx opens data1..dataN in every dataset; {', '.join(gaps[:3])} skip numbers (a field "
            "keeps one dataM in every dataset, as in FMI's volumes, which Radx cannot read either)"
        )
    converted = lrose_output(path)
    if no_how:
        raise ReaderLimitation(
            f"Radx gives every ray azimuth 0 and the 1970 epoch as time in a dataset without a "
            f"`how` group ({', '.join(no_how[:3])}...), as it does with the source file"
        )
    # Radx's range is `rstart` read as the first gate centre in km: check
    # that for every dataset, then compare on the ODIM_H5 geometry. A volume
    # whose datasets differ gets a per-ray `range(time, range)`.
    with netCDF4.Dataset(converted) as nc:
        radx_range = np.asarray(nc["range"][:], dtype="f8")
        starts = np.asarray(nc["sweep_start_ray_index"][:], dtype=int)
    for index, (centres, rstart, rscale) in enumerate(geometry):
        row = radx_range[starts[index]] if radx_range.ndim == 2 else radx_range
        radx = rstart * 1000.0 + np.arange(len(centres)) * rscale
        n = min(len(radx), len(row))
        if not np.allclose(row[:n], radx[:n], atol=0.5):
            raise AssertionError(
                f"sweep {index}: Radx range starts {row[0]} m with {row[1] - row[0]} m gates, "
                f"not rstart {rstart} read as km with {rscale} m gates"
            )
    tally.notes.append("Radx reads rstart as the first gate centre in km")
    quantity = {index: odim_quantities(path, meta, index) for index in range(len(meta["sweeps"]))}

    def names(source, field):
        position = [f["name"] for f in meta["sweeps"][source]["fields"]].index(field["name"])
        return quantity[source][position]

    ranges = {index: g[0] for index, g in enumerate(geometry)}
    import h5py

    with h5py.File(path, "r") as h5:
        by_azimuth = any(
            odim_rays(
                h5[d]["how"].attrs if "how" in h5[d] else {}, int(h5[d]["where"].attrs["nrays"])
            )
            is None
            for d in numbered(h5, "dataset")
        )
    if by_azimuth:
        tally.notes.append("rays paired by azimuth (no startazT: Radx derives its own ray times)")
    sentinels = {index: odim_sentinels(path, meta, index) for index in range(len(meta["sweeps"]))}

    def missing_as(source, field):
        position = [f["name"] for f in meta["sweeps"][source]["fields"]].index(field["name"])
        return sentinels[source][position]

    # Radx's ODIM reader ignores `nodata` and `undetect` (it masks only the
    # lowest code of an integer plane): a missing gate may read as the
    # decoded nodata code.
    compare_cfradial1_file(
        "Radx",
        converted,
        meta,
        tally,
        names=names,
        ranges=ranges,
        by_azimuth=by_azimuth,
        missing_as=missing_as,
        repacked=True,
    )


def check_fm301_lrose(path, meta, tally):
    raise ReaderLimitation(
        "LROSE Radx has no FM301 reader: it recognises CfRadial 2 by `Conventions` containing "
        "'Cf/Radial' and a `version` 2.x, then falls back to its Leosphere lidar reader, which "
        "requires the CfRadial 2.0 `radar_parameters/radar_*` names (FM301-2022 drops the prefix) "
        "and overruns its stack on numeric array sweep attributes"
    )


# --------------------------------------------------------------------------
# FM301 / CfRadial 2
# --------------------------------------------------------------------------


def check_fm301_datatree(engine):
    def check(path, meta, tally):
        import xarray as xr

        tree = xr.open_datatree(str(path), engine=engine)
        names = [str(n) for n in tree["sweep_group_name"].values]
        assert names == [f"sweep_{i}" for i in range(len(meta["sweeps"]))], names
        for index, sweep in enumerate(meta["sweeps"]):
            ds = tree[f"sweep_{index}"].to_dataset()
            time = datetimes_to_epoch(ds["time"].values)
            rays = pair_rays(sweep["time"], sweep["azimuth"], time, ds["azimuth"].values)
            gates = pair_gates(sweep["range"], ds["range"].values)
            for field in sweep["fields"]:
                compare_field(
                    f"xarray[{engine}] sweep_{index}",
                    field,
                    to_float(ds[field["name"]].values),
                    rays,
                    gates,
                    tally,
                )

    return check


def check_fm301_xradar(path, meta, tally):
    import xradar

    tree = xradar.io.open_cfradial2_datatree(str(path))
    for index, sweep in enumerate(meta["sweeps"]):
        ds = tree[f"sweep_{index}"].to_dataset()
        time = datetimes_to_epoch(ds["time"].values)
        rays = pair_rays(sweep["time"], sweep["azimuth"], time, ds["azimuth"].values)
        gates = pair_gates(sweep["range"], ds["range"].values)
        for field in sweep["fields"]:
            compare_field(
                f"xradar sweep_{index}", field, to_float(ds[field["name"]].values), rays, gates, tally
            )


def check_fm301_netcdf4(path, meta, tally):
    import netCDF4

    with netCDF4.Dataset(path) as nc:
        for index, sweep in enumerate(meta["sweeps"]):
            group = nc[f"sweep_{index}"]
            time = epoch(group["time"][:], group["time"].units)
            rays = pair_rays(sweep["time"], sweep["azimuth"], time, group["azimuth"][:])
            gates = pair_gates(sweep["range"], group["range"][:])
            for field in sweep["fields"]:
                compare_field(
                    f"netCDF4 sweep_{index}",
                    field,
                    to_float(group[field["name"]][:]),
                    rays,
                    gates,
                    tally,
                )


def check_fm301_h5py(path, meta, tally):
    """h5py: the raw packed arrays decoded by hand (no netCDF layer)."""
    import h5py

    with h5py.File(path, "r") as h5:
        for index, sweep in enumerate(meta["sweeps"]):
            group = h5[f"sweep_{index}"]
            time = epoch(group["time"][...], text(group["time"].attrs["units"]))
            rays = pair_rays(sweep["time"], sweep["azimuth"], time, group["azimuth"][...])
            gates = pair_gates(sweep["range"], group["range"][...])
            for field in sweep["fields"]:
                var = group[field["name"]]
                raw = var[...]
                attrs = var.attrs

                def one(name, default):
                    return np.asarray(attrs.get(name, default)).ravel()[0]

                values = raw.astype("f8") * float(one("scale_factor", 1.0)) + float(
                    one("add_offset", 0.0)
                )
                if "_FillValue" in attrs:
                    fill = one("_FillValue", 0)
                    values[(raw == fill) if np.isfinite(fill) else np.isnan(raw)] = np.nan
                compare_field(f"h5py sweep_{index}", field, values, rays, gates, tally)


# --------------------------------------------------------------------------
# The source file: a reader's behaviour on the data, not on the written file
# --------------------------------------------------------------------------


def source_file(case_dir):
    """The source volume's own file, when `write_all` copied it (HDF5 or
    classic netCDF sources)."""
    for name in ("source.h5", "source.nc"):
        if (case_dir / name).exists():
            return case_dir / name
    return None


def file_kind(path):
    """`odim`, `cfradial1` or `fm301`: which outputs' readers also read
    `path`; `None` for anything else."""
    with open(path, "rb") as f:
        head = f.read(4)
    if head.startswith(b"CDF"):
        return "cfradial1"
    import h5py

    try:
        with h5py.File(path, "r") as h5:
            if "what" in h5 and "object" in h5["what"].attrs:
                return "odim"
            if "sweep_group_name" in h5:
                return "fm301"
            if "time" in h5 and "range" in h5:
                return "cfradial1"
    except OSError:
        return None
    return None


def output_kind(filename):
    if filename.startswith("odim"):
        return "odim"
    if filename.startswith("cfradial1"):
        return "cfradial1"
    return "fm301"


def normalised(err, *paths):
    text = f"{type(err).__name__}: {err}"
    for path in paths:
        text = text.replace(str(path), "<file>").replace(path.name, "<file>")
    return text


def radx_reading(path, meta, tally):
    """Radx's CfRadial 1 conversion of `path` against the source volume: how
    Radx reads a file of any format it takes."""
    maskable = radx_masks_int16_code(path) if file_kind(path) == "cfradial1" else None
    compare_cfradial1_file(
        "Radx", lrose_output(path), meta, tally, repacked=True, maskable=maskable
    )


def fails_alike_on_source(case_dir, filename, reader, check, meta, err):
    """The reader's error on the source file, when it fails there exactly as
    on the written file; `None` otherwise (no source the reader takes, the
    reader reads the source, or it fails differently). Radx converts
    CfRadial 1 and 2 alike to CfRadial 1, so its reading of a CfRadial
    source is compared with its reading of a written CfRadial 1 file."""
    source = source_file(case_dir)
    if source is None:
        return None
    kind = file_kind(source)
    if reader == "lrose" and output_kind(filename) == "cfradial1" and kind in ("cfradial1", "fm301"):
        check = radx_reading
    elif kind != output_kind(filename):
        return None
    gc.collect()
    try:
        check(source, meta, Tally())
    except ReaderLimitation:
        return None
    except Exception as source_err:  # noqa: BLE001 - compared below
        written = normalised(err, case_dir / filename)
        if normalised(source_err, source) == written:
            return written
    return None


CHECKS = {
    "odim.h5": [
        ("h5py", check_odim_h5py),
        ("wradlib", check_odim_wradlib),
        ("xradar", check_odim_xradar),
        ("pyart", check_odim_pyart),
        ("lrose", check_odim_lrose),
    ],
    # `OdimWriteOptions::every_quantity`, when it differs from odim.h5.
    "odim-every.h5": [
        ("h5py", check_odim_h5py),
        ("pyart", check_odim_pyart),
        ("lrose", check_odim_lrose),
    ],
    "cfradial1.nc": [
        ("netCDF4", check_cfradial1_netcdf4),
        ("xradar", check_cfradial1_xradar),
        ("pyart", check_cfradial1_pyart),
        ("lrose", check_cfradial1_lrose),
    ],
    # `RangeLayout::PerRay`, when it differs from cfradial1.nc.
    "cfradial1-perray.nc": [
        ("netCDF4", check_cfradial1_netcdf4),
        ("xradar", check_cfradial1_xradar),
        ("pyart", check_cfradial1_pyart),
        ("lrose", check_cfradial1_lrose),
    ],
    # The same with `Cfradial1Options::unsigned_attribute`: readers that
    # apply `_Unsigned` (LROSE Radx does not, so it is not run).
    "cfradial1-unsigned.nc": [
        ("netCDF4", check_cfradial1_netcdf4),
        ("xradar", check_cfradial1_xradar),
        ("pyart", check_cfradial1_pyart),
    ],
    "fm301.nc": [
        ("xarray-netcdf4", check_fm301_datatree("netcdf4")),
        ("xarray-h5netcdf", check_fm301_datatree("h5netcdf")),
        ("xradar", check_fm301_xradar),
        ("netCDF4", check_fm301_netcdf4),
        ("h5py", check_fm301_h5py),
        ("lrose", check_fm301_lrose),
    ],
}


def main(argv):
    flags = {arg for arg in argv[1:] if arg.startswith("-")}
    args = [arg for arg in argv[1:] if not arg.startswith("-")]
    out = pathlib.Path(args[0])
    ids = args[1:] or sorted(p.name for p in out.iterdir() if (p / "expect").is_dir())
    lrose = "--no-lrose" not in flags
    if lrose:
        run_lrose(out, ids)
    counts = {"ok": 0, "LIMIT": 0, "FAIL": 0}
    for case in ids:
        case_dir = out / case
        meta = load_expect(case_dir)
        for filename, checks in CHECKS.items():
            path = case_dir / filename
            if not path.exists():
                error_file = case_dir / (filename + ".error")
                if error_file.exists():
                    print(f"{case} {filename}: refused by the writer: {error_file.read_text().strip()}")
                continue
            for reader, check in checks:
                if reader == "lrose" and not lrose:
                    continue
                tally = Tally()
                # xarray's file managers close files from finalizers that take
                # its non-reentrant HDF5 lock: a cyclic collection that runs
                # one while this thread holds the lock deadlocks (seen in
                # open_datatree after a few dozen checks). Collect between
                # checks, never during one.
                gc.collect()
                gc.disable()
                try:
                    check(path, meta, tally)
                    counts["ok"] += 1
                    print(f"{case} {filename} {reader}: ok ({tally.text()})")
                except ReaderLimitation as limit:
                    counts["LIMIT"] += 1
                    print(f"{case} {filename} {reader}: LIMIT {limit}")
                except Exception as err:  # noqa: BLE001 - report every reader failure
                    alike = fails_alike_on_source(case_dir, filename, reader, check, meta, err)
                    if alike is not None:
                        counts["LIMIT"] += 1
                        print(
                            f"{case} {filename} {reader}: LIMIT fails the same way on the source "
                            f"file: {alike}"
                        )
                        continue
                    counts["FAIL"] += 1
                    print(f"{case} {filename} {reader}: FAIL {type(err).__name__}: {err}")
                    if "-v" in flags:
                        traceback.print_exc()
                finally:
                    gc.enable()
    print(f"{counts['ok']} ok, {counts['LIMIT']} reader limitations, {counts['FAIL']} failures")
    return 1 if counts["FAIL"] else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
