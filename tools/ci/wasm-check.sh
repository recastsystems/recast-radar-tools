#!/usr/bin/env bash
# `cargo check --target wasm32-unknown-unknown` for every library crate that
# does not need networking (wave 2 plan, G.2 and G.3). Used by
# .github/workflows/ci.yml; run locally with `bash tools/ci/wasm-check.sh`
# (needs cargo-hack and `rustup target add wasm32-unknown-unknown`). This script
# is the definition of the check; docs/design/wasm.md and the README describe it.
#
# - Workspace crates: each checked on its own with default features.
#   cargo-hack runs one `cargo check` per package, so a crate cannot pass only
#   because another package in the same invocation turned on a feature of a
#   shared dependency. New crates are included automatically. Plain
#   `cargo check` builds only lib and bin targets, so dev-dependencies are not
#   built.
# - recast-radar-tools (facade): each feature alone, plus no features and the
#   defaults, except `net` and `full` (which includes `net`).
# - recast-radar-data: networking is its `net` feature (stream E.1), so it is
#   checked with --no-default-features, and with --no-default-features
#   --features async-client (the non-blocking client, which uses the browser's
#   fetch on wasm32); skipped while it has no `net` feature.
# - recast-radar-bench (the benchmark harness binary) is checked with the
#   workspace crates: every crate that does not need the network builds for
#   wasm32 (spec 9.7). Its file reads return I/O errors there.
# - recast-radar-testdata: not checked. It is the test-only corpus fetcher
#   (ureq + rustls, whose `ring` compiles C), never a normal dependency of a
#   library crate.
set -euo pipefail

target=wasm32-unknown-unknown
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

note() {
    if [[ -n "${GITHUB_ACTIONS:-}" ]]; then
        echo "::notice title=wasm32 check::$1"
    else
        echo "note: $1"
    fi
}

# True when the manifest's [features] table defines `net`.
has_net_feature() {
    awk '/^[[:space:]]*\[/ { section = $0 }
         section ~ /^[[:space:]]*\[features\][[:space:]]*$/ && /^[[:space:]]*net[[:space:]]*=/ { found = 1 }
         END { exit !found }' "$1"
}

cargo hack check --locked --target "$target" --workspace \
    --exclude recast-radar-tools \
    --exclude recast-radar-data \
    --exclude recast-radar-testdata

cargo hack check --locked --target "$target" -p recast-radar-tools \
    --each-feature --exclude-features net,full

if has_net_feature crates/recast-radar-data/Cargo.toml; then
    cargo check --locked --target "$target" -p recast-radar-data --no-default-features
    cargo check --locked --target "$target" -p recast-radar-data --no-default-features \
        --features async-client
else
    note "recast-radar-data skipped: it has no \`net\` feature yet (stream E.1)"
fi
