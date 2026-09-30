//! Every writer on real volumes of every source format: the file each
//! writer produces reads back (through the router, by content) with the
//! same rays and, gate by gate, the same values, missing gates and
//! undetect gates as the source; and a file of a writer's own format read
//! and written again reads back as the same volume. Independent readers
//! check the same files in `tools/writer_check.py`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::compare::{
    Expect, Tally, assert_matches, azimuth_order, by_name, cf1_order, cfradial1_expect,
    cfradial2_expect, odim_expect,
};
use common::diff::{assert_same_volume, volume_difference};
use recast_radar_core::model::{RangeCoord, Sweep, SweepMode, Volume};
use recast_radar_io::read_supported_volume_bytes;
use recast_radar_io_cfradial::{
    CfWriteError, Cfradial1Options, Cfradial2Options, RangeLayout, write_cfradial1, write_cfradial2,
};
use recast_radar_io_odim::{OdimWriteError, OdimWriteOptions, write_odim_h5_volume};

fn source(id: &str) -> Volume {
    let bytes = recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{id}: {err}"));
    read_supported_volume_bytes(&bytes).unwrap_or_else(|err| panic!("{id}: {err}"))
}

/// Like `source`, but `None` for a source that is not redistributed and not
/// in the testdata cache (the COW2 sweep), which the loops skip.
fn source_if_available(id: &str) -> Option<Volume> {
    let bytes = recast_radar_testdata::bytes_if_available(id)?;
    Some(read_supported_volume_bytes(&bytes).unwrap_or_else(|err| panic!("{id}: {err}")))
}

/// Sources of every format (committed fixtures, and the COW2 sweep, which is
/// not redistributed).
const SOURCES: &[&str] = &[
    // NEXRAD Level II: super-resolution dual-pol (2024), Message 1 (2005),
    // a 2020 volume with split cuts, Guam (2023).
    "l2-ktlx-20240315-000217-trim",
    "l2-klix-20050829-130035-trim",
    "l2-kdvn-20200810-180401-trim",
    "l2-pgua-20230524-030945-trim",
    // Message 1 (1999) whose ARCHIVE2 header has a NUL ICAO: no name.
    "l2-ktlx-19990504-002218-trim",
    // LROSE Radx's CfRadial 1 of an FMI volume: sweeps of 500 m and of
    // 250 m gates, every first gate centred at 0 m, in range(time, range).
    "cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry",
    // CfRadial 1 (classic and netCDF-4) and CfRadial 2 (Radx, xradar).
    "cfrad1-xsapr-sgp-20110520-ppi-classic",
    "cfrad1-xsapr-sgp-20110520-ppi-netcdf4",
    "cfrad1-irene-sr2-20110827-120420-sur-sweeps01",
    "cfrad2-radx-irene-sr2-20110827-120420-sur-r30km",
    "cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32",
    "cfrad2-xradar-xsapr-sgp-20110520-ppi",
    // DORADE.
    "dorade-noxp-20090501-190244-ppi",
    "dorade-cow2-20260521-225514-sur-head24",
    // ODIM_H5.
    "odim-dkrom-20260820-1130-pvol",
    "odim-fianj-20260924-2130-pvol-dataset1-trim",
    "odim-seang-20260924-2130-qcvol-dataset1-trim",
    // Datasets out of acquisition order (RMI), ODIM_H5 v2.4 (AEMET).
    "odim-bejab-20190606-0000-pvol",
    "odim-espdg-20260707-1927-pvol-dbzh-vradh",
];

/// RHI sources, which a PVOL cannot hold (the CfRadial writers can).
const RHI_SOURCES: &[&str] = &[
    "cfrad1-dow8-20211011-223602-rhi-trim3-classic",
    "cfrad2-xradar-dow8-20211011-223602-rhi-r300",
];

/// CfRadial 1 files: written again, they read back unchanged.
const CFRADIAL1_SOURCES: &[&str] = &[
    "cfrad1-xsapr-sgp-20110520-ppi-classic",
    "cfrad1-irene-sr2-20110827-120420-sur-sweeps01",
    "cfrad1-dow8-20211011-223602-rhi-trim3-classic",
];

fn odim_rows(sweep: &Sweep) -> Vec<usize> {
    azimuth_order(sweep)
}

#[test]
fn odim_writer_keeps_every_gate_of_every_source_format() {
    let mut total = Tally::default();
    for id in SOURCES {
        let Some(volume) = source_if_available(id) else {
            continue;
        };
        let written = write_odim_h5_volume(&volume, &OdimWriteOptions::default())
            .unwrap_or_else(|err| panic!("{id}: {err}"));
        let read =
            read_supported_volume_bytes(&written).unwrap_or_else(|err| panic!("{id}: {err}"));
        // ODIM stores the start of the first bin in whole metres.
        let tally = assert_matches(
            &volume,
            &read,
            &odim_expect(format!("{id} -> ODIM"), &volume),
        );
        eprintln!("{id} -> ODIM: {tally:?}");
        total.add(&tally);
    }
    assert!(total.values > 1_000_000 && total.undetect > 0, "{total:?}");
}

/// A NEXRAD split cut: the surveillance sweep holds the dual-polarization
/// moments, the Doppler sweep DBZH, VRADH and WRADH. Each quantity has
/// one `dataM` in every dataset (Py-ART reads `dataset1`'s plane names in
/// every dataset; numbering each dataset from 1 handed it VRADH as ZDR), so
/// the Doppler dataset skips numbers. With `every_quantity` every dataset
/// holds every quantity, the missing ones all `nodata`. Either way every
/// gate reads back.
#[test]
fn odim_writer_numbers_each_quantity_once_per_volume() {
    use recast_radar_core::model::Gate;
    use recast_radar_io_odim::hdf5::H5File;

    let id = "l2-ktlx-20240315-000217-trim";
    let volume = source(id);
    for every in [false, true] {
        let options = OdimWriteOptions::default().with_every_quantity(every);
        let written = write_odim_h5_volume(&volume, &options).unwrap();
        let file = H5File::open(&written).unwrap();
        let mut numbers = std::collections::BTreeMap::new();
        let mut planes_per_dataset = Vec::new();
        for dataset in file
            .child_names("/")
            .into_iter()
            .filter(|name| name.starts_with("dataset"))
        {
            let planes: Vec<String> = file
                .child_names(&format!("/{dataset}"))
                .into_iter()
                .filter(|name| name.starts_with("data"))
                .collect();
            planes_per_dataset.push(planes.len());
            for plane in planes {
                let quantity = file
                    .attr(&format!("/{dataset}/{plane}/what"), "quantity")
                    .and_then(|attr| attr.as_str())
                    .unwrap();
                if let Some(before) = numbers.insert(quantity.clone(), plane.clone()) {
                    assert_eq!(
                        before, plane,
                        "{id} (every {every}): {quantity} numbered twice"
                    );
                }
            }
        }
        assert_eq!(planes_per_dataset.len(), 2, "{id}");
        if every {
            assert!(
                planes_per_dataset
                    .iter()
                    .all(|count| *count == numbers.len()),
                "{id}: {planes_per_dataset:?} planes, {} quantities",
                numbers.len()
            );
        } else {
            assert_ne!(planes_per_dataset[0], planes_per_dataset[1], "{id}");
        }
        let read = read_supported_volume_bytes(&written).unwrap();
        assert_matches(
            &volume,
            &read,
            &Expect {
                what: format!("{id} -> ODIM (every quantity {every})"),
                row_order: odim_rows,
                volume_order: None,
                field: by_name,
                range_tolerance_m: 0.51,
                folded_is_missing: true,
                ray_step: 1,
            },
        );
        // The planes of quantities a sweep lacks read back as fields whose
        // every gate is missing.
        for (have, sweep) in volume.sweeps.iter().zip(&read.sweeps) {
            for field in &sweep.fields {
                if have.fields.iter().any(|f| f.name == field.name) {
                    continue;
                }
                assert!(every, "{id}: {} not in the source sweep", field.name);
                assert!(
                    (0..sweep.nrays()).all(|row| (0..field.ngates as usize)
                        .all(|gate| matches!(field.gate(row, gate), Some(Gate::Missing) | None))),
                    "{id}: {} holds data",
                    field.name
                );
            }
        }
    }
}

/// A NEXRAD Message 1 volume (no radar parameters, no location) still gets
/// a root `how` group, by which LROSE Radx recognises ODIM_H5, and a NaN
/// location, which reads back as no location.
#[test]
fn odim_writer_writes_a_root_how_group_and_nan_location() {
    use recast_radar_io_odim::hdf5::H5File;

    let volume = source("l2-klix-20050829-130035-trim");
    assert_eq!(volume.location.latitude_deg, None);
    let written = write_odim_h5_volume(&volume, &OdimWriteOptions::default()).unwrap();
    let file = H5File::open(&written).unwrap();
    assert!(file.has_object("/how"));
    let read = read_supported_volume_bytes(&written).unwrap();
    assert_eq!(read.location.latitude_deg, None);
}

#[test]
fn odim_writer_refuses_rhi_sweeps() {
    for id in RHI_SOURCES {
        let volume = source(id);
        assert!(volume.sweeps.iter().any(|s| s.sweep_mode == SweepMode::Rhi));
        match write_odim_h5_volume(&volume, &OdimWriteOptions::default()) {
            Err(OdimWriteError::Unrepresentable { what }) => {
                assert!(what.contains("rhi"), "{what}")
            }
            other => panic!("{id}: {other:?}"),
        }
    }
}

#[test]
fn cfradial1_writer_keeps_every_gate_of_every_source_format() {
    let mut total = Tally::default();
    let (mut ragged, mut reordered) = (0, 0);
    for id in SOURCES.iter().chain(RHI_SOURCES) {
        let Some(volume) = source_if_available(id) else {
            continue;
        };
        let written = write_cfradial1(&volume, &Cfradial1Options::default())
            .unwrap_or_else(|err| panic!("{id}: {err}"));
        assert_eq!(&written[..4], b"CDF\x02", "{id}");
        let read =
            read_supported_volume_bytes(&written).unwrap_or_else(|err| panic!("{id}: {err}"));
        // Gate centres pass through float32.
        let tally = assert_matches(
            &volume,
            &read,
            &cfradial1_expect(format!("{id} -> CfRadial 1"), RangeLayout::Auto),
        );
        // In `n_points` storage every sweep's rays are in time order (xradar
        // lays those rows out by ray time).
        if written_attr(&read, "n_gates_vary").as_deref() == Some("true") {
            for (index, sweep) in read.sweeps.iter().enumerate() {
                let times = &sweep.rays.time_s;
                assert!(
                    times
                        .windows(2)
                        .all(|pair| pair[1].partial_cmp(&pair[0]) != Some(std::cmp::Ordering::Less)),
                    "{id}: sweep {index} of n_points storage not in time order"
                );
            }
            ragged += 1;
            reordered += usize::from(
                volume
                    .sweeps
                    .iter()
                    .any(|sweep| sweep.rays.time_s.windows(2).any(|pair| pair[1] < pair[0])),
            );
        }
        eprintln!("{id} -> CfRadial 1: {tally:?}");
        total.add(&tally);
    }
    assert!(total.values > 1_000_000 && total.undetect > 0, "{total:?}");
    eprintln!("{ragged} volumes in n_points storage, {reordered} with rays out of time order");
    assert!(
        ragged > 0 && reordered > 0,
        "{ragged} ragged, {reordered} reordered"
    );
}

/// NEXRAD `u8`/`u16` fields in CfRadial 1: widened to `short`/`int` by
/// default (every reader, LROSE Radx included, reads them), or stored with
/// `_Unsigned = "true"` on request; either way every gate reads back.
#[test]
fn cfradial1_unsigned_fields_widen_or_keep_their_width() {
    let id = "l2-ktlx-20240315-000217-trim";
    let volume = source(id);
    let dtype = |volume: &Volume, name: &str| {
        volume.sweeps[0]
            .fields
            .iter()
            .find(|field| field.name.as_str() == name)
            .map(|field| field.data.dtype())
    };
    assert_eq!(dtype(&volume, "DBZH"), Some("uint8"));
    for (unsigned_attribute, expected) in [(false, "int16"), (true, "uint8")] {
        let options = Cfradial1Options::default().with_unsigned_attribute(unsigned_attribute);
        let written = write_cfradial1(&volume, &options).unwrap();
        let read = read_supported_volume_bytes(&written).unwrap();
        assert_eq!(dtype(&read, "DBZH"), Some(expected), "{unsigned_attribute}");
        assert_matches(
            &volume,
            &read,
            &cfradial1_expect(
                format!("{id} -> CfRadial 1 (_Unsigned {unsigned_attribute})"),
                RangeLayout::Auto,
            ),
        );
    }
}

/// The sweeps' geometries as read back, in the source's sweep order.
fn ranges_in_source_order(source: &Volume, read: &Volume) -> Vec<RangeCoord> {
    let mut ranges = vec![
        RangeCoord::Explicit {
            centers_m: Vec::new()
        };
        read.sweeps.len()
    ];
    // The sweep order does not depend on the layout.
    match cf1_order(source) {
        Some((order, _)) => {
            for (position, index) in order.iter().enumerate() {
                ranges[*index] = read.sweeps[position].range.clone();
            }
        }
        None => {
            for (index, sweep) in read.sweeps.iter().enumerate() {
                ranges[index] = sweep.range.clone();
            }
        }
    }
    ranges
}

fn written_attr(volume: &Volume, name: &str) -> Option<String> {
    volume
        .attrs
        .other
        .iter()
        .find(|(key, _)| &**key == name)
        .and_then(|(_, value)| value.as_text().map(str::to_owned))
}

/// Three sweeps of FMI Anjalankoski as LROSE Radx writes them (it reads
/// ODIM's `rstart`, the first bin's start, as the first gate centre): 500 m
/// and 250 m gates, every first gate centred at 0 m: no one `range(range)` holds them (the
/// 250 m gates straddle the 500 m grid's edges). The default layout keeps
/// each sweep's own geometry in `range(sweep, range)` with `n_gates_vary`
/// storage, `PerRay` states it per ray; both read back exactly, sweep by
/// sweep. `Common` refuses, naming the way out.
#[test]
fn cfradial1_writer_keeps_each_sweeps_gate_geometry() {
    let id = "cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry";
    let volume = source(id);
    let spacings: Vec<f64> = volume
        .sweeps
        .iter()
        .map(|sweep| match sweep.range {
            RangeCoord::Uniform { spacing_m, .. } => spacing_m,
            RangeCoord::Explicit { .. } => f64::NAN,
        })
        .collect();
    assert_eq!(spacings, [500.0, 500.0, 250.0], "{id}");
    for layout in [
        RangeLayout::Auto,
        RangeLayout::PerSweep,
        RangeLayout::PerRay,
    ] {
        let options = Cfradial1Options::default().with_range_layout(layout);
        let written =
            write_cfradial1(&volume, &options).unwrap_or_else(|err| panic!("{layout:?}: {err}"));
        let read = read_supported_volume_bytes(&written).unwrap();
        assert_eq!(
            written_attr(&read, "n_gates_vary").as_deref(),
            Some("true"),
            "{layout:?}"
        );
        let source_ranges: Vec<RangeCoord> = volume
            .sweeps
            .iter()
            .map(|sweep| sweep.range.clone())
            .collect();
        assert_eq!(
            ranges_in_source_order(&volume, &read),
            source_ranges,
            "{layout:?}: each sweep's geometry"
        );
        assert_matches(
            &volume,
            &read,
            &cfradial1_expect(format!("{id} -> CfRadial 1 ({layout:?})"), layout),
        );
    }
    match write_cfradial1(
        &volume,
        &Cfradial1Options::default().with_range_layout(RangeLayout::Common),
    ) {
        Err(CfWriteError::Unrepresentable(what)) => {
            assert!(what.contains("RangeLayout::PerSweep"), "{what}");
        }
        other => panic!(
            "{id}: Common layout gave {:?}",
            other.map(|bytes| bytes.len())
        ),
    }
}

/// NEXRAD Message 1 (KLIX 2005): the surveillance sweep has 1 km gates, the
/// Doppler sweep 250 m gates on the same grid. The default layout repeats
/// the 1 km gates onto one `range(range)` (every reader reads it);
/// `PerSweep` keeps the 1 km sweep's own geometry.
#[test]
fn cfradial1_writer_repeats_or_keeps_coarser_sweeps() {
    let id = "l2-klix-20050829-130035-trim";
    let volume = source(id);
    let source_ranges: Vec<RangeCoord> = volume
        .sweeps
        .iter()
        .map(|sweep| sweep.range.clone())
        .collect();
    let spacing = |range: &RangeCoord| match range {
        RangeCoord::Uniform { spacing_m, .. } => *spacing_m,
        RangeCoord::Explicit { .. } => f64::NAN,
    };
    assert!(
        source_ranges.iter().any(|range| spacing(range) == 1000.0)
            && source_ranges.iter().any(|range| spacing(range) == 250.0),
        "{id}: {source_ranges:?}"
    );
    let auto = read_supported_volume_bytes(
        &write_cfradial1(&volume, &Cfradial1Options::default()).unwrap(),
    )
    .unwrap();
    assert!(
        ranges_in_source_order(&volume, &auto)
            .iter()
            .all(|range| spacing(range) == 250.0),
        "{id}: default layout"
    );
    let per_sweep = read_supported_volume_bytes(
        &write_cfradial1(
            &volume,
            &Cfradial1Options::default().with_range_layout(RangeLayout::PerSweep),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        ranges_in_source_order(&volume, &per_sweep),
        source_ranges,
        "{id}: PerSweep"
    );
}

/// CfRadial 1.4 requires `instrument_name` and LROSE Radx refuses a file
/// without it ("Cannot find instrument_name attribute"). A Level II volume
/// whose ARCHIVE2 header has a NUL ICAO has no name: every layout writes
/// the attribute empty (RadxConvert then reads the file, as "unknown").
#[test]
fn cfradial1_writer_writes_instrument_name_for_a_volume_without_one() {
    use recast_radar_io_cfradial::netcdf3::Nc3File;

    let id = "l2-ktlx-19990504-002218-trim";
    let volume = source(id);
    assert_eq!(volume.attrs.instrument_name, "", "{id}");
    for layout in [
        RangeLayout::Auto,
        RangeLayout::PerSweep,
        RangeLayout::PerRay,
    ] {
        let written = write_cfradial1(
            &volume,
            &Cfradial1Options::default().with_range_layout(layout),
        )
        .unwrap();
        let file = Nc3File::open(&written).unwrap();
        assert_eq!(file.gattr_str("instrument_name"), Some(""), "{layout:?}");
    }
}

#[test]
fn cfradial1_files_read_back_unchanged() {
    for id in CFRADIAL1_SOURCES {
        let first = source(id);
        let written = write_cfradial1(&first, &Cfradial1Options::default())
            .unwrap_or_else(|err| panic!("{id}: {err}"));
        let second =
            read_supported_volume_bytes(&written).unwrap_or_else(|err| panic!("{id}: {err}"));
        assert_same_volume(&first, &second, &format!("{id} -> CfRadial 1"));
    }
}

#[test]
fn cfradial2_writer_keeps_every_gate_of_every_source_format() {
    let mut total = Tally::default();
    for id in SOURCES.iter().chain(RHI_SOURCES) {
        let Some(volume) = source_if_available(id) else {
            continue;
        };
        let written = write_cfradial2(&volume, &Cfradial2Options::default())
            .unwrap_or_else(|err| panic!("{id}: {err}"));
        let read =
            read_supported_volume_bytes(&written).unwrap_or_else(|err| panic!("{id}: {err}"));
        let tally = assert_matches(
            &volume,
            &read,
            &cfradial2_expect(format!("{id} -> CfRadial 2")),
        );
        eprintln!("{id} -> CfRadial 2: {tally:?}");
        total.add(&tally);
    }
    assert!(total.values > 1_000_000 && total.undetect > 0, "{total:?}");
}

/// Each writer's own output is a fixed point: a volume read from a file the
/// writer wrote, written and read again, is the same volume (ray times to a
/// microsecond where they pass through float64 seconds).
#[test]
fn every_writer_output_reads_back_unchanged_when_written_again() {
    type Write = fn(&Volume) -> Result<Vec<u8>, String>;
    let writers: [(&str, Write); 3] = [
        ("CfRadial 1", |v| {
            write_cfradial1(v, &Cfradial1Options::default()).map_err(|e| e.to_string())
        }),
        ("CfRadial 2", |v| {
            write_cfradial2(v, &Cfradial2Options::default()).map_err(|e| e.to_string())
        }),
        ("ODIM_H5", |v| {
            write_odim_h5_volume(v, &OdimWriteOptions::default()).map_err(|e| e.to_string())
        }),
    ];
    let mut failures = Vec::new();
    for id in SOURCES.iter().chain(RHI_SOURCES) {
        let Some(volume) = source_if_available(id) else {
            continue;
        };
        for (format, write) in writers {
            // A writer that refuses the source (ODIM and RHIs) has no output.
            let Ok(first) = write(&volume) else {
                continue;
            };
            let first = read_supported_volume_bytes(&first).unwrap();
            let second = match write(&first) {
                Ok(bytes) => read_supported_volume_bytes(&bytes).unwrap(),
                Err(err) => {
                    failures.push(format!("{id} -> {format}: written again: {err}"));
                    continue;
                }
            };
            if let Some(difference) = volume_difference(&first, &second) {
                failures.push(format!("{id} -> {format}: {difference}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn writers_refuse_an_empty_volume() {
    let mut volume = source("cfrad1-xsapr-sgp-20110520-ppi-classic");
    volume.sweeps.clear();
    assert!(matches!(
        write_cfradial1(&volume, &Cfradial1Options::default()),
        Err(CfWriteError::Unrepresentable(_))
    ));
    assert!(matches!(
        write_cfradial2(&volume, &Cfradial2Options::default()),
        Err(CfWriteError::Unrepresentable(_))
    ));
    assert!(matches!(
        write_odim_h5_volume(&volume, &OdimWriteOptions::default()),
        Err(OdimWriteError::Unrepresentable { .. })
    ));
}
