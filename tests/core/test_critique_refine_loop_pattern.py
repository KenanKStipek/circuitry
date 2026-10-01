"""The ``critique_refine_loop`` curation pattern critiques the *previous*
pass's refinement, not the original draft forever (issue #243).

Runs the actual bundled pattern YAML with a scripted adapter so the
assertion doesn't rely on a manual model run: pass 1's critique prompt must
contain pass 0's refined text, and pass 0's critique prompt must fall back
to the original draft (``prime.review.prev`` is absent on the first pass).
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store

PATTERN_PATH = Path("src/circuitry/curation/patterns/critique_refine_loop.yml")


@dataclass
class ScriptedAdapter:
    """Pops scripted replies in call order; echoes the prompt once exhausted."""

    name: str = "scripted"
    replies: list[str] = field(default_factory=list)
    prompts: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)
        text = self.replies.pop(0) if self.replies else prompt
        return GenerateResult(text=text, raw={"model": model})


def _critique_json(score: int, *, issues: list[str], strengths: list[str]) -> str:
    return json.dumps({"score": score, "issues": issues, "strengths": strengths})


def test_critique_refine_loop_critiques_the_previous_passs_refinement() -> None:
    orch = yaml.safe_load(PATTERN_PATH.read_text(encoding="utf-8"))
    root = compile_orchestration(orch=orch, root_name="prime")

    adapter = ScriptedAdapter(
        replies=[
            "ORIGINAL DRAFT",  # draft
            _critique_json(3, issues=["Tighten it"], strengths=["Clear"]),  # pass 0 critique
            "REFINED PASS 0",  # pass 0 refine
            "yes",  # while condition: still needs revision
            _critique_json(9, issues=[], strengths=["Tight"]),  # pass 1 critique
            "REFINED PASS 1",  # pass 1 refine
            "no",  # while condition: done
        ]
    )
    state: dict[str, Any] = {"input": {"topic": "serverless cold-start tradeoffs"}}
    DynamicRuntime(root, adapter=adapter, model="unit-test").execute(store=Store(state))

    critique_prompts = [p for p in adapter.prompts if "Critique the content below" in p]
    assert len(critique_prompts) == 2

    # First pass: prime.review.prev is absent, so the fallback reads the draft.
    assert "ORIGINAL DRAFT" in critique_prompts[0]
    assert "REFINED PASS 0" not in critique_prompts[0]

    # Second pass: critiques the previous pass's refinement, not the draft.
    assert "REFINED PASS 0" in critique_prompts[1]
    assert "ORIGINAL DRAFT" not in critique_prompts[1]

    assert state["prime"]["review"]["last"]["refine_step"]["value"]["refined"] == "REFINED PASS 1"
