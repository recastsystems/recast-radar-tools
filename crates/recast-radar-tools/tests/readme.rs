//! Keeps the repository README in step with the code it describes.
//!
//! - Every ```` ```rust ```` block is one of this crate's examples, verbatim,
//!   so the README's code compiles wherever the examples do (CI builds them
//!   with `clippy --all-targets --all-features`).
//! - The crate map lists exactly the workspace crates.
//! - The feature table matches this crate's `[features]` and `src/lib.rs`.
//! - The "No unsafe" list names exactly the crates that do not opt in to the
//!   workspace lints, which forbid `unsafe`.
//!
//! The inputs are the repository's own README and manifests, read at run time.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), Box<dyn Error>>;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> PathBuf {
    crate_dir().join("..").join("..")
}

fn read_text(path: &Path) -> Result<String, String> {
    fs::read_to_string(path)
        .map(|text| text.replace("\r\n", "\n"))
        .map_err(|err| format!("{}: {err}", path.display()))
}

fn readme() -> Result<String, String> {
    read_text(&repo_root().join("README.md"))
}

/// Text between `<!-- {name}:start -->` and `<!-- {name}:end -->`.
fn section<'a>(readme: &'a str, name: &str) -> Result<&'a str, String> {
    let start = format!("<!-- {name}:start -->");
    let end = format!("<!-- {name}:end -->");
    let from = readme
        .find(&start)
        .ok_or_else(|| format!("README has no `{start}` marker"))?
        + start.len();
    let len = readme[from..]
        .find(&end)
        .ok_or_else(|| format!("README has no `{end}` marker"))?;
    Ok(&readme[from..from + len])
}

/// Cells of each body row of the single Markdown table in `text`.
fn table_rows(text: &str) -> Vec<Vec<&str>> {
    text.lines()
        .map(str::trim)
        .filter(|line| line.starts_with('|'))
        .skip(2) // header and separator
        .map(|line| {
            line.trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The contents of each `code span` in `text`.
fn code_spans(text: &str) -> Vec<&str> {
    text.split('`').skip(1).step_by(2).collect()
}

/// Package name and manifest text of every crate under `crates/`.
fn workspace_manifests() -> Result<BTreeMap<String, String>, Box<dyn Error>> {
    let mut manifests = BTreeMap::new();
    for entry in fs::read_dir(repo_root().join("crates"))? {
        let path = entry?.path().join("Cargo.toml");
        if !path.is_file() {
            continue;
        }
        let manifest = read_text(&path)?;
        // The first `name` is the package's: `[package]` comes first.
        let name = manifest
            .lines()
            .find_map(|line| line.strip_prefix("name = \"")?.strip_suffix('"'))
            .ok_or_else(|| format!("{}: no package name", path.display()))?;
        manifests.insert(name.to_owned(), manifest);
    }
    Ok(manifests)
}

/// Whether a manifest has `[lints]` with `workspace = true`.
fn opts_into_workspace_lints(manifest: &str) -> bool {
    manifest.split("\n[lints]\n").nth(1).is_some_and(|rest| {
        rest.lines()
            .take_while(|line| !line.starts_with('['))
            .any(|line| line.trim() == "workspace = true")
    })
}

/// This crate's `[features]`: feature name to its listed entries.
fn manifest_features() -> Result<BTreeMap<String, Vec<String>>, Box<dyn Error>> {
    let manifest = read_text(&crate_dir().join("Cargo.toml"))?;
    let table = manifest
        .split("\n[features]\n")
        .nth(1)
        .ok_or("Cargo.toml has no [features] table")?;
    let table = table.split("\n[").next().unwrap_or(table);
    let table: String = table
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    let mut features = BTreeMap::new();
    let mut rest = table.as_str();
    while let Some(eq) = rest.find('=') {
        let name = rest[..eq].trim().to_owned();
        let open = rest[eq..].find('[').ok_or("feature without a list")? + eq;
        let close = rest[open..].find(']').ok_or("unclosed feature list")? + open;
        let entries = rest[open + 1..close]
            .split(',')
            .map(|entry| entry.trim().trim_matches('"'))
            .filter(|entry| !entry.is_empty())
            .map(str::to_owned)
            .collect();
        features.insert(name, entries);
        rest = &rest[close + 1..];
    }
    Ok(features)
}

/// Module re-exported under each feature in `src/lib.rs`
/// (`#[cfg(feature = "x")]` followed by `pub use recast_radar_y as module;`).
fn feature_modules() -> Result<BTreeMap<String, String>, Box<dyn Error>> {
    let lib = read_text(&crate_dir().join("src").join("lib.rs"))?;
    let mut modules = BTreeMap::new();
    let mut lines = lib.lines();
    while let Some(line) = lines.next() {
        let Some(feature) = line
            .strip_prefix("#[cfg(feature = \"")
            .and_then(|rest| rest.strip_suffix("\")]"))
        else {
            continue;
        };
        let module = lines
            .next()
            .and_then(|next| next.strip_prefix("pub use "))
            .and_then(|next| next.split(" as ").nth(1))
            .and_then(|next| next.strip_suffix(';'))
            .ok_or_else(|| {
                format!("lib.rs: no `pub use ... as module;` after feature {feature}")
            })?;
        modules.insert(feature.to_owned(), module.to_owned());
    }
    Ok(modules)
}

#[test]
fn rust_code_blocks_are_the_examples_verbatim() -> TestResult {
    let readme = readme()?;
    let mut shown = BTreeSet::new();
    let mut previous = "";
    let mut lines = readme.lines();
    while let Some(line) = lines.next() {
        if line.trim() == "```rust" {
            let path = previous
                .trim()
                .strip_prefix("<!-- example: ")
                .and_then(|rest| rest.strip_suffix(" -->"))
                .ok_or_else(|| {
                    format!(
                        "a ```rust block follows {previous:?}; put \
                         `<!-- example: <path> -->` on the line before it"
                    )
                })?;
            let mut block = String::new();
            for code in lines.by_ref() {
                if code.trim() == "```" {
                    break;
                }
                block.push_str(code);
                block.push('\n');
            }
            let file = read_text(&repo_root().join(path))?;
            assert!(
                block == file,
                "README code block for {path} differs from the file; copy the file into the README"
            );
            assert!(shown.insert(path.to_owned()), "{path} is shown twice");
            previous = "```";
        } else {
            previous = line;
        }
    }

    let mut examples = BTreeSet::new();
    for entry in fs::read_dir(crate_dir().join("examples"))? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.ends_with(".rs") {
            examples.insert(format!("crates/recast-radar-tools/examples/{name}"));
        }
    }
    assert!(!examples.is_empty(), "no examples found");
    assert_eq!(shown, examples, "README must show every facade example");
    Ok(())
}

#[test]
fn crate_map_lists_every_workspace_crate() -> TestResult {
    let readme = readme()?;
    let listed: BTreeSet<String> = table_rows(section(&readme, "crate-map")?)
        .iter()
        .filter_map(|cells| code_spans(cells.first()?).first().map(|s| (*s).to_owned()))
        .collect();
    let crates: BTreeSet<String> = workspace_manifests()?.into_keys().collect();
    assert_eq!(listed, crates, "README crate map vs crates/*/Cargo.toml");
    Ok(())
}

#[test]
fn feature_table_matches_manifest_and_lib() -> TestResult {
    let readme = readme()?;
    let features = manifest_features()?;
    let modules = feature_modules()?;
    let defaults: BTreeSet<&str> = features
        .get("default")
        .ok_or("no default feature")?
        .iter()
        .map(String::as_str)
        .collect();

    let mut documented = BTreeSet::new();
    for cells in table_rows(section(&readme, "features")?) {
        let [feature, module, krate, enables, default] = cells[..] else {
            return Err(format!("feature table row {cells:?} needs 5 cells").into());
        };
        let Some(&feature) = code_spans(feature).first() else {
            continue; // the always-on `core` row
        };
        let entries = features
            .get(feature)
            .ok_or_else(|| format!("README feature `{feature}` is not in Cargo.toml"))?;
        documented.insert(feature);

        let deps: Vec<&str> = entries
            .iter()
            .filter_map(|entry| entry.strip_prefix("dep:"))
            .collect();
        assert_eq!(code_spans(krate), deps, "crate column for `{feature}`");

        let implied: Vec<&str> = entries
            .iter()
            .map(String::as_str)
            .filter(|entry| !entry.starts_with("dep:") && !entry.contains('/'))
            .collect();
        assert_eq!(
            code_spans(enables),
            implied,
            "enables column for `{feature}`"
        );

        let module_spans = code_spans(module);
        let expected_module: Vec<&str> = modules
            .get(feature)
            .map(String::as_str)
            .into_iter()
            .collect();
        assert_eq!(
            module_spans, expected_module,
            "module column for `{feature}`"
        );

        assert_eq!(
            default == "yes",
            defaults.contains(feature),
            "default column for `{feature}`"
        );
    }

    let manifest: BTreeSet<&str> = features
        .keys()
        .map(String::as_str)
        .filter(|name| *name != "default")
        .collect();
    assert_eq!(documented, manifest, "README feature table vs Cargo.toml");
    Ok(())
}

#[test]
fn unsafe_exceptions_match_the_manifests() -> TestResult {
    let readme = readme()?;
    let named: BTreeSet<String> = code_spans(section(&readme, "lint-exceptions")?)
        .into_iter()
        .filter(|span| span.starts_with("recast-radar-"))
        .map(str::to_owned)
        .collect();
    let without_lints: BTreeSet<String> = workspace_manifests()?
        .into_iter()
        .filter(|(_, manifest)| !opts_into_workspace_lints(manifest))
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        named, without_lints,
        "README \"No unsafe\" list vs crates without `[lints] workspace = true`"
    );
    Ok(())
}
