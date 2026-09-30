//! Consistency checks over the real workspace manifest (`testdata/manifest.toml`
//! plus `testdata/*/manifest.toml`). Checks over committed or downloadable
//! entries pass vacuously while the manifest has no such entries.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;

use recast_radar_testdata::{
    Entry, TestdataError, cache_dir, entry, ids_with_tag, is_valid_id, load_manifest, local_path,
    manifest, manifest_files, path, testdata_dir,
};
use sha2::{Digest, Sha256};

fn entries() -> &'static [Entry] {
    &manifest().files
}

fn committed() -> impl Iterator<Item = &'static Entry> {
    entries().iter().filter(|e| e.committed.is_some())
}

/// Independent of the crate's hashing helpers.
fn sha256_and_len(path: &std::path::Path) -> (String, u64) {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) => panic!("open {}: {e}", path.display()),
    };
    let mut bytes = Vec::new();
    if let Err(e) = file.read_to_end(&mut bytes) {
        panic!("read {}: {e}", path.display());
    }
    let digest = Sha256::digest(&bytes);
    let hex = digest.iter().map(|b| format!("{b:02x}")).collect();
    (hex, bytes.len() as u64)
}

#[test]
fn manifest_parses() {
    let files = match manifest_files(testdata_dir()) {
        Ok(files) => files,
        Err(e) => panic!("{e}"),
    };
    let top = testdata_dir().join("manifest.toml");
    assert!(top.is_file(), "missing {}", top.display());
    assert_eq!(files.first(), Some(&top));
    let loaded = match load_manifest(testdata_dir()) {
        Ok(m) => m,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(&loaded, manifest());
    eprintln!(
        "{} manifest file(s), {} entries",
        files.len(),
        loaded.files.len()
    );
}

#[test]
fn every_entry_has_64_hex_sha256() {
    let bad: Vec<_> = entries()
        .iter()
        .filter(|e| {
            e.sha256.len() != 64
                || !e
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .map(|e| format!("{}: {:?}", e.id, e.sha256))
        .collect();
    assert!(
        bad.is_empty(),
        "sha256 must be 64 lowercase hex digits: {bad:#?}"
    );
}

#[test]
fn ids_are_unique() {
    // Case-insensitive: cache file names collide on Windows and macOS.
    let mut seen: HashMap<String, &str> = HashMap::new();
    let mut duplicates = Vec::new();
    for e in entries() {
        if let Some(previous) = seen.insert(e.id.to_ascii_lowercase(), &e.id) {
            duplicates.push(format!("{previous} / {}", e.id));
        }
    }
    assert!(duplicates.is_empty(), "duplicate ids: {duplicates:#?}");
}

#[test]
fn ids_are_valid_file_names() {
    let bad: Vec<_> = entries()
        .iter()
        .filter(|e| !is_valid_id(&e.id))
        .map(|e| e.id.as_str())
        .collect();
    assert!(
        bad.is_empty(),
        "ids must be [A-Za-z0-9._-], not starting or ending with '.': {bad:?}"
    );
}

#[test]
fn every_entry_has_a_source_and_size() {
    let mut bad = Vec::new();
    for e in entries() {
        let not_redistributed = e.tags.iter().any(|t| t == "not-redistributed");
        if e.committed.is_none() && e.urls.is_empty() && !not_redistributed {
            bad.push(format!("{}: neither committed nor urls", e.id));
        }
        if not_redistributed && (e.committed.is_some() || !e.urls.is_empty()) {
            bad.push(format!(
                "{}: not-redistributed but committed or downloadable",
                e.id
            ));
        }
        for url in &e.urls {
            if !url.starts_with("https://") && !url.starts_with("http://") {
                bad.push(format!("{}: url {url:?} is not http(s)", e.id));
            }
        }
        if e.size == 0 {
            bad.push(format!("{}: size is 0", e.id));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

#[test]
fn derived_from_refers_to_known_ids() {
    let ids: HashSet<&str> = entries().iter().map(|e| e.id.as_str()).collect();
    let bad: Vec<_> = entries()
        .iter()
        .filter_map(|e| {
            let source = e.derived_from.as_deref()?;
            (!ids.contains(source) || source == e.id).then(|| format!("{} <- {source}", e.id))
        })
        .collect();
    assert!(
        bad.is_empty(),
        "derived_from must name another manifest id: {bad:#?}"
    );
}

#[test]
fn committed_paths_exist_and_hash_match() {
    let mut checked = 0;
    let mut bad = Vec::new();
    for e in committed() {
        let resolved = match local_path(&e.id) {
            Ok(p) => p,
            Err(err) => {
                bad.push(format!("{}: {err}", e.id));
                continue;
            }
        };
        assert!(
            resolved.starts_with(testdata_dir()),
            "{}: committed path {} is outside testdata/",
            e.id,
            resolved.display()
        );
        let (sha, len) = sha256_and_len(&resolved);
        if sha != e.sha256 || len != e.size {
            bad.push(format!(
                "{}: file sha256 {sha} size {len}, manifest sha256 {} size {}",
                e.id, e.sha256, e.size
            ));
        }
        checked += 1;
    }
    assert!(bad.is_empty(), "{bad:#?}");
    eprintln!("verified {checked} committed file(s)");
}

#[test]
fn path_on_committed_id_returns_without_network() {
    for e in committed() {
        // `local_path` never touches the network, so success proves the
        // committed copy alone satisfies the entry; `path` must agree.
        let local = local_path(&e.id);
        let full = path(&e.id);
        match (local, full) {
            (Ok(local), Ok(full)) => assert_eq!(local, full, "{}", e.id),
            (local, full) => panic!("{}: local_path {local:?}, path {full:?}", e.id),
        }
        assert!(recast_radar_testdata::bytes(&e.id).is_ok_and(|b| b.len() as u64 == e.size));
    }
}

#[test]
fn unknown_id_is_an_error_not_offline() {
    let id = "no-such-testdata-id";
    assert!(entry(id).is_none());
    for result in [path(id), local_path(id)] {
        match result {
            Err(err @ TestdataError::UnknownId(_)) => {
                assert!(!err.is_offline());
                assert!(err.to_string().contains(id));
            }
            other => panic!("expected UnknownId, got {other:?}"),
        }
    }
}

#[test]
fn ids_with_tag_matches_manifest() {
    assert!(ids_with_tag("no-such-tag:\u{1}").is_empty());
    let tags: HashSet<&str> = entries()
        .iter()
        .flat_map(|e| e.tags.iter().map(String::as_str))
        .collect();
    for tag in tags {
        let ids = ids_with_tag(tag);
        let expected: Vec<&str> = entries()
            .iter()
            .filter(|e| e.tags.iter().any(|t| t == tag))
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(ids, expected, "tag {tag}");
        assert!(!ids.is_empty());
    }
}

#[test]
fn uncached_download_without_network_is_offline() {
    let uncached = entries()
        .iter()
        .find(|e| e.committed.is_none() && !cache_dir().join(&e.id).is_file());
    let Some(e) = uncached else {
        eprintln!("no uncached download entries; nothing to check");
        return;
    };
    match local_path(&e.id) {
        Err(err) => assert!(err.is_offline(), "{}: {err}", e.id),
        Ok(p) => panic!("{}: unexpectedly resolved to {}", e.id, p.display()),
    }
}

#[test]
fn smallest_download_resolves_or_skips() {
    let smallest = entries()
        .iter()
        .filter(|e| e.committed.is_none() && !e.ephemeral && !e.urls.is_empty())
        .min_by_key(|e| e.size);
    let Some(e) = smallest else {
        eprintln!("no downloadable entries; nothing to fetch");
        return;
    };
    let resolved = recast_radar_testdata::require_file!(&e.id);
    assert!(resolved.starts_with(cache_dir()), "{}", resolved.display());
    let (sha, len) = sha256_and_len(&resolved);
    assert_eq!((sha.as_str(), len), (e.sha256.as_str(), e.size), "{}", e.id);
}
