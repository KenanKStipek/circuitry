#!/usr/bin/env python3
"""Generate electricity-json's reader golden corpus from CPython 3.11's own
``json.loads``, plus Circuitry's own duplicate-key check (DESIGN.md §3.4,
runtime-semantics.md §1.2, issue #377).

Each case is one JSON text, which of electricity-json's two reader entry
points it exercises (``entry``: ``"loads"`` for plain ``json.loads``
semantics only, ``"load_json"`` for ``core/json_load.load_json`` semantics
only, or ``"both"`` when the two can't differ -- anything without a
repeated object key), and what reading it produces: the parsed value
(tagged the same way ``generate_value_corpus.py`` tags a `Value`) for valid
input; Circuitry's own ``DuplicateKeyError`` message, word for word, for a
repeated object key (``core/json_load.py``); or, for malformed JSON, only
the character position CPython's ``json.JSONDecodeError.pos`` reports — the
message text there is CPython's own, not Circuitry's, so electricity-json
only has to fail at the same position (DESIGN.md §1/§12). Two cases (a
lone UTF-16 surrogate escape) are a documented *divergence* instead: see
``divergent_lone_surrogate_case``.

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
# .github/workflows/electricity-generated.yml).
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
    return {"text": text, "entry": "both", "expect": {"kind": "ok", "value": encode(value)}}


def duplicate_key_cases(text: str) -> list[dict]:
    """One ``loads`` case (plain ``json.loads``: last value wins) and one
    ``load_json`` case (Circuitry's own loader: raises) for the same text
    -- the two entry points can only disagree on a repeated object key."""
    loads_value = json.loads(text)
    try:
        load_json(text)
    except DuplicateKeyError as e:
        load_json_case = {
            "text": text,
            "entry": "load_json",
            "expect": {"kind": "duplicate_key", "message": str(e)},
        }
    else:
        raise AssertionError(f"expected a DuplicateKeyError for {text!r}")
    loads_case = {
        "text": text,
        "entry": "loads",
        "expect": {"kind": "ok", "value": encode(loads_value)},
    }
    return [loads_case, load_json_case]


def syntax_error_case(text: str) -> dict:
    try:
        json.loads(text)
    except json.JSONDecodeError as e:
        return {"text": text, "entry": "both", "expect": {"kind": "syntax", "pos": e.pos}}
    raise AssertionError(f"expected a JSONDecodeError for {text!r}")


def divergent_lone_surrogate_case(text: str) -> dict:
    """A documented divergence (lib.rs's module docs), not a CPython
    parity assertion: CPython's ``json.loads`` keeps a lone UTF-16
    surrogate in the resulting ``str`` (confirmed directly -- ``len(v) ==
    1`` for each of these texts), which electricity-json can't represent
    in a Rust ``String`` and decodes to U+FFFD instead. Hand-writes the
    *electricity* side's own expected value rather than CPython's, since
    CPython's actual result -- a Python ``str`` holding an unpaired
    surrogate -- can't itself survive being written into this corpus file
    as UTF-8 JSON."""
    json.loads(text)  # still confirms CPython accepts the text at all
    return {
        "text": text,
        "entry": "both",
        "expect": {"kind": "ok", "value": encode("\ufffd")},
    }


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
        '"a\\/b"',  # \/ is accepted though JSON never requires escaping it
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
        # The repeated key keeps its *first* position, not its last --
        # CPython gives {"a": 3, "b": 2} in that order, not {"b": 2, "a": 3}.
        '{"a": 1, "b": 2, "a": 3}',
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
        '"\\u+041"',  # a leading '+' is never a valid \uXXXX hex digit
        '"\\u0041',  # \uXXXX escape with nothing after it, not even the closing quote
        '"\\ud800\\udc00',  # same, mid-surrogate-pair
        '"a\x01b"',  # an unescaped control character inside a string
    ]
    lone_surrogate_texts = [
        '"\\ud800"',  # a lone high surrogate, with nothing following it
        '"\\udc00"',  # a lone low surrogate
    ]
    cases = [valid_case(t) for t in valid_texts]
    for t in duplicate_key_texts:
        cases += duplicate_key_cases(t)
    cases += [syntax_error_case(t) for t in syntax_error_texts]
    cases += [divergent_lone_surrogate_case(t) for t in lone_surrogate_texts]
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
