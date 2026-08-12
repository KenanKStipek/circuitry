"""Tests for the mcp client tool plugin."""

from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from types import SimpleNamespace
from typing import Any

import pytest

from circuitry.plugins import build_plugin
from circuitry.plugins.base import validate_tool_result
from circuitry.plugins.mcp_client import McpPlugin, resolve_transport

_SERVERS: dict[str, Any] = {
    "local": {"command": "some-mcp-server", "args": ["--flag"]},
    "remote": {
        "url": "https://mcp.example.test/mcp",
        "headers": {"Authorization": "Bearer x"},
    },
}


# ---------------------------------------------------------------------------
# Fake session helpers
# ---------------------------------------------------------------------------


def _text_block(text: str) -> SimpleNamespace:
    return SimpleNamespace(type="text", text=text)


def _call_result(
    *,
    text: str | None = None,
    structured: Any = None,
    is_error: bool = False,
) -> SimpleNamespace:
    content = [_text_block(text)] if text is not None else []
    return SimpleNamespace(
        content=content, structuredContent=structured, isError=is_error
    )


class _FakeSession:
    def __init__(
        self,
        *,
        call_result: Any = None,
        tools: list[Any] | None = None,
        delay: float = 0.0,
    ) -> None:
        self.call_result = call_result
        self.tools = tools or []
        self.delay = delay
        self.calls: list[tuple[str, dict[str, Any]]] = []

    async def call_tool(self, name: str, arguments: dict[str, Any]) -> Any:
        self.calls.append((name, arguments))
        if self.delay:
            await asyncio.sleep(self.delay)
        return self.call_result

    async def list_tools(self) -> Any:
        if self.delay:
            await asyncio.sleep(self.delay)
        return SimpleNamespace(tools=self.tools)


def _install_fake_session(
    monkeypatch: pytest.MonkeyPatch, session: _FakeSession
) -> None:
    @asynccontextmanager
    async def fake_open(self: McpPlugin, cfg: dict[str, Any]) -> AsyncIterator[Any]:
        yield session

    monkeypatch.setattr(McpPlugin, "_open_session", fake_open)


# ---------------------------------------------------------------------------
# Transport resolution
# ---------------------------------------------------------------------------


def test_transport_inferred_stdio_from_command() -> None:
    assert resolve_transport({"command": "npx"}) == "stdio"


def test_transport_inferred_http_from_url() -> None:
    assert resolve_transport({"url": "https://x.test/mcp"}) == "http"


def test_transport_explicit_sse() -> None:
    assert resolve_transport({"transport": "sse", "url": "https://x.test"}) == "sse"


def test_transport_streamable_http_alias() -> None:
    cfg = {"transport": "streamable_http", "url": "https://x.test"}
    assert resolve_transport(cfg) == "http"


def test_transport_unknown_raises() -> None:
    with pytest.raises(ValueError, match="Unknown MCP transport"):
        resolve_transport({"transport": "carrier-pigeon", "url": "https://x.test"})


def test_transport_empty_config_raises() -> None:
    with pytest.raises(ValueError, match="'command'.*or 'url'"):
        resolve_transport({})


def test_transport_http_without_url_raises() -> None:
    with pytest.raises(ValueError, match="requires 'url'"):
        resolve_transport({"transport": "http"})


def test_transport_stdio_without_command_raises() -> None:
    with pytest.raises(ValueError, match="requires 'command'"):
        resolve_transport({"transport": "stdio"})


# ---------------------------------------------------------------------------
# call operation
# ---------------------------------------------------------------------------


def test_call_tool_returns_text_value(monkeypatch: pytest.MonkeyPatch) -> None:
    session = _FakeSession(call_result=_call_result(text="hello"))
    _install_fake_session(monkeypatch, session)

    plugin = McpPlugin(servers=_SERVERS)
    result = plugin.execute(
        params={"server": "local", "tool": "greet", "arguments": {"who": "world"}}
    )

    assert validate_tool_result(result, plugin_name="mcp") == []
    assert result.value == "hello"
    assert result.exit_code == 0
    assert result.stderr is None
    assert result.raw["server"] == "local"
    assert result.raw["tool"] == "greet"
    assert result.raw["transport"] == "stdio"
    assert result.raw["is_error"] is False
    assert session.calls == [("greet", {"who": "world"})]


def test_call_tool_prefers_structured_content_in_auto(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    session = _FakeSession(
        call_result=_call_result(text='{"a": 1}', structured={"a": 1})
    )
    _install_fake_session(monkeypatch, session)

    result = McpPlugin(servers=_SERVERS).execute(
        params={"server": "remote", "tool": "fetch"}
    )

    assert result.value == {"a": 1}
    assert result.raw["structured"] == {"a": 1}
    assert result.raw["transport"] == "http"


def test_parse_text_keeps_text_even_with_structured(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    session = _FakeSession(
        call_result=_call_result(text="raw text", structured={"a": 1})
    )
    _install_fake_session(monkeypatch, session)

    result = McpPlugin(servers=_SERVERS).execute(
        params={"server": "local", "tool": "t", "parse": "text"}
    )
    assert result.value == "raw text"


def test_parse_json_parses_text_content(monkeypatch: pytest.MonkeyPatch) -> None:
    session = _FakeSession(call_result=_call_result(text='[1, 2, 3]'))
    _install_fake_session(monkeypatch, session)

    result = McpPlugin(servers=_SERVERS).execute(
        params={"server": "local", "tool": "t", "parse": "json"}
    )
    assert result.value == [1, 2, 3]


def test_parse_json_with_invalid_body_raises(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    session = _FakeSession(call_result=_call_result(text="not-json"))
    _install_fake_session(monkeypatch, session)

    with pytest.raises(RuntimeError, match="parse='json'"):
        McpPlugin(servers=_SERVERS).execute(
            params={"server": "local", "tool": "t", "parse": "json"}
        )


def test_is_error_surfaces_in_stderr_and_exit_code(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Server-side tool errors surface to the caller, not raise — the
    orchestration decides (mirrors the http plugin's 4xx/5xx policy)."""
    session = _FakeSession(call_result=_call_result(text="boom", is_error=True))
    _install_fake_session(monkeypatch, session)

    result = McpPlugin(servers=_SERVERS).execute(
        params={"server": "local", "tool": "t"}
    )

    assert result.exit_code == 1
    assert result.stderr == "boom"
    assert result.value == "boom"
    assert result.raw["is_error"] is True
    assert validate_tool_result(result, plugin_name="mcp") == []


def test_is_error_without_text_gets_fallback_stderr(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    session = _FakeSession(call_result=_call_result(is_error=True))
    _install_fake_session(monkeypatch, session)

    result = McpPlugin(servers=_SERVERS).execute(
        params={"server": "local", "tool": "t"}
    )
    assert result.exit_code == 1
    assert "error" in (result.stderr or "").lower()


def test_multiple_text_blocks_joined(monkeypatch: pytest.MonkeyPatch) -> None:
    call_result = SimpleNamespace(
        content=[_text_block("one"), _text_block("two")],
        structuredContent=None,
        isError=False,
    )
    session = _FakeSession(call_result=call_result)
    _install_fake_session(monkeypatch, session)

    result = McpPlugin(servers=_SERVERS).execute(
        params={"server": "local", "tool": "t"}
    )
    assert result.value == "one\ntwo"


def test_timeout_raises_actionable_runtime_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    session = _FakeSession(call_result=_call_result(text="late"), delay=0.5)
    _install_fake_session(monkeypatch, session)

    with pytest.raises(RuntimeError, match="timed out"):
        McpPlugin(servers=_SERVERS).execute(
            params={"server": "local", "tool": "slow"},
            timeout_seconds=0.05,  # type: ignore[arg-type]
        )


# ---------------------------------------------------------------------------
# list_tools operation
# ---------------------------------------------------------------------------


def test_list_tools_returns_catalog(monkeypatch: pytest.MonkeyPatch) -> None:
    tools = [
        SimpleNamespace(
            name="create_issue",
            description="Create an issue",
            inputSchema={"type": "object"},
        )
    ]
    session = _FakeSession(tools=tools)
    _install_fake_session(monkeypatch, session)

    result = McpPlugin(servers=_SERVERS).execute(
        params={"server": "local", "operation": "list_tools"}
    )

    assert result.value == [
        {
            "name": "create_issue",
            "description": "Create an issue",
            "input_schema": {"type": "object"},
        }
    ]
    assert result.exit_code == 0
    assert result.raw["operation"] == "list_tools"
    assert validate_tool_result(result, plugin_name="mcp") == []


# ---------------------------------------------------------------------------
# Param validation
# ---------------------------------------------------------------------------


def test_missing_server_raises() -> None:
    with pytest.raises(ValueError, match=r"params\['server'\]"):
        McpPlugin(servers=_SERVERS).execute(params={"tool": "t"})


def test_unknown_server_lists_configured() -> None:
    with pytest.raises(ValueError, match="Unknown MCP server.*local.*remote"):
        McpPlugin(servers=_SERVERS).execute(params={"server": "nope", "tool": "t"})


def test_unknown_server_with_empty_config() -> None:
    with pytest.raises(ValueError, match=r"\(none\)"):
        McpPlugin().execute(params={"server": "any", "tool": "t"})


def test_call_without_tool_raises() -> None:
    with pytest.raises(ValueError, match=r"params\['tool'\]"):
        McpPlugin(servers=_SERVERS).execute(params={"server": "local"})


def test_unknown_operation_raises() -> None:
    with pytest.raises(ValueError, match="Unknown MCP operation"):
        McpPlugin(servers=_SERVERS).execute(
            params={"server": "local", "operation": "dance"}
        )


def test_non_dict_arguments_raises() -> None:
    with pytest.raises(ValueError, match=r"params\['arguments'\]"):
        McpPlugin(servers=_SERVERS).execute(
            params={"server": "local", "tool": "t", "arguments": [1, 2]}
        )


def test_unknown_parse_mode_raises() -> None:
    with pytest.raises(ValueError, match="Unknown parse mode"):
        McpPlugin(servers=_SERVERS).execute(
            params={"server": "local", "tool": "t", "parse": "yaml"}
        )


# ---------------------------------------------------------------------------
# Factory + check
# ---------------------------------------------------------------------------


def test_factory_builds_mcp_plugin_with_servers() -> None:
    runtime = {"plugins": {"mcp": {"servers": _SERVERS}}}
    plugin = build_plugin(plugin_name="mcp", runtime=runtime)
    assert plugin.name == "mcp"
    assert isinstance(plugin, McpPlugin)
    assert plugin.servers == _SERVERS


def test_factory_defaults_to_no_servers() -> None:
    plugin = build_plugin(plugin_name="mcp", runtime={})
    assert isinstance(plugin, McpPlugin)
    assert plugin.servers == {}


def test_check_ok_with_no_servers_mentions_config_path() -> None:
    r = McpPlugin().check()
    assert r.ok is True
    assert "runtime.plugins.mcp.servers" in (r.message or "")


def test_check_reports_missing_stdio_binary(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        "circuitry.plugins.mcp_client.shutil.which", lambda _cmd: None
    )
    r = McpPlugin(servers=_SERVERS).check()
    assert r.ok is False
    assert "binary:some-mcp-server" in r.missing


def test_check_ok_when_stdio_binary_present(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        "circuitry.plugins.mcp_client.shutil.which",
        lambda _cmd: "/usr/bin/some-mcp-server",
    )
    r = McpPlugin(servers=_SERVERS).check()
    assert r.ok is True
    assert r.missing == []


def test_check_flags_invalid_server_config() -> None:
    r = McpPlugin(servers={"bad": {}}).check()
    assert r.ok is False
    assert "bad" in (r.message or "")
