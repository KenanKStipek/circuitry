#!/usr/bin/env python3
"""Golden list of every effect-`*Definition` dataclass's own fields.

`electricity-bytecode`'s `tests/definition_fields.rs` checks this list
against a hand-maintained mapping from each Python field to the IR field
or type that carries it (issue #408's Scope section: "a test that every
field of each Python *Definition dataclass has an IR counterpart"). A
new field on one of these eight dataclasses changes this file, which
makes that Rust test fail (an unmapped field) until the IR carries it
and the mapping is updated to say so -- the point of generating this
list from the real dataclasses rather than hand-copying it once.

Scope: the eight top-level effect definitions issue #408 names,
`PromptDefinition`, `ToolDefinition`, `UseDefinition`, `YieldDefinition`,
`ReflectorDefinition`, `DynamicDefinition`, `ConditionalDefinition` and
`LoopDefinition` -- plus every dataclass nested inside one of their own
field types (`MessageDef`, `AssetRefDef`, `RetryPolicyDef`, `ExpectDef`,
`ConditionDef`, `LoopWhileDef`, `LoopEachDef`), so a new field on one of
those is caught the same way.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI, same as every other `electricity/scripts/generate_*.py`).
Usage: python3 generate_compiler_definition_fields.py [--check]
"""

from __future__ import annotations

import dataclasses
import json
import sys
from pathlib import Path

from circuitry.core.conditional import ConditionalDefinition, ConditionDef
from circuitry.core.dynamic import DynamicDefinition
from circuitry.core.expect import ExpectDef
from circuitry.core.loop import LoopDefinition, LoopEachDef, LoopWhileDef
from circuitry.core.prompt import (
    AssetRefDef,
    MessageDef,
    PromptDefinition,
    RetryPolicyDef,
)
from circuitry.core.reflector import ReflectorDefinition
from circuitry.core.tool import ToolDefinition
from circuitry.core.use import UseDefinition
from circuitry.core.yield_effect import YieldDefinition

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-bytecode"
    / "tests"
    / "golden"
    / "python_definition_fields.json"
)

# Order matches issue #408's own listing, nested types grouped right
# after the top-level definition that references them.
CLASSES = [
    PromptDefinition,
    MessageDef,
    AssetRefDef,
    RetryPolicyDef,
    ToolDefinition,
    ExpectDef,
    UseDefinition,
    YieldDefinition,
    ReflectorDefinition,
    DynamicDefinition,
    ConditionalDefinition,
    ConditionDef,
    LoopDefinition,
    LoopWhileDef,
    LoopEachDef,
]


def build_corpus() -> dict[str, list[str]]:
    return {cls.__name__: [f.name for f in dataclasses.fields(cls)] for cls in CLASSES}


def render(corpus: dict[str, list[str]]) -> str:
    return json.dumps(corpus, indent=2) + "\n"


def main() -> int:
    text = render(build_corpus())
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_text() if OUTPUT.exists() else ""
        if current != text:
            print(
                f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr
            )
            return 1
        return 0
    OUTPUT.write_text(text)
    print(f"wrote {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
