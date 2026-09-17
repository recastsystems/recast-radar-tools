//! A migrated crate whose dependency (render_t) is not migrated.
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]
pub fn run() -> f32 { render_t::draw() + core_t::geometry() as f32 }
