"""The ``agent`` tool's live progress: ``meta.live`` and the JSONL
transcript, published while a pi/Claude Code session runs (#636).

:class:`LiveTracker` is built once per tool attempt (one call to
``AgentPlugin.execute``) and lives across that attempt's turns, including a
repair turn. It is fed one line at a time, through :meth:`LiveTracker.handle_line`,
as ``agent_cli.run_agent_cli`` streams them (see ``core.cancellation.run_tracked``'s
``on_line``) -- never after the fact. Each line updates a few running
numbers (turns, tool calls, tokens, the current tool, ...), appends a JSONL
line to the session's transcript, and schedules a publish of the whole
snapshot through :class:`LivePublisher`, which coalesces updates to at most
one a second.

At the end of each turn, :meth:`LiveTracker.sync_turn_totals` overwrites the
running turn/tool-call/token counts with the authoritative ones the plugin's
own end-of-turn parse computed (``agent_cli.parse_pi_output`` /
``parse_claude_output``, via ``plugins.agent._Session``) -- that parse stays
the single source of truth for those numbers; this module's own incremental
reading of them is only ever a best-effort, intra-turn approximation of the
same numbers, which self-corrects at every turn boundary.

**Never published here**: prompt text, reply or thinking text, tool
arguments, tool output, file contents, the environment, or error text --
only numbers, times, ids, the model name and tool names. See
``docs/plugins/agent.md``.
"""

from __future__ import annotations

import json
import logging
import re
import threading
import time
from collections import deque
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .core.cancellation import deferred_signal_region

#: ``meta.live``'s schema version.
LIVE_SUMMARY_VERSION = 1

#: Tool names kept distinct before the rest count under "other" (#636).
MAX_TOOL_NAMES = 32
#: A tool name longer than this is truncated before counting against
#: MAX_TOOL_NAMES (#636).
MAX_TOOL_NAME_CHARS = 64
_OTHER_TOOL = "other"

#: ``turn_tokens`` keeps at most this many entries, oldest dropped first.
MAX_TURN_TOKENS = 512

#: ``meta.live`` is published at most this often per effect (#636).
PUBLISH_INTERVAL_SECONDS = 1.0

#: How much of a reply one transcript line keeps -- the same cap the
#: compact transcript has always used.
TRANSCRIPT_LINE_CHARS = 300

#: How much of a command one transcript tool target keeps.
_TARGET_COMMAND_CHARS = 120

#: pi prints many ``message_update`` delta lines; this is a plain prefix
#: check, cheaper than a full ``json.loads`` on every one of them, and safe
#: because pi's own lines always put ``"type"`` first (#636).
_PI_MESSAGE_UPDATE_RE = re.compile(r'^\{\s*"type"\s*:\s*"message_update"')

logger = logging.getLogger(__name__)

#: Argument keys this module reads to describe a tool call without ever
#: storing its arguments verbatim -- never any other key, and never the
#: values of keys it doesn't recognise. pi's own ``write`` carries
#: ``path``/``content``; its own ``edit`` carries ``path`` and an
#: ``edits`` list of ``{oldText, newText}`` (the same shape Claude Code's
#: ``MultiEdit`` uses under its own key names), or occasionally a single
#: top-level ``oldText``/``newText`` pair instead of a list.
_PATH_ARG_KEYS = ("file_path", "path", "filepath", "notebook_path")
_PATTERN_ARG_KEYS = ("pattern",)
_OLD_TEXT_KEYS = ("old_string", "old_text", "before", "oldText")
_NEW_TEXT_KEYS = ("new_string", "new_text", "after", "newText")
_CONTENT_KEYS = ("content", "new_content", "text")

#: Tool names (lowercased) this module can read diff stats from.
_WRITE_TOOL_NAMES = frozenset({"write"})
_EDIT_TOOL_NAMES = frozenset({"edit"})
_MULTI_EDIT_TOOL_NAMES = frozenset({"multiedit"})


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _is_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def _as_float(value: Any) -> float | None:
    return float(value) if _is_number(value) else None


def _as_int(value: Any) -> int | None:
    return int(value) if _is_number(value) else None


def _clip(text: str, limit: int) -> str:
    flat = " ".join(text.split())
    if len(flat) > limit:
        flat = flat[: limit - 3] + "..."
    return flat


def _line_count(text: Any) -> int | None:
    if not isinstance(text, str):
        return None
    if text == "":
        return 0
    return text.count("\n") + 1


def _first_str(args: Mapping[str, Any], keys: Sequence[str]) -> str | None:
    for key in keys:
        value = args.get(key)
        if isinstance(value, str) and value:
            return value
    return None


def tool_target(args: Any) -> str | None:
    """A short, content-free description of a tool call's arguments for the
    transcript: the path for read/edit/write, the first
    :data:`_TARGET_COMMAND_CHARS` characters of a command, or the pattern
    (and path, when given) for a search -- ``None`` for anything else
    (#636). Never the arguments themselves.
    """
    if not isinstance(args, dict):
        return None
    pattern = _first_str(args, _PATTERN_ARG_KEYS)
    path = _first_str(args, _PATH_ARG_KEYS)
    if pattern is not None:
        return f"{pattern} {path}".strip() if path else pattern
    if path is not None:
        return path
    command = args.get("command") or args.get("cmd")
    if isinstance(command, str):
        return command[:_TARGET_COMMAND_CHARS]
    if isinstance(command, list) and command and all(isinstance(part, str) for part in command):
        return " ".join(command)[:_TARGET_COMMAND_CHARS]
    return None


def diff_stats(name: str, args: Any) -> tuple[str | None, int | None, int | None]:
    """``(path, lines_added, lines_removed)`` for a write/edit/multiedit
    tool call recognised from *args*; ``(None, None, None)`` for anything
    else. Counts lines only, from the call's own arguments -- never file
    content, and never a diff computed from the file system (#636).
    """
    if not isinstance(args, dict):
        return None, None, None
    lowered = name.lower()
    path = _first_str(args, _PATH_ARG_KEYS)
    if lowered in _WRITE_TOOL_NAMES:
        added = _line_count(_first_str(args, _CONTENT_KEYS))
        return path, added, (0 if added is not None else None)
    if lowered in _EDIT_TOOL_NAMES:
        edits = args.get("edits")
        if isinstance(edits, list):
            # pi's own ``edit`` call carries a list of edits under this
            # same key, the same shape Claude Code's ``MultiEdit`` uses
            # (#636 follow-up) -- its own single old/new keys, checked
            # below, don't apply here.
            added, removed = _edits_list_stats(edits)
            return path, added, removed
        added = _line_count(_first_str(args, _NEW_TEXT_KEYS))
        removed = _line_count(_first_str(args, _OLD_TEXT_KEYS))
        return path, added, removed
    if lowered in _MULTI_EDIT_TOOL_NAMES:
        added, removed = _edits_list_stats(args.get("edits"))
        return path, added, removed
    return None, None, None


def _edits_list_stats(edits: Any) -> tuple[int | None, int | None]:
    """Summed ``(lines_added, lines_removed)`` over an ``edits`` list (the
    shape both Claude Code's ``MultiEdit`` and pi's own ``edit`` use);
    ``(None, None)`` when nothing in it was recognised."""
    added = removed = 0
    seen = False
    if isinstance(edits, list):
        for edit in edits:
            if not isinstance(edit, dict):
                continue
            a = _line_count(_first_str(edit, _NEW_TEXT_KEYS))
            r = _line_count(_first_str(edit, _OLD_TEXT_KEYS))
            if a is not None:
                added += a
                seen = True
            if r is not None:
                removed += r
                seen = True
    return (added if seen else None), (removed if seen else None)


class LivePublisher:
    """Publishes a snapshot through *publish* at most once every *interval*
    seconds, coalescing updates in between -- the same coalesced-writer
    shape ``cli.live_state.LiveStateMirror`` already uses, scoped to one
    effect instead of the whole run (#636).

    The first :meth:`update` always publishes immediately (``_next_due``
    starts at 0), so a watcher sees the effect the moment it starts. A
    change that lands inside the throttle window still goes out within
    about *interval* of it, even if nothing else happens afterwards: the
    writer thread wakes on its own once the window ends, it doesn't wait
    for the next :meth:`update` call.
    """

    def __init__(
        self,
        publish: Callable[[dict[str, Any]], None],
        *,
        interval: float = PUBLISH_INTERVAL_SECONDS,
        now: Callable[[], float] = time.monotonic,
    ) -> None:
        self._publish = publish
        self._interval = interval
        self._now = now
        self._wake = threading.Condition()
        self._pending: dict[str, Any] | None = None
        self._closed = False
        self._next_due = 0.0
        self._thread = threading.Thread(
            target=self._loop, daemon=True, name="circuitry-agent-live"
        )
        self._thread.start()

    def update(self, snapshot: dict[str, Any]) -> None:
        # `threading.Condition.__exit__` is pure Python (#555's deadlock
        # class): a signal landing mid-release on the main thread -- the
        # thread a sequential chain's own effect runs on -- would leak
        # this lock to it forever, and `_loop` (a different thread) would
        # then block on it for good.
        with deferred_signal_region(), self._wake:
            if self._closed:
                return
            self._pending = snapshot
            self._wake.notify()

    def _loop(self) -> None:
        while True:
            with self._wake:
                while self._pending is None and not self._closed:
                    self._wake.wait()
                if self._pending is None:
                    return
                delay = self._next_due - self._now()
                if delay > 0 and not self._closed:
                    self._wake.wait(delay)
                    continue
                snapshot = self._pending
                self._pending = None
            self._next_due = self._now() + self._interval
            # A failing *publish* (the running node already gone, a
            # cof instance that no longer owns the store, ...) must not
            # kill this thread -- the same as `LiveStateMirror._write_loop`
            # already does for its own writer thread (#636 review).
            try:
                self._publish(snapshot)
            except Exception:
                logger.warning("agent live publish failed", exc_info=True)

    def close(self) -> None:
        """Stop the writer thread. Any snapshot still pending is published
        first -- harmless, since the caller clears ``meta.live`` right
        after this returns."""
        with deferred_signal_region(), self._wake:
            self._closed = True
            self._wake.notify()
        # Bounded, for the same #555 reason `update` above is now guarded:
        # a hung `_loop` (its own publish callback stuck on something)
        # must not leave this call -- reached from an interrupt's own
        # unwind, inside `plugins/agent.py`'s `finally:` -- blocking the
        # whole run until a second Ctrl-C.
        self._thread.join(timeout=5)


@dataclass
class _PendingTool:
    seq: int
    bucket: str
    raw_name: str
    args: Any
    started: float
    started_at: str


class LiveTracker:
    """One tool attempt's ``meta.live`` and transcript, across its turns."""

    def __init__(
        self,
        *,
        engine: str,
        model: str,
        attempt: int,
        transcript_path: Path,
        publish: Callable[[dict[str, Any]], None],
        earlier_attempts: tuple[int | None, int | None] | None = None,
        interval: float = PUBLISH_INTERVAL_SECONDS,
        now: Callable[[], float] = time.monotonic,
    ) -> None:
        self._engine = engine
        self._model = model or None
        # A model param fixes `_model` for the whole attempt (#646): once
        # set here, no later `_note_model` call from the stream may
        # overwrite it.
        self._model_locked = bool(self._model)
        self._attempt = attempt
        self._transcript_path = transcript_path
        self._earlier_attempts = earlier_attempts
        self._lock = threading.Lock()
        self._transcript_lock = threading.Lock()
        self._session_id: str | None = None
        self._phase = "turn"
        self._started_at = _now_iso()
        self._turns = 0
        self._tool_calls = 0
        self._tools: dict[str, dict[str, float]] = {}
        self._current_tool: dict[str, Any] | None = None
        self._tokens_sent: int | None = None
        self._tokens_received: int | None = None
        self._cost_usd: float | None = None
        self._tokens_cached_in: int | None = None
        self._tokens_cache_write: int | None = None
        self._context_tokens: int | None = None
        self._context_window: int | None = None
        self._turn_tokens: deque[int] = deque(maxlen=MAX_TURN_TOKENS)
        self._files_touched: set[str] = set()
        self._lines_added: int | None = None
        self._lines_removed: int | None = None
        # Tools with a call id (Claude Code's ``tool_use.id``/
        # ``tool_result.tool_use_id``, pi's own ``toolCallId``) are matched
        # to their end by that id, keyed here, rather than FIFO -- a Task
        # subagent's own first tool result must not pop its parent
        # ``Task`` call off a plain queue (#636 follow-up). A call whose
        # event carries no id at all still queues FIFO in
        # ``_pending_fifo``.
        self._pending_by_id: dict[str, _PendingTool] = {}
        self._pending_fifo: deque[_PendingTool] = deque()
        self._tool_seq = 0
        # Claude Code prints one line per content block; several lines can
        # share one ``message.id`` (one API call). Turns and usage are
        # counted once per id, never once per line (#636 follow-up).
        self._seen_message_ids: set[str] = set()
        self._publisher = LivePublisher(publish, interval=interval, now=now)
        self._publish_now()

    # ---- publishing ----

    def _snapshot(self) -> dict[str, Any]:
        with self._lock:
            summary: dict[str, Any] = {
                "v": LIVE_SUMMARY_VERSION,
                "engine": self._engine,
                "model": self._model,
                "session_id": self._session_id,
                "transcript": str(self._transcript_path),
                "attempt": self._attempt,
                "phase": self._phase,
                "started_at": self._started_at,
                "updated_at": _now_iso(),
                "turns": self._turns,
                "tool_calls": self._tool_calls,
                "tools": {
                    name: {"calls": stats["calls"], "ms": round(stats["ms"])}
                    for name, stats in self._tools.items()
                },
                "current_tool": (
                    dict(self._current_tool) if self._current_tool is not None else None
                ),
                "tokens_sent": self._tokens_sent,
                "tokens_received": self._tokens_received,
                "cost_usd": self._cost_usd,
                "tokens_cached_in": self._tokens_cached_in,
                "tokens_cache_write": self._tokens_cache_write,
                "context_tokens": self._context_tokens,
                "context_window": self._context_window,
                "turn_tokens": list(self._turn_tokens),
                "files_changed": len(self._files_touched) if self._files_touched else None,
                "lines_added": self._lines_added,
                "lines_removed": self._lines_removed,
            }
            if self._earlier_attempts is not None:
                sent, received = self._earlier_attempts
                summary["earlier_attempts"] = {"tokens_sent": sent, "tokens_received": received}
            return summary

    def _publish_now(self) -> None:
        self._publisher.update(self._snapshot())

    def close(self) -> None:
        self._publisher.close()

    # ---- transcript ----

    def _write_transcript(self, kind: str, **fields: Any) -> None:
        line = json.dumps({"t": _now_iso(), "type": kind, **fields}, ensure_ascii=False)
        with self._transcript_lock:
            self._transcript_path.parent.mkdir(parents=True, exist_ok=True)
            with self._transcript_path.open("a", encoding="utf-8") as handle:
                handle.write(line + "\n")

    def note_reply(self, text: str) -> None:
        self._write_transcript("reply", text=_clip(text, TRANSCRIPT_LINE_CHARS))

    # ---- turn boundaries ----

    def start_turn(self, *, phase: str, session_id: str | None, label: str | None = None) -> None:
        """Called once, synchronously, right before the CLI for this turn
        (the main one or the repair turn) starts -- so the transcript
        carries a ``turn`` line and ``meta.live.phase`` updates even before
        the CLI has printed anything of its own.

        *phase* becomes ``meta.live.phase`` -- ``"turn"`` or ``"repair"``,
        the two values the issue's own contract names. *label*, defaulting
        to *phase*, is the transcript's own ``turn`` line value, which
        stays the caller's more readable ``"repair turn"`` even while
        ``phase`` is just ``"repair"`` (#636).
        """
        with self._lock:
            self._phase = phase
            if session_id:
                self._session_id = session_id
            self._current_tool = None
        self._write_transcript(
            "turn", label=(label if label is not None else phase), session_id=self._session_id
        )
        self._publish_now()

    def sync_turn_totals(
        self,
        *,
        turns: int,
        tool_calls: int,
        tokens_sent: int | None,
        tokens_received: int | None,
        cost_usd: float | None,
    ) -> None:
        """Overwrite the running turn/tool-call/token counts with the
        authoritative, cumulative totals the end-of-turn parse computed
        (``plugins.agent._Session``'s own running totals) -- the numbers
        this module reads off the stream as it goes are only ever a
        best-effort approximation of these, and this keeps the two from
        drifting apart at every turn boundary (#636).
        """
        with self._lock:
            self._turns = turns
            self._tool_calls = tool_calls
            self._tokens_sent = tokens_sent
            self._tokens_received = tokens_received
            self._cost_usd = cost_usd
        self._publish_now()

    # ---- the model, when params set none (#646) ----

    def _note_model(self, value: Any) -> bool:
        """Takes *value* as the model the CLI itself just reported --
        ``meta.live.model``/``meta.raw.model`` keep the latest one seen this
        way, unless a ``model`` param already fixed it (#646). Returns
        whether this changed anything worth a publish. Never called while
        ``self._lock`` is already held -- it isn't reentrant.
        """
        if not isinstance(value, str) or not value or self._model_locked:
            return False
        with self._lock:
            if self._model == value:
                return False
            self._model = value
            return True

    # ---- raw, once the attempt ends ----

    def raw_extra(self) -> dict[str, Any]:
        """The new ``meta.raw`` keys (#636, plus ``model`` for #646): the
        final state of everything this tracker accumulated over every turn
        of this attempt."""
        with self._lock:
            return {
                "model": self._model,
                "context_tokens": self._context_tokens,
                "context_window": self._context_window,
                "tool_counts": {
                    name: {"calls": stats["calls"], "ms": round(stats["ms"])}
                    for name, stats in self._tools.items()
                },
                "turn_tokens": list(self._turn_tokens),
                "files_changed": (len(self._files_touched) if self._files_touched else None),
                "lines_added": self._lines_added,
                "lines_removed": self._lines_removed,
            }

    # ---- tool name bucketing ----

    def _bucket_tool_name(self, name: str) -> str:
        name = (name or "?")[:MAX_TOOL_NAME_CHARS]
        if name in self._tools or len(self._tools) < MAX_TOOL_NAMES:
            return name
        return _OTHER_TOOL

    # ---- shared tool start/end ----

    def _start_tool(self, name: Any, args: Any, *, call_id: str | None = None) -> None:
        raw_name = name if isinstance(name, str) else "?"
        target = tool_target(args)
        started_at = _now_iso()
        with self._lock:
            self._tool_calls += 1
            bucket = self._bucket_tool_name(raw_name)
            entry = self._tools.setdefault(bucket, {"calls": 0, "ms": 0.0})
            entry["calls"] += 1
            self._tool_seq += 1
            seq = self._tool_seq
            pending = _PendingTool(
                seq=seq,
                bucket=bucket,
                raw_name=raw_name,
                args=args,
                started=time.monotonic(),
                started_at=started_at,
            )
            if call_id:
                self._pending_by_id[call_id] = pending
            else:
                self._pending_fifo.append(pending)
            # The tool just started is always the highest-seq pending one,
            # so it becomes `current_tool` outright -- no need to recompute
            # it from the rest of `_pending_by_id`/`_pending_fifo` (#636
            # review).
            self._current_tool = {"name": bucket, "started_at": started_at}
        self._write_transcript("tool_start", id=seq, tool=bucket, target=target)
        self._publish_now()

    def _latest_pending_tool(self) -> dict[str, Any] | None:
        """The still-pending tool call with the highest sequence number, as
        a ``current_tool`` dict, or ``None`` when nothing is pending --
        called with ``self._lock`` already held, right after popping
        whichever call just ended, so a Claude Task's own ``current_tool``
        doesn't go blank just because its subagent's tool call ended first
        (#636 review).
        """
        candidates = list(self._pending_by_id.values()) + list(self._pending_fifo)
        if not candidates:
            return None
        latest = max(candidates, key=lambda pending: pending.seq)
        return {"name": latest.bucket, "started_at": latest.started_at}

    def _end_tool(
        self, *, ok: bool, duration_ms: float | None = None, call_id: str | None = None
    ) -> None:
        with self._lock:
            pending: _PendingTool | None
            if call_id is not None:
                # Matched by the CLI's own call id -- never FIFO here: a
                # Task subagent's first result must pop *its own* pending
                # tool, not whatever else happens to be oldest (its parent
                # ``Task`` call, still running) (#636 follow-up).
                pending = self._pending_by_id.pop(call_id, None)
            elif self._pending_fifo:
                pending = self._pending_fifo.popleft()
            else:
                pending = None
            if pending is not None:
                ms = duration_ms if duration_ms is not None else (time.monotonic() - pending.started) * 1000
                self._tools.setdefault(pending.bucket, {"calls": 0, "ms": 0.0})["ms"] += ms
            self._current_tool = self._latest_pending_tool()
        if pending is None:
            self._publish_now()
            return
        path, added, removed = diff_stats(pending.raw_name, pending.args)
        if path is not None:
            with self._lock:
                self._files_touched.add(path)
        if added is not None:
            with self._lock:
                self._lines_added = (self._lines_added or 0) + added
        if removed is not None:
            with self._lock:
                self._lines_removed = (self._lines_removed or 0) + removed
        extra: dict[str, Any] = {}
        if path is not None:
            extra["lines_added"] = added
            extra["lines_removed"] = removed
        self._write_transcript("tool_end", id=pending.seq, ok=ok, ms=round(ms), **extra)
        self._publish_now()

    # ---- shared usage accumulation ----

    def _apply_usage(
        self,
        *,
        sent_parts: Sequence[Any],
        received: Any,
        cost: Any,
        cache_read: Any,
        cache_write: Any,
    ) -> None:
        with self._lock:
            context = 0
            any_sent = False
            for value in sent_parts:
                if _is_number(value):
                    self._tokens_sent = (self._tokens_sent or 0) + int(value)
                    context += int(value)
                    any_sent = True
            if _is_number(received):
                self._tokens_received = (self._tokens_received or 0) + int(received)
            if _is_number(cost):
                self._cost_usd = (self._cost_usd or 0.0) + float(cost)
            if _is_number(cache_read):
                self._tokens_cached_in = (self._tokens_cached_in or 0) + int(cache_read)
            if _is_number(cache_write):
                self._tokens_cache_write = (self._tokens_cache_write or 0) + int(cache_write)
            if any_sent:
                self._context_tokens = context
                self._turn_tokens.append(context)

    # ---- per-line handlers ----

    def handle_line(self, line: str) -> None:
        if not line.strip():
            return
        if self._engine == "pi":
            self._handle_pi_line(line)
        else:
            self._handle_claude_line(line)

    def _handle_pi_line(self, line: str) -> None:
        if _PI_MESSAGE_UPDATE_RE.match(line):
            return
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            return
        if not isinstance(event, dict):
            return
        kind = event.get("type")
        if kind == "session":
            session_id = event.get("id")
            if isinstance(session_id, str):
                with self._lock:
                    self._session_id = session_id
                self._publish_now()
            return
        if kind == "tool_execution_start":
            call_id = event.get("toolCallId")
            self._start_tool(
                event.get("toolName"),
                event.get("args"),
                call_id=call_id if isinstance(call_id, str) else None,
            )
            return
        if kind == "tool_execution_end":
            call_id = event.get("toolCallId")
            self._end_tool(
                ok=not bool(event.get("isError")),
                duration_ms=_as_float(event.get("durationMs")),
                call_id=call_id if isinstance(call_id, str) else None,
            )
            return
        if kind == "message_start":
            # pi's own ``message_start``, when it sends one, carries the
            # assistant message's ``model`` the same way ``message_end``
            # does (#646) -- nothing else here is read from it.
            message = event.get("message")
            model = message.get("model") if isinstance(message, dict) else None
            if self._note_model(model):
                self._publish_now()
            return
        if kind == "message_end":
            message = event.get("message")
            if not isinstance(message, dict) or message.get("role") != "assistant":
                return
            with self._lock:
                self._turns += 1
            self._note_model(message.get("model"))
            usage = message.get("usage")
            if isinstance(usage, dict):
                cost = usage.get("cost")
                self._apply_usage(
                    sent_parts=(usage.get("input"), usage.get("cacheRead"), usage.get("cacheWrite")),
                    received=usage.get("output"),
                    cost=cost.get("total") if isinstance(cost, dict) else cost,
                    cache_read=usage.get("cacheRead"),
                    cache_write=usage.get("cacheWrite"),
                )
            text = _pi_message_text(message.get("content"))
            if text:
                self.note_reply(text)
            self._publish_now()
            return

    def _handle_claude_line(self, line: str) -> None:
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            return
        if not isinstance(event, dict):
            return
        kind = event.get("type")
        if kind == "system":
            # Claude Code's own ``system/init`` line carries the model it
            # used (#646), alongside the session id this already read.
            session_id = event.get("session_id")
            model_changed = self._note_model(event.get("model"))
            session_changed = False
            if isinstance(session_id, str):
                with self._lock:
                    self._session_id = session_id
                session_changed = True
            if model_changed or session_changed:
                self._publish_now()
            return
        if kind == "assistant":
            message = event.get("message")
            if not isinstance(message, dict):
                return
            # Claude Code prints one line per content block; lines from
            # the same API call share ``message.id`` (and repeat the same
            # ``usage``) -- counted here only the first time that id is
            # seen, so a turn with several blocks isn't counted, nor its
            # usage summed, once per block (#636 follow-up).
            message_id = message.get("id")
            first_for_message = True
            if isinstance(message_id, str):
                with self._lock:
                    first_for_message = message_id not in self._seen_message_ids
                    self._seen_message_ids.add(message_id)
            # A subagent (Task tool) message carries its own
            # ``parent_tool_use_id``; it's its own session's own API call,
            # not one of this session's own turns, and its usage is its own
            # session's spend, not the main session's context -- so it's
            # left out of ``turns`` and tokens/context here (#636 review).
            # Its tool calls are still counted below like any other.
            is_subagent = event.get("parent_tool_use_id") is not None
            if first_for_message:
                if not is_subagent:
                    with self._lock:
                        self._turns += 1
                usage = message.get("usage")
                if isinstance(usage, dict) and not is_subagent:
                    # Per-message output_tokens is a placeholder mid-turn
                    # (#636 decision): received/cost stay null here and are
                    # filled in only from the final ``result`` line below.
                    self._apply_usage(
                        sent_parts=(
                            usage.get("input_tokens"),
                            usage.get("cache_read_input_tokens"),
                            usage.get("cache_creation_input_tokens"),
                        ),
                        received=None,
                        cost=None,
                        cache_read=usage.get("cache_read_input_tokens"),
                        cache_write=usage.get("cache_creation_input_tokens"),
                    )
            content = message.get("content")
            texts: list[str] = []
            if isinstance(content, list):
                for item in content:
                    if not isinstance(item, dict):
                        continue
                    if item.get("type") == "tool_use":
                        call_id = item.get("id")
                        self._start_tool(
                            item.get("name"),
                            item.get("input"),
                            call_id=call_id if isinstance(call_id, str) else None,
                        )
                    elif (
                        item.get("type") == "text"
                        and isinstance(item.get("text"), str)
                        and item["text"].strip()
                    ):
                        texts.append(item["text"])
            for text in texts:
                self.note_reply(text)
            self._publish_now()
            return
        if kind == "user":
            message = event.get("message")
            content = message.get("content") if isinstance(message, dict) else None
            if isinstance(content, list):
                for item in content:
                    if isinstance(item, dict) and item.get("type") == "tool_result":
                        call_id = item.get("tool_use_id")
                        self._end_tool(
                            ok=not bool(item.get("is_error")),
                            call_id=call_id if isinstance(call_id, str) else None,
                        )
            return
        if kind == "result":
            session_id = event.get("session_id")
            if isinstance(session_id, str):
                with self._lock:
                    self._session_id = session_id
            usage = event.get("usage")
            output = _as_int(usage.get("output_tokens")) if isinstance(usage, dict) else None
            if output is not None:
                with self._lock:
                    self._tokens_received = (self._tokens_received or 0) + output
            cost = _as_float(event.get("total_cost_usd"))
            if cost is not None:
                with self._lock:
                    self._cost_usd = (self._cost_usd or 0.0) + cost
            model_usage = event.get("modelUsage")
            if isinstance(model_usage, dict):
                for info in model_usage.values():
                    if isinstance(info, dict) and _is_number(info.get("contextWindow")):
                        with self._lock:
                            self._context_window = int(info["contextWindow"])
                        break
            self._publish_now()
            return


def _pi_message_text(content: Any) -> str:
    if isinstance(content, str):
        return content.strip()
    if not isinstance(content, list):
        return ""
    return "".join(
        item["text"]
        for item in content
        if isinstance(item, dict) and item.get("type") == "text" and isinstance(item.get("text"), str)
    ).strip()
