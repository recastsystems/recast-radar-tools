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
//! 2.1/2.2, v2.2-2.3). No real Archive II file ships in `tests/data` (Level
//! II volumes are megabytes), so the Archive II leg uses the same minimal
//! synthetic single-radial volume the `recast-radar-io-nexrad` unit tests
//! pin, rebuilt here byte-for-byte.

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
const ODIM_SYNTH: &[u8] = include_bytes!("../../recast-radar-io-odim/tests/data/odim_pvol_synth.h5");
const IMGW_KDP_MAX: &[u8] = include_bytes!(
    "../../recast-radar-io-odim/tests/data/imgw_polrad/2026071100150601KDP.max.h5"
);
const XSAPR_PPI: &[u8] = include_bytes!(
    "../../recast-radar-io-cfradial/tests/data/cfrad.xsapr_sgp_ppi_20110520.classic.nc"
);
const XSAPR_PPI_NETCDF4: &[u8] = include_bytes!(
    "../../recast-radar-io-cfradial/tests/data/cfrad.xsapr_sgp_ppi_20110520.netcdf4.nc"
);
const DOW8_RHI: &[u8] = include_bytes!(
    "../../recast-radar-io-cfradial/tests/data/cfrad.20211011_223602_DOW8_RHI.trim3.nc"
);
const CFRADIAL_SYNTH: &[u8] = include_bytes!("../../recast-radar-io-cfradial/tests/data/cfrad_synth.nc");
const COW2_SWEEP: &[u8] = include_bytes!(
    "../../recast-radar-io-dorade/tests/data/swp.1260521225514.COW2.229.1.0_SUR_v215.head24"
);

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
        (ODIM_SYNTH, "TEST", "odim_pvol_synth.h5"),
    ] {
        assert_routed_matches_direct(
            bytes,
            recast_radar_io_odim::odim::decode_odim_h5_volume(bytes).map_err(|err| err.to_string()),
            site,
            what,
        );
    }
}

#[test]
fn router_matches_direct_cfradial_decoder_on_classic_netcdf() {
    for (bytes, site, what) in [
        (XSAPR_PPI, "xsapr-sgp", "X-SAPR classic PPI"),
        (DOW8_RHI, "DOW8", "DOW8 native RHI"),
        (CFRADIAL_SYNTH, "SYNTH1", "cfrad_synth.nc"),
    ] {
        assert_routed_matches_direct(
            bytes,
            recast_radar_io_cfradial::cfradial::decode_cfradial1_volume(bytes).map_err(|err| err.to_string()),
            site,
            what,
        );
    }
}

#[test]
fn router_matches_direct_dorade_decoder_on_real_cow2_sweep() {
    assert_routed_matches_direct(
        COW2_SWEEP,
        recast_radar_io_dorade::dorade::decode_dorade_sweep_volume(COW2_SWEEP).map_err(|err| err.to_string()),
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

#[test]
fn image_decoder_and_volume_router_remain_separate() {
    let volume_error = decode_supported_volume_bytes(IMGW_KDP_MAX)
        .expect_err("IMAGE must not route into RadarVolume")
        .to_string();
    assert!(
        volume_error.contains("PVOL and SCAN only"),
        "{volume_error}"
    );

    let image_error = decode_odim_h5_cartesian_max(ODIM_SYNTH)
        .expect_err("PVOL must not route into Cartesian grid");
    assert!(image_error.to_string().contains("is not a Cartesian IMAGE"));
}

#[test]
fn router_decodes_synthetic_archive_ii_same_as_direct_decoder() {
    let bytes = synthetic_archive_ii();
    let direct = recast_radar_io_nexrad::decode_volume_from_bytes(&bytes)
        .expect("direct Archive II decode");
    let routed = decode_supported_volume_bytes(&bytes).expect("routed Archive II decode");
    assert_eq!(routed, direct);
}

#[test]
fn router_matches_direct_archive_ii_decoder_on_synthetic_volume() {
    let bytes = synthetic_archive_ii();
    assert_routed_matches_direct(
        &bytes,
        recast_radar_io_nexrad::decode_volume_from_bytes(&bytes).map_err(|err| err.to_string()),
        "KTLX",
        "synthetic Archive II",
    );
}

// --- Minimal Archive II builder (mirrors the in-crate unit-test fixture) ---

const VOLUME_HEADER_LEN: usize = 24;
const CONTROL_WORD_LEN: usize = 12;
const MESSAGE_HEADER_LEN: usize = 16;
const RECORD_BYTES: usize = 2432;
const MSG_31_HEADER_LEN: usize = 72;

/// One uncompressed AR2V record holding a single Message 31 radial with REF
/// and VEL moments — the same shape as `synthetic_archive(false)` in the
/// crate's unit tests.
fn synthetic_archive_ii() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"AR2V00000");
    bytes.extend_from_slice(b"1  ");
    bytes.extend_from_slice(&19_724u32.to_be_bytes());
    bytes.extend_from_slice(&1_000u32.to_be_bytes());
    bytes.extend_from_slice(b"KTLX");

    bytes.extend_from_slice(&[0u8; CONTROL_WORD_LEN]);
    let body = synthetic_message_31_body();
    let message_size = u16::try_from((MESSAGE_HEADER_LEN + body.len()) / 2).unwrap();
    bytes.extend_from_slice(&message_size.to_be_bytes());
    bytes.push(0);
    bytes.push(31);
    bytes.extend_from_slice(&7u16.to_be_bytes());
    bytes.extend_from_slice(&19_724u16.to_be_bytes());
    bytes.extend_from_slice(&1_000u32.to_be_bytes());
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&body);
    bytes.resize(VOLUME_HEADER_LEN + RECORD_BYTES, 0);
    bytes
}

fn synthetic_message_31_body() -> Vec<u8> {
    let mut body = vec![0u8; MSG_31_HEADER_LEN];
    body[0..4].copy_from_slice(b"AR2V");
    body[4..8].copy_from_slice(&1_000u32.to_be_bytes());
    body[8..10].copy_from_slice(&19_724u16.to_be_bytes());
    body[10..12].copy_from_slice(&1u16.to_be_bytes());
    body[12..16].copy_from_slice(&180.5f32.to_bits().to_be_bytes());
    body[18..20].copy_from_slice(&1u16.to_be_bytes());
    body[20] = 2;
    body[21] = 3;
    body[22] = 1;
    body[23] = 1;
    body[24..28].copy_from_slice(&0.5f32.to_bits().to_be_bytes());
    body[30..32].copy_from_slice(&4u16.to_be_bytes());

    let vol_pointer = body.len();
    push_volume_block(&mut body);
    let rad_pointer = body.len();
    push_radial_block(&mut body);
    let ref_pointer = body.len();
    push_u8_moment(&mut body, b"DREF", &[0, 66, 80]);
    let vel_pointer = body.len();
    push_u8_moment(&mut body, b"DVEL", &[129, 139, 119]);

    set_pointer(&mut body, 0, vol_pointer);
    set_pointer(&mut body, 2, rad_pointer);
    set_pointer(&mut body, 3, ref_pointer);
    set_pointer(&mut body, 4, vel_pointer);
    body
}

fn push_volume_block(body: &mut Vec<u8>) {
    body.extend_from_slice(b"RVOL");
    body.extend_from_slice(&1u16.to_be_bytes());
    body.push(1);
    body.push(0);
    body.extend_from_slice(&35.333f32.to_bits().to_be_bytes());
    body.extend_from_slice(&(-97.277f32).to_bits().to_be_bytes());
    body.extend_from_slice(&370i16.to_be_bytes());
    body.extend_from_slice(&20u16.to_be_bytes());
    for _ in 0..5 {
        body.extend_from_slice(&0.0f32.to_bits().to_be_bytes());
    }
    body.extend_from_slice(&212u16.to_be_bytes());
    body.extend_from_slice(&0u16.to_be_bytes());
}

fn push_radial_block(body: &mut Vec<u8>) {
    body.extend_from_slice(b"RRAD");
    body.extend_from_slice(&1u16.to_be_bytes());
    body.extend_from_slice(&0i16.to_be_bytes());
    body.extend_from_slice(&0.0f32.to_bits().to_be_bytes());
    body.extend_from_slice(&0.0f32.to_bits().to_be_bytes());
    body.extend_from_slice(&2_500i16.to_be_bytes());
    body.extend_from_slice(&0u16.to_be_bytes());
}

fn push_u8_moment(body: &mut Vec<u8>, id: &[u8; 4], gates: &[u8]) {
    body.extend_from_slice(id);
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&(gates.len() as u16).to_be_bytes());
    body.extend_from_slice(&0i16.to_be_bytes());
    body.extend_from_slice(&250i16.to_be_bytes());
    body.extend_from_slice(&0i16.to_be_bytes());
    body.extend_from_slice(&0i16.to_be_bytes());
    body.push(0);
    body.push(8);
    body.extend_from_slice(&2.0f32.to_bits().to_be_bytes());
    body.extend_from_slice(&66.0f32.to_bits().to_be_bytes());
    body.extend_from_slice(gates);
    if !body.len().is_multiple_of(2) {
        body.push(0);
    }
}

fn set_pointer(body: &mut [u8], pointer_index: usize, value: usize) {
    let offset = 32 + pointer_index * 4;
    body[offset..offset + 4].copy_from_slice(&(value as u32).to_be_bytes());
}
