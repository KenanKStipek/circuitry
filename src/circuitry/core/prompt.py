from __future__ import annotations

import copy
import hashlib
import json
import logging
import math
import threading
import time
from collections.abc import Callable, Iterator, Sequence
from contextlib import AbstractContextManager, contextmanager, nullcontext
from dataclasses import dataclass, replace
from dataclasses import field as _dataclass_field
from datetime import datetime, timezone
from typing import Any, Literal

from ..adapters import Adapter, build_adapter
from ..adapters._retry import (
    RetryInfo,
    classify_exception,
    failure_fields,
    next_backoff_delay_ms,
)
from ..adapters.base import (
    TRUNCATED_FINISH_REASONS,
    ChatMessage,
    GenerateOptions,
    GenerateProgress,
    GenerateResult,
    ImageInput,
    call_generate,
)
from ..adapters.factory import configured_timeout_seconds
from ..agent_live import LivePublisher
from ..allowlist_gate import AllowlistError, allowed_adapters, require_adapter
from ..cli.redaction import redact
from ..output import console as _console
from ..output import live_region as _live_region
from .answers import AnswerParseError, parse_boolean_answer, parse_number_answer
from .cancellation import get_token
from .concurrency import RUNTIME_CONFIG_KEY as _CONCURRENCY_LIMITER_KEY
from .effect_identity import model_call
from .prompt_compose import (
    declared_prompts,
    known_effect_names,
    render_with_composition,
)
from .prompt_files import DispatchFileSource, read_dispatch_file
from .prompt_record import PromptSentSetting, prompt_sent_fields, prompt_sent_setting
from .store import Store
from .templates import render_template
from .timeout_ref import TimeoutFrom, resolve_timeout_ms

logger = logging.getLogger(__name__)

#: A decode/schema-validation failure's raw reply is capped before it goes
#: into ``meta.fallback_attempts`` — state is serialized to ``--out``,
#: ``--json``, ``--live-state``, and a model's wall of "maybe" is only useful
#: as a diagnostic, not reproduced in full.
_RAW_REPLY_CAP_CHARS = 2000


class SchemaValidationError(ValueError):
    """A prompt's decoded output failed its declared JSON schema.

    Carries the model's raw, unparsed response text so a caller several
    layers up an exception chain (see ``core.decompose``'s planner-failure
    handling) can recover exactly what was rejected, without re-deriving it
    from the error message.
    """

    def __init__(self, message: str, *, raw_response_text: str | None = None) -> None:
        super().__init__(message)
        self.raw_response_text = raw_response_text


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


#: ``meta.live``'s own clock (#674), captured as a direct reference rather
#: than calling ``_now_iso()`` by name: a test that monkeypatches the
#: module attribute ``_now_iso`` to script a short, fixed sequence of
#: timestamps for ``created_at``/``completed_at`` (see
#: ``tests/core/test_prompt_retry_classification.py``) must not have its
#: sequence consumed by ``meta.live``'s own, much more frequent writes.
_live_now_iso = _now_iso


def _elapsed_str(seconds: float) -> str:
    if seconds >= 1:
        return f"{seconds:.2f}s"
    return f"{seconds * 1000:.0f}ms"


def _adapter_target(adapter: Any, model: str) -> str:
    """Return a human-readable 'adapter · model @ host' string."""
    from urllib.parse import urlparse

    adapter_name = getattr(adapter, "name", "unknown")
    base_url = getattr(adapter, "base_url", None) or getattr(adapter, "api_base", None)
    if base_url:
        host = urlparse(str(base_url)).hostname or str(base_url)
        return f"{adapter_name} · {model} @ {host}"
    return f"{adapter_name} · {model}"


def _decode_failure_status(exc: Exception) -> str:
    """The ``attempts_meta`` status for a decode/validation-stage failure."""
    if isinstance(exc, AnswerParseError):
        return "decode_failed"
    if isinstance(exc, SchemaValidationError):
        return "schema_invalid"
    return "failed"


def _cap_reply_text(text: str) -> str:
    """``text``, capped at :data:`_RAW_REPLY_CAP_CHARS` with a note of how much was cut."""
    if len(text) <= _RAW_REPLY_CAP_CHARS:
        return text
    return f"{text[:_RAW_REPLY_CAP_CHARS]}... [truncated, {len(text)} chars total]"


def _retry_info_for(exc: Exception | None) -> RetryInfo:
    """``RetryInfo`` for the error that ended a dispatch pass.

    A decode/schema-validation failure (an unreadable boolean/number, JSON
    that failed its schema) is always retryable here — it isn't an
    HTTP-classified adapter-call failure at all, retrying is what might get a
    better reply out of the same model, and treating it otherwise would
    change behaviour for an effect with no fallbacks configured, which #281
    requires stay exactly as it was. Everything else defers to
    :func:`classify_exception` (429/408/5xx/connection failures retry;
    400/401/403/404/422 and a missing key do not).
    """
    if isinstance(exc, (AnswerParseError, SchemaValidationError)):
        return RetryInfo(retryable=True)
    return classify_exception(exc)


def _clear_unreported_cache_fields(meta: dict[str, Any]) -> None:
    """Drops the four cache-count keys their shared seed sets to ``None``
    placeholders, for an exit path that never dispatches an attempt at all
    (a dry run, a successful decomposition) -- these are absent when not
    reported, unlike ``tokens_sent``/``tokens_received``, which stay null.
    """
    for key in (
        "tokens_cached_in",
        "tokens_cache_write",
        "tokens_cached_in_total",
        "tokens_cache_write_total",
    ):
        if meta.get(key) is None:
            meta.pop(key, None)


def _attempt_entry(
    *,
    created_at: str,
    completed_at: str,
    value: Any,
    error: str | None,
    fallback_attempts: list[dict[str, Any]],
    tokens_sent: int | None,
    tokens_received: int | None,
    tokens_cached_in: int | None = None,
    tokens_cache_write: int | None = None,
) -> dict[str, Any]:
    """One ``meta.attempts`` entry for a prompt: ``core.use._attempt_entry``'s
    counterpart, with a prompt's own per-attempt fields in place of a use
    effect's child-run ones. A prompt has no ``expect`` to carry (see
    :class:`PromptDefinition`), unlike ``tool``'s own entry.

    ``tokens_cached_in``/``tokens_cache_write`` are included only when not
    ``None`` -- unlike ``tokens_sent``/``tokens_received``, which are always
    present: absent, not null, when the provider never reported one.
    """
    entry: dict[str, Any] = {
        "created_at": created_at,
        "completed_at": completed_at,
        "value": copy.deepcopy(value),
        "error": error,
        "fallback_attempts": copy.deepcopy(fallback_attempts),
        "tokens_sent": tokens_sent,
        "tokens_received": tokens_received,
    }
    if tokens_cached_in is not None:
        entry["tokens_cached_in"] = tokens_cached_in
    if tokens_cache_write is not None:
        entry["tokens_cache_write"] = tokens_cache_write
    return entry


def _write_attempts(meta: dict[str, Any], attempts: list[dict[str, Any]]) -> None:
    """``meta.attempts``, always written as the last key of *meta* -- same
    contract as ``core.use._write_attempts``."""
    meta.pop("attempts", None)
    meta["attempts"] = list(attempts)


def _fallback_attempt_entry(
    *,
    adapter: str,
    model: str,
    status: str,
    error: str | None,
    res: GenerateResult,
    extra: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """One ``meta.fallback_attempts`` entry for an attempt that reached a
    provider reply (as opposed to a failed adapter call, which has no
    ``res`` and so no tokens at all). ``tokens_cached_in``/
    ``tokens_cache_write`` are included only when ``res`` carries them.
    """
    entry: dict[str, Any] = {
        "adapter": adapter,
        "model": model,
        "status": status,
        "error": error,
        "tokens_sent": res.tokens_sent,
        "tokens_received": res.tokens_received,
    }
    if res.tokens_cached_in is not None:
        entry["tokens_cached_in"] = res.tokens_cached_in
    if res.tokens_cache_write is not None:
        entry["tokens_cache_write"] = res.tokens_cache_write
    if extra:
        entry.update(extra)
    return entry


@dataclass
class _LiveAttemptState:
    """One attempt's running ``meta.live`` figures (#674), updated from
    whichever thread the streaming adapter's own reader thread calls
    ``on_progress`` from — never the thread running :meth:`PromptRuntime.execute`
    itself. Protected by ``lock`` for exactly that reason.
    """

    lock: threading.Lock = _dataclass_field(default_factory=threading.Lock)
    started_at: str = _dataclass_field(default_factory=_live_now_iso)
    first_token_at: str | None = None
    first_token_monotonic: float | None = None
    tokens_sent: int | None = None
    tokens_received: int | None = None
    tokens_received_estimate: int = 0
    chars_received: int = 0

    def tokens_per_second(self) -> float | None:
        # Below two deltas, or within 500ms of the first one, the estimate
        # is dividing a tiny count by noise (scheduling jitter, clock
        # resolution), giving a wildly inflated rate rather than a useful
        # figure.
        if self.first_token_monotonic is None or self.tokens_received_estimate < 2:
            return None
        elapsed = time.monotonic() - self.first_token_monotonic
        if elapsed < 0.5:
            return None
        return self.tokens_received_estimate / elapsed

    def apply(self, progress: GenerateProgress) -> None:
        if progress.delta_chars > 0:
            self.chars_received += progress.delta_chars
            self.tokens_received_estimate += progress.delta_count
            if self.first_token_at is None:
                self.first_token_at = _live_now_iso()
                self.first_token_monotonic = time.monotonic()
        if progress.tokens_sent is not None:
            self.tokens_sent = progress.tokens_sent
        if progress.tokens_received is not None:
            self.tokens_received = progress.tokens_received

    def snapshot(self, *, attempt: int, adapter: str, model: str, streaming: bool, tokens_sent_estimate: int) -> dict[str, Any]:
        return {
            "v": 1,
            "attempt": attempt,
            "adapter": adapter,
            "model": model,
            "started_at": self.started_at,
            "updated_at": _live_now_iso(),
            "streaming": streaming,
            "first_token_at": self.first_token_at,
            "tokens_sent": self.tokens_sent,
            "tokens_sent_estimate": tokens_sent_estimate,
            "tokens_received": self.tokens_received,
            "tokens_received_estimate": self.tokens_received_estimate,
            "chars_received": self.chars_received,
            "tokens_per_second": self.tokens_per_second(),
        }


class _PromptSpinner:
    """Animated single-line spinner for a prompt running in chain/sequential mode."""

    _SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"

    def __init__(
        self,
        name: str,
        target: str = "",
        token_hint: str = "",
        indent: str = "",
        ancestors: list | None = None,
    ) -> None:
        self._name = name
        self._target = target  # e.g. "ollama · model @ host"
        self._token_hint = token_hint  # e.g. "~374tok ↑"
        self._indent = indent
        self._start = time.monotonic()
        self._ancestors = ancestors or []

    def __rich__(self) -> str:
        from .dynamic import _render_ancestors

        elapsed = time.monotonic() - self._start
        char = self._SPINNER[int(elapsed * 8) % len(self._SPINNER)]
        parts: list[str] = []
        if self._target:
            parts.append(self._target)
        parts.append(_elapsed_str(elapsed))
        if self._token_hint:
            parts.append(self._token_hint)
        suffix = " | ".join(parts)
        lines = _render_ancestors(self._ancestors, self._SPINNER)
        lines.append(
            f"{self._indent}[info]{char}[/info] [cyan]◆[/cyan]"
            f" {self._name} [dim]{suffix}[/dim]"
        )
        return "\n".join(lines)


_IMAGE_SIGNATURES = (
    (b"\x89PNG\r\n\x1a\n", "image/png"),
    (b"\xff\xd8\xff", "image/jpeg"),
    (b"GIF87a", "image/gif"),
    (b"GIF89a", "image/gif"),
)


def _image_media_type(data: bytes) -> str | None:
    for signature, media_type in _IMAGE_SIGNATURES:
        if data.startswith(signature):
            return media_type
    if data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        return "image/webp"
    # Only the bytes decide, never the file name: a non-image named x.png must not
    # be sent, and these four formats are all the providers take.
    return None


def _load_image(ref: str) -> tuple[ImageInput, dict[str, Any]]:
    """An image asset and its state record: path, size and sha256, never bytes.

    ``http(s)://`` refs pass through as URLs; anything else is a local path
    (``~`` expanded, relative to the working directory) read here.
    """
    from pathlib import Path

    if ref.startswith(("http://", "https://")):
        return ImageInput(url=ref), {"kind": "image", "ref": ref}
    path = Path(ref).expanduser()
    try:
        data = path.read_bytes()
    except OSError as e:
        raise ValueError(f"image asset {ref!r} could not be read: {e}") from e
    media_type = _image_media_type(data)
    if media_type is None:
        raise ValueError(f"image asset {ref!r} is not a PNG, JPEG, GIF or WebP image")
    record = {
        "kind": "image",
        "ref": ref,
        "media_type": media_type,
        "size": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
    }
    return ImageInput(data=data, media_type=media_type), record


# Prompt types per the spec
PromptType = Literal["text", "json", "boolean", "tool", "number", "array", "object"]


def _complexity_meta(result: Any) -> dict[str, Any]:
    """Serialize a :class:`~circuitry.core.complexity.ComplexityScore` for state.

    Two deliberate departures from ``ComplexityScore.to_dict()``, which is the
    wire format for ``cof score`` rather than for a state node:

    * ``signals`` is a mapping keyed by signal name, not a list. A CEL
      condition addresses state by path, so a list would only be reachable by
      positional index — ``signals.prompt_size.contribution`` is a branchable
      expression, ``signals[0].contribution`` is a hostage to report order.
    * Per-signal ``detail`` is dropped. It carries the counted reference names
      and the full normalization tables, which would multiply the size of every
      prompt node in a persisted run to explain a number the ``note`` already
      summarizes. Re-score the effect to get it back.
    """
    return {
        "score": result.score,
        "max_score": result.max_score,
        "mode": result.mode,
        "estimated": result.estimated,
        "weight_total": result.weight_total,
        "signals": {
            signal.name: {
                "raw": signal.raw,
                "normalized": signal.normalized,
                "weight": signal.weight,
                "contribution": signal.contribution,
                "note": signal.note,
            }
            for signal in result.signals
        },
        "warnings": list(result.warnings),
    }


@dataclass(frozen=True)
class MessageDef:
    """A single message in a messages-based prompt."""

    role: Literal["system", "user", "assistant", "tool"]
    content: str
    #: ``content: {file: {from: ...}, within: ...}`` (#578): the text is read
    #: from this file when the effect dispatches; *content* is then ``""``.
    file: DispatchFileSource | None = None


@dataclass(frozen=True)
class AssetRefDef:
    """Reference to a non-text asset (image, file, audio)."""

    kind: str  # e.g. "image", "file", "audio"
    ref: str  # resolvable id/path/uri


@dataclass(frozen=True)
class RetryPolicyDef:
    """Retry configuration for prompts."""

    max_attempts: int = 1
    backoff_ms: int = 1000


@dataclass(frozen=True)
class SplitMergeDef:
    """``split.merge`` — a reduce prompt's own template (see
    :mod:`circuitry.core.decompose`'s size trigger)."""

    template: str


@dataclass(frozen=True)
class SplitDef:
    """``split`` on a ``prompt`` effect — the size trigger's per-effect overlay.

    Every field is optional and, when unset, defers to
    ``runtime.complexity.splitting``: ``enabled: False`` always opts the
    effect out regardless of the runtime switch; ``enabled`` left unset (the
    common case) means "follow the runtime switch"; ``budget_tokens`` set
    (with ``enabled`` unset or ``True``) splits even when the runtime switch
    is off.
    """

    enabled: bool | None = None
    budget_tokens: int | None = None
    max_parts: int | None = None
    separator: str | None = None
    merge: SplitMergeDef | None = None


@dataclass(frozen=True)
class PromptDefinition:
    """
    A Prompt is the atomic execution unit in Circuitry.

    Per the spec:
    - Exactly one of 'template' or 'messages' must be provided
    - prompt_type defines the expected output shape
    - schema provides JSON Schema for validation (if applicable)
    """

    name: str

    # Primary input form (exactly one must be provided)
    template: str | None = None
    #: ``template: {file: {from: ...}, within: ...}`` (#578): the text is read
    #: from this file when the effect dispatches; *template* is then ``None``.
    template_file: DispatchFileSource | None = None
    messages: Sequence[MessageDef] | None = None

    # Typing and decoding
    prompt_type: PromptType = "text"
    schema: dict[str, Any] | None = None

    # Model configuration
    model: str | None = None
    provider: str | None = None
    provider_fallbacks: Sequence[str] | None = None

    # Profile-only per-effect routing overlay (see core.compiler._overlay_effect
    # and cli.profiles) — never set directly from orchestration YAML, only
    # overlaid by a profile's `effects.<path>.routing` key. ``None``: no
    # override, this effect follows the run's router normally. ``False``: opt
    # this effect out of the router — it always dispatches on the model it
    # would have with routing switched off entirely. A non-empty ``str``: pin
    # the effect to the routing band with that ``name``, bypassing
    # score-based band selection.
    routing_override: bool | str | None = None

    # Execution parameters
    params: dict[str, Any] | None = None
    #: A literal, or ``{from: <path>, default: <int>}`` read from state at each
    #: attempt's dispatch (#564).
    timeout_ms: int | TimeoutFrom | None = None
    deterministic: bool = False

    # Prompt-local structured values
    inputs: dict[str, Any] | None = None

    # Non-text inputs
    assets: Sequence[AssetRefDef] | None = None

    # Reliability
    retries: RetryPolicyDef | None = None
    on_error: Literal["fail", "skip", "continue"] = "fail"

    # Description (for documentation/LLM guidance)
    description: str | None = None

    # False = skip execution and write a disabled node (see core.disabled).
    enabled: bool = True

    # Named concurrency-group slot this effect holds for the duration of its
    # dispatch (core.concurrency.RunConcurrencyLimiter) — None means it only
    # ever competes for runtime.max_concurrency's run-wide slots, if any are
    # configured. A prompt effect is a leaf, so this is where a slot is
    # actually held; see #274.
    group: str | None = None

    # The size trigger's per-effect overlay (#672). None (the default): the
    # runtime's own runtime.complexity.splitting switch and defaults apply
    # unmodified.
    split: SplitDef | None = None


class PromptRuntime:
    """
    Executes a PromptDefinition against adapter + store.
    Writes:
      <name>.value
      <name>.meta{created_at, completed_at, adapter, model, model_reason,
                  prompt_type, prompt_sent, prompt_sent_sha256, prompt_chars,
                  tokens_sent, tokens_received,
                  tokens_sent_total, tokens_received_total, error, dry_run,
                  fallback_attempts, fallback_recovered, retries_used?,
                  complexity?, assets?, finish_reason?, warnings?, cost_usd?,
                  attempts?}

    ``meta.created_at`` is the start of this pass's first attempt, not reset
    on a retry. ``meta.retries_used`` is set on both a successful and a
    failed outcome, to the index of the attempt that decided the outcome;
    it's absent, not 0, when the first attempt is also the last (#421) —
    same rule as a tool effect (core.tool.ToolRuntime).

    ``meta.attempts`` is present exactly when ``meta.retries_used`` is: one
    entry per attempt this pass made, oldest first, the last entry
    repeating the fields that decided the outcome. Each entry carries its
    own ``created_at``, ``completed_at``, ``value``, ``error``,
    ``fallback_attempts`` (that attempt's own provider tries — a #660
    ``error_code``/``http_status`` lives inside each of those, not at the
    entry's own top level), ``tokens_sent`` and ``tokens_received``. It is
    cleared at the start of every pass, so a reused node never shows an
    earlier pass's attempts, and it is always the last key of ``meta``. A
    prompt has no ``expect`` to carry, unlike a tool or use effect's own
    entry (core.tool.ToolRuntime, core.use.UseRuntime). The token totals
    below already include every attempt recorded here — ``meta.attempts``
    adds no new spend, just a readable history of it.

    ``assets`` (one ``{kind, ref, size?, sha256?, media_type?}`` per image
    sent — never the bytes), ``finish_reason`` (when the provider reports
    one), ``cost_usd`` (the winning attempt's cost, when its adapter reports
    one) and ``warnings`` (a reply cut off at the length limit, options an
    adapter ignored, an asset kind no adapter sends) appear only when they
    have something to say.

    ``tokens_sent``/``tokens_received`` are the winning attempt's own count,
    unchanged in meaning from before; ``tokens_sent_total``/
    ``tokens_received_total`` sum every attempt this execution made —
    failed, retried and fallen-back-from alike, across every retry pass, not
    just the chain that finally answered. Each entry in ``fallback_attempts``
    (the chain from the pass that decided the outcome) carries its own
    ``tokens_sent``/``tokens_received`` too, ``None`` for an attempt that
    never got a reply. An attempt whose reply couldn't be used — an
    unreadable boolean/number, or JSON that failed its schema — gets
    ``status: "decode_failed"``/``"schema_invalid"`` and a size-capped
    ``raw_reply``, and the chain moves to the next provider before the retry
    loop ever sees it (#281); a plain adapter-call failure keeps
    ``status: "failed"``, no ``raw_reply``.

    A failed pass only retries when the dispatch's last error classifies as
    retryable (429, 408, 5xx, a timeout, a dropped connection — see
    ``adapters/_retry.py``); anything else (400/401/403/404/422, a missing
    key) fails the effect immediately rather than spending the rest of the
    retry budget on something that cannot succeed. A retry's wait is
    exponential backoff with full jitter from ``retries.backoff_ms``, capped
    at 60s, overridden outright by a provider's own ``Retry-After`` when the
    failing adapter can supply one (litellm; curl-based adapters cannot yet —
    see the adapter docs).

    ``model`` is the resolved model and ``model_reason`` says who chose it:
    ``"explicit"`` when the definition names its own ``model:`` (which is also
    how a profile effect override arrives — it is overlaid onto the
    definition), ``"router"`` when the complexity router substituted a band's
    model for the run default, and ``"default"`` when the effect simply
    inherited the run's model.

    ``complexity`` is the one conditional key: it is present only when
    ``runtime.complexity.scoring.enabled`` is true, and absent entirely — not
    null, not an empty object — when it is not, so turning scoring off leaves
    the state tree byte-identical to a build without the feature. When present
    it carries ``{score, max_score, mode, estimated, weight_total, signals,
    warnings, band?}``, with ``signals`` keyed by signal name so a CEL
    condition can branch on ``state.<path>.meta.complexity.score`` or on any
    single signal's contribution. ``band`` is itself conditional on
    ``runtime.complexity.routing.enabled`` — ``{name, model}`` naming the band
    the score falls in and the model that band names. It is recorded whether or
    not the router acted on it: with ``respect_explicit`` on (the default) an
    effect that names its own model keeps ``model_reason == "explicit"`` and a
    ``band`` that says what routing *would* have picked. ``model_reason`` is
    the field that says whether the band was applied; ``band`` alone never
    implies it was.

    Every one of these keys except the result-bearing ones (``value``,
    ``tokens_*``, ``completed_at``, ``error``) is written *before* dispatch.
    That is what makes the complexity score readable on an effect that failed,
    which is the case worth diagnosing.

    ``decomposition`` is the other conditional key, and unlike ``complexity``
    it is written *at* dispatch, because it records a dispatch decision: it
    appears when either trigger acts — the size trigger (the estimated input
    strictly exceeds a configured budget) or the score trigger
    (``runtime.complexity.decomposition`` is enabled and this effect's score
    strictly exceeded the threshold), size checked first. ``trigger``
    (``"score"``/``"size"``) is the one key that tells the two apart; both
    share ``{decomposed, trigger, outcome, reason, depth, max_depth,
    chunk_count?, yaml?, result_path?, error?}``. The score trigger adds
    ``{score, threshold, plan?, fallback_model?}``; the size trigger adds
    ``{estimated_tokens, budget, merge?, split?}`` and never sets
    ``fallback_model`` (there is no route-up for size). Whichever trigger
    acted, the effect was either actually replaced by the fan-out (``value``
    then holds the child's merged result), routed up to a more capable model
    (score only), run as-is, or failed under ``on_failure: fail``. See
    :mod:`circuitry.core.decompose`.
    """

    def __init__(
        self,
        definition: PromptDefinition,
        *,
        adapter: Adapter,
        model: str,
        model_locked: bool = False,
        runtime_config: dict[str, Any] | None = None,
        dry_run: bool = False,
        timeout_seconds: int = 120,
        verbose: bool = False,
        depth: int = 0,
        cb_start: Callable[[], None] | None = None,
        cb_done: Callable[[str], None] | None = None,
        cb_error: Callable[[str], None] | None = None,
        cb_running: Callable[[str, int], None] | None = None,
        display_name: str | None = None,
        ancestors: list | None = None,
    ):
        self.defn = definition
        self.adapter = adapter
        self.model = model
        # True when the run default was pinned by a deliberate choice —
        # ``--model`` on the CLI or a profile's run-level ``model:`` — rather
        # than inherited from the orchestration or config. The complexity
        # router outranks the latter two and defers to the former, and this is
        # the only thing down here that can tell them apart: by the time a
        # model reaches this constructor it is just a string.
        self.model_locked = model_locked
        self.runtime_config = runtime_config or {}
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds
        self.verbose = verbose
        self.depth = depth
        self.cb_start = cb_start
        self.cb_done = cb_done
        self.cb_error = cb_error
        self.cb_running = cb_running
        self.display_name = display_name or definition.name
        self._ancestors = ancestors or []
        self._prompt_sent_setting: PromptSentSetting = prompt_sent_setting(self.runtime_config)

    def execute(self, *, store: Store, ctx: dict[str, Any]) -> None:
        node = store.ensure_dict(self.defn.name)
        node.setdefault("value", None)
        meta = node.get("meta")
        if not isinstance(meta, dict):
            meta = {}
            node["meta"] = meta

        # Build effective context with prompt-local inputs
        effective_ctx = dict(ctx)
        if self.defn.inputs:
            effective_ctx.update(self.defn.inputs)

        # Materialize prompt input. A template that fails to render is this
        # effect's failure, under its own on_error — never the raw text sent
        # to the model as if it had rendered.
        # A ``file: {from: ...}`` source is read here, once, before the first
        # attempt: retries and fallback reuse its text (#578).
        try:
            file_texts, file_records = self._read_dispatch_files(effective_ctx)
            messages = self._render_messages(effective_ctx, file_texts)
            prompt_sent = self._materialize_input(effective_ctx, messages, file_texts)
        except Exception as e:
            meta["created_at"] = _now_iso()
            meta["error"] = redact(str(e))
            meta.pop("error_code", None)
            meta.pop("http_status", None)
            meta["completed_at"] = _now_iso()
            node["value"] = None
            store.fire_effect_start(self.defn.name, node)
            if self.verbose:
                if self.cb_start is not None:
                    self.cb_start()
                line = (
                    f"{'  ' * self.depth}[err]✗[/err] [cyan]◆[/cyan] {self.display_name}"
                    " [dim]template did not render[/dim]"
                )
                if self.cb_error is not None:
                    self.cb_error(line)
                else:
                    _console.print(line)
            store.fire_effect_complete(self.defn.name, node)
            if self.defn.on_error == "fail":
                raise
            return

        # Resolved once, ahead of the meta block that reports it: a per-effect
        # ``model:`` always wins over the run's default, and dispatch further
        # down (_build_attempts) must build its attempt chain from this same
        # value rather than recomputing it — the resolution rule has exactly
        # one place to live.
        resolved_model = self.defn.model or self.model

        # Record metadata
        meta["created_at"] = _now_iso()
        meta["completed_at"] = None
        meta["adapter"] = getattr(self.adapter, "name", "unknown")
        meta["model"] = resolved_model
        # "explicit" when this effect names its own model (a per-effect
        # ``model:`` or a profile override overlaid onto the definition),
        # "default" when it inherits the run's. Overwritten with "router"
        # below if the router substitutes a band's model for the default.
        meta["model_reason"] = "explicit" if self.defn.model else "default"
        meta["prompt_type"] = self.defn.prompt_type
        # What ``runtime.state.prompt_sent`` leaves of the text, plus the two
        # fields that describe all of it (#577). Only this record is shaped:
        # ``prompt_sent`` itself stays whole for everything below.
        meta.update(prompt_sent_fields(prompt_sent, self._prompt_sent_setting))
        meta["tokens_sent"] = None
        meta["tokens_received"] = None
        # Seeded here, right next to the field each belongs with, so a
        # first-time assignment later keeps this position instead of
        # appending after whatever else this pass has written by then.
        meta["tokens_cached_in"] = None
        meta["tokens_cache_write"] = None
        meta["tokens_sent_total"] = None
        meta["tokens_received_total"] = None
        meta["tokens_cached_in_total"] = None
        meta["tokens_cache_write_total"] = None
        meta["error"] = None
        meta["dry_run"] = self.dry_run
        meta["fallback_attempts"] = []
        meta["fallback_recovered"] = False
        # Reset on every pass/attempt rather than only on success — otherwise
        # a reused node (an unnamed loop's repeated pass, a --state resume)
        # keeps a prior pass's retry count, decomposition record, or
        # generation-option metadata sitting next to this pass's error,
        # misrepresenting what actually happened on this pass (#260).
        meta.pop("retries_used", None)
        meta.pop("decomposition", None)
        meta.pop("answer", None)
        # Cleared here, once per pass, not per attempt -- a pass that ends
        # up making only one attempt must not keep a reused node's earlier
        # pass's meta.attempts around (#668).
        meta.pop("attempts", None)
        # Written only when they have content, so clear what an earlier
        # iteration of an unnamed loop (same store) left behind.
        for key in (
            "finish_reason",
            "warnings",
            "assets",
            "cost_usd",
            "template_files",
            "error_code",
            "http_status",
            # A stale `live` from a killed run (#674) must not survive a
            # `--resume` pass that never reaches `_live_attempt` at all
            # (dry run, decomposition, an early failure) -- the same
            # leftover `tool.py` pops for the agent tool's own `meta.live`.
            "live",
        ):
            meta.pop(key, None)
        meta["waiting_for"] = None
        if file_records:
            meta["template_files"] = file_records

        # Scored here, alongside the rest of the pre-dispatch meta, so the
        # score is on the node before anything can go wrong. A post-success
        # write would omit exactly the effects worth explaining.
        complexity, routed_model = self._score_and_route(rendered_prompt=prompt_sent)
        if complexity is not None:
            meta["complexity"] = complexity

        # The router's substitution, and its whole footprint: it replaces the
        # value ``resolved_model`` already holds and nothing else. Everything
        # downstream — the verbose target line, _build_attempts and therefore
        # the provider/provider_fallbacks chain — reads that same variable, so
        # a routed model behaves exactly like one the effect had named itself.
        if routed_model is not None:
            resolved_model = routed_model
            meta["model"] = resolved_model
            meta["model_reason"] = "router"

        # Everything an observer needs to reason about the decision being
        # dispatched — resolved adapter/model, the rendered prompt, and the
        # complexity score once scoring writes it here — is on the node by
        # now, so start fires before any of the execution branches below.
        store.fire_effect_start(self.defn.name, node)

        indent = "  " * self.depth
        estimated_out = len(prompt_sent) // 4
        t0 = time.monotonic()
        target = self._pre_dispatch_target(resolved_model) if self.verbose else ""

        if self.verbose and self.cb_start is not None:
            self.cb_start()
        if self.verbose and self.cb_running is not None:
            self.cb_running(target, estimated_out)

        if self.dry_run:
            node["value"] = None
            meta["completed_at"] = _now_iso()
            _clear_unreported_cache_fields(meta)
            if self.verbose:
                elapsed = time.monotonic() - t0
                if self.cb_done is not None:
                    line = (
                        f"{indent}[ok]✓[/ok] [cyan]◆[/cyan] {self.display_name}"
                        f" [dim]{target} | {_elapsed_str(elapsed)}[/dim]"
                    )
                    self.cb_done(line)
                else:
                    _console.print(
                        f"{indent}[ok]✓[/ok] [cyan]◆[/cyan] {self.display_name}"
                        f" [dim]{_elapsed_str(elapsed)}[/dim]"
                    )
            store.fire_effect_complete(self.defn.name, node)
            return

        # Determine retry policy: per-prompt config > runtime default > 1 (no retry)
        if self.defn.retries is not None:
            max_attempts = self.defn.retries.max_attempts
            backoff_ms = self.defn.retries.backoff_ms
        else:
            max_attempts = int(self.runtime_config.get("default_prompt_retries", 1))
            backoff_ms = 1000

        attempts_meta: list[dict[str, Any]] = []
        total_tokens_sent: int | None = None
        total_tokens_received: int | None = None
        total_tokens_cached_in: int | None = None
        total_tokens_cache_write: int | None = None
        # Every attempt this pass makes, oldest first (#668) -- written to
        # meta.attempts only once more than one attempt has run, exactly
        # when meta.retries_used is also set. Named apart from `attempts`
        # (the adapter/model fallback chain `_build_attempts` returns,
        # below) -- the two are unrelated lists of the same generic word.
        retry_attempts: list[dict[str, Any]] = []
        # Holds the retry loop's own index once it starts, so the except
        # block below can report retries_used even when the raise came from
        # inside the loop (0 if the failure happened before the loop ever
        # ran, e.g. decomposition or building the attempt chain).
        _attempt = 0
        final_fields: dict[str, Any] = {}
        try:
            # Decomposition sits right at the dispatch seam: it either replaces
            # the model call entirely (the merged child result lands at this
            # effect's own path and nothing below runs), falls through to a
            # normal dispatch — possibly on a more capable model (route up) —
            # or raises under `on_failure: fail`, where the ordinary error
            # path below records it like any other dispatch failure.
            dispatch_model = resolved_model
            decomposition = self._maybe_decompose(
                store=store,
                node=node,
                ctx=effective_ctx,
                complexity=complexity,
                prompt_sent=prompt_sent,
                resolved_model=resolved_model,
            )
            if decomposition is not None:
                meta["decomposition"] = decomposition.meta
                if decomposition.succeeded:
                    node["value"] = decomposition.value
                    meta["completed_at"] = _now_iso()
                    _clear_unreported_cache_fields(meta)
                    if self.verbose:
                        elapsed = time.monotonic() - t0
                        chunk_count = decomposition.meta.get("chunk_count")
                        verb = (
                            f"split · {chunk_count} parts"
                            if decomposition.meta.get("trigger") == "size"
                            else f"decomposed · {chunk_count} chunks"
                        )
                        line = (
                            f"{indent}[ok]✓[/ok] [cyan]◆[/cyan] {self.display_name}"
                            f" [dim]{verb}"
                            f" | {_elapsed_str(elapsed)}[/dim]"
                        )
                        if self.cb_done is not None:
                            self.cb_done(line)
                        else:
                            _console.print(line)
                    store.fire_effect_complete(self.defn.name, node)
                    return
                if decomposition.fail:
                    from .decompose import DecompositionError

                    raise DecompositionError(
                        f"decomposition of '{self.defn.name}' failed "
                        f"({decomposition.failure}): {decomposition.error}"
                    )
                if decomposition.fallback_model:
                    dispatch_model = decomposition.fallback_model

            # Acquired fresh each attempt below and released before this
            # iteration's end (backoff sleep included) — a retried/
            # fallen-back-from attempt still competes for a new slot each
            # time, the same contract tool.py's retry loop holds (#333 x
            # #281): a retrying GPU dispatch must never sleep through
            # backoff while still holding its group.
            limiter = self.runtime_config.get(_CONCURRENCY_LIMITER_KEY)

            def _on_wait(label: str) -> None:
                store.set(f"{self.defn.name}.meta.waiting_for", label)
                if self.verbose:
                    _console.print(
                        f"{indent}[info]…[/info] [cyan]◆[/cyan] {self.display_name}"
                        f" [dim]waiting for '{label}'[/dim]"
                    )

            def _on_acquired() -> None:
                store.set(f"{self.defn.name}.meta.waiting_for", None)

            options = self._generation_options(
                ctx=effective_ctx, messages=messages, meta=meta
            )
            option_warnings = list(meta.get("warnings", []))
            attempts = self._build_attempts(default_model=dispatch_model)

            next_delay_ms = backoff_ms
            # Numbers meta.live.attempt across every (adapter, model) this
            # execution tries, retries included -- never reset per pass
            # (#674).
            live_attempt_index = 0
            for _attempt in range(max_attempts):
                if _attempt > 0:
                    # Cancellation-aware, not a bare time.sleep — see
                    # core.use's identical comment (#356).
                    get_token().sleep_or_raise(next_delay_ms / 1000)
                    t0 = time.monotonic()
                    if self.verbose:
                        retry_line = (
                            f"{indent}[yellow]↺[/yellow] [cyan]◆[/cyan]"
                            f" {self.display_name} [dim]retry {_attempt}/{max_attempts - 1}[/dim]"
                        )
                        if self.cb_done is not None:
                            self.cb_done(retry_line)
                        else:
                            _console.print(retry_line)

                attempt_started_at = _now_iso()

                # Per attempt, from the state this attempt sees (#564). A
                # reference that can't resolve fails the effect outright
                # instead of retrying: a retry reads the same state.
                timeout_ms = resolve_timeout_ms(
                    self.defn.timeout_ms,
                    effective_ctx,
                    effect_label=f"Prompt effect '{self.defn.name}'",
                )

                if self.verbose and self.cb_start is None:
                    from rich.live import Live

                    live_cm: Any = _live_region(
                        lambda: Live(
                            _PromptSpinner(
                                name=self.display_name,
                                target=target,
                                token_hint=f"~{estimated_out}tok ↑",
                                indent=indent,
                                ancestors=self._ancestors,
                            ),
                            refresh_per_second=10,
                            transient=True,
                            console=_console,
                        )
                    )
                else:
                    live_cm = nullcontext()
                concurrency_cm: AbstractContextManager[None] = (
                    limiter.acquire(
                        group=self.defn.group, on_wait=_on_wait, on_acquired=_on_acquired
                    )
                    if limiter is not None
                    else nullcontext()
                )
                with concurrency_cm, live_cm, model_call(store, self.defn.name):
                    res, decoded_value, attempts_meta, generation_error, live_attempt_index = (
                        self._generate_with_fallbacks(
                            prompt=prompt_sent,
                            attempts=attempts,
                            timeout_ms=timeout_ms,
                            options=options,
                            meta=meta,
                            option_warnings=option_warnings,
                            store=store,
                            attempt_index=live_attempt_index,
                        )
                    )

                # Every attempt this pass made, win or lose, spent tokens —
                # accumulated across passes so a retried or fallen-back-from
                # attempt's cost isn't lost when this loop overwrites
                # `attempts_meta` with the next pass's chain.
                for attempt_entry in attempts_meta:
                    sent = attempt_entry.get("tokens_sent")
                    if sent is not None:
                        total_tokens_sent = (total_tokens_sent or 0) + sent
                    received = attempt_entry.get("tokens_received")
                    if received is not None:
                        total_tokens_received = (total_tokens_received or 0) + received
                    cached_in = attempt_entry.get("tokens_cached_in")
                    if cached_in is not None:
                        total_tokens_cached_in = (total_tokens_cached_in or 0) + cached_in
                    cache_write = attempt_entry.get("tokens_cache_write")
                    if cache_write is not None:
                        total_tokens_cache_write = (total_tokens_cache_write or 0) + cache_write
                meta["tokens_sent_total"] = total_tokens_sent
                meta["tokens_received_total"] = total_tokens_received
                if total_tokens_cached_in is not None:
                    meta["tokens_cached_in_total"] = total_tokens_cached_in
                else:
                    meta.pop("tokens_cached_in_total", None)
                if total_tokens_cache_write is not None:
                    meta["tokens_cache_write_total"] = total_tokens_cache_write
                else:
                    meta.pop("tokens_cache_write_total", None)

                if generation_error is None and res is not None:
                    # Success
                    node["value"] = decoded_value
                    meta["tokens_sent"] = res.tokens_sent
                    meta["tokens_received"] = res.tokens_received
                    if res.tokens_cached_in is not None:
                        meta["tokens_cached_in"] = res.tokens_cached_in
                    else:
                        meta.pop("tokens_cached_in", None)
                    if res.tokens_cache_write is not None:
                        meta["tokens_cache_write"] = res.tokens_cache_write
                    else:
                        meta.pop("tokens_cache_write", None)
                    cost_usd = getattr(res, "cost_usd", None)
                    if cost_usd is not None:
                        meta["cost_usd"] = cost_usd
                    meta["fallback_attempts"] = attempts_meta
                    meta["fallback_recovered"] = len(attempts_meta) > 1
                    meta["completed_at"] = _now_iso()
                    if attempts_meta:
                        last = attempts_meta[-1]
                        meta["adapter"] = last["adapter"]
                        meta["model"] = last["model"]
                    if _attempt > 0:
                        meta["retries_used"] = _attempt

                    retry_attempts.append(
                        _attempt_entry(
                            created_at=attempt_started_at,
                            completed_at=meta["completed_at"],
                            value=node["value"],
                            error=None,
                            fallback_attempts=attempts_meta,
                            tokens_sent=res.tokens_sent,
                            tokens_received=res.tokens_received,
                            tokens_cached_in=res.tokens_cached_in,
                            tokens_cache_write=res.tokens_cache_write,
                        )
                    )
                    if _attempt > 0:
                        _write_attempts(meta, retry_attempts)

                    if self.verbose:
                        elapsed = time.monotonic() - t0
                        suffix = _elapsed_str(elapsed)
                        sent = res.tokens_sent
                        recv = res.tokens_received
                        if sent is not None or recv is not None:
                            suffix += f" | ↑{sent or 0} ↓{recv or 0} tok"
                        completion_target = self._attempts_target(attempts_meta) or target
                        line = (
                            f"{indent}[ok]✓[/ok] [cyan]◆[/cyan] {self.display_name}"
                            f" [dim]{completion_target} | {suffix}[/dim]"
                        )
                        if self.cb_done is not None:
                            self.cb_done(line)
                        else:
                            _console.print(line)

                    store.fire_effect_complete(self.defn.name, node)
                    return

                # Every attempt in this pass's chain failed. Retry only if
                # the chain's LAST failure is a classified-retryable one
                # (429/408/5xx, a timeout, a dropped connection, or a
                # decode/schema failure — see _retry_info_for) and attempts
                # remain; anything else (400/401/403/404/422, a missing key,
                # an unclassified failure) fails the effect now rather than
                # spending the rest of the retry budget on something that
                # can't succeed. Deliberately the last error only, not "any
                # attempt in the chain was retryable": a primary 503 behind a
                # fallback that then answers with a permanent 401 is not
                # retried, while the chain tried in the opposite order is —
                # the fallback chain's own ordering already decided which
                # failure gets the final word on this pass.
                retry_info = _retry_info_for(generation_error)
                # generation_error is never None here: the branch above
                # already returned on `generation_error is None and res is
                # not None`, so reaching this point means the pass's whole
                # fallback chain failed.
                last_fallback = attempts_meta[-1] if attempts_meta else None
                retry_attempts.append(
                    _attempt_entry(
                        created_at=attempt_started_at,
                        completed_at=_now_iso(),
                        value=None,
                        error=redact(str(generation_error)),
                        fallback_attempts=attempts_meta,
                        tokens_sent=(
                            last_fallback.get("tokens_sent") if last_fallback else None
                        ),
                        tokens_received=(
                            last_fallback.get("tokens_received") if last_fallback else None
                        ),
                        tokens_cached_in=(
                            last_fallback.get("tokens_cached_in") if last_fallback else None
                        ),
                        tokens_cache_write=(
                            last_fallback.get("tokens_cache_write") if last_fallback else None
                        ),
                    )
                )
                if not retry_info.retryable or _attempt >= max_attempts - 1:
                    last_status = attempts_meta[-1]["status"] if attempts_meta else ""
                    if generation_error is not None and last_status in (
                        "decode_failed",
                        "schema_invalid",
                    ):
                        # A decode/schema failure is already the one concrete
                        # thing wrong with the reply — raise it as-is rather
                        # than wrapping it in the generic chain-exhaustion
                        # message, exactly as it propagated before this
                        # failure mode moved inside the fallback chain (#281).
                        raise generation_error
                    if attempts_meta:
                        final_fields = {
                            key: attempts_meta[-1][key]
                            for key in ("error_code", "http_status")
                            if key in attempts_meta[-1]
                        }
                    raise RuntimeError(
                        f"All adapter attempts failed: {attempts_meta}"
                    ) from generation_error

                if self.verbose:
                    elapsed = time.monotonic() - t0
                    failure_target = self._attempts_target(attempts_meta) or target
                    line = (
                        f"{indent}[err]✗[/err] [cyan]◆[/cyan] {self.display_name}"
                        f" [dim]{failure_target} | {_elapsed_str(elapsed)}[/dim]"
                    )
                    if self.cb_error is not None:
                        self.cb_error(line)
                    else:
                        _console.print(line)
                # The finished attempts, readable (and on --live-state)
                # before the next one starts (#668) -- another attempt
                # always follows past this point, whatever `_attempt` is.
                _write_attempts(meta, retry_attempts)
                store.publish()
                next_delay_ms = next_backoff_delay_ms(
                    retry_info, attempt_index=_attempt, base_ms=backoff_ms
                )

        except Exception as e:
            if self.verbose:
                elapsed = time.monotonic() - t0
                failure_target = self._attempts_target(attempts_meta) or target
                line = (
                    f"{indent}[err]✗[/err] [cyan]◆[/cyan] {self.display_name}"
                    f" [dim]{failure_target} | {_elapsed_str(elapsed)}[/dim]"
                )
                if self.cb_error is not None:
                    self.cb_error(line)
                else:
                    _console.print(line)
            # Stale if this attempt raised before its own fallback chain
            # ran at all (an unresolved timeout_ms reference between
            # attempts, say) -- attempts_meta would still hold a prior
            # attempt's chain in that case, which this attempt never made.
            attempt_reached_chain = len(retry_attempts) > _attempt
            meta["fallback_attempts"] = attempts_meta if attempt_reached_chain else []
            meta["fallback_recovered"] = False
            meta["error"] = redact(str(e))
            _clear_unreported_cache_fields(meta)
            meta.update(final_fields)
            meta["completed_at"] = _now_iso()
            # Same rule as the success path (#421): retries_used is set on
            # failure too, so a reader can time an effect's duration
            # consistently regardless of outcome — absent, not 0, for a
            # first-attempt failure, matching the success path's "only when
            # above 0".
            if _attempt > 0:
                meta["retries_used"] = _attempt
                if not attempt_reached_chain:
                    # This attempt never reached its own fallback chain (an
                    # unresolved timeout_ms reference between attempts, say)
                    # -- it gets an entry of its own rather than re-dating
                    # an earlier attempt's.
                    retry_attempts.append(
                        _attempt_entry(
                            created_at=attempt_started_at,
                            completed_at=meta["completed_at"],
                            value=None,
                            error=meta["error"],
                            fallback_attempts=[],
                            tokens_sent=None,
                            tokens_received=None,
                            tokens_cached_in=None,
                            tokens_cache_write=None,
                        )
                    )
                else:
                    # Resynced to the node's own deciding fields, which land
                    # a little later than what the failing attempt recorded
                    # for itself inside the loop (#668): completed_at, and
                    # error — a chain-exhaustion failure's entry held only
                    # that attempt's own last fallback error, not the
                    # wrapped "All adapter attempts failed: ..." text
                    # meta.error shows. Tokens follow meta's own
                    # null-on-failure fields, not the last fallback try's
                    # usage, which is still readable inside this entry's own
                    # fallback_attempts.
                    retry_attempts[-1]["completed_at"] = meta["completed_at"]
                    retry_attempts[-1]["error"] = meta["error"]
                    retry_attempts[-1]["tokens_sent"] = meta["tokens_sent"]
                    retry_attempts[-1]["tokens_received"] = meta["tokens_received"]
                    for key in ("tokens_cached_in", "tokens_cache_write"):
                        value = meta.get(key)
                        if value is not None:
                            retry_attempts[-1][key] = value
                        else:
                            retry_attempts[-1].pop(key, None)
                _write_attempts(meta, retry_attempts)
            if self.defn.on_error in ("skip", "continue"):
                # A reused node (an unnamed loop's prior pass, a resume)
                # must not let that pass's value survive next to this
                # pass's error (#260).
                node["value"] = None
            # Fires before the re-raise so the start/complete pair stays
            # balanced on the failure path too — the node carries meta.error.
            store.fire_effect_complete(self.defn.name, node)
            if self.defn.on_error == "fail":
                raise

    def _score_and_route(
        self, *, rendered_prompt: str
    ) -> tuple[dict[str, Any] | None, str | None]:
        """Score this prompt and route it: ``(complexity meta, routed model)``.

        Both halves are optional and independent of each other's presence:

        * The meta is ``None`` — the "add no key" signal — when scoring is off,
          and also when scoring *fails*: a diagnostic that cannot be produced
          must not become a reason the effect itself does not run. Scoring is
          otherwise pure and cheap (no model call, no IO), so it is safe to do
          on the dispatch path.
        * The routed model is ``None`` whenever nothing overrules the model
          already resolved — the router defers (routing off, no band table, or
          an explicit choice it will not overrule; see
          :func:`circuitry.core.router.route_model`), or a profile opted this
          effect out. A caller that gets ``None`` keeps the model it already
          resolved.

        Scoring and routing share this one method because they share one
        settings resolution: routing reads the score, and resolving the block
        twice per effect is how the two could ever disagree about what was
        configured.

        A profile's per-effect ``routing_override`` (opt-out or band pin) is
        settled first, ahead of scoring: an opt-out has nothing to score, and a
        pin names its model directly rather than deriving it from one, so
        neither needs the scoring substrate to be on. An explicit model —
        this effect's own ``model:`` (which is also how a profile ``model:``
        override arrives — overlaid onto the definition), or a run default
        locked by ``--model``/a profile's run-level ``model:`` — still
        outranks both, the same rule the score-based router already follows.
        """
        # Imported lazily: ``core`` reaching into ``cli`` at module scope would
        # invert the dependency the rest of this package maintains.
        from ..cli.complexity_config import (
            band_for,
            band_named,
            resolve_complexity_settings,
        )
        from .complexity import score as score_complexity
        from .router import route_model

        try:
            settings = resolve_complexity_settings(self.runtime_config)
        except Exception:
            # An invalid block is rejected at config resolution long before a
            # run reaches here; if one somehow does, the run is more valuable
            # than the score.
            logger.warning(
                "Could not resolve runtime.complexity; skipping the complexity "
                "score for prompt %r",
                self.defn.name,
                exc_info=True,
            )
            return None, None

        explicit = bool(self.defn.model) or self.model_locked
        override = self.defn.routing_override

        opted_out = False
        pinned_model: str | None = None
        pinned_band_name = ""
        if not explicit and override is not None:
            if override is False:
                opted_out = True
            else:
                pinned = band_named(override, settings.routing.bands)
                if pinned is None:
                    valid = ", ".join(
                        sorted(b.name for b in settings.routing.bands if b.name)
                    ) or "(no named bands configured)"
                    raise ValueError(
                        f"Prompt '{self.defn.name}' is pinned to routing band "
                        f"{override!r}, which is not in "
                        f"runtime.complexity.routing.bands. Valid band names: "
                        f"{valid}."
                    )
                pinned_model = pinned.model
                pinned_band_name = override

        if not settings.scoring.enabled:
            # Score-based routing requires scoring — config resolution rejects
            # the other combination — so no score means no score-based route.
            # A profile pin is not score-based and resolves the same either
            # way; an opt-out has no automatic route to cancel here regardless.
            return None, pinned_model

        # Passed straight through: ``runtime.complexity.scoring.weights`` is
        # keyed by ``complexity.SIGNAL_NAMES``, validated against that same
        # tuple at config resolution. No translation here, and none at any
        # other call site — a second vocabulary is what made a configured
        # weight silently do nothing.
        weights = dict(settings.scoring.weights)


        # An unconfigured keyword table resolves to ``{}``, which the scorer
        # reads as "disable the keyword signal". Only an explicit table should
        # replace the defaults, so empty means "unset" here.
        keyword_weights = dict(settings.scoring.keywords) or None

        try:
            result = score_complexity(
                self.defn,
                rendered_prompt=rendered_prompt,
                weights=weights,
                keyword_weights=keyword_weights,
                # The runtime knows its own nesting; loop membership and
                # reflector provenance are not plumbed through to a prompt yet
                # and default to the top-level case.
                structure={"depth": self.depth},
            )
        except Exception:
            logger.warning(
                "Complexity scoring failed for prompt %r; the effect will run "
                "without a recorded score",
                self.defn.name,
                exc_info=True,
            )
            return None, pinned_model

        meta = _complexity_meta(result)

        # The band this score falls in, per the configured routing table.
        # Recorded whenever routing is on, whether or not the router goes on
        # to act on it: on an effect that pins its own model the band is the
        # answer to "what would routing have picked", which is exactly the
        # question you ask before removing the pin. A non-empty band table
        # always has a catch-all (config resolution enforces it), so a band is
        # found whenever one is configured.
        if settings.routing.enabled and settings.routing.bands:
            band = band_for(result.score, settings.routing.bands)
            if band is not None:
                meta["band"] = {"name": band.name or "", "model": band.model or ""}

        if opted_out:
            return meta, None

        if pinned_model is not None:
            # Overwrites whatever the score-based lookup above recorded: the
            # pin is what actually dispatches, so ``band`` has to name the row
            # that applied, not the one the score would otherwise have landed
            # in.
            meta["band"] = {"name": pinned_band_name, "model": pinned_model}
            return meta, pinned_model

        # An effect that names its own ``model:`` — written on the effect or
        # overlaid there by a profile — and a run whose default was pinned with
        # ``--model`` are the same fact to the router: a human already decided.
        # It collapses them into one flag rather than ranking them, because all
        # three outrank it identically.
        decision = route_model(
            score=result.score,
            settings=settings.routing,
            explicit=explicit,
        )
        return meta, (decision.model if decision is not None else None)

    def _maybe_decompose(
        self,
        *,
        store: Store,
        node: dict[str, Any],
        ctx: dict[str, Any],
        complexity: dict[str, Any] | None,
        prompt_sent: str,
        resolved_model: str,
    ) -> Any:
        """Attempt runtime decomposition, or ``None`` when it does not apply.

        The size trigger runs first, measuring *prompt_sent* against a token
        budget — it needs no score at all. The (pre-existing) score trigger
        reads the score already recorded in the pre-dispatch meta block — no
        score (scoring off, or scoring failed) means nothing to compare
        against the threshold, so that trigger never fires. The heavy
        lifting, and every failure-semantics decision for both triggers,
        lives in :mod:`circuitry.core.decompose`; the returned result only
        tells this runtime which of its three postures to take (write the
        merged value, raise, or dispatch — possibly on a fallback model).
        """
        score = complexity.get("score") if complexity is not None else None

        from .decompose import maybe_decompose

        return maybe_decompose(
            self.defn,
            store=store,
            node=node,
            ctx=ctx,
            score=float(score) if isinstance(score, (int, float)) else None,
            # The raw template keeps its {{...}} references so the emitted
            # document can re-declare them as inputs; a messages-based prompt
            # has no single template, so the materialized (already-rendered,
            # self-contained) prompt stands in.
            source_template=self.defn.template or prompt_sent,
            prompt_sent=prompt_sent,
            adapter=self.adapter,
            # The run's default model, unchanged from before #672 — what an
            # isolated child/planner dispatches on when a chunk/part names no
            # model of its own. `resolved_model` (this effect's own already-
            # resolved model, router included) is passed separately, read
            # only by the size trigger's budget lookup
            # (`runtime.complexity.splitting.models.<resolved model>`).
            model=self.model,
            resolved_model=resolved_model,
            runtime_config=self.runtime_config,
            timeout_seconds=self.timeout_seconds,
            verbose=self.verbose,
            display_depth=self.depth,
        )

    def _build_attempts(self, *, default_model: str) -> list[tuple[str, str]]:
        attempts: list[tuple[str, str]] = []

        primary_adapter = getattr(self.adapter, "name", "unknown")
        attempts.append((primary_adapter, default_model))

        if self.defn.provider:
            attempts.insert(
                0, self._parse_provider_token(self.defn.provider, default_model)
            )

        attempts.extend(
            self._parse_provider_token(provider_token, default_model)
            for provider_token in self.defn.provider_fallbacks or ()
        )

        deduped: list[tuple[str, str]] = []
        seen: set[tuple[str, str]] = set()
        for attempt in attempts:
            if attempt not in seen:
                seen.add(attempt)
                deduped.append(attempt)
        return deduped

    def _parse_provider_token(self, token: str, default_model: str) -> tuple[str, str]:
        parsed = (token or "").strip()
        if not parsed:
            return (getattr(self.adapter, "name", "unknown"), default_model)
        if ":" not in parsed:
            return (parsed, default_model)
        adapter_name, model_name = parsed.split(":", 1)
        adapter_name = adapter_name.strip()
        model_name = model_name.strip() or default_model
        return (adapter_name, model_name)

    def _adapter_configured_timeout_seconds(self, adapter_name: str) -> int | None:
        """``runtime.adapters.<adapter_name>.timeout_seconds``, or ``None`` if
        unset/invalid. Delegates to :func:`adapters.factory.configured_timeout_seconds`
        so the name normalisation, the litellm ``timeout`` alias and the
        "0/negative means unset" rule all come from the one place
        :func:`~circuitry.adapters.build_adapter` and ``cli.runtime_shim``'s
        run-default lookup also use."""
        return configured_timeout_seconds(adapter_name, self.runtime_config)

    def _attempt_timeout_seconds(self, adapter_name: str, timeout_ms: int | None) -> int:
        """One attempt's budget: the effect's ``timeout_ms`` (*timeout_ms*, already
        resolved for this attempt), capped by the timeout of the adapter this
        attempt actually dispatches to.

        Precedence: this effect's ``timeout_ms`` (rounded up to whole
        seconds) first; then ``runtime.adapters.<adapter_name>.timeout_seconds``
        for *this* attempt's adapter — not the run default's, which can be a
        different adapter entirely once a ``provider:``/fallback sends this
        attempt elsewhere; then the run's own default timeout. Adapters take
        whole seconds, so a sub-second ``timeout_ms`` rounds up to one. ``0``
        or absent ``timeout_ms`` leaves the adapter's timeout alone.
        """
        adapter_budget = self._adapter_configured_timeout_seconds(adapter_name)
        if adapter_budget is None:
            adapter_budget = self.timeout_seconds
        if not timeout_ms:
            return adapter_budget
        effect_seconds = max(1, math.ceil(timeout_ms / 1000))
        return min(effect_seconds, adapter_budget)

    def _generation_options(
        self,
        *,
        ctx: dict[str, Any],
        messages: tuple[ChatMessage, ...],
        meta: dict[str, Any],
    ) -> GenerateOptions:
        """Build the adapter's options from ``params``, ``deterministic``,
        ``messages`` and ``assets``, recording each image in ``meta.assets``.

        ``temperature``, ``max_tokens`` and ``stop`` are lifted out of
        ``params`` so every adapter can map them to its provider's names; the
        rest of ``params`` passes through untouched. ``deterministic`` means
        temperature 0 unless ``params`` sets one.
        """
        params = dict(self.defn.params or {})
        temperature = params.pop("temperature", None)
        max_tokens = params.pop("max_tokens", None)
        stop = params.pop("stop", None)
        if self.defn.deterministic and temperature is None:
            temperature = 0
        if temperature is not None and (
            isinstance(temperature, bool) or not isinstance(temperature, (int, float))
        ):
            raise ValueError(
                f"Prompt '{self.defn.name}': params.temperature must be a number, "
                f"got {temperature!r}"
            )
        if max_tokens is not None and (
            isinstance(max_tokens, bool) or not isinstance(max_tokens, int) or max_tokens < 1
        ):
            raise ValueError(
                f"Prompt '{self.defn.name}': params.max_tokens must be a positive "
                f"integer, got {max_tokens!r}"
            )
        if isinstance(stop, str):
            stop = [stop]
        if stop is not None and (
            not isinstance(stop, list) or not all(isinstance(s, str) for s in stop)
        ):
            raise ValueError(
                f"Prompt '{self.defn.name}': params.stop must be a string or a list "
                f"of strings, got {stop!r}"
            )

        images: list[ImageInput] = []
        asset_meta: list[dict[str, Any]] = []
        for index, asset in enumerate(self.defn.assets or ()):
            ref = render_template(asset.ref, ctx, label=f"assets[{index}].ref").strip()
            if asset.kind != "image":
                self._add_warnings(
                    meta,
                    [f"asset kind {asset.kind!r} is not sent to any adapter; only images are ({ref})"],
                )
                continue
            image, record = _load_image(ref)
            images.append(image)
            asset_meta.append(record)
        if asset_meta:
            meta["assets"] = asset_meta

        json_schema = (
            self.defn.schema
            if self.defn.schema and self.defn.prompt_type in ("json", "object", "array")
            else None
        )
        return GenerateOptions(
            temperature=float(temperature) if temperature is not None else None,
            max_tokens=max_tokens,
            stop=tuple(stop or ()),
            params=params,
            deterministic=self.defn.deterministic,
            messages=messages,
            images=tuple(images),
            json_schema=json_schema,
        )

    def _record_reply(
        self, meta: dict[str, Any], res: GenerateResult, option_warnings: list[str]
    ) -> None:
        """``finish_reason`` and the reply's warnings, including truncation.

        Runs once per retry attempt, so it replaces the previous attempt's
        reply warnings and finish_reason rather than adding to them.
        """
        finish_reason = getattr(res, "finish_reason", None)
        warnings = list(getattr(res, "warnings", ()) or ())
        meta.pop("finish_reason", None)
        if finish_reason is not None:
            meta["finish_reason"] = finish_reason
            if finish_reason in TRUNCATED_FINISH_REASONS:
                warnings.append(
                    f"the reply was cut off by the length limit "
                    f"(finish_reason: {finish_reason}); raise params.max_tokens"
                )
        if option_warnings:
            meta["warnings"] = list(option_warnings)
        else:
            meta.pop("warnings", None)
        self._add_warnings(meta, warnings)

    def _add_warnings(self, meta: dict[str, Any], warnings: list[str]) -> None:
        for warning in warnings:
            logger.warning("prompt %r: %s", self.defn.name, warning)
            meta.setdefault("warnings", []).append(warning)

    @contextmanager
    def _live_attempt(
        self,
        *,
        store: Store,
        attempt_index: int,
        adapter: Any,
        adapter_name: str,
        model_name: str,
        prompt: str,
        options: GenerateOptions,
    ) -> Iterator[GenerateOptions]:
        """Publish ``meta.live`` for one attempt (#674): written when this
        attempt starts, updated from the adapter's own ``on_progress``
        calls (through :class:`~circuitry.agent_live.LivePublisher`'s usual
        once-a-second coalescing), and removed on every exit — success,
        failure or cancellation alike.

        Yields the ``GenerateOptions`` the caller should actually dispatch
        with: unchanged when *adapter* does not stream, or carrying an
        ``on_progress`` wired to this attempt's own live record when it
        does. ``--events``' own ``progress`` line (``Store.fire_progress``)
        is fired from here too, synchronously, on whichever thread calls
        ``on_progress`` -- never through ``LivePublisher``'s own background
        thread, whose ``update`` only ever overwrites a pending snapshot
        rather than queuing one: routing the line through it as well would
        make which delta's figures actually get published (and whether a
        fast burst of deltas produces one line or several) depend on when
        that thread happens to wake up, never deterministic. It is never
        fired for a zero-``delta_chars`` progress call (Anthropic's own
        ``message_start``, which only ever reports ``tokens_sent``) or on
        this method's own start/clear writes.
        """
        from .complexity import _estimate_tokens

        name = self.defn.name
        # `is True`, not `bool(...)`: a test double built from a bare
        # `MagicMock()` auto-creates a truthy `.streams` attribute on any
        # access, which `bool()` would read as streaming -- the same
        # reason `adapter_accepts_images` compares this way (#674).
        streaming = getattr(adapter, "streams", False) is True
        tokens_sent_estimate = _estimate_tokens(prompt)
        live_state = _LiveAttemptState()
        # Captured once, right now, on this method's own calling thread --
        # the effect's dispatch thread, which already called
        # `fire_effect_start` before this attempt began -- so every
        # `progress` line this attempt fires later (from the adapter's own
        # reader thread) carries the instance that was actually open when
        # this attempt started, never one a same-path concurrent `start`
        # (an unnamed tree-flow loop sharing one compiled path) has since
        # moved a path-keyed lookup on to.
        captured_instance = store.resolve_open_instance(name)

        def _snapshot_locked() -> dict[str, Any]:
            """Caller already holds ``live_state.lock``."""
            return live_state.snapshot(
                attempt=attempt_index,
                adapter=adapter_name,
                model=model_name,
                streaming=streaming,
                tokens_sent_estimate=tokens_sent_estimate,
            )

        def _snapshot() -> dict[str, Any]:
            with live_state.lock:
                return _snapshot_locked()

        # Only a streaming adapter ever calls `on_progress`, so a
        # non-streaming one has nothing for `LivePublisher`'s own writer
        # thread to do beyond the one write below and the clearing write
        # in `finally` -- skip spawning it at all.
        publisher = LivePublisher(lambda snapshot: store.set(f"{name}.meta.live", snapshot)) if streaming else None

        progress_lock = threading.Lock()
        last_progress_fired_at: list[float | None] = [None]

        def _on_progress(progress: GenerateProgress) -> None:
            with live_state.lock:
                live_state.apply(progress)
                snapshot = _snapshot_locked()
            assert publisher is not None  # only wired when `streaming`
            publisher.update(snapshot)
            if progress.delta_chars <= 0:
                return
            now = time.monotonic()
            with progress_lock:
                last_fired = last_progress_fired_at[0]
                if last_fired is not None and now - last_fired < 1.0:
                    return
                last_progress_fired_at[0] = now
            store.fire_progress(
                name,
                {
                    "tokens_received_estimate": snapshot["tokens_received_estimate"],
                    "chars_received": snapshot["chars_received"],
                    "tokens_sent": snapshot["tokens_sent"],
                    "tokens_sent_estimate": snapshot["tokens_sent_estimate"],
                    # Neither of these is part of the `progress` line
                    # itself -- `cli.events.EventLog.on_progress` reads
                    # `_instance` in place of its own fresh lookup (the
                    # capture above), and `attempt` only to reset
                    # `first_token_ms` on a new attempt, since one
                    # `start`/`end` pair covers every retry of a prompt.
                    "_instance": captured_instance,
                    "attempt": attempt_index,
                },
            )

        store.set(f"{name}.meta.live", _snapshot())
        live_options = replace(options, on_progress=_on_progress) if streaming else options
        try:
            yield live_options
        finally:
            if publisher is not None:
                publisher.close()
            store.unset(f"{name}.meta.live")

    def _generate_with_fallbacks(
        self,
        *,
        prompt: str,
        attempts: list[tuple[str, str]],
        meta: dict[str, Any],
        option_warnings: list[str],
        timeout_ms: int | None = None,
        options: GenerateOptions | None = None,
        store: Store,
        attempt_index: int = 0,
    ) -> tuple[GenerateResult | None, Any, list[dict[str, Any]], Exception | None, int]:
        """Try each ``(adapter, model)`` in order; the first reply that both
        the adapter call and decoding/schema validation accept wins:
        ``(result, decoded value, attempt log, the last error if none did,
        the next attempt index)``.

        A reply a provider actually returned but this effect can't use — an
        unreadable boolean/number, or JSON that fails its schema — is treated
        exactly like a failed adapter call: logged here and the next provider
        in the chain is tried, rather than bubbling out to ``execute``'s
        retry loop, which would restart the whole chain from the primary
        instead of continuing down the fallback list (#281). ``meta`` and
        ``option_warnings`` are threaded through so the winning attempt's
        ``finish_reason``/warnings/``answer`` land on the node exactly as a
        single-attempt dispatch always has; a losing attempt never touches
        them beyond its own ``attempts_meta`` entry.

        Resolving the adapter (``_resolve_adapter``) happens inside this same
        per-attempt ``try`` so a bad fallback name is logged like any other
        failed attempt instead of escaping raw and dropping whatever the
        chain had already recorded — except an :class:`AllowlistError`,
        which always propagates immediately: a ``provider:``/fallback outside
        the installed allowlist is a denial, not a transient failure, and
        must not silently fall through to the next adapter in the chain
        (often the run default).

        ``attempt_index`` numbers ``meta.live.attempt`` across every
        ``(adapter, model)`` this call tries, continuing from wherever the
        last pass (an earlier retry) left off — the returned index is what
        the next retry's own call starts from, so the count runs across the
        whole execution rather than resetting per pass (#674).
        """
        attempts_meta: list[dict[str, Any]] = []
        last_error: Exception | None = None
        options = options or GenerateOptions()

        for adapter_name, model_name in attempts:
            try:
                adapter = self._resolve_adapter(adapter_name)
                with self._live_attempt(
                    store=store,
                    attempt_index=attempt_index,
                    adapter=adapter,
                    adapter_name=adapter_name,
                    model_name=model_name,
                    prompt=prompt,
                    options=options,
                ) as live_options:
                    res = call_generate(
                        adapter,
                        model=model_name,
                        prompt=prompt,
                        timeout_seconds=self._attempt_timeout_seconds(adapter_name, timeout_ms),
                        options=live_options,
                    )
            except AllowlistError:
                # A security boundary, not a dispatch failure: an adapter
                # outside the installed allowlist must fail this effect
                # outright, never logged as a merely-failed attempt and
                # fallen through past — that would let a denied `provider:`
                # silently resolve to whatever adapter happens to be next in
                # the chain (often the run default) instead of refusing.
                raise
            except Exception as e:
                attempt_index += 1
                last_error = e
                attempts_meta.append(
                    {
                        "adapter": adapter_name,
                        "model": model_name,
                        "status": "failed",
                        "error": redact(str(e)),
                        "tokens_sent": None,
                        "tokens_received": None,
                        **{
                            key: redact(value)
                            for key, value in failure_fields(e).items()
                        },
                    }
                )
                continue
            attempt_index += 1

            # Before decoding: a reply cut off mid-JSON fails to parse, and
            # the truncation warning is what explains it.
            self._record_reply(meta, res, option_warnings)
            try:
                decoded_value = self._decode_and_validate(res, meta=meta)
            except Exception as e:
                last_error = e
                attempts_meta.append(
                    _fallback_attempt_entry(
                        adapter=adapter_name,
                        model=model_name,
                        status=_decode_failure_status(e),
                        error=redact(str(e)),
                        res=res,
                        extra={"raw_reply": _cap_reply_text(res.text)},
                    )
                )
                continue

            attempts_meta.append(
                _fallback_attempt_entry(
                    adapter=adapter_name,
                    model=model_name,
                    status="succeeded",
                    error=None,
                    res=res,
                )
            )
            return (res, decoded_value, attempts_meta, None, attempt_index)

        return (None, None, attempts_meta, last_error, attempt_index)

    def _decode_and_validate(self, res: GenerateResult, *, meta: dict[str, Any]) -> Any:
        """Decode ``res.text`` per ``prompt_type`` and validate it against
        ``schema`` when one is set, recording ``meta.answer`` along the way.

        Raises :class:`~circuitry.core.answers.AnswerParseError` on an
        unreadable boolean/number and :class:`SchemaValidationError` on a
        schema failure — the two decode-stage failures a fallback chain
        treats like a failed adapter call (#281).
        """
        if self.defn.prompt_type in ("boolean", "number"):
            meta["answer"] = res.text
        decoded_value = self._decode_output(res.text)
        if self.defn.schema and self.defn.prompt_type in ("json", "object", "array"):
            self._validate_schema(decoded_value, raw_response_text=res.text)
        return decoded_value

    def _resolve_adapter(self, adapter_name: str) -> Adapter:
        default_name = getattr(self.adapter, "name", "")
        if adapter_name == default_name:
            return self.adapter
        require_adapter(adapter_name, allowed_adapters(self.runtime_config))
        return build_adapter(adapter_name=adapter_name, runtime=self.runtime_config)

    def _pre_dispatch_target(self, model: str) -> str:
        """Verbose label for the adapter this effect will try first.

        Reads the same chain ``_build_attempts`` hands to dispatch — a
        per-effect ``provider:`` (including one overlaid by a profile) wins
        over the run-default adapter, exactly as ``_generate_with_fallbacks``
        tries it first. ``self.adapter`` alone would only be correct for a
        prompt with no override.

        Called ahead of the dispatch ``try`` block (there is no attempt or
        error to report yet), so a bad ``provider:`` must not raise here —
        that would skip the outer error handling (``on_error``, ``meta.error``,
        the balanced start/complete pair) entirely. Fall back to a plain
        label; the real build, and its real error, happen at dispatch time.
        """
        adapter_name, model_name = self._build_attempts(default_model=model)[0]
        try:
            adapter = self._resolve_adapter(adapter_name)
        except Exception:
            return f"{adapter_name} · {model_name}"
        return _adapter_target(adapter, model_name)

    def _attempts_target(self, attempts_meta: list[dict[str, Any]]) -> str:
        """Verbose label built from the attempts dispatch actually made.

        Any attempt before the last shows as ``adapter ✗`` — it was tried and
        failed, which is why a later one ran. The last entry is the one that
        answered (or, if every attempt failed, the one that failed last) and
        renders with its full ``adapter · model @ host`` target so this label
        always matches ``meta["adapter"]``/``meta["model"]`` rather than the
        pre-dispatch guess.
        """
        if not attempts_meta:
            return ""
        parts = [f"{attempt['adapter']} ✗" for attempt in attempts_meta[:-1]]
        last = attempts_meta[-1]
        try:
            adapter = self._resolve_adapter(last["adapter"])
            label = _adapter_target(adapter, last["model"])
        except Exception:
            label = f"{last['adapter']} · {last['model']}"
        if last.get("status") != "succeeded":
            label += " ✗"
        parts.append(label)
        return " → ".join(parts)

    def _read_dispatch_files(
        self, ctx: dict[str, Any]
    ) -> tuple[dict[str, str], list[dict[str, Any]]]:
        """Read each ``file: {from: ...}`` source this prompt will use (#578).

        Returns ``({field: text}, [meta.template_files record, ...])``, in the
        order the fields are used: the template, else each message's content.
        A source the prompt will not use (``messages`` next to a ``template``)
        is not read.
        """
        sources: list[tuple[str, DispatchFileSource]] = []
        if self.defn.template:
            pass
        elif self.defn.template_file is not None:
            sources.append(("template", self.defn.template_file))
        else:
            for index, msg in enumerate(self.defn.messages or ()):
                if msg.file is not None:
                    sources.append((f"messages[{index}].content", msg.file))
        texts: dict[str, str] = {}
        records: list[dict[str, Any]] = []
        for field, source in sources:
            read = read_dispatch_file(
                source, ctx, field=field, effect_label=f"Prompt effect '{self.defn.name}'"
            )
            texts[field] = read.text
            records.append(read.record)
        return texts, records

    def _render_messages(
        self, ctx: dict[str, Any], file_texts: dict[str, str] | None = None
    ) -> tuple[ChatMessage, ...]:
        """The effect's ``messages`` with each ``content`` rendered; ``()`` for a template.

        A message whose content is a ``file: {from: ...}`` takes its text from
        *file_texts* verbatim, unrendered.
        """
        if self.defn.template or self.defn.template_file is not None or not self.defn.messages:
            return ()
        declared = declared_prompts(self.runtime_config)
        known_names = known_effect_names(self.runtime_config)
        file_texts = file_texts or {}
        return tuple(
            ChatMessage(
                role=msg.role,
                content=(
                    file_texts[f"messages[{index}].content"]
                    if msg.file is not None
                    else render_with_composition(
                        msg.content,
                        ctx,
                        declared=declared,
                        known_effect_names=known_names,
                        label=f"messages[{index}].content",
                        escape=False,
                    )
                ),
            )
            for index, msg in enumerate(self.defn.messages)
        )

    def _materialize_input(
        self,
        ctx: dict[str, Any],
        messages: tuple[ChatMessage, ...] = (),
        file_texts: dict[str, str] | None = None,
    ) -> str:
        """The prompt as one string: the rendered template, or the turns
        flattened as ``role: content``.

        The flattened form is ``meta.prompt_sent`` and what an adapter that
        cannot take role-tagged turns receives; chat-capable adapters get
        ``messages`` themselves through :class:`GenerateOptions`.
        """
        if self.defn.template:
            return render_with_composition(
                self.defn.template,
                ctx,
                declared=declared_prompts(self.runtime_config),
                known_effect_names=known_effect_names(self.runtime_config),
                label="template",
                escape=False,
            )

        if self.defn.template_file is not None:
            return (file_texts or {})["template"]

        if messages:
            return "\n\n".join(f"{msg.role}: {msg.content}" for msg in messages)

        return ""

    def _decode_output(self, text: str) -> Any:
        """Decode the model output based on prompt_type.

        ``boolean``/``number`` go through the shared lenient parser (see
        ``core.answers``), which raises on an answer it cannot read rather
        than returning ``None`` — an unparseable reply is a dispatch failure,
        so ``on_error`` and retries apply to it exactly like a failed adapter
        call.
        """
        if self.defn.prompt_type not in ("boolean", "number") and not text:
            return None

        text = text.strip()

        if self.defn.prompt_type == "text":
            return text

        if self.defn.prompt_type == "boolean":
            return parse_boolean_answer(text)

        if self.defn.prompt_type == "number":
            return parse_number_answer(text)

        if self.defn.prompt_type in ("json", "object", "array"):
            # Try to extract JSON from the response
            return self._parse_json(text)

        # tool type - return as-is for now
        return text

    def _parse_json(self, text: str) -> Any:
        """Parse JSON from text, handling common model output patterns."""
        # Try direct parse first
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            pass

        # Try to extract JSON from markdown code blocks
        import re

        # Match ```json ... ``` or ``` ... ```
        patterns = [
            r"```json\s*([\s\S]*?)\s*```",
            r"```\s*([\s\S]*?)\s*```",
        ]

        for pattern in patterns:
            match = re.search(pattern, text)
            if match:
                try:
                    return json.loads(match.group(1))
                except json.JSONDecodeError:
                    continue

        # Try to find JSON object or array in text
        for start, end in [("{", "}"), ("[", "]")]:
            idx_start = text.find(start)
            idx_end = text.rfind(end)
            if idx_start != -1 and idx_end > idx_start:
                try:
                    return json.loads(text[idx_start : idx_end + 1])
                except json.JSONDecodeError:
                    continue

        return None

    def _validate_schema(self, value: Any, *, raw_response_text: str | None = None) -> None:
        """Validate value against JSON schema if provided.

        ``raw_response_text`` — the model's unparsed response — rides along on
        any raised :class:`SchemaValidationError` so a caller several layers up
        (decomposition's planner-failure handling, in particular) can recover
        what was actually returned without re-deriving it from the message.
        """
        if not self.defn.schema:
            return
        if value is None:
            raise SchemaValidationError(
                "JSON schema validation failed: model returned None (JSON could not be parsed). "
                "Check the model's response format.",
                raw_response_text=raw_response_text,
            )

        try:
            import jsonschema  # type: ignore[import-untyped]

            jsonschema.validate(value, self.defn.schema)
        except ImportError:
            # jsonschema not installed, skip validation
            pass
        except jsonschema.ValidationError as e:
            raise SchemaValidationError(
                f"Schema validation failed: {e.message}", raw_response_text=raw_response_text
            ) from e

