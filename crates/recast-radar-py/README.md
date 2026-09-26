# recast-radar (Python)

Python bindings for recast-radar-tools: weather radar files as xarray
DataTrees following WMO FM301 (CfRadial 2), `pyart.core.Radar` objects,
writers and data fetchers, with the decoding done in Rust.

```python
import recast_radar

tree = recast_radar.open("KTLX20240315_000217_V06")   # xarray.DataTree
radar = recast_radar.to_pyart("KTLX20240315_000217_V06")
```

Reads NEXRAD Level II and Level III, ODIM_H5, CfRadial 1 and 2 (classic
netCDF and netCDF-4), DORADE and JMA radar GRIB2, and writes NEXRAD Level II,
CfRadial 1, ODIM_H5 and FM301. The guide is `docs/guide/python.md` in the
repository.

The Rust crate `recast-radar-py` is the extension module
`recast_radar._native`; the package lives in `python/recast_radar`, the tests
in `pytests/`. Build a wheel with maturin (`maturin build --release`). The
wheels are private build artifacts: the package metadata carries the
`Private :: Do Not Upload` classifier, which PyPI rejects.
