#!/usr/bin/env python3
"""Copy Circuitry's decomposition planner byte for byte into electricity-vm.

`core/decompose.py::_planner_path` reads
``src/circuitry/curation/agents/decompose.yml`` by default; electricity has
no package-resource loader of its own, so `electricity_vm::decompose::
PLANNER_YAML` embeds a verbatim copy with `include_str!`, kept in sync by
this generator (the same pattern `generate_schema_copy.py`/
`generate_reflector_prime.py` use). Compared and written as bytes, not text
(the file holds non-ASCII em dashes): a text-mode round trip would normalize
line endings and silently pass `--check` despite a real divergence.

Usage: python3 generate_decompose_planner.py [--check]
"""

from __future__ import annotations

import sys
from pathlib import Path

_ELECTRICITY_DIR = Path(__file__).resolve().parent.parent
SOURCE = (
    _ELECTRICITY_DIR.parent
    / "src"
    / "circuitry"
    / "curation"
    / "agents"
    / "decompose.yml"
)
OUTPUT = (
    _ELECTRICITY_DIR
    / "crates"
    / "electricity-vm"
    / "src"
    / "decompose_planner.yml"
)


def main() -> int:
    data = SOURCE.read_bytes()
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_bytes() if OUTPUT.exists() else b""
        if current != data:
            print(f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr)
            return 1
        return 0
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_bytes(data)
    print(f"wrote {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
