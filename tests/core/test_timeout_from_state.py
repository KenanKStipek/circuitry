"""An effect's `timeout_ms` read from state: `{from: <path>, default: <int>}` (#564).

Tool and prompt effects accept the form `params` leaves and `use` inputs already
use. It is checked at compile time (a rooted path, a positive integer default)
and resolved at each attempt's dispatch against current state.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest
import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import _compile_effect, compile_orchestration
from circuitry.core.document_check import structural_errors
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.prompt import RetryPolicyDef
from circuitry.core.store import Store
from circuitry.core.timeout_ref import TimeoutFrom
from circuitry.core.tool import ToolDefinition, ToolRuntime
from circuitry.plugins import factory as plugin_factory
from circuitry.plugins.base import ToolResult


def _tool(name: str = "call", **fields: Any) -> dict[str, Any]:
    return {"type": "tool", "name": name, "provider": "shell", "params": {}, **fields}


def _prompt(name: str = "ask", **fields: Any) -> dict[str, Any]:
    return {"type": "prompt", "name": name, "template": "hi", **fields}


def _compiled(effect: dict[str, Any]) -> Any:
    return _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")


@dataclass
class _Timeouts:
    """A fake `shell` plugin recording every `timeout_seconds` it was dispatched with.

    Any other provider (the `json` source steps) builds the real plugin.
    """

    real_build_plugin: Any
    seen: list[int] = field(default_factory=list)
    fail_first: bool = False
    after_each: Any = None

    def build_plugin(self, **kw: Any) -> Any:
        if kw.get("plugin_name") != "shell":
            return self.real_build_plugin(**kw)
        plugin = MagicMock()

        def execute(*, params: dict[str, Any], timeout_seconds: int) -> ToolResult:
            self.seen.append(timeout_seconds)
            if self.after_each is not None:
                self.after_each()
            if self.fail_first and len(self.seen) == 1:
                return ToolResult(value=None, raw={}, ok=False, stderr="boom")
            return ToolResult(value="ok", raw={})

        plugin.execute.side_effect = execute
        return plugin


@pytest.fixture
def timeouts(monkeypatch: pytest.MonkeyPatch) -> _Timeouts:
    recorder = _Timeouts(real_build_plugin=plugin_factory.build_plugin)
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", recorder.build_plugin)
    return recorder


def _run_tool(defn: ToolDefinition, ctx: dict[str, Any]) -> dict[str, Any]:
    """The tool's node after one execution (its error, if any, is in `meta.error`)."""
    store = Store({})
    ToolRuntime(defn).execute(store=store, ctx=ctx)
    return store.state[defn.name]


def _run(orch: dict[str, Any], *, inputs: dict[str, Any] | None = None) -> dict[str, Any]:
    root = compile_orchestration(orch=orch)
    store = Store(state={"input": dict(inputs or {})})
    DynamicRuntime(
        root, adapter=MagicMock(), model="test", runtime_config={"_test_marker": True}
    ).execute(store=store)
    return store.state


# ── Compile and `cof check` ──────────────────────────────────────────────────


@pytest.mark.parametrize("make", [_tool, _prompt])
def test_compile_keeps_the_reference_and_its_default(make: Any) -> None:
    defn = _compiled(make(timeout_ms={"from": "  prime.spec.value.ms  ", "default": 3600000}))
    assert defn.timeout_ms == TimeoutFrom(path="prime.spec.value.ms", default=3600000)


@pytest.mark.parametrize("make", [_tool, _prompt])
def test_compile_a_reference_without_a_default(make: Any) -> None:
    assert _compiled(make(timeout_ms={"from": "input.ms"})).timeout_ms == TimeoutFrom(
        path="input.ms", default=None
    )


@pytest.mark.parametrize("make", [_tool, _prompt])
def test_compile_a_literal_is_unchanged(make: Any) -> None:
    assert _compiled(make(timeout_ms=2500)).timeout_ms == 2500
    assert _compiled(make()).timeout_ms is None


@pytest.mark.parametrize("make", [_tool, _prompt])
@pytest.mark.parametrize(
    ("timeout_ms", "message"),
    [
        ({"from": "ms"}, "not rooted at a state namespace or an enclosing loop binding"),
        ({"from": "state.input.ms"}, "Write 'input.ms' instead of 'state.input.ms'"),
        ({"from": ""}, "needs a non-empty path"),
        ({"from": 5}, "'from' must be a path string"),
        ({"default": 5}, "must be an integer, or {from: <path>"),
        ({"from": "input.ms", "other": 1}, "must be an integer, or {from: <path>"),
        ({"from": "input.ms", "default": "5000"}, "'default' must be a positive integer"),
        ({"from": "input.ms", "default": 1.5}, "'default' must be a positive integer"),
        ({"from": "input.ms", "default": 0}, "'default' must be a positive integer"),
        ({"from": "input.ms", "default": -3}, "'default' must be a positive integer"),
        ({"from": "input.ms", "default": True}, "'default' must be a positive integer"),
        ({"from": "input.ms", "default": None}, "'default' must be a positive integer"),
    ],
)
def test_compile_rejects_a_malformed_reference(
    make: Any, timeout_ms: dict[str, Any], message: str
) -> None:
    with pytest.raises(ValueError, match=message):
        compile_orchestration(orch={"effects": [make(timeout_ms=timeout_ms)]})


def test_compile_names_the_effect_and_where_it_is() -> None:
    with pytest.raises(ValueError, match=r"Tool effect 'call' timeout_ms at 'prime\.effects\[0\]'"):
        compile_orchestration(orch={"effects": [_tool(timeout_ms={"from": "ms"})]})


def _loop(body: dict[str, Any]) -> dict[str, Any]:
    return {
        "type": "loop",
        "name": "each_job",
        "each": {"in": "input.jobs", "as": "job"},
        "body": [body],
    }


@pytest.mark.parametrize("make", [_tool, _prompt])
def test_compile_accepts_a_loop_binding_inside_its_loop(make: Any) -> None:
    effect = make(timeout_ms={"from": "job.ms", "default": 1000})
    compile_orchestration(orch={"effects": [_loop(effect)]})


@pytest.mark.parametrize("make", [_tool, _prompt])
def test_compile_rejects_a_loop_binding_outside_its_loop(make: Any) -> None:
    after = make("after", timeout_ms={"from": "job.ms"})
    with pytest.raises(ValueError, match="loop bindings in scope: none here"):
        compile_orchestration(orch={"effects": [_loop(_tool("inner")), after]})


@pytest.mark.parametrize("make", [_tool, _prompt])
def test_schema_accepts_integers_and_references(make: Any) -> None:
    for timeout_ms in (0, 2500, {"from": "input.ms"}, {"from": "input.ms", "default": 1}):
        assert structural_errors({"effects": [make(timeout_ms=timeout_ms)]}) == []


@pytest.mark.parametrize("make", [_tool, _prompt])
@pytest.mark.parametrize(
    ("timeout_ms", "message"),
    [
        (-1, "timeout_ms: -1 is less than the minimum of 0"),
        ("5000", "timeout_ms: '5000' is not of type 'integer', 'object'"),
        ({}, "timeout_ms: 'from' is a required property"),
        ({"default": 5}, "timeout_ms: 'from' is a required property"),
        ({"from": ""}, "timeout_ms.from: '' should be non-empty"),
        ({"from": 7}, "timeout_ms.from: 7 is not of type 'string'"),
        ({"from": "input.ms", "default": "5"}, "timeout_ms.default: '5' is not of type 'integer'"),
        ({"from": "input.ms", "default": 0}, "timeout_ms.default: 0 is less than the minimum of 1"),
        ({"from": "input.ms", "extra": 1}, "Additional properties are not allowed"),
    ],
)
def test_schema_rejects_a_malformed_timeout(
    make: Any, timeout_ms: Any, message: str
) -> None:
    errors = structural_errors({"effects": [make(timeout_ms=timeout_ms)]})
    assert any(message in error for error in errors), errors


# ── Tool: resolved at dispatch ───────────────────────────────────────────────


def test_tool_reads_its_timeout_from_state(timeouts: _Timeouts) -> None:
    defn = ToolDefinition(
        name="t", provider="shell", params={}, timeout_ms=TimeoutFrom("prime.spec.value.ms", 9000)
    )
    ctx = {"prime": {"spec": {"value": {"ms": 4000}}}}
    node = _run_tool(defn, ctx)
    assert timeouts.seen == [4]
    assert node["meta"]["error"] is None


def test_tool_rounds_a_referenced_timeout_up_like_a_literal(timeouts: _Timeouts) -> None:
    defn = ToolDefinition(
        name="t", provider="shell", params={}, timeout_ms=TimeoutFrom("input.ms")
    )
    _run_tool(defn, {"input": {"ms": 1500}})
    _run_tool(defn, {"input": {"ms": 250}})
    assert timeouts.seen == [2, 1]


def test_tool_takes_the_default_when_the_path_is_missing(timeouts: _Timeouts) -> None:
    defn = ToolDefinition(
        name="t", provider="shell", params={}, timeout_ms=TimeoutFrom("input.ms", 7000)
    )
    _run_tool(defn, {"input": {}})
    assert timeouts.seen == [7]


def test_tool_takes_the_default_when_the_path_holds_null(timeouts: _Timeouts) -> None:
    defn = ToolDefinition(
        name="t", provider="shell", params={}, timeout_ms=TimeoutFrom("input.ms", 7000)
    )
    _run_tool(defn, {"input": {"ms": None}})
    assert timeouts.seen == [7]


def test_tool_uses_the_value_over_the_default_when_both_exist(timeouts: _Timeouts) -> None:
    defn = ToolDefinition(
        name="t", provider="shell", params={}, timeout_ms=TimeoutFrom("input.ms", 7000)
    )
    _run_tool(defn, {"input": {"ms": 2000}})
    assert timeouts.seen == [2]


def test_tool_fails_on_a_missing_path_without_a_default(timeouts: _Timeouts) -> None:
    defn = ToolDefinition(
        name="t",
        provider="shell",
        params={},
        timeout_ms=TimeoutFrom("prime.spec.value.ms"),
        on_error="continue",
    )
    node = _run_tool(defn, {})
    assert "Tool effect 't'" in node["meta"]["error"]
    assert "{from: prime.spec.value.ms}" in node["meta"]["error"]
    assert "did not resolve to a value and has no default" in node["meta"]["error"]
    assert timeouts.seen == []


@pytest.mark.parametrize("bad", ["5000", 2.5, 5000.0, True, 0, -5000, [1], {"ms": 1}])
def test_tool_fails_on_a_value_that_is_not_a_positive_integer(
    timeouts: _Timeouts, bad: Any
) -> None:
    defn = ToolDefinition(
        name="t",
        provider="shell",
        params={},
        timeout_ms=TimeoutFrom("input.ms", 7000),
        on_error="continue",
    )
    node = _run_tool(defn, {"input": {"ms": bad}})
    assert "{from: input.ms}" in node["meta"]["error"]
    assert f"resolved to {bad!r}; expected a positive integer" in node["meta"]["error"]
    assert node["value"] is None
    assert timeouts.seen == []


def test_tool_fails_the_run_with_on_error_fail(timeouts: _Timeouts) -> None:
    defn = ToolDefinition(
        name="t", provider="shell", params={}, timeout_ms=TimeoutFrom("input.ms")
    )
    with pytest.raises(ValueError, match=r"\{from: input\.ms\}"):
        ToolRuntime(defn).execute(store=Store({}), ctx={})
    assert timeouts.seen == []


def test_tool_does_not_retry_a_timeout_that_cannot_resolve(timeouts: _Timeouts) -> None:
    defn = ToolDefinition(
        name="t",
        provider="shell",
        params={},
        timeout_ms=TimeoutFrom("input.ms"),
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=1),
        on_error="continue",
    )
    node = _run_tool(defn, {})
    assert "did not resolve" in node["meta"]["error"]
    assert "retries_used" not in node["meta"]


def test_tool_resolves_again_on_each_attempt(timeouts: _Timeouts) -> None:
    ctx: dict[str, Any] = {"input": {"ms": 2000}}
    timeouts.fail_first = True
    # State moves on once the first attempt has been dispatched.
    timeouts.after_each = lambda: ctx["input"].update(ms=9000)
    defn = ToolDefinition(
        name="t",
        provider="shell",
        params={},
        timeout_ms=TimeoutFrom("input.ms"),
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=1),
    )
    node = _run_tool(defn, ctx)
    assert timeouts.seen == [2, 9]
    assert node["meta"]["retries_used"] == 1


def test_dry_run_does_not_resolve_the_timeout(timeouts: _Timeouts) -> None:
    defn = ToolDefinition(
        name="t", provider="shell", params={}, timeout_ms=TimeoutFrom("input.ms")
    )
    store = Store({})
    ToolRuntime(defn, dry_run=True).execute(store=store, ctx={})
    assert store.state["t"]["meta"]["dry_run"] is True
    assert timeouts.seen == []


# ── Tool: through a run ──────────────────────────────────────────────────────


def test_a_run_resolves_the_timeout_from_an_earlier_effects_output(
    timeouts: _Timeouts,
) -> None:
    state = _run(
        {
            "effects": [
                {
                    "type": "tool",
                    "name": "spec",
                    "provider": "json",
                    "params": {"mode": "parse", "input": '{"timeout_ms": 12000}'},
                },
                _tool("work", timeout_ms={"from": "prime.spec.value.timeout_ms", "default": 1000}),
            ]
        }
    )
    assert state["prime"]["work"]["meta"]["error"] is None
    assert timeouts.seen == [12]


def test_a_loop_body_resolves_its_timeout_per_iteration(timeouts: _Timeouts) -> None:
    _run(
        {
            "effects": [
                {
                    "type": "loop",
                    "name": "jobs",
                    "each": {"in": "input.jobs", "as": "job"},
                    "body": [_tool("work", timeout_ms={"from": "job.ms", "default": 5000})],
                }
            ]
        },
        inputs={"jobs": [{"ms": 1000}, {"ms": 3000}, {}]},
    )
    assert timeouts.seen == [1, 3, 5]


def test_a_use_child_reads_its_timeout_from_its_inputs(
    timeouts: _Timeouts, tmp_path: Path
) -> None:
    """`use` has no `timeout_ms` of its own: a child's effect reads its own from `input.<name>`."""
    child = tmp_path / "child.yml"
    child.write_text(
        yaml.dump(
            {
                "interface": {"inputs": {"budget_ms": {"type": "integer", "required": True}}},
                "effects": [_tool("work", timeout_ms={"from": "input.budget_ms"})],
            }
        ),
        encoding="utf-8",
    )
    use = {"type": "use", "name": "sub", "path": str(child)}
    state = _run(
        {
            "effects": [
                {**use, "name": "short", "inputs": {"budget_ms": 2000}},
                {**use, "name": "long", "inputs": {"budget_ms": {"from": "input.long_ms"}}},
            ]
        },
        inputs={"long_ms": 45000},
    )
    assert timeouts.seen == [2, 45]
    assert state["prime"]["short"]["meta"].get("error") is None
    assert state["prime"]["long"]["meta"].get("error") is None


# ── Prompt ───────────────────────────────────────────────────────────────────


@dataclass
class _RecordingAdapter:
    name: str = "recorder"
    calls: list[int] = field(default_factory=list)
    fail_first: bool = False

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        self.calls.append(timeout_seconds)
        if self.fail_first and len(self.calls) == 1:
            raise TimeoutError("timed out")
        return GenerateResult(text="ok", raw={})


def _run_prompt(
    effect: dict[str, Any],
    *,
    adapter: _RecordingAdapter,
    inputs: dict[str, Any] | None = None,
    adapter_timeout: int = 120,
) -> dict[str, Any]:
    root = compile_orchestration(orch={"effects": [effect]})
    store = Store(state={"input": dict(inputs or {})})
    DynamicRuntime(
        root,
        adapter=adapter,
        model="m",
        runtime_config={"_test_marker": True},
        timeout_seconds=adapter_timeout,
    ).execute(store=store)
    return store.state["prime"]["ask"]


def test_prompt_reads_its_timeout_from_state() -> None:
    adapter = _RecordingAdapter()
    _run_prompt(
        _prompt(timeout_ms={"from": "input.ms", "default": 9000}),
        adapter=adapter,
        inputs={"ms": 2500},
    )
    assert adapter.calls == [3]  # ceil(2500 / 1000), as for a literal


def test_prompt_takes_the_default_when_the_path_is_missing() -> None:
    adapter = _RecordingAdapter()
    _run_prompt(_prompt(timeout_ms={"from": "input.ms", "default": 7000}), adapter=adapter)
    assert adapter.calls == [7]


def test_prompt_timeout_from_state_is_still_capped_by_the_adapter() -> None:
    adapter = _RecordingAdapter()
    _run_prompt(
        _prompt(timeout_ms={"from": "input.ms"}),
        adapter=adapter,
        inputs={"ms": 600000},
        adapter_timeout=60,
    )
    assert adapter.calls == [60]


def test_prompt_fails_on_a_missing_path_without_a_default() -> None:
    adapter = _RecordingAdapter()
    node = _run_prompt(
        _prompt(timeout_ms={"from": "input.ms"}, on_error="continue"), adapter=adapter
    )
    assert "Prompt effect 'ask'" in node["meta"]["error"]
    assert "{from: input.ms}" in node["meta"]["error"]
    assert "has no default" in node["meta"]["error"]
    assert node["value"] is None
    assert adapter.calls == []


@pytest.mark.parametrize("bad", ["5000", 2.5, True, 0, -1])
def test_prompt_fails_on_a_value_that_is_not_a_positive_integer(bad: Any) -> None:
    adapter = _RecordingAdapter()
    node = _run_prompt(
        _prompt(timeout_ms={"from": "input.ms", "default": 7000}, on_error="continue"),
        adapter=adapter,
        inputs={"ms": bad},
    )
    assert "{from: input.ms}" in node["meta"]["error"]
    assert f"resolved to {bad!r}; expected a positive integer" in node["meta"]["error"]
    assert adapter.calls == []


def test_prompt_fails_the_run_with_on_error_fail() -> None:
    with pytest.raises(RuntimeError, match=r"prime\.ask: Prompt effect 'ask'.*\{from: input\.ms\}"):
        _run_prompt(_prompt(timeout_ms={"from": "input.ms"}), adapter=_RecordingAdapter())


def test_prompt_resolves_again_on_each_attempt() -> None:
    @dataclass
    class MovingState(_RecordingAdapter):
        state: dict[str, Any] = field(default_factory=dict)

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            try:
                return super().generate(
                    model=model, prompt=prompt, timeout_seconds=timeout_seconds
                )
            finally:
                self.state["input"]["ms"] = 8000  # state moves on after the first attempt

    adapter = MovingState(fail_first=True)
    root = compile_orchestration(
        orch={
            "effects": [
                _prompt(
                    timeout_ms={"from": "input.ms"},
                    retries={"max_attempts": 2, "backoff_ms": 1},
                )
            ]
        }
    )
    store = Store(state={"input": {"ms": 2000}})
    adapter.state = store.state
    DynamicRuntime(
        root, adapter=adapter, model="m", runtime_config={"_test_marker": True}
    ).execute(store=store)
    assert adapter.calls == [2, 8]


def test_prompt_dry_run_does_not_resolve_the_timeout() -> None:
    adapter = _RecordingAdapter()
    root = compile_orchestration(orch={"effects": [_prompt(timeout_ms={"from": "input.ms"})]})
    store = Store(state={"input": {}})
    DynamicRuntime(
        root, adapter=adapter, model="m", runtime_config={"_test_marker": True}, dry_run=True
    ).execute(store=store)
    assert store.state["prime"]["ask"]["meta"]["dry_run"] is True
    assert adapter.calls == []
