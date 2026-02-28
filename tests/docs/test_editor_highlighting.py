from __future__ import annotations

import json
import re
from pathlib import Path

GRAMMAR_PATH = Path("editor/vscode-circuitry/syntaxes/circuitry.tmLanguage.json")


def _grammar() -> dict:
    return json.loads(GRAMMAR_PATH.read_text(encoding="utf-8"))


def _compile(pattern: str) -> re.Pattern[str]:
    return re.compile(pattern, re.MULTILINE)


def test_grammar_declares_core_repositories() -> None:
    grammar = _grammar()
    repo = grammar["repository"]
    for key in ["comments", "types", "keys", "flowValues", "promptTypes"]:
        assert key in repo


def test_highlighting_patterns_match_representative_examples() -> None:
    grammar = _grammar()
    repo = grammar["repository"]

    types_pattern = _compile(repo["types"]["patterns"][0]["match"])
    keys_pattern = _compile(repo["keys"]["patterns"][0]["match"])
    flow_pattern = _compile(repo["flowValues"]["patterns"][0]["match"])
    prompt_type_pattern = _compile(repo["promptTypes"]["patterns"][0]["match"])

    hello = Path("orchestrations/_prompt.yml").read_text(encoding="utf-8")
    typed = Path("orchestrations/_prompt.yml").read_text(encoding="utf-8")
    multi = Path("orchestrations/_composition.yml").read_text(encoding="utf-8")

    assert types_pattern.search(hello)
    assert keys_pattern.search(hello)
    assert prompt_type_pattern.search(typed)
    assert flow_pattern.search(multi)
    assert types_pattern.search(multi)
