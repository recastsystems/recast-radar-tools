#!/usr/bin/env bash
# Fails if the facade without `net` would compile C or C++ for any target
# (wave 1 Global Constraints "Pure Rust"; README "Pure Rust"). Used by
# .github/workflows/ci.yml; run locally with `bash tools/ci/pure-rust-check.sh`.
#
# Resolves the dependency graph of recast-radar-tools with every facade feature
# except `net` and `full` (which includes `net`), for all targets
# (`--target all`), with normal and build dependencies, and fails if it
# contains `cc` or `cmake`, the crates that drive C and C++ builds. Features
# are additive, so the graph for any smaller feature set is a subgraph of this
# one. The member crates are enabled with their default features.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
manifest=crates/recast-radar-tools/Cargo.toml

# Feature names in the facade's [features] table, minus default, net and full.
features=$(awk '
    /^[[:space:]]*\[/ { in_features = ($0 ~ /^[[:space:]]*\[features\][[:space:]]*$/); next }
    in_features && /^[A-Za-z0-9_-]+[[:space:]]*=/ {
        name = $0
        sub(/[[:space:]]*=.*/, "", name)
        if (name != "default" && name != "net" && name != "full") {
            list = list (list == "" ? "" : ",") name
        }
    }
    END { print list }' "$manifest")

if [[ -z "$features" ]]; then
    echo "error: no features found in $manifest" >&2
    exit 1
fi
echo "facade features: $features"

packages=$(cargo tree --locked -p recast-radar-tools --no-default-features \
    --features "$features" -e build,normal --target all --prefix none)

if c_builders=$(grep -E '^(cc|cmake) v' <<<"$packages"); then
    echo "error: C/C++ build crates in the graph without \`net\`:" >&2
    echo "$c_builders" | sort -u >&2
    echo "Find the path with: cargo tree -p recast-radar-tools --no-default-features --features $features -e build,normal --target all -i cc" >&2
    exit 1
fi
echo "ok: no cc or cmake in $(sort -u <<<"$packages" | wc -l) packages (all targets)"
