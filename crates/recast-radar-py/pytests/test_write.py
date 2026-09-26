"""Writers and the polling publisher.

They go through the same backend registry as the ``recast-radar`` command
(``recast_radar_cli::backend::Backends::builtin``), which links the Level II,
CfRadial 1, ODIM_H5 and FM301 writers and the polling-directory publisher.
Each file written reads back with the source's sweeps and rays; the Level II
and CfRadial 1 files are also read by Py-ART and compared with Py-ART's (and
h5py's) reading of the source.
"""

from __future__ import annotations

import warnings

import numpy as np
import pytest

import recast_radar
from conftest import data_path

FORMATS = ["level2", "cfradial1", "odim", "fm301"]


def test_every_writer_and_the_publisher_are_linked():
    assert recast_radar.writers() == {format: True for format in FORMATS}
    assert recast_radar.publisher_available()


@pytest.mark.parametrize("format", FORMATS)
def test_written_files_read_back(ktlx_trim, tmp_path, format):
    volume = recast_radar.read(ktlx_trim)
    out = tmp_path / f"out.{format}"
    volume.write(out, format)
    again = recast_radar.read(out)
    assert again.nsweeps == volume.nsweeps
    assert recast_radar.read(recast_radar.to_bytes(volume, format, gzip=True)).nsweeps == volume.nsweeps


def test_convert_writes_the_decoded_file(ktlx_trim, tmp_path):
    out = tmp_path / "out.ar2v"
    recast_radar.convert(ktlx_trim, out, "level2")
    assert recast_radar.read(out).nsweeps == recast_radar.read(ktlx_trim).nsweeps
    # The input does not exist: an error, and no output.
    with pytest.raises(OSError):
        recast_radar.convert(tmp_path / "missing", tmp_path / "other.ar2v", "level2")
    assert not (tmp_path / "other.ar2v").exists()


def test_unknown_format_and_options_are_value_errors(ktlx_trim, tmp_path):
    volume = recast_radar.read(ktlx_trim)
    with pytest.raises(ValueError, match="unknown format"):
        volume.write(tmp_path / "x", "geotiff")
    with pytest.raises(ValueError):
        recast_radar.to_bytes(volume, "level2", compression="zstd")
    with pytest.raises(ValueError):
        recast_radar.to_bytes(volume, "level2", site="")


def test_publish_writes_a_polling_directory(ktlx_trim, tmp_path):
    volume = recast_radar.read(ktlx_trim)
    result = volume.publish(tmp_path, site="KTLX")
    listing = (tmp_path / "KTLX" / "dir.list").read_bytes()
    assert listing.endswith(b"\n") and b"\r" not in listing
    size, name = listing.decode().split()
    assert result["path"].name == name
    assert int(size) == result["path"].stat().st_size
    assert name.startswith("KTLX20240315_") and name.endswith("_V06.ar2v")
    assert (tmp_path / "config.cfg").read_bytes() == b"ListFile: dir.list\nSite: KTLX\n"
    assert (tmp_path / "grlevel2.cfg").read_bytes() == b"Site: KTLX\n"
    assert recast_radar.read(result["path"]).nsweeps == volume.nsweeps


def test_write_chunks_gives_one_archive(ktlx_trim, tmp_path):
    volume = recast_radar.read(ktlx_trim)
    chunks = recast_radar.write_chunks(volume)
    assert [c["kind"] for c in chunks][0] == "S" and chunks[-1]["kind"] == "E"
    paths = recast_radar.write_chunks(volume, tmp_path)
    assert all(p.is_file() for p in paths)
    whole = b"".join(c["data"] for c in chunks)
    assert recast_radar.read(whole).nsweeps == volume.nsweeps


def _pyart():
    return pytest.importorskip("pyart")


def test_level2_and_cfradial1_read_in_pyart_as_the_source(ktlx_trim, tmp_path):
    """The KTLX 2024 trim written as Level II (its NEXRAD codes copied) and
    as CfRadial 1: Py-ART reads every field of both as it reads the source
    Level II file, gate for gate."""
    pyart = _pyart()
    source = pyart.io.read_nexrad_archive(str(ktlx_trim))
    volume = recast_radar.read(ktlx_trim)
    level2 = tmp_path / "ktlx.ar2v"
    cfradial = tmp_path / "ktlx.nc"
    volume.write(level2, "level2")
    volume.write(cfradial, "cfradial1")
    for written in (
        pyart.io.read_nexrad_archive(str(level2)),
        pyart.io.read_cfradial(str(cfradial)),
    ):
        assert written.nsweeps == source.nsweeps
        assert written.nrays == source.nrays
        np.testing.assert_allclose(written.azimuth["data"], source.azimuth["data"], atol=1e-3)
        for name in ("reflectivity", "velocity", "spectrum_width"):
            if name not in source.fields:
                continue
            want = source.fields[name]["data"]
            ngates = want.shape[1]
            # The CfRadial 1 file names its fields as FM301 does.
            fm301 = {"reflectivity": "DBZH", "velocity": "VRADH", "spectrum_width": "WRADH"}[name]
            key = name if name in written.fields else fm301
            got = written.fields[key]["data"][:, :ngates]
            np.testing.assert_array_equal(np.ma.getmaskarray(got), np.ma.getmaskarray(want), err_msg=name)
            np.testing.assert_allclose(got.compressed(), want.compressed(), atol=1e-4, err_msg=name)


def test_odim_to_level2_reads_in_pyart_as_h5py_reads_the_source(tmp_path):
    """DMI Romo's ODIM_H5 volume written as Level II: Py-ART reads every
    DBZH and VRAD value the source's h5py reading has (the default coding
    is exact for 8-bit ODIM data), sweep by sweep, and the fields Level II
    cannot hold come as WriteWarnings."""
    pyart = _pyart()
    h5py = pytest.importorskip("h5py")
    path = data_path("odim-dkrom-20260820-1130-pvol")
    volume = recast_radar.read(path)
    out = tmp_path / "dkrom.ar2v"
    with pytest.warns(recast_radar.WriteWarning, match="left out: field TH"):
        volume.write(out, "level2")
    written = pyart.io.read_nexrad_archive(str(out))
    with h5py.File(path, "r") as f:
        datasets = sorted((k for k in f if k.startswith("dataset")), key=lambda k: int(k[7:]))
        assert written.nsweeps == len(datasets)
        for index, name in enumerate(datasets):
            group = f[name]
            rays = written.get_slice(index)
            for data in (k for k in group if k.startswith("data")):
                what = group[data]["what"].attrs
                quantity = what["quantity"].decode()
                field = {"DBZH": "reflectivity", "VRAD": "velocity"}.get(quantity)
                if field is None:
                    continue
                raw = group[data]["data"][...]
                valid = (raw != what["nodata"]) & (raw != what["undetect"])
                values = raw * what["gain"] + what["offset"]
                got = written.fields[field]["data"][rays][:, : raw.shape[1]]
                # The rays are written from the first one collected, not
                # from north: compare each sweep's values as a whole.
                assert int(valid.sum()) == int(got.count()), (index, quantity)
                np.testing.assert_allclose(
                    np.sort(got.compressed()), np.sort(values[valid]), atol=1e-4, err_msg=f"{index} {quantity}"
                )


def test_strict_refuses_and_options_reach_the_writer(tmp_path):
    """``strict`` refuses a write that would leave a field out, writing
    nothing; ``sweeps`` and ``sweeps_in_time_order`` select one JMA cycle;
    ``position`` and ``drop_negative_range_gates`` write a Message 1 volume;
    ``nyquist_velocity`` fills a JMA velocity volume's radials."""
    dkrom = recast_radar.read(data_path("odim-dkrom-20260820-1130-pvol"))
    with pytest.raises(recast_radar.UnrepresentableError, match="strict"):
        dkrom.write(tmp_path / "strict.ar2v", "level2", strict=True)
    assert not (tmp_path / "strict.ar2v").exists()

    itok = recast_radar.read(data_path("jma-n5-20260924-210000-rs47937"))
    with pytest.raises(recast_radar.UnrepresentableError, match="sweeps="):
        itok.to_bytes("level2")
    cycle = [0, 2, 3, 6, 7, 10, 11, 14, 16, 18, 20, 22, 24, 26, 28, 30, 32]
    data = itok.to_bytes("level2", sweeps=cycle, sweeps_in_time_order=True)
    again = recast_radar.read(data)
    assert again.nsweeps == 17
    assert abs(again.sweeps[0]["fixed_angle"] - 25.0) < 0.01
    with pytest.raises(ValueError):
        itok.to_bytes("level2", sweeps=[40])

    klix = recast_radar.read(data_path("l2-klix-20050829-130035-trim"))
    located = recast_radar.read(data_path("l2-klix-20210829-180425-trim"))
    position = (located.latitude, located.longitude, located.altitude)
    with pytest.raises(recast_radar.UnrepresentableError, match="position="):
        klix.to_bytes("level2")
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        data = klix.to_bytes("level2", position=position, drop_negative_range_gates=True)
    assert any("before the radar" in str(w.message) for w in caught)
    again = recast_radar.read(data)
    assert abs(again.latitude - located.latitude) < 1e-4
    assert again.nsweeps == klix.nsweeps

    velocity = recast_radar.read(data_path("jma-n6-20191012-090000-rs47773"))
    with pytest.warns(recast_radar.WriteWarning, match="without a Nyquist velocity"):
        velocity.to_bytes("level2")
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        data = velocity.to_bytes("level2", nyquist_velocity=26.48)
    assert recast_radar.read(data).nsweeps == velocity.nsweeps
