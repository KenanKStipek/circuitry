#!/usr/bin/env python3
"""A handful of trivial cases exercising `_compiler_corpus.py` end to end
(issue #408's lane A: "Plus ONE example generator ... so the harness is
exercised end to end"). Lane B/C/D's own generators
(`generate_compiler_{load,compile,compose}_corpus.py`) cover the real
surface; this one only proves the shared helper works.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI). Usage: python3 generate_compiler_smoke_corpus.py [--check]
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from _compiler_corpus import render_corpus, run_case, write_or_check

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-compiler"
    / "tests"
    / "golden"
    / "smoke.json"
)

CASES: list[dict] = [
    {
        "name": "minimal_valid_prompt",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: prompt\n"
                "    name: greet\n"
                "    template: 'Hello, {{input.name}}!'\n"
            )
        },
        "entry": "doc.yml",
    },
    {
        "name": "empty_document",
        "files": {"doc.yml": ""},
        "entry": "doc.yml",
    },
    {
        "name": "duplicate_effect_name",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: prompt\n"
                "    name: step\n"
                "    template: 'a'\n"
                "  - type: prompt\n"
                "    name: step\n"
                "    template: 'b'\n"
            )
        },
        "entry": "doc.yml",
    },
    {
        "name": "unsupported_suffix",
        "files": {"doc.txt": "effects: []\n"},
        "entry": "doc.txt",
    },
    {
        "name": "non_mapping_root",
        "files": {"doc.yml": "- 1\n- 2\n"},
        "entry": "doc.yml",
    },
]


def main() -> int:
    results = [run_case(case) for case in CASES]
    text = render_corpus(results)
    return write_or_check(OUTPUT, text, check="--check" in sys.argv[1:])


if __name__ == "__main__":
    raise SystemExit(main())
