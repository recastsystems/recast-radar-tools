//! Product dependent halfwords of the Product Description Block (ICD 2620001AD
//! Table V; 2620001P and 2620001H Table V for retired products; 2620063E
//! Table V for TDWR products; for the 1990s codes no later revision lists
//! (39, 40, 42, 49, 52, 53, 68-72, 74 and 83 edit times, 77, 88), the Table V
//! that NCDC reproduces in its Level III documentation DSI-7000): halfwords
//! 27, 28, 30 and 47-53 decoded by name, type and scale for each product code.
//!
//! [`ProductDescription::parameters`] returns them in halfword order. The data
//! level threshold halfwords (31-46) are not parameters: [`crate::levels`]
//! decodes them. Every halfword stays available raw in
//! [`ProductDescription::halfwords`].
//!
//! Supplemental scan (halfword 50 bits 0-4 of elevation products): Table V
//! Note 24 gives 1 = SAILS and 2 = MRLE. Real products carry the opposite
//! (`docs/level3/reference.md` section 5): KDDC 2020-08-17 had SAILS on and
//! MRLE off (its General Status Message lists supplemental cuts `AVSET SAILS`)
//! and its mid-volume 0.5 degree cut (elevation number 4, 143 s into the
//! volume) carries code 2. The decoder follows the products, as MetPy 1.7.1
//! does.

use chrono::{DateTime, Utc};

use crate::header::ProductDescription;

/// A decoded product dependent value.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ParameterValue {
    /// An integer (counts, flags, codes, integer-valued quantities).
    Int(i64),
    /// A scaled or `REAL*4` quantity.
    Float(f64),
    /// A date and time (a Julian date halfword paired with minutes after
    /// midnight).
    Time(DateTime<Utc>),
    /// A date alone (Julian date halfword), `YYYY-MM-DD`.
    Date(String),
    /// A named code.
    Text(&'static str),
    /// Characters stored in the halfword (high byte first), one `char` per
    /// byte (ISO 8859-1), such as the storm ID of product 53.
    Characters(String),
}

/// One product dependent value of a Product Description Block.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductParameter {
    /// First ICD halfword the value comes from (27-53).
    pub halfword: u8,
    /// Name, lower case with underscores, e.g. `max_reflectivity`.
    pub name: &'static str,
    /// UDUNITS units of the value, when it has any.
    pub units: Option<&'static str>,
    /// The value.
    pub value: ParameterValue,
}

/// How a halfword (or pair) is decoded.
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// Signed halfword.
    Int,
    /// Unsigned halfword.
    UInt,
    /// Signed halfword times a factor.
    Scaled(f64),
    /// `REAL*4` in this halfword (most significant) and the next.
    Real32,
    /// Unsigned 32-bit integer in this halfword (most significant) and the next.
    UInt32,
    /// Julian date (1 = 1970-01-01).
    Date,
    /// Julian date in this halfword, minutes after midnight in the next.
    DateMinutes,
    /// Julian date in this halfword, hours after midnight in the next.
    DateHours,
    /// High byte of the halfword.
    HighByte,
    /// Low byte of the halfword.
    LowByte,
    /// Halfword 50 bits 5-15: seconds from volume start to elevation start.
    DeltaTime,
    /// Halfword 50 bits 0-4: supplemental scan type.
    SupplementalScan,
    /// Two characters, high byte first.
    Chars,
}

/// `(halfword, name, kind, units)`.
type Spec = (u8, &'static str, Kind, Option<&'static str>);

const DEG: Option<&str> = Some("degree");
const DBZ: Option<&str> = Some("dBZ");
const KT: Option<&str> = Some("kt");
const INCH: Option<&str> = Some("in");
const KFT: Option<&str> = Some("kft");
const NMI: Option<&str> = Some("nmi");
const SEC: Option<&str> = Some("s");
const MIN: Option<&str> = Some("min");
const BYTES: Option<&str> = Some("bytes");
const NONE: Option<&str> = None;

const ELEVATION: Spec = (30, "elevation_angle", Kind::Scaled(0.1), DEG);
const AVSET: Spec = (
    30,
    "avset_termination_elevation_angle",
    Kind::Scaled(0.1),
    DEG,
);
const MAX_REFL: Spec = (47, "max_reflectivity", Kind::Int, DBZ);
const CALIBRATION: Spec = (51, "calibration_constant", Kind::Real32, Some("dB"));
const DELTA_TIME: Spec = (50, "elevation_delta_time", Kind::DeltaTime, SEC);
const SUPPLEMENTAL: Spec = (50, "supplemental_scan", Kind::SupplementalScan, NONE);
const COMPRESSION: Spec = (51, "compression_method", Kind::Int, NONE);
const UNCOMPRESSED: Spec = (52, "uncompressed_size", Kind::UInt32, BYTES);
const MAX_NEG_VEL: Spec = (47, "max_negative_velocity", Kind::Int, KT);
const MAX_POS_VEL: Spec = (48, "max_positive_velocity", Kind::Int, KT);
const MAX_SW: Spec = (47, "max_spectrum_width", Kind::Int, KT);
const WINDOW_AZ: Spec = (27, "window_azimuth", Kind::Scaled(0.1), DEG);
const WINDOW_RANGE: Spec = (28, "window_range", Kind::Scaled(0.1), NMI);
const BIAS_50: Spec = (50, "mean_field_bias", Kind::Scaled(0.01), NONE);
const NULL_PRODUCT_LOW: Spec = (30, "null_product_flag", Kind::LowByte, NONE);
const END_DATE_48: Spec = (48, "rainfall_end", Kind::DateMinutes, NONE);
const MAX_ACCUM_47: Spec = (47, "max_accumulation", Kind::Scaled(0.1), INCH);

/// Table V entries of a product code.
fn specs(code: i16) -> Vec<Spec> {
    use Kind::*;
    let mut v: Vec<Spec> = Vec::new();
    match code {
        16..=18 | 21 => v.extend([ELEVATION, MAX_REFL, CALIBRATION]),
        19 | 20 => v.extend([ELEVATION, MAX_REFL, DELTA_TIME, SUPPLEMENTAL, CALIBRATION]),
        22..=26 => v.extend([ELEVATION, MAX_NEG_VEL, MAX_POS_VEL]),
        27 => v.extend([
            ELEVATION,
            MAX_NEG_VEL,
            MAX_POS_VEL,
            DELTA_TIME,
            SUPPLEMENTAL,
        ]),
        28 | 29 => v.extend([ELEVATION, MAX_SW]),
        30 => v.extend([ELEVATION, MAX_SW, DELTA_TIME, SUPPLEMENTAL]),
        31 => v.extend([
            (27, "end_hour", Int, Some("h")),
            (28, "time_span", Int, Some("h")),
            (30, "null_product_flag", Int, NONE),
            (47, "max_rainfall", Scaled(0.1), INCH),
            (48, "rainfall_begin", DateMinutes, NONE),
            (50, "rainfall_end", DateMinutes, NONE),
            (52, "mean_field_bias", Scaled(0.01), NONE),
            (53, "gage_radar_pairs", Scaled(0.01), NONE),
        ]),
        32 => v.extend([
            MAX_REFL,
            (48, "hybrid_scan_date", Date, NONE),
            (49, "hybrid_scan_average_time", Int, MIN),
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        33 => v.extend([MAX_REFL, (48, "hybrid_scan_time", DateMinutes, NONE)]),
        34 => v.extend([
            (27, "segment_bit_map", UInt, NONE),
            (28, "cmd_generated_bypass_map", Int, NONE),
            (48, "bypass_map_time", DateMinutes, NONE),
            (50, "clutter_filter_map_time", DateMinutes, NONE),
        ]),
        95 | 96 | 98 => v.extend([MAX_REFL, CALIBRATION]),
        // Halfword 30 of 35 and 36 is not in any Table V obtained; real 36
        // products carry 0 there, as 37 and 38 do without AVSET.
        35..=38 | 97 => v.extend([AVSET, MAX_REFL, CALIBRATION]),
        // Composite Reflectivity Contour (DSI-7000 Table V). KGRR 2001-10-11:
        // halfword 47 is the largest DBZM of its cell attribute table,
        // halfwords 51-52 the calibration constant of the composite
        // reflectivity products of the same volume, and the contour interval
        // (5 dBZ) the step of its thresholds.
        39 | 40 => v.extend([MAX_REFL, CALIBRATION, (53, "contour_interval", Int, DBZ)]),
        41 => v.extend([AVSET, (47, "max_echo_top", Int, KFT)]),
        // Echo Tops Contour (DSI-7000 Table V; 0 = no echoes detected, Note
        // 5). KIND 1994-09-10: 37 kft and a 5000 ft interval, the step of
        // its thresholds.
        42 => v.extend([
            (47, "max_echo_top", Int, KFT),
            (53, "contour_interval", Int, Some("ft")),
        ]),
        43 => v.extend([
            WINDOW_AZ,
            WINDOW_RANGE,
            ELEVATION,
            MAX_REFL,
            (49, "height_of_phenomena", Int, KFT),
        ]),
        44 => v.extend([
            WINDOW_AZ,
            WINDOW_RANGE,
            ELEVATION,
            MAX_NEG_VEL,
            MAX_POS_VEL,
            (49, "height_of_phenomena", Int, KFT),
        ]),
        45 => v.extend([
            WINDOW_AZ,
            WINDOW_RANGE,
            ELEVATION,
            MAX_SW,
            (49, "height_of_phenomena", Int, KFT),
        ]),
        46 => v.extend([
            WINDOW_AZ,
            WINDOW_RANGE,
            ELEVATION,
            (47, "max_negative_shear", Scaled(0.001), Some("s-1")),
            (48, "max_positive_shear", Scaled(0.001), Some("s-1")),
            (49, "height_of_phenomena", Int, KFT),
        ]),
        47 => v.extend([
            (47, "max_severe_weather_probability", Int, Some("percent")),
            (48, "max_box_size", Scaled(0.1), NMI),
        ]),
        // Combined Moment (DSI-7000 Table V).
        49 => v.extend([
            WINDOW_AZ,
            WINDOW_RANGE,
            ELEVATION,
            MAX_REFL,
            (48, "max_negative_velocity", Int, KT),
            (49, "max_positive_velocity", Int, KT),
            (50, "max_spectrum_width", Int, KT),
        ]),
        48 => v.extend([
            (47, "max_wind_speed", Int, KT),
            (48, "max_wind_direction", Int, DEG),
            (49, "max_wind_altitude", Scaled(10.0), Some("ft")),
        ]),
        50..=52 | 85 | 86 => {
            v.extend([
                (47, "point1_azimuth", Scaled(0.1), DEG),
                (48, "point1_range", Scaled(0.1), NMI),
                (49, "point2_azimuth", Scaled(0.1), DEG),
                (50, "point2_range", Scaled(0.1), NMI),
            ]);
            if matches!(code, 50 | 85) {
                v.push(CALIBRATION);
            }
        }
        // Weak Echo Region (DSI-7000 Table V; checked on KCAE 1994-06-29
        // and KLOT 1994-11-06): halfwords 27-28 centre the window (the
        // volume's placement fits the base reflectivity of the same volume),
        // halfword 47 falls within the highest data level of the slices,
        // halfword 48 holds the two characters of the storm the window is
        // centred on (KCAE `53`, storm 53 of the storm tracking product of
        // the same volume at 186 deg / 60 nmi; KLOT `NS`, with the window at
        // the radar), and halfwords 49-50 are the elevation bit map of the
        // product request (DSI-7000 Table IIa Note 4): counting from the most
        // significant bit of halfword 49 as bit 0 (unused), bit `n` selects
        // elevation cut `n` (1-20), one bit per slice.
        53 => v.extend([
            WINDOW_AZ,
            WINDOW_RANGE,
            MAX_REFL,
            (48, "storm_id", Chars, NONE),
            (49, "elevation_bit_map", UInt32, NONE),
        ]),
        55 => v.extend([
            WINDOW_AZ,
            WINDOW_RANGE,
            ELEVATION,
            MAX_NEG_VEL,
            MAX_POS_VEL,
            (49, "motion_source_flag", Int, NONE),
            (50, "height_of_phenomena", Int, KFT),
            (51, "storm_speed", Scaled(0.1), KT),
            (52, "storm_direction", Scaled(0.1), DEG),
            (53, "alert_category", Int, NONE),
        ]),
        56 => v.extend([
            ELEVATION,
            MAX_NEG_VEL,
            MAX_POS_VEL,
            (49, "motion_source_flag", Int, NONE),
            (51, "average_storm_speed", Scaled(0.1), KT),
            (52, "average_storm_direction", Scaled(0.1), DEG),
        ]),
        57 => v.extend([AVSET, (47, "max_vil", Int, Some("kg m-2"))]),
        58 => v.push((47, "number_of_storms", Int, NONE)),
        61 => v.extend([
            (47, "number_of_tvs", Int, NONE),
            (48, "number_of_etvs", Int, NONE),
        ]),
        63 | 64 | 89 => v.extend([
            MAX_REFL,
            (48, "layer_bottom", Int, KFT),
            (49, "layer_top", Int, KFT),
            CALIBRATION,
        ]),
        // 2620001P lists no halfword 30 for 65; real products carry the AVSET
        // termination angle there, as 66 does (KTLX 2013: 19.5, KRAX 2022: 6.4).
        65..=67 | 90 => v.extend([
            AVSET,
            MAX_REFL,
            (48, "layer_bottom", Int, KFT),
            (49, "layer_top", Int, KFT),
            CALIBRATION,
        ]),
        // Layer Composite Turbulence (DSI-7000 Table V; no product found):
        // the maximum is in 0.1 cm^(2/3) s^-1 (the cube root of the eddy
        // dissipation rate).
        68..=72 => v.extend([
            (47, "max_turbulence", Scaled(0.1), NONE),
            (48, "layer_bottom", Int, KFT),
            (49, "layer_top", Int, KFT),
        ]),
        // Radar Coded Message and its unedited version (DSI-7000 Table V):
        // the RPG operator's edit decision time and editing timeout (60-540
        // and 60-1800 s; real IRM products carry 60 or 120, real RCMs 0) and
        // the edited indicator (nonzero when edited).
        74 | 83 => {
            v.extend([
                (49, "edit_decision_time", Int, SEC),
                (50, "editing_timeout", Int, SEC),
            ]);
            if code == 74 {
                v.push((51, "edited_indicator", Int, NONE));
            }
        }
        75 => v.push((47, "rpg_id", Int, NONE)),
        // PUP Text Message (DSI-7000 Table V): 0 = to all dedicated users,
        // else the line of the user.
        77 => v.extend([
            (47, "pup_id", Int, NONE),
            (49, "user_designation", Int, NONE),
        ]),
        78 | 79 => v.extend([
            (47, "max_rainfall", Scaled(0.1), INCH),
            (48, "mean_field_bias", Scaled(0.01), NONE),
            (49, "gage_radar_pairs", Scaled(0.01), NONE),
            (50, "rainfall_end", DateMinutes, NONE),
        ]),
        80 => v.extend([
            (47, "max_rainfall", Scaled(0.1), INCH),
            (48, "rainfall_begin", DateMinutes, NONE),
            (50, "rainfall_end", DateMinutes, NONE),
            (52, "mean_field_bias", Scaled(0.01), NONE),
            (53, "gage_radar_pairs", Scaled(0.01), NONE),
        ]),
        81 => v.extend([
            (47, "max_rainfall", Scaled(0.001), Some("dBA")),
            (48, "mean_field_bias", Scaled(0.01), NONE),
            (49, "gage_radar_pairs", Scaled(0.01), NONE),
            (50, "rainfall_end", DateMinutes, NONE),
        ]),
        84 => v.extend([
            (30, "wind_altitude", Int, KFT),
            (47, "wind_speed", Int, KT),
            (48, "wind_direction", Int, DEG),
            (49, "elevation_angle", Scaled(0.1), DEG),
            (50, "slant_range", Scaled(0.1), NMI),
            (51, "rms_error", Int, KT),
        ]),
        87 | 88 => v.extend([
            ELEVATION,
            (47, "max_shear", Scaled(0.001), Some("s-1")),
            (48, "max_shear_azimuth", Scaled(0.1), DEG),
            (49, "max_shear_range", Scaled(0.1), NMI),
            (50, "resolution", Scaled(0.01), NMI),
        ]),
        93 => v.extend([
            ELEVATION,
            MAX_NEG_VEL,
            MAX_POS_VEL,
            (50, "velocity_precision_code", Int, NONE),
        ]),
        94 | 153 => v.extend([
            ELEVATION,
            MAX_REFL,
            DELTA_TIME,
            SUPPLEMENTAL,
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        99 | 154 => v.extend([
            ELEVATION,
            MAX_NEG_VEL,
            MAX_POS_VEL,
            DELTA_TIME,
            SUPPLEMENTAL,
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        113 => v.extend([
            (27, "rpg_cut_number", Int, NONE),
            (28, "cmd_generated", Int, NONE),
            ELEVATION,
            (47, "clutter_filter_map_minutes", Int, MIN),
            (48, "clutter_filter_map_date", Date, NONE),
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        132 => v.extend([ELEVATION, DELTA_TIME, SUPPLEMENTAL]),
        133 | 139 | 164 => v.push(ELEVATION),
        134 => v.extend([
            AVSET,
            (47, "max_vil", Int, Some("kg m-2")),
            (48, "edited_radials", Int, NONE),
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        135 => v.extend([
            AVSET,
            (47, "max_echo_top", Int, KFT),
            (48, "edited_radials", Int, NONE),
            (49, "reflectivity_threshold", Int, DBZ),
            (50, "spurious_points_removed", Int, NONE),
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        137 => v.extend([
            (27, "requested_layer_bottom", Int, KFT),
            (28, "requested_layer_top", Int, KFT),
            MAX_REFL,
            (48, "layer_bottom", Int, KFT),
            (49, "layer_top", Int, KFT),
        ]),
        138 => v.extend([
            (27, "rainfall_begin", DateMinutes, NONE),
            (30, "mean_field_bias", Scaled(0.01), NONE),
            (47, "max_rainfall", Scaled(0.01), INCH),
            (48, "rainfall_end", DateMinutes, NONE),
            (50, "gage_radar_pairs", Scaled(0.01), NONE),
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        140 | 196 => {
            if code == 196 {
                v.push((27, "half_degree_scan_count", Int, NONE));
            }
            v.push((49, "detection_count", Int, NONE));
        }
        141 => v.extend([
            (27, "min_reflectivity_threshold", Int, DBZ),
            (28, "overlap_display_filter", Int, NONE),
            (30, "min_display_strength_rank", Int, NONE),
        ]),
        143 => v.extend([
            ELEVATION,
            (47, "number_of_tvs", Int, NONE),
            (48, "number_of_etvs", Int, NONE),
            DELTA_TIME,
            SUPPLEMENTAL,
        ]),
        144..=147 => {
            let scale = match code {
                144 => 0.001,
                145 | 146 => 0.01,
                _ => 0.1,
            };
            v.extend([
                (27, "missing_period", Int, MIN),
                (30, "use_rca_flag", Int, NONE),
                (47, "max_accumulation", Scaled(scale), INCH),
                (48, "accumulation_begin", DateMinutes, NONE),
                (50, "accumulation_end", DateMinutes, NONE),
                (52, "max_azimuth", Scaled(0.1), DEG),
                (53, "max_range", Scaled(0.1), NMI),
            ]);
        }
        149 => v.extend([
            (27, "min_reflectivity_threshold", Int, DBZ),
            ELEVATION,
            DELTA_TIME,
            SUPPLEMENTAL,
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        150 | 151 => v.extend([
            (27, "end_hour", Int, Some("h")),
            (28, "time_span", Int, Some("h")),
            (30, "scale_and_rca_flags", UInt, NONE),
            (47, "max_accumulation", Scaled(0.01), INCH),
            (48, "accumulation_begin", DateHours, NONE),
            (50, "accumulation_end", DateHours, NONE),
            (52, "max_azimuth", Scaled(0.1), DEG),
            (53, "max_range", Scaled(0.1), NMI),
        ]),
        152 | 189..=192 | 202 => v.extend([COMPRESSION, UNCOMPRESSED]),
        155 => v.extend([
            ELEVATION,
            MAX_SW,
            DELTA_TIME,
            SUPPLEMENTAL,
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        156 | 157 => v.extend([
            (27, "elevation_start_time", Int, NONE),
            (28, "elevation_end_time", Int, NONE),
            ELEVATION,
            (47, "min_value", Scaled(0.01), NONE),
            (48, "mean_value", Scaled(0.01), NONE),
            (49, "max_value", Scaled(0.01), NONE),
        ]),
        158 => v.extend([
            ELEVATION,
            (47, "min_zdr", Scaled(0.1), Some("dB")),
            (48, "max_zdr", Scaled(0.1), Some("dB")),
        ]),
        159 => v.extend([
            ELEVATION,
            (47, "min_zdr", Scaled(0.1), Some("dB")),
            (48, "max_zdr", Scaled(0.1), Some("dB")),
            DELTA_TIME,
            SUPPLEMENTAL,
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        160 => v.extend([
            ELEVATION,
            (47, "min_cc", Scaled(1.0 / 300.0), Some("1")),
            (48, "max_cc", Scaled(1.0 / 300.0), Some("1")),
        ]),
        161 | 167 => v.extend([
            ELEVATION,
            (47, "min_cc", Scaled(1.0 / 300.0), Some("1")),
            (48, "max_cc", Scaled(1.0 / 300.0), Some("1")),
            DELTA_TIME,
            SUPPLEMENTAL,
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        162 => v.extend([
            ELEVATION,
            (47, "min_kdp", Scaled(0.05), Some("degree km-1")),
            (48, "max_kdp", Scaled(0.05), Some("degree km-1")),
        ]),
        163 => v.extend([
            ELEVATION,
            (47, "min_kdp", Scaled(0.05), Some("degree km-1")),
            (48, "max_kdp", Scaled(0.05), Some("degree km-1")),
            DELTA_TIME,
            SUPPLEMENTAL,
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        165 => v.extend([
            ELEVATION,
            DELTA_TIME,
            SUPPLEMENTAL,
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        166 => v.extend([ELEVATION, DELTA_TIME, SUPPLEMENTAL]),
        168 => v.extend([
            ELEVATION,
            (47, "min_phidp", Int, DEG),
            (48, "max_phidp", Int, DEG),
            DELTA_TIME,
            SUPPLEMENTAL,
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        169 => v.extend([
            NULL_PRODUCT_LOW,
            MAX_ACCUM_47,
            END_DATE_48,
            BIAS_50,
            (51, "gage_radar_pairs", Scaled(0.01), NONE),
        ]),
        170 => v.extend([
            (27, "threshold_minimum_time", Int, MIN),
            (28, "total_time", Int, MIN),
            NULL_PRODUCT_LOW,
            MAX_ACCUM_47,
            END_DATE_48,
            BIAS_50,
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        171 => v.extend([
            (27, "accumulation_begin", DateMinutes, NONE),
            NULL_PRODUCT_LOW,
            MAX_ACCUM_47,
            END_DATE_48,
            BIAS_50,
            (51, "gage_radar_pairs", Scaled(0.01), NONE),
        ]),
        172 => v.extend([
            (27, "accumulation_begin", DateMinutes, NONE),
            NULL_PRODUCT_LOW,
            MAX_ACCUM_47,
            END_DATE_48,
            BIAS_50,
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        173 => v.extend([
            (27, "end_time", Int, MIN),
            (28, "time_span", Int, MIN),
            (30, "missing_period_flag", HighByte, NONE),
            NULL_PRODUCT_LOW,
            MAX_ACCUM_47,
            (48, "end_date", Date, NONE),
            (49, "start_time", Int, MIN),
            BIAS_50,
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        174 => v.extend([
            (47, "max_difference", Scaled(0.1), INCH),
            END_DATE_48,
            (50, "min_difference", Scaled(0.1), INCH),
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        175 => v.extend([
            (27, "accumulation_begin", DateMinutes, NONE),
            NULL_PRODUCT_LOW,
            (47, "max_difference", Scaled(0.1), INCH),
            END_DATE_48,
            (50, "min_difference", Scaled(0.1), INCH),
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        176 => v.extend([
            (27, "hybrid_rate_scan_time", DateMinutes, NONE),
            (30, "precipitation_detected_flag", HighByte, NONE),
            (30, "bias_applied_flag", LowByte, NONE),
            (47, "max_rate", Scaled(0.001), Some("in h-1")),
            (48, "percent_bins_filled", Scaled(0.01), Some("percent")),
            (49, "highest_elevation_angle", Scaled(0.1), DEG),
            BIAS_50,
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        177 | 197 => {
            v.extend([
                (47, "mode_filter_size", Int, NONE),
                (48, "percent_bins_filled", Scaled(0.01), Some("percent")),
                (49, "highest_elevation_angle", Scaled(0.1), DEG),
            ]);
            if code == 197 {
                v.push((50, "dry_snow_multiplier", Scaled(0.1), NONE));
            }
            v.extend([COMPRESSION, UNCOMPRESSED]);
        }
        178 => v.extend([
            AVSET,
            (47, "max_icing_top", Int, KFT),
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        179 => v.extend([
            AVSET,
            (47, "max_hail_top", Int, KFT),
            (48, "hsda_status", Int, NONE),
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        180 | 186 => v.extend([ELEVATION, MAX_REFL, COMPRESSION, UNCOMPRESSED]),
        181 | 187 => v.extend([ELEVATION, MAX_REFL]),
        182 => v.extend([
            ELEVATION,
            MAX_NEG_VEL,
            MAX_POS_VEL,
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        183 => v.extend([ELEVATION, MAX_NEG_VEL, MAX_POS_VEL]),
        184 => v.extend([ELEVATION, MAX_SW, COMPRESSION, UNCOMPRESSED]),
        185 => v.extend([ELEVATION, MAX_SW]),
        193 => v.extend([
            ELEVATION,
            MAX_REFL,
            (48, "edited_radials", Int, NONE),
            (49, "avset_status", Int, NONE),
            (50, "chaff_detection_status", Int, NONE),
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        195 => v.extend([
            ELEVATION,
            MAX_REFL,
            (48, "edited_radials", Int, NONE),
            (49, "avset_status", Int, NONE),
            COMPRESSION,
            UNCOMPRESSED,
        ]),
        _ => {}
    }
    v
}

/// `1970-01-01 + (days - 1)` plus `seconds`, or `None` for date 0 (unset).
fn julian(days: u16, seconds: i64) -> Option<DateTime<Utc>> {
    if days == 0 {
        return None;
    }
    DateTime::from_timestamp((i64::from(days) - 1) * 86_400 + seconds, 0)
}

impl ProductDescription {
    /// The product dependent values of Table V for this product code, in
    /// halfword order (see the [module documentation](crate::params)).
    /// Julian dates of 0 (unset) are left out; codes with no Table V entries
    /// give an empty list.
    pub fn parameters(&self) -> Vec<ProductParameter> {
        let hw = |n: u8| self.halfword(usize::from(n)).unwrap_or_default();
        let mut out = Vec::new();
        for (n, name, kind, units) in specs(self.product_code) {
            let raw = hw(n);
            let value = match kind {
                Kind::Int => ParameterValue::Int(i64::from(raw as i16)),
                Kind::UInt => ParameterValue::Int(i64::from(raw)),
                Kind::Scaled(factor) => ParameterValue::Float(f64::from(raw as i16) * factor),
                Kind::Real32 => ParameterValue::Float(f64::from(f32::from_bits(
                    (u32::from(raw) << 16) | u32::from(hw(n + 1)),
                ))),
                Kind::UInt32 => {
                    ParameterValue::Int(i64::from((u32::from(raw) << 16) | u32::from(hw(n + 1))))
                }
                Kind::Date => match julian(raw, 0) {
                    Some(date) => ParameterValue::Date(date.format("%Y-%m-%d").to_string()),
                    None => continue,
                },
                Kind::DateMinutes | Kind::DateHours => {
                    let unit = if matches!(kind, Kind::DateHours) {
                        3600
                    } else {
                        60
                    };
                    match julian(raw, i64::from(hw(n + 1) as i16) * unit) {
                        Some(time) => ParameterValue::Time(time),
                        None => continue,
                    }
                }
                Kind::HighByte => ParameterValue::Int(i64::from(raw >> 8)),
                Kind::LowByte => ParameterValue::Int(i64::from(raw & 0xFF)),
                Kind::DeltaTime => ParameterValue::Int(i64::from(raw >> 5)),
                Kind::SupplementalScan => ParameterValue::Text(match raw & 0x1F {
                    0 => "none",
                    1 => "mrle",
                    2 => "sails",
                    _ => "unknown",
                }),
                Kind::Chars => ParameterValue::Characters(
                    raw.to_be_bytes().iter().map(|&b| char::from(b)).collect(),
                ),
            };
            out.push(ProductParameter {
                halfword: n,
                name,
                units,
                value,
            });
        }
        out.sort_by_key(|p| p.halfword);
        out
    }
}
