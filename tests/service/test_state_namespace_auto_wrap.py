"""REST and scheduler auto-wrap the caller's `state` payload/seed under the
`input` namespace before a run ever dispatches — issue #86 stage 1's
trigger-interface half of the contract (rest.py / scheduler.py both call
core.state_ns.migrate_legacy_state on the caller-supplied dict).
"""

from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path

import pytest

from circuitry.cli.runtime_shim import RunRequest, RunResult
from circuitry.service import RecurringScheduler, ScheduledJob
from circuitry.service.rest import RestTriggerService


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


# ── REST ──────────────────────────────────────────────────────────────────


def test_rest_state_with_trigger_metadata_wraps_bare_root_keys() -> None:
    svc = RestTriggerService(auth_token=None)

    initial_state = svc._state_with_trigger_metadata(
        payload={"state": {"name": "Elena"}},
        request_id="req-1",
        path="/v1/triggers/run",
    )

    assert initial_state["input"] == {"name": "Elena"}
    assert "name" not in initial_state
    assert initial_state["runtime"]["trigger"]["interface"] == "rest"


def test_rest_state_with_trigger_metadata_leaves_namespaced_payload_alone() -> None:
    svc = RestTriggerService(auth_token=None)

    initial_state = svc._state_with_trigger_metadata(
        payload={"state": {"input": {"name": "Elena"}}},
        request_id="req-1",
        path="/v1/triggers/run",
    )

    assert initial_state["input"] == {"name": "Elena"}


def test_rest_trigger_end_to_end_with_bare_state_payload(tmp_path: Path) -> None:
    orch_path = tmp_path / "hello.yml"
    _write_orchestration(orch_path)

    svc = RestTriggerService(auth_token=None)
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={"X-Request-ID": "req-bare"},
        body=json.dumps(
            {
                "orchestration_path": str(orch_path),
                "dry_run": True,
                "state": {"name": "Elena"},
            }
        ),
    )

    assert response.status_code == 200
    assert response.body["ok"] is True
    assert response.body["status"] == "succeeded"


# ── Scheduler ────────────────────────────────────────────────────────────


def test_scheduler_job_state_bare_root_keys_land_under_input(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    orch_path = tmp_path / "due.yml"
    _write_orchestration(orch_path)

    captured: list[RunRequest] = []

    def fake_run(req: RunRequest) -> RunResult:
        captured.append(req)
        return RunResult(ok=True, state=dict(req.initial_state or {}), warnings=[])

    monkeypatch.setattr("circuitry.service.scheduler.run", fake_run)

    job = ScheduledJob(
        name="hourly",
        orchestration_path=orch_path,
        interval_seconds=60,
        dry_run=True,
        state={"name": "Elena"},
        start_at=datetime(2026, 2, 19, 12, 0, tzinfo=timezone.utc),
    )
    scheduler = RecurringScheduler(jobs=[job])
    records = scheduler.tick(now=datetime(2026, 2, 19, 12, 0, tzinfo=timezone.utc))

    assert len(records) == 1
    assert records[0].status == "succeeded"
    assert len(captured) == 1
    assert captured[0].initial_state["input"] == {"name": "Elena"}
    assert "name" not in captured[0].initial_state


def test_scheduler_job_without_state_still_gets_input_namespace(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    orch_path = tmp_path / "due.yml"
    _write_orchestration(orch_path)

    captured: list[RunRequest] = []

    def fake_run(req: RunRequest) -> RunResult:
        captured.append(req)
        return RunResult(ok=True, state=dict(req.initial_state or {}), warnings=[])

    monkeypatch.setattr("circuitry.service.scheduler.run", fake_run)

    job = ScheduledJob(
        name="hourly",
        orchestration_path=orch_path,
        interval_seconds=60,
        dry_run=True,
        start_at=datetime(2026, 2, 19, 12, 0, tzinfo=timezone.utc),
    )
    scheduler = RecurringScheduler(jobs=[job])
    scheduler.tick(now=datetime(2026, 2, 19, 12, 0, tzinfo=timezone.utc))

    assert captured[0].initial_state["input"] == {}
