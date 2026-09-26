#!/usr/bin/env python3
"""Derive the two HDF5 edge-case container fixtures from a real ODIM file.

The h5latest fixture (tools/derive_hdf5_latest.py) covers every version-4
chunk index with 8-byte addresses and lengths. A review of the reader with
h5py-made files found three structures it misread or read slowly, none of
which any radar producer seen writes, but all of which the HDF5 library
writes on request:

- extensible-array chunk indexes large enough for paged data blocks (more
  than 131,060 chunks at the library's default parameters), whose secondary
  blocks carry a page-init bitmap of ``ceil(pages / 8)`` bytes per data block;
- global heap collections in a file with 4-byte lengths, whose collection and
  object headers are padded to 8 bytes;
- committed (named) datatypes used by many attributes.

This script copies real data of the DMI Romo PVOL (corpus entry
odim-dkrom-20260820-1130-pvol) into two HDF5 "latest" format files (libver
latest/latest). Both have the root ``what``/``where``/``how`` attributes with
their strings stored as variable-length strings (global heap objects), and a
committed float64 datatype ``/f8`` that carries the source's root ``how``
attributes (so its header is large) and types every plane's ``what``
gain/offset/nodata/undetect.

``...h5edge-paged-ea.h5`` (4-byte addresses, 8-byte lengths):

- /dataset1/data1: DBZH of sweep 1 (360 x 474 u8) as a 1-D dataset of
  170,640 one-element chunks, maxshape unlimited: an extensible array whose
  last super block has paged data blocks, every page written;
- /dataset1/data2: VRAD of sweep 1, the same way, but only rays 0-9, 180 and
  350-359 written (fill value = its ``nodata`` code), so pages of a paged data
  block are left uninitialized and the page-init bitmap decides.

``...h5edge-len4.h5`` (4-byte addresses and 4-byte lengths; HDF5 2.0 cannot
open a dataset with an unlimited dimension in such a file, so it has none):

- sweeps 1 and 2 with all eight planes as contiguous datasets cut to the
  first 20 rays;
- /dataset2 stores its links dense (link phase change 0/0) in a fractal heap
  compressed with deflate (the group creation property list's filter
  pipeline).

Run with the reference venv (h5py 3.16.0, HDF5 2.0.0 when these fixtures
were made); the HDF5 C functions h5py does not wrap (H5Pset_deflate on a
group creation property list, H5Pset_link_phase_change) are called through
ctypes on h5py's own HDF5 library (Windows ``hdf5.dll``):

    python tools/derive_hdf5_edge.py <source.h5> <output-dir>
"""

import ctypes
import os
import sys

import h5py
import numpy as np

RAYS2 = 20
SPARSE_RAYS = list(range(0, 10)) + [180] + list(range(350, 360))

HDF5 = ctypes.CDLL(os.path.join(os.path.dirname(h5py.__file__), "hdf5.dll"))
HDF5.H5Pset_deflate.argtypes = [ctypes.c_int64, ctypes.c_uint]
HDF5.H5Pset_deflate.restype = ctypes.c_int
HDF5.H5Pset_link_phase_change.argtypes = [ctypes.c_int64, ctypes.c_uint, ctypes.c_uint]
HDF5.H5Pset_link_phase_change.restype = ctypes.c_int


def native_attribute_names(obj):
    names = []
    h5py.h5a.iterate(obj.id, lambda name: names.append(name.decode()),
                     index_type=h5py.h5.INDEX_NAME, order=h5py.h5.ITER_NATIVE)
    return names


def vlen_attrs(source, target):
    """Every attribute, strings as variable-length UTF-8 strings."""
    for name in native_attribute_names(source):
        value = source.attrs[name]
        aid = source.attrs.get_id(name)
        if aid.dtype.kind == "S":
            target.attrs.create(name, value.decode(), dtype=h5py.string_dtype())
        else:
            target.attrs.create(name, value, dtype=aid.dtype)


def plain_group(parent, name):
    gcpl = h5py.h5p.create(h5py.h5p.GROUP_CREATE)
    gcpl.set_obj_track_times(False)
    return h5py.Group(h5py.h5g.create(parent.id, name.encode(), gcpl=gcpl))


def filtered_dense_group(parent, name):
    gcpl = h5py.h5p.create(h5py.h5p.GROUP_CREATE)
    gcpl.set_obj_track_times(False)
    assert HDF5.H5Pset_link_phase_change(gcpl.id, 0, 0) >= 0
    assert HDF5.H5Pset_deflate(gcpl.id, 6) >= 0
    return h5py.Group(h5py.h5g.create(parent.id, name.encode(), gcpl=gcpl))


def ea_dataset(parent, name, values, fill, rows=None):
    """A 1-D u8 dataset of one-element chunks, maxshape unlimited."""
    flat = np.ascontiguousarray(values.reshape(-1))
    dcpl = h5py.h5p.create(h5py.h5p.DATASET_CREATE)
    dcpl.set_obj_track_times(False)
    dcpl.set_chunk((1,))
    dcpl.set_fill_value(np.array(fill, dtype=flat.dtype))
    space = h5py.h5s.create_simple(flat.shape, (h5py.h5s.UNLIMITED,))
    tid = h5py.h5t.py_create(flat.dtype)
    dsid = h5py.h5d.create(parent.id, name.encode(), tid, space, dcpl=dcpl)
    dataset = h5py.Dataset(dsid)
    if rows is None:
        dataset[...] = flat
    else:
        width = values.shape[1]
        for row in rows:
            dataset[row * width:(row + 1) * width] = values[row]
    return dataset


def contiguous(parent, name, values):
    dcpl = h5py.h5p.create(h5py.h5p.DATASET_CREATE)
    dcpl.set_obj_track_times(False)
    space = h5py.h5s.create_simple(values.shape)
    tid = h5py.h5t.py_create(values.dtype)
    dsid = h5py.h5d.create(parent.id, name.encode(), tid, space, dcpl=dcpl)
    dsid.write(h5py.h5s.ALL, h5py.h5s.ALL, np.ascontiguousarray(values))
    return h5py.Dataset(dsid)


def plane_what(source, target, committed):
    """A plane's what: quantity as a vlen string, the rest with the committed f8."""
    for name in native_attribute_names(source):
        value = source.attrs[name]
        if isinstance(value, bytes) or getattr(value, "dtype", None) is not None and value.dtype.kind == "S":
            target.attrs.create(name, value.decode(), dtype=h5py.string_dtype())
        else:
            target.attrs.create(name, np.float64(value), dtype=committed)


def new_file(path, sizes):
    fcpl = h5py.h5p.create(h5py.h5p.FILE_CREATE)
    fcpl.set_sizes(*sizes)
    fcpl.set_obj_track_times(False)
    fapl = h5py.h5p.create(h5py.h5p.FILE_ACCESS)
    fapl.set_libver_bounds(h5py.h5f.LIBVER_LATEST, h5py.h5f.LIBVER_LATEST)
    fid = h5py.h5f.create(path.encode(), h5py.h5f.ACC_TRUNC, fcpl=fcpl, fapl=fapl)
    return h5py.File(fid)


def root_groups(src, out):
    """Root attributes and what/where/how, strings as vlen strings; the
    committed f8 type carrying the root how attributes."""
    vlen_attrs(src, out)
    for name in ("what", "where", "how"):
        vlen_attrs(src[name], plain_group(out, name))
    out["f8"] = np.dtype("<f8")
    committed = out["f8"]
    vlen_attrs(src["how"], committed)
    return committed


def extensible_array_file(src, path):
    out = new_file(path, (4, 8))
    committed = root_groups(src, out)
    ds1 = plain_group(out, "dataset1")
    for name in ("what", "where"):
        vlen_attrs(src["dataset1"][name], plain_group(ds1, name))
    for name, rows in (("data1", None), ("data2", SPARSE_RAYS)):
        moment = plain_group(ds1, name)
        what = plain_group(moment, "what")
        plane_what(src["dataset1"][name]["what"], what, committed)
        values = src["dataset1"][name]["data"][()]
        nodata = int(src["dataset1"][name]["what"].attrs["nodata"])
        plane = ea_dataset(moment, "data", values, nodata, rows)
        vlen_attrs(src["dataset1"][name]["data"], plane)
    out.close()

    check = h5py.File(path, "r")
    assert check.id.get_create_plist().get_version()[0] == 3
    assert check.id.get_create_plist().get_sizes() == (4, 8)
    data1 = src["dataset1"]["data1"]["data"][()]
    assert np.array_equal(check["dataset1/data1/data"][()], data1.reshape(-1))
    data2 = src["dataset1"]["data2"]["data"][()]
    nodata = int(src["dataset1"]["data2"]["what"].attrs["nodata"])
    expect = np.full(data2.shape, nodata, dtype=data2.dtype)
    expect[SPARSE_RAYS] = data2[SPARSE_RAYS]
    assert np.array_equal(check["dataset1/data2/data"][()], expect.reshape(-1))
    assert check["dataset1/data1/what"].attrs.get_id("gain").get_type().committed()
    print(f"wrote {path} ({os.path.getsize(path)} bytes)")


def short_lengths_file(src, path):
    out = new_file(path, (4, 4))
    committed = root_groups(src, out)
    for sweep in ("dataset1", "dataset2"):
        group = filtered_dense_group(out, sweep) if sweep == "dataset2" else plain_group(out, sweep)
        for name in ("what", "where"):
            vlen_attrs(src[sweep][name], plain_group(group, name))
        for name in sorted(k for k in src[sweep] if k.startswith("data")):
            moment = plain_group(group, name)
            plane_what(src[sweep][name]["what"], plain_group(moment, "what"), committed)
            plane = contiguous(moment, "data", src[sweep][name]["data"][:RAYS2])
            vlen_attrs(src[sweep][name]["data"], plane)
    out.close()

    check = h5py.File(path, "r")
    assert check.id.get_create_plist().get_version()[0] == 3
    assert check.id.get_create_plist().get_sizes() == (4, 4)
    for sweep in ("dataset1", "dataset2"):
        for name in (k for k in src[sweep] if k.startswith("data")):
            assert np.array_equal(check[sweep][name]["data"][()], src[sweep][name]["data"][:RAYS2])
    assert check["what"].attrs["source"] == src["what"].attrs["source"].decode()
    print(f"wrote {path} ({os.path.getsize(path)} bytes)")


def main(source_path, out_dir):
    src = h5py.File(source_path, "r")
    extensible_array_file(src, os.path.join(out_dir, "dkrom.pvol.20260820T1130.h5edge-paged-ea.h5"))
    short_lengths_file(src, os.path.join(out_dir, "dkrom.pvol.20260820T1130.h5edge-len4.h5"))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
