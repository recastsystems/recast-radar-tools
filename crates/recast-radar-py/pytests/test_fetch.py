"""``recast_radar.fetch``.

The offline tests check argument handling and the selection logic. The
network tests (``RECAST_RADAR_NETWORK_TESTS=1``) make a handful of requests:
one AWS listing, for the Iowa Environmental Mesonet's polling server two
small text files, and for the SMHI archive two day catalogs and one volume.
"""

from __future__ import annotations

import datetime as dt
import time
from pathlib import Path

import pytest

import recast_radar
from recast_radar import fetch

from conftest import MANIFEST


def test_this_build_can_download():
    assert fetch.available


IEM = "https://mesonet-nexrad.agron.iastate.edu/level2/raw/"


def test_polling_is_polite():
    with pytest.raises(ValueError):
        fetch._Pace(IEM, 0.1)
    with pytest.raises(ValueError):
        fetch.polling("../etc", "unused", server=IEM)
    for count in (0, fetch.MAX_POLLING_COUNT + 1):
        with pytest.raises(ValueError, match="count"):
            fetch.polling("KTLX", "unused", server=IEM, count=count)
    assert fetch._Pace(IEM, 0.25).interval == 0.25
    assert fetch._Pace(IEM, 2.0).interval == 2.0


def test_files_that_are_not_volumes_are_skipped():
    for name in ("KXWA20260925_031115_V06.ar2v", "KBPP_20260924_2145.gz", "GAWX_20260924_2147"):
        assert fetch._is_volume_name(name), name
    for name in ("x.ar2v.tmp", "x.part", ".seen.json", "notes.txt", "dir.list", "grlevel2.cfg"):
        assert not fetch._is_volume_name(name), name


def test_the_polling_pace_is_shared_by_calls_to_one_server():
    server = "https://pace-test.invalid/polling/"
    fetch._Pace(server, 0.3).wait()
    start = time.monotonic()
    fetch._Pace(server + "KTLX/", 0.3).wait()  # another call, same host
    assert time.monotonic() - start >= 0.25
    start = time.monotonic()
    fetch._Pace("https://other-host.invalid/", 0.3).wait()
    assert time.monotonic() - start < 0.2


def test_unsafe_names_are_refused(tmp_path):
    for name in ("", "..", "a/b", "a\b", "c:x"):
        with pytest.raises(ValueError):
            fetch._plain_name(name)
    assert fetch._plain_name("bejab@20260612T1450@DBZH.h5")


def test_nearest_volumes_are_returned_in_time_order():
    def obj(stamp):
        time = dt.datetime.strptime(stamp, "%Y%m%d_%H%M%S").replace(tzinfo=dt.timezone.utc)
        return {"key": f"2024/03/15/KTLX/KTLX{stamp}_V06", "time": time}

    objects = [obj("20240315_000217"), obj("20240315_000712"), obj("20240315_001206")]
    chosen = fetch._nearest(objects, dt.datetime(2024, 3, 15, 0, 11), 2)
    assert [o["key"][-19:-4] for o in chosen] == ["20240315_000712", "20240315_001206"]


def test_providers_and_sites_are_listed_offline():
    providers = fetch.intl_providers()
    assert any(p["id"] == "jma" for p in providers)
    sites = fetch.intl_sites(providers[0]["id"])
    assert all(site["provider"] == providers[0]["id"] for site in sites)
    nexrad = fetch.nexrad_sites()
    assert any(site["id"] == "KTLX" for site in nexrad)


def test_unknown_provider():
    with pytest.raises(ValueError, match="unknown provider"):
        fetch.intl_sites("nowhere")


def test_archive_lookups_need_a_provider_with_an_archive():
    with_archive = {p["id"] for p in fetch.intl_providers() if p["archive"]}
    assert {"smhi", "australia-nci", "ord"} <= with_archive
    # Refused before any request.
    with pytest.raises(ValueError, match="has no archive"):
        fetch.intl_frames("dmi", "dkste", date="2026-06-12")
    with pytest.raises(ValueError, match="has no archive"):
        fetch.intl_frames("dmi", "dkste", when=dt.datetime(2026, 6, 12, 6, 0))
    with pytest.raises(ValueError, match="not both"):
        fetch.intl_frames("smhi", "hemse", date="2026-06-12", when=dt.datetime(2026, 6, 12))


@pytest.mark.network
def test_aws_level2_listing_has_the_conformance_volume():
    files = fetch.level2_files("KTLX", "2024-03-15")
    entry = MANIFEST["l2-ktlx-20240315-000217"]
    match = [f for f in files if f["name"] == "KTLX20240315_000217_V06"]
    assert match and match[0]["size"] == entry["size"]
    assert match[0]["time"] == dt.datetime(2024, 3, 15, 0, 2, 17, tzinfo=dt.timezone.utc)


@pytest.mark.network
def test_a_polling_server_lists_its_sites_and_files():
    # The pace spaces request starts (as the CLI does), so time from before
    # the first request: the second starts at least a second after it.
    start = time.monotonic()
    sites = fetch.polling_sites(IEM)
    assert "KTLX" in sites
    files = fetch.polling_files("KTLX", IEM)  # waits for the shared one-second pace
    assert time.monotonic() - start >= 0.99  # 1 s, less clock rounding
    assert files and all(f["url"].startswith(IEM + "KTLX/") for f in files)


@pytest.mark.network
def test_download_to_a_directory_keeps_complete_files(tmp_path):
    files = fetch.level2_files("KTLX", "2024-03-15")
    first = files[0]
    path = fetch.download(first["url"], tmp_path, name=first["name"], size=first["size"])
    assert isinstance(path, Path) and path.stat().st_size == first["size"]
    assert recast_radar.read(path).instrument_name == "KTLX"


@pytest.mark.network
def test_smhi_archive_gives_the_frames_nearest_a_time_and_keeps_downloads(tmp_path):
    # SMHI's dated catalog keeps about the last 24 hours.
    when = (dt.datetime.now(dt.timezone.utc) - dt.timedelta(hours=6)).replace(second=0, microsecond=0)
    day = when.date()
    frames = fetch.intl_frames("smhi", "angelholm", when=when, count=2)
    assert len(frames) == 2
    times = [frame["time"] for frame in frames]
    assert times == sorted(times)
    assert all(abs((t - when).total_seconds()) <= 15 * 60 for t in times), times
    whole_day = fetch.intl_frames("smhi", "angelholm", date=day)
    assert {f["identity"] for f in frames} <= {f["identity"] for f in whole_day}

    [[path]] = fetch.intl("smhi", "angelholm", tmp_path, when=frames[0]["time"])
    assert path.name == frames[0]["names"][0]
    assert recast_radar.read(path).nsweeps > 0
    before = path.stat().st_mtime_ns
    [[again]] = fetch.intl("smhi", "angelholm", tmp_path, when=frames[0]["time"])
    assert again == path and path.stat().st_mtime_ns == before  # not downloaded again
