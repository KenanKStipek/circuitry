#!/usr/bin/env python3
"""Copy Circuitry's reflector prime byte for byte into electricity-compiler.

`core.primes.REFLECTOR_PRIME_V1` is `ReflectorDefinition.prime_template`'s
default (issue #408's Scope section: "`prime_template` defaults to
Circuitry's reflector prime, copied byte for byte by a generator with
`--check`"). Lane C's compiler reads it through
`electricity_compiler::reflector_prime::REFLECTOR_PRIME`
(`include_str!("reflector_prime.txt")`) for a `reflector` effect that
omits `prime_template:`.

Usage: python3 generate_reflector_prime.py [--check]
"""

from __future__ import annotations

import sys
from pathlib import Path

from circuitry.core.primes import REFLECTOR_PRIME_V1

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-compiler"
    / "src"
    / "reflector_prime.txt"
)


def main() -> int:
    text = REFLECTOR_PRIME_V1
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_text() if OUTPUT.exists() else ""
        if current != text:
            print(
                f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr
            )
            return 1
        return 0
    OUTPUT.write_text(text)
    print(f"wrote {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
