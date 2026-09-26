"""``recast_radar.open`` against xradar on real files: groups, dimensions,
coordinates, field values (packed and decoded) and field attributes.

xradar 0.12 and this package differ in documented ways (design note
``docs/design/fm301-model.md`` sections 6.2, 7.1, 8.2 and 14; the Rust
conformance test ``crates/recast-radar-core/tests/fm301_conformance.rs``
lists them item by item). The checks below skip exactly those items:

* ``coarse-range``: on NEXRAD sweeps that mix 1 km reflectivity with 250 m
  Doppler moments, xradar takes the range from the coarser moment and
  misplaces the finer ones (6.2); range and fields are compared only where
  xradar's range equals ours.
* ``nexrad-fill``: xradar writes no ``_FillValue`` for NEXRAD, so its decoded
  fields show below-threshold and range-folded gates as numbers; ours are NaN
  (7.1). Decoded values are compared where ours are finite.
* ``odim-th-units``: ODIM ``TH`` is dBZ; xradar labels it "unitless" (8.2
  note 1).
* ``xradar-dropped-radials``: in sweep 9 of PGUA 2023-05-24 03:09, xradar
  0.12 loses radials 120 and 240 (azimuth numbers), the last radial of two
  LDM records; that elevation's records also hold RDA status messages
  (Message 2) between radials. Py-ART and MetPy read all 360 radials, as
  this package does; the other 358 are compared.
* xradar-only empty optional groups and variables that FM301 has no slot
  for are not required.
"""

from __future__ import annotations

import warnings

import numpy as np
import pytest

from conftest import gunzipped, data_path

xr = pytest.importorskip("xarray")
xradar = pytest.importorskip("xradar")
import recast_radar  # noqa: E402

OPENERS = {
    "nexrad": "open_nexradlevel2_datatree",
    "odim": "open_odim_datatree",
    "cfradial1": "open_cfradial1_datatree",
}

# (testdata id, xradar reader). Committed files first; the download-only
# full volumes are the FM301 conformance cases.
CASES = [
    ("l2-ktlx-20240315-000217-trim", "nexrad"),
    ("l2-kdvn-20200810-180401-trim", "nexrad"),
    ("l2-kilx-20260418-013553-trim", "nexrad"),
    ("l2-ktlx-20130520-201643-trim", "nexrad"),
    ("l2-kdmx-20080525-205148-trim", "nexrad"),
    ("l2-pgua-20230524-030945-trim", "nexrad"),
    ("odim-dkrom-20260820-1130-pvol", "odim"),
    ("odim-iesha-20260305-0115-pvol", "odim"),
    ("odim-espdg-20260707-1927-pvol-dbzh-vradh", "odim"),
    ("odim-bejab-20190606-0000-pvol", "odim"),
    ("odim-norst-20170421-0908-pvol", "odim"),
    ("cfrad1-irene-sr2-20110827-120420-sur-sweeps01", "cfradial1"),
    ("cfrad1-dow8-20211011-223602-rhi-trim3-classic", "cfradial1"),
    ("cfrad1-xsapr-sgp-20110520-ppi-classic", "cfradial1"),
    pytest.param("l2-ktlx-20240315-000217", "nexrad", marks=pytest.mark.slow),
    pytest.param("l2-kdvn-20200810-180401", "nexrad", marks=pytest.mark.slow),
    pytest.param("l2-kpah-20080415-235014", "nexrad", marks=pytest.mark.slow),
    pytest.param("l2-klix-20050829-130035", "nexrad", marks=pytest.mark.slow),
    # The full volumes of the other trimmed fixtures, which xradar skips.
    pytest.param("l2-kilx-20260418-013553", "nexrad", marks=pytest.mark.slow),
    pytest.param("l2-ktlx-20130520-201643", "nexrad", marks=pytest.mark.slow),
    pytest.param("l2-pgua-20230524-030945", "nexrad", marks=pytest.mark.slow),
]

# xradar-dropped-radials: (file, sweep) -> radials xradar 0.12 leaves out.
XRADAR_DROPPED_RADIALS = {("l2-pgua-20230524-030945", "sweep_9"): 2}

COORD_TOLERANCE_DEG = 1e-4


def _xradar(path, kind, tmp_path, **kwargs):
    opener = getattr(xradar.io, OPENERS[kind])
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        tree = opener(str(gunzipped(path, tmp_path)), optional_groups=True, **kwargs)
    if not _sweeps(tree):
        # xradar 0.12 drops every Level II sweep that has no end-of-elevation
        # radial (its default incomplete_sweep="drop"). The trimmed fixtures
        # that keep only part of each sweep (ktlx 2024, kdvn, kilx, ktlx 2013,
        # pgua: 120 to 480 of 720 radials) have none; incomplete_sweep="pad"
        # would regrid the rays, so they are not compared. The full volumes
        # they were cut from (in CASES) and the kdmx trim (whole sweeps) are.
        pytest.skip("xradar drops every sweep of this file as incomplete")
    return tree


def _ours(path, **kwargs):
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", RuntimeWarning)
        return recast_radar.open(path, **kwargs)


def _sweeps(tree):
    return sorted(
        (name for name in tree.children if name.startswith("sweep_")),
        key=lambda name: int(name.split("_")[1]),
    )


def _fields(ds, ray_dim):
    return [name for name, var in ds.data_vars.items() if var.dims == (ray_dim, "range")]


def _xradar_rays(file_id, sweep, theirs, ours):
    """``ours`` without the radials xradar leaves out
    (``xradar-dropped-radials``), matched by ray time."""
    ray_dim = theirs["time"].dims[0]
    dropped = XRADAR_DROPPED_RADIALS.get((file_id, sweep), 0)
    assert ours.sizes[ray_dim] - theirs.sizes[ray_dim] == dropped, sweep
    if not dropped:
        return ours
    a = theirs["time"].values.astype("datetime64[ns]")
    b = ours["time"].values.astype("datetime64[ns]")
    gap = np.abs(b[:, None] - a[None, :]).min(axis=1)
    keep = np.flatnonzero(gap <= np.timedelta64(1, "ms"))
    assert keep.size == theirs.sizes[ray_dim], sweep
    return ours.isel({ray_dim: keep})


def _same_range(theirs, ours) -> bool:
    a, b = np.asarray(theirs["range"].values), np.asarray(ours["range"].values)
    return a.shape == b.shape and np.allclose(a, b, rtol=1e-5, atol=0.01)


@pytest.mark.parametrize(("file_id", "kind"), CASES)
@pytest.mark.parametrize("first_dim", ["auto", "time"])
def test_packed_values_dims_and_coordinates_match_xradar(file_id, kind, first_dim, tmp_path):
    path = data_path(file_id)
    theirs = _xradar(path, kind, tmp_path, first_dim=first_dim, mask_and_scale=False)
    ours = _ours(path, first_dim=first_dim, decode=False)
    assert _sweeps(theirs) == _sweeps(ours)
    compared = 0
    for sweep in _sweeps(theirs):
        a = theirs[sweep].to_dataset(inherit=False)
        b = ours[sweep].to_dataset(inherit=False)
        ray_dim = a["time"].dims[0]
        assert b["time"].dims[0] == ray_dim, sweep
        b = _xradar_rays(file_id, sweep, a, b)
        assert a.sizes[ray_dim] == b.sizes[ray_dim], sweep
        for name in ("azimuth", "elevation"):
            np.testing.assert_allclose(
                a[name].values, b[name].values, atol=COORD_TOLERANCE_DEG, err_msg=f"{sweep}/{name}"
            )
        dt = np.abs(a["time"].values.astype("datetime64[ns]") - b["time"].values.astype("datetime64[ns]"))
        assert dt.max() <= np.timedelta64(1, "ms"), f"{sweep}/time"
        for name in ("sweep_mode", "sweep_fixed_angle"):
            if name in a:
                va, vb = a[name].values, b[name].values
                if va.dtype.kind in "SU":
                    assert str(va) == str(vb), f"{sweep}/{name}"
                else:
                    np.testing.assert_allclose(va, vb, atol=COORD_TOLERANCE_DEG)
        if not _same_range(a, b):
            continue  # coarse-range
        for name in _fields(a, ray_dim):
            assert name in b, f"{sweep}/{name} missing"
            assert b[name].dims == a[name].dims
            np.testing.assert_array_equal(
                np.asarray(a[name].values), np.asarray(b[name].values), err_msg=f"{sweep}/{name}"
            )
            assert b[name].dtype == a[name].dtype, f"{sweep}/{name}"
            compared += 1
    assert compared > 0


@pytest.mark.parametrize(("file_id", "kind"), CASES)
def test_decoded_values_and_field_attributes_match_xradar(file_id, kind, tmp_path):
    path = data_path(file_id)
    theirs = _xradar(path, kind, tmp_path, first_dim="auto")
    ours = _ours(path)
    compared = 0
    for sweep in _sweeps(theirs):
        a = theirs[sweep].to_dataset(inherit=False)
        b = _xradar_rays(file_id, sweep, a, ours[sweep].to_dataset(inherit=False))
        if not _same_range(a, b):
            continue  # coarse-range
        ray_dim = a["time"].dims[0]
        for name in _fields(a, ray_dim):
            va, vb = np.asarray(a[name].values), np.asarray(b[name].values)
            finite = np.isfinite(vb)
            # nexrad-fill: ours may be NaN where xradar has a number, never
            # the other way round.
            assert not (np.isnan(va) & finite).any(), f"{sweep}/{name}: NaN in xradar only"
            np.testing.assert_allclose(vb[finite], va[finite], rtol=1e-6, atol=1e-6, err_msg=f"{sweep}/{name}")
            for attr in ("standard_name", "long_name", "units"):
                if attr not in a[name].attrs:
                    continue
                if kind == "odim" and name == "TH" and attr == "units":
                    continue  # odim-th-units
                assert b[name].attrs.get(attr) == a[name].attrs[attr], f"{sweep}/{name}.{attr}"
            for key in ("scale_factor", "add_offset"):
                if key in a[name].encoding:
                    assert b[name].encoding.get(key) == pytest.approx(a[name].encoding[key]), key
            compared += 1
    assert compared > 0


@pytest.mark.parametrize(
    "file_id",
    [
        "l2-kdmx-20080525-205148-trim",
        pytest.param("l2-ktlx-20240315-000217", marks=pytest.mark.slow),
        pytest.param("l2-kdvn-20200810-180401", marks=pytest.mark.slow),
    ],
)
def test_nexrad_sweep_attributes_match_xradar(file_id, tmp_path):
    """xradar's NEXRAD group attributes (VCP and RDA status) come through."""
    path = data_path(file_id)
    theirs = _xradar(path, "nexrad", tmp_path)
    ours = _ours(path)
    for key in ("instrument_name", "scan_name", "rda_build_number", "number_elevation_cuts"):
        if key in theirs.attrs:
            assert str(ours.attrs[key]) == str(theirs.attrs[key]), key
    for sweep in _sweeps(theirs):
        for key, value in theirs[sweep].attrs.items():
            assert key in ours[sweep].attrs, f"{sweep}.{key}"
            assert str(ours[sweep].attrs[key]) == str(value), f"{sweep}.{key}"


@pytest.mark.parametrize(
    ("file_id", "kind"),
    [
        ("l2-kdmx-20080525-205148-trim", "nexrad"),
        ("odim-dkrom-20260820-1130-pvol", "odim"),
        ("cfrad1-irene-sr2-20110827-120420-sur-sweeps01", "cfradial1"),
    ],
)
def test_location_matches_xradar(file_id, kind, tmp_path):
    path = data_path(file_id)
    theirs = _xradar(path, kind, tmp_path)
    ours = _ours(path)
    for name in ("latitude", "longitude", "altitude"):
        np.testing.assert_allclose(float(ours[name].values), float(theirs[name].values), atol=1e-6)
