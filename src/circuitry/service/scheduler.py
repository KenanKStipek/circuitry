from __future__ import annotations

from copy import deepcopy
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any
from uuid import uuid4

from ..cli.config import CircuitryConfig
from ..cli.runtime_shim import RunRequest, run


@dataclass(frozen=True)
class ScheduledJob:
    name: str
    orchestration_path: Path
    interval_seconds: int
    dry_run: bool = False
    validate_only: bool = False
    verbose: bool = False
    state: dict[str, Any] | None = None
    out_path: Path | None = None
    start_at: datetime | None = None
    allowed_lateness_seconds: int = 30


@dataclass(frozen=True)
class ScheduledDispatchRecord:
    job_name: str
    invocation_id: str
    planned_at: str
    triggered_at: str
    completed_at: str
    delay_seconds: float
    delayed: bool
    status: str
    error: str | None


class RecurringScheduler:
    """Deterministic recurring dispatcher for orchestration jobs."""

    def __init__(
        self,
        *,
        jobs: list[ScheduledJob],
        config: CircuitryConfig | None = None,
    ) -> None:
        self._jobs = jobs
        self._config = config
        self._next_planned_by_job: dict[str, datetime] = {}
        now = _utc_now()
        for job in jobs:
            if job.interval_seconds <= 0:
                raise ValueError(
                    f"Scheduled job '{job.name}' must use interval_seconds > 0."
                )
            self._next_planned_by_job[job.name] = _ensure_utc(job.start_at or now)

    def tick(self, *, now: datetime | None = None) -> list[ScheduledDispatchRecord]:
        current = _ensure_utc(now or _utc_now())
        records: list[ScheduledDispatchRecord] = []

        for job in self._jobs:
            planned = self._next_planned_by_job[job.name]
            if current < planned:
                continue

            invocation_id = str(uuid4())
            delay_seconds = max(0.0, (current - planned).total_seconds())
            delayed = delay_seconds > float(job.allowed_lateness_seconds)

            record = self._dispatch_job(
                job=job,
                invocation_id=invocation_id,
                planned=planned,
                triggered=current,
                delay_seconds=delay_seconds,
                delayed=delayed,
            )
            records.append(record)

            self._next_planned_by_job[job.name] = self._next_planned_after(
                planned=planned,
                interval_seconds=job.interval_seconds,
                current=current,
            )

        return records

    def _dispatch_job(
        self,
        *,
        job: ScheduledJob,
        invocation_id: str,
        planned: datetime,
        triggered: datetime,
        delay_seconds: float,
        delayed: bool,
    ) -> ScheduledDispatchRecord:
        planned_at = planned.isoformat()
        triggered_at = triggered.isoformat()
        state = deepcopy(job.state or {})
        runtime = state.setdefault("runtime", {})
        runtime["schedule"] = {
            "job_name": job.name,
            "invocation_id": invocation_id,
            "planned_at": planned_at,
            "triggered_at": triggered_at,
            "delay_seconds": delay_seconds,
            "delayed": delayed,
            "status": "started",
            "error": None,
            "completed_at": None,
        }

        req = RunRequest(
            orchestration_path=job.orchestration_path,
            state_path=None,
            out_path=job.out_path,
            dry_run=job.dry_run,
            validate_only=job.validate_only,
            initial_state=state,
            verbose=job.verbose,
            config=self._config,
        )
        result = run(req)

        completed = _utc_now()
        completed_at = completed.isoformat()
        status = "succeeded" if result.ok else "failed"
        error = result.error

        result_runtime = result.state.setdefault("runtime", {})
        schedule_meta = result_runtime.setdefault("schedule", {})
        schedule_meta.update(
            {
                "status": status,
                "error": error,
                "completed_at": completed_at,
            }
        )

        history = result_runtime.setdefault("schedule_history", [])
        if isinstance(history, list):
            history.append(
                {
                    "job_name": job.name,
                    "invocation_id": invocation_id,
                    "planned_at": planned_at,
                    "triggered_at": triggered_at,
                    "completed_at": completed_at,
                    "delay_seconds": delay_seconds,
                    "delayed": delayed,
                    "status": status,
                    "error": error,
                }
            )

        return ScheduledDispatchRecord(
            job_name=job.name,
            invocation_id=invocation_id,
            planned_at=planned_at,
            triggered_at=triggered_at,
            completed_at=completed_at,
            delay_seconds=delay_seconds,
            delayed=delayed,
            status=status,
            error=error,
        )

    def _next_planned_after(
        self,
        *,
        planned: datetime,
        interval_seconds: int,
        current: datetime,
    ) -> datetime:
        next_planned = planned
        step = timedelta(seconds=interval_seconds)
        while next_planned <= current:
            next_planned = next_planned + step
        return next_planned


def _ensure_utc(dt: datetime) -> datetime:
    if dt.tzinfo is None:
        return dt.replace(tzinfo=timezone.utc)
    return dt.astimezone(timezone.utc)


def _utc_now() -> datetime:
    return datetime.now(timezone.utc)
