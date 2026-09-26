//! Decoder failures found by the real-feed survey (`docs/testdata/feeds-survey.md`).
//!
//! Every manifest entry tagged `known-failure:<slug>` (the North Dakota SWC
//! KXWA volume of `testdata/feeds/manifest.toml`, and one JMA fixture of
//! `testdata/other/manifest.toml`) must still fail the way its slug says.
//! These are not the decoders' tests: they keep the survey's record honest.
//! When a decoder fix lands, the matching check here fails; then remove the
//! tag from the manifest entry and give the decoder its own test.
//!
//! Every input is a real file as published (or a byte prefix of one, cut at a
//! record or message boundary); none is built or altered here.
//!
//! The Level II check also compares the raw bytes with what an independent
//! reader made of the same file ([`golden`]), so that the evidence for the
//! failure is pinned here and the decoder's own test, once it is fixed, has
//! its expected values.

use recast_radar_io_nexrad::messages::{self, MessageBody};
use recast_radar_testdata::{ids_with_tag, local_path, manifest};

/// What an independent reader made of the KXWA volume: Py-ART 2.3.0
/// (`pyart.io.read_nexrad_archive`) run on the whole file and on the
/// committed head on 2026-09-25 (`tools/feeds_survey/fixture_goldens.py`
/// prints them). A fixed decoder must reproduce these.
mod golden {
    /// Rays per sweep Py-ART reads from the whole KXWA volume
    /// (`ndswc-kxwa-20260924-214316`: 9 sweeps, 4,759 rays, 3,218 gates of
    /// 62 m from 31 m, VCP 0).
    pub const KXWA_RAYS_PER_SWEEP: [usize; 9] = [1084, 1084, 542, 542, 362, 362, 261, 261, 261];

    /// Rays Py-ART reads from the committed head of that volume
    /// (`ndswc-kxwa-20260924-214316-head41`): 40 rays of the first sweep,
    /// fixed angle 0.4834 deg.
    pub const KXWA_HEAD41_RAYS: usize = 40;
}

/// Every slug this file checks.
const CHECKED: &[&str] = &["level2-ldm-block-limit", "jma-lowest-level-valid"];

fn ids(slug: &str) -> Vec<&'static str> {
    let ids = ids_with_tag(&format!("known-failure:{slug}"));
    assert!(
        !ids.is_empty(),
        "no manifest entry is tagged known-failure:{slug}"
    );
    ids
}

/// A manifest entry's real bytes (committed, or checksum-pinned and cached).
fn load(id: &str) -> Vec<u8> {
    recast_radar_testdata::bytes(id).unwrap_or_else(|error| panic!("{id}: {error}"))
}

#[test]
fn every_known_failure_tag_is_checked() {
    for entry in &manifest().files {
        for tag in &entry.tags {
            if let Some(slug) = tag.strip_prefix("known-failure:") {
                assert!(CHECKED.contains(&slug), "{}: no check for {tag}", entry.id);
            }
        }
    }
}

/// A real volume with one radial per LDM record has 4,760 records; the
/// decoder caps LDM volumes at 4,096 and refuses it, though each record holds
/// one whole radial and Py-ART reads all 4,759 ([`golden::KXWA_RAYS_PER_SWEEP`],
/// checked here record by record). No committed file
/// reproduces this: the smallest prefix that does (the volume header and
/// 4,097 records) is 17,819,782 bytes, over the 2 MB cap for a committed
/// file (`docs/testdata/feeds-survey.md` section 4; committing it is the
/// owner's decision). The whole volume (20.6 MB) is checksum-pinned and resolved with
/// `local_path`, which never downloads: where the shared testdata cache holds
/// it, the check runs; where it does not (CI, any other machine), the check
/// is skipped with a note on stderr; a cached file with other bytes fails the
/// SHA-256 check and panics.
#[test]
fn level2_ldm_block_limit() {
    for id in ids("level2-ldm-block-limit") {
        let path = match local_path(id) {
            Ok(path) => path,
            Err(error) if error.is_offline() => {
                eprintln!("skipping {id} (not in the testdata cache): {error}");
                continue;
            }
            Err(error) => panic!("{id}: {error}"),
        };
        let raw = std::fs::read(&path).unwrap_or_else(|error| panic!("{id}: {error}"));
        let records = ldm_records(&raw[messages::volume_header_len(&raw)..]);
        assert!(records.len() > 4096, "{id}: {} records", records.len());
        // Every record after the metadata record is one radial, and the
        // radials are the ones Py-ART reads.
        let mut rays_per_sweep: Vec<usize> = Vec::new();
        let mut decoder = recast_radar_bzip2::Decoder::new();
        decoder.set_max_output(16 << 20);
        let mut stream = Vec::new();
        for (index, record) in records.iter().enumerate().skip(1) {
            stream.clear();
            decoder
                .decode_stream_into(&record[4..], &mut stream)
                .unwrap_or_else(|error| panic!("{id}: record {index}: {error:?}"));
            let elevations: Vec<usize> = messages::MessageWalker::new(&stream)
                .filter_map(|message| match message {
                    Ok((_, MessageBody::DigitalRadarDataGeneric(radial))) => {
                        Some(usize::from(radial.header.elevation_number))
                    }
                    _ => None,
                })
                .collect();
            let [elevation] = elevations[..] else {
                panic!("{id}: record {index} holds {} radials", elevations.len());
            };
            assert!(elevation >= 1, "{id}: record {index}");
            if rays_per_sweep.len() < elevation {
                rays_per_sweep.resize(elevation, 0);
            }
            rays_per_sweep[elevation - 1] += 1;
        }
        assert_eq!(rays_per_sweep, golden::KXWA_RAYS_PER_SWEEP, "{id}");
        let error = recast_radar_io_nexrad::read_volume_from_bytes(&raw)
            .err()
            .unwrap_or_else(|| panic!("{id}: more than 4,096 LDM records now decode"));
        assert!(
            error.to_string().contains("more than 4096 blocks"),
            "{id}: {error}"
        );
    }
}

/// The committed head of that volume (its first 41 LDM records) decodes, one
/// radial per record: the layout the full volume has 4,760 records of.
#[test]
fn kxwa_head_holds_one_radial_per_ldm_record() {
    let id = "ndswc-kxwa-20260924-214316-head41";
    let raw = load(id);
    let volume = recast_radar_io_nexrad::read_volume_from_bytes(&raw)
        .unwrap_or_else(|error| panic!("{id}: {error}"));
    let rays: usize = volume.sweeps.iter().map(|sweep| sweep.nrays()).sum();
    let records = ldm_records(&raw[messages::volume_header_len(&raw)..]);
    assert_eq!(records.len(), 41, "{id}");
    assert_eq!(rays, records.len() - 1, "{id}: not one radial per record");
    assert_eq!(rays, golden::KXWA_HEAD41_RAYS, "{id}");
}

/// Every committed feed fixture cut from a larger file (`derived_from`; the
/// KXWA head) is a byte prefix of it: its `derivation` starts with "First N
/// bytes", N being its size, and where the shared testdata cache holds the
/// source (the source is pinned, not committed, and never downloaded here)
/// the committed bytes are the source's first N bytes. Without the cache (CI)
/// only the first part runs, and the test prints "checked 0 of 1".
#[test]
fn committed_prefixes_are_prefixes_of_their_sources() {
    let derived: Vec<_> = manifest()
        .files
        .iter()
        .filter(|entry| {
            entry
                .committed
                .as_deref()
                .is_some_and(|path| path.starts_with("files/feeds/"))
        })
        .filter_map(|entry| Some((entry, entry.derived_from.as_deref()?)))
        .collect();
    assert_eq!(derived.len(), 1, "committed prefixes");
    let mut checked = 0;
    for (entry, source_id) in derived {
        let id = entry.id.as_str();
        let derivation = entry.derivation.as_deref().unwrap_or_default();
        let first = format!("First {} bytes", thousands(entry.size));
        assert!(derivation.starts_with(&first), "{id}: {derivation}");
        let source = recast_radar_testdata::entry(source_id)
            .unwrap_or_else(|| panic!("{id}: no source {source_id}"));
        assert!(source.committed.is_none(), "{source_id} is committed");
        assert!(
            source.size > entry.size,
            "{id}: not shorter than {source_id}"
        );
        let path = match local_path(source_id) {
            Ok(path) => path,
            Err(error) if error.is_offline() => {
                eprintln!("skipping {id} (source not in the testdata cache): {error}");
                continue;
            }
            Err(error) => panic!("{source_id}: {error}"),
        };
        let whole = std::fs::read(&path).unwrap_or_else(|error| panic!("{source_id}: {error}"));
        let prefix = load(id);
        assert!(
            whole.starts_with(&prefix),
            "{id}: not a prefix of {source_id}"
        );
        checked += 1;
    }
    eprintln!("checked {checked} of 1 committed prefix against its source");
}

/// `n` with a comma between groups of three digits, as the derivations write
/// byte counts.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// JMA's reflectivity levels (GRIB2 template 5.200): level 0 is outside the
/// observed range or missing, level 1 is "No Echo" (value 0.00), level 2 is
/// below 0.32 dBZ (0.16, negative dBZ included), and each level after it a
/// 0.32 dB class, its centre the value (0.48, 0.80 dBZ, ...). That is table 3
/// of JMA's format description of this product, "Per-radar polar radar echo
/// intensity GPV format (GRIB2, Ver. 2.00)", 2007-05-17
/// (<https://www.mri-jma.go.jp/Project/cons/data/SitePolar.pdf>), and the
/// level table in the file's bytes, checked here, is that table. The decoder
/// treats only level 0 as missing, so every "No Echo" gate decodes as a valid
/// 0.0 dBZ, and a sweep has no gate below threshold
/// (`docs/testdata/feeds-survey.md` section 3.2). No independent reader
/// decodes JMA's polar GRIB2 (ecCodes 2.48.0 has no definition of the local
/// grid template 3.50120), so the evidence pinned here is the file's own
/// level table, read from its bytes.
#[test]
fn jma_lowest_level_valid() {
    for id in ids("jma-lowest-level-valid") {
        let raw = load(id);
        // One ustar member: its header block, then one GRIB2 message whose
        // first section 5 holds the level table.
        assert_eq!(&raw[512..516], b"GRIB", "{id}");
        assert_eq!(raw[519], 2, "{id}: GRIB edition");
        let mut at = 512 + 16;
        let section5 = loop {
            let len = u32::from_be_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
            let len = usize::try_from(len).unwrap();
            if raw[at + 4] == 5 {
                break &raw[at..at + len];
            }
            at += len;
        };
        let be16 = |at: usize| u16::from_be_bytes([section5[at], section5[at + 1]]);
        assert_eq!(be16(9), 200, "{id}: data representation template");
        let levels = usize::from(be16(14));
        let decimal_scale = section5[16];
        let table: Vec<u16> = (0..levels).map(|level| be16(17 + 2 * level)).collect();
        assert_eq!(decimal_scale, 2, "{id}");
        assert_eq!(&table[..3], &[0, 16, 48], "{id}: levels 1-3 (0.01 dBZ)");
        assert!(
            table.windows(2).skip(1).all(|pair| pair[1] - pair[0] == 32),
            "{id}: the ladder from level 2 on is not 0.32 dB"
        );

        let volumes = recast_radar_io_jma::read_jma_tar_volumes(&raw, None)
            .unwrap_or_else(|error| panic!("{id}: {error}"));
        let (mut valid, mut at_level_1, mut off_table) = (0usize, 0usize, 0usize);
        for sweep in volumes.iter().flat_map(|volume| &volume.sweeps) {
            for field in sweep.fields.iter().filter(|f| f.name.to_string() == "DBZH") {
                let (rays, gates) = field.shape();
                for ray in 0..rays {
                    for gate in 0..gates {
                        let Some(value) = field.value(ray, gate) else {
                            continue;
                        };
                        valid += 1;
                        if value == 0.0 {
                            at_level_1 += 1;
                        } else if value < 0.16 - 1e-4 {
                            off_table += 1;
                        }
                    }
                }
            }
        }
        assert!(valid > 0, "{id}: no DBZH gate");
        assert_eq!(off_table, 0, "{id}: DBZH below 0.16 dBZ other than 0.0");
        assert!(
            at_level_1 > 0,
            "{id}: level 1 no longer decodes as a valid 0.0 dBZ ({valid} valid DBZH gates)"
        );
        eprintln!("{id}: {valid} valid DBZH gates, {at_level_1} of them 0.0 dBZ (level 1)");
    }
}

/// The LDM records (control word and payload) of the bytes after a volume
/// header.
fn ldm_records(mut bytes: &[u8]) -> Vec<&[u8]> {
    let mut records = Vec::new();
    while bytes.len() >= 4 {
        let control = i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let len = 4 + control.unsigned_abs() as usize;
        records.push(&bytes[..len.min(bytes.len())]);
        bytes = &bytes[len.min(bytes.len())..];
    }
    records
}
