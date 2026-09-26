//! HDF5 filters this crate does not decode, on real data: the X-SAPR
//! netCDF-4 CfRadial 1 PPI with its reflectivity stored twice more, one
//! chunk szip-compressed (filter 4, by netCDF-C with libaec) and one chunk
//! LZF-compressed (filter 32000, by h5py): `tools/derive_hdf5_filters.py`,
//! which checks both read back equal to `reflectivity_horizontal` through
//! netCDF4-python and h5py.

use recast_radar_hdf5::{Error, H5File, StorageLayout};

const ID: &str = "cfrad1-xsapr-sgp-20110520-ppi-netcdf4-szip-lzf";

#[test]
fn szip_and_lzf_planes_are_typed_unsupported_filter_errors() {
    let bytes = recast_radar_testdata::bytes(ID).unwrap_or_else(|err| panic!("{err}"));
    let file = H5File::open(&bytes).unwrap_or_else(|err| panic!("{err}"));
    // The source plane (deflate) reads.
    let source = file
        .dataset("/reflectivity_horizontal")
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(source.dims, [40, 42]);
    for (path, filter) in [("/reflectivity_szip", 4u16), ("/reflectivity_lzf", 32000)] {
        let info = file
            .dataset_info(path)
            .unwrap_or_else(|err| panic!("{path}: {err}"));
        assert_eq!(info.dims, [40, 42], "{path}");
        assert!(
            matches!(&info.layout, StorageLayout::Chunked { chunk_dims, .. } if chunk_dims == &[40, 42]),
            "{path}: {:?}",
            info.layout
        );
        let ids: Vec<u16> = info.filters.iter().map(|f| f.id).collect();
        assert_eq!(ids, [filter], "{path}");
        // The chunk index reads: one chunk.
        let chunks = file
            .chunk_locations(path)
            .unwrap_or_else(|err| panic!("{path}: {err}"));
        assert_eq!(chunks.len(), 1, "{path}");
        match file.dataset(path) {
            Err(Error::UnsupportedFilter { id, name }) => {
                assert_eq!(id, filter, "{path}: {name}");
                eprintln!("{path}: filter {id} ({name})");
            }
            other => panic!("{path}: {:?}", other.map(|data| data.dims)),
        }
    }
}
