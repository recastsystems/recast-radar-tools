use std::collections::{BTreeMap, HashSet};
use std::mem::size_of;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    AdditiveScattering, KernelModel, OutputError, ScienceError, ScienceMetadata, Sha256Digest,
    TMatrixImplementation,
};

pub const LUT_MAGIC: [u8; 8] = *b"BRSLUT01";
pub const LUT_SCHEMA_VERSION: u16 = 1;

const PREFIX_BYTES: usize = 8 + 2 + 4;
const MAX_HEADER_BYTES: usize = 16 * 1024 * 1024;
const MAX_AXES: usize = 16;

/// Fixed number of axis slots carried by [`PreparedInterpolationPlan`].
///
/// Only the prefix reported by
/// [`PreparedInterpolationPlan::active_axis_count`] is active. Remaining
/// offsets and fractions are deterministically zero-filled so plans can be
/// staged as fixed-size host records for a batched accelerator backend.
pub const PREPARED_INTERPOLATION_AXIS_SLOTS: usize = MAX_AXES;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AxisKind {
    EquivolumeDiameter,
    Temperature,
    BulkDensity,
    CondensedVolumeFraction,
    LiquidMassFraction,
    MinorToMajorAxisRatio,
    Frequency,
    RadarElevation,
    CantingAngle,
    RimeMassFraction,
    RimeDensity,
    TimeOffset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Meter,
    Kelvin,
    KilogramPerCubicMeter,
    UnitlessFraction,
    Hertz,
    Degree,
    Second,
    LinearReflectivityMillimeter6PerMeter3,
    LinearCovarianceMillimeter6PerMeter3,
    DegreePerKilometer,
    DecibelPerKilometer,
    ReflectivityWeightedMeterPerSecond,
    ReflectivityWeightedMeter2PerSecond2,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Axis {
    kind: AxisKind,
    unit: Unit,
    coordinates: Vec<f64>,
}

impl Axis {
    pub fn new(kind: AxisKind, unit: Unit, coordinates: Vec<f64>) -> Result<Self, LutError> {
        let axis = Self {
            kind,
            unit,
            coordinates,
        };
        axis.validate(0)?;
        Ok(axis)
    }

    #[must_use]
    pub const fn kind(&self) -> AxisKind {
        self.kind
    }

    #[must_use]
    pub const fn unit(&self) -> Unit {
        self.unit
    }

    #[must_use]
    pub fn coordinates(&self) -> &[f64] {
        &self.coordinates
    }

    fn validate(&self, index: usize) -> Result<(), LutError> {
        let expected = axis_unit(self.kind);
        if self.unit != expected {
            return Err(LutError::AxisUnit {
                index,
                kind: self.kind,
                expected,
                actual: self.unit,
            });
        }
        if self.coordinates.is_empty() {
            return Err(LutError::EmptyAxis { index });
        }
        for (coordinate_index, value) in self.coordinates.iter().copied().enumerate() {
            if !value.is_finite() {
                return Err(LutError::NonFiniteAxisCoordinate {
                    axis: index,
                    coordinate: coordinate_index,
                    value,
                });
            }
            if coordinate_index > 0 && self.coordinates[coordinate_index - 1] >= value {
                return Err(LutError::NonIncreasingAxis {
                    axis: index,
                    lower: self.coordinates[coordinate_index - 1],
                    upper: value,
                });
            }
        }
        Ok(())
    }

    fn locate(&self, value: f64) -> Result<Bracket, InterpolationError> {
        let first = self.coordinates[0];
        let last = self.coordinates[self.coordinates.len() - 1];
        if value < first || value > last {
            return Err(InterpolationError::OutsideAxis {
                kind: self.kind,
                value,
                minimum: first,
                maximum: last,
            });
        }
        if self.coordinates.len() == 1 || value == first {
            return Ok(Bracket {
                lower: 0,
                upper: 0,
                fraction: 0.0,
            });
        }
        if value == last {
            let index = self.coordinates.len() - 1;
            return Ok(Bracket {
                lower: index,
                upper: index,
                fraction: 0.0,
            });
        }

        let upper = self
            .coordinates
            .partition_point(|candidate| *candidate < value);
        if self.coordinates[upper] == value {
            return Ok(Bracket {
                lower: upper,
                upper,
                fraction: 0.0,
            });
        }
        let lower = upper - 1;
        let fraction =
            (value - self.coordinates[lower]) / (self.coordinates[upper] - self.coordinates[lower]);
        Ok(Bracket {
            lower,
            upper,
            fraction,
        })
    }
}

fn axis_unit(kind: AxisKind) -> Unit {
    match kind {
        AxisKind::EquivolumeDiameter => Unit::Meter,
        AxisKind::Temperature => Unit::Kelvin,
        AxisKind::BulkDensity | AxisKind::RimeDensity => Unit::KilogramPerCubicMeter,
        AxisKind::LiquidMassFraction
        | AxisKind::CondensedVolumeFraction
        | AxisKind::MinorToMajorAxisRatio
        | AxisKind::RimeMassFraction => Unit::UnitlessFraction,
        AxisKind::Frequency => Unit::Hertz,
        AxisKind::RadarElevation | AxisKind::CantingAngle => Unit::Degree,
        AxisKind::TimeOffset => Unit::Second,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    Zh,
    Zv,
    HhVvCovarianceReal,
    HhVvCovarianceImaginary,
    Kdp,
    Ah,
    Av,
    FallSpeedFirstMoment,
    FallSpeedSecondMoment,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputDescriptor {
    kind: OutputKind,
    unit: Unit,
}

impl OutputDescriptor {
    #[must_use]
    pub const fn kind(self) -> OutputKind {
        self.kind
    }

    #[must_use]
    pub const fn unit(self) -> Unit {
        self.unit
    }
}

fn canonical_outputs() -> Vec<OutputDescriptor> {
    use OutputKind::{
        Ah, Av, FallSpeedFirstMoment, FallSpeedSecondMoment, HhVvCovarianceImaginary,
        HhVvCovarianceReal, Kdp, Zh, Zv,
    };
    vec![
        OutputDescriptor {
            kind: Zh,
            unit: Unit::LinearReflectivityMillimeter6PerMeter3,
        },
        OutputDescriptor {
            kind: Zv,
            unit: Unit::LinearReflectivityMillimeter6PerMeter3,
        },
        OutputDescriptor {
            kind: HhVvCovarianceReal,
            unit: Unit::LinearCovarianceMillimeter6PerMeter3,
        },
        OutputDescriptor {
            kind: HhVvCovarianceImaginary,
            unit: Unit::LinearCovarianceMillimeter6PerMeter3,
        },
        OutputDescriptor {
            kind: Kdp,
            unit: Unit::DegreePerKilometer,
        },
        OutputDescriptor {
            kind: Ah,
            unit: Unit::DecibelPerKilometer,
        },
        OutputDescriptor {
            kind: Av,
            unit: Unit::DecibelPerKilometer,
        },
        OutputDescriptor {
            kind: FallSpeedFirstMoment,
            unit: Unit::ReflectivityWeightedMeterPerSecond,
        },
        OutputDescriptor {
            kind: FallSpeedSecondMoment,
            unit: Unit::ReflectivityWeightedMeter2PerSecond2,
        },
    ]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadEncoding {
    /// f64 little endian, one nine-component output per grid point, with the
    /// last declared axis varying fastest.
    F64LePointMajorLastAxisFastest,
}

/// Auditable generator identity. Package names are canonical lowercase keys.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratorMetadata {
    name: String,
    version: String,
    executable: String,
    source_revision: String,
    python_version: Option<String>,
    package_versions: BTreeMap<String, String>,
}

impl GeneratorMetadata {
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        executable: impl Into<String>,
        source_revision: impl Into<String>,
        python_version: Option<String>,
        package_versions: BTreeMap<String, String>,
    ) -> Result<Self, LutError> {
        let metadata = Self {
            name: name.into(),
            version: version.into(),
            executable: executable.into(),
            source_revision: source_revision.into(),
            python_version,
            package_versions,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    #[must_use]
    pub fn executable(&self) -> &str {
        &self.executable
    }

    #[must_use]
    pub fn source_revision(&self) -> &str {
        &self.source_revision
    }

    #[must_use]
    pub fn python_version(&self) -> Option<&str> {
        self.python_version.as_deref()
    }

    #[must_use]
    pub const fn package_versions(&self) -> &BTreeMap<String, String> {
        &self.package_versions
    }

    fn validate(&self) -> Result<(), LutError> {
        for (field, value) in [
            ("generator name", self.name.as_str()),
            ("generator version", self.version.as_str()),
            ("generator executable", self.executable.as_str()),
            ("generator source revision", self.source_revision.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(LutError::EmptyMetadata { field });
            }
        }
        if self
            .python_version
            .as_deref()
            .is_some_and(|version| version.trim().is_empty())
        {
            return Err(LutError::EmptyMetadata {
                field: "generator Python version",
            });
        }
        for (package, version) in &self.package_versions {
            if package.trim() != package
                || package.is_empty()
                || package.bytes().any(|byte| byte.is_ascii_uppercase())
            {
                return Err(LutError::NonCanonicalPackageName {
                    package: package.clone(),
                });
            }
            if version.trim().is_empty() {
                return Err(LutError::EmptyPackageVersion {
                    package: package.clone(),
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LutHeader {
    magic: String,
    schema_version: u16,
    axes: Vec<Axis>,
    outputs: Vec<OutputDescriptor>,
    generator: GeneratorMetadata,
    generator_config_utf8: String,
    config_sha256: Sha256Digest,
    science: ScienceMetadata,
    payload_encoding: PayloadEncoding,
    grid_point_count: u64,
    payload_byte_length: u64,
    payload_sha256: Sha256Digest,
}

impl LutHeader {
    #[must_use]
    pub fn magic(&self) -> &str {
        &self.magic
    }

    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    #[must_use]
    pub fn axes(&self) -> &[Axis] {
        &self.axes
    }

    #[must_use]
    pub fn outputs(&self) -> &[OutputDescriptor] {
        &self.outputs
    }

    #[must_use]
    pub const fn generator(&self) -> &GeneratorMetadata {
        &self.generator
    }

    #[must_use]
    pub fn generator_config_utf8(&self) -> &str {
        &self.generator_config_utf8
    }

    #[must_use]
    pub const fn config_sha256(&self) -> Sha256Digest {
        self.config_sha256
    }

    #[must_use]
    pub const fn science(&self) -> &ScienceMetadata {
        &self.science
    }

    #[must_use]
    pub const fn payload_encoding(&self) -> PayloadEncoding {
        self.payload_encoding
    }

    #[must_use]
    pub const fn grid_point_count(&self) -> u64 {
        self.grid_point_count
    }

    #[must_use]
    pub const fn payload_byte_length(&self) -> u64 {
        self.payload_byte_length
    }

    #[must_use]
    pub const fn payload_sha256(&self) -> Sha256Digest {
        self.payload_sha256
    }

    fn validate(&self) -> Result<usize, LutError> {
        if self.magic.as_bytes() != &LUT_MAGIC[..] {
            return Err(LutError::HeaderMagic {
                actual: self.magic.clone(),
            });
        }
        if self.schema_version != LUT_SCHEMA_VERSION {
            return Err(LutError::UnsupportedSchema {
                actual: self.schema_version,
            });
        }
        if self.axes.is_empty() || self.axes.len() > MAX_AXES {
            return Err(LutError::AxisCount {
                actual: self.axes.len(),
                maximum: MAX_AXES,
            });
        }
        let mut kinds = HashSet::new();
        for (index, axis) in self.axes.iter().enumerate() {
            axis.validate(index)?;
            if !kinds.insert(axis.kind) {
                return Err(LutError::DuplicateAxis { kind: axis.kind });
            }
        }
        if self.outputs != canonical_outputs() {
            return Err(LutError::NonCanonicalOutputs);
        }
        self.generator.validate()?;
        self.science.validate_additive_lut_compatibility()?;
        validate_generator_science(&self.generator, &self.science)?;
        let config_value: serde_json::Value = serde_json::from_str(&self.generator_config_utf8)
            .map_err(LutError::GeneratorConfigJson)?;
        if !config_value.is_object() {
            return Err(LutError::GeneratorConfigNotObject);
        }
        let actual_config_digest = Sha256Digest::compute(self.generator_config_utf8.as_bytes());
        if self.config_sha256 != actual_config_digest {
            return Err(LutError::ConfigDigestMismatch {
                expected: self.config_sha256,
                actual: actual_config_digest,
            });
        }

        let points = grid_point_count(&self.axes)?;
        if self.grid_point_count != points as u64 {
            return Err(LutError::GridPointCount {
                header: self.grid_point_count,
                axes: points as u64,
            });
        }
        let expected_payload = points
            .checked_mul(AdditiveScattering::COMPONENT_COUNT)
            .and_then(|values| values.checked_mul(size_of::<f64>()))
            .ok_or(LutError::DimensionOverflow)?;
        if self.payload_byte_length != expected_payload as u64 {
            return Err(LutError::HeaderPayloadLength {
                header: self.payload_byte_length,
                expected: expected_payload as u64,
            });
        }
        Ok(points)
    }
}

fn validate_generator_science(
    generator: &GeneratorMetadata,
    science: &ScienceMetadata,
) -> Result<(), LutError> {
    if matches!(
        science.kernel(),
        KernelModel::TMatrix {
            implementation: TMatrixImplementation::PyTMatrix033
        }
    ) {
        match generator.package_versions.get("pytmatrix") {
            Some(version) if version == "0.3.3" => {}
            actual => {
                return Err(LutError::PyTMatrixVersion {
                    actual: actual.cloned(),
                });
            }
        }
    }
    Ok(())
}

/// A typed query coordinate; LUT queries must supply these in header axis order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisCoordinate {
    kind: AxisKind,
    value: f64,
}

impl AxisCoordinate {
    pub fn new(kind: AxisKind, value: f64) -> Result<Self, InterpolationError> {
        if !value.is_finite() {
            return Err(InterpolationError::NonFiniteCoordinate { kind, value });
        }
        Ok(Self { kind, value })
    }

    #[must_use]
    pub const fn kind(self) -> AxisKind {
        self.kind
    }

    #[must_use]
    pub const fn value(self) -> f64 {
        self.value
    }
}

#[derive(Clone, Copy, Debug)]
struct Bracket {
    lower: usize,
    upper: usize,
    fraction: f64,
}

/// A validated, fixed-size multilinear-interpolation record prepared on the
/// CPU.
///
/// `base_point_index` is the all-lower-corner point in the LUT's
/// last-axis-fastest payload. The active prefixes of `upper_point_offsets` and
/// `upper_fractions` contain one entry per non-degenerate bracket, compacted in
/// the LUT header's exact axis order. Corner bit `n` selects the upper point of
/// active entry `n`. Inactive array tails are zero-filled.
///
/// The fixed-width integer fields, fixed arrays, and C field order make this a
/// stable host-side staging layout. Serde serialization remains the portable
/// representation; consumers must not infer native endianness from `repr(C)`.
/// A plan is tied to the [`OfflineLut`] whose axes prepared it.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedInterpolationPlan {
    base_point_index: u64,
    upper_point_offsets: [u64; PREPARED_INTERPOLATION_AXIS_SLOTS],
    upper_fractions: [f64; PREPARED_INTERPOLATION_AXIS_SLOTS],
    corner_count: u32,
    active_axis_count: u32,
}

impl PreparedInterpolationPlan {
    /// All-lower-corner index in the LUT's point-major payload.
    #[must_use]
    pub const fn base_point_index(&self) -> u64 {
        self.base_point_index
    }

    /// Number of active entries in the offset and fraction arrays.
    #[must_use]
    pub const fn active_axis_count(&self) -> u32 {
        self.active_axis_count
    }

    /// Fixed host array of upper-corner point offsets.
    ///
    /// Only `[..active_axis_count()]` is active; the tail is zero-filled.
    #[must_use]
    pub const fn upper_point_offsets(&self) -> &[u64; PREPARED_INTERPOLATION_AXIS_SLOTS] {
        &self.upper_point_offsets
    }

    /// Fixed host array of upper interpolation fractions.
    ///
    /// Entries correspond one-for-one with [`Self::upper_point_offsets`] in
    /// exact LUT axis order. Only `[..active_axis_count()]` is active.
    #[must_use]
    pub const fn upper_fractions(&self) -> &[f64; PREPARED_INTERPOLATION_AXIS_SLOTS] {
        &self.upper_fractions
    }

    /// Number of corners evaluated by this plan (`2^active_axis_count`).
    #[must_use]
    pub const fn corner_count(&self) -> u32 {
        self.corner_count
    }
}

/// Validated, immutable additive-scattering lookup table.
#[derive(Clone, Debug, PartialEq)]
pub struct OfflineLut {
    header: LutHeader,
    values: Vec<AdditiveScattering>,
}

impl OfflineLut {
    pub fn new(
        axes: Vec<Axis>,
        generator: GeneratorMetadata,
        generator_config_utf8: impl Into<String>,
        science: ScienceMetadata,
        values: Vec<AdditiveScattering>,
    ) -> Result<Self, LutError> {
        if axes.is_empty() || axes.len() > MAX_AXES {
            return Err(LutError::AxisCount {
                actual: axes.len(),
                maximum: MAX_AXES,
            });
        }
        let mut kinds = HashSet::new();
        for (index, axis) in axes.iter().enumerate() {
            axis.validate(index)?;
            if !kinds.insert(axis.kind) {
                return Err(LutError::DuplicateAxis { kind: axis.kind });
            }
        }
        let points = grid_point_count(&axes)?;
        if points != values.len() {
            return Err(LutError::ValueCount {
                expected: points,
                actual: values.len(),
            });
        }
        let generator_config_utf8 = generator_config_utf8.into();
        let payload = encode_payload(&values)?;
        let header = LutHeader {
            magic: String::from_utf8_lossy(&LUT_MAGIC).into_owned(),
            schema_version: LUT_SCHEMA_VERSION,
            axes,
            outputs: canonical_outputs(),
            generator,
            config_sha256: Sha256Digest::compute(generator_config_utf8.as_bytes()),
            generator_config_utf8,
            science,
            payload_encoding: PayloadEncoding::F64LePointMajorLastAxisFastest,
            grid_point_count: points as u64,
            payload_byte_length: payload.len() as u64,
            payload_sha256: Sha256Digest::compute(&payload),
        };
        header.validate()?;
        Ok(Self { header, values })
    }

    #[must_use]
    pub const fn header(&self) -> &LutHeader {
        &self.header
    }

    #[must_use]
    pub fn values(&self) -> &[AdditiveScattering] {
        &self.values
    }

    /// Encode deterministically. Header JSON comes from fixed struct field
    /// order and generator package versions are held in a BTreeMap.
    pub fn to_bytes(&self) -> Result<Vec<u8>, LutError> {
        let points = self.header.validate()?;
        if points != self.values.len() {
            return Err(LutError::ValueCount {
                expected: points,
                actual: self.values.len(),
            });
        }
        let payload = encode_payload(&self.values)?;
        let actual_digest = Sha256Digest::compute(&payload);
        if self.header.payload_sha256 != actual_digest {
            return Err(LutError::PayloadDigestMismatch {
                expected: self.header.payload_sha256,
                actual: actual_digest,
            });
        }
        assemble_file(&self.header, &payload)
    }

    /// Decode and validate magic, schema, all metadata, embedded config hash,
    /// payload length/hash, and every physical additive-output invariant.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, LutError> {
        if bytes.len() < PREFIX_BYTES {
            return Err(LutError::TruncatedPrefix {
                actual: bytes.len(),
            });
        }
        let truncated = || LutError::TruncatedPrefix {
            actual: bytes.len(),
        };
        let actual_magic: [u8; 8] = *bytes.first_chunk::<8>().ok_or_else(truncated)?;
        if actual_magic != LUT_MAGIC {
            return Err(LutError::FileMagic {
                actual: actual_magic,
            });
        }
        let schema = u16::from_le_bytes(
            *bytes
                .get(8..)
                .and_then(<[u8]>::first_chunk::<2>)
                .ok_or_else(truncated)?,
        );
        if schema != LUT_SCHEMA_VERSION {
            return Err(LutError::UnsupportedSchema { actual: schema });
        }
        let header_length = u32::from_le_bytes(
            *bytes
                .get(10..)
                .and_then(<[u8]>::first_chunk::<4>)
                .ok_or_else(truncated)?,
        ) as usize;
        if header_length == 0 || header_length > MAX_HEADER_BYTES {
            return Err(LutError::HeaderSize {
                actual: header_length,
            });
        }
        let header_end = PREFIX_BYTES
            .checked_add(header_length)
            .ok_or(LutError::DimensionOverflow)?;
        if header_end > bytes.len() {
            return Err(LutError::TruncatedHeader {
                declared: header_length,
                available: bytes.len() - PREFIX_BYTES,
            });
        }
        let header: LutHeader = serde_json::from_slice(&bytes[PREFIX_BYTES..header_end])
            .map_err(LutError::HeaderJson)?;
        if header.schema_version != schema {
            return Err(LutError::SchemaDisagreement {
                prefix: schema,
                header: header.schema_version,
            });
        }
        let points = header.validate()?;
        let payload = &bytes[header_end..];
        if payload.len() as u64 != header.payload_byte_length {
            return Err(LutError::PayloadLength {
                header: header.payload_byte_length,
                actual: payload.len() as u64,
            });
        }
        let actual_digest = Sha256Digest::compute(payload);
        if actual_digest != header.payload_sha256 {
            return Err(LutError::PayloadDigestMismatch {
                expected: header.payload_sha256,
                actual: actual_digest,
            });
        }
        let values = decode_payload(payload, points)?;
        Ok(Self { header, values })
    }

    /// Verify an externally supplied exact generator-config byte sequence.
    pub fn verify_generator_config(&self, exact_utf8: &[u8]) -> Result<(), LutError> {
        let actual = Sha256Digest::compute(exact_utf8);
        if actual == self.header.config_sha256 {
            Ok(())
        } else {
            Err(LutError::ExternalConfigDigestMismatch {
                expected: self.header.config_sha256,
                actual,
            })
        }
    }

    /// Validate and pre-bracket a query for deterministic multilinear
    /// interpolation with no extrapolation.
    ///
    /// Bracketing and typed-axis validation happen entirely on the CPU. The
    /// returned fixed-size plan can be copied into a larger batch without
    /// carrying query-axis metadata into an accelerator kernel.
    pub fn prepare_interpolation(
        &self,
        coordinates: &[AxisCoordinate],
    ) -> Result<PreparedInterpolationPlan, InterpolationError> {
        if coordinates.len() != self.header.axes.len() {
            return Err(InterpolationError::CoordinateCount {
                expected: self.header.axes.len(),
                actual: coordinates.len(),
            });
        }
        let strides = last_axis_fastest_strides(&self.header.axes)?;
        let mut base_point_index = 0_usize;
        let mut upper_point_offsets = [0_u64; PREPARED_INTERPOLATION_AXIS_SLOTS];
        let mut upper_fractions = [0.0_f64; PREPARED_INTERPOLATION_AXIS_SLOTS];
        let mut active_axis_count = 0_usize;
        for (index, (axis, coordinate)) in
            self.header.axes.iter().zip(coordinates.iter()).enumerate()
        {
            if axis.kind != coordinate.kind {
                return Err(InterpolationError::AxisOrder {
                    index,
                    expected: axis.kind,
                    actual: coordinate.kind,
                });
            }
            if !coordinate.value.is_finite() {
                return Err(InterpolationError::NonFiniteCoordinate {
                    kind: coordinate.kind,
                    value: coordinate.value,
                });
            }
            let bracket = axis.locate(coordinate.value)?;
            base_point_index = base_point_index
                .checked_add(
                    bracket
                        .lower
                        .checked_mul(strides[index])
                        .ok_or(InterpolationError::DimensionOverflow)?,
                )
                .ok_or(InterpolationError::DimensionOverflow)?;
            if bracket.lower != bracket.upper {
                let upper_delta = bracket
                    .upper
                    .checked_sub(bracket.lower)
                    .and_then(|delta| delta.checked_mul(strides[index]))
                    .ok_or(InterpolationError::DimensionOverflow)?;
                upper_point_offsets[active_axis_count] = u64::try_from(upper_delta)
                    .map_err(|_| InterpolationError::DimensionOverflow)?;
                upper_fractions[active_axis_count] = bracket.fraction;
                active_axis_count += 1;
            }
        }
        let active_axis_count =
            u32::try_from(active_axis_count).map_err(|_| InterpolationError::DimensionOverflow)?;
        let corner_count = 1_u32
            .checked_shl(active_axis_count)
            .ok_or(InterpolationError::DimensionOverflow)?;
        Ok(PreparedInterpolationPlan {
            base_point_index: u64::try_from(base_point_index)
                .map_err(|_| InterpolationError::DimensionOverflow)?,
            upper_point_offsets,
            upper_fractions,
            corner_count,
            active_axis_count,
        })
    }

    /// Execute a plan returned by [`Self::prepare_interpolation`].
    ///
    /// Corner masks, active axes, weight multiplication, and component
    /// accumulation retain the same ascending order as direct interpolation.
    /// The plan must have been prepared for this table.
    pub fn interpolate_prepared(
        &self,
        plan: &PreparedInterpolationPlan,
    ) -> Result<AdditiveScattering, InterpolationError> {
        let active_axis_count = usize::try_from(plan.active_axis_count)
            .map_err(|_| InterpolationError::DimensionOverflow)?;
        if active_axis_count > PREPARED_INTERPOLATION_AXIS_SLOTS {
            return Err(InterpolationError::InvalidPreparedPlan {
                reason: "active axis count exceeds the fixed host layout",
            });
        }
        let expected_corner_count = 1_u32
            .checked_shl(plan.active_axis_count)
            .ok_or(InterpolationError::DimensionOverflow)?;
        if plan.corner_count != expected_corner_count {
            return Err(InterpolationError::InvalidPreparedPlan {
                reason: "corner count does not equal 2^active_axis_count",
            });
        }
        let base_point_index = usize::try_from(plan.base_point_index)
            .map_err(|_| InterpolationError::DimensionOverflow)?;
        let mut maximum_point_index = base_point_index;
        for active_axis in 0..active_axis_count {
            let fraction = plan.upper_fractions[active_axis];
            if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
                return Err(InterpolationError::InvalidPreparedPlan {
                    reason: "active upper fraction is not finite and within [0, 1]",
                });
            }
            let offset = usize::try_from(plan.upper_point_offsets[active_axis])
                .map_err(|_| InterpolationError::DimensionOverflow)?;
            if offset == 0 {
                return Err(InterpolationError::InvalidPreparedPlan {
                    reason: "active upper point offset is zero",
                });
            }
            maximum_point_index = maximum_point_index
                .checked_add(offset)
                .ok_or(InterpolationError::DimensionOverflow)?;
        }
        if maximum_point_index >= self.values.len() {
            return Err(InterpolationError::InvalidPreparedPlan {
                reason: "prepared point index is outside the LUT payload",
            });
        }

        let mut accumulated = [0.0; AdditiveScattering::COMPONENT_COUNT];

        // Ascending corner masks and fixed axis order make evaluation stable
        // and reproducible across calls/platforms using IEEE-754 f64.
        for corner in 0..plan.corner_count {
            let mut point_index = base_point_index;
            let mut weight = 1.0;
            for active_axis in 0..active_axis_count {
                let upper = ((corner >> active_axis) & 1) == 1;
                let fraction = plan.upper_fractions[active_axis];
                if upper {
                    weight *= fraction;
                    point_index = point_index
                        .checked_add(
                            usize::try_from(plan.upper_point_offsets[active_axis])
                                .map_err(|_| InterpolationError::DimensionOverflow)?,
                        )
                        .ok_or(InterpolationError::DimensionOverflow)?;
                } else {
                    weight *= 1.0 - fraction;
                }
            }
            let components = self
                .values
                .get(point_index)
                .ok_or(InterpolationError::DimensionOverflow)?
                .components();
            for component in 0..AdditiveScattering::COMPONENT_COUNT {
                accumulated[component] += weight * components[component];
            }
        }

        AdditiveScattering::from_components(accumulated)
            .map_err(InterpolationError::InvalidInterpolatedOutput)
    }

    /// Deterministic multilinear interpolation with no extrapolation.
    pub fn interpolate(
        &self,
        coordinates: &[AxisCoordinate],
    ) -> Result<AdditiveScattering, InterpolationError> {
        let plan = self.prepare_interpolation(coordinates)?;
        self.interpolate_prepared(&plan)
    }
}

fn grid_point_count(axes: &[Axis]) -> Result<usize, LutError> {
    axes.iter().try_fold(1_usize, |count, axis| {
        count
            .checked_mul(axis.coordinates.len())
            .ok_or(LutError::DimensionOverflow)
    })
}

fn last_axis_fastest_strides(
    axes: &[Axis],
) -> Result<[usize; PREPARED_INTERPOLATION_AXIS_SLOTS], InterpolationError> {
    let mut strides = [1_usize; PREPARED_INTERPOLATION_AXIS_SLOTS];
    for index in (0..axes.len().saturating_sub(1)).rev() {
        strides[index] = strides[index + 1]
            .checked_mul(axes[index + 1].coordinates.len())
            .ok_or(InterpolationError::DimensionOverflow)?;
    }
    Ok(strides)
}

fn encode_payload(values: &[AdditiveScattering]) -> Result<Vec<u8>, LutError> {
    let capacity = values
        .len()
        .checked_mul(AdditiveScattering::COMPONENT_COUNT)
        .and_then(|components| components.checked_mul(size_of::<f64>()))
        .ok_or(LutError::DimensionOverflow)?;
    let mut payload = Vec::with_capacity(capacity);
    for value in values {
        for component in value.components() {
            payload.extend_from_slice(&component.to_le_bytes());
        }
    }
    Ok(payload)
}

fn decode_payload(payload: &[u8], points: usize) -> Result<Vec<AdditiveScattering>, LutError> {
    let point_bytes = AdditiveScattering::COMPONENT_COUNT * size_of::<f64>();
    let mut values = Vec::with_capacity(points);
    for (point, bytes) in payload.chunks_exact(point_bytes).enumerate() {
        let mut components = [0.0; AdditiveScattering::COMPONENT_COUNT];
        for (component, word) in components.iter_mut().zip(bytes.as_chunks::<8>().0) {
            *component = f64::from_le_bytes(*word);
        }
        values.push(
            AdditiveScattering::from_components(components)
                .map_err(|source| LutError::InvalidOutput { point, source })?,
        );
    }
    if values.len() != points || !payload.chunks_exact(point_bytes).remainder().is_empty() {
        return Err(LutError::PayloadLength {
            header: (points * point_bytes) as u64,
            actual: payload.len() as u64,
        });
    }
    Ok(values)
}

fn assemble_file(header: &LutHeader, payload: &[u8]) -> Result<Vec<u8>, LutError> {
    let header_json = serde_json::to_vec(header).map_err(LutError::HeaderJson)?;
    if header_json.is_empty() || header_json.len() > MAX_HEADER_BYTES {
        return Err(LutError::HeaderSize {
            actual: header_json.len(),
        });
    }
    let header_length = u32::try_from(header_json.len()).map_err(|_| LutError::HeaderSize {
        actual: header_json.len(),
    })?;
    let file_capacity = PREFIX_BYTES
        .checked_add(header_json.len())
        .and_then(|length| length.checked_add(payload.len()))
        .ok_or(LutError::DimensionOverflow)?;
    let mut file = Vec::with_capacity(file_capacity);
    file.extend_from_slice(&LUT_MAGIC);
    file.extend_from_slice(&LUT_SCHEMA_VERSION.to_le_bytes());
    file.extend_from_slice(&header_length.to_le_bytes());
    file.extend_from_slice(&header_json);
    file.extend_from_slice(payload);
    Ok(file)
}

#[derive(Debug, Error)]
pub enum LutError {
    #[error("LUT prefix is truncated: expected at least 14 bytes, got {actual}")]
    TruncatedPrefix { actual: usize },
    #[error("invalid LUT file magic {actual:?}")]
    FileMagic { actual: [u8; 8] },
    #[error("unsupported LUT schema version {actual}")]
    UnsupportedSchema { actual: u16 },
    #[error("invalid LUT header length {actual}")]
    HeaderSize { actual: usize },
    #[error("LUT header declares {declared} bytes but only {available} remain")]
    TruncatedHeader { declared: usize, available: usize },
    #[error("LUT header JSON is invalid: {0}")]
    HeaderJson(#[source] serde_json::Error),
    #[error("header magic {actual:?} does not match the schema magic")]
    HeaderMagic { actual: String },
    #[error("prefix schema {prefix} does not match header schema {header}")]
    SchemaDisagreement { prefix: u16, header: u16 },
    #[error("LUT must have 1..={maximum} axes, got {actual}")]
    AxisCount { actual: usize, maximum: usize },
    #[error("axis {index} ({kind:?}) must use {expected:?}, got {actual:?}")]
    AxisUnit {
        index: usize,
        kind: AxisKind,
        expected: Unit,
        actual: Unit,
    },
    #[error("axis {index} has no coordinates")]
    EmptyAxis { index: usize },
    #[error("axis {axis} coordinate {coordinate} is not finite: {value}")]
    NonFiniteAxisCoordinate {
        axis: usize,
        coordinate: usize,
        value: f64,
    },
    #[error("axis {axis} is not strictly increasing at {lower}, {upper}")]
    NonIncreasingAxis { axis: usize, lower: f64, upper: f64 },
    #[error("axis kind {kind:?} appears more than once")]
    DuplicateAxis { kind: AxisKind },
    #[error("output descriptors or units do not match schema-v1 canonical additive outputs")]
    NonCanonicalOutputs,
    #[error("{field} must not be empty")]
    EmptyMetadata { field: &'static str },
    #[error("generator package name {package:?} is not canonical lowercase text")]
    NonCanonicalPackageName { package: String },
    #[error("generator package {package} has an empty version")]
    EmptyPackageVersion { package: String },
    #[error("PyTMatrix tables must identify package pytmatrix version 0.3.3, got {actual:?}")]
    PyTMatrixVersion { actual: Option<String> },
    #[error("generator config is not valid JSON: {0}")]
    GeneratorConfigJson(#[source] serde_json::Error),
    #[error("generator config JSON must be an object")]
    GeneratorConfigNotObject,
    #[error("embedded generator config SHA-256 mismatch: expected {expected}, got {actual}")]
    ConfigDigestMismatch {
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    #[error("external generator config SHA-256 mismatch: expected {expected}, got {actual}")]
    ExternalConfigDigestMismatch {
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    #[error("axis dimensions overflow addressable table size")]
    DimensionOverflow,
    #[error("header grid-point count {header} does not match axes {axes}")]
    GridPointCount { header: u64, axes: u64 },
    #[error("header payload length {header} does not match schema-derived length {expected}")]
    HeaderPayloadLength { header: u64, expected: u64 },
    #[error("payload length {actual} does not match header length {header}")]
    PayloadLength { header: u64, actual: u64 },
    #[error("payload SHA-256 mismatch: expected {expected}, got {actual}")]
    PayloadDigestMismatch {
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    #[error("table has {actual} values but axes require {expected}")]
    ValueCount { expected: usize, actual: usize },
    #[error("invalid additive output at grid point {point}: {source}")]
    InvalidOutput {
        point: usize,
        #[source]
        source: OutputError,
    },
    #[error(transparent)]
    Science(#[from] ScienceError),
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum InterpolationError {
    #[error("coordinate for {kind:?} must be finite, got {value}")]
    NonFiniteCoordinate { kind: AxisKind, value: f64 },
    #[error("expected {expected} interpolation coordinates, got {actual}")]
    CoordinateCount { expected: usize, actual: usize },
    #[error("coordinate {index} must be for {expected:?}, got {actual:?}")]
    AxisOrder {
        index: usize,
        expected: AxisKind,
        actual: AxisKind,
    },
    #[error("{kind:?} coordinate {value} is outside [{minimum}, {maximum}]")]
    OutsideAxis {
        kind: AxisKind,
        value: f64,
        minimum: f64,
        maximum: f64,
    },
    #[error("interpolation dimensions overflow addressable table size")]
    DimensionOverflow,
    #[error("invalid prepared interpolation plan: {reason}")]
    InvalidPreparedPlan { reason: &'static str },
    #[error("interpolation produced an invalid additive output: {0}")]
    InvalidInterpolatedOutput(OutputError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_corpus::{self, CorpusLut, array, as_f64, as_usize, f64s};
    use crate::{MeltingModel, OrientationModel, TableValidation, TemporalSampling};

    const COMPONENT_NAMES: [&str; AdditiveScattering::COMPONENT_COUNT] = [
        "zh",
        "zv",
        "hh_vv_covariance_real",
        "hh_vv_covariance_imaginary",
        "kdp",
        "ah",
        "av",
        "fall_speed_first_moment",
        "fall_speed_second_moment",
    ];

    /// Axis coordinates of a golden query, in the table's declared order.
    fn query(table: &OfflineLut, values: &[f64]) -> Vec<AxisCoordinate> {
        table
            .header()
            .axes()
            .iter()
            .zip(values)
            .map(|(axis, value)| AxisCoordinate::new(axis.kind(), *value).unwrap())
            .collect()
    }

    // Frozen copy of the pre-plan implementation. Comparing component bits
    // against it protects corner/axis/arithmetic order while the prepared
    // layout becomes the common CPU/CUDA boundary.
    fn legacy_interpolate(
        table: &OfflineLut,
        coordinates: &[AxisCoordinate],
    ) -> Result<AdditiveScattering, InterpolationError> {
        if coordinates.len() != table.header.axes.len() {
            return Err(InterpolationError::CoordinateCount {
                expected: table.header.axes.len(),
                actual: coordinates.len(),
            });
        }
        let mut brackets = Vec::with_capacity(coordinates.len());
        for (index, (axis, coordinate)) in
            table.header.axes.iter().zip(coordinates.iter()).enumerate()
        {
            if axis.kind != coordinate.kind {
                return Err(InterpolationError::AxisOrder {
                    index,
                    expected: axis.kind,
                    actual: coordinate.kind,
                });
            }
            if !coordinate.value.is_finite() {
                return Err(InterpolationError::NonFiniteCoordinate {
                    kind: coordinate.kind,
                    value: coordinate.value,
                });
            }
            brackets.push(axis.locate(coordinate.value)?);
        }

        let strides = last_axis_fastest_strides(&table.header.axes)?;
        let active_axis_count = brackets
            .iter()
            .filter(|bracket| bracket.lower != bracket.upper)
            .count();
        let corner_count = 1_usize
            .checked_shl(active_axis_count as u32)
            .ok_or(InterpolationError::DimensionOverflow)?;
        let mut accumulated = [0.0; AdditiveScattering::COMPONENT_COUNT];
        for corner in 0..corner_count {
            let mut point_index = 0_usize;
            let mut weight = 1.0;
            let mut active_bit = 0_usize;
            for axis_index in 0..brackets.len() {
                let bracket = brackets[axis_index];
                let coordinate_index = if bracket.lower == bracket.upper {
                    bracket.lower
                } else {
                    let upper = ((corner >> active_bit) & 1) == 1;
                    active_bit += 1;
                    if upper {
                        weight *= bracket.fraction;
                        bracket.upper
                    } else {
                        weight *= 1.0 - bracket.fraction;
                        bracket.lower
                    }
                };
                point_index = point_index
                    .checked_add(
                        coordinate_index
                            .checked_mul(strides[axis_index])
                            .ok_or(InterpolationError::DimensionOverflow)?,
                    )
                    .ok_or(InterpolationError::DimensionOverflow)?;
            }
            let components = table
                .values
                .get(point_index)
                .ok_or(InterpolationError::DimensionOverflow)?
                .components();
            for component in 0..AdditiveScattering::COMPONENT_COUNT {
                accumulated[component] += weight * components[component];
            }
        }
        AdditiveScattering::from_components(accumulated)
            .map_err(InterpolationError::InvalidInterpolatedOutput)
    }

    fn assert_bit_identical(actual: AdditiveScattering, expected: AdditiveScattering) {
        for (index, (actual, expected)) in actual
            .components()
            .into_iter()
            .zip(expected.components())
            .enumerate()
        {
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "component {index}: {actual:.17e} != {expected:.17e}"
            );
        }
    }

    fn assert_relative(actual: &[f64], expected: &[f64], tolerance: f64, what: &str) {
        for (index, (a, e)) in actual.iter().zip(expected).enumerate() {
            let scale = e.abs().max(f64::MIN_POSITIVE);
            assert!(
                (a - e).abs() / scale <= tolerance,
                "{what}: component {index} ({}) {a:e} != {e:e}",
                COMPONENT_NAMES[index]
            );
        }
    }

    /// The two committed tables decode with the metadata their generator
    /// manifests record, and re-encode to exactly the committed bytes.
    #[test]
    fn committed_pytmatrix_tables_round_trip_byte_exactly() {
        for corpus in [test_corpus::rain(), test_corpus::dry_ice()] {
            let CorpusLut {
                table,
                bytes,
                config,
                golden,
            } = corpus;
            assert_eq!(table.to_bytes().unwrap(), bytes, "{}", golden["id"]);
            assert_eq!(table.to_bytes().unwrap(), table.to_bytes().unwrap());
            assert_eq!(table.header().magic(), "BRSLUT01");
            assert_eq!(table.header().schema_version(), 1);
            assert_eq!(table.values().len(), as_usize(&golden["grid_point_count"]));
            assert_eq!(
                table.header().grid_point_count(),
                golden["grid_point_count"].as_u64().unwrap()
            );
            assert_eq!(
                table.header().payload_byte_length(),
                golden["payload_byte_length"].as_u64().unwrap()
            );
            assert_eq!(
                table.header().payload_sha256(),
                Sha256Digest::from_hex(golden["payload_sha256"].as_str().unwrap()).unwrap()
            );
            assert_eq!(
                table.header().config_sha256(),
                Sha256Digest::from_hex(golden["config_sha256"].as_str().unwrap()).unwrap()
            );
            table.verify_generator_config(&config).unwrap();
            assert_eq!(
                table.header().generator_config_utf8().as_bytes(),
                &config[..]
            );
            assert_eq!(
                table
                    .header()
                    .generator()
                    .package_versions()
                    .get("pytmatrix"),
                Some(&"0.3.3".to_owned())
            );
            assert_eq!(
                *table.header().science().validation(),
                TableValidation::ResearchOnlyUnvalidated
            );
            assert!(matches!(
                table.header().science().kernel(),
                KernelModel::TMatrix {
                    implementation: TMatrixImplementation::PyTMatrix033
                }
            ));
            assert_eq!(*table.header().science().melting(), MeltingModel::Dry);
            assert_eq!(
                *table.header().science().temporal(),
                TemporalSampling::Instantaneous
            );
            for (axis, want) in table.header().axes().iter().zip(array(&golden["axes"])) {
                assert_eq!(
                    serde_json::to_value(axis.kind()).unwrap(),
                    want["kind"],
                    "axis kind"
                );
                assert_eq!(axis.coordinates().len(), as_usize(&want["count"]));
                assert_eq!(axis.coordinates()[0], as_f64(&want["first"]));
                assert_eq!(*axis.coordinates().last().unwrap(), as_f64(&want["last"]));
            }
            // Payload nodes read directly from the file bytes.
            for probe in array(&golden["probes"]) {
                let index = as_usize(&probe["index"]);
                let want = f64s(&probe["components"]);
                for (got, want) in table.values()[index].components().iter().zip(&want) {
                    assert_eq!(got.to_bits(), want.to_bits(), "node {index}");
                }
            }
        }
        assert!(matches!(
            test_corpus::rain().table.header().science().orientation(),
            OrientationModel::FixedEuler { .. }
        ));
        assert!(matches!(
            test_corpus::dry_ice()
                .table
                .header()
                .science()
                .orientation(),
            OrientationModel::GaussianCanting {
                standard_deviation_deg: 20.0,
                ..
            }
        ));
    }

    /// Interpolation at the post-freeze held-out nodes (coordinates absent
    /// from the grids, chosen from a public seed) reproduces the validator's
    /// own multilinear interpolation and agrees with the direct PyTMatrix
    /// recomputation exactly where the report says it does. Coordinates
    /// must be named in the declared axis order.
    #[test]
    fn held_out_nodes_match_the_validator_and_direct_pytmatrix() {
        let golden = test_corpus::golden("tmatrix_luts.json");
        let thresholds = &golden["thresholds"];
        let mut checked = 0;
        for corpus in [test_corpus::rain(), test_corpus::dry_ice()] {
            for node in array(&corpus.golden["held_out"]) {
                let coordinates = query(&corpus.table, &f64s(&node["coordinates"]));
                let actual = corpus.table.interpolate(&coordinates).unwrap().components();
                assert_relative(
                    &actual,
                    &f64s(&node["validator_interpolation"]),
                    1.0e-9,
                    "validator interpolation",
                );
                let direct = f64s(&node["direct_pytmatrix"]);
                // The report's per-component rule: |error| / max(|direct|,
                // absolute floor) within the predeclared relative budget.
                let within =
                    actual
                        .iter()
                        .zip(&direct)
                        .zip(COMPONENT_NAMES)
                        .all(|((a, d), name)| {
                            let floor = as_f64(&thresholds[name]["absolute"]);
                            (a - d).abs() / d.abs().max(floor)
                                <= as_f64(&thresholds[name]["relative"])
                        });
                assert_eq!(
                    within,
                    node["within_thresholds"].as_bool().unwrap(),
                    "{} node {}: {actual:?} vs direct {direct:?}",
                    corpus.golden["id"],
                    node["node_index"]
                );
                checked += 1;
            }
            let mut swapped = query(
                &corpus.table,
                &f64s(&corpus.golden["held_out"][0]["coordinates"]),
            );
            swapped.swap(0, 1);
            assert!(matches!(
                corpus.table.interpolate(&swapped).unwrap_err(),
                InterpolationError::AxisOrder { index: 0, .. }
            ));
        }
        assert_eq!(checked, 12);
    }

    /// The prepared plan of the rain table (dimensions [16, 3, 1, 1],
    /// last-axis-fastest strides [3, 1, 1, 1]) for a query between diameter
    /// nodes 1 and 2 and between the first two axis ratios: base at the
    /// all-lower corner, two active axes in header order, offsets 3 and 1,
    /// the singleton frequency and elevation axes omitted; serializable and
    /// bit-identical to the legacy corner walk.
    #[test]
    fn prepared_plan_has_fixed_serializable_axis_ordered_layout() {
        let corpus = test_corpus::rain();
        let want = &corpus.golden["plans"]["between"];
        let coordinates = query(&corpus.table, &f64s(&want["query"]));
        let plan = corpus.table.prepare_interpolation(&coordinates).unwrap();
        let layout = &want["plan"];
        assert_eq!(
            plan.base_point_index(),
            layout["base_point_index"].as_u64().unwrap()
        );
        assert_eq!(
            plan.active_axis_count(),
            as_usize(&layout["active_axis_count"]) as u32
        );
        assert_eq!(
            plan.corner_count(),
            as_usize(&layout["corner_count"]) as u32
        );
        let offsets: Vec<u64> = array(&layout["upper_point_offsets"])
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();
        assert_eq!(&plan.upper_point_offsets()[..offsets.len()], &offsets[..]);
        assert_eq!(offsets, vec![3, 1]);
        let fractions = f64s(&layout["upper_fractions"]);
        for (got, want) in plan.upper_fractions().iter().zip(&fractions) {
            assert_eq!(got.to_bits(), want.to_bits());
        }
        assert!(
            plan.upper_point_offsets()[offsets.len()..]
                .iter()
                .all(|value| *value == 0)
        );
        assert!(
            plan.upper_fractions()[fractions.len()..]
                .iter()
                .all(|value| *value == 0.0)
        );
        assert_eq!(size_of::<PreparedInterpolationPlan>(), 272);

        let encoded = serde_json::to_vec(&plan).unwrap();
        let decoded: PreparedInterpolationPlan = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, plan);
        let prepared = corpus.table.interpolate_prepared(&decoded).unwrap();
        assert_bit_identical(
            prepared,
            legacy_interpolate(&corpus.table, &coordinates).unwrap(),
        );
        assert_relative(
            &prepared.components(),
            &f64s(&want["interpolation"]),
            1.0e-12,
            "reference interpolation",
        );
    }

    /// Prepared execution on both tables is bit-identical to the legacy
    /// corner order at the held-out nodes and at the golden queries.
    #[test]
    fn prepared_execution_is_bit_identical_to_legacy_corner_order() {
        for corpus in [test_corpus::rain(), test_corpus::dry_ice()] {
            let mut queries: Vec<Vec<f64>> = array(&corpus.golden["held_out"])
                .iter()
                .map(|node| f64s(&node["coordinates"]))
                .collect();
            for plan in ["between", "exact_first", "exact_last"] {
                queries.push(f64s(&corpus.golden["plans"][plan]["query"]));
            }
            for values in queries {
                let coordinates = query(&corpus.table, &values);
                let expected = legacy_interpolate(&corpus.table, &coordinates).unwrap();
                let plan = corpus.table.prepare_interpolation(&coordinates).unwrap();
                assert_bit_identical(corpus.table.interpolate_prepared(&plan).unwrap(), expected);
                assert_bit_identical(corpus.table.interpolate(&coordinates).unwrap(), expected);
            }
        }
    }

    /// Queries on the exact first and last nodes of every axis (the
    /// singleton frequency and elevation axes included) bracket to the node
    /// itself: no active axis, one corner, the stored node returned.
    #[test]
    fn prepared_plan_handles_singletons_and_exact_boundaries() {
        for corpus in [test_corpus::rain(), test_corpus::dry_ice()] {
            for (name, expected_index) in [
                ("exact_first", 0),
                ("exact_last", corpus.table.values().len() - 1),
            ] {
                let want = &corpus.golden["plans"][name];
                let coordinates = query(&corpus.table, &f64s(&want["query"]));
                let plan = corpus.table.prepare_interpolation(&coordinates).unwrap();
                assert_eq!(plan.base_point_index(), expected_index as u64);
                assert_eq!(
                    plan.base_point_index(),
                    want["plan"]["base_point_index"].as_u64().unwrap()
                );
                assert_eq!(plan.active_axis_count(), 0);
                assert_eq!(plan.corner_count(), 1);
                let value = corpus.table.interpolate_prepared(&plan).unwrap();
                assert_bit_identical(
                    value,
                    legacy_interpolate(&corpus.table, &coordinates).unwrap(),
                );
                assert_bit_identical(value, corpus.table.values()[expected_index]);
            }
        }
    }

    /// Outside-axis failures of the dry-ice table (diameter 0.1-50 mm,
    /// ratios 0.7-1.0, singleton 2.7008 GHz and 0 deg) survive plan
    /// preparation unchanged.
    #[test]
    fn plan_preparation_preserves_outside_axis_failures() {
        let corpus = test_corpus::dry_ice();
        let axes = corpus.table.header().axes();
        let (dmin, dmax) = (
            axes[0].coordinates()[0],
            *axes[0].coordinates().last().unwrap(),
        );
        let (rmin, rmax) = (
            axes[1].coordinates()[0],
            *axes[1].coordinates().last().unwrap(),
        );
        let frequency = axes[2].coordinates()[0];

        let above_ratio = query(&corpus.table, &[dmin, rmax + 0.01, frequency, 0.0]);
        let expected = InterpolationError::OutsideAxis {
            kind: AxisKind::MinorToMajorAxisRatio,
            value: rmax + 0.01,
            minimum: rmin,
            maximum: rmax,
        };
        assert_eq!(
            corpus
                .table
                .prepare_interpolation(&above_ratio)
                .unwrap_err(),
            expected
        );
        assert_eq!(
            corpus.table.interpolate(&above_ratio).unwrap_err(),
            expected
        );

        let above_diameter = query(&corpus.table, &[dmax * 1.5, rmin, frequency, 0.0]);
        assert!(matches!(
            corpus.table.prepare_interpolation(&above_diameter),
            Err(InterpolationError::OutsideAxis {
                kind: AxisKind::EquivolumeDiameter,
                ..
            })
        ));

        let outside_singleton = query(&corpus.table, &[dmin, rmin, frequency, 0.5]);
        assert!(matches!(
            corpus.table.prepare_interpolation(&outside_singleton),
            Err(InterpolationError::OutsideAxis {
                kind: AxisKind::RadarElevation,
                minimum: 0.0,
                maximum: 0.0,
                ..
            })
        ));
        let other_frequency = query(&corpus.table, &[dmin, rmin, 2.8e9, 0.0]);
        assert!(matches!(
            corpus.table.prepare_interpolation(&other_frequency),
            Err(InterpolationError::OutsideAxis {
                kind: AxisKind::Frequency,
                ..
            })
        ));
    }

    /// The rain table never extrapolates below its 0.3 mm diameter floor,
    /// and non-finite coordinates are refused before any lookup.
    #[test]
    fn interpolation_refuses_extrapolation_and_nonfinite_coordinates() {
        let corpus = test_corpus::rain();
        let axes = corpus.table.header().axes();
        let below = query(
            &corpus.table,
            &[
                axes[0].coordinates()[0] / 2.0,
                0.9,
                axes[2].coordinates()[0],
                0.0,
            ],
        );
        assert!(matches!(
            corpus.table.interpolate(&below),
            Err(InterpolationError::OutsideAxis {
                kind: AxisKind::EquivolumeDiameter,
                ..
            })
        ));
        assert!(matches!(
            AxisCoordinate::new(AxisKind::Temperature, f64::NAN),
            Err(InterpolationError::NonFiniteCoordinate { .. })
        ));
        assert!(matches!(
            AxisCoordinate::new(AxisKind::EquivolumeDiameter, f64::INFINITY),
            Err(InterpolationError::NonFiniteCoordinate { .. })
        ));
        let short = &query(&corpus.table, &[0.001, 0.9, axes[2].coordinates()[0], 0.0])[..3];
        assert!(matches!(
            corpus.table.interpolate(short),
            Err(InterpolationError::CoordinateCount {
                expected: 4,
                actual: 3
            })
        ));
    }

    /// One flipped bit in the last payload byte of the committed rain table
    /// fails the payload digest; a config that is not the generator's exact
    /// bytes fails the external config digest.
    #[test]
    fn payload_and_external_config_hash_mismatches_fail_closed() {
        let corpus = test_corpus::rain();
        let mut bytes = corpus.bytes.clone();
        *bytes.last_mut().unwrap() ^= 0x01;
        assert!(matches!(
            OfflineLut::from_bytes(&bytes),
            Err(LutError::PayloadDigestMismatch { .. })
        ));
        let mut config = corpus.config.clone();
        // The dry-ice config is a different real generator config.
        assert!(matches!(
            corpus
                .table
                .verify_generator_config(&test_corpus::dry_ice().config),
            Err(LutError::ExternalConfigDigestMismatch { .. })
        ));
        // One byte of trailing whitespace is already a different config.
        config.push(b'\n');
        assert!(matches!(
            corpus.table.verify_generator_config(&config),
            Err(LutError::ExternalConfigDigestMismatch { .. })
        ));
    }

    /// Replacing the embedded config text of the committed table with the
    /// dry-ice table's real config while keeping the recorded config digest
    /// is caught: the digest is recomputed from the embedded bytes.
    #[test]
    fn embedded_config_hash_is_recomputed_not_trusted() {
        let corpus = test_corpus::rain();
        let mut header = corpus.table.header.clone();
        header.generator_config_utf8 = String::from_utf8(test_corpus::dry_ice().config).unwrap();
        let payload = encode_payload(&corpus.table.values).unwrap();
        let bytes = assemble_file(&header, &payload).unwrap();
        assert!(matches!(
            OfflineLut::from_bytes(&bytes),
            Err(LutError::ConfigDigestMismatch { .. })
        ));
    }

    /// A node of the real payload rewritten to a non-physical covariance
    /// (real part above sqrt(ZH * ZV)) is rejected even with the payload
    /// digest recomputed for the edited bytes.
    #[test]
    fn digest_cannot_hide_an_invalid_additive_grid_node() {
        let path = recast_radar_testdata::path("tmatrix-lut-rain-sband-pytmatrix-0.3.3").unwrap();
        let table = OfflineLut::from_bytes(&std::fs::read(path).unwrap()).unwrap();
        let mut header = table.header.clone();
        let mut payload = encode_payload(&table.values).unwrap();
        let point = table.values().len() / 2;
        let node = table.values()[point].components();
        let bound = (node[0] * node[1]).sqrt();
        let offset = (point * AdditiveScattering::COMPONENT_COUNT + 2) * size_of::<f64>();
        payload[offset..offset + 8].copy_from_slice(&(bound * 1.5).to_le_bytes());
        header.payload_sha256 = Sha256Digest::compute(&payload);
        let bytes = assemble_file(&header, &payload).unwrap();
        assert!(matches!(
            OfflineLut::from_bytes(&bytes),
            Err(LutError::InvalidOutput { point: p, .. }) if p == point
        ));
    }

    #[test]
    fn axes_require_exact_units_and_strict_ordering() {
        assert!(matches!(
            Axis::new(AxisKind::Temperature, Unit::Meter, vec![260.0]),
            Err(LutError::AxisUnit { .. })
        ));
        assert!(matches!(
            Axis::new(AxisKind::Temperature, Unit::Kelvin, vec![270.0, 270.0]),
            Err(LutError::NonIncreasingAxis { .. })
        ));
    }

    /// The file magic and the schema prefix of the committed bytes are
    /// checked before anything is parsed.
    #[test]
    fn file_magic_and_schema_are_never_guessed() {
        let path = recast_radar_testdata::path("tmatrix-lut-rain-sband-pytmatrix-0.3.3").unwrap();
        let committed = std::fs::read(path).unwrap();
        OfflineLut::from_bytes(&committed).unwrap();
        let mut bytes = committed.clone();
        bytes[0] = b'X';
        assert!(matches!(
            OfflineLut::from_bytes(&bytes),
            Err(LutError::FileMagic { .. })
        ));

        let mut bytes = committed.clone();
        bytes[8..10].copy_from_slice(&2_u16.to_le_bytes());
        assert!(matches!(
            OfflineLut::from_bytes(&bytes),
            Err(LutError::UnsupportedSchema { actual: 2 })
        ));

        assert!(matches!(
            OfflineLut::from_bytes(&committed[..10]),
            Err(LutError::TruncatedPrefix { actual: 10 })
        ));
        assert!(matches!(
            OfflineLut::from_bytes(&committed[..200]),
            Err(LutError::TruncatedHeader { .. })
        ));
    }

    /// The redundant header fields of the committed table (magic, schema,
    /// axis set, output descriptors) are each checked against the schema.
    #[test]
    fn redundant_header_contract_rejects_mislabeled_schema_axes_and_outputs() {
        let corpus = test_corpus::rain();
        let table = &corpus.table;
        let payload = encode_payload(&table.values).unwrap();

        let mut header = table.header.clone();
        header.magic = "NOTALUT!".to_owned();
        assert!(matches!(
            OfflineLut::from_bytes(&assemble_file(&header, &payload).unwrap()),
            Err(LutError::HeaderMagic { .. })
        ));

        let mut header = table.header.clone();
        header.schema_version = 2;
        assert!(matches!(
            OfflineLut::from_bytes(&assemble_file(&header, &payload).unwrap()),
            Err(LutError::SchemaDisagreement {
                prefix: 1,
                header: 2
            })
        ));

        let mut header = table.header.clone();
        header.axes.push(header.axes[0].clone());
        assert!(matches!(
            OfflineLut::from_bytes(&assemble_file(&header, &payload).unwrap()),
            Err(LutError::DuplicateAxis {
                kind: AxisKind::EquivolumeDiameter
            })
        ));

        let mut header = table.header.clone();
        header.outputs[0].unit = Unit::Degree;
        assert!(matches!(
            OfflineLut::from_bytes(&assemble_file(&header, &payload).unwrap()),
            Err(LutError::NonCanonicalOutputs)
        ));
    }

    /// The singleton frequency and elevation axes of both tables contribute
    /// no corner: a query on a grid node returns exactly the stored node,
    /// and each singleton coordinate is exact.
    #[test]
    fn singleton_axis_is_exact_and_does_not_duplicate_corner_weight() {
        for corpus in [test_corpus::rain(), test_corpus::dry_ice()] {
            let axes = corpus.table.header().axes();
            assert_eq!(
                axes[2].coordinates().len(),
                1,
                "frequency is a singleton axis"
            );
            assert_eq!(
                axes[3].coordinates().len(),
                1,
                "elevation is a singleton axis"
            );
            for probe in array(&corpus.golden["probes"]) {
                let coordinates = query(&corpus.table, &f64s(&probe["coordinates"]));
                let plan = corpus.table.prepare_interpolation(&coordinates).unwrap();
                assert_eq!(plan.active_axis_count(), 0);
                assert_eq!(plan.corner_count(), 1);
                assert_eq!(plan.base_point_index(), probe["index"].as_u64().unwrap());
                let expected = corpus.table.values()[as_usize(&probe["index"])];
                assert_eq!(corpus.table.interpolate(&coordinates).unwrap(), expected);
                assert_bit_identical(corpus.table.interpolate_prepared(&plan).unwrap(), expected);
            }
        }
    }

    #[test]
    fn pytmatrix_kernel_requires_exact_generator_package_version_metadata() {
        // Metadata validation only; no table or physical scattering fixture is
        // created or represented as PyTMatrix output in this test.
        let generator = GeneratorMetadata::new(
            "research-generator",
            "1",
            "generate.py",
            "uncommitted-test-source",
            Some("3.11.9".to_owned()),
            BTreeMap::new(),
        )
        .unwrap();
        let science = ScienceMetadata::new(
            KernelModel::TMatrix {
                implementation: TMatrixImplementation::PyTMatrix033,
            },
            OrientationModel::ExplicitBodyFrame,
            MeltingModel::Dry,
            TemporalSampling::Instantaneous,
            TableValidation::ResearchOnlyUnvalidated,
        )
        .unwrap();
        assert!(matches!(
            validate_generator_science(&generator, &science),
            Err(LutError::PyTMatrixVersion { actual: None })
        ));

        let mut packages = BTreeMap::new();
        packages.insert("pytmatrix".to_owned(), "0.3.3".to_owned());
        let pinned = GeneratorMetadata::new(
            "research-generator",
            "1",
            "generate.py",
            "uncommitted-test-source",
            Some("3.11.9".to_owned()),
            packages,
        )
        .unwrap();
        validate_generator_science(&pinned, &science).unwrap();
    }
}
