#!/usr/bin/env python3
"""Generate electricity-json's writer golden corpus from CPython 3.11's own
``json.dumps`` (DESIGN.md §3.4, §3.4.1, issue #377).

Each case is one Python value plus the exact text (or error) ``json.dumps``
produces under several *modes* — electricity-json's ``WriteMode`` plus the
one caller that passes ``default=str``. ``value`` reuses the tagged JSON
encoding from ``generate_value_corpus.py`` (see that script's module
docstring) so a `date`/`datetime`/`bytes` value, which JSON itself cannot
represent, still has a one-value test case covering the "writer raises"
rule (and the `default=str` exception to it).

Must be run with Python 3.11 (see ``generate_value_corpus.py``). Usage:
python3 generate_json_write_corpus.py [--check]
"""

from __future__ import annotations

import datetime
import json
import struct
import sys
from pathlib import Path

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-json"
    / "tests"
    / "golden"
    / "write_corpus.json"
)

# One mode per `json.dumps` call electricity-json's writer has to match:
# the two `--out` serializations (compact / pretty, runtime-semantics.md
# §8.3) plus `ensure_ascii=False` on its own (both values of the flag are
# load-bearing, DESIGN.md §3.4) and the redacted-`raw` size-cap path's
# `default=str` call (`cli/tool.py`'s `_capped_raw`).
MODES: list[dict] = [
    {"name": "compact", "indent": None, "sort_keys": False, "ensure_ascii": True, "default_str": False},
    {"name": "pretty", "indent": 2, "sort_keys": True, "ensure_ascii": True, "default_str": False},
    {"name": "ascii_false", "indent": None, "sort_keys": False, "ensure_ascii": False, "default_str": False},
    {"name": "default_str", "indent": None, "sort_keys": False, "ensure_ascii": False, "default_str": True},
]


def encode(value: object) -> dict:
    """Same tagged scheme as ``generate_value_corpus.py``'s ``encode``."""
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


def run_mode(value: object, mode: dict) -> dict:
    kwargs: dict = {"ensure_ascii": mode["ensure_ascii"]}
    if mode["indent"] is not None:
        kwargs["indent"] = mode["indent"]
    if mode["sort_keys"]:
        kwargs["sort_keys"] = True
    if mode["default_str"]:
        kwargs["default"] = str
    try:
        text = json.dumps(value, **kwargs)
    except TypeError:
        return {"text": None, "error": True}
    return {"text": text, "error": False}


def case(value: object) -> dict:
    return {
        "value": encode(value),
        "cases": [{"mode": mode["name"], **run_mode(value, mode)} for mode in MODES],
    }


def build_corpus() -> list[dict]:
    values: list[object] = [
        # Primitives and non-finite floats.
        None,
        True,
        False,
        0,
        -1,
        2**63,
        -(10**30),
        0.0,
        -0.0,
        1.5,
        1e16,
        0.00001,
        float("nan"),
        float("inf"),
        float("-inf"),
        "",
        "hello",
        "both\" and '",
        "tab\tnewline\nreturn\r\x00\x1f",
        "caf\u00e9",
        "\U0001f600",  # supplementary-plane emoji: surrogate pair under ensure_ascii
        # Lists.
        [],
        [1, 2, 3],
        [1, "a", None, True, 1.5, [2, 3]],
        # Dicts: plain string keys (compact keeps insertion order, pretty sorts).
        {},
        {"b": 2, "a": 1, "c": 3},
        # Numeric dict keys: pretty must sort numerically (1, 2, 10), never
        # lexicographically ("1", "10", "2") -- DESIGN.md §3.4.
        {10: "ten", 1: "one", 2: "two"},
        # Non-str key stringification: True -> "true", None -> "null",
        # a float key via its own repr rule.
        {True: "bool-key"},
        {None: "none-key"},
        {2.5: "float-key"},
        {float("nan"): "nan-key"},
        # Mixed key types: compact succeeds (no sort needed), pretty must
        # raise sort_keys=True comparing an int to a str.
        {0: "int-key", "s": "str-key"},
        # Values json.dumps itself can't encode without default=: the
        # writer raises, default_str stringifies via py_str instead.
        b"raw bytes \x00\xff",
        datetime.date(2020, 1, 2),
        datetime.datetime(2020, 1, 2, 3, 4, 5, 123456),  # noqa: DTZ001
        datetime.datetime(
            2020, 1, 2, 3, 4, 5, tzinfo=datetime.timezone(datetime.timedelta(hours=5, minutes=30))
        ),
        # Nested containers holding an unsupported value: the whole write
        # still raises/stringifies, not just the top level.
        {"ts": datetime.date(2020, 1, 2)},
        [b"x", 1],
        # A dict key json.dumps itself can't stringify at all: raises in
        # every mode, including default_str (default= is never consulted
        # for keys).
        {datetime.date(2020, 1, 2): "date-key"},
    ]
    return [case(v) for v in values]


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
