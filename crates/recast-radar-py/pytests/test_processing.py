"""Exercise installed Python/CLI frontends on a real, committed Level II volume."""
import json
import subprocess
import sys

import numpy as np
import pytest
import recast_radar as rr


def command(*args):
    return subprocess.run([sys.executable, "-m", "recast_radar", *map(str, args)], capture_output=True, text=True)


def test_catalog_matches_bundled_cli():
    result = command("products")
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == rr.products()
    assert {"VRADDH", "CREF", "ET", "VIL", "KDP", "RATE", "MESH"} <= {p["id"] for p in rr.products()}
    assert command("--help").returncode == 0
    assert command("not-a-command").returncode == 2


def test_products_preserve_input_and_roundtrip(ktlx_trim, tmp_path):
    source = rr.read(ktlx_trim)
    before = source.to_datatree(first_dim="time").load()
    result = rr.process(source, ["VRADDH", "CREF", "ET", "VIL", "REF_TEX"], band="s")
    inserted = {name for _, name in result.report["inserted"]}
    assert {"VRADDH", "CREF", "ET", "VIL"} <= inserted
    assert "VRADDH" not in source.field_names
    after = result.volume.to_datatree(first_dim="time").load()
    for sweep in source.sweeps:
        for name in sweep["fields"]:
            np.testing.assert_array_equal(before[f"sweep_{sweep['index']}"][name], after[f"sweep_{sweep['index']}"][name])
    output = tmp_path / "processed.nc"
    result.volume.write(output, "fm301")
    restored = rr.open(output, first_dim="time").load()
    for index, name in result.report["inserted"]:
        np.testing.assert_allclose(after[f"sweep_{index}"][name], restored[f"sweep_{index}"][name], equal_nan=True)


def test_cli_and_python_products_agree(ktlx_trim, tmp_path):
    expected = rr.process(ktlx_trim, ["CREF", "ET", "VIL"]).volume.to_datatree(first_dim="time").load()
    output = tmp_path / "cli.nc"
    run = command("process", ktlx_trim, "--products", "CREF,ET,VIL", "-o", output)
    assert run.returncode == 0, run.stderr
    report = json.loads(run.stdout)["processing"]
    actual = rr.open(output, first_dim="time").load()
    for index, name in report["inserted"]:
        np.testing.assert_array_equal(expected[f"sweep_{index}"][name], actual[f"sweep_{index}"][name])
    assert command("process", ktlx_trim, "--products", "CREF", "-o", output).returncode == 2


def test_render_matches_cli_pixels(ktlx_trim, tmp_path):
    from PIL import Image
    api_file = tmp_path / "api.png"
    cli_file = tmp_path / "cli.png"
    rgba = rr.render(ktlx_trim, api_file, size=128, sweep=0, field="DBZH")
    assert rgba.shape == (128, 128, 4) and rgba.dtype == np.uint8
    assert np.any(rgba[..., 3])
    run = command("render", ktlx_trim, "-o", cli_file, "--size", 128, "--sweep", 0, "--field", "DBZH")
    assert run.returncode == 0, run.stderr
    np.testing.assert_array_equal(rgba, np.asarray(Image.open(api_file)))
    np.testing.assert_array_equal(rgba, np.asarray(Image.open(cli_file)))


def test_unavailable_existing_and_invalid_options(ktlx_trim, tmp_path):
    result = rr.process(ktlx_trim, "MESH")
    assert result.report["unavailable"] and not result.report["inserted"]
    with pytest.raises(ValueError, match="unavailable"):
        rr.process(ktlx_trim, "MESH", strict=True)
    output = tmp_path / "must-not-exist.nc"
    run = command("process", ktlx_trim, "--products", "MESH", "--strict", "-o", output)
    assert run.returncode == 1 and not output.exists()
    first = rr.process(ktlx_trim, "CREF")
    second = rr.process(first.volume, "CREF")
    assert second.report["skipped_existing"] and not second.report["inserted"]
    for options in ({"products": "NOPE"}, {"products": []}, {"products": "CREF", "band": "bad"}, {"products": "CREF", "sweeps": [999]}):
        with pytest.raises(ValueError):
            rr.process(ktlx_trim, **options)
    with pytest.raises(ValueError):
        rr.render(ktlx_trim, size=0)


def test_cross_section_and_grid_match_cli(ktlx_trim, tmp_path):
    section_opts = dict(field="DBZH", start_km=[0, 0], end_km=[100, -100], width=32, height=16, top_m=12000.0)
    section = rr.cross_section(ktlx_trim, **section_opts)
    assert section.dims == ("height", "distance") and section.shape == (16, 32)
    assert section.attrs["units"] == "dBZ"
    assert section.height[0] == 12000 and section.height[-1] == 0
    config = tmp_path / "section-options.json"
    config.write_text(json.dumps(section_opts))
    output = tmp_path / "section.json"
    result = command("section", ktlx_trim, "--options", config, "-o", output)
    assert result.returncode == 0, result.stderr
    data = json.loads(output.read_text())
    np.testing.assert_allclose(section, np.asarray(data["values"], dtype=np.float32).reshape(data["shape"]), equal_nan=True)
    grid_opts = dict(fields=["DBZH"], shape=[2, 13, 13], limits_m=[[1000, 4000], [-80000, 80000], [-80000, 80000]], radius_m=4000.0)
    grid = rr.grid(ktlx_trim, **grid_opts)
    assert grid.DBZH.dims == ("z", "y", "x") and grid.DBZH.shape == (2, 13, 13)
    assert grid.DBZH.attrs["units"] == "dBZ"
    assert grid.crs.attrs["earth_radius"] == 6370997.0
    assert np.isfinite(grid.DBZH.values).any()
    config = tmp_path / "grid-options.json"
    config.write_text(json.dumps(grid_opts))
    output = tmp_path / "grid.json"
    result = command("grid", ktlx_trim, "--options", config, "-o", output)
    assert result.returncode == 0, result.stderr
    data = json.loads(output.read_text())
    np.testing.assert_allclose(grid.DBZH, np.asarray(data["fields"]["DBZH"], dtype=np.float32).reshape(data["shape"]), equal_nan=True)
    with pytest.raises(ValueError):
        rr.rhi_panel(ktlx_trim, sweep=0)
    with pytest.raises(ValueError):
        rr.cross_section(ktlx_trim, start_km=(0,0), end_km=(0,0))
    with pytest.raises(ValueError):
        rr.grid(ktlx_trim, **dict(grid_opts, fields=["missing"]))
    with pytest.raises(ValueError):
        rr.grid(ktlx_trim, **dict(grid_opts, shape=[0,1,1]))


def test_volume_dealias_and_confidence(ktlx_trim, tmp_path):
    profile = {"valid_time": "2024-03-15T00:00:00Z", "levels": [[0, 5, 10], [2000, 10, 15], [10000, 25, 30]]}
    result = rr.process(ktlx_trim, "VRADDH", dealias_method="volume", environment=profile)
    assert result.report["dealias_diagnostics"]["env_profile_used"]
    assert "VRADDH_CONFIDENCE" in result.volume.field_names
    tree = result.volume.to_datatree(first_dim="time").load()
    for index, name in result.report["inserted"]:
        if name == "VRADDH_CONFIDENCE":
            values = tree[f"sweep_{index}"][name].values
            assert np.nanmin(values) >= 0 and np.nanmax(values) <= 255
    config = tmp_path / "winds.json"
    config.write_text(json.dumps(profile))
    output = tmp_path / "dealiased.nc"
    run = command("process", ktlx_trim, "--products", "VRADDH", "--dealias-method", "volume", "--environment-profile", config, "-o", output)
    assert run.returncode == 0, run.stderr
    restored = rr.open(output, first_dim="time").load()
    for index, name in result.report["inserted"]:
        np.testing.assert_array_equal(tree[f"sweep_{index}"][name], restored[f"sweep_{index}"][name])
    with pytest.raises(ValueError):
        rr.process(ktlx_trim, "VRADDH", environment=profile)


def test_native_rhi_panel():
    from conftest import data_path
    volume = rr.read(data_path("cfrad1-dow8-20211011-223602-rhi-trim3-classic"))
    name = volume.sweeps[0]["fields"][0]
    section = rr.rhi_panel(volume, sweep=0, field=name, width=32, height=16)
    assert section.shape == (16, 32)
    assert section.distance[-1] == 200000 and section.height[0] == 20000
    assert np.isfinite(section.values).any()
