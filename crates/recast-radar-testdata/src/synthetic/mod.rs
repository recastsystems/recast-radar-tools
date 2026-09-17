//! Detector for synthetic radar inputs in workspace tests.
//!
//! Spec section 5 ("Enforcement") and plan stream C: every test input comes
//! from a real radar file. [`scan_workspace`] finds test code that feeds
//! something else, and `tests/no_synthetic.rs` fails on any finding that is
//! not listed in `testdata/synthetic-allowlist.toml`.
//!
//! # What is scanned
//!
//! - Rust test code: every item under `crates/*/tests/`, and `#[cfg(test)]`
//!   items and `#[test]` functions (plus out-of-line modules they declare)
//!   under `crates/*/src/`. Examples, benches and library code are not test
//!   code. `fuzz/` is outside `crates/` and is not scanned (spec section 5).
//! - Data files under `crates/` and `testdata/`.
//!
//! # Rules
//!
//! A finding is one item (function, constant, static, type, macro) or one data
//! file. Items are keyed by their inline module path, e.g. `tests::helper` or
//! `tests::Builder::build`; nested functions and closures belong to their
//! enclosing item. See [`Rule`] for the rules.
//!
//! Real-data evidence in an item (the `recast_radar_testdata` crate,
//! `require_file!`, `include_bytes!` of a file that is not flagged, `fs::read*`,
//! `File::open`, `*_from_path(..)`), directly or through a helper it calls,
//! marks byte-level edits as mutations of real bytes: it suppresses
//! [`Rule::ByteEncoding`], [`Rule::MagicLiteral`] and [`Rule::GateField`] for
//! that item. It does not suppress the other rules.

mod items;
mod lexer;
mod rules;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::cache::sha256_file;
use items::{Item, test_items};
use lexer::Lexed;
use rules::{ItemFacts, is_synthetic_name, item_facts, resolve_references};

pub use rules::MODEL_TYPES;

/// Allowlist file name under `testdata/`.
pub const ALLOWLIST_FILE_NAME: &str = "synthetic-allowlist.toml";

/// Conversion groups from plan task C.2 and the crates each one owns. Every
/// allowlist entry sits under the group that owns its path. Crates added
/// after C.1 join the group of the crates they serve: `recast-radar-bzip2`
/// (the Level II LDM record decoder) and `recast-radar-io-level3` are
/// io-nexrad; the `recast-radar-tools` facade is io-formats (its tests route
/// real files through the readers).
pub const GROUPS: &[(&str, &[&str])] = &[
    (
        "io-nexrad",
        &[
            "recast-radar-io-nexrad",
            "recast-radar-io-level3",
            "recast-radar-bzip2",
        ],
    ),
    (
        "io-formats",
        &[
            "recast-radar-io-odim",
            "recast-radar-io-cfradial",
            "recast-radar-io-dorade",
            "recast-radar-io-jma",
            "recast-radar-io",
            "recast-radar-tools",
        ],
    ),
    ("correct", &["recast-radar-correct"]),
    ("filters-map", &["recast-radar-filters", "recast-radar-map"]),
    ("retrieve", &["recast-radar-retrieve"]),
    ("track", &["recast-radar-track"]),
    (
        "render-bench",
        &["recast-radar-render", "recast-radar-bench"],
    ),
    (
        "core-data-scattering",
        &[
            "recast-radar-core",
            "recast-radar-data",
            "recast-radar-scattering",
            "recast-radar-testdata",
        ],
    ),
];

/// Group owning a workspace-relative path (`crates/<crate>/...`).
pub fn group_for_path(path: &str) -> Option<&'static str> {
    let crate_name = path.strip_prefix("crates/")?.split('/').next()?;
    GROUPS
        .iter()
        .find(|(_, crates)| crates.contains(&crate_name))
        .map(|(group, _)| *group)
}

/// Why an item or file was flagged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rule {
    /// The item, or a function or type defined inside it, is named as a
    /// fabricator: contains `synth`, `fake`, `fabricat`, `mock`, `dummy`,
    /// `handcraft`..., or is a `build_/make_/write_/encode_..._<radar noun>`
    /// builder (`build_message31_body`, `make_volume`, `write_zip`).
    SyntheticName,
    /// Encodes integers or floats with `to_be_bytes`/`to_le_bytes`/
    /// `to_ne_bytes` and writes into a buffer (`extend_from_slice`,
    /// `copy_from_slice`, `push`, `write_all`, ...), or writes a byte-string
    /// literal into one. Suppressed by real-data evidence.
    ByteEncoding,
    /// Uses a byte-string literal that starts with a radar or container
    /// signature (`\x89HDF`, `CDF\x01`, `AR2V`, `SSWB`, `GRIB`, `ustar`,
    /// `PK\x03\x04`, HDF5 `TREE`/`OHDR`, ...). Suppressed by real-data
    /// evidence.
    MagicLiteral,
    /// Constructs a radar model value by hand: `RadarVolume::new`,
    /// `ElevationCut::new`, a `Radial { .. }` or `MomentGrid { .. }` literal,
    /// `MomentGrid::new_u8`, `.push_cut(..)`, `.push_row(..)`, or a crate
    /// radar container listed in [`MODEL_TYPES`].
    ModelConstruction,
    /// Allocates a polar field `vec![<float>; rows * gates]` (dimension names
    /// such as `rows`, `rays`, `radials`, `azimuths`, `gates`, `bins`) to fill
    /// with made-up values. Suppressed by real-data evidence.
    GateField,
    /// `include_bytes!`/`include_str!` of a flagged data file.
    SyntheticDataFile,
    /// Uses a flagged item defined in the same file or test crate.
    UsesSynthetic,
    /// A data file named as synthetic (`*synth*`, `*fake*`, ...).
    SyntheticFileName,
    /// A binary file in a `tests`, `data`, `fixtures` or `testdata` directory
    /// whose SHA-256 is not in any manifest: not a known real file.
    UnmanifestedBinary,
    /// A script under a `tests/` or fixture directory that describes itself as
    /// generating synthetic data.
    SyntheticGenerator,
}

impl Rule {
    /// Stable kebab-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SyntheticName => "synthetic-name",
            Self::ByteEncoding => "byte-encoding",
            Self::MagicLiteral => "magic-literal",
            Self::ModelConstruction => "model-construction",
            Self::GateField => "gate-field",
            Self::SyntheticDataFile => "synthetic-data-file",
            Self::UsesSynthetic => "uses-synthetic",
            Self::SyntheticFileName => "synthetic-file-name",
            Self::UnmanifestedBinary => "unmanifested-binary",
            Self::SyntheticGenerator => "synthetic-generator",
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One rule hit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// Rule.
    pub rule: Rule,
    /// 1-based line.
    pub line: usize,
    /// Short description (`RadarVolume::new(..)`, `uses synthetic_archive`).
    pub detail: String,
}

/// A flagged item or data file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// Workspace-relative path with `/` separators.
    pub path: String,
    /// Qualified item name; `None` for a data file.
    pub name: Option<String>,
    /// The item is a `#[test]` function.
    pub is_test: bool,
    /// Line of the item (1 for data files).
    pub line: usize,
    /// Rule hits, in line order.
    pub hits: Vec<Hit>,
}

impl Finding {
    /// Distinct rules hit.
    pub fn rules(&self) -> BTreeSet<Rule> {
        self.hits.iter().map(|hit| hit.rule).collect()
    }

    /// Group owning the finding's path.
    pub fn group(&self) -> Option<&'static str> {
        group_for_path(&self.path)
    }

    /// `path` or `path :: name`.
    pub fn key(&self) -> (String, Option<String>) {
        (self.path.clone(), self.name.clone())
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(name) => write!(f, "{}:{} `{name}`", self.path, self.line)?,
            None => write!(f, "{} (data file)", self.path)?,
        }
        let rules: Vec<&str> = self.rules().into_iter().map(Rule::as_str).collect();
        write!(f, " [{}]", rules.join(", "))?;
        for hit in self.hits.iter().take(4) {
            write!(f, "; L{} {}: {}", hit.line, hit.rule, hit.detail)?;
        }
        if self.hits.len() > 4 {
            write!(f, "; ... {} more", self.hits.len() - 4)?;
        }
        Ok(())
    }
}

/// Directories never scanned.
fn skipped_dir(name: &str) -> bool {
    name.starts_with('.') || matches!(name, "target" | "node_modules" | "__pycache__")
}

fn rel_path(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut entries: Vec<_> = match fs::read_dir(dir) {
        Ok(read_dir) => read_dir.collect::<Result<_, _>>()?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() {
            if !skipped_dir(&name) {
                walk(&path, out)?;
            }
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// Scans the workspace at `root`. `manifest_sha256` holds the lowercase
/// SHA-256 of every manifest entry.
pub fn scan_workspace(root: &Path, manifest_sha256: &BTreeSet<String>) -> io::Result<Vec<Finding>> {
    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files)?;
    walk(&root.join("testdata"), &mut files)?;

    let mut findings = Vec::new();
    let mut flagged_files = BTreeSet::new();
    for path in files
        .iter()
        .filter(|p| p.extension().is_none_or(|e| e != "rs"))
    {
        if let Some(finding) = scan_data_file(root, path, manifest_sha256)? {
            flagged_files.insert(path.clone());
            findings.push(finding);
        }
    }

    let mut units: Vec<(PathBuf, Vec<Finding>)> = Vec::new();
    let mut claimed: BTreeSet<PathBuf> = BTreeSet::new();
    for path in files
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
    {
        let rel = rel_path(root, path);
        let mut parts = rel.split('/');
        let (Some("crates"), Some(_crate), Some(area)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let whole_file_is_test = match area {
            "tests" => {
                // Only test crate roots; modules they declare are parsed with
                // their root.
                rel.matches('/').count() == 3
            }
            "src" => false,
            _ => continue,
        };
        let (unit_findings, modules) =
            scan_rust_unit(root, path, whole_file_is_test, &flagged_files)?;
        claimed.extend(modules);
        units.push((path.clone(), unit_findings));
    }
    // A module file declared from test code belongs to its declaring unit;
    // drop its standalone scan (which misses its helpers).
    for (path, unit_findings) in units {
        if !claimed.contains(&path) {
            findings.extend(unit_findings);
        }
    }

    findings.sort_by(|a, b| (&a.path, a.line, &a.name).cmp(&(&b.path, b.line, &b.name)));
    findings.dedup_by(|a, b| a.key() == b.key());
    Ok(findings)
}

/// Scans one Rust source unit (a file plus the out-of-line modules its test
/// code declares) as test code. Returns the findings and the module files
/// other than `path` that the unit parsed.
fn scan_rust_unit(
    root: &Path,
    path: &Path,
    whole_file_is_test: bool,
    flagged_files: &BTreeSet<PathBuf>,
) -> io::Result<(Vec<Finding>, Vec<PathBuf>)> {
    let crate_name = rel_path(root, path)
        .split('/')
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    // (file path, lexed, item) for every test item of the unit.
    let mut sources: Vec<(PathBuf, Lexed)> = Vec::new();
    let mut unit_items: Vec<(usize, Item)> = Vec::new();
    let mut queue: Vec<(PathBuf, bool, Vec<String>)> =
        vec![(path.to_path_buf(), whole_file_is_test, Vec::new())];
    let mut seen = BTreeSet::new();
    while let Some((file, whole, prefix)) = queue.pop() {
        if !seen.insert(file.clone()) {
            continue;
        }
        let text = fs::read_to_string(&file)?;
        let lexed = Lexed::new(&text);
        let (items, modules) = test_items(&lexed, whole, &prefix);
        let source_index = sources.len();
        for module in modules {
            if let Some(module_file) = module_file(&file, &prefix, &module.mod_path) {
                queue.push((module_file, true, module.mod_path));
            }
        }
        unit_items.extend(items.into_iter().map(|item| (source_index, item)));
        sources.push((file, lexed));
    }
    let modules: Vec<PathBuf> = sources
        .iter()
        .skip(1)
        .map(|(file, _)| file.clone())
        .collect();
    if unit_items.is_empty() {
        return Ok((Vec::new(), modules));
    }

    let mut analyzed: Vec<(Item, ItemFacts)> = Vec::new();
    let mut item_source: Vec<usize> = Vec::new();
    for (source_index, item) in unit_items {
        let (file, lexed) = &sources[source_index];
        let mut facts = item_facts(lexed, &item, &crate_name);
        // include_bytes!/include_str!: synthetic files are hits, real files
        // are evidence.
        for (line, literal) in std::mem::take(&mut facts.includes) {
            let target = file.parent().map(|dir| dir.join(&literal));
            let target = target.and_then(|t| normalize(&t));
            match target {
                Some(target) if flagged_files.contains(&target) => facts.hits.push((
                    Rule::SyntheticDataFile,
                    line,
                    format!("includes {}", rel_path(root, &target)),
                )),
                Some(target) if target.is_file() && facts.evidence.is_none() => {
                    facts.evidence = Some((line, format!("includes {}", rel_path(root, &target))));
                }
                _ => {}
            }
        }
        analyzed.push((item, facts));
        item_source.push(source_index);
    }

    let uses = resolve_references(&analyzed);

    // Real-data evidence, propagated through helpers.
    let mut real: Vec<bool> = analyzed.iter().map(|(_, f)| f.evidence.is_some()).collect();
    loop {
        let mut changed = false;
        for index in 0..analyzed.len() {
            if !real[index] && uses[index].iter().any(|&u| real[u]) {
                real[index] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut hits: Vec<Vec<Hit>> = analyzed
        .iter()
        .enumerate()
        .map(|(index, (_, facts))| {
            let mut out: Vec<Hit> = facts
                .hits
                .iter()
                .map(|(rule, line, detail)| Hit {
                    rule: *rule,
                    line: *line,
                    detail: detail.clone(),
                })
                .collect();
            if !real[index] {
                out.extend(facts.suppressible.iter().map(|(rule, line, detail)| Hit {
                    rule: *rule,
                    line: *line,
                    detail: detail.clone(),
                }));
            }
            out
        })
        .collect();

    // Items that use flagged items are flagged, to a fixed point; then every
    // item lists all the flagged items it uses.
    let mut flagged: Vec<bool> = hits.iter().map(|h| !h.is_empty()).collect();
    loop {
        let mut changed = false;
        for index in 0..analyzed.len() {
            if !flagged[index] && uses[index].iter().any(|&u| flagged[u]) {
                flagged[index] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for index in 0..analyzed.len() {
        let facts = &analyzed[index].1;
        for &used_index in uses[index].iter().filter(|&&u| flagged[u]) {
            let used = &analyzed[used_index].0;
            // Methods are also referenced through their type name.
            let line = facts
                .references
                .get(&used.name)
                .or_else(|| {
                    used.impl_type
                        .as_ref()
                        .and_then(|t| facts.references.get(t))
                })
                .copied()
                .unwrap_or_else(|| {
                    sources[item_source[index]]
                        .1
                        .line(analyzed[index].0.name_token)
                });
            hits[index].push(Hit {
                rule: Rule::UsesSynthetic,
                line,
                detail: format!("uses `{}`", used.qualified_name()),
            });
        }
    }

    let mut findings = Vec::new();
    for (index, (item, _)) in analyzed.iter().enumerate() {
        let mut item_hits = std::mem::take(&mut hits[index]);
        if item_hits.is_empty() {
            continue;
        }
        item_hits.sort_by(|a, b| (a.line, a.rule).cmp(&(b.line, b.rule)));
        item_hits.dedup();
        let (file, lexed) = &sources[item_source[index]];
        findings.push(Finding {
            path: rel_path(root, file),
            name: Some(item.qualified_name()),
            is_test: item.is_test_fn,
            line: lexed.line(item.name_token),
            hits: item_hits,
        });
    }
    Ok((findings, modules))
}

/// Removes `.` and `..` components without touching the filesystem.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            other => out.push(other),
        }
    }
    Some(out)
}

/// File of an out-of-line module declared in `file` at inline path
/// `mod_path` (whose first `prefix.len()` segments are the file's own module
/// path).
fn module_file(file: &Path, prefix: &[String], mod_path: &[String]) -> Option<PathBuf> {
    let dir = file.parent()?;
    let stem = file.file_stem()?.to_string_lossy();
    let parent_is_tests_dir = dir.file_name().is_some_and(|name| name == "tests");
    let base = if matches!(stem.as_ref(), "lib" | "main" | "mod") || parent_is_tests_dir {
        dir.to_path_buf()
    } else {
        dir.join(stem.as_ref())
    };
    let inline = mod_path.get(prefix.len()..)?;
    let (name, parents) = inline.split_last()?;
    let base = parents.iter().fold(base, |acc, part| acc.join(part));
    [
        base.join(format!("{name}.rs")),
        base.join(name).join("mod.rs"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

const SCRIPT_EXTENSIONS: &[&str] = &["py", "sh", "r", "jl", "m", "ncl", "pl", "ipynb", "js"];

fn scan_data_file(
    root: &Path,
    path: &Path,
    manifest_sha256: &BTreeSet<String>,
) -> io::Result<Option<Finding>> {
    let rel = rel_path(root, path);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut hits = Vec::new();

    let lower = file_name.to_ascii_lowercase();
    let documentation = lower == ALLOWLIST_FILE_NAME || lower.ends_with(".md");
    if !documentation
        && ["synth", "fake", "fabricat", "mock", "dummy"]
            .iter()
            .any(|word| lower.contains(word))
    {
        hits.push(Hit {
            rule: Rule::SyntheticFileName,
            line: 1,
            detail: format!("file name `{file_name}`"),
        });
    }

    // Test data lives in these directories (and everything under the
    // top-level `testdata/`); binaries elsewhere, such as files a local run
    // leaves in a crate directory, are not test inputs.
    let in_test_data = rel.split('/').any(|part| {
        matches!(
            part,
            "tests" | "fixtures" | "testdata" | "test_data" | "data"
        )
    });

    let mut head = Vec::with_capacity(8192);
    fs::File::open(path)?.take(8192).read_to_end(&mut head)?;
    let binary = head.contains(&0)
        || std::str::from_utf8(&head).is_err_and(|error| error.error_len().is_some());
    if binary && in_test_data {
        let (sha256, _) = sha256_file(path)?;
        if !manifest_sha256.contains(&sha256) {
            hits.push(Hit {
                rule: Rule::UnmanifestedBinary,
                line: 1,
                detail: format!("sha256 {sha256} is not in a manifest"),
            });
        }
    }

    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !binary && in_test_data && SCRIPT_EXTENSIONS.contains(&extension.as_str()) {
        let text = fs::read_to_string(path)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let describes_synthetic = text.lines().any(|line| {
            line.contains("synthetic") && (line.contains("generat") || line.contains("fixture"))
                || line.contains("is synthetic")
        });
        if describes_synthetic
            || is_synthetic_name(path.file_stem().map_or("", |s| s.to_str().unwrap_or("")))
        {
            hits.push(Hit {
                rule: Rule::SyntheticGenerator,
                line: 1,
                detail: "script generates synthetic data".to_owned(),
            });
        }
    }

    Ok((!hits.is_empty()).then_some(Finding {
        path: rel,
        name: None,
        is_test: false,
        line: 1,
        hits,
    }))
}

/// Status of an allowlist entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AllowStatus {
    /// Synthetic input still to be converted to real data (plan C.2).
    Pending,
    /// Permanent, justified exception: a pure math or geometry helper that
    /// does not consume radar data, a fuzz regression input, or a corruption
    /// helper that starts from real bytes.
    Exception,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    path: String,
    name: Option<String>,
    status: AllowStatus,
    justification: String,
}

/// One allowlist entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllowEntry {
    /// Group table the entry is listed under.
    pub group: String,
    /// Workspace-relative path.
    pub path: String,
    /// Qualified item name; absent for a data file.
    pub name: Option<String>,
    /// Pending conversion or permanent exception.
    pub status: AllowStatus,
    /// Why the entry is here and what replaces it.
    pub justification: String,
}

impl AllowEntry {
    /// `(path, name)`, comparable with [`Finding::key`].
    pub fn key(&self) -> (String, Option<String>) {
        (self.path.clone(), self.name.clone())
    }
}

/// Parses an allowlist document: one array of tables per group
/// (`[[io-nexrad]]`, ...). Unknown groups and fields are errors.
pub fn parse_allowlist(text: &str) -> Result<Vec<AllowEntry>, String> {
    let groups: BTreeMap<String, Vec<RawEntry>> =
        toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(text)).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (group, entries) in groups {
        if !GROUPS.iter().any(|(known, _)| *known == group) {
            return Err(format!(
                "unknown group `{group}`; groups are {}",
                GROUPS
                    .iter()
                    .map(|(g, _)| *g)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        out.extend(entries.into_iter().map(|raw| AllowEntry {
            group: group.clone(),
            path: raw.path,
            name: raw.name,
            status: raw.status,
            justification: raw.justification,
        }));
    }
    Ok(out)
}

/// Renders findings as allowlist tables, grouped, for pasting.
pub fn allowlist_snippet(findings: &[Finding]) -> String {
    let mut by_group: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for finding in findings {
        by_group
            .entry(
                finding
                    .group()
                    .unwrap_or("<no group: add the crate to GROUPS>"),
            )
            .or_default()
            .push(finding);
    }
    let mut out = String::new();
    for (group, findings) in by_group {
        for finding in findings {
            let rules: Vec<&str> = finding.rules().into_iter().map(Rule::as_str).collect();
            let kind = match (&finding.name, finding.is_test) {
                (None, _) => "data file",
                (Some(_), true) => "test",
                (Some(_), false) => "helper",
            };
            out.push_str(&format!(
                "# {kind} (line {}): {}\n",
                finding.line,
                rules.join(", ")
            ));
            out.push_str(&format!("[[{group}]]\npath = {:?}\n", finding.path));
            if let Some(name) = &finding.name {
                out.push_str(&format!("name = {name:?}\n"));
            }
            out.push_str("status = \"pending\"\njustification = \"\"\n\n");
        }
    }
    out
}

#[cfg(test)]
mod tests;
