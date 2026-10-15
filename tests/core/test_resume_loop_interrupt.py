"""A loop stopped by an interrupt keeps its completed passes for `--resume` (#574).

Ctrl-C, SIGTERM/SIGHUP and a cancelled run raise a ``BaseException`` that is
not an ``Exception``. A named loop records on it what it records on a
failure (``meta.completed_passes``, ``meta.completed_at``, the interruption
as ``meta.error``) and re-raises the same exception, so `cof run --resume`
continues at the first pass that didn't finish, and the run still ends as an
interrupted run.

Each resumed run starts from the first run's state as saved (`--out`'s JSON
form), the way `cof run --resume` reads it back.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.interrupts import SigTermInterrupt
from circuitry.cli.runtime_shim import RunRequest, RunResult, run
from circuitry.core.cancellation import RunCancelledBySignal
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.saved_state import dumps_saved_state
from circuitry.core.store import Store


class InterruptingAdapter:
    """Records every prompt; raises the exception mapped to a prompt, once."""

    name = "interrupting"

    def __init__(self) -> None:
        self.calls: list[str] = []
        self.raises: dict[str, BaseException] = {}

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append(prompt)
        exc = self.raises.pop(prompt, None)
        if exc is not None:
            raise exc
        return GenerateResult(text=f"ok:{prompt}", raw={})


def _write(tmp_path: Path, name: str, content: dict[str, Any]) -> Path:
    path = tmp_path / name
    path.write_text(yaml.safe_dump(content, sort_keys=False), encoding="utf-8")
    return path


def _prompt(name: str, template: str) -> dict[str, Any]:
    return {"type": "prompt", "name": name, "template": template}


def _each_loop(name: str, in_path: str, body: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "type": "loop",
        "name": name,
        "flow": "chain",
        "each": {"in": in_path, "as": "item"},
        "body": body,
    }


def _run(
    orch: Path,
    adapter: InterruptingAdapter,
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


ITEMS = {"items": ["0", "1", "2", "3"]}


def _frames_orch(tmp_path: Path) -> Path:
    return _write(
        tmp_path,
        "frames.yml",
        {"effects": [_each_loop("frames", "input.items", [_prompt("render", "frame-{{item}}")])]},
    )


def test_an_each_loop_interrupted_by_ctrl_c_resumes_at_its_first_unfinished_pass(
    tmp_path: Path,
) -> None:
    orch = _frames_orch(tmp_path)
    adapter = InterruptingAdapter()
    adapter.raises = {"frame-2": KeyboardInterrupt()}

    first = _run(orch, adapter, inputs=ITEMS)
    assert first.interrupted is True
    assert first.error == "Interrupted (Ctrl-C/SIGINT)"
    meta = first.state["prime"]["frames"]["meta"]
    assert meta["completed_passes"] == [0, 1]
    assert meta["completed_at"]
    assert meta["error"] == "Interrupted (Ctrl-C/SIGINT)"
    value = first.state["prime"]["frames"]["value"]
    assert value["iterations"] == 2
    assert value["termination"] == {"reason": "error", "detail": "Interrupted (Ctrl-C/SIGINT)"}

    second = _run(orch, adapter, saved=_saved(first))
    assert second.ok is True, second.error
    assert adapter.calls[3:] == ["frame-2", "frame-3"]
    assert second.state["prime"]["frames"]["meta"]["completed_passes"] == [0, 1, 2, 3]


def test_a_while_loop_stopped_by_sigterm_resumes_at_its_first_unfinished_pass(
    tmp_path: Path,
) -> None:
    orch = _write(
        tmp_path,
        "polls.yml",
        {
            "effects": [
                {
                    "type": "loop",
                    "name": "polls",
                    "while": {"mode": "cel", "expr": "state.iter.count < 4"},
                    "max_iterations": 10,
                    "body": [_prompt("poll", "poll-{{iter.index}}")],
                }
            ]
        },
    )
    adapter = InterruptingAdapter()
    adapter.raises = {"poll-2": SigTermInterrupt()}

    first = _run(orch, adapter)
    assert first.interrupted is True
    assert first.sigterm is True
    meta = first.state["prime"]["polls"]["meta"]
    assert meta["completed_passes"] == [0, 1]
    assert meta["error"] == "Interrupted (SIGTERM)"

    second = _run(orch, adapter, saved=_saved(first))
    assert second.ok is True, second.error
    assert adapter.calls[3:] == ["poll-2", "poll-3"]


def test_an_each_loop_in_a_cancelled_run_resumes_at_its_first_unfinished_pass(
    tmp_path: Path,
) -> None:
    orch = _frames_orch(tmp_path)
    adapter = InterruptingAdapter()
    adapter.raises = {"frame-1": RunCancelledBySignal()}

    first = _run(orch, adapter, inputs=ITEMS)
    assert first.interrupted is True
    meta = first.state["prime"]["frames"]["meta"]
    assert meta["completed_passes"] == [0]
    assert meta["error"].startswith("Interrupted (")

    second = _run(orch, adapter, saved=_saved(first))
    assert second.ok is True, second.error
    assert adapter.calls[2:] == ["frame-1", "frame-2", "frame-3"]


def test_nested_loops_interrupted_by_ctrl_c_keep_their_completed_passes(
    tmp_path: Path,
) -> None:
    orch = _write(
        tmp_path,
        "grid.yml",
        {
            "effects": [
                _each_loop(
                    "rows",
                    "input.rows",
                    [
                        {
                            "type": "loop",
                            "name": "cells",
                            "flow": "chain",
                            "each": {"in": "input.cols", "as": "col"},
                            "body": [_prompt("cell", "{{item}}-{{col}}")],
                        }
                    ],
                )
            ]
        },
    )
    adapter = InterruptingAdapter()
    adapter.raises = {"b-1": KeyboardInterrupt()}

    first = _run(orch, adapter, inputs={"rows": ["a", "b"], "cols": ["0", "1"]})
    assert first.interrupted is True
    rows = first.state["prime"]["rows"]
    assert rows["meta"]["completed_passes"] == [0]
    assert rows["meta"]["error"] == "Interrupted (Ctrl-C/SIGINT)"
    cells = rows["iter_1"]["cells"]
    assert cells["meta"]["completed_passes"] == [0]
    assert cells["meta"]["error"] == "Interrupted (Ctrl-C/SIGINT)"

    second = _run(orch, adapter, saved=_saved(first))
    assert second.ok is True, second.error
    # The outer loop's finished pass `a` is not run again.
    assert "a-0" not in adapter.calls[4:]
    assert "a-1" not in adapter.calls[4:]
    assert adapter.calls[-1] == "b-1"


def test_a_loop_in_a_recorded_use_child_keeps_its_completed_passes_on_ctrl_c(
    tmp_path: Path,
) -> None:
    child = _write(
        tmp_path,
        "child.yml",
        {
            "interface": {"inputs": {"items": {"required": True}}},
            "effects": [
                _each_loop("frames", "input.items", [_prompt("render", "frame-{{item}}")])
            ],
        },
    )
    orch = _write(
        tmp_path,
        "parent.yml",
        {
            "runtime": {"state": {"record_children": True}},
            "effects": [
                {
                    "type": "use",
                    "name": "sub",
                    "path": str(child),
                    "inputs": {"items": {"from": "input.items"}},
                    "outputs": {"n": "prime.frames.value.iterations"},
                }
            ],
        },
    )
    adapter = InterruptingAdapter()
    adapter.raises = {"frame-2": KeyboardInterrupt()}

    first = _run(orch, adapter, inputs=ITEMS)
    assert first.interrupted is True
    frames = first.state["prime"]["sub"]["frames"]
    assert frames["meta"]["completed_passes"] == [0, 1]
    assert frames["meta"]["error"] == "Interrupted (Ctrl-C/SIGINT)"

    second = _run(orch, adapter, saved=_saved(first))
    assert second.ok is True, second.error
    assert adapter.calls[3:] == ["frame-2", "frame-3"]
    assert second.state["prime"]["sub"]["value"] == {"n": 4}


def test_an_interrupted_loop_reraises_the_same_exception_and_finally_still_runs(
    tmp_path: Path,
) -> None:
    orch = {
        "effects": [_each_loop("frames", "input.items", [_prompt("render", "frame-{{item}}")])],
        "finally": [_prompt("cleanup", "cleanup")],
    }
    interrupt = KeyboardInterrupt()
    adapter = InterruptingAdapter()
    adapter.raises = {"frame-1": interrupt}
    state: dict[str, Any] = {"input": dict(ITEMS)}

    root = compile_orchestration(orch=orch, root_name="prime")
    with pytest.raises(KeyboardInterrupt) as raised:
        DynamicRuntime(root, adapter=adapter, model="unit-test").execute(store=Store(state))

    assert raised.value is interrupt
    assert adapter.calls == ["frame-0", "frame-1", "cleanup"]
    assert state["prime"]["cleanup"]["value"] == "ok:cleanup"
    assert state["prime"]["frames"]["meta"]["completed_passes"] == [0]
