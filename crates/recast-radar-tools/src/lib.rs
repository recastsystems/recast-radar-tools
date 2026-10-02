//! Pure-Rust weather radar toolkit.
//!
//! Read NEXRAD Level II and Level III, ODIM_H5, CfRadial 1 and 2 (classic
//! netCDF and netCDF-4), DORADE and JMA radar files into one data model that
//! follows WMO FM301 (CfRadial 2), write that model as NEXRAD Level II,
//! CfRadial 1, CfRadial 2 / FM301 and ODIM_H5, fetch radar data from AWS and
//! other public feeds, dealias velocity, filter, compute derived products
//! and composites, track storm cells, and render sweeps to PNG. There is no
//! unsafe code and, without the `net` feature, no C in the build.
//!
//! This facade re-exports the `recast-radar-*` crates as modules, each
//! behind a Cargo feature, so an application depends on one crate and picks
//! the parts it needs. The data model ([`model`]) is always available.
//!
//! # Quick start
//!
//! ```no_run
//! # #[cfg(feature = "io")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use recast_radar_tools::io;
//! use recast_radar_tools::model::Quantity;
//!
//! // Any supported format: Level II, Level III, ODIM_H5, CfRadial 1 and 2,
//! // DORADE, JMA GRIB2 tar, optionally inside gzip or a single-file ZIP.
//! let bytes = std::fs::read("KTLX20240315_000217_V06")?;
//! let volume = io::read_supported_volume_bytes(&bytes)?;
//!
//! for sweep in &volume.sweeps {
//!     if let Some(reflectivity) = sweep.find(Quantity::Reflectivity) {
//!         // Physical value (dBZ) of ray 0, gate 100; `None` for no data,
//!         // below-threshold and range-folded gates.
//!         println!("{:.2} deg: {:?}", sweep.fixed_angle_deg, reflectivity.value(0, 100));
//!     }
//! }
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "io"))]
//! # fn main() {}
//! ```
//!
//! The user guide in the repository's `docs/guide/` walks through reading,
//! writing, the data model, fetching, processing and rendering, with a
//! runnable example for each (`crates/recast-radar-tools/examples/`). The
//! `recast-radar` command (`recast-radar-cli`) and the Python package
//! `recast_radar` (`recast-radar-py`) are built on the same crates.
//!
//! # Modules and features
//!
//! | Feature | Module | Crate | Contents |
//! |---|---|---|---|
//! | (always) | [`model`] | `recast-radar-core` | data model, FM301 view, beam geometry, field names, decode limits |
//! | `nexrad` | `nexrad` | `recast-radar-io-nexrad` | NEXRAD Archive II (Level II) |
//! | `write` | `nexrad::write` | `recast-radar-io-nexrad` | the Level II writer: Archive II, real-time chunks, polling directories |
//! | `level3` | `level3` | `recast-radar-io-level3` | NEXRAD and TDWR Level III products |
//! | `odim` | `odim` | `recast-radar-io-odim` | ODIM_H5 reader and writer |
//! | `cfradial` | `cfradial` | `recast-radar-io-cfradial` | CfRadial 1 and CfRadial 2 / FM301 readers and writers |
//! | `hdf5` | `hdf5` | `recast-radar-hdf5` | HDF5 reader and writer, the netCDF-4 data model |
//! | `dorade` | `dorade` | `recast-radar-io-dorade` | DORADE, mobile-radar archives |
//! | `jma` | `jma` | `recast-radar-io-jma` | JMA polar GRIB2 tar |
//! | `bufr` | `bufr` | `recast-radar-io-bufr` | WMO BUFR, Meteo-France PAG and PAM radar files |
//! | `io` | `io` | `recast-radar-io` | format-sniffing router; enables every format feature |
//! | `net` | `data` | `recast-radar-data` | AWS archive and real-time chunks, feeds |
//! | `correct` | `correct` | `recast-radar-correct` | velocity dealiasing |
//! | `filters` | `filters` | `recast-radar-filters` | gate filters, smoothing, interpolation |
//! | `retrieve` | `retrieve` | `recast-radar-retrieve` | derived products, VAD, GBVTD, rotation |
//! | `map` | `map` | `recast-radar-map` | composites, cross sections, RHI, resampling |
//! | `track` | `track` | `recast-radar-track` | cell tracking, swaths, temporal grids |
//! | `render` | `render` | `recast-radar-render` | CPU raster, PNG, color tables |
//! | `scattering` | `scattering` | `recast-radar-scattering` | scattering primitives, LUTs |
//! | `serde` | | | `Serialize` and `Deserialize` on the data model (`recast-radar-core/serde`) |
//! | `full` | | | all of the above |
//!
//! Default features: `io`, `correct`, `filters`, `retrieve`, `map`.
//!
//! A feature also enables the features of the member crates its crate
//! depends on (for example `track` enables `correct`, `map` and `retrieve`),
//! so every type a module's API names can be named through this crate.
//!
//! # Conventions
//!
//! - Decoders are named `read_*` and return a [`model::Volume`] (or, for
//!   Level III, a product): `nexrad::read_volume_from_path`,
//!   `odim::read_odim_h5_volume`, `io::read_supported_volume_bytes`. Byte
//!   entry points work everywhere, including `wasm32-unknown-unknown`;
//!   path entry points need a file system.
//! - Every decoder bounds what untrusted input can make it allocate and
//!   reports a typed error instead of panicking. Error enums and most other
//!   public enums are `#[non_exhaustive]`: match them with a wildcard arm.
//! - Fields keep the source's packed values; [`model::Field::value`] and
//!   [`model::Field::to_physical`] give physical values on demand.

// No code of its own, but the rule every library crate root states.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub use recast_radar_core as model;

#[cfg(feature = "hdf5")]
pub use recast_radar_hdf5 as hdf5;
#[cfg(feature = "io")]
pub use recast_radar_io as io;
#[cfg(feature = "bufr")]
pub use recast_radar_io_bufr as bufr;
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
