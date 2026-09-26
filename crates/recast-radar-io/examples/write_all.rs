//! Write corpus volumes with every writer, and what each gate should read
//! as, for checking the files with independent readers
//! (`tools/writer_check.py`).
//!
//! ```text
//! cargo run --release -p recast-radar-io --example write_all -- <out_dir> <testdata id or file>...
//! ```
//!
//! An argument naming an existing file is read from disk (its file name is
//! the id); anything else is a testdata id. For each id, `<out_dir>/<id>/`
//! receives `cfradial1.nc`,
//! `cfradial1-unsigned.nc` (unsigned fields with `_Unsigned`), `fm301.nc`
//! and `odim.h5` (a writer that refuses the volume leaves a `<writer>.error`
//! text file instead); `cfradial1-perray.nc` (`RangeLayout::PerRay`) and
//! `odim-every.h5` (`OdimWriteOptions::every_quantity`) when they differ
//! from the default output; and `expect/`: `meta.json` (per sweep: ray
//! azimuths, elevations and times in seconds since 1970 in storage order,
//! gate centres, field names, whether a field is a quality field and what it
//! qualifies) and per sweep and field
//! `s<sweep>_<field>.f32` (physical values, row-major rays x gates in
//! storage order, NaN for every sentinel) and `s<sweep>_<field>.u8`
//! (0 value, 1 missing, 2 undetect, 3 range folded). A source that is itself
//! an ODIM_H5 or netCDF (CfRadial) file is copied to `source.h5` or
//! `source.nc`,
//! so the check can tell a reader's behaviour on the source from a property
//! of the written file.

use std::fs;
use std::path::Path;

use recast_radar_core::model::Gate;
use recast_radar_io::{SupportedVolumeFormat, read_supported_volume_bytes};
use recast_radar_io_cfradial::{
    Cfradial1Options, Cfradial2Options, RangeLayout, write_cfradial1, write_cfradial2,
};
use recast_radar_io_odim::{OdimWriteOptions, write_odim_h5_volume};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let out = args.next().ok_or("usage: write_all <out_dir> <id>...")?;
    for arg in args {
        let path = Path::new(&arg);
        let (id, bytes) = if path.is_file() {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| arg.clone());
            (name, fs::read(path)?)
        } else {
            (arg.clone(), recast_radar_testdata::bytes(&arg)?)
        };
        let volume = match read_supported_volume_bytes(&bytes) {
            Ok(volume) => volume,
            Err(err) => {
                eprintln!("{id}: not decoded: {err}");
                continue;
            }
        };
        let dir = Path::new(&out).join(&id);
        fs::create_dir_all(dir.join("expect"))?;
        // netCDF files (classic and netCDF-4) as `.nc`: LROSE Radx picks its
        // reader by the extension.
        match recast_radar_io::sniff_supported_volume_format(&bytes) {
            SupportedVolumeFormat::OdimH5 => fs::write(dir.join("source.h5"), &bytes)?,
            SupportedVolumeFormat::CfRadial
            | SupportedVolumeFormat::CfRadialNetcdf4
            | SupportedVolumeFormat::CfRadial2 => fs::write(dir.join("source.nc"), &bytes)?,
            _ => {}
        }
        let outputs: [(&str, Result<Vec<u8>, String>); 4] = [
            (
                "cfradial1.nc",
                write_cfradial1(&volume, &Cfradial1Options::default()).map_err(|e| e.to_string()),
            ),
            (
                "cfradial1-unsigned.nc",
                write_cfradial1(
                    &volume,
                    &Cfradial1Options::default().with_unsigned_attribute(true),
                )
                .map_err(|e| e.to_string()),
            ),
            (
                "fm301.nc",
                write_cfradial2(&volume, &Cfradial2Options::default()).map_err(|e| e.to_string()),
            ),
            (
                "odim.h5",
                write_odim_h5_volume(&volume, &OdimWriteOptions::default())
                    .map_err(|e| e.to_string()),
            ),
        ];
        let mut written: Vec<(&str, Vec<u8>)> = Vec::new();
        for (name, result) in outputs {
            match result {
                Ok(bytes) => {
                    fs::write(dir.join(name), &bytes)?;
                    written.push((name, bytes));
                }
                Err(err) => fs::write(dir.join(format!("{name}.error")), err)?,
            }
        }
        // The variants, when they differ from the default output.
        let default_of = |name: &str| {
            written
                .iter()
                .find(|(written, _)| *written == name)
                .map(|(_, bytes)| bytes.as_slice())
        };
        let variants = [
            (
                "cfradial1-perray.nc",
                "cfradial1.nc",
                write_cfradial1(
                    &volume,
                    &Cfradial1Options::default().with_range_layout(RangeLayout::PerRay),
                )
                .ok(),
            ),
            (
                "odim-every.h5",
                "odim.h5",
                write_odim_h5_volume(
                    &volume,
                    &OdimWriteOptions::default().with_every_quantity(true),
                )
                .ok(),
            ),
        ];
        for (name, default, bytes) in variants {
            if let Some(bytes) = bytes
                && default_of(default) != Some(bytes.as_slice())
            {
                fs::write(dir.join(name), bytes)?;
            }
        }
        let reference = volume.time_reference.timestamp() as f64;
        let mut sweeps = Vec::new();
        for (index, sweep) in volume.sweeps.iter().enumerate() {
            let ngates = sweep.range.ngates();
            let mut fields = Vec::new();
            for field in &sweep.fields {
                let mut values = Vec::with_capacity(sweep.nrays() * ngates * 4);
                let mut classes = Vec::with_capacity(sweep.nrays() * ngates);
                for ray in 0..sweep.nrays() {
                    for gate in 0..ngates {
                        let start = field.gates.start as usize;
                        let stride = field.gates.stride.max(1) as usize;
                        let native = gate
                            .checked_sub(start)
                            .map(|g| g / stride)
                            .filter(|g| *g < field.ngates as usize);
                        let state = native
                            .and_then(|native| field.gate(ray, native))
                            .unwrap_or(Gate::Missing);
                        let (value, class) = match state {
                            Gate::Value(v) if v.is_finite() => (v, 0u8),
                            Gate::Value(_) | Gate::Missing => (f32::NAN, 1),
                            Gate::Undetect => (f32::NAN, 2),
                            Gate::RangeFolded => (f32::NAN, 3),
                        };
                        values.extend_from_slice(&value.to_le_bytes());
                        classes.push(class);
                    }
                }
                let stem = format!("s{index}_{}", field.name.as_str());
                fs::write(dir.join("expect").join(format!("{stem}.f32")), values)?;
                fs::write(dir.join("expect").join(format!("{stem}.u8")), classes)?;
                fields.push(serde_json::json!({
                    "name": field.name.as_str(),
                    "quality": field.attrs.is_quality_field == Some(true),
                    "qualified": field
                        .attrs
                        .qualified_variables
                        .iter()
                        .map(|name| name.as_str())
                        .collect::<Vec<_>>(),
                    "stem": stem,
                    "dtype": field.data.dtype(),
                    "fill_is_undetect": matches!(
                        field.data.coding(),
                        recast_radar_core::model::Coding::U8(c) if c.fill_value.is_some() && c.fill_value == c.undetect
                    ) || matches!(
                        field.data.coding(),
                        recast_radar_core::model::Coding::U16(c) if c.fill_value.is_some() && c.fill_value == c.undetect
                    ),
                }));
            }
            sweeps.push(serde_json::json!({
                "sweep_mode": sweep.sweep_mode.as_str(),
                "fixed_angle": sweep.fixed_angle_deg,
                "azimuth": sweep.rays.azimuth_deg,
                "elevation": sweep.rays.elevation_deg,
                "time": sweep.rays.time_s.iter().map(|t| reference + t).collect::<Vec<_>>(),
                "range": (0..ngates).map(|g| sweep.range.center_m(g).unwrap_or(f64::NAN)).collect::<Vec<_>>(),
                "fields": fields,
            }));
        }
        let meta = serde_json::json!({
            "id": id,
            "source_format": format!("{:?}", volume.provenance.source_format),
            "latitude": volume.location.latitude_deg,
            "longitude": volume.location.longitude_deg,
            "altitude": volume.location.altitude_m,
            "sweeps": sweeps,
        });
        fs::write(
            dir.join("expect").join("meta.json"),
            serde_json::to_vec_pretty(&meta)?,
        )?;
        eprintln!("{id}: written");
    }
    Ok(())
}
