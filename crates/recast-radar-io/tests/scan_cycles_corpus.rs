//! `scan_cycles` over every committed radar volume of the corpus: a file of
//! one volume scan (a Level II file, an ODIM polar volume, a CfRadial or
//! DORADE volume) is one scan cycle. The files known to hold more are JMA's
//! 10-minute tars (two 5-minute cycles each, `real_cycles.rs` in
//! recast-radar-core) and Hurum's 2026-06-12 14:46 velocity file, whose
//! 90 deg sweep was collected in the scan before (h5py `dataset8/what/starttime`
//! 14:38:53, the other sweeps from 14:46:30).

// A panic is how a test fails (clippy.toml).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use recast_radar_core::model::scan_cycles;
use recast_radar_io::read_supported_volume_bytes;
use recast_radar_testdata::Format;

/// Files of more than one scan cycle, and how many.
const MORE_THAN_ONE: [(&str, usize); 5] = [
    ("jma-n5-20191012-090000-rs47773", 2),
    ("jma-n6-20191012-090000-rs47773", 2),
    ("jma-n5-20260924-210000-rs47937", 2),
    ("jma-n6-20260924-210000-rs47937", 2),
    ("odim-nohur-20260612-1446-vradh", 2),
];

#[test]
fn committed_volumes_hold_one_scan_cycle_but_the_known_ones() {
    let mut checked = 0;
    let mut wrong = Vec::new();
    for entry in &recast_radar_testdata::manifest().files {
        // Fuzz regressions are mutated inputs, not scans.
        if entry.committed.is_none()
            || entry.id.starts_with("fuzz-")
            || !matches!(
                entry.format,
                Format::NexradLevel2
                    | Format::OdimH5
                    | Format::CfRadial1
                    | Format::CfRadial2
                    | Format::Dorade
                    | Format::JmaGrib2Tar
            )
        {
            continue;
        }
        let bytes = recast_radar_testdata::bytes(&entry.id).unwrap();
        // Committed malformed inputs (fuzz regressions, refusal tests) do
        // not decode; they hold no scan to count.
        let Ok(volume) = read_supported_volume_bytes(&bytes) else {
            continue;
        };
        checked += 1;
        let cycles = scan_cycles(&volume);
        let expected = MORE_THAN_ONE
            .iter()
            .find(|(id, _)| *id == entry.id)
            .map_or(1, |(_, count)| *count);
        if cycles.len() != expected {
            wrong.push(format!(
                "{}: {} cycles, expected {expected} ({:?})",
                entry.id,
                cycles.len(),
                cycles.get(1).and_then(|cycle| cycle.starts_with.as_ref())
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(checked >= 50, "only {checked} committed volumes decoded");
}
