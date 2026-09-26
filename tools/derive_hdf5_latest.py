#!/usr/bin/env python3
"""Derive the HDF5 1.10+ ("latest" format) container fixture from a real ODIM file.

No radar producer found in 2026-09 writes superblock v3 files or version-4
data layouts (every ODIM feed probed writes superblock v0/v1; netCDF-4
writes superblock v2 with v1 B-tree chunk indexes). The probe, on
2026-09-25, read the superblock version of every HDF5 file cached from the
recast-radar-data provider fetchers and the HDF5 probe downloads
(radar-corpus/feeds, radar-corpus/hdf5-probe), 222 ODIM_H5 files:

    ARPA Lombardia 10, ARPA Piemonte 2, CHMI 24, DMI 1, DWD 61, FMI 1,
    GeoSphere Austria 2, IMGW 2, KAIA (Estonia) 1, Meteo Romania 6,
    Meteo-France 1, OPERA ODIM Radar Data 93, SHMU 15, SMHI 2,
    BoM through NCI THREDDS 1 (a zip member)

218 are superblock v0 and 4 superblock v1 (SMHI's two and two ORD files);
none is v2 or v3. The committed netCDF-4 CfRadial files (Radx, xradar,
NCAR S-Pol, ARM X-SAPR) are superblock v2. To test those structures
on real radar data, this script copies the first two sweeps of the DMI Romo
PVOL (corpus entry odim-dkrom-20260820-1130-pvol) into a new container with
h5py and the HDF5 library's "latest" format, trimmed to the first 120 range
gates of every ray. Every attribute is copied with its stored datatype and
value, in the source's storage order, except `where/nbins`, which becomes
120; every data plane keeps its
stored values for those gates. The container is what changes:

- superblock v3 behind a 512-byte user block (all addresses relative to it);
- version 2 object headers everywhere; object header times on /dataset1;
- the root group and /dataset2 track and index creation order (links and
  attributes); /dataset1 does not (name index only);
- dense attribute storage: /how (30 attributes, default phase change) and
  /dataset1/how (phase change 0/0), whose 5.6-7.9 kB `azangels`, `aztimes`
  and `elangels` strings exceed the 4 kB managed-object limit and become
  huge fractal heap objects found through the huge-object v2 B-tree;
- dense links in /dataset1 (11 links) and /dataset2;
- one chunk index per /dataset1 data plane (version 4 layout messages):

    data1 DBZH   fixed array, paged (2x16 chunks, 1,440 > 1,024 entries),
                 shuffle + deflate + Fletcher-32
    data2 VRAD   implicit index (early allocation, no filters)
    data3 TH     fixed array, unpaged, no filters
    data4 WRAD   extensible array over rays (maxshape (None, 120)), deflate:
                 360 chunks reach past the index block into a secondary block
    data5 ZDR    extensible array over gates (maxshape (360, None)): the
                 unlimited dimension is not the first one; 360 chunks, no
                 filters
    data6 RHOHV  v2 B-tree (two unlimited dimensions), deflate (record type
                 11), 360 chunks in a two-level tree
    data7 PHIDP  v2 B-tree, no filters (record type 10), 720 chunks in a
                 two-level tree
    data8 LDR    single chunk, deflate

- /dataset2 data planes: data1 single chunk without filters, data2
  contiguous, the rest fixed arrays with deflate.

Run with the reference venv (h5py 3.16.0, HDF5 2.0.0 when this fixture was
made):

    python tools/derive_hdf5_latest.py <source.h5> <output.h5>
"""

import sys

import h5py
import numpy as np

USERBLOCK = 512
GATES = 120


def native_attribute_names(obj):
    """Attribute names in storage (header) order, as the source keeps them."""
    names = []
    h5py.h5a.iterate(obj.id, lambda name: names.append(name.decode()),
                     index_type=h5py.h5.INDEX_NAME, order=h5py.h5.ITER_NATIVE)
    return names


def copy_attrs(source, target):
    for name in native_attribute_names(source):
        aid = source.attrs.get_id(name)
        value = source.attrs[name]
        target.attrs.create(name, value, dtype=aid.dtype)


def group_plist(track_order=False, dense_attrs=False, times=False):
    gcpl = h5py.h5p.create(h5py.h5p.GROUP_CREATE)
    if track_order:
        flags = h5py.h5p.CRT_ORDER_TRACKED | h5py.h5p.CRT_ORDER_INDEXED
        gcpl.set_link_creation_order(flags)
        gcpl.set_attr_creation_order(flags)
    if dense_attrs:
        gcpl.set_attr_phase_change(0, 0)
    gcpl.set_obj_track_times(times)
    return gcpl


def make_group(parent, name, **kwargs):
    gid = h5py.h5g.create(parent.id, name.encode(), gcpl=group_plist(**kwargs))
    return h5py.Group(gid)


def make_dataset(parent, name, data, chunks=None, maxshape=None, gzip=False,
                 shuffle=False, fletcher32=False, early=False):
    dcpl = h5py.h5p.create(h5py.h5p.DATASET_CREATE)
    dcpl.set_obj_track_times(False)
    if chunks is not None:
        dcpl.set_chunk(chunks)
        if shuffle:
            dcpl.set_shuffle()
        if gzip:
            dcpl.set_deflate(6)
        if fletcher32:
            dcpl.set_fletcher32()
        if early:
            dcpl.set_alloc_time(h5py.h5d.ALLOC_TIME_EARLY)
    shape = data.shape
    maxdims = tuple(h5py.h5s.UNLIMITED if m is None else m for m in (maxshape or shape))
    space = h5py.h5s.create_simple(shape, maxdims)
    tid = h5py.h5t.py_create(data.dtype)
    dsid = h5py.h5d.create(parent.id, name.encode(), tid, space, dcpl=dcpl)
    dsid.write(h5py.h5s.ALL, h5py.h5s.ALL, np.ascontiguousarray(data))
    return h5py.Dataset(dsid)


def copy_leaf_groups(source, target, names, **kwargs):
    for name in names:
        if name in source:
            group = make_group(target, name, **kwargs)
            copy_attrs(source[name], group)
            if name == "where" and "nbins" in group.attrs:
                aid = source[name].attrs.get_id("nbins")
                group.attrs.modify("nbins", np.array(GATES, dtype=aid.dtype))


# data plane layouts for /dataset1, by data group name.
LAYOUTS = {
    "data1": dict(chunks=(2, 16), gzip=True, shuffle=True, fletcher32=True),
    "data2": dict(chunks=(60, 40), early=True),
    "data3": dict(chunks=(45, 30)),
    "data4": dict(chunks=(1, GATES), maxshape=(None, GATES), gzip=True),
    "data5": dict(chunks=(8, 16), maxshape=(360, None)),
    "data6": dict(chunks=(8, 16), maxshape=(None, None), gzip=True),
    "data7": dict(chunks=(6, 10), maxshape=(None, None)),
    "data8": dict(chunks=(360, GATES), gzip=True),
}


def main(source_path, output_path):
    src = h5py.File(source_path, "r")
    fcpl = h5py.h5p.create(h5py.h5p.FILE_CREATE)
    fcpl.set_userblock(USERBLOCK)
    flags = h5py.h5p.CRT_ORDER_TRACKED | h5py.h5p.CRT_ORDER_INDEXED
    fcpl.set_link_creation_order(flags)
    fcpl.set_attr_creation_order(flags)
    fcpl.set_obj_track_times(False)
    fapl = h5py.h5p.create(h5py.h5p.FILE_ACCESS)
    fapl.set_libver_bounds(h5py.h5f.LIBVER_LATEST, h5py.h5f.LIBVER_LATEST)
    fid = h5py.h5f.create(output_path.encode(), h5py.h5f.ACC_TRUNC, fcpl=fcpl, fapl=fapl)
    out = h5py.File(fid)
    copy_attrs(src, out)
    copy_leaf_groups(src, out, ["what", "where", "how"], track_order=True)

    # Sweep 1: no creation order, object header times, every chunk index.
    ds1 = make_group(out, "dataset1", times=True)
    copy_attrs(src["dataset1"], ds1)
    copy_leaf_groups(src["dataset1"], ds1, ["what", "where"])
    copy_leaf_groups(src["dataset1"], ds1, ["how"], dense_attrs=True)
    for name in sorted(k for k in src["dataset1"] if k.startswith("data")):
        moment = make_group(ds1, name)
        copy_attrs(src["dataset1"][name], moment)
        copy_leaf_groups(src["dataset1"][name], moment, ["what", "where", "how"])
        values = src["dataset1"][name]["data"][:, :GATES]
        plane = make_dataset(moment, "data", values, **LAYOUTS[name])
        copy_attrs(src["dataset1"][name]["data"], plane)

    # Sweep 2: creation order tracked and indexed; single chunk + contiguous.
    ds2 = make_group(out, "dataset2", track_order=True)
    copy_attrs(src["dataset2"], ds2)
    copy_leaf_groups(src["dataset2"], ds2, ["what", "where", "how"], track_order=True)
    for name in sorted(k for k in src["dataset2"] if k.startswith("data")):
        moment = make_group(ds2, name, track_order=True)
        copy_attrs(src["dataset2"][name], moment)
        copy_leaf_groups(src["dataset2"][name], moment, ["what", "where", "how"], track_order=True)
        values = src["dataset2"][name]["data"][:, :GATES]
        if name == "data1":
            plane = make_dataset(moment, "data", values, chunks=values.shape)
        elif name == "data2":
            plane = make_dataset(moment, "data", values)
        else:
            plane = make_dataset(moment, "data", values, chunks=(90, 60), gzip=True)
        copy_attrs(src["dataset2"][name]["data"], plane)
    out.close()

    check = h5py.File(output_path, "r")
    assert check.id.get_create_plist().get_version()[0] == 3
    for sweep in ("dataset1", "dataset2"):
        for name in (k for k in src[sweep] if k.startswith("data")):
            assert np.array_equal(check[sweep][name]["data"][()], src[sweep][name]["data"][:, :GATES])
    print(f"wrote {output_path}")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
