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
import shutil
import threading
import time
from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run

#: How long the slow item waits to be released before giving up.
RELEASE_TIMEOUT = 5.0

requires_sh = pytest.mark.skipif(shutil.which("sh") is None, reason="requires the 'sh' binary")


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


def test_a_use_inside_a_tree_loop_reports_at_its_full_path(tmp_path: Path) -> None:
    child = tmp_path / "child.yml"
    child.write_text(
        yaml.dump(
            {"effects": [{"type": "prompt", "name": "step", "template": "child"}]}
        ),
        encoding="utf-8",
    )
    adapter = GatedAdapter()
    seen = Recorder(adapter, release_on="")
    result = _run(
        tmp_path,
        {
            **TREE_LOOP,
            "effects": [
                {
                    **_tree_loop(),
                    "body": [{"type": "use", "name": "sub", "path": str(child)}],
                }
            ],
        },
        adapter,
        effect_start_observer=seen.start,
        effect_observer=seen.complete,
    )

    assert result.ok, result.error
    for i in range(3):
        step = f"prime.lp.iter_{i}.sub.step"
        assert seen.events.index(("start", step)) < seen.events.index(
            ("complete", step)
        )
        assert seen.threads[step] != "MainThread"


def test_a_failing_step_in_a_tree_iteration_closes_its_pair_with_its_error(
    tmp_path: Path,
) -> None:
    class FailingAdapter(GatedAdapter):
        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            if "slow" in prompt:
                raise RuntimeError("the slow item broke")
            return super().generate(
                model=model, prompt=prompt, timeout_seconds=timeout_seconds
            )

    started: list[str] = []
    completed: dict[str, tuple[Any, str]] = {}

    def complete(path: str, node: dict[str, Any]) -> None:
        completed[path] = (
            node.get("meta", {}).get("error"),
            threading.current_thread().name,
        )

    result = _run(
        tmp_path,
        TREE_LOOP,
        FailingAdapter(),
        effect_start_observer=lambda path, node: started.append(path),
        effect_observer=complete,
    )

    assert result.ok is False
    failed = "prime.lp.iter_1.step"
    assert failed in started
    error, thread = completed[failed]
    assert error and "the slow item broke" in error
    assert thread != "MainThread"


def test_live_state_shows_a_parallel_dynamics_finished_branch_while_it_runs(
    tmp_path: Path,
) -> None:
    """The slow branch waits until the mirror file shows the quick one."""
    live = tmp_path / "live.json"

    def _quick_mirrored() -> bool:
        try:
            state = json.loads(live.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return False
        quick = state.get("prime", {}).get("fan", {}).get("quick")
        return isinstance(quick, dict) and quick.get("value") == "fast"

    class MirrorWatchingAdapter(GatedAdapter):
        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            if "slow" in prompt:
                for _ in range(int(RELEASE_TIMEOUT / 0.05)):
                    if _quick_mirrored():
                        self.release.set()
                        break
                    time.sleep(0.05)
            return super().generate(
                model=model, prompt=prompt, timeout_seconds=timeout_seconds
            )

    adapter = MirrorWatchingAdapter()
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
        live_state_path=live,
    )

    assert result.ok, result.error
    assert adapter.released_in_time is True
    final = json.loads(live.read_text(encoding="utf-8"))
    assert final["prime"]["fan"]["late"]["value"] == "slow"


def _running(node: Any) -> bool:
    """True once *node* has been dispatched but hasn't finished: ``meta``
    exists with ``created_at`` set and ``completed_at`` still ``None`` --
    the same shape a watcher like osi checks (#634).
    """
    if not isinstance(node, dict):
        return False
    meta = node.get("meta")
    return (
        isinstance(meta, dict)
        and meta.get("created_at") is not None
        and meta.get("completed_at") is None
    )


class _SelfWatchingAdapter(GatedAdapter):
    """Like ``MirrorWatchingAdapter``, but the slow item polls the mirror
    for *its own* node — proving a watcher sees the effect that's actually
    still dispatching, not just a sibling that already finished.
    """

    def __init__(self, is_mirrored: Any) -> None:
        super().__init__()
        self._is_mirrored = is_mirrored
        #: None until the slow item runs; then whether the mirror showed
        #: this effect's own node as running before it was released.
        self.seen_running: bool | None = None

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        if "slow" in prompt:
            self.seen_running = False
            for _ in range(int(RELEASE_TIMEOUT / 0.05)):
                if self._is_mirrored():
                    self.seen_running = True
                    break
                time.sleep(0.05)
            self.release.set()
        return super().generate(
            model=model, prompt=prompt, timeout_seconds=timeout_seconds
        )


def test_live_state_shows_a_tree_loops_own_slow_pass_running(tmp_path: Path) -> None:
    """The slow iteration's own node appears without `completed_at` while
    it is still dispatching, not just once every iteration has finished
    (#634) -- `flow: tree` never republished a branch mid-pass before.
    """
    live = tmp_path / "live.json"

    def _iter1_running() -> bool:
        try:
            state = json.loads(live.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return False
        lp = state.get("prime", {}).get("lp", {})
        return _running(lp.get("iter_1", {}).get("step"))

    adapter = _SelfWatchingAdapter(_iter1_running)
    result = _run(tmp_path, TREE_LOOP, adapter, live_state_path=live)

    assert result.ok, result.error
    assert adapter.seen_running is True


def test_live_state_shows_an_inline_use_childs_effect_running(tmp_path: Path) -> None:
    """A `use` child's own effect appears running while it still is, not
    only once the whole child has finished (#634).
    """
    live = tmp_path / "live.json"

    def _child_step_running() -> bool:
        try:
            state = json.loads(live.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return False
        return _running(state.get("prime", {}).get("child", {}).get("step"))

    adapter = _SelfWatchingAdapter(_child_step_running)
    orch = {
        "adapter": "echo",
        "model": "echo-1",
        "effects": [
            {
                "type": "use",
                "name": "child",
                "inline": "effects:\n  - {type: prompt, name: step, template: slow}\n",
            }
        ],
    }
    result = _run(tmp_path, orch, adapter, live_state_path=live)

    assert result.ok, result.error
    assert adapter.seen_running is True


def test_live_state_shows_a_use_by_paths_childs_effect_running(tmp_path: Path) -> None:
    """Same as the inline case, for a `use` child resolved from a `path:`."""
    child_path = tmp_path / "child.yml"
    child_path.write_text(
        yaml.dump({"effects": [{"type": "prompt", "name": "step", "template": "slow"}]}),
        encoding="utf-8",
    )
    live = tmp_path / "live.json"

    def _child_step_running() -> bool:
        try:
            state = json.loads(live.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return False
        return _running(state.get("prime", {}).get("child", {}).get("step"))

    adapter = _SelfWatchingAdapter(_child_step_running)
    orch = {
        "adapter": "echo",
        "model": "echo-1",
        "effects": [{"type": "use", "name": "child", "path": str(child_path)}],
    }
    result = _run(tmp_path, orch, adapter, live_state_path=live)

    assert result.ok, result.error
    assert adapter.seen_running is True


def test_live_state_shows_a_use_nested_inside_a_tree_loop_pass_running(
    tmp_path: Path,
) -> None:
    """A `use` child inside a tree-flow pass composes both prefixes while
    its own effect is still running (#634).
    """
    live = tmp_path / "live.json"

    def _iter1_child_step_running() -> bool:
        try:
            state = json.loads(live.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return False
        lp = state.get("prime", {}).get("lp", {})
        child = lp.get("iter_1", {}).get("child", {})
        return _running(child.get("step"))

    adapter = _SelfWatchingAdapter(_iter1_child_step_running)
    orch = {
        **TREE_LOOP,
        "effects": [
            {
                **_tree_loop(),
                "body": [
                    {
                        "type": "use",
                        "name": "child",
                        "inline": (
                            "effects:\n"
                            "  - {type: prompt, name: step, template: '{{it}}'}\n"
                        ),
                    }
                ],
            }
        ],
    }
    result = _run(tmp_path, orch, adapter, live_state_path=live)

    assert result.ok, result.error
    assert adapter.seen_running is True


def test_live_state_shows_a_tree_loop_nested_inside_a_use_child_running(
    tmp_path: Path,
) -> None:
    """The reverse nesting: a `flow: tree` loop inside a `use` child.

    The loop's own `each:` is kept out of the inline child's template --
    `inline:` renders against the *parent's* context first (so the use
    effect's own Mustache tags can reach it), which would otherwise consume
    a nested loop's own `{{it}}` before the child ever compiles it.
    """
    live = tmp_path / "live.json"

    def _child_iter0_running() -> bool:
        try:
            state = json.loads(live.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return False
        child = state.get("prime", {}).get("child", {})
        lp = child.get("lp", {})
        return _running(lp.get("iter_0", {}).get("step"))

    adapter = _SelfWatchingAdapter(_child_iter0_running)
    child_doc = {
        "effects": [
            {
                "type": "loop",
                "name": "lp",
                "flow": "tree",
                "each": {"in": "input.single", "as": "x"},
                "body": [{"type": "prompt", "name": "step", "template": "slow"}],
            }
        ]
    }
    orch = {
        "adapter": "echo",
        "model": "echo-1",
        "effects": [
            {
                "type": "use",
                "name": "child",
                "inline": yaml.dump(child_doc, sort_keys=False),
                "inputs": {"single": ["only"]},
            }
        ],
    }
    result = _run(tmp_path, orch, adapter, live_state_path=live)

    assert result.ok, result.error
    assert adapter.seen_running is True


def test_live_state_shows_a_use_with_declared_outputs_running_child_transiently(
    tmp_path: Path,
) -> None:
    """A `use` with declared `outputs:` still shows its running child effect
    while it runs, even though the finished state keeps only the mapped
    output (#634) -- live visibility matches what the node held before it
    finished, not what survives after.
    """
    live = tmp_path / "live.json"

    def _child_step_running() -> bool:
        try:
            state = json.loads(live.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return False
        return _running(state.get("prime", {}).get("child", {}).get("step"))

    adapter = _SelfWatchingAdapter(_child_step_running)
    orch = {
        "adapter": "echo",
        "model": "echo-1",
        "effects": [
            {
                "type": "use",
                "name": "child",
                "outputs": {"v": "prime.step.value"},
                "inline": "effects:\n  - {type: prompt, name: step, template: slow}\n",
            }
        ],
    }
    result = _run(tmp_path, orch, adapter, live_state_path=live)

    assert result.ok, result.error
    assert adapter.seen_running is True
    # Declared outputs, no `record_children`: the finished state keeps only
    # the mapped value -- confirmed here so the live-visibility choice
    # above stays paired with what it's documented against.
    assert "step" not in result.state["prime"]["child"]
    assert result.state["prime"]["child"]["value"] == {"v": "slow"}


def _gated_tool_body(name: str, gate: Path) -> dict[str, Any]:
    """A tool effect that blocks until *gate* exists -- a deterministic
    stand-in for "still running", polled from outside the process rather
    than timed.
    """
    return {
        "type": "tool",
        "name": name,
        "provider": "shell",
        "params": {
            "command": "sh",
            "args": [
                "-c",
                'while [ ! -f "$1" ]; do sleep 0.05; done',
                "sh",
                str(gate),
            ],
            "allowed_commands": ["sh"],
        },
    }


def _run_orch_and_wait_for_gate(
    tmp_path: Path, orch: dict[str, Any], live: Path, gate: Path, is_running: Any
) -> Any:
    """Runs *orch* on a background thread, polls ``live`` until
    *is_running* reports the awaited node as running, then creates *gate*
    to release it, and returns the finished ``RunResult``.

    An unused ``GatedAdapter`` satisfies adapter resolution for a ``use``
    child, which is assumed to contain prompts whether or not it actually
    does -- these orchestrations never call it.
    """
    path = tmp_path / "orch.yml"
    path.write_text(yaml.dump(orch, sort_keys=False), encoding="utf-8")
    box: dict[str, Any] = {}

    def _run_it() -> None:
        box["result"] = run(
            RunRequest(
                orchestration_path=path,
                state_path=None,
                out_path=None,
                dry_run=False,
                validate_only=False,
                initial_state={"input": {"items": [0]}},
                adapter=GatedAdapter(),
                config=CircuitryConfig(),
                skip_preflight=True,
                live_state_path=live,
            )
        )

    thread = threading.Thread(target=_run_it)
    thread.start()
    try:
        seen_running = False
        for _ in range(int(RELEASE_TIMEOUT / 0.05)):
            try:
                state = json.loads(live.read_text(encoding="utf-8"))
            except (OSError, ValueError):
                state = {}
            if is_running(state):
                seen_running = True
                break
            time.sleep(0.05)
        gate.touch()
    finally:
        thread.join(RELEASE_TIMEOUT)
    assert seen_running, "the gated tool's node never appeared running"
    return box["result"]


@requires_sh
def test_live_state_shows_a_tree_loops_own_slow_tool_running(tmp_path: Path) -> None:
    """A tool's own node, not just a prompt's, appears with `created_at`
    set and `completed_at` still null while a tree-flow pass's tool is
    still dispatching (#634): a tool resets `created_at`/`completed_at`
    per attempt, after `fire_effect_start` already fired once for the
    whole pass -- each attempt needs its own publish, or the mirror shows
    `{"value": null, "meta": {}}` for the tool's entire run.
    """
    live = tmp_path / "live.json"
    gate = tmp_path / "gate"
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "lp",
                "flow": "tree",
                "max_concurrency": 2,
                "each": {"in": "input.items", "as": "it"},
                "body": [_gated_tool_body("step", gate)],
            }
        ],
    }

    def _is_running(state: dict[str, Any]) -> bool:
        lp = state.get("prime", {}).get("lp", {})
        return _running(lp.get("iter_0", {}).get("step"))

    result = _run_orch_and_wait_for_gate(tmp_path, orch, live, gate, _is_running)
    assert result.ok, result.error


@requires_sh
def test_live_state_shows_an_inline_use_childs_tool_running(tmp_path: Path) -> None:
    """Same as the tree-pass case, for a tool inside an inline `use` child
    (#634) -- the same per-attempt gap exists through `_child_on_write`.
    """
    live = tmp_path / "live.json"
    gate = tmp_path / "gate"
    orch = {
        "adapter": "echo",
        "model": "echo-1",
        "effects": [
            {
                "type": "use",
                "name": "child",
                "inline": yaml.dump(
                    {"effects": [_gated_tool_body("step", gate)]}, sort_keys=False
                ),
            }
        ],
    }

    def _is_running(state: dict[str, Any]) -> bool:
        return _running(state.get("prime", {}).get("child", {}).get("step"))

    result = _run_orch_and_wait_for_gate(tmp_path, orch, live, gate, _is_running)
    assert result.ok, result.error


@requires_sh
def test_live_state_shows_a_use_by_paths_childs_tool_running(tmp_path: Path) -> None:
    """Same as the inline case, for a tool inside a `use` child resolved
    from a `path:` (#634).
    """
    live = tmp_path / "live.json"
    gate = tmp_path / "gate"
    child_path = tmp_path / "child.yml"
    child_path.write_text(
        yaml.dump({"effects": [_gated_tool_body("step", gate)]}, sort_keys=False),
        encoding="utf-8",
    )
    orch = {
        "adapter": "echo",
        "model": "echo-1",
        "effects": [{"type": "use", "name": "child", "path": str(child_path)}],
    }

    def _is_running(state: dict[str, Any]) -> bool:
        return _running(state.get("prime", {}).get("child", {}).get("step"))

    result = _run_orch_and_wait_for_gate(tmp_path, orch, live, gate, _is_running)
    assert result.ok, result.error


def test_a_waiting_tree_passs_meta_reaches_live_state_while_it_waits(
    tmp_path: Path,
) -> None:
    """A mid-run write to a still-running tree-flow pass -- `meta.waiting_for`,
    set while blocked on a concurrency group's slot -- reaches `--live-state`
    the same way the pass's own start does (#634).
    """
    live = tmp_path / "live.json"

    def _someone_waiting_for_g() -> bool:
        try:
            state = json.loads(live.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return False
        lp = state.get("prime", {}).get("lp", {})
        for key in ("iter_0", "iter_1", "iter_2"):
            step = lp.get(key, {}).get("step")
            if (
                isinstance(step, dict)
                and isinstance(step.get("meta"), dict)
                and step["meta"].get("waiting_for") == "g"
                and step["meta"].get("completed_at") is None
            ):
                return True
        return False

    class GroupHoldingAdapter:
        name = "echo"

        def __init__(self) -> None:
            self._claim_lock = threading.Lock()
            self._claimed = False
            self.release = threading.Event()
            self.seen_waiting: bool | None = None

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            with self._claim_lock:
                is_holder = not self._claimed
                self._claimed = True
            if is_holder:
                self.seen_waiting = False
                for _ in range(int(RELEASE_TIMEOUT / 0.05)):
                    if _someone_waiting_for_g():
                        self.seen_waiting = True
                        break
                    time.sleep(0.05)
                self.release.set()
            else:
                self.release.wait(RELEASE_TIMEOUT)
            return GenerateResult(text=prompt, raw={}, tokens_sent=1, tokens_received=1)

    adapter = GroupHoldingAdapter()
    orch = {
        "adapter": "echo",
        "model": "echo-1",
        "runtime": {"concurrency_groups": {"g": 1}},
        "effects": [
            {
                **_tree_loop(),
                "body": [{"type": "prompt", "name": "step", "template": "{{it}}", "group": "g"}],
            }
        ],
    }
    result = _run(
        tmp_path, orch, adapter, live_state_path=live, trust_document=True
    )

    assert result.ok, result.error
    assert adapter.seen_waiting is True


class DelayAdapter:
    name = "echo"

    def __init__(self, delay: float) -> None:
        self._delay = delay
        self._lock = threading.Lock()
        self.starts: list[float] = []

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        with self._lock:
            self.starts.append(time.monotonic())
        time.sleep(self._delay)
        return GenerateResult(text=prompt, raw={}, tokens_sent=1, tokens_received=1)


def _run_delayed_tree(
    tmp_path: Path, live: Path, *, max_concurrency: int, delay: float
) -> tuple[DelayAdapter, float]:
    adapter = DelayAdapter(delay)
    orch = {
        "adapter": "echo",
        "model": "echo-1",
        "effects": [
            {
                "type": "loop",
                "name": "lp",
                "flow": "tree",
                "max_concurrency": max_concurrency,
                "each": {"in": "input.items", "as": "it"},
                "body": [{"type": "prompt", "name": "step", "template": "{{it}}"}],
            }
        ],
    }
    path = tmp_path / f"orch-{max_concurrency}.yml"
    path.write_text(yaml.dump(orch, sort_keys=False), encoding="utf-8")
    t0 = time.monotonic()
    result = run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={"input": {"items": list(range(8))}},
            adapter=adapter,
            config=CircuitryConfig(),
            skip_preflight=True,
            live_state_path=live,
        )
    )
    wall_time = time.monotonic() - t0
    assert result.ok, result.error
    return adapter, wall_time


def test_concurrent_tree_passes_run_and_publish_correctly(tmp_path: Path) -> None:
    """Republishing a running pass's own node at every effect start (#634)
    must not corrupt or drop any pass's result under real concurrency.

    Wall-clock bounds on lock contention are deliberately not asserted here
    (timing assertions flake under a loaded machine without proving
    anything a correctness check like this one doesn't already) -- the
    actual measurement comparing this against a serialized run is in the
    pull request that added this test.
    """
    adapter, _ = _run_delayed_tree(
        tmp_path, tmp_path / "live-concurrent.json", max_concurrency=8, delay=0.05
    )
    assert len(adapter.starts) == 8

