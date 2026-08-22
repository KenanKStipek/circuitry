"""
circuitry-mcp — Model Context Protocol server for circuitry orchestrations.

Exposes tools that let an MCP-capable host (e.g. Claude Code, Claude Desktop)
discover, validate, and drive circuitry orchestrations through a tool-loop:

  list_orchestrations  -> [{name, path, description}, ...]
  validate_orchestration(path)        -> {ok, errors, warnings}
  run_orchestration(orchestration, initial_state?, override_model?, override_to?)
                                      -> {run_id, status, pending_prompts, state, error}
  submit_response(run_id, prompt_id, response)
                                      -> same shape (one branch unblocked)
  get_run_state(run_id)               -> same shape with full state
  cancel_run(run_id)                  -> same shape with status='cancelled'

Run lifecycle is process-local: in-flight runs are lost when the server exits.
Final state for completed runs is still persisted via circuitry's existing
SQLite/Postgres persistence layer (configured per-orchestration).
"""

from __future__ import annotations

import logging
import sys
from datetime import datetime
from pathlib import Path
from typing import Any

from ..cli.app import _resolve_orchestration
from ..cli.registry import load_index
from ..cli.runtime_shim import validate as validate_orch_path
from .runs import Run, RunManager

logger = logging.getLogger(__name__)


_manager = RunManager()


# --------------------------------------------------------------------- helpers
def _to_json_safe(value: Any) -> Any:
    """Coerce non-JSON-serializable types (Path, datetime) into strings."""
    if isinstance(value, dict):
        return {str(k): _to_json_safe(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_to_json_safe(v) for v in value]
    if isinstance(value, (Path, datetime)):
        return str(value)
    if isinstance(value, (str, int, float, bool)) or value is None:
        return value
    # Fallback: stringify anything else (UUIDs, Enums w/o __str__, etc.)
    try:
        return str(value)
    except Exception:
        return None


def _run_response(run: Run, *, include_state: bool = False) -> dict[str, Any]:
    """Serialize a Run into the MCP wire shape."""
    with run._lock:
        status = run.status
        pending = [
            {
                "prompt_id": pp.prompt_id,
                "prompt": pp.prompt,
                "model": pp.model,
                "requested_at": pp.requested_at.isoformat(),
            }
            for pp in run.pending_prompts.values()
        ]
        snapshot = run.state if include_state or status.is_terminal else None
        error = run.error

    payload: dict[str, Any] = {
        "run_id": run.run_id,
        "status": status.value,
        "pending_prompts": pending,
        "state": _to_json_safe(snapshot) if snapshot is not None else None,
        "error": error,
    }
    return payload


def _error_response(message: str, *, run_id: str | None = None) -> dict[str, Any]:
    """Structured error response for tool calls. Never raises into the SDK."""
    return {
        "ok": False,
        "error": message,
        "run_id": run_id,
    }


# ------------------------------------------------------------------- tool impls
def _list_orchestrations_impl() -> list[dict[str, Any]]:
    entries = load_index()
    return [
        {
            "name": e.get("name") or "",
            "file": e.get("file") or "",
            "description": e.get("description") or "",
            "category": e.get("category") or "",
        }
        for e in entries
    ]


def _validate_orchestration_impl(path: str) -> dict[str, Any]:
    candidate = _resolve_orchestration(path)
    if candidate is None:
        return {"ok": False, "errors": [f"Orchestration not found: {path}"]}
    result = validate_orch_path(candidate)
    return {
        "ok": bool(result.get("ok")),
        "errors": list(result.get("errors") or []),
        "warnings": list(result.get("warnings") or []),
    }


def _run_orchestration_impl(
    *,
    orchestration: str,
    initial_state: dict[str, Any] | None = None,
    override_model: bool = False,
    override_to: str = "",
) -> dict[str, Any]:
    candidate = _resolve_orchestration(orchestration)
    if candidate is None:
        return _error_response(f"Orchestration not found: {orchestration}")
    try:
        run = _manager.start_run(
            orchestration_path=candidate,
            initial_state=initial_state,
            override_model=override_model,
            override_to=override_to,
        )
    except Exception as exc:
        logger.exception("Failed to start run")
        return _error_response(f"Failed to start run: {exc}")
    return _run_response(run)


def _submit_response_impl(
    *, run_id: str, prompt_id: str, response: str
) -> dict[str, Any]:
    try:
        run = _manager.submit_response(
            run_id=run_id, prompt_id=prompt_id, response_text=response
        )
    except KeyError as exc:
        return _error_response(str(exc).strip("'"), run_id=run_id)
    except Exception as exc:
        logger.exception("submit_response failed")
        return _error_response(f"submit_response failed: {exc}", run_id=run_id)
    return _run_response(run)


def _get_run_state_impl(*, run_id: str) -> dict[str, Any]:
    try:
        run = _manager.get_run(run_id)
    except KeyError as exc:
        return _error_response(str(exc).strip("'"), run_id=run_id)
    return _run_response(run, include_state=True)


def _cancel_run_impl(*, run_id: str) -> dict[str, Any]:
    try:
        run = _manager.cancel_run(run_id)
    except KeyError as exc:
        return _error_response(str(exc).strip("'"), run_id=run_id)
    return _run_response(run, include_state=True)


# ----------------------------------------------------------------- server wire
def _build_server() -> Any:
    """
    Construct and return the MCP server with tools registered.

    Lazy-imported so `from circuitry.mcp.server import ...` works in test
    environments without an MCP host attached.

    ``MCPServer`` is the mcp 2.0 name for what 1.x called ``FastMCP``
    (``mcp.server.fastmcp``); the decorator/run surface we use is unchanged.
    """
    from mcp.server import MCPServer

    server = MCPServer(
        name="circuitry-mcp",
        instructions=(
            "Drive circuitry orchestrations from a Claude conversation. "
            "Call list_orchestrations to discover, run_orchestration to "
            "start, then loop submit_response(run_id, prompt_id, ...) for "
            "each entry in pending_prompts (sequential or parallel), until "
            "status is completed/failed/cancelled. See `/cof` for details."
        ),
    )

    @server.tool()
    def list_orchestrations() -> list[dict[str, Any]]:
        """List discoverable bundled orchestrations."""
        return _list_orchestrations_impl()

    @server.tool()
    def validate_orchestration(path: str) -> dict[str, Any]:
        """Validate an orchestration YAML against the schema and compiler."""
        return _validate_orchestration_impl(path)

    @server.tool()
    def run_orchestration(
        orchestration: str,
        initial_state: dict[str, Any] | None = None,
        override_model: bool = False,
        override_to: str = "",
    ) -> dict[str, Any]:
        """
        Start an orchestration. Returns a run snapshot. If the orchestration
        is paused awaiting input, `pending_prompts` lists per-branch prompts
        the host should respond to via `submit_response`.

        override_model: if True, ignore the orchestration's pinned `model:`
        and run all prompts through Claude regardless. Useful for testing
        an orchestration end-to-end through the host before deploying it
        with its real backend.
        override_to: when override_model is True, the Claude model name to
        record in raw["model"] (empty = "let host pick").
        """
        return _run_orchestration_impl(
            orchestration=orchestration,
            initial_state=initial_state,
            override_model=override_model,
            override_to=override_to,
        )

    @server.tool()
    def submit_response(
        run_id: str, prompt_id: str, response: str
    ) -> dict[str, Any]:
        """Unblock the worker waiting on `prompt_id` with the host's response text."""
        return _submit_response_impl(
            run_id=run_id, prompt_id=prompt_id, response=response
        )

    @server.tool()
    def get_run_state(run_id: str) -> dict[str, Any]:
        """Inspect the current state of a run (full state included)."""
        return _get_run_state_impl(run_id=run_id)

    @server.tool()
    def cancel_run(run_id: str) -> dict[str, Any]:
        """Cancel a run; wakes all blocked branches and joins the worker thread."""
        return _cancel_run_impl(run_id=run_id)

    return server


def main() -> None:
    """stdio entrypoint wired to the `circuitry-mcp` console script."""
    # MCP framing uses stdout — keep all logs on stderr.
    logging.basicConfig(
        stream=sys.stderr,
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    server = _build_server()
    server.run("stdio")


if __name__ == "__main__":
    main()
