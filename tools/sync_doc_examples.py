"""Copy the facade examples into the README and the user guide.

Every ```rust block preceded by `<!-- example: <path> -->` in README.md and
docs/guide/*.md is replaced by the current contents of <path>.
crates/recast-radar-tools/tests/readme.rs fails when a copy differs from its
file; run this script (python tools/sync_doc_examples.py) after editing an
example.
"""

import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
MARKER = "<!-- example: "


def sync(document: pathlib.Path) -> bool:
    text = document.read_text(encoding="utf-8").replace("\r\n", "\n")
    lines = text.split("\n")
    out = []
    i = 0
    while i < len(lines):
        line = lines[i]
        out.append(line)
        stripped = line.strip()
        if stripped.startswith(MARKER) and stripped.endswith(" -->"):
            path = stripped[len(MARKER) : -len(" -->")]
            if i + 1 >= len(lines) or lines[i + 1].strip() != "```rust":
                sys.exit(f"{document}: {path}: the marker must be followed by ```rust")
            end = i + 2
            while end < len(lines) and lines[end].strip() != "```":
                end += 1
            if end == len(lines):
                sys.exit(f"{document}: {path}: unclosed code block")
            code = (ROOT / path).read_text(encoding="utf-8").replace("\r\n", "\n")
            out.append("```rust")
            out.extend(code.rstrip("\n").split("\n"))
            out.append("```")
            i = end + 1
            continue
        i += 1
    new = "\n".join(out)
    if new != text:
        document.write_text(new, encoding="utf-8", newline="\n")
        return True
    return False


def main() -> None:
    documents = [ROOT / "README.md", *sorted((ROOT / "docs" / "guide").glob("*.md"))]
    for document in documents:
        if sync(document):
            print(f"updated {document.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
