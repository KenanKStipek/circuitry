#!/usr/bin/env python3
"""Make the bundled orchestration reference byte-identical to ``docs/``.

``docs/orchestration-reference.md`` is the single source of truth; the copy
shipped in the wheel at ``src/circuitry/bundled/docs/orchestration-reference.md``
exists only so the installed package carries the reference offline, and must
never drift from it (#233). This script is the only thing allowed to write
the bundled copy.

    python scripts/sync-bundled-docs.py           # write the bundled copy
    python scripts/sync-bundled-docs.py --check    # exit 1 if it would change

Standard library only.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

SOURCE = REPO_ROOT / "docs" / "orchestration-reference.md"
BUNDLED = REPO_ROOT / "src" / "circuitry" / "bundled" / "docs" / "orchestration-reference.md"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit 1 if the bundled copy would change, without writing it",
    )
    args = parser.parse_args()

    source_text = SOURCE.read_text(encoding="utf-8")
    current_text = BUNDLED.read_text(encoding="utf-8") if BUNDLED.exists() else None

    if args.check:
        if current_text != source_text:
            print(
                f"{BUNDLED} is out of sync with {SOURCE} — "
                "run scripts/sync-bundled-docs.py",
                file=sys.stderr,
            )
            return 1
        return 0

    BUNDLED.write_text(source_text, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
