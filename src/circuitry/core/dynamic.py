from __future__ import annotations

import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from contextlib import nullcontext
from copy import deepcopy
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import TYPE_CHECKING, Any, Callable, Literal, Sequence, Union

from ..adapters import Adapter
from ..output import console as _console
from .prompt import PromptDefinition, PromptRuntime
from .store import Store

if TYPE_CHECKING:
    # Type-only imports to avoid circular imports at runtime
    from .conditional import ConditionalDefinition
    from .loop import LoopDefinition
    from .reflector import ReflectorDefinition
    from .tool import ToolDefinition


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


EffectDef = Union[
    "DynamicDefinition",
    PromptDefinition,
    "ReflectorDefinition",
    "ConditionalDefinition",
    "LoopDefinition",
    "ToolDefinition",
]


@dataclass(frozen=True)
class DynamicDefinition:
    name: str
    effects: Sequence[EffectDef]
    flow: Literal["chain", "tree"] = "chain"


class DynamicRuntime:
    def __init__(
        self,
        definition: DynamicDefinition,
        *,
        adapter: Adapter,
        model: str,
        runtime_config: dict[str, Any] | None = None,
        dry_run: bool = False,
        timeout_seconds: int = 120,
        verbose: bool = False,
        depth: int = 0,
    ):
        self.defn = definition
        self.adapter = adapter
        self.model = model
        self.runtime_config = runtime_config or {}
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds
        self.verbose = verbose
        self.depth = depth

    def execute(
        self, *, store: Store, ctx_override: dict[str, Any] | None = None
    ) -> None:
        dyn = store.ensure_dict(self.defn.name)
        dyn.setdefault("value", None)
        meta = dyn.get("meta")
        if not isinstance(meta, dict):
            meta = {}
            dyn["meta"] = meta

        meta.update(
            {
                "created_at": _now_iso(),
                "completed_at": None,
                "adapter": getattr(self.adapter, "name", "unknown"),
                "model": self.model,
                "tokens_sent": None,
                "tokens_received": None,
                "error": None,
                "flow": self.defn.flow,
                "dry_run": self.dry_run,
            }
        )

        if self.defn.flow not in ("chain", "tree"):
            meta["error"] = f"Unsupported flow: {self.defn.flow}"
            meta["completed_at"] = _now_iso()
            dyn["value"] = False
            raise ValueError(meta["error"])

        ctx = ctx_override if ctx_override is not None else store.state
        child_store = Store(dyn, on_write=store.on_write)

        try:
            if self.defn.flow == "chain":
                for idx, effect in enumerate(self.defn.effects):
                    effect_path = self._effect_path(effect=effect, index=idx)
                    try:
                        self._execute_effect(effect, store=child_store, ctx=ctx)
                    except Exception as e:
                        raise RuntimeError(f"{effect_path}: {e}") from e
                    finally:
                        if store.on_write:
                            store.on_write(store.state)
            else:
                # Tree semantics: all effects run concurrently against the same
                # deterministic snapshot from dynamic start, not sibling writes.
                tree_ctx = deepcopy(ctx)

                # Pre-allocate store slots sequentially to avoid concurrent dict mutation
                for effect in self.defn.effects:
                    name = getattr(effect, "name", None)
                    if name:
                        child_store.ensure_dict(name)

                tree_errors: list[Exception] = []

                # Build animated per-effect tracker for verbose display
                tree_tracker: _TreeStatus | None = None
                if self.verbose and self.defn.effects:
                    tracker_items = []
                    for effect in self.defn.effects:
                        _tl = _effect_type_label(effect)
                        _icon, _color = _EFFECT_STYLE.get(_tl, ("·", "white"))
                        _ename = getattr(effect, "name", None) or "?"
                        tracker_items.append((_ename, _icon, _color))
                    tree_tracker = _TreeStatus(
                        items=tracker_items,
                        indent="  " * self.depth,
                    )

                if tree_tracker is not None:
                    from rich.live import Live

                    live_ctx: Any = Live(
                        tree_tracker,
                        refresh_per_second=10,
                        transient=True,
                        console=_console,
                    )
                else:
                    live_ctx = nullcontext()

                with live_ctx:
                    with ThreadPoolExecutor(
                        max_workers=len(self.defn.effects)
                    ) as executor:
                        futures: dict = {
                            executor.submit(
                                self._execute_effect,
                                effect,
                                store=child_store,
                                ctx=tree_ctx,
                                tracker=tree_tracker,
                            ): self._effect_path(effect=effect, index=idx)
                            for idx, effect in enumerate(self.defn.effects)
                        }
                        for future in as_completed(futures):
                            try:
                                future.result()
                            except Exception as e:
                                tree_errors.append(e)

                if store.on_write:
                    store.on_write(store.state)

                if tree_errors:
                    raise tree_errors[0]

            dyn["value"] = True
            meta["completed_at"] = _now_iso()

        except Exception as e:
            dyn["value"] = False
            meta["error"] = str(e)
            meta["completed_at"] = _now_iso()
            raise

    def _execute_effect(
        self,
        effect: EffectDef,
        *,
        store: Store,
        ctx: dict[str, Any],
        cb_start: Callable[[], None] | None = None,
        cb_done: Callable[[str], None] | None = None,
        cb_error: Callable[[str], None] | None = None,
        tracker: "_TreeStatus | None" = None,
    ) -> None:
        """Execute a single effect within the dynamic."""
        # Local imports to avoid circular imports at module load time
        from .conditional import ConditionalDefinition, ConditionalRuntime
        from .loop import LoopDefinition, LoopRuntime
        from .reflector import ReflectorDefinition, ReflectorRuntime

        from .tool import ToolDefinition, ToolRuntime

        indent = "  " * self.depth
        type_label = _effect_type_label(effect)
        icon, color = _EFFECT_STYLE.get(type_label, ("·", "white"))
        name = getattr(effect, "name", None) or "?"
        is_prompt = isinstance(effect, PromptDefinition)
        is_tool = isinstance(effect, ToolDefinition)

        # If a tracker is provided, derive all callbacks from it
        _cb_running: Callable[[str, int], None] | None = None
        if tracker is not None:
            _n = name
            cb_start = lambda _k=_n: tracker.on_start(_k)
            cb_done = lambda line, _k=_n: tracker.on_done(_k, line)
            cb_error = lambda line, _k=_n: tracker.on_error(_k, line)
            _cb_running = lambda t, e, _k=_n: tracker.on_running(_k, t, e)

        if self.verbose and not is_prompt and not is_tool:
            if cb_start is not None:
                cb_start()
            else:
                _console.print(
                    f"{indent}[info]→[/info] [{color}]{icon}[/{color}] {name}"
                )

        t0 = time.monotonic()
        try:
            if is_prompt:
                PromptRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    runtime_config=self.runtime_config,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                    verbose=self.verbose,
                    depth=self.depth,
                    cb_start=cb_start,
                    cb_done=cb_done,
                    cb_error=cb_error,
                    cb_running=_cb_running,
                ).execute(store=store, ctx=ctx)

            elif isinstance(effect, DynamicDefinition):
                DynamicRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    runtime_config=self.runtime_config,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                    verbose=self.verbose,
                    depth=self.depth + 1,
                ).execute(store=store, ctx_override=ctx)

            elif isinstance(effect, ReflectorDefinition):
                ReflectorRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    runtime_config=self.runtime_config,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                    verbose=self.verbose,
                ).execute(store=store)

            elif isinstance(effect, ConditionalDefinition):
                ConditionalRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    runtime_config=self.runtime_config,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                    verbose=self.verbose,
                    depth=self.depth,
                ).execute(store=store, ctx=ctx)

            elif isinstance(effect, LoopDefinition):
                LoopRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    runtime_config=self.runtime_config,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                    verbose=self.verbose,
                    depth=self.depth,
                ).execute(store=store, ctx=ctx)

            elif isinstance(effect, ToolDefinition):
                ToolRuntime(
                    effect,
                    runtime_config=self.runtime_config,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                    verbose=self.verbose,
                    depth=self.depth,
                    cb_start=cb_start,
                    cb_done=cb_done,
                    cb_error=cb_error,
                    cb_running=_cb_running,
                ).execute(store=store, ctx=ctx)

            else:
                raise TypeError(f"Unsupported effect type: {type(effect)}")

            if self.verbose and not is_prompt and not is_tool:
                elapsed = time.monotonic() - t0
                suffix = _elapsed_str(elapsed)
                if isinstance(effect, DynamicDefinition):
                    sent, recv = _sum_tokens(store.state.get(name, {}))
                    if sent or recv:
                        suffix += f" | ↑{_fmt_tokens(sent)} ↓{_fmt_tokens(recv)} tok"
                line = (
                    f"{indent}[ok]✓[/ok] [{color}]{icon}[/{color}]"
                    f" {name} [dim]{suffix}[/dim]"
                )
                if cb_done is not None:
                    cb_done(line)
                else:
                    _console.print(line)

        except Exception:
            if self.verbose and not is_prompt and not is_tool:
                elapsed = time.monotonic() - t0
                suffix = _elapsed_str(elapsed)
                if isinstance(effect, DynamicDefinition):
                    sent, recv = _sum_tokens(store.state.get(name, {}))
                    if sent or recv:
                        suffix += f" | ↑{_fmt_tokens(sent)} ↓{_fmt_tokens(recv)} tok"
                line = (
                    f"{indent}[err]✗[/err] [{color}]{icon}[/{color}]"
                    f" {name} [dim]{suffix}[/dim]"
                )
                if cb_error is not None:
                    cb_error(line)
                else:
                    _console.print(line)
            raise

    def _effect_path(self, *, effect: EffectDef, index: int) -> str:
        name = getattr(effect, "name", None)
        if isinstance(name, str) and name:
            return f"{self.defn.name}.{name}"
        return f"{self.defn.name}.{type(effect).__name__}[{index}]"


def _effect_type_label(effect: Any) -> str:
    """Return a short human-readable type label for verbose output."""
    # Deferred imports to avoid circular dependency at module level
    from .conditional import ConditionalDefinition
    from .loop import LoopDefinition
    from .reflector import ReflectorDefinition
    from .tool import ToolDefinition

    if isinstance(effect, PromptDefinition):
        return "prompt"
    if isinstance(effect, DynamicDefinition):
        return "dynamic"
    if isinstance(effect, ConditionalDefinition):
        return "if"
    if isinstance(effect, LoopDefinition):
        return "loop"
    if isinstance(effect, ReflectorDefinition):
        return "reflector"
    if isinstance(effect, ToolDefinition):
        return "tool"
    return type(effect).__name__.lower()


# (icon, rich color) per primitive type
_EFFECT_STYLE: dict[str, tuple[str, str]] = {
    "prompt": ("◆", "cyan"),
    "dynamic": ("⬡", "blue"),
    "loop": ("↻", "yellow"),
    "if": ("◇", "magenta"),
    "reflector": ("✺", "green"),
    "tool": ("⚙", "white"),
}


def _make_start_cb(effect: Any, depth: int) -> Callable[[], None]:
    """Return a callback that prints the '→ <icon> <name>' start line for an effect."""
    type_label = _effect_type_label(effect)
    icon, color = _EFFECT_STYLE.get(type_label, ("·", "white"))
    name = getattr(effect, "name", None) or "?"
    indent = "  " * depth

    def _cb() -> None:
        _console.print(f"{indent}[info]→[/info] [{color}]{icon}[/{color}] {name}")

    return _cb


def _elapsed_str(seconds: float) -> str:
    if seconds >= 1:
        return f"{seconds:.2f}s"
    return f"{seconds * 1000:.0f}ms"


def _fmt_tokens(n: int) -> str:
    """Format a token count in compact human-readable form."""
    if n >= 1_000_000:
        return f"{n / 1_000_000:.1f}M"
    if n >= 1_000:
        return f"{n / 1_000:.1f}k"
    return str(n)


def _sum_tokens(d: dict) -> tuple[int, int]:
    """Recursively sum tokens_sent/received from all prompt meta dicts in a store subtree."""
    sent = recv = 0
    if not isinstance(d, dict):
        return sent, recv
    meta = d.get("meta")
    if isinstance(meta, dict):
        sent += meta.get("tokens_sent") or 0
        recv += meta.get("tokens_received") or 0
    for v in d.values():
        if isinstance(v, dict):
            s, r = _sum_tokens(v)
            sent += s
            recv += r
    return sent, recv


class _TreeStatus:
    """
    Tracks running state for parallel dynamic tree effects.
    Rendered as a multi-line animated block inside a single rich.live.Live context.
    Done/error lines are printed immediately; they are omitted from __rich__ to avoid duplicates.
    """

    _SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"

    def __init__(
        self,
        items: list[tuple[str, str, str]],  # (name, icon, color)
        indent: str = "",
    ) -> None:
        self._lock = threading.Lock()
        self._items = list(items)  # (name, icon, color)
        self._names = [name for name, _, _ in items]
        self._states: dict[str, str] = {name: "pending" for name, _, _ in items}
        self._targets: dict[str, str] = {name: "" for name, _, _ in items}
        self._estimated: dict[str, int] = {name: 0 for name, _, _ in items}
        self._start = time.monotonic()
        self._indent = indent

    def on_start(self, name: str) -> None:
        with self._lock:
            self._states[name] = "running"

    def on_running(self, name: str, target: str, estimated_out: int) -> None:
        with self._lock:
            self._targets[name] = target
            self._estimated[name] = estimated_out

    def on_done(self, name: str, line: str) -> None:
        with self._lock:
            self._states[name] = "done"
        _console.print(line)

    def on_error(self, name: str, line: str) -> None:
        with self._lock:
            self._states[name] = "error"
        _console.print(line)

    def __rich__(self) -> str:
        elapsed = time.monotonic() - self._start
        spinner_char = self._SPINNER[int(elapsed * 8) % len(self._SPINNER)]
        with self._lock:
            states = dict(self._states)
            targets = dict(self._targets)
            estimated = dict(self._estimated)
        lines: list[str] = []
        for name, icon, color in self._items:
            state = states.get(name, "pending")
            if state == "running":
                t = targets.get(name, "")
                e = estimated.get(name, 0)
                if e and t:
                    dim_suffix = f" [dim]~{e}tok ↑  {t}[/dim]"
                elif t:
                    dim_suffix = f" [dim]{t}[/dim]"
                elif e:
                    dim_suffix = f" [dim]~{e}tok ↑[/dim]"
                else:
                    dim_suffix = ""
                lines.append(
                    f"{self._indent}[info]{spinner_char}[/info] [{color}]{icon}[/{color}] {name}{dim_suffix}"
                )
            elif state == "pending":
                lines.append(f"{self._indent}[dim]· {icon} {name}[/dim]")
            # done/error: already printed via on_done/on_error; omit from live display
        return "\n".join(lines)

