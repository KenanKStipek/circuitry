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


def test_tool_runtime_records_resolved_binary_in_meta(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """issue #222: a plugin result carrying raw['binary'] (the resolved
    absolute executable) surfaces at meta.binary on the run record."""
    fake_result = ToolResult(
        value="out.png",
        raw={"binary": "/opt/imagemagick-omp/bin/magick"},
        stdout="",
        stderr="",
        exit_code=0,
    )

    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = fake_result

    monkeypatch.setattr(
        "circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin
    )

    defn = ToolDefinition(name="resize", provider="imagemagick", params={"args": []})
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["resize"]["meta"]["binary"] == "/opt/imagemagick-omp/bin/magick"


def test_tool_runtime_omits_binary_from_meta_when_result_has_none(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A plugin whose result carries no 'binary' key (e.g. a pure-Python
    tool) leaves meta.binary unset rather than recording a stale value."""
    fake_result = ToolResult(value=42, raw={}, stdout="", stderr="", exit_code=0)

    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = fake_result

    monkeypatch.setattr(
        "circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin
    )

    defn = ToolDefinition(name="calc", provider="math", params={})
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})

    assert "binary" not in store.state["calc"]["meta"]


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


# ---------------------------------------------------------------------------
# #262 part 1: one failure contract — ToolResult.ok, not just exceptions.
# ---------------------------------------------------------------------------


def test_tool_runtime_ok_false_sets_meta_error_without_an_exception(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A plugin that returns ok=False (no exception) must still fail the
    effect under on_error: fail, exactly like a raised exception."""
    fake_result = ToolResult(
        value=None, raw={"status": 500}, stderr="HTTP 500", exit_code=None, ok=False
    )
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = fake_result
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(name="call", provider="http", params={}, on_error="fail")
    store = _make_store()

    with pytest.raises(RuntimeError, match="HTTP 500"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["call"]["meta"]["error"] == "HTTP 500"


def test_tool_runtime_ok_false_on_error_skip_does_not_raise(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fake_result = ToolResult(
        value="body", raw={"status": 404}, stderr="HTTP 404", exit_code=None, ok=False
    )
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = fake_result
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(name="call", provider="http", params={}, on_error="skip")
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})  # should not raise

    assert store.state["call"]["value"] is None
    assert store.state["call"]["meta"]["error"] == "HTTP 404"


def test_tool_runtime_ok_true_leaves_meta_error_none(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fake_result = ToolResult(value="fine", raw={"status": 200}, exit_code=None, ok=True)
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = fake_result
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(name="call", provider="http", params={})
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["call"]["meta"]["error"] is None


def test_tool_runtime_sets_meta_status_code_from_raw_status(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fake_result = ToolResult(value="x", raw={"status": 204}, exit_code=None, ok=True)
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = fake_result
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(name="call", provider="http", params={})
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["call"]["meta"]["status_code"] == 204


def test_tool_runtime_does_not_set_status_code_for_non_http_family_providers(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An unrelated int raw['status'] from a non-HTTP-family plugin must not
    be mistaken for an HTTP status."""
    fake_result = ToolResult(value="x", raw={"status": 204}, exit_code=None, ok=True)
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = fake_result
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(name="call", provider="shell", params={})
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})

    assert "status_code" not in store.state["call"]["meta"]


def test_tool_runtime_meta_exit_code_is_the_process_exit_code(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A process-backed plugin's exit_code lands verbatim on meta.exit_code —
    the owner's orchestrations read state.prime.probe.meta.exit_code on
    shell tools and that must keep working."""
    fake_result = ToolResult(value="out", raw={}, exit_code=2)
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = fake_result
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(name="probe", provider="shell", params={})
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["probe"]["meta"]["exit_code"] == 2


# ---------------------------------------------------------------------------
# #262 part 1 raw + #264 part 5: meta.raw — redacted, size-capped.
# ---------------------------------------------------------------------------


def test_tool_runtime_records_meta_raw_from_tool_result_raw(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fake_result = ToolResult(
        value="x", raw={"status": 200, "headers": {"content-type": "text/plain"}}
    )
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = fake_result
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(name="call", provider="http", params={})
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["call"]["meta"]["raw"] == {
        "status": 200,
        "headers": {"content-type": "text/plain"},
    }


def test_tool_runtime_redacts_credential_like_values_in_meta_raw(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fake_result = ToolResult(
        value="x",
        raw={"headers": {"Authorization": "Bearer sk-abcdefghijklmnopqrstuvwxyz0123"}},
    )
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = fake_result
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(name="call", provider="http", params={})
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["call"]["meta"]["raw"]["headers"]["Authorization"] == "***REDACTED***"


def test_tool_runtime_caps_oversized_meta_raw(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from circuitry.core.tool import _RAW_META_MAX_BYTES

    huge = "x" * (_RAW_META_MAX_BYTES * 2)
    fake_result = ToolResult(value="x", raw={"body": huge})
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = fake_result
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(name="call", provider="http", params={})
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})

    raw_meta = store.state["call"]["meta"]["raw"]
    assert raw_meta["_truncated"] is True
    assert raw_meta["_original_bytes"] > _RAW_META_MAX_BYTES


def test_tool_runtime_meta_raw_is_json_safe_for_non_json_native_values(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A plugin raw with e.g. bytes/datetime must survive a plain json.dumps
    later (--out, the SQL stores use no default=), not just the capping
    pass's own json.dumps(..., default=str)."""
    import datetime as _dt
    import json as _json

    fake_result = ToolResult(
        value="x",
        raw={
            "when": _dt.datetime(2024, 1, 1, tzinfo=_dt.timezone.utc),
            "blob": b"\x00\x01",
        },
    )
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = fake_result
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(name="call", provider="http", params={})
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})

    raw_meta = store.state["call"]["meta"]["raw"]
    assert raw_meta == {
        "when": "2024-01-01 00:00:00+00:00",
        "blob": "b'\\x00\\x01'",
    }
    _json.dumps(raw_meta)  # must not raise


# ---------------------------------------------------------------------------
# #238: meta.params_rendered is redacted; the plugin still sees real values.
# ---------------------------------------------------------------------------


def test_tool_runtime_redacts_params_rendered_but_plugin_sees_real_values(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
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
        name="call",
        provider="http",
        params={"url": "https://x.test", "api_key": "{{secret}}"},
    )
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={"secret": "sk-abcdefghijklmnopqrstuvwxyz0123"})

    # The plugin receives the real, unredacted value.
    assert captured_params["api_key"] == "sk-abcdefghijklmnopqrstuvwxyz0123"
    # The stored run record does not.
    assert store.state["call"]["meta"]["params_rendered"]["api_key"] == "***REDACTED***"


def test_tool_runtime_redacts_url_userinfo_in_params_rendered_but_plugin_sees_real_url(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured_params: dict[str, Any] = {}

    def capturing_plugin(**kw: Any) -> Any:
        m = MagicMock()

        def execute(*, params: dict[str, Any], timeout_seconds: int) -> ToolResult:
            captured_params.update(params)
            return ToolResult(value="ok", raw={})

        m.execute.side_effect = execute
        return m

    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", capturing_plugin)

    url = "https://{{creds}}@x.test/"
    defn = ToolDefinition(name="call", provider="http", params={"url": url})
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={"creds": "user:pass"})

    # The plugin receives the real, unredacted URL.
    assert captured_params["url"] == "https://user:pass@x.test/"
    # The stored run record strips the userinfo.
    assert store.state["call"]["meta"]["params_rendered"]["url"] == "https://***REDACTED***@x.test/"


def _capturing_timeout_plugin(captured):
    def fake_build_plugin(**kw):
        m = MagicMock()

        def execute(*, params, timeout_seconds):
            captured["timeout_seconds"] = timeout_seconds
            return ToolResult(value="ok", raw={})

        m.execute.side_effect = execute
        return m

    return fake_build_plugin


class TestToolTimeoutResolution:
    """issue 257: timeout_ms rounds up instead of flooring to 0, and a
    tool's default timeout comes from runtime.tools.timeout_seconds, not
    the run's LLM adapter timeout."""

    def test_sub_second_timeout_ms_rounds_up_not_to_zero(self, monkeypatch):
        captured = {}
        monkeypatch.setattr(
            "circuitry.plugins.factory.build_plugin", _capturing_timeout_plugin(captured)
        )
        defn = ToolDefinition(name="t", provider="shell", params={}, timeout_ms=500)
        ToolRuntime(defn).execute(store=_make_store(), ctx={})
        assert captured["timeout_seconds"] == 1

    def test_timeout_ms_rounds_up_to_next_whole_second(self, monkeypatch):
        captured = {}
        monkeypatch.setattr(
            "circuitry.plugins.factory.build_plugin", _capturing_timeout_plugin(captured)
        )
        defn = ToolDefinition(name="t", provider="shell", params={}, timeout_ms=1500)
        ToolRuntime(defn).execute(store=_make_store(), ctx={})
        assert captured["timeout_seconds"] == 2

    def test_default_timeout_comes_from_runtime_tools_config(self, monkeypatch):
        captured = {}
        monkeypatch.setattr(
            "circuitry.plugins.factory.build_plugin", _capturing_timeout_plugin(captured)
        )
        defn = ToolDefinition(name="t", provider="shell", params={})
        ToolRuntime(defn, runtime_config={"tools": {"timeout_seconds": 900}}).execute(
            store=_make_store(), ctx={}
        )
        assert captured["timeout_seconds"] == 900

    def test_default_timeout_is_independent_of_llm_adapter_timeout(self, monkeypatch):
        """The ctor's timeout_seconds (the LLM adapter's resolved timeout,
        passed down unconditionally by dynamic/loop/conditional) must not
        leak into the tool's own default budget."""
        captured = {}
        monkeypatch.setattr(
            "circuitry.plugins.factory.build_plugin", _capturing_timeout_plugin(captured)
        )
        defn = ToolDefinition(name="t", provider="shell", params={})
        ToolRuntime(defn, timeout_seconds=120).execute(store=_make_store(), ctx={})
        assert captured["timeout_seconds"] == 300

    def test_timeout_ms_overrides_configured_default(self, monkeypatch):
        captured = {}
        monkeypatch.setattr(
            "circuitry.plugins.factory.build_plugin", _capturing_timeout_plugin(captured)
        )
        defn = ToolDefinition(name="t", provider="shell", params={}, timeout_ms=5000)
        ToolRuntime(defn, runtime_config={"tools": {"timeout_seconds": 900}}).execute(
            store=_make_store(), ctx={}
        )
        assert captured["timeout_seconds"] == 5


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


def test_tool_runtime_params_json_builds_array_from_native_list(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A prior step's value can already be a native list in state (e.g. an
    `array` prompt, `json` parse, MCP structuredContent, or a loop item) —
    not just a pre-serialized JSON string."""
    captured_params: dict[str, Any] = {}
    monkeypatch.setattr(
        "circuitry.plugins.factory.build_plugin", _capturing_plugin(captured_params)
    )

    defn = ToolDefinition(
        name="get_equity_quotes",
        provider="mcp",
        params={"server": "robinhood"},
        params_json='{"arguments": {"symbols": {{{prime.symbol_list.value}}} }}',
    )
    store = _make_store()
    ctx = {"prime": {"symbol_list": {"value": ["AAPL", "MSFT", "TSLA"]}}}

    ToolRuntime(defn).execute(store=store, ctx=ctx)

    assert captured_params["arguments"] == {"symbols": ["AAPL", "MSFT", "TSLA"]}


def test_tool_runtime_params_json_builds_object_from_native_dict_loop_item(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A loop `each.as` item is a native dict, not a JSON string."""
    captured_params: dict[str, Any] = {}
    monkeypatch.setattr(
        "circuitry.plugins.factory.build_plugin", _capturing_plugin(captured_params)
    )

    defn = ToolDefinition(
        name="save_person",
        provider="surrealdb",
        params={"mode": "create", "table": "person"},
        params_json='{"data": {{{item}}} }',
    )
    store = _make_store()
    ctx = {"item": {"name": "Ada", "score": 42}}

    ToolRuntime(defn).execute(store=store, ctx=ctx)

    assert captured_params["data"] == {"name": "Ada", "score": 42}


def test_tool_runtime_params_json_builds_empty_array_from_native_list(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An empty list is falsy; chevron must still splice `[]`, not ''."""
    captured_params: dict[str, Any] = {}
    monkeypatch.setattr(
        "circuitry.plugins.factory.build_plugin", _capturing_plugin(captured_params)
    )

    defn = ToolDefinition(
        name="x",
        provider="mcp",
        params={},
        params_json='{"symbols": {{{symbols}}} }',
    )
    store = _make_store()
    ctx: dict[str, Any] = {"symbols": []}

    ToolRuntime(defn).execute(store=store, ctx=ctx)

    assert captured_params["symbols"] == []


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


def test_tool_runtime_rejects_params_json_allowed_commands_override(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """allowed_commands is only honoured from a document's literal params
    block — never params_json, which can carry model-generated content.

    ``subprocess.run`` is monkeypatched to fail the test rather than run
    ``rm -rf /`` for real if this rejection ever regressed."""

    def _fail_if_called(*args: Any, **kwargs: Any) -> Any:
        raise AssertionError("shell plugin must not run when allowed_commands is rejected")

    monkeypatch.setattr(
        "circuitry.plugins._subprocess.subprocess.run", _fail_if_called
    )
    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={"command": "echo"},
        params_json='{"command": "rm", "allowed_commands": ["rm"], "args": ["-rf", "/"]}',
    )
    store = _make_store()

    with pytest.raises(ValueError, match="allowed_commands"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["x"]["value"] is None
    assert "allowed_commands" in store.state["x"]["meta"]["error"]


def test_tool_runtime_rejects_templated_allowed_commands() -> None:
    """A templated allowed_commands entry is not honoured even in the
    literal params block — only a plain, written-down list is."""
    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={"command": "echo", "allowed_commands": ["{{cmd}}"]},
    )
    store = _make_store()

    with pytest.raises(ValueError, match="allowed_commands"):
        ToolRuntime(defn).execute(store=store, ctx={"cmd": "echo"})


def test_tool_runtime_rejects_from_reference_as_whole_allowed_commands_value(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """#234's {from: <path>} must not become a back door around the
    'literal list only' guard on a security-sensitive param (#329 review)."""

    def _fail_if_called(*args: Any, **kwargs: Any) -> Any:
        raise AssertionError("shell plugin must not run when allowed_commands is rejected")

    monkeypatch.setattr("circuitry.plugins._subprocess.subprocess.run", _fail_if_called)
    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={"command": "rm", "allowed_commands": {"from": "prime.attacker.value"}},
    )
    store = _make_store()
    ctx = {"prime": {"attacker": {"value": ["rm"]}}}

    with pytest.raises(ValueError, match="allowed_commands"):
        ToolRuntime(defn).execute(store=store, ctx=ctx)


def test_tool_runtime_rejects_from_reference_as_an_allowed_commands_list_item(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def _fail_if_called(*args: Any, **kwargs: Any) -> Any:
        raise AssertionError("shell plugin must not run when allowed_commands is rejected")

    monkeypatch.setattr("circuitry.plugins._subprocess.subprocess.run", _fail_if_called)
    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={
            "command": "rm",
            "allowed_commands": ["ls", {"from": "prime.attacker.value"}],
        },
    )
    store = _make_store()
    ctx = {"prime": {"attacker": {"value": "rm"}}}

    with pytest.raises(ValueError, match="allowed_commands"):
        ToolRuntime(defn).execute(store=store, ctx=ctx)


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


def test_tool_runtime_params_json_non_object_skip_does_not_raise(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Valid JSON that isn't an object (e.g. a bare array) is still an error."""
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: MagicMock())

    defn = ToolDefinition(
        name="x", provider="mcp", params={}, params_json="[1, 2, 3]", on_error="skip"
    )
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})  # should not raise

    assert store.state["x"]["value"] is None
    assert "JSON object" in store.state["x"]["meta"]["error"]


def test_tool_runtime_params_json_invalid_json_continue_does_not_raise(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: MagicMock())

    defn = ToolDefinition(
        name="x", provider="mcp", params={}, params_json="{not valid json", on_error="continue"
    )
    store = _make_store()

    ToolRuntime(defn).execute(store=store, ctx={})  # should not raise

    assert store.state["x"]["value"] is None
    assert "params_json" in store.state["x"]["meta"]["error"]


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


def test_has_prompt_effects_returns_true_for_model_mode_if_with_no_prompt_effect() -> None:
    """Model mode is the compiler's own default for `if` (#254) — a document
    whose only model use is a model-mode condition still needs a real
    adapter, not the no-op one, even with zero separate `prompt` effects."""
    from circuitry.cli.runtime_shim import _has_prompt_effects

    root = compile_orchestration(
        orch={
            "effects": [
                {"type": "tool", "name": "n", "provider": "json", "params": {"input": "3"}},
                {
                    "type": "if",
                    "name": "big",
                    "if": {"mode": "model", "template": "Is it big?"},
                    "then": [
                        {"type": "tool", "name": "yes", "provider": "json", "params": {"input": "1"}}
                    ],
                },
            ]
        }
    )
    assert _has_prompt_effects(root) is True


def test_has_prompt_effects_returns_true_for_model_mode_while_with_no_prompt_effect() -> None:
    from circuitry.cli.runtime_shim import _has_prompt_effects

    root = compile_orchestration(
        orch={
            "effects": [
                {
                    "type": "loop",
                    "name": "poll",
                    "while": {"mode": "model", "template": "Keep going?"},
                    "body": [
                        {"type": "tool", "name": "t", "provider": "json", "params": {"input": "1"}}
                    ],
                }
            ]
        }
    )
    assert _has_prompt_effects(root) is True


def test_has_prompt_effects_returns_false_for_cel_mode_if() -> None:
    """A CEL-mode condition never calls generate(); the no-op adapter is
    still correct for a document whose only control flow is CEL-based."""
    from circuitry.cli.runtime_shim import _has_prompt_effects

    root = compile_orchestration(
        orch={
            "effects": [
                {
                    "type": "if",
                    "name": "ok",
                    "if": {"mode": "cel", "expr": "true"},
                    "then": [
                        {"type": "tool", "name": "yes", "provider": "json", "params": {"input": "1"}}
                    ],
                }
            ]
        }
    )
    assert _has_prompt_effects(root) is False


def test_has_prompt_effects_returns_true_for_tool_expect_model_mode() -> None:
    """A tool's own expect: {mode: model} calls adapter.generate() the same
    way a model-mode `if` does (#273 review) — a tool-only document with
    one still needs a real adapter, not the no-op."""
    from circuitry.cli.runtime_shim import _has_prompt_effects

    root = compile_orchestration(
        orch={
            "effects": [
                {
                    "type": "tool",
                    "name": "x",
                    "provider": "ffmpeg",
                    "params": {},
                    "expect": {"mode": "model", "template": "Does this look right?"},
                }
            ]
        }
    )
    assert _has_prompt_effects(root) is True


def test_has_prompt_effects_returns_false_for_tool_expect_cel_mode() -> None:
    from circuitry.cli.runtime_shim import _has_prompt_effects

    root = compile_orchestration(
        orch={
            "effects": [
                {
                    "type": "tool",
                    "name": "x",
                    "provider": "ffmpeg",
                    "params": {},
                    "expect": "size(value) > 0",
                }
            ]
        }
    )
    assert _has_prompt_effects(root) is False


def test_has_prompt_effects_sees_a_prompt_that_lives_only_in_finally() -> None:
    """A prompt that appears only in `finally:` still needs a real adapter
    (#272/#273 review, finding 5) — `_has_prompt_effects` walks a dynamic's
    `finally_effects` the same way it walks its body `effects`."""
    from circuitry.cli.runtime_shim import _has_prompt_effects

    root = compile_orchestration(
        orch={
            "effects": [
                {"type": "tool", "name": "x", "provider": "ffmpeg", "params": {}}
            ],
            "finally": [
                {"type": "prompt", "name": "cleanup_note", "template": "done"}
            ],
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
