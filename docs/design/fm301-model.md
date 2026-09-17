# FM301 data model for `recast-radar-core` (wave 2, task F.1)

Status: design note, revised after independent review (plan F.1). Section 16 lists every review
finding and its resolution. Date: 2026-09-16.
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
| Rust type definitions | 2 (`Volume`), 3 (`Sweep`), 4 (`Field`, `FieldName`), 12 (view), 13.2 (shim) |
| Binding strategy (no unsafe, no copy at decode) | 12.2, 12.3 |
| Review findings and resolutions | 16 |

---

## 0. Decisions at a glance

1. **Structure.** `Volume` is the FM301 root group. Each `Sweep` is one `sweep_<n>` group:
   one physical elevation cut, numbered in acquisition order. Split cuts and SAILS or
   MESO-SAILS repeats are separate sweeps, as in xradar, Py-ART and the current decoder
   (A.2). Each `Field` is one dataset variable, and `FieldName` is its variable name.
2. **Rays.** Ray coordinates are struct-of-arrays: `time_s: Vec<f64>`,
   `azimuth_deg: Vec<f32>`, `elevation_deg: Vec<f32>`. The model keeps rays in the source's
   storage order, which is acquisition order for NEXRAD, CfRadial and DORADE and
   azimuth-indexed order for ODIM and JMA. Decoders never reorder rows. The FM301 view orders
   rays through a row permutation, never by moving data. `FirstDim::Time` (the WMO flavor, and
   xradar's `first_dim="time"`) gives acquisition order under dimension `time`, so the `time`
   coordinate is monotonic. `FirstDim::Auto` reproduces xradar's default `first_dim="auto"`:
   rays sorted by angle under dimension `azimuth` or `elevation` (12.1). Each conformance
   comparison uses the same `first_dim` on both sides.
   `time_reference` is whole seconds, as FM301 `units` requires (section 2).
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
   present in the real corpus), `F32`, or `F64` (float64 ODIM planes and CfRadial `double`
   fields, kept unnarrowed). Float values are stored verbatim, including a source fill such as
   -9999, so decode never makes a rewrite pass. Each variant carries its coding: linear
   transform, `_FillValue`, `_Undetect`, range-folded code and `valid_range`. Physical values
   are computed on demand. The NEXRAD transform is still evaluated as `(raw - offset) / scale`
   in f32, so render checksums stay identical. Py-ART evaluates the same float32 expression.
5. **Sentinels.** For NEXRAD, raw 0 (below threshold) is FM301 `_Undetect`: radiated, but no
   valid echo (Table 301-10). It is also the CF `_FillValue`, per the spec, so xarray masks it
   as Py-ART does. The model tells the two meanings apart: `Field::gate` returns `Undetect`
   for raw 0 in a row the source provided and `Missing` for rows it did not. The view pads
   with `_FillValue`. Raw 1 is kept as `flag_values = 1`,
   `flag_meanings = "range_folded"` (Table 301-10). A vector `missing_value = [0, 1]` was
   tested and rejected: xarray warns when decoding it and refuses to write it back (A.6). A
   binding that decodes moves packed-unit attributes (`valid_range`, `flag_*`, `_Undetect`)
   into `.encoding` so they cannot be read as physical values (12.3).
6. **Names.** NEXRAD names follow xradar's mapping: REF→DBZH, VEL→VRADH, SW→WRADH, ZDR→ZDR,
   PHI→PHIDP, RHO→RHOHV, CFP→CCORH. JMA gets FM301 names. ODIM, CfRadial and DORADE keep their
   source names verbatim, which is what xradar returns for ODIM and CfRadial (A.4, A.5). A
   separate `Quantity` class answers lookups such as "the reflectivity field" regardless of
   spelling. Py-ART aliases follow the `pyart.config` defaults, and `PyartNames::Reader` gives
   the names each Py-ART reader produces (8.2).
7. **Format metadata.** NEXRAD RDA status, VCP, clutter maps and similar data live in typed
   structs owned by the format crate. They sit beside `Volume`, as
   `NexradVolume { volume, metadata }` (plan A.4), not inside it, because `core` must not
   depend on `io-*` crates. This departs from the literal wording of spec 4.2 (section 15).
8. **Shim.** `pub type` aliases cannot keep un-migrated code compiling, because that code uses
   legacy struct fields and struct literals: 1,561 field-access sites in 77 files and about 140
   struct literals (section 13.1). Instead:
   - The legacy model types (not geometry, refractivity or `bounded_read`) move unchanged to
     `recast_radar_core::legacy` and are re-exported at their old paths.
   - Conversions in both directions move gate buffers rather than copy them. Values the new
     model cannot hold exactly go into a `LegacyResidue` returned beside the `Volume`, not
     stored inside it, so a legacy → new → legacy round trip is bit-identical.
   - Deprecation warnings are opt-in, behind `--cfg recast_legacy_deprecation`, so other
     streams' `-D warnings` CI is unaffected. Kept legacy wrappers live in
     `#[allow(deprecated)] mod legacy_api`. A migrated crate declares
     `#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]`, which makes the migration
     gate fail only on that crate's own legacy uses (13.4, tested).
   - The legacy module is deleted at the end of F.3.
9. **Bindings without unsafe.** rust-numpy can wrap borrowed Rust memory only through an
   `unsafe fn`, and spec principle 2 forbids that. A binding therefore takes ownership: it
   moves each field's `Vec` into NumPy (`PyArray::from_vec`, safe and zero-copy) after
   recording the FM301 layout. `VolumeView` stays the Rust-side conformance surface (12.2).
10. **Passthrough.** Global attributes, sweep attributes, per-ray variables, calibration
    entries and root variables without a typed slot are kept verbatim with their source name
    and numeric type (`ExtraVariable`, `AttrValue`), so CfRadial and DORADE sources reach a
    DataTree or Py-ART `Radar` without losing metadata (sections 2, 3, 9, 11).

---

## 1. Group and variable layout

The FM301 view (section 12) builds these groups and variables from the in-memory model:

| FM301 path | Rust | Notes |
|---|---|---|
| `/` attributes (Tables 301-1..3, WMO-CF-2) | `Volume::attrs: GlobalAttrs` | section 11; unmodelled source attributes in `GlobalAttrs::other` |
| `/<name>` root variables without a slot (CfRadial `status_xml`, `grid_mapping`) | `Volume::extra_vars: Vec<ExtraVariable>` | verbatim name, dims, dtype and attributes |
| `/volume_number`, `/time_coverage_start`, `/time_coverage_end` | `Volume::{volume_number, time_coverage}` | |
| `/latitude`, `/longitude`, `/altitude`, `/altitude_agl` | `Volume::location` | xradar makes these root coordinates, inherited by sweeps |
| `/platform_type`, `/instrument_type`, `/primary_axis`, `/status_str` | `Volume::{platform_type, instrument_type, primary_axis, status_str}` | |
| `/sweep_group_name(sweep)`, `/sweep_fixed_angle(sweep)` | derived from `Volume::sweeps` | CfRadial 2 root variables. xradar emits them for ODIM and CfRadial 1 but not NEXRAD (A.2, A.4, A.5). The view always emits them |
| `/radar_parameters/*` | `Volume::radar_parameters` | Table 301-12; variable names differ per flavor (12.4) |
| `/radar_calibration/*` (dim `calib`) | `Volume::radar_calibration: Vec<RadarCalibration>` | Table 301-14; non-FM301 CfRadial entries (`k_squared_water`, `i0_dbm_*`, ...) in `RadarCalibration::extra` |
| `/georeferencing_correction/*` | `Volume::georeferencing_correction` | CfRadial only; FM301 covers fixed platforms only |
| `/sweep_<n>` | `Volume::sweeps[n]: Sweep` | `sweep_number == n` |
| `/sweep_<n>/time(time)`, `azimuth(time)`, `elevation(time)` | `Sweep::rays` | Tables 301-6a, 301-7a; ray dimension name and order follow `ViewOptions::first_dim` (12.1) |
| `/sweep_<n>/range(range)` | `Sweep::range: RangeCoord` | one per sweep (section 6) |
| `/sweep_<n>/frequency(frequency)` | `Volume::radar_parameters.frequency_hz` | constant over the volume; written into every sweep |
| `/sweep_<n>/sweep_number, sweep_mode, follow_mode, prt_mode, fixed_angle` | `Sweep` fields | Table 301-7a; xradar spells `fixed_angle` as `sweep_fixed_angle` |
| `/sweep_<n>/{polarization_mode, rays_are_indexed, rays_angle_resolution, qc_procedures, target_scan_rate}` | `Sweep` fields | Table 301-8a (scalars) |
| `/sweep_<n>/polarization_sequence(prt)` | `Sweep::polarization_sequence` | Table 301-8a |
| `/sweep_<n>/{nyquist_velocity, unambiguous_range, prt, ...}(time)` | `Sweep::ray_vars: RayVariables` | Table 301-8a (per ray); section 9 |
| `/sweep_<n>/<name>` without a slot (CfRadial `ray_start_range`, `georef_time`, ...) | `Sweep::extra_vars: Vec<ExtraVariable>` | verbatim; section 9 |
| `/sweep_<n>` attributes without a slot | `Sweep::other` | verbatim |
| `/sweep_<n>/monitoring/*` | `Sweep::monitoring` | Table 301-11; a child group (`Group::children`, 12.1) |
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
    /// (`Rays::time_s`, `RadarCalibration::time_s`). Always whole seconds
    /// (sub-second part zero): FM301 Table 301-6b writes it as
    /// `YYYY-MM-DDThh:mm:ssZ`, and any fraction lives in `time_s`.
    /// A source that states a reference (CfRadial `time.units`) keeps it, so `time_s` passes
    /// through unchanged. Otherwise it is the earliest ray time floored to the second.
    /// NEXRAD: the first radial's collection time floored, which is Py-ART's `get_times`
    /// rule (KTLX 2024: 00:02:17, first `time_s` 0.182). When the volume header time differs
    /// from the first radial, the header time stays in `NexradMetadata`.
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
    /// Root variables with no slot above (CfRadial `status_xml`, `grid_mapping`), verbatim
    /// and in file order. The Xradar flavor writes them; the WMO flavor drops non-FM301 names.
    pub extra_vars: Vec<ExtraVariable>,
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
    /// Source attributes with no slot above, verbatim, typed and in file order. For CfRadial:
    /// `Sub_conventions`, `original_format`, `driver`, `created`, `start_datetime`,
    /// `start_time`, `end_datetime`, `end_time`, `n_gates_vary` (DOW8, IRENE; A.5). Py-ART
    /// keeps all of them in `Radar.metadata`; xradar 0.12 drops them (A.5), so the view
    /// writes them only with `ViewOptions::passthrough = Passthrough::All` (12.1).
    pub other: Vec<(Box<str>, AttrValue)>,
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
    /// `scan_id` text as the source wrote it, whenever it is not exactly
    /// `ScanStrategy::id.to_string()`: non-numeric ids, and numeric ids with other spellings
    /// ("00", "+5"). `None` when `id` reproduces the text.
    pub scan_id_text: Option<String>,
    /// One leg per sweep: the legacy `ScanLegMetadata` with unchanged fields.
    pub legs: Vec<ScanLeg>,
}

/// Variable names differ by flavor; 12.4 has the table. FM301-2022 Table 301-12a has no
/// `radar_` prefix (`antenna_gain_h`, `receiver_bandwidth`); xradar 0.12 writes
/// `radar_antenna_gain_h` and `radar_receiver_bandwidth`; CfRadial 1 files write
/// `radar_antenna_gain_h` and `radar_rx_bandwidth` (DOW8, IRENE).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RadarParameters {
    /// Operating frequencies in Hz (`frequency` dimension). Legacy `radar_frequency_mhz` × 1e6.
    pub frequency_hz: Vec<f64>,
    pub antenna_gain_h_db: Option<f32>,
    pub antenna_gain_v_db: Option<f32>,
    pub beam_width_h_deg: Option<f32>,
    pub beam_width_v_deg: Option<f32>,
    pub receiver_bandwidth_hz: Option<f32>,
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
    /// Held as i32, the widest type a source uses (CfRadial 1 `r_calib_index` is int32 in DOW8
    /// and IRENE). The view writes the flavor's type: WMO flavor `byte` for
    /// `/radar_calibration/calib_index` (Table 301-14a) and `int` for the per-ray
    /// `calib_index(time)` (Table 301-8a), returning `ViewError::OutOfRange` if a value does
    /// not fit; Xradar flavor as xradar writes it (F.4 pins IRENE).
    pub calib_index: Option<i32>,
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
    /// Entries outside Table 301-14a, named as xradar names them (the CfRadial `r_calib_`
    /// prefix removed): `k_squared_water`, `i0_dbm_hc/vc/hx/vx`,
    /// `dynamic_range_db_hc/vc/hx/vx`, `dbz_correction` (DOW8; A.5). CfRadial
    /// `r_calib_base_dbz_1km_*` fills `base_1km_*_dbz` above, the name xradar uses.
    pub extra: Vec<(Box<str>, AttrValue)>,
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

Typed values shared by the model and the view:

```rust
// crates/recast-radar-core/src/model/values.rs

/// A numeric scalar that keeps its type. CF requires `_FillValue`, `valid_range` and
/// `flag_values` in the packed variable's type, and xarray picks the decoded dtype from the
/// type of `scale_factor` (float32 attributes on 8/16-bit data decode to float32; DOW8 A.5).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Scalar { I8(i8), U8(u8), I16(i16), U16(u16), I32(i32), U32(u32), I64(i64), U64(u64),
                  F32(f32), F64(f64) }

/// A typed 1-D buffer (row-major when the owner has more than one dimension).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ArrayBuf { I8(Vec<i8>), U8(Vec<u8>), I16(Vec<i16>), U16(Vec<u16>), I32(Vec<i32>),
                    U32(Vec<u32>), I64(Vec<i64>), F32(Vec<f32>), F64(Vec<f64>),
                    Text(Vec<Box<str>>) }

/// An attribute value with its type. `Bool` exists because xradar 0.12 writes Python bools
/// and ints for NEXRAD attributes (`mpda_vcp: False`, `number_elevation_cuts: 23`; A.2). The
/// Xradar flavor emits `Bool` as a bool. netCDF has no bool type, so the WMO flavor and file
/// writers emit "true"/"false" text, FM301's convention (xradar's own `to_cfradial2` raises
/// `TypeError` on a bool attribute).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AttrValue { Text(Box<str>), Bool(bool), Scalar(Scalar), Array(ArrayBuf) }

/// A source variable without a typed slot, kept verbatim.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtraVariable {
    /// Source name (CfRadial `georef_time`, `ray_start_range`, `status_xml`).
    pub name: Box<str>,
    /// Dimension names: `[]`, `["time"]`, `["time", "<source dim>"]`. A `"time"` dimension has
    /// `nrays` entries and follows the view's ray order.
    pub dims: Vec<Box<str>>,
    pub shape: Vec<u32>,
    pub values: ArrayBuf,
    pub attrs: Vec<(Box<str>, AttrValue)>,
}
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
    /// `polarization_sequence(prt)` (Table 301-8a): "H" or "V" per PRT, for
    /// `prt_mode = hybrid`.
    pub polarization_sequence: Option<Vec<Box<str>>>,
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
    /// Per-ray platform position and attitude for moving platforms (CfRadial 1
    /// `latitude(time)`, `heading(time)`, ...). Not FM301.
    pub platform_track: Option<Box<PlatformTrack>>,
    /// Sweep variables with no slot above, verbatim and in file order (section 9).
    pub extra_vars: Vec<ExtraVariable>,
    /// Sweep group attributes with no slot above, verbatim. NEXRAD's xradar sweep attributes
    /// come from `NexradMetadata` through `fm301::ExtraAttrs` instead.
    pub other: Vec<(Box<str>, AttrValue)>,
    /// Dataset variables, in source order.
    pub fields: Vec<Field>,
    /// Source cut number (NEXRAD ICD elevation number, 1-based). Not an FM301 variable.
    pub elevation_number: Option<u16>,
    /// `false` when rays stop before the end-of-elevation marker (truncated archive,
    /// real-time chunk). Same notion as xradar's `incomplete_sweep`.
    pub complete: bool,
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

// Each `Other` holds a source string outside Table 301-15 verbatim (Radx writes "not_set").
// Parsing maps a table spelling to its variant, so `Other` never holds a table value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FollowMode { None, Sun, Vehicle, Aircraft, Target, Manual, Other(Box<str>) }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrtMode { Fixed, Staggered, Dual, Hybrid, Other(Box<str>) }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolarizationMode { Horizontal, Vertical, HvAlt, HvSim, Circular, Other(Box<str>) }

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

/// Moving-platform position and attitude per ray (CfRadial 1 georeference variables). Not
/// FM301. The attitude vectors are the ones Py-ART's `Radar` has attributes for. Other
/// georeference variables (`georefs_applied`, `georef_time`, `georef_unit_num`,
/// `georef_unit_id`, platform velocities) stay in `Sweep::extra_vars` under their source
/// names, which is where xradar 0.12 keeps them (DOW8; A.5).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlatformTrack {
    pub latitude_deg: Vec<f64>,
    pub longitude_deg: Vec<f64>,
    pub altitude_m: Vec<f64>,
    pub altitude_agl_m: Option<Vec<f64>>,
    pub heading_deg: Option<Vec<f32>>,
    pub roll_deg: Option<Vec<f32>>,
    pub pitch_deg: Option<Vec<f32>>,
    pub drift_deg: Option<Vec<f32>>,
    pub rotation_deg: Option<Vec<f32>>,
    pub tilt_deg: Option<Vec<f32>>,
}
```

Construction API used by decoders. Every call is O(1) or O(fields); none copies gate data.

```rust
impl Sweep {
    pub fn new(sweep_number: u32, sweep_mode: SweepMode, fixed_angle_deg: f32) -> Self;
    pub fn reserve_rays(&mut self, rays: usize);
    /// Returns the new ray's index, which the decoder passes to `Field::push_row_*`.
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
    /// Append absent rows for rays at the end that a field never received, then check the
    /// invariants. Decoders call it once per sweep.
    pub fn seal(&mut self) -> Result<(), SweepError>;
}
```

Invariants after `seal`:

- Every `Rays` vector has `nrays` entries, and every present `RayVariables`,
  `PlatformTrack` or `"time"`-dimensioned `ExtraVariable` vector matches.
- Every field has `field.nrays == nrays`, and row `r` of every field belongs to ray `r`.
  `Field::push_row_*` takes the ray index and fills skipped rays with absent rows, so a
  moment missing from interior radials cannot shift later rows (4).
- Every field satisfies `field.gates.start + field.ngates * field.gates.stride <= range.ngates()`.
- `stride > 1` occurs only with `RangeCoord::Uniform`.
- Field names are unique within the sweep, compared by `FieldName::as_str()`.

All model structs have public fields, so a caller can move buffers out by destructuring
(`let Field { data, .. } = field;`). `Field::into_parts` and `FieldData::into_array` (4) are
the named forms that bindings use (12.2).

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
    /// Rows the source did not provide for this field, ascending. Each is filled with the
    /// coding's fill code (NaN for floats without one). Empty in the common case, which costs
    /// no allocation. Replaces legacy `MomentGrid::radial_indices`.
    pub absent_rows: Vec<u32>,
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
    /// Physical values (derived products, float32 sources), stored verbatim.
    F32 { values: Vec<f32>, coding: FloatCoding<f32> },
    /// float64 sources (espdg ODIM planes, CfRadial `double` fields), stored verbatim.
    /// xradar keeps these as float64, so raw hashes stay comparable (F.4).
    F64 { values: Vec<f64>, coding: FloatCoding<f64> },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct IntCoding<T> {
    pub transform: LinearTransform,
    /// CF `_FillValue`: the code for "no data" and the code the view pads with. NEXRAD 0,
    /// ODIM `nodata`, CfRadial `_FillValue`.
    pub fill_value: Option<T>,
    /// Table 301-10 `_Undetect`: radiated, but no valid echo. NEXRAD 0 (below threshold,
    /// equal to `fill_value`), ODIM `undetect`.
    pub undetect: Option<T>,
    /// NEXRAD 1. Exported as `flag_values = [1]`, `flag_meanings = "range_folded"`.
    pub range_folded: Option<T>,
    /// CF `valid_range` in packed units (WMO-CF.5.2.15).
    pub valid_range: Option<[T; 2]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum LinearTransform {
    /// physical = (raw - offset) / scale, evaluated in f32. NEXRAD ICD form; the current
    /// decoder and Py-ART both evaluate exactly this expression. The view writes
    /// `scale_factor = 1/scale` and `add_offset = -offset/scale` as float64, as xradar does.
    IcdScaleOffset { scale: f32, offset: f32 },
    /// physical = raw * scale_factor + add_offset. CF and ODIM gain-offset form.
    /// `attr_width` is the type the source wrote the two attributes in. The view writes them
    /// in that type, because xarray derives the decoded dtype from it (DOW8 `scale_factor` is
    /// float32 and decodes to float32 in xradar and Py-ART).
    CfScaleOffset { scale_factor: f64, add_offset: f64, attr_width: FloatWidth },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FloatWidth { F32, F64 }

/// Float fields are stored as the source wrote them. NaN is always missing. A non-NaN
/// source fill (for example -9999) stays in the data. `Field::gate` resolves it to `Missing`,
/// and the view writes it as `_FillValue`, so xarray and Py-ART mask it exactly as they mask
/// the source file. Derived fields use `FloatCoding::default()` and NaN; the view then
/// writes `_FillValue = NaN`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FloatCoding<T> {
    /// `None`: the values are physical. `Some`: an ODIM float plane whose `gain`/`offset`
    /// is not 1/0, applied on read like an integer field's transform, so decode never makes a
    /// scaling pass. espdg has gain 1 and offset 0 (`None`).
    pub transform: Option<LinearTransform>,
    pub fill_value: Option<T>,
    pub undetect: Option<T>,
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
    /// `flag_values`, `flag_masks` and `flag_meanings` of discrete fields (classification),
    /// besides the range-folded flag, which comes from the coding. Held as i64; the view
    /// writes `flag_values` and `flag_masks` in the variable's packed type (CF, Table 301-10)
    /// and returns `ViewError::OutOfRange` if one does not fit.
    pub flag_values: Vec<i64>,
    pub flag_masks: Vec<i64>,
    pub flag_meanings: Vec<Box<str>>,
    /// Source attributes with no slot above, verbatim, typed and in file order
    /// (for example CfRadial `grid_mapping`).
    pub other: Vec<(Box<str>, AttrValue)>,
}

/// One gate's value with its sentinel resolved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gate { Value(f32), Missing, Undetect, RangeFolded }

pub enum RowRef<'a> { U8(&'a [u8]), U16(&'a [u16]), I8(&'a [i8]), I16(&'a [i16]),
                      F32(&'a [f32]), F64(&'a [f64]) }

impl Field {
    pub fn new(name: FieldName, gates: GateMapping, ngates: u32, data: FieldData) -> Self;
    pub fn shape(&self) -> (usize, usize); // (nrays, ngates)
    pub fn row(&self, ray: usize) -> Option<RowRef<'_>>;
    /// Resolves a native gate, in this order:
    /// 1. `ray` is in `absent_rows`: `Missing`.
    /// 2. The raw value equals `undetect`: `Undetect`. This rule comes before the fill rule,
    ///    so NEXRAD raw 0 (fill and undetect both 0) reads as `Undetect` in a provided row.
    /// 3. It equals `fill_value` or is NaN: `Missing`.
    /// 4. It equals `range_folded`: `RangeFolded`. This comes before the `valid_range` rule,
    ///    because NEXRAD's `valid_range = [2, MAX]` excludes the range-folded code 1.
    /// 5. It is outside `valid_range`: `Missing`.
    /// 6. Otherwise: `Value(physical)`.
    /// Gates past the native extent are not native gates (`None`). In the view they are
    /// padding, written as `_FillValue`.
    pub fn gate(&self, ray: usize, gate: usize) -> Option<Gate>;
    /// Legacy `MomentGrid::scaled_value` semantics: `None` for every sentinel.
    pub fn value(&self, ray: usize, gate: usize) -> Option<f32>;
    /// 256-entry decode table for u8/i8 fields (NaN for sentinels) for hot loops.
    pub fn lut8(&self) -> Option<[f32; 256]>;
    /// Explicit float expansion. Decoders never call this.
    pub fn to_physical(&self) -> Vec<f32>;
    /// Native geometry on the given range: (centre of native gate 0, native spacing).
    pub fn native_geometry(&self, range: &RangeCoord) -> Option<(f64, f64)>;
    // Decode-time row pushes. `ray` is the index `Sweep::push_ray` returned. When
    // `ray > rows`, rows `rows..ray` are appended as absent rows first; `ray < rows` is
    // `FieldError::RowOrder`. Short rows are padded with the fill code, as the legacy
    // push_* methods do.
    pub fn reserve_rows(&mut self, rows: usize);
    pub fn push_row_u8(&mut self, ray: usize, row: &[u8]) -> Result<(), FieldError>;
    pub fn push_row_u16_be(&mut self, ray: usize, row_be: &[u8]) -> Result<(), FieldError>;
    pub fn push_row_i8(&mut self, ray: usize, row: &[i8]) -> Result<(), FieldError>;
    pub fn push_row_i16_be(&mut self, ray: usize, row_be: &[u8]) -> Result<(), FieldError>;
    pub fn push_row_f32(&mut self, ray: usize, row: &[f32]) -> Result<(), FieldError>;
    pub fn push_row_f64(&mut self, ray: usize, row: &[f64]) -> Result<(), FieldError>;
    /// Move out: bindings hand `FieldData`'s `Vec` to NumPy without copying (12.2).
    pub fn into_parts(self) -> FieldParts;
}

pub struct FieldParts {
    pub name: FieldName, pub quantity: Quantity, pub polarization: Polarization,
    pub attrs: FieldAttrs, pub nrays: u32, pub ngates: u32, pub gates: GateMapping,
    pub data: FieldData, pub absent_rows: Vec<u32>,
}

impl FieldData {
    /// The buffer, moved, with its coding. No copy.
    pub fn into_array(self) -> (ArrayBuf, Coding);
}
pub enum Coding { U8(IntCoding<u8>), U16(IntCoding<u16>), I8(IntCoding<i8>),
                  I16(IntCoding<i16>), F32(FloatCoding<f32>), F64(FloatCoding<f64>) }
```

```rust
// crates/recast-radar-core/src/model/names.rs

/// Dataset variable name. Known names are variants, so comparisons are cheap and each has
/// one spelling. Any other name is `Other`, verbatim. `Other` never holds a known spelling:
/// every constructor goes through `parse`.
/// `Serialize` and `Deserialize` are implemented by hand through `as_str` and `parse`, so
/// serialized names are the FM301 names ("DBZH", "VEL"), not Rust variant spellings.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
    /// Default field name in `pyart.config` (`PyartNames::Config`).
    pub pyart: Option<&'static str>,
    /// Name Py-ART's ODIM reader (`aux_io.read_odim_h5`) gives this quantity, if different
    /// (`PyartNames::Reader`, 8.2 note 3).
    pub pyart_odim: Option<&'static str>,
}

/// How a Py-ART export names fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PyartNames {
    /// `pyart.config` defaults for every source: the names Py-ART's algorithms expect.
    Config,
    /// The names Py-ART's reader for the volume's source format produces: config names for
    /// NEXRAD (`read_nexrad_archive`), verbatim names for CfRadial (`read_cfradial`),
    /// `aux_io` names for ODIM. Used for value and name conformance against those readers.
    Reader,
}

impl FieldName {
    pub fn as_str(&self) -> &str;
    /// Exact, case-sensitive match against the table; otherwise `Other`.
    pub fn parse(name: &str) -> FieldName;
    /// Message 31 or Message 1 data block name (space- or NUL-padded) -> xradar name.
    pub fn from_nexrad_block(name: &[u8]) -> FieldName;
    pub fn info(&self) -> Option<&'static NameInfo>;
    /// Py-ART alias for the mode and source format, or the name itself when there is none.
    pub fn pyart_name(&self, mode: PyartNames, source: SourceFormat) -> Cow<'_, str>;
}

impl Quantity {
    /// Classifier for verbatim names; replaces `canonical_moment`. Tries, in order:
    /// 1. `standard_name`, against FM301 Table 301-9 and the CF standard names
    ///    (`equivalent_reflectivity_factor`, `radial_velocity_of_scatterers_away_from_instrument`,
    ///    ...). Py-ART-written CfRadial (xsapr `reflectivity_horizontal`) carries one. An
    ///    unrecognised `standard_name` falls through: DOW8 writes the variable name (`DBZHC`).
    /// 2. The exact name against the Py-ART config and ODIM-reader names
    ///    (`reflectivity_horizontal` -> Reflectivity / H).
    /// 3. Suffix stripping of FM301, ODIM and CfRadial stems (DBZHC_F -> DBZHC -> DBZ ->
    ///    Reflectivity).
    /// Returns `(Other, Unspecified)` when nothing matches.
    pub fn classify(name: &str, standard_name: Option<&str>) -> (Quantity, Polarization);
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
| `volume_time` | `time_reference` = `volume_time` floored to the second | the full value goes to the residue (5.5) |
| `vcp: Option<VcpInfo { pattern }>` | `scan.vcp_pattern` | The conversion does not set `scan.id` or `scan.name` from the pattern, because that would make legacy `scan_name: None` come back as `Some`. The native io-nexrad decoder sets them (`id = pattern`, `name = "VCP-{pattern}"`) |
| `cuts` | `sweeps` | index i gives `sweep_number = i` |
| `metadata.source_path`, `archive_version`, `compression` | `provenance.source_path`, `source_version`, `compression` | |
| `metadata.message_count`, `decoded_radial_count`, `skipped_message_count` | `provenance.decode.*` | |
| `metadata.scan_mode` (`Option`) | every `sweep.sweep_mode`; `None` gives `AzimuthSurveillance` | section 10; the `Option` goes to the residue |
| `metadata.radar_frequency_mhz` (u32) | `radar_parameters.frequency_hz = [mhz × 1e6]` | exact both ways (u32 × 1e6 < 2^53) |
| `metadata.beam_width_h_deg` / `beam_width_v_deg` | `radar_parameters.beam_width_h_deg` / `beam_width_v_deg` | |
| `metadata.pulse_width_us` | `radar_parameters.pulse_width_s = us × 1e-6` | inexact in f32; residue |
| `metadata.prt_s` | `radar_parameters.prt_s` | |
| `metadata.unambiguous_range_km` | `radar_parameters.unambiguous_range_m = km × 1000` | inexact in f32; residue |
| `metadata.scan_name` | `scan.name` | |
| `metadata.scan_id` (String) | `scan.id` if it parses as i64; `scan.definition.scan_id_text` whenever the text is not `id.to_string()` | exact |
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
| `radials[i].time_offset_ms` | `rays.time_s[i]`, and the raw value kept in the residue | Meaning depends on the decoder (13.2). NEXRAD stores ms of day of collection: `time_s = (midnight of volume_time's date + ms) - time_reference`, plus one day when that is more than 12 h before the reference. CfRadial stores ms since volume start (truncated): `time_s = ms / 1000 + (volume_time - time_reference)`. DORADE stores ms since sweep start. ODIM and JMA store 0, so `time_s = 0` until native decoding |
| `radials[i].gate_range` | residue only | the field's `GateMapping` is authoritative |
| `radials[i].nyquist_velocity_mps` | `ray_vars.nyquist_velocity_mps[i]`, NaN for `None`; the whole vector is `None` if every radial is `None` | a legacy `Some(NaN)` would come back as `None`; residue |
| `radials[i].radial_status` | residue; io-nexrad also keeps it in `NexradMetadata` | not FM301 |
| `ray_instrument_metadata` (empty, or one entry per radial) | `ray_vars` vectors | an all-`None` non-empty vector would come back empty; residue keeps its length |
| `ray_instrument_metadata[i].prt_s` | `ray_vars.prt_s[i]`, NaN for `None` | as for Nyquist |
| `.unambiguous_range_km` | `ray_vars.unambiguous_range_m[i] = km × 1000` | inexact in f32; residue |
| `.pulse_count` (`Option<u32>`) | `ray_vars.n_samples[i]` (i32; -9999 when missing) | values above `i32::MAX` go to the residue, and the ray gets -9999 |
| `.independent_samples` | `ray_vars.independent_samples[i]`, NaN for `None` | as for Nyquist |
| `moments: BTreeMap<MomentType, MomentGrid>` | `fields: Vec<Field>` | BTreeMap order, the only order legacy kept. Native decoders use source order. The reverse inserts into a BTreeMap, so order is exact |
| (none) | `range` | built from the grids with `attach_geometry`, after converting each legacy `gate_range` to gate centres using that decoder's convention (6.6) |
| (none) | `complete` | `true` |

The legacy ODIM decoder synthesizes azimuths as `(ray + 0.5) × 360 / nrays` and ray times of
0. xradar returns measured per-ray values: iesha azimuths 0.5081, 1.5244, ... with the file's
`how/startazA`/`stopazA` present, and ray times that differ per ray. Native ODIM decoding in
F.3 matches xradar, which F.4 verifies.

### 5.3 `MomentGrid` / `MomentStorage` → `Field` / `FieldData`

| Legacy | New |
|---|---|
| `moment: MomentType` | `name` by 5.4; `quantity` and `polarization` from the legacy variant; the variant goes to the residue |
| `gate_range` | `ngates = gate_count`; `gates = attach_geometry(centre(gate_range), spacing, count)` |
| `scale`, `offset` | `IcdScaleOffset { scale, offset }`. Every legacy grid uses this form, including ODIM's inverted gain |
| `nodata: Option<u16>` | `coding.fill_value` (narrowed to u8 for `U8` storage; a value that does not fit goes to the residue). For NEXRAD sources `coding.undetect = Some(0)` as well (7.1) |
| `range_folded: Option<u16>` | `coding.range_folded` (narrowed the same way) |
| `radial_indices` | Removed: row `r` belongs to ray `r`. A non-identity mapping is scattered into rows, the rays without data become `absent_rows`, and the original indices go to the residue. No corpus grid has non-identity indices: the four NEXRAD probe volumes (A.2, A.3), and the reviewer's legacy probe over 22 decoded inputs (NEXRAD, ODIM, CfRadial, DORADE, JMA) |
| `storage: U8(v)` / `U16(v)` / `F32(v)` | `FieldData::U8 { values: v, .. }` / `U16` / `F32`, moved. Legacy F32 grids hold physical values with NaN for every sentinel (the legacy ODIM decoder applied gain and offset and narrowed float64 planes; the legacy CfRadial decoder expanded packed data), so they become `F32` with `FloatCoding::default()` |

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
| `Unknown("CFP")` from a NEXRAD source | `Ccorh` | NEXRAD block name. From any other source, `CFP` follows the next row |
| `Unknown(s)` | `FieldName::parse(s)`: a known name or `Other(s)` | migrated crates rename derived ids (8.3); the conversion does not |

Names within one cut are assigned in two passes, so they stay unique by `as_str()`:

1. Every `Unknown(s)` gets its name first, because `s` is the source's own spelling.
2. Each of the seven canonical variants then gets its default name from the table, unless that
   name is taken. A taken name means legacy `canonical_moment` folded two source variables
   onto one variant: for example a CfRadial file with `DBZ` and `DBZH` decodes to
   `Reflectivity` (from DBZ) plus `Unknown("DBZH")`. The variant then becomes
   `Other("<Variant>")` (`Other("Reflectivity")`, with `_2`, `_3` appended if that is taken
   too). Its `quantity` and `polarization` come from the variant, so `Sweep::find` still finds
   it. `Other` never holds a known spelling (4), so this cannot collide with a real FM301
   name. The source's original spelling (`DBZ`) was already lost by the legacy decoder; only
   native decoding restores it. No corpus file has this collision.

The reverse (`legacy_from_volume`, 13.2) uses the `MomentType` recorded in the residue when
there is one. Without a residue (natively decoded volumes), `FieldName::to_legacy_moment` is
the exact inverse of the table for the seven canonical names and `Ccorh` → `Unknown("CFP")`
(NEXRAD sources only), and `Unknown(name.as_str())` for every other name. A decoder's legacy
wrapper applies that decoder's own legacy naming instead where it differs: io-cfradial,
io-dorade and io-odim run `canonical_moment` with first-match-wins, as their legacy decoders
do (13.3).

`ProductId(String)` converts to `FieldName` through `parse`.

### 5.5 Exactness: the legacy residue

A legacy → new → legacy round trip must reproduce the legacy volume bit for bit (13.4). Every
value the new model cannot hold exactly is therefore returned beside the `Volume` in a
`LegacyResidue` (13.2), not stored inside the model:

| Value | Why the new model cannot hold it exactly |
|---|---|
| `volume_time` | `time_reference` is whole seconds (KLIX 2005: 13:00:12.833) |
| `metadata.scan_mode` | `Option`; the model's `sweep_mode` is mandatory, and the value is lost with zero cuts |
| `metadata.pulse_width_us`, `metadata.unambiguous_range_km` | µs → s and km → m in f32 are inexact (the reviewer's sweep: 257 of 5,594 sampled µs values and 19 of 836 km values do not return) |
| per ray `unambiguous_range_km` | same |
| per ray `pulse_count` above `i32::MAX` | `n_samples` is i32 |
| per ray `Some(NaN)` in Nyquist, PRT, independent samples | NaN encodes `None` |
| `ray_instrument_metadata.len()` when every entry is `None` | an all-missing vector is dropped |
| `Radial::time_offset_ms`, `Radial::gate_range`, `Radial::radial_status` | decoder-specific meanings (5.2, 6.6) |
| `MomentGrid::moment`, `gate_range`, `nodata`, `range_folded`, non-identity `radial_indices` | naming collisions (5.4), rounding and start-vs-centre conventions (6.6), u16 codes that do not fit u8 |

The reverse conversion uses a residue value only while the model still holds its forward
image. For example, it uses residue `pulse_width_us` only if `radar_parameters.pulse_width_s`
still equals `us × 1e-6` computed from it. A migrated algorithm that changed the model value
therefore wins over the stale residue. No tolerance list is needed. Every other mapped value
converts exactly in both directions (f32 → f64 → f32 widening, u8 → u16, `IcdScaleOffset`,
moved buffers).

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
  coordinate"`. Py-ART's default is `linear_interp=True`, which interpolates (KLIX 2005 fine
  gates 4..15 are 46.5, 46.5, 46.6875, ...; A.3). F.4 therefore calls `read_nexrad_archive`
  with `linear_interp=False` for every volume with coarse reflectivity (Message 1 and
  legacy-resolution Message 31). The option makes no difference for super-resolution volumes.
- **Coordinates are correct where xradar's are wrong.** KLIX 2005 sweep 2 gets a range of
  -375 m + 250 m × 548. Because the range is per sweep, a REF-only surveillance sweep keeps
  its own 1 km range (sweep 0: 0 m + 1000 m × 460), which matches xradar for those sweeps.
- **Decode cost is zero.** Rows are written once into native buffers, as `MomentGrid` does
  today. When a finer spacing arrives later in the same radial (REF at 1 km first, VEL at
  250 m next), `Sweep::attach_geometry` rewrites integer mappings; no data moves.
- **Bindings.** A PyO3 layer moves a field's `Vec<u8>` or `Vec<u16>` into NumPy without
  copying (12.2). The result is already the FM301 variable whenever
  `start == 0 && stride == 1 && ngates == range.ngates` and the view's ray order is the
  storage order. In KTLX 2024 that holds for 76 of 104 fields: every field of the Doppler
  cuts and of sweeps 12, 13 and 16..19, and REF and CCORH in every sweep. The other 28 fields
  become the native array inside a lazy backend array that pads on `__getitem__`, which is
  xradar's own mechanism. Those 28 are the dual-pol moments in the surveillance cuts plus
  VEL, SW and the dual-pol moments in sweeps 10 and 11.
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
fill_value: Some(0), undetect: Some(0), range_folded: Some(1), valid_range: Some([2, MAX]) }`.
MAX is 255 for u8. For u16 it is 65535 until stream A confirms the ICD data masks that xradar
applies (PHI `& 0x3FF`, ZDR `& 0x7FF`; section 15).

**Raw 0 has two roles.** ICD "below threshold" means the bin was radiated but produced no
valid echo, which is exactly FM301 Table 301-10 `_Undetect` ("an area (range bin) that has
been radiated but has not produced a valid echo"). Recording it only as missing would make
cross-format algorithms differ by source: rain accumulation, echo tops and VIL treat no echo
as zero and missing as unknown, and ODIM already has a separate undetect code. So `undetect`
is `Some(0)`, and `Field::gate` returns `Undetect` for raw 0 in a provided row and `Missing`
for absent rows (4). Raw 0 is also `_FillValue`. That keeps spec 4.2's masking behaviour,
matches Py-ART (which masks it), and is the only code free for padding: u8 has no unused
value, since 0 is below threshold, 1 is range folded and 2..255 are data.

The FM301 view writes the encoded integers (`uint8`, `uint16`) with these attributes, each
flag and range attribute in the variable's own type (CF, Table 301-10):

| Attribute | Value | Meaning |
|---|---|---|
| `scale_factor`, `add_offset` | float64 | CF packing |
| `_FillValue` | `0` | below threshold, and view padding beyond the native extent or absent rows; xarray masks it |
| `_Undetect` | `0` | FM301: raw 0 inside the data means radiated without a valid echo |
| `valid_range` | `[2, MAX]` | packed units |
| `flag_values`, `flag_meanings` | `[1]`, `"range_folded"` | |

In this encoded form, raw 0 cannot say whether a gate was padding or below threshold. The
model can (native extent and `absent_rows`), and a binding that needs the distinction reads it
from the model (12.2).

Consequences (A.6, run with xarray 2026.7.0; the review re-ran them with `_Undetect`, and they
were re-checked for this revision):

- `xr.decode_cf` turns below-threshold gates and padding into NaN, matching Py-ART.
- xarray moves `_FillValue`, `scale_factor` and `add_offset` into `.encoding` but leaves
  `_Undetect`, `valid_range`, `flag_values` and `flag_meanings` in `.attrs` of the decoded
  float64 variable, still in packed units. Read naively, that says "physical 1 dBZ is range
  folded, and valid values are 2..255 dBZ". The binding's decoded form therefore moves them
  into `.encoding` (12.3).
- Range folded decodes to `add_offset + scale_factor` (-32.5 dBZ) unless the consumer applies
  the flag. The binding masks it when `mask_range_folded = true`, the default, to match
  Py-ART, which masks `raw <= 1`. With `range_folded_variable = true` it also keeps the
  information as a lazy ancillary flag variable (12.3).
- netCDF4-python auto-masking also masks values outside `valid_range`, so a file written from
  the view reads range folded as masked in netCDF4-python (and Py-ART's `read_cfradial`) but
  as -32.5 in plain `xr.open_dataset`. The binding default gives the netCDF4-python and Py-ART
  result, and `valid_range` stays because WMO-CF.5.2.15 requires it.
- `missing_value = [0, 1]` would mask both on decode, but xarray emits `SerializationWarning`
  ("multiple fill values"), and `to_netcdf` raises `ValueError` (conflicting `_FillValue` and
  `missing_value`, or an array truth-value error when `_FillValue` is absent). Rejected.
- This deliberately differs from xradar 0.12, which writes no `_FillValue` for NEXRAD, so its
  below-threshold and padded gates decode to physical-looking numbers. Conformance tests
  compare xradar's raw arrays (`mask_and_scale=False`) against the view's encoded form.

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

Float planes are stored in their own width: espdg `float64` planes (gain 1, offset 0,
`nodata` 95.5, `undetect` -32.0) become `F64`, with `nodata` and `undetect` held verbatim as
`FloatCoding` values. xradar keeps them as float64 too, so F.4 can hash the raw arrays. Only
the legacy shim narrows them, as the legacy decoder always did (it applied gain and offset and
turned both sentinels into NaN).

### 7.3 CfRadial

Packed `byte` and `short` variables stay `I8` and `I16`, keeping the file's `scale_factor`,
`add_offset` and `_FillValue` (A.5):

- IRENE `DBZ` and `VEL`: int8, `_FillValue = -128`.
- DOW8 `DBZHC`, `VEL` and `WIDTH`: int16, `_FillValue = -32768`.

The file's bytes and attributes pass through unchanged; today the decoder expands them to
f32. `scale_factor` and `add_offset` keep their attribute type in
`CfScaleOffset::attr_width` (DOW8: float32, so xarray decodes to float32).

Float variables become `F32` or `F64`, keeping their `_FillValue` in the data and in
`FloatCoding::fill_value`. xsapr `reflectivity_horizontal` is float32 with `_FillValue`
-9999.0. It is stored verbatim, and the view writes `_FillValue = -9999.0`.

**Ragged sweeps.** CfRadial `n_gates_vary = "true"` stores each field as 1-D `(n_points)` data
with `ray_n_gates(time)` and `ray_start_index(time)`. The decoder has to lay those out into
rows anyway, so it writes each row padded to the sweep's largest `ray_n_gates` with the fill
code. That is the only pass, and there is no copy beyond the layout. Py-ART's `read_cfradial`
gives the same shape, with the padding masked. The source's `ray_n_gates` stays in
`Sweep::extra_vars`. A sweep whose `ray_start_range` or `ray_gate_spacing` varies from ray to
ray cannot share one `range` coordinate. The decoder returns
`CfRadialError::PerRayGeometry { sweep }`, a documented limitation. No corpus file exercises
either path yet. IRENE and DOW8 have `n_gates_vary = "false"` and constant `ray_start_range`
and `ray_gate_spacing` (checked for this revision), and the two xsapr files have neither
attribute nor variables and no `ray_n_gates`. A test is added when a real file turns up
(real-data rule).

### 7.4 Derived fields

Algorithms write `F32` with `FloatCoding::default()`, so NaN means missing. Quantised outputs
such as classifications may use `U8` with `flag_values` and `flag_meanings`.

---

## 8. Field names and Py-ART aliases (F.1 item 4)

### 8.1 Naming rule

A field gets the name xradar 0.12 gives it for the same file. For formats xradar does not
read, it gets the name that converting with Radx to CfRadial and opening with xradar would
give.

- **NEXRAD Level II:** xradar's `nexrad_mapping` (REF→DBZH, VEL→VRADH, SW→WRADH, ZDR,
  PHI→PHIDP, RHO→RHOHV, CFP→CCORH). Unknown block names stay verbatim (`Other`).
- **NEXRAD Level III radial products:** the FM301 name of the moment (N0B→DBZH, N0G→VRADH,
  ...), with the product code in the field attributes. Derived products keep the ICD mnemonic
  (`CR`, `ET`, `DVL`, `OHA`, ...); hydrometeor classifications are `REC` and the instantaneous
  precipitation rate `RR`. The full mapping of a radial, raster or generic product onto one
  sweep (bin geometry, elevation, ray time, level codings, the raster convention) is the
  `recast_radar_io_level3::volume` module documentation (F.3).
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
3. **Py-ART's readers do not all use the config names.** The ODIM reader (`aux_io`) uses
   `reflectivity_horizontal` (DBZH), `total_power_horizontal` (TH), `velocity_horizontal`
   (VRADH), `velocity` (VRAD), `spectrum_width` (WRAD), `differential_reflectivity`,
   `cross_correlation_ratio`, `differential_phase` and `linear_polarization_ratio` (LDR)
   (A.4). `read_cfradial` keeps variable names verbatim (A.5). The table's Py-ART column
   holds the `pyart.config` defaults (`PyartNames::Config`), and `NameInfo::pyart_odim`
   holds the ODIM reader's names. `PyartNames::Reader` gives, per source format, the names
   that format's Py-ART reader produces, which is the mode F.4 uses to compare against those
   readers. A `to_pyart(..., field_names=...)` binding can also accept a custom mapping.
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
| AH, PIA, ADP, PIDA | unchanged | specific_attenuation, path_integrated_attenuation, specific_differential_attenuation, `path_integrateddifferential_attenuation` (PIDA: pyart 2.2.5's default name, typo included, so Py-ART algorithms find the field; checked with `pyart.config.get_field_name`) |
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
    pub calib_index: Option<Vec<i32>>,            // calib_index(time), FM301 int (301-8a)
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
| calib_index | — | — | `r_calib_index` (int32 in DOW8 and IRENE) | CfRadial `r_calib_index (azimuth)`, float64 after decoding | — |

The existing rule still holds: PRF **codes** from VCP tables never become a physical PRT. The
legacy `ScanLegMetadata` codes move to `ScanDefinition::legs`.

CfRadial 1 carries per-ray variables that are not FM301. None is dropped:

- `ray_start_range` and `ray_gate_spacing` are checked against `range`. A ray-to-ray variation
  is a decode error (7.3). Both are also kept verbatim in `Sweep::extra_vars`, because xradar
  keeps them in the sweep group (DOW8, A.5).
- `georefs_applied`, `georef_time`, `georef_unit_num`, `georef_unit_id` and any other variable
  without a slot go to `Sweep::extra_vars` under their source names, as xradar keeps them.
  Attitude variables (`heading`, `roll`, `pitch`, `drift`, `rotation`, `tilt`) and
  `altitude_agl(time)` go to `PlatformTrack`, where the Py-ART export finds them for its
  `Radar` attributes.
- `measured_transmit_power_h` and `measured_transmit_power_v` go to `Monitoring`. The Xradar
  flavor writes them in the sweep group under those CfRadial names, as xradar does. The WMO
  flavor writes them as `monitoring/radar_measured_transmit_power_h/v` (Table 301-11).
- `r_calib_index` fills `calib_index`. The Xradar flavor writes `r_calib_index`, and the WMO
  flavor writes `calib_index` as `int`.

`ExtraVariable`s with a `"time"` dimension follow the view's ray order, like every other
`(time)` variable.

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
- A parsed string outside Table 301-15 (Radx writes `"not_set"`) becomes `Other(text)` in all
  three enums, and both flavors write it back verbatim. `sweep_mode` already works this way.

---

## 11. Global attributes and root variables (F.1 item 7)

| FM301 | Rust | NEXRAD | ODIM | CfRadial | xradar 0.12 observed | Py-ART `metadata` |
|---|---|---|---|---|---|---|
| `Conventions` | view constant | WMO flavor: "CF-1.8, WMO CF-1.0". Xradar flavor: `provenance.source_conventions` or "None" | same rule | same rule | NEXRAD "None"; ODIM "ODIM_H5/V2_2"; CfRadial "CF-1.6" | "CF/Radial instrument_parameters" |
| `wmo__cf_profile` | view constant | "FM 301-2022" (WMO flavor only) | same | same | not present | not present |
| `title`, `institution`, `references`, `source`, `history`, `comment` | `attrs.*` | `source` = "NEXRAD Level II" | `what/source` goes to `source` | verbatim | "None" placeholders; comment "im/exported using xradar" | "" placeholders; NEXRAD `original_container` "NEXRAD Level II" |
| `instrument_name` | `attrs.instrument_name` | ICAO from the volume header | NOD, else RAD, else WMO | verbatim | "KTLX"; ODIM "None" | "KTLX"; 1999 file "\x00\x00\x00\x00"; ODIM "" |
| `site_name` | `attrs.site_name` | — | PLC | verbatim | CfRadial "CPOLRVP" | CfRadial present |
| `scan_name`, `scan_id` | `scan.name`, `scan.id` (+ `scan_id_text`) | "VCP-212", 212 | `how/task` if present | verbatim ("IRENE_WINDS", "0") | NEXRAD "VCP-212"; 1999 and 2005 files "VCP-0"; CfRadial `scan_id` as an int32 attribute (DOW8) | `vcp_pattern` "212" |
| `platform_is_mobile` | `attrs.platform_is_mobile` | false | false | verbatim | CfRadial "false" | |
| `ray_times_increase` | `attrs.ray_times_increase` | computed | computed | verbatim | CfRadial "true" | |
| `simulated` | `attrs.simulated` | false | false | verbatim or BowEcho export | | |
| `wmo__wsi`, `wmo__id` | `attrs.wmo` | — | WIGOS / WMO from `what/source` | verbatim | | |
| `/volume_number` | `volume_number` | none (the view writes 0, as xradar does) | none | verbatim | 0; CfRadial 395 | |
| `/time_coverage_start`, `/time_coverage_end` | `time_coverage` (exact instants) | first and last ray; the view writes them floored to the second, "YYYY-MM-DDThh:mm:ssZ" | same | verbatim | strings floored to the second, "2024-03-15T00:02:17Z" / "…T00:08:18Z" | `time.units` "seconds since 2024-03-15T00:02:17Z" (the floored first radial, = `time_reference`) |
| source attributes without a slot | `attrs.other` | — | — | `Sub_conventions`, `original_format`, `driver`, `created`, `start_datetime`, `start_time`, `end_datetime`, `end_time`, `n_gates_vary` | not written for CfRadial 1 (A.5) | all kept in `metadata` |
| root variables without a slot | `extra_vars` | — | — | `status_xml`, `grid_mapping` | — | — |
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
`fm301::ExtraAttrs`, not from `Volume`. They keep xradar's Python types (A.2): bools
(`mpda_vcp`, `sails_cut`) as `AttrValue::Bool`, ints (`number_elevation_cuts`,
`rda_build_number`, `super_resolution`) as `Scalar::I64`, and `doppler_velocity_resolution`
as `Scalar::F64`. The WMO flavor writes the bools as "true"/"false".

For Message 1 files the Rust model keeps the NEXRAD location as `None`. It does not write 0
the way xradar and Py-ART do, because 0°N 0°E is a wrong position rather than a missing one.
The view writes the CF `_FillValue`. Py-ART's `station=` option (look the ICAO up in a table)
belongs in `recast-radar-data`'s site catalog.

---

## 12. FM301 view (conformance surface) and binding strategy

### 12.1 The view

```rust
// crates/recast-radar-core/src/fm301.rs

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// Reproduces xradar 0.12 names, attribute strings and attribute types
    /// (`sweep_fixed_angle`, "not_set", "meters per seconds", bool attributes). Used by F.4
    /// conformance and the DataTree binding.
    Xradar012,
    /// The FM301-2022 text: `fixed_angle`, `Conventions`, `wmo__cf_profile`, UDUNITS units,
    /// Table 301-12a names. Used by a future CfRadial 2 writer.
    Wmo2022,
}

/// Ray dimension and ray order. Only a row permutation changes; no data moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstDim {
    /// Dimension `time`; rays in acquisition order (stable sort by `time_s`). This is xradar's
    /// `first_dim="time"` (iesha: rotated so the first ray is at azimuth 136.51, A.4), and the
    /// only choice for `Wmo2022`, because a CF coordinate variable must be monotonic. For
    /// NEXRAD, CfRadial and DORADE, storage order is already acquisition order, so the
    /// permutation is the identity and nothing is reordered.
    Time,
    /// xradar's default `first_dim="auto"`: dimension `azimuth` or `elevation`, rays sorted by
    /// that angle. The dimension choice follows xradar 0.12's rule as observed, including
    /// `azimuth` for the DOW8 RHI (A.5). F.4 pins KTLX 2024, dkrom and DOW8.
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Passthrough {
    /// Write what the flavor's reference writes. For Xradar012 that is xradar 0.12's set:
    /// sweep `extra_vars` and calibration `extra` yes, root `attrs.other` no (A.5). For
    /// Wmo2022 it is FM301 names only.
    Flavor,
    /// Also every `other`, `extra_vars` and `extra` item, verbatim, for lossless CfRadial 2
    /// output.
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewOptions { pub flavor: Flavor, pub first_dim: FirstDim, pub passthrough: Passthrough }

pub struct VolumeView<'a> {
    /// "/" with children `radar_parameters`, `radar_calibration`,
    /// `georeferencing_correction` (when present) and `sweep_<n>`. A sweep group's child is
    /// `monitoring` (Table 301-11).
    pub root: Group<'a>,
    pub warnings: Vec<ViewWarning>,
}

pub struct Group<'a> {
    pub name: Cow<'a, str>,                     // "", "sweep_0", "monitoring", ...
    pub dims: Vec<(Cow<'a, str>, usize)>,       // ("time", 720), ("range", 1832), ("frequency", 1)
    pub variables: Vec<Variable<'a>>,
    pub attrs: Vec<(Cow<'a, str>, AttrValue)>,
    pub children: Vec<Group<'a>>,
}

pub struct Variable<'a> {
    pub name: Cow<'a, str>,
    pub dims: Vec<Cow<'a, str>>,
    pub values: Values<'a>,
    /// The encoded form's attributes, typed. Packing, fill and flag attributes are in the
    /// variable's packed type (7.1).
    pub attrs: Vec<(Cow<'a, str>, AttrValue)>,
}

pub enum Values<'a> {
    /// Contiguous, same shape and ray order as the variable: zero-copy.
    Borrowed(ArrayRef<'a>),
    /// A field read through its mapping: rows taken in `rows` order, native gates padded with
    /// `fill` and repeated `stride` times.
    Mapped { source: FieldSource, native: ArrayRef<'a>, native_gates: usize,
             mapping: GateMapping, out_gates: usize, fill: Scalar, rows: RowOrder },
    /// Small computed or reordered arrays (range centres, ray coordinates in sorted order,
    /// per-sweep scalars broadcast to rays).
    Owned(ArrayBuf),
    Scalar(Scalar),
    Text(Cow<'a, str>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowOrder { Identity, Permutation(Arc<[u32]>) } // one Arc shared per sweep
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldSource { pub sweep: u32, pub field: u32 }

pub enum ArrayRef<'a> { U8(&'a [u8]), U16(&'a [u16]), I8(&'a [i8]), I16(&'a [i16]),
                        I32(&'a [i32]), F32(&'a [f32]), F64(&'a [f64]) }
// Scalar, ArrayBuf and AttrValue are the model types (section 2).

pub enum ViewWarning {
    /// Ray times are not strictly increasing even in acquisition order. The source has no
    /// per-ray times (dkrom: equal ODIM start and end times, which xradar also warns about;
    /// legacy-converted ODIM and JMA, where every ray time is 0). The `time` coordinate is
    /// written anyway, as xradar writes it. A CF writer reports the warning.
    NonMonotonicTime { sweep: u32 },
}

#[derive(Debug, thiserror::Error)]
pub enum ViewError {
    #[error("{path}: attribute {attr} value does not fit the variable's type")]
    OutOfRange { path: String, attr: &'static str },
}

pub trait ExtraAttrs {
    fn root_attrs(&self, flavor: Flavor) -> Vec<(Cow<'static, str>, AttrValue)>;
    fn sweep_attrs(&self, sweep: usize, flavor: Flavor) -> Vec<(Cow<'static, str>, AttrValue)>;
}

pub fn volume_view<'a>(volume: &'a Volume, options: ViewOptions,
                       extra: Option<&'a dyn ExtraAttrs>) -> Result<VolumeView<'a>, ViewError>;

impl VolumeView<'_> {
    /// The same tree with no borrows: every field variable's `Values` is replaced by
    /// `DataRef::Field { source, native_gates, mapping, out_gates, fill, rows }`, and other
    /// values are copied (they are small). Bindings use this (12.2).
    pub fn layout(&self) -> VolumeLayout;
}
```

Fields are always written encoded: integers plus CF attributes, or floats with the source's
`_FillValue` (NaN for derived fields). xarray or the binding does the decoding, which is spec
4.3's "let xarray's CF decoding apply the packing". Building the view is O(rays + fields) per
sweep. The only per-sweep work is checking whether `time_s` is already sorted, plus one
`O(n log n)` sort over ray indices (a few hundred entries) when it is not.

### 12.2 Binding strategy: ownership moves to NumPy

rust-numpy 0.23 can wrap memory that Rust keeps owning only through
`PyArray::borrow_from_array`, which is `pub unsafe fn` (`numpy-0.23.0/src/array.rs` line 340).
Spec principle 2 forbids `unsafe` in our code, with no exception for bindings. The safe,
zero-copy constructors take ownership: `PyArray::from_vec` (line 612) and
`PyArray::from_owned_array` (line 446), which keep the Rust allocation alive inside a
`PySliceContainer`. A `VolumeView<'a>` borrows `&'a Volume`, so it cannot outlive a Python
call. The binding therefore does not expose the view's borrows. It works as follows, with no
`unsafe` and no gate copy:

1. Decode to an owned `Volume` (plus the format metadata).
2. `let layout = fm301::volume_view(&volume, options, extra)?.layout();` records names, dims,
   attributes, small arrays and one `DataRef` per field variable. The view is then dropped.
3. Consume the volume: `for sweep in volume.sweeps { for field in sweep.fields { ... } }` with
   `Field::into_parts` and `FieldData::into_array` (4). Each `Vec<u8>` / `Vec<u16>` /
   `Vec<i8>` / `Vec<i16>` / `Vec<f32>` / `Vec<f64>` goes to `PyArray::from_vec`, then
   `reshape([nrays, ngates])`. For a contiguous array, `reshape` returns a view and copies
   nothing. From then on NumPy owns the memory.
4. When the field's `DataRef` has `RowOrder::Identity`, `start == 0`, `stride == 1` and
   `native_gates == out_gates`, the reshaped array is the variable (KTLX 2024: 76 of 104
   fields, 6.5). Otherwise the variable is an xarray `BackendArray` (wrapped in
   `LazilyIndexedArray`) that holds the native array and applies row order, padding and
   repetition in `__getitem__`, as xradar's own NEXRAD backend array does.
5. The DataTree is built from the layout's encoded attributes and decoded as 12.3 describes.

Consequences, stated so a binding author does not rediscover them:

- The zero-copy path runs one way, from Rust to NumPy. To keep a Rust `Volume` alive (for
  example to run Rust algorithms on it later), a binding either copies arrays when Python
  reads them, or passes NumPy arrays back into Rust through `PyReadonlyArray::as_slice()`,
  which is a safe borrowed slice. An algorithm that needs an owned `Field` then costs one
  copy per input field.
- The model information that the encoded form cannot carry (padding vs below threshold, 7.1)
  is read by the binding from the model before step 3 moves the buffers.
- Rejected alternative: reference-counted buffers kept by Rust and lent to NumPy. That needs
  `borrow_from_array` or a hand-written buffer protocol (also `unsafe` in PyO3), so it would
  need a spec exception.

### 12.3 Decoded form in the binding

The layout is the encoded form. The binding's DataTree builder takes these options:

| Option | Default | Effect |
|---|---|---|
| `decode` | `true` | CF decoding, as `xr.decode_cf` / `mask_and_scale=True` |
| `mask_range_folded` | `true` | gates equal to the range-folded code become NaN (lazily), as Py-ART masks `raw <= 1` |
| `range_folded_variable` | `false` | adds, per field with a range-folded code, a lazy `uint8` variable `<FIELD>_flags(time, range)` with `flag_values = [1]` and `flag_meanings = "range_folded"`, and sets the field's `ancillary_variables = "<FIELD>_flags"` (Table 301-10). Range-folded information then survives masking. Off by default because xradar has no such variable |
| `packed_attrs` | `Encoding` | where packed-unit attributes go after decoding (next list) |
| `first_dim` | `Auto` | `FirstDim` (12.1), mirroring xradar's `first_dim` option, so the output can replace `open_*_datatree()` output |

When `decode` is true, xarray moves `_FillValue`, `scale_factor` and `add_offset` into
`.encoding` but leaves every other attribute in `.attrs`. That includes `_Undetect`,
`valid_range`, `valid_min`, `valid_max`, `flag_values`, `flag_masks` and `flag_meanings`, all
still in packed units on a float variable (verified: A.6). With the default
`packed_attrs = Encoding`, the binding moves those into `.encoding` as well, so no packed
number in `.attrs` can be read as a physical value (for example, by cf_xarray's `.cf.flags`).
Two trade-offs, both verified with xarray 2026.7.0:

- xarray's netCDF writers (`netcdf4` and `h5netcdf` engines) drop unknown `.encoding` keys, so
  a plain `to_netcdf` of the decoded tree loses `valid_range` and the flags. The binding's own
  CfRadial 2 writer writes them from the layout.
- `packed_attrs = Attrs` leaves the attributes where xarray and xradar 0.12 put them (ODIM
  `_Undetect` stays in `.attrs`, A.4). That gives drop-in parity and a correct file on
  `to_netcdf`, at the price of packed-unit numbers in `.attrs`.

Converting `valid_range` to physical `valid_min`/`valid_max` was considered and rejected. CF
requires those in the packed type on packed variables, so writing the tree back would produce
a non-conforming file, and netCDF4-python would compare packed data against physical bounds.

F.4 compares the encoded form only (`mask_and_scale=False` on the xradar side, `decode=false`
on ours), so these options do not affect conformance.

### 12.4 Names that differ by flavor

| Model | `Wmo2022` (FM301-2022 text) | `Xradar012` | CfRadial 1 file (decoder input) |
|---|---|---|---|
| `RadarParameters::antenna_gain_h_db` / `_v_db` | `radar_parameters/antenna_gain_h` / `_v` (Table 301-12a) | `radar_parameters/radar_antenna_gain_h` / `_v` | `radar_antenna_gain_h` / `_v` |
| `RadarParameters::beam_width_h_deg` / `_v_deg` | `radar_parameters/beam_width_h` / `_v` | `radar_parameters/radar_beam_width_h` / `_v` | `radar_beam_width_h` / `_v` |
| `RadarParameters::receiver_bandwidth_hz` | `radar_parameters/receiver_bandwidth` | `radar_parameters/radar_receiver_bandwidth` | `radar_rx_bandwidth` |
| `Sweep::fixed_angle_deg` | `fixed_angle` | `sweep_fixed_angle` | `fixed_angle(sweep)` |
| `Sweep::rays_angle_resolution_deg` | `rays_angle_resolution` | `rays_angle_resolution` | `ray_angle_res(sweep)` |
| `RadarCalibration::<name>` | `radar_calibration/<name>` | `radar_calibration/<name>` | `r_calib_<name>` |
| `RadarCalibration::base_1km_hc_dbz` | `radar_calibration/base_1km_hc` | `radar_calibration/base_1km_hc` | `r_calib_base_dbz_1km_hc` |
| `RadarCalibration::calib_index` | `radar_calibration/calib_index`, byte | as xradar writes it | — |
| `RayVariables::calib_index` | `calib_index(time)`, int | `r_calib_index` | `r_calib_index(time)` |
| `Monitoring::radar_measured_transmit_power_h_dbm` | `monitoring/radar_measured_transmit_power_h` | `measured_transmit_power_h` in the sweep group | `measured_transmit_power_h(time)` |

Section 14 lists the remaining flavor differences (standard names, units, `coordinates`).

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
- **Exhaustive matches:** on `MomentStorage`, which has 3 variants against `FieldData`'s 6
  variants with payloads; and on `MomentType` and `ScanMode` variants whose names differ
  (`Reflectivity` vs `Dbzh`, `Ppi` vs `AzimuthSurveillance`).

No alias of a new type under an old name would compile against that code, so there are no
aliases. The old names remain the **old types**.

### 13.2 Structure

Only the model types move. `lib.rs` keeps everything that is not the model: the module
declarations, `bounded_read` (used by io-nexrad, io and io-dorade as
`recast_radar_core::bounded_read`), the refractivity re-exports, `EARTH_RADIUS_M`,
`EFFECTIVE_EARTH_RADIUS_M`, `beam_height_above_radar_m`, `beam_ground_range_m`, and their two
tests. `refractivity.rs` imports `crate::EARTH_RADIUS_M` and `crate::beam_height_above_radar_m`,
so those have to stay at the crate root. The module declarations stay in `lib.rs` because
`mod field_names;` written inside `legacy.rs` would resolve to `src/legacy/field_names.rs`.

```rust
// crates/recast-radar-core/src/lib.rs during F.2 and F.3
pub mod bounded_read;            // unchanged
mod field_names;                 // unchanged path; `canonical_moment` gains the cfg_attr below
mod refractivity;                // unchanged
pub mod legacy;                  // src/legacy.rs
pub mod model;
pub mod fm301;

#[allow(deprecated)]
pub use legacy::*;               // every old name at its old path; un-migrated `use` lines compile
#[allow(deprecated)]
pub use field_names::canonical_moment;
pub use refractivity::{ /* unchanged */ };
pub use model::{Volume, Sweep, Field, FieldName, FieldData, Quantity, Polarization, GateMapping,
                RangeCoord, Rays, RayVariables, SweepMode, /* ... */};

pub const EARTH_RADIUS_M: f64 = 6_371_000.0;                        // unchanged, stays here
pub const EFFECTIVE_EARTH_RADIUS_M: f64 = EARTH_RADIUS_M * 4.0 / 3.0; // unchanged
pub fn beam_height_above_radar_m(/* unchanged */) -> f64;
pub fn beam_ground_range_m(/* unchanged */) -> f64;
```

```rust
// crates/recast-radar-core/src/legacy.rs
//! Pre-FM301 model types, moved from lib.rs @ 1989a03. Deleted at the end of F.3.
#![allow(deprecated)] // the module's own impls, `merge_radar_volumes`, conversions and tests
                      // use the deprecated items; rustc lints uses inside the defining crate

// Every model item keeps its definition and impls unchanged and gains one attribute:
#[cfg_attr(recast_legacy_deprecation, deprecated(note = "FM301 migration: see docs/design/fm301-model.md section 5"))]
pub struct RadarVolume { /* unchanged */ }
// Likewise: RadarSite, ElevationCut, Radial, GateRange, RadialStatus, MomentType, MomentGrid,
// MomentStorage, MomentRow, MomentGridError, VcpInfo, ScanLegMetadata, ScanMode,
// VolumeMetadata, RayInstrumentMetadata, RayInstrumentMetadataAlignmentError, ProductId,
// MergeReport, merge_radar_volumes, CUT_ELEVATION_MATCH_TOLERANCE_DEG.
// The lib.rs tests that exercise these items move here too.
// field_names.rs: `canonical_moment` gains the same attribute, and the file gains
// `#![allow(deprecated)]` because it names `crate::MomentType`.
```

Deprecating a struct also lints every access to its fields, including on values whose type
the caller never names (`let v = decode(..); v.cuts.len()` warns "use of deprecated field").
The fields therefore need no attributes of their own (tested, 13.4).

The residue lives outside the model, so `Sweep`'s derived `PartialEq`, serde and struct
literals are unaffected, and nothing breaks when the shim is removed:

```rust
/// Everything `volume_from_legacy` could not represent exactly (5.5).
#[derive(Clone, Debug, PartialEq)]
pub struct LegacyResidue {
    pub volume_time: DateTime<Utc>,
    pub scan_mode: Option<ScanMode>,
    pub pulse_width_us: Option<f32>,
    pub unambiguous_range_km: Option<f32>,
    /// One per sweep, in sweep order.
    pub sweeps: Vec<SweepResidue>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SweepResidue {
    /// `Radial::time_offset_ms` as written: ms of day (NEXRAD), ms since volume start
    /// (CfRadial), ms since sweep start (DORADE), 0 (ODIM, JMA).
    pub time_offset_ms: Vec<i32>,
    pub radial_gate_ranges: Vec<GateRange>,
    pub radial_status: Vec<Option<RadialStatus>>,
    pub nyquist_velocity_mps: Vec<Option<f32>>,
    /// Verbatim, 16 bytes per ray (covers km values, `Some(NaN)`, pulse counts above
    /// `i32::MAX`, and all-`None` vectors).
    pub ray_instrument_metadata: Vec<RayInstrumentMetadata>,
    /// One per field, in `Sweep::fields` order.
    pub fields: Vec<FieldResidue>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldResidue {
    /// The legacy key, so the reverse is exact even after a naming collision (5.4).
    pub moment: MomentType,
    /// Preserves each decoder's start-vs-centre convention and metre rounding (6.6).
    pub gate_range: GateRange,
    pub nodata: Option<u16>,
    pub range_folded: Option<u16>,
    /// `radial_indices` when they were not the identity (the rows were scattered, 5.3).
    pub radial_indices: Option<Vec<usize>>,
}

/// Legacy conventions of the decoder that produced a volume: gate-range meaning (6.6),
/// `time_offset_ms` meaning (5.2), naming (5.4). Derived from `Provenance::source_format`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegacyConvention { Nexrad, Odim, CfRadial, Dorade, Jma, Generic }

#[derive(Debug, thiserror::Error)]
pub enum LegacyConversionError {
    #[error("sweep {sweep}: field {field} gates do not align with the sweep range")]
    UnalignedGates { sweep: usize, field: String },
    #[error("sweep {sweep}: explicit (non-uniform) range has no legacy form")]
    ExplicitRange { sweep: usize },
    #[error("sweep {sweep}: {detail}")]
    Shape { sweep: usize, detail: String },
}

/// Moves every U8/U16/F32 buffer; no gate data is copied.
pub fn volume_from_legacy(v: RadarVolume) -> Result<(Volume, LegacyResidue), LegacyConversionError>;
/// With a residue: exact inverse (5.5). Without one (natively decoded volumes): derives the
/// legacy values from `convention` (13.3). I8/I16/F64 fields expand or narrow to legacy F32;
/// that is the only copy, and today's CfRadial and ODIM decoders already do it.
pub fn legacy_from_volume(v: Volume, residue: Option<&LegacyResidue>,
                          convention: LegacyConvention) -> Result<RadarVolume, LegacyConversionError>;

impl TryFrom<RadarVolume> for Volume { /* volume_from_legacy, residue dropped */ }
impl TryFrom<Volume> for RadarVolume { /* legacy_from_volume(v, None, source_format.into()) */ }

// Borrowed conversions for the legacy-signature wrappers in migrated crates. These clone.
pub fn sweep_from_cut(cut: &ElevationCut, meta: &VolumeMetadata, number: u32)
    -> Result<(Sweep, SweepResidue), LegacyConversionError>;
pub fn cut_from_sweep(sweep: &Sweep, residue: Option<&SweepResidue>, convention: LegacyConvention)
    -> Result<ElevationCut, LegacyConversionError>;
/// Adds the grid as a field of `sweep` (attaching its geometry) and returns its index.
pub fn field_from_grid(grid: &MomentGrid, sweep: &mut Sweep)
    -> Result<(usize, FieldResidue), LegacyConversionError>;
pub fn grid_from_field(field: &Field, range: &RangeCoord, residue: Option<&FieldResidue>,
                       convention: LegacyConvention) -> MomentGrid;

impl MomentType { pub fn to_field_name(&self, convention: LegacyConvention) -> FieldName; } // 5.4 table
impl FieldName  { pub fn to_legacy_moment(&self, convention: LegacyConvention) -> MomentType; } // 5.4 reverse

/// Structural equality with every f32/f64 compared by `to_bits`, NaN payloads included.
/// `Err` names the first difference (`cuts[3].moments[Velocity].storage[1042]`).
/// `PartialEq` cannot serve here: NaN != NaN (13.4).
#[doc(hidden)]
pub fn bit_identical(a: &RadarVolume, b: &RadarVolume) -> Result<(), String>;
```

An additive edit to the root `Cargo.toml` stops the opt-in cfg from triggering warnings:

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
unexpected_cfgs = { level = "warn", check-cfg = ["cfg(recast_legacy_deprecation)"] }
```

The workspace table applies only to crates that declare `[lints] workspace = true`. Today
`recast-radar-io-nexrad` and `recast-radar-render` do not (they still contain `unsafe`), and
those are exactly where F.3 adds `cfg_attr(recast_legacy_deprecation, ..)`. Stream D's D.1
adds `[lints] workspace = true` to both; the `safety` branch already has it. Whichever lands
first decides what F.3 does:

- If D.1 has reached `main` when F.3 touches those crates, nothing more is needed.
- Otherwise the crate gets its own table,
  `[lints.rust] unexpected_cfgs = { level = "warn", check-cfg = ["cfg(recast_legacy_deprecation)"] }`.
  That table is removed at the sync that brings D.1 in, because Cargo rejects the combination
  ("cannot override `workspace.lints` in `lints`"; tested, 13.4).

Deprecation is opt-in because an unconditional `#[deprecated]` would put roughly 1,500
warnings into crates owned by streams A, D, E and G. That would break the "do not add new
warnings" rule and G.2's planned `clippy -D warnings`. Without the cfg, nothing is deprecated
and nothing warns.

### 13.3 Legacy wrappers in migrated crates

When a crate's public function migrates, its old signature stays during F.3 as a thin wrapper.
All wrappers live in one module that allows deprecation, so the crate's own gate ignores them:

```rust
// Example: recast-radar-correct after fm301-algo migrates (lib.rs)
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))] // this crate is migrated

pub fn dealias_sweep(sweep: &Sweep, opts: &DealiasOptions) -> Result<Field, DealiasError>;

/// Legacy signatures, kept until shim removal. Only this module names legacy items.
#[allow(deprecated)]
pub mod legacy_api {
    use super::*;
    use recast_radar_core::{legacy, ElevationCut, MomentGrid, VolumeMetadata};

    #[cfg_attr(recast_legacy_deprecation, deprecated(note = "use dealias_sweep"))]
    pub fn dealias_cut(cut: &ElevationCut, meta: &VolumeMetadata, opts: &DealiasOptions)
        -> Result<MomentGrid, DealiasError> {
        let (sweep, _residue) = legacy::sweep_from_cut(cut, meta, 0)?;
        let field = dealias_sweep(&sweep, opts)?;
        Ok(legacy::grid_from_field(&field, &sweep.range, None, legacy::LegacyConvention::Generic))
    }
}
#[allow(deprecated)]
pub use legacy_api::*; // old paths keep resolving; callers still get the deprecation warning
```

A migrated crate that still has to call an un-migrated dependency's legacy API (render
calling `recast_radar_correct::dealias_velocity_grid` before fm301-algo lands) makes those
calls from a private `#[allow(deprecated)] mod legacy_bridge`. The bridge goes when the
dependency migrates.

Decoders follow the same pattern:

- `read_volume(..) -> Volume` is the native decoder.
- `decode_volume_from_bytes(..) -> RadarVolume` stays in `legacy_api`, implemented as
  `legacy::legacy_from_volume(read_volume(..)?, None, LegacyConvention::Nexrad)`.
- A natively decoded volume has no residue, so the reverse derives the legacy values from the
  decoder's legacy convention:
  - gate ranges: the 6.6 table;
  - NEXRAD `time_offset_ms`: ms of day of the collection time;
  - `volume_time`: the NEXRAD volume header time from `NexradMetadata`;
  - `Radial::gate_range`: the first field's native geometry;
  - `radial_status`: `NexradMetadata`'s per-ray status, which io-nexrad keeps for the
    duration of the shim;
  - names: 5.4;
  - `scan_name` and `scan_id`: `None` for NEXRAD, as the legacy decoder left them;
  - ODIM: undetect codes remapped onto nodata in place (the legacy decoder did this; the
    buffer is moved and modified, not copied).

Each io crate's existing real-file tests keep passing through these wrappers, which shows the
native decoders reproduce legacy output. A wrapper that changes values is caught there.
Natively decoded ODIM and CfRadial gate positions intentionally differ by half a gate (6.6);
those tests change in F.3 with that justification recorded.

**F.3 order.** The gate does not depend on migration order (13.4). API availability does:

| Sub-worktree | Order |
|---|---|
| fm301-io | io-nexrad, io-odim, io-cfradial, io-dorade, io-jma (independent); then io (router); then data |
| fm301-algo | filters and correct (depend only on core); then retrieve and map (use correct; map also uses filters); then track (uses correct, map, retrieve) |
| fm301-render | render once fm301-algo's `correct` migration is merged into `fm301` and synced, or earlier through `legacy_bridge`; bench (uses io, correct, render) last, after fm301-io and fm301-algo merge back |

### 13.4 Acceptance and removal

**F.2**

- `legacy.rs` holds exactly the model items listed in 13.2. Their definitions and impls are
  unchanged apart from the added `cfg_attr` lines and the module header. This is checked by a
  script that strips those lines and compares each item's text against
  `git show 1989a03:crates/recast-radar-core/src/lib.rs`. A `git diff -M` rename cannot be
  the test, because `lib.rs` keeps the geometry, refractivity and module declarations.
- `RUSTFLAGS="--cfg recast_legacy_deprecation" cargo check -p recast-radar-core --all-targets`
  reports no deprecation warning from core itself.
- Round trip: for every real corpus file that `recast-radar-io` decodes,
  `let (vol, res) = volume_from_legacy(v.clone())?;` followed by
  `bit_identical(&legacy_from_volume(vol, Some(&res), conv)?, &v)` returns `Ok`. There are no
  tolerances (5.5). `PartialEq` is not used: NaN != NaN, so `v == v.clone()` is already false
  for NaN-bearing files. Review evidence: IRENE CfRadial (50,697 NaN gates), DOW8 (141,702)
  and espdg ODIM (360,203) fail `v == v.clone()`, while JMA N5 and dkrom pass.
- All crates compile unchanged, `cargo test --workspace` passes, and checksums are identical,
  since nothing uses the new path yet.

**F.3**

- A migrated crate has `#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]` at its
  root, and so does each integration-test, bench and example target it migrates (each of those
  is its own crate). The gate is
  `CARGO_TARGET_DIR=target/legacy-gate RUSTFLAGS="--cfg recast_legacy_deprecation" cargo check -p <crate> --all-targets`,
  which must succeed. The separate target directory keeps the RUSTFLAGS change from
  invalidating the normal build cache. The deny applies only inside crates that declare it.
  Un-migrated dependencies still compile and only warn, so the gate works in any sub-worktree
  regardless of what the others have migrated.
- A companion grep, `grep -rn "RadarVolume\|ElevationCut\|MomentGrid\|MomentType\|legacy::" crates/<crate>`,
  shows matches only inside `legacy_api` and `legacy_bridge`.
- Checksums stay identical after fm301-render and bench move to `Volume`.
- Single-core decode time is measured on the native `read_volume`, before and after.

The gate design was tested in a standalone workspace, committed as
`tools/fm301_probe/shim_gate` (reproduce with `bash tools/fm301_probe/shim_gate/run.sh`). It
has four crates:

- `core_t`: `#[allow(deprecated)] mod legacy` with cfg-gated deprecations on structs and
  functions, and none on fields.
- `algo_t`: migrated, with the deny and a `legacy_api` module.
- `render_t`: un-migrated, using legacy types, algo's wrappers and field accesses.
- `bench_t`: migrated, depending on `render_t`.

The workspace also has the check-cfg entry. With the cfg:

- `cargo check --workspace` finishes. `render_t` gets 7 warnings, including field accesses on
  values whose type it never names.
- `-p algo_t` and `-p bench_t` pass.
- After adding one legacy use to `algo_t` outside `legacy_api`, `-p algo_t` fails
  ("error: use of deprecated struct `core_t::MomentGrid`") and so does `-p bench_t`, which
  depends on `algo_t`. That is correct, since `algo_t` claims to be migrated.
- The reviewer's alternative,
  `cargo rustc -p algo_t --lib -- --cfg recast_legacy_deprecation -D deprecated`, passes even
  with that stray use: `core_t` is compiled without the cfg and so has no deprecation
  attributes. It is not used.
- A manifest with both `[lints] workspace = true` and `[lints.rust]` fails to parse.

**Removal (the last F.3 step)**

- Delete `legacy.rs`, `canonical_moment` (replaced by `Quantity::classify`), every
  `legacy_api` and `legacy_bridge` module, the `deny(deprecated)` crate attributes, and every
  `unexpected_cfgs` check-cfg entry (workspace and per-crate).
- Rename `read_volume*` to the final names.
- `grep -rn "legacy::\|legacy_api\|RadarVolume\|MomentGrid\|ElevationCut\|recast_legacy_deprecation" crates Cargo.toml`
  finds nothing.

---

## 14. Where xradar 0.12, FM301-2022 and Py-ART disagree, and what this model does

| Topic | FM301-2022 text | xradar 0.12 | Py-ART 2.2.5 | Model / view |
|---|---|---|---|---|
| Ray dimension | `time` is primary (a CF coordinate, so monotonic) | `azimuth` (PPI) or `elevation` (RHI), rays sorted; `time` in acquisition order with `first_dim="time"` | a 1-D ray index across the volume | `FirstDim::Time`: `time`, acquisition order (a row permutation; identity for NEXRAD, CfRadial, DORADE). `FirstDim::Auto` (Xradar flavor): xradar's default. Model storage order never changes (12.1) |
| Time reference | `seconds since YYYY-MM-DDThh:mm:ssZ`, whole seconds (Table 301-6b) | `time` as datetime64 | first radial's time floored to the second | `time_reference` whole seconds; fraction in `time_s` (section 2) |
| Attribute types | `_Undetect`, `flag_values`, `flag_masks` "same as field data" (Table 301-10) | NEXRAD attributes as Python bool/int/float; `to_cfradial2` cannot write the bools | NEXRAD `vcp_pattern` as text | typed `AttrValue`; flag and range attributes in the packed type; bools as text in the WMO flavor |
| Fixed angle variable | `fixed_angle` | `sweep_fixed_angle` | `fixed_angle` (volume array) | Xradar flavor: `sweep_fixed_angle`; WMO flavor: `fixed_angle` |
| `range` units and meaning | "metres"; the attribute is named `meters_to_center_of_first_gate` but described as "range to start of first gate" | "meters", centre (ODIM 750 = rstart 500 + 250) | "meters", centre in the data; the ODIM reader's attribute says 0.0 while the data starts at 250 | centres; "meters" in the Xradar flavor, "metres" in the WMO flavor |
| azimuth / elevation `standard_name` | `sensor_to_target_azimuth_angle` / `sensor_to_target_elevation_angle` | `ray_azimuth_angle` / `ray_elevation_angle` | `beam_azimuth_angle` / `beam_elevation_angle` | per flavor |
| Unknown `follow_mode` / `prt_mode` | no "unknown" value | "not_set" | CfRadial passthrough | "not_set" when `None`; other source strings verbatim (`Other`) |
| NEXRAD sentinels | `_FillValue` and `valid_range` mandatory (WMO-CF.5.2.14, 5.2.15); `_Undetect` for radiated bins without a valid echo (Table 301-10) | no `_FillValue`, no `_Undetect` | masks raw <= 1 | `_FillValue = 0` and `_Undetect = 0`, range-folded flag, `valid_range`; the model tells undetect from missing (7.1) |
| Undetect after decoding | — | ODIM `_Undetect` left in `.attrs` of the decoded variable, in packed units | masked | binding moves it to `.encoding` by default (12.3) |
| Field `coordinates` attribute | "elevation azimuth range" | "elevation azimuth range latitude longitude altitude time" | "elevation azimuth range" (NEXRAD); "time range" (CfRadial) | per flavor |
| Different gate geometry within a sweep | one `range` | first moment's start and spacing, maximum count; misplaces moments | resampled to one volume range | section 6 |
| Different range lengths across sweeps | per-sweep `range` allowed | per sweep | one volume range | per sweep; a Py-ART export pads to the volume maximum |
| `wmo__parameter_uri` / `wmo__parameter_name` | mandatory on data variables (WMO-CF.5.2.9) | not present | not present | omitted; no registry entries identified (section 15) |
| ODIM TH | linear total power (Table 301-9) | labelled linear, unitless | ODIM reader: `total_power_horizontal` | name TH; attributes in dBZ per ODIM |

---

## 15. Deviations from spec and plan; open questions

**Deviations** (each justified in the section cited):

1. **Where format metadata lives.** Spec 4.2 puts "format-specific metadata in typed
   extension structs" inside `Volume`. Here the structs are typed but sit beside `Volume`
   (section 2), so that `core` does not depend on `io-*` crates (spec 4.1 dependency rule).
   This is consistent with plan A.4.
2. **Storage types.** Spec 4.2 lists `u8`, `u16` or `f32`. `I8` and `I16` are added so that
   CfRadial `byte`/`short` packed data is not expanded (IRENE, DOW8; 7.3). `F64` is added so
   float64 sources are not narrowed (espdg; 7.2), which keeps raw hashes comparable with
   xradar and avoids a narrowing pass at decode.
3. **Canonical names.** Spec 4.2's "canonical names are the FM301/xradar short names" is read
   as "whatever xradar names it", which means verbatim names for ODIM, CfRadial and DORADE
   (8.1).
4. **Type aliases.** Plan F.1 asks for them; there are none, because aliases cannot keep
   legacy field syntax compiling (13.1). Deprecation is gated behind a `cfg`.
5. **Range folded.** Spec 4.2's "raw 1 is the range-folded flag" becomes CF `flag_values`,
   which CF decoding does not mask. The binding masks it by default and can keep it as an
   ancillary flag variable (7.1, 12.3).
6. **Raw 0.** Spec 4.2 says "raw 0 is `_FillValue` (below threshold)". It stays `_FillValue`,
   and is also FM301 `_Undetect`, because below threshold means radiated without a valid echo
   (Table 301-10). The model separates undetect from missing (7.1). The CF masking the spec
   asks for is unchanged.
7. **"Without copying" in spec 4.3.** Spec 4.3 says the model exposes `&[u8]`/`&[u16]`/`&[f32]`
   "so a future PyO3 layer can hand NumPy arrays over without copying". Borrowed slices do
   exist (`Field::row`, `ArrayRef`). But lending them to NumPy needs an `unsafe fn` in
   rust-numpy, which spec principle 2 forbids. The no-copy handover therefore moves ownership
   (12.2). Borrowed slices remain the Rust API and the conformance surface.

**Open questions** (not settled by the review, which raised no finding on them)

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

## 16. Review resolutions

An independent review of the first version (commit `fd2b7b1`) returned `approve: false`, with
0 blockers, 6 major and 10 minor findings. Every finding is resolved in this revision. None is
rejected outright.

- Two of the reviewer's suggested alternatives were tested and not adopted: finding 1's
  `cargo rustc` scoping, and finding 11's forward naming rule.
- One factual claim in finding 5 was corrected: xradar 0.12 drops CfRadial root attributes
  that Py-ART keeps.
- Where the review offered options, the chosen one is named below.

"Checked" marks evidence re-run for this revision. The rest is the reviewer's own evidence,
which this revision relies on without re-running.

None of the resolutions adds work at decode:

- `undetect` and `attr_width` are metadata.
- `absent_rows` is an empty `Vec` in the common case.
- `push_row_*(ray, ..)` adds one integer comparison per row.
- `F64` and verbatim float fills remove a narrowing or rewrite pass.
- Native ODIM decoding stops remapping undetect onto nodata, removing a pass.
- Ray reordering happens in the view.
- The residue exists only in the shim.

| # | Severity | Finding | Resolution | Sections |
|---|---|---|---|---|
| 1 | major | The F.3 `-D deprecated` gate cannot pass: rustc lints uses inside core; the kept wrappers name legacy types; `RUSTFLAGS` reaches path dependencies in other sub-worktrees | **Accepted, with a different gate.** `legacy.rs` has `#![allow(deprecated)]`. Wrappers live in `#[allow(deprecated)] pub mod legacy_api` with an allowed re-export. Calls into un-migrated dependencies go through `legacy_bridge`. A migrated crate declares `#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]`, and the gate sets only the cfg through `RUSTFLAGS`, with no `-D`, so un-migrated dependencies warn and do not fail. Checked with `tools/fm301_probe/shim_gate/run.sh`: migrated crates pass; a stray use outside `legacy_api` fails in its own crate and in dependents; a migrated crate over an un-migrated dependency passes; deprecating a struct also flags field accesses. **Rejected alternative:** `cargo rustc -p <crate> -- --cfg recast_legacy_deprecation -D deprecated` passes vacuously even with a stray use, because core is compiled without the cfg (script step 7). The dependency order was adopted for API availability only; the gate does not depend on order | 0 item 8; 13.2; 13.3 (F.3 order); 13.4 |
| 2 | major | The F.2 round trip under `PartialEq` fails on NaN-bearing files (IRENE, DOW8, espdg) | **Accepted.** `legacy::bit_identical` compares every float by `to_bits` and names the first difference. Exactness comes from the residue (5.5), so there is no tolerance list | 5.5; 13.2; 13.4 |
| 3 | major | NEXRAD raw 0 is FM301 `_Undetect`, but was coded only as missing | **Accepted.** `undetect: Some(0)` alongside `fill_value: Some(0)`. The `Field::gate` resolution order is specified, with undetect before fill. `Field::absent_rows` marks rows the source did not provide (`Missing`). The view writes `_FillValue = 0` and `_Undetect = 0`, pads with `_FillValue`, and documents that the encoded form cannot separate the two. Recorded as deviation 15.6. Checked: FM301 Table 301-10 text; xarray keeps `_Undetect` as an attribute after `decode_cf` (`review_checks.py` part 3). One refinement: padding is not a native gate, so `Field::gate` returns `None` beyond the native extent, and the view's `_FillValue` is what marks it | 0 item 5; 4; 7.1; 14; 15 |
| 4 | major | After xarray decoding, `flag_values` and `valid_range` stay in `.attrs` in packed units | **Accepted, with an explicit decoded form.** `packed_attrs = Encoding` (default) moves `_Undetect`, `valid_range`, `valid_min/max` and `flag_*` into `.encoding`; `Attrs` keeps xradar's placement. `range_folded_variable = true` adds a lazy `<FIELD>_flags` ancillary variable. The netCDF4-python vs xarray reading difference is documented. F.4 compares the encoded form. Checked: xarray leaves those attributes in `.attrs`, and, a new finding, drops unknown `.encoding` keys on `to_netcdf` with both the `netcdf4` and `h5netcdf` engines. That trade-off is documented in 12.3. **Not adopted:** converting `valid_range` to physical `valid_min/valid_max`, because CF requires the packed type and a write-back would be non-conforming | 7.1; 12.3; A.6 |
| 5 | major | No passthrough for unmodelled metadata, so CfRadial and DORADE conversion is lossy | **Accepted.** Added `GlobalAttrs::other`, `Volume::extra_vars`, `Sweep::extra_vars`, `Sweep::other`, `RadarCalibration::extra` (named as xradar names them), and `PlatformTrack` attitude vectors (heading, roll, pitch, drift, rotation, tilt, `altitude_agl`); `georef*` go to `extra_vars`. `ViewOptions::passthrough`. **Correction to the finding (checked):** xradar 0.12 does not keep `Sub_conventions`, `original_format`, `driver`, `created`, `start_*`, `end_*` or `n_gates_vary` for CfRadial 1 (DOW8 root has 14 attributes). Only Py-ART keeps them, so the Xradar flavor writes root `other` only with `Passthrough::All`. xradar does keep the per-ray `ray_start_range`, `ray_gate_spacing` and `georef*` variables and the non-FM301 calibration entries (checked) | 0 item 10; 1; 2; 3; 9; 11; 12.1; A.5 |
| 6 | major | Zero-copy NumPy exposure conflicts with the no-unsafe rule | **Accepted: option (a), ownership moves.** Added `Field::into_parts`, `FieldData::into_array` and `VolumeView::layout()`. The binding moves each `Vec` with `PyArray::from_vec` and reshapes; non-trivial mappings use a lazy backend array. `VolumeView` stays the Rust conformance surface. Checked in `numpy-0.23.0`: `array.rs` line 340 `pub unsafe fn borrow_from_array`, line 446 `pub fn from_owned_array`, line 612 `pub fn from_vec`, and `borrow/mod.rs` line 267 safe `as_slice`. **Not adopted:** option (b), shared buffers plus one audited unsafe site, which would need a spec exception. Recorded as deviation 15.7 | 0 item 9; 4; 6.5; 12.2; 15 |
| 7 | minor | `unexpected_cfgs` check-cfg misses io-nexrad and render | **Accepted.** Checked: `grep -L '^\[lints\]'` lists exactly those two, and the `safety` branch (D.1) already adds `[lints] workspace = true` to both. A per-crate `[lints.rust]` table is added only if D.1 has not landed, and removed at that sync, because Cargo rejects both tables together (script step 8). No `build.rs` | 13.2 |
| 8 | minor | The time reference has sub-second precision, but FM301 and Py-ART use whole seconds | **Accepted.** `time_reference` is whole seconds, with the fraction in `time_s`. Sources that state a reference (CfRadial `time.units`) keep it; NEXRAD uses the first radial floored; a differing header time stays in `NexradMetadata`; legacy `volume_time` goes to the residue. Checked: Py-ART `get_times` uses `seconds=int(secs[0])`, units "seconds since 2024-03-15T00:02:17Z", first values 0.182, 0.204 | 0 item 2; 2; 5.1; 5.2; 5.5; 11 |
| 9 | minor | The `time` coordinate is non-monotonic for ODIM and JMA storage order; xradar parity needs `first_dim` | **Accepted: the view reorders through a row permutation.** `FirstDim::Time` gives acquisition order (the only choice in the WMO flavor); `FirstDim::Auto` gives xradar's default. The model's storage order is unchanged, so decode cost is unchanged. `ViewWarning::NonMonotonicTime` covers sources without per-ray times. The binding exposes `first_dim`. **Not adopted:** a non-coordinate ray dimension with `time` as an auxiliary coordinate, which would not match xradar's `first_dim="time"` layout (dimension `time`, A.2) | 0 item 2; 12.1; 12.3; 14 |
| 10 | minor | Unit conversions are inexact beyond the one listed tolerance | **Accepted, using the reviewer's residue option instead of a tolerance list.** Every non-invertible value is kept (5.5) and used only while the model still holds its forward image. The audit found four more cases, now covered: legacy `vcp` → `scan.name/id` (moved to native decoding), `scan_mode: None`, `Some(NaN)` in per-ray options, and all-`None` `ray_instrument_metadata` | 5.1; 5.2; 5.5 |
| 11 | minor | The reverse name mapping ("via Quantity", CFP) collides or loses names | **Accepted, with a different collision rule.** The residue records each field's `MomentType`. `to_legacy_moment` is the exact inverse for the seven variants and `Ccorh` → `CFP` (NEXRAD only), and `Unknown(as_str())` otherwise. **Suggested forward rule not adopted:** mapping `Unknown("DBZH")` to `Other("DBZH")` would still give two variables named `DBZH` by `as_str()`, and `Other` must never hold a known spelling. Instead, `Unknown` names are assigned first, and a colliding canonical variant becomes `Other("<Variant>")` with its quantity kept | 4; 5.4; 13.2 |
| 12 | minor | `AttrValue` and `LinearTransform` lose types that CF and xradar depend on | **Accepted.** Typed `Scalar`, `ArrayBuf` and `AttrValue { Text, Bool, Scalar, Array }`. Flag, range and fill attributes are written in the packed type; `CfScaleOffset::attr_width` added. Checked: xradar NEXRAD attribute types (`review_checks.py` part 2); appendix A.2 corrected | 2; 4; 7.3; 11; A.2 |
| 13 | minor | FM301 naming and structure gaps (radar_parameters names, `polarization_sequence`, `flag_masks`, `calib_index` type, monitoring group, enum `Other`) | **Accepted.** 12.4 per-flavor name table; `Sweep::polarization_sequence`; `FieldAttrs::flag_masks`; `Group::children`; `Other(Box<str>)` on `FollowMode`, `PrtMode` and `PolarizationMode`; `calib_index` held as i32 and written as byte (301-14a) or int (301-8a) in the WMO flavor. Checked: WMO-No. 306 Tables 301-8a, 301-10, 301-12a and 301-14a; CfRadial `r_calib_index` is int32 in DOW8 and IRENE; xradar writes `radar_receiver_bandwidth` from the file's `radar_rx_bandwidth` | 1; 2; 3; 4; 10; 12.1; 12.4 |
| 14 | minor | Py-ART alias table, classification and Py-ART conformance details | **Accepted.** PIDA alias is `path_integrateddifferential_attenuation` (checked). `PyartNames::{Config, Reader}` and `NameInfo::pyart_odim`. `Quantity::classify(name, standard_name)` tries standard name, then Py-ART names, then suffixes (xsapr `standard_name` checked; DOW8's name-as-standard-name falls through). F.4 uses `linear_interp=False` for coarse-reflectivity volumes. `FieldName` serde goes through `as_str`/`parse` | 4; 6.5; 8.2; 8.3 |
| 15 | minor | `legacy.rs` and `SweepResidue` mechanics are underspecified or contradictory | **Accepted.** Only the model types move. `lib.rs` keeps `bounded_read`, refractivity, geometry and the module declarations (checked: `refractivity.rs` imports `crate::EARTH_RADIUS_M` and `crate::beam_height_above_radar_m`, and `field_names.rs` imports `crate::MomentType`). The acceptance compares item text instead of a `git diff -M` rename. The residue moved out of `Sweep` into `LegacyResidue`, returned beside the `Volume`, and `Sweep::legacy` is gone | 0 item 8; 3; 13.2; 13.4 |
| 16 | minor | Float and ragged-source handling is ambiguous | **Accepted.** One rule for floats: stored verbatim, with the source fill as `_FillValue` and no rewrite pass. `FieldData::F64` added (espdg float64 checked, gain 1, offset 0); `FloatCoding::transform` covers ODIM float planes with other gain or offset. `push_row_*(ray, ..)` fills gaps as `absent_rows`. `n_gates_vary = "true"` is padded at decode; ray-to-ray geometry changes return `CfRadialError::PerRayGeometry` (a documented limitation). Checked: no corpus file uses either path | 0 item 4; 3; 4; 7.2; 7.3 |

---

## 17. F.2 implementation notes

F.2 implemented this note in `recast-radar-core`: `src/model/` (sections 2 to 4, 9, 10, plus
`merge_volumes`), `src/fm301/` (section 12.1 and `layout()`), `src/legacy.rs` and
`src/legacy/{convert,bit_identical}.rs` (section 13.2). Where the code differs from the text
above, the code is authoritative and the difference is listed here.

**API refinements**

1. `legacy::field_from_grid(grid, sweep, convention)` takes the `LegacyConvention`. The
   legacy `first_gate_m` means the gate start for ODIM and CfRadial and the centre elsewhere
   (6.6), so attaching a grid's geometry needs the convention.
2. `lib.rs` re-exports the legacy model items by an explicit list, not `pub use legacy::*`,
   so the conversion items (`volume_from_legacy`, `LegacyResidue`, ...) stay under
   `recast_radar_core::legacy::` only. Every old path still resolves.
3. The borrowed `legacy::sweep_from_cut` / `cut_from_sweep` have no volume time, so their ray
   times are relative: `time_s = time_offset_ms / 1000` and back. `volume_from_legacy` /
   `legacy_from_volume` use the per-decoder rules of 5.2.
4. `FieldResidue` gained `name` (a residue applies to the field of that name, not to a
   position, so a migrated algorithm that adds or reorders fields cannot misapply it) and
   `float_scale_offset` (legacy `F32` grids built by algorithm crates carry `scale`/`offset`
   other than 1/0, which the float coding has no slot for).
5. `Values::Mapped` and `DataRef::Field` carry `nrays`, `Variable` carries
   `source: Option<FieldSource>`, and `VolumeView` keeps a private reference to its volume
   (for `layout()` and `volume()`). A field with zero native gates would otherwise have no
   row count.
6. `NameInfo::xradar: Option<XradarAttrs>` holds xradar 0.12's `sweep_vars_mapping` entry
   verbatim (including its `ZV` standard-name typo); the Xradar flavor writes
   `standard_name`, `long_name` and `units` from it, and nothing when xradar has no entry.
   `units_xradar` is kept as specified.
7. Additional constructors and checks used by decoders: `Volume::new`, `Volume::seal`,
   `Sweep::add_field` (rejects duplicate names), `Field::push_absent_rows_to`,
   `IntCoding::nexrad(scale, offset)`, `SourceFormat::infer_from_markers`,
   `LegacyConvention::of_metadata`, `floor_to_second`. A row longer than `ngates` widens the
   field's existing rows (the legacy behaviour); `Sweep::seal` grows a uniform range to cover
   every field.
8. `model::merge_volumes` / `MergeReport { merged_fields, skipped_geometry,
   field_collisions }` / `MergeError` are the FM301 counterpart of `merge_radar_volumes`
   (6.5), added now so the F.3 sub-worktrees do not both edit core. They are not re-exported
   at the crate root while the legacy `MergeReport` is.

**Choices the text left open**

9. JMA legacy `first_gate_m` (GRIB2 template 3.50120 "range start") is treated as a gate
   centre, like NEXRAD, DORADE and generic volumes. Round trips are exact either way; F.3
   confirms the meaning when io-jma decodes natively.
10. A sweep with an explicit range has no legacy form: `cut_from_sweep` and
    `legacy_from_volume` return `ExplicitRange`; `grid_from_field` approximates the gate range
    from the first two centres.
11. Xradar flavor details not pinned by appendix A are provisional until F.4's goldens:
    `version` (source version for CfRadial, else "None"), `platform_is_mobile` (CfRadial
    only), `scan_id` (omitted for NEXRAD), `nyquist_velocity` attributes, ray-variable units,
    and the `elevation` ray dimension for non-CfRadial RHI sweeps.

**Acceptance evidence (13.4)**

- `tools/fm301_legacy_items.py` passes: 41 model items and 28 of 30 test items of
  `lib.rs@1989a03` are in `legacy.rs` verbatim apart from 21 `cfg_attr` deprecation
  attributes; the 4 geometry items and 2 geometry tests stay in `lib.rs` unchanged.
- `RUSTFLAGS="--cfg recast_legacy_deprecation" cargo check -p recast-radar-core --all-targets`
  reports no warning from core (the io crates it pulls in as dev-dependencies warn, as
  un-migrated crates should).
- Round trip (`crates/recast-radar-core/tests/legacy_round_trip.rs`): 31 committed fixtures,
  and 119 volumes of the full corpus (release, `-- --ignored`), are bit-identical after
  legacy -> FM301 -> legacy. Five corpus files are refused with `UnalignedGates`, and the
  test checks independently that their legacy geometry cannot share one range: the legacy
  decoder turns the headerless model-data file `l2-klix-20210829-175748-mdm` and the
  intermediate real-time chunks `l2chunk-kiwa-307-20260917-003629-{002,003,014,030}-i` into
  a few "Message 1" radials of garbage (gate spacings such as 52966 m and 27686 m in one
  radial). That is a decoder issue (stream A), not a model limitation.

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
  - `review_checks.py <KTLX 2024 file> <testdata/files/other> <scratch dir>` (added for
    section 16)
  - `shim_gate/run.sh`, a standalone Cargo workspace for the deprecation gate (13.4; added for
    section 16)

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
  - Python bools: mpda_vcp False, base_tilt_vcp False, vcp_truncated False,
    vcp_sequence_active False, avset_enabled True, ebc_enabled True.
  - Python ints: num_base_tilts 0, number_elevation_cuts 23, actual_elevation_cuts 20,
    super_res_status 2, rda_build_number 2200, operational_mode 4.
  - Python float: doppler_velocity_resolution 0.5. Strings: vcp_pulse_width "short" and the
    ones above.
  - (The first version of this note printed these as strings. The types were re-checked for
    this revision with `type(v)` on `dt["/"].attrs`. `xd.io.to_cfradial2` on this tree raises
    `TypeError: illegal data type for attribute b'mpda_vcp'` (review evidence).)

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
  "contiguous_doppler", channel_config "sz2_phase_coding" (strings); super_resolution 11 / 7,
  sails_sequence_number 0, mrle_sequence_number 0 (ints); sails_cut, mrle_cut, mpda_cut,
  base_tilt_cut False (bools).

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

**Added for the review resolutions** (`tools/fm301_probe/review_checks.py` part 4, netCDF4
1.7.4 and xradar 0.12.0):

- File contents (netCDF4), DOW8 and IRENE alike:
  - Global attributes, in order: `Conventions`, `Sub_conventions`, `version`, `title`,
    `institution`, `references`, `source`, `history`, `comment`, `original_format`,
    `driver`, `created`, `start_datetime`, `time_coverage_start`, `start_time`,
    `end_datetime`, `time_coverage_end`, `end_time`, `instrument_name`, `site_name`,
    `scan_name`, `scan_id`, `platform_is_mobile`, `n_gates_vary` ("false"),
    `ray_times_increase`.
  - Root variables include `status_xml`, `grid_mapping` (int32) and `radar_rx_bandwidth`,
    and `r_calib_*(r_calib)` including `r_calib_k_squared_water`, `r_calib_i0_dbm_*`,
    `r_calib_dynamic_range_db_*`, `r_calib_base_dbz_1km_*` and `r_calib_dbz_correction`.
    `r_calib_time` is a string.
  - Per-ray variables include `ray_start_range`, `ray_gate_spacing` (constant in both files),
    `georefs_applied` (int8), `n_samples` and `r_calib_index` (int32), `georef_time`,
    `georef_unit_num`, `georef_unit_id`, and (DOW8) `latitude/longitude/altitude/altitude_agl(time)`.
- xradar `open_cfradial1_datatree(DOW8, optional_groups=True)`:
  - Root attributes are only `Conventions` ("CF-1.7"), `version`, `title`, `institution`,
    `references`, `source`, `history`, `comment`, `instrument_name`, `site_name`,
    `scan_name`, `scan_id` (**int32**), `platform_is_mobile` and `ray_times_increase`.
    `Sub_conventions`, `original_format`, `driver`, `created`, `start_*`, `end_*` and
    `n_gates_vary` are **dropped** (Py-ART keeps them).
  - `sweep_0` keeps `ray_start_range`, `ray_gate_spacing`, `georefs_applied`, `georef_time`,
    `georef_unit_num`, `georef_unit_id`, `altitude_agl`, `r_calib_index`,
    `measured_transmit_power_h/v`. After CF decoding, `n_samples` and `r_calib_index` are
    float64.
  - `/radar_calibration` names drop the `r_calib_` prefix and keep the non-FM301 entries
    (`k_squared_water`, `i0_dbm_*`, `dynamic_range_db_*`, `dbz_correction`), with
    `base_dbz_1km_hc` renamed to `base_1km_hc`.
  - `/radar_parameters` holds `radar_beam_width_h/v`, `radar_antenna_gain_h/v` and
    `radar_receiver_bandwidth` (from the file's `radar_rx_bandwidth`).
- xsapr (`cfrad.xsapr_sgp_ppi_20110520.netcdf4.nc`): `reflectivity_horizontal` is float32
  with `standard_name = "equivalent_reflectivity_factor"` and `_FillValue` -9999.0. The file
  has no `n_gates_vary` attribute and no `ray_start_range`.
- `time.units`: IRENE "seconds since 2011-08-27T12:04:20Z" with first values 0.76, 0.76;
  xsapr "seconds since 2011-05-20T10:54:08Z".

### A.6 xarray CF decoding of NEXRAD sentinels (xarray 2026.7.0, h5netcdf)

Input: uint8 `[0, 1, 2, 66, 200, 255]` with scale_factor 0.5 and add_offset -33.0.

| Attributes | `decode_cf` result | Warnings | `to_netcdf` round trip |
|---|---|---|---|
| `_FillValue=0` | NaN, -32.5, -32.0, 0.0, 67.0, 94.5 | none | OK, identical |
| `_FillValue=0`, `missing_value=[0,1]` | NaN, NaN, -32.0, ... | SerializationWarning "multiple fill values ... decoding all values to NaN" (twice) | **ValueError**: conflicting `_FillValue` and `missing_value` |
| `missing_value=[0,1]` | NaN, NaN, -32.0, ... | same warning | **ValueError**: truth value of an array is ambiguous |
| `_FillValue=0`, `_Undetect=0`, `flag_values=[1]`, `flag_meanings="range_folded"` | NaN, -32.5, ... (flags kept as attributes) | none | OK |

Re-checked for the review resolutions (`tools/fm301_probe/review_checks.py` part 3, xarray
2026.7.0):

- `decode_cf` of the same array, with `_FillValue=0u8`, `_Undetect=0u8`,
  `valid_range=[2,255]u8`, `flag_values=[1]u8` and `flag_meanings="range_folded"`, gives
  float64 `[nan, -32.5, -32, 0, 67, 94.5]`.
- `.encoding` is then `{_FillValue: 0, scale_factor: 0.5, add_offset: -33.0, dtype: uint8}`.
- `.attrs` is then `{_Undetect: 0u8, valid_range: [2, 255]u8, flag_values: [1]u8,
  flag_meanings: "range_folded"}`, all still in packed units.
- After moving those four attributes into `.encoding`, `to_netcdf` with both the `netcdf4`
  and `h5netcdf` engines writes only `_FillValue`, `add_offset` and `scale_factor`. Unknown
  encoding keys are dropped (12.3).

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
# review resolutions (section 16)
$PY tools/fm301_probe/review_checks.py KTLX20240315_000217_V06 testdata/files/other <scratch dir>
bash tools/fm301_probe/shim_gate/run.sh
```
