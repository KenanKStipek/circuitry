"""``prime.<loop>.last`` — the final completed pass of a named loop.

Regression suite for #164. Before it, "the final pass" had no spelling: an
author had to pin ``iter_<N>`` with an ``N`` they cannot know for ``while``
loops or data-dependent ``each`` loops (#128 documented the workaround in the
wild — reading ``iter_0`` and meaning the last).

The contract under test:

* after a named loop completes, ``prime.<loop>.last`` is the final *completed*
  iteration's node — same shape as ``iter_<N>``, deep field paths included;
* a pass that errored under ``on_error: continue``/``break`` is skipped in
  favor of the last one that finished;
* a zero-iteration loop writes no ``last`` key, so reads fall through/render
  empty exactly like a missing ``iter_<N>``;
* inside the loop's own body (or ``while`` condition) the spelling lint-warns;
  after the loop it does not.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.lint import lint_orchestration
from circuitry.core.store import Store


@dataclass
class EchoAdapter:
    """Echoes every rendered prompt back, so a step's value IS its template.

    ``replies`` (when set) are consumed first, in call order — used to script
    while-condition verdicts. A prompt containing ``BOOM`` raises instead,
    which is how the error-pass tests make one specific iteration fail.
    """

    name: str = "echo"
    prompts: list[str] = field(default_factory=list)
    replies: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)
        if "BOOM" in prompt:
            raise RuntimeError("scripted iteration failure")
        text = self.replies.pop(0) if self.replies else prompt
        return GenerateResult(text=text, raw={"model": model})


def _run(
    orch: dict[str, Any],
    state: dict[str, Any],
    *,
    replies: list[str] | None = None,
) -> tuple[EchoAdapter, dict[str, Any]]:
    root = compile_orchestration(orch=orch, root_name="prime")
    adapter = EchoAdapter(replies=list(replies or []))
    DynamicRuntime(root, adapter=adapter, model="unit-test").execute(
        store=Store(state)
    )
    return adapter, state


def _each_loop(
    *,
    flow: str = "chain",
    on_error: str = "fail",
    body_template: str = "OUT {{item}}",
) -> dict[str, Any]:
    """A named each-loop followed by a post-loop reader of `last`."""
    return {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "flow": flow,
                "on_error": on_error,
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "step", "template": body_template}
                ],
            },
            {
                "type": "prompt",
                "name": "final",
                "template": "last=[{{prime.outer.last.step.value}}]",
            },
        ]
    }


# --------------------------------------------------------------------------
# Resolution matrix: each/while, chain/tree, deep field paths.
# --------------------------------------------------------------------------


@pytest.mark.parametrize("flow", ["chain", "tree"])
def test_last_resolves_to_the_final_pass_after_an_each_loop(flow: str) -> None:
    adapter, state = _run(
        _each_loop(flow=flow), {"input": {"items": ["alpha", "beta", "gamma"]}}
    )
    assert adapter.prompts[-1] == "last=[OUT gamma]"
    # Same shape as iter_<N> because it IS the final iter node (aliased).
    assert state["prime"]["outer"]["last"] is state["prime"]["outer"]["iter_2"]


def test_last_resolves_after_a_while_loop() -> None:
    """Two passes then stop — `last` means pass 1, not pass 0."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "review",
                "max_iterations": 5,
                "while": {"mode": "model", "template": "Continue?"},
                "body": [
                    {
                        "type": "prompt",
                        "name": "refine",
                        "template": "REFINE pass {{_loop_index}}",
                    }
                ],
            },
            {
                "type": "prompt",
                "name": "final",
                "template": "last=[{{prime.review.last.refine.value}}]",
            },
        ]
    }
    # Call order: cond, body, cond, body, cond. Body replies echo the prompt.
    adapter, state = _run(
        orch,
        {"input": {}},
        replies=["yes", "REFINE pass 0", "yes", "REFINE pass 1", "no"],
    )
    assert adapter.prompts[-1] == "last=[REFINE pass 1]"
    assert state["prime"]["review"]["last"] is state["prime"]["review"]["iter_1"]


def test_last_supports_deep_field_paths() -> None:
    """`prime.<loop>.last.<step>.value.<field>` digs into structured output."""
    orch = _each_loop()
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict[str, Any] = {"input": {"items": ["a", "b"]}}
    adapter = EchoAdapter()
    DynamicRuntime(root, adapter=adapter, model="unit-test").execute(
        store=Store(state)
    )
    # Simulate a structured value on the final pass, then render through it.
    state["prime"]["outer"]["iter_1"]["step"]["value"] = {"refined": "DEEP"}
    orch_reader = {
        "effects": [
            {
                "type": "prompt",
                "name": "reader",
                "template": "deep=[{{prime.outer.last.step.value.refined}}]",
            }
        ]
    }
    reader_root = compile_orchestration(orch=orch_reader, root_name="prime")
    DynamicRuntime(reader_root, adapter=adapter, model="unit-test").execute(
        store=Store(state)
    )
    assert adapter.prompts[-1] == "deep=[DEEP]"


# --------------------------------------------------------------------------
# Last *completed* semantics under on_error, and the zero-iteration case.
# --------------------------------------------------------------------------


@pytest.mark.parametrize("flow", ["chain", "tree"])
def test_last_skips_a_final_errored_pass_under_continue(flow: str) -> None:
    """The last *completed* pass wins, not the last attempted one."""
    adapter, state = _run(
        _each_loop(flow=flow, on_error="continue"),
        {"input": {"items": ["alpha", "beta", "BOOM"]}},
    )
    assert adapter.prompts[-1] == "last=[OUT beta]"
    assert state["prime"]["outer"]["last"] is state["prime"]["outer"]["iter_1"]


def test_last_under_break_is_the_pass_before_the_error() -> None:
    adapter, state = _run(
        _each_loop(on_error="break"),
        {"input": {"items": ["alpha", "BOOM", "gamma"]}},
    )
    assert adapter.prompts[-1] == "last=[OUT alpha]"
    assert state["prime"]["outer"]["last"] is state["prime"]["outer"]["iter_0"]


def test_zero_iteration_loop_writes_no_last_key() -> None:
    """Reads fall through/render empty exactly like a missing iter_<N>."""
    adapter, state = _run(_each_loop(), {"input": {"items": []}})
    assert "last" not in state["prime"]["outer"]
    assert adapter.prompts[-1] == "last=[]"


def test_while_loop_that_never_runs_writes_no_last_key() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "review",
                "while": {"mode": "model", "template": "Continue?"},
                "body": [
                    {"type": "prompt", "name": "refine", "template": "REFINE"}
                ],
            }
        ]
    }
    _, state = _run(orch, {"input": {}}, replies=["no"])
    assert "last" not in state["prime"]["review"]


def test_all_passes_errored_under_continue_writes_no_last_key() -> None:
    _, state = _run(
        _each_loop(on_error="continue"), {"input": {"items": ["BOOM", "BOOM"]}}
    )
    assert "last" not in state["prime"]["outer"]


# --------------------------------------------------------------------------
# Snapshot stability: `last` adds one key and disturbs nothing else.
# --------------------------------------------------------------------------


def test_last_is_additive_and_unrelated_paths_are_untouched() -> None:
    _, state = _run(_each_loop(), {"input": {"items": ["a", "b"]}})
    node = state["prime"]["outer"]
    assert set(node) == {"value", "meta", "iter_0", "iter_1", "last"}
    assert node["value"]["iterations"] == 2
    assert node["iter_0"]["step"]["value"] == "OUT a"
    assert node["iter_1"]["step"]["value"] == "OUT b"
    # The aliased node still serializes (no cycles).
    json.dumps(state["prime"], default=str)


def test_unnamed_loops_are_unchanged() -> None:
    """`last` is a named-loop concept: unnamed loops already overwrite at
    stable paths and gain no node at all."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "OUT {{item}}"}
                ],
            }
        ]
    }
    _, state = _run(orch, {"input": {"items": ["a", "b"]}})
    assert "last" not in state["prime"]
    assert state["prime"]["step"]["value"] == "OUT b"


# --------------------------------------------------------------------------
# Lint: in-body/in-condition use warns; post-loop use does not.
# --------------------------------------------------------------------------


def _lint_loop(*, body_template: str | None = None, while_template: str | None = None,
               tail_template: str | None = None) -> list[str]:
    loop: dict[str, Any] = {
        "type": "loop",
        "name": "review",
        "body": [
            {
                "type": "prompt",
                "name": "refine",
                "template": body_template or "improve the draft",
            }
        ],
    }
    if while_template is not None:
        loop["while"] = {"mode": "model", "template": while_template}
    else:
        loop["each"] = {"in": "input.items", "as": "item"}
    effects: list[dict[str, Any]] = [loop]
    if tail_template is not None:
        effects.append(
            {"type": "prompt", "name": "final", "template": tail_template}
        )
    return lint_orchestration({"effects": effects})


def test_last_in_the_loop_body_warns() -> None:
    warnings = _lint_loop(
        body_template="Base it on {{prime.review.last.refine.value}}"
    )
    assert len(warnings) == 1
    assert "'prime.review.last'" in warnings[0]
    assert "previous pass at best" in warnings[0]


def test_last_in_the_while_condition_warns() -> None:
    warnings = _lint_loop(
        while_template="Is {{prime.review.last.refine.value}} good enough?"
    )
    assert len(warnings) == 1
    assert "'prime.review.last'" in warnings[0]


def test_last_after_the_loop_does_not_warn() -> None:
    assert (
        _lint_loop(tail_template="{{prime.review.last.refine.value.refined}}")
        == []
    )


def test_critique_refine_loop_final_reads_the_last_refinement() -> None:
    """The #128 workaround is gone: `final` consumes the LAST pass's
    refinement via `last`, not the first via a pinned `iter_0`."""
    pattern = Path("src/circuitry/curation/patterns/critique_refine_loop.yml")
    text = pattern.read_text(encoding="utf-8")
    doc = yaml.safe_load(text)
    final = next(e for e in doc["effects"] if e.get("name") == "final")
    assert "{{prime.review.last.refine_step.value.refined}}" in final["template"]
    assert "iter_0" not in final["template"]
    # And the spelling is legal where it sits — post-loop use lints clean.
    assert lint_orchestration(doc) == []


def test_last_of_some_other_finished_loop_does_not_warn() -> None:
    """Only the *enclosing* loop's `last` is flagged — a completed sibling
    loop's `last` is a real reference."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "first_loop",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "OUT {{item}}"}
                ],
            },
            {
                "type": "loop",
                "name": "second_loop",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "prompt",
                        "name": "step",
                        "template": "seed: {{prime.first_loop.last.step.value}}",
                    }
                ],
            },
        ]
    }
    assert lint_orchestration(orch) == []
