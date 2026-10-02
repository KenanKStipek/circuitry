"""Tool params by reference: `{from: <path>}` leaves anywhere in `params` (#234).

Mirrors `use` inputs' own by-reference form (`tests/core/test_use_reference_and_record.py`):
same scope overlay (state namespaces, enclosing loop bindings), same
compile-time rooting check, same deep-copy-on-resolve contract. The
`default:` leaf is new here — `use` inputs have no equivalent, since a
`use` input's own fallback is the child's declared `interface.inputs`
default instead.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest

from circuitry.core.compiler import _compile_effect, compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store
from circuitry.core.tool import (
    ParamReference,
    ToolDefinition,
    ToolRuntime,
    param_reference,
)
from circuitry.plugins.base import ToolResult


def _make_store() -> Store:
    return Store({})


def _json(name: str, text: str) -> dict[str, Any]:
    return {"type": "tool", "name": name, "provider": "json", "params": {"mode": "parse", "input": text}}


def _run(orch: dict[str, Any], *, inputs: dict[str, Any] | None = None) -> dict[str, Any]:
    root = compile_orchestration(orch=orch)
    store = Store(state={"input": dict(inputs or {})})
    DynamicRuntime(root, adapter=MagicMock(), model="test", runtime_config={"_test_marker": True}).execute(
        store=store
    )
    return store.state


# ── param_reference ──────────────────────────────────────────────────────────


def test_param_reference_accepts_only_from_or_from_plus_default() -> None:
    assert param_reference({"from": "prime.x.value"}) == ParamReference(path="prime.x.value")
    assert param_reference({"from": "  prime.x.value  "}) == ParamReference(path="prime.x.value")
    assert param_reference({"from": "x", "default": [1, 2]}) == ParamReference(
        path="x", has_default=True, default=[1, 2]
    )
    assert param_reference({"from": "a", "other": 1}) is None
    assert param_reference({"from": 3}) is None
    assert param_reference({"from": "x", "default": 1, "other": 2}) is None
    assert param_reference("{{input.items}}") is None
    assert param_reference(["from"]) is None


# ── Compile-time validation ──────────────────────────────────────────────────


def test_compile_tool_params_leaves_reference_untouched() -> None:
    effect = {
        "type": "tool",
        "name": "call",
        "provider": "mcp",
        "params": {"arguments": {"symbols": {"from": "prime.symbol_list.value"}}},
    }
    defn = _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")
    assert isinstance(defn, ToolDefinition)
    assert defn.params == {"arguments": {"symbols": {"from": "prime.symbol_list.value"}}}


@pytest.mark.parametrize(
    ("path", "message"),
    [
        ("items", "not rooted at a state namespace or an enclosing loop binding"),
        ("state.prime.src.value", "Write 'prime.src.value' instead of 'state.prime.src.value'"),
        ("", "needs a non-empty path"),
    ],
)
def test_params_from_rejects_bad_roots_at_compile_time(path: str, message: str) -> None:
    orch = {
        "effects": [
            {"type": "tool", "name": "call", "provider": "json", "params": {"mode": "parse", "input": {"from": path}}},
        ]
    }
    with pytest.raises(ValueError, match=message):
        compile_orchestration(orch=orch)


def test_params_from_rejects_a_binding_outside_its_loop() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "ladder",
                "each": {"in": "input.rungs", "as": "r"},
                "body": [_json("noop", "true")],
            },
            {"type": "tool", "name": "after", "provider": "json", "params": {"mode": "parse", "input": {"from": "r"}}},
        ]
    }
    with pytest.raises(ValueError, match="loop bindings in scope: none here"):
        compile_orchestration(orch=orch)


def test_params_from_accepts_a_binding_inside_its_loop() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "ladder",
                "each": {"in": "input.rungs", "as": "r"},
                "body": [
                    {"type": "tool", "name": "t", "provider": "json", "params": {"mode": "parse", "input": "null", "got": {"from": "r"}}},
                ],
            }
        ]
    }
    compile_orchestration(orch=orch)  # must not raise


def test_params_from_error_names_the_params_path() -> None:
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "call",
                "provider": "json",
                "params": {"mode": "parse", "input": "null", "nested": {"deep": {"from": "items"}}},
            },
        ]
    }
    with pytest.raises(ValueError, match=r"Tool effect 'call' param 'params\.nested\.deep'"):
        compile_orchestration(orch=orch)


def test_params_from_is_validated_inside_a_list() -> None:
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "call",
                "provider": "json",
                "params": {"mode": "parse", "input": "null", "items": [{"from": "items"}]},
            },
        ]
    }
    with pytest.raises(ValueError, match=r"param 'params\.items\[0\]'"):
        compile_orchestration(orch=orch)


def test_malformed_template_elsewhere_in_params_is_still_caught() -> None:
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "call",
                "provider": "json",
                "params": {"mode": "parse", "input": "{{input.x"},
            },
        ]
    }
    with pytest.raises(ValueError, match="malformed Mustache template"):
        compile_orchestration(orch=orch)


# ── Runtime resolution ────────────────────────────────────────────────────────


def test_tool_runtime_resolves_a_from_leaf_to_the_native_value(monkeypatch: pytest.MonkeyPatch) -> None:
    captured: dict[str, Any] = {}

    def capturing_plugin(**kw: Any) -> Any:
        m = MagicMock()

        def execute(*, params: dict[str, Any], timeout_seconds: int) -> ToolResult:
            captured.update(params)
            return ToolResult(value="ok", raw={})

        m.execute.side_effect = execute
        return m

    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", capturing_plugin)

    defn = ToolDefinition(
        name="call",
        provider="mcp",
        params={"symbols": {"from": "prime.src.value"}, "label": "{{tag}}"},
    )
    store = _make_store()
    ctx = {"prime": {"src": {"value": [1, 2, 3]}}, "tag": "x"}

    ToolRuntime(defn).execute(store=store, ctx=ctx)

    assert captured["symbols"] == [1, 2, 3]
    assert captured["label"] == "x"


def test_tool_runtime_from_leaf_is_never_mustache_rendered(monkeypatch: pytest.MonkeyPatch) -> None:
    """The resolved value is used as-is even when it happens to be a string
    that looks like a template -- it is never passed through render_template."""
    captured: dict[str, Any] = {}

    def capturing_plugin(**kw: Any) -> Any:
        m = MagicMock()

        def execute(*, params: dict[str, Any], timeout_seconds: int) -> ToolResult:
            captured.update(params)
            return ToolResult(value="ok", raw={})

        m.execute.side_effect = execute
        return m

    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", capturing_plugin)

    defn = ToolDefinition(name="call", provider="mcp", params={"text": {"from": "prime.src.value"}})
    store = _make_store()
    ctx = {"prime": {"src": {"value": "{{not_a_binding}}"}}}

    ToolRuntime(defn).execute(store=store, ctx=ctx)

    assert captured["text"] == "{{not_a_binding}}"


def test_tool_runtime_deep_copies_the_resolved_value(monkeypatch: pytest.MonkeyPatch) -> None:
    captured: dict[str, Any] = {}

    def capturing_plugin(**kw: Any) -> Any:
        m = MagicMock()

        def execute(*, params: dict[str, Any], timeout_seconds: int) -> ToolResult:
            captured.update(params)
            return ToolResult(value="ok", raw={})

        m.execute.side_effect = execute
        return m

    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", capturing_plugin)

    defn = ToolDefinition(name="call", provider="mcp", params={"items": {"from": "prime.src.value"}})
    store = _make_store()
    source_value = [1, {"n": 2}]
    ctx = {"prime": {"src": {"value": source_value}}}

    ToolRuntime(defn).execute(store=store, ctx=ctx)

    assert captured["items"] == source_value
    assert captured["items"] is not source_value


def test_tool_runtime_from_resolves_at_any_depth_in_dicts_and_lists(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured: dict[str, Any] = {}

    def capturing_plugin(**kw: Any) -> Any:
        m = MagicMock()

        def execute(*, params: dict[str, Any], timeout_seconds: int) -> ToolResult:
            captured.update(params)
            return ToolResult(value="ok", raw={})

        m.execute.side_effect = execute
        return m

    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", capturing_plugin)

    defn = ToolDefinition(
        name="call",
        provider="mcp",
        params={
            "nested": {"deep": {"from": "prime.a.value"}},
            "items": ["literal", {"from": "prime.b.value"}],
        },
    )
    store = _make_store()
    ctx = {"prime": {"a": {"value": {"k": 1}}, "b": {"value": [9, 8]}}}

    ToolRuntime(defn).execute(store=store, ctx=ctx)

    assert captured["nested"]["deep"] == {"k": 1}
    assert captured["items"] == ["literal", [9, 8]]


def test_tool_runtime_unresolved_from_without_default_raises(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: MagicMock())

    defn = ToolDefinition(name="call", provider="mcp", params={"symbols": {"from": "prime.missing.value"}})
    store = _make_store()

    with pytest.raises(ValueError, match="did not resolve to a value"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert "params.symbols" in store.state["call"]["meta"]["error"]


def test_tool_runtime_unresolved_from_with_default_falls_back(monkeypatch: pytest.MonkeyPatch) -> None:
    captured: dict[str, Any] = {}

    def capturing_plugin(**kw: Any) -> Any:
        m = MagicMock()

        def execute(*, params: dict[str, Any], timeout_seconds: int) -> ToolResult:
            captured.update(params)
            return ToolResult(value="ok", raw={})

        m.execute.side_effect = execute
        return m

    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", capturing_plugin)

    defn = ToolDefinition(
        name="call",
        provider="mcp",
        params={"symbols": {"from": "prime.missing.value", "default": []}},
    )
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})

    assert captured["symbols"] == []
    assert store.state["call"]["meta"]["error"] is None


def test_tool_runtime_a_lookalike_mapping_is_passed_through_literally(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured: dict[str, Any] = {}

    def capturing_plugin(**kw: Any) -> Any:
        m = MagicMock()

        def execute(*, params: dict[str, Any], timeout_seconds: int) -> ToolResult:
            captured.update(params)
            return ToolResult(value="ok", raw={})

        m.execute.side_effect = execute
        return m

    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", capturing_plugin)

    defn = ToolDefinition(name="call", provider="mcp", params={"got": {"from": "x", "also": 1}})
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})

    assert captured["got"] == {"from": "x", "also": 1}


# ── End-to-end: loop bindings, nested loop `prev`, and real tool plugins ─────


def test_from_passes_structured_values_unchanged(tmp_path: Path) -> None:
    orch = {
        "effects": [
            _json("src", "[1, 2, 3]"),
            {
                "type": "tool",
                "name": "call",
                "provider": "json",
                "params": {"mode": "parse", "input": "null", "symbols": {"from": "prime.src.value"}},
            },
        ]
    }
    state = _run(orch)
    assert state["prime"]["call"]["meta"]["params_rendered"]["symbols"] == [1, 2, 3]


@pytest.mark.parametrize("flow", ["chain", "tree"])
def test_from_accepts_loop_bindings(flow: str) -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "ladder",
                "flow": flow,
                "max_concurrency": 4,
                "each": {"in": "input.rungs", "as": "r"},
                "body": [
                    {
                        "type": "tool",
                        "name": "whole",
                        "provider": "json",
                        "params": {"mode": "parse", "input": "null", "got": {"from": "r"}},
                    },
                    {
                        "type": "tool",
                        "name": "field",
                        "provider": "json",
                        "params": {"mode": "parse", "input": "null", "got": {"from": "r.factor"}},
                    },
                ],
            }
        ]
    }
    rungs = [{"name": "4k", "factor": 2}, {"name": "1080p", "factor": 4}]
    state = _run(orch, inputs={"rungs": rungs})
    for i, rung in enumerate(rungs):
        node = state["prime"]["ladder"][f"iter_{i}"]
        assert node["whole"]["meta"]["params_rendered"]["got"] == rung
        assert node["field"]["meta"]["params_rendered"]["got"] == rung["factor"]


def test_from_reads_prime_loop_prev_in_chain_flow() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "accum",
                "each": {"in": "input.items", "as": "x"},
                "body": [
                    {
                        "type": "tool",
                        "name": "step",
                        "provider": "json",
                        "params": {
                            "mode": "parse",
                            "input": "{{{x}}}",
                            "prev": {"from": "prime.accum.prev.step.value", "default": None},
                        },
                    }
                ],
            }
        ]
    }
    state = _run(orch, inputs={"items": [1, 2]})
    first = state["prime"]["accum"]["iter_0"]["step"]["meta"]["params_rendered"]
    second = state["prime"]["accum"]["iter_1"]["step"]["meta"]["params_rendered"]
    assert first["prev"] is None
    assert second["prev"] == 1
