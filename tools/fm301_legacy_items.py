#!/usr/bin/env python3
"""FM301 shim acceptance check (docs/design/fm301-model.md section 13.4, F.2).

`crates/recast-radar-core/src/legacy.rs` must hold exactly the legacy model
items of `lib.rs` at the pre-migration commit, with each definition and impl
unchanged apart from the added `cfg_attr(recast_legacy_deprecation, ...)`
attribute and the module header. `lib.rs` must keep the non-model items
(geometry constants and functions and their tests) unchanged.

The check splits both files into top-level items (doc comments and attributes
included) and the test modules into test functions, strips the added
`cfg_attr` attributes, and compares item texts.

Usage: python tools/fm301_legacy_items.py [base-commit]   (default 1989a03)
Exit status 0 when every check passes.
"""

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORE = "crates/recast-radar-core/src"
DEPRECATION = re.compile(
    r"#\[cfg_attr\(\s*recast_legacy_deprecation\s*,\s*deprecated\(note = \"[^\"]*\"\)\s*,?\s*\)\]\n",
    re.MULTILINE,
)
# Items that stay in lib.rs (not model items).
NON_MODEL = {
    "const EARTH_RADIUS_M",
    "const EFFECTIVE_EARTH_RADIUS_M",
    "fn beam_height_above_radar_m",
    "fn beam_ground_range_m",
}
NON_MODEL_TESTS = {
    "beam_height_matches_four_thirds_earth_reference",
    "ground_range_close_to_slant_range_at_low_tilt",
}


def git_show(commit: str, path: str) -> str:
    return subprocess.run(
        ["git", "-C", str(ROOT), "show", f"{commit}:{path}"],
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    ).stdout


def split_items(text: str, indent: str = "") -> list[str]:
    """Split source into items at `indent` depth: leading `///` docs and `#[..]`
    attributes plus the item through its closing brace or semicolon."""
    lines = text.split("\n")
    items, current, depth = [], [], 0
    for line in lines:
        stripped = line.strip()
        if depth == 0 and not current and (
            stripped == ""
            or (stripped.startswith("//") and not stripped.startswith("///"))
            or stripped.startswith("#![")  # module-level inner attribute (header)
        ):
            continue
        current.append(line)
        # Count braces outside strings and comments (good enough for this code).
        code = re.sub(r'"(\\.|[^"\\])*"', '""', line)
        code = re.sub(r"'(\\.|[^'\\])'", "''", code)
        code = code.split("//")[0]
        depth += code.count("{") + code.count("(") + code.count("[")
        depth -= code.count("}") + code.count(")") + code.count("]")
        if depth == 0 and stripped and not stripped.startswith(("///", "#[", "//")):
            if stripped.endswith(("}", ";", "};")):
                items.append("\n".join(current))
                current = []
    if current and any(l.strip() for l in current):
        items.append("\n".join(current))
    return items


def item_key(item: str) -> str:
    for line in item.split("\n"):
        stripped = line.strip()
        if stripped.startswith(("///", "#[", "//!")) or not stripped:
            continue
        m = re.match(r"(?:pub(?:\([^)]*\))?\s+)?(struct|enum|fn|const|impl|mod|use|type|trait)\b\s*(.*)", stripped)
        if not m:
            return stripped
        kind, rest = m.groups()
        if kind in ("struct", "enum", "fn", "const", "mod", "type", "trait"):
            name = re.match(r"([A-Za-z_][A-Za-z0-9_]*)", rest)
            return f"{kind} {name.group(1) if name else rest}"
        return f"{kind} {rest.rstrip('{').strip()}"
    return item


def tests_module(items: list[str]) -> tuple[list[str], str | None]:
    """(non-test items, test module text)."""
    rest, module = [], None
    for item in items:
        if item_key(item) == "mod tests":
            module = item
        else:
            rest.append(item)
    return rest, module


def test_functions(module: str) -> dict[str, str]:
    body = module.split("\n")
    # Drop `#[cfg(test)]`, `mod tests {`, the closing brace and `use` lines.
    start = next(i for i, l in enumerate(body) if l.startswith("mod tests"))
    inner = "\n".join(l[4:] if l.startswith("    ") else l for l in body[start + 1 : -1])
    functions = {}
    for item in split_items(inner):
        key = item_key(item)
        if key.startswith("use "):
            continue
        functions[key] = item
    return functions


def main() -> int:
    base = sys.argv[1] if len(sys.argv) > 1 else "1989a03"
    old_lib = git_show(base, f"{CORE}/lib.rs")
    new_lib = (ROOT / CORE / "lib.rs").read_text(encoding="utf-8")
    legacy = (ROOT / CORE / "legacy.rs").read_text(encoding="utf-8")
    legacy_stripped = DEPRECATION.sub("", legacy)
    failures = []

    old_items, old_tests = tests_module(split_items(old_lib))
    old_items = [i for i in old_items if not item_key(i).startswith(("use ", "mod ", "pub use"))]
    old_by_key = {item_key(i): i for i in old_items}
    model_keys = [k for k in old_by_key if k not in NON_MODEL]

    legacy_items, legacy_tests = tests_module(split_items(legacy_stripped))
    legacy_items = [
        i for i in legacy_items if not item_key(i).startswith(("use ", "mod ", "pub use"))
    ]
    legacy_by_key = {item_key(i): i for i in legacy_items}

    for key in model_keys:
        if key not in legacy_by_key:
            failures.append(f"legacy.rs is missing model item `{key}`")
        elif legacy_by_key[key] != old_by_key[key]:
            failures.append(f"legacy.rs item `{key}` differs from {base}")
    for key in legacy_by_key:
        if key not in model_keys:
            failures.append(f"legacy.rs has an item that is not a {base} model item: `{key}`")

    new_items, new_tests = tests_module(split_items(new_lib))
    new_by_key = {item_key(i): i for i in new_items}
    for key in NON_MODEL:
        if new_by_key.get(key) != old_by_key.get(key):
            failures.append(f"lib.rs item `{key}` differs from {base}")

    old_fns = test_functions(old_tests or "")
    legacy_fns = test_functions(legacy_tests or "mod tests {\n}")
    new_fns = test_functions(new_tests or "mod tests {\n}")
    for key, text in old_fns.items():
        name = key.split(" ", 1)[1]
        target = new_fns if name in NON_MODEL_TESTS else legacy_fns
        where = "lib.rs" if name in NON_MODEL_TESTS else "legacy.rs"
        if target.get(key) != text:
            failures.append(f"{where} test item `{key}` missing or changed")
    for key in legacy_fns:
        if key not in old_fns:
            failures.append(f"legacy.rs has a test item not in {base}: `{key}`")

    deprecated = len(DEPRECATION.findall(legacy))
    public = sum(1 for k in model_keys if old_by_key[k].lstrip().split("\n")[-1] and re.search(
        r"^pub (struct|enum|fn|const) ", old_by_key[k], re.MULTILINE))
    if deprecated != public:
        failures.append(f"{deprecated} cfg_attr deprecations for {public} public model items")

    print(f"base {base}: {len(model_keys)} model items, {len(old_fns)} test items, "
          f"{deprecated} deprecation attributes")
    for failure in failures:
        print("FAIL:", failure)
    if not failures:
        print("OK: legacy.rs holds exactly the model items, unchanged apart from cfg_attr lines")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
