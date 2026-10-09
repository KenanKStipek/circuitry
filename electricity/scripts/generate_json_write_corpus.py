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
import random
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
# load-bearing, DESIGN.md §3.4) and the generic `default=str` style used
# by `core/tool.py`'s redacted-`raw` size-cap path (`_capped_raw`) and
# several other callers (DESIGN.md §3.4). `compact_separators` pins
# `--events`'s own `separators=(",", ":")` (`cli/events.py::EventLog.
# _write_line`, runtime-semantics.md §8.7); `compact_separators_sorted`
# additionally covers `sort_keys=True` combined with that override, a
# combination no other mode here exercises (`pretty` only ever pairs
# `sort_keys=True` with `indent=2`).
MODES: list[dict] = [
    {"name": "compact", "indent": None, "sort_keys": False, "ensure_ascii": True, "default_str": False, "separators": None},
    {"name": "pretty", "indent": 2, "sort_keys": True, "ensure_ascii": True, "default_str": False, "separators": None},
    {"name": "ascii_false", "indent": None, "sort_keys": False, "ensure_ascii": False, "default_str": False, "separators": None},
    {"name": "default_str", "indent": None, "sort_keys": False, "ensure_ascii": False, "default_str": True, "separators": None},
    {"name": "compact_separators", "indent": None, "sort_keys": False, "ensure_ascii": True, "default_str": False, "separators": (",", ":")},
    {"name": "compact_separators_sorted", "indent": None, "sort_keys": True, "ensure_ascii": True, "default_str": False, "separators": (",", ":")},
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
    if mode["separators"] is not None:
        kwargs["separators"] = mode["separators"]
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


def _random_sort_key(rng: random.Random) -> object:
    choice = rng.random()
    if choice < 0.25:
        return rng.randint(-1000, 1000)
    if choice < 0.5:
        return rng.choice([True, False])
    if choice < 0.75:
        return round(rng.uniform(-1000.0, 1000.0), 6)
    # Occasionally a "nice" float that collides with an int/bool under
    # Python's numeric key equality (1 == 1.0 == True).
    return float(rng.randint(-5, 5))


def _distinct_sort_keys(rng: random.Random, size: int, with_nan: bool) -> list[object]:
    keys: list[object] = []
    seen: dict = {}
    while len(keys) < size:
        k = _random_sort_key(rng)
        if k in seen:
            continue
        seen[k] = True
        keys.append(k)
    if with_nan:
        keys.insert(rng.randrange(len(keys) + 1), float("nan"))
    rng.shuffle(keys)
    return keys


def random_nan_sort_cases(seed: int, n_with_nan: int, n_without_nan: int) -> list[object]:
    """Random dicts of 2-63 keys (kept under electricity-json's documented
    64-key divergence threshold) mixing ints, floats and bools in random
    insertion order, pinning ``sort_keys=True``'s CPython 3.11
    ``count_run``/``binarysort`` port against ``json.dumps(d,
    sort_keys=True)``'s actual key order. This is a seeded 200-case
    subset of a ~3500-case differential run (3000 NaN-keyed, 500 without,
    20 pinning that ``1``/``1.0``/``True`` collapse to one key) done
    against this exact port in a scratch worktree, confirmed to match
    with zero mismatches; CI only needs this smaller subset to keep
    pinning it going forward.
    """
    rng = random.Random(seed)
    cases: list[object] = []
    for _ in range(n_with_nan):
        size = rng.randint(1, 62)  # +1 for the NaN key => up to 63
        keys = _distinct_sort_keys(rng, size, with_nan=True)
        cases.append({k: i for i, k in enumerate(keys)})
    for _ in range(n_without_nan):
        size = rng.randint(2, 63)
        keys = _distinct_sort_keys(rng, size, with_nan=False)
        cases.append({k: i for i, k in enumerate(keys)})
    # 1 / 1.0 / True (and 0 / False / 0.0) collapse to one dict key in
    # Python; pin that this still holds with a NaN key in the mix.
    for _ in range(20):
        collapsing_keys: list[object] = [1, 1.0, True, 0, False, 0.0, float("nan")]
        rng.shuffle(collapsing_keys)
        cases.append({k: i for i, k in enumerate(collapsing_keys)})
    return cases


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
        "\b\f\x7f",  # backspace/form-feed named escapes, then plain DEL
        "caf\u00e9",
        "\U0001f600",  # supplementary-plane emoji: surrogate pair under ensure_ascii
        # Lists.
        [],
        [1, 2, 3],
        [1, "a", None, True, 1.5, [2, 3]],
        # Dicts: plain string keys (compact keeps insertion order, pretty sorts).
        {},
        {"b": 2, "a": 1, "c": 3},
        {"caf\u00e9": 1},  # a non-ASCII key, both ensure_ascii settings
        # Numeric dict keys: pretty must sort numerically (1, 2, 10), never
        # lexicographically ("1", "10", "2") -- DESIGN.md §3.4.
        {10: "ten", 1: "one", 2: "two"},
        # The whole numeric-tower family in one dict: True sorts as 1,
        # between 0.5 and 2 -- never grouped by Python type.
        {True: "bool-key", 0.5: "half-key", 2: "int-key"},
        # Non-str key stringification: True -> "true", None -> "null",
        # a float key via its own repr rule.
        {True: "bool-key"},
        {None: "none-key"},
        {2.5: "float-key"},
        {float("nan"): "nan-key"},
        # Two NaN keys are two distinct entries (nan != nan, so neither
        # collapses the other as a dict key); sort_keys must not raise
        # comparing them, or comparing one against an ordinary key.
        {float("nan"): "a", 1.0: 2},
        {float("nan"): "a", float("nan"): 1},
        # A NaN key that isn't adjacent to the key sort_keys moves past it:
        # CPython's count_run finds the ascending run [5.0, 6.0, 7.0, NaN,
        # 8.0] (NaN < x and x < NaN are both always False, so neither ever
        # breaks the run), then binarysort binary-inserts 1.0 at the front,
        # giving 1.0, 5.0, 6.0, 7.0, NaN, 8.0 -- not the order a plain
        # comparison sort that folds NaN comparisons to "equal" would give.
        {5.0: "a", 6.0: "b", 7.0: "c", float("nan"): "d", 8.0: "e", 1.0: "f"},
        # Mixed key types: compact succeeds (no sort needed), pretty must
        # raise sort_keys=True comparing an int to a str.
        {0: "int-key", "s": "str-key"},
        # None can never be ordered against anything, including under
        # sort_keys with only one other (otherwise-orderable) key.
        {None: "none-key", 1: "int-key"},
        # A nested dict also gets sorted under sort_keys, not just the
        # top level; an empty nested container stays empty either way.
        {"z": {"b": 2, "a": 1}, "a": {}, "b": []},
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
        # A descending opening run with a NaN key: count_run's reversal
        # of a strictly descending run must fire the same way whether or
        # not a NaN key is in the mix.
        {3.0: "a", 2.0: "b", 1.0: "c", float("nan"): "d", 0.5: "e"},
        {3.0: "a", float("nan"): "b", 2.0: "c", 1.0: "d"},
    ]
    values += random_nan_sort_cases(seed=377, n_with_nan=150, n_without_nan=30)
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
