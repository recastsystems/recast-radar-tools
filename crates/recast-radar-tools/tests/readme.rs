//! Keeps the repository README, the user guide, the crate manifests and CI
//! in step with each other.
//!
//! - Every ```` ```rust ```` block of the README and of the user guide
//!   (`docs/guide/*.md`) is one of this crate's examples, verbatim, so the
//!   documented code compiles wherever the examples do (CI builds them with
//!   `clippy --all-targets --all-features`), and every example is shown.
//!   Fragments that are not examples are marked ```` ```rust,ignore ````.
//! - Every ```` ```text ```` block of those documents is an example's output,
//!   marked `<!-- output: <example> <args> -->` (or `output-head:` for the
//!   first lines, `output-unchecked:` for a live download) on the line
//!   before it, naming an example shown in the same document, with testdata
//!   ids that exist and repository paths that resolve.
//!   `tools/check_example_outputs.py` (CI job `examples`) runs the examples
//!   as marked and compares their output with the blocks.
//! - Relative links in the README and the guide point at existing files.
//! - Every `model::` path the README and the guide name in prose resolves
//!   through the facade.
//! - The crate map lists exactly the workspace crates, and its Module column
//!   matches the re-exports in `src/lib.rs`.
//! - Every crate's manifest has the package metadata (description, keywords,
//!   categories, readme, workspace version, edition, license and
//!   rust-version), and its directory holds both license texts, so the
//!   packaged crate carries them.
//! - Every internal normal or build dependency states the workspace version
//!   beside its path, as `cargo package` needs.
//! - The feature table matches this crate's `[features]` and `src/lib.rs`,
//!   including which features are on by default, directly or through another
//!   default feature.
//! - Each feature enables the features of the member crates its crate depends
//!   on (in any `[dependencies]` table, target-specific ones included), apart
//!   from the exceptions listed here and in the README.
//! - The "No unsafe" list names exactly the crates that do not opt in to the
//!   workspace lints, which forbid `unsafe`, and every library crate root
//!   denies `clippy::unwrap_used` and `clippy::expect_used` outside its unit
//!   tests, as that section says.
//! - The minimum Rust version agrees between the workspace manifest, the
//!   README and the toolchain pinned in `.github/workflows/ci.yml`.
//!
//! Not checked here: prose outside these tables and lists, the printed
//! outputs themselves (the script above compares those), anchors within
//! links, and code blocks other than ```` ```rust ```` and ```` ```text ````.
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
/// Empty since recast-radar-data dropped its recast-radar-io-jma dependency
/// (stream E.1), which was the one case.
const NOT_IMPLIED: &[(&str, &str)] = &[];

/// Facade features that enable a feature whose crate their crate does not
/// depend on: (feature, feature it also enables). Described in the same places.
/// Empty since the router reads Level III products (stream level3-complete),
/// which ended the one case (`io` enabling `level3`).
const ALSO_IMPLIED: &[(&str, &str)] = &[];

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

/// Bodies of a manifest's `[<kind>]` table and of every
/// `[target.<cfg>.<kind>]` table, for `kind` `dependencies`,
/// `dev-dependencies` or `build-dependencies`.
fn dependency_tables<'a>(manifest: &'a str, kind: &str) -> Vec<&'a str> {
    let target_suffix = format!(".{kind}");
    let mut tables = Vec::new();
    let mut offset = 0;
    for line in manifest.split_inclusive('\n') {
        offset += line.len();
        let Some(name) = line
            .trim()
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        else {
            continue;
        };
        if name != kind && !(name.starts_with("target.") && name.ends_with(&target_suffix)) {
            continue;
        }
        let body = &manifest[offset..];
        let end = if body.starts_with('[') {
            0
        } else {
            body.find("\n[").map_or(body.len(), |at| at + 1)
        };
        tables.push(&body[..end]);
    }
    tables
}

/// `(name, value)` of each `recast-radar-*` entry in the given tables.
fn member_entries<'a>(tables: &[&'a str]) -> Vec<(&'a str, &'a str)> {
    tables
        .iter()
        .flat_map(|table| table.lines())
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.trim(), value.trim()))
        .filter(|(name, _)| name.starts_with("recast-radar-"))
        .collect()
}

/// Names of the `recast-radar-*` crates among a manifest's normal
/// dependencies (`[dependencies]` and every target-specific
/// `[target.<cfg>.dependencies]`).
fn member_dependencies(manifest: &str) -> BTreeSet<&str> {
    member_entries(&dependency_tables(manifest, "dependencies"))
        .into_iter()
        .map(|(name, _)| name)
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

/// The documents whose Rust code blocks must be examples: the README and
/// every page of the user guide (`docs/guide/*.md`), as (path relative to
/// the repository root, text).
fn documents() -> Result<Vec<(String, String)>, Box<dyn Error>> {
    let mut documents = vec![("README.md".to_owned(), readme()?)];
    let mut pages: Vec<PathBuf> = fs::read_dir(repo_root().join("docs").join("guide"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()?;
    pages.sort();
    for page in pages {
        if page.extension().is_some_and(|extension| extension == "md") {
            let name = page
                .file_name()
                .ok_or("guide page without a name")?
                .to_string_lossy()
                .into_owned();
            documents.push((format!("docs/guide/{name}"), read_text(&page)?));
        }
    }
    Ok(documents)
}

#[test]
fn rust_code_blocks_are_the_examples_verbatim() -> TestResult {
    let mut shown = BTreeSet::new();
    for (document, text) in documents()? {
        let mut in_document = BTreeSet::new();
        let mut previous = "";
        let mut lines = text.lines();
        while let Some(line) = lines.next() {
            if line.trim() == "```rust" {
                let path = previous
                    .trim()
                    .strip_prefix("<!-- example: ")
                    .and_then(|rest| rest.strip_suffix(" -->"))
                    .ok_or_else(|| {
                        format!(
                            "{document}: a ```rust block follows {previous:?}; put \
                             `<!-- example: <path> -->` on the line before it, or mark \
                             a fragment ```rust,ignore"
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
                    "{document}: the code block for {path} differs from the file; \
                     run `python tools/sync_doc_examples.py`"
                );
                assert!(
                    in_document.insert(path.to_owned()),
                    "{document}: {path} is shown twice"
                );
                shown.insert(path.to_owned());
                previous = "```";
            } else {
                previous = line;
            }
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
    assert_eq!(
        shown, examples,
        "the README and the user guide must show every facade example"
    );
    Ok(())
}

/// Markers that introduce an example's output; `tools/check_example_outputs.py`
/// reads the same ones.
const OUTPUT_MARKERS: [&str; 3] = [
    "<!-- output: ",
    "<!-- output-head: ",
    "<!-- output-unchecked: ",
];

#[test]
fn text_blocks_are_marked_example_outputs() -> TestResult {
    let mut marked = 0;
    for (document, text) in documents()? {
        let mut previous = "";
        for line in text.lines() {
            if line.trim() == "```text" {
                let marker = previous.trim();
                let body = OUTPUT_MARKERS
                    .iter()
                    .find_map(|prefix| marker.strip_prefix(prefix))
                    .and_then(|rest| rest.strip_suffix(" -->"))
                    .ok_or_else(|| {
                        format!(
                            "{document}: a ```text block follows {previous:?}; put                              `<!-- output: <example> <args> -->` (or `output-head:`,                              `output-unchecked:`) on the line before it; see                              tools/check_example_outputs.py"
                        )
                    })?;
                let mut words = body.split_whitespace();
                let example = words
                    .next()
                    .ok_or_else(|| format!("{document}: `{marker}` names no example"))?;
                let path = format!("crates/recast-radar-tools/examples/{example}.rs");
                assert!(
                    repo_root().join(&path).is_file(),
                    "{document}: `{marker}`: no file {path}"
                );
                assert!(
                    text.contains(&format!("<!-- example: {path} -->")),
                    "{document}: `{marker}`: the document does not show {path}"
                );
                if !marker.starts_with("<!-- output-unchecked: ") {
                    for word in words {
                        if let Some(id) = word.strip_prefix("testdata:") {
                            assert!(
                                recast_radar_testdata::entry(id).is_some(),
                                "{document}: `{marker}`: no testdata id {id}"
                            );
                        } else if let Some(relative) = word.strip_prefix("repo:") {
                            assert!(
                                repo_root().join(relative).is_file(),
                                "{document}: `{marker}`: no file {relative}"
                            );
                        }
                    }
                }
                marked += 1;
            }
            previous = line;
        }
    }
    assert!(marked > 0, "no example outputs found");
    Ok(())
}

/// Relative link targets (`[text](target)`) in a Markdown text: no URLs, no
/// in-page anchors, the `#fragment` removed.
fn relative_links(text: &str) -> Vec<&str> {
    text.split("](")
        .skip(1)
        .filter_map(|rest| rest.split(')').next())
        .map(|target| target.split('#').next().unwrap_or(target))
        .filter(|target| {
            !target.is_empty() && !target.contains("://") && !target.starts_with("mailto:")
        })
        .collect()
}

/// Every `model::` path that the README and the guide name outside code
/// blocks (whose code is an example and compiles). This `use` does not
/// compile if one of them does not resolve through the facade, and
/// `model_paths_named_in_the_docs_resolve` fails if a document names a path
/// that is not listed in [`DOC_MODEL_PATHS`].
#[allow(unused_imports)]
use recast_radar_tools::model::{
    ArrayBuf, Coding, FieldData, FloatWidth, LinearTransform, RowRef, Scalar, Volume, bounded_read,
    bounded_read::read_to_end_limited, fm301::ArrayRef, fm301::volume_view, merge_volumes,
};

/// The paths of the `use` above, as the documents write them.
const DOC_MODEL_PATHS: &[&str] = &[
    "model::ArrayBuf",
    "model::Coding",
    "model::FieldData",
    "model::FloatWidth",
    "model::LinearTransform",
    "model::RowRef",
    "model::Scalar",
    "model::Volume",
    "model::bounded_read",
    "model::bounded_read::read_to_end_limited",
    "model::fm301::ArrayRef",
    "model::fm301::volume_view",
    "model::merge_volumes",
];

/// `text` without its fenced code blocks.
fn prose(text: &str) -> String {
    let mut fenced = false;
    let mut out = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The `model::` paths in the code spans of `text`: from `model::` up to the
/// first character that cannot be part of a path.
fn model_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for span in code_spans(text) {
        for (start, _) in span.match_indices("model::") {
            let at_segment_start = span[..start]
                .chars()
                .next_back()
                .is_none_or(|before| !(before.is_alphanumeric() || before == '_'));
            if !at_segment_start {
                continue;
            }
            let path: String = span[start..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
                .collect();
            paths.push(path.trim_end_matches(':').to_owned());
        }
    }
    paths
}

#[test]
fn model_paths_named_in_the_docs_resolve() -> TestResult {
    let mut unlisted = Vec::new();
    let mut named = 0;
    for (document, text) in documents()? {
        for path in model_paths(&prose(&text)) {
            named += 1;
            if !DOC_MODEL_PATHS.contains(&path.as_str()) {
                unlisted.push(format!("{document}: {path}"));
            }
        }
    }
    assert!(named > 0, "no model:: paths found; is the scan broken?");
    assert!(
        unlisted.is_empty(),
        "model:: paths not in DOC_MODEL_PATHS (add each to the list and to the `use` \
         above it, which checks that it resolves): {unlisted:#?}"
    );
    Ok(())
}

#[test]
fn relative_links_in_the_readme_and_guide_resolve() -> TestResult {
    let mut broken = Vec::new();
    for (document, text) in documents()? {
        let dir = repo_root().join(&document);
        let dir = dir.parent().ok_or("document without a directory")?;
        for target in relative_links(&text) {
            if !dir.join(target).exists() {
                broken.push(format!("{document}: {target}"));
            }
        }
    }
    assert!(broken.is_empty(), "broken relative links: {broken:#?}");
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
        for key in ["version", "edition", "license", "rust-version"] {
            if toml_value(package, &format!("{key}.workspace")) != Some("true") {
                problems.push(format!("{name}: `{key}.workspace = true` missing"));
            }
        }
        // `license` names both licenses; cargo packages only files under the
        // crate's directory, so each crate carries the texts.
        for license in ["LICENSE-MIT", "LICENSE-APACHE"] {
            let path = member.dir.join(license);
            match (read_text(&path), read_text(&repo_root().join(license))) {
                (Ok(copy), Ok(original)) if copy == original => {}
                (Ok(_), Ok(_)) => {
                    problems.push(format!("{name}: {license} differs from the root copy"))
                }
                (Err(error), _) | (_, Err(error)) => problems.push(format!("{name}: {error}")),
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
            // Cargo infers the crate's own README.md (and nightly cargo warns
            // when a manifest names it explicitly).
            _ if member.dir.join("README.md").is_file() => Some(member.dir.join("README.md")),
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

/// A packaged crate's manifest names its dependencies by version, so every
/// internal dependency a package keeps (normal and build dependencies; cargo
/// drops path-only dev-dependencies) states the workspace version beside its
/// path.
#[test]
fn internal_dependencies_state_the_workspace_version() -> TestResult {
    let workspace = read_text(&repo_root().join("Cargo.toml"))?;
    let version = toml_table(&workspace, "[workspace.package]")
        .and_then(|package| toml_value(package, "version"))
        .ok_or("Cargo.toml: no [workspace.package] version")?;
    let mut problems = Vec::new();
    for (name, member) in workspace_members()? {
        let mut tables = dependency_tables(&member.manifest, "dependencies");
        tables.extend(dependency_tables(&member.manifest, "build-dependencies"));
        for (dependency, value) in member_entries(&tables) {
            let versioned = value
                .split(',')
                .filter_map(|part| part.split_once('='))
                .any(|(key, text)| {
                    key.trim_start_matches('{').trim() == "version" && text.trim() == version
                });
            if !versioned {
                problems.push(format!("{name} -> {dependency}: {value}"));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "internal dependencies without `version = {version}`: {problems:#?}"
    );
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
            continue; // the always-on `model` row
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

/// The README's "No unsafe" section says library code denies
/// `clippy::unwrap_used` and `clippy::expect_used`: every library crate root
/// (`src/lib.rs`) carries the attribute, outside its unit tests.
#[test]
fn library_roots_deny_unwrap_and_expect() -> TestResult {
    const ATTRIBUTE: &str =
        "#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]";
    assert!(readme()?.contains("denies `clippy::unwrap_used` and `clippy::expect_used`"));
    let mut roots = 0;
    for (name, member) in workspace_members()? {
        let root = member.dir.join("src").join("lib.rs");
        if !root.is_file() {
            continue;
        }
        roots += 1;
        let text = read_text(&root)?;
        assert!(
            text.lines().any(|line| line.trim() == ATTRIBUTE),
            "{name}: src/lib.rs lacks `{ATTRIBUTE}`"
        );
    }
    assert!(roots > 0, "no library crate roots found");
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
        .filter(|toolchain| !matches!(*toolchain, "stable" | "beta" | "nightly"))
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
