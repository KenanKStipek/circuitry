#!/usr/bin/env python3
"""Lane B's `difflib` corpus: `core.document_check._near_miss` (which wraps
`difflib.get_close_matches`, cutoff 0.8, plus the `_MISTAKEN_FOR` table) on
a handful of probe words against Circuitry's own real known-key sets --
not a hand-typed value, so a future change to a known-key set or to the
`difflib` port itself can't silently drift without this corpus (and
`golden_difflib.rs`, which replays it) catching it.

Covers: a hyphenated near miss normalized before matching (`Max-
Iterations`), surrounding whitespace, two near-misses of different known
keys in the same set, the `_MISTAKEN_FOR` table on two different known-key
sets that both happen to list the mistaken-for key, a non-ASCII word, a
long (250-character) word --autojunk's 200-character popular-element
threshold never triggers for any of `get_close_matches`' own inputs here,
since the known-key sets are always far shorter than 200 entries, and
`SequenceMatcher`'s `b2j`/`bjunk` business keys off the *shorter*
sequence's own length against 200, not the longer one's, so this case
exists only to pin that a long word still produces *no* match rather than
to exercise autojunk itself--, a word that scores exactly at the edge of
the cutoff, and a word scoring just under it.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI). Usage: python3 generate_compiler_difflib_corpus.py [--check]
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

from _compiler_corpus import write_or_check

from circuitry.core.document_check import _known_effect_keys, _near_miss

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-compiler"
    / "tests"
    / "golden"
    / "difflib.json"
)

#: (word, known-key-set name) -- `_known_effect_keys` is Circuitry's own
#: real per-effect-type known-key set, not a value this script invents.
PROBES: list[tuple[str, str]] = [
    ("Max-Iterations", "loop"),
    (" whlie ", "loop"),
    ("max_iteration", "loop"),
    ("min_iteraton", "loop"),
    ("adapter", "tool"),
    ("adapter", "prompt"),
    ("whilé", "loop"),
    ("a" * 250, "loop"),
    ("iterationsiterations", "loop"),
    ("whle_", "loop"),
]


def main() -> int:
    cases: list[dict[str, Any]] = []
    for word, known_set in PROBES:
        known = _known_effect_keys(known_set)
        cases.append(
            {
                "word": word,
                "known_set": known_set,
                "expected": _near_miss(word, known),
            }
        )
    text = json.dumps(cases, indent=2, ensure_ascii=False) + "\n"
    return write_or_check(OUTPUT, text, check="--check" in sys.argv[1:])


if __name__ == "__main__":
    raise SystemExit(main())
