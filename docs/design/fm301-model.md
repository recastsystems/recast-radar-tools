# FM301 data model for `recast-radar-core` (wave 2, task F.1)

Status: design note, awaiting independent review (plan F.1). Date: 2026-09-16.
Branch: `fm301` (main `79f3410` with branch `testdata` merged).

Inputs:

- Spec `docs/superpowers/specs/2026-09-16-recast-radar-tools-design.md`, sections 2, 4.2 and 4.3.
- Plan `docs/superpowers/plans/2026-09-16-wave2.md`: Stream F, and the Global Constraints of
  wave 2 and wave 1.
- WMO-No. 306 *Manual on Codes*, Volume I.2, 2023 edition (updated 2024), Part B section c,
  "WMO-CF Extensions". That covers the General Regulations WMO-CF.1 to WMO-CF.7 (Tables
  WMO-CF-1 and WMO-CF-2) and **FM 301-2022 WMO-CF RADIAL** (Regulations 301.1 to 301.8,
  Tables 301-1 to 301-15). Regulation and table numbers below refer to that text.
- What xradar 0.12.0 (`open_nexradlevel2_datatree`, `open_odim_datatree`,
  `open_cfradial1_datatree`) and arm_pyart 2.2.5 (`read_nexrad_archive`, `aux_io.read_odim_h5`,
  `read_cfradial`) actually return for real files (appendix A). xradar's own `model.py`
  describes its layout as "a temporary setup, since CfRadial2.1 and FM301 are not yet
  finalized", so xradar and the published FM301-2022 text differ in places. Section 14 lists
  the differences and how this model handles each.

User priority: future Python bindings must map cleanly onto the xradar `DataTree`
(FM301 / CfRadial 2) and the Py-ART `Radar`. Mirroring Py-ART's Python API names in Rust is
not a goal. Hard constraint: keep compact raw storage and lazy scaling. Decode must not force
float expansion or extra copies.

F.1 items and where they are covered:

| Item | Section |
|---|---|
| Exact mapping of legacy types onto FM301 | 5 |
| Per-moment gate geometry within one sweep | 6 |
| CF packing attributes | 7 |
| Canonical short names and Py-ART alias table | 8 |
| Per-ray instrument variables | 9 |
| `sweep_mode` | 10 |
| Global attributes | 11 |
| Compatibility shim | 13 |
| Rust type definitions | 2 (`Volume`), 3 (`Sweep`), 4 (`Field`, `FieldName`), 13.2 (shim) |

---

## 0. Decisions at a glance

1. **Structure.** `Volume` is the FM301 root group. Each `Sweep` is one `sweep_<n>` group:
   one physical elevation cut, numbered in acquisition order. Split cuts and SAILS or
   MESO-SAILS repeats are separate sweeps, as in xradar, Py-ART and the current decoder
   (A.2). Each `Field` is one dataset variable, and `FieldName` is its variable name.
2. **Rays.** Ray coordinates are struct-of-arrays: `time_s: Vec<f64>`,
   `azimuth_deg: Vec<f32>`, `elevation_deg: Vec<f32>`. Rays stay in the source's storage order,
   which is acquisition order for NEXRAD, CfRadial and DORADE and azimuth-indexed order for
   ODIM and JMA. Rust never sorts rays. The FM301 view uses `time` as the ray dimension.
   Conformance tests sort both sides by azimuth (elevation for RHI), which is xradar's
   default ordering.
3. **Gate geometry.** Each sweep has one sweep-level `range` coordinate. That is what FM301
   requires (Regulation 301.2.3, Table 301-6a), and xradar and Py-ART also produce one. Each
   field stores only its native gates, with no padding or resampling, plus a
   `GateMapping { start, stride }` onto the sweep range. Two cases need a mapping: truncated
   fields (modern NEXRAD dual-pol moments carry 1192 of 1832 gates) and coarse fields
   (Message 1 and legacy-resolution Message 31 reflectivity: 1 km gates on a 250 m range). The
   FM301 view pads or replicates those fields only when a caller reads them. xradar's backend
   array does the same padding. There are no per-field range coordinates. Section 6 justifies
   this against xradar and Py-ART output on the same files.
4. **Storage.** `FieldData` holds values row-major, `[nrays × ngates]`, in the source's
   encoding: `U8` or `U16` (NEXRAD, ODIM), `I8` or `I16` (CfRadial `byte`/`short` packing,
   present in the real corpus), or `F32`. Each variant carries its coding: linear transform,
   `_FillValue`, `_Undetect`, range-folded code and `valid_range`. Physical values are
   computed on demand. The NEXRAD transform is still evaluated as `(raw - offset) / scale` in
   f32, so render checksums stay identical. Py-ART evaluates the same float32 expression.
5. **Sentinels.** For NEXRAD, raw 0 is `_FillValue`, per the spec; xarray then masks it, as
   Py-ART does. Raw 1 is kept as `flag_values = 1`, `flag_meanings = "range_folded"` (Table
   301-10). A vector `missing_value = [0, 1]` was tested and rejected: xarray warns when
   decoding it and refuses to write it back (A.6).
6. **Names.** NEXRAD names follow xradar's mapping: REF→DBZH, VEL→VRADH, SW→WRADH, ZDR→ZDR,
   PHI→PHIDP, RHO→RHOHV, CFP→CCORH. JMA gets FM301 names. ODIM, CfRadial and DORADE keep their
   source names verbatim, which is what xradar returns for ODIM and CfRadial (A.4, A.5). A
   separate `Quantity` class answers lookups such as "the reflectivity field" regardless of
   spelling. Py-ART aliases follow the `pyart.config` defaults.
7. **Format metadata.** NEXRAD RDA status, VCP, clutter maps and similar data live in typed
   structs owned by the format crate. They sit beside `Volume`, as
   `NexradVolume { volume, metadata }` (plan A.4), not inside it, because `core` must not
   depend on `io-*` crates. This departs from the literal wording of spec 4.2 (section 15).
8. **Shim.** `pub type` aliases cannot keep un-migrated code compiling, because that code uses
   legacy struct fields and struct literals: 1,561 field-access sites in 77 files and about 140
   struct literals (section 13.1). Instead:
   - The legacy types move unchanged to `recast_radar_core::legacy` and are re-exported at
     their old paths.
   - Conversions in both directions move gate buffers rather than copy them.
   - Deprecation warnings are opt-in, behind `--cfg recast_legacy_deprecation`, so other
     streams' `-D warnings` CI is unaffected.
   - The legacy module is deleted at the end of F.3.

---

## 1. Group and variable layout

The FM301 view (section 12) builds these groups and variables from the in-memory model:

| FM301 path | Rust | Notes |
|---|---|---|
| `/` attributes (Tables 301-1..3, WMO-CF-2) | `Volume::attrs: GlobalAttrs` | section 11 |
| `/volume_number`, `/time_coverage_start`, `/time_coverage_end` | `Volume::{volume_number, time_coverage}` | |
| `/latitude`, `/longitude`, `/altitude`, `/altitude_agl` | `Volume::location` | xradar makes these root coordinates, inherited by sweeps |
| `/platform_type`, `/instrument_type`, `/primary_axis`, `/status_str` | `Volume::{platform_type, instrument_type, primary_axis, status_str}` | |
| `/sweep_group_name(sweep)`, `/sweep_fixed_angle(sweep)` | derived from `Volume::sweeps` | CfRadial 2 root variables. xradar emits them for ODIM and CfRadial 1 but not NEXRAD (A.2, A.4, A.5). The view always emits them |
| `/radar_parameters/*` | `Volume::radar_parameters` | Table 301-12 |
| `/radar_calibration/*` (dim `calib`) | `Volume::radar_calibration: Vec<RadarCalibration>` | Table 301-14 |
| `/georeferencing_correction/*` | `Volume::georeferencing_correction` | CfRadial only; FM301 covers fixed platforms only |
| `/sweep_<n>` | `Volume::sweeps[n]: Sweep` | `sweep_number == n` |
| `/sweep_<n>/time(time)`, `azimuth(time)`, `elevation(time)` | `Sweep::rays` | Tables 301-6a, 301-7a |
| `/sweep_<n>/range(range)` | `Sweep::range: RangeCoord` | one per sweep (section 6) |
| `/sweep_<n>/frequency(frequency)` | `Volume::radar_parameters.frequency_hz` | constant over the volume; written into every sweep |
| `/sweep_<n>/sweep_number, sweep_mode, follow_mode, prt_mode, fixed_angle` | `Sweep` fields | Table 301-7a; xradar spells `fixed_angle` as `sweep_fixed_angle` |
| `/sweep_<n>/{polarization_mode, rays_are_indexed, rays_angle_resolution, qc_procedures, target_scan_rate}` | `Sweep` fields | Table 301-8a (scalars) |
| `/sweep_<n>/{nyquist_velocity, unambiguous_range, prt, ...}(time)` | `Sweep::ray_vars: RayVariables` | Table 301-8a (per ray); section 9 |
| `/sweep_<n>/monitoring/*` | `Sweep::monitoring` | Table 301-11 |
| `/sweep_<n>/<FIELD>(time, range)` | `Sweep::fields: Vec<Field>` | Tables 301-9, 301-10; source order kept |

---

## 2. `Volume`

```rust
// crates/recast-radar-core/src/model/volume.rs
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// FM301 volume: the root group of an FM301 / CfRadial 2 file and the root node
/// of an xradar `DataTree`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Volume {
    /// Root attributes (Tables 301-1, 301-2, 301-3; WMO-CF-2).
    pub attrs: GlobalAttrs,
    /// `/volume_number`.
    pub volume_number: Option<i32>,
    /// Epoch for every "seconds since <reftime>" value in the volume
    /// (`Rays::time_s`, `RadarCalibration::time_s`). NEXRAD: volume header
    /// date/time, the same epoch Py-ART writes into `time.units`.
    pub time_reference: DateTime<Utc>,
    /// `/time_coverage_start`, `/time_coverage_end`: first and last ray.
    pub time_coverage: Option<TimeCoverage>,
    /// `/latitude`, `/longitude`, `/altitude`, `/altitude_agl`.
    pub location: Location,
    /// `/platform_type` (Table 301-15).
    pub platform_type: PlatformType,
    /// `/instrument_type` (Table 301-15).
    pub instrument_type: InstrumentType,
    /// `/primary_axis` (Table 301-15).
    pub primary_axis: Option<PrimaryAxis>,
    /// `/status_str`.
    pub status_str: Option<String>,
    /// `scan_name`, `scan_id`, and scan-table provenance.
    pub scan: ScanStrategy,
    /// `/radar_parameters` and the `frequency` coordinate.
    pub radar_parameters: RadarParameters,
    /// `/radar_calibration`, one entry per `calib` index.
    pub radar_calibration: Vec<RadarCalibration>,
    /// `/georeferencing_correction` (CfRadial 1/2; not part of FM301).
    pub georeferencing_correction: Option<Box<GeoreferencingCorrection>>,
    /// Source format, container version, decode statistics. Not exported as variables.
    pub provenance: Provenance,
    /// Model / forward-operator provenance for simulated volumes.
    pub simulation: Option<Box<SimulationProvenance>>,
    /// `sweep_0 ..` in acquisition order; `sweeps[i].sweep_number == i`.
    pub sweeps: Vec<Sweep>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeCoverage {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GlobalAttrs {
    pub title: Option<String>,
    pub institution: Option<String>,
    pub references: Option<String>,
    pub source: Option<String>,
    pub history: Option<String>,
    pub comment: Option<String>,
    /// `instrument_name`: NEXRAD ICAO; ODIM `what/source` NOD, else RAD, else WMO;
    /// CfRadial attribute. Empty when the source has none (KTLX 1999 has a NUL ICAO).
    pub instrument_name: String,
    pub site_name: Option<String>,
    /// `platform_is_mobile` (FM301 requires "false").
    pub platform_is_mobile: bool,
    pub ray_times_increase: Option<bool>,
    /// `simulated` (Table 301-3).
    pub simulated: bool,
    /// `wmo__*` attributes (WMO-CF.6.10).
    pub wmo: WmoAttrs,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WmoAttrs {
    pub wsi: Option<String>,                // wmo__wsi
    pub id: Option<String>,                 // wmo__id (ODIM "WMO:03962" -> "03962")
    pub originating_centre: Option<u16>,    // wmo__originating_centre (Common Code Table C-11)
    pub originating_sub_centre: Option<u16>,
    pub data_category: Option<u8>,          // wmo__data_category (C-13)
    pub data_policy: Option<WmoDataPolicy>, // wmo__data_policy
    pub update_sequence_number: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WmoDataPolicy { Core, Recommended }

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Location {
    /// WGS84 degrees north. `None` for Message 1 archives (no VOL block).
    pub latitude_deg: Option<f64>,
    pub longitude_deg: Option<f64>,
    /// Metres above MSL at the antenna's centre of rotation. NEXRAD: VOL height +
    /// feedhorn height, as xradar and Py-ART compute it (KTLX: 389 m).
    pub altitude_m: Option<f64>,
    pub altitude_agl_m: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlatformType {
    Fixed, Vehicle, Ship, Aircraft, AircraftFore, AircraftAft, AircraftTail,
    AircraftBelly, AircraftRoof, AircraftNose, SatelliteOrbit, SatelliteGeostat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstrumentType { Radar, Lidar }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrimaryAxis { AxisZ, AxisY, AxisX, AxisZPrime, AxisYPrime, AxisXPrime }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanStrategy {
    /// `scan_name`. NEXRAD: "VCP-212" (xradar's spelling).
    pub name: Option<String>,
    /// `scan_id` (FM301 int). NEXRAD: the VCP number.
    pub id: Option<i64>,
    /// NEXRAD VCP pattern (legacy `RadarVolume::vcp`; BowEcho CfRadial exports write it
    /// as `vcp_pattern`).
    pub vcp_pattern: Option<u16>,
    /// Scan-table provenance (legacy `vcp_source_*`, `vcp_pulse_length`,
    /// `vcp_adaptations`, `scan_legs`).
    pub definition: Option<Box<ScanDefinition>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanDefinition {
    pub source_document: Option<String>,
    pub source_revision: Option<String>,
    pub source_rda_build: Option<String>,
    pub source_figure: Option<String>,
    pub pulse_length: Option<String>,
    pub adaptations: Option<String>,
    /// Non-numeric `scan_id` text the source wrote, when `ScanStrategy::id` cannot hold it.
    pub scan_id_text: Option<String>,
    /// One leg per sweep: the legacy `ScanLegMetadata` with unchanged fields.
    pub legs: Vec<ScanLeg>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RadarParameters {
    /// Operating frequencies in Hz (`frequency` dimension). Legacy `radar_frequency_mhz` × 1e6.
    pub frequency_hz: Vec<f64>,
    pub antenna_gain_h_db: Option<f32>,     // radar_antenna_gain_h
    pub antenna_gain_v_db: Option<f32>,     // radar_antenna_gain_v
    pub beam_width_h_deg: Option<f32>,      // radar_beam_width_h
    pub beam_width_v_deg: Option<f32>,      // radar_beam_width_v
    pub receiver_bandwidth_hz: Option<f32>, // radar_receiver_bandwidth
    /// Volume-constant values that some sources declare instead of per-ray vectors
    /// (legacy `VolumeMetadata::{pulse_width_us, prt_s, unambiguous_range_km}`).
    /// For sweeps whose `RayVariables` lack them, the view broadcasts these into
    /// `(time)` variables.
    pub pulse_width_s: Option<f32>,
    pub prt_s: Option<f32>,
    pub unambiguous_range_m: Option<f32>,
}

/// One `calib` entry. Field names are the Table 301-14a variable names plus a unit suffix.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RadarCalibration {
    pub calib_index: Option<i16>,
    pub time_s: Option<f64>, // seconds since Volume::time_reference
    pub pulse_width_s: Option<f32>,
    pub antenna_gain_h_db: Option<f32>, pub antenna_gain_v_db: Option<f32>,
    pub xmit_power_h_dbm: Option<f32>, pub xmit_power_v_dbm: Option<f32>,
    pub two_way_waveguide_loss_h_db: Option<f32>, pub two_way_waveguide_loss_v_db: Option<f32>,
    pub two_way_radome_loss_h_db: Option<f32>, pub two_way_radome_loss_v_db: Option<f32>,
    pub receiver_mismatch_loss_db: Option<f32>,
    pub receiver_mismatch_loss_h_db: Option<f32>, pub receiver_mismatch_loss_v_db: Option<f32>,
    pub radar_constant_h: Option<f32>, pub radar_constant_v: Option<f32>,
    pub probert_jones_correction: Option<f32>, pub dielectric_factor_used: Option<f32>,
    pub noise_hc_dbm: Option<f32>, pub noise_vc_dbm: Option<f32>,
    pub noise_hx_dbm: Option<f32>, pub noise_vx_dbm: Option<f32>,
    pub receiver_gain_hc_db: Option<f32>, pub receiver_gain_vc_db: Option<f32>,
    pub receiver_gain_hx_db: Option<f32>, pub receiver_gain_vx_db: Option<f32>,
    pub base_1km_hc_dbz: Option<f32>, pub base_1km_vc_dbz: Option<f32>,
    pub base_1km_hx_dbz: Option<f32>, pub base_1km_vx_dbz: Option<f32>,
    pub sun_power_hc_dbm: Option<f32>, pub sun_power_vc_dbm: Option<f32>,
    pub sun_power_hx_dbm: Option<f32>, pub sun_power_vx_dbm: Option<f32>,
    pub noise_source_power_h_dbm: Option<f32>, pub noise_source_power_v_dbm: Option<f32>,
    pub power_measure_loss_h_db: Option<f32>, pub power_measure_loss_v_db: Option<f32>,
    pub coupler_forward_loss_h_db: Option<f32>, pub coupler_forward_loss_v_db: Option<f32>,
    pub zdr_correction_db: Option<f32>,
    pub ldr_correction_h_db: Option<f32>, pub ldr_correction_v_db: Option<f32>,
    pub system_phidp_deg: Option<f32>,
    pub test_power_h_dbm: Option<f32>, pub test_power_v_dbm: Option<f32>,
    pub receiver_slope_hc: Option<f32>, pub receiver_slope_vc: Option<f32>,
    pub receiver_slope_hx: Option<f32>, pub receiver_slope_vx: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub source_format: SourceFormat,
    pub source_path: Option<String>,
    /// As written: "AR2V0006", "ARCHIVE2.036", "H5rad 2.3", "CF-Radial-1.3".
    pub source_version: Option<String>,
    /// The source's `Conventions`, which xradar passes through ("ODIM_H5/V2_2", "CF-1.6").
    pub source_conventions: Option<String>,
    pub compression: Option<String>,
    pub decode: DecodeStats,
    /// Free text carried by BowEcho-written CfRadial files (legacy `polarization`, `calibration`).
    pub polarization_note: Option<String>,
    pub calibration_note: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SourceFormat {
    NexradLevel2, NexradLevel3, OdimH5, CfRadial1, CfRadial2, Dorade, JmaGrib2, Simulated,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodeStats {
    pub message_count: usize,
    pub decoded_ray_count: usize,
    pub skipped_message_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SimulationProvenance {
    pub forward_operator: Option<String>,
    pub forward_operator_config: Option<String>,
    pub source_model: Option<String>,
    pub microphysics_scheme: Option<String>,
    pub scattering_model: Option<String>,
}

/// One `Option<f32>` per name in xradar's `georeferencing_correction_subgroup`
/// (azimuth_correction, elevation_correction, range_correction, longitude_correction, ...).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GeoreferencingCorrection { /* ... */ }
```

`ScanLeg` is the legacy `ScanLegMetadata` with identical fields.

**Format extensions are typed and live beside the volume, not inside it.**

```rust
// recast-radar-io-nexrad (stream A.4): no change to core
pub struct NexradVolume { pub volume: Volume, pub metadata: NexradMetadata }

// recast-radar-io (router): typed, not an untyped map
pub enum FormatMetadata { None, Nexrad(Box<NexradMetadata>), Odim(Box<OdimMetadata>) /* ... */ }
pub struct Decoded { pub volume: Volume, pub metadata: FormatMetadata }
```

The FM301 view takes an optional `&dyn fm301::ExtraAttrs` (section 12). io-nexrad implements
it to emit xradar's NEXRAD root attributes (`scan_name`, `dynamic_scan_type`,
`rda_build_number`, ...) and sweep attributes (`waveform_type`, `sails_cut`, ...) from
`NexradMetadata` (A.2).

---

## 3. `Sweep`

```rust
// crates/recast-radar-core/src/model/sweep.rs

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sweep {
    /// `sweep_number`: 0-based acquisition index; the group is `sweep_<sweep_number>`.
    pub sweep_number: u32,
    /// `sweep_mode` (Table 301-15; section 10).
    pub sweep_mode: SweepMode,
    /// `follow_mode`. `None` means the source does not say; exported as "not_set".
    pub follow_mode: Option<FollowMode>,
    /// `prt_mode`. `None` means the source does not say; exported as "not_set".
    pub prt_mode: Option<PrtMode>,
    /// `polarization_mode`.
    pub polarization_mode: Option<PolarizationMode>,
    /// `fixed_angle` (xradar: `sweep_fixed_angle`). Target elevation; target azimuth for RHI.
    pub fixed_angle_deg: f32,
    /// `target_scan_rate`.
    pub target_scan_rate_deg_per_s: Option<f32>,
    /// `rays_are_indexed`, `rays_angle_resolution`.
    pub rays_are_indexed: Option<bool>,
    pub rays_angle_resolution_deg: Option<f32>,
    /// `qc_procedures`.
    pub qc_procedures: Option<String>,
    /// `time(time)`, `azimuth(time)`, `elevation(time)`, in source storage order.
    pub rays: Rays,
    /// `range(range)`: gate centres shared by every field (section 6).
    pub range: RangeCoord,
    /// Optional `(time)` instrument variables (section 9).
    pub ray_vars: RayVariables,
    /// `monitoring` subgroup (Table 301-11).
    pub monitoring: Option<Box<Monitoring>>,
    /// Per-ray platform position for moving platforms (CfRadial 1 `latitude(time)`, ...).
    /// Not FM301.
    pub platform_track: Option<Box<PlatformTrack>>,
    /// Dataset variables, in source order.
    pub fields: Vec<Field>,
    /// Source cut number (NEXRAD ICD elevation number, 1-based). Not an FM301 variable.
    pub elevation_number: Option<u16>,
    /// `false` when rays stop before the end-of-elevation marker (truncated archive,
    /// real-time chunk). Same notion as xradar's `incomplete_sweep`.
    pub complete: bool,
    /// Shim only (section 13): set by `TryFrom<RadarVolume>`; removed with the shim.
    #[doc(hidden)]
    #[serde(skip)]
    pub legacy: Option<Box<crate::legacy::SweepResidue>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rays {
    /// `time`: seconds since `Volume::time_reference`, at ray centre (FM301 double).
    pub time_s: Vec<f64>,
    /// `azimuth`: degrees clockwise from true north.
    pub azimuth_deg: Vec<f32>,
    /// `elevation`: degrees above horizontal.
    pub elevation_deg: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RangeCoord {
    /// `spacing_is_constant = "true"`: centre of gate j is `first_center_m + j * spacing_m`
    /// (`meters_to_center_of_first_gate`, `meters_between_gates`).
    Uniform { first_center_m: f64, spacing_m: f64, ngates: u32 },
    /// `spacing_is_constant = "false"`: explicit gate centres. CfRadial allows this, and a
    /// DORADE CSFD block can have several segments.
    Explicit { centers_m: Vec<f32> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SweepMode {
    Sector, Coplane, Rhi, VerticalPointing, Idle, AzimuthSurveillance,
    ElevationSurveillance, Sunscan, Pointing, ManualPpi, ManualRhi,
    DopplerBeamSwinging, ComplexTrajectory, ElectronicSteering,
    /// CfRadial 1.x values outside Table 301-15 ("calibration", "sunscan_rhi", ...), verbatim.
    Other(Box<str>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FollowMode { None, Sun, Vehicle, Aircraft, Target, Manual }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrtMode { Fixed, Staggered, Dual, Hybrid }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolarizationMode { Horizontal, Vertical, HvAlt, HvSim, Circular }

/// Table 301-11. Each variable is `(time)`; `None` means not provided.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Monitoring {
    pub radar_measured_transmit_power_h_dbm: Option<Vec<f32>>,
    pub radar_measured_transmit_power_v_dbm: Option<Vec<f32>>,
    pub radar_measured_sky_noise_dbm: Option<Vec<f32>>,
    pub radar_measured_cold_noise_dbm: Option<Vec<f32>>,
    pub radar_measured_hot_noise_dbm: Option<Vec<f32>>,
    pub phase_difference_transmit_hv_deg: Option<Vec<f32>>,
    pub antenna_pointing_accuracy_elev_deg: Option<Vec<f32>>,
    pub antenna_pointing_accuracy_az_deg: Option<Vec<f32>>,
    pub calibration_offset_h_db: Option<Vec<f32>>,
    pub calibration_offset_v_db: Option<Vec<f32>>,
    pub zdr_offset_db: Option<Vec<f32>>,
}

/// CfRadial 1 moving-platform position per ray. Not FM301.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlatformTrack {
    pub latitude_deg: Vec<f64>,
    pub longitude_deg: Vec<f64>,
    pub altitude_m: Vec<f64>,
}
```

Construction API used by decoders. Every call is O(1) or O(fields); none copies gate data.

```rust
impl Sweep {
    pub fn new(sweep_number: u32, sweep_mode: SweepMode, fixed_angle_deg: f32) -> Self;
    pub fn reserve_rays(&mut self, rays: usize);
    pub fn push_ray(&mut self, time_s: f64, azimuth_deg: f32, elevation_deg: f32) -> usize;
    /// Register a field's native geometry. Grows or refines `range` as needed and returns the
    /// field's mapping. Refining (a finer spacing arrives later) rewrites the existing fields'
    /// `GateMapping`s; no gate data moves (section 6.5).
    pub fn attach_geometry(&mut self, first_center_m: f64, spacing_m: f64, ngates: u32)
        -> Result<GateMapping, GeometryError>;
    pub fn field(&self, name: &FieldName) -> Option<&Field>;
    pub fn field_mut(&mut self, name: &FieldName) -> Option<&mut Field>;
    /// Preferred field for a quantity: H before unspecified before V.
    pub fn find(&self, quantity: Quantity) -> Option<&Field>;
    /// Append fill rows for rays a field never received and check the invariants.
    /// Decoders call it once per sweep.
    pub fn seal(&mut self) -> Result<(), SweepError>;
}
```

Invariants after `seal`:

- Every `Rays` vector has `nrays` entries.
- Every field has `field.nrays == nrays`.
- Every field satisfies `field.gates.start + field.ngates * field.gates.stride <= range.ngates()`.
- `stride > 1` occurs only with `RangeCoord::Uniform`.
- Field names are unique within the sweep.

---

## 4. `Field` and `FieldName`

```rust
// crates/recast-radar-core/src/model/field.rs

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// Variable name in the sweep group (section 8).
    pub name: FieldName,
    /// Semantic class regardless of spelling (DBZH, DBZ, DBZHC_F all -> Reflectivity).
    pub quantity: Quantity,
    pub polarization: Polarization,
    /// CF / FM301 attributes: standard_name, long_name, units, Table 301-10.
    pub attrs: FieldAttrs,
    /// Rows. Equal to the sweep's ray count once the sweep is sealed.
    pub nrays: u32,
    /// Native gates per row.
    pub ngates: u32,
    /// Where the native gates sit on `Sweep::range` (section 6).
    pub gates: GateMapping,
    /// Row-major `[nrays × ngates]` values in the source encoding (section 7).
    pub data: FieldData,
}

/// Native gate i covers sweep-range gates `start + i*stride ..= start + i*stride + stride - 1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateMapping {
    pub start: u32,
    /// 1: native spacing equals the sweep range spacing. 4: 1 km gates over 250 m.
    pub stride: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FieldData {
    U8 { values: Vec<u8>, coding: IntCoding<u8> },
    U16 { values: Vec<u16>, coding: IntCoding<u16> },
    I8 { values: Vec<i8>, coding: IntCoding<i8> },
    I16 { values: Vec<i16>, coding: IntCoding<i16> },
    /// Physical values (derived products, float sources). NaN always means missing.
    F32 { values: Vec<f32>, coding: FloatCoding },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct IntCoding<T> {
    pub transform: LinearTransform,
    /// CF `_FillValue`: NEXRAD 0 (below threshold), ODIM `nodata`, CfRadial `_FillValue`.
    pub fill_value: Option<T>,
    /// Table 301-10 `_Undetect`: ODIM `undetect`.
    pub undetect: Option<T>,
    /// NEXRAD 1. Exported as `flag_values = [1]`, `flag_meanings = "range_folded"`.
    pub range_folded: Option<T>,
    /// CF `valid_range` in packed units (WMO-CF.5.2.15).
    pub valid_range: Option<[T; 2]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum LinearTransform {
    /// physical = (raw - offset) / scale, evaluated in f32. NEXRAD ICD form; the current
    /// decoder and Py-ART both evaluate exactly this expression.
    IcdScaleOffset { scale: f32, offset: f32 },
    /// physical = raw * scale_factor + add_offset. CF and ODIM gain-offset form.
    CfScaleOffset { scale_factor: f64, add_offset: f64 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FloatCoding {
    /// A non-NaN `_FillValue` the source used (for example -9999), if any.
    pub fill_value: Option<f32>,
    pub undetect: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldAttrs {
    pub standard_name: Option<Cow<'static, str>>,
    pub long_name: Option<Cow<'static, str>>,
    pub units: Option<Cow<'static, str>>,
    // Table 301-10, all optional
    pub sampling_ratio: Option<f32>,
    pub is_discrete: Option<bool>,
    pub field_folds: Option<bool>,
    pub fold_limit_lower: Option<f32>,
    pub fold_limit_upper: Option<f32>,
    pub is_quality_field: Option<bool>,
    pub qualified_variables: Vec<FieldName>,
    pub ancillary_variables: Vec<FieldName>,
    pub thresholding_xml: Option<String>,
    /// `flag_values` / `flag_meanings` of discrete fields (classification), besides the
    /// range-folded flag.
    pub flags: Vec<(i32, Box<str>)>,
    /// Source attributes with no slot above, verbatim and in file order
    /// (for example CfRadial `grid_mapping`).
    pub other: Vec<(Box<str>, AttrValue)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AttrValue { Text(String), I64(i64), F64(f64), F64s(Vec<f64>), I64s(Vec<i64>) }

/// One gate's value with its sentinel resolved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gate { Value(f32), Missing, Undetect, RangeFolded }

pub enum RowRef<'a> { U8(&'a [u8]), U16(&'a [u16]), I8(&'a [i8]), I16(&'a [i16]), F32(&'a [f32]) }

impl Field {
    pub fn new(name: FieldName, gates: GateMapping, ngates: u32, data: FieldData) -> Self;
    pub fn shape(&self) -> (usize, usize); // (nrays, ngates)
    pub fn row(&self, ray: usize) -> Option<RowRef<'_>>;
    pub fn gate(&self, ray: usize, gate: usize) -> Option<Gate>;
    /// Legacy `MomentGrid::scaled_value` semantics: `None` for every sentinel.
    pub fn value(&self, ray: usize, gate: usize) -> Option<f32>;
    /// 256-entry decode table for u8/i8 fields (NaN for sentinels) for hot loops.
    pub fn lut8(&self) -> Option<[f32; 256]>;
    /// Explicit float expansion. Decoders never call this.
    pub fn to_physical(&self) -> Vec<f32>;
    /// Native geometry on the given range: (centre of native gate 0, native spacing).
    pub fn native_geometry(&self, range: &RangeCoord) -> Option<(f64, f64)>;
    // Decode-time row pushes; padding and extension behave like the legacy push_* methods.
    pub fn reserve_rows(&mut self, rows: usize);
    pub fn push_row_u8(&mut self, row: &[u8]) -> Result<(), FieldError>;
    pub fn push_row_u16_be(&mut self, row_be: &[u8]) -> Result<(), FieldError>;
    pub fn push_row_i8(&mut self, row: &[i8]) -> Result<(), FieldError>;
    pub fn push_row_i16_be(&mut self, row_be: &[u8]) -> Result<(), FieldError>;
    pub fn push_row_f32(&mut self, row: &[f32]) -> Result<(), FieldError>;
    pub fn push_fill_row(&mut self);
}
```

```rust
// crates/recast-radar-core/src/model/names.rs

/// Dataset variable name. Known names are variants, so comparisons are cheap and each has
/// one spelling. Any other name is `Other`, verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[non_exhaustive]
pub enum FieldName {
    // FM301-2022 Table 301-9
    Dbzh, Dbzv, Zh, Zv, Dbth, Dbtv, Th, Tv, Vradh, Vradv, Wradh, Wradv,
    Zdr, Ldr, Ldrh, Ldrv, Phidp, Kdp, Phihx, Rhohv, Rhohx, Rhovx,
    Dbm, Dbmhc, Dbmhx, Dbmvc, Dbmvx, Snr, Snrhc, Snrhx, Snrvc, Snrvx,
    Ncp, Ncph, Ncpv, Rr, Rec,
    // Outside Table 301-9, but emitted by xradar 0.12 for sources we decode, or by our algorithms
    Dbz, Vrad, Wrad, Ccorh, Ccorv, Sqih, Sqiv, Snrh, Snrv, Rate, Vraddh, Uzdr, Uphidp, Urhohv,
    /// A verbatim source name (CfRadial "VEL", DORADE "DBZHC_F", ODIM "QIND") or a derived id.
    Other(Box<str>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Quantity {
    Reflectivity, LinearReflectivity, TotalPower, LinearTotalPower,
    RadialVelocity, DealiasedRadialVelocity, SpectrumWidth,
    DifferentialReflectivity, LinearDepolarizationRatio, DifferentialPhase,
    SpecificDifferentialPhase, CorrelationCoefficient, CrossPolarDifferentialPhase,
    CrossPolarCorrelation, ReceivedPower, SignalToNoiseRatio, NormalizedCoherentPower,
    SignalQualityIndex, ClutterCorrection, PrecipitationRate, EchoClassification, Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Polarization { H, V, Hv, CopolarH, CrosspolarH, CopolarV, CrosspolarV, Unspecified }

/// Static metadata for a known name.
pub struct NameInfo {
    pub name: &'static str,
    pub quantity: Quantity,
    pub polarization: Polarization,
    pub standard_name: Option<&'static str>,
    pub long_name: &'static str,
    /// UDUNITS spelling for the WMO flavor ("m s-1", "dB", "degree").
    pub units: &'static str,
    /// Units verbatim from xradar 0.12 `sweep_vars_mapping` ("meters per seconds", "unitless").
    pub units_xradar: &'static str,
    /// Default field name in `pyart.config`.
    pub pyart: Option<&'static str>,
}

impl FieldName {
    pub fn as_str(&self) -> &str;
    /// Exact, case-sensitive match against the table; otherwise `Other`.
    pub fn parse(name: &str) -> FieldName;
    /// Message 31 or Message 1 data block name (space- or NUL-padded) -> xradar name.
    pub fn from_nexrad_block(name: &[u8]) -> FieldName;
    pub fn info(&self) -> Option<&'static NameInfo>;
    /// Py-ART alias, or the name itself for `Other`.
    pub fn pyart_name(&self) -> Cow<'_, str>;
}

impl Quantity {
    /// Suffix-stripping classifier for verbatim names; replaces `canonical_moment`
    /// (DBZHC_F -> DBZHC -> DBZ -> Reflectivity). Returns `Other` when nothing matches.
    pub fn classify(name: &str) -> (Quantity, Polarization);
}
```

---

## 5. Exact mapping from the legacy model (F.1 item 1)

### 5.1 `RadarVolume`, `RadarSite`, `VcpInfo` and `VolumeMetadata` → `Volume`

| Legacy | New | Rule |
|---|---|---|
| `site.id` | `attrs.instrument_name` | verbatim |
| `site.name` | `attrs.site_name` | verbatim |
| `site.latitude_deg` / `longitude_deg` / `elevation_m` (f32) | `location.latitude_deg` / `longitude_deg` / `altitude_m` (f64) | widened; f32 → f64 → f32 is exact |
| `volume_time` | `time_reference` | verbatim |
| `vcp: Option<VcpInfo { pattern }>` | `scan.vcp_pattern`; for NEXRAD also `scan.id = pattern` and `scan.name = "VCP-{pattern}"` | |
| `cuts` | `sweeps` | index i gives `sweep_number = i` |
| `metadata.source_path`, `archive_version`, `compression` | `provenance.source_path`, `source_version`, `compression` | |
| `metadata.message_count`, `decoded_radial_count`, `skipped_message_count` | `provenance.decode.*` | |
| `metadata.scan_mode` | every `sweep.sweep_mode` | section 10 |
| `metadata.radar_frequency_mhz` (u32) | `radar_parameters.frequency_hz = [mhz × 1e6]` | |
| `metadata.beam_width_h_deg` / `beam_width_v_deg` | `radar_parameters.beam_width_h_deg` / `beam_width_v_deg` | |
| `metadata.pulse_width_us` | `radar_parameters.pulse_width_s = us × 1e-6` | |
| `metadata.prt_s` | `radar_parameters.prt_s` | |
| `metadata.unambiguous_range_km` | `radar_parameters.unambiguous_range_m = km × 1000` | |
| `metadata.scan_name` | `scan.name` | |
| `metadata.scan_id` (String) | `scan.id` if it parses as i64; otherwise `scan.definition.scan_id_text` | |
| `metadata.vcp_source_{document, revision, rda_build, figure}`, `vcp_pulse_length`, `vcp_adaptations`, `scan_legs` | `scan.definition.{source_document, source_revision, source_rda_build, source_figure, pulse_length, adaptations, legs}` | |
| `metadata.polarization`, `calibration` | `provenance.polarization_note`, `calibration_note` | free text |
| `metadata.forward_operator`, `forward_operator_config`, `source_model`, `microphysics_scheme`, `scattering_model` | `simulation.*`; `attrs.simulated = true` when any is set | |
| (none) | `provenance.source_format` | inferred from the `archive_version` / `compression` markers each decoder sets; the marker list is fixed in F.2 against real files |

### 5.2 `ElevationCut` and `Radial` → `Sweep`, ray coordinates and ray variables

| Legacy | New | Rule |
|---|---|---|
| `elevation_deg` | `fixed_angle_deg` | Verbatim. Today NEXRAD writes the first ray's elevation (KTLX 2024 cut 0: 0.582). Native F.3 decoding, using stream A's Message 5, writes the VCP angle (0.4834), as xradar and Py-ART do |
| `elevation_number: Option<u8>` | `elevation_number: Option<u16>` | widened |
| `radials[i].azimuth_deg`, `.elevation_deg` | `rays.azimuth_deg[i]`, `rays.elevation_deg[i]` | array-of-structs to struct-of-arrays |
| `radials[i].time_offset_ms` | `rays.time_s[i]`, and the raw value kept in the residue | Meaning depends on the decoder (13.2). NEXRAD stores ms of day of collection: `time_s = (midnight of time_reference's date + ms) - time_reference`, plus one day when that is more than 12 h before the reference. CfRadial stores ms since volume start (truncated): `time_s = ms / 1000`. DORADE stores ms since sweep start. ODIM and JMA store 0, so `time_s = 0` until native decoding |
| `radials[i].gate_range` | residue only | the field's `GateMapping` is authoritative |
| `radials[i].nyquist_velocity_mps` | `ray_vars.nyquist_velocity_mps[i]`, NaN for `None`; the whole vector is `None` if every radial is `None` | |
| `radials[i].radial_status` | residue; io-nexrad also keeps it in `NexradMetadata` | not FM301 |
| `ray_instrument_metadata[i].prt_s` | `ray_vars.prt_s[i]` | |
| `.unambiguous_range_km` | `ray_vars.unambiguous_range_m[i] = km × 1000` | |
| `.pulse_count` (u32) | `ray_vars.n_samples[i]` (i32; -9999 when missing) | |
| `.independent_samples` | `ray_vars.independent_samples[i]` | |
| `moments: BTreeMap<MomentType, MomentGrid>` | `fields: Vec<Field>` | BTreeMap order, the only order legacy kept. Native decoders use source order |
| (none) | `range` | built from the grids with `attach_geometry`, after converting each legacy `gate_range` to gate centres using that decoder's convention (6.6) |
| (none) | `complete` | `true` |

The legacy ODIM decoder synthesizes azimuths as `(ray + 0.5) × 360 / nrays` and ray times of
0. xradar returns measured per-ray values: iesha azimuths 0.5081, 1.5244, ... with the file's
`how/startazA`/`stopazA` present, and ray times that differ per ray. Native ODIM decoding in
F.3 matches xradar, which F.4 verifies.

### 5.3 `MomentGrid` / `MomentStorage` → `Field` / `FieldData`

| Legacy | New |
|---|---|
| `moment: MomentType` | `name = moment.to_field_name()`; `quantity` and `polarization` from `name.info()` |
| `gate_range` | `ngates = gate_count`; `gates = attach_geometry(centre(gate_range), spacing, count)` |
| `scale`, `offset` | `IcdScaleOffset { scale, offset }`. Every legacy grid uses this form, including ODIM's inverted gain |
| `nodata: Option<u16>` | `coding.fill_value` (narrowed to u8 for `U8` storage) |
| `range_folded: Option<u16>` | `coding.range_folded` |
| `radial_indices` | Removed: rows align 1:1 with rays. A non-identity mapping is scattered into fill rows, with the original indices kept in the residue. Every grid in the four NEXRAD probe volumes has rows == radials (A.2, A.3) |
| `storage: U8(v)` / `U16(v)` / `F32(v)` | `FieldData::U8 { values: v, .. }` / `U16` / `F32`, moved |

`valid_range` is `None` for converted grids; native decoders set it.

### 5.4 `MomentType` → `FieldName`

| Legacy variant | `FieldName` | Note |
|---|---|---|
| `Reflectivity` | `Dbzh` | native ODIM, CfRadial and DORADE decoding gives the verbatim name instead (DBZH, DBZ, DBZHC) |
| `Velocity` | `Vradh` | |
| `SpectrumWidth` | `Wradh` | |
| `DifferentialReflectivity` | `Zdr` | |
| `CorrelationCoefficient` | `Rhohv` | |
| `DifferentialPhase` | `Phidp` | |
| `SpecificDifferentialPhase` | `Kdp` | |
| `Unknown("CFP")` | `Ccorh` | NEXRAD block name |
| `Unknown(s)` | `FieldName::parse(s)`: a known name or `Other(s)` | migrated crates rename derived ids (8.3); the conversion does not |

`ProductId(String)` converts to `FieldName` through `parse`.

---

## 6. Gate geometry within a sweep (F.1 item 2)

### 6.1 The real cases

Geometry below comes from MetPy `Level2File` per-moment headers, which is independent of our
decoder (A.2, A.3).

| Volume | Sweep(s) | Moments in the same radials | Difference |
|---|---|---|---|
| KTLX 2024-03-15 00:02 (Build 22.0, VCP 212) | 0, 2, 4, 6, 8, 14 (surveillance) | REF and CFP 1832 gates (sweep 6: 1712); ZDR, PHI, RHO 1192; all start at 2125 m with 250 m spacing | gate count only |
| same | 10, 11 (batch) | REF and CFP 1536 / 1336; VEL, SW, ZDR, PHI, RHO 1192 | gate count only |
| KLIX 2005-08-29 13:00 (Message 1, VCP 121) | 2, 3, 6, 7, 8..19 | REF at 0 m + 1000 m × 137..356; VEL and SW at -375 m + 250 m × 548..920 | spacing ×4, and start |
| KTLX 1999-05-04 00:22 (Message 1, VCP 11) | 4..15 | REF at 0 m + 1000 m; VEL and SW at -375 m + 250 m | spacing ×4, and start |
| KPAH 2008-04-15 23:50 (Message 31 legacy resolution, VCP 32) | 4..6 | REF at 500 m + 1000 m × 328 / 271 / 228; VEL and SW at 125 m + 250 m × 920 / 912 | spacing ×4, and start |

MetPy's first-gate value is the range to the gate **centre**. In all three legacy-resolution
volumes the gate edges line up: REF gate i spans exactly Doppler gates 4i..4i+3. For example,
the REF gate centred at 0 m covers -500..500 m, which is the span of the Doppler gates centred
at -375, -125, 125 and 375 m.

### 6.2 What xradar 0.12 does with those files (A.2, A.3)

- One `range` per sweep. Start and spacing come from the first moment in the sweep, and the
  gate count is the maximum over moments (`nexrad_level2.py`, `open_store_coordinates`).
- Shorter moments are padded with raw 0 (`np.pad(..., constant_values=0)`). No `_FillValue`
  is set, so the padding decodes to the moment's `add_offset`: KTLX 2024 `sweep_10/VRADH`
  (native 1192 gates, range 1536) holds exactly -64.5 m/s in every padded gate.
  Below-threshold gates decode the same way: `sweep_0/DBZH` has 1,036,042 gates equal to
  -33.0 dBZ and no NaN.
- Different spacing is not handled:
  - KLIX 2005 `sweep_2`: `range` is 548 gates × 1000 m from 0 (0..547 km). That is REF's
    spacing with VEL's gate count, so `VRADH` is drawn at 4× its true range.
  - VEL-only sweeps report `meters_to_center_of_first_gate = 65161`, which is -375 read as
    unsigned 16-bit.
  - KPAH 2008 sweeps 4..6: `range` is 920 gates × 1000 m from 500. `VRADH` is again
    misplaced ×4, and `DBZH` is padded from 328 to 920 gates.
- Message 1 spectrum width is missing from every sweep.
- KTLX 1999 raises `ValueError: conflicting sizes for dimension 'azimuth'`; 9 of its 16
  sweeps also fail when opened one at a time.
- Whole-file gzip input (`.gz`) raises `TypeError`.
- Rays are sorted by azimuth by default (`first_dim="auto"`). `first_dim="time"` keeps
  acquisition order.

### 6.3 What Py-ART 2.2.5 does with those files (A.2, A.3)

- One range for the whole volume: minimum first gate, minimum spacing, maximum extent over
  all sweeps and moments (`_find_range_params`).
  - KTLX 2024: 2125..459875 m, 1832 gates.
  - KLIX 2005 and KTLX 1999: -375..459375 m, 1840 gates.
  - KPAH 2008: 125..459875 m, 1840 gates.
- Coarse moments are resampled to 250 m.
  - With `linear_interp=True` (the default) values are interpolated: KLIX 2005
    reflectivity contains -13.9375 dBZ, which is not a multiple of 0.5.
  - With `False` each 1 km value is repeated at fine gates 4i..4i+3. KLIX 2005 sweep 2: REF
    gate 1 (46.5 dBZ) lands in fine gates 4..7 and gate 2 (48.0 dBZ) in 8..11, which is the
    edge alignment of 6.1.
- Gates beyond a moment's native count are masked. Raw values <= 1 (below threshold and range
  folded) are masked. Physical values are `(raw - offset) / scale` in float32.
- The Py-ART ODIM reader pads shorter sweeps with **unmasked NaN** and writes
  `meters_to_center_of_first_gate = 0.0` while `range[0] = 250.0` (A.4).
- `pyart.xradar.Xradar(dt)` (DataTree to Radar) cannot combine sweeps with different `range`
  lengths: `AlignmentError ... conflicting dimension sizes: {1832, 1192}` for KTLX 2024
  sweeps [0, 1] (A.7).

### 6.4 Options considered

| Option | FM301 conformant | Copies at decode | Lossless | Matches |
|---|---|---|---|---|
| A. One range per volume, resampled at decode (Py-ART) | the view yes; the model coarser than FM301 needs | yes: every field padded to the volume maximum, coarse fields interpolated | no (interpolation) | Py-ART only |
| B. A range per field (BowEcho today: `MomentGrid::gate_range`) | no: Regulation 301.2.3 says all rays contain the same collection of range bins, and datasets are `(time, range)` | none | yes | neither |
| C. Sweep range, padded and repeated at decode | yes | yes: padding and ×4 replication | yes | xradar's shape |
| **D. Sweep range, native field storage and `GateMapping`; padding and repetition happen in the view** | **yes** (the view) | **none** | **yes** | xradar and FM301 shape; values equal Py-ART with `linear_interp=False` |

### 6.5 Decision: D

Each sweep has one FM301 `range`. It uses the finest spacing among the sweep's fields and
covers the union of their extents. Each field keeps its native `[nrays × ngates]` buffer and a
`GateMapping { start, stride }`.

- **Truncated fields** (`stride = 1`, `ngates < range.ngates`): the view pads them with
  `_FillValue`, which is NaN after CF decoding. xradar's padding instead reads as -64.5 m/s.
  Positions are identical to xradar's.
- **Coarse fields** (`stride = 4`): the view repeats each value `stride` times, giving
  exactly Py-ART's `linear_interp=False` values. The model does no interpolation. The
  variable gets `comment = "native gate spacing 1000 m; values repeated on the 250 m range
  coordinate"`.
- **Coordinates are correct where xradar's are wrong.** KLIX 2005 sweep 2 gets a range of
  -375 m + 250 m × 548. Because the range is per sweep, a REF-only surveillance sweep keeps
  its own 1 km range (sweep 0: 0 m + 1000 m × 460), which matches xradar for those sweeps.
- **Decode cost is zero.** Rows are written once into native buffers, as `MomentGrid` does
  today. When a finer spacing arrives later in the same radial (REF at 1 km first, VEL at
  250 m next), `Sweep::attach_geometry` rewrites integer mappings; no data moves.
- **Bindings.** A PyO3 layer hands `&[u8]` or `&[u16]` to NumPy without copying whenever
  `start == 0 && stride == 1 && ngates == range.ngates`. In KTLX 2024 that holds for 76 of 104
  fields: every field of the Doppler cuts and of sweeps 12, 13 and 16..19, and REF and CCORH
  in every sweep. The other 28 fields go through a lazy backend array that pads on
  `__getitem__`, which is xradar's own mechanism. Those 28 are the dual-pol moments in the
  surveillance cuts plus VEL, SW and the dual-pol moments in sweeps 10 and 11.
- **Algorithms** keep working in native geometry through `Field::native_geometry`, as they do
  with `MomentGrid::gate_range` today.

**Alignment rule** used by `attach_geometry` for uniform ranges. Take a field with centre `c`
and spacing `s`, and a range with centre `c0` and spacing `s0`. They align when both of these
hold, each within 1e-6 × `s0`:

- `k = s / s0` is a positive integer;
- `m = ((c - s/2) - (c0 - s0/2)) / s0` is an integer ≥ 0.

Then `stride = k` and `start = m`.

- If the field's start edge lies before the range, the range is extended backwards and every
  existing `start` is increased to match.
- If the finer spacing is not an integer divisor, the call returns
  `GeometryError::Unaligned`. NEXRAD never produces that case (6.1).
- When `merge_volumes` assembles an ODIM multi-file volume, an unaligned incoming field is
  counted in `MergeReport::skipped_geometry` and dropped, just as mismatched azimuth
  geometry is today.

An explicit (non-uniform) range accepts only fields with identical centres: `stride = 1`,
`start = 0`, `ngates <= len`.

### 6.6 Legacy range semantics (found while writing the mapping)

Legacy `GateRange::first_gate_m` means different things in different decoders, but consumers
treat it uniformly (`first_gate_m + g * spacing`, nearest gate by `round`):

| Decoder | Value stored in `first_gate_m` | What that is |
|---|---|---|
| io-nexrad | ICD "range to centre of first gate" (2125) | centre |
| io-odim | `where/rstart` × 1000 (500 for dkrom) | **start** of the first bin; xradar reports the centre, 750 |
| io-cfradial | `round(range[0] - spacing/2)` | **start**, rounded (IRENE centre 0 m becomes -38) |
| io-dorade | first cell + range delay, rounded | DORADE cell distance |
| io-jma | `range_start_m`, rounded | per GRIB2 template |

Every value is also rounded to whole metres. DOW8's first centre is 62.456 m and its spacing
124.913 m. Stored as 125 m, the spacing error puts gate 949 83 m off.

`RangeCoord` stores f64 gate centres everywhere. As a result, ODIM and CfRadial fields move by
half a gate (250 m for dkrom) to their correct positions. In F.3 that is an intentional
output change for those formats only. The NEXRAD checksum baselines are unaffected, because
io-nexrad already stores centres in whole metres.

---

## 7. CF packing, sentinels and value evaluation (F.1 item 3)

### 7.1 NEXRAD Level II

ICD 2620002 encodes a data moment as `value = (raw - offset) / scale`, with raw 0 meaning
below threshold and raw 1 meaning range folded. The CF form is `scale_factor = 1/scale` and
`add_offset = -offset/scale`, computed in f64 for the view. The table shows values observed
in KTLX 2024 (MetPy headers) and in legacy files, next to what xradar writes.

| Block | Word | scale | offset | `scale_factor` | `add_offset` | xradar encoding (A.2) |
|---|---|---|---|---|---|---|
| REF | u8 | 2.0 | 66.0 | 0.5 | -33.0 | 0.5 / -33.0, uint8 |
| VEL | u8 | 2.0 | 129.0 | 0.5 | -64.5 | 0.5 / -64.5, uint8 |
| SW (Msg 31) | u8 | 2.0 | 129.0 | 0.5 | -64.5 | 0.5 / -64.5, uint8 |
| ZDR | u16 | 32.0 | 418.0 | 0.03125 | -13.0625 | 0.03125 / -13.0625, >u2 |
| PHI | u16 | 2.8361001 | 2.0 | 0.3525968633763486 | -0.7051937267526972 | same, >u2 |
| RHO | u8 | 300.0 | -60.5 | 0.0033333333 | 0.2016666667 | same, uint8 |
| CFP | u8 | 1.0 | 8.0 | 1.0 | -8.0 | 1.0 / -8.0, uint8 |
| REF (Msg 1) | u8 | 2.0 | 66.0 | 0.5 | -33.0 | 0.5 / -33.0 |
| VEL (Msg 1, 0.5 m/s resolution) | u8 | 2.0 | 129.0 | 0.5 | -64.5 | 0.5 / -64.5 |
| VEL (Msg 1, 1.0 m/s resolution) | u8 | 1.0 | 129.0 | 1.0 | -129.0 | not in the probe corpus |
| SW (Msg 1) | u8 | 2.0 | 129.0 | 0.5 | -64.5 | xradar's code uses offset 192; its output has no SW |

The coding for these fields is `IntCoding { transform: IcdScaleOffset { scale, offset },
fill_value: Some(0), undetect: None, range_folded: Some(1), valid_range: Some([2, MAX]) }`.
MAX is 255 for u8. For u16 it is 65535 until stream A confirms the ICD data masks that xradar
applies (PHI `& 0x3FF`, ZDR `& 0x7FF`; section 15).

The FM301 view writes the encoded integers (`uint8`, `uint16`) with `scale_factor`,
`add_offset`, `_FillValue = 0`, `valid_range`, `flag_values = [1]` and
`flag_meanings = "range_folded"`. Consequences (A.6, run with xarray 2026.7.0):

- `xr.decode_cf` turns below-threshold gates into NaN, matching Py-ART.
- Range folded decodes to `add_offset + scale_factor` (-32.5 dBZ) unless the consumer applies
  the flag. The binding's DataTree builder masks `flag_values` when
  `mask_range_folded = true`. That is the default, to match Py-ART, which masks `raw <= 1`.
- `missing_value = [0, 1]` would mask both on decode, but xarray emits `SerializationWarning`
  ("multiple fill values"), and `to_netcdf` raises `ValueError` (conflicting `_FillValue` and
  `missing_value`, or an array truth-value error when `_FillValue` is absent). Rejected.
- This deliberately differs from xradar 0.12, which writes no `_FillValue` for NEXRAD, so its
  below-threshold and padded gates decode to physical-looking numbers. Conformance tests
  compare xradar's raw arrays (`mask_and_scale=False`) against ours.

Evaluation stays in the source's own arithmetic:

- `IcdScaleOffset` computes `(raw as f32 - offset) / scale`, identical to the current
  `MomentGrid::scale_raw`. The NEXRAD render checksums in
  `docs/baselines/import-checksums.txt` therefore cannot move, and values compare exactly
  with Py-ART's float32 output.
- `CfScaleOffset` computes `raw * scale_factor + add_offset` in f64 and casts to f32,
  following xarray's CF decoding, for ODIM and CfRadial.

### 7.2 ODIM_H5

`physical = gain × raw + offset` maps to `CfScaleOffset { scale_factor: gain, add_offset:
offset }`. `nodata` maps to `fill_value` and `undetect` to `undetect`, and the two stay
distinct; today io-odim remaps undetect onto nodata. xradar puts `_FillValue = nodata`
(255.0 for dkrom and iesha) in the encoding and `_Undetect = 0.0` in the attributes.

Float planes (espdg `float64`) are narrowed to `F32`, with `nodata` and `undetect` held as
`FloatCoding` values. Raw hashes are not comparable for those, so F.4 compares value
summaries.

### 7.3 CfRadial

Packed `byte` and `short` variables stay `I8` and `I16`, keeping the file's `scale_factor`,
`add_offset` and `_FillValue` (A.5):

- IRENE `DBZ` and `VEL`: int8, `_FillValue = -128`.
- DOW8 `DBZHC`, `VEL` and `WIDTH`: int16, `_FillValue = -32768`.

The file's bytes and attributes pass through unchanged; today the decoder expands them to
f32. Float variables become `F32`.

### 7.4 Derived fields

Algorithms write `F32` with `FloatCoding::default()`. Quantised outputs such as
classifications may use `U8` with `flags`.

---

## 8. Field names and Py-ART aliases (F.1 item 4)

### 8.1 Naming rule

A field gets the name xradar 0.12 gives it for the same file. For formats xradar does not
read, it gets the name that converting with Radx to CfRadial and opening with xradar would
give.

- **NEXRAD Level II:** xradar's `nexrad_mapping` (REF→DBZH, VEL→VRADH, SW→WRADH, ZDR,
  PHI→PHIDP, RHO→RHOHV, CFP→CCORH). Unknown block names stay verbatim (`Other`).
- **NEXRAD Level III radial products:** the FM301 name of the moment (N0B→DBZH, N0G→VRADH,
  ...), with the product code in the field attributes. Details come with the level3 merge.
- **JMA:** FM301 names (DBZH for Pze, VRADH for Pvr).
- **ODIM:** `what/quantity` verbatim, as xradar returns it (dkrom: `DBZH VRAD TH WRAD ZDR
  RHOHV PHIDP LDR`; iesha: `DBZH TH VRADH`).
- **CfRadial 1/2:** variable names verbatim, as xradar and Py-ART both return them (IRENE
  `DBZ VEL`; DOW8 `DBZHC VEL WIDTH`).
- **DORADE:** parameter names verbatim. Radx's `DoradeRadxFile` keeps them, and DOW8's
  CfRadial file was written that way.

This reads spec 4.2's "canonical names are the FM301/xradar short names" literally: when a
source already names its variables, xradar keeps those names. `Quantity::classify` provides
the semantic class for lookups (`Sweep::find(Quantity::Reflectivity)`) and replaces
`canonical_moment`.

### 8.2 Table of known names

- `standard_name` and long names come from FM301 Table 301-9 where listed, otherwise from
  xradar.
- "xradar units" are copied verbatim from xradar's `sweep_vars_mapping` and are used by the
  Xradar flavor.
- "WMO units" are UDUNITS spellings for the WMO flavor.
- The Py-ART alias is the default field name in `pyart.config`: the names
  `read_nexrad_archive` produces and Py-ART's algorithms expect. "—" means Py-ART has no
  default, so the name passes through unchanged.

| Name | Quantity / pol | standard_name | WMO units | xradar units | Py-ART alias |
|---|---|---|---|---|---|
| DBZH | Reflectivity / H | radar_equivalent_reflectivity_factor_h | dBZ | dBZ | reflectivity |
| DBZV | Reflectivity / V | radar_equivalent_reflectivity_factor_v | dBZ | dBZ | — |
| ZH | LinearReflectivity / H | radar_linear_equivalent_reflectivity_factor_h | mm6 m-3 | unitless | — |
| ZV | LinearReflectivity / V | radar_linear_equivalent_reflectivity_factor_v | mm6 m-3 | unitless | — |
| DBTH | TotalPower / H | radar_equivalent_reflectivity_factor_h | dBZ | dBZ | total_power |
| DBTV | TotalPower / V | radar_equivalent_reflectivity_factor_v | dBZ | dBZ | — |
| TH | FM301: LinearTotalPower / H. **ODIM TH is dBZ** (note 1) | radar_linear_equivalent_reflectivity_factor_h | note 1 | unitless | total_power (when dBZ) |
| TV | LinearTotalPower / V | radar_linear_equivalent_reflectivity_factor_v | mm6 m-3 | unitless | — |
| VRADH | RadialVelocity / H | radial_velocity_of_scatterers_away_from_instrument_h | m s-1 | meters per seconds | velocity |
| VRADV | RadialVelocity / V | radial_velocity_of_scatterers_away_from_instrument_v | m s-1 | meters per second | — |
| WRADH | SpectrumWidth / H | radar_doppler_spectrum_width_h | m s-1 | meters per seconds | spectrum_width |
| WRADV | SpectrumWidth / V | radar_doppler_spectrum_width_v | m s-1 | meters per second | — |
| ZDR | DifferentialReflectivity / Hv | radar_differential_reflectivity_hv | dB | dB | differential_reflectivity |
| LDR | LinearDepolarizationRatio / Hv | radar_linear_depolarization_ratio | dB | dB | linear_polarization_ratio |
| LDRH | LinearDepolarizationRatio / H | radar_linear_depolarization_ratio_h | dB | — | linear_depolarization_ratio_h |
| LDRV | LinearDepolarizationRatio / V | radar_linear_depolarization_ratio_v | dB | — | linear_depolarization_ratio_v |
| PHIDP | DifferentialPhase / Hv | radar_differential_phase_hv | degree | degrees | differential_phase |
| KDP | SpecificDifferentialPhase / Hv | radar_specific_differential_phase_hv | degree km-1 | degrees per kilometer | specific_differential_phase |
| PHIHX | CrossPolarDifferentialPhase | radar_differential_phase_copolar_h_crosspolar_v | degree | — | — |
| RHOHV | CorrelationCoefficient / Hv | radar_correlation_coefficient_hv | 1 | unitless | cross_correlation_ratio |
| RHOHX, RHOVX | CrossPolarCorrelation | radar_correlation_coefficient_copolar_h_crosspolar_v, ..._copolar_v_crosspolar_h | 1 | — | — |
| DBM, DBMHC, DBMHX, DBMVC, DBMVX | ReceivedPower | radar_received_signal_power[_copolar_h, ...] | dBm | dBm (DBM) | — |
| SNR, SNRHC, SNRHX, SNRVC, SNRVX | SignalToNoiseRatio | radar_signal_to_noise_ratio[_copolar_h, ...] | dB | — | signal_to_noise_ratio (SNR) |
| NCP, NCPH, NCPV | NormalizedCoherentPower | radar_normalized_coherent_power[_h, _v] | 1 | — | normalized_coherent_power (NCP) |
| RR | PrecipitationRate | radar_estimated_precipitation_rate | mm h-1 | — | radar_estimated_rain_rate |
| REC | EchoClassification | radar_scatterer_classification | 1 | — | radar_echo_classification |
| DBZ | Reflectivity / Unspecified | radar_equivalent_reflectivity_factor | dBZ | dBZ | reflectivity |
| VRAD | RadialVelocity / Unspecified | radial_velocity_of_scatterers_away_from_instrument | m s-1 | meters per seconds | velocity |
| WRAD | SpectrumWidth / Unspecified | radar_doppler_spectrum_width | m s-1 | meters per second | spectrum_width |
| CCORH | ClutterCorrection / H | clutter_correction_h | dB | unitless | clutter_filter_power_removed |
| CCORV | ClutterCorrection / V | clutter_correction_v | dB | unitless | — |
| SQIH, SQIV | SignalQualityIndex | signal_quality_index_h, _v | 1 | unitless | normalized_coherent_power (SQIH) |
| SNRH, SNRV | SignalToNoiseRatio | signal_noise_ratio_h, _v | dB | unitless | signal_to_noise_ratio (SNRH) |
| RATE | PrecipitationRate | rainfall_rate | mm h-1 | mm h-1 | radar_estimated_rain_rate |
| VRADDH | DealiasedRadialVelocity / H | radial_velocity_of_scatterers_away_from_instrument_h | m s-1 | meters per seconds | corrected_velocity |
| UZDR, UPHIDP, URHOHV | uncorrected ZDR / PHIDP / RHOHV | as ZDR / PHIDP / RHOHV | as those | as those | — |

Notes:

1. **ODIM `TH`.** FM301 Table 301-9 defines TH as *linear* total power. ODIM's TH quantity is
   logarithmic, in dBZ (dkrom TH spans -32..75.5), yet xradar labels it "Linear total power H"
   with units "unitless". The name stays `TH` for xradar compatibility, but io-odim sets
   `quantity = TotalPower` and units `dBZ`, following the ODIM definition. This is the one
   place the Xradar flavor's attributes knowingly differ from xradar (section 14).
2. **CCORH and NEXRAD CFP.** CFP is the clutter power removed, in dB. xradar maps it to CCORH
   and labels it unitless. Py-ART names it `clutter_filter_power_removed`, in dB.
3. **Py-ART's ODIM reader uses different names:** `reflectivity_horizontal`,
   `total_power_horizontal`, `velocity_horizontal` for VRADH, `velocity` for VRAD. The alias
   table follows `pyart.config` defaults, not the `aux_io` names. A
   `to_pyart(..., field_names=...)` binding can accept a custom mapping.
4. xradar's units are inconsistent: "meters per second" for VRADV and WRADV but "meters per
   seconds" for VRADH and WRADH. Only the Xradar flavor reproduces them verbatim.

### 8.3 Derived products

Standalone quantities use a Table 301-9 or xradar name where one exists. Products derived
from one input field are named `<BASE>_<SUFFIX>`, with BASE the input field's name, so the
rule works equally for DBZH and DBZ inputs. Suffixes are `_CLEAN` (as in xradar's
`DBZH_CLEAN`), `_CORR`, `_TEX`, `_GRAD_R` and `_SD`. Mapping from the current
`DerivedSweepProduct` ids:

| Current id | New name | Py-ART alias |
|---|---|---|
| KDP | KDP | specific_differential_phase |
| PHIF | PHIDP_CLEAN | corrected_differential_phase |
| KDP_SD | KDP_SD | — |
| AH, PIA, ADP, PIDA | unchanged | specific_attenuation, path_integrated_attenuation, specific_differential_attenuation, path_integrated_differential_attenuation |
| REFC | `<REF>_CORR`, for example DBZH_CORR | corrected_reflectivity |
| ZDRC | ZDR_CORR | corrected_differential_reflectivity |
| RATE (hybrid) | RR | radar_estimated_rain_rate |
| RATE_Z, RATE_KDP | RR_Z, RR_KDP | — |
| LWC, HKE, CDR | unchanged | —, —, circular_depolarization_ratio |
| L_RHO | RHOHV_LOG | logarithmic_cross_correlation_ratio |
| REF_TEX, VEL_TEX, SW_TEX, ZDR_TEX, RHO_TEX, PHI_TEX, KDP_TEX | `<BASE>_TEX` | reflectivity_texture, —, —, differential_reflectivity_texture, cross_correlation_ratio_texture, differential_phase_texture, — |
| REF_GRAD_R, VEL_GRAD_R | `<BASE>_GRAD_R` | — |
| MET_QI, MET_MASK, TDS_SCORE, HAIL_SCORE, TURB | unchanged | — |
| dealiased velocity (correct crate) | VRADDH | corrected_velocity |

Volume products (LLCREF, EDEPTH, HMAX) and temporal products (DIFF, TREND, ACCUM, PROB) are
not polar dataset variables. They keep their ids until they get their own model.

---

## 9. Per-ray instrument variables (F.1 item 5)

```rust
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RayVariables {
    pub nyquist_velocity_mps: Option<Vec<f32>>,   // nyquist_velocity(time), m/s, NaN = missing
    pub unambiguous_range_m: Option<Vec<f32>>,    // unambiguous_range(time), m
    pub prt_s: Option<Vec<f32>>,                  // prt(time), s
    pub prt_ratio: Option<Vec<f32>>,              // prt_ratio(time)
    pub prt_sequence_s: Option<PrtSequence>,      // prt_sequence(time, prt)
    pub n_samples: Option<Vec<i32>>,              // n_samples(time), -9999 = missing
    pub pulse_width_s: Option<Vec<f32>>,          // pulse_width(time), s
    pub scan_rate_deg_per_s: Option<Vec<f32>>,    // scan_rate(time)
    pub antenna_transition: Option<Vec<u8>>,      // antenna_transition(time), 0/1
    pub calib_index: Option<Vec<i16>>,            // calib_index(time)
    pub rx_range_resolution_m: Option<Vec<f32>>,  // rx_range_resolution(time)
    /// Not FM301: effective independent samples (legacy RayInstrumentMetadata).
    pub independent_samples: Option<Vec<f32>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PrtSequence { pub nprt: u32, pub values_s: Vec<f32> } // row-major [time × prt]
```

`None` means the source did not provide the variable, and the view omits it (WMO-CF.5.3.9:
no meaning may be inferred from absence). A vector that is present always has `nrays` entries.

| FM301 variable | NEXRAD (Msg 31 / Msg 1) | ODIM | CfRadial | xradar 0.12 | Py-ART |
|---|---|---|---|---|---|
| nyquist_velocity | RAD block / Msg 1 header, 0.01 m/s units | `how/NI`, a scalar broadcast to rays | variable | NEXRAD: not present; ODIM: scalar with dims `()`; CfRadial: `(azimuth)` | `instrument_parameters.nyquist_velocity`, per ray, all formats; 0 on NEXRAD Msg 1 surveillance rays |
| unambiguous_range | RAD block / Msg 1 header, 0.1 km units → m | — | variable | NEXRAD: not present; CfRadial: `(azimuth)` | `instrument_parameters.unambiguous_range`, per ray, m |
| prt, prt_ratio | not provided now; stream A may later derive them from VCP and PRF tables | `how/lowprf`, `how/highprf` (Hz → s) when present | variables | CfRadial `(azimuth)` | `instrument_parameters.prt`, `prt_ratio` |
| n_samples | not per ray; Msg 5 pulse counts go to `NexradMetadata` (A.4) | not mapped | variable (IRENE 32, int32, `_FillValue` -9999) | CfRadial `(azimuth)` | `instrument_parameters.n_samples` |
| pulse_width | Msg 5 short/long is a category, **not** a duration, so it stays in `NexradMetadata` | `how/pulsewidth` (µs → s) | variable (IRENE 5e-7 s) | CfRadial `(azimuth)` | `instrument_parameters.pulse_width` |
| scan_rate | — | `how/rpm` → deg/s | variable | CfRadial `(azimuth)` | `scan_rate` |
| antenna_transition | — | — | variable (int8) | CfRadial `(azimuth)` | `antenna_transition` |
| calib_index | — | — | `r_calib_index` | CfRadial `r_calib_index (azimuth)` | — |

The existing rule still holds: PRF **codes** from VCP tables never become a physical PRT. The
legacy `ScanLegMetadata` codes move to `ScanDefinition::legs`.

CfRadial 1 carries some per-ray variables that are not FM301. `ray_start_range` and
`ray_gate_spacing` are checked against `range`, and a mismatch is a decode error.
`measured_transmit_power_h` and `measured_transmit_power_v` go to `Monitoring` as
`radar_measured_transmit_power_h/v`.

---

## 10. `sweep_mode`, `follow_mode`, `prt_mode`, `polarization_mode` (F.1 item 6)

| Source | `sweep_mode` | Evidence |
|---|---|---|
| NEXRAD Level II and III | `AzimuthSurveillance` | xradar and Py-ART both give `azimuth_surveillance` for every sweep (A.2, A.3) |
| ODIM PVOL / SCAN | `AzimuthSurveillance`, including the 90° birdbath sweep | xradar and Py-ART both do this (iesha `sweep_9`, fixed angle 90). ODIM PVOL has no scan-mode attribute, and inferring vertical pointing from the angle would diverge from xradar |
| CfRadial 1/2 | parsed from `sweep_mode`; unknown strings become `Other` | IRENE `azimuth_surveillance`, DOW8 `rhi` |
| DORADE | RADD scan mode: 8 SUR → `AzimuthSurveillance`; 1 PPI → `Sector`; 3 RHI → `Rhi`; 4 VER → `VerticalPointing`; 2 COP → `Coplane`; 7 IDL → `Idle`; 5 TAR → `Pointing`; 6 MAN → `ManualPpi`; 0 CAL → `Other("calibration")`; 9 AIR, 10 HOR → `Other` | DOW8's CfRadial file, written from DORADE by Radx's `DoradeRadxFile`, says `rhi`; the corpus DOW6 DORADE RHI has RADD scan mode 3. The PPI and MAN mappings are proposed and must be checked in F.3 against a Radx conversion of the NOXP sweeps (section 15) |
| JMA | `AzimuthSurveillance` | PPI volumes |

The legacy `VolumeMetadata::scan_mode` (one per volume) maps to every sweep: `Ppi` →
`AzimuthSurveillance`, `Rhi` → `Rhi`, `VerticalPointing` → `VerticalPointing`, `Other` →
`Other("other")`. The reverse conversion takes the mode all sweeps share, or `Other` if they
differ, which is today's `combined_scan_mode` rule.

- **`follow_mode`:** fixed ground radars (NEXRAD, ODIM, JMA) set `Some(FollowMode::None)`,
  which is the correct Table 301-15 value. CfRadial is parsed. xradar writes `"not_set"` for
  NEXRAD and ODIM, which is not a Table 301-15 value; the Xradar flavor writes `"not_set"`
  only when the field is `None`.
- **`prt_mode`:** `None` unless the source states it (CfRadial `fixed`, `staggered` or
  `dual`). Both flavors write `"not_set"` for `None` (open question, section 15).
- **`polarization_mode`:** CfRadial `polarization_mode`; ODIM `how/polmode`; NEXRAD `HvSim`
  when dual-pol moments are present (simultaneous H/V transmit), else `Horizontal`.

---

## 11. Global attributes and root variables (F.1 item 7)

| FM301 | Rust | NEXRAD | ODIM | CfRadial | xradar 0.12 observed | Py-ART `metadata` |
|---|---|---|---|---|---|---|
| `Conventions` | view constant | WMO flavor: "CF-1.8, WMO CF-1.0". Xradar flavor: `provenance.source_conventions` or "None" | same rule | same rule | NEXRAD "None"; ODIM "ODIM_H5/V2_2"; CfRadial "CF-1.6" | "CF/Radial instrument_parameters" |
| `wmo__cf_profile` | view constant | "FM 301-2022" (WMO flavor only) | same | same | not present | not present |
| `title`, `institution`, `references`, `source`, `history`, `comment` | `attrs.*` | `source` = "NEXRAD Level II" | `what/source` goes to `source` | verbatim | "None" placeholders; comment "im/exported using xradar" | "" placeholders; NEXRAD `original_container` "NEXRAD Level II" |
| `instrument_name` | `attrs.instrument_name` | ICAO from the volume header | NOD, else RAD, else WMO | verbatim | "KTLX"; ODIM "None" | "KTLX"; 1999 file "\x00\x00\x00\x00"; ODIM "" |
| `site_name` | `attrs.site_name` | — | PLC | verbatim | CfRadial "CPOLRVP" | CfRadial present |
| `scan_name`, `scan_id` | `scan.name`, `scan.id` | "VCP-212", 212 | `how/task` if present | verbatim ("IRENE_WINDS", "0") | NEXRAD "VCP-212"; 1999 and 2005 files "VCP-0" | `vcp_pattern` "212" |
| `platform_is_mobile` | `attrs.platform_is_mobile` | false | false | verbatim | CfRadial "false" | |
| `ray_times_increase` | `attrs.ray_times_increase` | computed | computed | verbatim | CfRadial "true" | |
| `simulated` | `attrs.simulated` | false | false | verbatim or BowEcho export | | |
| `wmo__wsi`, `wmo__id` | `attrs.wmo` | — | WIGOS / WMO from `what/source` | verbatim | | |
| `/volume_number` | `volume_number` | none (the view writes 0, as xradar does) | none | verbatim | 0; CfRadial 395 | |
| `/time_coverage_start`, `/time_coverage_end` | `time_coverage` | first and last ray, "YYYY-MM-DDThh:mm:ssZ" | same | verbatim | strings floored to the second, "2024-03-15T00:02:17Z" / "…T00:08:18Z" | `time.units` "seconds since 2024-03-15T00:02:17Z" |
| `/latitude`, `/longitude`, `/altitude` | `location` | VOL block; Msg 1 has none (`None`, written as `_FillValue`) | `where/lat`, `lon`, `height` | verbatim | root coordinates, float64 (NEXRAD altitude int64 389); KLIX 2005 Msg 1 gives 0 | length-1 arrays; Msg 1 gives 0.0 |
| `/platform_type`, `/instrument_type` | enums | fixed, radar | fixed, radar | verbatim | "fixed", "radar" | |
| `/altitude_agl`, `/primary_axis`, `/status_str` | fields | — | — | verbatim | CfRadial present | CfRadial present |

xradar also writes NEXRAD-specific root attributes: `dynamic_scan_type`, `mpda_vcp`,
`base_tilt_vcp`, `num_base_tilts`, `vcp_truncated`, `vcp_sequence_active`,
`number_elevation_cuts`, `doppler_velocity_resolution`, `vcp_pulse_width`, `avset_enabled`,
`ebc_enabled`, `super_res_status`, `rda_build_number`, `operational_mode` and
`actual_elevation_cuts`. It also writes sweep attributes: `waveform_type`, `channel_config`,
`super_resolution`, `sails_cut`, `sails_sequence_number`, `mrle_cut`, `mrle_sequence_number`,
`mpda_cut` and `base_tilt_cut`. All of these come from `NexradMetadata` through
`fm301::ExtraAttrs`, not from `Volume`.

For Message 1 files the Rust model keeps the NEXRAD location as `None`. It does not write 0
the way xradar and Py-ART do, because 0°N 0°E is a wrong position rather than a missing one.
The view writes the CF `_FillValue`. Py-ART's `station=` option (look the ICAO up in a table)
belongs in `recast-radar-data`'s site catalog.

---

## 12. FM301 view (binding and conformance boundary)

```rust
// crates/recast-radar-core/src/fm301.rs

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// Reproduces xradar 0.12 names and attribute strings (`sweep_fixed_angle`, "not_set",
    /// "meters per seconds"), with `time` as the ray dimension. Used by F.4 conformance and
    /// the DataTree binding.
    Xradar012,
    /// The FM301-2022 text: `fixed_angle`, `Conventions`, `wmo__cf_profile`, UDUNITS units.
    /// Used by a future CfRadial 2 writer.
    Wmo2022,
}

pub struct VolumeView<'a> {
    pub root: Group<'a>,
    pub sweeps: Vec<Group<'a>>,
    pub radar_parameters: Option<Group<'a>>,
    pub radar_calibration: Option<Group<'a>>,
    pub georeferencing_correction: Option<Group<'a>>,
}

pub struct Group<'a> {
    pub path: String,                     // "/", "/sweep_0", ...
    pub dims: Vec<(&'static str, usize)>, // ("time", 720), ("range", 1832), ("frequency", 1)
    pub variables: Vec<Variable<'a>>,
    pub attrs: Vec<(Cow<'static, str>, AttrValue)>,
}

pub struct Variable<'a> {
    pub name: Cow<'a, str>,
    pub dims: Vec<&'static str>,
    pub values: Values<'a>,
    /// Includes the CF encoding attributes (`scale_factor`, `add_offset`, `_FillValue`, ...).
    pub attrs: Vec<(Cow<'static, str>, AttrValue)>,
}

pub enum Values<'a> {
    /// Contiguous, same shape as the variable: zero-copy.
    Borrowed(ArrayRef<'a>),
    /// A field stored natively, read by padding with `fill` and repeating by `stride`.
    Mapped { native: ArrayRef<'a>, nrays: usize, native_gates: usize,
             mapping: GateMapping, out_gates: usize, fill: Scalar },
    /// Small computed arrays (range centres, per-sweep scalars broadcast to rays).
    Owned(ArrayBuf),
    Scalar(Scalar),
    Text(Cow<'a, str>),
}

pub enum ArrayRef<'a> { U8(&'a [u8]), U16(&'a [u16]), I8(&'a [i8]), I16(&'a [i16]),
                        I32(&'a [i32]), F32(&'a [f32]), F64(&'a [f64]) }
pub enum ArrayBuf { I32(Vec<i32>), F32(Vec<f32>), F64(Vec<f64>), Text(Vec<String>) }
pub enum Scalar { U8(u8), U16(u16), I8(i8), I16(i16), I32(i32), F32(f32), F64(f64) }

pub trait ExtraAttrs {
    fn root_attrs(&self, flavor: Flavor) -> Vec<(Cow<'static, str>, AttrValue)>;
    fn sweep_attrs(&self, sweep: usize, flavor: Flavor) -> Vec<(Cow<'static, str>, AttrValue)>;
}

pub fn volume_view<'a>(volume: &'a Volume, flavor: Flavor, extra: Option<&'a dyn ExtraAttrs>)
    -> VolumeView<'a>;
```

Fields are always written encoded: integers plus CF attributes, or `float32` with
`_FillValue = NaN` for `FieldData::F32`. xarray or the binding does the decoding, which is
spec 4.3's "let xarray's CF decoding apply the packing". Rays are written in storage order,
under dimension `time`.

---

## 13. Compatibility shim (F.1 item 8)

### 13.1 Why there are no type aliases

The plan asks for "type aliases and deprecated accessors for old names". A `pub type Old =
New` alias keeps a type name but not the old fields, variants or struct-literal syntax, and
un-migrated code relies on exactly those. Counts on `fm301` at `f73ce2a`:

- **Field accesses:** 1,561 sites in 77 files use `.cuts`, `.radials`, `.moments`,
  `.storage`, `.gate_range`, `.elevation_deg`, `.azimuth_deg`, `.time_offset_ms`,
  `.nyquist_velocity_mps`, `.radial_status`, `.radial_indices`, `.elevation_number` or
  `.ray_instrument_metadata`. That includes 132 in `recast-radar-core`'s own `lib.rs`.
- **Struct literals outside core:** `GateRange {` 46, `MomentGrid {` 45, `Radial {` 38,
  `RadarVolume {` 5, `RadarSite {` 4, others 3.
- **Exhaustive matches:** on `MomentStorage`, which has 3 variants against `FieldData`'s 5
  variants with payloads; and on `MomentType` and `ScanMode` variants whose names differ
  (`Reflectivity` vs `Dbzh`, `Ppi` vs `AzimuthSurveillance`).

No alias of a new type under an old name would compile against that code, so there are no
aliases. The old names remain the **old types**.

### 13.2 Structure

```rust
// crates/recast-radar-core/src/legacy.rs
//! Pre-FM301 model, moved verbatim from lib.rs @ 1989a03. Deleted at the end of F.3.

// Every public item keeps its definition and impls unchanged and gains:
#[cfg_attr(recast_legacy_deprecation, deprecated(note = "FM301 migration: see docs/design/fm301-model.md section 5"))]
pub struct RadarVolume { /* unchanged */ }
// Likewise: RadarSite, ElevationCut, Radial, GateRange, RadialStatus, MomentType, MomentGrid,
// MomentStorage, MomentRow, MomentGridError, VcpInfo, ScanLegMetadata, ScanMode,
// VolumeMetadata, RayInstrumentMetadata, RayInstrumentMetadataAlignmentError, ProductId,
// MergeReport, merge_radar_volumes, canonical_moment, CUT_ELEVATION_MATCH_TOLERANCE_DEG.

/// Legacy values whose meaning depends on the decoder that wrote them. Only
/// `TryFrom<RadarVolume> for Volume` fills this; it is what makes the reverse conversion exact.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SweepResidue {
    /// `Radial::time_offset_ms` as written: ms of day (NEXRAD), ms since volume start
    /// (CfRadial), ms since sweep start (DORADE), 0 (ODIM, JMA).
    pub time_offset_ms: Vec<i32>,
    /// `Radial::gate_range`, per radial.
    pub radial_gate_ranges: Vec<GateRange>,
    pub radial_status: Vec<Option<RadialStatus>>,
    /// `MomentGrid::gate_range` per field, in `Sweep::fields` order. Preserves each decoder's
    /// start-vs-centre convention and metre rounding (6.6).
    pub grid_gate_ranges: Vec<GateRange>,
    /// `MomentGrid::radial_indices` of grids whose rows were not the identity
    /// (the conversion scattered them into fill rows).
    pub sparse_rows: Vec<(usize, Vec<usize>)>,
}

#[derive(Debug, thiserror::Error)]
pub enum LegacyConversionError {
    #[error("sweep {sweep}: field {field} gates do not align with the sweep range")]
    UnalignedGates { sweep: usize, field: String },
    #[error("sweep {sweep}: explicit (non-uniform) range has no legacy form")]
    ExplicitRange { sweep: usize },
    #[error("sweep {sweep}: {detail}")]
    Shape { sweep: usize, detail: String },
}

// Whole-volume conversions move every U8/U16/F32 buffer; no gate data is copied.
impl TryFrom<RadarVolume> for Volume { type Error = LegacyConversionError; /* ... */ }
// I8/I16 fields expand to legacy F32. That is the only copy, and today's CfRadial decoder
// already expands.
impl TryFrom<Volume> for RadarVolume { type Error = LegacyConversionError; /* ... */ }

// Borrowed conversions for the legacy-signature wrappers in migrated crates. These clone.
pub fn sweep_from_cut(cut: &ElevationCut, meta: &VolumeMetadata, number: u32)
    -> Result<Sweep, LegacyConversionError>;
pub fn cut_from_sweep(sweep: &Sweep, format: SourceFormat)
    -> Result<ElevationCut, LegacyConversionError>;
pub fn field_from_grid(grid: &MomentGrid, range: &RangeCoord, format: SourceFormat)
    -> Result<Field, LegacyConversionError>;
pub fn grid_from_field(field: &Field, range: &RangeCoord, format: SourceFormat) -> MomentGrid;

impl MomentType { pub fn to_field_name(&self) -> FieldName; }     // Reflectivity -> Dbzh; Unknown("CFP") -> Ccorh
impl FieldName  { pub fn to_legacy_moment(&self) -> MomentType; } // via Quantity; Other(s) -> Unknown(s)
```

```rust
// crates/recast-radar-core/src/lib.rs during F.2 and F.3
pub mod legacy;
pub mod model;
pub mod fm301;
pub use legacy::*; // every old name at its old path, so un-migrated `use` lines compile unchanged
pub use model::{Volume, Sweep, Field, FieldName, FieldData, Quantity, Polarization, GateMapping,
                RangeCoord, Rays, RayVariables, SweepMode, /* ... */};
```

An additive edit to the root `Cargo.toml` stops the opt-in cfg from triggering warnings:

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
unexpected_cfgs = { level = "warn", check-cfg = ["cfg(recast_legacy_deprecation)"] }
```

Deprecation is opt-in because an unconditional `#[deprecated]` would put roughly 1,500
warnings into crates owned by streams A, D, E and G. That would break the "do not add new
warnings" rule and G.2's planned `clippy -D warnings`. A migrator runs
`RUSTFLAGS="--cfg recast_legacy_deprecation" cargo check -p <crate>` to list the legacy uses
left in that crate.

### 13.3 Deprecated accessors in migrated crates

When a crate's public function migrates, its old signature stays during F.3 as a thin
wrapper:

```rust
// Example: recast-radar-correct after fm301-algo migrates
pub fn dealias_sweep(sweep: &Sweep, opts: &DealiasOptions) -> Result<Field, DealiasError>;

#[cfg_attr(recast_legacy_deprecation, deprecated(note = "use dealias_sweep"))]
pub fn dealias_cut(cut: &ElevationCut, meta: &VolumeMetadata, opts: &DealiasOptions)
    -> Result<MomentGrid, DealiasError> {
    let sweep = legacy::sweep_from_cut(cut, meta, 0)?;
    let field = dealias_sweep(&sweep, opts)?;
    Ok(legacy::grid_from_field(&field, &sweep.range, SourceFormat::Unknown))
}
```

Decoders follow the same pattern:

- `read_volume(..) -> Volume` is the native decoder.
- `decode_volume_from_bytes(..) -> RadarVolume` stays, implemented as
  `RadarVolume::try_from(read_volume(..)?)`.
- A natively decoded volume has no residue, so `TryFrom<Volume>` derives the legacy values
  from each decoder's legacy convention: the 6.6 table for gate ranges; NEXRAD
  `time_offset_ms` = ms of day of the collection time; `Radial::gate_range` = the first
  field's native geometry; `radial_status` from `NexradMetadata`'s per-ray status, which
  io-nexrad keeps for the duration of the shim.

Each io crate's existing real-file tests keep passing through these wrappers, which shows the
native decoders reproduce legacy output.

### 13.4 Acceptance and removal

**F.2**

- `legacy.rs` holds the old code byte-identical, as a `git diff -M` rename.
- For every real corpus file that `recast-radar-io` can decode,
  `RadarVolume::try_from(Volume::try_from(v)) == v` under `PartialEq`, except that
  `unambiguous_range_km` is compared to 1e-6 relative because it goes km → m → km.
- All crates compile unchanged, `cargo test --workspace` passes, and checksums are identical,
  since nothing uses the new path yet.

**F.3**

- Migrated crates use only `model::*`:
  `RUSTFLAGS="--cfg recast_legacy_deprecation -D deprecated" cargo check -p <crate>` passes.
  The legacy wrappers stay.
- Checksums stay identical after fm301-render and bench move to `Volume`.
- Single-core decode time is measured on the native `read_volume`, before and after.

**Removal (the last F.3 step)**

- Delete `legacy.rs`, `Sweep::legacy`, the wrappers and the `unexpected_cfgs` entry.
- Rename `read_volume*` to the final names.
- `grep -rn "legacy::\|RadarVolume\|MomentGrid\|ElevationCut" crates` finds nothing.

---

## 14. Where xradar 0.12, FM301-2022 and Py-ART disagree, and what this model does

| Topic | FM301-2022 text | xradar 0.12 | Py-ART 2.2.5 | Model / view |
|---|---|---|---|---|
| Ray dimension | `time` is primary | `azimuth` (PPI) or `elevation` (RHI), rays sorted; `time` with `first_dim="time"` | a 1-D ray index across the volume | `time`, storage order, in both flavors |
| Fixed angle variable | `fixed_angle` | `sweep_fixed_angle` | `fixed_angle` (volume array) | Xradar flavor: `sweep_fixed_angle`; WMO flavor: `fixed_angle` |
| `range` units and meaning | "metres"; the attribute is named `meters_to_center_of_first_gate` but described as "range to start of first gate" | "meters", centre (ODIM 750 = rstart 500 + 250) | "meters", centre in the data; the ODIM reader's attribute says 0.0 while the data starts at 250 | centres; "meters" in the Xradar flavor, "metres" in the WMO flavor |
| azimuth / elevation `standard_name` | `sensor_to_target_azimuth_angle` / `sensor_to_target_elevation_angle` | `ray_azimuth_angle` / `ray_elevation_angle` | `beam_azimuth_angle` / `beam_elevation_angle` | per flavor |
| Unknown `follow_mode` / `prt_mode` | no "unknown" value | "not_set" | CfRadial passthrough | "not_set" when `None` |
| NEXRAD sentinels | `_FillValue` and `valid_range` mandatory (WMO-CF.5.2.14, 5.2.15) | no `_FillValue` | masks raw <= 1 | `_FillValue = 0`, range-folded flag, `valid_range` |
| Field `coordinates` attribute | "elevation azimuth range" | "elevation azimuth range latitude longitude altitude time" | "elevation azimuth range" (NEXRAD); "time range" (CfRadial) | per flavor |
| Different gate geometry within a sweep | one `range` | first moment's start and spacing, maximum count; misplaces moments | resampled to one volume range | section 6 |
| Different range lengths across sweeps | per-sweep `range` allowed | per sweep | one volume range | per sweep; a Py-ART export pads to the volume maximum |
| `wmo__parameter_uri` / `wmo__parameter_name` | mandatory on data variables (WMO-CF.5.2.9) | not present | not present | omitted; no registry entries identified (section 15) |
| ODIM TH | linear total power (Table 301-9) | labelled linear, unitless | ODIM reader: `total_power_horizontal` | name TH; attributes in dBZ per ODIM |

---

## 15. Deviations from spec and plan; open questions for the reviewer

**Deviations** (each justified in the section cited):

1. **Where format metadata lives.** Spec 4.2 puts "format-specific metadata in typed
   extension structs" inside `Volume`. Here the structs are typed but sit beside `Volume`
   (section 2), so that `core` does not depend on `io-*` crates (spec 4.1 dependency rule).
   This is consistent with plan A.4.
2. **Storage types.** Spec 4.2 lists `u8`, `u16` or `f32`. `I8` and `I16` are added so that
   CfRadial `byte`/`short` packed data is not expanded (IRENE, DOW8; 7.3).
3. **Canonical names.** Spec 4.2's "canonical names are the FM301/xradar short names" is read
   as "whatever xradar names it", which means verbatim names for ODIM, CfRadial and DORADE
   (8.1).
4. **Type aliases.** Plan F.1 asks for them; there are none, because aliases cannot keep
   legacy field syntax compiling (13.1). Deprecation is gated behind a `cfg`.
5. **Range folded.** Spec 4.2's "raw 1 is the range-folded flag" becomes CF `flag_values`,
   which CF decoding does not mask. The binding masks it by default (7.1).

**Open questions**

1. **`prt_mode` when unknown.** Table 301-15 has no "unknown" value. Should the view write
   "not_set" (as xradar does) or omit the mandatory variable?
2. **`wmo__parameter_uri` and `wmo__parameter_name`.** WMO-CF.5.2.9 makes them mandatory. No
   codes.wmo.int entries for radar moments were found in the Manual text. Omit them, or write
   a placeholder?
3. **16-bit NEXRAD `valid_range`.** xradar masks PHI with 0x3FF and ZDR with 0x7FF. Does ICD
   2620002 define those masks (stream A)? If it does, should `IntCoding` carry a bit mask?
4. **Message 1 spectrum-width offset.** The ICD, MetPy and the current decoder use 129;
   xradar's code uses 192 (and xradar emits no SW). Stream A should confirm against the ICD
   text.
5. **Range precision.** `RangeCoord` stores f64 centres and xradar writes float32; the Xradar
   flavor writes float32. Please confirm the conformance tolerance (1e-4 relative is ample).
6. **Per-sweep range in a Py-ART export.** Refinement leaves a Message 1 REF-only sweep at
   1 km (matching xradar) but puts a mixed sweep at 250 m, whereas Py-ART puts every sweep at
   250 m. Should a Py-ART-style export pad each sweep to the volume's finest range?
7. **DORADE PPI and MAN mode mapping (10).** Please verify against RadxConvert output for the
   NOXP sweeps.

---

## Appendix A. Observed structure on real files

### A.1 Environment and files

- Software: xradar 0.12.0, arm_pyart 2.2.5, metpy 1.7.1, xarray 2026.7.0, h5py 3.16.0,
  netCDF4 1.7.4, h5netcdf 1.8.1 (venv `radrs-cmp`).
- Run: 2026-09-16, on Windows.
- Scripts, committed in `tools/fm301_probe/`:
  - `probe_xradar.py <nexrad|odim|cfradial1> <file> [detail_sweeps]`
  - `probe_pyart.py <nexrad|odim|cfradial1> <file>`
  - `metpy_gates.py <level2 file>`

| Label | File | Corpus id |
|---|---|---|
| modern dual-pol | `KTLX20240315_000217_V06` (unidata-nexrad-level2, 2024/03/15/KTLX) | `l2-ktlx-20240315-000217` (testdata stream's level2 manifest, in progress) |
| Message 1 | `KTLX19990504_002218.gz` | `l2-ktlx-19990504-002218` |
| Message 1 with metadata | `KLIX20050829_130035.gz` | `l2-klix-20050829-130035` |
| Msg 31, legacy resolution | `KPAH20080415_235014_V04.gz` | `l2-kpah-20080415-235014` |
| ODIM PVOL, dual-pol | `testdata/files/other/odim/dkrom.pvol.20260820T1130.dualpol.h5` | `odim-dkrom-20260820-1130-pvol` |
| ODIM PVOL, gate tiers + 90° | `testdata/files/other/odim/iesha.pvol.20260305T0115.dbzh_th_vradh.h5` | `odim-iesha-20260305-0115-pvol` |
| CfRadial 1 PPI | `testdata/files/other/cfradial/cfrad.20110827_120420.760_CPOLRVP_IRENE_WINDS_SUR.sweeps0-1_DBZ_VEL.nc` | `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` |
| CfRadial 1 RHI | `testdata/files/other/cfradial/cfrad.20211011_223602_DOW8_RHI.trim3.nc` | `cfrad1-dow8-20211011-223602-rhi-trim3-classic` |

sha256 of each file, in table order:

```
366e63315c7f541cadbac3e800dd5a0bd4880865f103e79db654a93c3ac09856  KTLX20240315_000217_V06
1d94cbb7680e2ba2e888b098631762cc024924d1e04a0ca3465333dab761bf65  KTLX19990504_002218.gz
af4f106ef886e17cc809802b1f753329744842bf62727cf9082a2a1469b80d14  KLIX20050829_130035.gz
788b1956e9cb47c0d665e49d52b6657a55196296213e36d0cf928d019a8852bf  KPAH20080415_235014_V04.gz
e3fafbdadc0e270c379845b29beb47af1278986f446823c3d9dbc420f8c78ce8  dkrom.pvol.20260820T1130.dualpol.h5
a4bce7f0dcd8339d74904eb26f9514efff8f214e93a85ff440449230d8b5b5fe  iesha.pvol.20260305T0115.dbzh_th_vradh.h5
6c9d57caf330a1fdfe7366b6440204523853089342769c3b7d789a7edd89feb9  cfrad...IRENE_WINDS_SUR.sweeps0-1_DBZ_VEL.nc
1297315af9320dc7863d7b6843388300aa9d87386e873644487ca5f2be350e0a  cfrad.20211011_223602_DOW8_RHI.trim3.nc
```

### A.2 NEXRAD modern dual-pol: KTLX 2024-03-15 00:02 (VCP 212, SAILS ×3, Build 22.0)

**xradar `open_nexradlevel2_datatree(path)`**

Tree:

- `/` and `/sweep_0` .. `/sweep_19`.
- With `optional_groups=True`, `/radar_parameters`, `/georeferencing_correction` and
  `/radar_calibration` also appear, all empty.

Root:

- Variables: `volume_number` 0, `platform_type` "fixed", `instrument_type` "radar",
  `time_coverage_start` "2024-03-15T00:02:17Z", `time_coverage_end` "2024-03-15T00:08:18Z"
  (U20 strings).
- Coordinates: `latitude` 35.3333625793457 and `longitude` -97.27776336669922 (float64),
  `altitude` 389 (int64).
- **No `sweep_group_name` and no `sweep_fixed_angle`.**
- Attributes:
  - Conventions "None"; version, title, institution, references, source and history "None";
    comment "im/exported using xradar".
  - instrument_name "KTLX", scan_name "VCP-212", dynamic_scan_type "SAILS x 3".
  - mpda_vcp "False", base_tilt_vcp "False", num_base_tilts "0", vcp_truncated "False",
    vcp_sequence_active "False".
  - number_elevation_cuts "23", actual_elevation_cuts "20", doppler_velocity_resolution
    "0.5", vcp_pulse_width "short".
  - avset_enabled "True", ebc_enabled "True", super_res_status "2", rda_build_number "2200",
    operational_mode "4".

Sweep group:

- Dimensions `azimuth` and `range`.
- `azimuth(azimuth)`: float64 {standard_name ray_azimuth_angle, long_name
  azimuth_angle_from_true_north, units degrees, axis radial_azimuth_coordinate}, **sorted
  ascending** (0.1895 .. 359.7089); `time` is therefore not monotonic.
- `elevation(azimuth)`: float64.
- `time(azimuth)`: datetime64[ns], encoded as float64 "milliseconds since
  1970-01-01T00:00:00Z".
- `range(range)`: float32 {units meters, standard_name projection_range_coordinate,
  long_name range_to_measurement_volume, axis radial_range_coordinate,
  meters_between_gates 250.0, spacing_is_constant "true",
  meters_to_center_of_first_gate 2125.0}.
- Variables: the fields, plus `sweep_mode` "azimuth_surveillance", `sweep_number`,
  `prt_mode` "not_set", `follow_mode` "not_set", and `sweep_fixed_angle` (float64, from
  Message 5). No `nyquist_velocity` and no `unambiguous_range`.
- Attributes, sweep_0 / sweep_1: waveform_type "contiguous_surveillance" /
  "contiguous_doppler", channel_config "sz2_phase_coding", super_resolution "11" / "7",
  sails_cut "False", sails_sequence_number "0", mrle_cut "False", mrle_sequence_number "0",
  mpda_cut "False", base_tilt_cut "False".

Fields:

- Attributes standard_name, long_name and units come from `sweep_vars_mapping`.
- Encoding: `scale_factor`, `add_offset`, dtype (uint8 or >u2). **No `_FillValue`.**
- Decoded as float64:
  - sweep_0 DBZH has 1,036,042 gates at exactly -33.0 (below threshold) and no NaN.
  - sweep_10 VRADH has -64.5 in every gate from 1192 to 1535 (raw padding 0).

With `first_dim="time"`: dimensions (`time`, `range`), and azimuth[0:2] = 167.2723, 167.7475.
That is acquisition order, the same as Py-ART and the current decoder.

| sweep | rays | `sweep_fixed_angle` | xradar `range` size | xradar fields | MetPy native gates (all start 2.125 km, 250 m) |
|---|---|---|---|---|---|
| 0, 4, 8, 14 | 720 | 0.4834 | 1832 | DBZH ZDR PHIDP RHOHV CCORH | REF 1832, CFP 1832, ZDR/PHI/RHO 1192 |
| 2 | 720 | 0.8789 | 1832 | same | same |
| 6 | 720 | 1.3184 | 1712 | same | REF/CFP 1712, dual-pol 1192 |
| 1, 5, 9, 15 | 720 | 0.4834 | 1192 | DBZH VRADH WRADH | REF/VEL/SW 1192 |
| 3, 7 | 720 | 0.8789, 1.3184 | 1192 | DBZH VRADH WRADH | 1192 |
| 10 | 360 | 1.8018 | 1536 | DBZH VRADH WRADH ZDR PHIDP RHOHV CCORH | REF/CFP 1536, others 1192 |
| 11 | 360 | 2.4170 | 1336 | all 7 | REF/CFP 1336, others 1192 |
| 12, 13, 16, 17, 18, 19 | 360 | 3.1201, 3.9990, 5.0977, 6.4160, 7.9980, 10.0195 | 1160, 984, 820, 680, 540, 456 | all 7 | all equal to the range size |

Word sizes and ICD scale/offset: REF u8 2/66; VEL u8 2/129; SW u8 2/129; ZDR u16 32/418; PHI
u16 2.8361001/2; RHO u8 300/-60.5; CFP u8 1/8.

**Py-ART `read_nexrad_archive(path)`**

- nrays 11520, ngates 1832, nsweeps 20.
- `range` 2125..459875 m (1832 gates; meters_to_center_of_first_gate 2125,
  meters_between_gates 250).
- `time` in "seconds since 2024-03-15T00:02:17Z", first value 0.182 s. `azimuth` in
  acquisition order (first value 167.2723).
- `fixed_angle` is float32 and matches xradar. `sweep_mode` is b"azimuth_surveillance".
- Metadata: Conventions "CF/Radial instrument_parameters", version "1.3", instrument_name
  "KTLX", original_container "NEXRAD Level II", vcp_pattern "212".
- `instrument_parameters`: `unambiguous_range` (m, per ray, 467000 .. 127000) and
  `nyquist_velocity` (m/s, per ray, 8.27 .. 30.44).
- Fields, each a float32 MaskedArray (11520, 1832) with `_FillValue` -9999.0 and
  `coordinates` "elevation azimuth range": `reflectivity`, `velocity`, `spectrum_width`,
  `differential_reflectivity`, `differential_phase`, `cross_correlation_ratio`,
  `clutter_filter_power_removed`.
- Unmasked extents in sweep 0: reflectivity 1715, CFP 1692, dual-pol 1192, velocity 0 (the
  moment is absent, so fully masked).
- Masking is `raw <= 1`; values are `(raw - offset) / scale` in float32
  (`pyart/io/nexrad_level2.py`, `get_data`). The masked count in sweep 0, 1,036,042, equals
  xradar's count of -33.0 values.

**Current Rust decoder** (`recast-radar-io` example `dump_radar`, release build)

- 20 cuts of 720 or 360 radials, and **rows == radials for every grid**.
- Moment gate counts equal MetPy's.
- Cut `elevation_deg` is the first radial's elevation (0.582, 0.483, 0.777, 0.923, 0.409,
  ...), not the Message 5 angle.

### A.3 NEXRAD Message 1 and legacy-resolution Message 31

**KTLX 1999-05-04 00:22** (ARCHIVE2.036, NUL ICAO, VCP 11, 16 sweeps)

MetPy:

- Sweep 0: REF at 0 km + 1 km × 460.
- Sweep 1: VEL and SW at -0.375 km + 0.25 km × 920.
- Sweep 2: REF × 356.
- Sweep 3: VEL and SW × 920.
- Sweeps 4..15: REF (1 km, 356 → 70 gates) **and** VEL/SW (250 m from -375 m, 920 → 280
  gates) in the same radials.
- Scale/offset: REF 2/66, VEL 2/129, SW 2/129.

xradar:

- On the `.gz` file: `TypeError: unsupported operand type(s) for +: 'NoneType' and 'int'` in
  `init_next_record`; whole-file gzip is not handled.
- On the decompressed file, opening every sweep raises `ValueError: conflicting sizes for
  dimension 'azimuth': length 366 on 'azimuth' and length 367 on {...VRADH...}`.
- Opening one sweep at a time: sweeps 0, 7, 8, 9, 11, 12, 13 and 14 raise the same
  `ValueError`, and sweep 15 raises `IndexError`. Sweeps 1..6 and 10 open, with wrong content:
  - `sweep_1`: a 356-gate DBZH at 1000 m, fixed angle 0.4834.
  - `sweep_2`: VRADH at 250 m with `meters_to_center_of_first_gate` 65161.0 (-375 read as
    unsigned).
  - `sweep_3` to `sweep_6` and `sweep_10`: DBZH and VRADH on a 1000 m range of 920, 860 or
    440 gates, so VRADH is misplaced ×4.
- No WRADH in any sweep. No `_FillValue`: DBZH minimum -33.0, VRADH minimum -64.5.

Py-ART:

- nrays 5855, ngates 1840, nsweeps 16.
- `range` -375..459375 m at 250 m (meters_to_center_of_first_gate -375).
- instrument_name "\x00\x00\x00\x00"; latitude, longitude and altitude 0.0.
- fixed_angle 0.5, 0.5, 1.5, ... 19.5.
- Fields reflectivity, velocity, spectrum_width.
- Extents: sweep 0 reflectivity 1692 (460 km, upsampled); sweep 1 velocity 888.
- `nyquist_velocity` is 0 on surveillance rays.

Rust:

- 16 cuts. Cut 0: REF 460 × 1000 m from 0. Cut 1: VEL and SW 920 × 250 m from -375. Cuts 4
  onward carry both.
- Site id "", no location.

**KLIX 2005-08-29 13:00** (AR2V0001, metadata messages, VCP 121, 20 sweeps)

MetPy, per sweep:

| Sweep(s) | REF gates (0 m, 1 km) | VEL/SW gates (-375 m, 250 m) |
|---|---|---|
| 0 | 460 | — |
| 1 | — | 920 |
| 2 | 137 | 548 |
| 3 | 175 | 700 |
| 4 | 356 | — |
| 5 | — | 920 |
| 6 | 137 | 548 |
| 7 | 175 | 700 |
| 8 | 356 | 920 |
| 9 | 137 | 548 |
| 10 | 175 | 700 |
| 11 | 268 | 920 |
| 12 | 137 | 548 |
| 13 | 175 | 700 |
| 14 | 216 | 860 |
| 15 | 127 | 508 |
| 16 | 176 | 700 |
| 17 | 110 | 440 |
| 18 | 85 | 340 |
| 19 | 70 | 280 |

xradar (decompressed file; the `.gz` fails as above):

- 20 sweeps.
- sweep_0: `range` 0..459000 at 1000 m (460), DBZH, correct.
- sweep_1: VRADH, `range` 65161..294911 at 250 m (920).
- sweep_2: DBZH and VRADH, `range` 0..547000 at 1000 m (548). DBZH is padded from 137 to 548
  with raw 0, and VRADH is misplaced ×4. Sweeps 3 and 6..19 follow the same pattern.
- No WRADH.
- Root latitude, longitude and altitude are integer 0. scan_name "VCP-0",
  number_elevation_cuts "0", rda_build_number "0".

Py-ART:

- nrays 7252, ngates 1840, `range` -375..459375 m.
- Reflectivity is interpolated (minimum -13.9375).
- With `linear_interp=False`, REF gate i lands at fine gates 4i..4i+3. Sweep 2, ray 0:
  REF[1] = 46.5 fills fine gates 4..7 and REF[2] = 48.0 fills 8..11.
- With `linear_interp=True`, fine gates 4..15 are 46.5, 46.5, 46.6875, 47.0625, 47.4375,
  47.8125, 47.875, 47.625, 47.375, 47.125, 46.8125, 46.4375.

Rust: 20 cuts with grids as MetPy reports (REF 137 × 1000 m from 0, VEL 548 × 250 m from
-375).

**KPAH 2008-04-15 23:50** (AR2V0004, Build 10.0, Message 31 at legacy resolution, VCP 32, 7 sweeps)

MetPy:

- Sweep 0: REF at 0.5 km + 1 km × 460.
- Sweep 1: VEL and SW at 0.125 km + 0.25 km × 920.
- Sweep 2: REF × 406.
- Sweep 3: VEL and SW × 920.
- Sweep 4: REF × 328 with VEL/SW × 920.
- Sweep 5: REF × 271 with VEL/SW × 920.
- Sweep 6: REF × 228 with VEL/SW × 912.

xradar (decompressed file):

- sweep_1: `range` 125..229875 at 250 m.
- sweep_4..6: `range` 500..919500 at 1000 m (920, 920 and 912 gates), holding DBZH, VRADH and
  WRADH. VRADH and WRADH are misplaced ×4, and DBZH is padded.

Py-ART: `range` 125..459875 m (1840 gates). With `linear_interp=False`, REF gate 11
(-20.5 dBZ) lands at fine gates 44..47, the same edge alignment.

Rust: 7 cuts, matching MetPy.

### A.4 ODIM_H5 PVOL

**dkrom (DMI Rømø: 10 sweeps × 8 quantities)**

xradar `open_odim_datatree`:

- Warns 10 times: "Equal ODIM `starttime` and `endtime` values. Can't determine correct sweep
  start-, end- and raytimes."
- Root:
  - Dimension `sweep` = 10.
  - Variables: `volume_number`, `platform_type`, `instrument_type`, `time_coverage_start`
    "2026-08-20T11:30:00Z", `time_coverage_end` "2026-08-20T11:33:13Z",
    `sweep_fixed_angle(sweep)`, and `sweep_group_name(sweep)` as **int64** 0..9.
  - Coordinates: latitude 55.173111, longitude 8.552, altitude 15.0.
  - Attributes: Conventions "ODIM_H5/V2_2", instrument_name "None".
- Each sweep:
  - Dimensions azimuth 360, range 474. `range` 750..237250 at 500 m (centre = rstart 500 m +
    250 m). Azimuths 0.5, 1.5, ...
  - Fields, names verbatim: `DBZH VRAD TH WRAD ZDR RHOHV PHIDP LDR`. Each has attributes
    {`_Undetect` 0.0, standard_name, long_name, units} and encoding {dtype uint8,
    scale_factor, add_offset, `_FillValue` 255.0}.
  - Also: `nyquist_velocity` as a scalar (8.3, dims `()`), `sweep_mode`
    "azimuth_surveillance", `prt_mode` and `follow_mode` "not_set".
- TH is labelled "Linear total power H" with units "unitless", but its values span -32..75.5
  (dBZ). LDR is all NaN.
- With `optional_groups=True`, the `/radar_parameters`, `/georeferencing_correction` and
  `/radar_calibration` groups exist but are empty.

Py-ART `aux_io.read_odim_h5`:

- nrays 3600, ngates 474. `range` data 750..237250, but meters_to_center_of_first_gate 500.0.
- Fields: `reflectivity_horizontal`, `velocity` (from VRAD), `total_power_horizontal`,
  `spectrum_width`, `differential_reflectivity`, `cross_correlation_ratio`,
  `differential_phase`, `linear_polarization_ratio`. `nodata` and `undetect` are masked.
- Metadata: version "H5rad 2.0", source "WMO:06096,RAD:DN42,PLC:Romo,NOD:dkrom",
  original_container "odim_h5", odim_conventions "ODIM_H5/V2_0".
- `instrument_parameters` is empty.

**iesha (Met Éireann Shannon: 10 sweeps, gate tiers 497/350/240/100, top sweep at 90°)**

xradar:

- Per-sweep `range` sizes: 497 for six sweeps, 350 for two, then 240 and 100, all starting
  at centre 250 m with 500 m spacing.
- Fields `DBZH TH VRADH`.
- `sweep_9` (fixed angle 90.0) has `sweep_mode` "azimuth_surveillance".
- Azimuths are measured values: 0.5081, 1.5244, 2.5433.
- Ray times differ per ray: sweep_0 starts 01:19:22.354, and the earliest ray is at row 136.
- `first_dim="time"` rotates the sweep so its first ray is at azimuth 136.51.

Py-ART:

- One volume range 250..248250 (497 gates), yet meters_to_center_of_first_gate is **0.0**.
- Fields `reflectivity_horizontal`, `total_power_horizontal`, `velocity_horizontal`.
- In sweep 9 (100 native gates), gates 100..496 are **NaN but not masked** (mask fraction
  0.0).
- sweep_mode "azimuth_surveillance" for every sweep.

Raw ODIM, read with h5py:

- `dataset10/where`: {a1gate 127, elangle 90.0, nbins 100, nrays 360, rscale 500.0,
  rstart 0.0}.
- `dataset10/data1/what`: {gain 0.5, offset -32.0, nodata 255.0, undetect 0.0, quantity DBZH}.
- `dataset1/how` includes `startazA` and `stopazA` (no `startazT`), plus `NI`, `highprf`,
  `lowprf`, `pulsewidth`, `rpm`, `polmode`, `beamwH`, `beamwV`.
- Root `what`: {object PVOL, source "WMO:03962,NOD:iesha,PLC:Shannon", version "H5rad 2.3"}.

### A.5 CfRadial 1

**IRENE (SMART-R2 C-band, written by Radx as CfRadial 1.3, 2 sweeps)**

xradar `open_cfradial1_datatree`:

- Root dimensions: sweep 2, frequency 1.
- Root variables: `volume_number` (395, encoded int32), `platform_type` b"fixed",
  `primary_axis` b"axis_z", `status_str`, `instrument_type` b"radar",
  `time_coverage_start` and `time_coverage_end` (bytes), `altitude_agl` -65,
  `sweep_group_name(sweep)` (<U9), `sweep_fixed_angle(sweep)` (0.802, 1.4996).
- Root coordinates: latitude 34.73310471, longitude -76.66191101, altitude 0.0,
  `frequency(frequency)` 5.624624e9.
- Root attributes: Conventions "CF-1.6", version "CF-Radial-1.3", title "IRENE-WINDS",
  references "Conversion software: Radx::SigmetRadxFile", source "Sigmet IRIS software",
  instrument_name and site_name "CPOLRVP", scan_name "IRENE_WINDS", scan_id "0",
  platform_is_mobile "false", ray_times_increase "true".
- sweep_0:
  - Dimensions azimuth 360, range 1107, frequency 1.
  - `range` 0..82950 at 75.00000298 m (meters_to_center_of_first_gate 0.0).
  - Variables: `sweep_number`, `sweep_mode` "azimuth_surveillance", `prt_mode` b"fixed",
    `follow_mode` b"none", `sweep_fixed_angle`, `ray_start_range`, `ray_gate_spacing`,
    `pulse_width` (5e-7), `prt` (5.5556e-4), `prt_ratio`, `nyquist_velocity` (47.97),
    `unambiguous_range` (83275.68), `antenna_transition` (int8), `n_samples` (32; int32 with
    `_FillValue` -9999), `r_calib_index`, `measured_transmit_power_h` and `_v`, `scan_rate`,
    `DBZ`, `VEL`.
  - DBZ: encoding int8, scale_factor 0.5, add_offset 32.0, `_FillValue` -128; attributes
    {units dBZ, sampling_ratio 1.0, grid_mapping}.
  - VEL: int8, scale_factor 0.37771654, add_offset 0.0, `_FillValue` -128.
- With `optional_groups=True`: `/radar_parameters` (radar_beam_width_h/v,
  radar_antenna_gain_h/v, radar_receiver_bandwidth), `/georeferencing_correction` (empty),
  and `/radar_calibration` (46 variables, from time, pulse_width and xmit_power_h/v through
  receiver_slope_vx).
- `first_dim="time"` gives DBZ dimensions (`time`, `range`).

Py-ART `read_cfradial`:

- nrays 719, ngates 1107, scan_type "ppi".
- Fields `DBZ` and `VEL`, names verbatim. Attributes keep `_FillValue` -128 and add
  `coordinates` "time range".
- `instrument_parameters`: frequency, follow_mode, pulse_width, prt_mode, prt, prt_ratio,
  polarization_mode, nyquist_velocity, unambiguous_range, n_samples, radar_antenna_gain_h/v,
  radar_beam_width_h/v, radar_rx_bandwidth, measured_transmit_power_h/v.
- `radar_calibration` keys are `r_calib_*`.
- Metadata includes Sub_conventions, original_format "SIGMETRAW", driver "RadxConvert(NCAR)".

**DOW8 RHI (Radx DoradeRadxFile, CF-Radial-1.4, 1 sweep)**

xradar:

- Root dimensions: sweep 1, time 148, frequency 1. `latitude(time)` and `longitude(time)`
  are moving-platform arrays.
- sweep_0:
  - Dimensions **azimuth** 148, not elevation, even though sweep_mode is "rhi" and
    first_dim is "auto"; range 950.
  - `range` starts at 62.456512 m with spacing 124.913025 m. Fixed angle 184.00023.
  - Fields `DBZHC`, `VEL`, `WIDTH`: int16, scale_factor 0.01, `_FillValue` -32768;
    attributes {long_name and standard_name equal to the name, units, sampling_ratio,
    grid_mapping}.
  - Extra variables: `georefs_applied`, `georef_time`, `georef_unit_num`, `georef_unit_id`,
    `altitude_agl`.
- Root attribute comment: "Written by DoradeRadxFile object".

Py-ART: scan_type "rhi", nrays 148, ngates 950, field names verbatim.

### A.6 xarray CF decoding of NEXRAD sentinels (xarray 2026.7.0, h5netcdf)

Input: uint8 `[0, 1, 2, 66, 200, 255]` with scale_factor 0.5 and add_offset -33.0.

| Attributes | `decode_cf` result | Warnings | `to_netcdf` round trip |
|---|---|---|---|
| `_FillValue=0` | NaN, -32.5, -32.0, 0.0, 67.0, 94.5 | none | OK, identical |
| `_FillValue=0`, `missing_value=[0,1]` | NaN, NaN, -32.0, ... | SerializationWarning "multiple fill values ... decoding all values to NaN" (twice) | **ValueError**: conflicting `_FillValue` and `missing_value` |
| `missing_value=[0,1]` | NaN, NaN, -32.0, ... | same warning | **ValueError**: truth value of an array is ambiguous |
| `_FillValue=0`, `_Undetect=0`, `flag_values=[1]`, `flag_meanings="range_folded"` | NaN, -32.5, ... (flags kept as attributes) | none | OK |

### A.7 Py-ART's DataTree adapter

- `pyart.xradar.Xradar(open_nexradlevel2_datatree(KTLX 2024, sweep=[0, 2]))` works: nrays
  1440, ngates 1832.
- `sweep=[0, 1]` or `[0, 1, 10]` raises
  `xarray.structure.alignment.AlignmentError: cannot reindex or align along dimension 'range'
  because of conflicting dimension sizes: {1832, 1192}`, from `_combine_sweeps`'s
  `concat(..., join="outer")`.
- A Py-ART export therefore has to build the volume-level range itself (6.4) instead of going
  through this adapter.

### A.8 Reproduction

```bash
PY=<venv>/Scripts/python.exe
$PY tools/fm301_probe/probe_xradar.py nexrad KTLX20240315_000217_V06 2
$PY tools/fm301_probe/probe_pyart.py  nexrad KTLX20240315_000217_V06
$PY tools/fm301_probe/metpy_gates.py  KLIX20050829_130035.gz
gzip -dc KLIX20050829_130035.gz > KLIX20050829_130035.raw   # xradar 0.12 needs the decompressed file
$PY tools/fm301_probe/probe_xradar.py nexrad KLIX20050829_130035.raw 3
$PY tools/fm301_probe/probe_xradar.py odim testdata/files/other/odim/dkrom.pvol.20260820T1130.dualpol.h5 1
$PY tools/fm301_probe/probe_pyart.py  cfradial1 testdata/files/other/cfradial/cfrad.20211011_223602_DOW8_RHI.trim3.nc
cargo build --release -p recast-radar-io --example dump_radar
target/release/examples/dump_radar KTLX20240315_000217_V06
```
