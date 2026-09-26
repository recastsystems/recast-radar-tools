//! Fuzz regression inputs for the Level II writer (`fuzz/`, target
//! `level2-writer`).
//!
//! `fuzz-level2-writer-nexrad-moment-nan-scale` (testdata/fuzz/manifest.toml)
//! is a minimized libFuzzer mutation of the head of KIWA 2026-09-17 whose
//! REF moment header carries a NaN scale and offset. The decoder keeps the
//! codes with that coding; the writer did not count it as a NEXRAD coding
//! (it required a finite, non-zero scale), resolved every gate to NaN and
//! wrote them all below threshold. A Level II moment's codes are its data:
//! they must come back code for code, with the scale and offset bit for bit.

mod common;

use recast_radar_core::model::{Field, FieldData, LinearTransform, RowRef};
use recast_radar_io_nexrad::messages;
use recast_radar_io_nexrad::read_volume_with_metadata;
use recast_radar_io_nexrad::write::{
    Compression, SourceMetadata, WriteOptions, write_volume_with_source,
};

use common::load;

const NAN_SCALE: &str = "fuzz-level2-writer-nexrad-moment-nan-scale";

/// Scale and offset bits of a Level II field.
fn coding_bits(field: &Field) -> Option<(u32, u32)> {
    let transform = match &field.data {
        FieldData::U8 { coding, .. } => coding.transform,
        FieldData::U16 { coding, .. } => coding.transform,
        _ => return None,
    };
    match transform {
        LinearTransform::IcdScaleOffset { scale, offset } => {
            Some((scale.to_bits(), offset.to_bits()))
        }
        _ => None,
    }
}

fn codes(field: &Field, ray: usize) -> Option<Vec<u16>> {
    match field.row(ray)? {
        RowRef::U8(values) => Some(values.iter().map(|v| u16::from(*v)).collect()),
        RowRef::U16(values) => Some(values.to_vec()),
        _ => None,
    }
}

#[test]
fn nexrad_codes_with_a_nan_scale_are_copied() {
    let Some(bytes) = load(NAN_SCALE) else {
        return;
    };
    let source = read_volume_with_metadata(&bytes).unwrap_or_else(|e| panic!("{e}"));
    let record = messages::metadata_record(&bytes).ok();
    let nan_fields: Vec<(usize, String)> = source
        .volume
        .sweeps
        .iter()
        .enumerate()
        .flat_map(|(index, sweep)| {
            sweep
                .fields
                .iter()
                .filter(|field| {
                    coding_bits(field).is_some_and(|(scale, _)| f32::from_bits(scale).is_nan())
                })
                .map(move |field| (index, field.name.to_string()))
        })
        .collect();
    assert!(!nan_fields.is_empty(), "the input has a NaN-scale moment");

    for (with_source, compression) in [
        (false, Compression::Bzip2LdmRecords),
        (true, Compression::None),
    ] {
        let mut options = WriteOptions::default();
        options.compression = compression;
        let metadata = SourceMetadata {
            metadata: Some(&source.metadata),
            metadata_record: record.as_deref(),
            ..SourceMetadata::default()
        };
        let (written, summary) = write_volume_with_source(
            &source.volume,
            if with_source {
                metadata
            } else {
                SourceMetadata::default()
            },
            &options,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let again = read_volume_with_metadata(&written)
            .unwrap_or_else(|e| panic!("{e}"))
            .volume;
        for (index, name) in &nan_fields {
            let a = source.volume.sweeps[*index]
                .fields
                .iter()
                .find(|field| field.name.as_str() == name)
                .unwrap_or_else(|| panic!("{name}"));
            let report = summary
                .moments
                .iter()
                .find(|report| report.sweep == *index && report.field.as_str() == name)
                .unwrap_or_else(|| panic!("sweep {index} {name} not written"));
            assert!(report.exact, "sweep {index} {name}: copied codes");
            let out = summary
                .skipped_sweeps
                .iter()
                .filter(|skipped| **skipped < *index)
                .count();
            let b = again.sweeps[index - out]
                .fields
                .iter()
                .find(|field| field.name.as_str() == name)
                .unwrap_or_else(|| panic!("sweep {index} {name} not read back"));
            assert_eq!(coding_bits(a), coding_bits(b), "sweep {index} {name}");
            for ray in 0..source.volume.sweeps[*index].nrays() {
                assert_eq!(
                    codes(a, ray),
                    codes(b, ray),
                    "sweep {index} {name} ray {ray}"
                );
            }
        }
    }
}
