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
from pathlib import Path
from typing import Any

from ..core.report import encode_report_data
from ..core.report import now_iso_ms as _now_iso_ms
from ..core.saved_state import json_safe_default
from .version import resolve_version

logger = logging.getLogger(__name__)

#: An effect's (or the run's) error text is cut to this many characters
#: before it reaches the stream — the same limit the format documents.
_ERROR_MAX_CHARS = 500


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
        # The first `progress` line for a given `start` instance id records
        # its own `ms` as that instance's `first_token_ms`; every later
        # `progress` line for the same id reuses it (#674) -- protected by
        # `self._lock` like everything else that touches shared state here.
        self._first_token_ms: dict[int, int] = {}
        # One `start`/`end` pair covers every attempt of a retried prompt
        # (`core.prompt`'s own retry loop fires `effect_start` once, before
        # the attempt loop runs), so `first_token_ms` must be reset when a
        # new attempt's own first delta arrives, never carried over from an
        # earlier, failed attempt the way the bare instance id would. Keyed
        # the same as `_first_token_ms`; absent until the first progress
        # call for a given instance names its own attempt.
        self._first_token_ms_attempt: dict[int, int] = {}
        # `on_progress` fires from a streaming adapter's own reader thread
        # or `agent_live.LivePublisher`'s own throttling thread (#674) --
        # never the thread that called `on_start` for this path, so it
        # cannot use `_stack_for`'s per-thread stacks the way `on_complete`
        # does. This mirrors the *latest* open instance per path instead,
        # updated under the same lock: one prompt effect's own path is
        # never concurrently open with itself (a tree-flow loop's own
        # iterations each get their own, distinct path), so "latest" and
        # "the one this progress line belongs to" always agree in practice.
        self._open_by_path: dict[str, tuple[int, float]] = {}
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
        already checked ``self._disabled``.

        ``default=json_safe_default`` (#605): a
        report's own ``data`` can hold a shadow-state value
        ``json.dumps`` can't otherwise encode (a YAML date, ``!!binary``
        bytes), the same as ``--out``. Without it, encoding this one line
        raises and disables every later write on this stream for the rest
        of the run -- a watcher would silently lose every subsequent
        ``start``/``end``/``run_end`` too, not just this report.
        """
        self._file.write(
            json.dumps(payload, separators=(",", ":"), default=json_safe_default) + "\n"
        )
        self._file.flush()

    def run_start(
        self,
        *,
        run_id: str,
        orchestration: str,
        config: str | None,
        label: str | None = None,
    ) -> None:
        if self._disabled:
            return
        try:
            with self._lock:
                if self._disabled:
                    return
                payload = {
                    "v": 1,
                    "seq": self._take_seq(),
                    "ts": _now_iso_ms(),
                    "ev": "run_start",
                    "run_id": run_id,
                    "orchestration": orchestration,
                }
                # Present only when this run has one (#752), right after
                # `orchestration` — an unlabelled run's `run_start` line is
                # byte-for-byte what it was before this field existed.
                if label:
                    payload["label"] = label
                payload["config"] = config
                payload["engine"] = _engine_label()
                payload["pid"] = os.getpid()
                self._write_line(payload)
        except Exception as exc:
            self._disable("run_start", exc)

    def on_dispatch(self, path: str, concurrency: int, total: int) -> None:
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
                        "branches": total,
                        "concurrency": concurrency,
                    }
                )
        except Exception as exc:
            self._disable("dispatch", exc)

    def current_instance(self, path: str) -> tuple[int, float] | None:
        """The ``(instance id, started at)`` currently open for *path* --
        ``Store.open_instance_id``'s own backing call (#674 review): a
        streamed prompt's own attempt captures this once, right as it
        begins, rather than letting a later ``progress`` line re-derive it
        from this same lookup at fire time, which a same-path concurrent
        ``start`` could have since moved on to a different instance.
        """
        with self._lock:
            return self._open_by_path.get(path)

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
                started_at = time.monotonic()
                self._stack_for(path).append((instance_id, started_at))
                self._open_by_path[path] = (instance_id, started_at)
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
                    if self._open_by_path.get(path) == (instance_id, started_at):
                        del self._open_by_path[path]
                    self._first_token_ms.pop(instance_id, None)
                    self._first_token_ms_attempt.pop(instance_id, None)
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

    def on_progress(self, path: str, data: dict[str, Any]) -> None:
        """Write one ``progress`` line (#674): the same running figures
        ``meta.live`` carries (``tokens_received_estimate``,
        ``chars_received``, ``tokens_sent``, ``tokens_sent_estimate``),
        plus ``id`` and ``ms``/``first_token_ms`` computed from that
        instance's own start time -- the same reference :meth:`on_complete`'s
        own ``ms`` uses, so the two can never drift. Called only on a
        streamed prompt's own throttled publishes, never on the write that
        starts or clears ``meta.live``.

        ``data['_instance']`` (set by :meth:`Store.resolve_open_instance`,
        captured once when the attempt began) is preferred over a fresh
        ``_open_by_path`` lookup here: a same-path concurrent ``start`` (an
        unnamed tree-flow loop sharing one compiled path) can have since
        moved that lookup on to a different instance than the one this
        progress line actually belongs to. Absent only for a caller that
        predates the capture-at-attempt-start convention, in which case
        this falls back to the lookup as before.
        """
        if self._disabled:
            return
        try:
            with self._lock:
                if self._disabled:
                    return
                captured = data.get("_instance")
                open_instance = captured if captured is not None else self._open_by_path.get(path)
                if open_instance is not None:
                    instance_id, started_at = open_instance
                    ms = max(0, round((time.monotonic() - started_at) * 1000))
                else:
                    instance_id = None
                    ms = None
                attempt = data.get("attempt")
                if (
                    instance_id is not None
                    and attempt is not None
                    and self._first_token_ms_attempt.get(instance_id) != attempt
                ):
                    # A new attempt's own first delta -- the previous
                    # attempt's `first_token_ms`, if any, belongs to a
                    # failed try and must not leak into this one's.
                    self._first_token_ms.pop(instance_id, None)
                    self._first_token_ms_attempt[instance_id] = attempt
                first_token_ms: int | None
                if instance_id is None:
                    first_token_ms = None
                elif instance_id in self._first_token_ms:
                    first_token_ms = self._first_token_ms[instance_id]
                else:
                    first_token_ms = ms
                    self._first_token_ms[instance_id] = ms  # type: ignore[assignment]
                payload: dict[str, Any] = {
                    "v": 1,
                    "seq": self._take_seq(),
                    "ts": _now_iso_ms(),
                    "ev": "progress",
                    "id": instance_id,
                    "path": path,
                    "ms": ms,
                    "tokens_received_estimate": data.get("tokens_received_estimate"),
                    "chars_received": data.get("chars_received"),
                    "tokens_sent": data.get("tokens_sent"),
                    "tokens_sent_estimate": data.get("tokens_sent_estimate"),
                    "first_token_ms": first_token_ms,
                }
                self._write_line(payload)
        except Exception as exc:
            self._disable("progress", exc)

    def on_report(self, path: str, level: str, message: str, data: Any) -> None:
        """Write one ``report`` line (#605): ``v``, ``seq``, ``ts``, ``ev``,
        ``path``, ``level``, ``message``, then ``data`` (omitted when there
        is none) or ``data_truncated`` (the compact JSON's byte count) when
        that JSON exceeds 16 KiB -- a named report still keeps the full
        value in the shadow state, only this line is capped.
        """
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
                    "ev": "report",
                    "path": path,
                    "level": level,
                    "message": message,
                }
                value, truncated = encode_report_data(data)
                if truncated is not None:
                    payload["data_truncated"] = truncated
                elif value is not None:
                    payload["data"] = value
                self._write_line(payload)
        except Exception as exc:
            self._disable("report", exc)

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
