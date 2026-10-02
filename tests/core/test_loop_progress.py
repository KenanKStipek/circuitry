"""``meta.progress`` on a named loop's own node, and the interactive
progress-status line gated by ``progress_display`` (#271).

The contract under test:

* while a loop runs, its node carries ``meta.progress`` =
  ``{done, total, elapsed_s, eta_s}``; ``total`` is the collection length
  for ``each`` loops (chain and tree flow alike) and ``max_iterations``
  (or ``None``) for ``while`` loops; ``eta_s`` is ``None`` until at least
  one pass has completed, or whenever ``total`` itself is unknown;
* the state a chain loop publishes via ``on_write`` after each pass (#299)
  already carries the updated progress, so an observer sees it advance
  pass by pass, not only once at the end;
* the single interactive progress-status line only appears when
  ``progress_display`` is on, and never otherwise — including the default.
"""

from __future__ import annotations

from copy import deepcopy
from dataclasses import dataclass, field

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core import loop as loop_mod
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class EchoAdapter:
    name: str = "echo"
    prompts: list[str] = field(default_factory=list)

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        self.prompts.append(prompt)
        return GenerateResult(text=prompt, raw={"model": model})


def _each_orch() -> dict:
    return {
        "effects": [
            {
                "type": "loop",
                "name": "shots",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "{{item}}"},
                ],
            },
        ]
    }


def test_each_chain_loop_progress_reaches_done_equals_total() -> None:
    store = Store(state={"input": {"items": ["a", "b", "c"]}})
    root = compile_orchestration(orch=_each_orch(), root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    progress = store.state["prime"]["shots"]["meta"]["progress"]
    assert progress["done"] == 3
    assert progress["total"] == 3
    assert isinstance(progress["elapsed_s"], float)
    assert progress["eta_s"] == pytest.approx(0.0, abs=0.001)


def test_each_chain_loop_progress_advances_pass_by_pass() -> None:
    snapshots: list[dict] = []
    store = Store(
        state={"input": {"items": ["a", "b", "c"]}},
        on_write=lambda s: snapshots.append(deepcopy(s)),
    )
    root = compile_orchestration(orch=_each_orch(), root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    progress_dones = [
        s["prime"]["shots"]["meta"]["progress"]["done"]
        for s in snapshots
        if "progress" in s.get("prime", {}).get("shots", {}).get("meta", {})
    ]
    # Monotonically non-decreasing, and more than one snapshot carries it —
    # not just one write at the very end (progress is only ever published
    # once a pass actually finishes, so the first value seen is 1, not 0).
    assert progress_dones == sorted(progress_dones)
    assert len(set(progress_dones)) > 1
    assert progress_dones[-1] == 3


def test_each_tree_loop_progress_reaches_done_equals_total() -> None:
    store = Store(state={"input": {"items": ["a", "b", "c", "d"]}})
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "shots",
                "flow": "tree",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "{{item}}"},
                ],
            },
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    progress = store.state["prime"]["shots"]["meta"]["progress"]
    assert progress["done"] == 4
    assert progress["total"] == 4


def test_while_loop_progress_total_is_max_iterations() -> None:
    store = Store(state={"input": {}})
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "attempts",
                "min_iterations": 1,
                "max_iterations": 3,
                "while": {"mode": "cel", "expr": "state.iter.count < 3"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "pass {{_loop_index}}"},
                ],
            },
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    progress = store.state["prime"]["attempts"]["meta"]["progress"]
    assert progress["total"] == 3
    assert progress["done"] == 3
    assert progress["eta_s"] == pytest.approx(0.0, abs=0.001)


def test_while_loop_without_max_iterations_has_unknown_total_and_eta() -> None:
    store = Store(state={"input": {}})
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "attempts",
                "min_iterations": 1,
                "while": {"mode": "cel", "expr": "state.iter.count < 2"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "pass {{_loop_index}}"},
                ],
            },
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    progress = store.state["prime"]["attempts"]["meta"]["progress"]
    assert progress["done"] == 2
    # total unknown (no max_iterations) => eta is never computed either.
    assert progress["total"] is None
    assert progress["eta_s"] is None


def test_progress_display_off_by_default_no_status_line(monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[str] = []
    monkeypatch.setattr(
        loop_mod._console, "status", lambda text: calls.append(text) or _NullStatus()
    )
    store = Store(state={"input": {"items": ["a"]}})
    root = compile_orchestration(orch=_each_orch(), root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)
    assert calls == []


def test_progress_display_on_shows_one_status_line(monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[str] = []
    monkeypatch.setattr(
        loop_mod._console, "status", lambda text: calls.append(text) or _NullStatus()
    )
    store = Store(state={"input": {"items": ["a", "b"]}})
    root = compile_orchestration(orch=_each_orch(), root_name="prime")
    DynamicRuntime(
        root, adapter=EchoAdapter(), model="unit-test", verbose=True, progress_display=True
    ).execute(store=store)
    assert len(calls) == 1
    assert calls[0].startswith("shots 0/2")


class _NullStatus:
    def __enter__(self) -> _NullStatus:
        return self

    def __exit__(self, *exc: object) -> None:
        return None

    def update(self, _text: str) -> None:
        pass
