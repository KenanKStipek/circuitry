"""Nested `flow: tree` loops plus `use` children under `runtime.max_concurrency:
1` and a group cap of 1 never deadlock (#274).

Only leaf effects (tool, prompt) ever hold a `RunConcurrencyLimiter` slot;
every container (loop, dynamic, use) just waits on its own children the way
it always has. A blocked leaf is always waiting on another leaf's own,
unconditional, eventual release — never on a container that is in turn
waiting on it — so there is no cycle to deadlock on. This is a timing test:
a correct implementation finishes quickly; a deadlocked one would hang
forever, so every run below is joined with a generous timeout and the test
fails loudly (rather than hanging the suite) if that timeout is hit.
"""

from __future__ import annotations

import threading
import time
from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest
import yaml

from circuitry.core.compiler import compile_orchestration
from circuitry.core.concurrency import RUNTIME_CONFIG_KEY, RunConcurrencyLimiter
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store
from circuitry.plugins.base import ToolResult

_JOIN_TIMEOUT = 20.0


class _TrackingPlugin:
    def __init__(self, delay: float = 0.02) -> None:
        self._delay = delay
        self._lock = threading.Lock()
        self._in_flight = 0
        self.max_in_flight = 0
        self.calls = 0

    def execute(self, *, params: dict, timeout_seconds: int) -> ToolResult:
        with self._lock:
            self._in_flight += 1
            self.max_in_flight = max(self.max_in_flight, self._in_flight)
            self.calls += 1
        time.sleep(self._delay)
        with self._lock:
            self._in_flight -= 1
        return ToolResult(value="ok", raw={}, stdout="", stderr="", exit_code=0)


def _write_child(tmp_path: Path, *, group: str | None) -> Path:
    tool: dict[str, Any] = {"type": "tool", "name": "work", "provider": "fake"}
    if group is not None:
        tool["group"] = group
    child = {
        "effects": [
            {
                "type": "loop",
                "name": "inner",
                "flow": "tree",
                "max_concurrency": 3,
                "each": {"in": "input.subitems", "as": "sub"},
                "body": [tool],
            }
        ]
    }
    path = tmp_path / "child.yml"
    path.write_text(yaml.dump(child), encoding="utf-8")
    return path


def _outer_orch(child_path: Path) -> dict[str, Any]:
    return {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "flow": "tree",
                "max_concurrency": 3,
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "use",
                        "name": "child",
                        "path": str(child_path),
                        "inputs": {"subitems": {"from": "item"}},
                    }
                ],
            }
        ]
    }


def _run_with_timeout(root: Any, *, runtime_config: dict[str, Any], state: dict[str, Any]) -> Store:
    store = Store(state)
    adapter = MagicMock()
    adapter.name = "noop"
    error: list[BaseException] = []

    def go() -> None:
        try:
            DynamicRuntime(
                root, adapter=adapter, model="unit-test", runtime_config=runtime_config
            ).execute(store=store)
        except BaseException as exc:
            error.append(exc)

    thread = threading.Thread(target=go, daemon=True)
    thread.start()
    thread.join(timeout=_JOIN_TIMEOUT)
    assert not thread.is_alive(), (
        "run did not finish within the timeout — nested tree loops + use "
        "children under a concurrency cap appear to have deadlocked"
    )
    if error:
        raise error[0]
    return store


def test_nested_tree_loops_with_use_children_global_cap_one(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    plugin = _TrackingPlugin()
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: plugin)

    child_path = _write_child(tmp_path, group=None)
    root = compile_orchestration(orch=_outer_orch(child_path))
    limiter = RunConcurrencyLimiter(max_concurrency=1)
    runtime_config = {RUNTIME_CONFIG_KEY: limiter}
    state = {"input": {"items": [[0, 1, 2], [3, 4, 5], [6, 7, 8]]}}

    _run_with_timeout(root, runtime_config=runtime_config, state=state)

    assert plugin.calls == 9
    assert plugin.max_in_flight == 1


def test_nested_tree_loops_with_use_children_group_cap_one(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Same shape, gated by a named group instead of the run-wide cap."""
    plugin = _TrackingPlugin()
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: plugin)

    child_path = _write_child(tmp_path, group="gpu")
    root = compile_orchestration(orch=_outer_orch(child_path))
    limiter = RunConcurrencyLimiter(groups={"gpu": 1})
    runtime_config = {RUNTIME_CONFIG_KEY: limiter}
    state = {"input": {"items": [[0, 1, 2], [3, 4, 5], [6, 7, 8]]}}

    _run_with_timeout(root, runtime_config=runtime_config, state=state)

    assert plugin.calls == 9
    assert plugin.max_in_flight == 1


def test_nested_tree_loops_with_use_children_global_and_group_cap_one(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Both caps active at once, each at 1 — the group-first acquisition
    order (#274 review P1) must not deadlock a leaf that needs both a
    group slot and a global one."""
    plugin = _TrackingPlugin()
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: plugin)

    child_path = _write_child(tmp_path, group="gpu")
    root = compile_orchestration(orch=_outer_orch(child_path))
    limiter = RunConcurrencyLimiter(max_concurrency=1, groups={"gpu": 1})
    runtime_config = {RUNTIME_CONFIG_KEY: limiter}
    state = {"input": {"items": [[0, 1, 2], [3, 4, 5], [6, 7, 8]]}}

    _run_with_timeout(root, runtime_config=runtime_config, state=state)

    assert plugin.calls == 9
    assert plugin.max_in_flight == 1


def test_nested_tree_loops_with_use_children_cap_two_allows_real_parallelism(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A cap above 1 still lets more than one dispatch run at once — the
    safety net above isn't just serializing everything by accident."""
    plugin = _TrackingPlugin(delay=0.05)
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: plugin)

    child_path = _write_child(tmp_path, group=None)
    root = compile_orchestration(orch=_outer_orch(child_path))
    limiter = RunConcurrencyLimiter(max_concurrency=2)
    runtime_config = {RUNTIME_CONFIG_KEY: limiter}
    state = {"input": {"items": [[0, 1, 2], [3, 4, 5], [6, 7, 8]]}}

    _run_with_timeout(root, runtime_config=runtime_config, state=state)

    assert plugin.calls == 9
    assert plugin.max_in_flight == 2
