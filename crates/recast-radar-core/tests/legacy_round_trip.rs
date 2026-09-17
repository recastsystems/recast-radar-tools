//! FM301 shim acceptance (docs/design/fm301-model.md section 13.4): every real
//! corpus volume that `recast-radar-io` decodes survives
//! legacy -> FM301 -> legacy bit for bit, with no tolerances.
//!
//! - `committed_corpus_round_trips_bit_identically` runs over every committed
//!   fixture (never touches the network).
//! - `full_corpus_round_trips_bit_identically` (ignored; run with
//!   `cargo test --release -p recast-radar-core --test legacy_round_trip -- --ignored`)
//!   adds every downloadable full volume, fetching or reusing the cache.

// This test exercises the legacy model on purpose.
#![allow(deprecated, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use recast_radar_core::RadarVolume;
use recast_radar_core::legacy::{
    LegacyConvention, LegacyConversionError, bit_identical, legacy_from_volume, volume_from_legacy,
};
use recast_radar_core::model::SourceFormat;
use recast_radar_io::decode_supported_volume_bytes;
use recast_radar_testdata::{Entry, Format, TestdataError};

/// Formats the router decodes into a volume.
fn is_volume_format(entry: &Entry) -> bool {
    matches!(
        entry.format,
        Format::NexradLevel2
            | Format::NexradLevel2Chunk
            | Format::OdimH5
            | Format::CfRadial1
            | Format::CfRadial2
            | Format::Dorade
            | Format::JmaGrib2Tar
    )
}

#[derive(Default)]
struct Outcome {
    round_tripped: Vec<String>,
    /// Volumes the conversion refused because the legacy gate geometry of a cut
    /// cannot share one FM301 range (verified independently below).
    refused_unalignable: Vec<String>,
    not_decoded: Vec<String>,
    unavailable: Vec<String>,
    failures: Vec<String>,
}

/// `true` when two grids of the cut cannot be placed on one range: spacings
/// that are not integer multiples, or start edges that are not a whole number of
/// fine gates apart (design note 6.5), under the volume's legacy convention.
fn cut_geometry_is_unalignable(volume: &RadarVolume, sweep: usize) -> bool {
    let convention = LegacyConvention::of_metadata(&volume.metadata);
    let start_is_edge = matches!(
        convention,
        LegacyConvention::Odim | LegacyConvention::CfRadial
    );
    let Some(cut) = volume.cuts.get(sweep) else {
        return false;
    };
    let edges: Vec<(f64, f64)> = cut
        .moments
        .values()
        .map(|grid| {
            let first = f64::from(grid.gate_range.first_gate_m);
            let spacing = f64::from(grid.gate_range.gate_spacing_m);
            let edge = if start_is_edge {
                first
            } else {
                first - spacing / 2.0
            };
            (edge, spacing)
        })
        .collect();
    edges.iter().any(|(_, spacing)| *spacing <= 0.0)
        || edges.iter().enumerate().any(|(i, (edge_a, spacing_a))| {
            edges[i + 1..].iter().any(|(edge_b, spacing_b)| {
                let (fine, coarse) = if spacing_a <= spacing_b {
                    (*spacing_a, *spacing_b)
                } else {
                    (*spacing_b, *spacing_a)
                };
                let ratio = coarse / fine;
                let offset = (edge_a - edge_b) / fine;
                (ratio - ratio.round()).abs() > 1e-6 || (offset - offset.round()).abs() > 1e-6
            })
        })
}

fn check_volume(id: &str, volume: RadarVolume, outcome: &mut Outcome) {
    let original = volume.clone();
    // The clone itself must be bit-identical (guards the comparator).
    if let Err(err) = bit_identical(&original, &volume) {
        outcome.failures.push(format!("{id}: clone differs: {err}"));
        return;
    }
    let convention = LegacyConvention::of_metadata(&volume.metadata);
    let (fm301, residue) = match volume_from_legacy(volume) {
        Ok(converted) => converted,
        Err(LegacyConversionError::UnalignedGates { sweep, field })
            if cut_geometry_is_unalignable(&original, sweep) =>
        {
            outcome
                .refused_unalignable
                .push(format!("{id} (sweep {sweep}, field {field})"));
            return;
        }
        Err(err) => {
            outcome
                .failures
                .push(format!("{id}: volume_from_legacy: {err}"));
            return;
        }
    };
    if fm301.provenance.source_format == SourceFormat::Unknown {
        outcome
            .failures
            .push(format!("{id}: source format not inferred from markers"));
    }
    if let Err(err) = check_model_invariants(&fm301) {
        outcome
            .failures
            .push(format!("{id}: model invariants: {err}"));
    }
    let back = match legacy_from_volume(fm301, Some(&residue), convention) {
        Ok(back) => back,
        Err(err) => {
            outcome
                .failures
                .push(format!("{id}: legacy_from_volume: {err}"));
            return;
        }
    };
    match bit_identical(&back, &original) {
        Ok(()) => outcome.round_tripped.push(id.to_owned()),
        Err(err) => outcome.failures.push(format!("{id}: {err}")),
    }
}

/// A converted volume must already satisfy the sealed-sweep invariants: sealing
/// it again changes nothing structural.
fn check_model_invariants(volume: &recast_radar_core::Volume) -> Result<(), String> {
    let mut sealed = volume.clone();
    sealed.seal().map_err(|err| err.to_string())?;
    for (before, after) in volume.sweeps.iter().zip(&sealed.sweeps) {
        if before.range != after.range {
            return Err(format!(
                "sweep {}: seal changed the range",
                before.sweep_number
            ));
        }
        for (a, b) in before.fields.iter().zip(&after.fields) {
            if (a.nrays, a.ngates, a.gates, &a.absent_rows, a.data.len())
                != (b.nrays, b.ngates, b.gates, &b.absent_rows, b.data.len())
            {
                return Err(format!(
                    "sweep {} field {}: seal changed the field shape",
                    before.sweep_number, a.name
                ));
            }
            if a.nrays as usize != before.nrays() {
                return Err(format!("{} rows != rays", a.name));
            }
        }
    }
    Ok(())
}

fn run(entries: &[&Entry], resolve: impl Fn(&str) -> Result<PathBuf, TestdataError>) -> Outcome {
    let mut outcome = Outcome::default();
    for entry in entries {
        let path = match resolve(&entry.id) {
            Ok(path) => path,
            Err(err) if err.is_offline() => {
                outcome.unavailable.push(entry.id.clone());
                continue;
            }
            Err(err) => panic!("{}: {err}", entry.id),
        };
        let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", entry.id));
        match decode_supported_volume_bytes(&bytes) {
            Ok(volume) => check_volume(&entry.id, volume, &mut outcome),
            Err(_) => outcome.not_decoded.push(entry.id.clone()),
        }
    }
    outcome
}

fn report(outcome: &Outcome) {
    eprintln!(
        "round-tripped {} volumes: {:?}",
        outcome.round_tripped.len(),
        outcome.round_tripped
    );
    eprintln!(
        "refused (unalignable legacy gate geometry): {:?}",
        outcome.refused_unalignable
    );
    eprintln!("not decoded by the router: {:?}", outcome.not_decoded);
    eprintln!("unavailable offline: {:?}", outcome.unavailable);
    assert!(
        outcome.failures.is_empty(),
        "{} round-trip failures:\n{}",
        outcome.failures.len(),
        outcome.failures.join("\n")
    );
}

#[test]
fn committed_corpus_round_trips_bit_identically() {
    let manifest = recast_radar_testdata::manifest();
    let entries: Vec<&Entry> = manifest
        .files
        .iter()
        .filter(|entry| entry.committed.is_some() && is_volume_format(entry))
        .collect();
    let outcome = run(&entries, recast_radar_testdata::local_path);
    report(&outcome);
    // Committed volumes: 16 trimmed Level II, the start chunk, ODIM, CfRadial,
    // DORADE and JMA fixtures.
    assert!(
        outcome.round_tripped.len() >= 25,
        "only {} committed volumes decoded",
        outcome.round_tripped.len()
    );
    for format in ["l2-", "odim-", "cfrad1-", "dorade-", "jma-"] {
        assert!(
            outcome
                .round_tripped
                .iter()
                .any(|id| id.starts_with(format)),
            "no {format}* volume round-tripped"
        );
    }
}

#[test]
#[ignore = "downloads or reads cached full volumes; run in release"]
fn full_corpus_round_trips_bit_identically() {
    let manifest = recast_radar_testdata::manifest();
    let entries: Vec<&Entry> = manifest
        .files
        .iter()
        .filter(|entry| is_volume_format(entry))
        .collect();
    let outcome = run(&entries, recast_radar_testdata::path);
    report(&outcome);
}
