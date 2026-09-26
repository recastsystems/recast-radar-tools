"""``pyart.core.Radar`` objects from FM301 DataTrees.

Py-ART keeps a whole volume in one set of arrays: rays of every sweep in
acquisition order, one ``range`` for the volume. This module lays an FM301
DataTree out that way:

* Rays are taken in acquisition order (sorted by ``time`` when the tree's ray
  dimension is ``azimuth`` or ``elevation``). Paths, bytes and
  :class:`recast_radar.Volume` objects are read with ``first_dim="time"``, so
  their rays are in the decoder's order.
* The volume range starts at the smallest first gate centre, uses the
  smallest gate spacing and reaches the farthest gate of any sweep (Py-ART's
  ``_find_range_params``). Each volume gate takes the sweep gate whose extent
  holds its centre, so a sweep with coarser gates has each gate repeated over
  the finer ones it covers, which is what
  ``pyart.io.read_nexrad_archive(..., linear_interp=False)`` produces (with
  its 2:1 rule: the volume gate past the centre of the last coarse gate stays
  masked); gates a sweep does not have are masked.
* Fields are masked where the packed value is ``_FillValue``, ``_Undetect``
  or range folded, and where the value is NaN. They are scaled as xarray
  scales them (``raw * scale_factor + add_offset`` in the float type the
  attributes call for). NEXRAD fields are then cast to float32: the result is
  bit-identical to Py-ART's ``(raw - offset) / scale`` in float32
  (design note ``docs/design/fm301-model.md`` 12.3).
* Field names: ``field_names="config"`` gives Py-ART's defaults
  (``reflectivity``, ``velocity``, ...), ``"reader"`` the names the Py-ART
  reader for the source format gives, ``"fm301"`` the FM301 names, or pass a
  dict.

Trees made by other readers (xradar) work too, with FM301 or xradar names.
"""

from __future__ import annotations

import re
from typing import Any

import numpy as np
import xarray as xr

NEXRAD_SOURCES = ("nexrad_level2", "nexrad_level3")
# Py-ART's CfRadial reader keeps netCDF's decoded type; its other readers
# store fields as float32.
KEEP_TYPE_SOURCES = ("cfradial1", "cfradial2")

# Per-ray instrument variables Py-ART keeps in ``instrument_parameters``.
RAY_PARAMETERS = (
    "nyquist_velocity",
    "unambiguous_range",
    "prt",
    "prt_ratio",
    "pulse_width",
    "n_samples",
    "measured_transmit_power_h",
    "measured_transmit_power_v",
    "radar_measured_transmit_power_h",
    "radar_measured_transmit_power_v",
)
# Per-sweep text variables Py-ART keeps in ``instrument_parameters``.
SWEEP_TEXT_PARAMETERS = ("prt_mode", "follow_mode", "polarization_mode")
# radar_parameters group variables, by FM301 (WMO) or xradar name, to Py-ART's.
RADAR_PARAMETERS = {
    "radar_antenna_gain_h": "radar_antenna_gain_h",
    "radar_antenna_gain_v": "radar_antenna_gain_v",
    "antenna_gain_h": "radar_antenna_gain_h",
    "antenna_gain_v": "radar_antenna_gain_v",
    "radar_beam_width_h": "radar_beam_width_h",
    "radar_beam_width_v": "radar_beam_width_v",
    "beam_width_h": "radar_beam_width_h",
    "beam_width_v": "radar_beam_width_v",
    "radar_receiver_bandwidth": "radar_receiver_bandwidth",
    "receiver_bandwidth": "radar_receiver_bandwidth",
}


def to_pyart(source: Any, *, field_names: Any = "config", station=None, volume: int = 0):
    """A ``pyart.core.Radar`` from a path, bytes, a Volume or a DataTree."""
    from . import _native
    from ._tree import build_datatree

    if isinstance(source, xr.DataTree):
        tree = source
    else:
        if isinstance(source, _native.Volume):
            native = source._tree(first_dim="time")
        else:
            native = _native._open_tree(source, first_dim="time", station=station, volume=volume)
        tree = build_datatree(
            native,
            decode=False,
            decode_times=False,
            mask_range_folded=False,
            packed_attrs="attrs",
            storage_order=True,
            warn=False,
        )
    return radar_from_datatree(tree, field_names=field_names)


def _sweeps(tree: xr.DataTree) -> list[xr.Dataset]:
    found = []
    for name, node in tree.children.items():
        match = re.fullmatch(r"sweep_(\d+)", name)
        if match:
            found.append((int(match.group(1)), node.to_dataset(inherit=False)))
    if not found:
        raise ValueError("the DataTree has no sweep_<n> groups")
    return [ds for _, ds in sorted(found, key=lambda item: item[0])]


def _ray_dim(ds: xr.Dataset) -> str:
    for name in ("time", "azimuth", "elevation"):
        if name in ds.variables and ds[name].ndim == 1:
            return ds[name].dims[0]
    raise ValueError("sweep has no time, azimuth or elevation coordinate")


_UNITS = re.compile(r"^\s*(seconds|milliseconds)\s+since\s+(\S+)")


def _times(ds: xr.Dataset) -> np.ndarray:
    """Ray times as int64 nanoseconds since the epoch."""
    time = ds["time"]
    values = np.asarray(time.values)
    if np.issubdtype(values.dtype, np.datetime64):
        return values.astype("datetime64[ns]").astype(np.int64)
    units = time.attrs.get("units") or time.encoding.get("units", "")
    match = _UNITS.match(str(units))
    if not match:
        raise ValueError(f"time units {units!r} are not '<seconds|milliseconds> since <time>'")
    scale = 1e9 if match.group(1) == "seconds" else 1e6
    reference = np.datetime64(match.group(2).rstrip("Z").replace("T", " ").strip(), "ns")
    return reference.astype(np.int64) + np.round(values.astype(np.float64) * scale).astype(np.int64)


def _encoded_units_reference(ds: xr.Dataset) -> str | None:
    units = ds["time"].attrs.get("units")
    if units is None:
        return None
    match = _UNITS.match(str(units))
    return match.group(2) if match and match.group(1) == "seconds" else None


def _attr(var: xr.DataArray, key: str):
    if key in var.attrs:
        return var.attrs[key]
    return var.encoding.get(key)


def _float_dtype(dtype: np.dtype, scale_factor, add_offset) -> np.dtype:
    """xarray's choice of decoded float type (``_choose_float_dtype``)."""
    if scale_factor is not None or add_offset is not None:
        scale_type = np.dtype(type(scale_factor)) if scale_factor is not None else None
        offset_type = np.dtype(type(add_offset)) if add_offset is not None else None
        if (
            scale_factor is not None
            and add_offset is not None
            and scale_type == offset_type
            and scale_type in (np.dtype(np.float32), np.dtype(np.float64))
        ):
            if dtype.itemsize == 4 and np.issubdtype(dtype, np.integer):
                return np.dtype(np.float64)
            return scale_type
        if add_offset is not None:
            return np.dtype(np.float64)
        return scale_type
    if dtype.itemsize <= 4 and np.issubdtype(dtype, np.floating):
        return np.dtype(np.float32)
    if dtype.itemsize <= 2 and np.issubdtype(dtype, np.integer):
        return np.dtype(np.float32)
    return np.dtype(np.float64)


def _range_folded_code(var: xr.DataArray):
    values = _attr(var, "flag_values")
    meanings = _attr(var, "flag_meanings")
    if values is None or not meanings:
        return None
    names = str(meanings).split()
    codes = np.atleast_1d(values)
    for name, code in zip(names, codes):
        if name == "range_folded":
            return code
    return None


def _is_raw(var: xr.DataArray) -> bool:
    """Whether a variable holds its stored values (not CF-decoded)."""
    return not any(key in var.encoding for key in ("scale_factor", "add_offset", "_FillValue"))


def _field_values(var: xr.DataArray, float32: bool) -> np.ma.MaskedArray:
    """Physical values of one sweep's field, masked, rays in the given order.

    Masked: ``_FillValue``, ``missing_value``, ``_Undetect``, the range-folded
    code and NaN.
    """
    data = np.asarray(var.values)
    mask = np.zeros(data.shape, dtype=bool)
    if _is_raw(var):
        for key in ("_FillValue", "missing_value", "_Undetect"):
            code = var.attrs.get(key)
            if code is not None:
                mask |= data == code
        folded = _range_folded_code(var)
        if folded is not None:
            mask |= data == folded
        scale_factor = var.attrs.get("scale_factor")
        add_offset = var.attrs.get("add_offset")
        if scale_factor is None and add_offset is None and np.issubdtype(data.dtype, np.floating):
            values = data
        else:
            values = data.astype(_float_dtype(data.dtype, scale_factor, add_offset), copy=True)
            if scale_factor is not None:
                values *= scale_factor
            if add_offset is not None:
                values += add_offset
    else:
        values = data
        undetect = _attr(var, "_Undetect")
        if undetect is not None:
            scale_factor = var.encoding.get("scale_factor")
            add_offset = var.encoding.get("add_offset")
            packed = np.asarray(undetect)
            decoded = packed.astype(_float_dtype(packed.dtype, scale_factor, add_offset))
            if scale_factor is not None:
                decoded = decoded * scale_factor
            if add_offset is not None:
                decoded = decoded + add_offset
            mask |= values == np.asarray(decoded).astype(values.dtype)
    if np.issubdtype(values.dtype, np.floating):
        mask |= np.isnan(values)
    if float32:
        values = values.astype(np.float32)
    return np.ma.masked_array(values, mask=mask)


class _Range:
    """The volume range and where each sweep's gates fall on it.

    As Py-ART's ``_find_range_params``: the range starts at the smallest
    first gate centre, steps by the smallest gate spacing and stops before
    the farthest gate end (``first + spacing * (ngates - 0.5)``) of any sweep.
    A volume gate takes the sweep gate whose extent holds its centre, the
    lower gate on a tie, so a sweep with coarser gates has each gate repeated
    over the finer ones it covers. That is what
    ``pyart.io.read_nexrad_archive(..., linear_interp=False)`` does for the
    4:1 and 2:1 ratios it handles, including its 2:1 quirk: the volume gate
    past the centre of a sweep's last gate stays masked (Py-ART repeats each
    gate twice except the last).
    """

    # Sweep gate lookups are made in float64; a volume gate centre this close
    # (in sweep gates) to a sweep gate edge counts as on the edge.
    _TIE = 1e-6

    def __init__(self, sweeps: list[xr.Dataset]) -> None:
        geometry = []
        for ds in sweeps:
            centres = np.asarray(ds["range"].values, dtype=np.float64)
            if centres.size == 0:
                geometry.append((0.0, 0.0, 0))
                continue
            if centres.size > 1:
                spacing = float(np.median(np.diff(centres)))
            else:
                spacing = float(ds["range"].attrs.get("meters_between_gates", 1.0))
            geometry.append((float(centres[0]), spacing, int(centres.size)))
        self.explicit = None
        spacings = [spacing for _, spacing, n in geometry if n and spacing > 0]
        uniform = all(
            n < 2 or np.allclose(np.diff(np.asarray(ds["range"].values, dtype=np.float64)), spacing, rtol=1e-4)
            for ds, (_, spacing, n) in zip(sweeps, geometry)
        )
        if not uniform or not spacings:
            first = np.asarray(sweeps[0]["range"].values)
            if all(np.array_equal(np.asarray(ds["range"].values), first) for ds in sweeps):
                self.explicit = first
                self.spacing = float(np.median(np.diff(first))) if first.size > 1 else 0.0
                self.first = float(first[0]) if first.size else 0.0
                self.ngates = int(first.size)
                self.placement = [None for _ in sweeps]
                return
            raise ValueError("sweeps have non-uniform ranges that differ; Py-ART needs one range")
        self.spacing = min(spacings)
        self.first = min(first for first, spacing, n in geometry if n and spacing > 0)
        last = max(first + spacing * (n - 0.5) for first, spacing, n in geometry if n and spacing > 0)
        self.ngates = max(int(np.ceil((last - self.first) / self.spacing - self._TIE)), 0)
        volume = self.first + self.spacing * np.arange(self.ngates, dtype=np.float64)
        self.placement = [self._placement(volume, *sweep) for sweep in geometry]

    def _placement(self, volume: np.ndarray, first: float, spacing: float, n: int):
        """``(volume gates, sweep gates)`` index arrays, or ``(start, stop,
        offset)`` when the sweep's gates are the volume's shifted by
        ``offset`` (a plain slice copy)."""
        if not n or spacing <= 0:
            return (np.zeros(0, dtype=np.intp), np.zeros(0, dtype=np.intp))
        position = (volume - first) / spacing
        gate = np.ceil(position - 0.5 - self._TIE).astype(np.intp)
        keep = (gate >= 0) & (gate < n)
        ratio = spacing / self.spacing
        if abs(ratio - 2.0) < 1e-6:
            keep &= position <= (n - 1) + self._TIE
        targets = np.flatnonzero(keep)
        gates = gate[targets]
        if targets.size and np.array_equal(np.diff(targets), np.ones(targets.size - 1)) and np.array_equal(
            np.diff(gates), np.ones(gates.size - 1)
        ):
            return (int(targets[0]), int(targets[-1]) + 1, int(gates[0]))
        return (targets, gates)

    def centres(self) -> np.ndarray:
        if self.explicit is not None:
            return np.asarray(self.explicit, dtype=np.float32)
        return (self.first + self.spacing * np.arange(self.ngates)).astype(np.float32)

    def place(self, values: np.ma.MaskedArray, sweep: int) -> np.ma.MaskedArray:
        """``values`` (rays x sweep gates) on the volume range."""
        placement = self.placement[sweep]
        if placement is None:
            return values
        out = np.ma.masked_all((values.shape[0], self.ngates), dtype=values.dtype)
        values = np.ma.asarray(values)
        if len(placement) == 3:
            start, stop, offset = placement
            out[:, start:stop] = values[:, offset : offset + (stop - start)]
        else:
            targets, gates = placement
            if targets.size:
                out[:, targets] = values[:, gates]
        return out


def _field_name_map(tree: xr.DataTree, field_names: Any) -> Any:
    table = tree.encoding.get("pyart_names") or {}
    source = tree.encoding.get("source_format") or "unknown"
    if isinstance(field_names, dict):
        return lambda name: field_names.get(name, name)
    if field_names == "fm301":
        return lambda name: name
    if field_names not in ("config", "reader"):
        raise ValueError('field_names must be "config", "reader", "fm301" or a dict')

    def lookup(name: str) -> str:
        entry = table.get(name)
        if entry is not None:
            return entry[field_names]
        from ._native import pyart_field_name

        return pyart_field_name(name, field_names, source)

    return lookup


def _plain(value: Any) -> Any:
    if isinstance(value, np.generic):
        return value.item()
    return value


def radar_from_datatree(tree: xr.DataTree, *, field_names: Any = "config"):
    """Lay an FM301 DataTree out as a ``pyart.core.Radar``."""
    import pyart

    get_metadata = pyart.config.get_metadata
    fill_value = pyart.config.get_fillvalue()
    source = tree.encoding.get("source_format")
    root = tree.to_dataset(inherit=False)
    nexrad = source in NEXRAD_SOURCES or str(root.attrs.get("source", "")).startswith("NEXRAD")
    float32 = nexrad or source not in KEEP_TYPE_SOURCES
    name_of = _field_name_map(tree, field_names)

    sweeps = []
    for ds in _sweeps(tree):
        dim = _ray_dim(ds)
        times = _times(ds)
        order = np.argsort(times, kind="stable") if dim != "time" else np.arange(times.size)
        sweeps.append((ds.isel({dim: order}), dim, times[order]))

    all_times = np.concatenate([times for _, _, times in sweeps])
    reference = _encoded_units_reference(sweeps[0][0])
    if reference is not None and not all(
        _encoded_units_reference(ds) == reference for ds, _, _ in sweeps
    ):
        reference = None
    if reference is not None and not nexrad and all_times.size and (
        all_times.min() < np.datetime64(reference.rstrip("Z"), "ns").astype(np.int64)
    ):
        # A reference after the first ray (norst 2017: ODIM what/time is a
        # minute after the first ray) would give negative times; Py-ART's
        # times start at the volume's first ray. Its NEXRAD reader keeps the
        # volume header time even when that is later, so NEXRAD trees keep
        # theirs.
        reference = None
    if reference is not None:
        ref_text = reference if reference.endswith("Z") else reference + "Z"
        ref_ns = np.datetime64(ref_text.rstrip("Z"), "ns").astype(np.int64)
    else:
        ref_ns = (all_times.min() // 1_000_000_000) * 1_000_000_000
        ref_text = str(np.datetime64(int(ref_ns), "ns").astype("datetime64[s]")) + "Z"

    time = get_metadata("time")
    if reference is not None and ref_text.rstrip("Z") == reference.rstrip("Z"):
        time["data"] = np.concatenate(
            [np.asarray(ds["time"].values, dtype=np.float64) for ds, _, _ in sweeps]
        )
    else:
        time["data"] = (all_times - ref_ns) / 1e9
    time["units"] = f"seconds since {ref_text}"

    datasets = [ds for ds, _, _ in sweeps]
    volume_range = _Range(datasets)
    _range = get_metadata("range")
    _range["data"] = volume_range.centres()
    _range["meters_to_center_of_first_gate"] = float(volume_range.first)
    _range["meters_between_gates"] = float(volume_range.spacing)

    nrays = [ds.sizes[dim] for ds, dim, _ in sweeps]
    ends = np.cumsum(nrays, dtype=np.int64)
    starts = ends - np.asarray(nrays, dtype=np.int64)
    total = int(ends[-1]) if len(ends) else 0

    # The codes of a field coded by a Level III level table (`<name>_level`,
    # named in the field's `ancillary_variables`) are not a field of their own.
    codes = {
        ancillary
        for ds, _, _ in sweeps
        for var in ds.data_vars.values()
        for ancillary in str(var.attrs.get("ancillary_variables", "")).split()
        if ancillary.endswith("_level")
    }
    fields: dict[str, dict] = {}
    for index, (ds, dim, _) in enumerate(sweeps):
        for name, var in ds.data_vars.items():
            if var.dims != (dim, "range") or name.endswith("_flags") or name in codes:
                continue
            key = name_of(name)
            values = volume_range.place(_field_values(var, float32), index)
            entry = fields.get(key)
            if entry is None:
                dtype = values.dtype
                entry = get_metadata(key)
                for attr in ("units", "standard_name", "long_name"):
                    value = var.attrs.get(attr)
                    if value:
                        entry[attr] = value
                entry["_FillValue"] = fill_value
                entry["data"] = np.ma.masked_all((total, volume_range.ngates), dtype=dtype)
                fields[key] = entry
            data = entry["data"]
            if values.dtype != data.dtype:
                data = data.astype(np.result_type(data.dtype, values.dtype))
                entry["data"] = data
            data[starts[index] : ends[index]] = values
    for entry in fields.values():
        entry["data"].set_fill_value(fill_value)

    def per_ray(name: str, dtype=np.float32):
        return np.concatenate([np.asarray(ds[name].values).astype(dtype) for ds, _, _ in sweeps])

    azimuth = get_metadata("azimuth")
    azimuth["data"] = np.concatenate([np.asarray(ds["azimuth"].values) for ds, _, _ in sweeps])
    elevation = get_metadata("elevation")
    elevation["data"] = np.concatenate([np.asarray(ds["elevation"].values) for ds, _, _ in sweeps])

    def sweep_value(ds: xr.Dataset, *names: str):
        for name in names:
            if name in ds.variables:
                return np.asarray(ds[name].values).reshape(-1)[0]
        return None

    fixed_angle = get_metadata("fixed_angle")
    fixed_angle["data"] = np.array(
        [sweep_value(ds, "sweep_fixed_angle", "fixed_angle") for ds, _, _ in sweeps], dtype=np.float32
    )
    sweep_number = get_metadata("sweep_number")
    sweep_number["data"] = np.arange(len(sweeps), dtype=np.int32)
    modes = [str(_plain(sweep_value(ds, "sweep_mode")) or "azimuth_surveillance") for ds, _, _ in sweeps]
    sweep_mode = get_metadata("sweep_mode")
    sweep_mode["data"] = np.array(modes, dtype="S")
    sweep_start = get_metadata("sweep_start_ray_index")
    sweep_start["data"] = starts.astype(np.int32)
    sweep_end = get_metadata("sweep_end_ray_index")
    sweep_end["data"] = (ends - 1).astype(np.int32)

    if all(mode == "rhi" for mode in modes):
        scan_type = "rhi"
    elif all(mode == "vertical_pointing" for mode in modes):
        scan_type = "vpt"
    else:
        scan_type = "ppi"

    location = {}
    per_ray_location = all(
        all(name in ds.variables and ds[name].dims == (dim,) for name in ("latitude", "longitude"))
        for ds, dim, _ in sweeps
    )
    for name in ("latitude", "longitude", "altitude"):
        entry = get_metadata(name)
        if per_ray_location and all(name in ds.variables for ds, _, _ in sweeps):
            entry["data"] = per_ray(name, np.float64)
        else:
            value = root[name].values if name in root.variables else np.nan
            entry["data"] = np.atleast_1d(np.asarray(value, dtype=np.float64))
        location[name] = entry
    altitude_agl = None
    if "altitude_agl" in root.variables:
        altitude_agl = get_metadata("altitude_agl")
        altitude_agl["data"] = np.atleast_1d(np.asarray(root["altitude_agl"].values, dtype=np.float64))

    metadata = get_metadata("metadata")
    for key, value in root.attrs.items():
        metadata[key] = _plain(value)
    metadata["original_container"] = tree.encoding.get("format_name") or metadata.get("source", "")
    scan_name = str(root.attrs.get("scan_name", ""))
    vcp = re.fullmatch(r"VCP-(\d+)", scan_name)
    if vcp:
        metadata["vcp_pattern"] = int(vcp.group(1))

    instrument_parameters = {}
    for name in RAY_PARAMETERS:
        present = [ds[name] for ds, dim, _ in sweeps if name in ds.variables and ds[name].dims == (dim,)]
        if not present:
            continue
        dtype = np.result_type(present[0].dtype, np.float32)
        key = name.removeprefix("radar_") if name.startswith("radar_measured") else name
        entry = get_metadata(key)
        entry["data"] = np.concatenate(
            [
                np.asarray(ds[name].values, dtype=dtype)
                if name in ds.variables and ds[name].dims == (dim,)
                else np.full(ds.sizes[dim], np.nan, dtype=dtype)
                for ds, dim, _ in sweeps
            ]
        )
        instrument_parameters[key] = entry
    for name in SWEEP_TEXT_PARAMETERS:
        values = [sweep_value(ds, name) for ds, _, _ in sweeps]
        if all(value is not None for value in values):
            entry = get_metadata(name)
            entry["data"] = np.array([str(_plain(value)) for value in values], dtype="S")
            instrument_parameters[name] = entry
    if "radar_parameters" in tree.children:
        parameters = tree["radar_parameters"].to_dataset(inherit=False)
        for name, var in parameters.data_vars.items():
            target = RADAR_PARAMETERS.get(name)
            if target is None:
                continue
            entry = get_metadata(target)
            entry["data"] = np.atleast_1d(np.asarray(var.values))
            instrument_parameters[target] = entry
    frequency = root["frequency"] if "frequency" in root.variables else None
    if frequency is None and "frequency" in datasets[0].variables:
        frequency = datasets[0]["frequency"]
    if frequency is not None:
        entry = get_metadata("frequency")
        entry["data"] = np.atleast_1d(np.asarray(frequency.values, dtype=np.float32))
        instrument_parameters["frequency"] = entry

    radar_calibration = None
    if "radar_calibration" in tree.children:
        calibration = tree["radar_calibration"].to_dataset(inherit=False)
        radar_calibration = {
            name: {"data": np.atleast_1d(np.asarray(var.values)), **{k: _plain(v) for k, v in var.attrs.items()}}
            for name, var in calibration.data_vars.items()
        } or None

    optional = {}
    for key, names in (
        ("target_scan_rate", ("target_scan_rate",)),
        ("ray_angle_res", ("rays_angle_resolution", "ray_angle_res")),
    ):
        values = [sweep_value(ds, *names) for ds, _, _ in sweeps]
        if all(value is not None for value in values):
            entry = get_metadata(key)
            entry["data"] = np.array([float(value) for value in values], dtype=np.float32)
            optional[key] = entry
    indexed = [sweep_value(ds, "rays_are_indexed") for ds, _, _ in sweeps]
    if all(value is not None for value in indexed):
        entry = get_metadata("rays_are_indexed")
        entry["data"] = np.array([str(_plain(value)).lower() for value in indexed], dtype="S")
        optional["rays_are_indexed"] = entry
    for key in ("scan_rate", "antenna_transition"):
        if all(key in ds.variables and ds[key].dims == (dim,) for ds, dim, _ in sweeps):
            entry = get_metadata(key)
            dtype = sweeps[0][0][key].dtype if key == "antenna_transition" else np.float32
            entry["data"] = per_ray(key, dtype)
            optional[key] = entry

    return pyart.core.Radar(
        time,
        _range,
        fields,
        metadata,
        scan_type,
        location["latitude"],
        location["longitude"],
        location["altitude"],
        sweep_number,
        sweep_mode,
        fixed_angle,
        sweep_start,
        sweep_end,
        azimuth,
        elevation,
        altitude_agl=altitude_agl,
        instrument_parameters=instrument_parameters or None,
        radar_calibration=radar_calibration,
        **optional,
    )
