"""A running subprocess tool's live output: ``meta.live`` for ``shell`` and
every plugin built on ``plugins/_subprocess.py`` (#741).

:class:`SubprocessLiveTracker` is built once per subprocess attempt (one
call to ``run_binary``/``run_tracked``) and fed raw bytes, exactly as read
off the pipe before any decoding, as they arrive on stdout and stderr,
through :meth:`SubprocessLiveTracker.record` -- never after the fact. It
keeps a bounded tail of each stream and the running raw-byte counts, and
publishes a snapshot through :class:`~circuitry.agent_live.LivePublisher`,
coalescing updates to at most one a second the same way the ``agent``
tool's own ``meta.live`` (#636) and streamed prompts (#674) already do.
``record`` itself never builds that snapshot -- it only appends to the
relevant stream's tail and schedules a build; the actual, possibly
expensive text work (decoding, joining, trimming) happens at most once a
second, inside the publisher's own thread, right before it publishes
(see :class:`~circuitry.agent_live.LivePublisher`'s own docstring).

Never published here beyond the tails and byte counts themselves: no
arguments, environment, or anything the final node's ``meta.stdout``/
``meta.stderr`` wouldn't also carry.
"""

from __future__ import annotations

import codecs
import io
import threading
import time
from collections import deque
from collections.abc import Callable
from datetime import datetime, timezone
from typing import Any

from .agent_live import PUBLISH_INTERVAL_SECONDS, LivePublisher

#: ``meta.live``'s schema version for a subprocess tool (#741).
LIVE_SUMMARY_VERSION = 1

#: A tail keeps at most this many lines -- the current partial line (not
#: yet newline-terminated) counts as the last one of them, and shows as
#: soon as it has been read.
MAX_TAIL_LINES = 20

#: A tail, and any one line held inside it (complete or still partial),
#: is cut, at a character boundary, to stay within this many UTF-8-encoded
#: bytes.
MAX_TAIL_BYTES = 4096


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _clip_to_byte_limit(text: str, max_bytes: int) -> str:
    """*text*, cut from the front at a character boundary so its UTF-8
    encoding is at most *max_bytes* -- the end survives, never the start
    (#741)."""
    encoded = text.encode("utf-8")
    if len(encoded) <= max_bytes:
        return text
    # `errors="ignore"` only ever drops the leading fragment of a
    # multi-byte sequence this slice cut into -- everything after it was
    # already valid UTF-8 and decodes back unchanged.
    return encoded[-max_bytes:].decode("utf-8", errors="ignore")


def _join_within_byte_limit(lines: list[str], max_bytes: int) -> str:
    """*lines* joined with ``\\n``, dropping whole lines from the front
    until the join fits within *max_bytes* of UTF-8 -- never a fragment of
    the oldest kept line (#741). Always terminates within *max_bytes*
    because every individual line already arrives here no longer than
    *max_bytes* itself (see :meth:`_TailBuffer.append`'s own per-line
    clip), so the newest line alone is always enough to fit.
    """
    kept = list(lines)
    while kept:
        joined = "\n".join(kept)
        if len(joined.encode("utf-8")) <= max_bytes:
            return joined
        kept.pop(0)
    return ""


class _TailBuffer:
    """One stream's running raw-byte count and bounded tail (#741).

    Fed raw bytes, exactly as read off the pipe, through :meth:`append`.
    Decoded incrementally and lossily, independent of, and never as
    strict as, the final ``meta.stdout``/``meta.stderr`` decode (which
    this module never touches): ``errors="replace"`` substitutes U+FFFD
    for invalid bytes rather than raising, and an ``io.
    IncrementalNewlineDecoder`` on top applies the same universal-newline
    translation the final decode does (``\\r\\n`` and a lone ``\\r`` each
    end a line, a ``\\r\\n`` split across two reads still counted as one),
    so a tail's own line boundaries always land where the final text's
    would.

    The current, not yet newline-terminated line (``_carry``) is kept and
    shown as the tail's own last line even before it ends -- a
    ``printf "working…"; sleep 60`` shows ``working…`` live rather than
    nothing until the process finishes. Every line this buffer holds,
    complete or still partial, is clipped to :data:`MAX_TAIL_BYTES`
    characters from the front the moment it lands here, independent of
    :meth:`text`'s own whole-line trim below -- memory stays bounded by a
    handful of lines this size even for a stream that writes one huge
    line with no newline in it at all.
    """

    def __init__(self) -> None:
        self.total_bytes = 0
        self._decoder = io.IncrementalNewlineDecoder(
            codecs.getincrementaldecoder("utf-8")(errors="replace"), translate=True
        )
        self._carry = ""
        self._lines: deque[str] = deque(maxlen=MAX_TAIL_LINES)

    def append(self, chunk: bytes) -> None:
        self.total_bytes += len(chunk)
        text = self._decoder.decode(chunk)
        if not text:
            return
        *complete, carry = (self._carry + text).split("\n")
        for line in complete:
            self._lines.append(_clip_to_byte_limit(line, MAX_TAIL_BYTES))
        self._carry = _clip_to_byte_limit(carry, MAX_TAIL_BYTES)

    def text(self) -> str:
        lines = list(self._lines)
        if self._carry:
            lines.append(self._carry)
        elif not lines:
            return ""
        return _join_within_byte_limit(lines[-MAX_TAIL_LINES:], MAX_TAIL_BYTES)


class SubprocessLiveTracker:
    """One subprocess attempt's ``meta.live``: a snapshot of each stream's
    bounded tail and running raw-byte count, published through a
    :class:`~circuitry.agent_live.LivePublisher` at most once a second
    (#741)."""

    def __init__(
        self,
        publish: Callable[[dict[str, Any]], None],
        *,
        interval: float = PUBLISH_INTERVAL_SECONDS,
        now: Callable[[], float] = time.monotonic,
    ) -> None:
        self._lock = threading.Lock()
        self._started_at = _now_iso()
        self._stdout = _TailBuffer()
        self._stderr = _TailBuffer()
        self._publisher = LivePublisher(publish, interval=interval, now=now)
        # The bound method itself, not its result -- `LivePublisher` only
        # ever calls it, at most once a second, right before it publishes
        # (#741). Every call below passes this exact same object, so
        # `LivePublisher.update` only ever does an attribute lookup and a
        # `notify()`.
        self._publisher.update(self._snapshot)

    def record(self, stream: str, chunk: bytes) -> None:
        with self._lock:
            tail = self._stdout if stream == "stdout" else self._stderr
            tail.append(chunk)
        self._publisher.update(self._snapshot)

    def _snapshot(self) -> dict[str, Any]:
        with self._lock:
            return {
                "v": LIVE_SUMMARY_VERSION,
                "started_at": self._started_at,
                "updated_at": _now_iso(),
                "stdout_tail": self._stdout.text(),
                "stderr_tail": self._stderr.text(),
                "stdout_bytes": self._stdout.total_bytes,
                "stderr_bytes": self._stderr.total_bytes,
            }

    def close(self) -> None:
        self._publisher.close()
