"""Writers and the polling publisher.

They go through the same backend registry as the ``recast-radar`` command
(``recast_radar_cli::backend::Backends::builtin``), which links the Level II,
CfRadial 1, ODIM_H5 and FM301 writers and the polling-directory publisher.
Each file written reads back with the source's sweeps and rays.
"""

from __future__ import annotations

import pytest

import recast_radar

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
