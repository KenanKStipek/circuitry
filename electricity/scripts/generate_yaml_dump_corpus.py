#!/usr/bin/env python3
"""Generate electricity-yaml's `dump` golden corpus from real PyYAML.

Every case is `yaml.dump(value, default_flow_style=False, sort_keys=...)`
over a synthetic value (nothing copied from private orchestrations). Every
case defaults to `sort_keys=True` (reflector's own call); the mapping-key-
order cases alone (`key_order_cases`, below) are each recorded *twice* --
once `sort_keys=True`, once `sort_keys=False` -- since that is the one knob
ordering can actually depend on, so the one corpus serves M1-L2's
`safe_dump(sort_keys=False)` too without doubling every other case for a
knob that can't change their output (review finding 7 on PR #510's first
review: an earlier draft of this docstring overstated this).

Must be run with Python 3.11 and the pinned `PyYAML==6.0.3`
(`generator-constraints.txt`): PyYAML's own emitter output -- the exact
column at which width-80 folding breaks, the escape table, the float/int
text -- is what this corpus pins.

Mapping key order: `SafeRepresenter.represent_mapping`'s own `sort_keys=
True` path is `try: mapping = sorted(mapping) except TypeError: pass` --
confirmed directly against this installed PyYAML, a dict with mismatched
key types dumps successfully, in its *original* insertion order, never
raising. This corpus pins that fallback (`mixed_key_type_cases`), not a
raised error -- a correction to an earlier draft of the issue's own prose,
which assumed `sorted()` propagates the `TypeError`.

Usage: python3 generate_yaml_dump_corpus.py [--check]
"""

from __future__ import annotations

import datetime
import json
import sys
from pathlib import Path

import yaml  # type: ignore[import-untyped]

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-yaml"
    / "tests"
    / "golden"
    / "dump_corpus.json"
)


def enc(value: object) -> dict:
    """A JSON-safe, round-trippable tagged encoding of *value* -- the
    Rust side's `tests/golden_dump_corpus.rs` decodes this back into an
    `electricity_value::Value` exactly, so every `Value` variant this
    corpus exercises needs its own tag here.
    """
    if value is None:
        return {"t": "none"}
    if isinstance(value, bool):
        return {"t": "bool", "v": value}
    if isinstance(value, int):
        return {"t": "int", "v": str(value)}
    if isinstance(value, float):
        if value != value:  # NaN check, != on purpose
            text = "nan"
        elif value == float("inf"):
            text = "inf"
        elif value == float("-inf"):
            text = "-inf"
        else:
            text = repr(value)
        return {"t": "float", "v": text}
    if isinstance(value, str):
        return {"t": "str", "v": value}
    if isinstance(value, bytes):
        return {"t": "bytes", "v": list(value)}
    if isinstance(value, list):
        return {"t": "list", "v": [enc(item) for item in value]}
    if isinstance(value, dict):
        return {"t": "dict", "v": [[enc(k), enc(v)] for k, v in value.items()]}
    if isinstance(value, datetime.datetime):
        offset = value.utcoffset()
        return {
            "t": "datetime",
            "naive": value.replace(tzinfo=None).isoformat(),
            "offset_sec": int(offset.total_seconds()) if offset is not None else None,
        }
    if isinstance(value, datetime.date):
        return {"t": "date", "v": value.isoformat()}
    raise TypeError(f"generate_yaml_dump_corpus.py: no encoding for {type(value)}")


def case(value: object, *, sort_keys: bool = True) -> dict:
    return {
        "input": enc(value),
        "sort_keys": sort_keys,
        "expected": yaml.dump(value, default_flow_style=False, sort_keys=sort_keys),
    }


# ---------------------------------------------------------------------
# Nested containers, empty containers
# ---------------------------------------------------------------------


def container_cases() -> list[dict]:
    return [
        case([]),
        case({}),
        case({"a": []}),
        case({"a": {}}),
        case([1, 2, 3]),
        case({"a": 1, "b": 2}),
        case({"a": [1, 2]}),
        case([[1, 2], [3, 4]]),
        case([{"a": 1, "b": 2}]),
        case({"a": {"b": [1, 2], "c": 3}}),
        case({"a": [[1, 2], [3, [4, 5]]]}),
        case({"a": [{"x": 1}, {"y": 2}]}),
        case([[[1, 2], [3, 4]], [5]]),
        case([[[[["deep"]]]]]),
        case({"z": 1, "nested": {"y": 2, "x": [1, 2, 3]}}),
        case({"a": None, "b": True, "c": 1.5, "d": [1, "two", 3.0, None, True]}),
    ]


# ---------------------------------------------------------------------
# Strings needing each quoting style
# ---------------------------------------------------------------------


def quoting_cases() -> list[dict]:
    texts = [
        "hello",
        "yes",
        "Yes",
        "123",
        "1.5",
        "null",
        "~",
        "*foo",
        "&foo",
        "!foo",
        "|foo",
        ">foo",
        "%foo",
        "@foo",
        "`foo",
        "#foo",
        "foo #bar",
        "foo: bar",
        "foo:bar",
        "- foo",
        "-foo",
        "?foo",
        "foo?",
        " foo",
        "foo ",
        "foo\tbar",
        ":leading colon",
        "colon:no space",
        "trailing colon:",
        "skip: me",
        "héllo",
        "",
        "   test",
        "contains\x07bell",
        " leading and trailing \t",
    ]
    return [case({"a": t}) for t in texts] + [case(t) for t in texts]


# ---------------------------------------------------------------------
# YAML 1.1 bool/null look-alikes and numeric look-alikes as plain
# mapping keys (and standalone values) -- each must round-trip through
# `implicit_tag` to decide plain eligibility.
# ---------------------------------------------------------------------


def lookalike_cases() -> list[dict]:
    return [
        case({"null": "x", "true": "y"}),
        case({"on": 1, "off": 2}),
        case({"yes": "a", "no": "b"}),
    ]


# ---------------------------------------------------------------------
# Multi-line strings, longer than 80 columns, needing width-80 folding
# ---------------------------------------------------------------------


def multiline_and_width_cases() -> list[dict]:
    words = [f"word{i}" for i in range(30)]
    text = " ".join(words)
    long_word_run = " ".join(f"word{i}" for i in range(20))
    return [
        case({"a": "line1\nline2"}),
        case({"a": "line1\nline2\n"}),
        case({"a": "a\n\nb"}),
        case({"a": "a  b"}),
        case("a" * 90),
        case({"a": long_word_run}),
        case({"a": text}),
        case([text, text]),
        case({"outer": {"a": [text]}}),
        case({"outer": {"inner": text}}),
        case({"a": "word " * 20}),
        case({"a": "a" * 79}),
        case({"a": "a" * 80}),
        case({"a": "a" * 81}),
        case({"a": text + "\t"}),
        case({"a": "tab\there\nline2"}),
        case({"a": "x" * 40 + "\t" + "y" * 80}),
        case(
            {
                "a": (
                    "line1\nline2\nline3 with a lot more words to push this line "
                    "past col eighty definitely yes"
                )
            }
        ),
        case({"a": ("\u00fcn\u00efc\u00f6d\u00e9 " * 10)}),
        case({"a": ("\U0001F600 emoji text " * 8)}),
    ]


# ---------------------------------------------------------------------
# ints (big), floats, bools, None, dates
# ---------------------------------------------------------------------


def scalar_type_cases() -> list[dict]:
    return [
        case(5),
        case(None),
        case(True),
        case(False),
        case(1.0),
        case(1e20),
        case(float("inf")),
        case(float("nan")),
        case(-0.0),
        case(0.1),
        case(-1.5e-10),
        case({"a": -5}),
        case(123456789012345678901234567890),
        case(-123456789012345678901234567890),
        case({"a": 123456789012345678901234567890}),
        case(datetime.date(2024, 1, 1)),
        case(datetime.datetime(2024, 1, 1, 2, 3, 4)),  # naive, deliberately  # noqa: DTZ001
        case(datetime.datetime(2024, 1, 2, 3, 4, 5, 123456)),  # noqa: DTZ001
        case(
            datetime.datetime(
                2024, 1, 2, 3, 4, 5,
                tzinfo=datetime.timezone(datetime.timedelta(hours=5, minutes=30)),
            )
        ),
        case(datetime.datetime(2024, 1, 2, 3, 4, 5, tzinfo=datetime.timezone.utc)),
    ]


# ---------------------------------------------------------------------
# Control characters and non-ASCII forcing double-quoted escapes
# ---------------------------------------------------------------------


def escape_cases() -> list[dict]:
    chars = [
        "\x00", "\x07", "\x08", "\x0b", "\x0c", "\x0d", "\x1b", "\x7f",
        "\xa0", "\xad", "\x85", "\u2028", "\u2029", "\ufeff", "\u200b",
        '"', "\\",
    ]
    return [case({"a": c}) for c in chars]


# ---------------------------------------------------------------------
# Mapping key order: sort_keys true/false, and the mixed-key-type
# fallback (this module's own doc comment).
# ---------------------------------------------------------------------


def key_order_cases() -> list[dict]:
    unsorted = {3: "a", 1: "b", 2: "c"}
    return [
        case(unsorted, sort_keys=True),
        case(unsorted, sort_keys=False),
        case({"b": 1, "a": 2}, sort_keys=True),
        case({"b": 1, "a": 2}, sort_keys=False),
        case({1: "a", "b": "c"}, sort_keys=True),  # mixed types: fallback
        case({"b": "c", 1: "a"}, sort_keys=True),  # fallback, other order
        case({3: "a", 1: "b", "z": "c"}, sort_keys=True),  # 3 keys, mixed
    ]


def build_corpus() -> list[dict]:
    return (
        container_cases()
        + quoting_cases()
        + lookalike_cases()
        + multiline_and_width_cases()
        + scalar_type_cases()
        + escape_cases()
        + key_order_cases()
    )


def render(cases: list[dict]) -> str:
    return json.dumps(cases, indent=2, ensure_ascii=False) + "\n"


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
