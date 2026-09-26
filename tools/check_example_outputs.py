"""Run the facade examples whose output README.md and docs/guide/*.md show,
and fail when an output shown there differs from what the example prints.

Each ```text block that shows an example's output is preceded by a marker:

    <!-- output: <example> <arg>... -->       the block is the whole stdout
    <!-- output-head: <example> <arg>... -->  the block is the first lines
    <!-- output-unchecked: <example> <why> --> not run (a live download)

Arguments: `testdata:<id>` is the verified local path of that testdata entry
(downloaded into the cache when it is not there, as the tests do; see
recast-radar-testdata), `repo:<path>` a path in this repository, and
`dir:<name>` an empty directory created for the run and passed as `<name>`.
Anything else is passed as written. Each example runs in a fresh empty
directory, so relative paths in its output read as in the documents. On
Windows, `\\` in the output is compared as `/`.

crates/recast-radar-tools/tests/readme.rs checks that every ```text block of
those documents has a marker naming an existing example; this script checks
the outputs. It builds the examples with the release profile (the
CARGO_PROFILE_RELEASE_* environment variables apply, and the printed output
does not depend on them) and the testdata-path tool.

    python tools/check_example_outputs.py

CI runs it in the `examples` job.
"""

import json
import os
import pathlib
import shlex
import subprocess
import sys
import tempfile
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent
FACADE = "recast-radar-tools"
MARKERS = {
    "<!-- output: ": "exact",
    "<!-- output-head: ": "head",
    "<!-- output-unchecked: ": "unchecked",
}


def documents():
    return [ROOT / "README.md", *sorted((ROOT / "docs" / "guide").glob("*.md"))]


def output_blocks(document):
    """(line number, mode, example, args, expected lines) of each marked block."""
    lines = document.read_text(encoding="utf-8").replace("\r\n", "\n").split("\n")
    blocks = []
    for i, line in enumerate(lines):
        stripped = line.strip()
        mode = next((m for prefix, m in MARKERS.items() if stripped.startswith(prefix)), None)
        if mode is None:
            continue
        if not stripped.endswith(" -->"):
            sys.exit(f"{document}:{i + 1}: unterminated output marker")
        body = stripped[stripped.index(": ") + 2 : -len(" -->")]
        words = shlex.split(body)
        if not words:
            sys.exit(f"{document}:{i + 1}: the marker names no example")
        if i + 1 >= len(lines) or lines[i + 1].strip() != "```text":
            sys.exit(f"{document}:{i + 1}: the marker must be followed by ```text")
        end = i + 2
        while end < len(lines) and lines[end].strip() != "```":
            end += 1
        if end == len(lines):
            sys.exit(f"{document}:{i + 1}: unclosed ```text block")
        blocks.append((i + 1, mode, words[0], words[1:], lines[i + 2 : end]))
    return blocks


def cargo_build(args):
    """Build with cargo and return {target name: executable path}."""
    command = ["cargo", "build", "--release", "--locked", "--message-format=json-render-diagnostics", *args]
    print("+", " ".join(command), flush=True)
    result = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE, check=False)
    if result.returncode != 0:
        sys.exit(f"cargo build failed ({result.returncode})")
    executables = {}
    for line in result.stdout.decode("utf-8").splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("executable"):
            executables[message["target"]["name"]] = message["executable"]
    return executables


def required_features(examples):
    manifest = tomllib.loads((ROOT / "crates" / FACADE / "Cargo.toml").read_text(encoding="utf-8"))
    declared = {entry["name"]: entry.get("required-features", []) for entry in manifest.get("example", [])}
    features = set()
    for example in examples:
        if example not in declared:
            sys.exit(f"{example}: not an example of {FACADE}")
        features.update(declared[example])
    return sorted(features)


def testdata_path(tool, identifier):
    result = subprocess.run([tool, identifier], cwd=ROOT, capture_output=True, check=False)
    if result.returncode != 0:
        sys.exit(result.stderr.decode("utf-8", "replace").strip() or f"testdata-path {identifier} failed")
    return result.stdout.decode("utf-8").strip()


def main():
    checks = []
    for document in documents():
        for line, mode, example, args, expected in output_blocks(document):
            name = f"{document.relative_to(ROOT).as_posix()}:{line}"
            if mode == "unchecked":
                print(f"skip {name}: {example} ({' '.join(args)})")
                continue
            checks.append((name, mode, example, args, expected))
    if not checks:
        sys.exit("no output markers found")

    examples = sorted({example for _, _, example, _, _ in checks})
    features = required_features(examples)
    built = cargo_build(
        ["-p", FACADE, *[f"--example={example}" for example in examples]]
        + ([f"--features={','.join(features)}"] if features else [])
    )
    tool = cargo_build(["-p", "recast-radar-testdata", "--bin", "testdata-path"])["testdata-path"]

    failures = 0
    for name, mode, example, args, expected in checks:
        with tempfile.TemporaryDirectory() as scratch:
            argv = [built[example]]
            for arg in args:
                if arg.startswith("testdata:"):
                    argv.append(testdata_path(tool, arg[len("testdata:") :]))
                elif arg.startswith("repo:"):
                    argv.append(str(ROOT / arg[len("repo:") :]))
                elif arg.startswith("dir:"):
                    (pathlib.Path(scratch) / arg[len("dir:") :]).mkdir(parents=True)
                    argv.append(arg[len("dir:") :])
                else:
                    argv.append(arg)
            result = subprocess.run(argv, cwd=scratch, capture_output=True, check=False)
        stdout = result.stdout.decode("utf-8").replace("\r\n", "\n")
        if os.name == "nt":
            stdout = stdout.replace("\\", "/")
        actual = stdout.rstrip("\n").split("\n")
        shown = actual[: len(expected)] if mode == "head" else actual
        if result.returncode != 0 or shown != expected:
            failures += 1
            print(f"FAIL {name}: {example} {' '.join(args)} (exit {result.returncode})")
            print("  shown in the document:")
            print("".join(f"    {line}\n" for line in expected), end="")
            print("  printed:")
            print("".join(f"    {line}\n" for line in shown), end="")
            stderr = result.stderr.decode("utf-8", "replace").strip()
            if stderr:
                print(f"  stderr: {stderr}")
        else:
            print(f"ok   {name}: {example}")
    if failures:
        sys.exit(f"{failures} example output(s) differ from the documents")


if __name__ == "__main__":
    main()
