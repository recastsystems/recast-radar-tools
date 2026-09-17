//! Real-file loading shared by the integration tests.
//!
//! Corpus loops load each source with [`load`] or [`load_all`], which return
//! `None` only when a file is neither committed nor cached and cannot be
//! downloaded right now. After the loop, [`assert_checked_every_available`]
//! requires that every source whose files are then present locally was
//! actually checked, so a loop cannot pass while silently skipping files it
//! could read.

#![allow(dead_code)]

/// Real file bytes, or `None` (with a message) when the file cannot be
/// downloaded right now. Any other error (unknown id, hash mismatch) panics.
pub fn load(id: &str) -> Option<Vec<u8>> {
    match recast_radar_testdata::bytes(id) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.is_offline() => {
            eprintln!("skipping {id}: {error}");
            None
        }
        Err(error) => panic!("{error}"),
    }
}

/// Several files concatenated (a real-time volume is its chunks in order).
pub fn load_all(ids: &[&str]) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    for id in ids {
        bytes.extend_from_slice(&load(id)?);
    }
    Some(bytes)
}

/// True when every id is committed or in the download cache, without
/// touching the network.
pub fn available(ids: &[&str]) -> bool {
    ids.iter()
        .all(|id| recast_radar_testdata::local_path(id).is_ok())
}

/// Asserts that a corpus loop checked exactly the sources whose files are
/// available locally once the loop (which downloads what it can) has run:
/// every committed source, and every other source that is cached. `sources`
/// lists every source the loop applies to, each as its manifest ids.
pub fn assert_checked_every_available<S: AsRef<str>>(
    what: &str,
    checked: usize,
    sources: &[Vec<S>],
) {
    assert!(!sources.is_empty(), "{what}: no sources apply");
    let mut expected = 0;
    let mut committed = 0;
    let mut unavailable = Vec::new();
    for source in sources {
        let ids: Vec<&str> = source.iter().map(AsRef::as_ref).collect();
        if ids.iter().all(|id| {
            recast_radar_testdata::entry(id)
                .unwrap_or_else(|| panic!("{what}: unknown id {id}"))
                .committed
                .is_some()
        }) {
            committed += 1;
        }
        if available(&ids) {
            expected += 1;
        } else {
            unavailable.push(ids.join("+"));
        }
    }
    assert!(
        expected >= committed,
        "{what}: {committed} committed sources, only {expected} available"
    );
    assert_eq!(
        checked,
        expected,
        "{what}: checked {checked} sources, but {expected} of {} are available locally",
        sources.len()
    );
    eprintln!(
        "{what}: checked {checked} of {} sources ({committed} committed){}",
        sources.len(),
        if unavailable.is_empty() {
            String::new()
        } else {
            format!("; not available offline: {}", unavailable.join(", "))
        }
    );
}
