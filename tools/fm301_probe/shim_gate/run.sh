#!/usr/bin/env bash
# Reproduces the legacy-deprecation gate experiments in docs/design/fm301-model.md 13.4.
# A standalone workspace (not a member of the repository workspace). Only `cargo check` runs.
set -u
cd "$(dirname "$0")"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
CFG="--cfg recast_legacy_deprecation"

gate() {
    RUSTFLAGS="$CFG" cargo check -q "$@" 2>&1 | grep -E "^(warning|error)"
    echo "exit=${PIPESTATUS[0]}"
}

echo "1. no cfg, --workspace"; cargo check -q --workspace; echo "exit=$?"
echo "2. cfg, --workspace (render_t un-migrated: warnings only)"; gate --workspace
echo "3. cfg, -p algo_t (migrated)"; gate -p algo_t
echo "4. cfg, -p bench_t (migrated, depends on un-migrated render_t)"; gate -p bench_t

cp algo/src/lib.rs algo/src/lib.rs.orig
echo 'pub fn stray(g: &core_t::MomentGrid) -> f32 { g.scale }' >> algo/src/lib.rs
echo "5. stray legacy use outside legacy_api in algo_t: -p algo_t"; gate -p algo_t
echo "6. same stray use: -p bench_t"; gate -p bench_t
echo "7. rejected alternative: cfg and -D deprecated on the final crate only (passes vacuously)"
cargo rustc -q -p algo_t --lib --profile check -- $CFG -D deprecated; echo "exit=$?"
mv algo/src/lib.rs.orig algo/src/lib.rs

cp render/Cargo.toml render/Cargo.toml.orig
printf '[lints.rust]\nunexpected_cfgs = { level = "warn", check-cfg = ["cfg(recast_legacy_deprecation)"] }\n' >> render/Cargo.toml
echo "8. [lints] workspace = true combined with [lints.rust]"; cargo check -q -p render_t 2>&1 | tail -1
mv render/Cargo.toml.orig render/Cargo.toml
