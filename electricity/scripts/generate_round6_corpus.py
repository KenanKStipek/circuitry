#!/usr/bin/env python3
"""A differential corpus for ``electricity_value::pycompat::round6``.

``round(x, 6)`` is CPython's own correctly-rounded, half-even-on-the-exact-
binary-value float rounding (issue #449's gate lane, item 10) -- this
generator asks CPython itself for the right answer on a mix of edge cases
and random floats (both "ordinary" magnitudes and raw random bit patterns),
so the Rust port is checked against the real builtin, not a re-derivation of
its rounding rule.

Each float is recorded by its IEEE-754 bit pattern (hex), not its decimal
text, so the golden file carries the *exact* value verbatim regardless of
how any particular language chooses to print it.

Usage: python3 generate_round6_corpus.py [--check]
"""

from __future__ import annotations

import json
import math
import random
import struct
import sys
from pathlib import Path
from typing import Any

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-value"
    / "tests"
    / "golden"
    / "round6_corpus.json"
)

_EDGE_CASES = [
    0.0,
    -0.0,
    1.0,
    -1.0,
    0.5,
    -0.5,
    2.675,
    -2.675,
    1.0000005,
    0.9999995,
    0.0000005,
    -0.0000005,
    0.00000049999999999,
    0.123456789,
    -0.123456789,
    1e-10,
    -1e-10,
    1e10,
    -1e10,
    123456.789123456,
    -123456.789123456,
    1.5e300,
    -1.5e300,
    5e-324,  # smallest positive subnormal
    -5e-324,
    1.7976931348623157e308,  # near f64::MAX
    -1.7976931348623157e308,
]


def _hexbits(x: float) -> str:
    return struct.pack("<d", x).hex()


def _random_ordinary_floats(rng: random.Random, count: int) -> list[float]:
    values = []
    for _ in range(count):
        exponent = rng.uniform(-20, 20)
        mantissa = rng.uniform(-1.0, 1.0)
        values.append(mantissa * (10**exponent))
    return values


def _random_bit_pattern_floats(rng: random.Random, count: int) -> list[float]:
    values = []
    while len(values) < count:
        bits = rng.getrandbits(64)
        x = struct.unpack("<d", struct.pack("<Q", bits))[0]
        if math.isfinite(x):
            values.append(x)
    return values


def build_cases() -> list[dict[str, Any]]:
    rng = random.Random(449_148)  # issue numbers, for a reproducible seed
    values = list(_EDGE_CASES)
    values.extend(_random_ordinary_floats(rng, 3000))
    values.extend(_random_bit_pattern_floats(rng, 2000))

    cases = []
    for x in values:
        try:
            expected = round(x, 6)
        except OverflowError:
            continue
        cases.append({"x": _hexbits(x), "expected": _hexbits(expected)})
    return cases


def main() -> int:
    cases = build_cases()
    data = (json.dumps({"round6_cases": cases}, indent=2) + "\n").encode("utf-8")
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_bytes() if OUTPUT.exists() else b""
        if current != data:
            print(f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr)
            return 1
        return 0
    OUTPUT.write_bytes(data)
    print(f"wrote {len(cases)} case(s) to {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
