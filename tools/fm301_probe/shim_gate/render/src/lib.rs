//! An un-migrated crate: uses legacy types, a wrapper, and field accesses without naming types.
pub fn draw() -> f32 {
    let v = algo_t::decode();          // no type named
    let n = v.cuts.len() as f32;        // field access only
    n + algo_t::wrapper(&v.cuts[0]).scale
}
pub fn draw2(g: &core_t::MomentGrid) -> f32 { g.scale }
