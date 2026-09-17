//! Detector tests on small workspaces written to a temporary directory. The
//! Rust sources under test are string literals, which the detector ignores.

use super::*;

struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "recast-radar-testdata-detector-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        Self { root }
    }

    fn put(&self, rel: &str, contents: &[u8]) -> PathBuf {
        let path = self.root.join(rel);
        if let Some(dir) = path.parent()
            && let Err(e) = fs::create_dir_all(dir)
        {
            panic!("create {}: {e}", dir.display());
        }
        if let Err(e) = fs::write(&path, contents) {
            panic!("write {}: {e}", path.display());
        }
        path
    }

    fn scan(&self, known: &[&str]) -> Vec<Finding> {
        let known: BTreeSet<String> = known.iter().map(|s| (*s).to_owned()).collect();
        match scan_workspace(&self.root, &known) {
            Ok(findings) => findings,
            Err(e) => panic!("scan: {e}"),
        }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn names(findings: &[Finding]) -> Vec<String> {
    findings
        .iter()
        .map(|f| match &f.name {
            Some(name) => format!("{}::{name}", f.path),
            None => f.path.clone(),
        })
        .collect()
}

fn rules_of(findings: &[Finding], name: &str) -> Vec<&'static str> {
    findings
        .iter()
        .find(|f| f.name.as_deref() == Some(name))
        .map(|f| f.rules().into_iter().map(Rule::as_str).collect())
        .unwrap_or_default()
}

#[test]
fn real_file_tests_and_mutations_are_not_flagged() {
    let ws = Workspace::new("real");
    ws.put("crates/demo/tests/data/real.bin", &[0, 1, 2, 3]);
    // A binary outside test-data directories is not a test input.
    ws.put("crates/demo/assets/palette.bin", &[0, 9, 8, 7]);
    let sha = match crate::cache::sha256_file(&ws.root.join("crates/demo/tests/data/real.bin")) {
        Ok((sha, _)) => sha,
        Err(e) => panic!("{e}"),
    };
    ws.put(
        "crates/demo/tests/real.rs",
        br#"
const FIXTURE: &[u8] = include_bytes!("data/real.bin");

fn corpus(id: &str) -> Vec<u8> {
    match recast_radar_testdata::bytes(id) { Ok(b) => b, Err(e) => panic!("{e}") }
}

#[test]
fn decodes_fixture() {
    assert_eq!(&FIXTURE[..4], b"AR2V");
    let mut bytes = FIXTURE.to_vec();
    bytes[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(decode(&bytes).is_err());
}

#[test]
fn truncated_corpus_volume_fails() {
    let mut bytes = corpus("l2-ktlx-20240315-000217");
    bytes.extend_from_slice(b"\x89HDF");
    bytes[0..4].copy_from_slice(&7u32.to_le_bytes());
    let rows = 3; let gates = 4;
    let out = vec![f32::NAN; rows * gates];
    let volume = decode(&bytes).unwrap();
    let Radial { azimuth_deg, .. } = volume.cuts[0].radials[0].clone();
    match volume.cuts[0].radials[0] { Radial { elevation_deg, .. } => {} }
    let label = "synthetic and fake words in strings do not count";
    // nor in comments: fn synthetic_archive() { RadarVolume::new(..) }
}

fn typed() -> Radial { todo!() }

#[test]
fn failover_never_fakes_an_all_clear() {
    let cut = ElevationCut::from_decoded(&FIXTURE);
    assert!(cut.is_ok());
}
"#,
    );
    ws.put(
        "crates/demo/src/lib.rs",
        br#"
pub fn library() -> RadarVolume { RadarVolume::new(site(), now()) }
pub fn build_volume_bytes() -> Vec<u8> { let mut b = Vec::new(); b.extend_from_slice(&1u32.to_be_bytes()); b }
"#,
    );
    let findings = ws.scan(&[&sha]);
    assert!(findings.is_empty(), "{:#?}", names(&findings));
}

#[test]
fn byte_builders_and_their_callers_are_flagged() {
    let ws = Workspace::new("bytes");
    ws.put(
        "crates/demo/src/lib.rs",
        br#"
pub fn decode(bytes: &[u8]) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&19_724u32.to_be_bytes());
        out
    }

    fn put_i16(block: &mut [u8], offset: usize, value: i16) {
        let bytes = value.to_le_bytes();
        block[offset..offset + 2].copy_from_slice(&bytes);
    }

    fn tar_header() -> Vec<u8> {
        let mut header = vec![0u8; 512];
        header[257..262].copy_from_slice(b"ustar");
        header
    }

    #[test]
    fn decodes_archive() {
        decode(&archive());
    }

    #[test]
    fn indirect() {
        let mut block = vec![0u8; 4];
        helper(&mut block);
    }

    fn helper(block: &mut [u8]) {
        put_i16(block, 0, 1);
    }

    #[test]
    fn sniffs_hdf5_magic() {
        assert!(looks_like_hdf5(b"\x89HDF\r\n\x1a\nrest"));
    }

    #[test]
    fn pure_integer_decoding_is_fine() {
        assert_eq!(read_int(&[0xFF, 0xFE], true, true), -2);
        assert_eq!(u32::from_be_bytes([0, 0, 0, 1]), 1);
    }
}
"#,
    );
    let findings = ws.scan(&[]);
    assert_eq!(
        names(&findings),
        vec![
            "crates/demo/src/lib.rs::tests::archive",
            "crates/demo/src/lib.rs::tests::put_i16",
            "crates/demo/src/lib.rs::tests::tar_header",
            "crates/demo/src/lib.rs::tests::decodes_archive",
            "crates/demo/src/lib.rs::tests::indirect",
            "crates/demo/src/lib.rs::tests::helper",
            "crates/demo/src/lib.rs::tests::sniffs_hdf5_magic",
        ]
    );
    assert_eq!(rules_of(&findings, "tests::archive"), vec!["byte-encoding"]);
    assert_eq!(
        rules_of(&findings, "tests::tar_header"),
        vec!["byte-encoding", "magic-literal"]
    );
    assert_eq!(
        rules_of(&findings, "tests::decodes_archive"),
        vec!["uses-synthetic"]
    );
    assert_eq!(
        rules_of(&findings, "tests::sniffs_hdf5_magic"),
        vec!["magic-literal"]
    );
    assert!(findings.iter().all(|f| f.group().is_none()));
    let decodes = findings
        .iter()
        .find(|f| f.name.as_deref() == Some("tests::decodes_archive"));
    assert!(decodes.is_some_and(|f| f.is_test));
}

#[test]
fn model_construction_names_and_fields_are_flagged() {
    let ws = Workspace::new("model");
    ws.put(
        "crates/recast-radar-filters/src/smooth.rs",
        br#"
#[cfg(test)]
mod tests {
    fn grid(rows: usize, gates: usize, data: Vec<f32>) -> MomentGrid {
        MomentGrid { moment: MomentType::Reflectivity, gate_range: range(gates), scale: 1.0, offset: 0.0,
            nodata: None, range_folded: None, radial_indices: (0..rows).collect(), storage: MomentStorage::F32(data) }
    }

    fn cut() -> ElevationCut {
        let mut cut = ElevationCut::new(0.5, None);
        cut.radials.push(Radial { azimuth_deg: 1.0, ..template() });
        cut
    }

    fn range(gates: usize) -> GateRange { GateRange { first_gate_m: 0, gate_spacing_m: 250, gate_count: gates } }

    #[test]
    fn uniform_field() {
        let g = grid(8, 8, vec![35.0; 64]);
    }

    #[test]
    fn observed_field() {
        let (rows, gates) = (4, 5);
        let mut observed = vec![-5.0f32; rows * gates];
        let folds = vec![0i32; rows * gates];
    }

    #[test]
    fn swath() {
        let mut volume = volume_for_test();
        volume.push_cut(0.5, Some(1));
    }

    fn fake_header() {}

    fn geometry_only() {
        assert_eq!(beam_height(1000.0, 0.5), 8.0);
        let other_crates_type = Field { rows: 1, gates: 2 };
    }
}
"#,
    );
    let findings = ws.scan(&[]);
    assert_eq!(
        names(&findings)
            .iter()
            .map(|n| n.rsplit("::").next().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec![
            "grid",
            "cut",
            "uniform_field",
            "observed_field",
            "swath",
            "fake_header"
        ]
    );
    assert_eq!(
        rules_of(&findings, "tests::grid"),
        vec!["model-construction"]
    );
    assert_eq!(
        rules_of(&findings, "tests::observed_field"),
        vec!["gate-field"]
    );
    assert_eq!(
        rules_of(&findings, "tests::fake_header"),
        vec!["synthetic-name"]
    );
    assert!(findings.iter().all(|f| f.group() == Some("filters-map")));
}

#[test]
fn data_files_and_their_includes_are_flagged() {
    let ws = Workspace::new("files");
    ws.put(
        "crates/recast-radar-io-odim/tests/data/pvol_synth.h5",
        &[137, 72, 68, 70, 0, 1],
    );
    ws.put(
        "crates/recast-radar-io-odim/tests/data/unknown.h5",
        &[137, 72, 68, 70, 0, 2],
    );
    ws.put(
        "crates/recast-radar-io-odim/tests/data/known.h5",
        &[137, 72, 68, 70, 0, 3],
    );
    ws.put(
        "crates/recast-radar-io-odim/tests/data/gen_odim_fixture.py",
        b"\"\"\"Generate the fixture.\"\"\"\nprint('hello')\n",
    );
    ws.put(
        "crates/recast-radar-io-odim/tests/data/ref_odim.py",
        b"\"\"\"Independent reference dump of a real file.\"\"\"\n",
    );
    ws.put(
        "crates/recast-radar-io-odim/tests/data/README.md",
        b"synthetic notes\n",
    );
    let known = match crate::cache::sha256_file(
        &ws.root
            .join("crates/recast-radar-io-odim/tests/data/known.h5"),
    ) {
        Ok((sha, _)) => sha,
        Err(e) => panic!("{e}"),
    };
    ws.put(
        "crates/recast-radar-io-odim/tests/odim.rs",
        br#"
const PVOL: &[u8] = include_bytes!("data/pvol_synth.h5");
const KNOWN: &[u8] = include_bytes!("data/known.h5");

#[test]
fn decodes_all() {
    for bytes in [PVOL, KNOWN] { decode(bytes); }
}

#[test]
fn decodes_known() {
    decode(KNOWN);
}
"#,
    );
    let findings = ws.scan(&[&known]);
    assert_eq!(
        names(&findings),
        vec![
            "crates/recast-radar-io-odim/tests/data/gen_odim_fixture.py",
            "crates/recast-radar-io-odim/tests/data/pvol_synth.h5",
            "crates/recast-radar-io-odim/tests/data/unknown.h5",
            "crates/recast-radar-io-odim/tests/odim.rs::PVOL",
            "crates/recast-radar-io-odim/tests/odim.rs::decodes_all",
        ]
    );
    let pvol_file = &findings[1];
    assert_eq!(
        pvol_file
            .rules()
            .into_iter()
            .map(Rule::as_str)
            .collect::<Vec<_>>(),
        vec!["synthetic-file-name", "unmanifested-binary"]
    );
    assert_eq!(rules_of(&findings, "PVOL"), vec!["synthetic-data-file"]);
    assert_eq!(rules_of(&findings, "decodes_all"), vec!["uses-synthetic"]);
    assert!(findings.iter().all(|f| f.group() == Some("io-formats")));
}

#[test]
fn out_of_line_test_modules_and_module_scoping() {
    let ws = Workspace::new("modules");
    ws.put(
        "crates/demo/src/algo/mod.rs",
        br#"
pub fn run() {}
#[cfg(test)]
mod tests;
mod other_tests {
    #[cfg(test)]
    mod inner {
        fn cut() -> u8 { 1 }
        #[test]
        fn uses_local_cut() { cut(); }
    }
}
"#,
    );
    ws.put(
        "crates/demo/src/algo/tests.rs",
        br#"
fn cut() -> ElevationCut { ElevationCut::new(0.5, None) }
#[test]
fn uses_cut() { run_on(cut()); }
"#,
    );
    let findings = ws.scan(&[]);
    assert_eq!(
        names(&findings),
        vec![
            "crates/demo/src/algo/tests.rs::tests::cut",
            "crates/demo/src/algo/tests.rs::tests::uses_cut",
        ]
    );
}

#[test]
fn allowlist_parsing_and_groups() {
    let text = r#"
[[io-nexrad]]
path = "crates/recast-radar-io-nexrad/src/lib.rs"
name = "tests::helper"
status = "pending"
justification = "convert"

[[io-formats]]
path = "crates/recast-radar-io-odim/tests/data/x.h5"
status = "exception"
justification = "fuzz regression input"
"#;
    let entries = match parse_allowlist(text) {
        Ok(entries) => entries,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].group, "io-formats");
    assert_eq!(entries[0].name, None);
    assert_eq!(entries[1].status, AllowStatus::Pending);
    assert!(
        parse_allowlist("[[nope]]\npath = \"x\"\nstatus = \"pending\"\njustification = \"\"\n")
            .is_err()
    );
    assert!(
        parse_allowlist(
            "[[track]]\npath = \"x\"\nstatus = \"pending\"\njustification = \"\"\nextra = 1\n"
        )
        .is_err()
    );
    assert_eq!(
        group_for_path("crates/recast-radar-io/src/lib.rs"),
        Some("io-formats")
    );
    assert_eq!(
        group_for_path("crates/recast-radar-bench/src/main.rs"),
        Some("render-bench")
    );
    assert_eq!(
        group_for_path("crates/recast-radar-bzip2/tests/real_records.rs"),
        Some("io-nexrad")
    );
    assert_eq!(
        group_for_path("crates/recast-radar-tools/tests/facade_real_files.rs"),
        Some("io-formats")
    );
    assert_eq!(group_for_path("testdata/files/x"), None);
    assert_eq!(GROUPS.len(), 8);
}
