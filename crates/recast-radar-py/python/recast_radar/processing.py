"""Native radar products and images, shared with the recast-radar CLI."""
from __future__ import annotations

from dataclasses import dataclass
import json
import os
from typing import Any, Sequence

import numpy as np

from . import _native
from ._native import Volume


@dataclass(frozen=True)
class ProcessingResult:
    """A new volume and the inserted, skipped, and unavailable product report.

    Input volumes remain unchanged. Field names in ``inserted`` are the
    actual output names; product IDs can differ (for example LREF -> LLCREF).
    """

    volume: Volume
    report: dict[str, Any]


def products() -> list[dict[str, str]]:
    """List product IDs, descriptions, and scopes accepted by :func:`process`."""
    return json.loads(_native._products())


def process(
    source,
    products: str | Sequence[str],
    *,
    band: str | None = None,
    sweeps: Sequence[int] | None = None,
    overwrite: bool = False,
    threshold_dbz: float | None = None,
    height_m: float = 3000.0,
    freezing_level_m: float | None = None,
    minus20c_level_m: float | None = None,
    dealias_method: str = "region",
    previous=None,
    environment: dict[str, Any] | None = None,
    strict: bool = False,
    station: str | None = None,
) -> ProcessingResult:
    """Compute Rust products and retain the raw fields in a new Volume.

    ``source`` is a Volume, file path, or bytes. ``products`` is a product ID
    or sequence; use :func:`products` to list choices. Set ``band`` to ``s``,
    ``c``, or ``x`` for band-dependent retrievals. Unknown band is not assumed
    to be S band. ``sweeps`` limits sweep products; column products use the
    whole input volume and are placed on their base sweep.

    Heights are metres above radar altitude. Echo thresholds are dBZ.
    SHI/MESH/POSH require both hail-level heights, POH the freezing level.
    MESH uses Witt (1998) calibration. Dealiasing methods are ``region`` and
    ``pyart`` or ``volume``; missing Nyquist velocities are reported as unavailable.

    The volume engine accepts a previous Volume/path and an environment dict
    with RFC3339 ``valid_time`` and ``levels`` of [height_above_radar_m, u_mps,
    v_mps]. Solver diagnostics report whether these priors were accepted.
    ``VRADDH_CONFIDENCE`` stores branch confidence from 0 (no opinion) to 255.

    Inspect ``result.report`` for all outcomes. ``strict=True`` raises when
    a requested product lacks inputs. Existing fields are kept unless
    ``overwrite=True``. Computation releases the Python GIL.
    """
    volume = source if isinstance(source, Volume) else _native.read(source, station=station)
    if isinstance(source, Volume) and station is not None:
        raise ValueError("station applies to file inputs, not an existing Volume")
    names = [products] if isinstance(products, str) else list(products)
    options = dict(
        products=names, band=band, sweeps=None if sweeps is None else list(sweeps),
        overwrite=overwrite, height_m=height_m,
        freezing_level_m=freezing_level_m, minus20c_level_m=minus20c_level_m,
        dealias_method=dealias_method, environment=environment,
    )
    if threshold_dbz is not None:
        options["threshold_dbz"] = threshold_dbz
    previous_volume = None if previous is None else (previous if isinstance(previous, Volume) else _native.read(previous))
    output, report = _native._process(volume, json.dumps(options, allow_nan=False), previous=previous_volume)
    report = json.loads(report)
    if strict and report["unavailable"]:
        raise ValueError(f"unavailable products: {report['unavailable']}")
    return ProcessingResult(output, report)


def render(
    source,
    output: str | os.PathLike[str] | None = None,
    *,
    sweep: int | None = None,
    field: str | None = None,
    size: int = 1024,
    range_fraction: int = 94,
    dealias: bool = False,
    palette: str | os.PathLike[str] | None = None,
    station: str | None = None,
) -> np.ndarray:
    """Render one sweep to a uint8 RGBA array with NumPy-owned storage of shape (size, size, 4).

    Optionally write the same pixels as a PNG to ``output``. Uses the CLI's
    renderer, including GR .pal palettes and Level III rasters. ``sweep=None``
    selects the first sweep with ``field`` (default: reflectivity, then the
    first field). Size is 64..8192 pixels; range_fraction is 1..100 percent.
    An existing output PNG is replaced, as with the CLI's render command.
    """
    volume = source if isinstance(source, Volume) else _native.read(source, station=station)
    if isinstance(source, Volume) and station is not None:
        raise ValueError("station applies to file inputs, not an existing Volume")
    return _native._render(
        volume, output=output, sweep=sweep, field=field, size=size,
        range_fraction=range_fraction, dealias=dealias, palette=palette,
    )
