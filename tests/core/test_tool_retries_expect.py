"""`retries` and `expect:` on `tool` effects (#273)."""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

from circuitry.core.compiler import _compile_effect
from circuitry.core.expect import ExpectDef
from circuitry.core.prompt import RetryPolicyDef
from circuitry.core.store import Store
from circuitry.core.tool import ToolDefinition, ToolRuntime
from circuitry.plugins.base import ToolResult


def _make_store() -> Store:
    return Store({})


def _patch_plugin(monkeypatch: pytest.MonkeyPatch, plugin: MagicMock) -> None:
    monkeypatch.setattr(
        "circuitry.plugins.factory.build_plugin", lambda **kw: plugin
    )
    monkeypatch.setattr("circuitry.core.tool.time.sleep", lambda s: None)


# ---------------------------------------------------------------------------
# Compiler
# ---------------------------------------------------------------------------


def test_compile_tool_retries_field() -> None:
    effect = {
        "type": "tool",
        "name": "x",
        "provider": "ffmpeg",
        "retries": {"max_attempts": 3, "backoff_ms": 200},
    }
    defn = _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")
    assert isinstance(defn, ToolDefinition)
    assert defn.retries == RetryPolicyDef(max_attempts=3, backoff_ms=200)


def test_compile_tool_expect_bare_string_is_cel_shorthand() -> None:
    effect = {
        "type": "tool",
        "name": "x",
        "provider": "ffmpeg",
        "expect": "has(value.prompt_id)",
    }
    defn = _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")
    assert isinstance(defn, ToolDefinition)
    assert defn.expect == ExpectDef(mode="cel", expr="has(value.prompt_id)")


def test_compile_tool_expect_model_mode_requires_template() -> None:
    effect = {
        "type": "tool",
        "name": "x",
        "provider": "ffmpeg",
        "expect": {"mode": "model"},
    }
    with pytest.raises(ValueError, match="requires a non-empty 'template'"):
        _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")


def test_compile_tool_expect_bad_cel_syntax_raises() -> None:
    effect = {
        "type": "tool",
        "name": "x",
        "provider": "ffmpeg",
        "expect": "has(value.prompt_id",
    }
    with pytest.raises(ValueError, match="does not parse"):
        _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")


# ---------------------------------------------------------------------------
# Runtime — retries
# ---------------------------------------------------------------------------


def test_process_tool_retries_on_any_failure_then_succeeds(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    ok_result = ToolResult(value="done", raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.side_effect = [RuntimeError("transient"), ok_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["x"]["value"] == "done"
    assert store.state["x"]["meta"]["error"] is None
    assert store.state["x"]["meta"]["retries_used"] == 1
    assert plugin.execute.call_count == 2


def test_process_tool_exhausts_retries_and_fails(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    plugin = MagicMock()
    plugin.execute.side_effect = RuntimeError("always fails")
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=10),
    )
    store = _make_store()
    with pytest.raises(RuntimeError, match="always fails"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert plugin.execute.call_count == 2
    assert "always fails" in store.state["x"]["meta"]["error"]


def test_tool_without_retries_fails_on_first_attempt(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    plugin = MagicMock()
    plugin.execute.side_effect = RuntimeError("boom")
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(name="x", provider="shell", params={})
    store = _make_store()
    with pytest.raises(RuntimeError, match="boom"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert plugin.execute.call_count == 1


def test_http_family_5xx_is_retried(monkeypatch: pytest.MonkeyPatch) -> None:
    fail_result = ToolResult(
        value=None, raw={"status": 503}, stdout="", stderr="server busy", exit_code=None, ok=False
    )
    ok_result = ToolResult(value={"ok": True}, raw={"status": 200}, stdout="", stderr="", exit_code=None)
    plugin = MagicMock()
    plugin.execute.side_effect = [fail_result, ok_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="http",
        params={},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["x"]["value"] == {"ok": True}
    assert plugin.execute.call_count == 2


def test_http_family_4xx_is_not_retried(monkeypatch: pytest.MonkeyPatch) -> None:
    fail_result = ToolResult(
        value=None, raw={"status": 404}, stdout="", stderr="not found", exit_code=None, ok=False
    )
    plugin = MagicMock()
    plugin.execute.side_effect = [fail_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="http",
        params={},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
    )
    store = _make_store()
    with pytest.raises(RuntimeError, match="not found"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert plugin.execute.call_count == 1


# ---------------------------------------------------------------------------
# Runtime — expect
# ---------------------------------------------------------------------------


def test_expect_cel_passes_records_meta(monkeypatch: pytest.MonkeyPatch) -> None:
    ok_result = ToolResult(value={"prompt_id": "abc"}, raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.return_value = ok_result
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        expect=ExpectDef(mode="cel", expr="has(value.prompt_id)"),
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["x"]["meta"]["error"] is None
    assert store.state["x"]["meta"]["expect"]["result"] is True
    assert store.state["x"]["meta"]["expect"]["mode"] == "cel"


def test_expect_cel_fails_and_retries_then_succeeds(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    missing_result = ToolResult(value={}, raw={}, stdout="", stderr="", exit_code=0)
    present_result = ToolResult(value={"prompt_id": "abc"}, raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.side_effect = [missing_result, present_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
        expect=ExpectDef(mode="cel", expr="has(value.prompt_id)"),
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["x"]["value"] == {"prompt_id": "abc"}
    assert store.state["x"]["meta"]["retries_used"] == 1
    assert plugin.execute.call_count == 2


def test_expect_cel_exhausts_retries_fails_with_expect_message(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    missing_result = ToolResult(value={}, raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.return_value = missing_result
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=10),
        expect=ExpectDef(mode="cel", expr="has(value.prompt_id)"),
    )
    store = _make_store()
    with pytest.raises(RuntimeError, match="expect failed"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert plugin.execute.call_count == 2
    assert "expect failed" in store.state["x"]["meta"]["error"]


def test_expect_model_mode_asks_adapter_and_records_tokens(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from circuitry.adapters.base import GenerateResult

    ok_result = ToolResult(value="a summary", raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.return_value = ok_result
    _patch_plugin(monkeypatch, plugin)

    adapter = MagicMock()
    adapter.generate.return_value = GenerateResult(
        text="yes", raw={}, tokens_sent=12, tokens_received=1
    )

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        expect=ExpectDef(mode="model", template="Does this look right? {{value}}"),
    )
    store = _make_store()
    ToolRuntime(defn, adapter=adapter, model="m").execute(store=store, ctx={})

    assert store.state["x"]["meta"]["error"] is None
    expect_meta = store.state["x"]["meta"]["expect"]
    assert expect_meta["mode"] == "model"
    assert expect_meta["result"] is True
    assert expect_meta["tokens_sent"] == 12
    adapter.generate.assert_called_once()


def test_expect_model_mode_without_adapter_raises_configuration_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    ok_result = ToolResult(value="x", raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.return_value = ok_result
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        expect=ExpectDef(mode="model", template="ok?"),
    )
    store = _make_store()
    with pytest.raises(ValueError, match="requires the run's adapter/model"):
        ToolRuntime(defn).execute(store=store, ctx={})
