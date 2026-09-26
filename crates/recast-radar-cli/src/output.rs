//! Output helpers: atomic file writes and number formatting.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::CliError;

/// A temporary file next to `path` that becomes `path` on [`Self::commit`]
/// and is removed if dropped before.
pub(crate) struct AtomicFile {
    temp: PathBuf,
    target: PathBuf,
    writer: Option<BufWriter<File>>,
    sink: io::Sink,
}

impl AtomicFile {
    /// Create the temporary file. Fails when `path` exists and `replace` is
    /// false.
    pub(crate) fn create(path: &Path, replace: bool) -> Result<Self, CliError> {
        if !replace && path.exists() {
            return Err(CliError::Usage(format!(
                "{} exists (pass --force to replace it)",
                path.display()
            )));
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|err| CliError::io(parent, err))?;
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "output".to_owned());
        let temp = path.with_file_name(format!(
            ".{name}.partial-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let file = File::create(&temp).map_err(|err| CliError::io(&temp, err))?;
        Ok(Self {
            temp,
            target: path.to_path_buf(),
            writer: Some(BufWriter::new(file)),
            sink: io::sink(),
        })
    }

    /// The writer for the file's contents.
    pub(crate) fn writer(&mut self) -> &mut dyn Write {
        match &mut self.writer {
            Some(writer) => writer,
            None => &mut self.sink,
        }
    }

    /// Flush, then move the file into place.
    pub(crate) fn commit(mut self) -> Result<PathBuf, CliError> {
        if let Some(writer) = self.writer.take() {
            let file = writer
                .into_inner()
                .map_err(|err| CliError::io(&self.temp, err.into_error()))?;
            file.sync_all()
                .map_err(|err| CliError::io(&self.temp, err))?;
        }
        replace_file(&self.temp, &self.target).map_err(|err| CliError::io(&self.target, err))?;
        Ok(self.target.clone())
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        if self.writer.take().is_some() || self.temp.exists() {
            let _ = fs::remove_file(&self.temp);
        }
    }
}

/// Rename `from` to `to`, replacing `to` (Windows refuses to rename onto an
/// existing file).
pub(crate) fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(first) if to.exists() => {
            fs::remove_file(to).map_err(|_| first)?;
            fs::rename(from, to)
        }
        Err(err) => Err(err),
    }
}

/// Write `bytes` to `path` through a temporary file.
pub(crate) fn write_file_atomically(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    let mut file = AtomicFile::create(path, true)?;
    file.writer()
        .write_all(bytes)
        .map_err(|err| CliError::io(path, err))?;
    file.commit()?;
    Ok(())
}

/// `10.8 MB`, `741 kB`, `230 B` (decimal units).
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["kB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = "B";
    for next in UNITS {
        if value < 1000.0 {
            break;
        }
        value /= 1000.0;
        unit = next;
    }
    if value < 10.0 {
        format!("{value:.2} {unit}")
    } else if value < 100.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.0} {unit}")
    }
}

/// A float with `decimals` decimals, or `-` when absent or not finite.
pub(crate) fn opt_float(value: Option<f64>, decimals: usize) -> String {
    match value {
        Some(value) if value.is_finite() => format!("{value:.decimals$}"),
        _ => "-".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_like_file_managers() {
        assert_eq!(human_bytes(230), "230 B");
        assert_eq!(human_bytes(741_465), "741 kB");
        assert_eq!(human_bytes(10_786_581), "10.8 MB");
        assert_eq!(human_bytes(7_891_864), "7.89 MB");
    }

    #[test]
    fn an_atomic_file_appears_only_on_commit() {
        let dir =
            std::env::temp_dir().join(format!("recast-radar-cli-atomic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("out.txt");
        {
            let mut file = AtomicFile::create(&path, false).unwrap();
            write!(file.writer(), "abandoned").unwrap();
        }
        assert!(!path.exists());
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            0,
            "temporary file removed"
        );

        let mut file = AtomicFile::create(&path, false).unwrap();
        write!(file.writer(), "kept").unwrap();
        file.commit().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "kept");
        assert!(
            AtomicFile::create(&path, false).is_err(),
            "no silent overwrite"
        );
        let mut file = AtomicFile::create(&path, true).unwrap();
        write!(file.writer(), "replaced").unwrap();
        file.commit().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "replaced");
        fs::remove_dir_all(&dir).unwrap();
    }
}
