"""Saved state writes each loop's final pass once (#220).

In memory ``prime.<loop>.last`` is an alias of the final completed
``iter_<N>``; saved state writes it as ``{"$ref": "iter_<N>"}`` instead of a
second full copy, and every path that reads saved state back relinks it so
``last`` is the same alias again. State saved before the reference form
existed (``last`` as a full copy) keeps loading.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from circuitry.adapters.base import GenerateResult
from circuitry.cli.app import _write_state_json
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.saved_state import (
    compact_last_aliases,
    dumps_saved_state,
    link_last_refs,
)
from circuitry.core.store import Store
from circuitry.tui.inspector import load_state_file


@dataclass
class EchoAdapter:
    """Echoes every rendered prompt back, so a step's value IS its template."""

    name: str = "echo"
    prompts: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)
        return GenerateResult(text=prompt, raw={"model": model})


#: Outer loop over two items, each pass running an inner loop over two more.
NESTED_LOOPS: dict[str, Any] = {
    "effects": [
        {
            "type": "loop",
            "name": "outer",
            "each": {"in": "input.outer_items", "as": "o"},
            "body": [
                {
                    "type": "loop",
                    "name": "inner",
                    "each": {"in": "input.inner_items", "as": "i"},
                    "body": [
                        {"type": "prompt", "name": "step", "template": "PASS {{o}}-{{i}}"}
                    ],
                }
            ],
        }
    ]
}

NESTED_INPUT = {"outer_items": ["a", "b"], "inner_items": ["x", "y"]}

#: A second run that only reads the first run's loops through `last`.
READER: dict[str, Any] = {
    "effects": [
        {
            "type": "prompt",
            "name": "reader",
            "template": "read=[{{prime.outer.last.inner.last.step.value}}]",
        }
    ]
}


def _run_nested() -> dict[str, Any]:
    state: dict[str, Any] = {"input": dict(NESTED_INPUT)}
    root = compile_orchestration(orch=NESTED_LOOPS, root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
        store=Store(state)
    )
    return state


def _write_orch(path: Path, orch: dict[str, Any]) -> Path:
    path.write_text(json.dumps(orch), encoding="utf-8")
    return path


def _run_reader(tmp_path: Path, **source: Any) -> tuple[EchoAdapter, Any]:
    adapter = EchoAdapter()
    result = run(
        RunRequest(
            orchestration_path=_write_orch(tmp_path / "reader.json", READER),
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            adapter=adapter,
            skip_preflight=True,
            **source,
        )
    )
    assert result.ok, result.error
    return adapter, result


def test_saved_state_writes_each_loops_final_pass_once() -> None:
    state = _run_nested()
    saved = json.loads(dumps_saved_state(state))

    outer = saved["prime"]["outer"]
    assert outer["last"] == {"$ref": "iter_1"}
    assert outer["iter_1"]["inner"]["last"] == {"$ref": "iter_1"}
    assert outer["iter_0"]["inner"]["last"] == {"$ref": "iter_1"}
    # The final pass of the nested pair is written exactly once; a plain
    # json.dumps of the aliased state writes it four times (inner iter_1 and
    # inner `last`, both again inside the outer `last`).
    final_value = '"value": "PASS b-y"'
    assert json.dumps(saved).count(final_value) == 1
    assert json.dumps(state).count(final_value) == 4


def test_saving_leaves_the_in_memory_alias_untouched() -> None:
    state = _run_nested()
    dumps_saved_state(state, pretty=True)
    outer = state["prime"]["outer"]
    assert outer["last"] is outer["iter_1"]
    assert outer["iter_1"]["inner"]["last"] is outer["iter_1"]["inner"]["iter_1"]


def test_state_without_aliases_is_returned_as_is() -> None:
    state = {"input": {"items": [1, 2]}, "prime": {"greet": {"value": "hi"}}}
    assert compact_last_aliases(state) is state


def test_a_last_that_is_not_an_alias_is_saved_verbatim() -> None:
    """A step named `last` in a plain sequence is data, not a loop alias."""
    node = {"iter_0": {"value": 1}, "last": {"value": 1}}
    assert compact_last_aliases(node) is node


def test_link_last_refs_restores_the_alias() -> None:
    state = _run_nested()
    loaded = link_last_refs(json.loads(dumps_saved_state(state)))

    # Same content as the in-memory state, and `last` is an alias again.
    assert loaded == json.loads(json.dumps(state))
    outer = loaded["prime"]["outer"]
    assert outer["last"] is outer["iter_1"]
    assert outer["iter_0"]["inner"]["last"] is outer["iter_0"]["inner"]["iter_1"]


def test_link_last_refs_leaves_unresolvable_and_full_copy_forms_alone() -> None:
    dangling = {"iter_0": {"value": 1}, "last": {"$ref": "iter_7"}}
    not_iter = {"meta": {"value": 1}, "last": {"$ref": "meta"}}
    full_copy = {"iter_0": {"value": 1}, "last": {"value": 1}}
    link_last_refs(dangling)
    link_last_refs(not_iter)
    link_last_refs(full_copy)
    assert dangling["last"] == {"$ref": "iter_7"}
    assert not_iter["last"] == {"$ref": "meta"}
    assert full_copy["last"] == {"value": 1}
    assert full_copy["last"] is not full_copy["iter_0"]


def test_out_file_holds_references(tmp_path: Path) -> None:
    out = tmp_path / "out.json"
    _write_state_json(out=out, state=_run_nested(), pretty=False)
    saved = json.loads(out.read_text(encoding="utf-8"))
    assert saved["prime"]["outer"]["last"] == {"$ref": "iter_1"}


def test_a_previous_runs_state_file_resolves_last(tmp_path: Path) -> None:
    """`--state` with a saved run: `prime.<loop>.last` reads through the ref."""
    previous = tmp_path / "previous.json"
    _write_state_json(out=previous, state=_run_nested(), pretty=True)

    adapter, result = _run_reader(tmp_path, state_path=previous)

    assert adapter.prompts == ["read=[PASS b-y]"]
    outer = result.state["prime"]["outer"]
    assert outer["last"] is outer["iter_1"]


def test_a_previous_runs_state_as_initial_state_resolves_last(tmp_path: Path) -> None:
    """`--state` merged with `-e`, a TUI replay, REST: all pass initial_state."""
    saved = json.loads(dumps_saved_state(_run_nested()))
    adapter, _ = _run_reader(tmp_path, state_path=None, initial_state=saved)
    assert adapter.prompts == ["read=[PASS b-y]"]


def test_old_state_with_full_last_copies_keeps_loading(tmp_path: Path) -> None:
    previous = tmp_path / "previous.json"
    previous.write_text(json.dumps(_run_nested()), encoding="utf-8")

    adapter, _ = _run_reader(tmp_path, state_path=previous)

    assert adapter.prompts == ["read=[PASS b-y]"]


def test_tui_state_file_relinks_last(tmp_path: Path) -> None:
    """The Runs view browses a saved file exactly like the live run."""
    path = tmp_path / "out.json"
    _write_state_json(out=path, state=_run_nested(), pretty=False)

    loaded = load_state_file(path)

    assert loaded.ok
    assert loaded.state is not None
    outer = loaded.state["prime"]["outer"]
    assert outer["last"] is outer["iter_1"]
