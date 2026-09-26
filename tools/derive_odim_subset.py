#!/usr/bin/env python3
"""Derive a small ODIM_H5 fixture from a real, larger ODIM file by subsetting.

The operational volumes that carry the structures the ODIM decoder has to
keep (int16 planes, quality groups with legends, nested `how` groups) are
16-28 MB. This script rebuilds a chosen subset in a new file:

- the root group's attributes and the root `what`, `where` and `how` groups
  (with every subgroup);
- for each kept dataset, the dataset group's attributes, its `what`,
  `where` and `how` groups (with every subgroup), the chosen `dataM` groups
  and every dataset-level `qualityK` group.

Every group is rebuilt with its attributes in the source's storage order,
each with its stored datatype and value; every dataset with its datatype,
shape, chunk shape, filters (gzip level, shuffle, Fletcher-32) and values.
Nothing is renamed or renumbered: a kept `data14` stays `data14`, and a kept
`datasetN` stays `datasetN`. Only the container is new (default HDF5 file
format of the library h5py links, no object times, so the output is
byte-reproducible for one h5py/HDF5 build). HDF5's own object copy
(`H5Ocopy`) is not used: HDF5 2.0.0 cannot read back the groups it copies
out of the SMHI files ("ran off end of input buffer while decoding").

    python tools/derive_odim_subset.py <source.h5> <output.h5> \\
        dataset1:data1,data5,data14,data15 [datasetK:...]

A dataset given without a plane list keeps all its planes.
"""

import sys

import h5py


def native_attribute_names(obj):
    """Attribute names in storage (header) order, as the source keeps them."""
    names = []
    h5py.h5a.iterate(obj.id, lambda name: names.append(name.decode()),
                     index_type=h5py.h5.INDEX_NAME, order=h5py.h5.ITER_NATIVE)
    return names


def native_link_names(group):
    """Link names in the source's native order."""
    names = []
    h5py.h5g.iterate(group.id, lambda name: names.append(name.decode()))
    return names


def copy_attrs(source, target):
    for name in native_attribute_names(source):
        aid = source.attrs.get_id(name)
        target.attrs.create(name, source.attrs[name], dtype=aid.dtype)


def copy_dataset(source, parent, name):
    kwargs = dict(dtype=source.dtype, track_times=False)
    if source.chunks is not None:
        kwargs.update(chunks=source.chunks, shuffle=source.shuffle,
                      fletcher32=source.fletcher32)
        if source.compression is not None:
            kwargs.update(compression=source.compression,
                          compression_opts=source.compression_opts)
    target = parent.create_dataset(name, data=source[()], **kwargs)
    copy_attrs(source, target)


def copy_group(source, parent, name, keep=None):
    target = parent.create_group(name, track_order=False)
    copy_attrs(source, target)
    for child in native_link_names(source):
        if keep is not None and not keep(child):
            continue
        item = source[child]
        if isinstance(item, h5py.Dataset):
            copy_dataset(item, target, child)
        else:
            copy_group(item, target, child)
    return target


def main(source_path, output_path, selections):
    src = h5py.File(source_path, "r")
    with h5py.File(output_path, "w", track_order=False) as out:
        copy_attrs(src, out)
        for name in ("what", "where", "how"):
            if name in src:
                copy_group(src[name], out, name)
        for selection in selections:
            dataset, _, planes = selection.partition(":")
            source = src[dataset]
            planes = planes.split(",") if planes else None
            missing = [plane for plane in planes or [] if plane not in source]
            if missing:
                raise SystemExit(f"{dataset}: no {', '.join(missing)}")

            def keep(child, planes=planes):
                if child in ("what", "where", "how") or child.startswith("quality"):
                    return True
                return child.startswith("data") and (planes is None or child in planes)

            copy_group(source, out, dataset, keep)


if __name__ == "__main__":
    if len(sys.argv) < 4:
        raise SystemExit(__doc__)
    main(sys.argv[1], sys.argv[2], sys.argv[3:])
