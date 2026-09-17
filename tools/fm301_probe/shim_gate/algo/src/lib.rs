//! A migrated crate: denies deprecated uses under the cfg; legacy signatures live in legacy_api.
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]
use core_t::Field;
pub fn native(f: &Field) -> f32 { f.scale }
#[allow(deprecated)]
pub mod legacy_api {
    use core_t::MomentGrid;
    #[cfg_attr(recast_legacy_deprecation, deprecated(note = "use native"))]
    pub fn wrapper(g: &MomentGrid) -> MomentGrid {
        let f = core_t::Field::from(g.clone());
        core_t::legacy::from_field(&core_t::Field { scale: super::native(&f) })
    }
    #[cfg_attr(recast_legacy_deprecation, deprecated(note = "use read_volume"))]
    pub fn decode() -> core_t::RadarVolume { core_t::RadarVolume { cuts: vec![MomentGrid { scale: 1.0 }] } }
}
#[allow(deprecated)]
pub use legacy_api::*;
