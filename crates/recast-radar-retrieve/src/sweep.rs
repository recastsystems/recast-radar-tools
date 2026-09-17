//! Sweep-local radar product derivation.
//!
//! Products are dataset variables of one [`Sweep`], named after the design
//! note's rule (`docs/design/fm301-model.md` section 8.3): standalone
//! quantities keep a short id (`KDP`, `RR`, `AH`, `MET_QI`, ...); products
//! derived from one input field are `<BASE>_<SUFFIX>` with `BASE` the input
//! field's own name (`DBZH_TEX`, `DBZ_CORR`, `PHIDP_CLEAN`, `RHOHV_LOG`).
//! Inputs are found by [`Quantity`] ([`Sweep::find`]), so a sweep decoded
//! from any source format works regardless of its spelling of reflectivity.

use std::borrow::Cow;
use std::collections::BTreeSet;

use recast_radar_core::{
    Field, FieldAttrs, FieldData, FieldName, FloatCoding, Polarization, Quantity, Sweep, Volume,
};

/// Products that can be computed independently for each elevation cut.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DerivedSweepProduct {
    Kdp,
    FilteredDifferentialPhase,
    KdpUncertainty,
    SpecificAttenuation,
    PathIntegratedAttenuation,
    CorrectedReflectivity,
    SpecificDifferentialAttenuation,
    PathIntegratedDifferentialAttenuation,
    CorrectedDifferentialReflectivity,
    RainRateReflectivity,
    RainRateKdp,
    RainRateHybrid,
    LiquidWaterContent,
    HailKineticEnergy,
    CircularDepolarizationRatio,
    LogCorrelationRatio,
    ReflectivityTexture,
    VelocityTexture,
    SpectrumWidthTexture,
    DifferentialReflectivityTexture,
    CorrelationCoefficientTexture,
    DifferentialPhaseTexture,
    KdpTexture,
    ReflectivityRangeGradient,
    VelocityRangeGradient,
    MeteorologicalQuality,
    MeteorologicalGateMask,
    TdsConfidence,
    HailSignature,
    TurbulenceProxy,
}

impl DerivedSweepProduct {
    pub const ALL: &'static [Self] = &[
        Self::Kdp,
        Self::FilteredDifferentialPhase,
        Self::KdpUncertainty,
        Self::SpecificAttenuation,
        Self::PathIntegratedAttenuation,
        Self::CorrectedReflectivity,
        Self::SpecificDifferentialAttenuation,
        Self::PathIntegratedDifferentialAttenuation,
        Self::CorrectedDifferentialReflectivity,
        Self::RainRateReflectivity,
        Self::RainRateKdp,
        Self::RainRateHybrid,
        Self::LiquidWaterContent,
        Self::HailKineticEnergy,
        Self::CircularDepolarizationRatio,
        Self::LogCorrelationRatio,
        Self::ReflectivityTexture,
        Self::VelocityTexture,
        Self::SpectrumWidthTexture,
        Self::DifferentialReflectivityTexture,
        Self::CorrelationCoefficientTexture,
        Self::DifferentialPhaseTexture,
        Self::KdpTexture,
        Self::ReflectivityRangeGradient,
        Self::VelocityRangeGradient,
        Self::MeteorologicalQuality,
        Self::MeteorologicalGateMask,
        Self::TdsConfidence,
        Self::HailSignature,
        Self::TurbulenceProxy,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Kdp => "KDP",
            Self::FilteredDifferentialPhase => "PHIF",
            Self::KdpUncertainty => "KDP_SD",
            Self::SpecificAttenuation => "AH",
            Self::PathIntegratedAttenuation => "PIA",
            Self::CorrectedReflectivity => "REFC",
            Self::SpecificDifferentialAttenuation => "ADP",
            Self::PathIntegratedDifferentialAttenuation => "PIDA",
            Self::CorrectedDifferentialReflectivity => "ZDRC",
            Self::RainRateReflectivity => "RATE_Z",
            Self::RainRateKdp => "RATE_KDP",
            Self::RainRateHybrid => "RATE",
            Self::LiquidWaterContent => "LWC",
            Self::HailKineticEnergy => "HKE",
            Self::CircularDepolarizationRatio => "CDR",
            Self::LogCorrelationRatio => "L_RHO",
            Self::ReflectivityTexture => "REF_TEX",
            Self::VelocityTexture => "VEL_TEX",
            Self::SpectrumWidthTexture => "SW_TEX",
            Self::DifferentialReflectivityTexture => "ZDR_TEX",
            Self::CorrelationCoefficientTexture => "RHO_TEX",
            Self::DifferentialPhaseTexture => "PHI_TEX",
            Self::KdpTexture => "KDP_TEX",
            Self::ReflectivityRangeGradient => "REF_GRAD_R",
            Self::VelocityRangeGradient => "VEL_GRAD_R",
            Self::MeteorologicalQuality => "MET_QI",
            Self::MeteorologicalGateMask => "MET_MASK",
            Self::TdsConfidence => "TDS_SCORE",
            Self::HailSignature => "HAIL_SCORE",
            Self::TurbulenceProxy => "TURB",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Kdp => "Specific Differential Phase",
            Self::FilteredDifferentialPhase => "Filtered Differential Phase",
            Self::KdpUncertainty => "KDP Uncertainty",
            Self::SpecificAttenuation => "Specific Attenuation",
            Self::PathIntegratedAttenuation => "Path Integrated Attenuation",
            Self::CorrectedReflectivity => "Attenuation-Corrected Reflectivity",
            Self::SpecificDifferentialAttenuation => "Specific Differential Attenuation",
            Self::PathIntegratedDifferentialAttenuation => {
                "Path Integrated Differential Attenuation"
            }
            Self::CorrectedDifferentialReflectivity => {
                "Attenuation-Corrected Differential Reflectivity"
            }
            Self::RainRateReflectivity => "Rain Rate from Reflectivity",
            Self::RainRateKdp => "Rain Rate from KDP",
            Self::RainRateHybrid => "Hybrid Polarimetric Rain Rate",
            Self::LiquidWaterContent => "Radar Liquid-Water-Content Proxy",
            Self::HailKineticEnergy => "Hail Kinetic-Energy Flux",
            Self::CircularDepolarizationRatio => "Circular Depolarization Ratio",
            Self::LogCorrelationRatio => "Logarithmic Correlation Ratio",
            Self::ReflectivityTexture => "Reflectivity Texture",
            Self::VelocityTexture => "Velocity Texture",
            Self::SpectrumWidthTexture => "Spectrum-Width Texture",
            Self::DifferentialReflectivityTexture => "Differential-Reflectivity Texture",
            Self::CorrelationCoefficientTexture => "Correlation-Coefficient Texture",
            Self::DifferentialPhaseTexture => "Differential-Phase Texture",
            Self::KdpTexture => "KDP Texture",
            Self::ReflectivityRangeGradient => "Reflectivity Range Gradient",
            Self::VelocityRangeGradient => "Radial-Velocity Range Gradient",
            Self::MeteorologicalQuality => "Meteorological Gate Quality",
            Self::MeteorologicalGateMask => "Meteorological Gate Mask",
            Self::TdsConfidence => "Tornadic-Debris-Signature Diagnostic",
            Self::HailSignature => "Dual-Pol Hail-Signature Diagnostic",
            Self::TurbulenceProxy => "Doppler Turbulence Proxy",
        }
    }

    pub const fn units(self) -> &'static str {
        match self {
            Self::Kdp => "deg/km",
            Self::FilteredDifferentialPhase => "deg",
            Self::KdpUncertainty => "deg/km",
            Self::SpecificAttenuation => "dB/km",
            Self::PathIntegratedAttenuation => "dB",
            Self::CorrectedReflectivity => "dBZ",
            Self::SpecificDifferentialAttenuation => "dB/km",
            Self::PathIntegratedDifferentialAttenuation => "dB",
            Self::CorrectedDifferentialReflectivity => "dB",
            Self::RainRateReflectivity | Self::RainRateKdp | Self::RainRateHybrid => "mm/h",
            Self::LiquidWaterContent => "g/m^3",
            Self::HailKineticEnergy => "J/(m^2 s)",
            Self::CircularDepolarizationRatio => "dB",
            Self::LogCorrelationRatio => "1",
            Self::ReflectivityTexture => "dB",
            Self::VelocityTexture | Self::SpectrumWidthTexture | Self::TurbulenceProxy => "m/s",
            Self::DifferentialReflectivityTexture => "dB",
            Self::CorrelationCoefficientTexture => "1",
            Self::DifferentialPhaseTexture => "deg",
            Self::KdpTexture => "deg/km",
            Self::ReflectivityRangeGradient => "dBZ/km",
            Self::VelocityRangeGradient => "10^-3/s",
            Self::MeteorologicalQuality | Self::MeteorologicalGateMask => "1",
            Self::TdsConfidence | Self::HailSignature => "%",
        }
    }

    /// Products whose calibration or physical bounds change with transmit
    /// wavelength. These must fail closed when the radar band is unknown;
    /// silently applying the historical S-band default produces plausible
    /// but scientifically incorrect QPE and attenuation fields.
    pub const fn requires_known_radar_band(self) -> bool {
        matches!(
            self,
            Self::Kdp
                | Self::KdpUncertainty
                | Self::SpecificAttenuation
                | Self::PathIntegratedAttenuation
                | Self::CorrectedReflectivity
                | Self::SpecificDifferentialAttenuation
                | Self::PathIntegratedDifferentialAttenuation
                | Self::CorrectedDifferentialReflectivity
                | Self::RainRateKdp
                | Self::RainRateHybrid
                | Self::KdpTexture
        )
    }
}

impl DerivedSweepProduct {
    /// The input quantity whose field name is `BASE` in the product's
    /// `<BASE>_<SUFFIX>` name; `None` for products with a fixed id.
    pub fn base_quantity(self) -> Option<Quantity> {
        Some(match self {
            Self::ReflectivityTexture
            | Self::ReflectivityRangeGradient
            | Self::CorrectedReflectivity => Quantity::Reflectivity,
            Self::VelocityTexture | Self::VelocityRangeGradient => Quantity::RadialVelocity,
            Self::SpectrumWidthTexture => Quantity::SpectrumWidth,
            Self::DifferentialReflectivityTexture | Self::CorrectedDifferentialReflectivity => {
                Quantity::DifferentialReflectivity
            }
            Self::CorrelationCoefficientTexture | Self::LogCorrelationRatio => {
                Quantity::CorrelationCoefficient
            }
            Self::DifferentialPhaseTexture | Self::FilteredDifferentialPhase => {
                Quantity::DifferentialPhase
            }
            Self::KdpTexture => Quantity::SpecificDifferentialPhase,
            _ => return None,
        })
    }

    /// The `<SUFFIX>` of a base-named product.
    const fn suffix(self) -> Option<&'static str> {
        Some(match self {
            Self::ReflectivityTexture
            | Self::VelocityTexture
            | Self::SpectrumWidthTexture
            | Self::DifferentialReflectivityTexture
            | Self::CorrelationCoefficientTexture
            | Self::DifferentialPhaseTexture
            | Self::KdpTexture => "_TEX",
            Self::ReflectivityRangeGradient | Self::VelocityRangeGradient => "_GRAD_R",
            Self::CorrectedReflectivity | Self::CorrectedDifferentialReflectivity => "_CORR",
            Self::FilteredDifferentialPhase => "_CLEAN",
            Self::LogCorrelationRatio => "_LOG",
            _ => return None,
        })
    }

    /// The fixed FM301 / xradar id of a product without a base
    /// (design note 8.3), or the id used with the canonical base name.
    const fn fixed_id(self) -> Option<&'static str> {
        Some(match self {
            Self::Kdp => "KDP",
            Self::KdpUncertainty => "KDP_SD",
            Self::SpecificAttenuation => "AH",
            Self::PathIntegratedAttenuation => "PIA",
            Self::SpecificDifferentialAttenuation => "ADP",
            Self::PathIntegratedDifferentialAttenuation => "PIDA",
            Self::RainRateReflectivity => "RR_Z",
            Self::RainRateKdp => "RR_KDP",
            Self::RainRateHybrid => "RR",
            Self::LiquidWaterContent => "LWC",
            Self::HailKineticEnergy => "HKE",
            Self::CircularDepolarizationRatio => "CDR",
            Self::MeteorologicalQuality => "MET_QI",
            Self::MeteorologicalGateMask => "MET_MASK",
            Self::TdsConfidence => "TDS_SCORE",
            Self::HailSignature => "HAIL_SCORE",
            Self::TurbulenceProxy => "TURB",
            _ => return None,
        })
    }

    /// The canonical FM301 name of a base quantity, used when the sweep has
    /// no field of that quantity (the product is then unavailable anyway).
    fn canonical_base(quantity: Quantity) -> FieldName {
        match quantity {
            Quantity::Reflectivity => FieldName::Dbzh,
            Quantity::RadialVelocity => FieldName::Vradh,
            Quantity::SpectrumWidth => FieldName::Wradh,
            Quantity::DifferentialReflectivity => FieldName::Zdr,
            Quantity::CorrelationCoefficient => FieldName::Rhohv,
            Quantity::DifferentialPhase => FieldName::Phidp,
            Quantity::SpecificDifferentialPhase => FieldName::Kdp,
            _ => FieldName::Other("X".into()),
        }
    }

    /// The product's dataset variable name given its input field's name
    /// (design note 8.3). `base` is ignored by products with a fixed id;
    /// `None` uses the canonical name of the base quantity.
    pub fn field_name(self, base: Option<&FieldName>) -> FieldName {
        match (self.suffix(), self.base_quantity(), self.fixed_id()) {
            (Some(suffix), Some(quantity), _) => {
                let base = base
                    .cloned()
                    .unwrap_or_else(|| Self::canonical_base(quantity));
                FieldName::parse(&format!("{}{suffix}", base.as_str()))
            }
            (_, _, Some(id)) => FieldName::parse(id),
            _ => FieldName::parse(self.id()),
        }
    }

    /// The name this product gets in `sweep`: its base field is the sweep's
    /// preferred field of [`DerivedSweepProduct::base_quantity`].
    pub fn field_name_in(self, sweep: &Sweep) -> FieldName {
        let base = self
            .base_quantity()
            .and_then(|quantity| sweep.find(quantity))
            .map(|field| field.name.clone());
        self.field_name(base.as_ref())
    }

    /// The name with the canonical base names (`DBZH_TEX`, `DBZH_CORR`, ...).
    pub fn canonical_field_name(self) -> FieldName {
        self.field_name(None)
    }

    /// The product a dataset variable name refers to, if any: fixed ids, and
    /// `<BASE>_<SUFFIX>` names whose base classifies as the product's input
    /// quantity. Case-insensitive. `KDP` is never a derived product here: a
    /// field of that name is native specific differential phase.
    pub fn for_name(name: &FieldName) -> Option<Self> {
        let text = name.as_str();
        if let Some(product) = Self::ALL
            .iter()
            .copied()
            .filter(|product| *product != Self::Kdp)
            .find(|product| {
                product
                    .fixed_id()
                    .is_some_and(|id| id.eq_ignore_ascii_case(text))
            })
        {
            return Some(product);
        }
        Self::ALL.iter().copied().find(|product| {
            let (Some(suffix), Some(quantity)) = (product.suffix(), product.base_quantity()) else {
                return false;
            };
            let Some(base) = text
                .len()
                .checked_sub(suffix.len())
                .and_then(|split| text.is_char_boundary(split).then(|| text.split_at(split)))
                .filter(|(_, tail)| tail.eq_ignore_ascii_case(suffix))
                .map(|(base, _)| base)
            else {
                return false;
            };
            !base.is_empty() && Quantity::classify(base, None).0 == quantity
        })
    }

    /// Semantic class of the product's output.
    fn quantity(self) -> Quantity {
        match self {
            Self::Kdp => Quantity::SpecificDifferentialPhase,
            Self::RainRateReflectivity | Self::RainRateKdp | Self::RainRateHybrid => {
                Quantity::PrecipitationRate
            }
            _ => Quantity::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadarBand {
    /// No trustworthy transmit frequency, network classification, or
    /// site-specific metadata is available.
    Unknown,
    S,
    C,
    X,
}

impl RadarBand {
    pub const fn is_known(self) -> bool {
        !matches!(self, Self::Unknown)
    }

    fn rain_kdp_coefficients(self) -> (f32, f32) {
        match self {
            Self::Unknown => (f32::NAN, f32::NAN),
            Self::S => (50.70, 0.8500),
            Self::C => (29.70, 0.8500),
            Self::X => (15.81, 0.7992),
        }
    }

    fn attenuation_coefficients(self) -> (f32, f32) {
        // PHIDP-linear coefficients used by Py-ART's band table. The first
        // coefficient corrects horizontal reflectivity; the second corrects
        // differential reflectivity.
        match self {
            Self::Unknown => (f32::NAN, f32::NAN),
            Self::S => (0.04, 0.004),
            Self::C => (0.08, 0.03),
            Self::X => (0.28, 0.04),
        }
    }

    fn default_kdp_bounds(self) -> (f32, f32) {
        match self {
            // PHIF is band-independent and shares the phase-bundle pass.
            // Broad bounds let that pass run while band-sensitive outputs
            // are filtered by `derive_cut_in_place` below.
            Self::Unknown => (f32::NEG_INFINITY, f32::INFINITY),
            Self::S => (-2.0, 14.0),
            Self::C => (-2.0, 20.0),
            Self::X => (-2.0, 40.0),
        }
    }
}

#[derive(Clone, Debug)]
pub struct KdpConfig {
    /// Robust regression window length along a radial.
    pub window_km: f32,
    pub min_window_gates: usize,
    pub max_window_gates: usize,
    pub min_valid_gates: usize,
    pub max_interpolated_gap_gates: usize,
    pub phase_period_deg: f32,
    pub min_rho_hv: f32,
    pub min_reflectivity_dbz: f32,
    pub hampel_half_window: usize,
    pub hampel_sigma: f32,
    pub huber_k: f32,
    pub kdp_min_deg_km: f32,
    pub kdp_max_deg_km: f32,
    /// Emit estimates at short, internally interpolated PHIDP gaps. Keeping
    /// this false is conservative and prevents interpolation from inventing
    /// visible precipitation.
    pub emit_interpolated_gates: bool,
    /// Number of near-range valid gates used to estimate the system-phase
    /// baseline for attenuation products.
    pub phase_baseline_gates: usize,
}

impl KdpConfig {
    pub fn for_band(band: RadarBand) -> Self {
        let (kdp_min_deg_km, kdp_max_deg_km) = band.default_kdp_bounds();
        Self {
            window_km: 3.0,
            min_window_gates: 7,
            max_window_gates: 41,
            min_valid_gates: 5,
            max_interpolated_gap_gates: 2,
            phase_period_deg: 360.0,
            min_rho_hv: 0.80,
            min_reflectivity_dbz: -10.0,
            hampel_half_window: 3,
            hampel_sigma: 3.0,
            huber_k: 1.5,
            kdp_min_deg_km,
            kdp_max_deg_km,
            emit_interpolated_gates: false,
            phase_baseline_gates: 20,
        }
    }
}

#[derive(Clone, Debug)]
pub struct QpeConfig {
    /// Marshall-Palmer/NWS-style Z=a R^b relationship.
    pub z_r_a: f32,
    pub z_r_b: f32,
    pub kdp_alpha: f32,
    pub kdp_beta: f32,
    pub hybrid_min_kdp_deg_km: f32,
    pub hybrid_min_reflectivity_dbz: f32,
    pub hybrid_min_rho_hv: f32,
    pub max_rate_mm_h: f32,
}

impl QpeConfig {
    pub fn for_band(band: RadarBand) -> Self {
        let (kdp_alpha, kdp_beta) = band.rain_kdp_coefficients();
        Self {
            z_r_a: 300.0,
            z_r_b: 1.4,
            kdp_alpha,
            kdp_beta,
            hybrid_min_kdp_deg_km: 0.30,
            hybrid_min_reflectivity_dbz: 35.0,
            hybrid_min_rho_hv: 0.90,
            max_rate_mm_h: 300.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AttenuationConfig {
    pub horizontal_db_per_degree: f32,
    pub differential_db_per_degree: f32,
    pub max_pia_db: f32,
    pub max_pida_db: f32,
}

impl AttenuationConfig {
    pub fn for_band(band: RadarBand) -> Self {
        let (horizontal_db_per_degree, differential_db_per_degree) =
            band.attenuation_coefficients();
        Self {
            horizontal_db_per_degree,
            differential_db_per_degree,
            max_pia_db: 20.0,
            max_pida_db: 10.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TextureConfig {
    pub radial_radius: usize,
    pub gate_radius: usize,
    pub min_samples: usize,
}

impl Default for TextureConfig {
    fn default() -> Self {
        Self {
            radial_radius: 1,
            gate_radius: 2,
            min_samples: 5,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MeteoMaskConfig {
    pub minimum_quality: f32,
    pub minimum_reflectivity_dbz: f32,
}

impl Default for MeteoMaskConfig {
    fn default() -> Self {
        Self {
            minimum_quality: 0.50,
            minimum_reflectivity_dbz: -10.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DiagnosticConfig {
    pub tds_min_reflectivity_dbz: f32,
    pub tds_rho_ceiling: f32,
    pub hail_min_reflectivity_dbz: f32,
}

impl Default for DiagnosticConfig {
    fn default() -> Self {
        Self {
            tds_min_reflectivity_dbz: 20.0,
            tds_rho_ceiling: 0.95,
            hail_min_reflectivity_dbz: 45.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DerivationConfig {
    pub products: BTreeSet<DerivedSweepProduct>,
    pub overwrite_existing: bool,
    pub band: RadarBand,
    pub kdp: KdpConfig,
    pub qpe: QpeConfig,
    pub attenuation: AttenuationConfig,
    pub texture: TextureConfig,
    pub meteo_mask: MeteoMaskConfig,
    pub diagnostics: DiagnosticConfig,
}

impl DerivationConfig {
    pub fn kdp_only() -> Self {
        Self::with_products(RadarBand::S, [DerivedSweepProduct::Kdp])
    }

    pub fn analyst_defaults() -> Self {
        Self::with_products(
            RadarBand::S,
            [
                DerivedSweepProduct::Kdp,
                DerivedSweepProduct::FilteredDifferentialPhase,
                DerivedSweepProduct::KdpUncertainty,
                DerivedSweepProduct::RainRateHybrid,
                DerivedSweepProduct::MeteorologicalQuality,
            ],
        )
    }

    pub fn all_supported() -> Self {
        Self::with_products(RadarBand::S, DerivedSweepProduct::ALL.iter().copied())
    }

    pub fn with_products(
        band: RadarBand,
        products: impl IntoIterator<Item = DerivedSweepProduct>,
    ) -> Self {
        Self {
            products: products.into_iter().collect(),
            overwrite_existing: false,
            band,
            kdp: KdpConfig::for_band(band),
            qpe: QpeConfig::for_band(band),
            attenuation: AttenuationConfig::for_band(band),
            texture: TextureConfig::default(),
            meteo_mask: MeteoMaskConfig::default(),
            diagnostics: DiagnosticConfig::default(),
        }
    }

    pub fn set_band(&mut self, band: RadarBand) {
        self.band = band;
        self.kdp = KdpConfig::for_band(band);
        self.qpe = QpeConfig::for_band(band);
        self.attenuation = AttenuationConfig::for_band(band);
    }
}

impl Default for DerivationConfig {
    fn default() -> Self {
        Self::analyst_defaults()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SweepDerivationReport {
    pub inserted: Vec<String>,
    pub skipped_existing: Vec<String>,
    pub unavailable: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DerivationReport {
    pub sweeps_processed: usize,
    pub inserted: Vec<(usize, String)>,
    pub skipped_existing: Vec<(usize, String)>,
    pub unavailable: Vec<(usize, String)>,
}

/// Derive configured products on every sweep, preserving native fields
/// unless `overwrite_existing` is enabled.
pub fn derive_volume_in_place(volume: &mut Volume, config: &DerivationConfig) -> DerivationReport {
    let mut report = DerivationReport::default();
    for (sweep_index, sweep) in volume.sweeps.iter_mut().enumerate() {
        let sweep_report = derive_sweep_in_place(sweep, config);
        report.sweeps_processed += 1;
        report.inserted.extend(
            sweep_report
                .inserted
                .into_iter()
                .map(|id| (sweep_index, id)),
        );
        report.skipped_existing.extend(
            sweep_report
                .skipped_existing
                .into_iter()
                .map(|id| (sweep_index, id)),
        );
        report.unavailable.extend(
            sweep_report
                .unavailable
                .into_iter()
                .map(|id| (sweep_index, id)),
        );
    }
    report
}

/// The input fields of a sweep, resolved once per derivation
/// ([`Sweep::find`] by quantity).
struct Inputs<'a> {
    reflectivity: Option<&'a Field>,
    velocity: Option<&'a Field>,
    spectrum_width: Option<&'a Field>,
    zdr: Option<&'a Field>,
    rho: Option<&'a Field>,
    phi: Option<&'a Field>,
    kdp: Option<&'a Field>,
}

impl<'a> Inputs<'a> {
    fn of(sweep: &'a Sweep) -> Self {
        Self {
            reflectivity: sweep.find(Quantity::Reflectivity),
            velocity: sweep.find(Quantity::RadialVelocity),
            spectrum_width: sweep.find(Quantity::SpectrumWidth),
            zdr: sweep.find(Quantity::DifferentialReflectivity),
            rho: sweep.find(Quantity::CorrelationCoefficient),
            phi: sweep.find(Quantity::DifferentialPhase),
            kdp: sweep.find(Quantity::SpecificDifferentialPhase),
        }
    }
}

/// Derive configured products for one sweep and add them to its fields.
///
/// The function computes against an immutable snapshot and commits all
/// results at the end, so dependencies do not observe a half-mutated sweep.
/// Report entries carry product ids ([`DerivedSweepProduct::id`]), not
/// field names.
pub fn derive_sweep_in_place(
    sweep: &mut Sweep,
    config: &DerivationConfig,
) -> SweepDerivationReport {
    let mut report = SweepDerivationReport::default();
    let mut allowed_products = config.products.clone();
    if !config.band.is_known() {
        for product in config
            .products
            .iter()
            .copied()
            .filter(|product| product.requires_known_radar_band())
        {
            allowed_products.remove(&product);
            report.unavailable.push(product.id().to_owned());
        }
    }
    let requested = &allowed_products;
    if requested.is_empty() {
        return report;
    }

    let phase_needed = requested.iter().any(|product| {
        matches!(
            product,
            DerivedSweepProduct::Kdp
                | DerivedSweepProduct::FilteredDifferentialPhase
                | DerivedSweepProduct::KdpUncertainty
                | DerivedSweepProduct::SpecificAttenuation
                | DerivedSweepProduct::PathIntegratedAttenuation
                | DerivedSweepProduct::CorrectedReflectivity
                | DerivedSweepProduct::SpecificDifferentialAttenuation
                | DerivedSweepProduct::PathIntegratedDifferentialAttenuation
                | DerivedSweepProduct::CorrectedDifferentialReflectivity
                | DerivedSweepProduct::RainRateKdp
                | DerivedSweepProduct::RainRateHybrid
                | DerivedSweepProduct::KdpTexture
        )
    });

    let snapshot: &Sweep = sweep;
    let inputs = Inputs::of(snapshot);
    let phase_bundle = if phase_needed {
        derive_phase_bundle(snapshot, &inputs, &config.kdp)
    } else {
        None
    };

    // Prefer a source-provided KDP field for downstream products. If no native
    // KDP is present, use the robust PHIDP retrieval from this pass.
    let derived_kdp = phase_bundle.as_ref().map(|bundle| &bundle.kdp);
    let kdp_for_dependencies = inputs.kdp.or(derived_kdp);

    let phase_excess = phase_bundle.as_ref().map(|bundle| {
        phase_excess_field(
            snapshot,
            &bundle.filtered_phi,
            config.kdp.phase_baseline_gates,
        )
    });

    let pia = if needs_any(
        requested,
        &[
            DerivedSweepProduct::PathIntegratedAttenuation,
            DerivedSweepProduct::CorrectedReflectivity,
        ],
    ) {
        let product = DerivedSweepProduct::PathIntegratedAttenuation;
        phase_excess.as_ref().map_or_else(
            || {
                kdp_for_dependencies.map(|kdp| {
                    integrate_kdp(
                        snapshot,
                        kdp,
                        product,
                        config.attenuation.horizontal_db_per_degree,
                        config.attenuation.max_pia_db,
                    )
                })
            },
            |phase| {
                Some(scale_positive_field(
                    phase,
                    product,
                    config.attenuation.horizontal_db_per_degree,
                    config.attenuation.max_pia_db,
                ))
            },
        )
    } else {
        None
    };

    let pida = if needs_any(
        requested,
        &[
            DerivedSweepProduct::PathIntegratedDifferentialAttenuation,
            DerivedSweepProduct::CorrectedDifferentialReflectivity,
        ],
    ) {
        let product = DerivedSweepProduct::PathIntegratedDifferentialAttenuation;
        phase_excess.as_ref().map_or_else(
            || {
                kdp_for_dependencies.map(|kdp| {
                    integrate_kdp(
                        snapshot,
                        kdp,
                        product,
                        config.attenuation.differential_db_per_degree,
                        config.attenuation.max_pida_db,
                    )
                })
            },
            |phase| {
                Some(scale_positive_field(
                    phase,
                    product,
                    config.attenuation.differential_db_per_degree,
                    config.attenuation.max_pida_db,
                ))
            },
        )
    } else {
        None
    };

    let mut pending: Vec<Field> = Vec::new();

    for &product in DerivedSweepProduct::ALL {
        if !requested.contains(&product) {
            continue;
        }
        let output_name = product.field_name_in(snapshot);
        if snapshot.field(&output_name).is_some() && !config.overwrite_existing {
            report.skipped_existing.push(product.id().to_owned());
            continue;
        }

        let field = match product {
            DerivedSweepProduct::Kdp => phase_bundle.as_ref().map(|bundle| bundle.kdp.clone()),
            DerivedSweepProduct::FilteredDifferentialPhase => phase_bundle
                .as_ref()
                .map(|bundle| bundle.filtered_phi.clone()),
            DerivedSweepProduct::KdpUncertainty => phase_bundle
                .as_ref()
                .map(|bundle| bundle.uncertainty.clone()),
            DerivedSweepProduct::SpecificAttenuation => kdp_for_dependencies.map(|kdp| {
                scale_positive_field(
                    kdp,
                    product,
                    config.attenuation.horizontal_db_per_degree,
                    f32::INFINITY,
                )
            }),
            DerivedSweepProduct::PathIntegratedAttenuation => pia.clone(),
            DerivedSweepProduct::CorrectedReflectivity => {
                inputs.reflectivity.and_then(|reflectivity| {
                    pia.as_ref().map(|attenuation| {
                        add_aligned_field(snapshot, reflectivity, attenuation, product, 1.0, 1.0)
                    })
                })
            }
            DerivedSweepProduct::SpecificDifferentialAttenuation => {
                kdp_for_dependencies.map(|kdp| {
                    scale_positive_field(
                        kdp,
                        product,
                        config.attenuation.differential_db_per_degree,
                        f32::INFINITY,
                    )
                })
            }
            DerivedSweepProduct::PathIntegratedDifferentialAttenuation => pida.clone(),
            DerivedSweepProduct::CorrectedDifferentialReflectivity => inputs.zdr.and_then(|zdr| {
                pida.as_ref().map(|attenuation| {
                    add_aligned_field(snapshot, zdr, attenuation, product, 1.0, 1.0)
                })
            }),
            DerivedSweepProduct::RainRateReflectivity => inputs
                .reflectivity
                .map(|reflectivity| rain_rate_z_field(reflectivity, product, &config.qpe)),
            DerivedSweepProduct::RainRateKdp => {
                kdp_for_dependencies.map(|kdp| rain_rate_kdp_field(kdp, product, &config.qpe))
            }
            DerivedSweepProduct::RainRateHybrid => {
                hybrid_rain_rate_field(snapshot, &inputs, kdp_for_dependencies, &config.qpe)
            }
            DerivedSweepProduct::LiquidWaterContent => inputs
                .reflectivity
                .map(|reflectivity| liquid_water_content_field(reflectivity, product)),
            DerivedSweepProduct::HailKineticEnergy => inputs
                .reflectivity
                .map(|reflectivity| hail_kinetic_energy_field(reflectivity, product)),
            DerivedSweepProduct::CircularDepolarizationRatio => {
                circular_depolarization_ratio_field(snapshot, &inputs)
            }
            DerivedSweepProduct::LogCorrelationRatio => inputs
                .rho
                .map(|rho| log_correlation_ratio_field(rho, product)),
            DerivedSweepProduct::ReflectivityTexture => inputs.reflectivity.map(|source| {
                texture_field(
                    snapshot,
                    source,
                    product,
                    &config.texture,
                    TexturePeriod::Linear,
                )
            }),
            DerivedSweepProduct::VelocityTexture => inputs.velocity.map(|source| {
                texture_field(
                    snapshot,
                    source,
                    product,
                    &config.texture,
                    TexturePeriod::NyquistVelocity,
                )
            }),
            DerivedSweepProduct::SpectrumWidthTexture => inputs.spectrum_width.map(|source| {
                texture_field(
                    snapshot,
                    source,
                    product,
                    &config.texture,
                    TexturePeriod::Linear,
                )
            }),
            DerivedSweepProduct::DifferentialReflectivityTexture => inputs.zdr.map(|source| {
                texture_field(
                    snapshot,
                    source,
                    product,
                    &config.texture,
                    TexturePeriod::Linear,
                )
            }),
            DerivedSweepProduct::CorrelationCoefficientTexture => inputs.rho.map(|source| {
                texture_field(
                    snapshot,
                    source,
                    product,
                    &config.texture,
                    TexturePeriod::Linear,
                )
            }),
            DerivedSweepProduct::DifferentialPhaseTexture => inputs.phi.map(|source| {
                texture_field(
                    snapshot,
                    source,
                    product,
                    &config.texture,
                    TexturePeriod::Fixed(config.kdp.phase_period_deg),
                )
            }),
            DerivedSweepProduct::KdpTexture => kdp_for_dependencies.map(|source| {
                texture_field(
                    snapshot,
                    source,
                    product,
                    &config.texture,
                    TexturePeriod::Linear,
                )
            }),
            DerivedSweepProduct::ReflectivityRangeGradient => inputs
                .reflectivity
                .map(|source| range_gradient_field(snapshot, source, product)),
            DerivedSweepProduct::VelocityRangeGradient => inputs
                .velocity
                .map(|source| velocity_range_gradient_field(snapshot, source, product)),
            DerivedSweepProduct::MeteorologicalQuality => {
                meteorological_quality_field(snapshot, &inputs, product)
            }
            DerivedSweepProduct::MeteorologicalGateMask => {
                meteorological_mask_field(snapshot, &inputs, product, &config.meteo_mask)
            }
            DerivedSweepProduct::TdsConfidence => {
                tds_score_field(snapshot, &inputs, product, &config.diagnostics)
            }
            DerivedSweepProduct::HailSignature => {
                hail_score_field(snapshot, &inputs, product, &config.diagnostics)
            }
            DerivedSweepProduct::TurbulenceProxy => {
                turbulence_proxy_field(snapshot, &inputs, product, &config.texture)
            }
        };

        match field {
            Some(mut field) => {
                field.name = output_name;
                pending.push(field);
                report.inserted.push(product.id().to_owned());
            }
            None => report.unavailable.push(product.id().to_owned()),
        }
    }

    for field in pending {
        match sweep.field_index(&field.name) {
            Some(index) => sweep.fields[index] = field,
            None => sweep.fields.push(field),
        }
    }
    report
}

/// Derive one product without mutating the caller's sweep.
pub fn derive_product(
    sweep: &Sweep,
    product: DerivedSweepProduct,
    config: &DerivationConfig,
) -> Option<Field> {
    let mut copy = sweep.clone();
    let mut one = config.clone();
    one.products.clear();
    one.products.insert(product);
    // A caller asking for a derived product expects a newly computed value,
    // even if a field with the same name is already present in the snapshot.
    one.overwrite_existing = true;
    derive_sweep_in_place(&mut copy, &one);
    let name = product.field_name_in(sweep);
    copy.fields.into_iter().find(|field| field.name == name)
}

fn needs_any(requested: &BTreeSet<DerivedSweepProduct>, products: &[DerivedSweepProduct]) -> bool {
    products.iter().any(|product| requested.contains(product))
}

#[derive(Clone)]
struct PhaseBundle {
    filtered_phi: Field,
    kdp: Field,
    uncertainty: Field,
}

fn derive_phase_bundle(
    sweep: &Sweep,
    inputs: &Inputs<'_>,
    config: &KdpConfig,
) -> Option<PhaseBundle> {
    let phi = inputs.phi?;
    let rho = inputs.rho.and_then(|rho| FieldSampler::new(sweep, rho));
    let reflectivity = inputs
        .reflectivity
        .and_then(|reflectivity| FieldSampler::new(sweep, reflectivity));

    let (rows, gates) = phi.shape();
    let (phi_first_m, phi_spacing_m) = phi.native_geometry(&sweep.range)?;
    if rows == 0 || gates == 0 || phi_spacing_m <= 0.0 {
        return None;
    }

    let mut filtered_values = vec![f32::NAN; rows * gates];
    let mut kdp_values = vec![f32::NAN; rows * gates];
    let mut uncertainty_values = vec![f32::NAN; rows * gates];
    let spacing_km = phi_spacing_m / 1000.0;
    let window_gates = regression_window_gates(config, spacing_km as f32);

    for row in 0..rows {
        let mut values = vec![f32::NAN; gates];
        let mut original_valid = vec![false; gates];

        for gate in 0..gates {
            let Some(phase) = phi.value(row, gate) else {
                continue;
            };
            if !phase.is_finite() {
                continue;
            }
            let range_m = phi_first_m + gate as f64 * phi_spacing_m;
            if let Some(rho_sampler) = rho.as_ref()
                && let Some(rho_hv) = rho_sampler.sample(row, range_m)
                && (!rho_hv.is_finite() || rho_hv < config.min_rho_hv)
            {
                continue;
            }
            if let Some(ref_sampler) = reflectivity.as_ref()
                && let Some(dbz) = ref_sampler.sample(row, range_m)
                && (!dbz.is_finite() || dbz < config.min_reflectivity_dbz)
            {
                continue;
            }
            values[gate] = phase;
            original_valid[gate] = true;
        }

        unwrap_phase_in_place(
            &mut values,
            config.phase_period_deg,
            config.max_interpolated_gap_gates,
        );
        fill_short_gaps_in_place(&mut values, config.max_interpolated_gap_gates);
        let values = hampel_filter(&values, config.hampel_half_window, config.hampel_sigma);

        let mut xs = Vec::with_capacity(window_gates);
        let mut ys = Vec::with_capacity(window_gates);
        let mut fit_scratch = FitScratch::default();
        for (gate, originally_valid) in original_valid.iter().copied().enumerate().take(gates) {
            if !originally_valid && !config.emit_interpolated_gates {
                continue;
            }
            let half = window_gates / 2;
            let start = gate.saturating_sub(half);
            let end = (gate + half + 1).min(gates);
            xs.clear();
            ys.clear();
            for (sample_gate, value) in values.iter().copied().enumerate().take(end).skip(start) {
                if value.is_finite() {
                    xs.push((sample_gate as f64 - gate as f64) * spacing_km);
                    ys.push(value as f64);
                }
            }
            if xs.len() < config.min_valid_gates {
                continue;
            }
            let Some(fit) = robust_linear_fit(&xs, &ys, config.huber_k as f64, &mut fit_scratch)
            else {
                continue;
            };
            let index = row * gates + gate;
            filtered_values[index] = fit.intercept as f32;
            let kdp = (0.5 * fit.slope) as f32;
            if kdp.is_finite() && kdp >= config.kdp_min_deg_km && kdp <= config.kdp_max_deg_km {
                kdp_values[index] = kdp;
                uncertainty_values[index] = (0.5 * fit.slope_standard_error) as f32;
            }
        }
    }

    Some(PhaseBundle {
        filtered_phi: f32_field_like(
            phi,
            DerivedSweepProduct::FilteredDifferentialPhase,
            filtered_values,
        ),
        kdp: f32_field_like(phi, DerivedSweepProduct::Kdp, kdp_values),
        uncertainty: f32_field_like(phi, DerivedSweepProduct::KdpUncertainty, uncertainty_values),
    })
}

fn regression_window_gates(config: &KdpConfig, spacing_km: f32) -> usize {
    let minimum = config.min_window_gates.max(3);
    let maximum = config.max_window_gates.max(minimum);
    if !spacing_km.is_finite() || spacing_km <= 0.0 {
        return minimum | 1;
    }
    let mut gates = (config.window_km.max(spacing_km) / spacing_km).round() as usize;
    gates = gates.clamp(minimum, maximum);
    if gates.is_multiple_of(2) {
        gates = if gates < maximum {
            gates + 1
        } else {
            gates.saturating_sub(1).max(3)
        };
    }
    gates
}

fn unwrap_phase_in_place(values: &mut [f32], period_deg: f32, max_gap: usize) {
    if !period_deg.is_finite() || period_deg <= 0.0 {
        return;
    }
    let mut previous = None::<f32>;
    let mut gap = 0usize;
    for value in values.iter_mut() {
        if !value.is_finite() {
            gap = gap.saturating_add(1);
            continue;
        }
        if gap > max_gap {
            previous = None;
        }
        if let Some(last) = previous {
            let wraps = ((*value - last) / period_deg).round();
            *value -= wraps * period_deg;
        }
        previous = Some(*value);
        gap = 0;
    }
}

fn fill_short_gaps_in_place(values: &mut [f32], max_gap: usize) {
    if max_gap == 0 || values.len() < 3 {
        return;
    }
    let mut index = 0;
    while index < values.len() {
        if values[index].is_finite() {
            index += 1;
            continue;
        }
        let start = index;
        while index < values.len() && !values[index].is_finite() {
            index += 1;
        }
        let gap_len = index - start;
        if start == 0 || index >= values.len() || gap_len > max_gap {
            continue;
        }
        let left = values[start - 1];
        let right = values[index];
        if !left.is_finite() || !right.is_finite() {
            continue;
        }
        for offset in 0..gap_len {
            let fraction = (offset + 1) as f32 / (gap_len + 1) as f32;
            values[start + offset] = left + fraction * (right - left);
        }
    }
}

fn hampel_filter(values: &[f32], half_window: usize, sigma_threshold: f32) -> Vec<f32> {
    if half_window == 0 || values.is_empty() {
        return values.to_vec();
    }
    let mut filtered = values.to_vec();
    let mut neighborhood = Vec::with_capacity(half_window * 2 + 1);
    let mut deviations = Vec::with_capacity(half_window * 2 + 1);
    for index in 0..values.len() {
        let value = values[index];
        if !value.is_finite() {
            continue;
        }
        let start = index.saturating_sub(half_window);
        let end = (index + half_window + 1).min(values.len());
        neighborhood.clear();
        neighborhood.extend(
            values[start..end]
                .iter()
                .copied()
                .filter(|sample| sample.is_finite()),
        );
        let Some(median) = median_f32_mut(&mut neighborhood) else {
            continue;
        };
        deviations.clear();
        deviations.extend(neighborhood.iter().map(|sample| (sample - median).abs()));
        let Some(mad) = median_f32_mut(&mut deviations) else {
            continue;
        };
        let robust_sigma = 1.4826 * mad;
        let outlier = if robust_sigma > 1.0e-4 {
            (value - median).abs() > sigma_threshold.max(0.0) * robust_sigma
        } else {
            (value - median).abs() > sigma_threshold.max(1.0)
        };
        if outlier {
            filtered[index] = median;
        }
    }
    filtered
}

#[derive(Clone, Copy, Debug)]
struct LinearFit {
    slope: f64,
    intercept: f64,
    slope_standard_error: f64,
}

#[derive(Default)]
struct FitScratch {
    weights: Vec<f64>,
    residuals: Vec<f64>,
    ordered: Vec<f64>,
    deviations: Vec<f64>,
}

fn robust_linear_fit(
    xs: &[f64],
    ys: &[f64],
    huber_k: f64,
    scratch: &mut FitScratch,
) -> Option<LinearFit> {
    if xs.len() != ys.len() || xs.len() < 2 {
        return None;
    }
    scratch.weights.clear();
    scratch.weights.resize(xs.len(), 1.0);
    let mut slope = 0.0;
    let mut intercept = 0.0;

    for _ in 0..3 {
        (slope, intercept) = weighted_linear_fit(xs, ys, &scratch.weights)?;
        scratch.residuals.clear();
        scratch
            .residuals
            .extend(xs.iter().zip(ys).map(|(x, y)| y - (intercept + slope * x)));
        scratch.ordered.clear();
        scratch.ordered.extend_from_slice(&scratch.residuals);
        let median_residual = median_f64_mut(&mut scratch.ordered)?;
        scratch.deviations.clear();
        scratch.deviations.extend(
            scratch
                .residuals
                .iter()
                .map(|residual| (residual - median_residual).abs()),
        );
        let sigma = 1.4826 * median_f64_mut(&mut scratch.deviations)?;
        if !sigma.is_finite() || sigma <= 1.0e-8 {
            break;
        }
        let cutoff = huber_k.max(0.1) * sigma;
        for (weight, residual) in scratch.weights.iter_mut().zip(&scratch.residuals) {
            let magnitude = residual.abs();
            *weight = if magnitude <= cutoff {
                1.0
            } else {
                cutoff / magnitude
            };
        }
    }

    let weight_sum = scratch.weights.iter().sum::<f64>();
    let x_mean = xs
        .iter()
        .zip(&scratch.weights)
        .map(|(x, weight)| x * weight)
        .sum::<f64>()
        / weight_sum;
    let centered_x_sum = xs
        .iter()
        .zip(&scratch.weights)
        .map(|(x, weight)| weight * (x - x_mean).powi(2))
        .sum::<f64>();
    let residual_sum = xs
        .iter()
        .zip(ys)
        .zip(&scratch.weights)
        .map(|((x, y), weight)| weight * (y - (intercept + slope * x)).powi(2))
        .sum::<f64>();
    let degrees_of_freedom = (xs.len() as f64 - 2.0).max(1.0);
    let variance = residual_sum / degrees_of_freedom;
    let slope_standard_error = if centered_x_sum > 1.0e-12 {
        (variance / centered_x_sum).max(0.0).sqrt()
    } else {
        f64::NAN
    };

    Some(LinearFit {
        slope,
        intercept,
        slope_standard_error,
    })
}

fn weighted_linear_fit(xs: &[f64], ys: &[f64], weights: &[f64]) -> Option<(f64, f64)> {
    let sw = weights.iter().sum::<f64>();
    let sx = xs
        .iter()
        .zip(weights)
        .map(|(x, weight)| x * weight)
        .sum::<f64>();
    let sy = ys
        .iter()
        .zip(weights)
        .map(|(y, weight)| y * weight)
        .sum::<f64>();
    let sxx = xs
        .iter()
        .zip(weights)
        .map(|(x, weight)| x * x * weight)
        .sum::<f64>();
    let sxy = xs
        .iter()
        .zip(ys)
        .zip(weights)
        .map(|((x, y), weight)| x * y * weight)
        .sum::<f64>();
    let denominator = sw * sxx - sx * sx;
    if !denominator.is_finite() || denominator.abs() <= 1.0e-12 || sw <= 0.0 {
        return None;
    }
    let slope = (sw * sxy - sx * sy) / denominator;
    let intercept = (sy - slope * sx) / sw;
    (slope.is_finite() && intercept.is_finite()).then_some((slope, intercept))
}

fn median_f32_mut(values: &mut [f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        Some(0.5 * (values[middle - 1] + values[middle]))
    } else {
        Some(values[middle])
    }
}

fn median_f64_mut(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        Some(0.5 * (values[middle - 1] + values[middle]))
    } else {
        Some(values[middle])
    }
}

fn median_f32(values: &[f32]) -> Option<f32> {
    let mut sorted = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(|a, b| a.total_cmp(b));
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        Some(0.5 * (sorted[middle - 1] + sorted[middle]))
    } else {
        Some(sorted[middle])
    }
}

/// A physical `F32` field of `product` on `base`'s rays and native gates
/// (NaN = no data), named with the canonical base name until the pipeline
/// assigns the sweep-specific name.
pub(crate) fn f32_field_like(
    base: &Field,
    product: DerivedSweepProduct,
    values: Vec<f32>,
) -> Field {
    debug_assert_eq!(values.len(), base.nrays as usize * base.ngates as usize);
    physical_field(
        base,
        product.canonical_field_name(),
        product.quantity(),
        Some(product.units()),
        Some(product.display_name()),
        values,
    )
}

/// A physical `F32` field named `name` on `base`'s rays, native gates and
/// absent rows (a product is undefined where its source has no ray).
pub(crate) fn physical_field(
    base: &Field,
    name: FieldName,
    quantity: Quantity,
    units: Option<&'static str>,
    long_name: Option<&'static str>,
    values: Vec<f32>,
) -> Field {
    Field {
        name,
        quantity,
        polarization: Polarization::Unspecified,
        attrs: FieldAttrs {
            units: units.map(Cow::Borrowed),
            long_name: long_name.map(Cow::Borrowed),
            ..FieldAttrs::default()
        },
        nrays: base.nrays,
        ngates: base.ngates,
        gates: base.gates,
        data: FieldData::F32 {
            values,
            coding: FloatCoding::default(),
        },
        absent_rows: base.absent_rows.clone(),
    }
}

/// Centre of native gate `gate` of a field with native geometry
/// `(first_center_m, spacing_m)`.
fn gate_center_m(first_center_m: f64, spacing_m: f64, gate: usize) -> f64 {
    first_center_m + gate as f64 * spacing_m
}

/// Samples one field of a sweep by (ray, physical range): the gate whose
/// centre is nearest the range, within 0.55 of a gate. Rows of every field
/// of a sweep are the sweep's rays, so no radial lookup is needed.
pub(crate) struct FieldSampler<'a> {
    field: &'a Field,
    first_center_m: f64,
    spacing_m: f64,
}

impl<'a> FieldSampler<'a> {
    pub(crate) fn new(sweep: &Sweep, field: &'a Field) -> Option<Self> {
        let (first_center_m, spacing_m) = field.native_geometry(&sweep.range)?;
        Some(Self {
            field,
            first_center_m,
            spacing_m,
        })
    }

    pub(crate) fn sample(&self, row: usize, range_m: f64) -> Option<f32> {
        let spacing = self.spacing_m;
        if !spacing.is_finite() || spacing <= 0.0 {
            return None;
        }
        let gate_position = (range_m - self.first_center_m) / spacing;
        if !gate_position.is_finite() {
            return None;
        }
        let rounded = gate_position.round();
        if rounded < 0.0 || rounded >= self.field.ngates as f64 {
            return None;
        }
        if (gate_position - rounded).abs() > 0.55 {
            return None;
        }
        self.field.value(row, rounded as usize)
    }
}

fn phase_excess_field(sweep: &Sweep, filtered_phi: &Field, baseline_gates: usize) -> Field {
    let (rows, gates) = filtered_phi.shape();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        let baseline_candidates = (0..gates.min(baseline_gates.max(1)))
            .filter_map(|gate| filtered_phi.value(row, gate))
            .filter(|value| value.is_finite())
            .collect::<Vec<_>>();
        let Some(baseline) = median_f32(&baseline_candidates) else {
            continue;
        };
        for gate in 0..gates {
            if let Some(value) = filtered_phi.value(row, gate)
                && value.is_finite()
            {
                out[row * gates + gate] = (value - baseline).max(0.0);
            }
        }
    }
    let _ = sweep;
    physical_field(
        filtered_phi,
        FieldName::parse("PHI_EXCESS"),
        Quantity::Other,
        Some("deg"),
        None,
        out,
    )
}

fn scale_positive_field(
    source: &Field,
    product: DerivedSweepProduct,
    coefficient: f32,
    maximum: f32,
) -> Field {
    let (rows, gates) = source.shape();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            if let Some(value) = source.value(row, gate)
                && value.is_finite()
            {
                out[row * gates + gate] = (coefficient * value.max(0.0)).min(maximum);
            }
        }
    }
    f32_field_like(source, product, out)
}

/// Integrate KDP into two-way path attenuation. Since d(PHIDP)/dr = 2*KDP
/// and PIA = alpha*PHIDP, d(PIA)/dr = 2*alpha*KDP.
fn integrate_kdp(
    sweep: &Sweep,
    kdp: &Field,
    product: DerivedSweepProduct,
    coefficient: f32,
    maximum: f32,
) -> Field {
    let (rows, gates) = kdp.shape();
    let dr_km = kdp
        .native_geometry(&sweep.range)
        .map_or(0.0, |(_, spacing_m)| spacing_m.max(0.0) as f32 / 1000.0);
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        let mut integrated = 0.0f32;
        let mut previous = None::<f32>;
        for gate in 0..gates {
            let current = kdp
                .value(row, gate)
                .filter(|value| value.is_finite())
                .map(|value| value.max(0.0));
            if let Some(current) = current {
                let representative = previous.map_or(current, |last| 0.5 * (last + current));
                integrated += 2.0 * coefficient * representative * dr_km;
                out[row * gates + gate] = integrated.min(maximum);
                previous = Some(current);
            } else {
                previous = None;
            }
        }
    }
    f32_field_like(kdp, product, out)
}

fn add_aligned_field(
    sweep: &Sweep,
    base: &Field,
    other: &Field,
    product: DerivedSweepProduct,
    base_factor: f32,
    other_factor: f32,
) -> Field {
    let (rows, gates) = base.shape();
    let mut out = vec![f32::NAN; rows * gates];
    if let (Some(other), Some((first_m, spacing_m))) = (
        FieldSampler::new(sweep, other),
        base.native_geometry(&sweep.range),
    ) {
        for row in 0..rows {
            for gate in 0..gates {
                let Some(base_value) = base.value(row, gate) else {
                    continue;
                };
                let Some(other_value) = other.sample(row, gate_center_m(first_m, spacing_m, gate))
                else {
                    continue;
                };
                if base_value.is_finite() && other_value.is_finite() {
                    out[row * gates + gate] = base_factor * base_value + other_factor * other_value;
                }
            }
        }
    }
    f32_field_like(base, product, out)
}

fn rain_rate_z_field(
    reflectivity: &Field,
    product: DerivedSweepProduct,
    config: &QpeConfig,
) -> Field {
    let (rows, gates) = reflectivity.shape();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            let Some(dbz) = reflectivity.value(row, gate) else {
                continue;
            };
            if !dbz.is_finite() {
                continue;
            }
            let z_linear = 10.0f32.powf(0.1 * dbz);
            let rate = (z_linear / config.z_r_a.max(1.0e-6))
                .max(0.0)
                .powf(1.0 / config.z_r_b.max(1.0e-6));
            out[row * gates + gate] = rate.min(config.max_rate_mm_h);
        }
    }
    f32_field_like(reflectivity, product, out)
}

fn rain_rate_kdp_field(kdp: &Field, product: DerivedSweepProduct, config: &QpeConfig) -> Field {
    let (rows, gates) = kdp.shape();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            let Some(value) = kdp.value(row, gate) else {
                continue;
            };
            if !value.is_finite() {
                continue;
            }
            let rate = config.kdp_alpha * value.max(0.0).powf(config.kdp_beta);
            out[row * gates + gate] = rate.min(config.max_rate_mm_h);
        }
    }
    f32_field_like(kdp, product, out)
}

fn hybrid_rain_rate_field(
    sweep: &Sweep,
    inputs: &Inputs<'_>,
    kdp: Option<&Field>,
    config: &QpeConfig,
) -> Option<Field> {
    let product = DerivedSweepProduct::RainRateHybrid;
    match (inputs.reflectivity, kdp) {
        (None, None) => None,
        (None, Some(kdp)) => Some(rain_rate_kdp_field(kdp, product, config)),
        (Some(reflectivity), None) => Some(rain_rate_z_field(reflectivity, product, config)),
        (Some(reflectivity), Some(kdp)) => {
            let kdp_sampler = FieldSampler::new(sweep, kdp);
            let rho_sampler = inputs.rho.and_then(|rho| FieldSampler::new(sweep, rho));
            let (rows, gates) = reflectivity.shape();
            let mut out = vec![f32::NAN; rows * gates];
            if let (Some(kdp_sampler), Some((first_m, spacing_m))) =
                (kdp_sampler, reflectivity.native_geometry(&sweep.range))
            {
                for row in 0..rows {
                    for gate in 0..gates {
                        let Some(dbz) = reflectivity.value(row, gate) else {
                            continue;
                        };
                        if !dbz.is_finite() {
                            continue;
                        }
                        let range_m = gate_center_m(first_m, spacing_m, gate);
                        let kdp_value = kdp_sampler.sample(row, range_m);
                        let rho_value = rho_sampler
                            .as_ref()
                            .and_then(|sampler| sampler.sample(row, range_m));
                        let use_kdp = kdp_value.is_some_and(|value| {
                            value.is_finite()
                                && value >= config.hybrid_min_kdp_deg_km
                                && dbz >= config.hybrid_min_reflectivity_dbz
                                && rho_value.is_none_or(|rho| {
                                    rho.is_finite() && rho >= config.hybrid_min_rho_hv
                                })
                        });
                        let rate = if use_kdp {
                            config.kdp_alpha
                                * kdp_value.unwrap_or_default().max(0.0).powf(config.kdp_beta)
                        } else {
                            let z_linear = 10.0f32.powf(0.1 * dbz);
                            (z_linear / config.z_r_a.max(1.0e-6))
                                .max(0.0)
                                .powf(1.0 / config.z_r_b.max(1.0e-6))
                        };
                        out[row * gates + gate] = rate.min(config.max_rate_mm_h);
                    }
                }
            }
            Some(f32_field_like(reflectivity, product, out))
        }
    }
}

fn liquid_water_content_field(reflectivity: &Field, product: DerivedSweepProduct) -> Field {
    let (rows, gates) = reflectivity.shape();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            if let Some(dbz) = reflectivity.value(row, gate)
                && dbz.is_finite()
            {
                let z_linear = 10.0f64.powf(dbz.min(56.0) as f64 / 10.0);
                // Greene-Clark VIL integrand, converted kg/m^3 -> g/m^3.
                out[row * gates + gate] = (1000.0 * 3.44e-6 * z_linear.powf(4.0 / 7.0)) as f32;
            }
        }
    }
    f32_field_like(reflectivity, product, out)
}

fn hail_kinetic_energy_field(reflectivity: &Field, product: DerivedSweepProduct) -> Field {
    let (rows, gates) = reflectivity.shape();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            if let Some(dbz) = reflectivity.value(row, gate)
                && dbz.is_finite()
            {
                let weight = ((dbz - 40.0) / 10.0).clamp(0.0, 1.0);
                out[row * gates + gate] = if weight <= 0.0 {
                    0.0
                } else {
                    5.0e-6 * 10.0f32.powf(0.084 * dbz) * weight
                };
            }
        }
    }
    f32_field_like(reflectivity, product, out)
}

fn circular_depolarization_ratio_field(sweep: &Sweep, inputs: &Inputs<'_>) -> Option<Field> {
    let product = DerivedSweepProduct::CircularDepolarizationRatio;
    let zdr = inputs.zdr?;
    let rho = FieldSampler::new(sweep, inputs.rho?)?;
    let (first_m, spacing_m) = zdr.native_geometry(&sweep.range)?;
    let (rows, gates) = zdr.shape();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            let Some(zdr_db) = zdr.value(row, gate) else {
                continue;
            };
            let Some(rho_hv) = rho.sample(row, gate_center_m(first_m, spacing_m, gate)) else {
                continue;
            };
            if !zdr_db.is_finite() || !rho_hv.is_finite() {
                continue;
            }
            let zdr_linear = 10.0f32.powf(0.1 * zdr_db).max(1.0e-6);
            let inverse_sqrt = (1.0 / zdr_linear).sqrt();
            let numerator = 1.0 + 1.0 / zdr_linear - 2.0 * rho_hv * inverse_sqrt;
            let denominator = 1.0 + 1.0 / zdr_linear + 2.0 * rho_hv * inverse_sqrt;
            if numerator > 0.0 && denominator > 0.0 {
                out[row * gates + gate] = 10.0 * (numerator / denominator).log10();
            }
        }
    }
    Some(f32_field_like(zdr, product, out))
}

fn log_correlation_ratio_field(rho: &Field, product: DerivedSweepProduct) -> Field {
    let (rows, gates) = rho.shape();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            if let Some(value) = rho.value(row, gate)
                && value.is_finite()
            {
                let bounded = value.clamp(0.0, 0.9999);
                out[row * gates + gate] = -(1.0 - bounded).log10();
            }
        }
    }
    f32_field_like(rho, product, out)
}

#[derive(Clone, Copy)]
enum TexturePeriod {
    Linear,
    Fixed(f32),
    NyquistVelocity,
}

struct AzimuthNeighborhood {
    sorted_rows: Vec<usize>,
    rank_by_row: Vec<usize>,
    wraps: bool,
}

impl AzimuthNeighborhood {
    /// Rows of `field` (the sweep's rays it provides) ordered by azimuth.
    fn new(sweep: &Sweep, field: &Field) -> Self {
        let mut azimuth_rows = (0..field.nrays as usize)
            .filter_map(|row| {
                sweep
                    .rays
                    .azimuth_deg
                    .get(row)
                    .map(|azimuth| (azimuth.rem_euclid(360.0), row))
            })
            .collect::<Vec<_>>();
        azimuth_rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        let sorted_rows = azimuth_rows.iter().map(|(_, row)| *row).collect::<Vec<_>>();
        let mut rank_by_row = vec![0usize; field.nrays as usize];
        for (rank, row) in sorted_rows.iter().copied().enumerate() {
            if row < rank_by_row.len() {
                rank_by_row[row] = rank;
            }
        }
        let largest_gap = match (azimuth_rows.first(), azimuth_rows.last()) {
            (Some(first), Some(last)) if azimuth_rows.len() >= 2 => {
                let mut largest = 0.0f32;
                for pair in azimuth_rows.windows(2) {
                    largest = largest.max(pair[1].0 - pair[0].0);
                }
                largest.max(360.0 - last.0 + first.0)
            }
            _ => 360.0,
        };
        Self {
            sorted_rows,
            rank_by_row,
            wraps: azimuth_rows.len() >= 180 && largest_gap <= 10.0,
        }
    }

    fn rows_near(&self, row: usize, radius: usize) -> Vec<usize> {
        if self.sorted_rows.is_empty() || row >= self.rank_by_row.len() {
            return Vec::new();
        }
        let rank = self.rank_by_row[row] as isize;
        let count = self.sorted_rows.len() as isize;
        let mut rows = Vec::with_capacity(radius * 2 + 1);
        for offset in -(radius as isize)..=(radius as isize) {
            let mut neighbor = rank + offset;
            if self.wraps {
                neighbor = neighbor.rem_euclid(count);
            } else if neighbor < 0 || neighbor >= count {
                continue;
            }
            rows.push(self.sorted_rows[neighbor as usize]);
        }
        rows
    }
}

/// Nyquist velocity of ray `row` when the sweep has one and it is finite.
fn ray_nyquist(sweep: &Sweep, row: usize) -> Option<f32> {
    sweep
        .ray_vars
        .nyquist_velocity_mps
        .as_ref()?
        .get(row)
        .copied()
        .filter(|value| !value.is_nan())
}

fn texture_field(
    sweep: &Sweep,
    source: &Field,
    product: DerivedSweepProduct,
    config: &TextureConfig,
    period_policy: TexturePeriod,
) -> Field {
    let (rows, gates) = source.shape();
    let neighborhood = AzimuthNeighborhood::new(sweep, source);
    let mut out = vec![f32::NAN; rows * gates];

    for row in 0..rows {
        let neighbor_rows = neighborhood.rows_near(row, config.radial_radius);
        let period = match period_policy {
            TexturePeriod::Linear => None,
            TexturePeriod::Fixed(period) => (period > 0.0 && period.is_finite()).then_some(period),
            TexturePeriod::NyquistVelocity => ray_nyquist(sweep, row)
                .map(|nyquist| 2.0 * nyquist.abs())
                .filter(|period| *period > 0.0 && period.is_finite()),
        };
        for gate in 0..gates {
            let gate_start = gate.saturating_sub(config.gate_radius);
            let gate_end = (gate + config.gate_radius + 1).min(gates);
            let mut samples = Vec::with_capacity(neighbor_rows.len() * (gate_end - gate_start));
            for &neighbor_row in &neighbor_rows {
                for neighbor_gate in gate_start..gate_end {
                    if let Some(value) = source.value(neighbor_row, neighbor_gate)
                        && value.is_finite()
                    {
                        samples.push(value);
                    }
                }
            }
            if samples.len() < config.min_samples {
                continue;
            }
            let reference = source
                .value(row, gate)
                .filter(|value| value.is_finite())
                .unwrap_or(samples[0]);
            if let Some(period) = period {
                for sample in &mut samples {
                    *sample = reference + wrapped_delta(*sample - reference, period);
                }
            }
            let mean = samples.iter().sum::<f32>() / samples.len() as f32;
            let variance = samples
                .iter()
                .map(|sample| (sample - mean).powi(2))
                .sum::<f32>()
                / samples.len() as f32;
            out[row * gates + gate] = variance.max(0.0).sqrt();
        }
    }
    f32_field_like(source, product, out)
}

fn wrapped_delta(delta: f32, period: f32) -> f32 {
    (delta + 0.5 * period).rem_euclid(period) - 0.5 * period
}

fn range_gradient_field(sweep: &Sweep, source: &Field, product: DerivedSweepProduct) -> Field {
    range_gradient_field_with_period(sweep, source, product, |_| None)
}

fn velocity_range_gradient_field(
    sweep: &Sweep,
    source: &Field,
    product: DerivedSweepProduct,
) -> Field {
    range_gradient_field_with_period(sweep, source, product, |row| {
        ray_nyquist(sweep, row)
            .map(|nyquist| 2.0 * nyquist.abs())
            .filter(|period| *period > 0.0 && period.is_finite())
    })
}

fn range_gradient_field_with_period<F>(
    sweep: &Sweep,
    source: &Field,
    product: DerivedSweepProduct,
    period_for_row: F,
) -> Field
where
    F: Fn(usize) -> Option<f32>,
{
    let (rows, gates) = source.shape();
    let spacing_km = source
        .native_geometry(&sweep.range)
        .map_or(f32::NAN, |(_, spacing_m)| spacing_m as f32 / 1000.0);
    let mut out = vec![f32::NAN; rows * gates];
    if !spacing_km.is_finite() || spacing_km <= 0.0 {
        return f32_field_like(source, product, out);
    }
    for row in 0..rows {
        let period = period_for_row(row);
        for gate in 0..gates {
            let left = (1..=2).find_map(|distance| {
                gate.checked_sub(distance).and_then(|index| {
                    source
                        .value(row, index)
                        .filter(|value| value.is_finite())
                        .map(|value| (index, value))
                })
            });
            let right = (1..=2).find_map(|distance| {
                let index = gate + distance;
                (index < gates)
                    .then(|| source.value(row, index))
                    .flatten()
                    .filter(|value| value.is_finite())
                    .map(|value| (index, value))
            });
            let gradient = match (left, right) {
                (Some((left_gate, left_value)), Some((right_gate, right_value))) => Some(
                    range_delta(left_value, right_value, period)
                        / ((right_gate - left_gate) as f32 * spacing_km),
                ),
                (Some((left_gate, left_value)), None) => source
                    .value(row, gate)
                    .filter(|value| value.is_finite())
                    .map(|center| {
                        range_delta(left_value, center, period)
                            / ((gate - left_gate) as f32 * spacing_km)
                    }),
                (None, Some((right_gate, right_value))) => source
                    .value(row, gate)
                    .filter(|value| value.is_finite())
                    .map(|center| {
                        range_delta(center, right_value, period)
                            / ((right_gate - gate) as f32 * spacing_km)
                    }),
                (None, None) => None,
            };
            if let Some(gradient) = gradient
                && gradient.is_finite()
            {
                out[row * gates + gate] = gradient;
            }
        }
    }
    f32_field_like(source, product, out)
}

fn range_delta(left: f32, right: f32, period: Option<f32>) -> f32 {
    let delta = right - left;
    period
        .map(|period| wrapped_delta(delta, period))
        .unwrap_or(delta)
}

fn meteorological_quality_field(
    sweep: &Sweep,
    inputs: &Inputs<'_>,
    product: DerivedSweepProduct,
) -> Option<Field> {
    let base = inputs.rho.or(inputs.reflectivity)?;
    let rho = inputs.rho.and_then(|rho| FieldSampler::new(sweep, rho));
    let reflectivity = inputs
        .reflectivity
        .and_then(|reflectivity| FieldSampler::new(sweep, reflectivity));
    let (first_m, spacing_m) = base.native_geometry(&sweep.range)?;
    let (rows, gates) = base.shape();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            let range_m = gate_center_m(first_m, spacing_m, gate);
            let rho_value = rho
                .as_ref()
                .and_then(|sampler| sampler.sample(row, range_m));
            let dbz = reflectivity
                .as_ref()
                .and_then(|sampler| sampler.sample(row, range_m));
            if rho_value.is_none() && dbz.is_none() {
                continue;
            }
            let rho_score = rho_value
                .filter(|value| value.is_finite())
                .map(|value| ((value - 0.70) / 0.30).clamp(0.0, 1.0));
            let reflectivity_score = dbz
                .filter(|value| value.is_finite())
                .map(|value| ((value + 10.0) / 20.0).clamp(0.0, 1.0));
            out[row * gates + gate] = match (rho_score, reflectivity_score) {
                (Some(rho_score), Some(reflectivity_score)) => {
                    0.75 * rho_score + 0.25 * reflectivity_score
                }
                (Some(score), None) | (None, Some(score)) => score,
                (None, None) => f32::NAN,
            };
        }
    }
    Some(f32_field_like(base, product, out))
}

fn meteorological_mask_field(
    sweep: &Sweep,
    inputs: &Inputs<'_>,
    product: DerivedSweepProduct,
    config: &MeteoMaskConfig,
) -> Option<Field> {
    let quality =
        meteorological_quality_field(sweep, inputs, DerivedSweepProduct::MeteorologicalQuality)?;
    let reflectivity = inputs
        .reflectivity
        .and_then(|reflectivity| FieldSampler::new(sweep, reflectivity));
    let (first_m, spacing_m) = quality.native_geometry(&sweep.range)?;
    let (rows, gates) = quality.shape();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            let Some(score) = quality.value(row, gate) else {
                continue;
            };
            if !score.is_finite() {
                continue;
            }
            let reflectivity_ok = reflectivity.as_ref().is_none_or(|sampler| {
                sampler
                    .sample(row, gate_center_m(first_m, spacing_m, gate))
                    .is_some_and(|dbz| dbz.is_finite() && dbz >= config.minimum_reflectivity_dbz)
            });
            out[row * gates + gate] = if score >= config.minimum_quality && reflectivity_ok {
                1.0
            } else {
                0.0
            };
        }
    }
    Some(f32_field_like(&quality, product, out))
}

fn tds_score_field(
    sweep: &Sweep,
    inputs: &Inputs<'_>,
    product: DerivedSweepProduct,
    config: &DiagnosticConfig,
) -> Option<Field> {
    let reflectivity = inputs.reflectivity?;
    let rho = FieldSampler::new(sweep, inputs.rho?)?;
    let zdr = inputs.zdr.and_then(|zdr| FieldSampler::new(sweep, zdr));
    let (first_m, spacing_m) = reflectivity.native_geometry(&sweep.range)?;
    let (rows, gates) = reflectivity.shape();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            let Some(dbz) = reflectivity.value(row, gate) else {
                continue;
            };
            let range_m = gate_center_m(first_m, spacing_m, gate);
            let Some(rho_hv) = rho.sample(row, range_m) else {
                continue;
            };
            if !dbz.is_finite() || !rho_hv.is_finite() {
                continue;
            }
            if dbz < config.tds_min_reflectivity_dbz || rho_hv > config.tds_rho_ceiling {
                out[row * gates + gate] = 0.0;
                continue;
            }
            let reflectivity_score =
                ((dbz - config.tds_min_reflectivity_dbz) / 30.0).clamp(0.0, 1.0);
            let rho_score = ((config.tds_rho_ceiling - rho_hv) / 0.25).clamp(0.0, 1.0);
            let zdr_score = zdr
                .as_ref()
                .and_then(|sampler| sampler.sample(row, range_m))
                .filter(|value| value.is_finite())
                .map_or(0.5, |value| ((3.0 - value) / 5.0).clamp(0.0, 1.0));
            out[row * gates + gate] =
                100.0 * reflectivity_score * rho_score * (0.5 + 0.5 * zdr_score);
        }
    }
    Some(f32_field_like(reflectivity, product, out))
}

fn hail_score_field(
    sweep: &Sweep,
    inputs: &Inputs<'_>,
    product: DerivedSweepProduct,
    config: &DiagnosticConfig,
) -> Option<Field> {
    let reflectivity = inputs.reflectivity?;
    let rho = inputs.rho.and_then(|rho| FieldSampler::new(sweep, rho));
    let zdr = inputs.zdr.and_then(|zdr| FieldSampler::new(sweep, zdr));
    let (first_m, spacing_m) = reflectivity.native_geometry(&sweep.range)?;
    let (rows, gates) = reflectivity.shape();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            let Some(dbz) = reflectivity.value(row, gate) else {
                continue;
            };
            if !dbz.is_finite() {
                continue;
            }
            if dbz < config.hail_min_reflectivity_dbz {
                out[row * gates + gate] = 0.0;
                continue;
            }
            let range_m = gate_center_m(first_m, spacing_m, gate);
            let reflectivity_score =
                ((dbz - config.hail_min_reflectivity_dbz) / 20.0).clamp(0.0, 1.0);
            let zdr_score = zdr
                .as_ref()
                .and_then(|sampler| sampler.sample(row, range_m))
                .filter(|value| value.is_finite())
                .map_or(0.5, |value| ((2.5 - value) / 3.5).clamp(0.0, 1.0));
            let rho_score = rho
                .as_ref()
                .and_then(|sampler| sampler.sample(row, range_m))
                .filter(|value| value.is_finite())
                .map_or(0.5, |value| ((0.99 - value) / 0.12).clamp(0.0, 1.0));
            out[row * gates + gate] =
                100.0 * reflectivity_score * (0.55 + 0.25 * zdr_score + 0.20 * rho_score);
        }
    }
    Some(f32_field_like(reflectivity, product, out))
}

fn turbulence_proxy_field(
    sweep: &Sweep,
    inputs: &Inputs<'_>,
    product: DerivedSweepProduct,
    texture_config: &TextureConfig,
) -> Option<Field> {
    match (inputs.spectrum_width, inputs.velocity) {
        (None, None) => None,
        (Some(width), None) => Some(remap_field(width, product, |value| value.max(0.0))),
        (None, Some(velocity)) => Some(texture_field(
            sweep,
            velocity,
            product,
            texture_config,
            TexturePeriod::NyquistVelocity,
        )),
        (Some(width), Some(velocity)) => {
            let velocity_texture = texture_field(
                sweep,
                velocity,
                DerivedSweepProduct::VelocityTexture,
                texture_config,
                TexturePeriod::NyquistVelocity,
            );
            let velocity_texture = FieldSampler::new(sweep, &velocity_texture);
            let (rows, gates) = width.shape();
            let mut out = vec![f32::NAN; rows * gates];
            if let (Some(velocity_texture), Some((first_m, spacing_m))) =
                (velocity_texture, width.native_geometry(&sweep.range))
            {
                for row in 0..rows {
                    for gate in 0..gates {
                        let Some(sw) = width.value(row, gate) else {
                            continue;
                        };
                        let texture = velocity_texture
                            .sample(row, gate_center_m(first_m, spacing_m, gate))
                            .filter(|value| value.is_finite())
                            .unwrap_or(0.0);
                        if sw.is_finite() {
                            out[row * gates + gate] = sw.max(0.0).hypot(texture.max(0.0));
                        }
                    }
                }
            }
            Some(f32_field_like(width, product, out))
        }
    }
}

fn remap_field(
    source: &Field,
    product: DerivedSweepProduct,
    transform: impl Fn(f32) -> f32,
) -> Field {
    let (rows, gates) = source.shape();
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            if let Some(value) = source.value(row, gate)
                && value.is_finite()
            {
                out[row * gates + gate] = transform(value);
            }
        }
    }
    f32_field_like(source, product, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_phase_crossing_zero() {
        let mut values = vec![350.0, 355.0, 2.0, 7.0, f32::NAN, 12.0];
        unwrap_phase_in_place(&mut values, 360.0, 8);
        assert!((values[2] - 362.0).abs() < 1.0e-5);
        assert!((values[3] - 367.0).abs() < 1.0e-5);
        assert!((values[5] - 372.0).abs() < 1.0e-5);
    }

    #[test]
    fn unwrap_does_not_bridge_long_missing_phase_gap() {
        let mut values = vec![350.0, 355.0, f32::NAN, f32::NAN, f32::NAN, 2.0, 7.0];
        unwrap_phase_in_place(&mut values, 360.0, 2);
        assert!((values[0] - 350.0).abs() < 1.0e-5);
        assert!((values[1] - 355.0).abs() < 1.0e-5);
        assert!((values[5] - 2.0).abs() < 1.0e-5);
        assert!((values[6] - 7.0).abs() < 1.0e-5);
    }

    #[test]
    fn hampel_filter_removes_spike_in_flat_window() {
        let values = vec![5.0, 5.0, 5.0, 50.0, 5.0, 5.0, 5.0];
        let filtered = hampel_filter(&values, 3, 3.0);
        assert_eq!(filtered[3], 5.0);
    }

    #[test]
    fn s_band_coefficients_match_reference_tables() {
        assert_eq!(RadarBand::S.rain_kdp_coefficients(), (50.70, 0.8500));
        assert_eq!(RadarBand::S.attenuation_coefficients(), (0.04, 0.004));
    }
}
