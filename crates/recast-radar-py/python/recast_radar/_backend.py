"""xarray backend: ``engine="recast_radar"``.

``xr.open_datatree(path, engine="recast_radar")`` is :func:`recast_radar.open`;
``xr.open_dataset(path, engine="recast_radar", group="sweep_0")`` returns one
group as a Dataset (with the root's coordinates). The engine is never picked
automatically: pass ``engine="recast_radar"``.
"""

from __future__ import annotations

from xarray.backends import BackendEntrypoint


class RecastRadarBackendEntrypoint(BackendEntrypoint):
    """Radar files through recast_radar (Level II/III, ODIM_H5, CfRadial 1,
    DORADE, JMA GRIB2)."""

    description = "Weather radar files as FM301 trees, decoded by recast-radar-tools"
    url = "docs/guide/python.md"
    open_dataset_parameters = (
        "filename_or_obj",
        "drop_variables",
        "mask_and_scale",
        "decode_times",
        "group",
        "first_dim",
    )

    def guess_can_open(self, filename_or_obj) -> bool:  # noqa: D401 - xarray's API
        return False

    def open_dataset(
        self,
        filename_or_obj,
        *,
        drop_variables=None,
        mask_and_scale=True,
        decode_times=True,
        group=None,
        first_dim="auto",
        **options,
    ):
        tree = self.open_datatree(
            filename_or_obj,
            mask_and_scale=mask_and_scale,
            decode_times=decode_times,
            first_dim=first_dim,
            **options,
        )
        node = tree[group] if group not in (None, "", "/") else tree
        try:
            # Also the root's non-index coordinates (latitude, longitude,
            # altitude), as xradar's single-group datasets have them.
            ds = node.to_dataset(inherit="all_coords")
        except (TypeError, ValueError):
            ds = node.to_dataset(inherit=True)
        if node is not tree:
            # xarray before "all_coords" (2025.6 and older, the last releases
            # for Python 3.10) takes any true `inherit` as True and leaves
            # the root's scalar coordinates out: add them here.
            root = tree.to_dataset(inherit=False)
            extra = {
                name: root[name]
                for name in root.coords
                if name not in ds.variables and root[name].ndim == 0
            }
            if extra:
                ds = ds.assign_coords(extra)
        if drop_variables:
            ds = ds.drop_vars(drop_variables, errors="ignore")
        return ds

    def open_datatree(
        self,
        filename_or_obj,
        *,
        drop_variables=None,
        mask_and_scale=True,
        decode_times=True,
        first_dim="auto",
        **options,
    ):
        from . import open as open_tree

        for unused in ("use_cftime", "concat_characters", "decode_coords", "decode_timedelta"):
            options.pop(unused, None)
        tree = open_tree(
            filename_or_obj,
            decode=bool(mask_and_scale),
            decode_times=bool(decode_times),
            first_dim=first_dim,
            **options,
        )
        if drop_variables:
            tree = tree.map_over_datasets(
                lambda ds: ds.drop_vars(drop_variables, errors="ignore")
            )
        return tree
