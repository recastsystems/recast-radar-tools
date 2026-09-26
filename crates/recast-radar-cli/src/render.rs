//! `render`: one field of one sweep to PNG.
//!
//! Polar sweeps go through `recast_radar_render::render_field_image` (or,
//! with `--palette`, its viewport renderer with the palette in the field's
//! color-table family). Level III raster products, which the model carries
//! as a sweep with `sweep_mode = "raster"`, are drawn cell by cell.

use std::fs;
use std::io::Write;
use std::path::Path;

use image::{ImageBuffer, Rgba, RgbaImage};
use recast_radar_core::model::{FieldName, Quantity, Sweep, Volume};
use recast_radar_render::{
    self as render, ColorTable, ColorTableSet, RasterOptions, ViewportFieldCache,
    ViewportRasterOptions,
};

use crate::open::{self, OpenOptions};
use crate::{CliError, RenderArgs};

pub(crate) fn run(args: &RenderArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let input = open::open_path(&args.file, &OpenOptions::from_args(&args.input, false))?;
    let path = input.path.clone();
    let mut volumes = input.into_volumes();
    if volumes.is_empty() {
        return Err(CliError::Failed(format!(
            "{}: holds no sweep data to render",
            path.display()
        )));
    }
    let mut volume = volumes.swap_remove(0).volume;
    let palette = match &args.palette {
        Some(path) => Some(load_palette(path)?),
        None => None,
    };

    let targets: Vec<(usize, FieldName)> = if args.all_sweeps {
        volume
            .sweeps
            .iter()
            .enumerate()
            .filter_map(|(index, sweep)| {
                pick_field(sweep, args.field.as_deref()).map(|f| (index, f))
            })
            .collect()
    } else {
        match args.sweep {
            Some(index) => {
                let sweep = volume.sweeps.get(index).ok_or_else(|| {
                    CliError::Usage(format!(
                        "--sweep {index}: the file has {} sweep(s)",
                        volume.sweeps.len()
                    ))
                })?;
                let field = pick_field(sweep, args.field.as_deref()).ok_or_else(|| {
                    CliError::Usage(format!(
                        "sweep {index} has no field matching `{}` (it has {})",
                        args.field.as_deref().unwrap_or("reflectivity"),
                        field_list(sweep)
                    ))
                })?;
                vec![(index, field)]
            }
            None => volume
                .sweeps
                .iter()
                .enumerate()
                .find_map(|(index, sweep)| {
                    pick_field(sweep, args.field.as_deref()).map(|f| (index, f))
                })
                .into_iter()
                .collect(),
        }
    };
    if targets.is_empty() {
        return Err(CliError::Usage(format!(
            "no sweep has a field matching `{}`",
            args.field.as_deref().unwrap_or("reflectivity")
        )));
    }
    if args.all_sweeps {
        fs::create_dir_all(&args.output).map_err(|err| CliError::io(&args.output, err))?;
    }

    for (index, name) in targets {
        let name = if args.dealias {
            dealias(&mut volume, index, &name)?
        } else {
            name
        };
        let output = if args.all_sweeps {
            args.output.join(format!(
                "{}_{}_sweep{index:02}_{}.png",
                volume.attrs.instrument_name,
                volume.time_reference.format("%Y%m%d_%H%M%S"),
                name.as_str()
            ))
        } else {
            args.output.clone()
        };
        if volume.sweeps[index].sweep_mode.as_str() == "rhi" {
            eprintln!(
                "note: sweep {index} is an RHI; it is drawn as a plan view (azimuth around, range out)"
            );
        }
        let image = render_image(&volume, index, &name, args, palette.as_ref())?;
        save_png(&image, &output)?;
        writeln!(
            out,
            "wrote {} (sweep {index}, {} deg, {})",
            output.display(),
            crate::output::opt_float(Some(f64::from(volume.sweeps[index].fixed_angle_deg)), 2),
            name.as_str()
        )?;
    }
    Ok(())
}

fn field_list(sweep: &Sweep) -> String {
    let names: Vec<&str> = sweep.fields.iter().map(|f| f.name.as_str()).collect();
    if names.is_empty() {
        "no fields".to_owned()
    } else {
        names.join(" ")
    }
}

/// A quantity keyword, or `None` for a field name.
fn quantity_keyword(spec: &str) -> Option<Quantity> {
    Some(match spec.to_ascii_lowercase().as_str() {
        "reflectivity" | "ref" | "dbz" => Quantity::Reflectivity,
        "velocity" | "vel" => Quantity::RadialVelocity,
        "width" | "sw" | "spectrum_width" => Quantity::SpectrumWidth,
        "zdr" | "differential_reflectivity" => Quantity::DifferentialReflectivity,
        "rhohv" | "cc" | "rho" | "correlation" => Quantity::CorrelationCoefficient,
        "phidp" | "phi" => Quantity::DifferentialPhase,
        "kdp" => Quantity::SpecificDifferentialPhase,
        _ => return None,
    })
}

/// The field `spec` asks for, or without one the sweep's reflectivity, else
/// its first field.
fn pick_field(sweep: &Sweep, spec: Option<&str>) -> Option<FieldName> {
    match spec {
        Some(spec) => resolve_field(sweep, spec),
        None => resolve_field(sweep, "reflectivity")
            .or_else(|| sweep.fields.first().map(|field| field.name.clone())),
    }
}

/// The field of `sweep` named `spec` (case-insensitive), or the sweep's
/// preferred field of the quantity `spec` names.
pub(crate) fn resolve_field(sweep: &Sweep, spec: &str) -> Option<FieldName> {
    if let Some(field) = sweep
        .fields
        .iter()
        .find(|field| field.name.as_str().eq_ignore_ascii_case(spec))
    {
        return Some(field.name.clone());
    }
    let quantity = quantity_keyword(spec)?;
    sweep.find(quantity).map(|field| field.name.clone())
}

fn dealias(volume: &mut Volume, index: usize, name: &FieldName) -> Result<FieldName, CliError> {
    let dealiased = render::dealiased_velocity_field(volume, index, name)
        .map_err(|err| CliError::Failed(err.to_string()))?;
    let dealiased_name = dealiased.name.clone();
    let sweep = &mut volume.sweeps[index];
    sweep.fields.retain(|field| field.name != dealiased_name);
    sweep
        .add_field(dealiased)
        .map_err(|err| CliError::Failed(err.to_string()))?;
    sweep
        .seal()
        .map_err(|err| CliError::Failed(err.to_string()))?;
    Ok(dealiased_name)
}

fn load_palette(path: &Path) -> Result<ColorTable, CliError> {
    let text = fs::read_to_string(path).map_err(|err| CliError::io(path, err))?;
    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "palette".to_owned());
    ColorTable::parse_gr_pal(name, &text).map_err(|err| CliError::Decode {
        path: path.to_path_buf(),
        message: format!("not a GR2Analyst color table: {err}"),
    })
}

fn render_image(
    volume: &Volume,
    index: usize,
    name: &FieldName,
    args: &RenderArgs,
    palette: Option<&ColorTable>,
) -> Result<RgbaImage, CliError> {
    let sweep = &volume.sweeps[index];
    if sweep.sweep_mode.as_str() == "raster" {
        return render_raster(sweep, name, args.size, palette);
    }
    let failed = |err: render::RenderError| CliError::Failed(err.to_string());
    let Some(palette) = palette else {
        let options = RasterOptions {
            width: args.size,
            height: args.size,
            range_fraction: args.range_fraction,
        };
        return render::render_field_image(volume, index, name, options).map_err(failed);
    };

    let field = sweep
        .field(name)
        .ok_or_else(|| CliError::Failed(format!("sweep {index} has no field {}", name.as_str())))?;
    let mut tables = ColorTableSet::default();
    tables.set_family(render::color_family_for_field(field), palette.clone());
    let (first_center_m, spacing_m) = field.native_geometry(&sweep.range).ok_or_else(|| {
        CliError::Failed(format!(
            "field {} of sweep {index} has no gate geometry",
            name.as_str()
        ))
    })?;
    let max_range_km =
        ((first_center_m + spacing_m * f64::from(field.ngates)) / 1000.0).max(0.001) as f32;
    let center = (args.size as f32 - 1.0) / 2.0;
    let radius_px = center * f32::from(args.range_fraction) / 100.0;
    let km_per_px = max_range_km / radius_px.max(1.0);
    let options = ViewportRasterOptions {
        width: args.size,
        height: args.size,
        radar_x_px: center,
        radar_y_px: center,
        km_per_px_x: km_per_px,
        km_per_px_y: km_per_px,
        rotation_rad: 0.0,
    };
    let cache =
        ViewportFieldCache::new_with_color_tables(volume, index, name, &tables).map_err(failed)?;
    let mut pixels = vec![0u8; args.size as usize * args.size as usize * 4];
    let (width, height) = cache
        .render_field_rgba_into(volume, options, &mut pixels)
        .map_err(failed)?;
    ImageBuffer::from_raw(width, height, pixels)
        .ok_or_else(|| CliError::Failed("rendered buffer has the wrong size".to_owned()))
}

/// Draw a raster product: rows from north to south, columns from west to
/// east, scaled to `size` pixels on the longer side.
fn render_raster(
    sweep: &Sweep,
    name: &FieldName,
    size: u32,
    palette: Option<&ColorTable>,
) -> Result<RgbaImage, CliError> {
    let field = sweep
        .field(name)
        .ok_or_else(|| CliError::Failed(format!("no field {}", name.as_str())))?;
    let (rows, columns) = field.shape();
    if rows == 0 || columns == 0 {
        return Err(CliError::Failed(format!(
            "raster field {} is empty",
            name.as_str()
        )));
    }
    let tables = ColorTableSet::default();
    let table = match palette {
        Some(palette) => palette,
        None => tables.for_family(render::color_family_for_field(field)),
    };
    let scale = f64::from(size) / rows.max(columns) as f64;
    let width = ((columns as f64 * scale).round() as u32).max(1);
    let height = ((rows as f64 * scale).round() as u32).max(1);
    let mut image = RgbaImage::new(width, height);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let row = ((f64::from(y) / scale) as usize).min(rows - 1);
        let column = ((f64::from(x) / scale) as usize).min(columns - 1);
        if let Some(value) = field.value(row, column) {
            *pixel = Rgba(table.color_for_value(value));
        }
    }
    Ok(image)
}

fn save_png(image: &RgbaImage, path: &Path) -> Result<(), CliError> {
    let mut bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .map_err(|err| CliError::Failed(format!("PNG encoding failed: {err}")))?;
    crate::output::write_file_atomically(path, &bytes)
}
