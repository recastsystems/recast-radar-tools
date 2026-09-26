use std::collections::HashSet;

use thiserror::Error;

/// The kind of microphysics scheme a particle state comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MicrophysicsFamily {
    /// A conventional bulk scheme with fixed hydrometeor categories.
    Conventional,
    /// The Predicted Particle Properties (P3) scheme.
    P3,
    /// The ISHMAEL ice scheme.
    Ishmael,
}

/// A hydrometeor category of a conventional bulk scheme.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ConventionalHydrometeor {
    /// Cloud water.
    CloudWater,
    /// Rain.
    Rain,
    /// Cloud ice.
    CloudIce,
    /// Snow.
    Snow,
    /// Graupel.
    Graupel,
    /// Hail.
    Hail,
}

/// An ice category of the ISHMAEL scheme.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum IshmaelIceCategory {
    /// Small ice.
    SmallIce,
    /// Planar-nucleated ice.
    Planar,
    /// Columnar-nucleated ice.
    Columnar,
    /// Aggregates.
    Aggregate,
    /// Rimed ice.
    Rimed,
}

/// Thermodynamic values shared by normalized particle states.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParticleEnvironment {
    temperature_k: f64,
    air_density_kg_m3: f64,
}

impl ParticleEnvironment {
    /// An environment from a positive, finite temperature and air density.
    pub fn new(temperature_k: f64, air_density_kg_m3: f64) -> Result<Self, ParticleError> {
        positive("temperature", temperature_k)?;
        positive("air density", air_density_kg_m3)?;
        Ok(Self {
            temperature_k,
            air_density_kg_m3,
        })
    }

    /// Air temperature, K.
    #[must_use]
    pub const fn temperature_k(self) -> f64 {
        self.temperature_k
    }

    /// Air density, kg m⁻³.
    #[must_use]
    pub const fn air_density_kg_m3(self) -> f64 {
        self.air_density_kg_m3
    }
}

/// Single-particle geometry/property state used to query a kernel or LUT.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParticleShape {
    equivolume_diameter_m: f64,
    bulk_density_kg_m3: f64,
    minor_to_major_axis_ratio: f64,
    liquid_mass_fraction: f64,
}

impl ParticleShape {
    /// A shape from a positive diameter and bulk density, an axis ratio in (0, 1]
    /// and a liquid mass fraction in [0, 1].
    pub fn new(
        equivolume_diameter_m: f64,
        bulk_density_kg_m3: f64,
        minor_to_major_axis_ratio: f64,
        liquid_mass_fraction: f64,
    ) -> Result<Self, ParticleError> {
        positive("equivolume diameter", equivolume_diameter_m)?;
        positive("particle bulk density", bulk_density_kg_m3)?;
        fraction_exclusive_zero("minor-to-major axis ratio", minor_to_major_axis_ratio)?;
        fraction("liquid mass fraction", liquid_mass_fraction)?;
        Ok(Self {
            equivolume_diameter_m,
            bulk_density_kg_m3,
            minor_to_major_axis_ratio,
            liquid_mass_fraction,
        })
    }

    /// Diameter of the sphere of equal volume, m.
    #[must_use]
    pub const fn equivolume_diameter_m(self) -> f64 {
        self.equivolume_diameter_m
    }

    /// Bulk density of the particle, kg m⁻³.
    #[must_use]
    pub const fn bulk_density_kg_m3(self) -> f64 {
        self.bulk_density_kg_m3
    }

    /// Ratio of the minor to the major axis of the spheroid, (0, 1].
    #[must_use]
    pub const fn minor_to_major_axis_ratio(self) -> f64 {
        self.minor_to_major_axis_ratio
    }

    /// Fraction of the particle's mass that is liquid, [0, 1].
    #[must_use]
    pub const fn liquid_mass_fraction(self) -> f64 {
        self.liquid_mass_fraction
    }
}

/// A particle state of a conventional bulk scheme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConventionalParticleState {
    category: ConventionalHydrometeor,
    environment: ParticleEnvironment,
    shape: ParticleShape,
    mixing_ratio_kgkg: f64,
    number_per_kg: Option<f64>,
}

impl ConventionalParticleState {
    /// A state from a category, environment and shape, a positive mixing ratio and an optional positive number concentration.
    pub fn new(
        category: ConventionalHydrometeor,
        environment: ParticleEnvironment,
        shape: ParticleShape,
        mixing_ratio_kgkg: f64,
        number_per_kg: Option<f64>,
    ) -> Result<Self, ParticleError> {
        positive("mixing ratio", mixing_ratio_kgkg)?;
        optional_positive("number concentration per kg", number_per_kg)?;
        Ok(Self {
            category,
            environment,
            shape,
            mixing_ratio_kgkg,
            number_per_kg,
        })
    }

    /// The hydrometeor category.
    #[must_use]
    pub const fn category(self) -> ConventionalHydrometeor {
        self.category
    }

    /// The environment.
    #[must_use]
    pub const fn environment(self) -> ParticleEnvironment {
        self.environment
    }

    /// The particle shape.
    #[must_use]
    pub const fn shape(self) -> ParticleShape {
        self.shape
    }

    /// Mixing ratio, kg kg⁻¹.
    #[must_use]
    pub const fn mixing_ratio_kgkg(self) -> f64 {
        self.mixing_ratio_kgkg
    }

    /// Number concentration, kg⁻¹, for double-moment categories.
    #[must_use]
    pub const fn number_per_kg(self) -> Option<f64> {
        self.number_per_kg
    }
}

/// Property-aware P3 ice state. These normalized quantities are not inferred
/// from conventional snow/graupel category names.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct P3ParticleState {
    environment: ParticleEnvironment,
    shape: ParticleShape,
    total_ice_mixing_ratio_kgkg: f64,
    total_ice_number_per_kg: f64,
    rime_mass_fraction: f64,
    rime_density_kg_m3: f64,
}

impl P3ParticleState {
    /// A state from its environment, shape, total-ice mixing ratio and number, rime mass fraction and rime density.
    pub fn new(
        environment: ParticleEnvironment,
        shape: ParticleShape,
        total_ice_mixing_ratio_kgkg: f64,
        total_ice_number_per_kg: f64,
        rime_mass_fraction: f64,
        rime_density_kg_m3: f64,
    ) -> Result<Self, ParticleError> {
        positive("P3 total-ice mixing ratio", total_ice_mixing_ratio_kgkg)?;
        positive("P3 total-ice number concentration", total_ice_number_per_kg)?;
        fraction("P3 rime mass fraction", rime_mass_fraction)?;
        positive("P3 rime density", rime_density_kg_m3)?;
        Ok(Self {
            environment,
            shape,
            total_ice_mixing_ratio_kgkg,
            total_ice_number_per_kg,
            rime_mass_fraction,
            rime_density_kg_m3,
        })
    }

    /// The environment.
    #[must_use]
    pub const fn environment(self) -> ParticleEnvironment {
        self.environment
    }

    /// The particle shape.
    #[must_use]
    pub const fn shape(self) -> ParticleShape {
        self.shape
    }

    /// Total-ice mixing ratio, kg kg⁻¹.
    #[must_use]
    pub const fn total_ice_mixing_ratio_kgkg(self) -> f64 {
        self.total_ice_mixing_ratio_kgkg
    }

    /// Total-ice number concentration, kg⁻¹.
    #[must_use]
    pub const fn total_ice_number_per_kg(self) -> f64 {
        self.total_ice_number_per_kg
    }

    /// Fraction of the ice mass that is rime, [0, 1].
    #[must_use]
    pub const fn rime_mass_fraction(self) -> f64 {
        self.rime_mass_fraction
    }

    /// Density of the rime, kg m⁻³.
    #[must_use]
    pub const fn rime_density_kg_m3(self) -> f64 {
        self.rime_density_kg_m3
    }
}

/// Property-aware ISHMAEL ice state with its native category retained.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IshmaelParticleState {
    category: IshmaelIceCategory,
    environment: ParticleEnvironment,
    shape: ParticleShape,
    mixing_ratio_kgkg: f64,
    number_per_kg: f64,
    rime_mass_fraction: f64,
}

impl IshmaelParticleState {
    /// A state from its category, environment, shape, mixing ratio, number concentration and rime mass fraction.
    pub fn new(
        category: IshmaelIceCategory,
        environment: ParticleEnvironment,
        shape: ParticleShape,
        mixing_ratio_kgkg: f64,
        number_per_kg: f64,
        rime_mass_fraction: f64,
    ) -> Result<Self, ParticleError> {
        positive("ISHMAEL mixing ratio", mixing_ratio_kgkg)?;
        positive("ISHMAEL number concentration", number_per_kg)?;
        fraction("ISHMAEL rime mass fraction", rime_mass_fraction)?;
        Ok(Self {
            category,
            environment,
            shape,
            mixing_ratio_kgkg,
            number_per_kg,
            rime_mass_fraction,
        })
    }

    /// The ISHMAEL category.
    #[must_use]
    pub const fn category(self) -> IshmaelIceCategory {
        self.category
    }

    /// The environment.
    #[must_use]
    pub const fn environment(self) -> ParticleEnvironment {
        self.environment
    }

    /// The particle shape.
    #[must_use]
    pub const fn shape(self) -> ParticleShape {
        self.shape
    }

    /// Mixing ratio, kg kg⁻¹.
    #[must_use]
    pub const fn mixing_ratio_kgkg(self) -> f64 {
        self.mixing_ratio_kgkg
    }

    /// Number concentration, kg⁻¹.
    #[must_use]
    pub const fn number_per_kg(self) -> f64 {
        self.number_per_kg
    }

    /// Fraction of the ice mass that is rime, [0, 1].
    #[must_use]
    pub const fn rime_mass_fraction(self) -> f64 {
        self.rime_mass_fraction
    }
}

/// A particle state of any scheme family.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum ParticleState {
    /// A conventional bulk scheme state.
    Conventional(ConventionalParticleState),
    /// A P3 state.
    P3(P3ParticleState),
    /// An ISHMAEL state.
    Ishmael(IshmaelParticleState),
}

impl ParticleState {
    /// The scheme family of the state.
    #[must_use]
    pub const fn family(self) -> MicrophysicsFamily {
        match self {
            Self::Conventional(_) => MicrophysicsFamily::Conventional,
            Self::P3(_) => MicrophysicsFamily::P3,
            Self::Ishmael(_) => MicrophysicsFamily::Ishmael,
        }
    }
}

/// A model variable a particle state was derived from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceVariable {
    name: String,
    units: String,
}

impl SourceVariable {
    /// A variable from its name and units (neither empty).
    pub fn new(name: impl Into<String>, units: impl Into<String>) -> Result<Self, ProvenanceError> {
        let name = name.into();
        let units = units.into();
        required_text("source variable name", &name)?;
        required_text("source variable units", &units)?;
        Ok(Self { name, units })
    }

    /// Name of the variable (`QRAIN`, `QICE`, ...).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its units.
    #[must_use]
    pub fn units(&self) -> &str {
        &self.units
    }
}

/// A closure assumption made when the scheme's variables do not determine a particle state.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ClosureAssumption {
    /// The scheme's own closure: nothing assumed.
    SchemeNative,
    /// A fixed-intercept size distribution was assumed.
    FixedInterceptPsd {
        /// Identifier of the assumption.
        identifier: String,
    },
    /// A fixed number concentration was assumed.
    FixedNumberConcentration {
        /// Identifier of the assumption.
        identifier: String,
    },
    /// A particle shape was diagnosed.
    DiagnosedShape {
        /// Identifier of the assumption.
        identifier: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProvenanceCore {
    scheme_name: String,
    mapping_revision: String,
    source_variables: Vec<SourceVariable>,
    assumptions: Vec<ClosureAssumption>,
}

impl ProvenanceCore {
    fn new(
        scheme_name: impl Into<String>,
        mapping_revision: impl Into<String>,
        source_variables: Vec<SourceVariable>,
        assumptions: Vec<ClosureAssumption>,
    ) -> Result<Self, ProvenanceError> {
        let scheme_name = scheme_name.into();
        let mapping_revision = mapping_revision.into();
        required_text("scheme name", &scheme_name)?;
        required_text("mapping revision", &mapping_revision)?;
        if source_variables.is_empty() {
            return Err(ProvenanceError::NoSourceVariables);
        }
        let mut names = HashSet::new();
        for variable in &source_variables {
            let normalized = variable.name.to_ascii_uppercase();
            if !names.insert(normalized) {
                return Err(ProvenanceError::DuplicateSourceVariable {
                    name: variable.name.clone(),
                });
            }
        }
        for assumption in &assumptions {
            match assumption {
                ClosureAssumption::SchemeNative => {}
                ClosureAssumption::FixedInterceptPsd { identifier }
                | ClosureAssumption::FixedNumberConcentration { identifier }
                | ClosureAssumption::DiagnosedShape { identifier } => {
                    required_text("closure assumption identifier", identifier)?;
                }
            }
        }
        Ok(Self {
            scheme_name,
            mapping_revision,
            source_variables,
            assumptions,
        })
    }
}

macro_rules! provenance_type {
    ($(#[$meta:meta])* $name:ident, $extra_name:ident : $extra_type:ty) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub struct $name {
            core: ProvenanceCore,
            $extra_name: $extra_type,
        }

        impl $name {
            /// Name of the microphysics scheme.
            #[must_use]
            pub fn scheme_name(&self) -> &str {
                &self.core.scheme_name
            }

            /// Revision of the mapping from scheme variables to particle states.
            #[must_use]
            pub fn mapping_revision(&self) -> &str {
                &self.core.mapping_revision
            }

            /// The model variables the state was derived from.
            #[must_use]
            pub fn source_variables(&self) -> &[SourceVariable] {
                &self.core.source_variables
            }

            /// The closure assumptions made.
            #[must_use]
            pub fn assumptions(&self) -> &[ClosureAssumption] {
                &self.core.assumptions
            }
        }
    };
}

provenance_type!(
    /// Where a conventional bulk-scheme particle state came from: the scheme,
    /// the variable mapping, the source variables and the closure assumptions.
    ConventionalProvenance,
    wrf_mp_physics: Option<i32>
);
provenance_type!(
    /// Where a P3 particle state came from, with the P3 revision.
    P3Provenance,
    p3_revision: String
);
provenance_type!(
    /// Where an ISHMAEL particle state came from, with the ISHMAEL revision.
    IshmaelProvenance,
    ishmael_revision: String
);

impl ConventionalProvenance {
    /// Provenance from the scheme name, mapping revision, source variables (at least one, no duplicates), assumptions, and the WRF `mp_physics` id if known (positive).
    pub fn new(
        scheme_name: impl Into<String>,
        mapping_revision: impl Into<String>,
        source_variables: Vec<SourceVariable>,
        assumptions: Vec<ClosureAssumption>,
        wrf_mp_physics: Option<i32>,
    ) -> Result<Self, ProvenanceError> {
        if let Some(value) = wrf_mp_physics
            && value <= 0
        {
            return Err(ProvenanceError::InvalidSchemeId { value });
        }
        Ok(Self {
            core: ProvenanceCore::new(
                scheme_name,
                mapping_revision,
                source_variables,
                assumptions,
            )?,
            wrf_mp_physics,
        })
    }

    /// The WRF `mp_physics` id of the scheme, if known.
    #[must_use]
    pub const fn wrf_mp_physics(&self) -> Option<i32> {
        self.wrf_mp_physics
    }
}

impl P3Provenance {
    /// Provenance from the scheme name, mapping revision, source variables, assumptions and P3 revision.
    pub fn new(
        scheme_name: impl Into<String>,
        mapping_revision: impl Into<String>,
        source_variables: Vec<SourceVariable>,
        assumptions: Vec<ClosureAssumption>,
        p3_revision: impl Into<String>,
    ) -> Result<Self, ProvenanceError> {
        let p3_revision = p3_revision.into();
        required_text("P3 revision", &p3_revision)?;
        Ok(Self {
            core: ProvenanceCore::new(
                scheme_name,
                mapping_revision,
                source_variables,
                assumptions,
            )?,
            p3_revision,
        })
    }

    /// Revision of the P3 scheme.
    #[must_use]
    pub fn p3_revision(&self) -> &str {
        &self.p3_revision
    }
}

impl IshmaelProvenance {
    /// Provenance from the scheme name, mapping revision, source variables, assumptions and ISHMAEL revision.
    pub fn new(
        scheme_name: impl Into<String>,
        mapping_revision: impl Into<String>,
        source_variables: Vec<SourceVariable>,
        assumptions: Vec<ClosureAssumption>,
        ishmael_revision: impl Into<String>,
    ) -> Result<Self, ProvenanceError> {
        let ishmael_revision = ishmael_revision.into();
        required_text("ISHMAEL revision", &ishmael_revision)?;
        Ok(Self {
            core: ProvenanceCore::new(
                scheme_name,
                mapping_revision,
                source_variables,
                assumptions,
            )?,
            ishmael_revision,
        })
    }

    /// Revision of the ISHMAEL scheme.
    #[must_use]
    pub fn ishmael_revision(&self) -> &str {
        &self.ishmael_revision
    }
}

/// Provenance of a particle state of any scheme family.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ParticleProvenance {
    /// Conventional scheme provenance.
    Conventional(ConventionalProvenance),
    /// P3 provenance.
    P3(P3Provenance),
    /// ISHMAEL provenance.
    Ishmael(IshmaelProvenance),
}

impl ParticleProvenance {
    /// The scheme family of the provenance.
    #[must_use]
    pub const fn family(&self) -> MicrophysicsFamily {
        match self {
            Self::Conventional(_) => MicrophysicsFamily::Conventional,
            Self::P3(_) => MicrophysicsFamily::P3,
            Self::Ishmael(_) => MicrophysicsFamily::Ishmael,
        }
    }
}

/// A state/provenance pair that cannot cross-label P3, ISHMAEL, and
/// conventional category mappings.
#[derive(Clone, Debug, PartialEq)]
pub struct ParticleRecord {
    state: ParticleState,
    provenance: ParticleProvenance,
}

impl ParticleRecord {
    /// A record from a state and a provenance of the same scheme family.
    pub fn new(
        state: ParticleState,
        provenance: ParticleProvenance,
    ) -> Result<Self, ProvenanceError> {
        if state.family() != provenance.family() {
            return Err(ProvenanceError::FamilyMismatch {
                state: state.family(),
                provenance: provenance.family(),
            });
        }
        Ok(Self { state, provenance })
    }

    /// The particle state.
    #[must_use]
    pub const fn state(&self) -> ParticleState {
        self.state
    }

    /// Its provenance.
    #[must_use]
    pub const fn provenance(&self) -> &ParticleProvenance {
        &self.provenance
    }
}

fn finite(field: &'static str, value: f64) -> Result<(), ParticleError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(ParticleError::NonFinite { field, value })
    }
}

fn positive(field: &'static str, value: f64) -> Result<(), ParticleError> {
    finite(field, value)?;
    if value > 0.0 {
        Ok(())
    } else {
        Err(ParticleError::OutOfRange { field, value })
    }
}

fn optional_positive(field: &'static str, value: Option<f64>) -> Result<(), ParticleError> {
    if let Some(value) = value {
        positive(field, value)?;
    }
    Ok(())
}

fn fraction(field: &'static str, value: f64) -> Result<(), ParticleError> {
    finite(field, value)?;
    if (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(ParticleError::OutOfRange { field, value })
    }
}

fn fraction_exclusive_zero(field: &'static str, value: f64) -> Result<(), ParticleError> {
    finite(field, value)?;
    if (0.0..=1.0).contains(&value) && value > 0.0 {
        Ok(())
    } else {
        Err(ParticleError::OutOfRange { field, value })
    }
}

fn required_text(field: &'static str, value: &str) -> Result<(), ProvenanceError> {
    if value.trim().is_empty() {
        Err(ProvenanceError::EmptyText { field })
    } else {
        Ok(())
    }
}

/// Why a particle state value was rejected.
#[derive(Clone, Debug, Error, PartialEq)]
#[non_exhaustive]
pub enum ParticleError {
    /// A value is not finite.
    #[error("{field} must be finite, got {value}")]
    NonFinite {
        /// Which value.
        field: &'static str,
        /// The value.
        value: f64,
    },
    /// A value is outside its physical range.
    #[error("{field} is outside its valid physical range: {value}")]
    OutOfRange {
        /// Which value.
        field: &'static str,
        /// The value.
        value: f64,
    },
}

/// Why a provenance was rejected.
#[derive(Clone, Debug, Error, PartialEq)]
#[non_exhaustive]
pub enum ProvenanceError {
    /// A required text is empty.
    #[error("{field} must not be empty")]
    EmptyText {
        /// Which text.
        field: &'static str,
    },
    /// No source variable is named.
    #[error("particle provenance must name at least one source variable")]
    NoSourceVariables,
    /// A source variable is named twice (case-insensitively).
    #[error("source variable {name} appears more than once")]
    DuplicateSourceVariable {
        /// Its name.
        name: String,
    },
    /// The WRF `mp_physics` id is not positive.
    #[error("WRF microphysics scheme id must be positive, got {value}")]
    InvalidSchemeId {
        /// The id.
        value: i32,
    },
    /// The state and the provenance belong to different scheme families.
    #[error("particle state family {state:?} does not match provenance family {provenance:?}")]
    FamilyMismatch {
        /// Family of the state.
        state: MicrophysicsFamily,
        /// Family of the provenance.
        provenance: MicrophysicsFamily,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment() -> ParticleEnvironment {
        ParticleEnvironment::new(268.0, 1.0).unwrap()
    }

    fn shape() -> ParticleShape {
        ParticleShape::new(1.0e-3, 500.0, 0.8, 0.1).unwrap()
    }

    fn source(name: &str) -> SourceVariable {
        SourceVariable::new(name, "kg kg-1").unwrap()
    }

    fn number_source(name: &str) -> SourceVariable {
        SourceVariable::new(name, "kg-1").unwrap()
    }

    #[test]
    fn property_aware_states_reject_invalid_fractions() {
        assert!(matches!(
            P3ParticleState::new(environment(), shape(), 1.0e-4, 1.0e6, 1.1, 400.0),
            Err(ParticleError::OutOfRange {
                field: "P3 rime mass fraction",
                ..
            })
        ));
        assert!(matches!(
            ParticleShape::new(1.0e-3, 500.0, 0.0, 0.0),
            Err(ParticleError::OutOfRange {
                field: "minor-to-major axis ratio",
                ..
            })
        ));
    }

    #[test]
    fn state_cannot_be_cross_labeled_with_another_scheme_family() {
        let conventional = ConventionalParticleState::new(
            ConventionalHydrometeor::Snow,
            environment(),
            shape(),
            1.0e-4,
            None,
        )
        .unwrap();
        let p3_provenance = P3Provenance::new(
            "P3",
            "mapping-v1",
            vec![source("QICE")],
            vec![ClosureAssumption::SchemeNative],
            "P3-r7",
        )
        .unwrap();
        assert_eq!(
            ParticleRecord::new(
                ParticleState::Conventional(conventional),
                ParticleProvenance::P3(p3_provenance),
            )
            .unwrap_err(),
            ProvenanceError::FamilyMismatch {
                state: MicrophysicsFamily::Conventional,
                provenance: MicrophysicsFamily::P3,
            }
        );
    }

    #[test]
    fn provenance_requires_unique_auditable_source_fields() {
        let duplicate = ConventionalProvenance::new(
            "Morrison",
            "mapping-v1",
            vec![source("QRAIN"), source("qrain")],
            vec![ClosureAssumption::SchemeNative],
            Some(10),
        )
        .unwrap_err();
        assert_eq!(
            duplicate,
            ProvenanceError::DuplicateSourceVariable {
                name: "qrain".to_owned(),
            }
        );
    }

    #[test]
    fn each_microphysics_family_retains_its_native_state_and_provenance() {
        let conventional = ParticleRecord::new(
            ParticleState::Conventional(
                ConventionalParticleState::new(
                    ConventionalHydrometeor::Rain,
                    environment(),
                    shape(),
                    1.0e-4,
                    Some(2.0e6),
                )
                .unwrap(),
            ),
            ParticleProvenance::Conventional(
                ConventionalProvenance::new(
                    "Morrison",
                    "mapping-v1",
                    vec![source("QRAIN"), number_source("QNRAIN")],
                    vec![ClosureAssumption::SchemeNative],
                    Some(10),
                )
                .unwrap(),
            ),
        )
        .unwrap();
        assert_eq!(
            conventional.state().family(),
            MicrophysicsFamily::Conventional
        );

        let p3 = ParticleRecord::new(
            ParticleState::P3(
                P3ParticleState::new(environment(), shape(), 1.0e-4, 1.0e6, 0.4, 500.0).unwrap(),
            ),
            ParticleProvenance::P3(
                P3Provenance::new(
                    "P3",
                    "mapping-v1",
                    vec![source("QICE"), number_source("QNI")],
                    vec![ClosureAssumption::SchemeNative],
                    "P3-r7",
                )
                .unwrap(),
            ),
        )
        .unwrap();
        assert_eq!(p3.state().family(), MicrophysicsFamily::P3);

        let ishmael = ParticleRecord::new(
            ParticleState::Ishmael(
                IshmaelParticleState::new(
                    IshmaelIceCategory::Aggregate,
                    environment(),
                    shape(),
                    1.0e-4,
                    1.0e6,
                    0.2,
                )
                .unwrap(),
            ),
            ParticleProvenance::Ishmael(
                IshmaelProvenance::new(
                    "Jensen ISHMAEL",
                    "mapping-v1",
                    vec![source("QICE_AGG"), number_source("QNICE_AGG")],
                    vec![ClosureAssumption::SchemeNative],
                    "ISHMAEL-r1",
                )
                .unwrap(),
            ),
        )
        .unwrap();
        assert_eq!(ishmael.state().family(), MicrophysicsFamily::Ishmael);
    }
}
