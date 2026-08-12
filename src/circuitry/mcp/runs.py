"""
Run manager for circuitry-mcp.

Coordinates orchestration runs against a `HostClaudeAdapter` whose
`request_handler` blocks on per-prompt queues. The MCP server's tools
(`run_orchestration`, `submit_response`, etc.) call into this manager;
the manager owns the worker thread per run, the prompt routing, and the
quiescence detection that lets the host see all parallel branches in one
round-trip.

Threading invariants:
  - The per-run `_lock` is held only for short critical sections (dict
    mutations, status reads). It is **never** held while blocked on a
    queue, to avoid deadlocks between worker and tool-call threads.
  - All blocking `Queue.get` calls use bounded timeouts and re-check the
    `cancel_event`, so cancellation always wakes blocked workers.
  - On cancel, we push a sentinel onto every pending response queue to
    unblock workers immediately rather than waiting for the timeout.
"""

from __future__ import annotations

import logging
import queue
import threading
import uuid
from copy import deepcopy
from dataclasses import dataclass, field
from datetime import datetime, timezone
from enum import Enum
from pathlib import Path
from typing import Any

from ..adapters import HostClaudeAdapter, HostPromptRequest, RunCancelled
from ..cli.config import resolve_config
from ..cli.runtime_shim import RunRequest
from ..cli.runtime_shim import run as run_orchestration

logger = logging.getLogger(__name__)


class RunStatus(str, Enum):
    PENDING = "pending"
    RUNNING = "running"
    PAUSED = "paused"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"

    @property
    def is_terminal(self) -> bool:
        return self in (RunStatus.COMPLETED, RunStatus.FAILED, RunStatus.CANCELLED)


# Pushed onto a per-prompt response queue to wake a blocked worker on cancel.
_CANCEL_SENTINEL: object = object()


@dataclass
class PendingPrompt:
    prompt_id: str
    prompt: str
    model: str
    requested_at: datetime
    response_queue: queue.Queue = field(default_factory=lambda: queue.Queue(maxsize=1), repr=False)


@dataclass
class Run:
    run_id: str
    orchestration_path: Path
    status: RunStatus = RunStatus.PENDING
    state: dict[str, Any] = field(default_factory=dict)
    pending_prompts: dict[str, PendingPrompt] = field(default_factory=dict)
    error: str | None = None
    cancel_event: threading.Event = field(default_factory=threading.Event, repr=False)
    thread: threading.Thread | None = field(default=None, repr=False)
    started_at: datetime = field(default_factory=lambda: datetime.now(timezone.utc))
    completed_at: datetime | None = None
    _lock: threading.RLock = field(default_factory=threading.RLock, repr=False)


class RunManager:
    """
    Owns the lifecycle of in-flight orchestration runs driven via the host
    LLM (e.g. Claude through MCP). One worker thread per run; per-prompt
    blocking queues route each branch's response back to the correct waiter.
    """

    def __init__(
        self,
        *,
        quiesce_seconds: float = 0.05,
        quiesce_max_wait_seconds: float = 5.0,
        cancel_join_timeout: float = 5.0,
        worker_poll_interval: float = 0.1,
    ) -> None:
        self._runs: dict[str, Run] = {}
        self._lock = threading.RLock()
        self._quiesce = quiesce_seconds
        self._quiesce_max_wait = quiesce_max_wait_seconds
        self._cancel_join_timeout = cancel_join_timeout
        self._worker_poll_interval = worker_poll_interval

    # ----------------------------------------------------------------- public
    def start_run(
        self,
        *,
        orchestration_path: Path,
        initial_state: dict[str, Any] | None = None,
        override_model: bool = False,
        override_to: str = "",
    ) -> Run:
        run = Run(run_id=uuid.uuid4().hex, orchestration_path=orchestration_path)
        with self._lock:
            self._runs[run.run_id] = run

        adapter = HostClaudeAdapter(
            request_handler=lambda req: self._handler_for(run, req),
            override_model=override_model,
            override_to=override_to,
        )
        cfg = resolve_config()

        def _observe_state(snapshot: dict[str, Any]) -> None:
            # Snapshot is a reference to the worker's live state dict; deepcopy
            # under the per-run lock so callers of get_state() see a frozen
            # view that won't tear under concurrent mutation.
            with run._lock:
                run.state = deepcopy(snapshot)

        request = RunRequest(
            orchestration_path=orchestration_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state=initial_state,
            verbose=False,
            config=cfg,
            adapter=adapter,
            state_observer=_observe_state,
        )
        thread = threading.Thread(
            target=self._thread_target,
            args=(run, request),
            name=f"circuitry-run-{run.run_id[:8]}",
            daemon=True,
        )
        with run._lock:
            run.thread = thread
            run.status = RunStatus.RUNNING
        thread.start()
        self._wait_for_quiesce(run)
        return run

    def submit_response(
        self, *, run_id: str, prompt_id: str, response_text: str
    ) -> Run:
        run = self._require_run(run_id)
        with run._lock:
            pending = run.pending_prompts.get(prompt_id)
            if pending is None:
                raise KeyError(
                    f"Unknown prompt_id {prompt_id!r} for run {run_id} "
                    f"(known: {sorted(run.pending_prompts)})"
                )
            pending.response_queue.put(response_text)
        self._wait_for_quiesce(run)
        return run

    def get_state(self, run_id: str) -> dict[str, Any]:
        run = self._require_run(run_id)
        with run._lock:
            return deepcopy(run.state)

    def get_run(self, run_id: str) -> Run:
        return self._require_run(run_id)

    def cancel_run(self, run_id: str) -> Run:
        run = self._require_run(run_id)
        run.cancel_event.set()

        # Wake every blocked worker. Mutate the dict under the lock, but push
        # the sentinel after — Queue.put can briefly block (size=1) if a
        # worker is mid-handoff, and we never want to hold the lock while
        # blocking on a queue.
        pending_snapshot: list[PendingPrompt] = []
        with run._lock:
            pending_snapshot = list(run.pending_prompts.values())
        for pending in pending_snapshot:
            try:
                pending.response_queue.put_nowait(_CANCEL_SENTINEL)
            except queue.Full:
                # Worker has already received a response; nothing to wake.
                pass

        thread = run.thread
        if thread is not None and thread.is_alive():
            thread.join(timeout=self._cancel_join_timeout)

        with run._lock:
            # Even if the worker itself didn't catch RunCancelled in time,
            # mark the run cancelled and clear pending prompts so subsequent
            # submit_response calls return a structured error.
            if not run.status.is_terminal:
                run.status = RunStatus.CANCELLED
            run.pending_prompts.clear()
            if run.completed_at is None:
                run.completed_at = datetime.now(timezone.utc)
        return run

    # ----------------------------------------------------------------- worker
    def _thread_target(self, run: Run, request: RunRequest) -> None:
        try:
            result = run_orchestration(request)
            with run._lock:
                run.state = deepcopy(result.state)
                if result.ok:
                    run.status = RunStatus.COMPLETED
                else:
                    # runtime_shim swallows exceptions and returns ok=False;
                    # propagate the error string to the run.
                    run.status = RunStatus.FAILED
                    run.error = result.error or "Orchestration failed"
        except RunCancelled as exc:
            with run._lock:
                run.status = RunStatus.CANCELLED
                run.error = str(exc) or None
        except Exception as exc:
            logger.exception("Unhandled exception in run worker")
            with run._lock:
                run.status = RunStatus.FAILED
                run.error = str(exc) or type(exc).__name__
        finally:
            with run._lock:
                run.pending_prompts.clear()
                if run.completed_at is None:
                    run.completed_at = datetime.now(timezone.utc)

    def _handler_for(self, run: Run, host_req: HostPromptRequest) -> str:
        # Cancel-before-queue check.
        if run.cancel_event.is_set():
            raise RunCancelled(f"Run {run.run_id} cancelled before queueing prompt")

        prompt_id = uuid.uuid4().hex
        pending = PendingPrompt(
            prompt_id=prompt_id,
            prompt=host_req.prompt,
            model=host_req.model,
            requested_at=datetime.now(timezone.utc),
        )
        with run._lock:
            run.pending_prompts[prompt_id] = pending
            run.status = RunStatus.PAUSED

        # Block on the per-prompt response queue WITHOUT holding the run lock,
        # otherwise concurrent submit_response/cancel calls would deadlock.
        try:
            while True:
                if run.cancel_event.is_set():
                    raise RunCancelled(f"Run {run.run_id} cancelled while awaiting response")
                try:
                    item = pending.response_queue.get(timeout=self._worker_poll_interval)
                except queue.Empty:
                    continue
                if item is _CANCEL_SENTINEL:
                    raise RunCancelled(f"Run {run.run_id} cancelled mid-prompt")
                response_text = item
                break
        finally:
            with run._lock:
                run.pending_prompts.pop(prompt_id, None)
                if not run.pending_prompts and run.status == RunStatus.PAUSED:
                    # No more parallel branches blocking; back to RUNNING until
                    # the next prompt effect (or completion).
                    run.status = RunStatus.RUNNING

        # Final cancel check before returning text — a `submit_response` race
        # with `cancel_run` should still surface as cancelled.
        if run.cancel_event.is_set():
            raise RunCancelled(f"Run {run.run_id} cancelled after response")

        if not isinstance(response_text, str):
            response_text = str(response_text)
        return response_text

    # ----------------------------------------------------------------- internal
    def _require_run(self, run_id: str) -> Run:
        with self._lock:
            run = self._runs.get(run_id)
        if run is None:
            raise KeyError(f"Unknown run_id: {run_id}")
        return run

    def _wait_for_quiesce(self, run: Run) -> None:
        """
        Poll until either the run reaches a terminal status, or its
        `pending_prompts` keyset has been stable for `quiesce` seconds.

        Used after `start_run` and `submit_response` so the caller sees a
        settled snapshot rather than racing the worker thread spawning the
        first parallel branch. Returns early on cancel.
        """
        deadline = _monotonic() + self._quiesce_max_wait
        last_keys: frozenset[str] | None = None
        last_change = _monotonic()

        while True:
            if run.cancel_event.is_set():
                return
            with run._lock:
                terminal = run.status.is_terminal
                current_keys = frozenset(run.pending_prompts)
            if terminal:
                return
            now = _monotonic()
            if last_keys is None or current_keys != last_keys:
                last_keys = current_keys
                last_change = now
            elif now - last_change >= self._quiesce:
                return
            if now >= deadline:
                return
            # Sleep for the smaller of (poll granularity, time-to-quiesce).
            remaining = self._quiesce - (now - last_change)
            sleep_for = min(0.01, max(0.001, remaining))
            threading_sleep(sleep_for)


# Module-level shims so tests can monkeypatch them if ever needed without
# poking at imported names from inside class methods.
def _monotonic() -> float:
    import time

    return time.monotonic()


def threading_sleep(seconds: float) -> None:
    import time

    time.sleep(seconds)
