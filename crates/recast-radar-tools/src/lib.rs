//! Pure-Rust weather radar toolkit.
//!
//! This facade re-exports the `recast-radar-*` crates as modules, each behind
//! a Cargo feature, so an application depends on one crate and picks the parts
//! it needs. The data model ([`core`]) is always available.
//!
//! | Feature | Module | Crate | Contents |
//! |---|---|---|---|
//! | (always) | [`core`] | `recast-radar-core` | data model, geometry, field names |
//! | `nexrad` | `nexrad` | `recast-radar-io-nexrad` | NEXRAD Archive II (Level II) |
//! | `level3` | `level3` | `recast-radar-io-level3` | NEXRAD and TDWR Level III products |
//! | `odim` | `odim` | `recast-radar-io-odim` | ODIM_H5 |
//! | `cfradial` | `cfradial` | `recast-radar-io-cfradial` | CfRadial 1 |
//! | `dorade` | `dorade` | `recast-radar-io-dorade` | DORADE, mobile-radar archives |
//! | `jma` | `jma` | `recast-radar-io-jma` | JMA polar GRIB2 tar |
//! | `io` | `io` | `recast-radar-io` | format-sniffing router; enables every format feature |
//! | `net` | `data` | `recast-radar-data` | AWS archive and real-time chunks, feeds |
//! | `correct` | `correct` | `recast-radar-correct` | velocity dealiasing |
//! | `filters` | `filters` | `recast-radar-filters` | gate filters, smoothing, interpolation |
//! | `retrieve` | `retrieve` | `recast-radar-retrieve` | derived products, VAD, GBVTD, rotation |
//! | `map` | `map` | `recast-radar-map` | composites, cross sections, RHI, resampling |
//! | `track` | `track` | `recast-radar-track` | cell tracking, swaths, temporal grids |
//! | `render` | `render` | `recast-radar-render` | CPU raster, PNG, color tables |
//! | `scattering` | `scattering` | `recast-radar-scattering` | scattering primitives, LUTs |
//! | `serde` | | | placeholder: forwards to `recast-radar-core/serde`, which does nothing yet |
//! | `full` | | | all of the above |
//!
//! Default features: `io`, `correct`, `filters`, `retrieve`, `map`.
//!
//! A feature also enables the features of the member crates its crate
//! depends on (for example `track` enables `correct`, `map` and `retrieve`),
//! so every type a module's API names can be named through this crate.

pub use recast_radar_core as core;

#[cfg(feature = "io")]
pub use recast_radar_io as io;
#[cfg(feature = "cfradial")]
pub use recast_radar_io_cfradial as cfradial;
#[cfg(feature = "dorade")]
pub use recast_radar_io_dorade as dorade;
#[cfg(feature = "jma")]
pub use recast_radar_io_jma as jma;
#[cfg(feature = "level3")]
pub use recast_radar_io_level3 as level3;
#[cfg(feature = "nexrad")]
pub use recast_radar_io_nexrad as nexrad;
#[cfg(feature = "odim")]
pub use recast_radar_io_odim as odim;

#[cfg(feature = "net")]
pub use recast_radar_data as data;

#[cfg(feature = "correct")]
pub use recast_radar_correct as correct;
#[cfg(feature = "filters")]
pub use recast_radar_filters as filters;
#[cfg(feature = "map")]
pub use recast_radar_map as map;
#[cfg(feature = "render")]
pub use recast_radar_render as render;
#[cfg(feature = "retrieve")]
pub use recast_radar_retrieve as retrieve;
#[cfg(feature = "scattering")]
pub use recast_radar_scattering as scattering;
#[cfg(feature = "track")]
pub use recast_radar_track as track;
