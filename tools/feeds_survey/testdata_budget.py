"""The committed-testdata budget across the stream branches (section 4).

recast-radar-testdata's tests/trim.rs caps the committed testdata: the sum of
`size` over every manifest entry with a `committed` path must stay at or under
COMMITTED_TOTAL_CAP_BYTES (60,000,000). The stream branches each add fixtures,
so whether a fixture fits depends on what the other branches have added by the
time they merge. This script recounts that from git, read only (`git ls-tree`
and `git show` of each branch tip; nothing is checked out or written):

- per branch, the bytes its committed entries add to `main` (a committed path
  counted once, whatever its entry id; a path whose size changed counts at its
  new size);
- all branches merged: the union of every committed path, each once;
- the same plus the KXWA prefix that known failure 1 would need
  (17,819,782 bytes), and what a given cap would leave.

Branches default to every local branch but `main`; name them to restrict.

Usage: python tools/feeds_survey/testdata_budget.py [--cap BYTES] [branch ...]
"""

import argparse
import subprocess
import tomllib
from datetime import datetime, timezone

KXWA_PREFIX_BYTES = 17_819_782
TRIM_RS_CAP = 60_000_000


def git(*args):
    return subprocess.run(
        ["git", *args], capture_output=True, text=True, encoding="utf-8", check=True
    ).stdout


def committed(ref):
    """{committed path: size} over every testdata manifest at `ref`."""
    paths = {}
    for name in git("ls-tree", "-r", "--name-only", ref, "--", "testdata").split():
        if not name.endswith("manifest.toml"):
            continue
        data = tomllib.loads(git("show", f"{ref}:{name}"))
        for entry in data.get("file", []):
            if entry.get("committed"):
                # Committed paths are relative to the testdata directory.
                paths[entry["committed"]] = int(entry["size"])
    return paths


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--cap", type=int, default=None,
                        help="a proposed total cap, to print what it would leave")
    parser.add_argument("branches", nargs="*")
    args = parser.parse_args()

    branches = args.branches or [
        b for b in git("for-each-ref", "--format=%(refname:short)", "refs/heads").split()
        if b != "main"
    ]
    now = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%MZ")
    base = committed("main")
    union = dict(base)
    print(f"recounted {now}")
    print(f"main {git('rev-parse', '--short', 'main').strip()}: "
          f"{sum(base.values()):,} bytes in {len(base)} committed files")
    for branch in branches:
        tip = git("rev-parse", "--short", branch).strip()
        paths = committed(branch)
        added = {p: s for p, s in paths.items() if base.get(p) != s}
        removed = [p for p in base if p not in paths]
        print(f"{branch} {tip}: adds {sum(added.values()):,} bytes in {len(added)} files"
              + (f", removes {len(removed)}" if removed else ""))
        for path, size in added.items():
            if path in union and union[path] != size and path not in base:
                print(f"  note: {path} is {union[path]:,} bytes on another branch, {size:,} here")
            union[path] = size
    total = sum(union.values())
    with_prefix = total + KXWA_PREFIX_BYTES
    print(f"all merged: {total:,} bytes in {len(union)} files "
          f"({TRIM_RS_CAP - total:,} under the {TRIM_RS_CAP:,} cap)")
    print(f"with the KXWA prefix: {with_prefix:,} bytes "
          f"({with_prefix - TRIM_RS_CAP:,} over the {TRIM_RS_CAP:,} cap)")
    if args.cap is not None:
        print(f"a {args.cap:,}-byte cap would leave {args.cap - with_prefix:,} bytes "
              f"with the prefix, {args.cap - total:,} without")


if __name__ == "__main__":
    main()
