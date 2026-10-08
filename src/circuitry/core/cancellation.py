"""Run-wide cancellation: SIGINT/SIGTERM/SIGHUP stop a run promptly (#356).

A real SIGINT/SIGTERM only ever raises ``KeyboardInterrupt``/
``SigTermInterrupt`` (see ``cli.interrupts``) in the *main* thread — that is
how CPython signal delivery works. A tree-flow ``dynamic``/``loop`` branch
running on a ``ThreadPoolExecutor`` worker thread never sees that exception,
so without this module a cancelled run's queued branches would still all
start and its running branches would run to completion before the process
could exit (#356's "leaving the `with` block waits for every submitted
branch").

:class:`CancellationToken` is the shared, run-wide answer: one instance per
`cof run`/`run-library` invocation (installed by
``cli.interrupts.sigterm_as_interrupt``, which calls :meth:`request` from
its signal handler — in the main thread, but synchronously, before the
handler raises). Worker-thread code that would otherwise block
indefinitely without ever checking back in with the main thread — a retry
backoff sleep, a concurrency-slot wait, the next loop iteration — polls it
instead:

  - :meth:`check` / :meth:`sleep_or_raise` raise :class:`RunCancelledBySignal`, a
    ``BaseException`` (not ``Exception``) so it propagates through
    ``dynamic``/``loop``/``use``'s own ``except Exception`` the same way a
    real ``KeyboardInterrupt`` already does, and is recognized as a
    cancellation (not an ordinary failure) by their shared
    ``is_cancellation = not isinstance(exc, Exception)`` check.
  - :meth:`track` registers a subprocess so that :meth:`request` can kill
    it the moment cancellation is requested, regardless of which thread
    started it — its whole process group, via :func:`kill_process_group`,
    when it was started with ``start_new_session=True`` and so is its own
    process-group leader (:func:`run_tracked`, whenever a SIGINT/SIGTERM
    handler is actually installed — see :attr:`CancellationToken.armed`);
    just the process itself otherwise, since ``killpg`` on a process still
    sharing this interpreter's own process group would take this process
    down with it (#357 review N1).

A queued-but-not-yet-started ``ThreadPoolExecutor`` future is handled
separately, by ``executor.shutdown(cancel_futures=True)`` at each tree-flow
call site — stdlib concurrent.futures already refuses to start a cancelled
future, which is simpler and more precise than anything this module could
add on top.
"""

from __future__ import annotations

import contextvars
import os
import signal
import subprocess
import threading
import time
from collections.abc import Callable, Iterable, Iterator, Sequence
from concurrent.futures import Executor, Future
from contextlib import contextmanager
from multiprocessing.process import BaseProcess
from typing import Any, TypeVar

_T = TypeVar("_T")

#: Set (to a count, so nested cleanup scopes compose) while the current
#: context is inside :meth:`CancellationToken.cleanup` — a module-level
#: ``ContextVar``, not an attribute on the token, so it can suppress
#: cancellation for exactly the thread(s) running a ``finally:`` block
#: without affecting any other branch still running concurrently. See
#: :func:`submit_with_context` for how it reaches a worker thread at all.
_cleanup_depth: contextvars.ContextVar[int] = contextvars.ContextVar(
    "circuitry_cancellation_cleanup_depth", default=0
)


class RunCancelledBySignal(BaseException):
    """A run was cancelled (SIGINT/SIGTERM) while this code was waiting.

    Deliberately a direct ``BaseException`` subclass, not ``Exception``
    (nor ``KeyboardInterrupt`` — that is reserved for what a real signal
    raises in the main thread; this is raised by worker-thread code
    polling :class:`CancellationToken` instead) — see the module
    docstring for why that distinction matters to every container's own
    cancellation check.
    """


def kill_process_group(proc: subprocess.Popen[str] | subprocess.Popen[bytes]) -> None:
    """Best-effort SIGKILL of *proc*'s whole process group.

    No-op once the process has already exited. POSIX only — Windows gets
    no process-group isolation here (``start_new_session`` is a POSIX
    concept), so a cancelled run on Windows falls back to killing just the
    immediate child, the same as before this module existed.

    Public: ``plugins._subprocess.run_binary`` reuses this same helper for
    its own (pre-existing) per-call timeout kill, so a timed-out subprocess
    and a cancelled one are torn down identically.

    A child only ever gets its own process group when it was started with
    ``start_new_session=True`` — :func:`run_tracked` only does that while
    :attr:`CancellationToken.armed` is set (#357 review N1). Otherwise the
    child's ``pgid`` *is* this interpreter's own ``getpgrp()``, so
    ``killpg`` on it would SIGKILL this process and its whole group along
    with the child — e.g. an unarmed timeout under the SDK, the MCP
    server, or any embedder that never installs ``cof run``'s signal
    handler. Guard against that case and kill just the child instead.
    """
    if proc.poll() is not None:
        return
    if os.name != "posix":
        try:
            proc.kill()
        except OSError:
            pass
        return
    try:
        pgid = os.getpgid(proc.pid)
    except ProcessLookupError:
        return
    if pgid == os.getpgrp():
        try:
            proc.kill()
        except ProcessLookupError:
            pass
        return
    try:
        os.killpg(pgid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def kill_tracked_process(proc: BaseProcess) -> None:
    """Best-effort SIGKILL of *proc*'s own pid only -- never a process group
    (#357 follow-up).

    Unlike :func:`kill_process_group`, which kills a ``subprocess.Popen``
    started with ``start_new_session=True`` (its own session leader) via
    ``killpg``, ``plugins.python_eval``'s sandboxed child is a
    ``multiprocessing.Process`` started in cof's own process group, not its
    own session -- ``killpg`` would SIGKILL this interpreter and its whole
    process group right along with the child. ``Process.kill()`` already
    targets only the child's own pid, so there is no group-vs-pid branch to
    make here, unlike :func:`kill_process_group`.
    """
    if proc.is_alive():
        try:
            proc.kill()
        except (OSError, ValueError):
            pass


class CancellationToken:
    """Run-wide cancellation flag plus the subprocess registry it kills.

    One instance backs an entire `cof run`/`run-library` invocation — see
    the module docstring. Not thread-local: the whole point is that a
    signal, delivered to the main thread only, must be visible to every
    worker thread a tree-flow branch is running on.
    """

    def __init__(self) -> None:
        self._event = threading.Event()
        # RLock, not a plain Lock (#356 review F4): the signal handler's
        # own call to request() takes this same lock from the *main*
        # thread, synchronously, wherever that thread's Python bytecode
        # happened to be interrupted — including inside track()/reset(),
        # both of which also take it. A plain Lock would deadlock the
        # whole process the instant a signal lands in that window; an
        # RLock lets the same thread reacquire it instead.
        self._lock = threading.RLock()
        self._processes: set[subprocess.Popen[str] | subprocess.Popen[bytes]] = set()
        #: python_eval's own sandboxed child (#357 follow-up) -- tracked
        #: separately from `_processes`, not folded into the same set,
        #: because it is killed differently: `kill_tracked_process` (pid
        #: only, never `killpg`) rather than `kill_process_group`.
        self._mp_processes: set[BaseProcess] = set()
        self._signum: int | None = None
        self._armed = False

    def reset(self) -> None:
        """Clear cancellation state for a fresh run.

        Called when `cof run`'s own signal-handling scope is entered and
        exited (``cli.interrupts.sigterm_as_interrupt``) — this token is a
        module-level singleton (:func:`get_token`), so a test harness or an
        embedder running several orchestrations in one process must not see
        an earlier run's cancellation leak into the next one.
        """
        self._event.clear()
        with self._lock:
            self._processes.clear()
            self._mp_processes.clear()
        self._signum = None

    def request(self, signum: int | None = None) -> bool:
        """Request cancellation, killing every tracked process group now.

        *signum*, when given, names the signal that caused this (SIGINT vs
        SIGTERM) — see :attr:`signum`; a :class:`RunCancelledBySignal` raised by a
        worker-thread poll carries no signal of its own, so ``runtime_shim``
        reads it from here instead to still pick exit code 143 over 130
        when it is this token's own ``RunCancelledBySignal`` (not the main thread's
        ``SigTermInterrupt``) that happens to reach the top first.

        Returns ``True`` the first time this is called (this call is the
        one that triggered cancellation); ``False`` if a prior call already
        did — the caller (the signal handler) uses that to end the process
        immediately, with no further cleanup, on a second signal.
        """
        first = not self._event.is_set()
        if first and signum is not None:
            self._signum = signum
        self._event.set()
        with self._lock:
            procs = list(self._processes)
            mp_procs = list(self._mp_processes)
        for proc in procs:
            kill_process_group(proc)
        for mp_proc in mp_procs:
            kill_tracked_process(mp_proc)
        return first

    @property
    def signum(self) -> int | None:
        """The signal that triggered cancellation, or ``None`` if none has."""
        return self._signum

    def is_set(self) -> bool:
        return self._event.is_set()

    @property
    def armed(self) -> bool:
        """Whether a SIGINT/SIGTERM handler is currently installed for this
        process (``cli.interrupts.sigterm_as_interrupt``'s own scope).

        :func:`run_tracked` only starts a tracked child in its own process
        group (``start_new_session``) while this is true (#356 review F5)
        — unarmed (an embedder: the SDK, the MCP server, the REST host, or
        a direct unit test of this module), a shell step keeps its normal
        controlling-terminal access (``ssh``/``sudo``/a credential prompt
        on ``/dev/tty``) and stays in this process's own group, so an
        external SIGKILL sent to that group still reaches it — at the
        cost of ``request()`` having no process group of its own to kill
        if that embedder is cancelled some other way.
        """
        return self._armed

    def arm(self) -> None:
        self._armed = True

    def disarm(self) -> None:
        self._armed = False

    def check(self) -> None:
        """Raise :class:`RunCancelledBySignal` if cancellation has been
        requested — unless the current context is inside :meth:`cleanup`.
        """
        if self._event.is_set() and not self.in_cleanup():
            raise RunCancelledBySignal("run cancelled")

    def sleep_or_raise(self, seconds: float) -> None:
        """Sleep up to *seconds*, raising :class:`RunCancelledBySignal` instead if
        cancellation is (or becomes) set before the sleep elapses.

        A cancellation-aware drop-in for ``time.sleep`` on any code path
        that might run on a worker thread — a real signal only ever
        interrupts a blocking ``time.sleep`` in the *main* thread. Inside
        :meth:`cleanup`, sleeps the full duration like plain ``time.sleep``
        instead — a retry backoff in a ``finally:`` must not abort early
        just because the run it's cleaning up from was already cancelled.

        Polls :attr:`_event` in short slices rather than one single
        ``wait(timeout=seconds)`` for the whole backoff (#385 follow-up,
        same reasoning as :func:`as_completed_promptly`): a retry backoff
        can run on the *main* thread too (a sequential chain's own retry,
        not just a tree-flow worker's), and a single long wait there is
        exactly the kind of blocking call that defers a signal delivered
        to some *other* thread (a ``Live`` refresh thread, the MCP
        client's own pool thread) until it returns on its own — this
        thread never gets to execute the bytecode that would run the
        pending handler until then. Slicing the wait means this thread
        returns to bytecode at least every poll interval regardless of
        which thread the signal reached.
        """
        if self.in_cleanup():
            time.sleep(max(0.0, seconds))
            return
        remaining = max(0.0, seconds)
        while True:
            if self._event.wait(timeout=min(remaining, _SIGNAL_POLL_SECONDS)):
                raise RunCancelledBySignal("run cancelled during backoff")
            remaining -= _SIGNAL_POLL_SECONDS
            if remaining <= 0:
                break

    def in_cleanup(self) -> bool:
        """Whether the current context is inside this token's own
        :meth:`cleanup` scope — see :meth:`check`/:meth:`sleep_or_raise`."""
        return _cleanup_depth.get() > 0

    @contextmanager
    def cleanup(self) -> Iterator[None]:
        """Suppress :meth:`check`/:meth:`sleep_or_raise` for the current
        context (and any worker thread started inside it via
        :func:`submit_with_context`) while a ``finally:`` block runs.

        A run's ``finally:`` must still run to completion after the run it
        is cleaning up from was cancelled (#356) — without this, the first
        signal's own ``is_set()`` would make every nested
        dynamic/loop/use/conditional inside that ``finally:`` immediately
        raise :class:`RunCancelledBySignal` the moment it next polls the
        token, abandoning the rest of the ``finally:`` list. A *second*
        signal arriving during cleanup is unaffected by this: the signal
        handler (``cli.interrupts``) kills every tracked process and calls
        ``os._exit`` directly, without going through :meth:`check` at all.

        A ``contextvars.ContextVar``, not a plain counter on ``self``: it
        must suppress cancellation only for the thread(s) actually running
        this ``finally:``, never for an unrelated branch still running
        concurrently elsewhere in the same cancelled run.
        """
        token = _cleanup_depth.set(_cleanup_depth.get() + 1)
        try:
            yield
        finally:
            _cleanup_depth.reset(token)

    @contextmanager
    def track(
        self, proc: subprocess.Popen[str] | subprocess.Popen[bytes]
    ) -> Iterator[None]:
        """Register *proc* so :meth:`request` kills it too.

        Killed via :func:`kill_process_group`, which only ``killpg``'s
        *proc*'s whole process group when it is that group's own leader
        (POSIX, started with ``start_new_session=True``) — otherwise it
        falls back to killing just *proc* itself, since *proc* sharing this
        interpreter's own process group would mean a group-kill took this
        process down with it too (#357 review N1).

        :func:`run_tracked` already calls :meth:`check` before ``Popen``,
        but cancellation can still be requested in the gap between that
        check and this registration -- :meth:`request` only kills the
        processes it can already see, so a process added right after it
        took its snapshot would otherwise run to completion unseen (#357
        review N2). Close that race here: if cancellation is already set
        the moment *proc* is registered, kill it immediately.
        """
        with self._lock:
            self._processes.add(proc)
            already_cancelled = self._event.is_set() and not self.in_cleanup()
        if already_cancelled:
            kill_process_group(proc)
        try:
            yield
        finally:
            with self._lock:
                self._processes.discard(proc)

    @contextmanager
    def track_process(self, proc: BaseProcess) -> Iterator[None]:
        """Register a ``multiprocessing`` child (``plugins.python_eval``'s
        sandboxed process) so :meth:`request` kills it too (#357 follow-up).

        Unlike :meth:`track`, this never calls ``killpg``: the child isn't
        a session leader of its own, so :meth:`request` kills it via
        :func:`kill_tracked_process` (pid only) instead. Mirrors
        :meth:`track`'s own already-cancelled race close: if cancellation
        is set the moment *proc* is registered, kill it immediately rather
        than leaving it to run unseen.
        """
        with self._lock:
            self._mp_processes.add(proc)
            already_cancelled = self._event.is_set() and not self.in_cleanup()
        if already_cancelled:
            kill_tracked_process(proc)
        try:
            yield
        finally:
            with self._lock:
                self._mp_processes.discard(proc)


_token = CancellationToken()


def get_token() -> CancellationToken:
    """Return the one :class:`CancellationToken` for this process.

    A bare module-level singleton, not threaded through ``runtime_config``
    like :class:`core.concurrency.RunConcurrencyLimiter`: cancellation is a
    process-wide, signal-driven concern (the signal handler that calls
    :meth:`CancellationToken.request` has no ``runtime_config`` of its own
    to read it from), not a per-run configuration value. Safe for tests
    that never install a signal handler: :meth:`CancellationToken.is_set`
    stays ``False`` and every poll below is a no-op unless something
    actually calls :meth:`CancellationToken.request`.
    """
    return _token


def submit_with_context(
    executor: Executor, fn: Callable[..., _T], /, *args: Any, **kwargs: Any
) -> Future[_T]:
    """``executor.submit(fn, *args, **kwargs)``, carrying the submitting
    thread's ``contextvars`` into the worker thread.

    A bare ``ThreadPoolExecutor.submit`` does not do this on its own (only
    asyncio's executor glue does) — so without this, a nested tree-flow
    ``dynamic``/parallel ``loop`` started from inside a ``finally:`` block
    would lose :meth:`CancellationToken.in_cleanup`'s suppression the
    moment it hit its own worker thread, and immediately abort with
    ``RunCancelledBySignal`` instead of running (#356). Every tree-flow /
    parallel-loop submit site in this package goes through this instead of
    calling ``executor.submit`` directly.
    """
    ctx = contextvars.copy_context()
    return executor.submit(ctx.run, fn, *args, **kwargs)


#: How often :func:`as_completed_promptly` wakes the main thread up to
#: check for a pending signal — see that function's own docstring for why
#: this can't just be ``concurrent.futures.as_completed``'s own unbounded
#: wait.
_SIGNAL_POLL_SECONDS = 0.2


def as_completed_promptly(
    futures: Iterable[Future[Any]], *, poll_seconds: float = _SIGNAL_POLL_SECONDS
) -> Iterator[Future[Any]]:
    """``concurrent.futures.as_completed``, but never blocks the main
    thread in one single, unbounded wait (#385 follow-up).

    POSIX delivers a process-directed signal (SIGINT/SIGTERM/SIGHUP) to
    *any* thread that doesn't have it blocked — not necessarily the main
    thread, and not necessarily every thread. CPython only ever runs the
    registered Python-level handler on the main thread, so when a signal
    lands on a tree-flow/parallel-loop worker thread instead, the C-level
    handler just records that the signal is pending; the main thread still
    has to notice and actually call it. It does that either by executing
    bytecode (checking the eval-loop's own "pending calls" flag) or by its
    own blocking call being interrupted and retrying. Plain
    ``as_completed(futures)`` with no timeout sits in exactly one such
    blocking call the whole time — a ``threading.Condition.wait()`` with
    no timeout, underneath a ``lock.acquire()`` that blocks in the kernel
    (``pthread_cond_wait``) until some *other* thread notifies it. If the
    signal never reaches the main thread's own blocking call, nothing
    wakes it: it just keeps waiting, oblivious, until whichever branch
    it's waiting on happens to finish on its own. In production that
    silently delays Ctrl-C/`kill`/a hangup for as long as the running
    branch takes; a repro that force-delivers a signal to a worker thread
    (``signal.pthread_kill``) while the main thread sits in
    ``as_completed`` confirms the handler then only runs once the future
    completes, however long that takes.

    The fix is the same one a blocking ``lock.acquire()`` already gets for
    free when the signal *does* land on the main thread: never wait
    unboundedly. ``add_done_callback`` (not ``concurrent.futures.wait``,
    which this deliberately avoids calling in a loop — see below) plus a
    private, bounded ``threading.Event.wait(poll_seconds)`` means the main
    thread returns from its own blocking call every *poll_seconds*
    regardless of which thread the signal landed on, executes a few
    bytecodes, and so picks up a pending signal within *poll_seconds*
    instead of only when a future happens to complete.

    Deliberately not ``concurrent.futures.wait(pending, timeout=...)`` in
    a loop (an earlier draft of this, and the shape ``as_completed``
    itself polls with when given a ``timeout``): both route every call
    through ``_AcquireFutures``, which acquires *every* pending future's
    own ``_condition`` lock in one Python-level loop, with no
    ``try/finally`` of its own, before doing anything else — if a signal
    is handled (raises) partway through that loop, the futures already
    locked in earlier iterations are never released, wedging any worker
    thread that later calls ``set_result``/``set_exception`` on one of
    them. ``as_completed`` only risks that once per call; calling
    ``wait()`` every *poll_seconds* would re-enter it continuously for as
    long as this run keeps going, for every still-pending future each
    time. Registering a callback up front instead touches each future's
    own lock at most once (the same single-lock exposure any ordinary
    ``Future.result()``/``add_done_callback`` call already has, signal or
    not), and the repeating part of this loop — waiting on a private
    ``threading.Event`` nothing else ever locks — carries none of that
    multi-future risk no matter how many times it polls.
    """
    fs = list(futures)
    ready: list[Future[Any]] = []
    ready_lock = threading.Lock()
    wake = threading.Event()

    def _on_done(f: Future[Any]) -> None:
        with ready_lock:
            ready.append(f)
        wake.set()

    for f in fs:
        f.add_done_callback(_on_done)

    remaining = len(fs)
    while remaining:
        wake.wait(poll_seconds)
        wake.clear()
        with ready_lock:
            batch, ready[:] = ready[:], []
        remaining -= len(batch)
        yield from batch


def wait_for_cancelled_branches(futures: Iterable[Future[Any]]) -> None:
    """Wait for every already-running branch to actually finish, before a
    tree-flow dynamic/parallel loop's own cancellation path re-raises past
    them — with no total time limit, only :func:`as_completed_promptly`'s
    own polled wait (#385 follow-up).

    Call this right after ``executor.shutdown(wait=False,
    cancel_futures=True)`` in that path. An earlier draft of this bounded
    the wait to a fixed grace period instead: once it elapsed, this
    function returned with a branch still running, the dynamic/loop
    re-raised past it, and ``cli.interrupts.sigterm_as_interrupt`` went on
    to call ``token.reset()`` on its way out of the run. A branch still
    running at that point then found the cancellation flag already
    cleared at its own next ``get_token().check()`` and started its next
    effect with nothing left to track or kill it — silently undoing the
    very cancellation this function exists to wait out. There is no safe
    bound to pick instead: the flag must stay set until every branch's own
    worker thread has actually returned. A second SIGINT/SIGTERM is still
    handled within one poll interval no matter which thread it reaches,
    exactly as :func:`as_completed_promptly` already guarantees for the
    main loop — the only further cost a branch the kill cannot reach adds
    here is delaying process exit until it returns, same as it already
    does on an uncancelled run.
    """
    for _ in as_completed_promptly(futures):
        pass


def acquire_promptly(
    sem: threading.Semaphore, *, poll_seconds: float = _SIGNAL_POLL_SECONDS
) -> None:
    """``sem.acquire()``, but in short polled slices rather than one
    unbounded C-level wait (#385 follow-up).

    Same reasoning as :func:`as_completed_promptly`/
    :meth:`CancellationToken.sleep_or_raise`: a thread blocked here for an
    adapter's own concurrency slot (e.g. ``adapters.cyberdiner``'s
    ``max_in_flight``) must keep returning to bytecode, or a signal
    delivered to some *other* thread goes unnoticed until whoever holds
    the slot releases it, however long that takes.
    """
    while not sem.acquire(timeout=poll_seconds):
        pass


def poll_promptly(
    conn: Any, timeout: float, *, poll_seconds: float = _SIGNAL_POLL_SECONDS
) -> bool:
    """``multiprocessing.connection.Connection.poll(timeout)``, but in
    short polled slices rather than one single wait (#385 follow-up).

    Same reasoning as :func:`as_completed_promptly`: ``plugins.python_eval``
    calls this from whichever thread is running that step — a tree-flow
    worker, or the main thread in a plain sequential chain — to wait for
    its sandboxed child's result. A single ``poll(wall_seconds)`` is
    bounded by the step's own timeout already, but that can still be
    minutes long, during which this thread never returns to bytecode to
    notice a signal delivered to some *other* thread. *timeout* is kept
    exactly via a wall-clock deadline computed up front.
    """
    deadline = time.monotonic() + timeout
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return False
        if conn.poll(min(poll_seconds, remaining)):
            return True


def _communicate_promptly(
    proc: subprocess.Popen[Any],
    *,
    input: str | bytes | None,
    timeout: float | None,
    poll_seconds: float = _SIGNAL_POLL_SECONDS,
) -> tuple[Any, Any]:
    """``proc.communicate()``, but never in one single wait as long as
    *timeout* itself (#385 follow-up).

    A plain ``proc.communicate(timeout=timeout)`` sits in one ``select()``
    bounded only by *timeout*, which can be minutes long for a slow curl/
    ffmpeg step — exactly the kind of single blocking call that defers a
    signal delivered to some *other* thread (ordinarily the main thread
    here, in a plain sequential chain, with a Rich ``Live`` refresh thread
    alive alongside it for the duration of the step) until it returns on
    its own. Calling ``communicate`` repeatedly with a short *poll_seconds*
    timeout instead returns this thread to bytecode that often, while
    *timeout* — the step's own deadline — is kept exactly via a wall-clock
    deadline computed up front, not reset by each retry.

    Retrying ``communicate()`` after its own ``TimeoutExpired`` is safe:
    passing *input* again is what the stdlib actually forbids once
    communication has started (``ValueError: Cannot send input after
    starting communication``), so this passes it only on the very first
    call and ``None`` on every retry; stdout/stderr accumulate across
    calls rather than resetting, so no output is lost to the slicing.
    """
    if timeout is None:
        deadline = None
    else:
        deadline = time.monotonic() + timeout
        full_timeout = timeout
    first = True
    while True:
        if deadline is None:
            slice_timeout = poll_seconds
        else:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise subprocess.TimeoutExpired(proc.args, full_timeout)
            slice_timeout = min(poll_seconds, remaining)
        try:
            return proc.communicate(
                input=input if first else None, timeout=slice_timeout
            )
        except subprocess.TimeoutExpired:
            first = False


def run_tracked(
    cmd: Sequence[str],
    *,
    capture_output: bool = True,
    text: bool = True,
    timeout: float | None = None,
    input: str | bytes | None = None,
    cwd: str | None = None,
    env: dict[str, str] | None = None,
    pass_fds: Sequence[int] = (),
    new_session: bool = False,
) -> subprocess.CompletedProcess[Any]:
    """``subprocess.run``, but tracked by :func:`get_token` for the whole
    time the child can block (#356).

    Every plugin/adapter in this catalog whose child can run for more
    than an instant (curl — so every model adapter and HTTP-backed
    plugin that goes through ``curl_support.run_curl``, plus ffmpeg,
    ComfyUI's own curl calls, a PDF renderer, gpg) goes through this
    instead of calling ``subprocess.run`` directly, so a cancelled run's
    signal handler can kill it the same way it already kills a
    ``run_binary`` child, rather than the main thread blocking on it
    until it finishes on its own. ``pass_fds`` exists for
    ``curl_support._run_curl_posix``, which hands curl its request
    config over an inherited pipe fd rather than argv or a temp file.

    Also refuses to even start a new child once cancellation has already
    been requested — ``get_token().check()`` before the ``Popen`` —
    unless called from inside :meth:`CancellationToken.cleanup` (a
    ``finally:`` block must still be able to run its own subprocess
    steps after the run it's cleaning up from was cancelled).

    ``new_session=True`` starts the child in its own process group even
    when the token is not armed, so a timeout or a cancellation kills
    everything the child started — for a headless child that never needs
    the controlling terminal (``agent_cli``'s coding-agent CLIs).
    """
    token = get_token()
    token.check()
    cmd = list(cmd)
    popen_kwargs: dict[str, Any] = {"cwd": cwd, "env": env}
    if capture_output:
        popen_kwargs["stdout"] = subprocess.PIPE
        popen_kwargs["stderr"] = subprocess.PIPE
    if input is not None:
        popen_kwargs["stdin"] = subprocess.PIPE
    if text:
        popen_kwargs["text"] = True
    if pass_fds:
        popen_kwargs["pass_fds"] = tuple(pass_fds)
    proc = subprocess.Popen(
        cmd,
        start_new_session=(os.name == "posix" and (token.armed or new_session)),
        **popen_kwargs,
    )
    with proc, token.track(proc):
        try:
            stdout, stderr = _communicate_promptly(proc, input=input, timeout=timeout)
        except subprocess.TimeoutExpired:
            kill_process_group(proc)
            proc.wait()
            raise
        except BaseException:
            # Cancellation already killed this process's group from the
            # signal handler; this is the fallback for anything else
            # that unwinds through here, the same as ``run_binary``.
            kill_process_group(proc)
            proc.wait()
            raise
    return subprocess.CompletedProcess(cmd, proc.returncode, stdout, stderr)
