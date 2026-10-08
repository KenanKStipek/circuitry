#!/usr/bin/env python3
"""Generate electricity-yaml's golden corpus from Circuitry's real YAML loader.

Every case is a synthetic YAML document (nothing copied from private
orchestrations) run through `circuitry.core.yaml_load.load_yaml` --
`yaml.safe_load` plus Circuitry's own duplicate-key check -- the ground
truth electricity-yaml's composer must match (DESIGN.md §3.2, issue #376):

- on success: `repr(value)`, compared against the Rust composer's own
  `Value::py_repr()` (already proven byte-identical to CPython's `repr()`
  by electricity-value's own golden corpus, so a textual comparison here
  is both sufficient and exact -- it tells int/float/bool/str/bytes apart,
  which a looser equality (Python's own `==`, or electricity-value's
  `py_eq`) would not: `1 == 1.0 == True` in Python, but the loader must
  still produce the *right* variant for each).
- on Circuitry's own `DuplicateKeyError`: its exact message, word for word.
- on any other error (a PyYAML/stdlib failure -- bad syntax, more than one
  document, an unresolvable tag, a bad explicit-tag literal, ...): only
  the position, taken from `problem_mark` when the exception carries one
  (every `yaml.YAMLError` that points at a place in the document does);
  `None`/`None` when it doesn't (a raw `ValueError`/`KeyError` out of
  `int()`/`float()`/a dict lookup has no mark at all) -- the Rust side
  then only has to agree that *something* failed (DESIGN.md §1, §12:
  third-party text only has to fail at the same place with a non-empty
  message, never the same words).

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI). Usage: python3 generate_yaml_corpus.py [--check]
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

from circuitry.core.yaml_load import DuplicateKeyError, load_yaml

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-yaml"
    / "tests"
    / "golden"
    / "corpus.json"
)


def run_case(yaml_text: str) -> dict:
    try:
        value = load_yaml(yaml_text)
    except DuplicateKeyError as exc:
        return {"kind": "duplicate_key_error", "message": str(exc)}
    except Exception as exc:
        mark = getattr(exc, "problem_mark", None)
        if mark is not None:
            return {"kind": "error", "line": mark.line, "column": mark.column}
        return {"kind": "error", "line": None, "column": None}
    return {"kind": "value", "repr": repr(value)}


def case(yaml_text: str) -> dict:
    return {"yaml": yaml_text, **run_case(yaml_text)}


# ---------------------------------------------------------------------
# runtime-semantics.md §1.1's table, row by row
# ---------------------------------------------------------------------


def table_cases() -> list[dict]:
    return [
        case("a: on\n"),
        case("a: yes\n"),
        case("a: off\n"),
        case("a: no\n"),
        case("a: 0x1A\n"),
        case("a: 017\n"),
        case("a: 1:30:00\n"),
        case("a: 2024-01-01\n"),
        case("a: .inf\n"),
        case("a: .nan\n"),
    ]


# ---------------------------------------------------------------------
# Booleans: three casings, y/n excluded
# ---------------------------------------------------------------------


def bool_cases() -> list[dict]:
    yaml_texts = [
        "a: on\n", "a: On\n", "a: ON\n",
        "a: off\n", "a: Off\n", "a: OFF\n",
        "a: yes\n", "a: Yes\n", "a: YES\n",
        "a: no\n", "a: No\n", "a: NO\n",
        "a: true\n", "a: True\n", "a: TRUE\n",
        "a: false\n", "a: False\n", "a: FALSE\n",
        "a: y\n", "a: Y\n", "a: n\n", "a: N\n",  # excluded: stay strings
        "a: 'on'\n",  # quoted: stays string
        "a: \"Yes\"\n",
        "a: |\n  on\n",  # block literal: stays string
    ]
    return [case(y) for y in yaml_texts]


# ---------------------------------------------------------------------
# Integers: hex, binary, legacy octal (not 0o), sexagesimal, underscores
# ---------------------------------------------------------------------


def int_cases() -> list[dict]:
    yaml_texts = [
        "a: 0x1A\n", "a: 0X1a\n", "a: -0x1A\n",
        "a: 0b101\n", "a: -0b101\n",
        "a: 017\n", "a: 0017\n", "a: -017\n",
        "a: 0o17\n",  # not legacy octal: stays the string "0o17"
        "a: 1:30:00\n", "a: -1:30:00\n", "a: 1:2:3\n",
        "a: 1_000_000\n", "a: 0x1_A\n", "a: 0b1_01\n",
        "a: 0\n", "a: -0\n", "a: 00\n",
        "a: 123456789012345678901234567890\n",  # bigger than i64
        "a: 0xFFFFFFFFFFFFFFFFFF\n",  # bigger than i64, hex
        "a: 1e3\n",  # no literal dot, no sexagesimal/hex/octal form: stays string
    ]
    return [case(y) for y in yaml_texts]


# ---------------------------------------------------------------------
# Floats: a literal dot required, signed exponent required, sexagesimal,
# .inf/.nan
# ---------------------------------------------------------------------


def float_cases() -> list[dict]:
    yaml_texts = [
        "a: 1.0\n", "a: -1.5\n", "a: .5\n", "a: 5.\n",
        "a: 1.0e+3\n", "a: 1.0e-3\n", "a: 1e3\n",  # no sign on exponent: string
        "a: 1.0E+3\n",
        "a: .inf\n", "a: .Inf\n", "a: .INF\n", "a: -.inf\n",
        "a: .nan\n", "a: .NaN\n", "a: .NAN\n",
        "a: 1:30:00.5\n", "a: -1:30:00.5\n",
        "a: 1_000.5\n",
    ]
    return [case(y) for y in yaml_texts]


# ---------------------------------------------------------------------
# Nulls: ~, null/Null/NULL, empty scalar
# ---------------------------------------------------------------------


def null_cases() -> list[dict]:
    yaml_texts = [
        "a: ~\n", "a: null\n", "a: Null\n", "a: NULL\n",
        "a:\n", "a: \n",
        "a: ''\n",  # quoted empty string: NOT null, stays ""
    ]
    return [case(y) for y in yaml_texts]


# ---------------------------------------------------------------------
# Timestamps: dates, naive and offset date-times
# ---------------------------------------------------------------------


def timestamp_cases() -> list[dict]:
    yaml_texts = [
        "a: 2024-01-01\n",
        "a: 2024-1-1\n",
        "a: 2024-01-02 03:04:05\n",  # naive
        "a: 2024-01-02T03:04:05\n",  # naive
        "a: 2024-01-02T03:04:05Z\n",  # UTC
        "a: 2024-01-02T03:04:05+05:30\n",
        "a: 2024-01-02T03:04:05-05:30\n",
        "a: 2024-01-02T03:04:05.123456Z\n",
        "a: 2024-01-02t03:04:05z\n",
        "a: 2024-01-02  03:04:05\n",  # multiple spaces, no T
    ]
    return [case(y) for y in yaml_texts]


# ---------------------------------------------------------------------
# Explicit tags
# ---------------------------------------------------------------------


def explicit_tag_cases() -> list[dict]:
    yaml_texts = [
        "a: !!str on\n",  # forced string: stays "on", not bool
        "a: !!str 123\n",
        "a: !!int \"42\"\n",  # forced int from a quoted scalar
        "a: !!int abc\n",  # error: not a valid int literal
        "a: !!bool YeS\n",  # explicit bool: case-insensitive, unlike implicit
        "a: !!bool banana\n",  # error: not a recognized bool literal
        "a: !!float \"1.5\"\n",
        "a: !!float xyz\n",  # error
        "a: !!null anything\n",  # always None regardless of text
        "a: !!timestamp \"2024-01-01\"\n",
        "a: !!timestamp not-a-date\n",  # error
        "a: !!binary aGVsbG8=\n",
        "a: !!binary \"not base64!\"\n",  # error
        "a: ! on\n",  # bare non-specific tag: still fully implicit, any style
        "a: ! \"on\"\n",  # ... even quoted (confirmed against the real loader)
        "a: ! [1, 2]\n",  # ... and on a sequence/mapping, same as no tag at all
        "a: ! {x: 1}\n",
        "a: !custom foo\n",  # unknown tag: known-divergence error
    ]
    return [case(y) for y in yaml_texts]


# ---------------------------------------------------------------------
# Known divergences: PyYAML tags `Value` cannot represent at all.
#
# `!!set` constructs a Python `set`; `!!omap`/`!!pairs` construct a list
# of tuples. `Value` has no `Set` or tuple variant (DESIGN.md §3.1), so
# electricity-yaml's composer refuses these tags outright (an
# `UnresolvableTag` error) rather than lossily approximating them as a
# `Dict`/`List` that would quietly stop round-tripping the way the real
# loader's `set`/tuples do. These cases record what the *real* loader
# does (for documentation) separately from what electricity-yaml is
# expected to do (`rust_error_tag`) -- deliberately not compared for
# equality, per DESIGN.md §3.2's "documented known-divergence case,
# never a silent difference".
# ---------------------------------------------------------------------


def _stable_repr(value: object) -> str:
    """`repr(value)`, except a `set`'s elements are sorted by their own
    repr first -- a plain Python `set` iterates in a hash-randomized
    order (a fresh, unpredictable one each interpreter run), which would
    make this generator's output nondeterministic. Only used for the
    documentation-only `python_repr` field below, never compared for
    equality on the Rust side.
    """
    if isinstance(value, set):
        return "{" + ", ".join(sorted(repr(v) for v in value)) + "}"
    if isinstance(value, dict):
        inner = ", ".join(f"{k!r}: {_stable_repr(v)}" for k, v in value.items())
        return "{" + inner + "}"
    if isinstance(value, list):
        return "[" + ", ".join(_stable_repr(v) for v in value) + "]"
    return repr(value)


def known_divergence_cases() -> list[dict]:
    cases = []
    for yaml_text, rust_error_tag in [
        ("a: !!set\n  x: null\n  y: null\n", "tag:yaml.org,2002:set"),
        ("a: !!omap\n  - x: 1\n  - y: 2\n", "tag:yaml.org,2002:omap"),
        ("a: !!pairs\n  - x: 1\n  - y: 2\n", "tag:yaml.org,2002:pairs"),
    ]:
        value = load_yaml(yaml_text)
        cases.append(
            {
                "yaml": yaml_text,
                "kind": "known_divergence",
                "python_repr": _stable_repr(value),
                "rust_error_tag": rust_error_tag,
            }
        )
    return cases


# ---------------------------------------------------------------------
# `=` and `<<` standing alone as a value (not a merge/value tag use)
# ---------------------------------------------------------------------


def value_and_merge_tag_errors() -> list[dict]:
    yaml_texts = [
        "a: =\n",  # implicit resolution to the `=` tag: no constructor
        "a: <<\n",  # implicit resolution to the merge tag outside a key: no constructor
        "a: !!value foo\n",
        "a: !!merge foo\n",
    ]
    return [case(y) for y in yaml_texts]


# ---------------------------------------------------------------------
# Anchors and aliases
# ---------------------------------------------------------------------


def anchor_cases() -> list[dict]:
    yaml_texts = [
        "a: &x hello\nb: *x\n",
        "a: &x [1, 2, 3]\nb: *x\nc: *x\n",
        "a: &x {p: 1, q: 2}\nb: *x\n",
        "items:\n  - &x {n: 1}\n  - *x\n  - *x\n",
        "a: *undefined\n",  # error: undefined alias
    ]
    return [case(y) for y in yaml_texts]


# ---------------------------------------------------------------------
# `<<:` merge keys
# ---------------------------------------------------------------------


def merge_cases() -> list[dict]:
    yaml_texts = [
        "base: &base\n  x: 1\n  y: 2\nderived:\n  <<: *base\n  y: 3\n",
        "base: &base\n  x: 1\n  y: 2\nderived:\n  z: 9\n  <<: *base\n  y: 3\n",
        "b1: &b1\n  x: 1\nb2: &b2\n  x: 2\nmerged:\n  <<: [*b1, *b2]\n",
        "b1: &b1\n  x: 1\nb2: &b2\n  y: 2\nmerged:\n  <<: [*b1, *b2]\n  x: 99\n",
        "a: &a\n  x: 1\nb: &b\n  x: 2\nc:\n  <<: *a\n  <<: *b\n",  # later bare << wins
        "a: &a\n  x: 1\nb:\n  <<: *a\n  <<: *a\n",  # repeated merge key: no dup error
        "a: &a\n  x: 1\nb:\n  <<: *a\n",  # <<: as a single-key shorthand
        "a: &a\n  x: 1\nb:\n  <<: {p: 9}\n",  # inline mapping merge source, no alias
        "a:\n  <<: [1, 2]\n",  # error: list items aren't mappings
        "a:\n  <<: 5\n",  # error: merge value is neither mapping nor sequence
    ]
    return [case(y) for y in yaml_texts]


# ---------------------------------------------------------------------
# Duplicate-key errors, across key types
# ---------------------------------------------------------------------


def duplicate_key_cases() -> list[dict]:
    yaml_texts = [
        "a: 1\na: 2\n",
        "1: a\n1: b\n",
        "true: a\nyes: b\n",
        "null: a\n~: b\n",
        "1.0: a\n1: b\n",
        "'1': a\n1: b\n",  # NOT a duplicate: string key vs int key
        "a:\n  b: 1\n  b: 2\nc: 3\n",  # nested mapping
    ]
    return [case(y) for y in yaml_texts]


# ---------------------------------------------------------------------
# Structural: multi-document, empty document, unhashable key
# ---------------------------------------------------------------------


def structural_cases() -> list[dict]:
    yaml_texts = [
        "",
        "   \n\n",
        "# just a comment\n",
        "a: 1\n---\nb: 2\n",
        "? [1, 2]\n: v\n",  # error: unhashable (list) key
        "a: [1, [2, 3], {b: 4}]\n",
        "a:\n  - 1\n  - 2\n  -\n    x: 1\n",
    ]
    return [case(y) for y in yaml_texts]


def build_corpus() -> list[dict]:
    return (
        table_cases()
        + bool_cases()
        + int_cases()
        + float_cases()
        + null_cases()
        + timestamp_cases()
        + explicit_tag_cases()
        + known_divergence_cases()
        + value_and_merge_tag_errors()
        + anchor_cases()
        + merge_cases()
        + duplicate_key_cases()
        + structural_cases()
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
