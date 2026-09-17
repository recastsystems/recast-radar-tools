//! Locations (workspace, committed fixtures, download cache) and SHA-256
//! verification.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

/// Environment variable overriding the download cache directory.
pub const CACHE_ENV: &str = "RECAST_RADAR_TESTDATA";

/// Environment variable that, when set to anything other than `""` or `"0"`,
/// forbids downloads: files that are neither committed nor cached are reported
/// as offline.
pub const OFFLINE_ENV: &str = "RECAST_RADAR_TESTDATA_OFFLINE";

const CACHE_SUBDIR: [&str; 2] = ["recast-radar-tools", "testdata"];

/// Workspace root, located at compile time from this crate's
/// `CARGO_MANIFEST_DIR` (`<root>/crates/recast-radar-testdata`).
pub fn workspace_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        crate_dir
            .ancestors()
            .nth(2)
            .unwrap_or(crate_dir)
            .to_path_buf()
    })
}

/// `<workspace>/testdata`: manifests and committed fixtures.
pub fn testdata_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| workspace_root().join("testdata"))
}

/// Download cache directory shared by all worktrees: `$RECAST_RADAR_TESTDATA`,
/// else `%LOCALAPPDATA%\recast-radar-tools\testdata` on Windows, else
/// `$XDG_CACHE_HOME/recast-radar-tools/testdata`, else
/// `$HOME/.cache/recast-radar-tools/testdata`, else
/// `<workspace>/.testdata-cache`.
pub fn cache_dir() -> PathBuf {
    cache_dir_from(|name| std::env::var_os(name), cfg!(windows))
        .unwrap_or_else(|| workspace_root().join(".testdata-cache"))
}

fn cache_dir_from(env: impl Fn(&str) -> Option<OsString>, windows: bool) -> Option<PathBuf> {
    let non_empty = |name: &str| {
        env(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if let Some(dir) = non_empty(CACHE_ENV) {
        return Some(dir);
    }
    let base = windows
        .then(|| non_empty("LOCALAPPDATA"))
        .flatten()
        .or_else(|| non_empty("XDG_CACHE_HOME"))
        .or_else(|| non_empty("HOME").map(|home| home.join(".cache")))?;
    Some(CACHE_SUBDIR.iter().fold(base, |dir, part| dir.join(part)))
}

/// True when [`OFFLINE_ENV`] forbids downloads.
pub(crate) fn offline_forced() -> bool {
    std::env::var_os(OFFLINE_ENV).is_some_and(|value| !value.is_empty() && value != "0")
}

/// Absolute path of a committed fixture. `relative` is relative to
/// `testdata/`; a leading `testdata/` component is also accepted. Absolute
/// paths and `.`/`..` components are rejected.
pub(crate) fn committed_path(relative: &str) -> io::Result<PathBuf> {
    resolve_committed(workspace_root(), testdata_dir(), relative)
}

fn resolve_committed(root: &Path, testdata: &Path, relative: &str) -> io::Result<PathBuf> {
    let rel = Path::new(relative);
    let plain = !relative.is_empty()
        && rel
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if !plain {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("committed path {relative:?} must be a plain relative path under testdata/"),
        ));
    }
    let under_testdata = rel
        .components()
        .next()
        .is_some_and(|first| first.as_os_str() == "testdata");
    Ok(if under_testdata {
        root.join(rel)
    } else {
        testdata.join(rel)
    })
}

/// True when `id` can be used as a cache file name on every platform.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && !id.ends_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    to_hex(&Sha256::digest(bytes))
}

/// Lowercase hex SHA-256 and length of the file at `path`.
pub fn sha256_file(path: &Path) -> io::Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut len = 0u64;
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hasher.update(&buf[..n]);
        len += n as u64;
    }
    Ok((to_hex(&hasher.finalize()), len))
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

/// Adds the path to an I/O error message.
pub(crate) fn io_context(path: &Path, error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn workspace_root_holds_workspace_manifest() {
        assert!(workspace_root().join("Cargo.toml").is_file());
        assert!(
            workspace_root()
                .join("crates")
                .join("recast-radar-testdata")
                .is_dir()
        );
    }

    #[test]
    fn cache_dir_precedence() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            }
        };
        let all: &[(&str, &str)] = &[
            (CACHE_ENV, "/override"),
            ("LOCALAPPDATA", "/local"),
            ("XDG_CACHE_HOME", "/xdg"),
            ("HOME", "/home/u"),
        ];
        let sub = |base: &str| Path::new(base).join("recast-radar-tools").join("testdata");
        assert_eq!(
            cache_dir_from(env(all), true),
            Some(PathBuf::from("/override"))
        );
        assert_eq!(cache_dir_from(env(&all[1..]), true), Some(sub("/local")));
        assert_eq!(cache_dir_from(env(&all[1..]), false), Some(sub("/xdg")));
        assert_eq!(
            cache_dir_from(env(&all[3..]), true),
            Some(sub("/home/u/.cache"))
        );
        let empty_override: &[(&str, &str)] = &[(CACHE_ENV, ""), ("HOME", "/h")];
        assert_eq!(
            cache_dir_from(env(empty_override), false),
            Some(sub("/h/.cache"))
        );
        assert_eq!(cache_dir_from(env(&[]), true), None);
    }

    #[test]
    fn committed_path_rules() {
        let root = Path::new("/ws");
        let td = Path::new("/ws/testdata");
        let ok = |rel: &str| resolve_committed(root, td, rel).ok();
        assert_eq!(ok("files/level3/a"), Some(td.join("files/level3/a")));
        assert_eq!(
            ok("testdata/files/level3/a"),
            Some(root.join("testdata/files/level3/a"))
        );
        assert_eq!(ok("../secret"), None);
        assert_eq!(ok("files/../x"), None);
        assert_eq!(ok("./files/x"), None);
        assert_eq!(ok("/abs/x"), None);
        assert_eq!(ok(""), None);
    }

    #[test]
    fn id_validation() {
        assert!(is_valid_id("l2-ktlx-20240315-000217"));
        assert!(is_valid_id("KTLX20130520_201643_V06.gz"));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id(".hidden"));
        assert!(!is_valid_id("a/b"));
        assert!(!is_valid_id("a b"));
        assert!(!is_valid_id("trailing."));
    }
}
