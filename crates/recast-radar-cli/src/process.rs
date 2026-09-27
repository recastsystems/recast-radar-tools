//! Shared product processing for the command line and Python bindings.
//!
//! Uses the scientific crates directly and keeps the input volume unchanged.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;

use clap::Args;
use recast_radar_core::{Field, FieldAttrs, FieldData, FieldName, FloatCoding, Quantity, Volume};
use recast_radar_correct as correct;
use recast_radar_map as map;
use recast_radar_retrieve::{self as retrieve, DerivationConfig, DerivedSweepProduct, RadarBand};
use serde::{Deserialize, Serialize};

use crate::backend::{Backends, OutputFormat, WriteInput, WriteOptions};
use crate::open::{self, Loaded, OpenOptions};
use crate::{CliError, InputArgs};

/// Caller-supplied winds at the radar site; heights are metres above radar altitude.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentProfile {
    /// RFC3339 valid time with a timezone.
    pub valid_time: String,
    /// Increasing (height above radar in m, eastward wind m/s, northward wind m/s) levels.
    pub levels: Vec<(f32, f32, f32)>,
}

/// Parameters shared by Python and the command line.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProcessOptions {
    /// Product IDs, as listed by `products`.
    pub products: Vec<String>,
    /// Band (`s`, `c`, or `x`). Unknown by default; band-dependent products report unavailable.
    pub band: Option<String>,
    /// Only these sweep indices for sweep products. Column products use the whole volume.
    pub sweeps: Option<Vec<usize>>,
    /// Replace derived fields that already exist.
    pub overwrite: bool,
    /// Reflectivity threshold for echo tops, base, and depth (dBZ).
    pub threshold_dbz: f32,
    /// Height for the low-level composite, in metres above the radar.
    pub height_m: f32,
    /// Freezing-level height above the radar (metres); required for hail products.
    pub freezing_level_m: Option<f32>,
    /// Minus-20-C-level height above the radar (metres); required for SHI/MESH/POSH.
    pub minus20c_level_m: Option<f32>,
    /// Dealiasing engine: `region`, `pyart`, or `volume`.
    pub dealias_method: String,
    /// Environmental winds for the volume dealiaser; freshness is checked by the solver.
    pub environment: Option<EnvironmentProfile>,
}

impl Default for ProcessOptions {
    fn default() -> Self {
        Self {
            products: Vec::new(),
            band: None,
            sweeps: None,
            overwrite: false,
            threshold_dbz: map::ECHO_TOP_THRESHOLD_DBZ,
            height_m: 3000.0,
            freezing_level_m: None,
            minus20c_level_m: None,
            dealias_method: "region".into(),
            environment: None,
        }
    }
}

/// One product available through both frontends.
#[derive(Clone, Debug, Serialize)]
pub struct Product {
    /// Stable argument accepted by `process`.
    pub id: String,
    /// Human-readable description.
    pub name: String,
    /// `sweep` or `column`.
    pub scope: String,
}

const EXTRA: &[(&str, &str, &str)] = &[
    ("VRADDH", "Dealiased radial velocity", "sweep"),
    ("AZ_SHEAR", "Azimuthal shear", "sweep"),
    ("RAD_DIV", "Radial divergence", "sweep"),
    ("CREF", "Composite reflectivity", "column"),
    ("ET", "Echo top", "column"),
    ("VIL", "Vertically integrated liquid", "column"),
    ("VILD", "VIL density", "column"),
    ("SHI", "Severe hail index", "column"),
    (
        "MESH",
        "Maximum estimated size of hail (Witt 1998)",
        "column",
    ),
    ("POSH", "Probability of severe hail", "column"),
    ("POH", "Probability of hail", "column"),
    ("EBASE", "Echo base", "column"),
    ("EDEPTH", "Echo depth", "column"),
    ("HMAX", "Height of maximum reflectivity", "column"),
    ("LREF", "Low-level composite reflectivity", "column"),
];

/// List the supported product IDs, including all sweep-derivation products.
pub fn products() -> Vec<Product> {
    let mut products: Vec<_> = DerivedSweepProduct::ALL
        .iter()
        .map(|p| Product {
            id: p.id().into(),
            name: p.display_name().into(),
            scope: "sweep".into(),
        })
        .collect();
    products.extend(EXTRA.iter().map(|(id, name, scope)| Product {
        id: (*id).into(),
        name: (*name).into(),
        scope: (*scope).into(),
    }));
    products
}

/// Product outcomes. Indices always refer to the original volume's sweep order.
#[derive(Debug, Default, Serialize)]
pub struct ProcessReport {
    /// `(sweep index, output field name)` pairs added or replaced.
    pub inserted: Vec<(usize, String)>,
    /// Products kept because overwrite was disabled.
    pub skipped_existing: Vec<(usize, String)>,
    /// `(sweep index, requested product ID, reason)` for unavailable inputs.
    pub unavailable: Vec<(Option<usize>, String, String)>,
    /// Volume-solver diagnostics, including whether temporal and environmental evidence was used.
    pub dealias_diagnostics: Option<serde_json::Value>,
}

fn output_name(id: &str, sweep: &recast_radar_core::Sweep) -> String {
    DerivedSweepProduct::ALL
        .iter()
        .find(|p| p.id() == id)
        .map(|p| p.field_name_in(sweep).as_str().to_owned())
        .unwrap_or_else(|| id.to_owned())
}

fn bad(message: impl Into<String>) -> CliError {
    CliError::Usage(message.into())
}

fn config(options: &ProcessOptions, nsweeps: usize) -> Result<DerivationConfig, CliError> {
    if options.products.is_empty() {
        return Err(bad(
            "select at least one product; see `recast-radar products`",
        ));
    }
    let known = products();
    for id in &options.products {
        if !known.iter().any(|p| p.id.eq_ignore_ascii_case(id)) {
            return Err(bad(format!(
                "unknown product {id:?}; see `recast-radar products`"
            )));
        }
    }
    if let Some(indices) = &options.sweeps {
        let unique: BTreeSet<_> = indices.iter().collect();
        if indices.is_empty()
            || unique.len() != indices.len()
            || indices.iter().any(|&i| i >= nsweeps)
        {
            return Err(bad(
                "sweeps must be nonempty, unique indices within the input volume",
            ));
        }
    }
    if !options.threshold_dbz.is_finite() || !options.height_m.is_finite() || options.height_m < 0.0
    {
        return Err(bad(
            "threshold_dbz must be finite; height_m must be finite and nonnegative",
        ));
    }
    for height in [options.freezing_level_m, options.minus20c_level_m]
        .into_iter()
        .flatten()
    {
        if !height.is_finite() || height < 0.0 {
            return Err(bad("hail level heights must be finite and nonnegative"));
        }
    }
    if let (Some(h0), Some(h20)) = (options.freezing_level_m, options.minus20c_level_m)
        && h20 <= h0
    {
        return Err(bad("minus20c_level_m must be above freezing_level_m"));
    }
    if !matches!(
        options.dealias_method.as_str(),
        "region" | "pyart" | "volume"
    ) {
        return Err(bad("dealias_method must be region, pyart, or volume"));
    }
    let band = match options
        .band
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None => RadarBand::Unknown,
        Some("s") => RadarBand::S,
        Some("c") => RadarBand::C,
        Some("x") => RadarBand::X,
        _ => return Err(bad("band must be s, c, or x")),
    };
    let mut cfg = DerivationConfig::with_products(
        band,
        DerivedSweepProduct::ALL.iter().copied().filter(|p| {
            options
                .products
                .iter()
                .any(|id| p.id().eq_ignore_ascii_case(id))
        }),
    );
    cfg.overwrite_existing = options.overwrite;
    Ok(cfg)
}

fn insert(
    volume: &mut Volume,
    index: usize,
    field: Field,
    options: &ProcessOptions,
    report: &mut ProcessReport,
) -> Result<(), CliError> {
    let sweep = &mut volume.sweeps[index];
    let name = field.name.as_str().to_owned();
    if sweep.fields.iter().any(|f| f.name == field.name) && !options.overwrite {
        report.skipped_existing.push((index, name));
        return Ok(());
    }
    sweep.fields.retain(|f| f.name != field.name);
    sweep
        .add_field(field)
        .map_err(|e| CliError::Failed(e.to_string()))?;
    sweep.seal().map_err(|e| CliError::Failed(e.to_string()))?;
    report.inserted.push((index, name));
    Ok(())
}

/// Compute products on a clone of `source`, preserving raw fields and metadata.
pub fn process(
    source: &Loaded,
    options: &ProcessOptions,
    previous: Option<&Loaded>,
) -> Result<(Loaded, ProcessReport), CliError> {
    let cfg = config(options, source.volume.sweeps.len())?;
    let mut loaded = source.clone();
    let volume = &mut loaded.volume;
    let mut report = ProcessReport::default();
    let wanted: BTreeSet<_> = options
        .products
        .iter()
        .map(|p| p.to_ascii_uppercase())
        .collect();
    if options.dealias_method != "volume" && (previous.is_some() || options.environment.is_some()) {
        return Err(bad(
            "previous and environment require dealias_method=volume",
        ));
    }
    let environment = options
        .environment
        .as_ref()
        .map(|profile| {
            let valid_time = chrono::DateTime::parse_from_rfc3339(&profile.valid_time)
                .map_err(|e| bad(format!("invalid environment valid_time: {e}")))?
                .with_timezone(&chrono::Utc);
            Ok::<_, CliError>(correct::EnvironmentalWindProfile {
                valid_time,
                levels: profile
                    .levels
                    .iter()
                    .map(|&(height_m_arl, u_mps, v_mps)| correct::EnvWindLevel {
                        height_m_arl,
                        u_mps,
                        v_mps,
                    })
                    .collect(),
            })
        })
        .transpose()?;
    let solution = (options.dealias_method == "volume"
        && ["VRADDH", "AZ_SHEAR", "RAD_DIV"]
            .iter()
            .any(|id| wanted.contains(*id)))
    .then(|| {
        correct::dealias_volume(
            &source.volume,
            previous.map(|p| correct::TemporalPrior::Volume(&p.volume)),
            environment.as_ref(),
        )
    });
    if let Some(solution) = &solution {
        let d = solution.diagnostics();
        report.dealias_diagnostics = Some(serde_json::json!({
            "velocity_tilts":d.velocity_tilts,"nodes":d.nodes,"graph_edges":d.graph_edges,
            "components":d.components,"enumerated_components":d.enumerated_components,
            "enumeration_beat_heuristic":d.enumeration_beat_heuristic,"energy":d.energy,
            "env_profile_used":d.env_profile_used,"temporal_prior_used":d.temporal_prior_used,
            "couplet_masked":d.couplet_masked,"speck_snapped":d.speck_snapped,"patch_changed":d.patch_changed,
            "ring_closed":d.ring_closed,"patch_reverted":d.patch_reverted,"box_moved":d.box_moved,
            "plane_moved":d.plane_moved,"repair_aborts":d.repair_aborts,
        }));
    }
    for i in 0..volume.sweeps.len() {
        if options
            .sweeps
            .as_ref()
            .is_some_and(|indices| !indices.contains(&i))
        {
            continue;
        }
        let r = retrieve::derive_sweep_in_place(&mut volume.sweeps[i], &cfg);
        report.inserted.extend(
            r.inserted
                .into_iter()
                .map(|id| (i, output_name(&id, &volume.sweeps[i]))),
        );
        report.skipped_existing.extend(
            r.skipped_existing
                .into_iter()
                .map(|id| (i, output_name(&id, &volume.sweeps[i]))),
        );
        report.unavailable.extend(
            r.unavailable
                .into_iter()
                .map(|name| (Some(i), name, "missing source fields or radar band".into())),
        );
        for id in ["VRADDH", "AZ_SHEAR", "RAD_DIV"] {
            if !wanted.contains(id) {
                continue;
            }
            let sweep = &volume.sweeps[i];
            let Some(raw) = sweep.find(Quantity::RadialVelocity) else {
                report
                    .unavailable
                    .push((Some(i), id.into(), "no radial velocity field".into()));
                continue;
            };
            if correct::dealias_skipped_no_nyquist(sweep, raw) {
                report
                    .unavailable
                    .push((Some(i), id.into(), "no valid Nyquist velocity".into()));
                continue;
            }
            let dealiased = if let Some(solution) = &solution {
                let Some(field) = solution.tilt_field(i) else {
                    report.unavailable.push((
                        Some(i),
                        id.into(),
                        "volume solver found no usable velocity geometry".into(),
                    ));
                    continue;
                };
                field.clone()
            } else if options.dealias_method == "pyart" {
                correct::dealias_velocity_pyart_region(sweep, raw)
            } else {
                correct::dealias_velocity(sweep, raw)
            };
            let field = match id {
                "AZ_SHEAR" => retrieve::azimuthal_shear_from_dealiased(sweep, &dealiased),
                "RAD_DIV" => retrieve::radial_divergence_from_dealiased(sweep, &dealiased),
                _ => dealiased,
            };
            let confidence = if id == "VRADDH" {
                solution
                    .as_ref()
                    .and_then(|sol| sol.tilt_confidence(i))
                    .map(|c| {
                        let mut quality = field.clone();
                        quality.name = FieldName::from("VRADDH_CONFIDENCE");
                        quality.quantity = Quantity::Other;
                        quality.attrs = FieldAttrs {
                            units: Some("1".into()),
                            long_name: Some(
                                "Dealiasing branch confidence: 0 no opinion, 255 decisive".into(),
                            ),
                            is_quality_field: Some(true),
                            qualified_variables: vec![FieldName::Vraddh],
                            ..FieldAttrs::default()
                        };
                        quality.data = FieldData::F32 {
                            values: c.values().iter().map(|&v| f32::from(v)).collect(),
                            coding: FloatCoding::default(),
                        };
                        quality
                    })
            } else {
                None
            };
            let name = field.name.as_str().to_owned();
            insert(volume, i, field, options, &mut report)?;
            if report
                .inserted
                .iter()
                .any(|(index, field)| *index == i && *field == name)
                && let Some(confidence) = confidence
            {
                insert(volume, i, confidence, options, &mut report)?;
            }
        }
    }
    // Column products use the unmodified source, so selection order cannot change their inputs.
    let source = &source.volume;
    for (id, _, scope) in EXTRA {
        if *scope != "column" || !wanted.contains(*id) {
            continue;
        }
        let base = if matches!(*id, "EBASE" | "EDEPTH" | "HMAX" | "LREF") {
            retrieve::reflectivity_column_base_sweep(source)
        } else {
            map::column_base_sweep(source)
        };
        let Some(base) = base else {
            report.unavailable.push((
                None,
                (*id).into(),
                "no eligible PPI reflectivity sweep".into(),
            ));
            continue;
        };
        let field = match *id {
            "CREF" => map::composite_reflectivity(source),
            "ET" => map::echo_top(source, options.threshold_dbz),
            "VIL" => map::vil(source),
            "VILD" => map::vil_density(source),
            "EBASE" => retrieve::echo_base(source, options.threshold_dbz),
            "EDEPTH" => retrieve::echo_depth(source, options.threshold_dbz),
            "HMAX" => retrieve::height_of_max_reflectivity(source),
            "LREF" => retrieve::low_level_composite_reflectivity(source, options.height_m),
            "POH" => options.freezing_level_m.and_then(|h| map::poh(source, h)),
            "SHI" | "MESH" | "POSH" => options
                .freezing_level_m
                .zip(options.minus20c_level_m)
                .and_then(|(h0, h20)| {
                    map::hail(source, h0, h20, map::MeshCalibration::Witt1998).map(|h| match *id {
                        "SHI" => h.shi,
                        "MESH" => h.mesh_mm,
                        _ => h.posh_pct,
                    })
                }),
            _ => None,
        };
        match field {
            Some(field) => insert(volume, base, field, options, &mut report)?,
            None => report.unavailable.push((
                Some(base),
                (*id).into(),
                "missing geometry, source fields, or required hail-level heights".into(),
            )),
        }
    }
    Ok((loaded, report))
}

/// `process` command arguments.
#[derive(Debug, Args)]
pub struct ProcessArgs {
    /// Input radar file (one volume).
    pub file: PathBuf,
    /// Comma-separated product IDs; see `products`.
    #[arg(long, value_delimiter = ',', required = true)]
    pub products: Vec<String>,
    /// Output file. FM301 preserves derived fields without NEXRAD restrictions.
    #[arg(short, long)]
    pub output: PathBuf,
    /// Output format.
    #[arg(long, value_enum, default_value = "fm301")]
    pub to: OutputFormat,
    /// Radar band (s, c, x) for band-dependent products.
    #[arg(long, value_parser = ["s", "c", "x"])]
    pub band: Option<String>,
    /// Selected sweep indices for sweep products (columns still use all sweeps).
    #[arg(long, value_delimiter = ',')]
    pub sweeps: Option<Vec<usize>>,
    /// Replace existing derived fields in the output volume.
    #[arg(long)]
    pub overwrite: bool,
    /// Reflectivity threshold for echo products (dBZ).
    #[arg(long, default_value_t = map::ECHO_TOP_THRESHOLD_DBZ)]
    pub threshold_dbz: f32,
    /// Low-level composite ceiling above the radar (metres).
    #[arg(long, default_value_t = 3000.0)]
    pub height_m: f32,
    /// Freezing level above the radar (metres).
    #[arg(long)]
    pub freezing_level_m: Option<f32>,
    /// Minus-20-C level above the radar (metres).
    #[arg(long)]
    pub minus20c_level_m: Option<f32>,
    /// Dealiasing engine.
    #[arg(long, default_value = "region", value_parser = ["region", "pyart", "volume"])]
    pub dealias_method: String,
    /// Previous radar file for the volume dealiaser.
    #[arg(long)]
    pub previous: Option<PathBuf>,
    /// Environmental wind profile JSON (valid_time and levels) for the volume dealiaser.
    #[arg(long)]
    pub environment_profile: Option<PathBuf>,
    /// Refuse unavailable products or writer omissions before writing.
    #[arg(long)]
    pub strict: bool,
    /// Replace an existing output file.
    #[arg(long)]
    pub force: bool,
    /// What to decode from the input.
    #[command(flatten)]
    pub input: InputArgs,
}

pub(crate) fn run(
    args: &ProcessArgs,
    backends: &Backends,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let input = open::open_path(&args.file, &OpenOptions::from_args(&args.input, true))?;
    let volumes = input.into_volumes();
    if volumes.len() != 1 {
        return Err(bad(
            "process requires one volume; select a station or split the input first",
        ));
    }
    let source = volumes
        .first()
        .ok_or_else(|| bad("input contains no volume"))?;
    let options = ProcessOptions {
        products: args.products.clone(),
        band: args.band.clone(),
        sweeps: args.sweeps.clone(),
        overwrite: args.overwrite,
        threshold_dbz: args.threshold_dbz,
        height_m: args.height_m,
        freezing_level_m: args.freezing_level_m,
        minus20c_level_m: args.minus20c_level_m,
        dealias_method: args.dealias_method.clone(),
        environment: args
            .environment_profile
            .as_ref()
            .map(|path| {
                let text = std::fs::read_to_string(path).map_err(|e| CliError::io(path, e))?;
                serde_json::from_str(&text)
                    .map_err(|e| bad(format!("invalid environment profile: {e}")))
            })
            .transpose()?,
    };
    let previous = args
        .previous
        .as_ref()
        .map(|path| {
            let mut volumes =
                open::open_path(path, &OpenOptions::from_args(&args.input, true))?.into_volumes();
            if volumes.len() != 1 {
                return Err(bad("previous input must contain one volume"));
            }
            volumes
                .pop()
                .ok_or_else(|| bad("previous input contains no volume"))
        })
        .transpose()?;
    let (loaded, report) = process(source, &options, previous.as_ref())?;
    if args.strict && !report.unavailable.is_empty() {
        return Err(CliError::Failed(format!(
            "unavailable products: {:?}",
            report.unavailable
        )));
    }
    let write_options = WriteOptions {
        strict: args.strict,
        ..WriteOptions::default()
    };
    let source_name = args.file.display().to_string();
    let input = WriteInput {
        volume: &loaded.volume,
        metadata: &loaded.metadata,
        source_name: Some(&source_name),
    };
    let mut bytes = Vec::new();
    let written = backends
        .writer(args.to)?
        .write(&input, &write_options, &mut bytes)?;
    let mut file = crate::output::AtomicFile::create(&args.output, args.force)?;
    file.writer().write_all(&bytes)?;
    file.commit()?;
    let result = serde_json::json!({"output": args.output, "processing": report, "left_out": written.left_out, "notes": written.notes});
    writeln!(
        out,
        "{}",
        serde_json::to_string_pretty(&result).map_err(|e| CliError::Failed(e.to_string()))?
    )?;
    Ok(())
}
