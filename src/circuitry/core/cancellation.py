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
`cof run`/`run-library` invocation, installed by
``cli.interrupts.sigterm_as_interrupt``. Its ``_SignalWatcher`` thread
calls :meth:`request` first, directly off the raw SIGINT/SIGTERM/SIGHUP
itself (#404) — the main-thread handler also calls it, idempotently, but
no longer relies on its return value to tell a first signal from a
second (``cli.interrupts._SignalCount`` does that instead). Worker-thread
code that would otherwise block
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
    down with it (#357).

A queued-but-not-yet-started ``ThreadPoolExecutor`` future is handled
separately, by ``executor.shutdown(cancel_futures=True)`` at each tree-flow
call site — stdlib concurrent.futures already refuses to start a cancelled
future, which is simpler and more precise than anything this module could
add on top.
"""

from __future__ import annotations

import contextvars
import logging
import os
import queue
import signal
import subprocess
import threading
import time
from collections.abc import Callable, Iterable, Iterator, Sequence
from concurrent.futures import Executor, Future
from contextlib import contextmanager
from multiprocessing.process import BaseProcess
from typing import Any, TypeVar

logger = logging.getLogger(__name__)

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
    :attr:`CancellationToken.armed` is set (#357). Otherwise the
    child's ``pgid`` *is* this interpreter's own ``getpgrp()``, so
    ``killpg`` on it would SIGKILL this process and its whole group along
    with the child — e.g. an unarmed timeout under the SDK, the MCP
    server, or any embedder that never installs ``cof run``'s signal
    handler. Guard against that case and kill just the child instead.

    ``PermissionError`` is ignored the same as ``ProcessLookupError``
    (#385 round 3, cause confirmed by the #385 review's own probe): once
    this call's own ``killpg`` has killed the group's last live member,
    that member is a zombie until something actually ``wait()``s on it
    (:func:`_communicate_promptly`/``run_tracked``'s own cleanup, which
    can run later than this) — on macOS, a further ``killpg`` at that
    pgid before the reap raises ``EPERM``, not ``ESRCH``, even sending
    the exact same signal to the exact same, still-correctly-owned pgid
    (confirmed directly: ``killpg(pgid, SIGKILL)`` a second time on a
    pgid whose one member a first ``SIGKILL`` already killed but nothing
    has reaped yet reliably raises ``PermissionError``, every time, on
    this platform). Not pgid reuse by some unrelated process: a reused
    pgid would belong to this same user on an ordinary dev machine, and
    ``killpg`` to a group you own does not raise ``EPERM`` — best-effort,
    same as the rest of this function.
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
        except (ProcessLookupError, PermissionError):
            pass
        return
    try:
        os.killpg(pgid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


def _isolated_pgid(
    proc: subprocess.Popen[str] | subprocess.Popen[bytes],
) -> int | None:
    """*proc*'s own process-group id, only when it is isolated in one of
    its own -- i.e. exactly the case :func:`kill_process_group` itself
    already treats as safe to ``killpg`` -- else ``None`` (#385 round 3).

    Captured once, right after ``Popen``, by :func:`run_tracked`: a pgid
    can never be reused while any process in it is still alive, so this
    stays valid for repeat kills for as long as the group has a single
    member left, including one born from a fork after the first kill
    (see :func:`_repeat_group_kill`).
    """
    if os.name != "posix":
        return None
    pid = getattr(proc, "pid", None)
    if pid is None:
        return None
    try:
        pgid = os.getpgid(pid)
    except ProcessLookupError:
        return None
    return None if pgid == os.getpgrp() else pgid


def _repeat_group_kill(pgid: int | None) -> bool:
    """``os.killpg(pgid, SIGKILL)`` again, ignoring ``ProcessLookupError``
    (the whole group is already gone) and ``PermissionError`` (the
    group's last live member is now a zombie nothing has reaped yet --
    see :func:`kill_process_group`'s own docstring for why that, not
    pgid reuse, is what actually raises it here) (#385 round 3).

    A real production race, not just a quirk of this suite's own test
    scripts: ``bash -c "a; b"`` forks a *child* process to run a non-tail
    command like ``b`` rather than exec'ing into it (macOS's /bin/bash
    3.2 always does this; other shells can too for a background job, a
    subshell, a pipeline stage). If a cancellation's own
    :func:`kill_process_group` call (a single ``killpg``) lands in the
    narrow window between that ``fork()`` and the new child actually
    joining its parent's process group at the kernel level, the new
    child is not yet a member of the group the signal was multicast to
    and survives it, invisibly: it keeps running, and because it
    inherited the same stdout/stderr pipes, ``_communicate_promptly``
    never sees EOF until it does. Calling this again on every slice
    while cancellation stays set (:func:`_communicate_promptly`) closes
    that window within one poll interval regardless of when exactly the
    fork lost the first race -- a pgid can never be reused while any
    member of it is still alive, so repeating the same ``killpg`` is
    always safe, never reaches a different, unrelated process.

    Returns whether this call found the group already gone
    (``ProcessLookupError``, i.e. ``ESRCH``) -- the one outcome that
    actually means the pgid is now free for the kernel to reuse; a live
    member or an unreaped zombie (``PermissionError``, see
    :func:`kill_process_group`'s own docstring) both still own it.
    :func:`_communicate_promptly`'s own retry loop stops calling this
    again for the same pgid once it returns ``True`` (#411): the leader
    that could fork the escaping child this function exists to keep
    chasing is itself gone the moment ``killpg`` reports ``ESRCH``, so
    there is nothing left in this group for a further call to reach,
    and continuing to call it for as long as something outside the
    group still holds the step's own pipe open would otherwise leave a
    window for that now-free pgid to be reused.
    """
    if pgid is None:
        return True
    try:
        os.killpg(pgid, signal.SIGKILL)
    except ProcessLookupError:
        return True
    except PermissionError:
        pass
    return False


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


#: Bound at import time, not read off `threading.Event` fresh inside
#: :meth:`CancellationToken.reset`: a test that monkeypatches
#: `threading.Event` itself for its own duration (e.g.
#: `test_as_completed_promptly_no_longer_uses_a_private_threading_event`)
#: must not also break every *other* test's own autouse teardown calling
#: `reset()` -- this name was already bound to the real class long before
#: any such patch could apply.
_Event = threading.Event


class CancellationToken:
    """Run-wide cancellation flag plus the subprocess registry it kills.

    One instance backs an entire `cof run`/`run-library` invocation — see
    the module docstring. Not thread-local: the whole point is that a
    signal, delivered to the main thread only, must be visible to every
    worker thread a tree-flow branch is running on.
    """

    def __init__(self) -> None:
        self._event = _Event()
        # RLock, not a plain Lock (#356): the signal handler's
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

        Builds a fresh :class:`threading.Event` rather than calling
        ``clear()`` on the existing one: an earlier version of this
        method relied on the reasoning that ``sigterm_as_interrupt``
        only ever calls this before its own handlers are installed or
        after they've been restored, so a signal landing on either call
        can't reach a handler that could leak this lock -- that
        reasoning was wrong. The *default* SIGINT handler installed at
        either of those points still raises ``KeyboardInterrupt``
        asynchronously,
        same as always, and :meth:`request`'s own ``Event.set()`` (the
        one call on this object that *does* still touch its lock, since
        :meth:`sleep_or_raise` is lock-free on the main thread) could
        already have left that lock owned by the time this runs -- in
        which case the old ``clear()`` call would try to take the same
        non-reentrant lock on the same thread and deadlock this method,
        and every later run in the same process, forever. Replacing the
        object outright instead of touching its lock makes that leak (if
        one ever happened) irrelevant: nothing ever touches the old
        ``Event`` again. Safe because no worker thread ever waits on
        :attr:`_event` at either call site this runs from -- entry, before
        any branch exists; exit, after every one has already been joined.
        """
        self._event = _Event()
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
        did. Nothing uses that return value for the first/second-signal
        decision any more (#404): ``cli.interrupts``' watcher thread
        ordinarily calls this before the main-thread handler gets a
        chance to, so by the time the handler's own call happens, even
        for a genuinely first signal, it would already read ``False`` --
        ``cli.interrupts._SignalCount`` tracks that instead, independent
        of this call's own return value.
        """
        first = not self._event.is_set()
        if first:
            if signum is not None:
                self._signum = signum
            # Only ever set once: a repeat call -- the watcher thread's
            # own, racing the main thread's handler, or a second
            # signal's handler that reaches here regardless -- skips
            # `Event.set()`'s own `Condition`-backed call outright
            # rather than taking its lock again for no reason, the same
            # #555 exposure against this token's own `Event` instead of
            # a `Future`'s.
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
        group (``start_new_session``) while this is true (#356)
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

        Polls in short slices rather than one single wait for the whole
        backoff (#385 follow-up, same reasoning as
        :func:`as_completed_promptly`): a retry backoff can run on the
        *main* thread too (a sequential chain's own retry, not just a
        tree-flow worker's), and a single long wait there is exactly the
        kind of blocking call that defers a signal delivered to some
        *other* thread (a ``Live`` refresh thread, the MCP client's own
        pool thread) until it returns on its own — this thread never gets
        to execute the bytecode that would run the pending handler until
        then. Slicing the wait means this thread returns to bytecode at
        least every poll interval regardless of which thread the signal
        reached.

        On the *main* thread, each slice is a plain ``time.sleep`` plus
        :attr:`_event`'s lock-free :meth:`~threading.Event.is_set` --
        never :meth:`~threading.Event.wait` there: ``Event.wait`` sits on
        a ``threading.Condition``, whose pure-Python ``__exit__`` a first
        signal's own handler raising mid-release would leak, the same
        #555 mechanism found against a ``Future``'s lock, just against
        this token's ``Event`` instead -- and the main thread is exactly
        where that handler runs. A worker thread never runs a signal
        handler directly (CPython only ever raises one on the main
        thread), so it keeps the lower-latency ``Event.wait`` -- there
        is nothing for it to leak.
        """
        if self.in_cleanup():
            time.sleep(max(0.0, seconds))
            return
        remaining = max(0.0, seconds)
        # Lock-free on the main thread: `Event.wait` sits on a
        # `Condition`, whose own pure-Python `__exit__` a first signal's
        # handler raising mid-release would leak (the same #555
        # mechanism, against this token's own `Event` instead of a
        # `Future`'s) -- and the main thread is exactly where that
        # handler runs. `time.sleep` plus `Event.is_set()` (a plain
        # attribute read with no lock at all) has no such exposure: a
        # signal interrupts the sleep itself, the same way it always
        # interrupted a bare `time.sleep` before this module existed.
        # Worker threads never run a signal handler
        # directly (CPython only ever raises one on the main thread), so
        # they keep the lower-latency `Event.wait` -- it has nothing to
        # leak there.
        on_main_thread = threading.current_thread() is threading.main_thread()
        while True:
            slice_seconds = min(remaining, _SIGNAL_POLL_SECONDS)
            if on_main_thread:
                time.sleep(slice_seconds)
                cancelled = self._event.is_set()
            else:
                cancelled = self._event.wait(timeout=slice_seconds)
            if cancelled:
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
        process down with it too (#357).

        :func:`run_tracked` already calls :meth:`check` before ``Popen``,
        but cancellation can still be requested in the gap between that
        check and this registration -- :meth:`request` only kills the
        processes it can already see, so a process added right after it
        took its snapshot would otherwise run to completion unseen (#357).
        Close that race here: if cancellation is already set
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


#: Main-thread-only (never touched off it, and never locked -- see
#: `deferred_signal_region`'s own docstring): how many nested deferral
#: regions are currently active.
_defer_depth = 0

#: Main-thread-only: a first signal's own exception, parked here instead
#: of raised immediately, for as long as `_defer_depth` is nonzero. At
#: most one at a time -- a second signal still ends the process at once
#: (`cli.interrupts`'s own handler), region or not, so there is never a
#: second one to park.
_deferred_signal_exception: BaseException | None = None


def is_deferring_first_signal() -> bool:
    """Whether the main thread is currently inside a
    :func:`deferred_signal_region` (#555) -- read only by
    ``cli.interrupts``'s own signal handler, to decide whether a first
    signal's exception can be raised immediately (as before #555) or must
    be parked with :func:`defer_first_signal_exception` until every
    region the main thread is nested in has exited.
    """
    return _defer_depth > 0


def defer_first_signal_exception(exc: BaseException) -> None:
    """Park *exc* instead of letting the signal handler raise it now
    (#555). Called only by ``cli.interrupts``'s own handler, only for a
    first signal, only while :func:`is_deferring_first_signal` is true.
    """
    global _deferred_signal_exception
    _deferred_signal_exception = exc


def _take_deferred_signal_exception() -> BaseException | None:
    global _deferred_signal_exception
    exc = _deferred_signal_exception
    _deferred_signal_exception = None
    return exc


def check_deferred_signal() -> None:
    """Raise a signal's exception parked by
    :func:`defer_first_signal_exception`, if one is waiting (#555) --
    unless the main thread is still nested inside a
    :func:`deferred_signal_region` (checked here too, not just by callers:
    a cancel path's own wait -- :func:`wait_for_cancelled_branches` via
    :func:`as_completed_promptly`'s poll -- runs *inside* the same region
    that wraps the ``executor.shutdown(cancel_futures=True)`` ahead of it,
    specifically so a first signal landing during that wait doesn't cut it
    short (#555): the exception stays parked until every
    already-running branch has actually finished and the outermost region
    exits, exactly as it would if it had landed anywhere else inside one).

    Call this from a coordination loop's own poll, outside any lock --
    the whole point of parking the exception in the first place was to
    never raise it while the main thread owns a lock a worker thread also
    needs, so a call site that isn't sure it qualifies should rely on
    :func:`deferred_signal_region` exiting instead, which raises
    automatically (below), rather than call this itself from inside one.
    """
    if threading.current_thread() is not threading.main_thread():
        # The parked exception is the main thread's own: a coordination
        # loop on a worker thread (a tree nested inside a tree branch) must
        # never take it and raise it there instead.
        return
    if _defer_depth > 0:
        return
    exc = _take_deferred_signal_exception()
    if exc is not None:
        raise exc


@contextmanager
def deferred_signal_region() -> Iterator[None]:
    """Mark a region where the main thread is about to touch a lock a
    worker thread also takes -- an ``add_done_callback``/``cancel`` call
    on a :class:`concurrent.futures.Future` that isn't finished yet,
    ``executor.submit`` (which can call ``Thread.start``, itself a wait on
    an ``Event``), or ``executor.shutdown(cancel_futures=True)`` (which
    calls ``Future.cancel()`` on every not-yet-started one) (#555).

    A first SIGINT/SIGTERM/SIGHUP's own handler ordinarily raises
    straight into whatever bytecode the main thread happens to be running
    (that's the entire point of #404's watcher closing the window before
    it). Unlike a C-level ``threading.Lock``, whose ``__enter__``/
    ``__exit__`` are single, uninterruptible C calls, a
    ``threading.Condition`` (what every ``Future``'s own ``_condition``
    is, and what ``Event``/``Semaphore`` are built on) has a *pure-Python*
    ``__exit__`` -- a signal's registered handler can run, and raise,
    between that frame being entered and the line inside it that actually
    releases the lock, leaking it held forever. A worker thread that then
    tries to take the same lock (``set_result``/``set_exception`` on the
    same future, another ``executor.submit``, ...) blocks forever on it:
    the exact deadlock #555 found (confirmed deterministically with
    ``sys.settrace`` raising ``KeyboardInterrupt`` from a 'call' event on
    ``Condition.__exit__``).

    Nesting composes via the depth counter: entering again while already
    inside one more region just counts; the exception (if a first signal
    arrives at all while any region is active) is only actually raised
    once the outermost one exits, by which point the main thread holds
    none of these locks. A second signal is unaffected -- ``cli.
    interrupts``'s own handler still takes the immediate ``os._exit``
    path for one (or the SIGHUP no-op), region or not; there would be
    nothing to defer a second time regardless, since only ever one
    exception is parked.
    """
    if threading.current_thread() is not threading.main_thread():
        # A Python-level signal handler only ever runs on the main thread,
        # so nothing is raised asynchronously on this one and there is
        # nothing to defer. Counting this region would also make the main
        # thread's own handler defer a signal it should raise, and hand
        # that exception to this thread at the region's exit.
        yield
        return
    global _defer_depth
    _defer_depth += 1
    try:
        yield
    finally:
        _defer_depth -= 1
        if _defer_depth == 0:
            check_deferred_signal()


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

    The *caller* must already be inside its own :func:`deferred_signal_region`
    that also covers recording the future this returns somewhere the
    cancellation path will see it (e.g. ``futures[future] = idx`` right
    after this call, in the same region) (#555 follow-up). The actual lock
    this call itself risks leaking isn't ``Thread.start``'s own startup
    ``Event`` -- by the time ``submit`` returns from waiting on that, the
    new thread has already called ``Event.set()`` and never touches that
    lock again, so a signal leaking it there wedges nothing. It's
    ``ThreadPoolExecutor._adjust_thread_count``'s own ``_idle_semaphore``,
    which ``submit`` tries to acquire (``timeout=0``, still a
    ``Condition``-backed call) every time, and which every worker thread
    releases after each work item it finishes -- a first signal's handler
    raising mid-release of *that* lock wedges the next worker to call
    ``_idle_semaphore.release()``. So this call itself needs the same
    protection :func:`deferred_signal_region` gives any other call in
    #555's audit. It does not open one of its own, though: nesting a
    region here that closes before returning would raise the moment *this*
    call's own region exits, before the future it just got back is ever
    recorded by the caller -- silently losing track of a branch the pool
    already started running, with nothing left to wait for it, cancel it,
    or even notice it exists (confirmed: this exact bug, #555 follow-up).
    Asserting the caller already holds one, rather than silently reusing
    its own narrower one, is what makes forgetting that loud instead of a
    rare, signal-timing-dependent lost branch.
    """
    assert threading.current_thread() is not threading.main_thread() or (
        is_deferring_first_signal()
    ), (
        "submit_with_context must be called inside the caller's own "
        "deferred_signal_region, covering both this call and recording "
        "its returned future -- see this function's own docstring"
    )
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
    bounded ``queue.SimpleQueue.get(timeout=poll_seconds)`` means the main
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
    not) -- wrapped in :func:`deferred_signal_region` (#555), since that
    one touch is still a main-thread ``with future._condition:`` a worker
    thread's own ``set_result``/``set_exception`` also takes, and a first
    signal's handler raising mid-release of it would leak the lock and
    deadlock that worker (confirmed deterministically, not just in
    theory — see that function's own docstring).

    The repeating part of this loop used a private ``threading.Event``
    before #555: ``Event.wait``/``.set()`` are themselves built on a
    ``threading.Condition``, so a signal landing on the main thread
    *inside* ``wake.wait(poll_seconds)`` -- not registering the
    callback, the wait itself -- had exactly the same lock-leak exposure,
    just against this loop's own private lock instead of a future's.
    ``queue.SimpleQueue`` is a plain ``get``/``put`` with no Python-level
    ``Condition`` anywhere in either: the worker's callback calling
    ``put`` and this loop's own ``get(timeout=poll_seconds)`` are both
    single, uninterruptible C calls, the same way a bare ``threading.
    Lock`` is -- removing the lock entirely removes the exposure, rather
    than needing its own ``deferred_signal_region``.
    """
    fs = list(futures)
    done: queue.SimpleQueue[Future[Any]] = queue.SimpleQueue()

    def _on_done(f: Future[Any]) -> None:
        done.put(f)

    with deferred_signal_region():
        for f in fs:
            f.add_done_callback(_on_done)

    remaining = len(fs)
    while remaining:
        # Outside any lock -- see `check_deferred_signal`'s own
        # docstring -- so a signal parked by a nested
        # `deferred_signal_region` elsewhere (not this function's own,
        # which already raised at its exit above if it had one) still
        # surfaces within one `poll_seconds` rather than only once this
        # whole wait finally ends.
        check_deferred_signal()
        try:
            f = done.get(timeout=poll_seconds)
        except queue.Empty:
            continue
        remaining -= 1
        yield f


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

    No :func:`deferred_signal_region` of its own (#555 audit): every lock
    this function's own body could touch is already inside
    :func:`as_completed_promptly`'s -- this is a thin ``for _ in ...:
    pass`` over it and touches nothing else.
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

    Each ``sem.acquire(...)`` call is wrapped in its own
    ``deferred_signal_region``: a plain ``threading.Semaphore`` is
    ``Condition``-backed, the same #555-style exposure the dispatch
    sites and :class:`core.concurrency.RunConcurrencyLimiter` are
    audited for, and this is called on the
    main thread whenever a chain runs sequentially (``adapters.
    cyberdiner``'s own ``max_in_flight`` gate). Unlike
    ``RunConcurrencyLimiter.acquire()``, this function's own caller has
    no ``held`` list and no ``try`` block active yet when it calls this
    -- its ``finally: sem.release()`` is written *after* this call
    returns, so a deferred exception raised at this function's own
    region exit, right after a successful acquire, would otherwise
    escape before that ``finally`` is ever registered, leaking the slot
    with nothing left to release it. Caught here instead: if the region
    raises after the semaphore was actually acquired, this releases it
    again itself before re-raising, so the caller never has to.
    """
    while True:
        acquired = False
        try:
            with deferred_signal_region():
                acquired = sem.acquire(timeout=poll_seconds)
        except BaseException:
            if acquired:
                sem.release()
            raise
        if acquired:
            return


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


def _feed_stdin(stdin: Any, data: bytes) -> None:
    """Write the whole of *data* (already encoded to bytes by the caller)
    to *stdin* and close it, from a thread of its own (#385 follow-up,
    see ``_communicate_promptly``).

    Always bytes, never ``str``: encoding happens in the calling thread,
    before this thread starts, specifically so a ``UnicodeEncodeError``
    (a lone surrogate, or a character the locale's own encoding can't
    represent) raises synchronously there and fails the step the same
    way it always has, rather than surfacing only as a traceback from
    this thread's own excepthook while the child just sees EOF (#385).

    ``BrokenPipeError`` means the reader (the child, or whatever still
    held its read end) is gone — nothing left to write to, so this just
    stops, the same as ``subprocess``'s own ``_stdin_write`` does.
    """
    try:
        stdin.write(data)
    except BrokenPipeError:
        pass
    finally:
        try:
            stdin.close()
        except (BrokenPipeError, OSError):
            pass


def _read_lines_promptly(
    stdout: Any,
    on_line: Callable[[str], None],
    lines: list[str],
    decode_errors: list[BaseException],
) -> None:
    """Read *stdout* line by line, append each raw line to *lines* and call
    *on_line* with it (trailing newline stripped), until EOF (#636 follow-up
    to the stdin-feeder thread below).

    Runs on a thread of its own, started right after ``Popen`` and joined
    once the tracked process has been waited on, so it never changes
    ``run_tracked``'s own poll cadence or timeout/cancellation behaviour --
    it only gives a caller (today, the ``agent`` tool) each line as it
    arrives instead of only once the whole call finishes. EOF arrives on
    its own once the child's write end closes, whether that is because the
    child exited normally or because its process group was killed -- the
    same reasoning :func:`_feed_stdin` already relies on for the opposite
    direction.

    *on_line* itself is never allowed to stop this loop: a failure inside
    it (a disk-full transcript append, say) would otherwise leave stdout
    undrained and unclosed, so the child either blocks on a full pipe
    until its own timeout or sees EPIPE/SIGPIPE mid-session -- either way
    corrupting the output ``run_tracked``'s own end-of-call parse still
    needs to be authoritative. So any exception it raises is logged once
    and *on_line* is never called again for the rest of this stream, while
    reading into *lines* continues exactly as if ``on_line`` were never
    passed at all (#636 follow-up).

    With ``text=True``, invalid bytes mid-stream raise a
    ``UnicodeDecodeError`` (a ``ValueError``) out of the ``for`` loop below
    instead of just skipping the bad line -- ``communicate()`` itself raises
    exactly the same error, loudly, which is what a step that hits one must
    still do (#636 follow-up). Stopping here instead, the way a bare
    ``except (ValueError, OSError): pass`` once did, closed ``stdout`` while
    the child could still be writing to it, so the child saw EPIPE/SIGPIPE
    mid-session and ``run_tracked`` returned truncated output with no error
    at all. Instead, the decode error is recorded into *decode_errors* for
    ``run_tracked`` to re-raise once this thread is joined, and the rest of
    the child's output is drained from the raw byte stream underneath --
    ``errors="replace"``, since by then nothing reads these lines for
    anything but keeping the pipe open to EOF.
    """
    on_line_failed = False
    decode_error: BaseException | None = None
    try:
        for raw_line in stdout:
            lines.append(raw_line)
            if on_line_failed:
                continue
            try:
                on_line(raw_line.rstrip("\n"))
            except Exception:
                on_line_failed = True
                logger.exception("run_tracked on_line callback failed; no longer calling it")
    except ValueError as exc:
        decode_error = exc
        buffer = getattr(stdout, "buffer", None)
        if buffer is not None:
            try:
                lines.extend(raw_bytes.decode(stdout.encoding, errors="replace") for raw_bytes in buffer)
            except OSError:
                pass
    except OSError:
        pass
    finally:
        try:
            stdout.close()
        except OSError:
            pass
        if decode_error is not None:
            decode_errors.append(decode_error)


def _communicate_promptly(
    proc: subprocess.Popen[Any],
    *,
    input: str | bytes | None,
    timeout: float | None,
    pgid: int | None = None,
    poll_seconds: float = _SIGNAL_POLL_SECONDS,
    pgid_seen_gone: list[bool] | None = None,
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

    *input* is never handed to ``communicate()`` itself (a regression:
    CPython 3.11's POSIX ``Popen._communicate`` only ever registers
    ``stdin`` for writing, and only ever closes it, on the *first* call —
    ``self.stdin and input`` gates the selector registration, ``not
    self._communication_started`` gates the close. Passing *input* again
    on a retry raises ``ValueError: Cannot send input after starting
    communication``, so an earlier version of this function passed it
    only on the first call and ``None`` after — but then any input not
    fully written inside that first short slice was simply never resumed
    or closed, and a child that hadn't yet finished reading stdin waited
    for EOF until *timeout* itself, or forever with no timeout). Instead,
    if there is input to send *and* ``proc.stdin`` is a real pipe (a
    ``subprocess.Popen`` always has one whenever ``run_tracked`` passed
    ``input``; a bare duck-typed test double usually does not), a helper
    thread (the same shape CPython's own Windows ``communicate`` uses for
    stdin) writes and closes it on its own, and ``proc.stdin`` is set to
    ``None`` first so ``communicate`` — now only ever called with
    ``input=None`` — never touches it itself: stdout/stderr are its only
    job here. The thread isn't joined: once the deadline is hit or the
    process is killed, the reader going away unblocks a pending write
    with ``BrokenPipeError`` on its own almost at once, and joining
    unconditionally would reintroduce the exact unbounded-wait bug class
    this module exists to close, this time inside the one place meant to
    guarantee *timeout* itself. Without a real ``proc.stdin`` to hand to a
    thread, this falls back to the pre-fix shape (*input* on the first
    call, ``None`` after) — safe there only because none of this
    catalog's own ``Popen`` test doubles actually model a retry losing
    unwritten input.

    *pgid*, when given (an isolated child -- see its own helper
    ``_isolated_pgid``), is repeat-killed (``_repeat_group_kill``) after
    every slice in which cancellation is still set outside
    ``CancellationToken.cleanup``: the single ``killpg`` call
    ``kill_process_group`` already sent from the signal handler can race
    a child the tracked process forks right at that instant (see that
    helper's own docstring), so this keeps retrying it, at the same
    cadence as everything else here, until ``communicate`` actually sees
    EOF. Stops calling it again, for the rest of this call, the first
    time it reports the group already gone (#411) -- the leader can
    exit on its own right before the signal, in which case
    ``CancellationToken.request()``'s own ``kill_process_group`` finds
    it already reaped and sends no ``killpg`` at all, leaving this loop
    to send the first one; once that confirms the group is empty, the
    pgid is free for the kernel to reuse, and a further ``killpg`` here
    could then reach an unrelated process that happened to get it,
    rather than this one's own already-gone group.

    *pgid_seen_gone*, when given, records that same outcome for
    ``run_tracked`` to see even when this call raises instead of
    returning: a plain local variable here would be lost the moment an
    exception unwinds through this function, and ``run_tracked``'s own
    ``except`` blocks need to know not to send one more ``killpg`` to a
    pgid this call already confirmed empty (#411 follow-up).
    """
    # getattr, not proc.stdin: a Popen-like test double that never
    # models a `stdin` attribute at all (most of this catalog's own
    # subprocess.Popen fakes) must keep working exactly as before --
    # for those, falling back to passing *input* to `communicate()` on
    # the first call only (the pre-fix shape) is safe, since none of
    # them actually exercise a retry that would lose it.
    stdin = getattr(proc, "stdin", None)
    threaded_stdin = input is not None and stdin is not None
    if threaded_stdin:
        # Encoded here, on the calling thread -- not inside `_feed_stdin`'s
        # own thread -- so a `UnicodeEncodeError` surfaces synchronously
        # and fails this step the same way it always has, instead of
        # only ever printing from the writer thread's own excepthook
        # while the child just sees EOF (#385). `text=True`
        # gives a `TextIOWrapper` with its own `.buffer`, the raw byte
        # stream `communicate()` itself writes/reads through -- the same
        # invariant `subprocess.Popen.communicate` itself enforces (text
        # mode takes `str` input, binary mode takes `bytes`) means *input*
        # matches whichever of these two branches applies.
        assert stdin is not None
        is_text_mode = hasattr(stdin, "buffer")
        if is_text_mode:
            assert isinstance(input, str)
            data: bytes = input.encode(stdin.encoding, stdin.errors)
            # `detach()`, not a plain `.buffer` read (#385 review
            # regression, confirmed by direct measurement): once this
            # function returns, `stdin` -- the local name for the
            # `TextIOWrapper` -- goes out of scope and is garbage
            # collected almost at once. Its own finalizer still calls
            # `close()`, which flushes *and closes the buffer it wraps*
            # even though nothing was ever written through the wrapper
            # itself -- on *this*, the main thread. That flush blocks for
            # as long as the writer thread's own in-flight `write()` holds
            # the buffer's internal lock (a pipe nothing reads never
            # unblocks it), in C, where no pending signal is ever checked
            # (measured: ~0.03s before this regression, ~6s after, with a
            # SIGINT at 1.5s only surfacing at 6.08s). `detach()`
            # disconnects the wrapper from its buffer first, so its own
            # finalizer has nothing left to flush or close -- the writer
            # thread becomes the buffer's only owner.
            raw_stdin = stdin.detach()
        else:
            raw_stdin = stdin
            assert isinstance(input, bytes)
            data = input
        proc.stdin = None
        threading.Thread(
            target=_feed_stdin, args=(raw_stdin, data), daemon=True
        ).start()
    if timeout is None:
        deadline = None
    else:
        deadline = time.monotonic() + timeout
        full_timeout = timeout
    first = True
    pgid_gone = False
    while True:
        if deadline is None:
            slice_timeout = poll_seconds
        else:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise subprocess.TimeoutExpired(proc.args, full_timeout)
            slice_timeout = min(poll_seconds, remaining)
        try:
            if threaded_stdin:
                return proc.communicate(timeout=slice_timeout)
            return proc.communicate(
                input=input if first else None, timeout=slice_timeout
            )
        except subprocess.TimeoutExpired:
            first = False
            token = get_token()
            if pgid is not None and not pgid_gone and token.is_set() and not token.in_cleanup():
                pgid_gone = _repeat_group_kill(pgid)
                if pgid_gone and pgid_seen_gone is not None:
                    pgid_seen_gone.append(True)


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
    on_line: Callable[[str], None] | None = None,
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

    ``on_line``, when given, is called with each line of stdout (trailing
    newline stripped) as it arrives, from a reader thread of its own — the
    poll cadence above, the stdin feeder thread, the repeat group kill and
    this function's timeout/cancellation behaviour are all unchanged either
    way (#636). The returned ``CompletedProcess.stdout`` is unaffected: the
    reader thread only observes the bytes, it doesn't consume them instead
    of ``communicate()``. Invalid bytes mid-stream (a ``UnicodeDecodeError``
    with ``text=True``) still fail this call once the child has fully exited
    and the reader thread has drained the rest of its output, the same as a
    plain ``communicate()`` always has for the same bytes (#636 follow-up).
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
    # Captured once, right after Popen, not re-derived later: a pgid
    # can never be reused while any member of it is still alive, so
    # this stays valid through every repeat kill below, including one
    # needed because of a process this same pgid's own leader forked
    # after this (#385 round 3, see _repeat_group_kill's own docstring).
    pgid = _isolated_pgid(proc)
    # A plain local in `_communicate_promptly` itself would be lost the
    # moment an exception unwinds through it -- this list is how it
    # tells the `except` blocks below not to repeat a `killpg` against a
    # pgid it already confirmed empty (#411 follow-up).
    pgid_seen_gone: list[bool] = []
    # `on_line`'s reader thread owns `proc.stdout` from here on -- the same
    # steal-the-attribute trick `_communicate_promptly` already plays with
    # `proc.stdin`, so `communicate()` below never touches it and `with
    # proc:`'s own `__exit__` never double-closes it.
    stdout_lines: list[str] | None = None
    reader_thread: threading.Thread | None = None
    reader_started = False
    decode_errors: list[BaseException] = []
    if on_line is not None and proc.stdout is not None:
        stdout_lines = []
        raw_stdout = proc.stdout
        proc.stdout = None
        reader_thread = threading.Thread(
            target=_read_lines_promptly,
            args=(raw_stdout, on_line, stdout_lines, decode_errors),
            daemon=True,
        )
    with proc, token.track(proc):
        try:
            # Started only once *proc* is already tracked -- not right
            # after `Popen` -- so a signal delivered between stealing
            # `proc.stdout` above and getting here, or a failed
            # `Thread.start()` itself, can't orphan the CLI: either way
            # `proc` is already inside `token.track(proc)` and this `with`
            # block's own cleanup below still runs (#636 follow-up).
            if reader_thread is not None:
                reader_thread.start()
                reader_started = True
            try:
                stdout, stderr = _communicate_promptly(
                    proc, input=input, timeout=timeout, pgid=pgid,
                    pgid_seen_gone=pgid_seen_gone,
                )
            except subprocess.TimeoutExpired:
                kill_process_group(proc)
                proc.wait()
                # One more, after the process we were tracking has actually
                # exited: a child it forked at the exact instant of the kill
                # above must not be left running past a timed-out step
                # either -- unless `_communicate_promptly` already confirmed
                # the group empty, in which case the pgid may already be
                # free for the kernel to reuse, and this would risk reaching
                # an unrelated process instead of this one's own.
                if not pgid_seen_gone:
                    _repeat_group_kill(pgid)
                raise
            except BaseException:
                # Cancellation already killed this process's group from the
                # signal handler; this is the fallback for anything else
                # that unwinds through here, the same as ``run_binary``.
                kill_process_group(proc)
                proc.wait()
                if not pgid_seen_gone:
                    _repeat_group_kill(pgid)
                raise
        finally:
            # The process has been waited on by now on every path above
            # (normally inside `communicate()`, or by the `except` blocks'
            # own `proc.wait()`), so the child's stdout write end is closed
            # and this thread is at or near EOF already -- bounded anyway,
            # so a bug here can never join forever.
            if reader_thread is not None and reader_started:
                reader_thread.join(timeout=5)

    if stdout_lines is not None:
        stdout = "".join(stdout_lines)
    if decode_errors:
        # Re-raised loudly, the same as a plain `communicate()` always has
        # for the same bytes -- a decode error must fail this step, not
        # return it truncated with no error at all (#636 follow-up).
        raise decode_errors[0]
    return subprocess.CompletedProcess(cmd, proc.returncode, stdout, stderr)
