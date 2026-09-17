//! Tests read real radar files (spec section 5, plan task C.1).
//!
//! Scans workspace test code and data files for synthetic radar inputs (see
//! `recast_radar_testdata::synthetic` for the rules) and fails on any finding
//! that `testdata/synthetic-allowlist.toml` does not list, and on allowlist
//! entries that no longer match a finding. `docs/testdata/synthetic-inventory.md`
//! gives the real replacement for each entry.
//!
//! Converting a test: replace its synthetic input with a corpus file
//! (`recast_radar_testdata::require_file!`), then delete its allowlist entry
//! (and the entries of helpers it no longer needs). To list every current
//! finding as allowlist tables, run
//!
//! ```text
//! RECAST_RADAR_SYNTHETIC_REPORT=findings.toml cargo test -p recast-radar-testdata --test no_synthetic
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use recast_radar_testdata::synthetic::{
    ALLOWLIST_FILE_NAME, AllowEntry, Finding, allowlist_snippet, group_for_path, parse_allowlist,
    scan_workspace,
};
use recast_radar_testdata::{manifest, testdata_dir, workspace_root};

const REPORT_ENV: &str = "RECAST_RADAR_SYNTHETIC_REPORT";

fn findings() -> Vec<Finding> {
    let known: BTreeSet<String> = manifest()
        .files
        .iter()
        .map(|entry| entry.sha256.to_ascii_lowercase())
        .collect();
    match scan_workspace(workspace_root(), &known) {
        Ok(findings) => findings,
        Err(e) => panic!("scanning the workspace: {e}"),
    }
}

fn allowlist() -> Vec<AllowEntry> {
    let path = testdata_dir().join(ALLOWLIST_FILE_NAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => panic!("read {}: {e}", path.display()),
    };
    match parse_allowlist(&text) {
        Ok(entries) => entries,
        Err(e) => panic!("parse {}: {e}", path.display()),
    }
}

fn describe(key: &(String, Option<String>)) -> String {
    match &key.1 {
        Some(name) => format!("{} `{name}`", key.0),
        None => format!("{} (data file)", key.0),
    }
}

#[test]
fn allowlist_entries_are_well_formed() {
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in allowlist() {
        let key = entry.key();
        if !seen.insert(key.clone()) {
            problems.push(format!("duplicate entry {}", describe(&key)));
        }
        match group_for_path(&entry.path) {
            Some(group) if group == entry.group => {}
            Some(group) => problems.push(format!(
                "{} is listed under [[{}]] but belongs to [[{group}]]",
                describe(&key),
                entry.group
            )),
            None => problems.push(format!(
                "{} is not in a conversion group's crate",
                describe(&key)
            )),
        }
        if entry.justification.trim().is_empty() {
            problems.push(format!("{} has an empty justification", describe(&key)));
        }
    }
    assert!(
        problems.is_empty(),
        "testdata/{ALLOWLIST_FILE_NAME}:\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn test_inputs_are_real_or_allowlisted() {
    let findings = findings();
    if let Some(path) = std::env::var_os(REPORT_ENV).filter(|v| !v.is_empty()) {
        if let Err(e) = fs::write(&path, allowlist_snippet(&findings)) {
            panic!("write {}: {e}", std::path::Path::new(&path).display());
        }
        eprintln!(
            "wrote {} findings to {}",
            findings.len(),
            std::path::Path::new(&path).display()
        );
    }

    let allowed: BTreeMap<(String, Option<String>), AllowEntry> = allowlist()
        .into_iter()
        .map(|entry| (entry.key(), entry))
        .collect();
    let found: BTreeSet<(String, Option<String>)> = findings.iter().map(Finding::key).collect();

    let unlisted: Vec<&Finding> = findings
        .iter()
        .filter(|finding| !allowed.contains_key(&finding.key()))
        .collect();
    let stale: Vec<&AllowEntry> = allowed
        .iter()
        .filter(|(key, _)| !found.contains(*key))
        .map(|(_, entry)| entry)
        .collect();

    let mut message = String::new();
    if !unlisted.is_empty() {
        message.push_str(&format!(
            "{} test item(s) or data file(s) build synthetic radar inputs. Use real files \
             (recast_radar_testdata::require_file!, ids in testdata/**/manifest.toml; mutate real \
             bytes for corruption cases). Only a justified exception may be allowlisted:\n",
            unlisted.len()
        ));
        for finding in &unlisted {
            message.push_str(&format!("  {finding}\n"));
        }
        let owned: Vec<Finding> = unlisted.iter().map(|f| (*f).clone()).collect();
        message.push_str(&format!(
            "\nAllowlist tables for testdata/{ALLOWLIST_FILE_NAME} (fill in status and justification):\n\n{}",
            allowlist_snippet(&owned)
        ));
    }
    if !stale.is_empty() {
        message.push_str(&format!(
            "\n{} allowlist {} no longer {} a finding (converted, renamed or deleted); remove or rename in testdata/{ALLOWLIST_FILE_NAME}:\n",
            stale.len(),
            if stale.len() == 1 { "entry" } else { "entries" },
            if stale.len() == 1 { "matches" } else { "match" }
        ));
        for entry in &stale {
            message.push_str(&format!(
                "  [[{}]] {}\n",
                entry.group,
                describe(&entry.key())
            ));
        }
    }
    assert!(message.is_empty(), "{message}");
}
