#!/usr/bin/env python3
"""Generate electricity-json's reader golden corpus from CPython 3.11's own
``json.loads``, plus Circuitry's own duplicate-key check (DESIGN.md §3.4,
runtime-semantics.md §1.2, issue #377).

Each case is one JSON text and what reading it produces: the parsed value
(tagged the same way ``generate_value_corpus.py`` tags a `Value`) for valid
input; Circuitry's own ``DuplicateKeyError`` message, word for word, for a
repeated object key (``core/json_load.py``); or, for malformed JSON, only
the character position CPython's ``json.JSONDecodeError.pos`` reports — the
message text there is CPython's own, not Circuitry's, so electricity-json
only has to fail at the same position (DESIGN.md §1/§12).

Must be run with Python 3.11 (see ``generate_value_corpus.py``). Usage:
python3 generate_json_read_corpus.py [--check]
"""

from __future__ import annotations

import json
import struct
import sys
from pathlib import Path

# Generators may import Circuitry itself so duplicate-key expectations come
# from Circuitry's own loader, never a re-implementation (see
# .github/workflows/electricity.yml's "generated-files" job).
from circuitry.core.json_load import DuplicateKeyError, load_json

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-json"
    / "tests"
    / "golden"
    / "read_corpus.json"
)


def encode(value: object) -> dict:
    """Same tagged scheme as ``generate_value_corpus.py``'s ``encode``,
    restricted to what JSON can actually produce (no bytes/date/datetime)."""
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
    if isinstance(value, list):
        return {"t": "list", "v": [encode(item) for item in value]}
    if isinstance(value, dict):
        return {"t": "dict", "v": [[encode(k), encode(v)] for k, v in value.items()]}
    raise TypeError(f"no tagged encoding for {type(value)!r}")


def valid_case(text: str) -> dict:
    value = json.loads(text)
    return {"text": text, "expect": {"kind": "ok", "value": encode(value)}}


def duplicate_key_case(text: str) -> dict:
    try:
        load_json(text)
    except DuplicateKeyError as e:
        return {"text": text, "expect": {"kind": "duplicate_key", "message": str(e)}}
    raise AssertionError(f"expected a DuplicateKeyError for {text!r}")


def syntax_error_case(text: str) -> dict:
    try:
        json.loads(text)
    except json.JSONDecodeError as e:
        return {"text": text, "expect": {"kind": "syntax", "pos": e.pos}}
    raise AssertionError(f"expected a JSONDecodeError for {text!r}")


def build_corpus() -> list[dict]:
    valid_texts = [
        "null",
        "true",
        "false",
        "0",
        "-1",
        "123456789012345678901234567890",
        "-123456789012345678901234567890",
        "1.5",
        "1e16",
        "1E16",
        "-1.5e-10",
        "NaN",
        "Infinity",
        "-Infinity",
        '"hello"',
        '""',
        '"with \\"quote\\" and \\\\backslash"',
        '"tab\\tnewline\\nreturn\\r"',
        '"\\u00e9\\u4e2d\\ud83d\\ude00"',  # é, 中, then a surrogate-pair emoji
        '"\\u0041"',  # a BMP escape for an otherwise-plain ASCII letter
        "[]",
        "[1, 2, 3]",
        '[1, "a", null, true, 1.5, [2, 3]]',
        "{}",
        '{"a": 1, "b": [1, 2], "c": {"d": 3}}',
        '  { "a" : 1 , "b" : 2 }  ',
        '{"a": 1}\n',
    ]
    duplicate_key_texts = [
        '{"a": 1, "a": 2}',
        '{"a": 1, "nested": {"x": 1, "x": 2}}',
        '{"items": [{"a": 1}, {"a": 1, "a": 2}]}',
        '{"a": {"b": {"b": 1, "b": 2}}}',
    ]
    syntax_error_texts = [
        "",
        "   ",
        "{",
        "[",
        "{,}",
        "[1, 2",
        "[1, 2,]",
        '{"a": 1,}',
        '{"a" 1}',
        '{"a": }',
        '{"a": 1 "b": 2}',
        "[1 2]",
        '"unterminated',
        '"bad \\x escape"',
        '"bad \\u12 escape"',
        "nul",
        "tru",
        "01",
        "--1",
        "1 2",
        "{}}",
        '{"a": 1} extra',
    ]
    cases = [valid_case(t) for t in valid_texts]
    cases += [duplicate_key_case(t) for t in duplicate_key_texts]
    cases += [syntax_error_case(t) for t in syntax_error_texts]
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
