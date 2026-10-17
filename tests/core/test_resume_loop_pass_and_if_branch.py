"""`cof run --resume` inside an unfinished loop pass and `if` branch (#573).

The first pass not in a named chain loop's ``meta.completed_passes``
continues inside its saved ``iter_<N>`` node: body effects that finished
without error are reused, and the first one that didn't runs again, with
everything after it. A ``while`` loop runs that pass without asking its
condition again. An unfinished named ``if`` evaluates its condition again;
when it selects the branch its saved node took, that branch continues the
same way, and the other branch runs from its start. A ``use`` in such a pass
or branch continues inside its child (with ``record_children``), as at the
top level (#567). Tree-flow loops, ``reflector``, and passes or branches
with no saved record of their own rerun from their start, as before.

Each resumed run starts from the first run's state as saved (`--out`'s JSON
form), the way `cof run --resume` reads it back.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.runtime_shim import RunRequest, RunResult, run
from circuitry.core.saved_state import dumps_saved_state

#: How a `mode: model` loop or `if` condition's prompt starts.
CONDITION = "Evaluate the following condition"


class ScriptedAdapter:
    """Records every prompt; fails or interrupts on demand.

    A condition prompt is recorded as ``CONDITION`` and answered yes; a
    reflector's planning prompt is answered with a finished plan.
    """

    name = "scripted"

    def __init__(self) -> None:
        self.calls: list[str] = []
        self.fail_prompts: set[str] = set()
        self.interrupt_prompts: set[str] = set()
        #: Fail the condition check with this number (1 = the first).
        self.fail_condition: int | None = None
        self._conditions = 0

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        if prompt.startswith(CONDITION):
            self.calls.append(CONDITION)
            self._conditions += 1
            if self._conditions == self.fail_condition:
                raise RuntimeError("condition model down")
            return GenerateResult(text="yes", raw={})
        if "plan-me" in prompt:
            self.calls.append("plan-me")
            return GenerateResult(text="done: true", raw={})
        self.calls.append(prompt)
        if prompt in self.interrupt_prompts:
            raise KeyboardInterrupt
        if prompt in self.fail_prompts:
            raise RuntimeError(f"boom: {prompt}")
        return GenerateResult(text=f"ok:{prompt}", raw={})


def _write(tmp_path: Path, name: str, content: dict[str, Any]) -> Path:
    path = tmp_path / name
    path.write_text(yaml.safe_dump(content, sort_keys=False), encoding="utf-8")
    return path


def _prompt(name: str, template: str) -> dict[str, Any]:
    return {"type": "prompt", "name": name, "template": template}


def _abc(suffix: str) -> list[dict[str, Any]]:
    return [_prompt(step, f"{step}-{suffix}") for step in ("a", "b", "c")]


def _run(
    orch: Path,
    adapter: ScriptedAdapter,
    *,
    inputs: dict[str, Any] | None = None,
    saved: dict[str, Any] | None = None,
) -> RunResult:
    return run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state={"input": dict(inputs or {})} if saved is None else saved,
            resume=saved is not None,
        )
    )


def _saved(result: RunResult) -> dict[str, Any]:
    """*result*'s state as `--out` writes it and `--resume` reads it back."""
    loaded: dict[str, Any] = json.loads(dumps_saved_state(result.state))
    return loaded


def _resume_calls(
    orch: Path, adapter: ScriptedAdapter, first: RunResult, saved: dict[str, Any] | None = None
) -> tuple[RunResult, list[str]]:
    """Resume *first*, and return the second run with the prompts it sent."""
    adapter.fail_prompts = set()
    adapter.interrupt_prompts = set()
    adapter.fail_condition = None
    before = len(adapter.calls)
    second = _run(orch, adapter, saved=_saved(first) if saved is None else saved)
    return second, adapter.calls[before:]


def _each(name: str, body: list[dict[str, Any]], *, flow: str = "chain") -> dict[str, Any]:
    return {
        "type": "loop",
        "name": name,
        "flow": flow,
        "each": {"in": "input.items", "as": "item"},
        "body": body,
    }


ITEMS = {"items": ["0", "1", "2"]}


# -- loop passes --------------------------------------------------------------


def test_a_failed_each_pass_resumes_at_its_first_unfinished_body_effect(
    tmp_path: Path,
) -> None:
    orch = _write(tmp_path, "each.yml", {"effects": [_each("frames", _abc("{{item}}"))]})
    adapter = ScriptedAdapter()
    adapter.fail_prompts = {"b-1"}

    first = _run(orch, adapter, inputs=ITEMS)
    assert first.ok is False

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    # Pass 1's `a` is reused; its `b` and `c` run, then pass 2 runs whole.
    assert calls == ["b-1", "c-1", "a-2", "b-2", "c-2"]
    frames = second.state["prime"]["frames"]
    assert frames["iter_1"]["a"]["value"] == "ok:a-1"
    assert frames["iter_1"]["c"]["value"] == "ok:c-1"
    assert frames["meta"]["completed_passes"] == [0, 1, 2]
    assert frames["value"]["iterations"] == 3


def test_an_interrupted_each_pass_resumes_at_its_first_unfinished_body_effect(
    tmp_path: Path,
) -> None:
    orch = _write(tmp_path, "each.yml", {"effects": [_each("frames", _abc("{{item}}"))]})
    adapter = ScriptedAdapter()
    adapter.interrupt_prompts = {"c-0"}

    first = _run(orch, adapter, inputs=ITEMS)
    assert first.interrupted is True

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    assert calls == ["c-0", "a-1", "b-1", "c-1", "a-2", "b-2", "c-2"]


def test_a_resumed_each_pass_reads_its_reused_effects_and_the_previous_pass(
    tmp_path: Path,
) -> None:
    body = [
        _prompt("a", "a-{{item}}"),
        _prompt("b", "b-{{prime.a.value}}-{{prime.frames.prev.b.value}}"),
    ]
    orch = _write(tmp_path, "each.yml", {"effects": [_each("frames", body)]})
    adapter = ScriptedAdapter()
    adapter.fail_prompts = {"b-ok:a-1-ok:b-ok:a-0-"}

    first = _run(orch, adapter, inputs={"items": ["0", "1"]})
    assert first.ok is False

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    assert calls == ["b-ok:a-1-ok:b-ok:a-0-"]


def test_an_interrupted_while_pass_resumes_inside_without_asking_its_condition(
    tmp_path: Path,
) -> None:
    orch = _write(
        tmp_path,
        "while.yml",
        {
            "effects": [
                {
                    "type": "loop",
                    "name": "polls",
                    "while": {"mode": "model", "template": "more?"},
                    "max_iterations": 3,
                    "body": _abc("{{iter.index}}"),
                }
            ]
        },
    )
    adapter = ScriptedAdapter()
    adapter.interrupt_prompts = {"b-1"}

    first = _run(orch, adapter)
    assert first.interrupted is True
    assert adapter.calls == [CONDITION, "a-0", "b-0", "c-0", CONDITION, "a-1", "b-1"]

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    # Pass 1 continues at `b` with no condition check; pass 2 asks first.
    assert calls == ["b-1", "c-1", CONDITION, "a-2", "b-2", "c-2"]
    assert second.state["prime"]["polls"]["meta"]["completed_passes"] == [0, 1, 2]


def test_a_failed_while_pass_runs_on_even_when_its_condition_would_now_be_false(
    tmp_path: Path,
) -> None:
    orch = _write(
        tmp_path,
        "while.yml",
        {
            "effects": [
                {
                    "type": "loop",
                    "name": "polls",
                    "while": {"mode": "cel", "expr": "state.input.go == true"},
                    "max_iterations": 3,
                    "body": _abc("{{iter.index}}"),
                }
            ]
        },
    )
    adapter = ScriptedAdapter()
    adapter.fail_prompts = {"b-1"}

    first = _run(orch, adapter, inputs={"go": True})
    assert first.ok is False

    saved = _saved(first)
    saved["input"]["go"] = False
    second, calls = _resume_calls(orch, adapter, first, saved)
    assert second.ok is True, second.error
    # The pass the saved run stopped in finishes; the next check ends the loop.
    assert calls == ["b-1", "c-1"]
    polls = second.state["prime"]["polls"]
    assert polls["value"]["termination"]["reason"] == "condition_false"
    assert polls["meta"]["completed_passes"] == [0, 1]


def test_a_while_pass_that_never_started_asks_its_condition_and_runs_from_its_start(
    tmp_path: Path,
) -> None:
    orch = _write(
        tmp_path,
        "while.yml",
        {
            "effects": [
                {
                    "type": "loop",
                    "name": "polls",
                    "while": {"mode": "model", "template": "more?"},
                    "max_iterations": 2,
                    "body": _abc("{{iter.index}}"),
                }
            ]
        },
    )
    adapter = ScriptedAdapter()
    # The second condition check fails: pass 1 never starts.
    adapter.fail_condition = 2

    first = _run(orch, adapter)
    assert first.ok is False
    assert adapter.calls == [CONDITION, "a-0", "b-0", "c-0", CONDITION]
    saved = _saved(first)
    assert "iter_1" not in saved["prime"]["polls"]

    second, calls = _resume_calls(orch, adapter, first, saved)
    assert second.ok is True, second.error
    assert calls == [CONDITION, "a-1", "b-1", "c-1"]


def test_nested_loops_resume_inside_the_inner_loops_unfinished_pass(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "grid.yml",
        {
            "effects": [
                {
                    "type": "loop",
                    "name": "rows",
                    "flow": "chain",
                    "each": {"in": "input.rows", "as": "row"},
                    "body": [
                        _prompt("head", "head-{{row}}"),
                        {
                            "type": "loop",
                            "name": "cells",
                            "flow": "chain",
                            "each": {"in": "input.cols", "as": "col"},
                            "body": [
                                _prompt("x", "x-{{row}}{{col}}"),
                                _prompt("y", "y-{{row}}{{col}}"),
                            ],
                        },
                    ],
                }
            ]
        },
    )
    adapter = ScriptedAdapter()
    adapter.interrupt_prompts = {"y-b1"}

    first = _run(orch, adapter, inputs={"rows": ["a", "b"], "cols": ["0", "1"]})
    assert first.interrupted is True

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    assert calls == ["y-b1"]


def test_a_use_in_a_resumed_pass_continues_inside_its_recorded_child(tmp_path: Path) -> None:
    child = _write(
        tmp_path,
        "child.yml",
        {
            "interface": {
                "inputs": {"topic": {"required": True}},
                "outputs": {"last": {"path": "prime.c.value"}},
            },
            "effects": _abc("{{input.topic}}"),
        },
    )
    body = [
        _prompt("prep", "prep-{{item}}"),
        {"type": "use", "name": "sub", "path": str(child), "inputs": {"topic": "{{item}}"}},
    ]
    orch = _write(
        tmp_path,
        "parent.yml",
        {"runtime": {"state": {"record_children": True}}, "effects": [_each("frames", body)]},
    )
    adapter = ScriptedAdapter()
    adapter.fail_prompts = {"c-1"}

    first = _run(orch, adapter, inputs={"items": ["0", "1"]})
    assert first.ok is False

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    # Pass 1's `prep` and the child's `a` and `b` are reused.
    assert calls == ["c-1"]
    assert second.state["prime"]["frames"]["iter_1"]["sub"]["value"] == {"last": "ok:c-1"}


def test_a_use_in_a_resumed_pass_without_record_children_runs_its_child_from_its_start(
    tmp_path: Path,
) -> None:
    child = _write(
        tmp_path,
        "child.yml",
        {"interface": {"inputs": {"topic": {"required": True}}}, "effects": _abc("{{input.topic}}")},
    )
    body = [
        _prompt("prep", "prep-{{item}}"),
        {"type": "use", "name": "sub", "path": str(child), "inputs": {"topic": "{{item}}"}},
    ]
    orch = _write(tmp_path, "parent.yml", {"effects": [_each("frames", body)]})
    adapter = ScriptedAdapter()
    adapter.fail_prompts = {"c-1"}

    first = _run(orch, adapter, inputs={"items": ["0", "1"]})
    assert first.ok is False

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    assert calls == ["a-1", "b-1", "c-1"]


# -- if branches --------------------------------------------------------------


def _gate(condition: dict[str, Any], *, name: str | None = "gate") -> dict[str, Any]:
    effect: dict[str, Any] = {
        "type": "if",
        "if": condition,
        "then": _abc("then"),
        "else": [_prompt("x", "x-else"), _prompt("y", "y-else")],
    }
    if name is not None:
        effect["name"] = name
    return effect


FLAG = {"mode": "cel", "expr": "state.input.flag == true"}


def test_an_if_that_selects_its_saved_branch_again_resumes_inside_it(tmp_path: Path) -> None:
    orch = _write(tmp_path, "if.yml", {"effects": [_gate(FLAG), _prompt("after", "after")]})
    adapter = ScriptedAdapter()
    adapter.fail_prompts = {"b-then"}

    first = _run(orch, adapter, inputs={"flag": True})
    assert first.ok is False

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    assert calls == ["b-then", "c-then", "after"]
    gate = second.state["prime"]["gate"]
    assert gate["a"]["value"] == "ok:a-then"
    assert gate["value"]["branch"] == "then"
    assert [e["name"] for e in gate["value"]["effects"]] == ["a", "b", "c"]


def test_an_interrupted_if_branch_resumes_inside_it(tmp_path: Path) -> None:
    orch = _write(tmp_path, "if.yml", {"effects": [_gate(FLAG)]})
    adapter = ScriptedAdapter()
    adapter.interrupt_prompts = {"c-then"}

    first = _run(orch, adapter, inputs={"flag": True})
    assert first.interrupted is True

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    assert calls == ["c-then"]


def test_an_if_that_selects_the_other_branch_runs_it_from_its_start(tmp_path: Path) -> None:
    orch = _write(tmp_path, "if.yml", {"effects": [_gate(FLAG)]})
    adapter = ScriptedAdapter()
    adapter.fail_prompts = {"y-else"}

    first = _run(orch, adapter, inputs={"flag": False})
    assert first.ok is False
    assert first.state["prime"]["gate"]["meta"]["branch"] == "else"

    saved = _saved(first)
    saved["input"]["flag"] = True
    second, calls = _resume_calls(orch, adapter, first, saved)
    assert second.ok is True, second.error
    assert calls == ["a-then", "b-then", "c-then"]
    assert second.state["prime"]["gate"]["meta"]["branch"] == "then"


def test_a_model_mode_if_is_asked_again_before_it_resumes_inside_its_branch(
    tmp_path: Path,
) -> None:
    orch = _write(
        tmp_path, "if.yml", {"effects": [_gate({"mode": "model", "template": "go on?"})]}
    )
    adapter = ScriptedAdapter()
    adapter.fail_prompts = {"b-then"}

    first = _run(orch, adapter)
    assert first.ok is False

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    assert calls == [CONDITION, "b-then", "c-then"]


def test_an_if_in_a_resumed_pass_resumes_inside_its_branch(tmp_path: Path) -> None:
    body = [_prompt("prep", "prep-{{item}}"), _gate(FLAG)]
    orch = _write(tmp_path, "each.yml", {"effects": [_each("frames", body)]})
    adapter = ScriptedAdapter()
    adapter.fail_prompts = {"b-then"}

    first = _run(orch, adapter, inputs={"items": ["0"], "flag": True})
    assert first.ok is False

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    assert calls == ["b-then", "c-then"]


def test_a_use_in_a_resumed_branch_continues_inside_its_recorded_child(tmp_path: Path) -> None:
    child = _write(
        tmp_path,
        "child.yml",
        {"interface": {"inputs": {"topic": {"required": True}}}, "effects": _abc("{{input.topic}}")},
    )
    gate = {
        "type": "if",
        "name": "gate",
        "if": FLAG,
        "then": [
            _prompt("prep", "prep"),
            {"type": "use", "name": "sub", "path": str(child), "inputs": {"topic": "t"}},
        ],
    }
    orch = _write(
        tmp_path,
        "parent.yml",
        {"runtime": {"state": {"record_children": True}}, "effects": [gate]},
    )
    adapter = ScriptedAdapter()
    adapter.fail_prompts = {"b-t"}

    first = _run(orch, adapter, inputs={"flag": True})
    assert first.ok is False

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    assert calls == ["b-t", "c-t"]


# -- unchanged ----------------------------------------------------------------


def test_a_named_if_whose_condition_raised_runs_its_branch_from_its_start(
    tmp_path: Path,
) -> None:
    orch = _write(
        tmp_path, "if.yml", {"effects": [_gate({"mode": "model", "template": "go on?"})]}
    )
    adapter = ScriptedAdapter()
    adapter.fail_condition = 1

    first = _run(orch, adapter)
    assert first.ok is False
    assert "branch" not in first.state["prime"]["gate"]["meta"]

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    assert calls == [CONDITION, "a-then", "b-then", "c-then"]


def test_an_unnamed_if_reruns_its_branch_from_its_start(tmp_path: Path) -> None:
    orch = _write(tmp_path, "if.yml", {"effects": [_gate(FLAG, name=None)]})
    adapter = ScriptedAdapter()
    adapter.fail_prompts = {"b-then"}

    first = _run(orch, adapter, inputs={"flag": True})
    assert first.ok is False

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    assert calls == ["a-then", "b-then", "c-then"]


def test_an_unnamed_loop_reruns_whole(tmp_path: Path) -> None:
    loop = _each("frames", _abc("{{item}}"))
    del loop["name"]
    orch = _write(tmp_path, "each.yml", {"effects": [loop]})
    adapter = ScriptedAdapter()
    adapter.fail_prompts = {"b-1"}

    first = _run(orch, adapter, inputs={"items": ["0", "1"]})
    assert first.ok is False

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    assert calls == ["a-0", "b-0", "c-0", "a-1", "b-1", "c-1"]


def test_a_tree_flow_each_loop_reruns_whole(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "tree.yml",
        {"effects": [_each("frames", _abc("{{item}}"), flow="tree")]},
    )
    adapter = ScriptedAdapter()
    adapter.fail_prompts = {"b-1"}

    first = _run(orch, adapter, inputs={"items": ["0", "1"]})
    assert first.ok is False

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    assert sorted(calls) == ["a-0", "a-1", "b-0", "b-1", "c-0", "c-1"]


def test_a_reflector_in_a_resumed_pass_reruns_from_its_start_though_it_finished(tmp_path: Path) -> None:
    """Pins the rerun: a reflector records no ``meta.completed_at`` on success.

    So `--resume` cannot tell a finished reflector from an unfinished one and
    always runs it again from its start; #585 tracks changing that.
    """
    reflector = {
        "type": "reflector",
        "name": "planner",
        "effects": [_prompt("propose_steps", "plan-me {{item}}")],
    }
    body = [_prompt("a", "a-{{item}}"), reflector, _prompt("b", "b-{{item}}")]
    orch = _write(tmp_path, "each.yml", {"effects": [_each("frames", body)]})
    adapter = ScriptedAdapter()
    adapter.fail_prompts = {"b-0"}

    first = _run(orch, adapter, inputs={"items": ["0"]})
    assert first.ok is False
    assert adapter.calls == ["a-0", "plan-me", "b-0"]

    second, calls = _resume_calls(orch, adapter, first)
    assert second.ok is True, second.error
    # `a` is reused; the reflector plans again from its start.
    assert calls == ["plan-me", "b-0"]
    planner = second.state["prime"]["frames"]["iter_0"]["planner"]
    assert len(planner["meta"]["iterations"]) == 2
