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

import atexit
import json
import os
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


#: Test-only hook (electricity/docs/spec/scripted-replies.md §7): a
#: conformance harness runs ``cof run`` as a *subprocess* and has no
#: handle to the adapter instance it built, so it cannot call
#: :meth:`ScriptedAdapter.leftover_replies` directly after the run. When
#: this variable names a writable path, every :class:`ScriptedAdapter`
#: built in the process registers itself here; at process exit, their
#: ``leftover_replies()`` counts are merged (summed per path, across every
#: instance — a fallback chain can build more than one) and written to
#: that path as a single JSON object. Never read when unset: an ordinary
#: run pays nothing for this.
LEFTOVER_REPLIES_EXPORT_ENV_VAR = "CIRCUITRY_TEST_SCRIPTED_LEFTOVER_REPLIES_FILE"

_export_lock = threading.Lock()
_export_instances: list[ScriptedAdapter] = []
_export_hook_registered = False


def _merge_leftover_replies(instances: list[ScriptedAdapter]) -> dict[str, int]:
    merged: dict[str, int] = {}
    for instance in instances:
        for path, count in instance.leftover_replies().items():
            merged[path] = merged.get(path, 0) + count
    return merged


def _write_leftover_export() -> None:
    target = os.environ.get(LEFTOVER_REPLIES_EXPORT_ENV_VAR)
    if not target:
        return
    with _export_lock:
        merged = _merge_leftover_replies(_export_instances)
    Path(target).write_text(json.dumps(merged), encoding="utf-8")


def _register_for_leftover_export(adapter: ScriptedAdapter) -> None:
    global _export_hook_registered
    if not os.environ.get(LEFTOVER_REPLIES_EXPORT_ENV_VAR):
        return
    with _export_lock:
        _export_instances.append(adapter)
        if not _export_hook_registered:
            atexit.register(_write_leftover_export)
            _export_hook_registered = True


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


#: Keys a text reply / an error mapping may carry — anything else is a
#: load-time error (a typo like ``token_sent`` must not silently drop the
#: count it was meant to set).
_TEXT_REPLY_KEYS = frozenset({"text", "tokens_sent", "tokens_received", "finish_reason"})
_ERROR_KEYS = frozenset({"kind", "status", "message"})


def _is_plain_int(value: Any) -> bool:
    """``True`` for an ``int``, ``False`` for a ``bool`` (a ``bool`` *is* an
    ``int`` in Python, but ``status: true``/``tokens_sent: false`` are typos,
    not valid values)."""
    return isinstance(value, int) and not isinstance(value, bool)


def _reject_unknown_keys(
    entry: dict[str, Any], allowed: frozenset[str], *, path: str, index: int, source: Path
) -> None:
    unknown = sorted(set(entry) - allowed)
    if unknown:
        raise ScriptedRepliesError(
            f"{source}: reply {index} for path '{path}': unknown key(s) "
            f"{unknown}; one of {sorted(allowed)}"
        )


def _parse_error(entry: dict[str, Any], *, path: str, index: int, source: Path) -> _Reply:
    error = entry["error"]
    if not isinstance(error, dict) or "kind" not in error:
        raise ScriptedRepliesError(
            f"{source}: reply {index} for path '{path}': 'error' must be a "
            "mapping with a 'kind'"
        )
    _reject_unknown_keys(error, _ERROR_KEYS, path=path, index=index, source=source)
    kind = error["kind"]
    if kind not in _DEFAULT_STATUS:
        raise ScriptedRepliesError(
            f"{source}: reply {index} for path '{path}': unknown error kind "
            f"{kind!r}; one of {sorted(_DEFAULT_STATUS)}"
        )
    status = error.get("status", _DEFAULT_STATUS[kind])
    if status is not None and not _is_plain_int(status):
        raise ScriptedRepliesError(
            f"{source}: reply {index} for path '{path}': 'status' must be an int"
        )
    # Every kind except timeout/connection carries a status by construction
    # (``_retry_info`` asserts as much at call time) — an explicit
    # ``status: null`` must be rejected here, not left to crash later with
    # an assertion that names no path.
    if status is None and kind not in _ALWAYS_RETRYABLE:
        raise ScriptedRepliesError(
            f"{source}: reply {index} for path '{path}': error kind {kind!r} "
            "needs a 'status'"
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
        if "text" in entry:
            raise ScriptedRepliesError(
                f"{source}: reply {index} for path '{path}': exactly one of "
                "'text' or 'error', not both"
            )
        return _parse_error(entry, path=path, index=index, source=source)
    _reject_unknown_keys(entry, _TEXT_REPLY_KEYS, path=path, index=index, source=source)
    text = entry.get("text")
    if not isinstance(text, str):
        raise ScriptedRepliesError(
            f"{source}: reply {index} for path '{path}' needs a string "
            "'text', or an 'error'"
        )
    for key in ("tokens_sent", "tokens_received"):
        value = entry.get(key)
        if value is not None and (not _is_plain_int(value) or value < 0):
            raise ScriptedRepliesError(
                f"{source}: reply {index} for path '{path}': '{key}' must be an int >= 0"
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
    _registered_for_export: bool = field(default=False, repr=False, compare=False)

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
        if not self._registered_for_export:
            # Registered here, on first actual dispatch -- never at
            # construction time -- so a preflight-only instance (built
            # solely to call `check()`, which loads the replies file but
            # never consumes one, see `cli.runtime_shim.preflight`) never
            # contributes a false "leftover" count for replies nothing
            # ever asked it to use.
            _register_for_leftover_export(self)
            self._registered_for_export = True
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
                # Names only the effect path, never the replies-file path:
                # this lands verbatim in the run's own `meta.error`/
                # `fallback_attempts[].error` state (spec §6), which a
                # conformance harness compares byte-for-byte, and the
                # replies file is typically a harness-generated temp path
                # that differs run to run.
                raise RuntimeError(
                    f"scripted adapter: no reply configured for path '{path}'"
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
