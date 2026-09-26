//! `sweep_<n>` groups: ray coordinates, the range coordinate, per-ray
//! instrument variables and fields (`docs/design/fm301-model.md` sections 3,
//! 6, 9 and 10).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::field::{Field, FieldError, GateMapping};
use super::names::{FieldName, Polarization, Quantity};
use super::values::{AttrValue, ExtraVariable};
use super::volume::SourceFormat;

/// One FM301 sweep group: one physical elevation cut, numbered in acquisition
/// order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sweep {
    /// `sweep_number`: 0-based acquisition index; the group is
    /// `sweep_<sweep_number>`.
    pub sweep_number: u32,
    /// `sweep_mode` (Table 301-15; section 10).
    pub sweep_mode: SweepMode,
    /// `follow_mode`. `None` means the source does not say; exported as
    /// "not_set".
    pub follow_mode: Option<FollowMode>,
    /// `prt_mode`. `None` means the source does not say; exported as "not_set".
    pub prt_mode: Option<PrtMode>,
    /// `polarization_mode`.
    pub polarization_mode: Option<PolarizationMode>,
    /// `polarization_sequence(prt)` (Table 301-8a): "H" or "V" per PRT.
    pub polarization_sequence: Option<Vec<Box<str>>>,
    /// `fixed_angle` (xradar: `sweep_fixed_angle`). Target elevation; target
    /// azimuth for RHI.
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
    /// Per-ray platform position and attitude for moving platforms. Not FM301.
    pub platform_track: Option<Box<PlatformTrack>>,
    /// Sweep variables with no slot above, verbatim and in file order.
    pub extra_vars: Vec<ExtraVariable>,
    /// Sweep group attributes with no slot above, verbatim.
    pub other: Vec<(Box<str>, AttrValue)>,
    /// Dataset variables, in source order.
    pub fields: Vec<Field>,
    /// Source cut number (NEXRAD ICD elevation number, 1-based). Not an FM301
    /// variable.
    pub elevation_number: Option<u16>,
    /// `false` when rays stop before the end-of-elevation marker (truncated
    /// archive, real-time chunk).
    pub complete: bool,
}

/// Ray coordinates, struct-of-arrays.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rays {
    /// `time`: seconds since `Volume::time_reference`, at ray centre.
    pub time_s: Vec<f64>,
    /// `azimuth`: degrees clockwise from true north.
    pub azimuth_deg: Vec<f32>,
    /// `elevation`: degrees above horizontal.
    pub elevation_deg: Vec<f32>,
}

impl Rays {
    /// Number of rays (the azimuth vector's length).
    pub fn len(&self) -> usize {
        self.azimuth_deg.len()
    }

    pub fn is_empty(&self) -> bool {
        self.azimuth_deg.is_empty()
    }
}

/// The sweep's `range` coordinate: gate centres in metres.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RangeCoord {
    /// `spacing_is_constant = "true"`: centre of gate j is
    /// `first_center_m + j * spacing_m`. `spacing_m == 0` with no gates means
    /// "not yet set" ([`Sweep::new`]).
    Uniform {
        first_center_m: f64,
        spacing_m: f64,
        ngates: u32,
    },
    /// `spacing_is_constant = "false"`: explicit gate centres.
    Explicit { centers_m: Vec<f32> },
}

impl RangeCoord {
    /// The unset coordinate a new sweep starts with.
    pub const UNSET: RangeCoord = RangeCoord::Uniform {
        first_center_m: 0.0,
        spacing_m: 0.0,
        ngates: 0,
    };

    pub fn ngates(&self) -> usize {
        match self {
            Self::Uniform { ngates, .. } => *ngates as usize,
            Self::Explicit { centers_m } => centers_m.len(),
        }
    }

    /// Centre of gate `gate` in metres.
    pub fn center_m(&self, gate: usize) -> Option<f64> {
        match self {
            Self::Uniform {
                first_center_m,
                spacing_m,
                ngates,
            } => (gate < *ngates as usize).then(|| first_center_m + gate as f64 * spacing_m),
            Self::Explicit { centers_m } => centers_m.get(gate).map(|c| f64::from(*c)),
        }
    }

    /// Every gate centre in metres, as float32 (FM301 `range` type).
    pub fn centers_f32(&self) -> Vec<f32> {
        match self {
            Self::Uniform {
                first_center_m,
                spacing_m,
                ngates,
            } => (0..*ngates)
                .map(|gate| (first_center_m + f64::from(gate) * spacing_m) as f32)
                .collect(),
            Self::Explicit { centers_m } => centers_m.clone(),
        }
    }

    /// Constant gate spacing, `None` for explicit centres.
    pub fn spacing_m(&self) -> Option<f64> {
        match self {
            Self::Uniform { spacing_m, .. } => Some(*spacing_m),
            Self::Explicit { .. } => None,
        }
    }
}

macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $($variant:ident => $text:literal,)* }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $($variant,)*
            /// A source spelling outside the table, verbatim. Never a table
            /// spelling.
            Other(Box<str>),
        }

        impl $name {
            /// The FM301 / CfRadial spelling.
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $text,)*
                    Self::Other(text) => text,
                }
            }

            /// Table spelling to its variant (exact, after trimming); anything
            /// else is `Other` verbatim.
            pub fn parse(text: &str) -> Self {
                match text.trim() {
                    $($text => Self::$variant,)*
                    _ => Self::Other(text.into()),
                }
            }
        }
    };
}

string_enum! {
    /// `sweep_mode` (FM301 Table 301-15; CfRadial 1.x values outside it are
    /// `Other`).
    SweepMode {
        Sector => "sector",
        Coplane => "coplane",
        Rhi => "rhi",
        VerticalPointing => "vertical_pointing",
        Idle => "idle",
        AzimuthSurveillance => "azimuth_surveillance",
        ElevationSurveillance => "elevation_surveillance",
        Sunscan => "sunscan",
        Pointing => "pointing",
        ManualPpi => "manual_ppi",
        ManualRhi => "manual_rhi",
        DopplerBeamSwinging => "doppler_beam_swinging",
        ComplexTrajectory => "complex_trajectory",
        ElectronicSteering => "electronic_steering",
    }
}

string_enum! {
    /// `follow_mode` (Table 301-15).
    FollowMode {
        None => "none",
        Sun => "sun",
        Vehicle => "vehicle",
        Aircraft => "aircraft",
        Target => "target",
        Manual => "manual",
    }
}

string_enum! {
    /// `prt_mode` (Table 301-15).
    PrtMode {
        Fixed => "fixed",
        Staggered => "staggered",
        Dual => "dual",
        Hybrid => "hybrid",
    }
}

string_enum! {
    /// `polarization_mode` (Table 301-15).
    PolarizationMode {
        Horizontal => "horizontal",
        Vertical => "vertical",
        HvAlt => "hv_alt",
        HvSim => "hv_sim",
        Circular => "circular",
    }
}

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

impl Monitoring {
    /// The slot of the Table 301-11 variable `name` (FM301 name, as
    /// [`Monitoring::variables`] lists them); `None` for another name.
    pub fn variable_mut(&mut self, name: &str) -> Option<&mut Option<Vec<f32>>> {
        Some(match name {
            "radar_measured_transmit_power_h" => &mut self.radar_measured_transmit_power_h_dbm,
            "radar_measured_transmit_power_v" => &mut self.radar_measured_transmit_power_v_dbm,
            "radar_measured_sky_noise" => &mut self.radar_measured_sky_noise_dbm,
            "radar_measured_cold_noise" => &mut self.radar_measured_cold_noise_dbm,
            "radar_measured_hot_noise" => &mut self.radar_measured_hot_noise_dbm,
            "phase_difference_transmit_hv" => &mut self.phase_difference_transmit_hv_deg,
            "antenna_pointing_accuracy_elev" => &mut self.antenna_pointing_accuracy_elev_deg,
            "antenna_pointing_accuracy_az" => &mut self.antenna_pointing_accuracy_az_deg,
            "calibration_offset_h" => &mut self.calibration_offset_h_db,
            "calibration_offset_v" => &mut self.calibration_offset_v_db,
            "zdr_offset" => &mut self.zdr_offset_db,
            _ => return None,
        })
    }

    /// `(FM301 name, values)` for every present variable, in table order.
    pub fn variables(&self) -> Vec<(&'static str, &[f32])> {
        let all: [(&'static str, &Option<Vec<f32>>); 11] = [
            (
                "radar_measured_transmit_power_h",
                &self.radar_measured_transmit_power_h_dbm,
            ),
            (
                "radar_measured_transmit_power_v",
                &self.radar_measured_transmit_power_v_dbm,
            ),
            (
                "radar_measured_sky_noise",
                &self.radar_measured_sky_noise_dbm,
            ),
            (
                "radar_measured_cold_noise",
                &self.radar_measured_cold_noise_dbm,
            ),
            (
                "radar_measured_hot_noise",
                &self.radar_measured_hot_noise_dbm,
            ),
            (
                "phase_difference_transmit_hv",
                &self.phase_difference_transmit_hv_deg,
            ),
            (
                "antenna_pointing_accuracy_elev",
                &self.antenna_pointing_accuracy_elev_deg,
            ),
            (
                "antenna_pointing_accuracy_az",
                &self.antenna_pointing_accuracy_az_deg,
            ),
            ("calibration_offset_h", &self.calibration_offset_h_db),
            ("calibration_offset_v", &self.calibration_offset_v_db),
            ("zdr_offset", &self.zdr_offset_db),
        ];
        all.into_iter()
            .filter_map(|(name, values)| values.as_deref().map(|values| (name, values)))
            .collect()
    }
}

/// Moving-platform position and attitude per ray (CfRadial 1 georeference
/// variables). Not FM301.
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

/// Optional `(time)` instrument variables (Table 301-8a). A present vector has
/// one entry per ray.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RayVariables {
    /// `nyquist_velocity(time)`, m/s, NaN = missing.
    pub nyquist_velocity_mps: Option<Vec<f32>>,
    /// `unambiguous_range(time)`, m.
    pub unambiguous_range_m: Option<Vec<f32>>,
    /// `prt(time)`, s.
    pub prt_s: Option<Vec<f32>>,
    /// `prt_ratio(time)`.
    pub prt_ratio: Option<Vec<f32>>,
    /// `prt_sequence(time, prt)`.
    pub prt_sequence_s: Option<PrtSequence>,
    /// `n_samples(time)`, -9999 = missing.
    pub n_samples: Option<Vec<i32>>,
    /// `pulse_width(time)`, s.
    pub pulse_width_s: Option<Vec<f32>>,
    /// `scan_rate(time)`, deg/s.
    pub scan_rate_deg_per_s: Option<Vec<f32>>,
    /// `antenna_transition(time)`, 0/1.
    pub antenna_transition: Option<Vec<u8>>,
    /// `calib_index(time)`, FM301 int (301-8a).
    pub calib_index: Option<Vec<i32>>,
    /// `rx_range_resolution(time)`, m.
    pub rx_range_resolution_m: Option<Vec<f32>>,
    /// Not FM301: effective independent samples (DORADE / research radars).
    pub independent_samples: Option<Vec<f32>>,
}

/// `prt_sequence(time, prt)`, row-major.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PrtSequence {
    pub nprt: u32,
    pub values_s: Vec<f32>,
}

impl RayVariables {
    /// `(name, length)` of every present per-ray vector, for invariant checks.
    fn lengths(&self) -> Vec<(&'static str, usize)> {
        let mut out = Vec::new();
        let mut push = |name: &'static str, len: Option<usize>| {
            if let Some(len) = len {
                out.push((name, len));
            }
        };
        push(
            "nyquist_velocity",
            self.nyquist_velocity_mps.as_ref().map(Vec::len),
        );
        push(
            "unambiguous_range",
            self.unambiguous_range_m.as_ref().map(Vec::len),
        );
        push("prt", self.prt_s.as_ref().map(Vec::len));
        push("prt_ratio", self.prt_ratio.as_ref().map(Vec::len));
        push(
            "prt_sequence",
            self.prt_sequence_s
                .as_ref()
                .map(|seq| seq.values_s.len() / seq.nprt.max(1) as usize),
        );
        push("n_samples", self.n_samples.as_ref().map(Vec::len));
        push("pulse_width", self.pulse_width_s.as_ref().map(Vec::len));
        push("scan_rate", self.scan_rate_deg_per_s.as_ref().map(Vec::len));
        push(
            "antenna_transition",
            self.antenna_transition.as_ref().map(Vec::len),
        );
        push("calib_index", self.calib_index.as_ref().map(Vec::len));
        push(
            "rx_range_resolution",
            self.rx_range_resolution_m.as_ref().map(Vec::len),
        );
        push(
            "independent_samples",
            self.independent_samples.as_ref().map(Vec::len),
        );
        out
    }

    /// `true` when no variable is present.
    pub fn is_empty(&self) -> bool {
        self.lengths().is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Error)]
pub enum GeometryError {
    #[error("invalid gate geometry: first centre {first_center_m} m, spacing {spacing_m} m")]
    Invalid { first_center_m: f64, spacing_m: f64 },
    #[error(
        "gates (first centre {first_center_m} m, spacing {spacing_m} m) do not align with the \
         sweep range (first centre {range_first_center_m} m, spacing {range_spacing_m} m)"
    )]
    Unaligned {
        first_center_m: f64,
        spacing_m: f64,
        range_first_center_m: f64,
        range_spacing_m: f64,
    },
    #[error("gates do not match the explicit range centres")]
    ExplicitMismatch,
    #[error("range gate count exceeds u32")]
    TooManyGates,
}

#[derive(Clone, Debug, PartialEq, Error)]
pub enum SweepError {
    #[error("sweep {what} has {len} entries for {nrays} rays")]
    RayLength {
        what: String,
        len: usize,
        nrays: usize,
    },
    #[error("field {field}: {source}")]
    Field {
        field: String,
        #[source]
        source: FieldError,
    },
    #[error("field {field} has {len} values, expected {expected}")]
    FieldLength {
        field: String,
        len: usize,
        expected: usize,
    },
    #[error("field {field} absent rows are not ascending and within the rays")]
    AbsentRows { field: String },
    #[error("field {field} extends to range gate {end}, past the explicit range's {range_gates}")]
    FieldExtent {
        field: String,
        end: u64,
        range_gates: usize,
    },
    #[error("field {field} has stride {stride} on a range that does not allow it")]
    Stride { field: String, stride: u32 },
    #[error("duplicate field name {name}")]
    DuplicateName { name: String },
    #[error("sweep at index {index} has sweep_number {sweep_number}")]
    SweepNumber { index: usize, sweep_number: u32 },
    /// [`Sweep::reorder_rays`] was given an order that is not a
    /// permutation of the sweep's rays.
    #[error("a ray order that is not a permutation of the sweep's {nrays} rays")]
    RayOrder {
        /// Rays in the sweep.
        nrays: usize,
    },
    #[error(transparent)]
    Geometry(#[from] GeometryError),
}

/// `length` is a whole number of `step`s (0 included), within `tolerance`.
fn is_multiple(length: f64, step: f64, tolerance: f64) -> bool {
    let steps = (length / step).round();
    (steps * step - length).abs() <= tolerance
}

impl Sweep {
    /// An empty sweep with an unset range and every optional item absent.
    pub fn new(sweep_number: u32, sweep_mode: SweepMode, fixed_angle_deg: f32) -> Self {
        Self {
            sweep_number,
            sweep_mode,
            follow_mode: None,
            prt_mode: None,
            polarization_mode: None,
            polarization_sequence: None,
            fixed_angle_deg,
            target_scan_rate_deg_per_s: None,
            rays_are_indexed: None,
            rays_angle_resolution_deg: None,
            qc_procedures: None,
            rays: Rays::default(),
            range: RangeCoord::UNSET,
            ray_vars: RayVariables::default(),
            monitoring: None,
            platform_track: None,
            extra_vars: Vec::new(),
            other: Vec::new(),
            fields: Vec::new(),
            elevation_number: None,
            complete: true,
        }
    }

    /// Number of rays.
    pub fn nrays(&self) -> usize {
        self.rays.len()
    }

    /// The sweep's elevation as one angle, the way the products take it:
    /// beam height and ground range of the column products, the lowest-tilt
    /// choice of the trackers and the bench, and the dealiasers' tilt
    /// geometry.
    ///
    /// For NEXRAD Level II (`source` is [`SourceFormat::NexradLevel2`]) this
    /// is the elevation of the first ray in storage order. The legacy model
    /// stored that value as the cut elevation, and the BowEcho app and `main`
    /// still use it. It differs from [`Sweep::fixed_angle_deg`], the
    /// Message 5 cut angle that xradar and Py-ART report, while the antenna
    /// settles onto the cut: KTLX 2024-03-15 sweeps 0, 1, 4 and 9 read 0.582,
    /// 0.483, 0.409 and 0.478 deg on 0.4834 deg cuts. For every other source,
    /// and for a Level II sweep without rays, it is `fixed_angle_deg`
    /// (design note `docs/design/fm301-model.md` section 5.2).
    ///
    /// `source` is the whole volume's `provenance.source_format`, so every
    /// sweep of a volume is read under one format. That holds after a merge
    /// too: [`merge_volumes`](crate::merge_volumes) rejects parts whose
    /// source formats differ, so no volume mixes sweeps whose tilt elevation
    /// follows different rules.
    pub fn tilt_elevation_deg(&self, source: SourceFormat) -> f32 {
        match source {
            SourceFormat::NexradLevel2 => self
                .rays
                .elevation_deg
                .first()
                .copied()
                .unwrap_or(self.fixed_angle_deg),
            _ => self.fixed_angle_deg,
        }
    }

    pub fn reserve_rays(&mut self, rays: usize) {
        self.rays.time_s.reserve(rays);
        self.rays.azimuth_deg.reserve(rays);
        self.rays.elevation_deg.reserve(rays);
    }

    /// Append a ray and return its index, which the decoder passes to
    /// `Field::push_row_*`.
    pub fn push_ray(&mut self, time_s: f64, azimuth_deg: f32, elevation_deg: f32) -> usize {
        let index = self.rays.azimuth_deg.len();
        self.rays.time_s.push(time_s);
        self.rays.azimuth_deg.push(azimuth_deg);
        self.rays.elevation_deg.push(elevation_deg);
        index
    }

    /// Register a field's native geometry (centre of its first gate, spacing,
    /// gate count) and return its mapping onto the sweep range, growing or
    /// refining the range as needed (section 6.5).
    ///
    /// - A spacing that is an integer multiple of the range spacing maps with
    ///   that stride; a finer spacing that divides it refines the range and
    ///   rewrites the mappings of fields already in [`Sweep::fields`] (no gate
    ///   data moves). Add a field to `fields` before attaching the next
    ///   geometry.
    /// - When one spacing divides the other and the first gates are a whole
    ///   number of the finer spacing apart centre to centre, but not edge to
    ///   edge, the range is refined to half the finer spacing (gates of 1 km
    ///   and 500 m both centred at 0 m map onto a 250 m range with strides 4
    ///   and 2).
    /// - A start edge before the range extends the range backwards.
    /// - Anything else is [`GeometryError::Unaligned`]. Tolerance: 1e-6 of the
    ///   range spacing.
    pub fn attach_geometry(
        &mut self,
        first_center_m: f64,
        spacing_m: f64,
        ngates: u32,
    ) -> Result<GateMapping, GeometryError> {
        if !first_center_m.is_finite() || !spacing_m.is_finite() || spacing_m <= 0.0 {
            return Err(GeometryError::Invalid {
                first_center_m,
                spacing_m,
            });
        }
        let fields = &mut self.fields;
        match &mut self.range {
            RangeCoord::Explicit { centers_m } => {
                if ngates as usize > centers_m.len() {
                    return Err(GeometryError::ExplicitMismatch);
                }
                for (gate, center) in centers_m.iter().take(ngates as usize).enumerate() {
                    let expected = first_center_m + gate as f64 * spacing_m;
                    if (f64::from(*center) - expected).abs() > 1e-3_f64.max(1e-6 * spacing_m) {
                        return Err(GeometryError::ExplicitMismatch);
                    }
                }
                Ok(GateMapping::IDENTITY)
            }
            RangeCoord::Uniform {
                first_center_m: c0,
                spacing_m: s0,
                ngates: n0,
            } => {
                if *s0 <= 0.0 || !s0.is_finite() {
                    *c0 = first_center_m;
                    *s0 = spacing_m;
                    *n0 = ngates;
                    return Ok(GateMapping::IDENTITY);
                }
                let unaligned = GeometryError::Unaligned {
                    first_center_m,
                    spacing_m,
                    range_first_center_m: *c0,
                    range_spacing_m: *s0,
                };
                // Work on copies so a failed call leaves the sweep unchanged.
                let (mut range_c0, mut range_s0, mut range_n0) = (*c0, *s0, u64::from(*n0));
                let mut refine = 1u32;
                // The range spacing after this call: the finer of the two
                // spacings when it divides the other and the offset between
                // the start edges. When the spacings nest but the first
                // gates share a centre instead of an edge (converted feeds
                // put gates of 1 km and 500 m both centred at 0 m, whose
                // edges are 250 m apart), half the finer spacing.
                let tolerance = 1e-6 * range_s0;
                let edge_offset = (first_center_m - spacing_m / 2.0) - (range_c0 - range_s0 / 2.0);
                let finer = range_s0.min(spacing_m);
                let mut target = finer;
                if !is_multiple(edge_offset, finer, tolerance) {
                    let nested = is_multiple(range_s0.max(spacing_m), finer, tolerance);
                    let centred = is_multiple(first_center_m - range_c0, finer, tolerance);
                    if !(nested && centred) {
                        return Err(unaligned);
                    }
                    target = finer / 2.0;
                }
                if target < range_s0 - tolerance {
                    // The target spacing must divide the range spacing.
                    let k = (range_s0 / target).round();
                    if k < 2.0
                        || (k * target - range_s0).abs() > 1e-6 * range_s0
                        || k > f64::from(u32::MAX)
                    {
                        return Err(unaligned);
                    }
                    refine = k as u32;
                    let edge = range_c0 - range_s0 / 2.0;
                    range_s0 = target;
                    range_c0 = edge + target / 2.0;
                    range_n0 *= u64::from(refine);
                }
                let tolerance = 1e-6 * range_s0;
                let stride = (spacing_m / range_s0).round();
                if stride < 1.0
                    || (stride * range_s0 - spacing_m).abs() > tolerance
                    || stride > f64::from(u32::MAX)
                {
                    return Err(unaligned);
                }
                let edge = first_center_m - spacing_m / 2.0;
                let edge0 = range_c0 - range_s0 / 2.0;
                let m = ((edge - edge0) / range_s0).round();
                if (edge - edge0 - m * range_s0).abs() > tolerance || m.abs() > f64::from(u32::MAX)
                {
                    return Err(unaligned);
                }
                let shift = if m < 0.0 { (-m) as u64 } else { 0 };
                let start = if m < 0.0 { 0 } else { m as u64 };
                let mapping = GateMapping {
                    start: u32::try_from(start).map_err(|_| GeometryError::TooManyGates)?,
                    stride: stride as u32,
                };
                let end = mapping.end(ngates).ok_or(GeometryError::TooManyGates)?;
                let range_gates = u32::try_from((range_n0 + shift).max(end))
                    .map_err(|_| GeometryError::TooManyGates)?;
                let remap = |gates: GateMapping| -> Option<GateMapping> {
                    Some(GateMapping {
                        start: gates
                            .start
                            .checked_mul(refine)?
                            .checked_add(u32::try_from(shift).ok()?)?,
                        stride: gates.stride.max(1).checked_mul(refine)?,
                    })
                };
                if fields.iter().any(|field| remap(field.gates).is_none()) {
                    return Err(GeometryError::TooManyGates);
                }
                for field in fields.iter_mut() {
                    if let Some(gates) = remap(field.gates) {
                        field.gates = gates;
                    }
                }
                *c0 = range_c0 - shift as f64 * range_s0;
                *s0 = range_s0;
                *n0 = range_gates;
                Ok(mapping)
            }
        }
    }

    /// Append a field, rejecting a name already present (compared by
    /// `as_str`). Returns its index.
    pub fn add_field(&mut self, field: Field) -> Result<usize, SweepError> {
        if self.field(&field.name).is_some() {
            return Err(SweepError::DuplicateName {
                name: field.name.as_str().to_owned(),
            });
        }
        self.fields.push(field);
        Ok(self.fields.len() - 1)
    }

    pub fn field(&self, name: &FieldName) -> Option<&Field> {
        self.fields
            .iter()
            .find(|field| field.name.as_str() == name.as_str())
    }

    pub fn field_mut(&mut self, name: &FieldName) -> Option<&mut Field> {
        self.fields
            .iter_mut()
            .find(|field| field.name.as_str() == name.as_str())
    }

    pub fn field_index(&self, name: &FieldName) -> Option<usize> {
        self.fields
            .iter()
            .position(|field| field.name.as_str() == name.as_str())
    }

    /// Preferred field for a quantity: H before unspecified before V, then
    /// source order.
    pub fn find(&self, quantity: Quantity) -> Option<&Field> {
        fn rank(polarization: Polarization) -> u8 {
            match polarization {
                Polarization::H | Polarization::CopolarH => 0,
                Polarization::Unspecified | Polarization::Hv => 1,
                Polarization::V | Polarization::CopolarV => 2,
                Polarization::CrosspolarH | Polarization::CrosspolarV => 3,
            }
        }
        self.fields
            .iter()
            .filter(|field| field.quantity == quantity)
            .min_by_key(|field| rank(field.polarization))
    }

    /// Put the rays in `order` (`order[i]` is the current row that becomes
    /// row `i`; a permutation of `0..nrays`): the ray coordinates, every
    /// per-ray variable, the monitoring and platform track vectors, the
    /// per-ray extra variables and every field's rows (absent rows
    /// included). Returns [`SweepError::RayOrder`] and leaves the sweep
    /// unchanged when `order` is not a permutation of the rays. The sweep
    /// must be sealed (every field has a row per ray).
    pub fn reorder_rays(&mut self, order: &[usize]) -> Result<(), SweepError> {
        let nrays = self.nrays();
        let mut seen = vec![false; nrays];
        if order.len() != nrays
            || order
                .iter()
                .any(|row| *row >= nrays || std::mem::replace(&mut seen[*row], true))
        {
            return Err(SweepError::RayOrder { nrays });
        }
        if self
            .fields
            .iter()
            .any(|field| field.nrays as usize != nrays)
        {
            return Err(SweepError::RayOrder { nrays });
        }
        fn take<T: Copy>(values: &mut Vec<T>, order: &[usize]) {
            if values.len() == order.len() {
                *values = order.iter().map(|row| values[*row]).collect();
            }
        }
        fn take_opt<T: Copy>(values: &mut Option<Vec<T>>, order: &[usize]) {
            if let Some(values) = values {
                take(values, order);
            }
        }
        fn take_rows<T: Copy>(values: &mut Vec<T>, row_len: usize, order: &[usize]) {
            if row_len == 0 || values.len() != order.len() * row_len {
                return;
            }
            let mut out = Vec::with_capacity(values.len());
            for row in order {
                out.extend_from_slice(&values[row * row_len..(row + 1) * row_len]);
            }
            *values = out;
        }
        take(&mut self.rays.time_s, order);
        take(&mut self.rays.azimuth_deg, order);
        take(&mut self.rays.elevation_deg, order);
        let vars = &mut self.ray_vars;
        take_opt(&mut vars.nyquist_velocity_mps, order);
        take_opt(&mut vars.unambiguous_range_m, order);
        take_opt(&mut vars.prt_s, order);
        take_opt(&mut vars.prt_ratio, order);
        take_opt(&mut vars.n_samples, order);
        take_opt(&mut vars.pulse_width_s, order);
        take_opt(&mut vars.scan_rate_deg_per_s, order);
        take_opt(&mut vars.antenna_transition, order);
        take_opt(&mut vars.calib_index, order);
        take_opt(&mut vars.rx_range_resolution_m, order);
        take_opt(&mut vars.independent_samples, order);
        if let Some(sequence) = &mut vars.prt_sequence_s {
            take_rows(&mut sequence.values_s, sequence.nprt as usize, order);
        }
        if let Some(monitoring) = &mut self.monitoring {
            for values in [
                &mut monitoring.radar_measured_transmit_power_h_dbm,
                &mut monitoring.radar_measured_transmit_power_v_dbm,
                &mut monitoring.radar_measured_sky_noise_dbm,
                &mut monitoring.radar_measured_cold_noise_dbm,
                &mut monitoring.radar_measured_hot_noise_dbm,
                &mut monitoring.phase_difference_transmit_hv_deg,
                &mut monitoring.antenna_pointing_accuracy_elev_deg,
                &mut monitoring.antenna_pointing_accuracy_az_deg,
                &mut monitoring.calibration_offset_h_db,
                &mut monitoring.calibration_offset_v_db,
                &mut monitoring.zdr_offset_db,
            ] {
                take_opt(values, order);
            }
        }
        if let Some(track) = &mut self.platform_track {
            take(&mut track.latitude_deg, order);
            take(&mut track.longitude_deg, order);
            take(&mut track.altitude_m, order);
            take_opt(&mut track.altitude_agl_m, order);
            for values in [
                &mut track.heading_deg,
                &mut track.roll_deg,
                &mut track.pitch_deg,
                &mut track.drift_deg,
                &mut track.rotation_deg,
                &mut track.tilt_deg,
            ] {
                take_opt(values, order);
            }
        }
        let order_u32: Vec<u32> = order.iter().map(|row| *row as u32).collect();
        for extra in &mut self.extra_vars {
            if extra.is_per_ray() {
                let row_len: usize = extra.shape.iter().skip(1).map(|n| *n as usize).product();
                if let Some(values) = extra.values.take_rows(row_len.max(1), &order_u32) {
                    extra.values = values;
                }
            }
        }
        // Where each old row went, for the absent rows.
        let mut new_row = vec![0u32; nrays];
        for (to, from) in order.iter().enumerate() {
            new_row[*from] = to as u32;
        }
        for field in &mut self.fields {
            let row_len = field.ngates as usize;
            match &mut field.data {
                super::field::FieldData::U8 { values, .. } => take_rows(values, row_len, order),
                super::field::FieldData::U16 { values, .. } => take_rows(values, row_len, order),
                super::field::FieldData::I8 { values, .. } => take_rows(values, row_len, order),
                super::field::FieldData::I16 { values, .. } => take_rows(values, row_len, order),
                super::field::FieldData::I32 { values, .. } => take_rows(values, row_len, order),
                super::field::FieldData::F32 { values, .. } => take_rows(values, row_len, order),
                super::field::FieldData::F64 { values, .. } => take_rows(values, row_len, order),
            }
            for row in &mut field.absent_rows {
                *row = new_row[*row as usize];
            }
            field.absent_rows.sort_unstable();
        }
        Ok(())
    }

    /// Append absent rows for rays at the end that a field never received, grow
    /// a uniform range to cover every field, then check the invariants
    /// (section 3). Decoders call it once per sweep.
    pub fn seal(&mut self) -> Result<(), SweepError> {
        let nrays = self.rays.azimuth_deg.len();
        let ray_len = |what: &str, len: usize| -> Result<(), SweepError> {
            if len == nrays {
                Ok(())
            } else {
                Err(SweepError::RayLength {
                    what: what.to_owned(),
                    len,
                    nrays,
                })
            }
        };
        ray_len("time", self.rays.time_s.len())?;
        ray_len("elevation", self.rays.elevation_deg.len())?;
        for (name, len) in self.ray_vars.lengths() {
            ray_len(name, len)?;
        }
        if let Some(seq) = &self.ray_vars.prt_sequence_s
            && seq.values_s.len() != nrays * seq.nprt as usize
        {
            return Err(SweepError::RayLength {
                what: "prt_sequence values".to_owned(),
                len: seq.values_s.len(),
                nrays,
            });
        }
        if let Some(monitoring) = &self.monitoring {
            for (name, values) in monitoring.variables() {
                ray_len(name, values.len())?;
            }
        }
        if let Some(track) = &self.platform_track {
            ray_len("latitude", track.latitude_deg.len())?;
            ray_len("longitude", track.longitude_deg.len())?;
            ray_len("altitude", track.altitude_m.len())?;
            for (name, len) in [
                ("altitude_agl", track.altitude_agl_m.as_ref().map(Vec::len)),
                ("heading", track.heading_deg.as_ref().map(Vec::len)),
                ("roll", track.roll_deg.as_ref().map(Vec::len)),
                ("pitch", track.pitch_deg.as_ref().map(Vec::len)),
                ("drift", track.drift_deg.as_ref().map(Vec::len)),
                ("rotation", track.rotation_deg.as_ref().map(Vec::len)),
                ("tilt", track.tilt_deg.as_ref().map(Vec::len)),
            ] {
                if let Some(len) = len {
                    ray_len(name, len)?;
                }
            }
        }
        for extra in &self.extra_vars {
            if extra.is_per_ray() {
                ray_len(&extra.name, extra.shape.first().map_or(0, |n| *n as usize))?;
            }
        }

        let mut names = HashSet::with_capacity(self.fields.len());
        let range_gates = self.range.ngates();
        let explicit = matches!(self.range, RangeCoord::Explicit { .. });
        let mut needed_gates = range_gates as u64;
        for field in &mut self.fields {
            let label = field.name.as_str().to_owned();
            if !names.insert(label.clone()) {
                return Err(SweepError::DuplicateName { name: label });
            }
            field
                .push_absent_rows_to(nrays)
                .map_err(|source| SweepError::Field {
                    field: label.clone(),
                    source,
                })?;
            let expected = nrays * field.ngates as usize;
            if field.data.len() != expected {
                return Err(SweepError::FieldLength {
                    field: label,
                    len: field.data.len(),
                    expected,
                });
            }
            if !field.absent_rows.windows(2).all(|pair| pair[0] < pair[1])
                || field
                    .absent_rows
                    .last()
                    .is_some_and(|last| *last as usize >= nrays)
            {
                return Err(SweepError::AbsentRows { field: label });
            }
            if field.gates.stride == 0 || (explicit && field.gates.stride != 1) {
                return Err(SweepError::Stride {
                    field: label,
                    stride: field.gates.stride,
                });
            }
            let end = field.gates.end(field.ngates).unwrap_or(u64::MAX);
            if explicit && end > range_gates as u64 {
                return Err(SweepError::FieldExtent {
                    field: label,
                    end,
                    range_gates,
                });
            }
            needed_gates = needed_gates.max(end);
        }
        if let RangeCoord::Uniform { ngates, .. } = &mut self.range {
            *ngates = u32::try_from(needed_gates).map_err(|_| GeometryError::TooManyGates)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn mode_strings_parse_and_keep_unknown_spellings() {
        assert_eq!(SweepMode::parse("rhi"), SweepMode::Rhi);
        assert_eq!(
            SweepMode::parse("calibration"),
            SweepMode::Other("calibration".into())
        );
        assert_eq!(FollowMode::parse("not_set").as_str(), "not_set");
        assert_eq!(PolarizationMode::HvSim.as_str(), "hv_sim");
    }
}
