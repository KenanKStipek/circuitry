"""SIGINT/SIGTERM/SIGHUP-as-cancellation for the CLI's own run commands (#338, #356).

Ctrl-C/SIGINT already raises ``KeyboardInterrupt`` in the main thread via
CPython's default handler, and ``runtime_shim.run`` treats that exactly
like any other failure (#270 F6). SIGTERM (``kill <pid>``, a process
manager, a system shutdown) has no such default — the interpreter just
dies — so a run killed that way wrote neither ``--out`` nor its persistence
snapshot and could never be resumed.

``sigterm_as_interrupt`` installs handlers for *both* signals, for the
duration of one run:

  - SIGTERM raises :class:`SigTermInterrupt` in the main thread — a
    ``KeyboardInterrupt`` subclass, so every place that already treats
    `KeyboardInterrupt` as "an interrupted, still-resumable run" (the
    ``except (Exception, KeyboardInterrupt)`` in ``runtime_shim.run``, the
    ``BaseException`` catch around ``finally:`` in ``core.dynamic``) keeps
    working unchanged, while callers that care which signal it was can
    still ``isinstance`` for this specific subclass to pick exit code 143
    over 130.
  - SIGINT raises plain ``KeyboardInterrupt``, the same as CPython's own
    default handler — installing a handler for it at all is only needed so
    the *first* SIGINT, like the first SIGTERM, also calls
    ``core.cancellation.CancellationToken.request`` (#356): that is what
    lets a tree-flow branch running on a worker thread — which a signal
    never reaches directly — see that the run was cancelled and stop
    promptly instead of running to completion while the main thread waits.
  - A watcher thread sets the run's cancellation flag and kills every
    tracked process directly off the raw signal itself, via
    ``signal.set_wakeup_fd`` (#404) — see :class:`_SignalWatcher`'s own
    docstring for why the main-thread handler above cannot be the only
    thing that does this.
  - A first signal's own exception is not always raised the instant the
    handler below runs: while the main thread is inside one of
    ``core.cancellation``'s own ``deferred_signal_region`` scopes (#555
    -- a handful of ``concurrent.futures`` call sites in ``core.dynamic``/
    ``core.loop`` that briefly touch a lock a worker thread also takes),
    it is parked instead and raised once that region exits, by which
    point the main thread holds none of those locks. ``request()`` still
    runs immediately either way -- only the exception that unwinds the
    call stack is delayed, never the token's own flag or the kill of
    every tracked process.
  - SIGHUP (a terminal hangup: the terminal window closing, an SSH
    disconnect) is handled exactly like SIGTERM — a hangup kills the
    terminal, not this process's session, and with every tracked child
    started in its own process group (``core.cancellation.run_tracked``)
    a bare, unhandled SIGHUP would otherwise end `cof` with no cleanup at
    all while those children kept running as orphans. Left alone if it is
    already ignored on entry (``signal.getsignal(signal.SIGHUP) is
    signal.SIG_IGN`` — e.g. launched under ``nohup``, which is exactly the
    mechanism by which a caller opts out of this), and skipped entirely on
    a platform with no SIGHUP (Windows).
  - A *second* SIGINT/SIGTERM, while the first one's cleanup
    (``finally:`` blocks, state persistence) is still running, ends the
    process at once via ``os._exit`` — the same exit code, no traceback —
    rather than escaping as an uncaught exception from inside that cleanup.
  - SIGHUP never counts as that second signal (#357 follow-up): a closed
    terminal can deliver SIGHUP to a foreground job twice, a millisecond
    or so apart (observed from an interactive zsh; a plain `kill -HUP`
    or macOS's own bash only ever send one), and the second one landing
    inside this same signal-handling window must not race the first
    one's own cleanup to ``os._exit``. Once cancellation has already been
    requested by *any* signal, every further SIGHUP here is a pure no-op.
    This scope also leaves SIGHUP set to ``SIG_IGN`` — instead of
    restoring whatever handler it found on entry — when it exits with
    cancellation already requested, so a SIGHUP arriving after `cof`
    stops watching for it (while `--out`/the `--last` stash are still
    being written — ``runtime.persistence`` is already written by this
    point, inside the run itself) cannot kill the process before that
    finishes; it is never set to ``SIG_IGN`` any earlier than that, since
    a step can still start a child in here and a child inherits
    ``SIG_IGN`` across ``exec`` the same way `nohup` relies on.
    A second SIGINT/SIGTERM still ends the process at once as above.

Only `cof run`/`run-library` install this, only while they run, and only on
the main thread: ``signal.signal`` itself raises off the main thread, and
even if it didn't, an embedder (the SDK, the MCP server, the REST host)
calling into this package from a worker thread must keep its own signal
handling — this module must never install anything on its behalf.
"""

from __future__ import annotations

import os
import signal
import threading
from collections.abc import Iterator
from contextlib import contextmanager
from types import FrameType

from ..core.cancellation import (
    defer_first_signal_exception,
    get_token,
    is_deferring_first_signal,
)

#: Conventional POSIX exit code for each signal (128 + signal number) —
#: what a second SIGINT/SIGTERM during cleanup exits with, matching the
#: code the first signal's own (uninterrupted) cleanup path would have
#: produced. No SIGHUP entry: SIGHUP never takes this path (#357
#: follow-up) — its own first-signal exit code (129) is computed in
#: `cli.app` instead, from `result.sighup`.
_EXIT_CODES: dict[int, int] = {signal.SIGINT: 130, signal.SIGTERM: 143}


class SigTermInterrupt(KeyboardInterrupt):
    """Raised in the main thread when SIGTERM arrives during a run."""


class SigHupInterrupt(KeyboardInterrupt):
    """Raised in the main thread when SIGHUP (a terminal hangup) arrives
    during a run — treated exactly like :class:`SigTermInterrupt`."""


class _SignalCount:
    """How many countable signals the main-thread handler has itself seen
    so far this scope — the first-vs-second-signal decision (#404).

    Deliberately not derived from ``CancellationToken.request()``'s own
    return value, the way it was before this class existed: once
    :class:`_SignalWatcher` is installed, that call is no longer the
    first thing to see a given signal — the watcher thread's own call to
    ``request()`` off the raw signal itself (see that class's docstring)
    now ordinarily beats the main thread's Python-level handler to it, so
    by the time the handler below runs, even for a genuinely *first*
    signal, ``request()`` would already report ``False``. Counting here
    instead keeps the first/second decision exactly what it always was:
    whether *this handler* has run before in this scope, not whether the
    token's flag happened to already be set by the time it got around to
    checking.
    """

    def __init__(self) -> None:
        self._count = 0

    def note(self) -> bool:
        """Record one more countable signal; return whether this is the
        first one this scope has seen."""
        first = self._count == 0
        self._count += 1
        return first


def _make_handler(exc: type[BaseException], signal_count: _SignalCount):
    def _handler(signum: int, frame: FrameType | None) -> None:
        del frame
        # Idempotent, and still the only thing that sets the token's flag
        # and kills tracked processes on a platform with no `_SignalWatcher`
        # at all (Windows -- `signal.set_wakeup_fd` there requires a
        # socket, not a plain `os.pipe()` fd, see `sigterm_as_interrupt`)
        # -- never delete this call, even though on POSIX `_SignalWatcher`
        # now ordinarily beats it to the same `request()`.
        get_token().request(signum=signum)
        if not signal_count.note():
            if signum == getattr(signal, "SIGHUP", None):
                # Every further SIGHUP is a no-op once cancellation has
                # already been requested by any signal (#357 follow-up) —
                # never the `os._exit` a second SIGINT/SIGTERM takes, so a
                # closed terminal's own double-hangup can't race the first
                # one's cleanup.
                return
            # A second SIGINT/SIGTERM while cleanup from the first is still
            # running ends the run at once, same exit code, no traceback
            # (#356) — the first signal's own cleanup gets no further say.
            os._exit(_EXIT_CODES.get(signum, 128 + signum))
        if is_deferring_first_signal():
            # The main thread is inside a `core.cancellation.
            # deferred_signal_region` -- ordinarily touching a lock a
            # worker thread also takes (#555). Raising `exc` here, into
            # whatever bytecode that is, risks leaking that lock forever
            # if it lands between the region's own `__exit__` being
            # entered and the line inside it that actually releases the
            # lock -- a real, confirmed deadlock, not a theoretical one.
            # `request()` above already set the token's flag and killed
            # every tracked process regardless; only *raising this
            # exception* -- unwinding the call stack -- waits for the
            # region to exit and do it instead.
            defer_first_signal_exception(exc())
            return
        raise exc

    return _handler


def _watched_signals() -> frozenset[int]:
    """Signals :class:`_SignalWatcher` reacts to — exactly the ones
    `sigterm_as_interrupt` itself installs a main-thread handler for.

    Built once `SIGHUP`'s own availability is known, not a module-level
    literal, since it doesn't exist on Windows.
    """
    signums = {signal.SIGINT, signal.SIGTERM}
    if hasattr(signal, "SIGHUP"):
        signums.add(signal.SIGHUP)
    return frozenset(signums)


class _SignalWatcher:
    """Sets the run's cancellation flag (and kills every tracked process)
    directly off a SIGINT/SIGTERM/SIGHUP, on whichever thread the OS
    happened to deliver it to — not whenever the main thread next gets
    around to running the Python-level handler above (#404).

    CPython only ever runs a ``signal.signal``-registered handler's
    Python bytecode on the main thread, and only the next time that
    thread's own eval loop checks for one — ordinarily within a fraction
    of a second, but with no actual bound: a long call that holds the
    GIL continuously or the OS simply not scheduling that thread for a
    while can delay it past the point where a tree-flow branch's own
    next step has already started (#399).
    ``core.cancellation.CancellationToken.request`` — the thing that
    actually matters for correctness, not the exit-code bookkeeping the
    handler above still owns — does not need to wait for that: this
    watcher calls it from a dedicated thread blocked in ``os.read()`` on
    the read end of a pipe registered with ``signal.set_wakeup_fd``.
    Writing to that fd happens inside the signal's own low-level,
    async-signal-safe C trampoline — on whichever thread the signal
    actually reached, with no dependency on the GIL or on any thread's
    own bytecode eval loop for that write alone — so this watcher's
    ``os.read()`` unblocks the instant the signal is delivered, exactly
    like any other reader of a pipe that just received data, regardless
    of what the main thread (or any other thread) happens to be doing at
    that moment. The watcher thread's own subsequent call to ``request``
    is ordinary Python bytecode, though, and does need the GIL like any
    other thread's — it only ever waits for it, briefly, never for the
    main thread's own handler.

    Confirmed with a real test before relying on this (not just
    asyncio's own docs): a plain ``asyncio.run()`` with no signal handler
    of its own registered — exactly what ``plugins.mcp_client`` does for
    every MCP call — never touches ``signal.set_wakeup_fd`` at all, so
    this watcher keeps working unaffected across one. Only code that
    explicitly calls ``loop.add_signal_handler`` (asyncio's own opt-in
    signal mechanism — unused anywhere in this package, directly or via
    the ``mcp``/``anyio`` libraries the MCP client plugin depends on)
    would replace the wakeup fd out from under this watcher; nothing
    here does.
    """

    def __init__(self) -> None:
        self._watched = _watched_signals()
        read_fd, write_fd = os.pipe()
        # `signal.set_wakeup_fd` requires its fd to be non-blocking: the
        # write it does from inside a signal handler must never block.
        os.set_blocking(write_fd, False)
        self._read_fd = read_fd
        self._write_fd = write_fd
        self._previous_wakeup_fd = signal.set_wakeup_fd(write_fd)
        self._thread = threading.Thread(
            target=self._run, name="circuitry-signal-watcher", daemon=True
        )
        self._thread.start()
        # Read by `_reset_signal_state_in_fork_child`, below, in every
        # child this process forks from here on -- see that function's
        # own docstring for why a forked child needs to know this.
        global _active_watcher
        _active_watcher = self

    def _run(self) -> None:
        while True:
            try:
                data = os.read(self._read_fd, 1024)
            except OSError:
                return
            if not data:
                # The write end was closed by close() below -- the pipe
                # has reached EOF, so this watcher is done.
                return
            for signum in data:
                if signum in self._watched:
                    get_token().request(signum=signum)

    def close(self) -> None:
        """Restore the previous wakeup fd and stop the watcher thread.

        Order matters: the wakeup fd is restored *before* this closes
        its own write end, so a signal arriving in between writes to
        whatever the caller had registered before this watcher existed
        (or nowhere, if it was -1) rather than to an fd this is about to
        close. Closing the write end afterwards is what actually unblocks
        the watcher thread's own ``os.read()`` -- a closed write end on a
        pipe delivers EOF to the reader at once, *provided every copy of
        it is closed* -- a forked child inherits its own copy of both
        pipe fds (see ``_reset_signal_state_in_fork_child``), and a pipe
        reports EOF only once every writer has closed its own copy, not
        just this process's.
        """
        global _active_watcher
        signal.set_wakeup_fd(self._previous_wakeup_fd)
        _active_watcher = None
        os.close(self._write_fd)
        self._thread.join(timeout=5.0)
        if not self._thread.is_alive():
            os.close(self._read_fd)
        # else: something else still holds a copy of the write end open
        # (ordinarily `_reset_signal_state_in_fork_child` failing to run,
        # or running before this watcher was registered as active) and
        # the thread is still blocked in `os.read()` on `_read_fd` --
        # closing it out from under that blocked read is unsafe (another
        # thread could reopen the same fd number in between), so this
        # leaves it open rather than risk that; it is reclaimed when the
        # process exits either way.


#: The one `_SignalWatcher` currently armed in this process, or `None` --
#: read by `_reset_signal_state_in_fork_child` in every child this
#: process ever forks (see that function's own docstring). Set by
#: `_SignalWatcher.__init__`, cleared by its own `close()`.
_active_watcher: _SignalWatcher | None = None


def _reset_signal_state_in_fork_child() -> None:
    """Undo a forked child's inherited wakeup-fd arming (#404 review).

    A forked child -- ``plugins.python_eval``'s sandboxed-code child is
    the one example in this codebase -- inherits both CPython's own
    process-global ``signal.set_wakeup_fd`` setting and the watcher's two
    pipe fds: ``fork``, unlike ``exec``, duplicates the whole fd table,
    and close-on-exec only takes effect on an actual ``exec``, which a
    plugin that never execs (python_eval runs Python in the child, not a
    new program) never reaches. Left alone, while a watcher is armed,
    that has two consequences for a signal landing in the child before
    its own code gets a chance to reset its handlers (``python_eval``
    does, but only after this hook has already run):

      (a) the child's own low-level signal trampoline would still write
          into the *parent*'s wakeup pipe -- a SIGTERM delivered to the
          child (the same process group as the parent) would cancel the
          parent's run, not just end the child.
      (b) the child keeps its own copy of both pipe fds open for as long
          as it lives. A pipe only reports EOF once every copy of its
          write end, across every process, is closed -- so
          :meth:`_SignalWatcher.close` closing the parent's own write end
          is not enough while such a child survives, and its
          ``join(timeout=5.0)`` would wait out the full five seconds
          every time.

    Registered once, at module import time, via
    ``os.register_at_fork(after_in_child=...)`` -- it runs in *every*
    child this process ever forks, armed watcher or not, but is a no-op
    (``_active_watcher`` is ``None``) whenever none is.
    """
    watcher = _active_watcher
    if watcher is None:
        return
    signal.set_wakeup_fd(-1)
    for fd in (watcher._read_fd, watcher._write_fd):
        try:
            os.close(fd)
        except OSError:
            pass


if os.name == "posix" and hasattr(os, "register_at_fork"):
    os.register_at_fork(after_in_child=_reset_signal_state_in_fork_child)


@contextmanager
def sigterm_as_interrupt() -> Iterator[None]:
    """Make SIGINT/SIGTERM/SIGHUP cancel the ``with`` body's run promptly (#356).

    A no-op off the main thread, so it is always safe to wrap a CLI command
    body in this regardless of how that command happens to be invoked.
    """
    if threading.current_thread() is not threading.main_thread():
        yield
        return

    token = get_token()
    token.reset()
    token.arm()
    signal_count = _SignalCount()
    # POSIX only: `signal.set_wakeup_fd` requires a *socket* handle on
    # Windows, not a plain `os.pipe()` fd -- an unarmed watcher there
    # leaves this exactly as it was before #404 (the main-thread handler
    # alone), not worse.
    watcher = _SignalWatcher() if os.name == "posix" else None
    previous_sigint = signal.signal(
        signal.SIGINT, _make_handler(KeyboardInterrupt, signal_count)
    )
    previous_sigterm = signal.signal(
        signal.SIGTERM, _make_handler(SigTermInterrupt, signal_count)
    )
    # SIGHUP is installed only when the platform has one (not Windows) and
    # it isn't already ignored on entry -- a caller that ran `cof` under
    # `nohup` (or otherwise set SIG_IGN) means it, and a hangup can't reach
    # an ignored handler anyway, so leave it exactly as found and never
    # touch it again on the way out either.
    install_sighup = (
        hasattr(signal, "SIGHUP")
        and signal.getsignal(signal.SIGHUP) is not signal.SIG_IGN
    )
    previous_sighup = (
        signal.signal(signal.SIGHUP, _make_handler(SigHupInterrupt, signal_count))
        if install_sighup
        else None
    )
    try:
        yield
    finally:
        # Read before `token.reset()` clears it below: whether *this* run
        # was cancelled by any signal, not just SIGHUP, decides whether
        # SIGHUP is left ignored past this point (#357 follow-up).
        cancelled = token.is_set()
        signal.signal(signal.SIGINT, previous_sigint)
        signal.signal(signal.SIGTERM, previous_sigterm)
        if install_sighup:
            # A cancelled run still has `--out` and the `--last` stash to
            # write after this scope exits (`cli.app`, outside this
            # `with`) — `runtime.persistence` is already written by this
            # point, inside `run()` while these handlers were still
            # installed. A SIGHUP landing in that window must not revert
            # to whatever handler was here before (SIG_DFL would kill the
            # process outright) and undo that write. Set SIG_IGN only now,
            # once no step can start a new child to inherit it — never
            # inside the handler itself, see the module docstring. An
            # uncancelled run restores exactly what it found, as always.
            signal.signal(
                signal.SIGHUP, signal.SIG_IGN if cancelled else previous_sighup
            )
        if watcher is not None:
            watcher.close()
        token.disarm()
        token.reset()
