//! Keeps the repository README, the crate manifests and CI in step with each
//! other.
//!
//! - Every ```` ```rust ```` block is one of this crate's examples, verbatim,
//!   so the README's code compiles wherever the examples do (CI builds them
//!   with `clippy --all-targets --all-features`).
//! - The crate map lists exactly the workspace crates, and its Module column
//!   matches the re-exports in `src/lib.rs`.
//! - Every crate's manifest has the package metadata (description, keywords,
//!   categories, readme, workspace edition, license and rust-version).
//! - The feature table matches this crate's `[features]` and `src/lib.rs`,
//!   including which features are on by default, directly or through another
//!   default feature.
//! - Each feature enables the features of the member crates its crate depends
//!   on, apart from the exceptions listed here and in the README.
//! - The "No unsafe" list names exactly the crates that do not opt in to the
//!   workspace lints, which forbid `unsafe`.
//! - The minimum Rust version agrees between the workspace manifest, the
//!   README and the toolchain pinned in `.github/workflows/ci.yml`.
//!
//! Not checked: prose outside these tables and lists, the example output
//! excerpts, and code blocks other than ```` ```rust ````.
//!
//! The inputs are the repository's own files, read at run time, so this file
//! is not part of the packaged crate (`exclude` in Cargo.toml).

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), Box<dyn Error>>;

/// Facade features that do not enable the feature of a member crate that
/// their crate depends on: (feature, feature it does not enable). The README's
/// Features section and `src/lib.rs` describe each one.
const NOT_IMPLIED: &[(&str, &str)] = &[
    // recast-radar-data uses recast-radar-io-jma only internally.
    ("net", "jma"),
];

/// Facade features that enable a feature whose crate their crate does not
/// depend on: (feature, feature it also enables). Described in the same places.
const ALSO_IMPLIED: &[(&str, &str)] = &[
    // `io` turns on every format decoder; the router does not read Level III.
    ("io", "level3"),
];

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

/// Body of the TOML table `header` (for example `[package]`) in a manifest:
/// the lines after the header line, up to the next table header.
fn toml_table<'a>(manifest: &'a str, header: &str) -> Option<&'a str> {
    let start = if manifest.starts_with(&format!("{header}\n")) {
        header.len() + 1
    } else {
        manifest.find(&format!("\n{header}\n"))? + header.len() + 2
    };
    let body = &manifest[start..];
    let end = body.find("\n[").map_or(body.len(), |at| at + 1);
    Some(&body[..end])
}

/// Value text after `key = ` on a line of a TOML table body.
fn toml_value<'a>(table: &'a str, key: &str) -> Option<&'a str> {
    table.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name.trim() == key).then(|| value.trim())
    })
}

/// Entries of a single-line TOML string array such as `["a", "b"]`.
fn toml_string_array(value: &str) -> Option<Vec<&str>> {
    let inner = value.strip_prefix('[')?.strip_suffix(']')?;
    Some(
        inner
            .split(',')
            .map(|entry| entry.trim().trim_matches('"'))
            .filter(|entry| !entry.is_empty())
            .collect(),
    )
}

/// One crate under `crates/`.
struct Member {
    dir: PathBuf,
    manifest: String,
}

/// Every crate under `crates/`, by package name.
fn workspace_members() -> Result<BTreeMap<String, Member>, Box<dyn Error>> {
    let mut members = BTreeMap::new();
    for entry in fs::read_dir(repo_root().join("crates"))? {
        let dir = entry?.path();
        let path = dir.join("Cargo.toml");
        if !path.is_file() {
            continue;
        }
        let manifest = read_text(&path)?;
        let name = toml_table(&manifest, "[package]")
            .and_then(|package| toml_value(package, "name"))
            .map(|name| name.trim_matches('"').to_owned())
            .ok_or_else(|| format!("{}: no package name", path.display()))?;
        members.insert(name, Member { dir, manifest });
    }
    Ok(members)
}

/// Whether a manifest has `[lints]` with `workspace = true`.
fn opts_into_workspace_lints(manifest: &str) -> bool {
    toml_table(manifest, "[lints]")
        .is_some_and(|lints| lints.lines().any(|line| line.trim() == "workspace = true"))
}

/// Names of the `recast-radar-*` crates in a manifest's `[dependencies]`.
fn member_dependencies(manifest: &str) -> BTreeSet<&str> {
    toml_table(manifest, "[dependencies]")
        .into_iter()
        .flat_map(str::lines)
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
        .filter(|name| name.starts_with("recast-radar-"))
        .collect()
}

/// This crate's `[features]`: feature name to its listed entries.
fn manifest_features() -> Result<BTreeMap<String, Vec<String>>, Box<dyn Error>> {
    let manifest = read_text(&crate_dir().join("Cargo.toml"))?;
    let table = toml_table(&manifest, "[features]").ok_or("Cargo.toml has no [features] table")?;
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

/// Features a feature's list turns on directly (not `dep:` or `crate/feature`
/// entries).
fn implied_features(entries: &[String]) -> impl Iterator<Item = &str> {
    entries
        .iter()
        .map(String::as_str)
        .filter(|entry| !entry.starts_with("dep:") && !entry.contains('/'))
}

/// `feature` and every feature it turns on, directly or indirectly.
fn feature_closure<'a>(
    features: &'a BTreeMap<String, Vec<String>>,
    feature: &'a str,
) -> BTreeSet<&'a str> {
    let mut closure = BTreeSet::new();
    let mut pending = vec![feature];
    while let Some(next) = pending.pop() {
        if closure.insert(next)
            && let Some(entries) = features.get(next)
        {
            pending.extend(implied_features(entries));
        }
    }
    closure
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

/// Every `pub use recast_radar_x as module;` in `src/lib.rs`, feature-gated or
/// not: package name (`recast-radar-x`) to module name.
fn crate_modules() -> Result<BTreeMap<String, String>, Box<dyn Error>> {
    let lib = read_text(&crate_dir().join("src").join("lib.rs"))?;
    Ok(lib
        .lines()
        .filter_map(|line| {
            let (lib_name, module) = line
                .strip_prefix("pub use ")?
                .strip_suffix(';')?
                .split_once(" as ")?;
            Some((lib_name.replace('_', "-"), module.to_owned()))
        })
        .collect())
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
    let crates: BTreeSet<String> = workspace_members()?.into_keys().collect();
    assert_eq!(listed, crates, "README crate map vs crates/*/Cargo.toml");
    Ok(())
}

#[test]
fn crate_map_modules_match_the_facade_reexports() -> TestResult {
    let readme = readme()?;
    let modules = crate_modules()?;
    let mut listed = BTreeSet::new();
    for cells in table_rows(section(&readme, "crate-map")?) {
        let [krate, module, _contents] = cells[..] else {
            return Err(format!("crate map row {cells:?} needs 3 cells").into());
        };
        let krate = *code_spans(krate)
            .first()
            .ok_or_else(|| format!("crate map row {cells:?} has no crate name"))?;
        let expected: Vec<&str> = modules.get(krate).map(String::as_str).into_iter().collect();
        assert_eq!(
            code_spans(module),
            expected,
            "Module column for `{krate}` vs src/lib.rs"
        );
        listed.insert(krate);
    }
    for krate in modules.keys() {
        assert!(
            listed.contains(krate.as_str()),
            "src/lib.rs re-exports `{krate}`, which the crate map does not list"
        );
    }
    Ok(())
}

#[test]
fn every_crate_has_package_metadata() -> TestResult {
    let mut problems = Vec::new();
    for (name, member) in workspace_members()? {
        let Some(package) = toml_table(&member.manifest, "[package]") else {
            problems.push(format!("{name}: no [package] table"));
            continue;
        };
        for key in ["edition", "license", "rust-version"] {
            if toml_value(package, &format!("{key}.workspace")) != Some("true") {
                problems.push(format!("{name}: `{key}.workspace = true` missing"));
            }
        }
        if toml_value(package, "description").is_none_or(|value| value.len() <= 2) {
            problems.push(format!("{name}: no description"));
        }
        match toml_value(package, "keywords").and_then(toml_string_array) {
            Some(keywords) if (1..=5).contains(&keywords.len()) => {}
            other => problems.push(format!("{name}: keywords {other:?}, want 1 to 5")),
        }
        if toml_value(package, "categories")
            .and_then(toml_string_array)
            .is_none_or(|categories| categories.is_empty())
        {
            problems.push(format!("{name}: no categories"));
        }
        let readme = match (
            toml_value(package, "readme.workspace"),
            toml_value(package, "readme"),
        ) {
            (Some("true"), _) => Some(repo_root().join("README.md")),
            (_, Some(path)) => Some(member.dir.join(path.trim_matches('"'))),
            _ => None,
        };
        match readme {
            Some(path) if path.is_file() => {}
            Some(path) => problems.push(format!("{name}: readme {} not found", path.display())),
            None => problems.push(format!("{name}: no readme")),
        }
    }
    assert!(problems.is_empty(), "package metadata: {problems:#?}");
    Ok(())
}

#[test]
fn feature_table_matches_manifest_and_lib() -> TestResult {
    let readme = readme()?;
    let features = manifest_features()?;
    let modules = feature_modules()?;
    let defaults = features.get("default").ok_or("no default feature")?;

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

        let implied: Vec<&str> = implied_features(entries).collect();
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

        // "yes" when listed in `default`, "via `x`" when only default
        // features turn it on, empty when it is off by default.
        let expected_default = if defaults.iter().any(|name| name == feature) {
            "yes".to_owned()
        } else {
            let via: Vec<String> = defaults
                .iter()
                .filter(|name| feature_closure(&features, name.as_str()).contains(feature))
                .map(|name| format!("`{name}`"))
                .collect();
            if via.is_empty() {
                String::new()
            } else {
                format!("via {}", via.join(" "))
            }
        };
        assert_eq!(default, expected_default, "default column for `{feature}`");
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
fn features_enable_the_features_of_member_dependencies() -> TestResult {
    let features = manifest_features()?;
    let members = workspace_members()?;
    // Member crate to the facade feature that adds it (`dep:crate`).
    let feature_of_crate: BTreeMap<&str, &str> = features
        .iter()
        .flat_map(|(feature, entries)| {
            entries
                .iter()
                .filter_map(|entry| entry.strip_prefix("dep:"))
                .map(move |krate| (krate, feature.as_str()))
        })
        .collect();

    for (&krate, &feature) in &feature_of_crate {
        let member = members.get(krate).ok_or_else(|| {
            format!("feature `{feature}` adds `{krate}`, which is not under crates/")
        })?;
        let entries = &features[feature];
        let depended_on: BTreeSet<&str> = member_dependencies(&member.manifest)
            .into_iter()
            .filter_map(|dependency| feature_of_crate.get(dependency).copied())
            .collect();
        let implied: BTreeSet<&str> = implied_features(entries).collect();

        for &(exception, not_implied) in NOT_IMPLIED.iter().filter(|(f, _)| *f == feature) {
            assert!(
                depended_on.contains(not_implied) && !implied.contains(not_implied),
                "NOT_IMPLIED ({exception}, {not_implied}) no longer applies: `{krate}` \
                 does not depend on that feature's crate, or `{exception}` enables it. \
                 Remove the exception here, in the README Features section, in src/lib.rs \
                 and in Cargo.toml"
            );
        }
        for &(exception, also) in ALSO_IMPLIED.iter().filter(|(f, _)| *f == feature) {
            assert!(
                !depended_on.contains(also),
                "ALSO_IMPLIED ({exception}, {also}) no longer applies: `{krate}` now \
                 depends on that feature's crate. Remove the exception here, in the README \
                 Features section, in src/lib.rs and in Cargo.toml"
            );
        }
        let expected: BTreeSet<&str> = depended_on
            .into_iter()
            .filter(|dependency| !NOT_IMPLIED.contains(&(feature, *dependency)))
            .chain(
                ALSO_IMPLIED
                    .iter()
                    .filter(|(f, _)| *f == feature)
                    .map(|&(_, also)| also),
            )
            .collect();
        assert_eq!(
            implied, expected,
            "`{feature}` should enable the features of the member crates `{krate}` depends on"
        );
    }
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
    let without_lints: BTreeSet<String> = workspace_members()?
        .into_iter()
        .filter(|(_, member)| !opts_into_workspace_lints(&member.manifest))
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        named, without_lints,
        "README \"No unsafe\" list vs crates without `[lints] workspace = true`"
    );
    Ok(())
}

#[test]
fn minimum_rust_version_agrees_across_manifest_readme_and_ci() -> TestResult {
    let workspace = read_text(&repo_root().join("Cargo.toml"))?;
    let msrv = toml_table(&workspace, "[workspace.package]")
        .and_then(|package| toml_value(package, "rust-version"))
        .map(|value| value.trim_matches('"'))
        .ok_or("Cargo.toml: no [workspace.package] rust-version")?;

    let readme = readme()?;
    assert!(
        readme.contains(&format!("Minimum Rust version: {msrv} ")),
        "README should say `Minimum Rust version: {msrv} (...)`"
    );

    let ci = read_text(&repo_root().join(".github").join("workflows").join("ci.yml"))?;
    let pins: BTreeSet<&str> = ci
        .split("dtolnay/rust-toolchain@")
        .skip(1)
        .filter_map(|rest| rest.split_whitespace().next())
        .filter(|toolchain| *toolchain != "stable")
        .collect();
    assert!(
        !pins.is_empty(),
        "ci.yml pins no job to the minimum Rust version"
    );
    for pin in pins {
        assert!(
            pin == msrv || pin.starts_with(&format!("{msrv}.")),
            "ci.yml pins Rust {pin}, but the minimum Rust version is {msrv}"
        );
        assert!(
            readme.contains(&format!("Rust {pin},")),
            "README's CI list should name the pinned Rust {pin}"
        );
    }
    Ok(())
}
