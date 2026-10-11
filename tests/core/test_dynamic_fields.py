"""`dynamic`'s `max_concurrency`, `on_error`, `stop_on_error` and `labels`
(#251 part 1): accepted, documented, and previously dropped by the
compiler. Also #289: a tree-flow dynamic's own `meta.error` names the
failing child's path the same way chain flow does.
"""

from __future__ import annotations

import threading
import time
from dataclasses import dataclass, field

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"model": model})


@dataclass
class SlowFailAdapter:
    """Sleeps, then raises, for one named prompt; answers every other prompt
    immediately. Used to make ``stop_on_error`` timing-independent: the
    failing child takes real time to fail, so a correct implementation
    must not depend on it failing before its siblings are even submitted.
    """

    failing_prompt: str
    delay: float = 0.05
    name: str = "slowfail"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        if prompt == self.failing_prompt:
            time.sleep(self.delay)
            raise RuntimeError("boom")
        return GenerateResult(text=prompt, raw={"model": model})


@dataclass
class SleepingAdapter:
    """Records how many calls are in flight at once, sleeping `delay` each.

    An optional ``barrier``, sized to the concurrency a test expects, makes
    the peak deterministic instead of inferred from the wall clock (#456):
    on a slow runner a sleep can end before a sibling even starts, so a
    tight upper bound doesn't prove a lower one. A child waits on the
    barrier while counted as in flight; a real regression that never gets
    enough callers there times out with ``BrokenBarrierError`` instead of
    the test hanging or just racing the clock.
    """

    delay: float = 0.08
    name: str = "sleeping"
    barrier: threading.Barrier | None = None
    max_concurrent: int = field(default=0)
    _lock: threading.Lock = field(default_factory=threading.Lock)
    _in_flight: int = field(default=0)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        with self._lock:
            self._in_flight += 1
            self.max_concurrent = max(self.max_concurrent, self._in_flight)
        if self.barrier is not None:
            self.barrier.wait()
        time.sleep(self.delay)
        with self._lock:
            self._in_flight -= 1
        return GenerateResult(text=prompt, raw={"model": model})


def _tree_of_prompts(n: int, *, max_concurrency: int | None = None) -> dict:
    dynamic: dict = {
        "type": "dynamic",
        "name": "fanout",
        "flow": "tree",
        "effects": [
            {"type": "prompt", "name": f"p{i}", "template": f"do {i}"}
            for i in range(n)
        ],
    }
    if max_concurrency is not None:
        dynamic["max_concurrency"] = max_concurrency
    return {"effects": [dynamic]}


def test_max_concurrency_bounds_the_tree_worker_pool() -> None:
    """max_concurrency: 1 serializes four otherwise-parallel children."""
    orch = _tree_of_prompts(4, max_concurrency=1)
    root = compile_orchestration(orch=orch, root_name="prime")
    adapter = SleepingAdapter(delay=0.08)

    DynamicRuntime(root, adapter=adapter, model="unit-test").execute(store=Store({}))

    assert adapter.max_concurrent == 1


def test_unset_max_concurrency_still_runs_every_child_at_once() -> None:
    """Omitted max_concurrency keeps today's behavior: no bound at all.

    A barrier sized to all four children (#456), not a sleep, proves the
    peak: on a slow runner the fourth child can start after the first has
    already finished, so ``== 4`` inferred purely from an 80ms sleep window
    was observed to flake in CI.
    """
    orch = _tree_of_prompts(4)
    root = compile_orchestration(orch=orch, root_name="prime")
    adapter = SleepingAdapter(delay=0.08, barrier=threading.Barrier(4, timeout=5))

    DynamicRuntime(root, adapter=adapter, model="unit-test").execute(store=Store({}))

    assert adapter.max_concurrent == 4


def test_max_concurrency_two_bounds_four_children_to_two_at_a_time() -> None:
    """A barrier sized to the cap (#456), not a sleep, proves two children
    reach it together: the pool's own two worker threads each pick up two
    of the four children in turn, so the (reusable) barrier is satisfied
    twice.
    """
    orch = _tree_of_prompts(4, max_concurrency=2)
    root = compile_orchestration(orch=orch, root_name="prime")
    adapter = SleepingAdapter(delay=0.08, barrier=threading.Barrier(2, timeout=5))

    DynamicRuntime(root, adapter=adapter, model="unit-test").execute(store=Store({}))

    assert adapter.max_concurrent == 2


# ── on_error ─────────────────────────────────────────────────────────────────


def _chain_with_failing_child(on_error: str | None) -> dict:
    d: dict = {
        "type": "dynamic",
        "name": "d",
        "flow": "chain",
        "effects": [
            {
                "type": "tool",
                "name": "bad",
                "provider": "json",
                "params": {"mode": "parse", "input": "not json"},
            }
        ],
    }
    if on_error is not None:
        d["on_error"] = on_error
    return {
        "effects": [
            d,
            {"type": "prompt", "name": "after", "template": "ran"},
        ]
    }


def test_dynamic_default_on_error_fail_propagates_and_stops_the_run() -> None:
    orch = _chain_with_failing_child(None)
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {}

    with pytest.raises(RuntimeError):
        DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=Store(state))

    assert "after" not in state.get("prime", {})


def test_dynamic_on_error_skip_isolates_the_failure_and_the_run_continues() -> None:
    """The exact #251 repro: on_error: skip around a failing child lets the
    sibling after the dynamic run."""
    orch = _chain_with_failing_child("skip")
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {}

    DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=Store(state))

    assert state["prime"]["d"]["value"] is False
    assert state["prime"]["d"]["meta"]["error"] is not None
    assert state["prime"]["after"]["value"] == "ran"


def test_dynamic_on_error_continue_also_isolates_the_failure() -> None:
    orch = _chain_with_failing_child("continue")
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {}

    DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=Store(state))

    assert state["prime"]["d"]["value"] is False
    assert state["prime"]["after"]["value"] == "ran"


def test_dynamic_on_error_skip_in_tree_flow_also_isolates_the_failure() -> None:
    d = {
        "type": "dynamic",
        "name": "d",
        "flow": "tree",
        "on_error": "skip",
        "effects": [
            {
                "type": "tool",
                "name": "bad",
                "provider": "json",
                "params": {"mode": "parse", "input": "not json"},
            }
        ],
    }
    orch = {"effects": [d, {"type": "prompt", "name": "after", "template": "ran"}]}
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {}

    DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=Store(state))

    assert state["prime"]["d"]["value"] is False
    assert state["prime"]["after"]["value"] == "ran"


# ── stop_on_error ────────────────────────────────────────────────────────────


def test_stop_on_error_cancels_children_not_yet_started() -> None:
    """max_concurrency: 1 plus stop_on_error: true means only the first
    (failing) child ever starts; the rest are cancelled before they run.

    The failing child sleeps before it raises, so this is deterministic
    rather than depending on it failing before its siblings are even
    submitted: the one worker thread must not pick up ``never_a`` until
    after ``bad``'s failure is recorded, however long ``bad`` takes.
    """
    d = {
        "type": "dynamic",
        "name": "d",
        "flow": "tree",
        "max_concurrency": 1,
        "stop_on_error": True,
        "on_error": "skip",
        "effects": [
            {"type": "prompt", "name": "bad", "template": "bad"},
            {"type": "prompt", "name": "never_a", "template": "a"},
            {"type": "prompt", "name": "never_b", "template": "b"},
        ],
    }
    orch = {"effects": [d]}
    root = compile_orchestration(orch=orch, root_name="prime")
    adapter = SlowFailAdapter(failing_prompt="bad")
    state: dict = {}

    DynamicRuntime(root, adapter=adapter, model="m").execute(store=Store(state))

    assert "never_a" not in state["prime"]["d"]
    assert "never_b" not in state["prime"]["d"]


def test_without_stop_on_error_all_tree_children_run_to_completion() -> None:
    d = {
        "type": "dynamic",
        "name": "d",
        "flow": "tree",
        "max_concurrency": 1,
        "on_error": "skip",
        "effects": [
            {
                "type": "tool",
                "name": "bad",
                "provider": "json",
                "params": {"mode": "parse", "input": "not json"},
            },
            {"type": "prompt", "name": "still_runs", "template": "a"},
        ],
    }
    orch = {"effects": [d]}
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {}

    DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=Store(state))

    assert state["prime"]["d"]["still_runs"]["value"] == "a"


# ── labels ───────────────────────────────────────────────────────────────────


def test_labels_are_recorded_on_meta() -> None:
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "d",
                "labels": {"team": "platform", "tier": "1"},
                "effects": [{"type": "prompt", "name": "s", "template": "x"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {}

    DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=Store(state))

    assert state["prime"]["d"]["meta"]["labels"] == {"team": "platform", "tier": "1"}


def test_no_labels_records_none() -> None:
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "d",
                "effects": [{"type": "prompt", "name": "s", "template": "x"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {}

    DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=Store(state))

    assert state["prime"]["d"]["meta"]["labels"] is None


# ── #289: tree flow names the failing child's path, like chain flow ────────


def test_tree_flow_error_names_the_failing_child_path_like_chain_does() -> None:
    def _dynamic(flow: str) -> dict:
        return {
            "effects": [
                {
                    "type": "dynamic",
                    "name": "context",
                    "flow": flow,
                    "effects": [
                        {
                            "type": "tool",
                            "name": "ok_step",
                            "provider": "json",
                            "params": {"mode": "stringify", "input": "fine"},
                        },
                        {
                            "type": "tool",
                            "name": "search",
                            "provider": "json",
                            "params": {"mode": "parse", "input": "not json"},
                        },
                    ],
                }
            ]
        }

    chain_root = compile_orchestration(orch=_dynamic("chain"), root_name="prime")
    tree_root = compile_orchestration(orch=_dynamic("tree"), root_name="prime")

    chain_state: dict = {}
    tree_state: dict = {}

    with pytest.raises(RuntimeError):
        DynamicRuntime(chain_root, adapter=EchoAdapter(), model="m").execute(
            store=Store(chain_state)
        )
    with pytest.raises(RuntimeError):
        DynamicRuntime(tree_root, adapter=EchoAdapter(), model="m").execute(
            store=Store(tree_state)
        )

    chain_error = chain_state["prime"]["context"]["meta"]["error"]
    tree_error = tree_state["prime"]["context"]["meta"]["error"]

    assert chain_error.startswith("context.search:")
    assert tree_error.startswith("context.search:")


# ── several tree failures: each keeps its own error type in the listing ────


def test_tree_flow_several_failures_keep_their_own_error_type() -> None:
    """TreeExecutionError's listing names each failure's own error type
    (ValueError, from the json tool's parse failure here), not the
    RuntimeError every entry is wrapped in to carry its child's path."""
    d = {
        "type": "dynamic",
        "name": "d",
        "flow": "tree",
        "effects": [
            {
                "type": "tool",
                "name": "bad1",
                "provider": "json",
                "params": {"mode": "parse", "input": "not json"},
            },
            {
                "type": "tool",
                "name": "bad2",
                "provider": "json",
                "params": {"mode": "parse", "input": "also not json"},
            },
        ],
    }
    orch = {"effects": [d]}
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {}

    with pytest.raises(RuntimeError) as exc_info:
        DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(
            store=Store(state)
        )

    message = str(exc_info.value)
    assert "2 effects failed in parallel" in message
    assert message.count("ValueError") == 2
    assert "RuntimeError" not in message
