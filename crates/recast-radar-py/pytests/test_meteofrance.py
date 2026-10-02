"""Meteo-France polar radar BUFR (PAG, PAM) from Python.

The files are not redistributed: the tests skip unless they are in the
testdata cache.
"""

from __future__ import annotations

import warnings

import numpy as np
import pytest

import recast_radar
from conftest import cached_data_path

PAG = "meteofrance-pag-07274-20130619-1200-a"
PAM = "meteofrance-pam-07274-20130619-1200-a"


def _paths():
    pag, pam = cached_data_path(PAG), cached_data_path(PAM)
    if pag is None or pam is None:
        pytest.skip("Meteo-France files are not in the testdata cache")
    return pag, pam


def test_files_read_sniff_and_open_as_datatree():
    pag, pam = _paths()
    assert recast_radar.sniff(pag.read_bytes()) == "meteofrance_bufr"
    volume = recast_radar.read(pam)
    assert volume.source_format == "meteofrance_bufr"
    assert volume.instrument_name == "07274"
    assert volume.nsweeps == 1
    tree = recast_radar.open(pam).load()
    sweep = tree["sweep_0"].ds
    assert (sweep.sizes["azimuth"], sweep.sizes["range"]) == (720, 1066)
    assert {"DBZH", "RHOHV", "PHIDP", "ZDR"} <= set(sweep.data_vars)
    assert np.nanmax(sweep.PHIDP.values) <= 359


def test_one_elevation_merges_and_writes_level2():
    pag, pam = _paths()
    merged = recast_radar.merge([recast_radar.read(pag), recast_radar.read(pam)])
    assert [sweep["nrays"] for sweep in merged.sweeps] == [720, 360]
    # Every field listed is written: nothing is left out (the only note is
    # that Py-ART cannot mix the 240 m and 1 km gates).
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        data = merged.to_bytes(
            "level2", site="LFBH", fields=["DBZH", "VRADH", "ZDR", "PHIDP", "RHOHV"]
        )
    assert not [w for w in caught if str(w.message).startswith("left out")]
    again = recast_radar.read(data)
    assert again.nsweeps == 2
    assert again.instrument_name == "LFBH"
