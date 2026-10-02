"""Weather radar files as xarray DataTrees and Py-ART radars.

``recast_radar`` reads NEXRAD Level II and Level III, ODIM_H5, CfRadial 1,
DORADE and JMA radar GRIB2 files with the pure-Rust decoders of
recast-radar-tools, and presents them the way xradar does: an
``xarray.DataTree`` following WMO FM301 (CfRadial 2).

>>> import recast_radar
>>> tree = recast_radar.open("KTLX20240315_000217_V06")      # doctest: +SKIP
>>> tree["sweep_0"]["DBZH"]                                 # doctest: +SKIP
>>> radar = recast_radar.to_pyart("KTLX20240315_000217_V06")  # doctest: +SKIP

Main entry points:

``open(source, ...)``
    Decode a file (path or bytes) into a DataTree. Field buffers move from the
    decoder into NumPy without a copy.
``read(source)`` / ``read_all(source)``
    Decode into :class:`Volume` objects kept in Rust, for writing and repeated
    conversion.
``to_pyart(source)``
    A ``pyart.core.Radar`` from a path, bytes, a :class:`Volume` or a DataTree.
``dump(source)``
    Every decoded value as a dict, including Level II metadata messages and
    Level III graphic and tabular products (storm tables, symbols).
``write(volume, path, format)``, ``to_bytes``, ``convert``, ``publish``
    Radar file writers (Level II, CfRadial 1, ODIM_H5, FM301) and the
    GR2Analyst polling-directory publisher (see ``writers()``). What a
    writer leaves out or codes more coarsely than the source comes as
    :class:`WriteWarning`.
``recast_radar.fetch``
    Downloads from the AWS NEXRAD archives, international feeds and
    GR2Analyst polling servers.

User guide: ``docs/guide/python.md`` in the recast-radar-tools repository.
"""

from __future__ import annotations

import os
import warnings
from typing import Any, Mapping, Sequence, Union

from . import _native
from ._native import (
    DecodeError,
    FetchError,
    UnavailableError,
    UnrepresentableError,
    Volume,
    merge,
    pyart_field_name,
    read,
    read_all,
    sniff,
    split_scan_cycles,
)
from ._tree import build_datatree
from .mapping import cross_section, grid, rhi_panel
from .processing import ProcessingResult, process, products, render

__version__: str = _native.__version__

Source = Union[str, "os.PathLike[str]", bytes, bytearray, memoryview]


class WriteWarning(UserWarning):
    """What a writer left out of a volume (a field or sweep the format
    cannot hold) or changed (a coding coarser than the source, radials
    reordered, a missing Nyquist velocity). Pass ``strict=True`` to refuse a
    write that would leave something out."""


def _warn(report: tuple[list[str], list[str]], stacklevel: int = 3) -> None:
    left_out, notes = report
    for line in left_out:
        warnings.warn(f"left out: {line}", WriteWarning, stacklevel=stacklevel)
    for line in notes:
        warnings.warn(line, WriteWarning, stacklevel=stacklevel)


def _write_options(
    *,
    quantization: str,
    nyquist_velocity: float | None,
    unambiguous_range: float | None,
    drop_negative_range_gates: bool,
    sweeps: Sequence[int] | None,
    sweeps_in_time_order: bool,
    sweeps_by_elevation: bool,
    fields: Sequence[str] | None,
    field_map: Mapping[str, str] | None,
    position: tuple[float, float, float] | None,
    strict: bool,
) -> dict[str, Any]:
    return {
        "quantization": quantization,
        "nyquist_velocity": nyquist_velocity,
        "unambiguous_range": unambiguous_range,
        "drop_negative_range_gates": drop_negative_range_gates,
        "sweeps": None if sweeps is None else [int(index) for index in sweeps],
        "sweeps_in_time_order": sweeps_in_time_order,
        "sweeps_by_elevation": sweeps_by_elevation,
        "fields": None if fields is None else [str(name) for name in fields],
        "field_map": None
        if field_map is None
        else [(str(field), str(moment)) for field, moment in dict(field_map).items()],
        "position": None if position is None else tuple(float(value) for value in position),
        "strict": strict,
    }


_WRITE_OPTIONS_DOC = """
    Level II options (``docs/level2/writer.md``): ``quantization`` is
    ``"standard"`` (the default: NOAA's codings where they hold every value,
    which GR2Analyst and every Level II reader expect), ``"compatible"``
    (NEXRAD's word sizes, which xradar 0.12 reads) or ``"precise"`` (never
    coarser than the source: 16-bit moments and PHI codes past 1023, which
    readers that keep only NEXRAD's bits misread); no policy clips a value.
    ``field_map`` writes fields as the moments named, ahead of the field the
    writer would pick, such as ``{"UPHIDP": "PHI"}`` (moments ``REF``,
    ``VEL``, ``SW``, ``ZDR``, ``PHI``, ``RHO``, ``CFP``).
    ``nyquist_velocity`` (m/s) and ``unambiguous_range`` (m) are the radar's
    own values for radials whose source has none (JMA);
    ``drop_negative_range_gates`` leaves out gates centred before the radar
    (Message 1 volumes).

    Every format: ``sweeps`` keeps only those sweeps (0-based indices), in
    that order (Level II holds at most 32); ``sweeps_in_time_order`` puts
    them in the order their first rays were collected;
    ``sweeps_by_elevation`` in order of elevation angle, lowest first;
    ``fields`` keeps only the fields with those names (such as
    ``["DBZH", "VRADH", "UPHIDP"]``; a name no sweep has is refused);
    ``position`` is the
    site position ``(latitude_deg, longitude_deg, height_m)`` to write (a
    Message 1 volume has none). ``strict`` refuses, writing nothing, a write
    that would leave out a field or sweep; otherwise what is left out and
    the writer's notes come as :class:`WriteWarning`.
"""

__all__ = [
    "cross_section",
    "grid",
    "rhi_panel",
    "ProcessingResult",
    "process",
    "products",
    "render",
    "DecodeError",
    "FetchError",
    "UnavailableError",
    "UnrepresentableError",
    "Volume",
    "WriteWarning",
    "__version__",
    "convert",
    "dump",
    "fetch",
    "merge",
    "open",
    "open_datatree",
    "publish",
    "publisher_available",
    "pyart_field_name",
    "read",
    "read_all",
    "sniff",
    "split_scan_cycles",
    "to_bytes",
    "to_datatree",
    "to_pyart",
    "write",
    "write_chunks",
    "writers",
]


def open(  # noqa: A001 - the package's main entry point, like xarray.open_dataset
    source: Source,
    *,
    first_dim: str = "auto",
    decode: bool = True,
    decode_times: bool = True,
    mask_range_folded: bool = True,
    range_folded_variable: bool = False,
    packed_attrs: str = "encoding",
    flavor: str = "xradar",
    passthrough: str = "flavor",
    station: str | None = None,
    volume: int = 0,
):
    """Decode a radar file into an ``xarray.DataTree`` following FM301.

    Parameters
    ----------
    source
        A path (``str`` or ``os.PathLike``) or the file's bytes. The format is
        detected from the contents; gzip and single-file ZIP wrappers are
        removed.
    first_dim
        ``"auto"`` (xradar's default): rays sorted by azimuth (by elevation for
        RHI sweeps) under dimension ``azimuth``/``elevation``. ``"time"``: rays
        in acquisition order under dimension ``time``.
    decode
        CF decoding of packed fields (``scale_factor``, ``add_offset``,
        ``_FillValue``), as ``xr.decode_cf(mask_and_scale=True)``. With
        ``False`` fields keep their packed integers and every attribute.
    decode_times
        Decode ``time`` to ``datetime64``.
    mask_range_folded
        With ``decode``, range-folded gates (NEXRAD raw 1) read as NaN, as
        Py-ART masks them.
    range_folded_variable
        Add ``<FIELD>_flags`` (``uint8``, 1 where the gate is range folded)
        beside each field that has a range-folded code, linked through the
        field's ``ancillary_variables``.
    packed_attrs
        ``"encoding"``: after decoding, ``_Undetect``, ``valid_range``,
        ``valid_min``, ``valid_max`` and the ``flag_*`` attributes of packed
        fields (numbers in packed units) move into ``encoding``. ``"attrs"``:
        they stay in ``attrs``, as xradar leaves them.
    flavor
        ``"xradar"``: xradar 0.12's names and attribute spellings.
        ``"wmo"``: the FM301-2022 text (needs ``first_dim="time"``).
    passthrough
        ``"all"`` also writes the source attributes xradar drops (CfRadial
        global attributes, ODIM ``how`` attributes, DORADE VOLD text).
    station
        JMA tars hold one volume per station: the station to read (JMA id or
        station number). The first station by default.
    volume
        For inputs that hold several volumes (mobile-radar ZIP archives), the
        index of the one to read.

    Values are read lazily: ``tree.load()`` decodes everything. Level III
    products open as a one-sweep volume of their data array.
    """
    native = _native._open_tree(
        source,
        flavor=flavor,
        first_dim=first_dim,
        passthrough=passthrough,
        station=station,
        volume=volume,
    )
    return build_datatree(
        native,
        decode=decode,
        decode_times=decode_times,
        mask_range_folded=mask_range_folded,
        range_folded_variable=range_folded_variable,
        packed_attrs=packed_attrs,
    )


#: ``open`` under the name xarray and xradar use for DataTree readers.
open_datatree = open


def to_datatree(
    volume: Volume,
    *,
    first_dim: str = "auto",
    decode: bool = True,
    decode_times: bool = True,
    mask_range_folded: bool = True,
    range_folded_variable: bool = False,
    packed_attrs: str = "encoding",
    flavor: str = "xradar",
    passthrough: str = "flavor",
):
    """The DataTree of a :class:`Volume` (a copy: the volume stays usable).

    The options are those of :func:`open`.
    """
    native = volume._tree(flavor=flavor, first_dim=first_dim, passthrough=passthrough)
    return build_datatree(
        native,
        decode=decode,
        decode_times=decode_times,
        mask_range_folded=mask_range_folded,
        range_folded_variable=range_folded_variable,
        packed_attrs=packed_attrs,
    )


def to_pyart(source: Any, *, field_names: Any = "config", station: str | None = None, volume: int = 0):
    """A ``pyart.core.Radar`` from a path, bytes, a :class:`Volume` or a DataTree.

    ``field_names`` is ``"config"`` (Py-ART's default names, which its
    algorithms look for: ``reflectivity``, ``velocity``, ...), ``"reader"``
    (the names Py-ART's own reader for the source format gives), ``"fm301"``
    (the FM301 names unchanged) or a dict from FM301 name to Py-ART name.
    See :mod:`recast_radar._pyart` for the conventions.
    """
    from ._pyart import to_pyart as convert

    return convert(source, field_names=field_names, station=station, volume=volume)


def dump(
    source: Source,
    *,
    data: bool = False,
    rays: bool = False,
    station: str | None = None,
    all_stations: bool = False,
) -> dict[str, Any]:
    """Every decoded value of a file as a dict: what ``recast-radar dump
    --json`` prints.

    Unlike :func:`open`, this also reads what is not a radar volume: Level
    III graphic and tabular products (storm tracking, mesocyclone, hail and
    TVS products: every display packet under ``level3["symbology"]`` and
    ``level3["graphic"]``, the text pages under ``level3["tabular"]``) and
    Level II real-time chunks (``records``). For NEXRAD Level II it gives the
    metadata messages under ``volumes[i]["format_metadata"]["nexrad"]``
    (also :attr:`Volume.format_metadata`). ``data`` adds every gate value
    (and the bins of Level III data packets); ``rays`` every ray's values.
    The layout is described in ``docs/guide/cli.md`` (``dump``).
    """
    import json

    text = _native._dump(source, data=data, rays=rays, station=station, all_stations=all_stations)
    return json.loads(text)


def writers() -> dict[str, bool]:
    """Each output format and whether this build can write it."""
    return dict(_native.writers())


def publisher_available() -> bool:
    """Whether this build can publish to a GR2Analyst polling directory."""
    return _native.publisher_available()


def write(
    volume: Volume,
    path: str | os.PathLike[str],
    format: str,  # noqa: A002 - the natural name
    *,
    compression: str = "bzip2",
    gzip: bool = False,
    site: str | None = None,
    overwrite: bool = False,
    quantization: str = "standard",
    nyquist_velocity: float | None = None,
    unambiguous_range: float | None = None,
    drop_negative_range_gates: bool = False,
    sweeps: Sequence[int] | None = None,
    sweeps_in_time_order: bool = False,
    sweeps_by_elevation: bool = False,
    fields: Sequence[str] | None = None,
    field_map: Mapping[str, str] | None = None,
    position: tuple[float, float, float] | None = None,
    strict: bool = False,
):
    """Write ``volume`` to ``path`` as ``format``; returns the path.

    ``format`` is ``"level2"`` (NEXRAD Archive II), ``"cfradial1"``,
    ``"odim"`` (ODIM_H5 PVOL) or ``"fm301"`` (CfRadial 2 in netCDF-4).
    ``compression`` (``"bzip2"`` or ``"none"``) packs Level II records,
    ``gzip`` wraps any format in gzip, and ``site`` replaces the radar
    identifier (the 4-character ICAO for Level II). The file is written
    through a temporary file and renamed into place.
    {options}
    Raises :class:`UnavailableError` when this build has no writer for the
    format (see :func:`writers`), and :class:`UnrepresentableError` when the
    format cannot hold the volume.
    """
    written, report = _native._write(
        volume,
        os.fspath(path),
        format,
        compression=compression,
        gzip=gzip,
        site=site,
        overwrite=overwrite,
        **_write_options(
            quantization=quantization,
            nyquist_velocity=nyquist_velocity,
            unambiguous_range=unambiguous_range,
            drop_negative_range_gates=drop_negative_range_gates,
            sweeps=sweeps,
            sweeps_in_time_order=sweeps_in_time_order,
            sweeps_by_elevation=sweeps_by_elevation,
            fields=fields,
            field_map=field_map,
            position=position,
            strict=strict,
        ),
    )
    _warn(report)
    return written


write.__doc__ = (write.__doc__ or "").replace("{options}", _WRITE_OPTIONS_DOC)


def write_chunks(
    volume: Volume,
    dest: str | os.PathLike[str] | None = None,
    *,
    site: str | None = None,
    overwrite: bool = False,
    quantization: str = "standard",
    nyquist_velocity: float | None = None,
    unambiguous_range: float | None = None,
    drop_negative_range_gates: bool = False,
    sweeps: Sequence[int] | None = None,
    sweeps_in_time_order: bool = False,
    sweeps_by_elevation: bool = False,
    fields: Sequence[str] | None = None,
    field_map: Mapping[str, str] | None = None,
    position: tuple[float, float, float] | None = None,
    strict: bool = False,
):
    """``volume`` as NEXRAD Level II real-time chunks: the ``S``, ``I`` and
    ``E`` files of the ``unidata-nexrad-level2-chunks`` bucket. The start
    chunk holds the volume header and the metadata record, every later chunk
    one bzip2 LDM record of radials; concatenated, they are one Archive II
    file.

    Without ``dest``, returns ``[{"key", "kind", "number", "data"}]`` in
    order, where ``key`` is the chunk's object key in the bucket
    (``SITE/VOLUME/YYYYMMDD-HHMMSS-NNN-K``). With ``dest``, writes each chunk
    to ``dest/key`` (through a temporary file) and returns the paths; an
    existing chunk file is replaced only with ``overwrite``. ``site``
    replaces the radar identifier. The other options are :func:`write`'s.

    Raises :class:`UnavailableError` when this build has no Level II writer
    with real-time chunk output.
    """
    chunks, report = _native._write_chunks(
        volume,
        site=site,
        **_write_options(
            quantization=quantization,
            nyquist_velocity=nyquist_velocity,
            unambiguous_range=unambiguous_range,
            drop_negative_range_gates=drop_negative_range_gates,
            sweeps=sweeps,
            sweeps_in_time_order=sweeps_in_time_order,
            sweeps_by_elevation=sweeps_by_elevation,
            fields=fields,
            field_map=field_map,
            position=position,
            strict=strict,
        ),
    )
    _warn(report)
    rows = [
        {"key": key, "kind": kind, "number": number, "data": data}
        for key, kind, number, data in chunks
    ]
    if dest is None:
        return rows
    from pathlib import Path

    from .fetch import _write_atomically

    paths = []
    for row in rows:
        parts = row["key"].split("/")
        if any(part in ("", ".", "..") for part in parts):
            raise ValueError(f"chunk key {row['key']!r} would leave {dest!s}")
        paths.append(Path(dest).joinpath(*parts))
    if not overwrite:
        for path in paths:
            if path.exists():
                raise FileExistsError(f"{path} exists (pass overwrite=True to replace it)")
    for row, path in zip(rows, paths):
        _write_atomically(path, row["data"])
    return paths


def to_bytes(
    volume: Volume,
    format: str,  # noqa: A002
    *,
    compression: str = "bzip2",
    gzip: bool = False,
    site: str | None = None,
    quantization: str = "standard",
    nyquist_velocity: float | None = None,
    unambiguous_range: float | None = None,
    drop_negative_range_gates: bool = False,
    sweeps: Sequence[int] | None = None,
    sweeps_in_time_order: bool = False,
    sweeps_by_elevation: bool = False,
    fields: Sequence[str] | None = None,
    field_map: Mapping[str, str] | None = None,
    position: tuple[float, float, float] | None = None,
    strict: bool = False,
) -> bytes:
    """``volume`` encoded as ``format``, in memory. See :func:`write`."""
    data, report = _native._to_bytes(
        volume,
        format,
        compression=compression,
        gzip=gzip,
        site=site,
        **_write_options(
            quantization=quantization,
            nyquist_velocity=nyquist_velocity,
            unambiguous_range=unambiguous_range,
            drop_negative_range_gates=drop_negative_range_gates,
            sweeps=sweeps,
            sweeps_in_time_order=sweeps_in_time_order,
            sweeps_by_elevation=sweeps_by_elevation,
            fields=fields,
            field_map=field_map,
            position=position,
            strict=strict,
        ),
    )
    _warn(report)
    return data


def convert(
    source: Source,
    path: str | os.PathLike[str],
    format: str,  # noqa: A002
    *,
    compression: str = "bzip2",
    gzip: bool = False,
    site: str | None = None,
    overwrite: bool = False,
    station: str | None = None,
    volume: int = 0,
    **options: Any,
):
    """Read ``source`` and write it to ``path`` as ``format``; ``options``
    are :func:`write`'s (``quantization``, ``sweeps``, ``position``, ...).

    Checks that the writer exists before decoding anything.
    """
    _native._require_writer(format)
    loaded = read(source, station=station, volume=volume)
    return write(
        loaded,
        path,
        format,
        compression=compression,
        gzip=gzip,
        site=site,
        overwrite=overwrite,
        **options,
    )


def publish(
    volume: Volume,
    root: str | os.PathLike[str],
    *,
    site: str | None = None,
    keep: int = 30,
    compression: str = "bzip2",
    update_site_config: bool = True,
    quantization: str = "standard",
    nyquist_velocity: float | None = None,
    unambiguous_range: float | None = None,
    drop_negative_range_gates: bool = False,
    sweeps: Sequence[int] | None = None,
    sweeps_in_time_order: bool = False,
    sweeps_by_elevation: bool = False,
    fields: Sequence[str] | None = None,
    field_map: Mapping[str, str] | None = None,
    position: tuple[float, float, float] | None = None,
    strict: bool = False,
) -> dict:
    """Place ``volume`` in a GR2Analyst polling directory (the GRLevelX
    polling conventions).

    Writes the volume as Level II into ``<root>/<SITE>/`` under the NWS
    archive's name (``SITEYYYYMMDD_HHMMSS_V06.ar2v``), updates the site's
    ``dir.list`` (``<size> <name>`` lines, oldest first), keeps the newest
    ``keep`` volumes and, with ``update_site_config``, adds the site to
    ``config.cfg`` and ``grlevel2.cfg``. The other options are
    :func:`write`'s. Returns ``{"site", "path", "dir_list", "removed",
    "left_out", "notes"}``.

    Raises :class:`UnavailableError` when this build has no publisher.
    """
    result = _native._publish(
        volume,
        os.fspath(root),
        site=site,
        keep=keep,
        compression=compression,
        update_site_config=update_site_config,
        **_write_options(
            quantization=quantization,
            nyquist_velocity=nyquist_velocity,
            unambiguous_range=unambiguous_range,
            drop_negative_range_gates=drop_negative_range_gates,
            sweeps=sweeps,
            sweeps_in_time_order=sweeps_in_time_order,
            sweeps_by_elevation=sweeps_by_elevation,
            fields=fields,
            field_map=field_map,
            position=position,
            strict=strict,
        ),
    )
    _warn((result["left_out"], result["notes"]))
    return result


from . import fetch  # noqa: E402 - needs the names above
