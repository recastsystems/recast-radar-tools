"""The bundled Rust CLI; no external executable is required."""
from __future__ import annotations

import sys
from . import _native


def main() -> int:
    """Run recast-radar and return its exit status."""
    return _native._cli(sys.argv[1:])
