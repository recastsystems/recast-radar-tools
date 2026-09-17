//! Data level to physical value mapping (ICD 2620001AD Figure 3-6 sheet 6 Note 1;
//! `docs/level3/reference.md` sections 5 and 8).
//!
//! Radial, raster and generic products store each range bin as an integer
//! data level. What a level means depends on the product code and on the
//! Product Description Block halfwords 31-53. [`DataLevels::from_description`]
//! reads those halfwords and returns a mapping; [`DataLevels::level`] then
//! turns any level into a [`Level`]: a physical value, a class of a
//! categorical product, or a flag such as "below threshold".
//!
//! | Encoding ([`LevelEncoding`]) | Products |
//! |---|---|
//! | [`Thresholds`](LevelEncoding::Thresholds) (16 threshold halfwords) | 16-31, 33, 35-38, 41, 43-46, 48, 50, 51, 55-57, 63-67, 78-80, 84-87, 89, 90, 95-98, 132, 133, 137, 144-147, 150, 151, 158, 160, 162, 164, 169, 171, 181, 183, 185, 187 |
//! | [`Linear`](LevelEncoding::Linear) (minimum and increment) | 32, 81, 93, 94, 99, 138, 153-155, 180, 182, 186, 193, 195 |
//! | [`ScaleOffset`](LevelEncoding::ScaleOffset) (`REAL*4` scale and offset) | 159, 161, 163, 167, 168, 170, 172-176, 189-192 |
//! | [`Vil`](LevelEncoding::Vil) | 134 |
//! | [`EchoTops`](LevelEncoding::EchoTops) | 135 |
//! | [`Classes`](LevelEncoding::Classes) | 34, 113, 165, 177, 197 |
//! | [`Edr`](LevelEncoding::Edr) | 156, 157 |
//!
//! Graphic, alphanumeric and generic products without data levels, and TDWR
//! product 184 (its 256-level encoding is not given in 2620063E), have no mapping.

use crate::header::ProductDescription;

/// What one data level of a product means.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Level {
    /// A physical value in the product's [`units`](DataLevels::units).
    Value(f64),
    /// An echo top (product 135) flagged as "topped": the true top is at or
    /// above this value.
    Topped(f64),
    /// A class of a categorical product.
    Class(Class),
    /// A special condition with no physical value.
    Flag(LevelFlag),
    /// A level the product's encoding does not define.
    Undefined,
}

impl Level {
    /// The physical value of [`Level::Value`] and [`Level::Topped`]; `None` otherwise.
    pub fn value(&self) -> Option<f64> {
        match *self {
            Self::Value(v) | Self::Topped(v) => Some(v),
            _ => None,
        }
    }
}

/// A special data level with no physical value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LevelFlag {
    /// Below threshold (threshold code TH; digital level 0).
    BelowThreshold,
    /// Missing data (products 32, 94, 153, 180, 186, 193, 195 level 1).
    Missing,
    /// Range folded (threshold code RF; digital velocity level 1).
    RangeFolded,
    /// No data (threshold code ND; products 170, 172-175 level 0).
    NoData,
    /// Blank (threshold code BLANK).
    Blank,
    /// Flagged data (product 134 level 1), or a leading/trailing flag value the
    /// ICD does not name.
    Flagged,
    /// Bad data (product 135 level 1).
    Bad,
    /// Reserved (product 134 level 255).
    Reserved,
    /// No accumulation (product 81 level 0).
    NoAccumulation,
    /// Outside the coverage area (product 81 level 255).
    OutsideCoverage,
    /// Edited/removed (product 193 level 2).
    EditRemove,
    /// Chaff detected (product 193 level 254).
    Chaff,
}

/// One class of a categorical product.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Class {
    /// The code identifying the class: the data level for products 34, 113,
    /// 165, 177 and 197 (e.g. 10 for BI), the threshold code (4-16) for
    /// threshold-coded products.
    pub code: u16,
    /// Displayed code or short name, e.g. `BI`.
    pub label: &'static str,
    /// Meaning, e.g. `Biological`.
    pub description: &'static str,
}

/// One decoded data level threshold halfword (Figure 3-6 sheet 6 Note 1): a
/// numeric value or a code, with display qualifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Threshold {
    /// The raw halfword.
    pub raw: u16,
}

/// Names of threshold codes 0-16 (Figure 3-6 sheet 6 Note 1).
const THRESHOLD_CODES: [(&str, &str); 17] = [
    ("BLANK", "Blank"),
    ("TH", "Below threshold"),
    ("ND", "No data"),
    ("RF", "Range folded"),
    ("BI", "Biological"),
    ("GC", "AP/Ground clutter"),
    ("IC", "Ice crystals"),
    ("GR", "Graupel"),
    ("WS", "Wet snow"),
    ("DS", "Dry snow"),
    ("RA", "Light and moderate rain"),
    ("HR", "Heavy rain"),
    ("BD", "Big drops"),
    ("HA", "Hail and rain mixed"),
    ("UK", "Unknown"),
    ("LH", "Large hail"),
    ("GH", "Giant hail"),
];

impl Threshold {
    /// The code in the low byte when the most significant bit is set.
    pub fn code(&self) -> Option<u8> {
        (self.raw & 0x8000 != 0).then_some((self.raw & 0xFF) as u8)
    }

    /// The numeric value when the most significant bit is clear: the low
    /// byte, divided by 100, 20 or 10 when bit 14, 13 or 12 is set, negated
    /// when bit 8 ("-") is set.
    pub fn value(&self) -> Option<f64> {
        if self.raw & 0x8000 != 0 {
            return None;
        }
        let mut value = f64::from(self.raw & 0xFF);
        if self.raw & 0x4000 != 0 {
            value /= 100.0;
        } else if self.raw & 0x2000 != 0 {
            value /= 20.0;
        } else if self.raw & 0x1000 != 0 {
            value /= 10.0;
        }
        if self.raw & 0x0100 != 0 {
            value = -value;
        }
        Some(value)
    }

    /// Bit 11: display as "greater than".
    pub fn greater_than(&self) -> bool {
        self.raw & 0x0800 != 0
    }

    /// Bit 10: display as "less than".
    pub fn less_than(&self) -> bool {
        self.raw & 0x0400 != 0
    }

    /// Bit 9: display with a "+" sign.
    pub fn plus(&self) -> bool {
        self.raw & 0x0200 != 0
    }

    /// Display label, e.g. `ND`, `-28`, `>0.00`, `<TH`.
    pub fn label(&self) -> String {
        let mut label = String::new();
        if self.less_than() {
            label.push('<');
        } else if self.greater_than() {
            label.push('>');
        }
        match self.code() {
            Some(code) => label.push_str(
                THRESHOLD_CODES
                    .get(usize::from(code))
                    .map_or("?", |(name, _)| name),
            ),
            None => {
                if self.raw & 0x0100 != 0 {
                    label.push('-');
                } else if self.plus() {
                    label.push('+');
                }
                let magnitude = self.value().unwrap_or_default().abs();
                let text = if self.raw & 0x6000 != 0 {
                    format!("{magnitude:.2}")
                } else if self.raw & 0x1000 != 0 {
                    format!("{magnitude:.1}")
                } else {
                    format!("{magnitude}")
                };
                label.push_str(&text);
            }
        }
        label
    }

    fn level(&self) -> Level {
        match self.code() {
            Some(0) => Level::Flag(LevelFlag::Blank),
            Some(1) => Level::Flag(LevelFlag::BelowThreshold),
            Some(2) => Level::Flag(LevelFlag::NoData),
            Some(3) => Level::Flag(LevelFlag::RangeFolded),
            Some(code) => THRESHOLD_CODES.get(usize::from(code)).map_or(
                Level::Undefined,
                |&(label, description)| {
                    Level::Class(Class {
                        code: u16::from(code),
                        label,
                        description,
                    })
                },
            ),
            None => self.value().map_or(Level::Undefined, Level::Value),
        }
    }
}

/// Parameters of an equally spaced encoding: `value = first_value + (N -
/// first_level) * increment` for `first_level <= N < first_level + count`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Linear {
    /// Lowest data level carrying a value.
    pub first_level: u16,
    /// Number of value levels (from halfword 33, limited to the levels that fit).
    pub count: u16,
    /// Value of `first_level`.
    pub first_value: f64,
    /// Value increment per level.
    pub increment: f64,
    /// Levels with a special meaning; they take precedence over values.
    pub flags: &'static [(u16, LevelFlag)],
}

/// How a product encodes its data levels, with the parameters read from its
/// Product Description Block.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum LevelEncoding {
    /// Halfwords 31-46 describe levels 0-15.
    Thresholds([Threshold; 16]),
    /// Equally spaced values from a minimum and an increment (halfwords 31-33).
    Linear(Linear),
    /// `value = (N - offset) / scale` with `REAL*4` scale (halfwords 31-32) and
    /// offset (33-34); halfword 36 is the maximum level, 37 and 38 the number
    /// of leading and trailing flag levels.
    ScaleOffset {
        /// Scale.
        scale: f32,
        /// Offset.
        offset: f32,
        /// Maximum data level.
        max_level: u16,
        /// Number of flag levels at the bottom (0, 1, ...).
        leading_flags: u16,
        /// Number of flag levels at the top (..., max).
        trailing_flags: u16,
        /// Meaning of individual leading flag levels, when the ICD names them.
        flags: &'static [(u16, LevelFlag)],
    },
    /// High resolution VIL (134): linear below `log_start`, logarithmic from it.
    /// Coefficients are 16-bit floats (halfwords 31, 32, 34, 35).
    Vil {
        /// Linear scale (halfword 31).
        linear_scale: f64,
        /// Linear offset (halfword 32).
        linear_offset: f64,
        /// First level using the log relationship (halfword 33).
        log_start: u16,
        /// Log scale (halfword 34).
        log_scale: f64,
        /// Log offset (halfword 35).
        log_offset: f64,
    },
    /// High resolution enhanced echo tops (135): `value = (N & data_mask) /
    /// scale - offset` kft, topped when `N & topped_mask != 0`.
    EchoTops {
        /// Data mask (halfword 31).
        data_mask: u16,
        /// Scale (halfword 32).
        scale: u16,
        /// Offset (halfword 33).
        offset: u16,
        /// Topped mask (halfword 34).
        topped_mask: u16,
    },
    /// Enumerated classes; levels not listed are undefined.
    Classes(&'static [(u16, Level)]),
    /// Eddy dissipation rate (156, 157; described from MetPy 1.7.1):
    /// `value = scale * N + offset` for `leading_flags <= N < levels`.
    Edr {
        /// Scale (signed halfword 31 / 1000).
        scale: f64,
        /// Offset (signed halfword 32 / 1000).
        offset: f64,
        /// Number of data levels (halfword 33).
        levels: u16,
        /// Number of leading flag levels (halfword 34).
        leading_flags: u16,
    },
}

/// The data level mapping of one product.
#[derive(Debug, Clone, PartialEq)]
pub struct DataLevels {
    product_code: i16,
    units: Option<&'static str>,
    encoding: LevelEncoding,
}

const BT_RF: &[(u16, LevelFlag)] = &[(0, LevelFlag::BelowThreshold), (1, LevelFlag::RangeFolded)];
const BT_MISSING: &[(u16, LevelFlag)] = &[(0, LevelFlag::BelowThreshold), (1, LevelFlag::Missing)];
const DQA_EDITED: &[(u16, LevelFlag)] = &[
    (0, LevelFlag::BelowThreshold),
    (1, LevelFlag::Missing),
    (2, LevelFlag::EditRemove),
    (254, LevelFlag::Chaff),
];
const DPA_FLAGS: &[(u16, LevelFlag)] = &[
    (0, LevelFlag::NoAccumulation),
    (255, LevelFlag::OutsideCoverage),
];
const NO_DATA: &[(u16, LevelFlag)] = &[(0, LevelFlag::NoData)];

const fn class(code: u16, label: &'static str, description: &'static str) -> (u16, Level) {
    (
        code,
        Level::Class(Class {
            code,
            label,
            description,
        }),
    )
}

const fn flag(code: u16, flag: LevelFlag) -> (u16, Level) {
    (code, Level::Flag(flag))
}

/// Products 165 (version 0) and 177: hydrometeor classes (Note 1 table).
const HYDROMETEOR: &[(u16, Level)] = &[
    flag(0, LevelFlag::BelowThreshold),
    class(10, "BI", "Biological"),
    class(20, "GC", "Anomalous propagation/ground clutter"),
    class(30, "IC", "Ice crystals"),
    class(40, "DS", "Dry snow"),
    class(50, "WS", "Wet snow"),
    class(60, "RA", "Light and/or moderate rain"),
    class(70, "HR", "Heavy rain"),
    class(80, "BD", "Big drops (rain)"),
    class(90, "GR", "Graupel"),
    class(100, "HA", "Hail, possibly with rain"),
    class(140, "UK", "Unknown classification"),
    flag(150, LevelFlag::RangeFolded),
];

/// Product 165 version 1 and later: HA sub-classified into LH and GH.
const HYDROMETEOR_V1: &[(u16, Level)] = &[
    flag(0, LevelFlag::BelowThreshold),
    class(10, "BI", "Biological"),
    class(20, "GC", "Anomalous propagation/ground clutter"),
    class(30, "IC", "Ice crystals"),
    class(40, "DS", "Dry snow"),
    class(50, "WS", "Wet snow"),
    class(60, "RA", "Light and/or moderate rain"),
    class(70, "HR", "Heavy rain"),
    class(80, "BD", "Big drops (rain)"),
    class(90, "GR", "Graupel"),
    class(100, "HA", "Hail, possibly with rain"),
    class(110, "LH", "Large hail"),
    class(120, "GH", "Giant hail"),
    class(140, "UK", "Unknown classification"),
    flag(150, LevelFlag::RangeFolded),
];

/// Product 197: rain rate classes (Note 1 table).
const RAIN_RATE: &[(u16, Level)] = &[
    class(0, "NP", "No precipitation (biota or no echo)"),
    class(10, "UF", "Unfilled"),
    class(20, "CZ", "Convective R(Z,ZDR)"),
    class(30, "TZ", "Tropical R(Z,ZDR)"),
    class(40, "SA", "Specific attenuation"),
    class(50, "KL", "R(KDP) 27 coefficient"),
    class(60, "KH", "R(KDP) 44 coefficient"),
    class(70, "Z1", "R(Z)"),
    class(80, "Z6", "R(Z) * 0.6"),
    class(90, "Z8", "R(Z) * 0.8"),
    class(100, "SI", "R(Z) * multiplier"),
];

/// Product 34 version 0: clutter filter control, 8 levels (2620003AE 34.2.2).
const CLUTTER_FILTER_V0: &[(u16, Level)] = &[
    class(0, "Filter off", "Disable filter"),
    class(1, "No clutter", "Bypass map in control"),
    class(2, "Low", "Bypass map in control"),
    class(3, "Medium", "Bypass map in control"),
    class(4, "High", "Bypass map in control"),
    class(5, "Low", "Force filter"),
    class(6, "Medium", "Force filter"),
    class(7, "High", "Force filter"),
];

/// Product 34 version 1: clutter filter control, 4 levels (2620003AE 34.2.2).
const CLUTTER_FILTER_V1: &[(u16, Level)] = &[
    class(0, "Filter off", "Disable filter"),
    class(1, "No clutter", "Bypass map in control"),
    class(4, "Clutter", "Bypass map in control"),
    class(7, "Force filter", "Force filter"),
];

/// Product 113: power removed control, 13 levels (2620003AE 34.5.2.2).
const POWER_REMOVED: &[(u16, Level)] = &[
    class(0, "No filter applied", "No filter applied"),
    class(1, "No clutter", "Bypass map in control"),
    class(2, "Low", "Bypass map in control"),
    class(3, "Medium", "Bypass map in control"),
    class(4, "High", "Bypass map in control"),
    class(5, "No filter", "Disable filter"),
    class(6, "No clutter", "Force filter"),
    class(7, "Low", "Force filter"),
    class(8, "Medium", "Force filter"),
    class(9, "High", "Force filter"),
    class(10, "Point clutter", "Point clutter removed"),
    class(11, "Filter error", "Filtering in \"filter disabled\""),
    class(
        12,
        "Dual-pol filtered",
        "Filtering only in dual-pol moments",
    ),
];

/// 16-bit float of product 134 (Note 1): sign bit, 5 exponent bits, 10 fraction bits.
fn float16(raw: u16) -> f64 {
    let fraction = f64::from(raw & 0x03FF) / 1024.0;
    let exponent = i32::from((raw >> 10) & 0x1F);
    let magnitude = if exponent == 0 {
        2.0 * fraction
    } else {
        2f64.powi(exponent - 16) * (1.0 + fraction)
    };
    if raw & 0x8000 != 0 {
        -magnitude
    } else {
        magnitude
    }
}

impl DataLevels {
    /// The data level mapping for the product described by `desc`, or `None`
    /// when the product has no data levels or their encoding is unknown.
    pub fn from_description(desc: &ProductDescription) -> Option<Self> {
        let hw = |n: usize| desc.halfword(n).unwrap_or_default();
        let signed = |n: usize| f64::from(hw(n) as i16);
        let real = |n: usize| f32::from_bits((u32::from(hw(n)) << 16) | u32::from(hw(n + 1)));
        let linear = |first_level: u16,
                      max_level: u16,
                      first_value: f64,
                      increment: f64,
                      flags: &'static [(u16, LevelFlag)]| {
            LevelEncoding::Linear(Linear {
                first_level,
                count: hw(33).min(max_level - first_level + 1),
                first_value,
                increment,
                flags,
            })
        };
        let scale_offset = |flags| LevelEncoding::ScaleOffset {
            scale: real(31),
            offset: real(33),
            max_level: hw(36),
            leading_flags: hw(37),
            trailing_flags: hw(38),
            flags,
        };
        let code = desc.product_code;
        let (encoding, units) = match code {
            16..=31
            | 33
            | 35..=38
            | 41
            | 43..=46
            | 48
            | 50
            | 51
            | 55..=57
            | 63..=67
            | 78..=80
            | 84..=87
            | 89
            | 90
            | 95..=98
            | 132
            | 133
            | 137
            | 144..=147
            | 150
            | 151
            | 158
            | 160
            | 162
            | 164
            | 169
            | 171
            | 181
            | 183
            | 185
            | 187 => {
                let mut thresholds = [Threshold { raw: 0 }; 16];
                for (i, t) in thresholds.iter_mut().enumerate() {
                    t.raw = hw(31 + i);
                }
                (LevelEncoding::Thresholds(thresholds), None)
            }
            32 | 94 | 153 | 180 | 186 | 195 => (
                linear(2, 255, signed(31) / 10.0, signed(32) / 10.0, BT_MISSING),
                Some("dBZ"),
            ),
            193 => (
                linear(2, 255, signed(31) / 10.0, signed(32) / 10.0, DQA_EDITED),
                Some("dBZ"),
            ),
            93 | 99 | 154 | 182 => (
                linear(2, 255, signed(31) / 10.0, signed(32) / 10.0, BT_RF),
                Some("m s-1"),
            ),
            155 => (
                linear(129, 255, signed(31) / 10.0, signed(32) / 10.0, BT_RF),
                Some("m s-1"),
            ),
            81 => (
                linear(1, 254, signed(31) / 10.0, signed(32) / 1000.0, DPA_FLAGS),
                Some("dBA"),
            ),
            // Level 0 is no accumulation (the minimum, 0) and level 1 the first
            // non-zero accumulation: value = (hw31 + N * hw32) / 100 inches.
            138 => (
                LevelEncoding::Linear(Linear {
                    first_level: 0,
                    count: hw(33).min(256),
                    first_value: signed(31) / 100.0,
                    increment: signed(32) / 100.0,
                    flags: &[],
                }),
                Some("in"),
            ),
            134 => (
                LevelEncoding::Vil {
                    linear_scale: float16(hw(31)),
                    linear_offset: float16(hw(32)),
                    log_start: hw(33),
                    log_scale: float16(hw(34)),
                    log_offset: float16(hw(35)),
                },
                Some("kg m-2"),
            ),
            135 => (
                LevelEncoding::EchoTops {
                    data_mask: hw(31),
                    scale: hw(32),
                    offset: hw(33),
                    topped_mask: hw(34),
                },
                Some("kft"),
            ),
            159 | 191 => (scale_offset(BT_RF), Some("dB")),
            161 | 167 | 190 => (scale_offset(BT_RF), Some("1")),
            163 | 192 => (scale_offset(BT_RF), Some("deg km-1")),
            168 => (scale_offset(BT_RF), Some("deg")),
            189 => (scale_offset(BT_RF), Some("dBZ")),
            170 | 172..=175 => (scale_offset(NO_DATA), Some("0.01 in")),
            176 => (scale_offset(&[]), Some("in h-1")),
            165 if desc.version >= 1 => (LevelEncoding::Classes(HYDROMETEOR_V1), None),
            165 | 177 => (LevelEncoding::Classes(HYDROMETEOR), None),
            197 => (LevelEncoding::Classes(RAIN_RATE), None),
            34 if desc.version >= 1 => (LevelEncoding::Classes(CLUTTER_FILTER_V1), None),
            34 => (LevelEncoding::Classes(CLUTTER_FILTER_V0), None),
            113 => (LevelEncoding::Classes(POWER_REMOVED), None),
            156 | 157 => (
                LevelEncoding::Edr {
                    scale: signed(31) / 1000.0,
                    offset: signed(32) / 1000.0,
                    levels: hw(33),
                    leading_flags: hw(34),
                },
                None,
            ),
            _ => return None,
        };
        Some(Self {
            product_code: code,
            units,
            encoding,
        })
    }

    /// The product code this mapping was built for.
    pub fn product_code(&self) -> i16 {
        self.product_code
    }

    /// Units of [`Level::Value`] (UDUNITS spelling, e.g. `dBZ`, `m s-1`,
    /// `0.01 in`), or `None` when not stated by the ICD in machine-usable form
    /// (threshold-coded products, categorical products, EDR).
    pub fn units(&self) -> Option<&'static str> {
        self.units
    }

    /// The encoding and its parameters.
    pub fn encoding(&self) -> &LevelEncoding {
        &self.encoding
    }

    /// What data level `n` means.
    pub fn level(&self, n: u16) -> Level {
        let finite = |v: f64| {
            if v.is_finite() {
                Level::Value(v)
            } else {
                Level::Undefined
            }
        };
        match &self.encoding {
            LevelEncoding::Thresholds(thresholds) => thresholds
                .get(usize::from(n))
                .map_or(Level::Undefined, Threshold::level),
            LevelEncoding::Linear(l) => {
                if let Some(&(_, flag)) = l.flags.iter().find(|(level, _)| *level == n) {
                    Level::Flag(flag)
                } else if n >= l.first_level && n - l.first_level < l.count {
                    finite(l.first_value + f64::from(n - l.first_level) * l.increment)
                } else {
                    Level::Undefined
                }
            }
            LevelEncoding::ScaleOffset {
                scale,
                offset,
                max_level,
                leading_flags,
                trailing_flags,
                flags,
            } => {
                if n > *max_level {
                    Level::Undefined
                } else if n < *leading_flags || max_level - n < *trailing_flags {
                    let named = flags.iter().find(|(level, _)| *level == n);
                    Level::Flag(named.map_or(LevelFlag::Flagged, |&(_, flag)| flag))
                } else {
                    finite((f64::from(n) - f64::from(*offset)) / f64::from(*scale))
                }
            }
            LevelEncoding::Vil {
                linear_scale,
                linear_offset,
                log_start,
                log_scale,
                log_offset,
            } => match n {
                0 => Level::Flag(LevelFlag::BelowThreshold),
                1 => Level::Flag(LevelFlag::Flagged),
                255 => Level::Flag(LevelFlag::Reserved),
                256.. => Level::Undefined,
                n if n < *log_start => finite((f64::from(n) - linear_offset) / linear_scale),
                n => finite(((f64::from(n) - log_offset) / log_scale).exp()),
            },
            LevelEncoding::EchoTops {
                data_mask,
                scale,
                offset,
                topped_mask,
            } => match n {
                0 => Level::Flag(LevelFlag::BelowThreshold),
                1 => Level::Flag(LevelFlag::Bad),
                256.. => Level::Undefined,
                n => {
                    let value = f64::from(n & data_mask) / f64::from(*scale) - f64::from(*offset);
                    match finite(value) {
                        Level::Value(v) if n & topped_mask != 0 => Level::Topped(v),
                        other => other,
                    }
                }
            },
            LevelEncoding::Classes(table) => table
                .iter()
                .find(|(level, _)| *level == n)
                .map_or(Level::Undefined, |&(_, level)| level),
            LevelEncoding::Edr {
                scale,
                offset,
                levels,
                leading_flags,
            } => {
                if n >= *levels {
                    Level::Undefined
                } else if n < *leading_flags {
                    Level::Flag(LevelFlag::Flagged)
                } else {
                    finite(scale * f64::from(n) + offset)
                }
            }
        }
    }

    /// The physical value of data level `n`; `None` for classes, flags and
    /// undefined levels.
    pub fn value(&self, n: u16) -> Option<f64> {
        self.level(n).value()
    }

    /// A lookup table of physical values for levels `0..len` (at most 65536):
    /// `table[n]` is the value of level `n` as `f32`, NaN where the level has
    /// no physical value.
    pub fn lookup_table(&self, len: usize) -> Vec<f32> {
        (0..len.min(1 << 16))
            .map(|n| {
                u16::try_from(n)
                    .ok()
                    .and_then(|n| self.value(n))
                    .map_or(f32::NAN, |v| v as f32)
            })
            .collect()
    }
}
