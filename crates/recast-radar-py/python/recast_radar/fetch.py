"""Downloads: the AWS NEXRAD archives, international feeds and GR2Analyst
polling servers.

Every function that saves files writes each one through a temporary file and
renames it into place, and skips a file that is already there with the
listed size (international frames, whose listings give no size: a non-empty
file under the part's name, which always stands for the same upstream file).
Downloads release the GIL.

Listing functions return lists of dictionaries; times are timezone-aware
``datetime`` objects in UTC.

Polling servers (GR2Analyst polling directories, such as the Iowa
Environmental Mesonet's https://mesonet-nexrad.agron.iastate.edu/level2/raw/)
are often run by volunteers: every polling function waits until ``interval``
seconds (default 1, minimum 0.25) have passed since the process's last
request to that server, and :func:`polling` fetches the newest ``count``
volumes (default 1, at most 100).
"""

from __future__ import annotations

import datetime as _dt
import os
import re
import threading
import time as _time
import urllib.parse
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Iterable

from . import _native

__all__ = [
    "available",
    "download",
    "intl",
    "intl_frames",
    "intl_providers",
    "intl_sites",
    "level2",
    "level2_files",
    "level3",
    "level3_files",
    "nexrad_sites",
    "polling",
    "polling_files",
    "polling_sites",
    "realtime",
]

#: Whether this build can download (the ``net`` feature).
available: bool = bool(getattr(_native, "NET", False))

#: Least ``interval`` the polling functions accept, in seconds (the CLI's
#: ``fetch polling --interval-ms`` has the same floor).
MIN_POLLING_INTERVAL = 0.25
#: Most volumes one :func:`polling` call fetches (the CLI's limit too).
MAX_POLLING_COUNT = 100
CHUNK_DOWNLOADS = 4

_SAFE_NAME = re.compile(r"^[A-Za-z0-9._@+-]+$")


def _require() -> None:
    if not available:
        raise _native.UnavailableError("this build of recast_radar has no network support")


def _datetime(text: str | None) -> _dt.datetime | None:
    if text is None:
        return None
    return _dt.datetime.fromisoformat(text.replace("Z", "+00:00"))


def _objects(rows: Iterable[dict]) -> list[dict]:
    out = []
    for row in rows:
        row = dict(row)
        row["time"] = _datetime(row.get("time"))
        out.append(row)
    return out


def _date_text(date: _dt.date | str) -> str:
    if isinstance(date, _dt.datetime):
        date = date.astimezone(_dt.timezone.utc).date() if date.tzinfo else date.date()
    if isinstance(date, _dt.date):
        return date.isoformat()
    return str(date)


def _utc(when: _dt.datetime) -> _dt.datetime:
    if when.tzinfo is None:
        return when.replace(tzinfo=_dt.timezone.utc)
    return when.astimezone(_dt.timezone.utc)


def _plain_name(name: str) -> str:
    if not name or name in (".", "..") or not _SAFE_NAME.match(name):
        raise ValueError(f"{name!r} is not a safe file name")
    return name


def _write_atomically(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(f".{path.name}.{os.getpid()}.part")
    try:
        temp.write_bytes(data)
        os.replace(temp, path)
    finally:
        if temp.exists():
            temp.unlink()


def download(url: str, dest: str | os.PathLike | None = None, *, name: str | None = None,
             size: int | None = None) -> bytes | Path:
    """Download ``url``. Without ``dest`` return the bytes; with ``dest`` (a
    directory) save the file as ``name`` (default: the URL's last segment)
    and return its path. A file already there with ``size`` bytes is kept.
    """
    _require()
    if dest is None:
        return _native._fetch_bytes(url)
    name = _plain_name(name or url.split("?")[0].rstrip("/").rsplit("/", 1)[-1])
    path = Path(dest) / name
    if size is not None and path.is_file() and path.stat().st_size == size:
        return path
    data = _native._fetch_bytes(url)
    if size is not None and len(data) != size:
        raise _native.FetchError(f"{url}: downloaded {len(data)} bytes, the listing says {size}")
    _write_atomically(path, data)
    return path


def _nearest(objects: list[dict], when: _dt.datetime | None, count: int) -> list[dict]:
    if when is not None:
        target = _utc(when)
        objects = sorted(
            objects,
            key=lambda o: abs((o["time"] - target).total_seconds()) if o["time"] else float("inf"),
        )
    chosen = objects[:count]
    return sorted(chosen, key=lambda o: o["key"])


# --- NEXRAD Level II ---------------------------------------------------------------


def level2_files(site: str, date: _dt.date | str | None = None, *, days_back: int = 1,
                 count: int = 10) -> list[dict]:
    """Level II volumes in the AWS archive (``unidata-nexrad-level2``).

    With ``date``: every volume of that UTC day. Without: the newest ``count``,
    looking back ``days_back`` days. Each entry is ``{"key", "name", "size",
    "time", "url"}``, oldest first.
    """
    _require()
    if date is not None:
        return _objects(_native._level2_objects(site, _date_text(date)))
    rows = _objects(_native._recent_level2_objects(site, days_back, count))
    return list(reversed(rows))


def level2(site: str, when: _dt.datetime | None = None, dest: str | os.PathLike = ".", *,
           count: int = 1) -> list[Path]:
    """Download the ``count`` Level II volumes nearest ``when`` (UTC; naive
    times are UTC), or the newest ones, into ``dest``. Returns their paths,
    oldest first.
    """
    objects = level2_files(site, _utc(when).date() if when else None, count=count)
    chosen = _nearest(objects, when, count) if when else objects[-count:]
    if not chosen:
        raise _native.FetchError(f"no Level II volumes for {site.upper()}")
    return [download(o["url"], dest, name=o["name"], size=o["size"]) for o in chosen]


def realtime(site: str, dest: str | os.PathLike | None = None, *, keep_chunks: bool = False):
    """The newest real-time Level II volume of ``site``, assembled from the
    ``unidata-nexrad-level2-chunks`` bucket into one Archive II file.

    Returns ``(data, info)`` without ``dest``, else ``(path, info)``. ``info``
    has ``site``, ``volume_id``, ``volume_time``, ``complete`` and ``chunks``.
    An incomplete volume (scan in progress) is saved with a ``.part`` suffix;
    it decodes, with the sweeps received so far.
    """
    _require()
    info = _native._realtime_volume(site)
    info["volume_time"] = _datetime(info["volume_time"])
    chunks = info["chunks"]
    with ThreadPoolExecutor(max_workers=min(CHUNK_DOWNLOADS, max(len(chunks), 1))) as pool:
        parts = list(pool.map(lambda chunk: _native._fetch_bytes(chunk["url"]), chunks))
    for chunk, data in zip(chunks, parts):
        if len(data) != chunk["size"]:
            raise _native.FetchError(
                f"chunk {chunk['key']}: {len(data)} bytes, the listing says {chunk['size']}"
            )
    assembled = b"".join(parts)
    if dest is None:
        return assembled, info
    stamp = info["volume_time"].strftime("%Y%m%d_%H%M%S")
    name = f"{info['site']}{stamp}_V06{'' if info['complete'] else '.part'}"
    path = Path(dest) / name
    _write_atomically(path, assembled)
    if keep_chunks:
        chunk_dir = Path(dest) / f"{info['site']}_{info['volume_id']:03d}_{stamp}"
        for chunk, data in zip(chunks, parts):
            _write_atomically(chunk_dir / _plain_name(chunk["name"]), data)
    return path, info


# --- NEXRAD Level III --------------------------------------------------------------


def level3_files(site: str, product: str, date: _dt.date | str | None = None, *,
                 days_back: int = 1, count: int = 10) -> list[dict]:
    """Level III products in the AWS archive, as :func:`level2_files`."""
    _require()
    if date is not None:
        return _objects(_native._level3_objects(site, product, _date_text(date)))
    rows = _objects(_native._recent_level3_objects(site, product, days_back, count))
    return list(reversed(rows))


def level3(site: str, product: str, when: _dt.datetime | None = None,
           dest: str | os.PathLike = ".", *, count: int = 1) -> list[Path]:
    """Download Level III products (for example ``product="N0B"``), as
    :func:`level2`."""
    objects = level3_files(site, product, _utc(when).date() if when else None, count=count)
    chosen = _nearest(objects, when, count) if when else objects[-count:]
    if not chosen:
        raise _native.FetchError(f"no {product.upper()} products for {site.upper()}")
    return [download(o["url"], dest, name=o["name"], size=o["size"]) for o in chosen]


def nexrad_sites() -> list[dict]:
    """The built-in NEXRAD site table: ``{"id", "name", "latitude", "longitude"}``."""
    _require()
    return list(_native._nexrad_sites())


# --- International feeds -----------------------------------------------------------


def intl_providers() -> list[dict]:
    """International providers: ``{"id", "name", "country", "sites",
    "recent", "archive"}``."""
    _require()
    return list(_native._intl_providers())


def intl_sites(provider: str, *, online: bool = False) -> list[dict]:
    """A provider's sites (``online=True`` asks the provider's catalog)."""
    _require()
    return list(_native._intl_sites(provider, online))


def _frames(rows: Iterable[dict]) -> list[dict]:
    out = []
    for row in rows:
        row = dict(row)
        row["time"] = _datetime(row.get("time"))
        out.append(row)
    return out


def intl_frames(provider: str, site: str, *, count: int = 1,
                date: _dt.date | str | None = None,
                when: _dt.datetime | None = None) -> list[dict]:
    """Frames of an international site, oldest first. Nothing is downloaded.

    By default the newest ``count``. With ``date`` (providers whose
    :func:`intl_providers` entry has ``"archive": True``): every archived
    frame of that UTC day. With ``when`` (UTC; naive times are UTC): the
    ``count`` archived frames nearest it, searched an hour either side (ten
    minutes per frame when ``count`` is larger).

    Each frame is ``{"identity", "time", "merge", "urls", "names"}``:
    ``time`` is the scan time read from the identity (``None`` when it has
    none), ``names`` the file names :func:`intl` saves the parts under, and a
    frame with ``merge`` true is split into parts of one scan.
    """
    _require()
    if date is not None and when is not None:
        raise ValueError("pass date or when, not both")
    if when is not None:
        stamp = _utc(when).isoformat().replace("+00:00", "Z")
        return _frames(_native._intl_archive_nearest(provider, site, stamp, count))
    if date is not None:
        return _frames(_native._intl_archive_day(provider, site, _date_text(date)))
    return _frames(_native._intl_frames(provider, site, count))


def intl(provider: str, site: str, dest: str | os.PathLike | None = None, *, count: int = 1,
         when: _dt.datetime | None = None, date: _dt.date | str | None = None):
    """Download frames of an international site: the newest ``count``, the
    ``count`` archived frames nearest ``when``, or the first ``count`` of the
    UTC day ``date`` (see :func:`intl_frames`).

    With ``dest``: save every part and return, per frame, the list of paths;
    a non-empty file already there under the part's name is kept. Without:
    decode each frame, merging split frames (per-sweep or per-quantity
    files), and return :class:`recast_radar.Volume` objects. JMA tars hold
    every station; this reads ``site``'s.
    """
    frames = intl_frames(provider, site, count=count, date=date, when=when)
    if date is not None:
        frames = frames[:count]
    results = []
    for frame in frames:
        if dest is not None:
            paths = []
            for url, name in zip(frame["urls"], frame["names"]):
                path = Path(dest) / _plain_name(name)
                if path.is_file() and path.stat().st_size > 0:
                    paths.append(path)
                    continue
                paths.append(download(url, dest, name=name))
            results.append(paths)
            continue
        station = site if provider.lower() == "jma" else None
        parts = [_native.read(_native._fetch_bytes(url), station=station) for url in frame["urls"]]
        results.append(parts[0] if len(parts) == 1 else _native.merge(parts))
    return results


# --- GR2Analyst polling servers ----------------------------------------------------


class _Pace:
    """At most one request every ``interval`` seconds to one server, shared
    by every call in the process (``polling_sites``, ``polling_files`` and
    ``polling`` one after another still wait for each other)."""

    _lock = threading.Lock()
    _last: dict[str, float] = {}

    def __init__(self, server: str, interval: float) -> None:
        if interval < MIN_POLLING_INTERVAL:
            raise ValueError(f"interval must be at least {MIN_POLLING_INTERVAL} s")
        parts = urllib.parse.urlsplit(server)
        self.key = parts.netloc.lower() or server
        self.interval = interval

    def wait(self) -> None:
        with self._lock:
            last = self._last.get(self.key)
            now = _time.monotonic()
            delay = 0.0 if last is None else self.interval - (now - last)
            if delay > 0:
                _time.sleep(delay)
            self._last[self.key] = _time.monotonic()


_NOT_VOLUMES = (".tmp", ".part", ".json", ".txt", ".cfg", ".list", ".html")


def _is_volume_name(name: str) -> bool:
    """Whether a listed name can be a volume: not hidden, not a file still
    being written and not a text or state file (as ``recast-radar-data``'s
    ``polling::is_volume_name``)."""
    return not name.startswith(".") and not name.lower().endswith(_NOT_VOLUMES)


def _check_site(site: str) -> str:
    if not re.fullmatch(r"[A-Za-z0-9_-]+", site):
        raise ValueError(f"site {site!r} must be letters, digits, '_' or '-'")
    return site


def polling_sites(server: str, *, interval: float = 1.0) -> list[str]:
    """The sites a GR2Analyst polling server lists in its ``config.cfg``.

    Waits until ``interval`` seconds have passed since this process's last
    request to the server."""
    _require()
    _Pace(server, interval).wait()
    return list(_native._polling_sites(server))


def polling_files(site: str, server: str, *, interval: float = 1.0) -> list[dict]:
    """A polling site's ``dir.list``: ``{"name", "size", "url"}``, oldest first.

    Waits until ``interval`` seconds have passed since this process's last
    request to the server."""
    _require()
    _check_site(site)
    _Pace(server, interval).wait()
    return [
        {"name": name, "size": size, "url": _native._polling_file_url(server, site, name)}
        for size, name in _native._polling_dir_list(server, site)
    ]


def polling(site: str, dest: str | os.PathLike = ".", *, server: str, count: int = 1,
            interval: float = 1.0) -> list[Path]:
    """Download the newest ``count`` volumes of a polling site of ``server``
    into ``dest/<site>/``, at most one request every ``interval`` seconds.

    ``count`` is 1 to :data:`MAX_POLLING_COUNT`. Listed files that are not
    volumes (one still being written, such as ``.tmp`` or ``.part``, or a
    state or text file) are skipped. Polling servers rewrite a volume while
    it grows, so the listed size is a hint: a file whose size differs from
    ``dir.list`` is still kept.
    """
    _require()
    _check_site(site)
    if not 1 <= count <= MAX_POLLING_COUNT:
        raise ValueError(f"count must be 1 to {MAX_POLLING_COUNT}")
    pace = _Pace(server, interval)
    pace.wait()
    entries = [entry for entry in _native._polling_dir_list(server, site) if _is_volume_name(entry[1])]
    if not entries:
        raise _native.FetchError(f"{server.rstrip('/')}/{site}/dir.list lists no volume")
    folder = Path(dest) / site
    paths = []
    for size, name in entries[-count:]:
        path = folder / _plain_name(name)
        if path.is_file() and path.stat().st_size == size:
            paths.append(path)
            continue
        pace.wait()
        paths.append(download(_native._polling_file_url(server, site, name), folder, name=name))
    return paths
