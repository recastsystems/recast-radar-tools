//! The FM301 root group (`docs/design/fm301-model.md` sections 2 and 11).

use chrono::{DateTime, Duration, Timelike, Utc};
use serde::{Deserialize, Serialize};

use super::sweep::{Sweep, SweepError};
use super::values::{AttrValue, ExtraVariable};

/// FM301 volume: the root group of an FM301 / CfRadial 2 file and the root node
/// of an xradar `DataTree`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Volume {
    /// Root attributes (Tables 301-1, 301-2, 301-3; WMO-CF-2).
    pub attrs: GlobalAttrs,
    /// `/volume_number`.
    pub volume_number: Option<i32>,
    /// Epoch for every "seconds since `<reftime>`" value in the volume
    /// (`Rays::time_s`, `RadarCalibration::time_s`). Always whole seconds: FM301
    /// Table 301-6b writes it as `YYYY-MM-DDThh:mm:ssZ`, and any fraction lives
    /// in `time_s`.
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
    /// Root variables with no slot above, verbatim and in file order.
    pub extra_vars: Vec<ExtraVariable>,
    /// Source format, container version, decode statistics. Not exported as
    /// variables.
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
    /// `instrument_name`: NEXRAD ICAO; ODIM `what/source` NOD, else RAD, else
    /// WMO; CfRadial attribute. Empty when the source has none.
    pub instrument_name: String,
    pub site_name: Option<String>,
    /// `platform_is_mobile`: `true` when the source says the platform moves
    /// (a CfRadial file's attribute; a DORADE airborne or shipborne radar,
    /// as LROSE Radx writes it), else `false`, the only value FM301 2022
    /// uses for the fixed platforms it describes.
    pub platform_is_mobile: bool,
    pub ray_times_increase: Option<bool>,
    /// `simulated` (Table 301-3).
    pub simulated: bool,
    /// `wmo__*` attributes (WMO-CF.6.10).
    pub wmo: WmoAttrs,
    /// Source attributes with no slot above, verbatim, typed and in file order.
    pub other: Vec<(Box<str>, AttrValue)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WmoAttrs {
    /// `wmo__wsi`.
    pub wsi: Option<String>,
    /// `wmo__id` (ODIM "WMO:03962" -> "03962").
    pub id: Option<String>,
    /// `wmo__originating_centre` (Common Code Table C-11).
    pub originating_centre: Option<u16>,
    pub originating_sub_centre: Option<u16>,
    /// `wmo__data_category` (C-13).
    pub data_category: Option<u8>,
    /// `wmo__data_policy`.
    pub data_policy: Option<WmoDataPolicy>,
    pub update_sequence_number: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WmoDataPolicy {
    Core,
    Recommended,
}

impl WmoDataPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Recommended => "recommended",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Location {
    /// WGS84 degrees north. `None` when the source has no location (Message 1
    /// archives).
    pub latitude_deg: Option<f64>,
    pub longitude_deg: Option<f64>,
    /// Metres above MSL at the antenna's centre of rotation.
    pub altitude_m: Option<f64>,
    pub altitude_agl_m: Option<f64>,
}

macro_rules! table_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal,)* }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $($variant,)*
        }

        impl $name {
            /// The Table 301-15 spelling.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)*
                }
            }

            /// Table 301-15 spelling (trimmed) to its variant.
            pub fn parse(text: &str) -> Option<Self> {
                match text.trim() {
                    $($text => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

table_enum! {
    /// `platform_type` (Table 301-15).
    PlatformType {
        Fixed => "fixed",
        Vehicle => "vehicle",
        Ship => "ship",
        Aircraft => "aircraft",
        AircraftFore => "aircraft_fore",
        AircraftAft => "aircraft_aft",
        AircraftTail => "aircraft_tail",
        AircraftBelly => "aircraft_belly",
        AircraftRoof => "aircraft_roof",
        AircraftNose => "aircraft_nose",
        SatelliteOrbit => "satellite_orbit",
        SatelliteGeostat => "satellite_geostat",
    }
}

table_enum! {
    /// `instrument_type` (Table 301-15).
    InstrumentType {
        Radar => "radar",
        Lidar => "lidar",
    }
}

table_enum! {
    /// `primary_axis` (Table 301-15).
    PrimaryAxis {
        AxisZ => "axis_z",
        AxisY => "axis_y",
        AxisX => "axis_x",
        AxisZPrime => "axis_z_prime",
        AxisYPrime => "axis_y_prime",
        AxisXPrime => "axis_x_prime",
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanStrategy {
    /// `scan_name`. NEXRAD: "VCP-212" (xradar's spelling).
    pub name: Option<String>,
    /// `scan_id` (FM301 int). NEXRAD: the VCP number.
    pub id: Option<i64>,
    /// NEXRAD VCP pattern.
    pub vcp_pattern: Option<u16>,
    /// Scan-table provenance.
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
    /// `ScanStrategy::id.to_string()`: non-numeric ids, and numeric ids with
    /// other spellings ("00", "+5"). `None` when `id` reproduces the text.
    pub scan_id_text: Option<String>,
    /// One leg per sweep.
    pub legs: Vec<ScanLeg>,
}

/// Source-qualified provenance for one physical scan leg / sweep.
///
/// PRF values here are source-table *codes* and pulse counts, never
/// frequencies or pulse-repetition times.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanLeg {
    /// Zero-based row within the qualified source definition.
    pub source_row_index: Option<u16>,
    pub elevation_deg: Option<f32>,
    pub azimuth_rate_deg_per_second: Option<f32>,
    pub source_period_seconds: Option<f32>,
    /// Source waveform abbreviation (for example `CS`, `CD/W`, or `SZCD`).
    pub waveform: Option<String>,
    /// `surveillance`, `doppler`, or `all` for catalog-backed synthetic cuts.
    pub moment_coverage: Option<String>,
    pub surveillance_prf_code: Option<u8>,
    pub surveillance_pulse_count: Option<u16>,
    pub doppler_prf_code: Option<u8>,
    pub doppler_pulse_count: Option<u16>,
}

/// `/radar_parameters`. Variable names differ by flavor (design note 12.4).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RadarParameters {
    /// Operating frequencies in Hz (`frequency` dimension).
    pub frequency_hz: Vec<f64>,
    pub antenna_gain_h_db: Option<f32>,
    pub antenna_gain_v_db: Option<f32>,
    pub beam_width_h_deg: Option<f32>,
    pub beam_width_v_deg: Option<f32>,
    pub receiver_bandwidth_hz: Option<f32>,
    /// Volume-constant values some sources declare instead of per-ray vectors.
    /// For sweeps whose `RayVariables` lack them, the view broadcasts these into
    /// `(time)` variables.
    pub pulse_width_s: Option<f32>,
    pub prt_s: Option<f32>,
    pub unambiguous_range_m: Option<f32>,
}

/// One `calib` entry of `/radar_calibration` (Table 301-14a names plus a unit
/// suffix).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RadarCalibration {
    pub calib_index: Option<i32>,
    /// Seconds since `Volume::time_reference`.
    pub time_s: Option<f64>,
    pub pulse_width_s: Option<f32>,
    pub antenna_gain_h_db: Option<f32>,
    pub antenna_gain_v_db: Option<f32>,
    pub xmit_power_h_dbm: Option<f32>,
    pub xmit_power_v_dbm: Option<f32>,
    pub two_way_waveguide_loss_h_db: Option<f32>,
    pub two_way_waveguide_loss_v_db: Option<f32>,
    pub two_way_radome_loss_h_db: Option<f32>,
    pub two_way_radome_loss_v_db: Option<f32>,
    pub receiver_mismatch_loss_db: Option<f32>,
    pub receiver_mismatch_loss_h_db: Option<f32>,
    pub receiver_mismatch_loss_v_db: Option<f32>,
    pub radar_constant_h: Option<f32>,
    pub radar_constant_v: Option<f32>,
    pub probert_jones_correction: Option<f32>,
    pub dielectric_factor_used: Option<f32>,
    pub noise_hc_dbm: Option<f32>,
    pub noise_vc_dbm: Option<f32>,
    pub noise_hx_dbm: Option<f32>,
    pub noise_vx_dbm: Option<f32>,
    pub receiver_gain_hc_db: Option<f32>,
    pub receiver_gain_vc_db: Option<f32>,
    pub receiver_gain_hx_db: Option<f32>,
    pub receiver_gain_vx_db: Option<f32>,
    pub base_1km_hc_dbz: Option<f32>,
    pub base_1km_vc_dbz: Option<f32>,
    pub base_1km_hx_dbz: Option<f32>,
    pub base_1km_vx_dbz: Option<f32>,
    pub sun_power_hc_dbm: Option<f32>,
    pub sun_power_vc_dbm: Option<f32>,
    pub sun_power_hx_dbm: Option<f32>,
    pub sun_power_vx_dbm: Option<f32>,
    pub noise_source_power_h_dbm: Option<f32>,
    pub noise_source_power_v_dbm: Option<f32>,
    pub power_measure_loss_h_db: Option<f32>,
    pub power_measure_loss_v_db: Option<f32>,
    pub coupler_forward_loss_h_db: Option<f32>,
    pub coupler_forward_loss_v_db: Option<f32>,
    pub zdr_correction_db: Option<f32>,
    pub ldr_correction_h_db: Option<f32>,
    pub ldr_correction_v_db: Option<f32>,
    pub system_phidp_deg: Option<f32>,
    pub test_power_h_dbm: Option<f32>,
    pub test_power_v_dbm: Option<f32>,
    pub receiver_slope_hc: Option<f32>,
    pub receiver_slope_vc: Option<f32>,
    pub receiver_slope_hx: Option<f32>,
    pub receiver_slope_vx: Option<f32>,
    /// Entries outside Table 301-14a, named as xradar names them (the CfRadial
    /// `r_calib_` prefix removed): `k_squared_water`, `i0_dbm_hc`, ...
    pub extra: Vec<(Box<str>, AttrValue)>,
}

impl RadarCalibration {
    /// `(Table 301-14a name, value)` for every float entry, in table order
    /// (`calib_index` and `time` excluded).
    pub fn float_entries(&self) -> [(&'static str, Option<f32>); 48] {
        [
            ("pulse_width", self.pulse_width_s),
            ("antenna_gain_h", self.antenna_gain_h_db),
            ("antenna_gain_v", self.antenna_gain_v_db),
            ("xmit_power_h", self.xmit_power_h_dbm),
            ("xmit_power_v", self.xmit_power_v_dbm),
            ("two_way_waveguide_loss_h", self.two_way_waveguide_loss_h_db),
            ("two_way_waveguide_loss_v", self.two_way_waveguide_loss_v_db),
            ("two_way_radome_loss_h", self.two_way_radome_loss_h_db),
            ("two_way_radome_loss_v", self.two_way_radome_loss_v_db),
            ("receiver_mismatch_loss", self.receiver_mismatch_loss_db),
            ("receiver_mismatch_loss_h", self.receiver_mismatch_loss_h_db),
            ("receiver_mismatch_loss_v", self.receiver_mismatch_loss_v_db),
            ("radar_constant_h", self.radar_constant_h),
            ("radar_constant_v", self.radar_constant_v),
            ("probert_jones_correction", self.probert_jones_correction),
            ("dielectric_factor_used", self.dielectric_factor_used),
            ("noise_hc", self.noise_hc_dbm),
            ("noise_vc", self.noise_vc_dbm),
            ("noise_hx", self.noise_hx_dbm),
            ("noise_vx", self.noise_vx_dbm),
            ("receiver_gain_hc", self.receiver_gain_hc_db),
            ("receiver_gain_vc", self.receiver_gain_vc_db),
            ("receiver_gain_hx", self.receiver_gain_hx_db),
            ("receiver_gain_vx", self.receiver_gain_vx_db),
            ("base_1km_hc", self.base_1km_hc_dbz),
            ("base_1km_vc", self.base_1km_vc_dbz),
            ("base_1km_hx", self.base_1km_hx_dbz),
            ("base_1km_vx", self.base_1km_vx_dbz),
            ("sun_power_hc", self.sun_power_hc_dbm),
            ("sun_power_vc", self.sun_power_vc_dbm),
            ("sun_power_hx", self.sun_power_hx_dbm),
            ("sun_power_vx", self.sun_power_vx_dbm),
            ("noise_source_power_h", self.noise_source_power_h_dbm),
            ("noise_source_power_v", self.noise_source_power_v_dbm),
            ("power_measure_loss_h", self.power_measure_loss_h_db),
            ("power_measure_loss_v", self.power_measure_loss_v_db),
            ("coupler_forward_loss_h", self.coupler_forward_loss_h_db),
            ("coupler_forward_loss_v", self.coupler_forward_loss_v_db),
            ("zdr_correction", self.zdr_correction_db),
            ("ldr_correction_h", self.ldr_correction_h_db),
            ("ldr_correction_v", self.ldr_correction_v_db),
            ("system_phidp", self.system_phidp_deg),
            ("test_power_h", self.test_power_h_dbm),
            ("test_power_v", self.test_power_v_dbm),
            ("receiver_slope_hc", self.receiver_slope_hc),
            ("receiver_slope_vc", self.receiver_slope_vc),
            ("receiver_slope_hx", self.receiver_slope_hx),
            ("receiver_slope_vx", self.receiver_slope_vx),
        ]
    }

    /// Set the float entry with the Table 301-14a `name` (one of the names
    /// [`Self::float_entries`] returns). Returns `false`, leaving `self`
    /// unchanged, when `name` is not one of them.
    pub fn set_float_entry(&mut self, name: &str, value: Option<f32>) -> bool {
        let slot: &mut Option<f32> = match name {
            "pulse_width" => &mut self.pulse_width_s,
            "antenna_gain_h" => &mut self.antenna_gain_h_db,
            "antenna_gain_v" => &mut self.antenna_gain_v_db,
            "xmit_power_h" => &mut self.xmit_power_h_dbm,
            "xmit_power_v" => &mut self.xmit_power_v_dbm,
            "two_way_waveguide_loss_h" => &mut self.two_way_waveguide_loss_h_db,
            "two_way_waveguide_loss_v" => &mut self.two_way_waveguide_loss_v_db,
            "two_way_radome_loss_h" => &mut self.two_way_radome_loss_h_db,
            "two_way_radome_loss_v" => &mut self.two_way_radome_loss_v_db,
            "receiver_mismatch_loss" => &mut self.receiver_mismatch_loss_db,
            "receiver_mismatch_loss_h" => &mut self.receiver_mismatch_loss_h_db,
            "receiver_mismatch_loss_v" => &mut self.receiver_mismatch_loss_v_db,
            "radar_constant_h" => &mut self.radar_constant_h,
            "radar_constant_v" => &mut self.radar_constant_v,
            "probert_jones_correction" => &mut self.probert_jones_correction,
            "dielectric_factor_used" => &mut self.dielectric_factor_used,
            "noise_hc" => &mut self.noise_hc_dbm,
            "noise_vc" => &mut self.noise_vc_dbm,
            "noise_hx" => &mut self.noise_hx_dbm,
            "noise_vx" => &mut self.noise_vx_dbm,
            "receiver_gain_hc" => &mut self.receiver_gain_hc_db,
            "receiver_gain_vc" => &mut self.receiver_gain_vc_db,
            "receiver_gain_hx" => &mut self.receiver_gain_hx_db,
            "receiver_gain_vx" => &mut self.receiver_gain_vx_db,
            "base_1km_hc" => &mut self.base_1km_hc_dbz,
            "base_1km_vc" => &mut self.base_1km_vc_dbz,
            "base_1km_hx" => &mut self.base_1km_hx_dbz,
            "base_1km_vx" => &mut self.base_1km_vx_dbz,
            "sun_power_hc" => &mut self.sun_power_hc_dbm,
            "sun_power_vc" => &mut self.sun_power_vc_dbm,
            "sun_power_hx" => &mut self.sun_power_hx_dbm,
            "sun_power_vx" => &mut self.sun_power_vx_dbm,
            "noise_source_power_h" => &mut self.noise_source_power_h_dbm,
            "noise_source_power_v" => &mut self.noise_source_power_v_dbm,
            "power_measure_loss_h" => &mut self.power_measure_loss_h_db,
            "power_measure_loss_v" => &mut self.power_measure_loss_v_db,
            "coupler_forward_loss_h" => &mut self.coupler_forward_loss_h_db,
            "coupler_forward_loss_v" => &mut self.coupler_forward_loss_v_db,
            "zdr_correction" => &mut self.zdr_correction_db,
            "ldr_correction_h" => &mut self.ldr_correction_h_db,
            "ldr_correction_v" => &mut self.ldr_correction_v_db,
            "system_phidp" => &mut self.system_phidp_deg,
            "test_power_h" => &mut self.test_power_h_dbm,
            "test_power_v" => &mut self.test_power_v_dbm,
            "receiver_slope_hc" => &mut self.receiver_slope_hc,
            "receiver_slope_vc" => &mut self.receiver_slope_vc,
            "receiver_slope_hx" => &mut self.receiver_slope_hx,
            "receiver_slope_vx" => &mut self.receiver_slope_vx,
            _ => return false,
        };
        *slot = value;
        true
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub source_format: SourceFormat,
    pub source_path: Option<String>,
    /// As written: "AR2V0006", "ARCHIVE2.036", "H5rad 2.3", "CF-Radial-1.3".
    pub source_version: Option<String>,
    /// The source's `Conventions` ("ODIM_H5/V2_2", "CF-1.6").
    pub source_conventions: Option<String>,
    pub compression: Option<String>,
    pub decode: DecodeStats,
    /// Free text carried by BowEcho-written CfRadial files.
    pub polarization_note: Option<String>,
    pub calibration_note: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SourceFormat {
    NexradLevel2,
    NexradLevel3,
    OdimH5,
    CfRadial1,
    CfRadial2,
    Dorade,
    JmaGrib2,
    Simulated,
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

/// `/georeferencing_correction`: one `Option<f32>` per name in xradar's
/// `georeferencing_correction_subgroup`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GeoreferencingCorrection {
    pub azimuth_correction: Option<f32>,
    pub elevation_correction: Option<f32>,
    pub range_correction: Option<f32>,
    pub longitude_correction: Option<f32>,
    pub latitude_correction: Option<f32>,
    pub pressure_altitude_correction: Option<f32>,
    pub radar_altitude_correction: Option<f32>,
    pub eastward_ground_speed_correction: Option<f32>,
    pub northward_ground_speed_correction: Option<f32>,
    pub vertical_velocity_correction: Option<f32>,
    pub heading_correction: Option<f32>,
    pub roll_correction: Option<f32>,
    pub pitch_correction: Option<f32>,
    pub drift_correction: Option<f32>,
    pub rotation_correction: Option<f32>,
    pub tilt_correction: Option<f32>,
}

impl GeoreferencingCorrection {
    /// `(name, value)` in xradar's order.
    pub fn entries(&self) -> [(&'static str, Option<f32>); 16] {
        [
            ("azimuth_correction", self.azimuth_correction),
            ("elevation_correction", self.elevation_correction),
            ("range_correction", self.range_correction),
            ("longitude_correction", self.longitude_correction),
            ("latitude_correction", self.latitude_correction),
            (
                "pressure_altitude_correction",
                self.pressure_altitude_correction,
            ),
            ("radar_altitude_correction", self.radar_altitude_correction),
            (
                "eastward_ground_speed_correction",
                self.eastward_ground_speed_correction,
            ),
            (
                "northward_ground_speed_correction",
                self.northward_ground_speed_correction,
            ),
            (
                "vertical_velocity_correction",
                self.vertical_velocity_correction,
            ),
            ("heading_correction", self.heading_correction),
            ("roll_correction", self.roll_correction),
            ("pitch_correction", self.pitch_correction),
            ("drift_correction", self.drift_correction),
            ("rotation_correction", self.rotation_correction),
            ("tilt_correction", self.tilt_correction),
        ]
    }
}

/// `time` floored to the whole second (the form of `Volume::time_reference`).
pub fn floor_to_second(time: DateTime<Utc>) -> DateTime<Utc> {
    time.with_nanosecond(0).unwrap_or(time)
}

impl Volume {
    /// An empty fixed-platform radar volume. `time_reference` is floored to the
    /// whole second.
    pub fn new(instrument_name: impl Into<String>, time_reference: DateTime<Utc>) -> Self {
        Self {
            attrs: GlobalAttrs {
                instrument_name: instrument_name.into(),
                ..GlobalAttrs::default()
            },
            volume_number: None,
            time_reference: floor_to_second(time_reference),
            time_coverage: None,
            location: Location::default(),
            platform_type: PlatformType::Fixed,
            instrument_type: InstrumentType::Radar,
            primary_axis: None,
            status_str: None,
            scan: ScanStrategy::default(),
            radar_parameters: RadarParameters::default(),
            radar_calibration: Vec::new(),
            georeferencing_correction: None,
            extra_vars: Vec::new(),
            provenance: Provenance::default(),
            simulation: None,
            sweeps: Vec::new(),
        }
    }

    /// Absolute time of a `time_s` offset.
    pub fn instant(&self, time_s: f64) -> Option<DateTime<Utc>> {
        if !time_s.is_finite() {
            return None;
        }
        let nanos = (time_s * 1e9).round();
        if nanos.abs() > i64::MAX as f64 {
            return None;
        }
        self.time_reference
            .checked_add_signed(Duration::nanoseconds(nanos as i64))
    }

    /// Absolute time of ray `ray` of sweep `sweep`.
    pub fn ray_time(&self, sweep: usize, ray: usize) -> Option<DateTime<Utc>> {
        self.instant(*self.sweeps.get(sweep)?.rays.time_s.get(ray)?)
    }

    /// [`Sweep::tilt_elevation_deg`] of sweep `sweep` for this volume's
    /// source format; `None` when the sweep does not exist.
    pub fn tilt_elevation_deg(&self, sweep: usize) -> Option<f32> {
        Some(
            self.sweeps
                .get(sweep)?
                .tilt_elevation_deg(self.provenance.source_format),
        )
    }

    /// First and last ray time over all sweeps (by value, not storage order).
    pub fn ray_time_extent(&self) -> Option<TimeCoverage> {
        let mut extent: Option<(f64, f64)> = None;
        for time in self
            .sweeps
            .iter()
            .flat_map(|sweep| sweep.rays.time_s.iter().copied())
            .filter(|time| time.is_finite())
        {
            extent = Some(match extent {
                None => (time, time),
                Some((lo, hi)) => (lo.min(time), hi.max(time)),
            });
        }
        let (lo, hi) = extent?;
        Some(TimeCoverage {
            start: self.instant(lo)?,
            end: self.instant(hi)?,
        })
    }

    /// Seal every sweep and check `sweeps[i].sweep_number == i`.
    pub fn seal(&mut self) -> Result<(), SweepError> {
        for (index, sweep) in self.sweeps.iter_mut().enumerate() {
            if sweep.sweep_number as usize != index {
                return Err(SweepError::SweepNumber {
                    index,
                    sweep_number: sweep.sweep_number,
                });
            }
            sweep.seal()?;
        }
        Ok(())
    }
}
