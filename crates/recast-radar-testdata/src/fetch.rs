//! Downloads into the cache: stream to a temporary file in the cache
//! directory, verify SHA-256, then rename into place.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::TestdataError;
use crate::cache::{io_context, sha256_file, to_hex};
use crate::manifest::Entry;
use sha2::{Digest, Sha256};

/// Why a single URL failed.
enum Failure {
    /// Network unreachable, DNS, TLS, timeout, connection dropped mid-body.
    Transport(String),
    /// The server answered but refused (HTTP status, bad URI, redirects).
    Server(String),
    /// Downloaded bytes did not match the manifest hash.
    Hash(String),
    /// Local filesystem error in the cache directory.
    Local(io::Error),
}

/// Removes the temporary file unless it was renamed into place.
struct TempFile {
    path: PathBuf,
    armed: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Download `entry` to `dest`, trying each URL in order.
pub(crate) fn download(entry: &Entry, dest: &Path) -> Result<(), TestdataError> {
    if entry.urls.is_empty() {
        return Err(TestdataError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "testdata `{}` has no committed file and no download URLs",
                entry.id
            ),
        )));
    }
    let dir = dest.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).map_err(|e| TestdataError::Io(io_context(dir, &e)))?;

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_recv_body(Some(Duration::from_secs(30 * 60)))
        .build()
        .into();

    let mut failures = Vec::new();
    let mut only_transport = true;
    let mut mismatch = None;
    for url in &entry.urls {
        let failure = match fetch_one(&agent, url, entry, dir) {
            Ok(tmp) => return persist(tmp, dest, entry),
            Err(failure) => failure,
        };
        match failure {
            Failure::Transport(message) => failures.push(format!("{url}: {message}")),
            Failure::Server(message) => {
                only_transport = false;
                failures.push(format!("{url}: {message}"));
            }
            Failure::Hash(actual) => {
                only_transport = false;
                failures.push(format!("{url}: sha256 {actual}"));
                mismatch = Some(actual);
            }
            Failure::Local(error) => return Err(TestdataError::Io(error)),
        }
    }
    if let Some(actual) = mismatch {
        return Err(TestdataError::HashMismatch {
            id: entry.id.clone(),
            expected: entry.sha256.clone(),
            actual,
        });
    }
    let summary = failures.join("; ");
    if only_transport || entry.ephemeral {
        Err(TestdataError::Offline {
            id: entry.id.clone(),
            source: summary,
        })
    } else {
        Err(TestdataError::Io(io::Error::other(format!(
            "download of testdata `{}` failed: {summary}",
            entry.id
        ))))
    }
}

fn fetch_one(
    agent: &ureq::Agent,
    url: &str,
    entry: &Entry,
    dir: &Path,
) -> Result<TempFile, Failure> {
    let response = agent.get(url).call().map_err(classify)?;
    let mut reader = response.into_body().into_reader();

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = TempFile {
        path: dir.join(format!(".{}.{}.{nanos}.part", entry.id, std::process::id())),
        armed: true,
    };
    let mut file =
        File::create(&tmp.path).map_err(|e| Failure::Local(io_context(&tmp.path, &e)))?;

    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(Failure::Transport(error.to_string())),
        };
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .map_err(|e| Failure::Local(io_context(&tmp.path, &e)))?;
    }
    file.flush()
        .and_then(|()| file.sync_all())
        .map_err(|e| Failure::Local(io_context(&tmp.path, &e)))?;
    drop(file);

    let actual = to_hex(&hasher.finalize());
    if actual.eq_ignore_ascii_case(&entry.sha256) {
        Ok(tmp)
    } else {
        Err(Failure::Hash(actual))
    }
}

fn persist(mut tmp: TempFile, dest: &Path, entry: &Entry) -> Result<(), TestdataError> {
    match fs::rename(&tmp.path, dest) {
        Ok(()) => {
            tmp.armed = false;
            Ok(())
        }
        // Another process may have placed a verified copy concurrently.
        Err(error) => match sha256_file(dest) {
            Ok((actual, _)) if actual.eq_ignore_ascii_case(&entry.sha256) => Ok(()),
            _ => Err(TestdataError::Io(io_context(dest, &error))),
        },
    }
}

fn classify(error: ureq::Error) -> Failure {
    match error {
        ureq::Error::StatusCode(code) => Failure::Server(format!("HTTP {code}")),
        ureq::Error::BadUri(_) | ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => {
            Failure::Server(error.to_string())
        }
        other => Failure::Transport(other.to_string()),
    }
}
