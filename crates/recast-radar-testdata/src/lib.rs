//! Real radar test corpus for recast-radar-tools (dev-only).
//!
//! The corpus is described by `testdata/manifest.toml` plus every
//! `testdata/*/manifest.toml` in the workspace (located at compile time from
//! this crate's `CARGO_MANIFEST_DIR`). Each entry is either committed under
//! `testdata/` or downloaded on first use into a shared cache directory (see
//! [`cache_dir`]). Every file handed out is verified against the manifest
//! SHA-256.
//!
//! ```no_run
//! # fn decode(_: &std::path::Path) {}
//! #[test]
//! fn decodes_ktlx() {
//!     let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217");
//!     decode(&path);
//! }
//! ```

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod cache;
mod fetch;
mod manifest;
pub mod synthetic;
pub mod trim;

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

pub use cache::{
    CACHE_ENV, OFFLINE_ENV, cache_dir, is_valid_id, sha256_file, sha256_hex, testdata_dir,
    workspace_root,
};
pub use manifest::{
    Entry, Format, MANIFEST_FILE_NAME, Manifest, ManifestError, load_manifest, manifest_files,
};

/// Error resolving a testdata file.
#[derive(Debug)]
pub enum TestdataError {
    /// No manifest entry has this id.
    UnknownId(String),
    /// The file is not committed, not cached, and could not be downloaded
    /// because the network is unavailable (or downloads are disabled, or an
    /// ephemeral URL has expired). Tests should skip.
    Offline {
        /// Entry id.
        id: String,
        /// What was tried.
        source: String,
    },
    /// The committed, cached or downloaded file does not match the manifest.
    HashMismatch {
        /// Entry id.
        id: String,
        /// SHA-256 from the manifest.
        expected: String,
        /// SHA-256 of the bytes found.
        actual: String,
    },
    /// Filesystem error, or a download refused by the server.
    Io(io::Error),
}

impl TestdataError {
    /// True when the file is unavailable only because it cannot be fetched
    /// right now; callers should skip rather than fail.
    pub fn is_offline(&self) -> bool {
        matches!(self, Self::Offline { .. })
    }
}

impl fmt::Display for TestdataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownId(id) => write!(
                f,
                "unknown testdata id `{id}` (not in testdata/manifest.toml or testdata/*/manifest.toml)"
            ),
            Self::Offline { id, source } => {
                write!(f, "testdata `{id}` is not available offline: {source}")
            }
            Self::HashMismatch {
                id,
                expected,
                actual,
            } => write!(
                f,
                "testdata `{id}` sha256 mismatch: manifest {expected}, file {actual}"
            ),
            Self::Io(error) => write!(f, "testdata I/O error: {error}"),
        }
    }
}

impl std::error::Error for TestdataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for TestdataError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// The workspace manifest: `testdata/manifest.toml` plus every
/// `testdata/*/manifest.toml`, loaded once.
///
/// # Panics
///
/// Panics with the file and parse error when a manifest is unreadable or
/// invalid, so a broken manifest fails every test that uses the corpus.
pub fn manifest() -> &'static Manifest {
    static MANIFEST: OnceLock<Manifest> = OnceLock::new();
    MANIFEST.get_or_init(|| match load_manifest(testdata_dir()) {
        Ok(manifest) => manifest,
        Err(error) => panic!("recast-radar-testdata: invalid manifest: {error}"),
    })
}

/// Manifest entry with the given id.
pub fn entry(id: &str) -> Option<&'static Entry> {
    manifest().get(id)
}

/// Path to a verified copy of `id`: the committed file if the entry has one,
/// else the cached download (downloading it first if needed). The SHA-256 is
/// checked once per process per id.
pub fn path(id: &str) -> Result<PathBuf, TestdataError> {
    resolve(id, true)
}

/// Like [`path`], but never touches the network: files that are neither
/// committed nor already cached yield [`TestdataError::Offline`].
pub fn local_path(id: &str) -> Result<PathBuf, TestdataError> {
    resolve(id, false)
}

/// Contents of the verified file for `id` (see [`path`]).
pub fn bytes(id: &str) -> Result<Vec<u8>, TestdataError> {
    let path = path(id)?;
    fs::read(&path).map_err(|e| TestdataError::Io(cache::io_context(&path, &e)))
}

/// Ids of all entries carrying `tag`, in manifest order.
pub fn ids_with_tag(tag: &str) -> Vec<&'static str> {
    manifest()
        .files
        .iter()
        .filter(|entry| entry.tags.iter().any(|t| t == tag))
        .map(|entry| entry.id.as_str())
        .collect()
}

/// Resolve a path, skipping the test with a message when the file is only
/// unavailable because it cannot be downloaded right now; panics on any other
/// error.
#[macro_export]
macro_rules! require_file {
    ($id:expr) => {
        match $crate::path($id) {
            Ok(p) => p,
            Err(e) if e.is_offline() => {
                eprintln!("skipping: {e}");
                return;
            }
            Err(e) => panic!("{e}"),
        }
    };
}

fn verified() -> std::sync::MutexGuard<'static, HashMap<String, PathBuf>> {
    static VERIFIED: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    VERIFIED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn resolve(id: &str, network: bool) -> Result<PathBuf, TestdataError> {
    let entry = entry(id).ok_or_else(|| TestdataError::UnknownId(id.to_owned()))?;
    if let Some(path) = verified().get(id) {
        return Ok(path.clone());
    }

    let path = if let Some(relative) = &entry.committed {
        let path = cache::committed_path(relative).map_err(|e| {
            TestdataError::Io(io::Error::new(e.kind(), format!("testdata `{id}`: {e}")))
        })?;
        verify(entry, &path)?;
        path
    } else {
        if !is_valid_id(id) {
            return Err(TestdataError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("testdata id `{id}` is not a valid cache file name"),
            )));
        }
        let path = cache_dir().join(id);
        if path.is_file() {
            verify(entry, &path)?;
        } else {
            if !network || cache::offline_forced() {
                let reason = if network {
                    format!("downloads disabled by {OFFLINE_ENV}")
                } else {
                    "network access not requested".to_owned()
                };
                return Err(TestdataError::Offline {
                    id: id.to_owned(),
                    source: format!(
                        "not committed and not cached at {}; {reason}",
                        path.display()
                    ),
                });
            }
            // One download at a time per process; recheck after waiting.
            static DOWNLOAD: Mutex<()> = Mutex::new(());
            let _guard = DOWNLOAD
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if path.is_file() {
                verify(entry, &path)?;
            } else {
                // Verifies the SHA-256 before renaming into place.
                fetch::download(entry, &path)?;
            }
        }
        path
    };

    verified().insert(id.to_owned(), path.clone());
    Ok(path)
}

fn verify(entry: &Entry, path: &std::path::Path) -> Result<(), TestdataError> {
    let (actual, _len) =
        sha256_file(path).map_err(|e| TestdataError::Io(cache::io_context(path, &e)))?;
    if actual.eq_ignore_ascii_case(&entry.sha256) {
        Ok(())
    } else {
        Err(TestdataError::HashMismatch {
            id: entry.id.clone(),
            expected: entry.sha256.clone(),
            actual,
        })
    }
}
