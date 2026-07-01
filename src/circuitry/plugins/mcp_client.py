"""MCP client tool plugin — call tools on external MCP servers.

Connects to Model Context Protocol servers declared in runtime config and
invokes their tools from orchestrations. This is the client-side complement
to ``circuitry-mcp`` (which exposes circuitry itself AS an MCP server).

Server connections are declared under ``runtime.plugins.mcp.servers`` —
like adapters they are named, referenced from YAML by name only, and
credential-bearing fields (``env`` values, ``Authorization`` headers) are
redacted by the existing serialization redaction before any state artifact
is written to disk::

    {
      "runtime": {
        "plugins": {
          "mcp": {
            "servers": {
              "github": {
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-github"],
                "env": {"GITHUB_PERSONAL_ACCESS_TOKEN": "..."}
              },
              "internal": {
                "url": "https://mcp.example.com/mcp",
                "headers": {"Authorization": "Bearer ..."}
              }
            }
          }
        }
      }
    }

Transport is inferred from the config shape: ``command`` → stdio
(subprocess), ``url`` → streamable HTTP. An explicit ``transport`` field
(``stdio`` | ``http`` | ``sse``) is also accepted for the ambiguous cases.

Params:
  - ``server`` (required): name of a configured server.
  - ``tool`` (required unless ``operation: list_tools``): tool to invoke.
  - ``arguments`` (optional): mapping of tool arguments. String values
    support Mustache rendering like every tool param.
  - ``operation`` (optional, ``call`` | ``list_tools``, default ``call``):
    ``list_tools`` returns the server's tool catalog so orchestrations can
    discover capabilities at runtime (e.g. feed them to a reflector).
  - ``parse`` (optional, ``auto`` | ``json`` | ``text``, default ``auto``):
    ``auto`` prefers the server's ``structuredContent`` when present, else
    returns the joined text content. ``json`` force-parses the text content.

ToolResult shape:
  - ``value``: structured content, parsed JSON, or text per ``parse``; for
    ``list_tools`` a list of ``{name, description, input_schema}``.
  - ``raw``: ``{server, tool, transport, operation, is_error, content,
    structured}``.
  - ``stdout``: ``None``. ``stderr``: the error text when the server flags
    ``isError`` (surfaced rather than raised — let the orchestration decide,
    mirroring the http plugin's 4xx/5xx policy).
  - ``exit_code``: 0 on success, 1 when the server flags ``isError``.

Lifecycle: one fresh connection per call (subprocess spawn for stdio).
Deliberately simple — it is correct and thread-safe under tree flow, where
effects execute on a ThreadPoolExecutor. A per-run session cache is a
future optimization if stdio spawn overhead matters.
"""

from __future__ import annotations

import asyncio
import importlib.util
import json as _json
import shutil
from contextlib import asynccontextmanager
from dataclasses import dataclass, field
from typing import Any, AsyncIterator, Coroutine

from ..preflight import CheckResult
from .base import ToolResult

_TRANSPORTS = ("stdio", "http", "sse")


def resolve_transport(cfg: dict[str, Any]) -> str:
    """Return the transport for a server config, validating its shape.

    ``transport`` may be set explicitly (``stdio`` | ``http`` | ``sse``;
    ``streamable_http`` is accepted as an alias of ``http``). Otherwise it
    is inferred: ``command`` → stdio, ``url`` → http.
    """
    explicit = cfg.get("transport")
    if explicit is not None:
        transport = str(explicit).strip().lower()
        if transport == "streamable_http":
            transport = "http"
        if transport not in _TRANSPORTS:
            raise ValueError(
                f"Unknown MCP transport: {explicit!r}. "
                f"Supported: {', '.join(_TRANSPORTS)}."
            )
    elif cfg.get("command"):
        transport = "stdio"
    elif cfg.get("url"):
        transport = "http"
    else:
        raise ValueError(
            "MCP server config requires 'command' (stdio subprocess) "
            "or 'url' (streamable HTTP)."
        )

    if transport == "stdio" and not cfg.get("command"):
        raise ValueError("MCP transport 'stdio' requires 'command'.")
    if transport in ("http", "sse") and not cfg.get("url"):
        raise ValueError(f"MCP transport {transport!r} requires 'url'.")
    return transport


def _text_of(block: Any) -> str | None:
    if getattr(block, "type", None) == "text":
        text = getattr(block, "text", None)
        if isinstance(text, str):
            return text
    return None


def _dump_block(block: Any) -> dict[str, Any]:
    dump = getattr(block, "model_dump", None)
    if callable(dump):
        try:
            result = dump(mode="json")
            if isinstance(result, dict):
                return result
        except Exception:  # noqa: BLE001 — raw dump is best-effort metadata
            pass
    return {"type": getattr(block, "type", None), "text": getattr(block, "text", None)}


def _describe_tool(tool: Any) -> dict[str, Any]:
    return {
        "name": getattr(tool, "name", None),
        "description": getattr(tool, "description", None),
        "input_schema": getattr(tool, "inputSchema", None),
    }


def _run_coroutine(coro: Coroutine[Any, Any, Any]) -> Any:
    """Run a coroutine to completion from sync code.

    ``asyncio.run`` per invocation keeps the plugin thread-safe under tree
    flow (each worker thread gets its own event loop). If a loop is already
    running in this thread (embedding scenarios), fall back to a dedicated
    thread so we never call ``asyncio.run`` inside a running loop.
    """
    try:
        asyncio.get_running_loop()
    except RuntimeError:
        return asyncio.run(coro)
    from concurrent.futures import ThreadPoolExecutor

    with ThreadPoolExecutor(max_workers=1) as pool:
        return pool.submit(asyncio.run, coro).result()


@dataclass(frozen=True)
class McpPlugin:
    servers: dict[str, Any] = field(default_factory=dict)
    name: str = "mcp"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        server_name = params.get("server")
        if not isinstance(server_name, str) or not server_name.strip():
            raise ValueError(
                "McpPlugin requires params['server'] as a non-empty string "
                "naming a server under runtime.plugins.mcp.servers."
            )
        server_name = server_name.strip()

        cfg = self.servers.get(server_name)
        if not isinstance(cfg, dict):
            configured = ", ".join(sorted(self.servers)) or "(none)"
            raise ValueError(
                f"Unknown MCP server: {server_name!r}. Configured servers: "
                f"{configured}. Declare servers under runtime.plugins.mcp.servers "
                "in config.json."
            )
        transport = resolve_transport(cfg)

        operation = str(params.get("operation", "call")).strip().lower()
        if operation not in ("call", "list_tools"):
            raise ValueError(
                f"Unknown MCP operation: {operation!r}. "
                "Supported: call, list_tools."
            )

        tool_name = params.get("tool")
        if operation == "call":
            if not isinstance(tool_name, str) or not tool_name.strip():
                raise ValueError(
                    "McpPlugin requires params['tool'] as a non-empty string "
                    "(or set operation: list_tools to discover tools)."
                )
            tool_name = tool_name.strip()

        arguments = params.get("arguments") or {}
        if not isinstance(arguments, dict):
            raise ValueError("McpPlugin params['arguments'] must be an object.")

        parse = str(params.get("parse", "auto")).strip().lower()
        if parse not in ("auto", "json", "text"):
            raise ValueError(
                f"Unknown parse mode: {parse!r}. Supported: auto, json, text."
            )

        async def _go() -> dict[str, Any]:
            async with self._open_session(cfg) as session:
                if operation == "list_tools":
                    listed = await session.list_tools()
                    return {"kind": "list", "tools": list(listed.tools)}
                called = await session.call_tool(tool_name, arguments)
                return {"kind": "call", "result": called}

        async def _go_timed() -> dict[str, Any]:
            return await asyncio.wait_for(_go(), timeout=float(timeout_seconds))

        try:
            outcome = _run_coroutine(_go_timed())
        except asyncio.TimeoutError as exc:
            raise RuntimeError(
                f"MCP {operation} on server {server_name!r} timed out "
                f"after {timeout_seconds}s."
            ) from exc

        if outcome["kind"] == "list":
            tools_payload = [_describe_tool(t) for t in outcome["tools"]]
            return ToolResult(
                value=tools_payload,
                raw={
                    "server": server_name,
                    "tool": None,
                    "transport": transport,
                    "operation": "list_tools",
                    "is_error": False,
                    "content": None,
                    "structured": None,
                },
                stdout=None,
                stderr=None,
                exit_code=0,
            )

        result = outcome["result"]
        content = list(getattr(result, "content", None) or [])
        structured = getattr(result, "structuredContent", None)
        is_error = bool(getattr(result, "isError", False))
        texts = [t for t in (_text_of(c) for c in content) if t is not None]
        text = "\n".join(texts)

        value: Any
        if parse == "json":
            try:
                value = _json.loads(text) if text else None
            except _json.JSONDecodeError as exc:
                raise RuntimeError(
                    f"McpPlugin: parse='json' but tool {tool_name!r} returned "
                    f"non-JSON text: {exc}"
                ) from exc
        elif parse == "text":
            value = text
        else:  # auto
            value = structured if structured is not None else text

        return ToolResult(
            value=value,
            raw={
                "server": server_name,
                "tool": tool_name,
                "transport": transport,
                "operation": "call",
                "is_error": is_error,
                "content": [_dump_block(c) for c in content],
                "structured": structured,
            },
            stdout=None,
            stderr=(text or "MCP tool reported an error.") if is_error else None,
            exit_code=1 if is_error else 0,
        )

    @asynccontextmanager
    async def _open_session(self, cfg: dict[str, Any]) -> AsyncIterator[Any]:
        """Open a transport + initialized ClientSession for a server config.

        Isolated as the single SDK seam so tests can substitute a fake
        session without touching subprocesses or sockets.
        """
        from mcp import ClientSession

        transport = resolve_transport(cfg)
        if transport == "stdio":
            from mcp import StdioServerParameters
            from mcp.client.stdio import stdio_client

            env_cfg = cfg.get("env") or None
            env: dict[str, str] | None = None
            if env_cfg is not None:
                overrides = {str(k): str(v) for k, v in dict(env_cfg).items()}
                try:
                    from mcp.client.stdio import get_default_environment

                    env = {**get_default_environment(), **overrides}
                except ImportError:
                    env = overrides
            kwargs: dict[str, Any] = {
                "command": str(cfg["command"]),
                "args": [str(a) for a in (cfg.get("args") or [])],
                "env": env,
            }
            if cfg.get("cwd"):
                kwargs["cwd"] = str(cfg["cwd"])
            server_params = StdioServerParameters(**kwargs)
            async with stdio_client(server_params) as (read, write):
                async with ClientSession(read, write) as session:
                    await session.initialize()
                    yield session
        elif transport == "sse":
            from mcp.client.sse import sse_client

            async with sse_client(
                str(cfg["url"]), headers=cfg.get("headers") or None
            ) as (read, write):
                async with ClientSession(read, write) as session:
                    await session.initialize()
                    yield session
        else:  # http (streamable)
            from mcp.client.streamable_http import streamablehttp_client

            async with streamablehttp_client(
                str(cfg["url"]), headers=cfg.get("headers") or None
            ) as (read, write, _get_session_id):
                async with ClientSession(read, write) as session:
                    await session.initialize()
                    yield session

    def check(self) -> CheckResult:
        missing: list[str] = []
        problems: list[str] = []

        if importlib.util.find_spec("mcp") is None:
            missing.append("library:mcp")

        for name in sorted(self.servers):
            server_cfg = self.servers.get(name)
            if not isinstance(server_cfg, dict):
                problems.append(f"server {name!r}: config must be an object")
                continue
            try:
                transport = resolve_transport(server_cfg)
            except ValueError as exc:
                problems.append(f"server {name!r}: {exc}")
                continue
            if transport == "stdio":
                command = str(server_cfg.get("command") or "")
                if command and shutil.which(command) is None:
                    missing.append(f"binary:{command}")

        if missing or problems:
            return CheckResult(
                ok=False,
                missing=sorted(set(missing)),
                message="; ".join(problems) or None,
            )
        if not self.servers:
            return CheckResult(
                ok=True,
                missing=[],
                message=(
                    "no MCP servers configured; declare them under "
                    "runtime.plugins.mcp.servers in config.json."
                ),
            )
        return CheckResult(ok=True, missing=[])
