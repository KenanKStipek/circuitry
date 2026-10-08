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
import re
import sys
from pathlib import Path

import yaml  # type: ignore[import-untyped]

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
        # PyYAML's `construct_yaml_int` never calls `.strip()`: it checks
        # the sign on the *raw* first character, then prefix detection on
        # the (sign-stripped, otherwise untrimmed) rest -- only the final
        # `int(value, base)` call tolerates surrounding whitespace.
        'a: !!int "- 42"\n',  # sign then an inner space: the final int() call strips it
        'a: !!int " 0x1A"\n',  # leading space blocks the "0x" prefix check: error
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
        # exactly halfway between two doubles once summed in PyYAML's own
        # right-to-left order (`digit*base` from the smallest unit up,
        # `base *= 60`) -- round-half-to-even then lands one ulp below
        # what summing the parts left-to-right would give
        "a: 1:30:00.06\n",
        # See `int_cases`'s comment on PyYAML never trimming up front:
        # leading whitespace blocks the exact `.inf` check, and falls
        # through to a plain `float(" .inf")`, which (unlike a bare
        # `"inf"`) CPython's own `float()` rejects too.
        'a: !!float " .inf"\n',
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
        "a: 0000-01-01\n",  # error: Python's `datetime.date` rejects year 0
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
        "a: !!int \" 42 \"\n",  # Python's `int()` strips the surrounding whitespace itself
        "a: !!float \" 1.5 \"\n",  # ... and so does `float()`
        "a: ! |\n  12\n",  # a literal block scalar forced to implicit resolution: an int, trailing newline and all
        "a: !!bool YeS\n",  # explicit bool: case-insensitive, unlike implicit
        "a: !!bool banana\n",  # error: not a recognized bool literal
        "a: !!float \"1.5\"\n",
        "a: !!float xyz\n",  # error
        "a: !!null anything\n",  # always None regardless of text
        "a: !!timestamp \"2024-01-01\"\n",
        "a: !!timestamp not-a-date\n",  # error
        "a: !!binary aGVsbG8=\n",
        "a: !!binary \"not base64!\"\n",  # error
        "a: !!binary \"aGVs!bG8=\"\n",  # a stray non-alphabet byte: skipped, not an error
        "a: ! on\n",  # bare non-specific tag: still fully implicit, any style
        "a: ! \"on\"\n",  # ... even quoted (confirmed against the real loader)
        "a: ! [1, 2]\n",  # ... and on a sequence/mapping, same as no tag at all
        "a: ! {x: 1}\n",
        "a: !custom foo\n",  # unknown tag: known-divergence error
        "!custom foo\n",  # ... and at the document root, where it's also the root tag
        # A bare `!` forcing *implicit* resolution on a literal block
        # scalar (always `\n`-terminated): `null`'s and the implicit
        # timestamp pattern's own regexes need the same trailing `\n?`
        # float_re/int_re already have.
        "a: ! |\n  ~\n",
        "a: ! |\n  2024-01-01\n",
        "a: ! |\n  .inf\n",  # bool/null/timestamp all miss; float still does too: error
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


def _divergence_from_error(
    yaml_text: str,
    *,
    rust_error_contains: str | None = None,
    rust_error_tag: str | None = None,
) -> dict:
    """A `known_divergence` case whose `python_repr` documents whatever
    Circuitry's *real* `load_yaml` actually does -- a value, or (unlike
    `known_divergence_cases`'s own inline loop below, which only ever
    expects success) an error -- so a divergence that both sides fail on,
    just differently, can still be pinned without the Rust side matching
    Circuitry's own exact message or position (`golden_corpus.rs`'s
    `KnownDivergence` arm only ever compares `rust_error_tag`/
    `rust_error_contains` against Rust's *own* error, never Circuitry's).
    """
    try:
        python_repr = _stable_repr(load_yaml(yaml_text))
    except Exception as exc:
        python_repr = f"<{type(exc).__name__}: {exc}>"
    entry: dict = {"yaml": yaml_text, "kind": "known_divergence", "python_repr": python_repr}
    if rust_error_tag is not None:
        entry["rust_error_tag"] = rust_error_tag
    if rust_error_contains is not None:
        entry["rust_error_contains"] = rust_error_contains
    return entry


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
    # Divergences whose Rust side fails with something other than an
    # `UnresolvableTag` -- matched against the error's `Display` instead
    # of a tag (`repr()`, not `_stable_repr()`: Python's own `repr()`
    # already breaks a reference cycle on its own as `[...]`, but
    # `_stable_repr()`'s plain recursion would loop forever on one).
    for yaml_text, rust_error_contains in [
        # PyYAML builds a real self-referential structure (it registers
        # an anchor before composing its own children); electricity-yaml
        # doesn't support that (DESIGN.md §3.2).
        ("a: &a [*a]\n", "self-referential anchor is not supported"),
        # A bare alias immediately followed by `:` with no space, used as
        # a mapping key: PyYAML's (YAML 1.1) scanner accepts it; saphyr-
        # parser 0.1.0 (YAML 1.2)'s simple-key lookahead does not, raising
        # its own "found unknown anchor" instead -- a scanner-level
        # 1.1-vs-1.2 difference, not anything this crate's composer
        # controls.
        ("x: &k a\nm:\n  *k: 1\n", "found unknown anchor"),
    ]:
        value = load_yaml(yaml_text)
        cases.append(
            {
                "yaml": yaml_text,
                "kind": "known_divergence",
                "python_repr": repr(value),
                "rust_error_contains": rust_error_contains,
            }
        )
    cases.extend(_extra_known_divergence_cases())
    return cases


def _extra_known_divergence_cases() -> list[dict]:
    return [
        # An anchor name outside `[0-9A-Za-z_-]` (e.g. `&x.y`): PyYAML's
        # scanner restricts anchor names to that set (`scanner.py:917-924`)
        # and *fails*; saphyr-parser 0.1.0's is more permissive and loads
        # it -- the opposite direction from every other case here (Rust
        # succeeds where Circuitry fails), which this corpus's
        # `known_divergence` kind has no way to express (it only checks
        # that Rust's side *also* fails, in some documented way); recorded
        # in `lib.rs`'s "Known divergences" section instead, not pinned
        # here.
        # PyYAML's `construct_yaml_float` returns a *fresh* NaN object for
        # an explicit `!!float nan` (no leading dot) -- unlike `.nan`,
        # which returns the one shared `nan_value` singleton -- so two of
        # them do *not* collide in CPython's dict despite both being NaN.
        # This crate's `Value` has no such identity to track (every NaN
        # float is just `f64::NAN`), so its duplicate-key check treats
        # every NaN key as the same key regardless of spelling, reporting
        # a duplicate here where Circuitry's loader keeps both.
        _divergence_from_error(
            "!!float nan: 1\n!!float nan: 2\n", rust_error_contains="duplicate key"
        ),
        # saphyr-parser 0.1.0's scanner starts a literal/folded block
        # scalar's span at its first *content* line, not at the `|`/`>`
        # indicator itself; PyYAML's own mark is the indicator's
        # position. Both sides raise `DuplicateKeyError` for this
        # document (two mapping keys that are both the block-scalar text
        # `"k\n"`), but at different lines, so this is checked loosely
        # (message contains "duplicate key") rather than word for word.
        _divergence_from_error(
            "? |\n  k\n: 1\n? |\n  k\n: 2\n", rust_error_contains="duplicate key"
        ),
        # PyYAML's `!!seq`/`!!map` constructors are generators, so a node
        # with the wrong structural kind only fails one *construction
        # round* later, after every *key* in the same mapping (including,
        # here, an unhashable one) has already been checked -- this
        # crate's composer instead rejects a mismatched container tag
        # immediately, during composition, before any sibling key is even
        # looked at. Reproducing PyYAML's exact laziness would mean
        # dispatching every constructor purely by *tag* and deferring
        # seq/map bodies a round, independently of a node's own
        # structural shape -- a materially larger redesign than this P2
        # finding's fix budget, so it's recorded as a divergence instead.
        _divergence_from_error("a: !!seq x\n[b]: 1\n", rust_error_tag="tag:yaml.org,2002:seq"),
        _divergence_from_error(
            "!!seq x: 1\na: 1\na: 2\n", rust_error_tag="tag:yaml.org,2002:seq"
        ),
        # A self-referential alias is refused during *composition* (this
        # crate's `Node` tree has no way to represent a cycle), which
        # pre-empts a shallower sibling's duplicate-key error that PyYAML
        # -- which registers an anchor before composing its own children,
        # so never even attempts to resolve the cycle eagerly -- reports
        # instead. Deferring self-reference detection the way
        # `!!seq`/`!!map`'s laziness above would need is the same kind of
        # redesign, declined for the same reason.
        _divergence_from_error(
            "a: 1\na: 2\nb: &b [*b]\n",
            rust_error_contains="self-referential anchor is not supported",
        ),
        # PyYAML's `int()`/`float()` accept non-ASCII (e.g. Arabic-Indic,
        # fullwidth) decimal digits under an explicit tag -- the same
        # digits CPython's own `int`/`float` constructors accept from
        # any string; `num_bigint::BigInt::from_str_radix` and Rust's
        # `f64::from_str` don't (finding 3, PR #388's third review).
        _divergence_from_error(
            'a: !!int "\u0661\u0662"\n', rust_error_contains="invalid int scalar"
        ),
        _divergence_from_error(
            'a: !!float "\uff11.\uff15"\n', rust_error_contains="invalid float scalar"
        ),
        # `flatten_mapping` only retags a bare `=` key to `str` when it's
        # reached *through* a merge -- a key reached directly, as an
        # ordinary mapping value (never through `<<:`), is untouched and
        # still has no constructor. PyYAML's own `MappingNode`s are
        # mutable and shared: once `b`'s merge flattens `&a` and retags
        # its `=` key in place, `deep.inner` -- the very same node,
        # reached directly, not through a merge -- now sees the already-
        # retagged key too, so it loads fine. electricity-yaml's `Node`
        # tree instead clones a shared anchor per alias
        # (`crate::MAX_NODES`'s doc comment): `b`'s merge retags its own
        # *copy*, leaving `deep.inner`'s own, never-merged node with its
        # original, un-retagged `=` key -- which still has no
        # constructor, so loading fails (finding 8, PR #388's third
        # review).
        _divergence_from_error(
            "deep:\n  inner: &a {=: 1}\nb: {<<: *a}\n",
            rust_error_tag="tag:yaml.org,2002:value",
        ),
        # PyYAML's `Reader.forward` (`reader.py`) counts U+2028 (LINE
        # SEPARATOR), U+2029 (PARAGRAPH SEPARATOR) and U+0085 (NEL) as
        # line breaks, same as `\n`; `saphyr-parser` 0.1.0 only counts
        # `\r`/`\n`. Both sides still report the same duplicate key here
        # (the comment line is skipped by both scanners regardless of
        # how many *line breaks* it contains), just reporting it one
        # line apart -- Circuitry's own line is one higher, since it
        # counts the embedded separator as an extra line break the
        # comment's own `\n` doesn't repeat (finding 3, PR #388's third
        # review; the column-in-a-*scalar* -- not a comment -- direction
        # of this same gap is documented below instead, since there
        # Circuitry fails outright while electricity-yaml successfully
        # loads a value, the one direction this corpus's `known_divergence`
        # kind has no way to express).
        _divergence_from_error(
            "# x \u2028\nx: 1\nx: 2\n", rust_error_contains="duplicate key"
        ),
        _divergence_from_error(
            "# x \u2029\nx: 1\nx: 2\n", rust_error_contains="duplicate key"
        ),
        _divergence_from_error(
            "# x \x85\nx: 1\nx: 2\n", rust_error_contains="duplicate key"
        ),
    ]


def merge_source_mutation_divergence_cases() -> list[dict]:
    """circuitry#390: Circuitry's own `_UniqueKeyLoader.construct_mapping`
    mutates a `MappingNode`'s `.value` list in its duplicate-key
    pre-pass, but `SafeConstructor.flatten_mapping` *also* mutates that
    same list in place when expanding a `<<:` merge key -- so a merge
    source shared by two mappings (one merged into another that's itself
    merged into a third) can already have been expanded once by the time
    the pre-pass walks the second mapping's keys, raising a
    `DuplicateKeyError` that plain `yaml.safe_load` does not raise on the
    same document. This is a bug in Circuitry's own loader, not in the
    YAML it's loading -- electricity-yaml must not reproduce it, so this
    one case's expected value deliberately comes from `yaml.safe_load`,
    not from `load_yaml` (every other case in this file's ground truth).
    """
    yaml_text = (
        "base: &base\n"
        "  retries: 1\n"
        "  timeout: 5\n"
        "templates:\n"
        "  defaults: &defaults\n"
        "    <<: *base\n"
        "    retries: 2\n"
        "effect:\n"
        "  <<: *defaults\n"
    )
    return [
        {
            "yaml": yaml_text,
            "kind": "value",
            "repr": repr(yaml.safe_load(yaml_text)),
            "note": (
                "circuitry#390: Circuitry's own load_yaml raises a false "
                "DuplicateKeyError on this document; electricity-yaml "
                "matches yaml.safe_load instead -- a deliberate, "
                "documented divergence from Circuitry's own loader, not "
                "from this crate's ground truth in general."
            ),
        }
    ]


# ---------------------------------------------------------------------
# `=` and `<<` standing alone as a value (not a merge/value tag use)
# ---------------------------------------------------------------------


def value_and_merge_tag_errors() -> list[dict]:
    yaml_texts = [
        "a: =\n",  # implicit resolution to the `=` tag: no constructor
        "a: <<\n",  # implicit resolution to the merge tag outside a key: no constructor
        "a: !!value foo\n",
        "a: !!merge foo\n",
        # A bare `=` used directly as a mapping's *own* key (not reached
        # through a merge): `_UniqueKeyLoader.construct_mapping`'s own
        # duplicate-key pre-pass constructs every one of a mapping's own
        # keys *before* `super().construct_mapping()` -- and the
        # `flatten_mapping` call inside it, which is what retags a `=`
        # key to `str` -- ever runs, so this still has no constructor.
        "{=: 5}\n",
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
        "a: &x 1\nb: &x 2\n",  # error: a reused anchor name
        # same reused-anchor check, across a lone \r (not \r\n or \n):
        # both loaders count a lone \r as its own line break, so this is
        # an ordinary parity case, not a divergence (PR #388's fourth
        # review; previously mishandled by this crate's own byte-offset
        # bookkeeping, not by saphyr-parser, which already counts it
        # correctly).
        "a: &x 1\rb: &x 2\r",
        # an alias used as a mapping key, then duplicated -- the position
        # for both is the *anchor's* own, confirmed against the real
        # loader (a space before `:` sidesteps a saphyr-parser 0.1.0
        # scanner limitation around a bare alias directly against `:`,
        # itself recorded as a known divergence below)
        "x: &k a\nm:\n  *k : 1\n  *k : 2\n",
    ]
    return [case(y) for y in yaml_texts]


# ---------------------------------------------------------------------
# Anchor-duplicate ordering and position: checked, and recorded, before
# a node's own children are composed, matching PyYAML's `compose_node`
# (`composer.py:72-77, :108, :126) -- not after, the way comparing a
# shallower anchor only once its own node is fully built would.
# ---------------------------------------------------------------------


def anchor_order_cases() -> list[dict]:
    yaml_texts = [
        # the outer sequence's own `&x` is the *first* occurrence
        # (declared before its items are even composed), so the nested
        # `&x` on `1` is correctly the duplicate, not the reverse
        "a: &x\n  - &x 1\n",
        "a: &x\n  b: &x 1\n  c: [\n",  # same, but the parser error after it never gets a chance
        # a root-level anchor/tag: an implicit document start's own span
        # is the first content token's, so the composer must not advance
        # past it before scanning for a leading `&`/`!`
        "&m\na: &m 1\n",
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
        # `flatten_mapping` retags a `=` key to `str` wherever it's found
        # while flattening -- including inside a merge *source*, which
        # (unlike this mapping's own direct keys, `value_and_merge_tag_errors`'
        # `{=: 5}` case) never goes through the outer pre-pass that would
        # otherwise fail on it first.
        "<<: {=: 1}\n",
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
        # a tag in front of each key: the position is the *tag's* own
        # start, not the resolved scalar's (confirmed: column 1 both
        # times against the real loader, not column 7)
        "!!str a: 1\n!!str a: 2\n",
        # PyYAML's `construct_yaml_float` returns the one shared
        # `nan_value` object for every `.nan` scalar, so two of them
        # collide in CPython's dict despite `NaN != NaN` generally
        "a:\n  .nan: 1\n  .nan: 2\n",
        # the *outer* `a` is the duplicate Python reports, never descending
        # into the first `a`'s own (otherwise-fine) nested mapping to find
        # a `b` that only looks duplicated once `a` has already failed
        "a:\n  b: 1\n  b: 2\na: 3\n",
        # the duplicate on `a` itself must win over a sibling value that
        # would otherwise fail first in a naive depth-first walk
        "a: !!int x\na: 1\n",
        "? [1, 2]\n: v\na: 1\na: 2\n",
        # the single-document check runs once the *whole* tree is
        # composed, before anything in it (including this duplicate) is
        # ever constructed -- so a second document after it is the error
        # that's reported, not the duplicate `a` (first review)
        "a: 1\na: 2\n---\nb: 1\n",
        # a *parser* error (an unclosed flow sequence for `c`) after an
        # already-composed duplicate key never gets a chance to surface:
        # the whole document must compose cleanly before any mapping's
        # own duplicate-key check ever runs (first review)
        "a:\n  b: 1\n  b: 2\nc: [\n",
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
        "a: [1, 2\n",  # error: unclosed flow sequence
        "a: b: c\n",  # error: a second `:` isn't allowed in this context
        "a:\n  b: 1\n c: 2\n",  # error: inconsistent block-mapping indentation
        "a:\n\tb: 1\n",  # error: a tab can't start a block-mapping entry
    ]
    return [case(y) for y in yaml_texts]


# ---------------------------------------------------------------------
# An unknown escape character (or, more generally, any other error
# PyYAML's own scanner marks at a character *inside* a double-quoted
# scalar) inside a double-quoted scalar: `saphyr-parser` 0.1.0 always
# marks these at the scalar's own opening quote instead
# (`resolve_flow_scalar_escape_sequence` in its scanner, which never
# advances past the `start_mark` it's given), so the composer recomputes
# the real position by replaying PyYAML's own escape-scanning rules from
# the scalar's start. The ASCII and non-ASCII forms pin the same fix in
# both column-counting regimes (characters, never bytes).
# ---------------------------------------------------------------------


def escape_position_cases() -> list[dict]:
    yaml_texts = [
        'a: "é\\q"\n',
        'a: "x\\q"\n',
    ]
    return [case(y) for y in yaml_texts]


# ---------------------------------------------------------------------
# Non-ASCII text: `node_prefix`'s gap-scanning converts a `saphyr_parser
# ::Marker`'s position into a byte offset via its line and column
# (never its `index()`, which counts *characters*, not bytes, despite
# its own doc comment) -- every case below exercises that conversion
# somewhere a multi-byte character could misalign it: a scalar value, a
# key, a comment, and right next to an anchor, each checked either for
# the exact value or (for the error cases) the exact position.
# ---------------------------------------------------------------------


def non_ascii_cases() -> list[dict]:
    yaml_texts = [
        "a: \u00e9\nb: 1\n",  # a two-byte character (\u00e9, 'é') in a value
        "\u00e9: 1\n",  # ... and in a key
        "a: \u2014 em dash\n",  # a three-byte character (an em dash)
        "a: \U0001f642\n",  # a four-byte character (an emoji, outside the BMP)
        "a: \u4e2d\u6587\n",  # a CJK scalar value
        "# caf\u00e9 \u2014 \U0001f642\na: 1\n",  # non-ASCII in a comment ahead of real content
        "a: &x \u00e9\nb: *x\n",  # non-ASCII right after an anchor
        "a: &x 1\nb: *x  # caf\u00e9 \U0001f642\n",  # ... and in a trailing comment near an alias
    ]
    cases = [case(y) for y in yaml_texts]
    # Error cases: the non-ASCII text sits *before* the error site, so a
    # misaligned byte offset would either panic outright or report the
    # wrong line/column for the duplicate-key/duplicate-anchor error
    # that follows it.
    cases.append(case("# caf\u00e9\n!!str a: 1\n!!str a: 2\n"))
    cases.append(case("\u00e9: 1\n\u00e9: 2\n"))
    cases.append(case("a: \u00e9\nb: &x 1\nc: &x 2\n"))
    return cases


# ---------------------------------------------------------------------
# D1 (the fix-pass orchestrator notes on PR #388): a non-ASCII variant of
# *every* case above, mechanically derived -- never handwritten -- so
# this corpus's non-ASCII coverage isn't limited to the handful of cases
# that happened to be written with non-ASCII text in mind. Two mechanical
# transformations, applied to every case's own `yaml` text:
#
# - a non-ASCII comment line prepended (`# ...\n`), which shifts every
#   line number in the case by exactly one and leaves the parsed value
#   or error *position* on each of those shifted lines otherwise
#   unchanged -- exercising the same byte-offset-vs-character-index
#   conversion `node_prefix` relies on (`compose.rs`), just ahead of the
#   case's own content rather than inside it. Applied to every case,
#   with no exceptions.
# - for a case whose own top-level key is the literal placeholder `a`
#   (the overwhelming majority of this generator's cases): a non-ASCII
#   suffix appended to that key's own name (`a` -> `a\u00e9`), which
#   shifts every *column* on that key's own line and in any later
#   sibling key's reported position, while leaving the case's own
#   semantics (the *value*, or the duplicate/error condition, under
#   test) completely untouched -- `a` is never itself a YAML keyword, so
#   renaming it can't change what the case is actually testing. Applied
#   only where the case has such a key; skipped otherwise.
#
# Each transformation is tried with a 2-byte ("\u00e9"), 3-byte
# ("\u2014") and 4-byte ("\U0001f642") character in turn, rotating by
# the case's own index so the corpus as a whole exercises all three
# without tripling every single case. Every variant's expected result is
# *rederived from scratch* -- never copied from the original case --
# through whichever of this file's own case-building paths produced the
# original (`case()`, a `known_divergence`, or circuitry#390's own
# `yaml.safe_load`-sourced expectation), so a transformation that
# happens to change the parsed value (the key rename does; the comment
# never does) is still checked against the real loader's own answer,
# never assumed to match the original case's.
# ---------------------------------------------------------------------

NON_ASCII_MARKERS = ["\u00e9", "\u2014", "\U0001f642"]  # 2, 3, 4 UTF-8 bytes

_TOP_LEVEL_A_KEY = re.compile(r"(?m)^a:")


def _prepend_non_ascii_comment(yaml_text: str, marker: str) -> str:
    return f"# non-ascii marker {marker}\n{yaml_text}"


def _rename_a_key(yaml_text: str, marker: str) -> str | None:
    """`yaml_text` with every top-level `a:` key renamed to `a<marker>:`,
    or `None` if it has no such key (`a` is only ever a *placeholder*
    name here, never load-bearing, so renaming every occurrence at once
    keeps a duplicate-key case testing the same duplicate, a merge case
    merging the same thing, and so on).
    """
    if not _TOP_LEVEL_A_KEY.search(yaml_text):
        return None
    return _TOP_LEVEL_A_KEY.sub(f"a{marker}:", yaml_text)


def _rebuild_case(original: dict, new_yaml: str, original_index: int) -> dict:
    """Rederives `original`'s own *kind* of expected result for
    `new_yaml`, through the real loader -- never by copying `original`'s
    own fields across, since the key-rename transformation (never the
    comment one) can change the parsed value. Tagged with
    `original_index` (`original`'s own index in the base corpus, before
    any variants are appended): `golden_corpus.rs`'s `Expected::Error`
    arm compares a variant's column *drift* against that same case's
    own drift, rather than directly against `column` within the
    ordinary small tolerance (a scanner-level 1.1-vs-1.2 difference in
    *which* token a third-party error names) -- that tolerance exists
    for whatever drift the *original* case already has, not for
    whatever a non-ASCII transform -- renaming a key, or prepending a
    one-line comment -- adds on top of it, which should always be
    exactly zero.
    """
    if original.get("note"):  # circuitry#390's own yaml.safe_load-sourced case
        entry = {
            "yaml": new_yaml,
            "kind": "value",
            "repr": repr(yaml.safe_load(new_yaml)),
            "note": original["note"],
        }
    elif original["kind"] == "known_divergence":
        entry = _divergence_from_error(
            new_yaml,
            rust_error_tag=original.get("rust_error_tag"),
            rust_error_contains=original.get("rust_error_contains"),
        )
    else:
        entry = case(new_yaml)
    entry["original_index"] = original_index
    return entry


def non_ascii_variants(cases: list[dict]) -> list[dict]:
    variants = []
    for i, original in enumerate(cases):
        marker = NON_ASCII_MARKERS[i % len(NON_ASCII_MARKERS)]
        commented = _prepend_non_ascii_comment(original["yaml"], marker)
        variants.append(_rebuild_case(original, commented, i))
        renamed = _rename_a_key(original["yaml"], marker)
        if renamed is not None:
            variants.append(_rebuild_case(original, renamed, i))
    return variants


# ---------------------------------------------------------------------
# Finding 3 (PR #388's third review): PyYAML's `Reader` (`reader.py`)
# strips a leading BOM before scanning ever starts -- no column counted
# for it (`scan_to_next_token`'s own comment: "the byte order mark is
# stripped if it's the first character in the stream") -- and rejects
# any non-printable character (a raw ANSI escape, a stray control byte --
# exactly what tool or model output can contain) immediately, with a
# `ReaderError` that carries no mark at all. `saphyr-parser` 0.1.0 has
# neither check.
# ---------------------------------------------------------------------


def reader_edge_cases() -> list[dict]:
    yaml_texts = [
        "\ufeffa: 1\n",
        # the stripped BOM must not hide this as two different keys
        "\ufeffa: 1\na: 2\n",
        'a: "\x1b[0m"\n',  # a raw ANSI escape
        "a: \x80\n",  # a raw C1 control byte
    ]
    return [case(y) for y in yaml_texts]


# ---------------------------------------------------------------------
# The orchestrator's own verification probes for PR #388's escape-
# position fix: non-ASCII text inside a scalar, ahead of a reported
# position (a duplicate key, a scanner/parser error), rather than in a
# leading comment or a key's own name the way `non_ascii_cases()`'s and
# `non_ascii_variants()`'s mechanical derivation already cover. All
# already pass -- pinned here as permanent corpus cases rather than left
# as throwaway probes.
# ---------------------------------------------------------------------


def scalar_non_ascii_cases() -> list[dict]:
    yaml_texts = [
        "a: caf\u00e9\nb: 1\nb: 2\n",
        'a: "na\u00efve \u2014 r\u00e9sum\u00e9"\nb: 1\nb: 2\n',
        "a: |\n  h\u00e9llo\n  w\u00f6rld\nb: 1\nb: 2\n",
        "a: 'x \U0001f642 y'\nk: 1\nk: 2\n",
        "a: [\u00e9, \u00fc, \U0001f642]\nb: {x: \u00e9, x: 1}\n",
        "key: value with \u00fcn\u00efc\u00f6d\u00e9\n  bad indent: 1\n",
        (
            "effects: [{type: prompt, name: x, template: 'Hi \U0001f642'}, "
            "{type: tool, name: y, provider: shell, params: {cmd: echo \u00e9}}]\n"
        ),
        "a: \u00e9\r\nb: 1\r\nb: 2\r\n",
        "a: \u00e9\rb: 1\rb: 2\r",
        "{\u00e9: 1, \u00e9: 2}\n",
        "msg: >\n  \u00fcn\n  zwei\nmsg: 2\n",
        'a: "\u4e2d\u6587" \nb: [1, 2\n',
        (
            "defaults: &d\n  retries: {max_attempts: 2, backoff_ms: 100}\n  on_error: continue\n"
            "effects:\n  - <<: *d\n    type: prompt\n    name: a\n    template: |\n"
            "      R\u00e9sum\u00e9 the text: {{{input.text}}}\n  - <<: *d\n    type: tool\n    name: b\n"
        ),
    ]
    return [case(y) for y in yaml_texts]


def build_corpus() -> list[dict]:
    base = (
        table_cases()
        + bool_cases()
        + int_cases()
        + float_cases()
        + null_cases()
        + timestamp_cases()
        + explicit_tag_cases()
        + known_divergence_cases()
        + merge_source_mutation_divergence_cases()
        + value_and_merge_tag_errors()
        + anchor_cases()
        + anchor_order_cases()
        + merge_cases()
        + duplicate_key_cases()
        + structural_cases()
        + escape_position_cases()
        + non_ascii_cases()
        + reader_edge_cases()
        + scalar_non_ascii_cases()
    )
    return base + non_ascii_variants(base)


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
