from __future__ import annotations

import json
import logging
import time
from collections.abc import Callable, Sequence
from contextlib import nullcontext
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Literal

from ..adapters import Adapter, build_adapter
from ..adapters.base import GenerateResult
from ..output import console as _console
from .store import Store

logger = logging.getLogger(__name__)


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


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


def _render(template: str, ctx: dict[str, Any]) -> str:
    try:
        import chevron  # type: ignore

        return chevron.render(template, ctx)
    except Exception:
        logger.warning("Chevron template rendering failed; returning raw template", exc_info=True)
        return template


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
    timeout_ms: int | None = None
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


class PromptRuntime:
    """
    Executes a PromptDefinition against adapter + store.
    Writes:
      <name>.value
      <name>.meta{created_at, completed_at, adapter, model, model_reason,
                  prompt_type, prompt_sent, tokens_sent, tokens_received,
                  error, dry_run, fallback_attempts, fallback_recovered,
                  retries_used?, complexity?}

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
    appears only when ``runtime.complexity.decomposition`` is enabled and this
    effect's score strictly exceeded the threshold. It carries the attempt's
    whole story — ``{decomposed, outcome, reason, score, threshold, depth,
    max_depth, plan?, chunk_count?, yaml?, result_path?, fallback_model?,
    error?}`` — whether the effect was actually replaced by the planned
    fan-out (``value`` then holds the child's merged result), routed up to a
    more capable model, run as-is, or failed under ``on_failure: fail``. See
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

        # Materialize prompt input
        prompt_sent = self._materialize_input(effective_ctx)

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
        meta["prompt_sent"] = prompt_sent
        meta["tokens_sent"] = None
        meta["tokens_received"] = None
        meta["error"] = None
        meta["dry_run"] = self.dry_run
        meta["fallback_attempts"] = []
        meta["fallback_recovered"] = False

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
        target = _adapter_target(self.adapter, resolved_model) if self.verbose else ""

        if self.verbose and self.cb_start is not None:
            self.cb_start()
        if self.verbose and self.cb_running is not None:
            self.cb_running(target, estimated_out)

        if self.dry_run:
            node["value"] = None
            meta["completed_at"] = _now_iso()
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
            )
            if decomposition is not None:
                meta["decomposition"] = decomposition.meta
                if decomposition.succeeded:
                    node["value"] = decomposition.value
                    meta["completed_at"] = _now_iso()
                    if self.verbose:
                        elapsed = time.monotonic() - t0
                        chunk_count = decomposition.meta.get("chunk_count")
                        line = (
                            f"{indent}[ok]✓[/ok] [cyan]◆[/cyan] {self.display_name}"
                            f" [dim]decomposed · {chunk_count} chunks"
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

            attempts = self._build_attempts(default_model=dispatch_model)

            for _attempt in range(max_attempts):
                if _attempt > 0:
                    time.sleep(backoff_ms / 1000)
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

                try:
                    if self.verbose and self.cb_start is None:
                        from rich.live import Live

                        live_cm = Live(
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
                    else:
                        live_cm = nullcontext()
                    with live_cm:
                        res, attempts_meta, generation_error = self._generate_with_fallbacks(
                            prompt=prompt_sent, attempts=attempts
                        )
                    if generation_error is not None or res is None:
                        raise RuntimeError(
                            f"All adapter attempts failed: {attempts_meta}"
                        ) from generation_error

                    # Decode and validate output based on prompt_type
                    decoded_value = self._decode_output(res.text)

                    # Validate against schema if provided
                    if self.defn.schema and self.defn.prompt_type in (
                        "json",
                        "object",
                        "array",
                    ):
                        self._validate_schema(decoded_value)

                    # Success
                    node["value"] = decoded_value
                    meta["tokens_sent"] = res.tokens_sent
                    meta["tokens_received"] = res.tokens_received
                    meta["fallback_attempts"] = attempts_meta
                    meta["fallback_recovered"] = len(attempts_meta) > 1
                    meta["completed_at"] = _now_iso()
                    if attempts_meta:
                        last = attempts_meta[-1]
                        meta["adapter"] = last["adapter"]
                        meta["model"] = last["model"]
                    if _attempt > 0:
                        meta["retries_used"] = _attempt

                    if self.verbose:
                        elapsed = time.monotonic() - t0
                        suffix = _elapsed_str(elapsed)
                        sent = res.tokens_sent
                        recv = res.tokens_received
                        if sent is not None or recv is not None:
                            suffix += f" | ↑{sent or 0} ↓{recv or 0} tok"
                        line = (
                            f"{indent}[ok]✓[/ok] [cyan]◆[/cyan] {self.display_name}"
                            f" [dim]{target} | {suffix}[/dim]"
                        )
                        if self.cb_done is not None:
                            self.cb_done(line)
                        else:
                            _console.print(line)

                    store.fire_effect_complete(self.defn.name, node)
                    return

                except Exception:
                    if _attempt < max_attempts - 1:
                        # Show failure for this attempt, then retry
                        if self.verbose:
                            elapsed = time.monotonic() - t0
                            line = (
                                f"{indent}[err]✗[/err] [cyan]◆[/cyan] {self.display_name}"
                                f" [dim]{target} | {_elapsed_str(elapsed)}[/dim]"
                            )
                            if self.cb_error is not None:
                                self.cb_error(line)
                            else:
                                _console.print(line)
                        continue
                    raise  # Last attempt — propagate to outer handler

        except Exception as e:
            if self.verbose:
                elapsed = time.monotonic() - t0
                line = (
                    f"{indent}[err]✗[/err] [cyan]◆[/cyan] {self.display_name}"
                    f" [dim]{target} | {_elapsed_str(elapsed)}[/dim]"
                )
                if self.cb_error is not None:
                    self.cb_error(line)
                else:
                    _console.print(line)
            meta["fallback_attempts"] = attempts_meta
            meta["fallback_recovered"] = False
            meta["error"] = str(e)
            meta["completed_at"] = _now_iso()
            if self.defn.on_error == "skip":
                node["value"] = None
            # continue: keep going with None value
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
    ) -> Any:
        """Attempt runtime decomposition, or ``None`` when it does not apply.

        Decomposition reads the score already recorded in the pre-dispatch
        meta block — no score (scoring off, or scoring failed) means nothing
        to compare against the threshold, so the effect runs untouched. The
        heavy lifting, and every failure-semantics decision, lives in
        :mod:`circuitry.core.decompose`; the returned result only tells this
        runtime which of its three postures to take (write the merged value,
        raise, or dispatch — possibly on a fallback model).
        """
        if complexity is None:
            return None
        score = complexity.get("score")
        if not isinstance(score, (int, float)):
            return None

        from .decompose import maybe_decompose

        return maybe_decompose(
            self.defn,
            store=store,
            node=node,
            ctx=ctx,
            score=float(score),
            # The raw template keeps its {{...}} references so the emitted
            # document can re-declare them as inputs; a messages-based prompt
            # has no single template, so the materialized (already-rendered,
            # self-contained) prompt stands in.
            source_template=self.defn.template or prompt_sent,
            adapter=self.adapter,
            model=self.model,
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

    def _generate_with_fallbacks(
        self, *, prompt: str, attempts: list[tuple[str, str]]
    ) -> tuple[GenerateResult | None, list[dict[str, Any]], Exception | None]:
        attempts_meta: list[dict[str, Any]] = []
        last_error: Exception | None = None

        for adapter_name, model_name in attempts:
            adapter = self._resolve_adapter(adapter_name)
            try:
                res = adapter.generate(
                    model=model_name,
                    prompt=prompt,
                    timeout_seconds=self.timeout_seconds,
                )
                attempts_meta.append(
                    {
                        "adapter": adapter_name,
                        "model": model_name,
                        "status": "succeeded",
                        "error": None,
                    }
                )
                return (res, attempts_meta, None)
            except Exception as e:
                last_error = e
                attempts_meta.append(
                    {
                        "adapter": adapter_name,
                        "model": model_name,
                        "status": "failed",
                        "error": str(e),
                    }
                )

        return (None, attempts_meta, last_error)

    def _resolve_adapter(self, adapter_name: str) -> Adapter:
        default_name = getattr(self.adapter, "name", "")
        if adapter_name == default_name:
            return self.adapter
        return build_adapter(adapter_name=adapter_name, runtime=self.runtime_config)

    def _materialize_input(self, ctx: dict[str, Any]) -> str:
        """Materialize the prompt input from template or messages."""
        if self.defn.template:
            return _render(self.defn.template, ctx)

        if self.defn.messages:
            # Format messages into a prompt string
            # For more sophisticated handling, this would be adapter-specific
            lines = []
            for msg in self.defn.messages:
                content = _render(msg.content, ctx)
                lines.append(f"{msg.role}: {content}")
            return "\n\n".join(lines)

        return ""

    def _decode_output(self, text: str) -> Any:
        """Decode the model output based on prompt_type."""
        if not text:
            return None

        text = text.strip()

        if self.defn.prompt_type == "text":
            return text

        if self.defn.prompt_type == "boolean":
            lower = text.lower()
            if lower in ("true", "yes", "1", "y"):
                return True
            if lower in ("false", "no", "0", "n"):
                return False
            return None

        if self.defn.prompt_type == "number":
            try:
                if "." in text:
                    return float(text)
                return int(text)
            except ValueError:
                return None

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

    def _validate_schema(self, value: Any) -> None:
        """Validate value against JSON schema if provided."""
        if not self.defn.schema:
            return
        if value is None:
            raise ValueError(
                "JSON schema validation failed: model returned None (JSON could not be parsed). "
                "Check the model's response format."
            )

        try:
            import jsonschema  # type: ignore[import-untyped]

            jsonschema.validate(value, self.defn.schema)
        except ImportError:
            # jsonschema not installed, skip validation
            pass
        except jsonschema.ValidationError as e:
            raise ValueError(f"Schema validation failed: {e.message}") from e

