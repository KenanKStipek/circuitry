from __future__ import annotations

import copy
import json
import logging
import math
import time
from collections.abc import Callable
from contextlib import AbstractContextManager, nullcontext
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Literal

from ..adapters import Adapter
from ..adapters._retry import (
    RetryInfo,
    classify_exception,
    next_backoff_delay_ms,
    status_is_retryable,
)
from ..cli.redaction import redact
from ..output import console as _console
from ..output import live_region as _live_region
from ..plugins.base import ToolResult
from .cancellation import get_token
from .concurrency import RUNTIME_CONFIG_KEY as _CONCURRENCY_LIMITER_KEY
from .expect import ExpectDef, evaluate_expect, expect_failure_summary
from .prompt import RetryPolicyDef
from .store import Store
from .templates import render_template
from .use import _resolve_reference

logger = logging.getLogger(__name__)


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


#: Providers whose raw["status"] is an HTTP status code, not some other
#: plugin's unrelated int field that happens to be named "status".
_HTTP_FAMILY_PROVIDERS = frozenset({"http", "web_fetch", "webhook", "linear"})

#: Cap on meta.raw's serialized size, in bytes, after redaction. Protects
#: state (and anything that mirrors it: --out, --live-state, persisted
#: snapshots) from a provider response ToolResult.raw is large enough to
#: carry (e.g. a big HTTP body echoed verbatim) — see orchestration-reference.md.
_RAW_META_MAX_BYTES = 64 * 1024


def _capped_raw(raw: dict[str, Any]) -> dict[str, Any]:
    """Redact *raw*, then replace it with a truncation marker if it's still
    too big to store safely in state.

    Returns the JSON round-tripped copy, not *redacted* itself: callers
    that serialize state (``--out``, the SQL/Postgres stores) use plain
    ``json.dumps`` with no ``default=``, so a plugin ``raw`` containing
    e.g. ``bytes`` or a ``datetime`` would otherwise crash them later.
    """
    redacted = redact(raw)
    try:
        encoded = json.dumps(redacted, ensure_ascii=False, default=str).encode("utf-8")
    except (TypeError, ValueError):
        return redacted
    if len(encoded) <= _RAW_META_MAX_BYTES:
        result: dict[str, Any] = json.loads(encoded)
        return result
    return {
        "_truncated": True,
        "_original_bytes": len(encoded),
        "_preview": encoded[:_RAW_META_MAX_BYTES].decode("utf-8", errors="ignore"),
    }


def _elapsed_str(seconds: float) -> str:
    if seconds >= 1:
        return f"{seconds:.2f}s"
    return f"{seconds * 1000:.0f}ms"


def _plugin_target(plugin: Any, mtag: str = "") -> str:
    """Return 'name · model @ host' or 'name @ host' mirroring _adapter_target in prompt.py."""
    from urllib.parse import urlparse

    name = getattr(plugin, "name", "unknown")
    base_url = getattr(plugin, "base_url", None)
    label = f"{name} · {mtag}" if mtag else name
    if base_url:
        host = urlparse(str(base_url)).hostname or str(base_url)
        return f"{label} @ {host}"
    return label


def _model_tag(rendered: dict[str, Any]) -> str:
    """Return a short model name from rendered params, stripping the file extension."""
    import os
    model = rendered.get("model")
    if not model:
        return ""
    name = os.path.basename(str(model))
    for ext in (".safetensors", ".ckpt", ".pt", ".bin", ".gguf"):
        if name.endswith(ext):
            name = name[: -len(ext)]
            break
    return name


def _format_output(value: Any) -> str:
    """Format a result value for the done line (e.g. file path, with size if it's a file)."""
    if value is None:
        return ""
    s = str(value)
    try:
        import os
        size = os.path.getsize(s)
        if size >= 1_048_576:
            size_str = f"{size / 1_048_576:.1f} MB"
        elif size >= 1024:
            size_str = f"{size / 1024:.0f} KB"
        else:
            size_str = f"{size} B"
        return f"{s} ({size_str})"
    except OSError:
        return s


#: The one or two keys of a by-reference ``params`` leaf: ``{from: <path>}``,
#: optionally with a ``default:`` for when the path doesn't resolve (#234).
PARAM_REFERENCE_KEY = "from"
PARAM_DEFAULT_KEY = "default"


@dataclass(frozen=True)
class ParamReference:
    path: str
    has_default: bool = False
    default: Any = None


def param_reference(value: Any) -> ParamReference | None:
    """A ``params`` leaf's ``{from: <path>}`` (optionally with ``default:``), or None.

    Only a mapping with exactly the keys ``{"from"}`` or ``{"from", "default"}``,
    and a string ``from`` value, is a reference — any other mapping (including
    one with other keys mixed in) is passed through literally, same as before.
    ``params_json`` is the way to pass a literal one-key ``{from: ...}`` dict.
    """
    if not isinstance(value, dict):
        return None
    keys = set(value.keys())
    if keys == {PARAM_REFERENCE_KEY}:
        path = value[PARAM_REFERENCE_KEY]
        return ParamReference(path=path.strip()) if isinstance(path, str) else None
    if keys == {PARAM_REFERENCE_KEY, PARAM_DEFAULT_KEY}:
        path = value[PARAM_REFERENCE_KEY]
        if isinstance(path, str):
            return ParamReference(
                path=path.strip(), has_default=True, default=value[PARAM_DEFAULT_KEY]
            )
        return None
    return None


def _render_params(params: dict[str, Any], ctx: dict[str, Any], *, name: str) -> dict[str, Any]:
    """Resolve by-reference leaves, then Mustache-render every remaining string in params.

    A ``{from: <path>}`` leaf, at any depth in ``params`` (objects and lists),
    passes the value at *path* through untouched — typed, deep-copied, never
    rendered — the same contract as a ``use`` effect's own by-reference
    inputs (``core.use._render_inputs``). A path that doesn't resolve raises,
    naming the param's own path, unless the leaf also carries ``default:``,
    which is used as-is (mirroring how ``interface.inputs`` defaults fill an
    unresolved/absent value). Any other value that fails to render also
    raises, naming its path, rather than handing the tool unrendered text.
    """

    def _render_value(v: Any, path: str) -> Any:
        ref = param_reference(v)
        if ref is not None:
            resolved = _resolve_reference(ctx, ref.path)
            if resolved is None:
                if ref.has_default:
                    return copy.deepcopy(ref.default)
                raise ValueError(
                    f"Tool effect '{name}' param '{path}': "
                    f"'{{from: {ref.path}}}' did not resolve to a value."
                )
            return copy.deepcopy(resolved)
        if isinstance(v, str):
            return render_template(v, ctx, label=path)
        if isinstance(v, dict):
            return {k: _render_value(vv, f"{path}.{k}") for k, vv in v.items()}
        if isinstance(v, list):
            return [_render_value(item, f"{path}[{i}]") for i, item in enumerate(v)]
        return v

    return {k: _render_value(v, f"params.{k}") for k, v in params.items()}


class _JsonAwareDict(dict):
    """A dict that stringifies as JSON so chevron's ``{{{...}}}`` splices real JSON, not repr()."""

    _CHEVRON_return_scope_when_falsy = True

    def __str__(self) -> str:
        return json.dumps(self, ensure_ascii=False, default=str)


class _JsonAwareList(list):
    """A list that stringifies as JSON so chevron's ``{{{...}}}`` splices real JSON, not repr()."""

    _CHEVRON_return_scope_when_falsy = True

    def __str__(self) -> str:
        return json.dumps(self, ensure_ascii=False, default=str)


def _json_aware_ctx(value: Any) -> Any:
    """Recursively wrap dict/list nodes so a native state value (not just a
    pre-serialized JSON string) splices into a params_json template as JSON.

    Chevron's ``{{{...}}}`` calls plain str() on whatever it finds, which
    turns a real Python list/dict into its repr (e.g. ``['AAPL', 'MSFT']``),
    not JSON. State values produced by array/object prompts, the json
    plugin, MCP structuredContent, surrealdb results, and loop items are all
    native lists/dicts, so this wrapping is what makes those the common case.
    """
    if isinstance(value, dict):
        return _JsonAwareDict((k, _json_aware_ctx(v)) for k, v in value.items())
    if isinstance(value, list):
        return _JsonAwareList(_json_aware_ctx(v) for v in value)
    return value


def _render_params_json(template: str, ctx: dict[str, Any]) -> dict[str, Any]:
    """Mustache-render params_json, then parse the result as a JSON object.

    params_json exists so a runtime-built array/object (e.g. a list of symbols
    from a prior step) can reach a tool call. Silently ignoring a bad template
    or malformed JSON would run the tool with a different params object than
    the author wrote, which is worse than surfacing the error.
    """
    rendered_text = render_template(template, _json_aware_ctx(ctx), label="params_json")
    try:
        parsed = json.loads(rendered_text)
    except json.JSONDecodeError as e:
        raise ValueError(f"params_json did not render to valid JSON: {e}") from e
    if not isinstance(parsed, dict):
        raise ValueError(
            "params_json must render to a JSON object (dict), got "
            f"{type(parsed).__name__}"
        )
    return parsed


def _deep_merge_params(base: dict[str, Any], overlay: dict[str, Any]) -> dict[str, Any]:
    """Deep-merge *overlay* onto *base*; overlay keys win, nested dicts recurse."""
    merged = dict(base)
    for key, value in overlay.items():
        if key in merged and isinstance(merged[key], dict) and isinstance(value, dict):
            merged[key] = _deep_merge_params(merged[key], value)
        else:
            merged[key] = value
    return merged


#: Params a tool plugin treats as a security boundary (today: the ``shell``
#: plugin's allowlist). Honoured only from a document's literal, unrendered
#: ``params`` block — never ``params_json`` (runtime-built, can carry
#: model-generated content) and never a templated value within the literal
#: block, so neither can widen what a document is allowed to run.
_SECURITY_SENSITIVE_PARAM_KEYS: frozenset[str] = frozenset({"allowed_commands"})


def _reject_templated_security_params(raw_params: dict[str, Any]) -> None:
    """Raise if a security-sensitive param's literal value isn't a plain list of strings.

    Covers: a templated string element (``{{`` — its real content isn't known
    until render time, which defeats the point of a boundary the document
    author is meant to write down plainly); a ``{from: <path>}`` reference as
    the whole value or as a list item (#234 made this reachable — a
    by-reference value can carry runtime or model-generated content, e.g.
    REST/MCP caller input or an LLM step's output, same as a templated
    string); and any other non-string list item.
    """
    for key in _SECURITY_SENSITIVE_PARAM_KEYS:
        if key not in raw_params:
            continue
        value = raw_params[key]
        if param_reference(value) is not None:
            raise ValueError(
                f"params.{key} must be a literal list of strings; a by-reference "
                "'{from: ...}' value is not honoured for this security-sensitive setting."
            )
        if not isinstance(value, list):
            continue
        for item in value:
            if not isinstance(item, str):
                raise ValueError(
                    f"params.{key} must be a literal list of strings; a "
                    "by-reference '{from: ...}' value is not honoured for this "
                    "security-sensitive setting."
                )
            if "{{" in item:
                raise ValueError(
                    f"params.{key} must be a literal list of strings; a templated "
                    "value is not honoured for this security-sensitive setting."
                )


def _reject_params_json_security_overrides(overlay: dict[str, Any]) -> None:
    """Raise if a rendered ``params_json`` tries to set a security-sensitive key.

    ``params_json`` is a runtime-built JSON object — it can carry
    model-generated content (#203) — so it must never be the source of a
    plugin's own allowlist; only a document's literal ``params`` block is.
    """
    overridden = _SECURITY_SENSITIVE_PARAM_KEYS & overlay.keys()
    if overridden:
        raise ValueError(
            f"params_json must not set {sorted(overridden)}: security-sensitive "
            "settings are only honoured from a document's literal params block."
        )


class _ToolSpinner:
    """Animated single-line spinner for a tool effect running in sequential mode."""

    _SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"

    def __init__(
        self,
        name: str,
        target: str = "",
        indent: str = "",
        ancestors: list | None = None,
    ) -> None:
        self._name = name
        self._target = target
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
        suffix = " | ".join(parts)
        lines = _render_ancestors(self._ancestors, self._SPINNER)
        lines.append(
            f"{self._indent}[info]{char}[/info] [white]⚙[/white]"
            f" {self._name} [dim]{suffix}[/dim]"
        )
        return "\n".join(lines)


@dataclass(frozen=True)
class ToolDefinition:
    name: str
    provider: str
    params: dict[str, Any]
    params_json: str | None = None
    prompt: str | None = None
    model: str | None = None
    timeout_ms: int | None = None
    on_error: Literal["fail", "skip", "continue"] = "fail"
    description: str | None = None

    # Reliability (#273) — same key names, default and backoff as prompts
    # (see core.prompt.RetryPolicyDef). None: one attempt, no retry.
    retries: RetryPolicyDef | None = None

    # Output check (#273), run after a successful attempt — see core.expect.
    expect: ExpectDef | None = None

    # False = skip execution and write a disabled node (see core.disabled).
    enabled: bool = True

    # Named concurrency-group slot this effect holds for the duration of its
    # dispatch (core.concurrency.RunConcurrencyLimiter) — None means it only
    # ever competes for runtime.max_concurrency's run-wide slots, if any are
    # configured. A tool effect is a leaf, so this is where a slot is
    # actually held; see #274.
    group: str | None = None


#: Fallback when neither the effect's ``timeout_ms`` nor
#: ``runtime.tools.timeout_seconds`` in config is set. Tools get their own
#: budget here, independent of whatever the run's LLM adapter is
#: configured with (``runtime.adapters.<name>.timeout_seconds``) — see
#: ToolRuntime._resolve_timeout_seconds.
DEFAULT_TOOL_TIMEOUT_SECONDS = 300


class ToolRuntime:
    """
    Executes a ToolDefinition against a plugin + store.
    Writes:
      <name>.value
      <name>.meta{created_at, completed_at, provider, params_rendered, stdout, stderr, exit_code, error}

      ``meta.params_rendered`` is redacted (``cli.redaction.redact``) before
      storage; the plugin itself still receives the unredacted values (#238).

      ``meta.exit_code`` means a process exit code only (binary/shell/other
      process-backed plugins) — it is ``None`` for every other plugin. A
      tool fails — ``meta.error`` is set and ``on_error`` applies — whenever
      the plugin raises, or returns a ``ToolResult`` with ``ok=False``. HTTP-
      family plugins (``http``, ``web_fetch``, ``webhook``, ``linear``) fail
      this way on a 4xx/5xx response by default and set ``meta.status_code``
      to the HTTP status; each has a per-effect opt-out param (see each
      plugin's docstring) that restores the old always-succeeds behaviour.
      Soft-failure plugins (``wikipedia``, ``dns``, ``port_check``,
      ``validate_yaml``) report their own outcome via ``value``/``raw``
      fields with ``ok`` staying ``True`` — see orchestration-reference.md.

      ``meta.raw`` is the plugin's ``ToolResult.raw``, redacted and
      size-capped (``_RAW_META_MAX_BYTES``); always set for a tool effect
      that returned a result. Prompt effects never set it.

      ``meta.binary`` is also set for binary-wrapping tool plugins
      (the resolved absolute executable path), when the plugin's result
      carries one.
    """

    def __init__(
        self,
        definition: ToolDefinition,
        *,
        runtime_config: dict[str, Any] | None = None,
        dry_run: bool = False,
        # Accepted for the dynamic/loop/conditional containers that pass their
        # resolved LLM-adapter timeout here unconditionally, but not used to
        # compute the tool's own timeout budget below — see _resolve_timeout_seconds.
        timeout_seconds: int = 300,
        verbose: bool = False,
        depth: int = 0,
        cb_start: Callable[[], None] | None = None,
        cb_done: Callable[[str], None] | None = None,
        cb_error: Callable[[str], None] | None = None,
        cb_running: Callable[[str, int], None] | None = None,
        display_name: str | None = None,
        ancestors: list | None = None,
        # Only needed for `expect: {mode: model, ...}` (#273) — every other
        # tool effect ignores both. Optional (unlike prompt/use, which always
        # have one): most call sites exist to build/run a plugin, not to ask
        # a model anything.
        adapter: Adapter | None = None,
        model: str | None = None,
        model_locked: bool = False,
    ):
        self.defn = definition
        self.runtime_config = runtime_config or {}
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds
        self.verbose = verbose
        self.depth = depth
        self.cb_start = cb_start
        self.cb_done = cb_done
        self.cb_error = cb_error
        self.adapter = adapter
        self.model = model
        self.model_locked = model_locked
        self.cb_running = cb_running
        self.display_name = display_name or definition.name
        self._ancestors = ancestors or []

    def _resolve_timeout_seconds(self) -> int:
        """The tool's own timeout budget, in whole seconds.

        ``timeout_ms`` on the effect wins when set, rounded up so a
        sub-second budget (e.g. 500) never floors to 0 and gets handed to
        a subprocess/curl/urlopen call as "no timeout" or "fail instantly"
        depending on the plugin. Otherwise ``runtime.tools.timeout_seconds``
        in config, defaulting to DEFAULT_TOOL_TIMEOUT_SECONDS — deliberately
        not ``self.timeout_seconds`` (the run's LLM adapter timeout, inherited
        unconditionally from the dynamic/loop/conditional containers): a tool
        run on the no-op adapter, or one that just needs more time than the
        model's socket timeout allows, shouldn't be bound by either.
        """
        if self.defn.timeout_ms:
            return max(1, math.ceil(self.defn.timeout_ms / 1000))
        tools_cfg = self.runtime_config.get("tools")
        raw = tools_cfg.get("timeout_seconds") if isinstance(tools_cfg, dict) else None
        if raw is None:
            return DEFAULT_TOOL_TIMEOUT_SECONDS
        try:
            return max(1, int(raw))
        except (TypeError, ValueError):
            logger.warning(
                "Invalid runtime.tools.timeout_seconds=%r; using %ds",
                raw,
                DEFAULT_TOOL_TIMEOUT_SECONDS,
            )
            return DEFAULT_TOOL_TIMEOUT_SECONDS

    def _is_retryable_failure(
        self, *, status_code: int | None, failure: BaseException | None = None
    ) -> bool:
        """Whether a failed attempt is worth retrying.

        HTTP-family tools (``http``, ``web_fetch``, ``webhook``, ``linear``)
        classify by status the same way an adapter dispatch does (429/408/
        5xx retryable; any other 4xx is not). When the failure never got far
        enough to carry a status at all (a raised exception, a connection
        that never completed), ``failure`` is classified the same way an
        adapter's own dispatch failure is (:func:`classify_exception`) — a
        ``URLError``/``TimeoutError``/``ConnectionError`` (DNS failure,
        refused connection, a request that never got a reply) is retryable,
        matching ``RETRYABLE_CURL_EXIT_CODES``; anything else is not. Every
        other (process) tool — ``shell``, ``ffmpeg``, ``comfyui``, ... —
        retries on any failure: there is no status to classify, and a
        process failing once (a transient GPU watchdog kill, a flaky read)
        is exactly the case #273 exists for.
        """
        if self.defn.provider in _HTTP_FAMILY_PROVIDERS:
            if status_code is not None:
                return status_is_retryable(status_code)
            return classify_exception(failure).retryable
        return True

    def execute(self, *, store: Store, ctx: dict[str, Any]) -> None:
        from ..allowlist_gate import allowed_tools, require_tool
        from ..capability_gate import require_within_ceiling
        from ..plugins.capabilities import capabilities_of
        from ..plugins.factory import build_plugin

        node = store.ensure_dict(self.defn.name)
        node.setdefault("value", None)
        meta = node.get("meta")
        if not isinstance(meta, dict):
            meta = {}
            node["meta"] = meta

        indent = "  " * self.depth
        timeout_seconds = self._resolve_timeout_seconds()

        if self.verbose and self.cb_start is not None:
            self.cb_start()

        if self.dry_run:
            t0 = time.monotonic()
            node["value"] = None
            meta["completed_at"] = _now_iso()
            meta["dry_run"] = True
            mtag = _model_tag({"model": self.defn.model})
            if self.verbose:
                elapsed = time.monotonic() - t0
                _label = f"{self.defn.provider} · {mtag}" if mtag else self.defn.provider
                line = (
                    f"{indent}[ok]✓[/ok] [white]⚙[/white] {self.display_name}"
                    f" [dim]{_label} | {_elapsed_str(elapsed)}[/dim]"
                )
                if self.cb_done is not None:
                    self.cb_done(line)
                else:
                    _console.print(line)
            return

        # Below the dry-run return, which writes its node without firing
        # complete — start mirrors that exactly so the pair stays balanced.
        store.fire_effect_start(self.defn.name, node)

        # Retry policy: per-tool config, or one attempt (#273) — same key
        # names and backoff curve as prompts (core.prompt.RetryPolicyDef).
        if self.defn.retries is not None:
            max_attempts = max(1, self.defn.retries.max_attempts)
            backoff_ms = self.defn.retries.backoff_ms
        else:
            max_attempts = 1
            backoff_ms = 1000

        # Checked once before the first attempt, not per-attempt: neither
        # depends on anything a retry could change, and a tool blocked by
        # either should never even start its backoff schedule.
        require_tool(self.defn.provider, allowed_tools(self.runtime_config))
        # Backstop for #275: the static consent walk can miss a tool effect
        # reached through a form it doesn't parse (e.g. a legacy 'steps:'
        # branch type it doesn't recognize, or a provider: only knowable
        # after templating) — this check sees the provider actually about
        # to run, so no such gap can skip the capability ceiling.
        require_within_ceiling(
            f"tool '{self.defn.provider}'",
            capabilities_of(self.defn.provider),
            self.runtime_config,
        )

        next_delay_ms = backoff_ms
        for attempt_index in range(max_attempts):
            if attempt_index > 0:
                # Cancellation-aware, not a bare time.sleep — see core.use's
                # identical comment (#356).
                get_token().sleep_or_raise(next_delay_ms / 1000)
                if self.verbose:
                    retry_line = (
                        f"{indent}[yellow]↺[/yellow] [white]⚙[/white]"
                        f" {self.display_name} [dim]retry {attempt_index}/{max_attempts - 1}[/dim]"
                    )
                    if self.cb_done is not None:
                        self.cb_done(retry_line)
                    else:
                        _console.print(retry_line)

            t0 = time.monotonic()
            mtag = ""

            meta["created_at"] = _now_iso()
            meta["completed_at"] = None
            meta["provider"] = self.defn.provider
            meta["error"] = None
            # Reset every pass/attempt rather than only on success — otherwise
            # a reused node (an unnamed loop's repeated pass, a --state
            # resume, or this effect's own earlier attempt) keeps a prior
            # pass's result sitting next to this pass's error (#260).
            meta["stdout"] = None
            meta["stderr"] = None
            meta["exit_code"] = None
            meta.pop("binary", None)
            meta.pop("status_code", None)
            meta.pop("raw", None)
            meta.pop("expect", None)
            meta.pop("retries_used", None)
            meta["waiting_for"] = None

            target = self.defn.provider  # fallback if build_plugin fails before we can compute it
            result: ToolResult | None = None
            failure: Exception | None = None
            retryable = False
            # Set only on the ok=False path below, from the plugin's own
            # response headers — an exception never reached a response, so
            # it never has one to read.
            retry_after: str | None = None
            try:
                # Render top-level prompt/model, then merge with params (params take precedence)
                top_level: dict[str, Any] = {}
                if self.defn.prompt is not None:
                    top_level["prompt"] = render_template(self.defn.prompt, ctx, label="prompt")
                if self.defn.model is not None:
                    top_level["model"] = self.defn.model

                _reject_templated_security_params(self.defn.params)
                params = _render_params(self.defn.params, ctx, name=self.defn.name)
                if self.defn.params_json is not None:
                    params_json_overlay = _render_params_json(self.defn.params_json, ctx)
                    _reject_params_json_security_overrides(params_json_overlay)
                    params = _deep_merge_params(params, params_json_overlay)

                rendered = {**top_level, **params}
                meta["params_rendered"] = redact(rendered)
                mtag = _model_tag(rendered)

                # Build plugin early so we can use its target string in the spinner
                # (require_tool/require_within_ceiling already checked once
                # above, before the first attempt)
                plugin = build_plugin(
                    plugin_name=self.defn.provider,
                    runtime=self.runtime_config,
                )
                target = _plugin_target(plugin, mtag)

                if self.cb_running is not None:
                    self.cb_running(target, 0)

                def _on_wait(label: str) -> None:
                    store.set(f"{self.defn.name}.meta.waiting_for", label)
                    if self.verbose:
                        _console.print(
                            f"{indent}[info]…[/info] [white]⚙[/white] {self.display_name}"
                            f" [dim]waiting for '{label}'[/dim]"
                        )

                def _on_acquired() -> None:
                    store.set(f"{self.defn.name}.meta.waiting_for", None)

                limiter = self.runtime_config.get(_CONCURRENCY_LIMITER_KEY)
                concurrency_cm: AbstractContextManager[None] = (
                    limiter.acquire(
                        group=self.defn.group, on_wait=_on_wait, on_acquired=_on_acquired
                    )
                    if limiter is not None
                    else nullcontext()
                )

                # Acquired fresh for this attempt and released by the `with`
                # below before the retry loop's backoff sleep (top of the
                # next iteration) or an expect: check runs — a retrying GPU
                # step must never sleep while holding its group, and a
                # model-mode expect: must not hold it either (#333 x #273).
                with concurrency_cm:
                    # Show spinner while running (if verbose and no external cb_start — same pattern as PromptRuntime)
                    if self.verbose and self.cb_start is None:
                        from rich.live import Live

                        live_cm: Any = _live_region(
                            lambda _target=target: Live(
                                _ToolSpinner(
                                    name=self.display_name,
                                    target=_target,
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

                    with live_cm:
                        result = plugin.execute(params=rendered, timeout_seconds=timeout_seconds)

            except Exception as e:
                failure = e
                retryable = self._is_retryable_failure(status_code=None, failure=e)

            if failure is not None:
                meta["error"] = str(failure)
                meta["completed_at"] = _now_iso()
                is_last_attempt = attempt_index >= max_attempts - 1
                if self.verbose:
                    elapsed = time.monotonic() - t0
                    _tgt = f"{self.defn.provider} · {mtag}" if mtag else self.defn.provider
                    line = (
                        f"{indent}[err]✗[/err] [white]⚙[/white] {self.display_name}"
                        f" [dim]{_tgt} | {_elapsed_str(elapsed)}[/dim]"
                    )
                    if self.cb_error is not None:
                        self.cb_error(line)
                    else:
                        _console.print(line)
                if retryable and not is_last_attempt:
                    next_delay_ms = next_backoff_delay_ms(
                        RetryInfo(retryable=True), attempt_index=attempt_index, base_ms=backoff_ms
                    )
                    continue
                if self.defn.on_error in ("skip", "continue"):
                    node["value"] = None
                # Fires before the re-raise so the start/complete pair stays
                # balanced on the failure path too — the node carries meta.error.
                store.fire_effect_complete(self.defn.name, node)
                if self.defn.on_error == "fail":
                    raise failure
                return

            assert result is not None

            node["value"] = result.value
            meta["stdout"] = result.stdout
            meta["stderr"] = result.stderr
            meta["exit_code"] = result.exit_code
            status_code: int | None = None
            if self.defn.provider in _HTTP_FAMILY_PROVIDERS and isinstance(
                result.raw.get("status"), int
            ):
                status_code = result.raw["status"]
                meta["status_code"] = status_code
            if "binary" in result.raw:
                meta["binary"] = result.raw["binary"]
            meta["raw"] = _capped_raw(result.raw)

            if not result.ok:
                failure = RuntimeError(
                    result.stderr or f"{self.defn.provider} tool reported failure (ok=False)"
                )
                retryable = self._is_retryable_failure(status_code=status_code)
                if self.defn.provider in _HTTP_FAMILY_PROVIDERS:
                    headers = result.raw.get("headers")
                    if isinstance(headers, dict):
                        retry_after = headers.get("retry-after") or headers.get("Retry-After")
            else:
                # Success — the expect check, if any, decides whether this
                # attempt actually counts as one (#273). A failed expect is
                # always worth retrying: unlike an HTTP status, it carries
                # no classification of its own, and the motivating case
                # (a flaky image-generation result) is exactly "try again".
                # The call itself can also raise (no adapter/model available
                # for mode: model, a bad template) — caught here, not left to
                # escape past retries/on_error with the start/complete pair
                # left unbalanced (#273 review).
                if self.defn.expect is not None:
                    try:
                        outcome = evaluate_expect(
                            self.defn.expect,
                            value=node["value"],
                            meta=meta,
                            ctx=ctx,
                            adapter=self.adapter,
                            model=self.model,
                            timeout_seconds=timeout_seconds,
                            effect_label=f"tool '{self.defn.name}'",
                        )
                    except Exception as expect_exc:
                        failure = expect_exc
                        retryable = True
                    else:
                        meta["expect"] = outcome.meta
                        if not outcome.passed:
                            failure = RuntimeError(
                                f"expect failed: {expect_failure_summary(self.defn.expect)}"
                            )
                            retryable = True

            meta["completed_at"] = _now_iso()

            if failure is not None:
                meta["error"] = str(failure)
                is_last_attempt = attempt_index >= max_attempts - 1
                if self.verbose:
                    elapsed = time.monotonic() - t0
                    line = (
                        f"{indent}[err]✗[/err] [white]⚙[/white] {self.display_name}"
                        f" [dim]{target} | {_elapsed_str(elapsed)}[/dim]"
                    )
                    if self.cb_error is not None:
                        self.cb_error(line)
                    else:
                        _console.print(line)
                if retryable and not is_last_attempt:
                    next_delay_ms = next_backoff_delay_ms(
                        RetryInfo(retryable=True, retry_after=retry_after),
                        attempt_index=attempt_index,
                        base_ms=backoff_ms,
                    )
                    continue
                if self.defn.on_error in ("skip", "continue"):
                    node["value"] = None
                store.fire_effect_complete(self.defn.name, node)
                if self.defn.on_error == "fail":
                    raise failure
                return

            if attempt_index > 0:
                meta["retries_used"] = attempt_index

            if self.verbose:
                elapsed = time.monotonic() - t0
                suffix = _elapsed_str(elapsed)
                out = _format_output(result.value)
                if out:
                    suffix += f" → {out}"
                line = (
                    f"{indent}[ok]✓[/ok] [white]⚙[/white] {self.display_name}"
                    f" [dim]{target} | {suffix}[/dim]"
                )
                if self.cb_done is not None:
                    self.cb_done(line)
                else:
                    _console.print(line)

            store.fire_effect_complete(self.defn.name, node)
            return
