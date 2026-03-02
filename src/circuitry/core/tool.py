from __future__ import annotations

import time
from contextlib import nullcontext
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Callable, Literal

from ..output import console as _console
from .store import Store


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _elapsed_str(seconds: float) -> str:
    if seconds >= 1:
        return f"{seconds:.2f}s"
    return f"{seconds * 1000:.0f}ms"


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
        return params


class _ToolSpinner:
    """Animated single-line spinner for a tool effect running in sequential mode."""

    _SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"

    def __init__(self, text: str, indent: str = "") -> None:
        self._text = text
        self._indent = indent
        self._start = time.monotonic()

    def __rich__(self) -> str:
        elapsed = time.monotonic() - self._start
        char = self._SPINNER[int(elapsed * 8) % len(self._SPINNER)]
        return f"{self._indent}[info]{char}[/info] [white]⚙[/white] {self._text}"


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
        display_name: str | None = None,
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
        self.display_name = display_name or definition.name

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
            if self.verbose:
                elapsed = time.monotonic() - t0
                line = (
                    f"{indent}[ok]✓[/ok] [white]⚙[/white] {self.display_name}"
                    f" [dim]{self.defn.provider} | {_elapsed_str(elapsed)}[/dim]"
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
                top_level["prompt"] = self.defn.prompt
        if self.defn.model is not None:
            top_level["model"] = self.defn.model

        rendered = {**top_level, **_render_params(self.defn.params, ctx)}
        meta["params_rendered"] = rendered

        try:
            # Show spinner while running (if verbose and no external cb_start — same pattern as PromptRuntime)
            if self.verbose and self.cb_start is None:
                from rich.live import Live

                live_cm = Live(
                    _ToolSpinner(
                        f"{self.display_name} [dim]{self.defn.provider}[/dim]",
                        indent=indent,
                    ),
                    refresh_per_second=10,
                    transient=True,
                    console=_console,
                )
            else:
                live_cm = nullcontext()

            with live_cm:
                plugin = build_plugin(
                    plugin_name=self.defn.provider,
                    runtime=self.runtime_config,
                )
                result = plugin.execute(params=rendered, timeout_seconds=timeout_seconds)

            node["value"] = result.value
            meta["stdout"] = result.stdout
            meta["stderr"] = result.stderr
            meta["exit_code"] = result.exit_code
            meta["completed_at"] = _now_iso()

            if self.verbose:
                elapsed = time.monotonic() - t0
                line = (
                    f"{indent}[ok]✓[/ok] [white]⚙[/white] {self.display_name}"
                    f" [dim]{self.defn.provider} | {_elapsed_str(elapsed)}[/dim]"
                )
                if self.cb_done is not None:
                    self.cb_done(line)
                else:
                    _console.print(line)

        except Exception as e:
            if self.verbose:
                elapsed = time.monotonic() - t0
                line = (
                    f"{indent}[err]✗[/err] [white]⚙[/white] {self.display_name}"
                    f" [dim]{self.defn.provider} | {_elapsed_str(elapsed)}[/dim]"
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
