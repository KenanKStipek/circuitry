from __future__ import annotations

from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest

from circuitry.core.compiler import _compile_effect, compile_orchestration
from circuitry.core.store import Store
from circuitry.core.tool import ToolDefinition, ToolRuntime
from circuitry.plugins.base import ToolResult

# ---------------------------------------------------------------------------
# Compiler tests
# ---------------------------------------------------------------------------


def test_compile_tool_effect_produces_tool_definition() -> None:
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "transcode",
                "provider": "ffmpeg",
                "params": {"input": "a.mp4", "output": "b.mp4"},
            }
        ]
    }
    root = compile_orchestration(orch=orch)
    assert len(root.effects) == 1
    defn = root.effects[0]
    assert isinstance(defn, ToolDefinition)
    assert defn.name == "transcode"
    assert defn.provider == "ffmpeg"
    assert defn.params == {"input": "a.mp4", "output": "b.mp4"}


def test_compile_tool_effect_missing_name_raises() -> None:
    effect = {"type": "tool", "provider": "ffmpeg"}
    with pytest.raises(ValueError, match="missing required field 'name'"):
        _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")


def test_compile_tool_effect_missing_provider_raises() -> None:
    effect = {"type": "tool", "name": "do_thing"}
    with pytest.raises(ValueError, match="'provider'"):
        _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")


def test_compile_tool_effect_on_error_defaults_to_fail() -> None:
    effect = {"type": "tool", "name": "x", "provider": "ffmpeg"}
    defn = _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")
    assert isinstance(defn, ToolDefinition)
    assert defn.on_error == "fail"


def test_compile_tool_effect_on_error_skip() -> None:
    effect = {"type": "tool", "name": "x", "provider": "ffmpeg", "on_error": "skip"}
    defn = _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")
    assert isinstance(defn, ToolDefinition)
    assert defn.on_error == "skip"


def test_compile_tool_effect_params_json_field() -> None:
    effect = {
        "type": "tool",
        "name": "get_equity_quotes",
        "provider": "mcp",
        "params": {"server": "robinhood"},
        "params_json": '{"arguments": {"symbols": {{{prime.symbol_list.value}}} }}',
    }
    defn = _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")
    assert isinstance(defn, ToolDefinition)
    assert defn.params == {"server": "robinhood"}
    assert (
        defn.params_json
        == '{"arguments": {"symbols": {{{prime.symbol_list.value}}} }}'
    )


def test_compile_tool_effect_params_json_defaults_to_none() -> None:
    effect = {"type": "tool", "name": "x", "provider": "ffmpeg"}
    defn = _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")
    assert isinstance(defn, ToolDefinition)
    assert defn.params_json is None


def test_compile_tool_effect_params_json_wrong_type_raises() -> None:
    effect = {
        "type": "tool",
        "name": "x",
        "provider": "mcp",
        "params_json": {"not": "a string"},
    }
    with pytest.raises(ValueError, match="params_json"):
        _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")


def test_compile_prompt_type_image_raises_migration_error() -> None:
    effect = {
        "type": "prompt",
        "name": "gen",
        "prompt_type": "image",
        "template": "a cat",
    }
    with pytest.raises(ValueError, match="no longer supported"):
        _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")


# ---------------------------------------------------------------------------
# ToolRuntime tests
# ---------------------------------------------------------------------------


def _make_store() -> Store:
    return Store({})


def test_tool_runtime_writes_value_to_store(monkeypatch: pytest.MonkeyPatch) -> None:
    fake_result = ToolResult(value="/out/video.mp4", raw={}, stdout="", stderr="", exit_code=0)

    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = fake_result

    monkeypatch.setattr(
        "circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin
    )

    defn = ToolDefinition(name="transcode", provider="ffmpeg", params={"input": "a.mp4", "output": "/out/video.mp4"})
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["transcode"]["value"] == "/out/video.mp4"
    assert store.state["transcode"]["meta"]["provider"] == "ffmpeg"
    assert store.state["transcode"]["meta"]["error"] is None


def test_tool_runtime_dry_run_skips_plugin(monkeypatch: pytest.MonkeyPatch) -> None:
    build_plugin_called = False

    def fake_build_plugin(**kw: Any) -> Any:
        nonlocal build_plugin_called
        build_plugin_called = True
        return MagicMock()

    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", fake_build_plugin)

    defn = ToolDefinition(name="transcode", provider="ffmpeg", params={})
    store = _make_store()

    ToolRuntime(defn, dry_run=True).execute(store=store, ctx={})

    assert not build_plugin_called
    assert store.state["transcode"]["meta"].get("dry_run") is True


def test_tool_runtime_on_error_skip_does_not_raise(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def bad_plugin(**kw: Any) -> Any:
        m = MagicMock()
        m.execute.side_effect = RuntimeError("plugin exploded")
        return m

    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", bad_plugin)

    defn = ToolDefinition(
        name="bad", provider="ffmpeg", params={}, on_error="skip"
    )
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})  # should not raise

    assert store.state["bad"]["value"] is None
    assert "plugin exploded" in store.state["bad"]["meta"]["error"]


def test_tool_runtime_on_error_fail_raises(monkeypatch: pytest.MonkeyPatch) -> None:
    def bad_plugin(**kw: Any) -> Any:
        m = MagicMock()
        m.execute.side_effect = RuntimeError("plugin exploded")
        return m

    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", bad_plugin)

    defn = ToolDefinition(
        name="bad", provider="ffmpeg", params={}, on_error="fail"
    )
    store = _make_store()

    with pytest.raises(RuntimeError, match="plugin exploded"):
        ToolRuntime(defn).execute(store=store, ctx={})


def test_tool_runtime_mustache_renders_params(monkeypatch: pytest.MonkeyPatch) -> None:
    captured_params: dict[str, Any] = {}

    def capturing_plugin(**kw: Any) -> Any:
        m = MagicMock()

        def execute(*, params: dict[str, Any], timeout_seconds: int) -> ToolResult:
            captured_params.update(params)
            return ToolResult(value="ok", raw={})

        m.execute.side_effect = execute
        return m

    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", capturing_plugin)

    defn = ToolDefinition(
        name="test",
        provider="ffmpeg",
        params={"input": "{{source}}", "output": "{{dest}}"},
    )
    store = _make_store()

    ToolRuntime(defn).execute(
        store=store, ctx={"source": "/in/a.mp4", "dest": "/out/b.mp4"}
    )

    assert captured_params["input"] == "/in/a.mp4"
    assert captured_params["output"] == "/out/b.mp4"


def _capturing_plugin(captured_params: dict[str, Any]) -> Any:
    def fake_build_plugin(**kw: Any) -> Any:
        m = MagicMock()

        def execute(*, params: dict[str, Any], timeout_seconds: int) -> ToolResult:
            captured_params.update(params)
            return ToolResult(value="ok", raw={})

        m.execute.side_effect = execute
        return m

    return fake_build_plugin


def test_tool_runtime_params_json_builds_array_from_prior_step(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """mcp's arguments.symbols as a real array built at runtime, not one call per symbol."""
    captured_params: dict[str, Any] = {}
    monkeypatch.setattr(
        "circuitry.plugins.factory.build_plugin", _capturing_plugin(captured_params)
    )

    defn = ToolDefinition(
        name="get_equity_quotes",
        provider="mcp",
        params={"server": "robinhood", "tool": "get_equity_quotes"},
        params_json='{"arguments": {"symbols": {{{prime.symbol_list.value}}} }}',
    )
    store = _make_store()
    ctx = {"prime": {"symbol_list": {"value": '["AAPL", "MSFT", "TSLA"]'}}}

    ToolRuntime(defn).execute(store=store, ctx=ctx)

    assert captured_params["server"] == "robinhood"
    assert captured_params["tool"] == "get_equity_quotes"
    assert captured_params["arguments"] == {"symbols": ["AAPL", "MSFT", "TSLA"]}
    assert store.state["get_equity_quotes"]["meta"]["params_rendered"] == captured_params


def test_tool_runtime_params_json_builds_nested_object(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """surrealdb create/upsert receiving a nested object built at runtime."""
    captured_params: dict[str, Any] = {}
    monkeypatch.setattr(
        "circuitry.plugins.factory.build_plugin", _capturing_plugin(captured_params)
    )

    defn = ToolDefinition(
        name="save_person",
        provider="surrealdb",
        params={"mode": "create", "table": "person"},
        params_json='{"data": {{{prime.person_json.value}}} }',
    )
    store = _make_store()
    ctx = {"prime": {"person_json": {"value": '{"name": "Ada", "score": 42}'}}}

    ToolRuntime(defn).execute(store=store, ctx=ctx)

    assert captured_params["mode"] == "create"
    assert captured_params["table"] == "person"
    assert captured_params["data"] == {"name": "Ada", "score": 42}


def test_tool_runtime_params_json_wins_on_key_conflict(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured_params: dict[str, Any] = {}
    monkeypatch.setattr(
        "circuitry.plugins.factory.build_plugin", _capturing_plugin(captured_params)
    )

    defn = ToolDefinition(
        name="x",
        provider="mcp",
        params={"arguments": {"symbols": ["placeholder"], "keep": "me"}},
        params_json='{"arguments": {"symbols": ["AAPL"]}}',
    )
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})

    # params_json wins on the conflicting key, but a deep merge keeps
    # sibling keys from params that params_json didn't mention.
    assert captured_params["arguments"] == {"symbols": ["AAPL"], "keep": "me"}


def test_tool_runtime_params_json_invalid_json_fail_raises(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: MagicMock())

    defn = ToolDefinition(
        name="x", provider="mcp", params={}, params_json="{not valid json", on_error="fail"
    )
    store = _make_store()

    with pytest.raises(ValueError, match="params_json"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert "params_json" in store.state["x"]["meta"]["error"]


def test_tool_runtime_params_json_invalid_json_skip_does_not_raise(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: MagicMock())

    defn = ToolDefinition(
        name="x", provider="mcp", params={}, params_json="[1, 2, 3]", on_error="skip"
    )
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})  # should not raise

    assert store.state["x"]["value"] is None
    assert "JSON object" in store.state["x"]["meta"]["error"]


# ---------------------------------------------------------------------------
# Schema validation tests (via circuitry validate)
# ---------------------------------------------------------------------------


def test_validate_accepts_tool_effect(tmp_path: Path) -> None:
    from circuitry.cli.runtime_shim import validate

    path = tmp_path / "tool.yml"
    path.write_text(
        """
effects:
  - type: tool
    name: transcode
    provider: ffmpeg
    params:
      input: a.mp4
      output: b.mp4
""".strip()
        + "\n",
        encoding="utf-8",
    )
    result = validate(path)
    assert result["ok"] is True, result["errors"]


def test_validate_accepts_tool_effect_with_params_json(tmp_path: Path) -> None:
    from circuitry.cli.runtime_shim import validate

    path = tmp_path / "tool.yml"
    path.write_text(
        """
effects:
  - type: tool
    name: get_equity_quotes
    provider: mcp
    params:
      server: robinhood
      tool: get_equity_quotes
    params_json: '{"arguments": {"symbols": {{{prime.symbol_list.value}}} }}'
""".strip()
        + "\n",
        encoding="utf-8",
    )
    result = validate(path)
    assert result["ok"] is True, result["errors"]


def test_validate_rejects_tool_missing_provider(tmp_path: Path) -> None:
    from circuitry.cli.runtime_shim import validate

    path = tmp_path / "bad_tool.yml"
    path.write_text(
        """
effects:
  - type: tool
    name: transcode
""".strip()
        + "\n",
        encoding="utf-8",
    )
    result = validate(path)
    assert result["ok"] is False


def test_has_prompt_effects_returns_false_for_tool_only() -> None:
    from circuitry.cli.runtime_shim import _has_prompt_effects

    root = compile_orchestration(
        orch={
            "effects": [
                {"type": "tool", "name": "x", "provider": "ffmpeg", "params": {}}
            ]
        }
    )
    assert _has_prompt_effects(root) is False


def test_has_prompt_effects_returns_true_for_prompt() -> None:
    from circuitry.cli.runtime_shim import _has_prompt_effects

    root = compile_orchestration(
        orch={
            "effects": [
                {"type": "prompt", "name": "greet", "template": "Hello"}
            ]
        }
    )
    assert _has_prompt_effects(root) is True


# ---------------------------------------------------------------------------
# Tool effects nested inside control-flow containers
#
# Conditional branches and loop bodies dispatch on effect type. A type the
# dispatch chain forgets used to fall straight through — the effect was
# recorded as executed, wrote nothing, and the run reported success. These
# pin the composition down.
# ---------------------------------------------------------------------------


def _tool_returning(value: Any) -> Any:
    plugin = MagicMock()
    plugin.execute.return_value = ToolResult(value=value, raw={}, exit_code=0)
    return plugin


def test_tool_effect_runs_inside_a_conditional_branch(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from circuitry.core.dynamic import DynamicRuntime

    monkeypatch.setattr(
        "circuitry.plugins.factory.build_plugin",
        lambda **kw: _tool_returning("then-branch"),
    )

    root = compile_orchestration(
        orch={
            "effects": [
                {
                    "type": "if",
                    "if": {"mode": "cel", "expr": "true"},
                    "then": [
                        {"type": "tool", "name": "picked", "provider": "json"}
                    ],
                    "else": [
                        {"type": "tool", "name": "picked", "provider": "json"}
                    ],
                }
            ]
        }
    )
    store = Store({})
    DynamicRuntime(root, adapter=MagicMock(), model="test").execute(store=store)

    assert store.state["prime"]["picked"]["value"] == "then-branch"


def test_tool_effect_runs_inside_a_loop_body(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from circuitry.core.dynamic import DynamicRuntime

    monkeypatch.setattr(
        "circuitry.plugins.factory.build_plugin",
        lambda **kw: _tool_returning("ran"),
    )

    root = compile_orchestration(
        orch={
            "effects": [
                {
                    "type": "loop",
                    "name": "twice",
                    "each": {"in": "input.items", "as": "item"},
                    "body": [
                        {"type": "tool", "name": "step", "provider": "json"}
                    ],
                }
            ]
        }
    )
    store = Store({"input": {"items": ["a", "b"]}})
    DynamicRuntime(root, adapter=MagicMock(), model="test").execute(store=store)

    loop_node = store.state["prime"]["twice"]
    assert loop_node["iter_0"]["step"]["value"] == "ran"
    assert loop_node["iter_1"]["step"]["value"] == "ran"


def test_unnamed_loop_body_overwrites_at_a_stable_path(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The idiom the wizard's revision loop depends on: an unnamed loop merges
    its body into the parent scope, so a CEL while-condition can observe the
    body's own latest output instead of an unreachable iter_<N> node."""
    from circuitry.core.dynamic import DynamicRuntime

    values = iter(["first", "second", "stop"])
    plugin = MagicMock()
    plugin.execute.side_effect = lambda **kw: ToolResult(
        value=next(values), raw={}, exit_code=0
    )
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: plugin)

    root = compile_orchestration(
        orch={
            "effects": [
                {"type": "tool", "name": "probe", "provider": "json"},
                {
                    "type": "loop",
                    "max_iterations": 5,
                    "while": {
                        "mode": "cel",
                        "expr": 'state.prime.probe.value != "stop"',
                    },
                    "body": [
                        {"type": "tool", "name": "probe", "provider": "json"}
                    ],
                },
            ]
        }
    )
    store = Store({})
    DynamicRuntime(root, adapter=MagicMock(), model="test").execute(store=store)

    # first (pre-loop) → second → stop, then the condition goes false.
    assert store.state["prime"]["probe"]["value"] == "stop"
    assert plugin.execute.call_count == 3
