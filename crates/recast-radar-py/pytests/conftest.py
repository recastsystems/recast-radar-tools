"""Shared fixtures: real radar files from the repository's testdata manifests.

Files are resolved as ``recast-radar-testdata`` resolves them: the committed
copy under ``testdata/`` when the manifest entry has ``committed``, otherwise
the download cache (``$RECAST_RADAR_TESTDATA``, else
``%LOCALAPPDATA%/recast-radar-tools/testdata`` on Windows, else
``$XDG_CACHE_HOME`` or ``~/.cache``), downloading from the manifest URLs when
the file is missing. Every file is checked against its SHA-256. With
``RECAST_RADAR_TESTDATA_OFFLINE`` set, a test whose file is not available is
skipped instead of downloading.

Network tests (``@pytest.mark.network``) run only with
``RECAST_RADAR_NETWORK_TESTS=1``.
"""

from __future__ import annotations

import gzip
import hashlib
import os
import shutil
import urllib.request
from pathlib import Path

import pytest

try:
    import tomllib
except ModuleNotFoundError:  # Python 3.10: the tomli backport has the same API
    tomllib = pytest.importorskip("tomli", reason="the testdata manifests need tomllib (3.11+) or tomli")

ROOT = Path(__file__).resolve().parents[3]
TESTDATA = ROOT / "testdata"


def _manifest() -> dict[str, dict]:
    entries: dict[str, dict] = {}
    paths = sorted(TESTDATA.glob("*/manifest.toml"))
    top = TESTDATA / "manifest.toml"
    if top.is_file():
        paths.insert(0, top)
    for path in paths:
        with open(path, "rb") as f:
            for entry in tomllib.load(f).get("file", []):
                entries[entry["id"]] = entry
    return entries


MANIFEST = _manifest()


def cache_dir() -> Path:
    if os.environ.get("RECAST_RADAR_TESTDATA"):
        return Path(os.environ["RECAST_RADAR_TESTDATA"])
    if os.name == "nt" and os.environ.get("LOCALAPPDATA"):
        return Path(os.environ["LOCALAPPDATA"]) / "recast-radar-tools" / "testdata"
    base = os.environ.get("XDG_CACHE_HOME") or (Path.home() / ".cache")
    return Path(base) / "recast-radar-tools" / "testdata"


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def _download(entry: dict, path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    errors = []
    for url in entry.get("urls", []):
        temp = path.with_name(path.name + ".part")
        try:
            with urllib.request.urlopen(url, timeout=120) as response, open(temp, "wb") as out:
                shutil.copyfileobj(response, out)
            if _sha256(temp) != entry["sha256"]:
                raise ValueError("sha256 mismatch")
            os.replace(temp, path)
            return
        except Exception as exc:  # noqa: BLE001 - try the next URL
            errors.append(f"{url}: {exc}")
            if temp.exists():
                temp.unlink()
    pytest.skip(f"{entry['id']}: download failed ({'; '.join(errors) or 'no urls'})")


def data_path(file_id: str) -> Path:
    """The verified local path of testdata file ``file_id``."""
    entry = MANIFEST.get(file_id)
    if entry is None:
        raise KeyError(f"no testdata entry {file_id!r}")
    committed = entry.get("committed")
    if committed:
        path = TESTDATA / committed.removeprefix("testdata/")
    else:
        path = cache_dir() / file_id
        if not path.is_file():
            if os.environ.get("RECAST_RADAR_TESTDATA_OFFLINE"):
                pytest.skip(f"{file_id}: not cached and RECAST_RADAR_TESTDATA_OFFLINE is set")
            _download(entry, path)
    digest = _sha256(path)
    assert digest == entry["sha256"], f"{file_id}: sha256 {digest} != manifest"
    return path


def gunzipped(path: Path, tmp_path: Path) -> Path:
    """``path``, or a decompressed copy when it is gzip (xradar cannot read
    whole-file gzip)."""
    with open(path, "rb") as f:
        magic = f.read(2)
    if magic != b"\x1f\x8b":
        return path
    out = tmp_path / (path.name + ".raw")
    with gzip.open(path, "rb") as src, open(out, "wb") as dst:
        shutil.copyfileobj(src, dst)
    return out


def pytest_collection_modifyitems(config, items):
    if os.environ.get("RECAST_RADAR_NETWORK_TESTS") == "1":
        return
    skip = pytest.mark.skip(reason="network test: set RECAST_RADAR_NETWORK_TESTS=1")
    for item in items:
        if "network" in item.keywords:
            item.add_marker(skip)


@pytest.fixture
def ktlx_trim() -> Path:
    """KTLX 2024-03-15 00:02 (Build 22, VCP 212), first two sweeps."""
    return data_path("l2-ktlx-20240315-000217-trim")
