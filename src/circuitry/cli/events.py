"""The ``--events`` writer: a JSONL stream of effect starts and ends (#419).

See ``electricity/docs/spec/runtime-semantics.md``'s "Event stream
(``--events``)" section for the format and ordering rules this
implements; ``docs/orchestration-reference.md``'s "Watching a Run" section
documents the flag itself.

:class:`EventLog` is wired into ``cli.runtime_shim.run`` next to
``LiveStateMirror``: ``on_start``/``on_complete`` ride the same
``Store.effect_start``/``effect_complete`` hooks that drive the live-state
mirror and runtime plugins, and ``on_dispatch`` composes with the existing
``concurrent_dispatch`` callback rather than replacing it.

This stream must never change what a run does or how it ends — not even
when something about *this* class goes wrong. Every public method here
therefore catches any exception of its own (not just an ``OSError``
writing the file), logs one warning, and disables all further writes for
the rest of this instance's life, the same contract an ``OSError`` already
had. A composed observer that raises elsewhere — e.g. a plugin's own
``on_effect_complete`` throwing before this one runs — is a different
concern this class cannot see or fix; what it *can* guarantee is that an
``on_complete`` this instance is actually called with is always safe,
including one with no matching ``on_start`` on this thread's stack (a
double-fire, or a call this instance never saw the start of): that is not
an error here, it just writes ``end`` with ``"id": null`` and no ``ms``.
"""

from __future__ import annotations

import json
import logging
import os
import threading
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .version import resolve_version

logger = logging.getLogger(__name__)

#: An effect's (or the run's) error text is cut to this many characters
#: before it reaches the stream — the same limit the format documents.
_ERROR_MAX_CHARS = 500


def _now_iso_ms() -> str:
    """Wall-clock UTC, millisecond precision, ``Z`` suffix — e.g.
    ``"2026-10-08T19:56:22.433Z"``, matching the format's own example."""
    return (
        datetime.now(timezone.utc)
        .isoformat(timespec="milliseconds")
        .replace("+00:00", "Z")
    )


def _truncate_error(error: str) -> str:
    return error[:_ERROR_MAX_CHARS]


def _engine_label() -> str:
    return f"cof {resolve_version()}"


class EventLog:
    """Writes one JSONL line per ``run_start``/``dispatch``/``start``/
    ``end``/``run_end`` event to *path*.

    Opened once, here, create-or-truncate (the parent directory is made
    first, like ``--live-state``) — the file is ready before the first
    effect, the same as ``--live-state``'s first write. Every event is one
    ``write()`` of one whole line, flushed at once, under one lock — a
    reader tailing the file never sees a torn line, except a trailing one
    still being written. A failure to open the file, or anything that goes
    wrong writing to it, is logged once (a warning) and then ignored for
    the rest of this instance's life: writing this stream must never fail
    the run, the same as the live-state mirror (see the module docstring).

    Instance ids come from one counter, incremented under the same lock as
    every write, so ``id`` stays unique even across several concurrent
    tree-flow branches. Pairing a ``start`` with its ``end`` uses a
    per-thread stack keyed by path (``threading.local``): ``on_start`` and
    ``on_complete`` for one effect instance always arrive on the same
    thread, nested instances included, so a thread's own stack for a path
    holds exactly the ids that thread has opened there and not yet closed.
    """

    def __init__(self, path: Path) -> None:
        self._lock = threading.Lock()
        self._seq = 0
        self._next_instance_id = 0
        self._local = threading.local()
        self._disabled = False
        self._file: Any = None
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            # Kept open for this instance's whole lifetime, closed by
            # :meth:`close` -- not a `with` block, which would close it
            # right after this line.
            self._file = open(path, "w", encoding="utf-8")  # noqa: SIM115
        except OSError as exc:
            self._disable("open", exc)

    def _disable(self, operation: str, exc: Exception) -> None:
        if not self._disabled:
            logger.warning(
                "--events %s failed, disabling further writes: %s", operation, exc
            )
        self._disabled = True

    def _stack_for(self, path: str) -> list[tuple[int, float]]:
        stacks: dict[str, list[tuple[int, float]]] | None = getattr(
            self._local, "stacks", None
        )
        if stacks is None:
            stacks = {}
            self._local.stacks = stacks
        return stacks.setdefault(path, [])

    def _take_seq(self) -> int:
        seq = self._seq
        self._seq += 1
        return seq

    def _write_line(self, payload: dict[str, Any]) -> None:
        """Write one JSON line; caller holds ``self._lock`` and has
        already checked ``self._disabled``."""
        self._file.write(json.dumps(payload, separators=(",", ":")) + "\n")
        self._file.flush()

    def run_start(self, *, run_id: str, orchestration: str) -> None:
        if self._disabled:
            return
        try:
            with self._lock:
                if self._disabled:
                    return
                self._write_line(
                    {
                        "v": 1,
                        "seq": self._take_seq(),
                        "ts": _now_iso_ms(),
                        "ev": "run_start",
                        "run_id": run_id,
                        "orchestration": orchestration,
                        "engine": _engine_label(),
                        "pid": os.getpid(),
                    }
                )
        except Exception as exc:
            self._disable("run_start", exc)

    def on_dispatch(self, path: str, branches: int) -> None:
        if self._disabled:
            return
        try:
            with self._lock:
                if self._disabled:
                    return
                self._write_line(
                    {
                        "v": 1,
                        "seq": self._take_seq(),
                        "ts": _now_iso_ms(),
                        "ev": "dispatch",
                        "path": path,
                        "branches": branches,
                    }
                )
        except Exception as exc:
            self._disable("dispatch", exc)

    def on_start(self, path: str, node: dict[str, Any]) -> None:
        del node
        if self._disabled:
            return
        try:
            with self._lock:
                if self._disabled:
                    return
                instance_id = self._next_instance_id
                self._next_instance_id += 1
                self._stack_for(path).append((instance_id, time.monotonic()))
                self._write_line(
                    {
                        "v": 1,
                        "seq": self._take_seq(),
                        "ts": _now_iso_ms(),
                        "ev": "start",
                        "id": instance_id,
                        "path": path,
                    }
                )
        except Exception as exc:
            self._disable("start", exc)

    def on_complete(self, path: str, node: dict[str, Any]) -> None:
        if self._disabled:
            return
        try:
            with self._lock:
                if self._disabled:
                    return
                stack = self._stack_for(path)
                # A completion with no matching start on this thread's own
                # stack (a double-fire; a call whose start this instance
                # never saw) is not an error: emit `id: null` and no `ms`,
                # and carry on -- see the module docstring.
                ms: int | None
                if stack:
                    instance_id, started_at = stack.pop()
                    ms = max(0, round((time.monotonic() - started_at) * 1000))
                else:
                    instance_id = None
                    ms = None
                meta = node.get("meta") if isinstance(node, dict) else None
                error = meta.get("error") if isinstance(meta, dict) else None
                payload: dict[str, Any] = {
                    "v": 1,
                    "seq": self._take_seq(),
                    "ts": _now_iso_ms(),
                    "ev": "end",
                    "id": instance_id,
                    "path": path,
                    "ok": error is None,
                }
                if ms is not None:
                    payload["ms"] = ms
                if error is not None:
                    payload["error"] = _truncate_error(str(error))
                self._write_line(payload)
        except Exception as exc:
            self._disable("end", exc)

    def run_end(self, *, ok: bool, error: str | None, signal: str | None) -> None:
        if self._disabled:
            return
        try:
            with self._lock:
                if self._disabled:
                    return
                payload: dict[str, Any] = {
                    "v": 1,
                    "seq": self._take_seq(),
                    "ts": _now_iso_ms(),
                    "ev": "run_end",
                    "ok": ok,
                }
                if not ok and error is not None:
                    payload["error"] = _truncate_error(error)
                if signal is not None:
                    payload["signal"] = signal
                self._write_line(payload)
        except Exception as exc:
            self._disable("run_end", exc)

    def close(self) -> bool:
        """Stop writing and close the file; returns whether this instance
        ever failed to open or write it — the caller folds that into one
        run-level warning, the same as ``LiveStateMirror.close``."""
        with self._lock:
            if self._file is not None:
                try:
                    self._file.close()
                except OSError:
                    pass
                self._file = None
            return self._disabled
