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
    warnings: list[str] = field(default_factory=list)
    cancel_event: threading.Event = field(default_factory=threading.Event, repr=False)
    thread: threading.Thread | None = field(default=None, repr=False)
    started_at: datetime = field(default_factory=lambda: datetime.now(timezone.utc))
    completed_at: datetime | None = None
    # How many concurrent "settle points" (a registered prompt, or a branch
    # that finished without one) `_wait_for_settle` should wait to see
    # before returning — 1 by default (the ordinary sequential case), raised
    # by a `flow: tree` loop's or parallel `dynamic`'s own
    # `concurrent_dispatch` signal (accumulated across nested dispatches,
    # bounded by `max_concurrency`) and lowered by its `branch_settled`
    # signal as each dispatched branch finishes (#237). Reset to 1 every time
    # `_wait_for_settle` returns. `submit_response` consults it too, after
    # the answered prompt's own removal — see `_wait_for_prompt_gone`.
    expected_concurrent: int = 1
    # A plain RLock everywhere this was already `with run._lock:`; also a
    # Condition so `_wait_for_settle`/`_wait_for_prompt_gone` can block on a
    # real notification instead of polling on a fixed interval (#237).
    _lock: threading.Condition = field(default_factory=threading.Condition, repr=False)


class RunManager:
    """
    Owns the lifecycle of in-flight orchestration runs driven via the host
    LLM (e.g. Claude through MCP). One worker thread per run; per-prompt
    blocking queues route each branch's response back to the correct waiter.
    """

    def __init__(
        self,
        *,
        quiesce_max_wait_seconds: float = 5.0,
        cancel_join_timeout: float = 5.0,
        worker_poll_interval: float = 0.1,
        max_concurrent_runs: int = 50,
        max_retained_runs: int = 200,
    ) -> None:
        self._runs: dict[str, Run] = {}
        self._lock = threading.RLock()
        # The overall safety-net budget `_wait_for_settle`/`_wait_for_prompt_gone`
        # give up after — not a debounce window; both are event-driven (#237).
        self._quiesce_max_wait = quiesce_max_wait_seconds
        self._cancel_join_timeout = cancel_join_timeout
        self._worker_poll_interval = worker_poll_interval
        # A run (and its worker thread) is kept until explicitly cancelled or
        # the retention cap below evicts it — nothing else ever drops one, so
        # a client that starts runs in a loop and never cancels/evicts them
        # could otherwise exhaust host memory and threads one call at a time.
        self._max_concurrent_runs = max_concurrent_runs
        self._max_retained_runs = max_retained_runs

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
            active = sum(1 for r in self._runs.values() if not r.status.is_terminal)
            if active >= self._max_concurrent_runs:
                raise RuntimeError(
                    f"circuitry-mcp: {active} run(s) already in flight "
                    f"(limit {self._max_concurrent_runs}); cancel one or wait "
                    "for it to finish before starting another."
                )
            self._evict_oldest_terminal_runs_locked()
            self._runs[run.run_id] = run

        adapter = HostClaudeAdapter(
            request_handler=lambda req: self._handler_for(run, req),
            override_model=override_model,
            override_to=override_to,
        )
        cfg = resolve_config()

        def _on_concurrent_dispatch(_effect_path: str, branch_count: int) -> None:
            with run._lock:
                # Accumulate, not overwrite: this dispatch replaces the one
                # settle point it was itself going to contribute with
                # *branch_count* of its own (0 if the tree/dynamic is empty
                # and resolves without dispatching anything) — so a tree
                # nested inside another tree branch or a `use:` child adds
                # to what's still outstanding instead of clobbering it.
                # Floored at 1: a fresh dispatch always owes at least one
                # settle point immediately after it fires; real completions
                # (`_on_branch_settled`) are what bring it down from there.
                run.expected_concurrent = max(
                    1, run.expected_concurrent + branch_count - 1
                )
                run._lock.notify_all()

        def _on_branch_settled(_effect_path: str) -> None:
            with run._lock:
                # One branch of some earlier dispatch just finished —
                # whether or not it ever registered a prompt. One fewer
                # settle point still outstanding.
                run.expected_concurrent = max(0, run.expected_concurrent - 1)
                run._lock.notify_all()

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
            concurrent_dispatch_observer=_on_concurrent_dispatch,
            branch_settled_observer=_on_branch_settled,
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
        self._wait_for_settle(run)
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
        self._wait_for_prompt_gone(run, prompt_id)
        # The answered prompt is gone, but the step it unblocked may not have
        # reached its own next settle point yet (another prompt, or
        # completion) — wait the same way `start_run` does so a client never
        # sees a `running` snapshot with the worker simply not yet scheduled
        # (#237).
        self._wait_for_settle(run)
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
            run._lock.notify_all()
        return run

    # ----------------------------------------------------------------- worker
    def _thread_target(self, run: Run, request: RunRequest) -> None:
        try:
            result = run_orchestration(request)
            with run._lock:
                run.state = deepcopy(result.state)
                run.warnings = list(result.warnings)
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
                run._lock.notify_all()

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
            run._lock.notify_all()

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
                run._lock.notify_all()

        # Final cancel check before returning text — a `submit_response` race
        # with `cancel_run` should still surface as cancelled.
        if run.cancel_event.is_set():
            raise RunCancelled(f"Run {run.run_id} cancelled after response")

        if not isinstance(response_text, str):
            response_text = str(response_text)
        return response_text

    def _evict_oldest_terminal_runs_locked(self) -> None:
        """Drop the oldest-completed terminal runs once adding one more would
        exceed ``max_retained_runs``. Caller holds ``self._lock``. Never
        touches a non-terminal run — its worker thread may still be running
        and a client may still be waiting to call ``get_run``/
        ``submit_response`` on it.
        """
        if len(self._runs) < self._max_retained_runs:
            return
        terminal = sorted(
            (
                run
                for run in self._runs.values()
                if run.status.is_terminal and run.completed_at is not None
            ),
            key=lambda run: run.completed_at,  # type: ignore[arg-type,return-value]
        )
        overflow = len(self._runs) - self._max_retained_runs + 1
        for run in terminal[:overflow]:
            self._runs.pop(run.run_id, None)

    # ----------------------------------------------------------------- internal
    def _require_run(self, run_id: str) -> Run:
        with self._lock:
            run = self._runs.get(run_id)
        if run is None:
            raise KeyError(f"Unknown run_id: {run_id}")
        return run

    def _wait_for_settle(self, run: Run) -> None:
        """
        Block until the run reaches a terminal status, or `pending_prompts`
        holds at least `expected_concurrent` entries — 1 by default (the
        ordinary sequential case), raised by a `flow: tree` loop's or
        parallel `dynamic`'s own `concurrent_dispatch` signal (accumulated,
        not overwritten, so a tree nested inside another tree branch or a
        `use:` child adds to what's outstanding instead of clobbering it;
        bounded by `max_concurrency` when the dispatcher sets one), and
        lowered again by its `branch_settled` signal as each dispatched
        branch finishes — including one that never registers a prompt at all
        (a tool-only branch, a `when:` skip) (#237). Reset to the baseline of
        1 on every return, so the next call starts fresh rather than reusing
        a stale branch count from a dispatch this wait already resolved.

        Event-driven: waits on `run._lock` (a :class:`threading.Condition`),
        woken by every prompt registration/removal, status change,
        dispatch/settle signal, or cancellation — never a fixed debounce
        window a scheduling delay can race. `quiesce_max_wait_seconds`
        remains a safety-net budget, for a run that genuinely pauses fewer
        branches than were dispatched before any of them can report in,
        rather than a quiescence timer.
        """
        deadline = _monotonic() + self._quiesce_max_wait
        with run._lock:
            while True:
                if run.cancel_event.is_set() or run.status.is_terminal:
                    return
                if len(run.pending_prompts) >= run.expected_concurrent:
                    # Settled — reset to the ordinary-sequential baseline so
                    # the *next* wait (another `submit_response`, or a later
                    # dispatch) starts fresh instead of carrying this wave's
                    # branch count forward into a step that may dispatch
                    # nothing, or something smaller (#237).
                    run.expected_concurrent = 1
                    return
                remaining = deadline - _monotonic()
                if remaining <= 0:
                    run.expected_concurrent = 1
                    return
                run._lock.wait(timeout=min(remaining, self._worker_poll_interval))

    def _wait_for_prompt_gone(self, run: Run, prompt_id: str) -> None:
        """
        Block until *prompt_id* is no longer listed as pending (the worker
        popped it once `submit_response` unblocked it), or the run is
        terminal/cancelled — so a client re-reading state never sees the
        prompt it just answered as still current (#237). Event-driven, same
        mechanism as :meth:`_wait_for_settle`.
        """
        deadline = _monotonic() + self._quiesce_max_wait
        with run._lock:
            while True:
                if run.cancel_event.is_set() or run.status.is_terminal:
                    return
                if prompt_id not in run.pending_prompts:
                    return
                remaining = deadline - _monotonic()
                if remaining <= 0:
                    return
                run._lock.wait(timeout=min(remaining, self._worker_poll_interval))


# Module-level shim so tests can monkeypatch it if ever needed without poking
# at imported names from inside class methods.
def _monotonic() -> float:
    import time

    return time.monotonic()
