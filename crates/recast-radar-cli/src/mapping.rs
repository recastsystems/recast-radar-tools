//! Cross sections and Cartesian grids shared by Python and CLI frontends.

use std::io::Write;
use std::path::PathBuf;

use clap::Args;
use recast_radar_core::{FieldName, Quantity, Volume};
use recast_radar_map as map;
use serde::{Deserialize, Serialize};

use crate::CliError;
use crate::open::{self, OpenOptions};

/// Section options. Distances follow the Rust mapper: horizontal endpoints
/// in kilometres east/north of the radar; heights in metres above radar altitude.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SectionOptions {
    /// Exact dataset field name.
    pub field: String,
    /// Starting east/north location, km.
    pub start_km: (f32, f32),
    /// Ending east/north location, km.
    pub end_km: (f32, f32),
    /// Horizontal samples, including both endpoints.
    pub width: usize,
    /// Vertical samples, from top to radar altitude.
    pub height: usize,
    /// Top height, metres above the radar.
    pub top_m: f32,
    /// Apply the Rust mapper's horizontal smoothing.
    pub smooth: bool,
    /// Select a native RHI sweep instead of a reconstructed PPI section.
    pub rhi_sweep: Option<usize>,
    /// Maximum range for a native RHI panel (metres).
    pub max_range_m: f32,
}

impl Default for SectionOptions {
    fn default() -> Self {
        Self {
            field: "DBZH".into(),
            start_km: (0.0, 0.0),
            end_km: (100.0, 0.0),
            width: 512,
            height: 256,
            top_m: 20000.0,
            smooth: true,
            rhi_sweep: None,
            max_range_m: 200000.0,
        }
    }
}

/// A section with units and explicit endpoint coordinates.
#[derive(Serialize)]
pub struct Section {
    /// Output width and height.
    pub shape: (usize, usize),
    /// Field name.
    pub field: String,
    /// Source field units, when recorded.
    pub units: Option<String>,
    /// Height coordinates, metres above the radar, descending.
    pub height_m: Vec<f32>,
    /// Distance along the section, metres, ascending.
    pub distance_m: Vec<f32>,
    /// Values in row-major (height, distance) order; NaN means no data.
    pub values: Vec<f32>,
}

/// Recorded units, or the model's canonical WMO units for a known field.
pub fn field_units(field: &recast_radar_core::Field) -> Option<&str> {
    field
        .attrs
        .units
        .as_deref()
        .or_else(|| field.name.info().map(|info| info.units))
}

/// Reconstruct a section or sample a native RHI using the existing mapper.
pub fn section(volume: &Volume, o: &SectionOptions) -> Result<Section, CliError> {
    if o.width < 2 || o.height < 2 || o.width.checked_mul(o.height).is_none_or(|n| n > 4_194_304) {
        return Err(CliError::Usage(
            "section dimensions must be >=2 and total at most 4194304 samples".into(),
        ));
    }
    if ![
        o.start_km.0,
        o.start_km.1,
        o.end_km.0,
        o.end_km.1,
        o.top_m,
        o.max_range_m,
    ]
    .iter()
    .all(|v| v.is_finite())
        || o.top_m <= 0.0
        || o.max_range_m <= 0.0
    {
        return Err(CliError::Usage(
            "section coordinates must be finite and heights/range positive".into(),
        ));
    }
    let name = FieldName::from(o.field.as_str());
    let source = volume
        .sweeps
        .iter()
        .find_map(|s| s.field(&name))
        .ok_or_else(|| CliError::Usage(format!("no field {}", o.field)))?;
    let units = field_units(source).map(str::to_owned);
    let result = if let Some(index) = o.rhi_sweep {
        let sweep = volume
            .sweeps
            .get(index)
            .ok_or_else(|| CliError::Usage("RHI sweep index out of range".into()))?;
        if !map::sweep_looks_like_rhi(sweep) {
            return Err(CliError::Usage("selected sweep is not an RHI".into()));
        }
        let field = sweep
            .field(&name)
            .ok_or_else(|| CliError::Usage("RHI sweep lacks requested field".into()))?;
        map::rhi_panel(sweep, field, o.width, o.height, o.top_m, o.max_range_m)
    } else {
        if o.start_km == o.end_km {
            return Err(CliError::Usage("section endpoints must differ".into()));
        }
        let policy = match source.quantity {
            Quantity::CorrelationCoefficient => map::InterpPolicy::CcGuard,
            Quantity::RadialVelocity | Quantity::DealiasedRadialVelocity => {
                map::InterpPolicy::VelocityGuard
            }
            _ => map::InterpPolicy::LinearAngle,
        };
        let smoothing = if o.smooth {
            map::CrossSectionSmoothing::Smoothed
        } else {
            map::CrossSectionSmoothing::Native
        };
        map::field_section_with_smoothing(
            volume, &name, policy, o.start_km, o.end_km, o.width, o.height, o.top_m, smoothing,
        )
    }
    .ok_or_else(|| CliError::Failed("no usable geometry for this section".into()))?;
    Ok(Section {
        shape: (result.height, result.width),
        field: o.field.clone(),
        units,
        height_m: (0..result.height)
            .map(|i| result.top_m * (1.0 - i as f32 / (result.height - 1) as f32))
            .collect(),
        distance_m: (0..result.width)
            .map(|i| result.length_m * i as f32 / (result.width - 1) as f32)
            .collect(),
        values: result.values,
    })
}

/// Cartesian gridding options. Axis order is (z, y, x); all limits are metres.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GridOptions {
    /// Exact source field names.
    pub fields: Vec<String>,
    /// Number of points along z, y, x.
    pub shape: (usize, usize, usize),
    /// Inclusive z, y, x limits in metres from the origin.
    pub limits_m: [(f64, f64); 3],
    /// Origin latitude, longitude, altitude in degrees/degrees/metres MSL.
    #[serde(default)]
    pub origin: Option<(f64, f64, f64)>,
    /// barnes2, barnes, cressman, or nearest; defaults to barnes2.
    #[serde(default = "default_weighting")]
    pub weighting: String,
    /// Fixed radius of influence, metres; omitted uses the Rust distance-beam default.
    #[serde(default)]
    pub radius_m: Option<f32>,
}

fn default_weighting() -> String {
    "barnes2".into()
}

/// Grid one or more volumes with the native mapper's geometry and weighting.
pub fn grid(volumes: &[&Volume], o: &GridOptions) -> Result<map::CartesianGrid, CliError> {
    if o.fields.is_empty()
        || o.fields
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != o.fields.len()
    {
        return Err(CliError::Usage(
            "select at least one field, without duplicates".into(),
        ));
    }
    if o.limits_m
        .iter()
        .any(|(a, b)| !a.is_finite() || !b.is_finite() || b < a)
    {
        return Err(CliError::Usage(
            "grid limits must be finite and increasing".into(),
        ));
    }
    if let Some((lat, lon, alt)) = o.origin
        && (!lat.is_finite()
            || lat.abs() > 90.0
            || !lon.is_finite()
            || lon.abs() > 360.0
            || !alt.is_finite())
    {
        return Err(CliError::Usage("invalid grid origin".into()));
    }
    if o.radius_m.is_some_and(|r| !r.is_finite() || r <= 0.0) {
        return Err(CliError::Usage(
            "radius_m must be finite and positive".into(),
        ));
    }
    if o.fields
        .iter()
        .any(|name| matches!(name.as_str(), "ROI" | "x" | "y" | "z" | "crs"))
    {
        return Err(CliError::Usage(
            "ROI, x, y, z, and crs are reserved grid output names".into(),
        ));
    }
    let fields: Vec<_> = o
        .fields
        .iter()
        .map(|name| FieldName::from(name.as_str()))
        .collect();
    for name in &fields {
        if !volumes
            .iter()
            .any(|v| v.sweeps.iter().any(|s| s.field(name).is_some()))
        {
            return Err(CliError::Usage(format!("no source has field {name}")));
        }
    }
    let spec = map::GridSpec {
        shape: o.shape,
        z_limits_m: o.limits_m[0],
        y_limits_m: o.limits_m[1],
        x_limits_m: o.limits_m[2],
        origin: o.origin.map(
            |(latitude_deg, longitude_deg, altitude_m)| map::GridOrigin {
                latitude_deg,
                longitude_deg,
                altitude_m,
            },
        ),
    };
    let weighting = match o.weighting.as_str() {
        "barnes2" => map::GridWeighting::Barnes2,
        "barnes" => map::GridWeighting::Barnes,
        "cressman" => map::GridWeighting::Cressman,
        "nearest" => map::GridWeighting::Nearest,
        _ => return Err(CliError::Usage("unknown grid weighting".into())),
    };
    let mut options = map::GridOptions {
        weighting,
        ..map::GridOptions::default()
    };
    if let Some(radius_m) = o.radius_m {
        options.roi = map::RadiusOfInfluence::Constant { radius_m };
    }
    map::grid_from_volumes(volumes, &fields, &spec, &options)
        .map_err(|e| CliError::Usage(e.to_string()))
}

/// Portable CLI grid output; NaNs serialize as null, with shape and coordinates preserved.
pub fn grid_json(grid: &map::CartesianGrid, volumes: &[&Volume]) -> serde_json::Value {
    serde_json::json!({
        "shape": grid.shape, "x_m": grid.x_m, "y_m": grid.y_m, "z_m": grid.z_m,
        "origin": [grid.origin.latitude_deg,grid.origin.longitude_deg,grid.origin.altitude_m],
        "fields": grid.fields.iter().map(|f| (f.name.as_str().to_owned(), serde_json::json!(f.values))).collect::<serde_json::Map<_,_>>(),
        "roi_m": grid.roi_m,
        "units": grid.fields.iter().map(|f| (f.name.as_str().to_owned(), serde_json::json!(volumes.iter().flat_map(|v| &v.sweeps).find_map(|s| s.field(&f.name).and_then(field_units))))).collect::<serde_json::Map<_,_>>(),
    })
}

/// CLI map operation configured with a JSON options file, using the same schema as Python.
#[derive(Debug, Args)]
pub struct MappingArgs {
    /// One file for sections, one or more for grids.
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
    /// JSON options; see docs/guide/frontend-processing.md.
    #[arg(long)]
    pub options: PathBuf,
    /// Output JSON containing shape, coordinates, and values (null for missing).
    #[arg(short, long)]
    pub output: PathBuf,
    /// Replace an existing output file.
    #[arg(long)]
    pub force: bool,
}

pub(crate) fn run(args: &MappingArgs, is_grid: bool, out: &mut dyn Write) -> Result<(), CliError> {
    let config =
        std::fs::read_to_string(&args.options).map_err(|e| CliError::io(&args.options, e))?;
    let mut volumes = Vec::new();
    for path in &args.files {
        volumes.extend(open::open_path(path, &OpenOptions::default())?.into_volumes());
    }
    let value = if is_grid {
        let options: GridOptions =
            serde_json::from_str(&config).map_err(|e| CliError::Usage(e.to_string()))?;
        let refs: Vec<_> = volumes.iter().map(|v| &v.volume).collect();
        grid_json(&grid(&refs, &options)?, &refs)
    } else {
        if volumes.len() != 1 {
            return Err(CliError::Usage(
                "section requires exactly one volume".into(),
            ));
        }
        let options: SectionOptions =
            serde_json::from_str(&config).map_err(|e| CliError::Usage(e.to_string()))?;
        serde_json::to_value(section(&volumes[0].volume, &options)?)
            .map_err(|e| CliError::Failed(e.to_string()))?
    };
    let mut file = crate::output::AtomicFile::create(&args.output, args.force)?;
    serde_json::to_writer(file.writer(), &value).map_err(|e| CliError::Failed(e.to_string()))?;
    file.commit()?;
    writeln!(out, "wrote {}", args.output.display())?;
    Ok(())
}
