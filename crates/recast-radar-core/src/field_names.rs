//! Research-radar field-name mapping shared by the DORADE and CfRadial
//! decoders.

// Names the legacy `MomentType`; deleted with the legacy model at the end of
// FM301 migration task F.3 (`model::Quantity::classify` replaces it).
#![allow(deprecated)]

use crate::MomentType;

/// Map a DORADE parameter name or CfRadial field name onto the canonical
/// moment set.
///
/// Names come from three generations of writers: solo-era two-letter codes
/// (DZ/VE/SW), RaXPol long names (DBZ/VEL/WIDTH/RHOHV), and Radx `_F`
/// (filtered) / polarization suffixed names (DBZHC_F/VEL_F/ZDR_F/RHOHV_F).
/// Suffixes are stripped iteratively until a stem matches or no suffix
/// remains, so `DBZHC_F` → `DBZHC` → `DBZ`. CfRadial field names follow the
/// same lineage (Radx writes both), so both decoders share this map.
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use model::Quantity::classify")
)]
pub fn canonical_moment(name: &str) -> Option<MomentType> {
    let normalized = name.trim().to_ascii_uppercase();
    let mut stem = normalized.as_str();
    loop {
        if let Some(moment) = match_moment_stem(stem) {
            return Some(moment);
        }
        stem = ["_F", "_HC", "_VC", "HC", "_V", "_H"]
            .iter()
            .find_map(|suffix| stem.strip_suffix(suffix).filter(|rest| !rest.is_empty()))?;
    }
}

fn match_moment_stem(stem: &str) -> Option<MomentType> {
    match stem {
        "DBZ" | "DZ" | "DBZH" | "DBZV" | "REF" | "CZ" | "UZ" => Some(MomentType::Reflectivity),
        "VR" | "VE" | "VEL" | "VU" | "VG" | "VT" => Some(MomentType::Velocity),
        "SW" | "WIDTH" | "SPW" | "SPECTRUM_WIDTH" => Some(MomentType::SpectrumWidth),
        "ZDR" | "ZD" | "UZDR" => Some(MomentType::DifferentialReflectivity),
        "RHOHV" | "RHO" | "RH" | "ROHV" => Some(MomentType::CorrelationCoefficient),
        "PHIDP" | "PHI" | "PH" | "UPHIDP" => Some(MomentType::DifferentialPhase),
        "KDP" | "KD" => Some(MomentType::SpecificDifferentialPhase),
        _ => None,
    }
}
