//! Routing-equivalence tests for `decode_supported_volume_bytes` against the
//! format crates' REAL format-validation fixtures.
//!
//! The shared magic-byte router (DORADE → ODIM_H5 → CfRadial classic netCDF →
//! NEXRAD Archive II) must hand every fixture to the same decoder the
//! format-specific tests call directly, with an identical decoded volume (or
//! an identical stringified error for the pinned netCDF-4 rejection case).
//!
//! Fixtures live in the format crates' `tests/data` directories; provenance
//! is documented in `recast-radar-io-odim/tests/odim_real_files.rs`,
//! `recast-radar-io-odim/tests/odim_real.rs`,
//! `recast-radar-io-cfradial/tests/cfradial_real_files.rs`, and
//! `recast-radar-io-dorade/tests/dorade_real.rs`. ODIM_H5 is
//! the EUMETNET OPERA Data Information Model (Michelson et al., OPERA WP
//! 2.1/2.2, v2.2-2.3). The OPERA ORD volumes (iesha, dkrom), the Irene
//! CfRadial and the NEXRAD Archive II volumes come from the real corpus
//! (`recast_radar_testdata`, ids in `testdata/**/manifest.toml`); the
//! Archive II radial counts are Py-ART / MetPy values from
//! `tools/golden_io_formats.py`, section `router`.

use recast_radar_core::{MomentStorage, RadarVolume};
use recast_radar_io::decode_supported_volume_bytes;
use recast_radar_io_odim::odim_cartesian::decode_odim_h5_cartesian_max;

const BEJAB: &[u8] = include_bytes!("../../recast-radar-io-odim/tests/data/bejab.pvol.hdf");
const BEWID: &[u8] = include_bytes!(
    "../../recast-radar-io-odim/tests/data/20130429043000.rad.bewid.pvol.dbzh.scan1.hdf"
);
const NORST: &[u8] =
    include_bytes!("../../recast-radar-io-odim/tests/data/T_PAGZ35_C_ENMI_20170421090837.hdf");
const ESPDG: &[u8] =
    include_bytes!("../../recast-radar-io-odim/tests/data/espdg.pvol.20260707.dbzh_vradh.h5");
const IMGW_KDP_MAX: &[u8] =
    include_bytes!("../../recast-radar-io-odim/tests/data/imgw_polrad/2026071100150601KDP.max.h5");
const XSAPR_PPI: &[u8] = include_bytes!(
    "../../recast-radar-io-cfradial/tests/data/cfrad.xsapr_sgp_ppi_20110520.classic.nc"
);
const XSAPR_PPI_NETCDF4: &[u8] = include_bytes!(
    "../../recast-radar-io-cfradial/tests/data/cfrad.xsapr_sgp_ppi_20110520.netcdf4.nc"
);
const DOW8_RHI: &[u8] = include_bytes!(
    "../../recast-radar-io-cfradial/tests/data/cfrad.20211011_223602_DOW8_RHI.trim3.nc"
);
const COW2_SWEEP: &[u8] = include_bytes!(
    "../../recast-radar-io-dorade/tests/data/swp.1260521225514.COW2.229.1.0_SUR_v215.head24"
);

fn corpus(id: &str) -> Vec<u8> {
    recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{err}"))
}

fn assert_routed_matches_direct(
    bytes: &[u8],
    direct: Result<RadarVolume, String>,
    expected_site: &str,
    what: &str,
) {
    let direct = direct.unwrap_or_else(|err| panic!("direct decode of {what} failed: {err}"));
    let routed = decode_supported_volume_bytes(bytes)
        .unwrap_or_else(|err| panic!("routed decode of {what} failed: {err}"));
    assert_eq!(routed.site.id, expected_site, "{what} site id");
    assert!(!routed.cuts.is_empty(), "{what} decoded no cuts");
    // Float moment planes carry NaN fills, and `NaN != NaN` under PartialEq
    // would fail even for byte-identical decodes — compare F32 storage
    // bitwise, everything else structurally.
    assert_eq!(
        f32_plane_bits(&routed),
        f32_plane_bits(&direct),
        "{what}: routed F32 planes != direct decode"
    );
    assert_eq!(
        with_f32_planes_cleared(routed),
        with_f32_planes_cleared(direct),
        "{what}: routed != direct decode"
    );
}

/// Every F32 moment plane in cut/moment iteration order, as raw bit patterns.
fn f32_plane_bits(volume: &RadarVolume) -> Vec<Vec<u32>> {
    volume
        .cuts
        .iter()
        .flat_map(|cut| cut.moments.values())
        .filter_map(|grid| match &grid.storage {
            MomentStorage::F32(values) => {
                Some(values.iter().map(|value| value.to_bits()).collect())
            }
            _ => None,
        })
        .collect()
}

/// The same volume with F32 plane contents emptied (compared bitwise above).
fn with_f32_planes_cleared(mut volume: RadarVolume) -> RadarVolume {
    for cut in &mut volume.cuts {
        for grid in cut.moments.values_mut() {
            if let MomentStorage::F32(values) = &mut grid.storage {
                values.clear();
            }
        }
    }
    volume
}

#[test]
fn router_matches_direct_odim_decoder_on_real_pvols() {
    for (bytes, site, what) in [
        (BEJAB, "BEJAB", "bejab.pvol.hdf"),
        (BEWID, "BEWID", "bewid scan1.hdf"),
        (NORST, "NORST", "T_PAGZ35 ENMI .hdf"),
        (ESPDG, "ESPDG", "espdg v2-OHDR dbzh_vradh.h5"),
    ] {
        assert_routed_matches_direct(
            bytes,
            recast_radar_io_odim::odim::decode_odim_h5_volume(bytes).map_err(|err| err.to_string()),
            site,
            what,
        );
    }
    // OPERA ORD archive objects: source NOD:iesha and NOD:dkrom (h5py).
    for (id, site) in [
        ("odim-iesha-20260305-0115-pvol", "IESHA"),
        ("odim-dkrom-20260820-1130-pvol", "DKROM"),
    ] {
        let bytes = corpus(id);
        assert_routed_matches_direct(
            &bytes,
            recast_radar_io_odim::odim::decode_odim_h5_volume(&bytes)
                .map_err(|err| err.to_string()),
            site,
            id,
        );
    }
}

#[test]
fn router_matches_direct_cfradial_decoder_on_classic_netcdf() {
    for (bytes, site, what) in [
        (XSAPR_PPI, "xsapr-sgp", "X-SAPR classic PPI"),
        (DOW8_RHI, "DOW8", "DOW8 native RHI"),
    ] {
        assert_routed_matches_direct(
            bytes,
            recast_radar_io_cfradial::cfradial::decode_cfradial1_volume(bytes)
                .map_err(|err| err.to_string()),
            site,
            what,
        );
    }
    // Radx-written classic CfRadial 1.3: instrument_name CPOLRVP (netCDF4).
    let irene = corpus("cfrad1-irene-sr2-20110827-120420-sur-sweeps01");
    assert_routed_matches_direct(
        &irene,
        recast_radar_io_cfradial::cfradial::decode_cfradial1_volume(&irene)
            .map_err(|err| err.to_string()),
        "CPOLRVP",
        "Irene SMART-R2 classic PPI",
    );
}

#[test]
fn router_matches_direct_dorade_decoder_on_real_cow2_sweep() {
    assert_routed_matches_direct(
        COW2_SWEEP,
        recast_radar_io_dorade::dorade::decode_dorade_sweep_volume(COW2_SWEEP)
            .map_err(|err| err.to_string()),
        "COW2",
        "COW2 sweepfile head24",
    );
}

#[test]
fn router_sends_netcdf4_cfradial_to_the_hdf5_side_like_the_app_chains_did() {
    // netCDF-4 is an HDF5 container: the router must dispatch it to the ODIM
    // decoder (HDF5 magic outranks netCDF), reproducing the historical app
    // routing chains and their conversion-guidance error text exactly.
    let direct_err = recast_radar_io_odim::odim::decode_odim_h5_volume(XSAPR_PPI_NETCDF4)
        .expect_err("netCDF-4 CfRadial must not decode as ODIM")
        .to_string();
    let routed_err = decode_supported_volume_bytes(XSAPR_PPI_NETCDF4)
        .expect_err("netCDF-4 CfRadial must not decode through the router")
        .to_string();
    assert_eq!(routed_err, direct_err);
}

/// The KLIX 2021-08-29 model-data (`_MDM`) file: an LDM record without an
/// Archive II volume header holding one Message 29. The magic-byte router
/// sends it to the Level II decoder, which rejects it (before wave 3 it read
/// the compressed bytes as records); the router surfaces that error.
#[test]
fn router_rejects_model_data_file_like_the_direct_decoder() {
    let path = recast_radar_testdata::require_file!("l2-klix-20210829-175748-mdm");
    let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let direct_err = recast_radar_io_nexrad::decode_volume_from_bytes(&bytes)
        .expect_err("model-data file must not decode")
        .to_string();
    let routed_err = decode_supported_volume_bytes(&bytes)
        .expect_err("model-data file must not decode")
        .to_string();
    assert_eq!(routed_err, direct_err);
    assert!(
        routed_err.starts_with("no Archive II volume header"),
        "unexpected error text: {routed_err}"
    );
}

#[test]
fn image_decoder_and_volume_router_remain_separate() {
    let volume_error = decode_supported_volume_bytes(IMGW_KDP_MAX)
        .expect_err("IMAGE must not route into RadarVolume")
        .to_string();
    assert!(
        volume_error.contains("PVOL and SCAN only"),
        "{volume_error}"
    );

    // /what object = PVOL (h5py) is not a Cartesian IMAGE.
    let pvol = corpus("odim-iesha-20260305-0115-pvol");
    let image_error =
        decode_odim_h5_cartesian_max(&pvol).expect_err("PVOL must not route into Cartesian grid");
    assert!(image_error.to_string().contains("is not a Cartesian IMAGE"));
}

/// Real Archive II volumes: AR2V0006 LDM bzip2 records (KTLX 2024 trim),
/// ARCHIVE2 Message 1 records (KTLX 1999 trim), and a whole-file gzip object
/// with Message 31 radials (KPAH 2008 AR2V0004, a download entry). Py-ART
/// `read_nexrad_archive` and MetPy `Level2File` agree on the counts.
const ARCHIVE_II: [(&str, usize, &[usize]); 3] = [
    ("l2-ktlx-20240315-000217-trim", 960, &[480, 480]),
    ("l2-ktlx-19990504-002218-trim", 734, &[367, 367]),
    ("l2-kpah-20080415-235014", 2520, &[360; 7]),
];

fn archive_ii_bytes(id: &str) -> Option<Vec<u8>> {
    match recast_radar_testdata::path(id) {
        Ok(path) => Some(std::fs::read(path).expect("read Archive II volume")),
        Err(err) if err.is_offline() => {
            eprintln!("skipping {id}: {err}");
            None
        }
        Err(err) => panic!("{err}"),
    }
}

#[test]
fn router_decodes_real_archive_ii_same_as_direct_decoder() {
    for (id, radials, per_sweep) in ARCHIVE_II {
        let Some(bytes) = archive_ii_bytes(id) else {
            continue;
        };
        let direct = recast_radar_io_nexrad::decode_volume_from_bytes(&bytes)
            .unwrap_or_else(|err| panic!("direct decode {id}: {err}"));
        let routed = decode_supported_volume_bytes(&bytes)
            .unwrap_or_else(|err| panic!("routed decode {id}: {err}"));
        assert_eq!(routed, direct, "{id}");
        assert_eq!(routed.metadata.decoded_radial_count, radials, "{id}");
        let counts: Vec<usize> = routed.cuts.iter().map(|cut| cut.radials.len()).collect();
        assert_eq!(counts, per_sweep, "{id}");
    }
}

#[test]
fn router_matches_direct_archive_ii_decoder_on_real_volumes() {
    // Site ids: MetPy station KTLX / KPAH; the 1999 ARCHIVE2 header carries a
    // NUL ICAO (MetPy reads four NUL bytes), so its id comes from the decoder
    // fallback and is compared with the direct decode only.
    for (id, _, _) in ARCHIVE_II {
        let Some(bytes) = archive_ii_bytes(id) else {
            continue;
        };
        let direct =
            recast_radar_io_nexrad::decode_volume_from_bytes(&bytes).map_err(|err| err.to_string());
        let site = match id {
            "l2-kpah-20080415-235014" => "KPAH".to_owned(),
            "l2-ktlx-20240315-000217-trim" => "KTLX".to_owned(),
            _ => direct.as_ref().expect("direct decode").site.id.clone(),
        };
        assert_routed_matches_direct(&bytes, direct, &site, id);
    }
}
