from __future__ import annotations

import json
from collections.abc import Mapping
from copy import deepcopy
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from uuid import uuid4

from ..cli.config import CircuitryConfig, resolve_config
from ..cli.runtime_shim import RunRequest, run
from ..core.state_ns import migrate_legacy_state


@dataclass(frozen=True)
class RestResponse:
    status_code: int
    body: dict[str, Any]
    headers: dict[str, str]


class RestTriggerService:
    """Minimal REST trigger interface for orchestration execution.

    *config*, when omitted, is never a bare, allowlist-open
    ``CircuitryConfig()``: it is resolved the same way ``cof run`` resolves
    one for a document under *orchestration_root* — global config, then a
    project config discovered under *orchestration_root* if trusted (#284's
    trust rules; an untrusted discovered project config is skipped, same as
    the CLI), then environment variables. This makes a host's
    ``runtime.plugins.shell.allowed_commands`` pin (and every other host
    setting: adapters, enabled_tools/adapters/plugins, persistence) apply to
    REST runs, and ensures preflight — gated on a non-``None`` config —
    always runs for a non-dry-run REST request. An embedder that passes an
    explicit *config* has that win outright, with no resolution.
    """

    def __init__(
        self,
        *,
        auth_token: str | None = None,
        allow_unauthenticated: bool = False,
        config: CircuitryConfig | None = None,
        orchestration_root: Path | str | None = None,
    ) -> None:
        if not auth_token and not allow_unauthenticated:
            raise ValueError(
                "RestTriggerService requires auth_token, or "
                "allow_unauthenticated=True to explicitly opt into running "
                "without one."
            )
        self._auth_token = auth_token
        self._orchestration_root = (
            Path(orchestration_root) if orchestration_root is not None else Path.cwd()
        ).resolve()
        self._config = (
            config if config is not None else resolve_config(cwd=self._orchestration_root)
        )

    def handle_http_request(
        self,
        *,
        method: str,
        path: str,
        headers: Mapping[str, str],
        body: bytes | str,
    ) -> RestResponse:
        request_id = self._request_id_from_headers(headers)

        if method.upper() != "POST" or path != "/v1/triggers/run":
            return self._response(
                status_code=404,
                request_id=request_id,
                body={"ok": False, "error": "Not found."},
            )

        auth_error = self._validate_auth(headers)
        if auth_error is not None:
            return self._response(
                status_code=401,
                request_id=request_id,
                body={"ok": False, "error": auth_error},
            )

        payload_or_error = self._parse_payload(body)
        if isinstance(payload_or_error, str):
            return self._response(
                status_code=400,
                request_id=request_id,
                body={"ok": False, "error": payload_or_error},
            )
        payload = payload_or_error

        validation_error = self._validate_payload(payload)
        if validation_error is not None:
            return self._response(
                status_code=400,
                request_id=request_id,
                body={"ok": False, "error": validation_error},
            )

        initial_state = self._state_with_trigger_metadata(
            payload=payload, request_id=request_id, path=path
        )

        req = RunRequest(
            orchestration_path=self._resolve_under_root(payload["orchestration_path"]),
            state_path=None,
            out_path=(
                self._resolve_under_root(payload["out_path"])
                if payload.get("out_path")
                else None
            ),
            dry_run=bool(payload.get("dry_run", False)),
            validate_only=bool(payload.get("validate_only", False)),
            initial_state=initial_state,
            verbose=bool(payload.get("verbose", False)),
            config=self._config,
        )

        result = run(req)
        runtime = result.state.setdefault("runtime", {})
        trigger = runtime.setdefault("trigger", {})
        trigger.update(
            {
                "interface": "rest",
                "request_id": request_id,
                "status": "succeeded" if result.ok else "failed",
                "completed_at": _now_iso(),
                "error": result.error,
            }
        )

        if result.ok:
            return self._response(
                status_code=200,
                request_id=request_id,
                body={
                    "ok": True,
                    "status": "succeeded",
                    "runtime": {
                        "trigger": trigger,
                        "last_run": runtime.get("last_run"),
                    },
                },
            )

        return self._response(
            status_code=500,
            request_id=request_id,
            body={
                "ok": False,
                "status": "failed",
                "error": result.error,
                "runtime": {
                    "trigger": trigger,
                    "last_run": runtime.get("last_run"),
                },
            },
        )

    def _validate_auth(self, headers: Mapping[str, str]) -> str | None:
        if not self._auth_token:
            return None

        header_value = _header_value(headers, "authorization")
        expected = f"Bearer {self._auth_token}"
        if header_value != expected:
            return "Unauthorized: bearer token is missing or invalid."
        return None

    def _parse_payload(self, body: bytes | str) -> dict[str, Any] | str:
        raw = body.decode("utf-8") if isinstance(body, bytes) else body
        if not raw.strip():
            return "Request body must be a JSON object."

        try:
            decoded = json.loads(raw)
        except json.JSONDecodeError:
            return "Request body must be valid JSON."

        if not isinstance(decoded, dict):
            return "Request body must be a JSON object."

        return decoded

    def _validate_payload(self, payload: dict[str, Any]) -> str | None:
        orch_path = payload.get("orchestration_path")
        if not isinstance(orch_path, str) or not orch_path.strip():
            return "Field 'orchestration_path' is required and must be a non-empty string."
        if not self._path_confined(orch_path):
            return (
                "Field 'orchestration_path' must resolve inside the service's "
                f"orchestration root ({self._orchestration_root})."
            )

        if "state" in payload and not isinstance(payload["state"], dict):
            return "Field 'state' must be a JSON object when provided."

        for bool_field in ("dry_run", "validate_only", "verbose"):
            if bool_field in payload and not isinstance(payload[bool_field], bool):
                return f"Field '{bool_field}' must be a boolean when provided."

        if "out_path" in payload:
            out_path = payload["out_path"]
            if not isinstance(out_path, str):
                return "Field 'out_path' must be a string when provided."
            if not self._path_confined(out_path):
                return (
                    "Field 'out_path' must resolve inside the service's "
                    f"orchestration root ({self._orchestration_root})."
                )

        return None

    def _resolve_under_root(self, raw: str) -> Path:
        """Resolve *raw* against ``orchestration_root`` when relative; absolute
        paths are resolved as given. Either way, confinement is checked
        separately via :meth:`_is_confined`."""
        candidate = Path(raw)
        if candidate.is_absolute():
            return candidate.resolve()
        return (self._orchestration_root / candidate).resolve()

    def _path_confined(self, raw: str) -> bool:
        """Like ``_is_confined(_resolve_under_root(raw))``, but a malformed
        path (e.g. one with an embedded NUL byte) is treated as unconfined
        rather than raising out of validation."""
        try:
            resolved = self._resolve_under_root(raw)
        except (OSError, ValueError):
            return False
        return self._is_confined(resolved)

    def _is_confined(self, resolved: Path) -> bool:
        return resolved.is_relative_to(self._orchestration_root)

    def _state_with_trigger_metadata(
        self, *, payload: dict[str, Any], request_id: str, path: str
    ) -> dict[str, Any]:
        # The caller's `state` payload is caller input: bare root keys are
        # wrapped under the `input` namespace before the run sees them.
        initial_state = migrate_legacy_state(deepcopy(payload.get("state", {})))
        runtime = initial_state.setdefault("runtime", {})
        runtime["trigger"] = {
            "interface": "rest",
            "request_id": request_id,
            "path": path,
            "received_at": _now_iso(),
            "status": "started",
            "error": None,
            "completed_at": None,
        }
        return initial_state

    def _response(
        self, *, status_code: int, request_id: str, body: dict[str, Any]
    ) -> RestResponse:
        envelope = {"request_id": request_id, **body}
        return RestResponse(
            status_code=status_code,
            body=envelope,
            headers={
                "content-type": "application/json",
                "x-request-id": request_id,
            },
        )

    def _request_id_from_headers(self, headers: Mapping[str, str]) -> str:
        header_request_id = _header_value(headers, "x-request-id")
        if header_request_id:
            return header_request_id
        return str(uuid4())


def _header_value(headers: Mapping[str, str], name: str) -> str | None:
    for key, value in headers.items():
        if key.lower() == name.lower() and isinstance(value, str):
            return value
    return None


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()
