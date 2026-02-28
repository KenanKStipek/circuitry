from __future__ import annotations

import json
from pathlib import Path

from circuitry.service import RestTriggerService


def _write_orchestration(path: Path) -> None:
    path.write_text(
        """
name: hello_root
adapter: openai
model: gpt-4o-mini
effects:
  - type: prompt
    name: hello
    template: "hi"
""".strip()
        + "\n",
        encoding="utf-8",
    )


def test_rest_trigger_success_returns_request_tracking_metadata(tmp_path: Path) -> None:
    orch_path = tmp_path / "hello.yml"
    _write_orchestration(orch_path)

    svc = RestTriggerService(auth_token="secret")
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={
            "Authorization": "Bearer secret",
            "X-Request-ID": "req-123",
        },
        body=json.dumps(
            {
                "orchestration_path": str(orch_path),
                "dry_run": True,
                "state": {"input": {"name": "Elena"}},
            }
        ),
    )

    assert response.status_code == 200
    assert response.headers["x-request-id"] == "req-123"
    assert response.body["ok"] is True
    assert response.body["status"] == "succeeded"
    assert response.body["request_id"] == "req-123"

    trigger = response.body["runtime"]["trigger"]
    assert trigger["request_id"] == "req-123"
    assert trigger["interface"] == "rest"
    assert trigger["status"] == "succeeded"
    assert trigger["completed_at"] is not None

    last_run = response.body["runtime"]["last_run"]
    assert isinstance(last_run, dict)
    assert last_run["orchestration_path"] == str(orch_path)


def test_rest_trigger_rejects_missing_or_invalid_token() -> None:
    svc = RestTriggerService(auth_token="secret")
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"orchestration_path": "orchestrations/dynamic_hello.yml"}),
    )

    assert response.status_code == 401
    assert response.body["ok"] is False
    assert "Unauthorized" in response.body["error"]


def test_rest_trigger_rejects_invalid_payload_shape() -> None:
    svc = RestTriggerService(auth_token=None)
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"dry_run": True}),
    )

    assert response.status_code == 400
    assert response.body["ok"] is False
    assert "orchestration_path" in response.body["error"]


def test_rest_trigger_returns_runtime_failure_details(tmp_path: Path) -> None:
    svc = RestTriggerService(auth_token=None)
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={"X-Request-ID": "req-fail"},
        body=json.dumps(
            {
                "orchestration_path": str(tmp_path / "missing.yml"),
                "dry_run": True,
            }
        ),
    )

    assert response.status_code == 500
    assert response.body["ok"] is False
    assert response.body["status"] == "failed"
    assert response.body["request_id"] == "req-fail"
    assert response.body["error"]

    trigger = response.body["runtime"]["trigger"]
    assert trigger["status"] == "failed"
    assert trigger["error"] == response.body["error"]
