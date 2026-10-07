"""Run-wide cancellation: SIGINT/SIGTERM stop a run promptly (#356).

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
  - :meth:`track` registers a subprocess (started with
    ``start_new_session=True`` so it is its own process-group leader) so
    :meth:`request` can kill its whole group the moment cancellation is
    requested, regardless of which thread started it.

A queued-but-not-yet-started ``ThreadPoolExecutor`` future is handled
separately, by ``executor.shutdown(cancel_futures=True)`` at each tree-flow
call site — stdlib concurrent.futures already refuses to start a cancelled
future, which is simpler and more precise than anything this module could
add on top.
"""

from __future__ import annotations

import os
import signal
import subprocess
import threading
from collections.abc import Iterator
from contextlib import contextmanager


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
    try:
        os.killpg(pgid, signal.SIGKILL)
    except ProcessLookupError:
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
        self._lock = threading.Lock()
        self._processes: set[subprocess.Popen[str] | subprocess.Popen[bytes]] = set()
        self._signum: int | None = None

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
        for proc in procs:
            kill_process_group(proc)
        return first

    @property
    def signum(self) -> int | None:
        """The signal that triggered cancellation, or ``None`` if none has."""
        return self._signum

    def is_set(self) -> bool:
        return self._event.is_set()

    def check(self) -> None:
        """Raise :class:`RunCancelledBySignal` if cancellation has been requested."""
        if self._event.is_set():
            raise RunCancelledBySignal("run cancelled")

    def sleep_or_raise(self, seconds: float) -> None:
        """Sleep up to *seconds*, raising :class:`RunCancelledBySignal` instead if
        cancellation is (or becomes) set before the sleep elapses.

        A cancellation-aware drop-in for ``time.sleep`` on any code path
        that might run on a worker thread — a real signal only ever
        interrupts a blocking ``time.sleep`` in the *main* thread.
        """
        if self._event.wait(timeout=max(0.0, seconds)):
            raise RunCancelledBySignal("run cancelled during backoff")

    @contextmanager
    def track(
        self, proc: subprocess.Popen[str] | subprocess.Popen[bytes]
    ) -> Iterator[None]:
        """Register *proc* so :meth:`request` kills its process group too.

        *proc* must have been started with ``start_new_session=True`` (POSIX)
        for the group-kill to reach only this process's own descendants, not
        this interpreter's own process group.
        """
        with self._lock:
            self._processes.add(proc)
        try:
            yield
        finally:
            with self._lock:
                self._processes.discard(proc)


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
