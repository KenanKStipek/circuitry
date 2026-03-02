from __future__ import annotations

from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest

from circuitry.core.compiler import compile_orchestration, _compile_effect
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
