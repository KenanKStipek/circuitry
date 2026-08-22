from __future__ import annotations

from datetime import datetime, timedelta, timezone
from pathlib import Path

from circuitry.service import RecurringScheduler, ScheduledJob

BASE = datetime(2026, 2, 19, 12, 0, tzinfo=timezone.utc)


def _write_orchestration(path: Path) -> None:
    path.write_text(
        """
name: schedule_root
adapter: openai
model: gpt-4o-mini
effects:
  - type: prompt
    name: ping
    template: "ping"
""".strip()
        + "\n",
        encoding="utf-8",
    )


def test_scheduler_dispatches_when_job_becomes_due(tmp_path: Path) -> None:
    orch_path = tmp_path / "due.yml"
    _write_orchestration(orch_path)

    job = ScheduledJob(
        name="hourly",
        orchestration_path=orch_path,
        interval_seconds=60,
        dry_run=True,
        start_at=BASE,
    )
    scheduler = RecurringScheduler(jobs=[job])

    before_due = scheduler.tick(now=BASE - timedelta(seconds=1))
    assert before_due == []

    due = scheduler.tick(now=BASE)
    assert len(due) == 1
    assert due[0].job_name == "hourly"
    assert due[0].status == "succeeded"


def test_scheduler_marks_delayed_dispatches_for_diagnostics(tmp_path: Path) -> None:
    orch_path = tmp_path / "delayed.yml"
    _write_orchestration(orch_path)

    job = ScheduledJob(
        name="delayed",
        orchestration_path=orch_path,
        interval_seconds=60,
        dry_run=True,
        start_at=BASE,
        allowed_lateness_seconds=5,
    )
    scheduler = RecurringScheduler(jobs=[job])

    late = scheduler.tick(now=BASE + timedelta(seconds=12))
    assert len(late) == 1
    assert late[0].status == "succeeded"
    assert late[0].delayed is True
    assert late[0].delay_seconds >= 12.0


def test_scheduler_reports_failed_runs(tmp_path: Path) -> None:
    job = ScheduledJob(
        name="missing",
        orchestration_path=tmp_path / "missing.yml",
        interval_seconds=60,
        dry_run=True,
        start_at=BASE,
    )
    scheduler = RecurringScheduler(jobs=[job])

    records = scheduler.tick(now=BASE)
    assert len(records) == 1
    assert records[0].status == "failed"
    assert records[0].error


def test_scheduler_keeps_invocation_history_traceable(tmp_path: Path) -> None:
    orch_path = tmp_path / "trace.yml"
    _write_orchestration(orch_path)

    job = ScheduledJob(
        name="trace",
        orchestration_path=orch_path,
        interval_seconds=60,
        dry_run=True,
        start_at=BASE,
    )
    scheduler = RecurringScheduler(jobs=[job])

    first = scheduler.tick(now=BASE)
    second = scheduler.tick(now=BASE + timedelta(seconds=61))

    assert len(first) == 1
    assert len(second) == 1
    assert first[0].invocation_id != second[0].invocation_id
    assert first[0].planned_at != second[0].planned_at
