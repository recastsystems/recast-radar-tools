"""Coordinate-aware cross sections, RHI panels, and multi-radar Cartesian grids."""
from __future__ import annotations
import json
import os
from typing import Sequence

import numpy as np
import xarray as xr
from . import _native
from ._native import Volume


def _volume(source):
    return source if isinstance(source, Volume) else _native.read(source)


def cross_section(
    source, *, start_km: tuple[float, float], end_km: tuple[float, float],
    field: str = "DBZH", width: int = 512, height: int = 256,
    top_m: float = 20000.0, smooth: bool = True,
) -> xr.DataArray:
    """Reconstruct a section between (east, north) endpoints in km from the radar.

    Coordinates are height above the radar (m, descending) and distance along
    the path (m, ascending). Both endpoints are sampled. Interpolation uses
    the native field-family policy, including correlation and velocity guards.
    Set smooth=False to leave native gaps and samples unblended horizontally.
    """
    options = dict(start_km=start_km, end_km=end_km, field=field, width=width,
                   height=height, top_m=top_m, smooth=smooth)
    return _section(source, options)


def rhi_panel(source, *, sweep: int, field: str = "DBZH", width: int = 512,
              height: int = 256, top_m: float = 20000.0,
              max_range_m: float = 200000.0) -> xr.DataArray:
    """Sample a native RHI sweep on ground-range and height coordinates in metres."""
    return _section(source, dict(rhi_sweep=sweep, field=field, width=width,
        height=height, top_m=top_m, max_range_m=max_range_m))


def _section(source, options) -> xr.DataArray:
    section = _native._section(_volume(source), json.dumps(options, allow_nan=False))
    attrs = {"height_reference": "radar_altitude"}
    if section["units"] is not None:
        attrs["units"] = section["units"]
    for key in ("start_km", "end_km"):
        if key in options:
            attrs[key] = list(options[key])
    return xr.DataArray(section["values"], name=section["field"],
        dims=("height", "distance"), attrs=attrs,
        coords={"height": ("height", section["height_m"], {"units": "m", "positive": "up", "long_name": "height above radar"}),
                "distance": ("distance", section["distance_m"], {"units": "m", "long_name": "distance along section"})})


def grid(sources, *, fields: Sequence[str], shape: tuple[int, int, int],
         limits_m: Sequence[tuple[float, float]],
         origin: tuple[float, float, float] | None = None,
         weighting: str = "barnes2", radius_m: float | None = None) -> xr.Dataset:
    """Grid one or more volumes with Rust's gate-to-grid mapper.

    Axis order is (z, y, x). limits_m contains inclusive limits for those axes.
    x is east and y north. origin is (latitude_deg, longitude_deg, altitude_m
    MSL); omitted uses the first radar. Weighting is barnes2, barnes, cressman,
    or nearest. radius_m sets a fixed radius of influence; None uses the native
    distance-beam default. ROI is returned in metres beside the requested fields.
    File inputs use the first volume; pass read_all() results for archives.
    """
    if isinstance(sources, (Volume, str, os.PathLike, bytes, bytearray, memoryview)):
        sources = [sources]
    if isinstance(fields, str):
        fields = [fields]
    volumes = [_volume(source) for source in sources]
    options = dict(fields=list(fields), shape=shape, limits_m=limits_m,
                   origin=origin, weighting=weighting, radius_m=radius_m)
    result = _native._grid(volumes, json.dumps(options, allow_nan=False))
    lat, lon, alt = result["origin"]
    ds = xr.Dataset(coords={
        "x": ("x", result["x_m"], {"units": "m", "axis": "X", "long_name": "east of origin"}),
        "y": ("y", result["y_m"], {"units": "m", "axis": "Y", "long_name": "north of origin"}),
        "z": ("z", result["z_m"], {"units": "m", "axis": "Z", "positive": "up", "long_name": "height above origin"}),
    }, attrs={"origin_latitude": lat, "origin_longitude": lon, "origin_altitude_m": alt})
    for name, values in result["fields"].items():
        attrs = {"units": result["units"][name]} if result["units"][name] is not None else {}
        ds[name] = (("z", "y", "x"), values, attrs)
    ds["ROI"] = (("z", "y", "x"), result["roi_m"], {"units": "m", "long_name": "radius of influence"})
    if np.isfinite([lat,lon]).all():
        ds["crs"] = xr.DataArray(0,attrs={"grid_mapping_name": "azimuthal_equidistant",
            "latitude_of_projection_origin": lat, "longitude_of_projection_origin": lon,
            "earth_radius": 6370997.0})
        for name in [*result["fields"],"ROI"]:
            ds[name].attrs["grid_mapping"] = "crs"
    return ds
