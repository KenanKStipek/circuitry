from __future__ import annotations

import json
from copy import deepcopy
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Mapping
from uuid import uuid4

from ..cli.config import CircuitryConfig
from ..cli.runtime_shim import RunRequest, run


@dataclass(frozen=True)
class RestResponse:
    status_code: int
    body: dict[str, Any]
    headers: dict[str, str]


class RestTriggerService:
    """Minimal REST trigger interface for orchestration execution."""

    def __init__(
        self,
        *,
        auth_token: str | None,
        config: CircuitryConfig | None = None,
    ) -> None:
        self._auth_token = auth_token
        self._config = config

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
            orchestration_path=Path(payload["orchestration_path"]),
            state_path=None,
            out_path=Path(payload["out_path"]) if payload.get("out_path") else None,
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

        if "state" in payload and not isinstance(payload["state"], dict):
            return "Field 'state' must be a JSON object when provided."

        for bool_field in ("dry_run", "validate_only", "verbose"):
            if bool_field in payload and not isinstance(payload[bool_field], bool):
                return f"Field '{bool_field}' must be a boolean when provided."

        if "out_path" in payload and not isinstance(payload["out_path"], str):
            return "Field 'out_path' must be a string when provided."

        return None

    def _state_with_trigger_metadata(
        self, *, payload: dict[str, Any], request_id: str, path: str
    ) -> dict[str, Any]:
        initial_state = deepcopy(payload.get("state", {}))
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
