//! Manifest schema and loading.
//!
//! The corpus is described by `testdata/manifest.toml` plus every
//! `testdata/*/manifest.toml`. Each file holds `[[file]]` tables; unknown keys
//! are rejected so that typos (`commited`, `[[files]]`) fail loudly instead of
//! silently changing how a fixture is resolved.

use std::convert::Infallible;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Name of a manifest file inside `testdata/` and its immediate subdirectories.
pub const MANIFEST_FILE_NAME: &str = "manifest.toml";

/// All manifest entries, in load order (top-level manifest first, then
/// subdirectory manifests sorted by directory name).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// One entry per `[[file]]` table.
    #[serde(rename = "file", default)]
    pub files: Vec<Entry>,
}

/// One real file in the corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Unique id; also the file name of the cached download.
    pub id: String,
    /// File format.
    pub format: Format,
    /// Download URLs, tried in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
    /// Lowercase hex SHA-256 of the file contents.
    pub sha256: String,
    /// File size in bytes.
    pub size: u64,
    /// Path of the committed copy, relative to `testdata/` (a leading
    /// `testdata/` component is also accepted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed: Option<String>,
    /// Id of the entry this file was derived from (e.g. a trimmed volume).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<String>,
    /// How the file was derived from `derived_from`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation: Option<String>,
    /// True when the URLs expire (e.g. real-time chunks); a failed download is
    /// then reported as offline rather than as an error.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ephemeral: bool,
    /// Free-form tags such as `era:2024`, `vcp:212`, `provider:aws`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// File format of a manifest entry. Serialized as a kebab-case string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
#[non_exhaustive]
pub enum Format {
    /// `nexrad-level2`: NEXRAD Level II archive volume.
    NexradLevel2,
    /// `nexrad-level2-chunk`: NEXRAD Level II real-time chunk.
    NexradLevel2Chunk,
    /// `nexrad-level3`: NEXRAD/TDWR Level III product.
    NexradLevel3,
    /// `odim-h5`: ODIM_H5 (OPERA) HDF5 file.
    OdimH5,
    /// `cfradial1`: CfRadial 1.x netCDF file.
    CfRadial1,
    /// `cfradial2`: CfRadial 2 / FM301 netCDF file.
    CfRadial2,
    /// `dorade`: DORADE sweep file.
    Dorade,
    /// `jma-grib2-tar`: JMA radar GRIB2 tar archive.
    JmaGrib2Tar,
    /// `meteofrance-bufr`: Meteo-France radar BUFR (PAG, PAM).
    MeteoFranceBufr,
    /// Any other format string, kept verbatim.
    Other(String),
}

impl Format {
    /// Canonical manifest spelling.
    pub fn as_str(&self) -> &str {
        match self {
            Self::NexradLevel2 => "nexrad-level2",
            Self::NexradLevel2Chunk => "nexrad-level2-chunk",
            Self::NexradLevel3 => "nexrad-level3",
            Self::OdimH5 => "odim-h5",
            Self::CfRadial1 => "cfradial1",
            Self::CfRadial2 => "cfradial2",
            Self::Dorade => "dorade",
            Self::JmaGrib2Tar => "jma-grib2-tar",
            Self::MeteoFranceBufr => "meteofrance-bufr",
            Self::Other(other) => other,
        }
    }
}

impl From<&str> for Format {
    fn from(value: &str) -> Self {
        match value {
            "nexrad-level2" => Self::NexradLevel2,
            "nexrad-level2-chunk" => Self::NexradLevel2Chunk,
            "nexrad-level3" => Self::NexradLevel3,
            "odim-h5" => Self::OdimH5,
            "cfradial1" | "cf-radial1" | "cfradial-1" => Self::CfRadial1,
            "cfradial2" | "cf-radial2" | "cfradial-2" => Self::CfRadial2,
            "dorade" => Self::Dorade,
            "jma-grib2-tar" => Self::JmaGrib2Tar,
            "meteofrance-bufr" => Self::MeteoFranceBufr,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl From<String> for Format {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}

impl From<Format> for String {
    fn from(value: Format) -> Self {
        match value {
            Format::Other(other) => other,
            known => known.as_str().to_owned(),
        }
    }
}

impl FromStr for Format {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from(s))
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Manifest {
    /// Parse one manifest document.
    pub fn from_toml_str(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(text))
    }

    /// Serialize as a manifest document (`[[file]]` tables).
    pub fn to_toml_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string(self)
    }

    /// Entry with the given id (first match).
    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.files.iter().find(|entry| entry.id == id)
    }
}

/// Error loading a manifest file.
#[derive(Debug)]
#[non_exhaustive]
pub enum ManifestError {
    /// The manifest file or directory could not be read.
    Io {
        /// Path being read.
        path: PathBuf,
        /// Underlying error.
        error: io::Error,
    },
    /// The manifest file is not valid TOML or does not match the schema.
    Parse {
        /// Path being parsed.
        path: PathBuf,
        /// Underlying error.
        error: toml::de::Error,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, error } => write!(f, "reading {}: {error}", path.display()),
            Self::Parse { path, error } => write!(f, "parsing {}: {error}", path.display()),
        }
    }
}

impl std::error::Error for ManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { error, .. } => Some(error),
            Self::Parse { error, .. } => Some(error),
        }
    }
}

/// Manifest files under `testdata_dir`: `manifest.toml` (if present), then
/// `*/manifest.toml` sorted by directory name. A missing directory yields an
/// empty list.
pub fn manifest_files(testdata_dir: &Path) -> Result<Vec<PathBuf>, ManifestError> {
    let mut paths = Vec::new();
    let top = testdata_dir.join(MANIFEST_FILE_NAME);
    if top.is_file() {
        paths.push(top);
    }
    let read_dir = match fs::read_dir(testdata_dir) {
        Ok(read_dir) => read_dir,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(paths),
        Err(error) => {
            return Err(ManifestError::Io {
                path: testdata_dir.to_path_buf(),
                error,
            });
        }
    };
    let mut subdirs = Vec::new();
    for dir_entry in read_dir {
        let dir_entry = dir_entry.map_err(|error| ManifestError::Io {
            path: testdata_dir.to_path_buf(),
            error,
        })?;
        let candidate = dir_entry.path().join(MANIFEST_FILE_NAME);
        if dir_entry.path().is_dir() && candidate.is_file() {
            subdirs.push((dir_entry.file_name(), candidate));
        }
    }
    subdirs.sort();
    paths.extend(subdirs.into_iter().map(|(_, path)| path));
    Ok(paths)
}

/// Load and concatenate every manifest under `testdata_dir` (see
/// [`manifest_files`]). Duplicate ids are kept; the manifest tests reject them.
pub fn load_manifest(testdata_dir: &Path) -> Result<Manifest, ManifestError> {
    let mut manifest = Manifest::default();
    for path in manifest_files(testdata_dir)? {
        let text = fs::read_to_string(&path).map_err(|error| ManifestError::Io {
            path: path.clone(),
            error,
        })?;
        let part = Manifest::from_toml_str(&text).map_err(|error| ManifestError::Parse {
            path: path.clone(),
            error,
        })?;
        manifest.files.extend(part.files);
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN_EXAMPLE: &str = r#"
[[file]]
id = "l2-ktlx-20240315-000217"
format = "nexrad-level2"
urls = ["https://unidata-nexrad-level2.s3.amazonaws.com/2024/03/15/KTLX/KTLX20240315_000217_V06"]
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
size = 10786581
tags = ["era:2024", "vcp:212", "bench", "trim"]
description = "KTLX 2024-03-15 00:02Z; radrs/nexrad comparison volume"
"#;

    #[test]
    fn parses_plan_schema_with_defaults() {
        let manifest = Manifest::from_toml_str(PLAN_EXAMPLE).map_err(|e| e.to_string());
        let Ok(manifest) = manifest else {
            panic!("plan example must parse: {manifest:?}");
        };
        assert_eq!(manifest.files.len(), 1);
        let entry = &manifest.files[0];
        assert_eq!(entry.id, "l2-ktlx-20240315-000217");
        assert_eq!(entry.format, Format::NexradLevel2);
        assert_eq!(entry.size, 10_786_581);
        assert_eq!(entry.urls.len(), 1);
        assert_eq!(entry.tags, ["era:2024", "vcp:212", "bench", "trim"]);
        assert_eq!(entry.committed, None);
        assert_eq!(entry.derived_from, None);
        assert_eq!(entry.derivation, None);
        assert!(!entry.ephemeral);
    }

    #[test]
    fn comment_only_manifest_is_empty() {
        let manifest = Manifest::from_toml_str("# header only\n").map_err(|e| e.to_string());
        assert_eq!(manifest, Ok(Manifest::default()));
        let with_bom = Manifest::from_toml_str("\u{feff}# header\r\n").map_err(|e| e.to_string());
        assert_eq!(with_bom, Ok(Manifest::default()));
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Manifest::from_toml_str(&PLAN_EXAMPLE.replace("[[file]]", "[[files]]")).is_err());
        let typo = PLAN_EXAMPLE.replace("size = ", "commited = \"files/x\"\nsize = ");
        assert!(Manifest::from_toml_str(&typo).is_err());
    }

    #[test]
    fn format_strings_round_trip() {
        let known = [
            Format::NexradLevel2,
            Format::NexradLevel2Chunk,
            Format::NexradLevel3,
            Format::OdimH5,
            Format::CfRadial1,
            Format::CfRadial2,
            Format::Dorade,
            Format::JmaGrib2Tar,
            Format::MeteoFranceBufr,
        ];
        for format in known {
            assert_eq!(Format::from(format.as_str()), format);
            assert!(!matches!(format, Format::Other(_)));
        }
        assert_eq!(Format::from("cf-radial1"), Format::CfRadial1);
        assert_eq!(Format::from("uf"), Format::Other("uf".to_owned()));
        assert_eq!(String::from(Format::Other("uf".to_owned())), "uf");
    }

    #[test]
    fn serializes_back_to_file_tables() {
        let Ok(manifest) = Manifest::from_toml_str(PLAN_EXAMPLE) else {
            panic!("plan example must parse");
        };
        let Ok(text) = manifest.to_toml_string() else {
            panic!("manifest must serialize");
        };
        assert!(text.contains("[[file]]"), "{text}");
        assert!(text.contains("format = \"nexrad-level2\""), "{text}");
        assert!(!text.contains("ephemeral"), "{text}");
        assert_eq!(Manifest::from_toml_str(&text).ok(), Some(manifest));
    }
}
