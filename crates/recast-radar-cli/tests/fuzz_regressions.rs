//! Fuzz regression inputs for the command's input handling (`fuzz/`,
//! targets `level2-records` and `cli-open`).
//!
//! `fuzz-level2-records-volume-header-date-overflow`
//! (testdata/fuzz/manifest.toml) is a minimized libFuzzer mutation of the
//! committed TBWI status-only stub: its 24-byte AR2V0008 volume header has
//! the date field 0x2EFFFFFF (788,529,151 days). The record summary added
//! that many days to the Unix epoch with chrono's panicking `+`; it now
//! leaves the header time out.

use recast_radar_cli::open::{Contents, OpenOptions, open_bytes};
use recast_radar_cli::records::summarize;

const FUZZ_INPUT: &str = "fuzz-level2-records-volume-header-date-overflow";
const SEED: &str = "l2-tbwi-20230601-175101-stub";

fn testdata_bytes(id: &str) -> Vec<u8> {
    recast_radar_testdata::bytes(id).unwrap_or_else(|e| panic!("{id}: {e}"))
}

#[test]
fn an_out_of_range_header_date_is_not_a_panic() {
    let input = testdata_bytes(FUZZ_INPUT);
    assert_eq!(&input[..8], b"AR2V0008");
    assert_eq!(&input[12..16], &[0x2E, 0xFF, 0xFF, 0xFF]);
    // Summaries and the command's decoding return instead of panicking.
    if let Ok(summary) = summarize(&input) {
        assert!(summary.messages.is_empty());
    }
    for metadata in [false, true] {
        let mut options = OpenOptions::default();
        options.metadata = metadata;
        let _ = open_bytes(&input, &options);
    }
}

/// The same edit on the whole real seed: the records still summarize, with
/// no header time, where the seed itself has one.
#[test]
fn the_seed_with_the_edited_date_still_summarizes() {
    let seed = testdata_bytes(SEED);
    let original = summarize(&seed).unwrap_or_else(|e| panic!("{e}"));
    assert!(original.header_time.is_some());
    let mut edited = seed.clone();
    edited[12..16].copy_from_slice(&[0xFF; 4]);
    let summary = summarize(&edited).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(summary.header_time, None);
    assert_eq!(summary.messages, original.messages);
    assert!(matches!(
        open_bytes(&edited, &OpenOptions::default()),
        Ok(Contents::Level2Records(_)) | Ok(Contents::Volumes(_)) | Err(_)
    ));
}
