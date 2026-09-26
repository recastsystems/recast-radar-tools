"""Build an ``xarray.DataTree`` from the native FM301 tree.

The native side (``src/tree.rs``) hands over the FM301 view of a volume as
nested dictionaries. Field buffers arrive as NumPy arrays that own the
decoder's memory (no copy was made); a field whose buffer is not already the
FM301 variable (rays in another order, gates padded or repeated) is wrapped in
:class:`FieldArray`, which applies the mapping when values are read, as
xradar's own backend arrays do. CF decoding is xarray's (``xr.decode_cf``),
so it is lazy too.

Design note: ``docs/design/fm301-model.md`` sections 12.2 and 12.3.
"""

from __future__ import annotations

import warnings
from typing import Any

import numpy as np
import xarray as xr
from xarray.backends import BackendArray
from xarray.core import indexing

# Attributes in packed units that stay in ``attrs`` after xarray's CF
# decoding; ``packed_attrs="encoding"`` moves them into ``encoding``
# (design note 12.3).
PACKED_ATTRS = (
    "_Undetect",
    "valid_range",
    "valid_min",
    "valid_max",
    "flag_values",
    "flag_masks",
    "flag_meanings",
)

# Variables that are coordinates wherever they appear (xradar's layout: the
# ray and gate coordinates in sweeps, the location and frequency at the root).
COORDINATE_NAMES = frozenset(
    {"time", "range", "azimuth", "elevation", "latitude", "longitude", "altitude", "frequency"}
)

FLAG_SUFFIX = "_flags"


class FieldArray(BackendArray):
    """A field buffer read through its FM301 mapping.

    ``native`` is the moved ``[nrays, native_gates]`` buffer in storage ray
    order. Row ``i`` of the variable is storage row ``rows[i]`` (all rows in
    order when ``rows`` is None); native gate ``j`` covers range gates
    ``start + j*stride`` to ``start + j*stride + stride - 1``; other gates hold
    ``fill``. With ``remap``, values equal to that code read as ``fill`` (range
    folding masked by CF decoding). With ``flags_of``, the array is ``uint8``:
    1 where the native value equals that code, else 0.
    """

    def __init__(
        self,
        native: np.ndarray,
        *,
        rows: np.ndarray | None,
        start: int,
        stride: int,
        out_gates: int,
        nrays: int,
        fill: Any,
        remap: Any = None,
        flags_of: Any = None,
    ) -> None:
        self.native = native
        self.rows = rows
        self.start = int(start)
        self.stride = max(int(stride), 1)
        self.out_gates = int(out_gates)
        self.remap = remap
        self.flags_of = flags_of
        self.shape = (int(nrays), self.out_gates)
        if flags_of is not None:
            self.dtype = np.dtype(np.uint8)
            self.fill = np.uint8(0)
        else:
            self.dtype = native.dtype
            self.fill = fill

    def __getitem__(self, key):
        return indexing.explicit_indexing_adapter(
            key, self.shape, indexing.IndexingSupport.OUTER, self._getitem
        )

    def _getitem(self, key):
        row_key, gate_key = key
        if self.rows is None:
            block = self.native[row_key]
        else:
            block = self.native[self.rows[row_key]]
        single_row = block.ndim == 1
        if single_row:
            block = block[np.newaxis, :]
        if self.flags_of is not None:
            block = (block == self.flags_of).astype(np.uint8)
        elif self.remap is not None:
            block = np.where(block == self.remap, self.fill, block).astype(self.dtype, copy=False)
        out = self._gates(block)[:, gate_key]
        if single_row:
            out = out[0]
        return out

    def _gates(self, block: np.ndarray) -> np.ndarray:
        native_gates = block.shape[1]
        if self.start == 0 and self.stride == 1 and native_gates == self.out_gates:
            return block
        out = np.full((block.shape[0], self.out_gates), self.fill, dtype=self.dtype)
        expanded = block if self.stride == 1 else np.repeat(block, self.stride, axis=1)
        end = min(self.out_gates, self.start + expanded.shape[1])
        if end > self.start:
            out[:, self.start : end] = expanded[:, : end - self.start]
        return out


def _lazy(array: BackendArray) -> indexing.LazilyIndexedArray:
    return indexing.LazilyIndexedArray(array)


def _shape(dims: tuple[str, ...], sizes: dict[str, int]) -> tuple[int, ...] | None:
    try:
        return tuple(sizes[dim] for dim in dims)
    except KeyError:
        return None


def _plain_data(var: dict, sizes: dict[str, int]) -> np.ndarray:
    data = var["data"]
    kind = var["kind"]
    if kind == "array" and isinstance(data, list):
        array = np.array(data, dtype=str) if data else np.array([], dtype=str)
    else:
        array = np.asarray(data)
    dims = tuple(var["dims"])
    shape = _shape(dims, sizes)
    if shape is not None and array.shape != shape and array.size == int(np.prod(shape)):
        array = array.reshape(shape)
    return array


def _field_variables(var: dict, attrs: dict, options: dict) -> list[tuple[str, xr.Variable]]:
    """The field's variable, and its range-folded flag variable when asked."""
    name = var["name"]
    dims = tuple(var["dims"])
    native = var["data"]
    range_folded = var["range_folded"]
    spec = dict(
        rows=var["rows"],
        start=var["start"],
        stride=var["stride"],
        out_gates=var["out_gates"],
        nrays=var["nrays"],
        fill=var["fill"],
    )
    remap = None
    if (
        options["decode"]
        and options["mask_range_folded"]
        and range_folded is not None
        and "_FillValue" in attrs
        and range_folded != attrs["_FillValue"]
    ):
        remap = range_folded
    if remap is None and var["zero_copy"]:
        data = native
    else:
        data = _lazy(FieldArray(native, remap=remap, **spec))
    out = []
    if options["range_folded_variable"] and range_folded is not None:
        flag_name = name + FLAG_SUFFIX
        flags = _lazy(FieldArray(native, flags_of=range_folded, **spec))
        flag_attrs = {
            "long_name": f"range-folded gates of {name}",
            "flag_values": np.array([1], dtype=np.uint8),
            "flag_meanings": "range_folded",
        }
        previous = attrs.get("ancillary_variables")
        attrs["ancillary_variables"] = f"{previous} {flag_name}" if previous else flag_name
        out.append((flag_name, xr.Variable(dims, flags, flag_attrs)))
    out.insert(0, (name, xr.Variable(dims, data, attrs)))
    return out


def _storage_order(var: dict, ray_order: tuple[str, np.ndarray] | None) -> dict:
    """``var`` with its rays in storage order (``ray_order`` is the ray
    dimension and the inverse of the view's permutation)."""
    if ray_order is None:
        return var
    ray_dim, inverse = ray_order
    if var["kind"] == "field":
        var = dict(var)
        var["rows"] = None
        var["zero_copy"] = (
            int(var["start"]) == 0
            and int(var["stride"]) <= 1
            and int(var["native_gates"]) == int(var["out_gates"])
        )
        return var
    if var["kind"] == "array" and var["dims"] and var["dims"][0] == ray_dim:
        var = dict(var)
        data = var["data"]
        if isinstance(data, list):
            var["data"] = [data[int(i)] for i in inverse]
        else:
            var["data"] = np.asarray(data)[inverse]
    return var


def _ray_order(group: dict, inherited):
    """The inverse permutation that puts this group's rays in storage order,
    from its fields (every field of a sweep shares one permutation)."""
    for var in group["variables"]:
        if var["kind"] == "field":
            rows = var["rows"]
            if rows is None:
                return None
            return var["dims"][0], np.argsort(np.asarray(rows), kind="stable")
    return inherited


def _dataset(group: dict, sizes: dict[str, int], options: dict, ray_order=None) -> xr.Dataset:
    data_vars: dict[str, xr.Variable] = {}
    coords: dict[str, xr.Variable] = {}
    own_dims = {name for name, _ in group["dims"]}
    for var in group["variables"]:
        var = _storage_order(var, ray_order)
        attrs = dict(var["attrs"])
        if var["kind"] == "field":
            pairs = _field_variables(var, attrs, options)
        else:
            pairs = [(var["name"], xr.Variable(tuple(var["dims"]), _plain_data(var, sizes), attrs))]
        for name, variable in pairs:
            if name in COORDINATE_NAMES or name in own_dims:
                coords[name] = variable
            else:
                data_vars[name] = variable
    ds = xr.Dataset(data_vars, coords=coords, attrs=dict(group["attrs"]))
    if options["decode"] or options["decode_times"]:
        ds = xr.decode_cf(
            ds,
            mask_and_scale=options["decode"],
            decode_times=options["decode_times"],
            decode_coords=True,
            decode_timedelta=False,
        )
    if options["decode"] and options["packed_attrs"] == "encoding":
        for variable in ds.variables.values():
            # Only scaled variables have attributes in units other than
            # their values' (a flag variable's flag_values are its own).
            if "scale_factor" not in variable.encoding and "add_offset" not in variable.encoding:
                continue
            for key in PACKED_ATTRS:
                if key in variable.attrs:
                    variable.encoding[key] = variable.attrs.pop(key)
    return ds


def _collect(
    group: dict, path: str, sizes: dict[str, int], options: dict, out: dict, ray_order=None
) -> None:
    sizes = {**sizes, **{name: int(length) for name, length in group["dims"]}}
    if options["storage_order"]:
        ray_order = _ray_order(group, ray_order)
    out[path] = _dataset(group, sizes, options, ray_order)
    for child in group["children"]:
        child_path = f"{path.rstrip('/')}/{child['name']}"
        _collect(child, child_path, sizes, options, out, ray_order)


def build_datatree(
    native: dict,
    *,
    decode: bool = True,
    decode_times: bool = True,
    mask_range_folded: bool = True,
    range_folded_variable: bool = False,
    packed_attrs: str = "encoding",
    storage_order: bool = False,
    warn: bool = True,
) -> xr.DataTree:
    """The DataTree of a native tree dictionary (see ``src/tree.rs``).

    ``storage_order`` puts every sweep's rays in the order the decoder read
    them (the file's order), whatever ``first_dim`` sorted them by; the ray
    dimension keeps its name. ``warn`` reports the view's warnings (ray times
    that do not increase) as ``RuntimeWarning``.
    """
    if packed_attrs not in ("encoding", "attrs"):
        raise ValueError(f'packed_attrs must be "encoding" or "attrs", not {packed_attrs!r}')
    options = {
        "decode": bool(decode),
        "decode_times": bool(decode_times),
        "mask_range_folded": bool(mask_range_folded),
        "range_folded_variable": bool(range_folded_variable),
        "packed_attrs": packed_attrs,
        "storage_order": bool(storage_order),
    }
    if warn:
        for message in native.get("warnings", ()):
            warnings.warn(message, RuntimeWarning, stacklevel=3)
    datasets: dict[str, xr.Dataset] = {}
    _collect(native["tree"], "/", {}, options, datasets)
    tree = xr.DataTree.from_dict(datasets)
    tree.encoding = {
        **tree.encoding,
        "source_format": native.get("source_format"),
        "format_name": native.get("format_name"),
        "pyart_names": native.get("pyart_names", {}),
        "flavor": native.get("flavor"),
        "first_dim": native.get("first_dim"),
    }
    return tree
