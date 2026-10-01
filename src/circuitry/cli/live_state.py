from __future__ import annotations

import logging
import os
import tempfile
import threading
import time
from pathlib import Path
from typing import Any

from ..core.saved_state import dumps_saved_state

logger = logging.getLogger(__name__)

#: The least time between two full serialisations of the ``--live-state``
#: mirror. Every ``on_write`` hands the mirror a snapshot; the ones that land
#: inside the interval are coalesced into the next write, so a run makes at
#: most ``run time / interval`` serialisations plus the final one.
LIVE_STATE_INTERVAL_SECONDS = 0.5


def _encode(state: dict[str, Any]) -> str | None:
    """The mirror's file contents, or ``None`` if *state* is not JSON yet."""
    try:
        return dumps_saved_state(state) + "\n"
    except (TypeError, ValueError, OverflowError):
        return None


def _replace_file(path: Path, payload: str) -> None:
    """Write *payload* to *path* atomically via tmp-file + rename.

    ``mkstemp`` opens with ``O_CREAT | O_EXCL`` under a random name it
    generates itself, in the same directory as *path* (so the rename stays
    on one filesystem) — unlike a predictable ``path.with_suffix(".tmp")``
    sibling, a local attacker can't pre-create this path as a symlink and
    have it followed on write.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=str(path.parent)
    )
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(payload)
        os.replace(tmp_name, str(path))
    except BaseException:
        try:
            os.unlink(tmp_name)
        except OSError:
            pass
        raise


def write_live_state(path: Path, state: dict[str, Any]) -> None:
    """Write state JSON atomically via tmp-file + rename.

    Silently skips the write if the state cannot be serialized to valid JSON
    (e.g. contains non-serializable objects mid-execution).
    """
    payload = _encode(state)
    if payload is not None:
        _replace_file(path, payload)


class LiveStateMirror:
    """The ``--live-state`` writer: coalesced, and never doing I/O under the store lock.

    Called as ``Store.on_write`` — by the runtime after each effect, and by
    ``Store.set`` while it holds the lock every tree-flow branch shares. Only
    the first call writes the file itself: ``run`` makes it with the initial
    snapshot, before any effect and outside the lock, so an unwritable path
    fails the run up front. Every later call just records the snapshot and
    wakes a writer thread, which serialises the newest one at most once per
    *interval* — under *store_lock*, so it never reads a state halfway through
    a ``Store.set`` — and writes the file after releasing it. No branch waits
    on disk, and a failed write there is logged and recorded (see
    :meth:`close`) rather than failing the run.

    :meth:`close` stops the thread and writes the state it is given
    synchronously: the run's final state, including everything recorded after
    the last effect, so the mirror ends equal to ``--out``. It reports
    whether any write, mid-run or this final one, failed, so the caller can
    fold that into a single warning rather than leaving it silent.
    """

    def __init__(
        self,
        path: Path,
        *,
        store_lock: threading.RLock,
        interval: float = LIVE_STATE_INTERVAL_SECONDS,
    ) -> None:
        self._path = path
        self._store_lock = store_lock
        self._interval = interval
        self._wake = threading.Condition()
        #: The newest snapshot not yet serialised; guarded by ``_wake``.
        self._pending: dict[str, Any] | None = None
        self._first_written = False
        self._closed = False
        self._next_due = 0.0
        #: Set once by any write after the first — mid-run or the final one
        #: in :meth:`close` — that failed to serialise or reach disk. Read by
        #: :meth:`close`, which folds the whole run into a single warning
        #: instead of the silent log this always was.
        self._had_failure = False
        self._thread = threading.Thread(
            target=self._write_loop, name="circuitry-live-state", daemon=True
        )
        self._thread.start()

    def __call__(self, state: dict[str, Any]) -> None:
        with self._wake:
            first = not self._first_written
            self._first_written = True
            if not first:
                self._pending = state
                self._wake.notify()
                return
        payload = _encode(state)
        if payload is not None:
            _replace_file(self._path, payload)
        self._next_due = time.monotonic() + self._interval

    def _write_loop(self) -> None:
        while True:
            with self._wake:
                while self._pending is None and not self._closed:
                    self._wake.wait()
                if self._closed:
                    return
                delay = self._next_due - time.monotonic()
                if delay > 0:
                    # Let more writes coalesce; close() cuts the wait short.
                    self._wake.wait(delay)
                    continue
                state = self._pending
                self._pending = None
            assert state is not None
            self._next_due = time.monotonic() + self._interval
            # The mirror is for watchers: a failure here — expected (an
            # unencodable state, a disk error) or not (e.g. a RecursionError
            # out of the recursive compaction in saved_state.py on very deep
            # or circular state) — is recorded, never allowed to kill this
            # thread or fail the run it mirrors.
            try:
                with self._store_lock:
                    payload = _encode(state)
                if payload is not None:
                    self._write(payload)
            except Exception:
                logger.exception("--live-state mirror write failed for %s", self._path)
                self._had_failure = True

    def _write(self, payload: str) -> None:
        try:
            _replace_file(self._path, payload)
        except OSError as exc:
            logger.warning("Could not write --live-state %s: %s", self._path, exc)
            self._had_failure = True

    def close(self, final_state: dict[str, Any]) -> bool:
        """Stop the writer thread, then write *final_state* to the mirror.

        Returns whether any write — mid-run or this final one — failed to
        serialise or reach disk over the mirror's whole lifetime, so the
        caller can fold it into one warning on the run's result.
        """
        with self._wake:
            if self._closed:
                return self._had_failure
            self._closed = True
            self._pending = None
            self._wake.notify()
        self._thread.join()
        # The run's own result must always win: a failure writing the final
        # snapshot (or an unexpected one, e.g. RecursionError) is recorded,
        # never raised into run()'s finally.
        try:
            payload = _encode(final_state)
            if payload is not None:
                self._write(payload)
        except Exception:
            logger.exception("--live-state mirror final write failed for %s", self._path)
            self._had_failure = True
        return self._had_failure
