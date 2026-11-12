"""A running subprocess tool's live output: ``meta.live`` for ``shell`` and
every plugin built on ``plugins/_subprocess.py`` (#741).

:class:`SubprocessLiveTracker` is built once per subprocess attempt (one
call to ``run_binary``/``run_tracked``) and fed raw bytes as they arrive on
stdout and stderr, through :meth:`SubprocessLiveTracker.record` -- never
after the fact. It keeps a bounded tail of each stream and the running byte
counts, and publishes a snapshot through :class:`~circuitry.agent_live.LivePublisher`,
coalescing updates to at most one a second the same way the ``agent`` tool's
own ``meta.live`` (#636) and streamed prompts (#674) already do.

Never published here beyond the tails and byte counts themselves: no
arguments, environment, or anything the final node's ``meta.stdout``/
``meta.stderr`` wouldn't also carry.
"""

from __future__ import annotations

import codecs
import threading
import time
from collections import deque
from collections.abc import Callable
from datetime import datetime, timezone
from typing import Any

from .agent_live import PUBLISH_INTERVAL_SECONDS, LivePublisher

#: ``meta.live``'s schema version for a subprocess tool (#741).
LIVE_SUMMARY_VERSION = 1

#: A tail keeps at most this many lines -- a partial last line with no
#: trailing newline yet counts as one of them.
MAX_TAIL_LINES = 20

#: A tail is cut, at a line or character boundary, to stay within this many
#: UTF-8-encoded bytes.
MAX_TAIL_BYTES = 4096


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _trim_to_byte_limit(text: str, max_bytes: int) -> str:
    """*text*, cut from the front at a character boundary so its UTF-8
    encoding is at most *max_bytes* -- the tail end survives, never the
    start (#741)."""
    encoded = text.encode("utf-8")
    if len(encoded) <= max_bytes:
        return text
    # `errors="ignore"` only ever drops the leading fragment of a
    # multi-byte sequence this slice cut into -- everything after it was
    # already valid UTF-8 and decodes back unchanged.
    return encoded[-max_bytes:].decode("utf-8", errors="ignore")


class _TailBuffer:
    """One stream's running byte count and bounded tail (#741).

    Bytes are decoded incrementally as UTF-8 with invalid bytes replaced
    -- independent of, and never as strict as, the final ``meta.stdout``/
    ``meta.stderr`` decode, which stays exactly as today's (#741's own
    ``run_tracked`` change never touches it). A multi-byte character split
    across two chunks is handled correctly by the incremental decoder
    rather than replaced twice at the seam.
    """

    def __init__(self) -> None:
        self.total_bytes = 0
        self._decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        self._carry = ""
        self._lines: deque[str] = deque(maxlen=MAX_TAIL_LINES)

    def append(self, chunk: bytes) -> None:
        self.total_bytes += len(chunk)
        text = self._decoder.decode(chunk)
        if not text:
            return
        *complete, self._carry = (self._carry + text).split("\n")
        self._lines.extend(complete)

    def text(self) -> str:
        lines = list(self._lines)
        if self._carry:
            lines.append(self._carry)
        tail = "\n".join(lines[-MAX_TAIL_LINES:])
        return _trim_to_byte_limit(tail, MAX_TAIL_BYTES)


class SubprocessLiveTracker:
    """One subprocess attempt's ``meta.live``: a snapshot of each stream's
    bounded tail and running byte count, published through a
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
        self._publish_now()

    def record(self, stream: str, chunk: bytes) -> None:
        with self._lock:
            tail = self._stdout if stream == "stdout" else self._stderr
            tail.append(chunk)
        self._publish_now()

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

    def _publish_now(self) -> None:
        self._publisher.update(self._snapshot())

    def close(self) -> None:
        self._publisher.close()
