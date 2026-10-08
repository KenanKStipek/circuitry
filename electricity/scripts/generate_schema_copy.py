#!/usr/bin/env python3
"""Keep electricity-schema's bundled schema files identical to Circuitry's.

Copies every ``*.json`` file in ``src/circuitry/schema/`` byte for byte into
``electricity/crates/electricity-schema/schema/`` -- the same pattern as
Circuitry's own bundled-docs sync (``scripts/sync-bundled-docs.py``), so
electricity-schema stays self-contained (DESIGN.md §4 step 3, §17) without a
hand-ported, independently-drifting copy. The moment the two directories
disagree, ``--check`` fails loudly rather than letting `electricity-schema`
silently produce a different accept/reject verdict than `cof check`.

Usage: python3 generate_schema_copy.py [--check]
"""

from __future__ import annotations

import sys
from pathlib import Path

_ELECTRICITY_DIR = Path(__file__).resolve().parent.parent
_SOURCE_DIR = _ELECTRICITY_DIR.parent / "src" / "circuitry" / "schema"
_DEST_DIR = _ELECTRICITY_DIR / "crates" / "electricity-schema" / "schema"


def main() -> int:
    check = "--check" in sys.argv[1:]
    sources = sorted(_SOURCE_DIR.glob("*.json"))
    if not sources:
        print(f"{_SOURCE_DIR}: no *.json schema files found", file=sys.stderr)
        return 1

    stale: list[str] = []
    wanted_names = set()
    for source in sources:
        wanted_names.add(source.name)
        dest = _DEST_DIR / source.name
        text = source.read_text(encoding="utf-8")
        current = dest.read_text(encoding="utf-8") if dest.exists() else None
        if current != text:
            stale.append(str(dest))
            if not check:
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_text(text, encoding="utf-8")

    extra = (
        [str(p) for p in _DEST_DIR.glob("*.json") if p.name not in wanted_names]
        if _DEST_DIR.exists()
        else []
    )
    if extra:
        stale.extend(extra)
        if not check:
            for path_str in extra:
                Path(path_str).unlink()

    if check:
        if stale:
            for path in stale:
                print(f"{path} is stale; run without --check to regenerate", file=sys.stderr)
            return 1
        return 0

    if stale:
        print(f"wrote {len(stale)} file(s) to {_DEST_DIR}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
