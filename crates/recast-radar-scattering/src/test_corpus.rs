//! Real scattering inputs for the unit tests: the committed PyTMatrix 0.3.3
//! lookup tables (corpus ids `tmatrix-lut-*-sband-pytmatrix-0.3.3`, their
//! exact generator configs) and the goldens `tools/scattering_golden.py`
//! writes to `testdata/golden/scattering/` from the LUT bytes, the
//! post-freeze held-out PyTMatrix report and the WRF P3 table text.

use serde_json::Value;

use crate::{OfflineLut, PsdFallSpeedAuthority, PsdFallSpeedProvenance, Sha256Digest};

/// Bytes of a committed corpus file.
pub(crate) fn corpus_bytes(id: &str) -> Vec<u8> {
    let path = recast_radar_testdata::path(id).unwrap_or_else(|error| panic!("{id}: {error}"));
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Parsed golden file `testdata/golden/scattering/<name>`.
pub(crate) fn golden(name: &str) -> Value {
    let path = recast_radar_testdata::testdata_dir()
        .join("golden")
        .join("scattering")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

pub(crate) fn as_f64(value: &Value) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("expected a number, got {value}"))
}

pub(crate) fn as_usize(value: &Value) -> usize {
    value
        .as_u64()
        .unwrap_or_else(|| panic!("expected an unsigned integer, got {value}")) as usize
}

pub(crate) fn array(value: &Value) -> &Vec<Value> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("expected an array, got {value}"))
}

pub(crate) fn f64s(value: &Value) -> Vec<f64> {
    array(value).iter().map(as_f64).collect()
}

/// One committed table: the validated LUT, its file bytes, its exact
/// generator config bytes and its golden entry.
pub(crate) struct CorpusLut {
    pub(crate) table: OfflineLut,
    pub(crate) bytes: Vec<u8>,
    pub(crate) config: Vec<u8>,
    pub(crate) golden: Value,
}

impl CorpusLut {
    /// The singleton frequency node of a conventional table.
    pub(crate) fn lut_frequency_hz(&self) -> f64 {
        let axis = self
            .table
            .header()
            .axes()
            .iter()
            .find(|axis| axis.kind() == crate::AxisKind::Frequency)
            .expect("frequency axis");
        assert_eq!(axis.coordinates().len(), 1, "singleton frequency axis");
        axis.coordinates()[0]
    }
}

fn load(table_id: &str, golden_key: &str) -> CorpusLut {
    let bytes = corpus_bytes(table_id);
    let config = corpus_bytes(&format!("{table_id}-config"));
    let table =
        OfflineLut::from_bytes(&bytes).unwrap_or_else(|error| panic!("{table_id}: {error}"));
    let golden = golden("tmatrix_luts.json")["tables"][golden_key].clone();
    assert_eq!(
        Sha256Digest::compute(&bytes),
        Sha256Digest::from_hex(golden["lut_sha256"].as_str().expect("sha256")).expect("hex"),
        "{table_id}: golden pins another LUT"
    );
    CorpusLut {
        table,
        bytes,
        config,
        golden,
    }
}

/// Conventional liquid-rain table: 16 diameters x 3 axis ratios x singleton
/// frequency x singleton elevation, Atlas fall speeds.
pub(crate) fn rain() -> CorpusLut {
    load("tmatrix-lut-rain-sband-pytmatrix-0.3.3", "rain")
}

/// Conventional dry-ice spheroid table: 29 diameters x 3 axis ratios x
/// singleton frequency x singleton elevation, Schiller-Naumann fall speeds.
pub(crate) fn dry_ice() -> CorpusLut {
    load("tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3", "dry_ice")
}

/// The fall-speed law a committed generator config declares, as a versioned
/// external provenance token: the SHA-256 of the exact `terminal_velocity`
/// object of the config (its law name and every numeric parameter).
pub(crate) fn config_fall_speed_provenance(config: &[u8]) -> PsdFallSpeedProvenance {
    let parsed: Value = serde_json::from_slice(config).expect("generator config is JSON");
    let law = serde_json::to_vec(&parsed["terminal_velocity"]).expect("terminal_velocity object");
    PsdFallSpeedProvenance::new(
        PsdFallSpeedAuthority::ExternalVersionedResearch,
        Sha256Digest::compute(&law),
    )
}
