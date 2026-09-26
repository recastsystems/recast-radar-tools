"""``recast_radar.to_pyart`` against Py-ART's own readers on real files.

Py-ART is the reference for the layout (one volume range, rays in file
order, masked fields): ``pyart.io.read_nexrad_archive(linear_interp=False)``,
``pyart.aux_io.read_odim_h5`` and ``pyart.io.read_cfradial``. Fields must
agree bit for bit, masks included, under the reader's own field names.

Items where Py-ART's readers report something else, each skipped only for
its format:

* ``message-1``: Message 1 volumes have no site location (Py-ART writes 0,
  this package NaN) and no cut angles (Py-ART takes the VCP's target angles,
  this package the first ray's elevation).
* ``odim-time``: Py-ART's ODIM reader gives whole-second times from the sweep
  start; this package the per-ray ``startazT``/``stopazT`` times.
* ``odim-angles``: Py-ART's ODIM reader gives azimuths in -180..180 and the
  nominal elevation for every ray; compared modulo 360 and not at all.
* ``odim-rstart-metres``: espdg writes ``where/rstart`` in metres, which
  Py-ART reads as kilometres.
* ``cfradial-range``: a CfRadial range the decoder found uniform is
  regenerated from its first centre and spacing, so it differs from the
  file's float32 values in the last bits.
* ``cfradial-sweep-number``: Py-ART keeps the file's ``sweep_number``
  (FM301 numbers sweeps from 0).
"""

from __future__ import annotations

import shutil
import warnings

import numpy as np
import pytest

from conftest import data_path

pyart = pytest.importorskip("pyart")
import recast_radar  # noqa: E402

CASES = [
    ("l2-ktlx-20240315-000217-trim", "nexrad"),
    ("l2-kdvn-20200810-180401-trim", "nexrad"),
    ("l2-klix-20050829-130035-trim", "nexrad"),
    ("l2-ktlx-19990504-002218-trim", "nexrad"),
    ("l2-kilx-20260418-013553-trim", "nexrad"),
    ("l2-pgua-20230524-030945-trim", "nexrad"),
    ("odim-dkrom-20260820-1130-pvol", "odim"),
    ("odim-iesha-20260305-0115-pvol", "odim"),
    ("odim-espdg-20260707-1927-pvol-dbzh-vradh", "odim"),
    ("odim-bejab-20190606-0000-pvol", "odim"),
    ("cfrad1-irene-sr2-20110827-120420-sur-sweeps01", "cfradial1"),
    ("cfrad1-dow8-20211011-223602-rhi-trim3-classic", "cfradial1"),
    ("cfrad1-xsapr-sgp-20110520-ppi-classic", "cfradial1"),
    pytest.param("l2-ktlx-20240315-000217", "nexrad", marks=pytest.mark.slow),
    pytest.param("l2-kdvn-20200810-180401", "nexrad", marks=pytest.mark.slow),
    pytest.param("l2-kpah-20080415-235014", "nexrad", marks=pytest.mark.slow),
    pytest.param("l2-klix-20050829-130035", "nexrad", marks=pytest.mark.slow),
    pytest.param("l2-ktlx-19990504-002218", "nexrad", marks=pytest.mark.slow),
]

MESSAGE_1 = {
    "l2-klix-20050829-130035-trim",
    "l2-ktlx-19990504-002218-trim",
    "l2-klix-20050829-130035",
    "l2-ktlx-19990504-002218",
}


def _pyart(path, kind, tmp_path):
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        if kind == "nexrad":
            if path.name.endswith(".gz") and path.read_bytes()[:2] != bytes([0x1F, 0x8B]):
                # Py-ART opens *.gz files with gzip, whatever the bytes.
                path = shutil.copyfile(path, tmp_path / path.name.removesuffix(".gz"))
            return pyart.io.read_nexrad_archive(str(path), linear_interp=False)
        if kind == "odim":
            return pyart.aux_io.read_odim_h5(str(path))
        return pyart.io.read_cfradial(str(path))


def _text(array) -> list[str]:
    array = np.asarray(array)
    if array.ndim == 2 and array.dtype.kind == "S":  # netCDF character array
        return [b"".join(row).decode().strip("\x00 ") for row in array]
    return [v.decode() if isinstance(v, bytes) else str(v) for v in array]


@pytest.mark.parametrize(("file_id", "kind"), CASES)
def test_to_pyart_matches_the_pyart_reader(file_id, kind, tmp_path):
    path = data_path(file_id)
    theirs = _pyart(path, kind, tmp_path)
    ours = recast_radar.to_pyart(path, field_names="reader")
    assert isinstance(ours, pyart.core.Radar)

    assert (ours.nsweeps, ours.nrays, ours.ngates) == (theirs.nsweeps, theirs.nrays, theirs.ngates)
    assert ours.scan_type == theirs.scan_type
    for name in ("sweep_start_ray_index", "sweep_end_ray_index"):
        np.testing.assert_array_equal(getattr(ours, name)["data"], getattr(theirs, name)["data"])
    assert _text(ours.sweep_mode["data"]) == _text(theirs.sweep_mode["data"])
    if kind != "cfradial1":  # cfradial-sweep-number
        np.testing.assert_array_equal(ours.sweep_number["data"], theirs.sweep_number["data"])

    if file_id != "odim-espdg-20260707-1927-pvol-dbzh-vradh":  # odim-rstart-metres
        # cfradial-range: a CfRadial range the decoder found uniform is
        # regenerated from its first centre and spacing (float32 noise).
        np.testing.assert_allclose(ours.range["data"], theirs.range["data"], rtol=1e-5, atol=0.01)
    if kind == "odim":  # odim-angles
        diff = (ours.azimuth["data"] - theirs.azimuth["data"] + 180.0) % 360.0 - 180.0
        assert np.abs(diff).max() < 1e-3
    else:
        np.testing.assert_allclose(ours.azimuth["data"], theirs.azimuth["data"], atol=1e-4)
        np.testing.assert_allclose(ours.elevation["data"], theirs.elevation["data"], atol=1e-4)
    if kind != "odim":  # odim-time
        assert ours.time["units"] == theirs.time["units"]
        np.testing.assert_allclose(ours.time["data"], theirs.time["data"], atol=1e-3)
    if file_id not in MESSAGE_1:  # message-1
        np.testing.assert_allclose(ours.fixed_angle["data"], theirs.fixed_angle["data"], atol=1e-3)
        for name in ("latitude", "longitude", "altitude"):
            np.testing.assert_allclose(getattr(ours, name)["data"], getattr(theirs, name)["data"], atol=1e-6)

    assert sorted(ours.fields) == sorted(theirs.fields)
    for name, entry in theirs.fields.items():
        a = entry["data"]
        b = ours.fields[name]["data"]
        assert b.shape == a.shape, name
        assert b.dtype == a.dtype, name
        mask_a = np.ma.getmaskarray(a) | ~np.isfinite(np.ma.getdata(a))
        mask_b = np.ma.getmaskarray(b)
        np.testing.assert_array_equal(mask_b, mask_a, err_msg=f"{name} mask")
        np.testing.assert_array_equal(
            np.ma.getdata(b)[~mask_b], np.ma.getdata(a)[~mask_a], err_msg=f"{name} values"
        )


@pytest.mark.parametrize("field_names", ["config", "fm301"])
def test_field_name_modes(ktlx_trim, field_names):
    radar = recast_radar.to_pyart(ktlx_trim, field_names=field_names)
    expected = "reflectivity" if field_names == "config" else "DBZH"
    assert expected in radar.fields


def test_custom_field_names(ktlx_trim):
    radar = recast_radar.to_pyart(ktlx_trim, field_names={"DBZH": "dbz"})
    assert "dbz" in radar.fields and "VRADH" in radar.fields


def test_datatree_volume_and_path_give_the_same_radar(ktlx_trim):
    """The generic DataTree path (rays sorted back into time order) and the
    Volume path give Py-ART the same fields as a path does."""
    reference = recast_radar.to_pyart(ktlx_trim)
    tree = recast_radar.open(ktlx_trim)  # first_dim="auto", decoded
    volume = recast_radar.read(ktlx_trim)
    for radar in (recast_radar.to_pyart(tree), volume.to_pyart()):
        assert sorted(radar.fields) == sorted(reference.fields)
        for name, entry in reference.fields.items():
            np.testing.assert_array_equal(
                np.ma.getmaskarray(radar.fields[name]["data"]), np.ma.getmaskarray(entry["data"])
            )
            np.testing.assert_array_equal(
                np.ma.filled(radar.fields[name]["data"], 0), np.ma.filled(entry["data"], 0)
            )


def test_an_xradar_tree_converts_too(tmp_path):
    xradar = pytest.importorskip("xradar")
    path = data_path("odim-dkrom-20260820-1130-pvol")
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        tree = xradar.io.open_odim_datatree(str(path))
    radar = recast_radar.to_pyart(tree, field_names="fm301")
    ours = recast_radar.to_pyart(path, field_names="fm301")
    assert radar.nrays == ours.nrays and radar.nsweeps == ours.nsweeps
    np.testing.assert_array_equal(
        np.ma.getmaskarray(radar.fields["DBZH"]["data"]), np.ma.getmaskarray(ours.fields["DBZH"]["data"])
    )


def test_mixed_gate_spacings_keep_each_gate_at_its_range():
    """FMI Anjalankoski through LROSE Radx (CfRadial 1, a range per ray):
    sweeps 0 and 1 have 500 m gates and sweep 2 250 m gates, all from a
    first gate centre of 0 m. Every 250 m gate of sweep 2 sits at its own
    range in the Py-ART layout, and every 500 m gate of sweep 0 covers the
    two 250 m volume gates whose centres it holds (ties to the nearer gate
    below)."""
    path = data_path("cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry")
    radar = recast_radar.to_pyart(path, field_names="fm301")
    tree = recast_radar.open(path, first_dim="time", decode=False)
    volume_range = radar.range["data"]
    assert volume_range[0] == 0.0 and volume_range[1] == 250.0

    fine = tree["sweep_2"]
    fine_range = fine["range"].values
    index = np.searchsorted(volume_range, fine_range)
    np.testing.assert_array_equal(volume_range[index], fine_range)
    ours = radar.get_field(2, "DBZH")
    raw = fine["DBZH"].values
    fill = fine["DBZH"].attrs.get("_FillValue")
    undetect = fine["DBZH"].attrs.get("_Undetect")
    valid = (raw != fill) & (raw != undetect)
    scaled = (raw * fine["DBZH"].attrs["scale_factor"] + fine["DBZH"].attrs["add_offset"]).astype(np.float32)
    placed = np.ma.getdata(ours)[:, index]
    np.testing.assert_array_equal(placed[valid], scaled[valid])

    coarse = tree["sweep_0"]
    n = coarse.sizes["range"]
    last_centre = float(coarse["range"].values[-1])
    gate = np.arange(radar.ngates)
    covering = np.ceil(volume_range / 500.0 - 0.5).astype(int)
    # Py-ART's 2:1 rule leaves the volume gate past the last coarse gate's
    # centre masked.
    covered = (covering < n) & (volume_range <= last_centre)
    past = gate[volume_range > last_centre]
    assert np.ma.getmaskarray(radar.get_field(0, "DBZH"))[:, past].all()
    raw = coarse["DBZH"].values
    valid = (raw != fill) & (raw != undetect)
    scaled = (raw * coarse["DBZH"].attrs["scale_factor"] + coarse["DBZH"].attrs["add_offset"]).astype(np.float32)
    placed = np.ma.getdata(radar.get_field(0, "DBZH"))[:, gate[covered]]
    source = scaled[:, covering[covered]]
    keep = valid[:, covering[covered]]
    np.testing.assert_array_equal(placed[keep], source[keep])


# --- Level III against pyart.io.read_nexrad_level3 ---------------------------------
#
# Fields and masks agree bit for bit under the reader's names, with these
# documented differences:
#
# * ``level3-geometry``: Py-ART gives gate starts (``first_bin`` +
#   ``i * range_scale``, with its 0.999 km range scale for 1 km products)
#   and radial start angles; this package gives gate and ray centres, as
#   FM301 and the other readers do. Generic-packet products (DPR) agree.
# * ``level3-time``: Py-ART gives every ray the volume start time; this
#   package the product's sweep time, in the same units.
# * ``level3-elevation``: Py-ART writes 0 when the product has no elevation
#   angle (hybrid-scan and layer products such as DHR, HHC, DPR) and for
#   products missing from its elevation table (TDWR TV0); this package NaN,
#   or the elevation of the product description (TV0: 0.3 deg).
# * ``level3-dhr``: Py-ART's DHR is 1.0 dBZ above this package's; MetPy's
#   ``Level3File.map_data`` agrees with this package (checked below).
# * ``level3-dpr-zero``: Py-ART masks DPR's raw value 0 (no precipitation);
#   this package keeps it as 0 mm/h.
# * ``level3-srm-name``: Py-ART's reader calls N0S (product 56) ``velocity``;
#   this package keeps the FM301 name ``SRM``.

LEVEL3_CASES = [
    "l3-byx-n0q-20150124-2106",
    "l3-ftg-n0b-20220304-1820",
    "l3-tlx-n0x-20260622-080623",
    "l3-tlx-n0u-20220503-005231",
    "l3-tlx-n0c-20260622-080623",
    "l3-tlx-n0k-20260622-080623",
    "l3-tlx-n0h-20260622-080623",
    "l3-tlx-n0s-20260622-080623",
    "l3-mci-tv0-20160526-2154",
    "l3-tlx-hhc-20260622-080623",
    "l3-tlx-dpr-20260622-080623",
    "l3-tlx-dhr-20260622-080623",
]


@pytest.mark.parametrize("file_id", LEVEL3_CASES)
def test_level3_to_pyart_matches_the_pyart_reader(file_id):
    path = data_path(file_id)
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        theirs = pyart.io.read_nexrad_level3(str(path))
    ours = recast_radar.to_pyart(path, field_names="reader")

    assert (ours.nsweeps, ours.nrays, ours.ngates) == (theirs.nsweeps, theirs.nrays, theirs.ngates)
    # level3-geometry
    half_gate = float(ours.range["meters_between_gates"]) / 2
    same = np.allclose(ours.range["data"], theirs.range["data"], atol=1.0)
    starts = np.allclose(ours.range["data"] - half_gate, theirs.range["data"], rtol=1.5e-3, atol=1.0)
    assert same or starts
    # Ray centres against start angles: half of each radial's width (about
    # 1 or 0.5 deg).
    offset = (ours.azimuth["data"] - theirs.azimuth["data"] + 180.0) % 360.0 - 180.0
    assert offset.min() > 0.0 and offset.max() <= 0.6
    # level3-time
    assert ours.time["units"] == theirs.time["units"]
    # level3-elevation
    theirs_angle = float(theirs.fixed_angle["data"][0])
    ours_angle = float(ours.fixed_angle["data"][0])
    assert ours_angle == pytest.approx(theirs_angle, abs=1e-3) or (
        theirs_angle == 0.0 and (np.isnan(ours_angle) or file_id == "l3-mci-tv0-20160526-2154")
    )

    names = {"SRM": "velocity"}  # level3-srm-name
    assert sorted(names.get(name, name) for name in ours.fields) == sorted(theirs.fields)
    for name, entry in ours.fields.items():
        a = theirs.fields[names.get(name, name)]["data"]
        b = entry["data"]
        assert b.shape == a.shape and b.dtype == a.dtype, name
        mask_a = np.ma.getmaskarray(a)
        mask_b = np.ma.getmaskarray(b)
        values_b = np.ma.getdata(b)
        if file_id == "l3-tlx-dpr-20260622-080623":  # level3-dpr-zero
            assert (mask_a | ~mask_b).all()
            np.testing.assert_array_equal(values_b[mask_a & ~mask_b], 0.0)
            mask_b = mask_a
        np.testing.assert_array_equal(mask_b, mask_a, err_msg=f"{name} mask")
        shift = -1.0 if file_id == "l3-tlx-dhr-20260622-080623" else 0.0  # level3-dhr
        np.testing.assert_array_equal(
            values_b[~mask_b], np.ma.getdata(a)[~mask_a] + np.float32(shift), err_msg=f"{name} values"
        )


def test_level3_dhr_levels_agree_with_metpy():
    """level3-dhr: MetPy's data-level mapping of DHR gives this package's
    values (Py-ART's are 1.0 dBZ higher)."""
    level3 = pytest.importorskip("metpy.io")
    path = data_path("l3-tlx-dhr-20260622-080623")
    product = level3.Level3File(str(path))
    radials = product.sym_block[0][0]
    codes = np.asarray(radials["data"])
    metpy = np.asarray(product.map_data(codes), dtype=np.float64)
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", RuntimeWarning)  # DHR radials carry no times
        sweep = recast_radar.open(path, first_dim="time")["sweep_0"]
    # MetPy keeps the radials in file order, as first_dim="time" does; codes
    # 0 (below threshold) and 1 (missing) are NaN in MetPy.
    ours = sweep["DBZH"].values.astype(np.float64)
    assert ours.shape == metpy.shape
    valid = codes >= 2
    np.testing.assert_array_equal(np.isnan(metpy), ~valid)
    np.testing.assert_allclose(ours[valid], metpy[valid], atol=1e-4)
