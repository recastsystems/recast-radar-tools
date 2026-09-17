# Test corpus: ODIM_H5, CfRadial, DORADE, JMA and carried-over Level III

This page covers `testdata/other/manifest.toml` and the committed files under
`testdata/files/other/`. The Level II corpus is described in `corpus.md`. The
corpus holds only real radar files. A few files are derived from real files
by copying raw bytes (trimming, taking a subset, changing the container, or
extracting an archive member). Every derived file names its source and has
a recipe on this page.

Entries: 30. Committed: 22 files, 15,181,236 bytes, each under 2 MB.
Download on first use: 8 files, 123,175,908 bytes, verified by sha256 and cached under
`%LOCALAPPDATA%\recast-radar-tools\testdata` or `$RECAST_RADAR_TESTDATA`.

Checked on 2026-09-16:

- `cargo test -p recast-radar-testdata` passes 23 tests, including the hash
  check on all 22 committed files.
- Every URL in the manifest was downloaded and matched the sha256 in the
  manifest. Zenodo archives also matched the md5 that Zenodo publishes.
- Independent readers checked the files:
  - Py-ART 2.2.5 `read_odim_h5` and xradar 0.12.0 `open_odim_datatree` read
    all 6 ODIM polar volumes.
  - h5py 3.16.0 showed the HDF5 layout of every ODIM file, including the
    IMGW images.
  - Py-ART `read_cfradial` read all 4 committed CfRadial files and the
    downloaded DOW8 RHI and S-Pol volume. xradar read the S-Pol volume.
  - A standalone Python DORADE block walker and a JMA GRIB2 section and
    run-length walker checked the DORADE and JMA files.

## Layout

| Directory | Contents |
|---|---|
| `files/other/odim/` | ODIM_H5 polar volumes (PVOL) |
| `files/other/odim/imgw_polrad/` | ODIM_H5 Cartesian IMAGE products (IMGW CMAX) |
| `files/other/cfradial/` | CfRadial 1.x in classic netCDF and netCDF-4 containers |
| `files/other/dorade/` | DORADE sweep files |
| `files/other/jma/` | JMA polar GRIB2 tars with one station each |
| `files/other/nexrad-level3/` | NEXRAD Level III VWP (Product 48) carried over from BowEcho |

## Entries

"C" means the file is committed. "D" means it is downloaded on first use.
"derived" means the bytes come from a real file by a recipe in the
Derivation recipes section below.

### ODIM_H5

| id | C/D | bytes | what it covers | source (license) |
|---|---|---|---|---|
| `odim-bejab-20190606-0000-pvol` | C | 640209 | H5rad 2.0, superblock v0, 11 DBZH sweeps, gate count changes between sweeps | wradlib-data (MIT) |
| `odim-bewid-20130429-0430-pvol-dbzh-scan1` | C | 348893 | H5rad 2.1, variable-length string attributes (global heap), root `how/NI` | wradlib-data (MIT) |
| `odim-norst-20170421-0908-pvol` | C | 422385 | H5rad 2.2, **superblock v1**, lowest sweep has 720 rays | open-radar-data (MIT) |
| `odim-espdg-20260707-1927-pvol-dbzh-vradh` | C | 162450 | H5rad 2.4 IRIS export, **v2 object headers** (OHDR/OCHK), float64 planes, `rstart` in metres | OPERA ORD 24h bucket (CC BY 4.0), URL expired |
| `odim-imgw-ram-20260711-0015-{kdp,phidp,rhohv,zdr}-max` | C | 33775, 32534, 61984, 59793 | ODIM IMAGE (Cartesian MAX with side projections), `what` on `dataset1`, version string `H5rd 2.3`, source has only a WMO number | IMGW-PIB datastore (attribution required), URL expired |
| `odim-iesha-20260305-0115-pvol` | C | 1667065 | **new** H5rad 2.3, 10 sweeps DBZH+TH+VRADH up to a 90 deg vertical sweep, widespread echo | OPERA ORD archive (CC BY 4.0) |
| `odim-dkrom-20260820-1130-pvol` | C | 1695131 | **new** H5rad 2.0 dual-pol, 10 sweeps x 8 quantities (VRAD/WRAD names, LDR all nodata), elevations not whole degrees | OPERA ORD archive (CC BY 4.0) |

### CfRadial

| id | C/D | bytes | what it covers | source (license) |
|---|---|---|---|---|
| `cfrad1-xsapr-sgp-20110520-ppi-netcdf4` | C | 75587 | Published netCDF-4 CfRadial 1.2 PPI (40x42). BowEcho used it to test routing and the guidance error for netCDF-4 input | Py-ART test data (BSD-3-Clause) |
| `cfrad1-xsapr-sgp-20110520-ppi-classic` | C, derived | 13624 | Classic-container copy of the file above | derived |
| `cfrad1-dow8-20211011-223602-rhi` | D | 1682730 | Native DOW8 RHI, CF-Radial-1.4 netCDF-4, 8 fields, mobile `latitude(time)` | open-radar-data (MIT) |
| `cfrad1-dow8-20211011-223602-rhi-trim3-classic` | C, derived | 888428 | Classic-container copy of the RHI above with 3 fields | derived |
| `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` | C, derived | 1664412 | **new** Radx-written **classic** CfRadial 1.3 PPI (SMART-R2, Hurricane Irene), 2 sweeps, int8 packed DBZ/VEL with `_FillValue` | Zenodo 10.5281/zenodo.3494891 (CC BY 4.0) |
| `cfrad1-spol-20080604-002217-sur` | D | 15418562 | **new** full CfRadial 1.2 PPI volume (S-Pol, 9 sweeps), netCDF-4 | open-radar-data (MIT) |
| `cfrad2-spol-20080604-002217-sur` | D | 15487749 | **new** the same volume in CfRadial 2 group layout | open-radar-data (MIT) |

### DORADE

| id | C/D | bytes | what it covers | source (license) |
|---|---|---|---|---|
| `dorade-cow2-20260521-225514-sur-head24` | C, derived | 37380 | Big-endian, HRD RLE, CSFD, antenna-transition rays, staggered PRT; first 24 of 719 rays | CSWR COW2 deployment; source URL and license unknown |
| `dorade-noxp-20090501-sweeps-tgz` | D | 324596 | Zenodo archive holding the next two entries | Zenodo 10.5281/zenodo.14194361 (CC BY 4.0) |
| `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi` | C, derived | 939268 each | **new** consecutive single-tilt set: little-endian, uncompressed, CSFD, 51 rays over 360 deg, RADD lat/lon written as 0 | archive member, unmodified |
| `dorade-noxp-20090525-sweeps-tgz` | D | 5813887 | Zenodo archive holding the next entry | Zenodo 10.5281/zenodo.14194361 (CC BY 4.0) |
| `dorade-noxp-20090525-203211-sector` | C, derived | 1634536 | **new** little-endian sector PPI with echo, 8 dual-pol fields | archive member, unmodified |
| `dorade-dow6-20211230-222139-rhi-head41` | C, derived | 1471504 | **new** first real **DORADE RHI** (RADD scan mode 3): little-endian HRD RLE, CELV, 32 fields, rays 0-40 of 156 | Zenodo DOI 10.48514/JKJ0-TE44, FARM Marshall Fire (CC BY 4.0) |

### JMA GRIB2 tar

| id | C/D | bytes | what it covers | source (license) |
|---|---|---|---|---|
| `jma-n5-20191012-090000` | D | 39106560 | **new** full N5 (reflectivity) tar at Typhoon Hagibis landfall, 20 stations | NICT mirror of JMA (terms not stated) |
| `jma-n6-20191012-090000` | D | 13209600 | **new** full N6 (radial velocity) tar at the same time, 20 stations | NICT mirror of JMA (terms not stated) |
| `jma-n5-20191012-090000-rs47773` | C, derived | 1761280 | Station TAKA (Osaka) N5 member: 26 sweeps, four descending elevation ladders | derived |
| `jma-n6-20191012-090000-rs47773` | C, derived | 624640 | Station TAKA N6 member: 13 sweeps, 25% of velocity gates non-missing | derived |

### NEXRAD Level III (carried over)

| id | C/D | bytes | what it covers | source (license) |
|---|---|---|---|---|
| `l3-kbmx-19980416-archive-tarz` | D | 32132224 | NCEI day archive (`.tar.Z`) holding the VWP product | Google Cloud `gcp-public-data-nexrad-l3` (NOAA, public domain) |
| `l3-kbmx-19980416-0006-nvw` | C, derived | 7090 | Product 48 VWP whose HHMM timeline crosses midnight | archive member, unmodified |

The Level III stream (`testdata/level3/manifest.toml`, branch `level3`) owns
Level III coverage. This file is here only because it was among the BowEcho
fixtures carried over. It may move to the Level III manifest when the branches
merge.

## BowEcho carryover (radar-bow@66ceb9c `crates/nexrad_io/tests/data`)

The files were extracted with `git -C radar-bow archive 66ceb9c`.

| BowEcho path | Here | Provenance check |
|---|---|---|
| `bejab.pvol.hdf` | `odim/bejab.pvol.hdf` | sha256 equals wradlib-data at commit 67337e5 |
| `20130429043000.rad.bewid.pvol.dbzh.scan1.hdf` | `odim/` (same name) | sha256 equals wradlib-data at commit 67337e5 |
| `T_PAGZ35_C_ENMI_20170421090837.hdf` | `odim/` (same name) | sha256 equals open-radar-data at commit ff39154 and its pooch registry |
| `espdg.pvol.20260707.dbzh_vradh.h5` | `odim/` (same name) | Came from the ORD 24h bucket on 2026-07-07 (`odim_real_files.rs` header). That object has expired and the ORD archive has no ES data for that day, so only the committed copy remains |
| `imgw_polrad/*.max.h5` | `odim/imgw_polrad/` | sha256 equals the values in BowEcho's `imgw_polrad/README.md`. The datastore is rolling and the URLs now return HTML |
| `cfrad.xsapr_sgp_ppi_20110520.netcdf4.nc` | `cfradial/` (same name) | sha256 equals Py-ART `example_cfradial_ppi.nc` at commit 1edc407 |
| `cfrad.xsapr_sgp_ppi_20110520.classic.nc` | `cfradial/` (same name) | Rebuilt from the Py-ART file with `convert_cfradial.py`. The output matches byte for byte |
| `cfrad.20211011_223602_DOW8_RHI.trim3.nc` | `cfradial/` (same name) | Rebuilt from the open-radar-data DOW8 RHI with `convert_cfradial.py ... DBZHC,VEL,WIDTH`. The output matches byte for byte |
| `swp.1260521225514.COW2.229.1.0_SUR_v215.head24` | `dorade/` (same name) | `dorade_real.rs` describes it as the first 37,380 bytes of a real COW2 sweep file from 2026-05-21. BowEcho recorded neither the full file nor a public source, and it was not found locally |
| `nexrad_vwp/KBMX_SDUS54_NVWBMX_199804160006` | `nexrad-level3/` (same name) | Extracted from the Google Cloud copy of the NCEI archive named in `nexrad_vwp/README.md`. sha256 matches |

These files were not carried over:

- `cfrad_synth.nc` and `gen_cfradial_fixture.py`: synthetic. The replacement
  is `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`. The full volumes
  `cfrad1-spol-*` and `cfrad2-spol-*` are also new download entries.
- `odim_pvol_synth.h5` and `gen_odim_fixture.py`: synthetic. The replacements
  are `odim-iesha-20260305-0115-pvol` and `odim-dkrom-20260820-1130-pvol`.
- `ref_cfradial.py` and `ref_odim.py`: scripts that print BowEcho golden
  values in the `dump_radar.rs` text format. They are test tooling, not
  corpus files. They remain at radar-bow@66ceb9c for the wave 2 test
  conversion.
- `convert_cfradial.py`: its recipe is reproduced below.
- `README.md` files: their provenance is now in the manifest and on this page.

### What the synthetic fixtures covered, and what the real replacements cover

| Synthetic feature | Real coverage now |
|---|---|
| ODIM PVOL with DBZH and VRADH | iesha (DBZH/TH/VRADH), dkrom (VRAD plus dual-pol), espdg (DBZH/VRADH float64) |
| ODIM gzip-chunked u8 planes | all PVOLs |
| ODIM **contiguous** (unchunked) data layout | **gap**: every real file checked uses chunked+gzip (bejab, bewid, norst, espdg, iesha, dkrom) |
| ODIM nodata/undetect sentinel gates | all real PVOLs contain both |
| CfRadial classic container | xsapr classic (converted), DOW8 trim3 (converted), **Irene, written natively by Radx** |
| CfRadial **UNLIMITED `time`** (record-variable interleave) | **gap**: `time` is a fixed dimension in the Irene file and in both conversions. No real classic CfRadial file with an unlimited record dimension was found |
| CfRadial packed short with scale/offset | Irene packs int8 with scale/offset. **Gap**: no real int16 packed field |
| CfRadial float field with `_FillValue` | **gap** in classic files. The xsapr and DOW8 netCDF-4 originals have float fields |
| Two PPI sweeps with per-ray PRT, unambiguous range and sample counts | Irene: 2 sweeps, per-ray `prt`, `prt_ratio`, `unambiguous_range`, `n_samples`, `nyquist_velocity` |

## Derivation recipes

All scripts were run with the Python venv named in the wave 1 plan:
netCDF4-python 1.7.4, h5py 3.16.0, numpy 2.5.3.

### `convert_cfradial.py` (from BowEcho): netCDF-4 to classic container

Run as `convert_cfradial.py in.nc out.nc [FIELD,FIELD,...]`. It makes a raw
copy of each variable. It turns off mask/scale, keeps `_FillValue`, copies
all attributes and dimensions (keeping any unlimited dimension), and drops
`(time, range)` fields that are not listed.

```python
import sys, netCDF4
def main(src_path, dst_path, keep_fields=None):
    src = netCDF4.Dataset(src_path)
    dst = netCDF4.Dataset(dst_path, "w", format="NETCDF3_CLASSIC")
    field_vars = {k for k, v in src.variables.items() if v.dimensions == ("time", "range")}
    drop = field_vars - set(keep_fields) if keep_fields is not None else set()
    dst.setncatts({k: src.getncattr(k) for k in src.ncattrs()})
    for name, dim in src.dimensions.items():
        dst.createDimension(name, None if dim.isunlimited() else len(dim))
    for name, var in src.variables.items():
        if name in drop:
            continue
        var.set_auto_maskandscale(False)
        fill = var.getncattr("_FillValue") if "_FillValue" in var.ncattrs() else None
        out = dst.createVariable(name, var.dtype, var.dimensions, fill_value=fill)
        out.set_auto_maskandscale(False)
        out.setncatts({k: var.getncattr(k) for k in var.ncattrs() if k != "_FillValue"})
        if var.shape:
            out[:] = var[:]
        else:
            out[()] = var[()]
    src.close(); dst.close()
if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], sys.argv[3].split(",") if len(sys.argv) > 3 else None)
```

### `cfrad_subset.py`: first N sweeps and chosen fields of a classic CfRadial

This script produced `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` with
`cfrad_subset.py <member> out.nc 2 DBZ,VEL`. Running it twice gives
identical bytes. Every kept variable and attribute was compared with the
source volume, and all were byte-identical.

```python
import sys, netCDF4
src_path, dst_path, nsweeps, keep = sys.argv[1], sys.argv[2], int(sys.argv[3]), set(sys.argv[4].split(","))
src = netCDF4.Dataset(src_path)
assert src.file_format == "NETCDF3_CLASSIC"
nrays = int(src.variables["sweep_end_ray_index"][nsweeps - 1]) + 1
assert int(src.variables["sweep_start_ray_index"][0]) == 0
dst = netCDF4.Dataset(dst_path, "w", format="NETCDF3_CLASSIC")
dst.setncatts({k: src.getncattr(k) for k in src.ncattrs()})
for name, dim in src.dimensions.items():
    dst.createDimension(name, None if dim.isunlimited() else {"time": nrays, "sweep": nsweeps}.get(name, len(dim)))
fields = {k for k, v in src.variables.items() if v.dimensions == ("time", "range")}
for name, var in src.variables.items():
    if name in fields and name not in keep:
        continue
    var.set_auto_maskandscale(False)
    fill = var.getncattr("_FillValue") if "_FillValue" in var.ncattrs() else None
    out = dst.createVariable(name, var.dtype, var.dimensions, fill_value=fill)
    out.set_auto_maskandscale(False)
    out.setncatts({k: var.getncattr(k) for k in var.ncattrs() if k != "_FillValue"})
    index = tuple(slice(0, nrays) if d == "time" else slice(0, nsweeps) if d == "sweep" else slice(None) for d in var.dimensions)
    if var.shape:
        out[:] = var[index]
    else:
        out[()] = var[()]
src.close(); dst.close()
```

The source archive is 1.96 GB, so it is not a manifest entry. Use streaming
extraction: the member is the first file in `sr2_winds.tar.gz`, and reading
can stop once it is out.

```python
import tarfile, urllib.request
name = "sr2_winds/cfrad.20110827_120420.760_to_20110827_120802.081_CPOLRVP_IRENE_WINDS_SUR.nc"
r = urllib.request.urlopen("https://zenodo.org/records/3494891/files/sr2_winds.tar.gz")
t = tarfile.open(fileobj=r, mode="r|gz")
for m in t:
    if m.name == name:
        open("member.nc", "wb").write(t.extractfile(m).read())
        break
r.close()
```

The member's sha256 is in the manifest `derivation`.

### `tarslice.py`: one JMA station per tar

The member's original 512-byte ustar header block and its data blocks are
copied without change. Zero blocks are then added up to the next
10240-byte record, with at least two zero blocks.

```python
import sys
src, pattern, dst = sys.argv[1], sys.argv[2], sys.argv[3]
data = open(src, "rb").read(); pos = 0; out = None
while pos + 512 <= len(data):
    hdr = data[pos:pos + 512]
    if hdr == b"\0" * 512: break
    size = int(hdr[124:136].rstrip(b"\0 ").decode() or "0", 8)
    nblocks = (size + 511) // 512
    if pattern in hdr[0:100].rstrip(b"\0").decode():
        assert out is None; out = data[pos:pos + 512 + nblocks * 512]
    pos += 512 + nblocks * 512
total = ((len(out) + 1024 + 10239) // 10240) * 10240
open(dst, "wb").write(out + b"\0" * (total - len(out)))
```

It was run as `tarslice.py <jma-n5-20191012-090000> RS47773 out.tar`, and
the same way for N6.

### Head trims (DORADE)

- `dorade-dow6-20211230-222139-rhi-head41`: `head -c 1471504 <member>`.
  Offset 1,471,504 is where the RYIB block of ray 41 starts. The block walk
  from the start of the file gives these RYIB offsets: ray 0 at 10,372,
  ray 40 at 1,432,764, ray 41 at 1,471,504. The member comes from a 9.2 GB
  zip. Read the zip central directory with HTTP range requests (Zenodo
  supports them), then read the member's local header and deflate stream
  (compressed size 4,488,121 bytes). About 4.5 MB is transferred. A Python
  `zipfile.ZipFile` over a seekable file-like object that issues `Range`
  requests is enough.
- `dorade-cow2-20260521-225514-sur-head24`: carried over as is (see above).

### Archive members

`tar -xzf 2009.NOX.sweep.0501.tar.gz <member>` and the same for 0525, using
the member paths in the manifest. The `.../all/` paths in these archives are
hard links to the same files. For the Level III file, run
`gzip -dc NWS_NEXRAD_NXL3_KBMX_19980416000000_19980416235959.tar.Z | tar -x KBMX_SDUS54_NVWBMX_199804160006`.

## Notes on the new sources

- **OPERA ORD archive**: `https://s3.waw3-1.cloudferro.com/openradar-archive/{yyyy}/{mm}/{dd}/{CC}/{site}/PVOL/{site}@{stamp}@{elevs}@{moments}.h5`.
  Listing is anonymous S3 `ListObjectsV2`. Data is EUMETNET OPERA, CC BY 4.0.
  The manifest URLs encode `@` as `%40`, and both spellings return the same
  bytes. The two volumes were picked from surveys of daily file sizes as
  sub-2 MB volumes with echo.
- **Irene SMART-R2**: from Alford, Biggerstaff and Bodine (2019), "Data for
  'Transition of the hurricane boundary layer during the landfall of
  Hurricane Irene (2011)'", doi:10.5281/zenodo.3494891, CC BY 4.0. The files
  were converted from Sigmet RAW by RadxConvert in 2019 and kept the classic
  container. The same record's `sr2_rhi.tar.gz` (3.2 MB) holds two native
  classic CfRadial **RHI** files of about 2.6 MB each. They are over the size
  cap and are not used, but they would be a real classic RHI if one is needed.
- **VORTEX-2 NOXP**: Mansell and Burgess (2024), doi:10.5281/zenodo.14194361,
  CC BY 4.0. Sweep files were written by `sigmet_dorade` as little-endian,
  uncompressed DORADE with 1001 gates. The May 2009 archives inspected (0501,
  0507, 0509, 0510, 0513, 0522, 0525, 0527B, 0530) hold only single-tilt
  volumes. The June 2009 archives inspected (0601A, 0601B, 0604-0607, 0609,
  0610, 0612, 0614) hold 12- and 18-sweep volumes with sweep files of
  3.7-5.4 MB, which is over the cap.
- **FARM Marshall Fire DOW6**: Wurman and Kosiba (2023),
  doi:10.48514/JKJ0-TE44, CC BY 4.0. The zip holds 399 files: 396 `_SUR_`
  sweeps (2.5-68 MB) and 3 `_RHI_` sweeps (6.3, 6.3 and 14.5 MB).
- **JMA via NICT**: `https://pawr.nict.go.jp/jmadata/JMA-PolarCoordsRadar/{yyyy}/{mm}/{dd}/`
  keeps N5/N6 tars back to 2017. Some days are missing, for example
  2026/01/15. The mirror states no redistribution terms.
- **NEXRAD Level III archive**: the Google Cloud public bucket
  `gcp-public-data-nexrad-l3` keeps NCEI day tars back to the 1990s.

## Licensing and attribution

- The IMGW-PIB datastore terms require attribution to "Instytut Meteorologii
  i Gospodarki Wodnej – Państwowy Instytut Badawczy" and a note when the data
  has been processed. The fixtures are unmodified.
- OPERA ORD (espdg, iesha, dkrom) is CC BY 4.0. Attribute EUMETNET OPERA and
  the national services: AEMET, Met Éireann, DMI.
- The Zenodo datasets (Irene, NOXP, Marshall Fire) are CC BY 4.0. Cite the
  DOIs above. Derived fixtures are marked as derived in the manifest.
- wradlib-data and open-radar-data are MIT. The Py-ART test data is
  BSD-3-Clause.
- **Unresolved**: the terms for JMA data from the NICT mirror
  (`license:unknown`), and the source and license of the COW2 head24 fixture
  (`license:unknown`). Review both before publishing the repository.

## Tags

Tags used in this manifest:

- `provider:*` and `license:*` give the source and license.
- `carryover:bowecho` marks files carried over from BowEcho.
- `era:YYYY`, `site:*`, `country:*`, `network:*` and `project:*` identify
  time and place.
- `object:pvol|image`, `odim:h5rad-*`, `hdf5:*` and `dtype:*` describe
  ODIM_H5 and HDF5 structure.
- `container:netcdf3-classic|netcdf4`, `cfradial:*` and `writer:*` describe
  netCDF files.
- `dorade:rle|uncompressed|csfd|celv` and `endian:*` describe DORADE files.
- `scan:ppi|rhi|sector|sur` gives the scan type.
- `moments:*` lists the fields.
- `regime:*` and `echo:*` describe the weather.
- `quirk:*` marks writer oddities that decoders must handle.
- `replaces:cfrad_synth|odim_pvol_synth` marks the real replacements for the
  synthetic fixtures.
- `derived` and `derivation:container-conversion|subset|head-trim|archive-member`
  mark derived files.
- `archive` and `contains:*` mark source archives.
- `sweepset:*` groups consecutive DORADE sweeps.

## Gaps

1. **No real classic CfRadial file with an UNLIMITED `time` dimension.** The
   record-interleaved read path has no real coverage. BowEcho searched and
   found every public CfRadial sample to be netCDF-4. This search found only
   the fixed-dimension Irene files.
2. **No real ODIM file with a contiguous (unchunked) data layout.** Every
   public PVOL checked uses chunked+gzip.
3. **No multi-elevation DORADE volume under 2 MB per sweep.** NOXP 2009 June
   volumes and FARM DOW6 volumes are real multi-sweep sets but their sweeps
   are 3.7-68 MB. They are available only inside large tar.gz or zip
   archives, and the testdata crate cannot fetch archive members. The
   committed DORADE set is single-tilt: two consecutive NOXP volumes, one
   NOXP sector sweep with echo, and trimmed COW2 and DOW6 sweeps.
4. **No full DORADE RHI sweep.** The DOW6 RHI is trimmed to rays 0-40 (30.0
   to 13.0 deg). The full 6.3 MB member can be read from the Zenodo zip with
   range requests.
5. **No public source for the COW2 fixture.** The original sweep file and its
   URL are unknown, and so is its license.
6. **Expired URLs**: espdg (ORD 24h bucket) and the IMGW datastore. The
   committed copies are the only copies.
7. **License review needed** for the JMA/NICT data and the COW2 file (see
   Licensing and attribution).
8. **Possible id overlap with the Level III stream**: the Level III entries
   here use the `l3-` prefix. Check for collisions when `level3` merges.
