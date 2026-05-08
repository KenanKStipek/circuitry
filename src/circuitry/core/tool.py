from __future__ import annotations

import logging
import time
from contextlib import nullcontext
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Callable, Literal

from ..output import console as _console
from .store import Store

logger = logging.getLogger(__name__)


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


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
    """Recursively Mustache-render all string values in params against ctx."""
    try:
        import chevron  # type: ignore

        def _render_value(v: Any) -> Any:
            if isinstance(v, str):
                return chevron.render(v, ctx)
            if isinstance(v, dict):
                return {k: _render_value(vv) for k, vv in v.items()}
            if isinstance(v, list):
                return [_render_value(item) for item in v]
            return v

        return {k: _render_value(v) for k, v in params.items()}
    except Exception:
        logger.warning("Tool param rendering failed; returning raw params", exc_info=True)
        return params


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
    prompt: str | None = None
    model: str | None = None
    timeout_ms: int | None = None
    on_error: Literal["fail", "skip", "continue"] = "fail"
    description: str | None = None


class ToolRuntime:
    """
    Executes a ToolDefinition against a plugin + store.
    Writes:
      <name>.value
      <name>.meta{created_at, completed_at, provider, params_rendered, stdout, stderr, exit_code, error}
    """

    def __init__(
        self,
        definition: ToolDefinition,
        *,
        runtime_config: dict[str, Any] | None = None,
        dry_run: bool = False,
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

    def execute(self, *, store: Store, ctx: dict[str, Any]) -> None:
        from ..plugins.factory import build_plugin

        node = store.ensure_dict(self.defn.name)
        node.setdefault("value", None)
        meta = node.get("meta")
        if not isinstance(meta, dict):
            meta = {}
            node["meta"] = meta

        indent = "  " * self.depth
        timeout_seconds = (
            self.defn.timeout_ms // 1000 if self.defn.timeout_ms else self.timeout_seconds
        )
        t0 = time.monotonic()
        mtag = ""

        meta["created_at"] = _now_iso()
        meta["completed_at"] = None
        meta["provider"] = self.defn.provider
        meta["error"] = None

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

        # Render top-level prompt/model, then merge with params (params take precedence)
        top_level: dict[str, Any] = {}
        if self.defn.prompt is not None:
            try:
                import chevron  # type: ignore
                top_level["prompt"] = chevron.render(self.defn.prompt, ctx)
            except Exception:
                logger.warning("Tool prompt template rendering failed; using raw prompt", exc_info=True)
                top_level["prompt"] = self.defn.prompt
        if self.defn.model is not None:
            top_level["model"] = self.defn.model

        rendered = {**top_level, **_render_params(self.defn.params, ctx)}
        meta["params_rendered"] = rendered
        mtag = _model_tag(rendered)

        target = self.defn.provider  # fallback if build_plugin fails before we can compute it
        try:
            # Build plugin early so we can use its target string in the spinner
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

            node["value"] = result.value
            meta["stdout"] = result.stdout
            meta["stderr"] = result.stderr
            meta["exit_code"] = result.exit_code
            meta["completed_at"] = _now_iso()

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
            if self.defn.on_error == "fail":
                raise
            elif self.defn.on_error == "skip":
                node["value"] = None
            # continue: keep going with None value
