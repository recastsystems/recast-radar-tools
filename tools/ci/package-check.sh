#!/usr/bin/env bash
# `cargo package --workspace --locked`: a dry run that packages and verifies
# the .crate file of every workspace crate under target/package. Nothing is
# uploaded. Used by .github/workflows/ci.yml; run locally with
# `bash tools/ci/package-check.sh` (extra arguments go to cargo package).
#
# cargo prints two kinds of warning that are expected here; they are counted,
# not shown:
# - "ignoring test/example `x` as `tests/...` (or `examples/...`) is not
#   included in the published package": the crates exclude tests/ and
#   examples/ on purpose (docs/design/packaging.md), because those targets
#   read the repository's test corpus or use path-only dev-dependencies that
#   cargo drops from a package. cargo then drops the targets as well.
# - "manifest has no documentation, homepage or repository": whether a
#   published crate links to the private repository is the owner's decision
#   (same document).
# Any other warning fails the check, as does any error.
#
# A local rerun after the crates changed can verify against the previous
# run: cargo takes the workspace crates a package depends on from a
# temporary registry under the same name and version every run, and keeps
# both their unpacked sources ($CARGO_HOME/registry/src/-<hash>/, one
# directory per worktree's registry) and their builds
# (target/debug/.fingerprint/recast-radar-*). A dependent then fails to
# build against the old API. Remove this worktree's two before rerunning;
# CI starts from neither.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# cargo verifies each crate against the others through a local registry it
# writes to <target>/package/tmp-registry. Once, with a copy left by an
# earlier run in that directory, verification failed with "failed to
# download recast-radar-bzip2 v0.1.0 ... no hash listed" (cargo 1.94,
# Windows) and passed in a fresh target directory. A restored build cache
# could bring such a copy back, so it is removed first and cargo writes a
# fresh one. The target directory is cargo's own (CARGO_TARGET_DIR or
# build.target-dir apply); a --target-dir passed as an argument is not seen
# here.
target_dir="$(cargo metadata --no-deps --format-version 1 |
    sed -E 's/.*"target_directory":"([^"]*)".*/\1/; s#\\\\#/#g')"
if [[ -n "$target_dir" && -d "$target_dir/package/tmp-registry" ]]; then
    rm -rf "$target_dir/package/tmp-registry"
fi

log="$(mktemp)"
trap 'rm -f "$log"' EXIT

status=0
cargo package --workspace --locked "$@" >"$log" 2>&1 || status=$?
if [[ $status -ne 0 ]]; then
    cat "$log"
    echo "package-check: cargo package failed (exit $status)" >&2
    exit "$status"
fi

excluded_target='^warning: ignoring (test|example) `[^`]+` as `(tests|examples)[/\\][^`]+` is not included in the published package$'
no_links='^warning: manifest has no documentation, homepage or repository$'

warnings="$(tr -d '\r' <"$log" | grep '^warning' || true)"
excluded="$(grep -cE "$excluded_target" <<<"$warnings" || true)"
unlinked="$(grep -cE "$no_links" <<<"$warnings" || true)"
unexpected="$(grep -vE "$excluded_target|$no_links" <<<"$warnings" || true)"

tr -d '\r' <"$log" | grep -E '^ +(Packaged|Verifying|Finished)' || true
echo "package-check: $excluded excluded test and example targets left out," \
    "$unlinked crates without documentation/homepage/repository links (expected)"
if [[ -n "$unexpected" ]]; then
    cat "$log"
    echo "package-check: unexpected warnings:" >&2
    echo "$unexpected" >&2
    exit 1
fi
