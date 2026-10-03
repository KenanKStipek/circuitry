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


def test_each_chain_loop_progress_counts_a_failed_pass_as_done() -> None:
    """Tree flow counts a failed pass toward ``done`` the moment its branch
    finishes (success or error). A chain loop under ``on_error: continue``
    used to leave ``done`` frozen on a failed pass instead — the line would
    freeze and the ETA would overestimate the remaining time (#331 finding
    6)."""

    @dataclass
    class _BoomOnSecond:
        name: str = "echo"
        calls: list[str] = field(default_factory=list)

        def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
            self.calls.append(prompt)
            if prompt == "b":
                raise RuntimeError("scripted failure")
            return GenerateResult(text=prompt, raw={"model": model})

    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "shots",
                "on_error": "continue",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "{{item}}"},
                ],
            },
        ]
    }
    store = Store(state={"input": {"items": ["a", "b", "c"]}})
    root = compile_orchestration(orch=orch, root_name="prime")
    DynamicRuntime(root, adapter=_BoomOnSecond(), model="unit-test").execute(store=store)

    progress = store.state["prime"]["shots"]["meta"]["progress"]
    # All 3 passes ran (one failed) — done reaches total, not 2 (successes
    # only), which would freeze the line one pass early.
    assert progress["done"] == 3
    assert progress["total"] == 3


def test_while_loop_progress_counts_a_failed_pass_as_done() -> None:
    @dataclass
    class _BoomOnSecond:
        name: str = "echo"
        calls: int = 0

        def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
            self.calls += 1
            if self.calls == 2:
                raise RuntimeError("scripted failure")
            return GenerateResult(text="ok", raw={"model": model})

    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "attempts",
                "on_error": "continue",
                "while": {"mode": "cel", "expr": "state.iter.count < 3"},
                "max_iterations": 3,
                "body": [
                    {"type": "prompt", "name": "step", "template": "go"},
                ],
            },
        ]
    }
    store = Store(state={})
    root = compile_orchestration(orch=orch, root_name="prime")
    DynamicRuntime(root, adapter=_BoomOnSecond(), model="unit-test").execute(store=store)

    progress = store.state["prime"]["attempts"]["meta"]["progress"]
    assert progress["done"] == 3
    assert progress["total"] == 3


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


def test_three_nested_named_chain_loops_with_progress_display_do_not_deadlock(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Regression for #331 finding 1: the progress-status guard used to
    decide ownership *and* yield inside the same non-reentrant lock, so the
    third of three nested named loops on one thread blocked forever waiting
    on a lock the still-suspended second loop's generator never released.
    Only the outermost loop should ever claim the live status line; the
    nested ones get ``None`` back immediately rather than blocking.
    """
    calls: list[str] = []
    monkeypatch.setattr(
        loop_mod._console, "status", lambda text: calls.append(text) or _NullStatus()
    )
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "each": {"in": "input.items", "as": "o"},
                "body": [
                    {
                        "type": "loop",
                        "name": "middle",
                        "each": {"in": "input.items", "as": "m"},
                        "body": [
                            {
                                "type": "loop",
                                "name": "inner",
                                "each": {"in": "input.items", "as": "i"},
                                "body": [
                                    {
                                        "type": "prompt",
                                        "name": "step",
                                        "template": "{{i}}",
                                    },
                                ],
                            },
                        ],
                    },
                ],
            },
        ]
    }
    store = Store(state={"input": {"items": ["a"]}})
    root = compile_orchestration(orch=orch, root_name="prime")
    DynamicRuntime(
        root, adapter=EchoAdapter(), model="unit-test", verbose=True, progress_display=True
    ).execute(store=store)

    # Only the outermost loop ever claims the single live region — the
    # nested ones see it already held and skip without blocking.
    assert len(calls) == 1
    assert calls[0].startswith("outer 0/1")


class _NullStatus:
    def __enter__(self) -> _NullStatus:
        return self

    def __exit__(self, *exc: object) -> None:
        return None

    def update(self, _text: str) -> None:
        pass


def test_tree_loop_iter_tracker_renders_a_progress_header(monkeypatch: pytest.MonkeyPatch) -> None:
    """A tree loop's animated per-iteration tracker otherwise has no
    `k/N, ~ETA left` line at all — only per-iteration pending/running/done
    rows and an ancestor line with just an elapsed timer (#331 finding 3).
    """
    tracker = loop_mod._LoopIterTracker(
        total=4,
        name="step",
        indent="  ",
        icon="◆",
        color="cyan",
        ancestors=[],
        loop_name="shots",
    )
    tracker.set_progress({"done": 2, "total": 4, "elapsed_s": 10.0, "eta_s": 10.0})
    rendered = tracker.__rich__()
    assert "shots 2/4" in rendered


def test_tree_loop_iter_tracker_omits_header_when_unnamed() -> None:
    """An unnamed tree loop has no `meta.progress` and no name to show, so
    the tracker shouldn't fabricate a header for it."""
    tracker = loop_mod._LoopIterTracker(
        total=2,
        name="step",
        indent="  ",
        icon="◆",
        color="cyan",
        ancestors=[],
        loop_name=None,
    )
    rendered = tracker.__rich__()
    assert "/2" not in rendered
