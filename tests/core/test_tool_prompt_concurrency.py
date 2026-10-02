"""`ToolRuntime`/`PromptRuntime` hold a `RunConcurrencyLimiter` slot for the
duration of their actual dispatch, acquired before it and released after
(#274). The limiter rides `runtime_config["_concurrency_limiter"]` the same
way every other run-wide setting does; a `runtime_config` with no such key
dispatches exactly as it did before this feature existed.
"""

from __future__ import annotations

import threading
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.concurrency import (
    RUNTIME_CONFIG_KEY,
    RunConcurrencyLimiter,
    UnknownConcurrencyGroupError,
)
from circuitry.core.prompt import PromptDefinition, PromptRuntime
from circuitry.core.store import Store
from circuitry.core.tool import ToolDefinition, ToolRuntime
from circuitry.plugins.base import ToolResult


class _TrackingPlugin:
    """A fake tool plugin that sleeps and records peak in-flight calls."""

    def __init__(self, delay: float = 0.03) -> None:
        self._delay = delay
        self._lock = threading.Lock()
        self._in_flight = 0
        self.max_in_flight = 0

    def execute(self, *, params: dict, timeout_seconds: int) -> ToolResult:
        with self._lock:
            self._in_flight += 1
            self.max_in_flight = max(self.max_in_flight, self._in_flight)
        time.sleep(self._delay)
        with self._lock:
            self._in_flight -= 1
        return ToolResult(value="ok", raw={}, stdout="", stderr="", exit_code=0)


@dataclass
class _TrackingAdapter:
    delay: float = 0.03
    name: str = "tracking"
    max_concurrent: int = field(default=0)
    _lock: threading.Lock = field(default_factory=threading.Lock)
    _in_flight: int = field(default=0)

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        with self._lock:
            self._in_flight += 1
            self.max_concurrent = max(self.max_concurrent, self._in_flight)
        time.sleep(self.delay)
        with self._lock:
            self._in_flight -= 1
        return GenerateResult(text=prompt, raw={"model": model})


def test_tool_runtime_respects_global_cap(monkeypatch: pytest.MonkeyPatch) -> None:
    plugin = _TrackingPlugin()
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: plugin)

    limiter = RunConcurrencyLimiter(max_concurrency=1)
    runtime_config = {RUNTIME_CONFIG_KEY: limiter}
    store = Store({})

    def run_one(i: int) -> None:
        defn = ToolDefinition(name=f"t{i}", provider="fake", params={})
        ToolRuntime(defn, runtime_config=runtime_config).execute(store=store, ctx={})

    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(run_one, range(4)))

    assert plugin.max_in_flight == 1


def test_tool_runtime_without_a_limiter_is_unaffected(monkeypatch: pytest.MonkeyPatch) -> None:
    """No `_concurrency_limiter` in runtime_config: full, uncapped concurrency,
    exactly like before this feature existed."""
    plugin = _TrackingPlugin(delay=0.05)
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: plugin)
    store = Store({})

    def run_one(i: int) -> None:
        defn = ToolDefinition(name=f"t{i}", provider="fake", params={})
        ToolRuntime(defn).execute(store=store, ctx={})

    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(run_one, range(4)))

    assert plugin.max_in_flight == 4


def test_tool_runtime_group_cap(monkeypatch: pytest.MonkeyPatch) -> None:
    plugin = _TrackingPlugin()
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: plugin)

    limiter = RunConcurrencyLimiter(groups={"gpu": 1})
    runtime_config = {RUNTIME_CONFIG_KEY: limiter}
    store = Store({})

    def run_one(i: int) -> None:
        defn = ToolDefinition(name=f"t{i}", provider="fake", params={}, group="gpu")
        ToolRuntime(defn, runtime_config=runtime_config).execute(store=store, ctx={})

    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(run_one, range(4)))

    assert plugin.max_in_flight == 1


def test_tool_runtime_waiting_for_meta_is_visible_while_blocked(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    plugin = _TrackingPlugin(delay=0.1)
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: plugin)

    limiter = RunConcurrencyLimiter(groups={"gpu": 1})
    runtime_config = {RUNTIME_CONFIG_KEY: limiter}
    store = Store({})
    seen_waiting: list[str | None] = []

    def watch() -> None:
        # Poll the second effect's node until it shows up waiting, or the
        # first effect finishes — whichever happens first.
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            node = store.state.get("second")
            if node is not None:
                waiting = node.get("meta", {}).get("waiting_for")
                if waiting is not None:
                    seen_waiting.append(waiting)
                    return
            time.sleep(0.005)

    def run(name: str) -> None:
        defn = ToolDefinition(name=name, provider="fake", params={}, group="gpu")
        ToolRuntime(defn, runtime_config=runtime_config).execute(store=store, ctx={})

    with ThreadPoolExecutor(max_workers=3) as pool:
        f_first = pool.submit(run, "first")
        f_watch = pool.submit(watch)
        time.sleep(0.02)
        f_second = pool.submit(run, "second")
        f_first.result()
        f_second.result()
        f_watch.result()

    assert seen_waiting == ["gpu"]
    # Cleared once acquired.
    assert store.state["second"]["meta"]["waiting_for"] is None


def test_tool_runtime_unknown_group_raises(monkeypatch: pytest.MonkeyPatch) -> None:
    plugin = _TrackingPlugin()
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: plugin)

    limiter = RunConcurrencyLimiter(groups={"gpu": 1})
    runtime_config = {RUNTIME_CONFIG_KEY: limiter}
    defn = ToolDefinition(name="t", provider="fake", params={}, group="comfy")
    store = Store({})

    with pytest.raises(UnknownConcurrencyGroupError):
        ToolRuntime(defn, runtime_config=runtime_config).execute(store=store, ctx={})


def test_prompt_runtime_respects_global_cap() -> None:
    adapter = _TrackingAdapter()
    limiter = RunConcurrencyLimiter(max_concurrency=1)
    runtime_config = {RUNTIME_CONFIG_KEY: limiter}
    store = Store({})

    def run_one(i: int) -> None:
        defn = PromptDefinition(name=f"p{i}", template="go")
        PromptRuntime(
            defn, adapter=adapter, model="unit-test", runtime_config=runtime_config
        ).execute(store=store, ctx={})

    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(run_one, range(4)))

    assert adapter.max_concurrent == 1


def test_prompt_runtime_group_cap() -> None:
    adapter = _TrackingAdapter()
    limiter = RunConcurrencyLimiter(groups={"gpu": 1})
    runtime_config = {RUNTIME_CONFIG_KEY: limiter}
    store = Store({})

    def run_one(i: int) -> None:
        defn = PromptDefinition(name=f"p{i}", template="go", group="gpu")
        PromptRuntime(
            defn, adapter=adapter, model="unit-test", runtime_config=runtime_config
        ).execute(store=store, ctx={})

    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(run_one, range(4)))

    assert adapter.max_concurrent == 1


def test_prompt_runtime_without_a_limiter_is_unaffected() -> None:
    adapter = _TrackingAdapter(delay=0.05)
    store = Store({})

    def run_one(i: int) -> None:
        defn = PromptDefinition(name=f"p{i}", template="go")
        PromptRuntime(defn, adapter=adapter, model="unit-test").execute(store=store, ctx={})

    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(run_one, range(4)))

    assert adapter.max_concurrent == 4
