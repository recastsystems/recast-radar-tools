//! `fuzz-tools`: seed corpora and stable replay for the recast-radar fuzz
//! targets.
//!
//! ```text
//! fuzz-tools seeds [OUT_DIR]          write seeds/<target>/ from the testdata manifest
//! fuzz-tools replay <target> <path>.. run inputs (files or directories) through a harness
//! fuzz-tools regressions              replay the testdata entries tagged fuzz-regression
//! fuzz-tools smoke <target> <n> [DIR] n seeded mutations of every seed of the target
//! fuzz-tools mutate <target> <runs> <rng-seed> <path>..
//!                                     stable mutation fuzzing from those inputs
//!                                     (crashes to artifacts/<target>/; see `mutate`)
//! ```
//!
//! `smoke` is a repeatable stand-in for a fuzzing campaign on machines
//! without libFuzzer: every file under `DIR` (default `seeds/<target>/`) is
//! mutated `n` times with a fixed pseudo-random sequence (bit flips,
//! boundary bytes, overwritten, inserted and deleted spans, truncation; the
//! length changes so every length-selected mode is reached), and each
//! mutant runs through the harness. A panicking mutant is saved under
//! `artifacts/<target>/smoke-*` for `replay`.
//!
//! Seeds are real test files resolved by manifest id through
//! `recast-radar-testdata` (committed fixtures, or sha256-verified downloads
//! cached on first use), copied verbatim or cut down by the derivations in
//! [`Derivation`]. No seed byte is synthesized: derived seeds are record-,
//! block- or chunk-aligned selections and concatenations of real bytes.

use std::fs;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use recast_radar_fuzz::{Harness, TARGETS, harness};

mod mutate;

/// Archive II volume header length.
const L2_VOLUME_HEADER_LEN: usize = 24;
/// Archive II record control word preceding each message header.
const L2_CONTROL_WORD_LEN: usize = 12;
/// Archive II message header length.
const L2_MESSAGE_HEADER_LEN: usize = 16;
/// Fixed Archive II record length (everything except message 31 after the
/// metadata records).
const L2_RECORD_BYTES: usize = 2432;
/// Fixed metadata records at the start of a modern Archive II volume.
const L2_METADATA_RECORDS: usize = 134;

/// How a seed is cut from its real source file(s).
#[derive(Clone, Copy, Debug)]
enum Derivation {
    /// The file, byte for byte.
    Verbatim,
    /// Level II, decompressed to the uncompressed record stream: the volume
    /// header, every metadata record except empty or type-0 records and the
    /// clutter-filter/bypass maps (messages 13 and 15), the first radial
    /// messages (up to 64 KiB) and the last radial messages of the volume (up
    /// to 16 KiB, including the end-of-volume radial). Whole records only.
    L2Sparse,
    /// Level II, decompressed: the volume header, all records before the
    /// first radial message, and the first radial messages (up to 32 KiB) —
    /// the prefix a partial download yields.
    L2Head,
    /// LDM block-bzip2 Level II as published: the volume header and the
    /// leading whole bzip2 blocks, as many as fit in 256 KiB (at least one).
    L2BlockHead,
    /// The bzip2 stream of LDM record `n` of a block-bzip2 Level II file (or
    /// real-time chunk), byte for byte, without its control word.
    LdmRecord(usize),
    /// The decompressed contents of LDM record `n`: the bytes a Level II
    /// writer compresses into that record.
    LdmPayload(usize),
    /// This file followed by the real-time chunk(s) named here, as a
    /// real-time client assembles a volume.
    ConcatChunks(&'static [&'static str]),
    /// DORADE sweepfile: every descriptor block and the first `n` ray groups
    /// (RYIB/ASIB/RDAT...), cut at a block boundary.
    DoradeHead(usize),
}

impl Derivation {
    fn suffix(self) -> String {
        match self {
            Self::Verbatim => String::new(),
            Self::L2Sparse => ".l2-sparse".to_owned(),
            Self::L2Head => ".l2-head".to_owned(),
            Self::L2BlockHead => ".l2-block-head".to_owned(),
            Self::LdmRecord(n) => format!(".ldm-record{n}"),
            Self::LdmPayload(n) => format!(".ldm-payload{n}"),
            Self::ConcatChunks(ids) => format!(".plus-{}-chunks", ids.len()),
            Self::DoradeHead(rays) => format!(".head{rays}"),
        }
    }
}

/// One seed: target, testdata manifest id, derivation.
type Seed = (&'static str, &'static str, Derivation);

use Derivation::{
    ConcatChunks, DoradeHead, L2BlockHead, L2Head, L2Sparse, LdmPayload, LdmRecord, Verbatim,
};

const KIWA_CHUNK_S: &str = "l2chunk-kiwa-307-20260917-003629-001-s";
const KIWA_CHUNK_I2: &str = "l2chunk-kiwa-307-20260917-003629-002-i";

const SEEDS: &[Seed] = &[
    // level2_volume: small real files as published (LDM bzip2 status-only
    // stub, truncated gzip message 1 volume, whole gzip AR2V0001 volume,
    // real-time start chunk, a headerless intermediate chunk, and the two
    // chunks assembled), plus record- and block-aligned cuts of larger
    // volumes. The L2Sparse set covers every archive header generation
    // (ARCHIVE2 1991/1999/2003, AR2V0001 with message 1 and the mislabelled
    // message 31, AR2V0004/6/8), message 1 vs 31, VOL block 44/52, RAD block
    // 20/28, 8- and 16-bit ZDR, CFP, SAILS/MESO-SAILS, TDWR, message 32, and
    // RDA builds 10 through 24.
    ("level2_volume", "l2-tbwi-20230601-175101-stub", Verbatim),
    ("level2_volume", "l2-ktlx-19990503-230052", Verbatim),
    ("level2_volume", "l2-kvwx-20080415-235337", Verbatim),
    ("level2_volume", KIWA_CHUNK_S, Verbatim),
    ("level2_volume", KIWA_CHUNK_I2, Verbatim),
    (
        "level2_volume",
        KIWA_CHUNK_S,
        ConcatChunks(&[KIWA_CHUNK_I2]),
    ),
    ("level2_volume", "l2-kbox-20220129-150537", L2BlockHead),
    ("level2_volume", "l2-klix-20050829-130035", L2Head),
    ("level2_volume", "l2-kpah-20080415-235014", L2Head),
    ("level2_volume", "l2-kiwa-20260917-003629", L2Head),
    ("level2_volume", "l2-ktlx-19910605-162126", L2Sparse),
    ("level2_volume", "l2-ktlx-19990503-230052", L2Sparse),
    ("level2_volume", "l2-ktlx-20030508-221041", L2Sparse),
    ("level2_volume", "l2-klix-20050829-130035", L2Sparse),
    ("level2_volume", "l2-kvwx-20080415-235337", L2Sparse),
    ("level2_volume", "l2-kpah-20080415-235014", L2Sparse),
    ("level2_volume", "l2-kvnx-20110315-000203", L2Sparse),
    ("level2_volume", "l2-koax-20140616-205305", L2Sparse),
    ("level2_volume", "l2-klix-20210829-180425", L2Sparse),
    ("level2_volume", "l2-kbox-20220129-150537", L2Sparse),
    ("level2_volume", "l2-tstl-20230331-230314", L2Sparse),
    ("level2_volume", "l2-pahg-20250909-212549", L2Sparse),
    ("level2_volume", "l2-kiwa-20260917-003629", L2Sparse),
    // level2_writer: small real volumes the decoder accepts, one per case
    // the writer handles differently: a real-time start chunk alone and with
    // the next chunk, Message 1 volumes (a blank ICAO, gates before the
    // radar), the AR2V0001 header over Message 31, legacy resolution, 8- and
    // 16-bit ZDR with CFP, VOL block 52, TDWR, and a volume head.
    ("level2_writer", KIWA_CHUNK_S, Verbatim),
    (
        "level2_writer",
        KIWA_CHUNK_S,
        ConcatChunks(&[KIWA_CHUNK_I2]),
    ),
    ("level2_writer", "l2-ktlx-19910605-162126-trim", Verbatim),
    ("level2_writer", "l2-klix-20050829-130035-trim", Verbatim),
    ("level2_writer", "l2-kvwx-20080415-235337", L2Sparse),
    ("level2_writer", "l2-kpah-20080415-235014", L2Sparse),
    ("level2_writer", "l2-kvnx-20110315-000203", L2Sparse),
    ("level2_writer", "l2-klix-20210829-180425", L2Sparse),
    ("level2_writer", "l2-kbox-20220129-150537", L2Sparse),
    ("level2_writer", "l2-tstl-20230331-230314", L2Sparse),
    ("level2_writer", "l2-kiwa-20260917-003629", L2Head),
    // level2_writer_router: real volumes of the other formats, one per case
    // the writer's quantiser and geometry handle differently: 8-bit ODIM on
    // the typical REF coding and on its own grid (DBZH and VRADH), mixed
    // gains across sweeps (the Norwegian vertical scan), dual-polarization
    // moments with fields left out (TH, LDR); float CfRadial on a 0.01 grid
    // and 8-bit CfRadial with 255 levels (Irene VEL); 16-bit DORADE at 0.01
    // and 0.0001, a sector scan and an RHI (refused); JMA float levels on
    // an uneven table of hundredths.
    (
        "level2_writer_router",
        "odim-espdg-20260707-1927-pvol-dbzh-vradh",
        Verbatim,
    ),
    (
        "level2_writer_router",
        "odim-norst-20170421-0908-pvol",
        Verbatim,
    ),
    (
        "level2_writer_router",
        "odim-dkrom-20260820-1130-pvol",
        Verbatim,
    ),
    (
        "level2_writer_router",
        "cfrad1-xsapr-sgp-20110520-ppi-classic",
        Verbatim,
    ),
    (
        "level2_writer_router",
        "cfrad1-irene-sr2-20110827-120420-sur-sweeps01",
        Verbatim,
    ),
    (
        "level2_writer_router",
        "dorade-cow2-20260521-225514-sur-head24",
        Verbatim,
    ),
    (
        "level2_writer_router",
        "dorade-noxp-20090525-203211-sector",
        DoradeHead(8),
    ),
    (
        "level2_writer_router",
        "dorade-dow6-20211230-222139-rhi-head41",
        DoradeHead(4),
    ),
    (
        "level2_writer_router",
        "jma-n6-20191012-090000-rs47773",
        Verbatim,
    ),
    // io_router: one small real file per routed format, plus gzip and
    // block-bzip Level II.
    ("io_router", "l2-tbwi-20230601-175101-stub", Verbatim),
    ("io_router", "l2-ktlx-19990503-230052", Verbatim),
    ("io_router", KIWA_CHUNK_S, Verbatim),
    ("io_router", "l2-kvnx-20110315-000203", L2Sparse),
    ("io_router", "odim-imgw-ram-20260711-0015-kdp-max", Verbatim),
    (
        "io_router",
        "odim-espdg-20260707-1927-pvol-dbzh-vradh",
        Verbatim,
    ),
    (
        "io_router",
        "cfrad1-xsapr-sgp-20110520-ppi-classic",
        Verbatim,
    ),
    (
        "io_router",
        "cfrad1-xsapr-sgp-20110520-ppi-netcdf4",
        Verbatim,
    ),
    (
        "io_router",
        "dorade-cow2-20260521-225514-sur-head24",
        Verbatim,
    ),
    ("io_router", "jma-n6-20191012-090000-rs47773", Verbatim),
    (
        "io_router",
        "cfrad2-xradar-xsapr-sgp-20110520-ppi",
        Verbatim,
    ),
    // odim: every committed ODIM_H5 file (superblock v0/v1, v2 object
    // headers, vlen strings, float64, Cartesian composites) plus the
    // netCDF-4 (HDF5) CfRadial file.
    ("odim", "odim-bejab-20190606-0000-pvol", Verbatim),
    ("odim", "odim-bewid-20130429-0430-pvol-dbzh-scan1", Verbatim),
    ("odim", "odim-norst-20170421-0908-pvol", Verbatim),
    ("odim", "odim-espdg-20260707-1927-pvol-dbzh-vradh", Verbatim),
    ("odim", "odim-imgw-ram-20260711-0015-kdp-max", Verbatim),
    ("odim", "odim-imgw-ram-20260711-0015-phidp-max", Verbatim),
    ("odim", "odim-imgw-ram-20260711-0015-rhohv-max", Verbatim),
    ("odim", "odim-imgw-ram-20260711-0015-zdr-max", Verbatim),
    ("odim", "odim-iesha-20260305-0115-pvol", Verbatim),
    ("odim", "odim-dkrom-20260820-1130-pvol", Verbatim),
    (
        "odim",
        "odim-dkrom-20260820-1130-pvol-h5latest-trim",
        Verbatim,
    ),
    ("odim", "cfrad1-xsapr-sgp-20110520-ppi-netcdf4", Verbatim),
    // int16 planes, quality groups with legends, nested how groups.
    (
        "odim",
        "odim-seang-20260924-2130-qcvol-dataset1-trim",
        Verbatim,
    ),
    (
        "odim",
        "odim-fianj-20260924-2130-pvol-dataset1-trim",
        Verbatim,
    ),
    ("odim", "odim-deboo-20260924-2130-sweep-th-00", Verbatim),
    ("odim", "odim-itdes-20260924-2135-pvol-class", Verbatim),
    // hdf5: one file per HDF5 layout: superblock v0 (old-style groups, v1
    // B-tree chunks), v0 with vlen strings, v1 with 8-byte sizes, v0 with
    // AEMET's v2 object headers, v2 netCDF-4 (dense links and attributes,
    // fractal heaps, v2 B-trees), and the v3 "latest" container with every
    // version-4 chunk index and huge heap objects.
    ("hdf5", "odim-bejab-20190606-0000-pvol", Verbatim),
    ("hdf5", "odim-bewid-20130429-0430-pvol-dbzh-scan1", Verbatim),
    ("hdf5", "odim-norst-20170421-0908-pvol", Verbatim),
    ("hdf5", "odim-espdg-20260707-1927-pvol-dbzh-vradh", Verbatim),
    ("hdf5", "odim-imgw-ram-20260711-0015-kdp-max", Verbatim),
    ("hdf5", "cfrad1-xsapr-sgp-20110520-ppi-netcdf4", Verbatim),
    (
        "hdf5",
        "odim-dkrom-20260820-1130-pvol-h5latest-trim",
        Verbatim,
    ),
    // Paged extensible-array data blocks, committed datatypes, 4-byte
    // addresses; 4-byte lengths and a deflated dense-link fractal heap.
    (
        "hdf5",
        "odim-dkrom-20260820-1130-pvol-h5edge-paged-ea",
        Verbatim,
    ),
    (
        "hdf5",
        "odim-dkrom-20260820-1130-pvol-h5edge-len4",
        Verbatim,
    ),
    // cfradial: every committed CfRadial 1 and CfRadial 2 file (classic and
    // netCDF-4; Radx and xradar writers).
    (
        "cfradial",
        "cfrad1-xsapr-sgp-20110520-ppi-classic",
        Verbatim,
    ),
    (
        "cfradial",
        "cfrad1-xsapr-sgp-20110520-ppi-netcdf4",
        Verbatim,
    ),
    (
        "cfradial",
        "cfrad1-dow8-20211011-223602-rhi-trim3-classic",
        Verbatim,
    ),
    (
        "cfradial",
        "cfrad1-irene-sr2-20110827-120420-sur-sweeps01",
        Verbatim,
    ),
    (
        "cfradial",
        "cfrad2-radx-irene-sr2-20110827-120420-sur-r30km",
        Verbatim,
    ),
    (
        "cfradial",
        "cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32",
        Verbatim,
    ),
    ("cfradial", "cfrad2-xradar-xsapr-sgp-20110520-ppi", Verbatim),
    (
        "cfradial",
        "cfrad2-xradar-dow8-20211011-223602-rhi-r300",
        Verbatim,
    ),
    // n_gates_vary storage from LROSE Radx: per-ray gate geometry in a
    // range(time, range), and one range in netCDF-4; netCDF-4 user-defined
    // types.
    (
        "cfradial",
        "cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry",
        Verbatim,
    ),
    (
        "cfradial",
        "cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4",
        Verbatim,
    ),
    (
        "cfradial",
        "cfrad1-xsapr-sgp-20110520-ppi-netcdf4-user-types",
        Verbatim,
    ),
    // dorade: the committed sweepfiles, head-trimmed to a few rays
    // (little/big endian, uncompressed/HRD RLE, CSFD/CELV, PPI/RHI/SUR/AIR,
    // ground-based and airborne ASIB values), plus one complete sweepfile
    // for the end-of-sweep path.
    ("dorade", "dorade-cow2-20260521-225514-sur-head24", Verbatim),
    ("dorade", "dorade-noxp-20090501-190244-ppi", DoradeHead(8)),
    (
        "dorade",
        "dorade-noxp-20090525-203211-sector",
        DoradeHead(8),
    ),
    (
        "dorade",
        "dorade-dow6-20211230-222139-rhi-head41",
        DoradeHead(4),
    ),
    ("dorade", "dorade-noxp-20090501-190324-ppi", Verbatim),
    (
        "dorade",
        "dorade-n42rf-tm-20181010-123925-air-head48",
        DoradeHead(4),
    ),
    // dorade_archive: the committed deployment zip (three head-trimmed NOXP
    // sweepfiles of one volume run and three text members).
    (
        "dorade_archive",
        "dorade-noxp-20090610-003210-heads-zip",
        Verbatim,
    ),
    // level3: one real product per framing and packet family: WMO heading
    // (1995 NCEI archive, 2013 KTLX), NOAAPort with zlib frames (KMCI 2016),
    // bzip2 (N0Q, N0B, EET, DVL, DPR), radial 16 and 0xAF1F, raster 0xBA07
    // with graphic pages (NCR), packets 17 and 18 (DPA), the stand-alone
    // rate array (1995 SUP), generic radial and text components (DPR,
    // RSL), symbols and SCIT (NST, NHI, NTV, NMD, NME), cell trends (NSS),
    // contours (N0M), VWP, radar coded message, General Status Message, a
    // free text message, the storm tables of 1996-2003 (legacy STI, hail, TVS
    // and combined attribute tables, both Mesocyclone layouts), the
    // unedited Radar Coded Message (packets 30-32), and the 1993-2001
    // products: cross section (packet 7), weak echo region (eight rasters,
    // packet 7), velocity azimuth display (packet 9), a window with short
    // radials (46), combined shear, a composite reflectivity contour with its
    // attribute table, stand-alone storm tables (101) and a VWP whose tabular
    // offset names the end of its message.
    ("level3", "l3-lot-050-19941031-1358", Verbatim),
    ("level3", "l3-lot-053-19941106-0246", Verbatim),
    ("level3", "l3-lot-084-19931120-0721", Verbatim),
    ("level3", "l3-ftg-046-19940930-1849", Verbatim),
    ("level3", "l3-tlx-087-19940308-1939", Verbatim),
    ("level3", "l3-grr-039-20011011-0631", Verbatim),
    ("level3", "l3-lot-101-19930824-0005", Verbatim),
    ("level3", "l3-tlx-101-20010503-0007", Verbatim),
    ("level3", "l3-lot-nvw-19931120-0721", Verbatim),
    ("level3", "l3-fws-n0r-19950517-2304", Verbatim),
    ("level3", "l3-fws-sup-19950517-2304", Verbatim),
    ("level3", "l3-fws-dpa-19950517-2304", Verbatim),
    ("level3", "l3-fws-rcm-19950517-2310", Verbatim),
    ("level3", "l3-fws-nme-19950517-2316", Verbatim),
    ("level3", "l3-mci-dpa-20160526-2154", Verbatim),
    ("level3", "l3-mci-n1p-20160526-2154", Verbatim),
    ("level3", "l3-tlx-n0v-20130520-2016", Verbatim),
    ("level3", "l3-tlx-ncr-20130520-2016", Verbatim),
    ("level3", "l3-tlx-nst-20130520-2016", Verbatim),
    ("level3", "l3-tlx-nhi-20130520-2016", Verbatim),
    ("level3", "l3-tlx-ntv-20130520-2016", Verbatim),
    ("level3", "l3-tlx-nmd-20130520-2016", Verbatim),
    ("level3", "l3-tlx-nss-20130520-2016", Verbatim),
    ("level3", "l3-tlx-nvw-20130520-2016", Verbatim),
    ("level3", "l3-tlx-rcm-20220503-004553", Verbatim),
    ("level3", "l3-tlx-n0m-20130520-2016", Verbatim),
    ("level3", "l3-tlx-eet-20130520-2016", Verbatim),
    ("level3", "l3-tlx-dvl-20130520-2016", Verbatim),
    ("level3", "l3-tlx-dpr-20130520-2016", Verbatim),
    ("level3", "l3-tlx-rsl-20130520-2358", Verbatim),
    ("level3", "l3-tlx-n0q-20220503-005231", Verbatim),
    ("level3", "l3-tlx-n0b-20260622-080623", Verbatim),
    ("level3", "l3-ddc-gsm-20200817-1000", Verbatim),
    ("level3", "l3-abr-ftm-20110428-1331", Verbatim),
    ("level3", "l3-ilx-nst-19960419-2303", Verbatim),
    ("level3", "l3-ilx-nhi-19960419-2303", Verbatim),
    ("level3", "l3-ilx-ntv-19960419-2303", Verbatim),
    ("level3", "l3-ilx-ncz-19960419-2320", Verbatim),
    ("level3", "l3-lzk-ncz-19970301-1912", Verbatim),
    ("level3", "l3-sgf-nme-20030504-2332", Verbatim),
    ("level3", "l3-ilx-irm-19960419-2309", Verbatim),
    // io_router: Level III now routes too.
    ("io_router", "l3-tlx-n0v-20130520-2016", Verbatim),
    ("io_router", "l3-mci-dpa-20160526-2154", Verbatim),
    // jma: both committed single-station tars.
    ("jma", "jma-n6-20191012-090000-rs47773", Verbatim),
    ("jma", "jma-n5-20191012-090000-rs47773", Verbatim),
    // bzip2: single LDM records, each one bzip2 stream: a status-only
    // record under 100 bytes, the committed KIWA metadata record and a
    // committed 120-radial record, the smallest and the last TDWR records,
    // and a build 20.1 metadata record.
    ("bzip2", "l2-tbwi-20230601-175101-stub", LdmRecord(0)),
    ("bzip2", KIWA_CHUNK_S, LdmRecord(0)),
    ("bzip2", KIWA_CHUNK_I2, LdmRecord(0)),
    ("bzip2", "l2-tstl-20230331-230314", LdmRecord(1)),
    ("bzip2", "l2-tstl-20230331-230314", LdmRecord(69)),
    ("bzip2", "l2-kbox-20220129-150537", LdmRecord(0)),
    // bzip2_encode: what a Level II writer compresses, the decompressed
    // LDM records (status-only, metadata, 120-radial and TDWR records; the
    // largest are multi-block at the low levels), and already-compressed
    // bytes (a published LDM record: no runs, flat symbol mix).
    (
        "bzip2_encode",
        "l2-tbwi-20230601-175101-stub",
        LdmPayload(0),
    ),
    ("bzip2_encode", KIWA_CHUNK_S, LdmPayload(0)),
    ("bzip2_encode", KIWA_CHUNK_I2, LdmPayload(0)),
    ("bzip2_encode", "l2-tstl-20230331-230314", LdmPayload(1)),
    ("bzip2_encode", "l2-tstl-20230331-230314", LdmPayload(69)),
    ("bzip2_encode", KIWA_CHUNK_I2, LdmRecord(0)),
    // writers: small real volumes of every format the writers take
    // (Level II message 31 and message 1, CfRadial 1 classic and netCDF-4,
    // CfRadial 2, DORADE, ODIM_H5 with quality groups and legends, an RHI).
    ("writers", "l2-ktlx-19990503-230052", Verbatim),
    ("writers", "l2-kvnx-20110315-000203", L2Sparse),
    ("writers", "cfrad1-xsapr-sgp-20110520-ppi-classic", Verbatim),
    ("writers", "cfrad1-xsapr-sgp-20110520-ppi-netcdf4", Verbatim),
    (
        "writers",
        "cfrad1-dow8-20211011-223602-rhi-trim3-classic",
        Verbatim,
    ),
    ("writers", "cfrad2-xradar-xsapr-sgp-20110520-ppi", Verbatim),
    (
        "writers",
        "cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32",
        Verbatim,
    ),
    (
        "writers",
        "dorade-cow2-20260521-225514-sur-head24",
        Verbatim,
    ),
    (
        "writers",
        "odim-bewid-20130429-0430-pvol-dbzh-scan1",
        Verbatim,
    ),
    (
        "writers",
        "odim-espdg-20260707-1927-pvol-dbzh-vradh",
        Verbatim,
    ),
    (
        "writers",
        "odim-fianj-20260924-2130-pvol-dataset1-trim",
        Verbatim,
    ),
    // Sweeps of 500 m and 250 m gates, every first gate centred at 0 m
    // (LROSE Radx's CfRadial 1 of FMI Anjalankoski): per-sweep geometry in
    // CfRadial 1 from a ragged Radx file.
    (
        "writers",
        "cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry",
        Verbatim,
    ),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("seeds") => write_seeds(
            args.get(1)
                .map_or_else(|| fuzz_dir().join("seeds"), PathBuf::from),
        ),
        Some("replay") if args.len() >= 3 => replay(&args[1], args[2..].iter().map(PathBuf::from)),
        Some("regressions") => replay_regressions(),
        Some("smoke") if (3..=4).contains(&args.len()) => match args[2].parse() {
            Ok(iterations) => smoke(
                &args[1],
                iterations,
                args.get(3)
                    .map_or_else(|| fuzz_dir().join("seeds").join(&args[1]), PathBuf::from),
            ),
            Err(_) => Err(other_error(format!("`{}` is not a count", args[2]))),
        },
        Some("mutate") if args.len() >= 5 => run_mutate(&args[1], &args[2], &args[3], &args[4..]),
        _ => {
            eprintln!(
                "usage: fuzz-tools seeds [OUT_DIR]\n       fuzz-tools replay <target> <file-or-dir>...\n       fuzz-tools regressions\n       fuzz-tools smoke <target> <mutations-per-seed> [SEED_DIR]\n       fuzz-tools mutate <target> <runs> <rng-seed> <file-or-dir>...\ntargets: {}",
                TARGETS
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn fuzz_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from(".."), Path::to_path_buf)
}

fn other_error(message: String) -> io::Error {
    io::Error::other(message)
}

fn testdata_bytes(id: &str) -> io::Result<Vec<u8>> {
    recast_radar_testdata::bytes(id).map_err(|err| other_error(format!("testdata `{id}`: {err}")))
}

fn write_seeds(out: PathBuf) -> io::Result<bool> {
    let mut all_written = true;
    for &(target, id, derivation) in SEEDS {
        let name = format!("{id}{}", derivation.suffix());
        let bytes = match derive(id, derivation) {
            Ok(bytes) => bytes,
            Err(err) => {
                eprintln!("SKIP {target:<14} {name}: {err}");
                all_written = false;
                continue;
            }
        };
        let dir = out.join(target);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(&name), &bytes)?;
        println!("{target:<14} {:>9}  {name}", bytes.len());
    }
    Ok(all_written)
}

fn derive(id: &str, derivation: Derivation) -> io::Result<Vec<u8>> {
    let source = testdata_bytes(id)?;
    match derivation {
        Derivation::Verbatim => Ok(source),
        Derivation::L2Sparse => {
            let normalized = normalize_l2(&source)?;
            l2_sparse(&normalized)
        }
        Derivation::L2Head => {
            let normalized = normalize_l2(&source)?;
            l2_head(&normalized)
        }
        Derivation::L2BlockHead => l2_block_head(&source),
        Derivation::LdmRecord(n) => ldm_record(&source, n),
        Derivation::LdmPayload(n) => {
            let record = ldm_record(&source, n)?;
            let mut payload = Vec::new();
            recast_radar_bzip2::Decoder::new()
                .decode_stream_into(&record, &mut payload)
                .map_err(|e| other_error(format!("LDM record {n} of {id}: {e}")))?;
            Ok(payload)
        }
        Derivation::ConcatChunks(more) => {
            let mut bytes = source;
            for id in more {
                bytes.extend_from_slice(&testdata_bytes(id)?);
            }
            Ok(bytes)
        }
        Derivation::DoradeHead(rays) => dorade_head(&source, rays),
    }
}

fn normalize_l2(source: &[u8]) -> io::Result<Vec<u8>> {
    recast_radar_io_nexrad::normalize_archive_bytes(source)
        .map(|(bytes, _)| bytes)
        .map_err(|err| other_error(format!("cannot decompress Level II: {err}")))
}

/// One Archive II record in an uncompressed record stream.
#[derive(Clone, Copy, Debug)]
struct L2Record {
    start: usize,
    end: usize,
    message_type: u8,
    size_halfwords: u16,
}

impl L2Record {
    fn is_radial(&self) -> bool {
        matches!(self.message_type, 1 | 31)
    }

    fn len(&self) -> usize {
        self.end - self.start
    }
}

/// Split an uncompressed Archive II stream into records with the decoder's
/// framing: fixed 2432-byte records, except message 31 after the metadata
/// records (or packed back to back from the start), which is
/// control word + message length.
fn l2_records(bytes: &[u8]) -> Vec<L2Record> {
    let mut records = Vec::new();
    let mut cursor = L2_VOLUME_HEADER_LEN;
    let mut variable_msg31 = false;
    while let Some(header) = bytes
        .get(cursor + L2_CONTROL_WORD_LEN..cursor + L2_CONTROL_WORD_LEN + L2_MESSAGE_HEADER_LEN)
    {
        let size_halfwords = u16::from_be_bytes([header[0], header[1]]);
        let message_type = header[3];
        if size_halfwords == 0 && records.len() >= L2_METADATA_RECORDS {
            break;
        }
        let variable_len = L2_CONTROL_WORD_LEN + usize::from(size_halfwords) * 2;
        let len = if message_type == 31
            && (variable_msg31 || records.len() >= L2_METADATA_RECORDS || {
                let next = cursor + variable_len;
                next == bytes.len()
                    || bytes.get(next + L2_CONTROL_WORD_LEN + 3).copied() == Some(31)
            }) {
            variable_msg31 = true;
            variable_len
        } else {
            L2_RECORD_BYTES
        };
        let end = (cursor + len).min(bytes.len());
        records.push(L2Record {
            start: cursor,
            end,
            message_type,
            size_halfwords,
        });
        cursor = end;
    }
    records
}

fn take_until(records: &[L2Record], budget: usize) -> usize {
    let mut used = 0;
    let mut count = 0;
    for record in records {
        if count > 0 && used + record.len() > budget {
            break;
        }
        used += record.len();
        count += 1;
    }
    count
}

fn l2_sparse(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let records = l2_records(bytes);
    let radials: Vec<L2Record> = records
        .iter()
        .copied()
        .filter(L2Record::is_radial)
        .collect();
    if radials.is_empty() {
        return Err(other_error("no radial messages".to_owned()));
    }
    let first = take_until(&radials, 64 * 1024);
    let reversed: Vec<L2Record> = radials[first..].iter().rev().copied().collect();
    let last = take_until(&reversed, 16 * 1024);

    let mut out = bytes[..L2_VOLUME_HEADER_LEN].to_vec();
    let metadata = records.iter().filter(|record| {
        !record.is_radial()
            && record.size_halfwords != 0
            && !matches!(record.message_type, 0 | 13 | 15)
    });
    let tail = radials.len() - last..radials.len();
    for record in metadata.chain(&radials[..first]).chain(&radials[tail]) {
        out.extend_from_slice(&bytes[record.start..record.end]);
    }
    Ok(out)
}

fn l2_head(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let records = l2_records(bytes);
    let Some(first_radial) = records.iter().position(L2Record::is_radial) else {
        return Err(other_error("no radial messages".to_owned()));
    };
    let radials = take_until(&records[first_radial..], 32 * 1024);
    let end = records[first_radial + radials - 1].end;
    Ok(bytes[..end].to_vec())
}

fn l2_block_head(bytes: &[u8]) -> io::Result<Vec<u8>> {
    const BUDGET: usize = 256 * 1024;
    let mut cursor = L2_VOLUME_HEADER_LEN;
    let mut end = None;
    while let Some(word) = bytes.get(cursor..cursor + 4) {
        let size = i32::from_be_bytes([word[0], word[1], word[2], word[3]]).unsigned_abs() as usize;
        let block_end = cursor + 4 + size;
        if size == 0 || block_end > bytes.len() || (end.is_some() && block_end > BUDGET) {
            break;
        }
        end = Some(block_end);
        cursor = block_end;
    }
    end.map(|end| bytes[..end].to_vec())
        .ok_or_else(|| other_error("no whole bzip2 block after the volume header".to_owned()))
}

fn ldm_record(bytes: &[u8], n: usize) -> io::Result<Vec<u8>> {
    // Archive files start with the volume header; intermediate real-time
    // chunks start with the first control word.
    let mut cursor = if bytes.starts_with(b"AR2V") || bytes.starts_with(b"ARCH") {
        L2_VOLUME_HEADER_LEN
    } else {
        0
    };
    let mut index = 0;
    while let Some(word) = bytes.get(cursor..cursor + 4) {
        let control = i32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        let size = control.unsigned_abs() as usize;
        let Some(record) = bytes.get(cursor + 4..cursor + 4 + size) else {
            break;
        };
        if size == 0 || !record.starts_with(b"BZh") {
            break;
        }
        if index == n {
            return Ok(record.to_vec());
        }
        if control < 0 {
            break;
        }
        index += 1;
        cursor += 4 + size;
    }
    Err(other_error(format!(
        "no LDM record {n} ({index} whole bzip2 records found)"
    )))
}

fn dorade_head(bytes: &[u8], rays: usize) -> io::Result<Vec<u8>> {
    let length_at = |offset: usize, big_endian: bool| -> Option<usize> {
        let word: [u8; 4] = bytes.get(offset + 4..offset + 8)?.try_into().ok()?;
        let len = if big_endian {
            u32::from_be_bytes(word)
        } else {
            u32::from_le_bytes(word)
        };
        usize::try_from(len).ok()
    };
    let big_endian = match (length_at(0, false), length_at(0, true)) {
        (Some(le), _) if (8..=bytes.len()).contains(&le) => false,
        (_, Some(be)) if (8..=bytes.len()).contains(&be) => true,
        _ => return Err(other_error("not a DORADE descriptor block".to_owned())),
    };
    let mut cursor = 0;
    let mut ray_groups = 0;
    while let Some(len) = length_at(cursor, big_endian) {
        if &bytes[cursor..cursor + 4] == b"RYIB" {
            if ray_groups == rays {
                return Ok(bytes[..cursor].to_vec());
            }
            ray_groups += 1;
        }
        if len < 8 || cursor + len > bytes.len() {
            break;
        }
        cursor += len;
    }
    Err(other_error(format!(
        "sweepfile has {ray_groups} ray groups before its end, wanted more than {rays}"
    )))
}

fn replay(target: &str, paths: impl Iterator<Item = PathBuf>) -> io::Result<bool> {
    let harness =
        harness(target).ok_or_else(|| other_error(format!("unknown target `{target}`")))?;
    let mut files = Vec::new();
    for path in paths {
        collect_files(&path, &mut files)?;
    }
    replay_files(target, harness, &files)
}

fn run_mutate(target: &str, runs: &str, seed: &str, paths: &[String]) -> io::Result<bool> {
    let harness =
        harness(target).ok_or_else(|| other_error(format!("unknown target `{target}`")))?;
    let runs: u64 = runs
        .parse()
        .map_err(|_| other_error(format!("runs `{runs}` is not a count")))?;
    let seed: u64 = seed
        .parse()
        .map_err(|_| other_error(format!("rng seed `{seed}` is not a number")))?;
    let mut files = Vec::new();
    for path in paths {
        collect_files(Path::new(path), &mut files)?;
    }
    let out = fuzz_dir().join("artifacts").join(target);
    mutate::mutate(target, harness, &files, runs, seed, &out)
}

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<_>>()?;
        entries.sort();
        for entry in entries {
            collect_files(&entry, files)?;
        }
    } else {
        files.push(path.to_path_buf());
    }
    Ok(())
}

fn replay_files(target: &str, harness: Harness, files: &[PathBuf]) -> io::Result<bool> {
    let mut clean = true;
    for file in files {
        let data = fs::read(file)?;
        let started = Instant::now();
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| harness(&data)));
        let millis = started.elapsed().as_secs_f64() * 1e3;
        let status = match outcome {
            Ok(true) => "decoded",
            Ok(false) => "rejected",
            Err(_) => "PANIC",
        };
        clean &= outcome.is_ok();
        println!(
            "{target:<14} {status:<8} {millis:>9.1} ms {:>9} B  {}",
            data.len(),
            file.display()
        );
    }
    Ok(clean)
}

/// xorshift64* pseudo-random numbers: the same mutants on every run.
struct Mutator(u64);

impl Mutator {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number below `bound` (at least 1).
    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }

    /// One to four mutations of `data`.
    fn mutate(&mut self, data: &mut Vec<u8>) {
        for _ in 0..=self.below(4) {
            if data.is_empty() {
                data.push(0);
            }
            let at = self.below(data.len());
            match self.below(7) {
                0 => data[at] ^= 1 << self.below(8),
                1 => data[at] = [0x00, 0x01, 0x7F, 0x80, 0xFF][self.below(5)],
                2 => {
                    let end = (at + 1 + self.below(8)).min(data.len());
                    for byte in &mut data[at..end] {
                        *byte = self.next() as u8;
                    }
                }
                3 => {
                    let count = 1 + self.below(5);
                    let fill = self.next() as u8;
                    data.splice(at..at, std::iter::repeat_n(fill, count));
                }
                4 => {
                    let end = (at + 1 + self.below(5)).min(data.len());
                    data.drain(at..end);
                }
                5 => {
                    // Copy a span over another place (a field or header
                    // repeated elsewhere in the file).
                    let from = self.below(data.len());
                    let len = (1 + self.below(16))
                        .min(data.len() - from)
                        .min(data.len() - at);
                    let span = data[from..from + len].to_vec();
                    data[at..at + len].copy_from_slice(&span);
                }
                _ => data.truncate(at.max(1)),
            }
        }
    }
}

/// `iterations` seeded mutants of every file under `dir` through the
/// harness of `target`; `false` when any panicked.
fn smoke(target: &str, iterations: usize, dir: PathBuf) -> io::Result<bool> {
    let harness =
        harness(target).ok_or_else(|| other_error(format!("unknown target `{target}`")))?;
    let mut files = Vec::new();
    collect_files(&dir, &mut files)?;
    if files.is_empty() {
        return Err(other_error(format!(
            "no seeds under {} (run `fuzz-tools seeds`)",
            dir.display()
        )));
    }
    let artifacts = fuzz_dir().join("artifacts").join(target);
    let started = Instant::now();
    let (mut runs, mut accepted, mut panics) = (0usize, 0usize, 0usize);
    for (index, file) in files.iter().enumerate() {
        let seed = fs::read(file)?;
        let mut mutator = Mutator(0x9E37_79B9_7F4A_7C15 ^ (index as u64 + 1));
        let mut seed_accepted = 0usize;
        for iteration in 0..iterations {
            let mut data = seed.clone();
            mutator.mutate(&mut data);
            runs += 1;
            match panic::catch_unwind(AssertUnwindSafe(|| harness(&data))) {
                Ok(true) => seed_accepted += 1,
                Ok(false) => {}
                Err(_) => {
                    panics += 1;
                    fs::create_dir_all(&artifacts)?;
                    let name = format!("smoke-{index}-{iteration}");
                    fs::write(artifacts.join(&name), &data)?;
                    eprintln!(
                        "PANIC {target} {} mutant {iteration}: saved {}",
                        file.display(),
                        artifacts.join(&name).display()
                    );
                }
            }
        }
        accepted += seed_accepted;
        println!(
            "{target:<14} {seed_accepted:>6}/{iterations} accepted  {}",
            file.display()
        );
    }
    println!(
        "{target}: {runs} mutants of {} seeds in {:.1} s, {accepted} accepted, {panics} panicked",
        files.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(panics == 0)
}

/// Tag on every fuzz regression input in the testdata manifest
/// (`testdata/fuzz/manifest.toml`).
const REGRESSION_TAG: &str = "fuzz-regression";
/// Tag prefix naming the harness that found a regression input.
const TARGET_TAG_PREFIX: &str = "fuzz-target:";

/// Replay every testdata entry tagged `fuzz-regression` through the harness
/// named by its `fuzz-target:` tag, then through `io_router`, which routes
/// every format.
fn replay_regressions() -> io::Result<bool> {
    let ids = recast_radar_testdata::ids_with_tag(REGRESSION_TAG);
    if ids.is_empty() {
        return Err(other_error(format!(
            "no testdata entries tagged `{REGRESSION_TAG}`"
        )));
    }
    let mut clean = true;
    for id in ids {
        let entry = recast_radar_testdata::entry(id)
            .ok_or_else(|| other_error(format!("testdata `{id}`: no manifest entry")))?;
        let mut targets: Vec<&str> = entry
            .tags
            .iter()
            .filter_map(|tag| tag.strip_prefix(TARGET_TAG_PREFIX))
            .collect();
        if targets.is_empty() {
            return Err(other_error(format!(
                "testdata `{id}`: no `{TARGET_TAG_PREFIX}<target>` tag"
            )));
        }
        if !targets.contains(&"io_router") {
            targets.push("io_router");
        }
        let path = recast_radar_testdata::path(id)
            .map_err(|err| other_error(format!("testdata `{id}`: {err}")))?;
        for target in targets {
            let harness = harness(target).ok_or_else(|| {
                other_error(format!("testdata `{id}`: unknown target `{target}`"))
            })?;
            clean &= replay_files(target, harness, std::slice::from_ref(&path))?;
        }
    }
    Ok(clean)
}
