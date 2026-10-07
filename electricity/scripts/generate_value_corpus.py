#!/usr/bin/env python3
"""Generate electricity-value's golden corpus from CPython 3.11 itself.

Writes a JSON array of `{"value": <tagged value>, "py_str": ..., "py_repr":
...}` cases. `py_str`/`py_repr` come directly from CPython's own `str()`/
`repr()` -- the ground truth electricity-value's Rust implementation must
match byte for byte (DESIGN.md §3.1, issue #361).

`value` uses a small tagged JSON encoding (`{"t": "<kind>", ...}`) for
whatever plain JSON cannot represent on its own: bytes (hex), big integers
(decimal string, so JSON-number precision never enters into it), floats
(raw IEEE-754 bits as hex, so the exact bit pattern -- including every NaN
payload and signed zero -- survives untouched), dates/date-times (ISO text
plus a separate UTC offset field), and dicts (a list of `[key, value]`
pairs rather than a JSON object, since dict keys here can be any `Value`,
not just strings).

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI, matching DESIGN.md's Unicode-parity requirement, see
generate_unicode_printable_ranges.py) so generated reprs match the
`electricity-value` crate's own target. Usage: python3
generate_value_corpus.py [--check]
"""

from __future__ import annotations

import datetime
import json
import math
import random
import struct
import sys
import unicodedata
from pathlib import Path

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-value"
    / "tests"
    / "golden"
    / "corpus.json"
)

SEED = 20261007


# ---------------------------------------------------------------------
# Tagged encoding
# ---------------------------------------------------------------------


def encode(value: object) -> dict:
    if value is None:
        return {"t": "none"}
    if isinstance(value, bool):
        return {"t": "bool", "v": value}
    if isinstance(value, int):
        return {"t": "int", "v": str(value)}
    if isinstance(value, float):
        bits = struct.unpack("<Q", struct.pack("<d", value))[0]
        return {"t": "float", "bits": format(bits, "016x")}
    if isinstance(value, str):
        return {"t": "str", "v": value}
    if isinstance(value, (bytes, bytearray)):
        return {"t": "bytes", "v": bytes(value).hex()}
    if isinstance(value, list):
        return {"t": "list", "v": [encode(item) for item in value]}
    if isinstance(value, dict):
        return {"t": "dict", "v": [[encode(k), encode(v)] for k, v in value.items()]}
    if isinstance(value, datetime.datetime):
        naive = value.replace(tzinfo=None)
        offset = value.utcoffset()
        return {
            "t": "datetime",
            "naive": naive.isoformat(timespec="microseconds"),
            "offset_seconds": None if offset is None else round(offset.total_seconds()),
        }
    if isinstance(value, datetime.date):
        return {"t": "date", "v": value.isoformat()}
    raise TypeError(f"no tagged encoding for {type(value)!r}")


def case(value: object) -> dict:
    return {"value": encode(value), "py_str": str(value), "py_repr": repr(value)}


# ---------------------------------------------------------------------
# Hand-picked edge cases
# ---------------------------------------------------------------------


def hand_picked_cases() -> list[dict]:
    values: list[object] = [
        None,
        True,
        False,
        0,
        1,
        -1,
        2**63 - 1,
        -(2**63),
        2**63,
        10**30,
        -(10**30),
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.1,
        0.5,
        100.0,
        9999999999999998.0,  # just below the switch to exponent notation
        1e16,  # the switch to exponent notation
        1e15,
        0.0001,  # the lowest magnitude still in fixed notation
        0.00009999,  # just below that switch, now exponential
        0.00001,
        5e-324,
        sys.float_info.max,
        float("inf"),
        float("-inf"),
        float("nan"),
        "",
        "hello",
        "it's",
        '"',  # only a double quote: must stay single-quoted
        'both\' and "',
        "tab\tnewline\nreturn\r",
        "".join(chr(c) for c in range(0x20)),
        "\x7f",
        "\xad",  # Cf, non-printable
        "\u200b",  # Cf, non-printable
        "\U0001fffe",  # Cn (unassigned as of Unicode 14), non-printable
        "caf\u00e9",  # printable non-ASCII
        "\U0001f600",  # printable emoji (So), supplementary plane
        "h\u00e9llo w\u00f6rld \u03a9 \u03b2 \u4e2d\u6587 \U0001f389",
        b"",
        b"abc",
        bytes([0, 255, 10, 9, 39, 34]),
        bytes(range(256)),
        [],
        [1, 2, 3],
        [1, "a", None, True, 1.5],
        [[1, 2], [3, [4, 5]]],
        {},
        {"a": 1},
        {1: "int", "a": "str", None: "none"},
        {True: "yes", False: "no"},
        {1: "a", 1.0: "b"},  # same key: last write wins  # noqa: F601
        datetime.date(2020, 1, 1),
        datetime.date(1, 1, 1),
        datetime.date(9999, 12, 31),
        datetime.datetime(2020, 1, 2, 3, 4, 5),  # naive, deliberately  # noqa: DTZ001
        datetime.datetime(2020, 1, 2, 3, 4, 0),  # noqa: DTZ001
        datetime.datetime(2020, 1, 2, 3, 4, 5, 123456),  # noqa: DTZ001
        datetime.datetime(2020, 1, 2, 3, 4, 5, tzinfo=datetime.timezone.utc),
        datetime.datetime(
            2020, 1, 2, 3, 4, 5, 123456,
            tzinfo=datetime.timezone(datetime.timedelta(hours=5, minutes=30)),
        ),
        datetime.datetime(
            2020, 1, 2, 3, 4, 5,
            tzinfo=datetime.timezone(
                datetime.timedelta(hours=-5, minutes=-30, seconds=-15)
            ),
        ),
        datetime.datetime(
            2020, 1, 2, 3, 4, 5, tzinfo=datetime.timezone(datetime.timedelta(0))
        ),
    ]
    return [case(v) for v in values]


# ---------------------------------------------------------------------
# Seeded random floats
# ---------------------------------------------------------------------


def random_float_cases(rng: random.Random, count: int) -> list[dict]:
    cases = []
    for _ in range(count):
        bits = rng.getrandbits(64)
        f = struct.unpack("<d", struct.pack("<Q", bits))[0]
        cases.append(case(f))
    # A few additional floats sampled from realistic magnitude ranges, not
    # just raw bit patterns (which skew towards huge exponents). Built with
    # `math.ldexp` (exact binary scaling) rather than `10**magnitude`
    # (`pow()`, whose last bit isn't guaranteed to match between the
    # platform that regenerates this file and the one that checks it),
    # so the file is byte-identical across platforms.
    for _ in range(count // 4):
        sign = rng.choice((-1.0, 1.0))
        f = sign * math.ldexp(rng.random(), rng.randint(-1000, 1000))
        cases.append(case(f))
    return cases


# ---------------------------------------------------------------------
# Seeded random strings across Unicode general categories
# ---------------------------------------------------------------------


def sample_codepoints_by_category() -> dict[str, list[int]]:
    """One or two representative, stable codepoints per General Category."""
    table: dict[str, list[int]] = {}
    candidates = [
        0x41, 0x7A,  # Lu, Ll
        0x1C5,  # Lt (titlecase)
        0x2B0,  # Lm (modifier letter)
        0x3042,  # Lo (hiragana)
        0x300,  # Mn (combining acute)
        0x903,  # Mc (devanagari sign visarga)
        0x488,  # Me (combining cyrillic hundred thousands)
        0x30,  # Nd (digit zero)
        0x2160,  # Nl (roman numeral one)
        0xB2,  # No (superscript two)
        0x5F,  # Pc (underscore)
        0x2D,  # Pd (hyphen-minus)
        0x28,  # Ps
        0x29,  # Pe
        0xAB,  # Pi
        0xBB,  # Pf
        0x21,  # Po (exclamation)
        0x2B,  # Sm (plus)
        0x24,  # Sc (dollar)
        0x5E,  # Sk (circumflex accent)
        0x2600,  # So (sun symbol)
        0x20,  # Zs (space) -- printable exception
        0x2028,  # Zl
        0x2029,  # Zp
        0x0,  # Cc (NUL)
        0xAD,  # Cf (soft hyphen)
        0xE000,  # Co (private use)
        0x1FFFE,  # Cn (unassigned)
        0x1F600,  # So (emoji)
        0x4E2D,  # Lo (CJK)
    ]
    for cp in candidates:
        cat = unicodedata.category(chr(cp))
        table.setdefault(cat, []).append(cp)
    return table


# Hand-verified Cf/Cn codepoints above the Basic Multilingual Plane (astral
# plane), against Unicode 14.0.0 (the database Python 3.11 ships): U+E0001
# LANGUAGE TAG and U+E0020 TAG SPACE are Cf; U+2FFFE, U+3FFFE and U+10FFFE
# are unassigned noncharacters (Cn). Hand-picking rather than scanning for
# these keeps the generator itself simple; the generated
# `unicode_nonprintable_ranges.rs` table is still built from a full scan
# (`generate_unicode_printable_ranges.py`), so these are only samples of it.
ASTRAL_CF_CN_CODEPOINTS = [0xE0001, 0xE0020, 0x2FFFE, 0x3FFFE, 0x10FFFE]


def c0_c1_control_codepoints() -> list[int]:
    return list(range(0x20)) + list(range(0x80, 0xA0))


def random_codepoint(rng: random.Random) -> int:
    """A codepoint drawn from the whole assigned range, skipping surrogates

    (which `chr()` accepts but which can't round-trip through UTF-8, the
    encoding this corpus file is written in).
    """
    while True:
        cp = rng.randrange(0x110000)
        if not (0xD800 <= cp <= 0xDFFF):
            return cp


def random_string_cases(rng: random.Random, count: int) -> list[dict]:
    by_category = sample_codepoints_by_category()
    category_pool = [cp for cps in by_category.values() for cp in cps]
    ascii_pool = list(range(0x20, 0x7F))
    control_pool = c0_c1_control_codepoints()
    # Weighted so every pool gets exercised across `count` strings: ASCII
    # and the category pool stay common, while control chars, astral Cf/Cn,
    # and whole-range draws (which sample the generated Unicode table's
    # range boundaries directly, not just its hand-picked representatives)
    # each get a meaningful share.
    weighted_pools = [
        (0.35, ascii_pool),
        (0.20, category_pool),
        (0.15, control_pool),
        (0.15, ASTRAL_CF_CN_CODEPOINTS),
    ]
    cases = []
    for _ in range(count):
        length = rng.randint(0, 12)
        chars = []
        for _ in range(length):
            roll = rng.random()
            cumulative = 0.0
            cp = None
            for weight, pool in weighted_pools:
                cumulative += weight
                if roll < cumulative:
                    cp = rng.choice(pool)
                    break
            if cp is None:
                cp = random_codepoint(rng)
            chars.append(chr(cp))
        cases.append(case("".join(chars)))
    return cases


# ---------------------------------------------------------------------


def build_corpus() -> list[dict]:
    rng = random.Random(SEED)
    cases = hand_picked_cases()
    cases += random_float_cases(rng, 200)
    cases += random_string_cases(rng, 100)
    return cases


def render(cases: list[dict]) -> str:
    return json.dumps(cases, indent=2, ensure_ascii=False, sort_keys=True) + "\n"


def main() -> int:
    text = render(build_corpus())
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_text() if OUTPUT.exists() else ""
        if current != text:
            print(f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr)
            return 1
        return 0
    OUTPUT.write_text(text)
    print(f"wrote {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
