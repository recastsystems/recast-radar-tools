//! `docs/testdata/corpus.md` lists every manifest entry and indexes every
//! entry by tag. That part of the page is generated from the manifests and
//! must stay current: this test renders it and compares it with the text
//! between the markers. After changing a manifest, regenerate it with
//!
//! ```text
//! RECAST_RADAR_TESTDATA_BLESS=1 cargo test -p recast-radar-testdata --test corpus_doc
//! ```

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use recast_radar_testdata::{
    Entry, Manifest, manifest, manifest_files, testdata_dir, workspace_root,
};

const BLESS_ENV: &str = "RECAST_RADAR_TESTDATA_BLESS";
const BEGIN: &str = "<!-- BEGIN GENERATED: crates/recast-radar-testdata/tests/corpus_doc.rs -->";
const END: &str = "<!-- END GENERATED -->";

fn doc_path() -> PathBuf {
    workspace_root()
        .join("docs")
        .join("testdata")
        .join("corpus.md")
}

/// Manifest files with their entries, in load order.
fn manifests() -> Vec<(String, Manifest)> {
    let files = match manifest_files(testdata_dir()) {
        Ok(files) => files,
        Err(e) => panic!("{e}"),
    };
    files
        .into_iter()
        .map(|path| {
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) => panic!("read {}: {e}", path.display()),
            };
            let parsed = match Manifest::from_toml_str(&text) {
                Ok(parsed) => parsed,
                Err(e) => panic!("parse {}: {e}", path.display()),
            };
            (display_path(&path), parsed)
        })
        .collect()
}

fn display_path(path: &Path) -> String {
    let rel = path.strip_prefix(workspace_root()).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Byte count with thousands separators.
fn bytes(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Natural order: digit runs compare as numbers (`msg:2` < `msg:13`).
fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn chunks(s: &str) -> Vec<(bool, &str)> {
        let mut out = Vec::new();
        let mut start = 0;
        let bytes = s.as_bytes();
        for i in 1..=bytes.len() {
            if i == bytes.len() || bytes[i].is_ascii_digit() != bytes[start].is_ascii_digit() {
                out.push((bytes[start].is_ascii_digit(), &s[start..i]));
                start = i;
            }
        }
        out
    }
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(cb.iter()) {
        let ord = match (x, y) {
            ((true, x), (true, y)) => {
                let (xt, yt) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                xt.len()
                    .cmp(&yt.len())
                    .then_with(|| xt.cmp(yt))
                    .then_with(|| x.len().cmp(&y.len()))
            }
            ((_, x), (_, y)) => x.cmp(y),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    ca.len().cmp(&cb.len())
}

/// Splits an id at its last run of digits: (prefix, digits, suffix).
fn split_last_number(id: &str) -> Option<(&str, &str, &str)> {
    // Byte positions next to ASCII digits are always char boundaries.
    let bytes = id.as_bytes();
    let end = bytes.iter().rposition(u8::is_ascii_digit)? + 1;
    let start = bytes[..end]
        .iter()
        .rposition(|b| !b.is_ascii_digit())
        .map_or(0, |i| i + 1);
    Some((&id[..start], &id[start..end], &id[end..]))
}

/// Formats ids in the given order, writing a run of three or more ids that
/// differ only in a consecutive, equal-width last number as
/// `prefix{first..last}suffix`.
fn compact_ids(ids: &[&str]) -> String {
    let mut parts = Vec::new();
    let mut i = 0;
    while i < ids.len() {
        let mut j = i + 1;
        if let Some((prefix, digits, suffix)) = split_last_number(ids[i]) {
            let mut previous: Option<u64> = digits.parse().ok();
            while j < ids.len() {
                let Some((p, d, s)) = split_last_number(ids[j]) else {
                    break;
                };
                let next: Option<u64> = d.parse().ok();
                let consecutive =
                    matches!((previous, next), (Some(a), Some(b)) if a.checked_add(1) == Some(b));
                if p != prefix || s != suffix || d.len() != digits.len() || !consecutive {
                    break;
                }
                previous = next;
                j += 1;
            }
            if j - i >= 3 {
                let last = split_last_number(ids[j - 1]).map_or("", |(_, d, _)| d);
                parts.push(format!("`{prefix}{{{digits}..{last}}}{suffix}`"));
                i = j;
                continue;
            }
        }
        parts.push(format!("`{}`", ids[i]));
        i += 1;
    }
    parts.join(", ")
}

fn location(e: &Entry) -> String {
    match (&e.committed, e.ephemeral) {
        (Some(path), _) => format!("committed `{path}`"),
        (None, true) => "download (ephemeral URL)".to_owned(),
        (None, false) => "download".to_owned(),
    }
}

fn render() -> String {
    let manifests = manifests();
    let all = &manifest().files;
    let mut out = String::new();
    let w = &mut out;

    // Totals.
    let _ = writeln!(w, "### Totals\n");
    let _ = writeln!(
        w,
        "| manifest | entries | committed files | committed bytes | download files | download bytes |"
    );
    let _ = writeln!(w, "|---|---:|---:|---:|---:|---:|");
    let mut grand = [0u64; 5];
    for (name, m) in &manifests {
        let committed: Vec<&Entry> = m.files.iter().filter(|e| e.committed.is_some()).collect();
        let downloads: Vec<&Entry> = m.files.iter().filter(|e| e.committed.is_none()).collect();
        let row = [
            m.files.len() as u64,
            committed.len() as u64,
            committed.iter().map(|e| e.size).sum(),
            downloads.len() as u64,
            downloads.iter().map(|e| e.size).sum(),
        ];
        for (g, r) in grand.iter_mut().zip(row) {
            *g += r;
        }
        let _ = writeln!(
            w,
            "| `{name}` | {} | {} | {} | {} | {} |",
            row[0],
            row[1],
            bytes(row[2]),
            row[3],
            bytes(row[4])
        );
    }
    let _ = writeln!(
        w,
        "| **all** | **{}** | **{}** | **{}** | **{}** | **{}** |\n",
        grand[0],
        grand[1],
        bytes(grand[2]),
        grand[3],
        bytes(grand[4])
    );

    let mut formats: BTreeMap<&str, [u64; 2]> = BTreeMap::new();
    for e in all {
        let slot = formats.entry(e.format.as_str()).or_default();
        slot[usize::from(e.committed.is_none())] += 1;
    }
    let _ = writeln!(w, "| format | committed | download |");
    let _ = writeln!(w, "|---|---:|---:|");
    for (format, [committed, download]) in &formats {
        let _ = writeln!(w, "| `{format}` | {committed} | {download} |");
    }
    let _ = writeln!(w);

    // Every entry.
    let _ = writeln!(w, "### Entries\n");
    for (name, m) in &manifests {
        let _ = writeln!(w, "#### `{name}`\n");
        if m.files.is_empty() {
            let _ = writeln!(w, "No entries.\n");
            continue;
        }
        let _ = writeln!(w, "| id | format | where | bytes | derived from |");
        let _ = writeln!(w, "|---|---|---|---:|---|");
        for e in &m.files {
            let derived = e
                .derived_from
                .as_deref()
                .map_or_else(String::new, |d| format!("`{d}`"));
            let _ = writeln!(
                w,
                "| `{}` | `{}` | {} | {} | {derived} |",
                e.id,
                e.format.as_str(),
                location(e),
                bytes(e.size)
            );
        }
        let _ = writeln!(w);
    }

    // Every entry by tag.
    let _ = writeln!(w, "### Index by tag\n");
    let _ = writeln!(
        w,
        "Tags are grouped by the part before `:`. Ids are in manifest order. \
         `prefix{{a..b}}suffix` stands for every id with a number from `a` to `b` \
         (same digit count) in that place.\n"
    );
    let mut namespaces: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in all {
        for tag in &e.tags {
            let ns = tag.split_once(':').map_or("", |(ns, _)| ns);
            let tags = namespaces.entry(ns).or_default();
            if !tags.contains(&tag.as_str()) {
                tags.push(tag);
            }
        }
    }
    let untagged: Vec<&str> = all
        .iter()
        .filter(|e| e.tags.is_empty())
        .map(|e| e.id.as_str())
        .collect();
    for (ns, tags) in &mut namespaces {
        tags.sort_by(|a, b| natural_cmp(a, b));
        let heading = if ns.is_empty() {
            "Tags without a namespace".to_owned()
        } else {
            format!("`{ns}:`")
        };
        let _ = writeln!(w, "#### {heading}\n");
        for tag in tags.iter() {
            let ids: Vec<&str> = all
                .iter()
                .filter(|e| e.tags.iter().any(|t| t == tag))
                .map(|e| e.id.as_str())
                .collect();
            let _ = writeln!(w, "- `{tag}` ({}): {}", ids.len(), compact_ids(&ids));
        }
        let _ = writeln!(w);
    }
    if !untagged.is_empty() {
        let _ = writeln!(w, "#### Entries without tags\n");
        let _ = writeln!(w, "{}\n", compact_ids(&untagged));
    }
    out.trim_end().to_owned()
}

#[test]
fn compact_ids_collapses_consecutive_runs_only() {
    let ids = [
        "c-001-s", "c-002-i", "c-003-i", "c-004-i", "c-070-e", "a-1", "a-2", "b",
    ];
    assert_eq!(
        compact_ids(&ids),
        "`c-001-s`, `c-{002..004}-i`, `c-070-e`, `a-1`, `a-2`, `b`"
    );
    assert_eq!(natural_cmp("msg:2", "msg:13"), Ordering::Less);
    assert_eq!(natural_cmp("build:9.1", "build:10.0"), Ordering::Less);
    assert_eq!(natural_cmp("vcp:212", "vcp:35"), Ordering::Greater);
    assert_eq!(bytes(25_575_337), "25,575,337");
    assert_eq!(bytes(999), "999");
}

#[test]
fn corpus_doc_lists_every_entry_by_tag() {
    let path = doc_path();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text.replace("\r\n", "\n"),
        Err(e) => panic!("read {}: {e}", path.display()),
    };
    let (Some(begin), Some(end)) = (text.find(BEGIN), text.find(END)) else {
        panic!(
            "{} must contain the markers {BEGIN} and {END}",
            path.display()
        );
    };
    assert!(begin < end, "{}: markers out of order", path.display());
    let current = text[begin + BEGIN.len()..end].trim();
    let expected = render();
    if current == expected {
        return;
    }
    let bless = std::env::var(BLESS_ENV).is_ok_and(|v| !v.is_empty() && v != "0");
    if bless {
        let updated = format!(
            "{}{BEGIN}\n\n{expected}\n\n{}",
            &text[..begin],
            &text[end..]
        );
        if let Err(e) = fs::write(&path, updated) {
            panic!("write {}: {e}", path.display());
        }
        eprintln!("regenerated the manifest index in {}", path.display());
        return;
    }
    let first_difference = current
        .lines()
        .zip(expected.lines())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| current.lines().count().min(expected.lines().count()));
    panic!(
        "{} is out of date with the manifests (first difference at generated line {}: {:?} vs expected {:?}).\n\
         Regenerate with: {BLESS_ENV}=1 cargo test -p recast-radar-testdata --test corpus_doc",
        path.display(),
        first_difference + 1,
        current.lines().nth(first_difference),
        expected.lines().nth(first_difference),
    );
}
