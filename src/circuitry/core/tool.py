from __future__ import annotations

import json
import logging
import math
import time
from collections.abc import Callable
from contextlib import nullcontext
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Literal

from ..cli.redaction import redact
from ..output import console as _console
from ..plugins.base import ToolResult
from .store import Store
from .templates import render_template

logger = logging.getLogger(__name__)


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


#: Cap on meta.raw's serialized size, in bytes, after redaction. Protects
#: state (and anything that mirrors it: --out, --live-state, persisted
#: snapshots) from a provider response ToolResult.raw is large enough to
#: carry (e.g. a big HTTP body echoed verbatim) — see orchestration-reference.md.
_RAW_META_MAX_BYTES = 64 * 1024


def _capped_raw(raw: dict[str, Any]) -> dict[str, Any]:
    """Redact *raw*, then replace it with a truncation marker if it's still
    too big to store safely in state."""
    redacted = redact(raw)
    try:
        encoded = json.dumps(redacted, ensure_ascii=False, default=str).encode("utf-8")
    except (TypeError, ValueError):
        return redacted
    if len(encoded) <= _RAW_META_MAX_BYTES:
        return redacted
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


def _render_params(params: dict[str, Any], ctx: dict[str, Any]) -> dict[str, Any]:
    """Recursively Mustache-render all string values in params against ctx.

    A value that fails to render raises (naming its path) rather than handing
    the tool unrendered text.
    """

    def _render_value(v: Any, path: str) -> Any:
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

    # False = skip execution and write a disabled node (see core.disabled).
    enabled: bool = True


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

    def execute(self, *, store: Store, ctx: dict[str, Any]) -> None:
        from ..allowlist_gate import allowed_tools, require_tool
        from ..plugins.factory import build_plugin

        node = store.ensure_dict(self.defn.name)
        node.setdefault("value", None)
        meta = node.get("meta")
        if not isinstance(meta, dict):
            meta = {}
            node["meta"] = meta

        indent = "  " * self.depth
        timeout_seconds = self._resolve_timeout_seconds()
        t0 = time.monotonic()
        mtag = ""

        meta["created_at"] = _now_iso()
        meta["completed_at"] = None
        meta["provider"] = self.defn.provider
        meta["error"] = None
        # Reset every pass/attempt rather than only on success — otherwise a
        # reused node (an unnamed loop's repeated pass, a --state resume)
        # keeps a prior pass's result sitting next to this pass's error
        # (#260).
        meta["stdout"] = None
        meta["stderr"] = None
        meta["exit_code"] = None
        meta.pop("binary", None)
        meta.pop("status_code", None)
        meta.pop("raw", None)

        if self.verbose and self.cb_start is not None:
            self.cb_start()

        if self.dry_run:
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

        target = self.defn.provider  # fallback if build_plugin fails before we can compute it
        result: ToolResult | None = None
        try:
            # Render top-level prompt/model, then merge with params (params take precedence)
            top_level: dict[str, Any] = {}
            if self.defn.prompt is not None:
                top_level["prompt"] = render_template(self.defn.prompt, ctx, label="prompt")
            if self.defn.model is not None:
                top_level["model"] = self.defn.model

            params = _render_params(self.defn.params, ctx)
            if self.defn.params_json is not None:
                params = _deep_merge_params(params, _render_params_json(self.defn.params_json, ctx))

            rendered = {**top_level, **params}
            meta["params_rendered"] = redact(rendered)
            mtag = _model_tag(rendered)

            # Build plugin early so we can use its target string in the spinner
            require_tool(self.defn.provider, allowed_tools(self.runtime_config))
            plugin = build_plugin(
                plugin_name=self.defn.provider,
                runtime=self.runtime_config,
            )
            target = _plugin_target(plugin, mtag)

            if self.cb_running is not None:
                self.cb_running(target, 0)

            # Show spinner while running (if verbose and no external cb_start — same pattern as PromptRuntime)
            if self.verbose and self.cb_start is None:
                from rich.live import Live

                live_cm = Live(
                    _ToolSpinner(
                        name=self.display_name,
                        target=target,
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
                result = plugin.execute(params=rendered, timeout_seconds=timeout_seconds)

        except Exception as e:
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
            meta["error"] = str(e)
            meta["completed_at"] = _now_iso()
            if self.defn.on_error in ("skip", "continue"):
                node["value"] = None
            # Fires before the re-raise so the start/complete pair stays
            # balanced on the failure path too — the node carries meta.error.
            store.fire_effect_complete(self.defn.name, node)
            if self.defn.on_error == "fail":
                raise
            return

        assert result is not None

        node["value"] = result.value
        meta["stdout"] = result.stdout
        meta["stderr"] = result.stderr
        meta["exit_code"] = result.exit_code
        if isinstance(result.raw.get("status"), int):
            meta["status_code"] = result.raw["status"]
        if "binary" in result.raw:
            meta["binary"] = result.raw["binary"]
        meta["raw"] = _capped_raw(result.raw)
        meta["completed_at"] = _now_iso()

        if not result.ok:
            error_message = result.stderr or f"{self.defn.provider} tool reported failure (ok=False)"
            meta["error"] = error_message
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
            if self.defn.on_error in ("skip", "continue"):
                node["value"] = None
            store.fire_effect_complete(self.defn.name, node)
            if self.defn.on_error == "fail":
                raise RuntimeError(error_message)
            return

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
