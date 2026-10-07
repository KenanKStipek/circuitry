"""The ``scripted`` adapter: deterministic model replies, no network, ever.

Reads an ordered reply queue per calling-effect path from a replies file —
YAML or JSON, keyed by the path, see ``electricity/docs/spec/scripted-replies.md``
for the exact format (electricity's own scripted adapter reads the identical
file). Built for the conformance suite (electricity/DESIGN.md §12) and for
anyone who wants to run an orchestration deterministically without a real
model behind it.

A call is matched to its reply queue by ``core.effect_identity.current_call_path``,
which every model-call site in the runtime sets for the duration of its call
(a prompt effect's attempt, including retries; a ``model-mode`` ``expect:``
re-ask) — never by the order calls happen to arrive in, which a tree-flow
loop or dynamic makes nondeterministic.
"""

from __future__ import annotations

import json
import threading
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ..core.effect_identity import current_call_path
from ..core.yaml_load import load_yaml
from ..preflight import CheckResult
from ._retry import AdapterCallError, RetryInfo, status_is_retryable
from .base import GenerateOptions, GenerateResult, ignored_options_warning

#: Named error kinds a reply can carry, each mapping onto the runtime's own
#: retry classification (``core.adapters._retry``) — the same distinction a
#: real adapter's own exception carries, so the retry/fallback loop treats a
#: scripted failure identically to a real one. ``"http"`` takes an explicit
#: ``status`` and defers to :func:`status_is_retryable`; every other kind has
#: a default status a reply may still override.
_DEFAULT_STATUS: dict[str, int | None] = {
    "timeout": None,
    "connection": None,
    "rate_limited": 429,
    "server_error": 500,
    "invalid_request": 400,
    "unauthorized": 401,
    "not_found": 404,
    "http": None,
}
_ALWAYS_RETRYABLE = frozenset({"timeout", "connection"})


class ScriptedRepliesError(RuntimeError):
    """The replies file itself is malformed — a load/``check()``-time failure."""


@dataclass(frozen=True)
class _Reply:
    text: str | None = None
    tokens_sent: int | None = None
    tokens_received: int | None = None
    finish_reason: str | None = None
    error_kind: str | None = None
    error_message: str | None = None
    error_status: int | None = None


def _load_raw(path: Path) -> Any:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as e:
        raise ScriptedRepliesError(f"scripted adapter: cannot read {path}: {e}") from e
    if path.suffix == ".json":
        try:
            return json.loads(text)
        except json.JSONDecodeError as e:
            raise ScriptedRepliesError(f"{path}: invalid JSON: {e}") from e
    try:
        return load_yaml(text)
    except Exception as e:
        raise ScriptedRepliesError(f"{path}: invalid YAML: {e}") from e


def _parse_error(entry: dict[str, Any], *, path: str, index: int, source: Path) -> _Reply:
    error = entry["error"]
    if not isinstance(error, dict) or "kind" not in error:
        raise ScriptedRepliesError(
            f"{source}: reply {index} for path '{path}': 'error' must be a "
            "mapping with a 'kind'"
        )
    kind = error["kind"]
    if kind not in _DEFAULT_STATUS:
        raise ScriptedRepliesError(
            f"{source}: reply {index} for path '{path}': unknown error kind "
            f"{kind!r}; one of {sorted(_DEFAULT_STATUS)}"
        )
    status = error.get("status", _DEFAULT_STATUS[kind])
    if kind == "http" and status is None:
        raise ScriptedRepliesError(
            f"{source}: reply {index} for path '{path}': error kind 'http' "
            "needs a 'status'"
        )
    if status is not None and not isinstance(status, int):
        raise ScriptedRepliesError(
            f"{source}: reply {index} for path '{path}': 'status' must be an int"
        )
    message = error.get("message")
    if message is not None and not isinstance(message, str):
        raise ScriptedRepliesError(
            f"{source}: reply {index} for path '{path}': 'message' must be a string"
        )
    return _Reply(error_kind=kind, error_message=message, error_status=status)


def _parse_reply(entry: Any, *, path: str, index: int, source: Path) -> _Reply:
    if not isinstance(entry, dict):
        raise ScriptedRepliesError(
            f"{source}: reply {index} for path '{path}' must be a mapping"
        )
    if "error" in entry:
        return _parse_error(entry, path=path, index=index, source=source)
    text = entry.get("text")
    if not isinstance(text, str):
        raise ScriptedRepliesError(
            f"{source}: reply {index} for path '{path}' needs a string "
            "'text', or an 'error'"
        )
    for key in ("tokens_sent", "tokens_received"):
        value = entry.get(key)
        if value is not None and not isinstance(value, int):
            raise ScriptedRepliesError(
                f"{source}: reply {index} for path '{path}': '{key}' must be an int"
            )
    finish_reason = entry.get("finish_reason")
    if finish_reason is not None and not isinstance(finish_reason, str):
        raise ScriptedRepliesError(
            f"{source}: reply {index} for path '{path}': 'finish_reason' must be a string"
        )
    return _Reply(
        text=text,
        tokens_sent=entry.get("tokens_sent"),
        tokens_received=entry.get("tokens_received"),
        finish_reason=finish_reason,
    )


def _parse_replies(raw: Any, *, source: Path) -> dict[str, list[_Reply]]:
    if raw is None:
        raw = {}
    if not isinstance(raw, dict):
        raise ScriptedRepliesError(
            f"{source}: replies file must be a mapping of path -> list of replies"
        )
    out: dict[str, list[_Reply]] = {}
    for path, entries in raw.items():
        if not isinstance(path, str) or not path:
            raise ScriptedRepliesError(f"{source}: every key must be a non-empty string path")
        if not isinstance(entries, list) or not entries:
            raise ScriptedRepliesError(
                f"{source}: replies for path '{path}' must be a non-empty list"
            )
        out[path] = [
            _parse_reply(entry, path=path, index=i, source=source)
            for i, entry in enumerate(entries)
        ]
    return out


def _retry_info(reply: _Reply) -> RetryInfo:
    assert reply.error_kind is not None
    status = reply.error_status
    if reply.error_kind in _ALWAYS_RETRYABLE:
        return RetryInfo(retryable=True, status=status)
    assert status is not None
    return RetryInfo(retryable=status_is_retryable(status), status=status)


@dataclass
class ScriptedAdapter:
    """Deterministic, no-network ``generate()`` for conformance testing.

    One instance consumes its replies file's queues for its own lifetime —
    the run's own default adapter instance is built once and reused for
    every dispatch (``core.prompt._resolve_adapter``), so this is the normal
    case; an instance resolved fresh per fallback attempt instead starts its
    own, independent queues.
    """

    name: str = "scripted"
    replies_file: str = "scripted-replies.yaml"
    _lock: threading.Lock = field(default_factory=threading.Lock, repr=False, compare=False)
    _queues: dict[str, list[_Reply]] | None = field(
        default=None, repr=False, compare=False
    )

    def _path(self) -> Path:
        return Path(self.replies_file).expanduser()

    def _ensure_loaded(self) -> dict[str, list[_Reply]]:
        with self._lock:
            if self._queues is None:
                self._queues = _parse_replies(_load_raw(self._path()), source=self._path())
            return self._queues

    def generate(
        self,
        *,
        model: str,
        prompt: str,
        timeout_seconds: int = 120,
        options: GenerateOptions | None = None,
    ) -> GenerateResult:
        del model, prompt, timeout_seconds
        path = current_call_path()
        if path is None:
            raise RuntimeError(
                "scripted adapter: no model-call path is set for this call; "
                "it must be dispatched through the runtime (core.effect_identity.model_call)"
            )
        queues = self._ensure_loaded()
        with self._lock:
            queue = queues.get(path)
            if not queue:
                raise RuntimeError(
                    f"scripted adapter: no reply configured for path '{path}' "
                    f"in {self._path()}"
                )
            reply = queue.pop(0)
        if reply.error_kind is not None:
            message = reply.error_message or (
                f"scripted adapter: {reply.error_kind} at '{path}'"
            )
            raise AdapterCallError(message, retry_info=_retry_info(reply))
        return GenerateResult(
            text=reply.text or "",
            raw={"path": path},
            tokens_sent=reply.tokens_sent,
            tokens_received=reply.tokens_received,
            finish_reason=reply.finish_reason,
            warnings=ignored_options_warning(self.name, options),
        )

    def leftover_replies(self) -> dict[str, int]:
        """Replies never consumed at a path, keyed by path.

        Empty when every configured reply was used. The run itself never
        reads this — a leftover reply is not a run failure on its own, by
        design (an unmatched *call* fails that call outright; an unused
        *reply* just means the script over-provisioned). A conformance
        harness that built this instance calls it after the run to fail
        the case on anything left over.
        """
        with self._lock:
            queues = self._queues or {}
            return {path: len(queue) for path, queue in queues.items() if queue}

    def check(self) -> CheckResult:
        try:
            self._ensure_loaded()
        except ScriptedRepliesError as e:
            return CheckResult(ok=False, missing=[], message=str(e))
        return CheckResult(ok=True)
