"""The package API on real files: inputs, options, zero-copy buffers, the
``Volume`` class, every input format, and the xarray backend."""

from __future__ import annotations

import pathlib
import warnings

import numpy as np
import pytest
import xarray as xr

from conftest import data_path

import recast_radar


def _root_buffer(array: np.ndarray):
    """The object that owns an array's memory."""
    while isinstance(array.base, np.ndarray):
        array = array.base
    return array.base


def _backing_array(data) -> np.ndarray:
    """The NumPy array under xarray's lazy indexing wrappers (xarray 2025.6,
    the last release for Python 3.10, keeps an undecoded variable wrapped in
    a ``LazilyIndexedArray`` of a full-slice key; later releases unwrap it)."""
    while not isinstance(data, np.ndarray):
        key = getattr(data, "key", None)
        if key is not None:
            assert all(k == slice(None) for k in key.tuple), key
        data = data.array
    return data


# --- inputs ------------------------------------------------------------------------


@pytest.mark.parametrize("kind", ["str", "path", "bytes", "bytearray", "memoryview"])
def test_every_input_kind_decodes_the_same(ktlx_trim, kind):
    data = ktlx_trim.read_bytes()
    source = {
        "str": str(ktlx_trim),
        "path": pathlib.Path(ktlx_trim),
        "bytes": data,
        "bytearray": bytearray(data),
        "memoryview": memoryview(data),
    }[kind]
    tree = recast_radar.open(source, decode=False)
    reference = recast_radar.open(ktlx_trim, decode=False)
    np.testing.assert_array_equal(tree["sweep_0"]["DBZH"].values, reference["sweep_0"]["DBZH"].values)


def test_missing_file_raises_file_not_found(tmp_path):
    with pytest.raises(FileNotFoundError):
        recast_radar.open(tmp_path / "missing.ar2v")


def test_text_is_not_a_radar_file():
    readme = pathlib.Path(recast_radar.__file__).parent / "__init__.py"
    with pytest.raises(recast_radar.DecodeError) as info:
        recast_radar.open(readme)
    assert isinstance(info.value, ValueError)


def test_wrong_argument_type_is_a_type_error():
    with pytest.raises(TypeError):
        recast_radar.open(12345)


# --- zero copy and lazy fields -----------------------------------------------------


def test_packed_fields_are_the_decoders_buffers(ktlx_trim):
    """With decode=False and first_dim="time", a Level II field whose gates
    are the sweep's range is the moved Rust buffer itself."""
    tree = recast_radar.open(ktlx_trim, decode=False, first_dim="time")
    data = _backing_array(tree["sweep_1"]["DBZH"].variable._data)
    assert not data.flags.owndata
    assert type(_root_buffer(data)).__name__ == "PySliceContainer"


def test_mapped_fields_are_lazy_until_read(ktlx_trim):
    """Under first_dim="auto" rays are permuted, so fields are lazy views of
    the moved buffers."""
    tree = recast_radar.open(ktlx_trim, decode=False)
    variable = tree["sweep_0"]["ZDR"].variable
    assert not isinstance(variable._data, np.ndarray)
    time_order = recast_radar.open(ktlx_trim, decode=False, first_dim="time")
    order = np.argsort(time_order["sweep_0"]["azimuth"].values, kind="stable")
    np.testing.assert_array_equal(variable.values, time_order["sweep_0"]["ZDR"].values[order])
    # Partial reads go through the same mapping.
    np.testing.assert_array_equal(variable[5:9, 100:120].values, variable.values[5:9, 100:120])
    np.testing.assert_array_equal(variable[3].values, variable.values[3])


def test_truncated_dual_pol_moments_are_padded_with_fill(ktlx_trim):
    """KTLX 2024's surveillance cut carries ZDR on fewer gates than DBZH; the
    rest reads as _FillValue (NaN once decoded)."""
    raw = recast_radar.open(ktlx_trim, decode=False, first_dim="time")["sweep_0"]
    fill = raw["ZDR"].attrs["_FillValue"]
    zdr = raw["ZDR"].values
    assert (zdr[:, 1192:] == fill).all()
    decoded = recast_radar.open(ktlx_trim, first_dim="time")["sweep_0"]["ZDR"].values
    assert np.isnan(decoded[:, 1192:]).all()


# --- decoding options --------------------------------------------------------------


def test_range_folding_is_masked_or_kept_or_flagged(ktlx_trim):
    raw = recast_radar.open(ktlx_trim, decode=False, first_dim="time")["sweep_1"]["VRADH"].values
    folded = raw == 1
    assert folded.any()
    masked = recast_radar.open(ktlx_trim, first_dim="time")["sweep_1"]["VRADH"].values
    assert np.isnan(masked[folded]).all()
    kept = recast_radar.open(ktlx_trim, first_dim="time", mask_range_folded=False)["sweep_1"]
    encoding = kept["VRADH"].encoding
    np.testing.assert_array_equal(
        kept["VRADH"].values[folded], 1 * encoding["scale_factor"] + encoding["add_offset"]
    )
    flagged = recast_radar.open(ktlx_trim, first_dim="time", range_folded_variable=True)["sweep_1"]
    flags = flagged["VRADH_flags"]
    assert flags.dtype == np.uint8
    assert flags.attrs["flag_meanings"] == "range_folded"
    assert flagged["VRADH"].attrs["ancillary_variables"] == "VRADH_flags"
    np.testing.assert_array_equal(flags.values == 1, folded)


def test_packed_attributes_move_to_encoding_by_default(ktlx_trim):
    decoded = recast_radar.open(ktlx_trim)["sweep_0"]["DBZH"]
    assert "valid_range" not in decoded.attrs
    assert decoded.encoding["valid_range"].tolist() == [2, 255]
    assert decoded.encoding["flag_meanings"] == "range_folded"
    kept = recast_radar.open(ktlx_trim, packed_attrs="attrs")["sweep_0"]["DBZH"]
    assert kept.attrs["valid_range"].tolist() == [2, 255]
    raw = recast_radar.open(ktlx_trim, decode=False)["sweep_0"]["DBZH"]
    assert raw.attrs["scale_factor"] == 0.5 and raw.attrs["add_offset"] == -33.0
    assert raw.dtype == np.uint8


def test_decoded_values_follow_the_icd(ktlx_trim):
    tree = recast_radar.open(ktlx_trim, first_dim="time")
    raw = recast_radar.open(ktlx_trim, decode=False, first_dim="time")
    dbzh = tree["sweep_0"]["DBZH"].values
    codes = raw["sweep_0"]["DBZH"].values
    valid = codes >= 2
    np.testing.assert_array_equal(dbzh[valid], codes[valid] * 0.5 - 33.0)
    assert np.isnan(dbzh[~valid]).all()
    assert np.issubdtype(tree["sweep_0"]["time"].dtype, np.datetime64)


def test_wmo_flavor_uses_the_fm301_names(ktlx_trim):
    tree = recast_radar.open(ktlx_trim, flavor="wmo", first_dim="time")
    assert "fixed_angle" in tree["sweep_0"]
    assert "sweep_fixed_angle" not in tree["sweep_0"]
    with pytest.raises(ValueError):
        recast_radar.open(ktlx_trim, flavor="wmo", first_dim="auto")
    with pytest.raises(ValueError):
        recast_radar.open(ktlx_trim, first_dim="range")


def test_passthrough_all_adds_source_attributes():
    path = data_path("odim-dkrom-20260820-1130-pvol")
    flavor = recast_radar.open(path)
    everything = recast_radar.open(path, passthrough="all")
    assert len(everything["sweep_0"].attrs) > len(flavor["sweep_0"].attrs)


def test_decode_times_off_keeps_seconds(ktlx_trim):
    tree = recast_radar.open(ktlx_trim, decode_times=False)
    time = tree["sweep_0"]["time"]
    assert time.dtype == np.float64
    assert time.attrs["units"].startswith("seconds since 2024-03-15T00:02:17")


# --- every input format --------------------------------------------------------------


@pytest.mark.parametrize(
    ("file_id", "sweeps", "field"),
    [
        ("l2-ktlx-20240315-000217-trim", 2, "DBZH"),
        ("l2-klix-20050829-130035-trim", 2, "DBZH"),
        ("odim-bejab-20190606-0000-pvol", 11, "DBZH"),
        ("cfrad1-irene-sr2-20110827-120420-sur-sweeps01", 2, "DBZ"),
        # netCDF-4 CfRadial 1 and CfRadial 2 / FM301, through the HDF5 reader.
        ("cfrad1-xsapr-sgp-20110520-ppi-netcdf4", 1, "reflectivity_horizontal"),
        ("cfrad2-xradar-xsapr-sgp-20110520-ppi", 1, "reflectivity_horizontal"),
        ("cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32", 4, "DBZH"),
        ("dorade-noxp-20090501-190244-ppi", 1, None),
        ("dorade-dow6-20211230-222139-rhi-head41", 1, None),
        ("jma-n5-20191012-090000-rs47773", 26, None),
        ("l3-byx-n0q-20150124-2106", 1, None),
    ],
)
def test_every_format_opens(file_id, sweeps, field):
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", RuntimeWarning)
        tree = recast_radar.open(data_path(file_id))
    names = [name for name in tree.children if name.startswith("sweep_")]
    assert len(names) == sweeps
    ds = tree["sweep_0"].to_dataset()
    fields = [name for name, var in ds.data_vars.items() if var.ndim == 2]
    assert fields
    if field:
        assert field in fields
    tree.load()


def test_level3_without_data_array_is_a_decode_error():
    with pytest.raises(recast_radar.DecodeError, match="no data array"):
        recast_radar.open(data_path("l3-kdvn-20200810-1804-nst"))


def test_dump_reads_level3_graphic_products():
    """A product without a data array (58, Storm Tracking Information) is
    read by ``dump``: its storm-id packets carry MetPy's positions."""
    metpy_io = pytest.importorskip("metpy.io")
    path = data_path("l3-kdvn-20200810-1804-nst")
    report = recast_radar.dump(path)
    level3 = report["level3"]
    assert report["format"] == "NEXRAD Level III"
    assert level3["message_header"]["code"] == 58
    ours = {
        storm["id"]: (storm["i"] / 4, storm["j"] / 4)
        for layer in level3["symbology"]["layers"]
        for packet in layer
        for storm in (packet["packet"].get("StormIds", []) if isinstance(packet["packet"], dict) else [])
    }
    product = metpy_io.Level3File(str(path))
    theirs = {
        packet["id"]: (packet["x"], packet["y"])
        for layer in product.sym_block
        for packet in layer
        if packet.get("type") == "Storm ID"
    }
    assert ours == theirs and len(ours) == 45
    assert any("STORM ID" in line for page in level3["tabular"]["pages"] for line in page) or any(
        "STORM ID" in packet["packet"].get("text", "")
        for page in level3["graphic"]["pages"]
        for packet in page["packets"]
    )
    # The same from the file's bytes.
    assert recast_radar.dump(path.read_bytes())["level3"]["message_header"] == level3["message_header"]


def test_level2_metadata_messages_are_reachable(ktlx_trim):
    volume = recast_radar.read(ktlx_trim)
    nexrad = volume.format_metadata["nexrad"]
    # VCP 212, Build 22.0 (the manifest description, from Py-ART and MetPy).
    assert nexrad["vcp"]["pattern_number"] == 212
    assert nexrad["build"] == {"RdaBuild": 2200}
    assert len(nexrad["vcp"]["cuts"]) == nexrad["vcp"]["number_of_cuts"]
    assert nexrad == recast_radar.dump(ktlx_trim)["volumes"][0]["format_metadata"]["nexrad"]
    assert recast_radar.read(data_path("odim-bejab-20190606-0000-pvol")).format_metadata is None


def test_mobile_archive_members_are_volumes():
    path = data_path("dorade-noxp-20090610-003210-heads-zip")
    volumes = recast_radar.read_all(path)
    assert volumes
    assert all(volume.label for volume in volumes)
    last = recast_radar.open(path, volume=len(volumes) - 1)
    assert len([name for name in last.children if name.startswith("sweep_")]) == volumes[-1].nsweeps
    with pytest.raises(IndexError):
        recast_radar.open(path, volume=len(volumes))


def test_jma_station_selection():
    path = data_path("jma-n5-20191012-090000-rs47773")
    everything = recast_radar.read_all(path, all_stations=True)
    assert everything
    one = recast_radar.read(path, station=everything[0].instrument_name)
    assert one.instrument_name == everything[0].instrument_name


# --- Volume ------------------------------------------------------------------------


def test_volume_properties(ktlx_trim):
    volume = recast_radar.read(ktlx_trim)
    assert volume.source_format == "nexrad_level2"
    assert volume.format_name == "NEXRAD Level II"
    assert volume.instrument_name == "KTLX"
    assert volume.vcp == 212 and volume.scan_name == "VCP-212"
    assert volume.nsweeps == len(volume) == 2
    assert volume.time_reference.isoformat() == "2024-03-15T00:02:17+00:00"
    assert volume.latitude == pytest.approx(35.333, abs=1e-3)
    assert volume.has_level2_metadata
    sweep = volume.sweeps[0]
    assert sweep["nrays"] == 480 and sweep["ngates"] == 1832  # the trim keeps 480 rays
    assert sweep["gate_spacing"] == 250.0
    assert "DBZH" in volume.field_names
    assert "KTLX" in repr(volume)


def test_volume_conversions_copy(ktlx_trim):
    volume = recast_radar.read(ktlx_trim)
    first = volume.to_datatree(decode=False)
    second = recast_radar.to_datatree(volume, decode=False)
    np.testing.assert_array_equal(first["sweep_0"]["DBZH"].values, second["sweep_0"]["DBZH"].values)
    assert volume.nsweeps == 2  # still usable


def test_merge_joins_the_parts_of_one_scan():
    dbzh = recast_radar.read(data_path("odim-bejab-20260612-1450-dbzh"))
    vrad = recast_radar.read(data_path("odim-bejab-20260612-1450-vrad"))
    merged = recast_radar.merge([dbzh, vrad])
    assert {"DBZH", "VRAD"} <= set(merged.field_names)
    assert merged.nsweeps == dbzh.nsweeps
    with pytest.raises(ValueError):
        recast_radar.merge([])


def test_sniff():
    assert recast_radar.sniff(data_path("odim-bejab-20190606-0000-pvol").read_bytes()[:64]) == "odim_h5"
    assert recast_radar.sniff(b"AR2V0006.") == "nexrad_level2"


def test_pyart_field_names():
    assert recast_radar.pyart_field_name("DBZH") == "reflectivity"
    assert recast_radar.pyart_field_name("DBZH", "reader", "odim_h5") == "reflectivity_horizontal"
    assert recast_radar.pyart_field_name("XYZ") == "XYZ"


# --- xarray backend ----------------------------------------------------------------


def test_xarray_engine(ktlx_trim):
    tree = xr.open_datatree(ktlx_trim, engine="recast_radar")
    reference = recast_radar.open(ktlx_trim)
    np.testing.assert_array_equal(tree["sweep_0"]["DBZH"].values, reference["sweep_0"]["DBZH"].values)
    ds = xr.open_dataset(ktlx_trim, engine="recast_radar", group="sweep_1", mask_and_scale=False)
    assert ds["VRADH"].dtype == np.uint8
    assert "latitude" in ds.coords  # inherited from the root
