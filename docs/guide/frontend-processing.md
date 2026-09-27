# Processing from Python and the command line

The Python package and CLI call the same Rust processing and rendering code.
Install the wheel to get both `recast_radar` and the `recast-radar` command.
`python -m recast_radar` runs that command without a separate binary.

List the available products:

```python
import recast_radar as radar
print(radar.products())
```

```sh
recast-radar products
```

Compute products and write an augmented volume:

```python
result = radar.process("volume.ar2v", ["VRADDH", "CREF", "ET", "VIL"], band="s")
print(result.report)
result.volume.write("products.nc", "fm301")
tree = result.volume.to_datatree()
```

```sh
recast-radar process volume.ar2v --products VRADDH,CREF,ET,VIL --band s -o products.nc
```

The result preserves the original volume's fields, sweep order, and metadata.
Python input `Volume` objects remain unchanged. The report lists inserted
fields, existing fields kept, and products unavailable because inputs are
missing. Use `strict=True` / `--strict` to reject unavailable products.
The CLI writes FM301 by default and reports writer omissions as well. It
refuses an existing destination unless `--force` is supplied.

Sweep processing includes all `DerivedSweepProduct` entries in the Rust
retrieval crate: KDP, filtered differential phase, attenuation corrections,
rain-rate estimates, textures, gradients, and quality diagnostics. It also
includes region-based or Py-ART-compatible velocity dealiasing, azimuthal
shear, and radial divergence. `dealias_method="pyart"` / `--dealias-method
pyart` selects the alternative engine.

Column processing includes composite reflectivity, echo tops, VIL, VIL
density, hail products, echo base/depth, height of maximum reflectivity, and
low-level composite reflectivity. Output field names are reported explicitly;
some differ from the requested product IDs.

Set `band="s"`, `"c"`, or `"x"` for band-dependent retrievals. The frontend
does not assume a band when it is unknown. Heights (`height_m`,
`freezing_level_m`, `minus20c_level_m`) are metres **above radar altitude**.
Hail products require the relevant temperature-level heights; MESH uses the
Witt (1998) calibration. Echo thresholds are dBZ. `sweeps=` limits sweep
products; column products still use the entire volume.

Render without Py-ART or matplotlib:

```python
rgba = radar.render(result.volume, "composite.png", field="CREF", size=1024)
assert rgba.shape == (1024, 1024, 4)
```

```sh
recast-radar render products.nc --field CREF -o composite.png
```

`render` returns a NumPy RGBA array and optionally writes a PNG with the same
pixels. `sweep`, `field`, `size`, `range_fraction`, `dealias`, and `palette`
use the CLI's rendering behavior. Pass a GR `.pal` path to `palette` to use
an existing color table. Computation releases the Python GIL.


## Volume dealiasing

Use `dealias_method="volume"` (CLI: `--dealias-method volume`) for the
whole-volume solver. It accepts `previous=` as a Volume or file path, and
`environment=` as a dictionary:

```python
profile = {"valid_time": "2024-03-15T00:00:00Z",
           "levels": [[0, 5, 10], [2000, 10, 15], [10000, 25, 30]]}
result = radar.process("volume.ar2v", "VRADDH", dealias_method="volume",
                       environment=profile)
```

Levels are `[height_above_radar_m, eastward_wind_mps, northward_wind_mps]`.
The CLI takes this JSON through `--environment-profile winds.json` and a
previous radar file through `--previous`. Solver diagnostics state whether
the supplied evidence was usable; stale profiles may be ignored by the
solver. `VRADDH_CONFIDENCE` records per-gate confidence from 0 (no opinion)
to 255 (decisive), on the same native geometry as the velocity field.

## Sections and grids

```python
section = radar.cross_section("volume.ar2v", field="DBZH",
    start_km=(0, 0), end_km=(100, -100), width=512, height=256, top_m=12000)
panel = radar.rhi_panel("native-rhi.nc", sweep=0, field="DBZH")
grid = radar.grid(["site-a.ar2v", "site-b.ar2v"], fields=["DBZH"],
    shape=(10, 101, 101),
    limits_m=((0, 9000), (-100000, 100000), (-100000, 100000)))
```

Sections and RHI panels return xarray DataArrays with height/distance
coordinates. Grids return a Dataset with z/y/x coordinates, source units,
radius of influence, and projection metadata when location is known.
Height coordinates are relative to radar altitude for sections and to the
grid origin altitude for grids. `origin=(latitude, longitude, altitude_m)`
selects an explicit grid origin.

The `section` and `grid` commands accept JSON options with the same names:

```sh
recast-radar section volume.ar2v --options section-options.json -o section.json
recast-radar grid site-a.ar2v site-b.ar2v --options grid-options.json -o grid.json
```

For example, `section-options.json`:

```json
{"field":"DBZH","start_km":[0,0],"end_km":[100,-100],"width":512,"height":256,"top_m":12000}
```

And `grid-options.json`:

```json
{"fields":["DBZH"],"shape":[10,101,101],"limits_m":[[0,9000],[-100000,100000],[-100000,100000]]}
```

CLI outputs contain shape, coordinate arrays, units, and row-major values;
missing values are JSON null. `section --options` also accepts `rhi_sweep`
and `max_range_m` to sample a native RHI. Grid weighting choices are
`barnes2`, `barnes`, `cressman`, and `nearest`; `radius_m` overrides the
native distance-beam radius with a constant. Neither frontend changes the
Rust interpolation, weighting, or field values.
