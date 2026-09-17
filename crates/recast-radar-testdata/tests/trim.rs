//! Trimmed Level II fixtures: every manifest entry whose `derivation` starts
//! with `trim-level2` is committed, within the size budget, and reproduced
//! byte for byte by the trim tool, both from itself (offline) and from its
//! full source volume (downloaded into the cache if needed; skipped offline).

use recast_radar_testdata::trim::{TrimOptions, trim_level2};
use recast_radar_testdata::{Entry, Format, bytes, entry, manifest, path, sha256_hex};

/// Plan TD.3: hard cap per trimmed file.
const TRIMMED_HARD_CAP_BYTES: u64 = 2_000_000;
/// Plan TD.3: total committed testdata.
const COMMITTED_TOTAL_CAP_BYTES: u64 = 60_000_000;

fn trimmed() -> Vec<(&'static Entry, TrimOptions)> {
    manifest()
        .files
        .iter()
        .filter_map(|e| {
            let parsed = TrimOptions::from_derivation(e.derivation.as_deref()?)?;
            match parsed {
                Ok(options) => Some((e, options)),
                Err(error) => panic!("{}: bad trim-level2 derivation: {error}", e.id),
            }
        })
        .collect()
}

fn read(id: &str) -> Vec<u8> {
    match bytes(id) {
        Ok(bytes) => bytes,
        Err(error) => panic!("{id}: {error}"),
    }
}

#[test]
fn trimmed_entries_are_committed_derived_and_small() {
    let entries = trimmed();
    for (e, options) in &entries {
        assert_eq!(e.format, Format::NexradLevel2, "{}", e.id);
        let Some(committed) = &e.committed else {
            panic!("{}: trimmed fixtures must be committed", e.id);
        };
        assert!(
            committed.ends_with(".trim.V06"),
            "{}: committed name {committed}",
            e.id
        );
        let Some(source) = e.derived_from.as_deref().and_then(entry) else {
            panic!("{}: derived_from must name a manifest entry", e.id);
        };
        assert_eq!(source.format, Format::NexradLevel2, "{}", e.id);
        assert!(
            source.tags.iter().any(|t| t == "trim"),
            "{}: source {} is not tagged trim",
            e.id,
            source.id
        );
        assert!(
            options.sweeps.is_some() && options.max_bytes.is_none(),
            "{}: derivation must give --sweeps and no --max-bytes: {options:?}",
            e.id
        );
        assert!(
            e.size <= TRIMMED_HARD_CAP_BYTES,
            "{}: {} bytes is over the {TRIMMED_HARD_CAP_BYTES}-byte hard cap",
            e.id,
            e.size
        );
    }
    let committed_total: u64 = manifest()
        .files
        .iter()
        .filter(|e| e.committed.is_some())
        .map(|e| e.size)
        .sum();
    assert!(
        committed_total <= COMMITTED_TOTAL_CAP_BYTES,
        "committed testdata totals {committed_total} bytes"
    );
    eprintln!(
        "{} trimmed fixture(s); committed testdata totals {committed_total} bytes",
        entries.len()
    );
}

#[test]
fn trimming_a_trimmed_fixture_reproduces_it() {
    for (e, options) in trimmed() {
        let fixture = read(&e.id);
        for (label, opts) in [
            ("derivation options", options),
            ("defaults", TrimOptions::default()),
        ] {
            let out = match trim_level2(&fixture, &opts) {
                Ok(out) => out,
                Err(error) => panic!("{}: trimming with {label}: {error}", e.id),
            };
            assert!(
                out.bytes == fixture,
                "{}: trimming the fixture with {label} changed it ({} -> {} bytes)",
                e.id,
                fixture.len(),
                out.bytes.len()
            );
            assert_eq!(out.report.options.sweeps, options.sweeps, "{}", e.id);
            assert!(out.report.split_cut, "{}: split cut not detected", e.id);
        }
    }
}

#[test]
fn trimmed_fixtures_reproduce_from_source_volumes() {
    let mut reproduced = 0;
    for (e, options) in trimmed() {
        let Some(source_id) = e.derived_from.as_deref() else {
            panic!("{}: no derived_from", e.id);
        };
        let source = match path(source_id) {
            Ok(path) => path,
            Err(error) if error.is_offline() => {
                eprintln!("skipping {}: {error}", e.id);
                continue;
            }
            Err(error) => panic!("{}: {error}", e.id),
        };
        let source = match std::fs::read(&source) {
            Ok(bytes) => bytes,
            Err(error) => panic!("{}: {error}", source.display()),
        };
        let out = match trim_level2(&source, &options) {
            Ok(out) => out,
            Err(error) => panic!("{}: trimming {source_id}: {error}", e.id),
        };
        assert_eq!(
            (sha256_hex(&out.bytes), out.bytes.len() as u64),
            (e.sha256.clone(), e.size),
            "{}: `trim-level2 {}` on {source_id} does not reproduce the fixture",
            e.id,
            options.to_args()
        );
        reproduced += 1;
    }
    eprintln!("reproduced {reproduced} trimmed fixture(s) from their sources");
}
