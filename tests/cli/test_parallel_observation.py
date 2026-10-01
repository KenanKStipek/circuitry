"""Parallel work reported while it runs (#252).

A ``flow: tree`` loop's iterations and a parallel dynamic's branches each
write into an isolated store. Those stores still report to the run's
observers — effect observers, runtime plugins, ``--live-state`` — at every
effect's full path, from the worker thread, while the siblings are still
running.

The adapter makes that checkable without timing guesses: the slow item blocks
until the test has observed the fast item, and records whether it ever did.
An observer that only hears about parallel work once the whole container
finishes would leave the slow item waiting out its timeout.
"""

from __future__ import annotations

import json
import threading
import time
from pathlib import Path
from typing import Any

import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run

#: How long the slow item waits to be released before giving up.
RELEASE_TIMEOUT = 5.0


class GatedAdapter:
    """Echoes the prompt; a prompt containing ``slow`` waits for ``release``."""

    name = "echo"

    def __init__(self) -> None:
        self.release = threading.Event()
        self.released_in_time: bool | None = None

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        if "slow" in prompt:
            self.released_in_time = self.release.wait(RELEASE_TIMEOUT)
        return GenerateResult(text=prompt, raw={}, tokens_sent=1, tokens_received=1)


def _run(
    tmp_path: Path, orch: dict[str, Any], adapter: GatedAdapter, **kwargs: Any
) -> Any:
    path = tmp_path / "orch.yml"
    path.write_text(yaml.dump(orch, sort_keys=False), encoding="utf-8")
    return run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={"input": {"items": ["fast", "slow", "fast too"]}},
            adapter=adapter,
            config=CircuitryConfig(),
            skip_preflight=True,
            **kwargs,
        )
    )


def _tree_loop(name: str = "lp") -> dict[str, Any]:
    return {
        "type": "loop",
        "name": name,
        "flow": "tree",
        "each": {"in": "input.items", "as": "it"},
        "body": [{"type": "prompt", "name": "step", "template": "{{it}}"}],
    }


TREE_LOOP: dict[str, Any] = {
    "adapter": "echo",
    "model": "echo-1",
    "effects": [_tree_loop()],
}


class Recorder:
    """Every start/complete, in arrival order, and on which thread."""

    def __init__(self, adapter: GatedAdapter, release_on: str) -> None:
        self.events: list[tuple[str, str]] = []
        self.threads: dict[str, str] = {}
        self._adapter = adapter
        self._release_on = release_on
        self._lock = threading.Lock()

    def start(self, path: str, node: dict[str, Any]) -> None:
        with self._lock:
            self.events.append(("start", path))

    def complete(self, path: str, node: dict[str, Any]) -> None:
        with self._lock:
            self.events.append(("complete", path))
            self.threads[path] = threading.current_thread().name
        if path == self._release_on:
            self._adapter.release.set()


def test_a_tree_loop_reports_each_iteration_while_the_loop_runs(
    tmp_path: Path,
) -> None:
    adapter = GatedAdapter()
    seen = Recorder(adapter, release_on="prime.lp.iter_0.step")
    result = _run(
        tmp_path,
        TREE_LOOP,
        adapter,
        effect_start_observer=seen.start,
        effect_observer=seen.complete,
    )

    assert result.ok, result.error
    # The slow iteration was released by the observer hearing iteration 0
    # land — which can only happen while the loop is still running.
    assert adapter.released_in_time is True

    steps = [f"prime.lp.iter_{i}.step" for i in range(3)]
    for step in steps:
        assert seen.events.count(("start", step)) == 1
        assert seen.events.count(("complete", step)) == 1
        assert seen.events.index(("start", step)) < seen.events.index(
            ("complete", step)
        )
        # The loop's own pair brackets every iteration's.
        assert seen.events.index(("start", "prime.lp")) < seen.events.index(
            ("start", step)
        )
        assert seen.events.index(("complete", step)) < seen.events.index(
            ("complete", "prime.lp")
        )
        # Reported from the worker that ran it, not replayed at the end.
        assert seen.threads[step] != "MainThread"
    assert seen.events.index(("complete", "prime.lp.iter_0.step")) < (
        seen.events.index(("complete", "prime.lp.iter_1.step"))
    )


def test_a_tree_loop_keeps_its_isolated_results_in_index_order(
    tmp_path: Path,
) -> None:
    """Observation changes nothing about what the loop writes."""
    adapter = GatedAdapter()
    seen = Recorder(adapter, release_on="prime.lp.iter_0.step")
    result = _run(
        tmp_path,
        {**TREE_LOOP, "effects": [{**_tree_loop(), "collect": "step"}]},
        adapter,
        effect_observer=seen.complete,
    )

    assert result.ok, result.error
    loop = result.state["prime"]["lp"]
    assert loop["collected"]["value"] == ["fast", "slow", "fast too"]
    assert [loop[f"iter_{i}"]["step"]["value"] for i in range(3)] == [
        "fast",
        "slow",
        "fast too",
    ]
    assert loop["value"]["iterations"] == 3


def test_a_parallel_dynamic_reports_each_branch_while_it_runs(
    tmp_path: Path,
) -> None:
    adapter = GatedAdapter()
    seen = Recorder(adapter, release_on="prime.fan.quick")
    result = _run(
        tmp_path,
        {
            "adapter": "echo",
            "model": "echo-1",
            "effects": [
                {
                    "type": "dynamic",
                    "name": "fan",
                    "flow": "tree",
                    "effects": [
                        {"type": "prompt", "name": "quick", "template": "fast"},
                        {"type": "prompt", "name": "late", "template": "slow"},
                    ],
                }
            ],
        },
        adapter,
        effect_start_observer=seen.start,
        effect_observer=seen.complete,
    )

    assert result.ok, result.error
    assert adapter.released_in_time is True
    for branch in ("prime.fan.quick", "prime.fan.late"):
        assert seen.events.index(("start", "prime.fan")) < seen.events.index(
            ("start", branch)
        )
        assert seen.events.index(("start", branch)) < seen.events.index(
            ("complete", branch)
        )
        assert seen.events.index(("complete", branch)) < seen.events.index(
            ("complete", "prime.fan")
        )
        assert seen.threads[branch] != "MainThread"
    assert result.state["prime"]["fan"]["late"]["value"] == "slow"


def test_nested_parallel_work_reports_at_its_full_path(tmp_path: Path) -> None:
    """A parallel dynamic inside a tree loop's iteration nests both prefixes."""
    adapter = GatedAdapter()
    seen = Recorder(adapter, release_on="prime.lp.iter_0.fan.quick")
    result = _run(
        tmp_path,
        {
            "adapter": "echo",
            "model": "echo-1",
            "effects": [
                {
                    **_tree_loop(),
                    "body": [
                        {
                            "type": "dynamic",
                            "name": "fan",
                            "flow": "tree",
                            "effects": [
                                {"type": "prompt", "name": "quick", "template": "q"},
                                {
                                    "type": "prompt",
                                    "name": "echo",
                                    "template": "{{it}}",
                                },
                            ],
                        }
                    ],
                }
            ],
        },
        adapter,
        effect_observer=seen.complete,
    )

    assert result.ok, result.error
    assert adapter.released_in_time is True
    completed = {path for kind, path in seen.events if kind == "complete"}
    for i in range(3):
        assert {
            f"prime.lp.iter_{i}.fan",
            f"prime.lp.iter_{i}.fan.quick",
            f"prime.lp.iter_{i}.fan.echo",
        } <= completed


def test_live_state_shows_a_tree_loops_finished_iterations_while_it_runs(
    tmp_path: Path,
) -> None:
    """The slow iteration waits until the mirror file shows iteration 0."""
    live = tmp_path / "live.json"

    def _iteration_0_mirrored() -> bool:
        try:
            state = json.loads(live.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return False
        step = state.get("prime", {}).get("lp", {}).get("iter_0", {}).get("step")
        return isinstance(step, dict) and step.get("value") == "fast"

    class MirrorWatchingAdapter(GatedAdapter):
        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            if "slow" in prompt:
                for _ in range(int(RELEASE_TIMEOUT / 0.05)):
                    if _iteration_0_mirrored():
                        self.release.set()
                        break
                    time.sleep(0.05)
            return super().generate(
                model=model, prompt=prompt, timeout_seconds=timeout_seconds
            )

    adapter = MirrorWatchingAdapter()
    result = _run(tmp_path, TREE_LOOP, adapter, live_state_path=live)

    assert result.ok, result.error
    assert adapter.released_in_time is True
    # And the mirror still ends equal to the merged final state.
    final = json.loads(live.read_text(encoding="utf-8"))
    assert [final["prime"]["lp"][f"iter_{i}"]["step"]["value"] for i in range(3)] == [
        "fast",
        "slow",
        "fast too",
    ]


def test_a_state_observer_never_sees_a_branch_dict_still_being_written(
    tmp_path: Path,
) -> None:
    """Snapshots from parallel work carry copies of each branch, not the
    dicts the branch's thread is still mutating."""
    snapshots: list[dict[str, Any]] = []
    adapter = GatedAdapter()
    adapter.release.set()

    result = _run(
        tmp_path, TREE_LOOP, adapter, state_observer=snapshots.append
    )

    assert result.ok, result.error
    final_iterations = {
        id(result.state["prime"]["lp"][f"iter_{i}"]) for i in range(3)
    }
    mid_loop = [
        s["prime"]["lp"]
        for s in snapshots
        if "iter_0" in s.get("prime", {}).get("lp", {})
        and s["prime"]["lp"].get("value") is None
    ]
    assert mid_loop, "no snapshot was published while the loop ran"
    for lp in mid_loop:
        for key, value in lp.items():
            if key.startswith("iter_"):
                assert id(value) not in final_iterations
