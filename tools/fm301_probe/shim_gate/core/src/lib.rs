//! Stand-in for recast-radar-core during F.2/F.3: legacy types with cfg-gated deprecation,
//! conversions inside the allow(deprecated) legacy module, geometry left at the root.
#[allow(deprecated)]
pub mod legacy {
    #[cfg_attr(recast_legacy_deprecation, deprecated(note = "legacy"))]
    #[derive(Clone, Debug, PartialEq)]
    pub struct MomentGrid { pub scale: f32 }
    #[cfg_attr(recast_legacy_deprecation, deprecated(note = "legacy"))]
    #[derive(Clone, Debug, PartialEq)]
    pub struct RadarVolume {
        pub cuts: Vec<MomentGrid>,
    }
    #[cfg_attr(recast_legacy_deprecation, deprecated(note = "legacy"))]
    pub fn merge(parts: Vec<MomentGrid>) -> Option<MomentGrid> { parts.into_iter().next() }
    pub fn from_field(f: &crate::model::Field) -> MomentGrid { MomentGrid { scale: f.scale } }
    impl From<MomentGrid> for crate::model::Field {
        fn from(g: MomentGrid) -> Self { crate::model::Field { scale: g.scale } }
    }
}
pub mod model {
    pub struct Field { pub scale: f32 }
}
pub fn geometry() -> f64 { 1.0 }
#[allow(deprecated)]
pub use legacy::*;
pub use model::Field;
