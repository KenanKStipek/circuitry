from __future__ import annotations

import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
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


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


EffectDef = Union[
    "DynamicDefinition",
    PromptDefinition,
    "ReflectorDefinition",
    "ConditionalDefinition",
    "LoopDefinition",
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

                with ThreadPoolExecutor(
                    max_workers=len(self.defn.effects)
                ) as executor:
                    futures: dict = {
                        executor.submit(
                            self._execute_effect,
                            effect,
                            store=child_store,
                            ctx=tree_ctx,
                            # Print a static "→" start line; suppresses per-prompt Live spinner.
                            cb_start=_make_start_cb(effect, self.depth),
                            cb_done=_console.print,
                            cb_error=_console.print,
                        ): self._effect_path(effect=effect, index=idx)
                        for idx, effect in enumerate(self.defn.effects)
                    }
                    for future in as_completed(futures):
                        try:
                            future.result()
                        except Exception as e:
                            tree_errors.append(e)

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
    ) -> None:
        """Execute a single effect within the dynamic."""
        # Local imports to avoid circular imports at module load time
        from .conditional import ConditionalDefinition, ConditionalRuntime
        from .loop import LoopDefinition, LoopRuntime
        from .reflector import ReflectorDefinition, ReflectorRuntime

        indent = "  " * self.depth
        type_label = _effect_type_label(effect)
        icon, color = _EFFECT_STYLE.get(type_label, ("·", "white"))
        name = getattr(effect, "name", None) or "?"
        is_prompt = isinstance(effect, PromptDefinition)

        if self.verbose and not is_prompt:
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

            else:
                raise TypeError(f"Unsupported effect type: {type(effect)}")

            if self.verbose and not is_prompt:
                elapsed = time.monotonic() - t0
                suffix = _elapsed_str(elapsed)
                if isinstance(effect, DynamicDefinition):
                    sent, recv = _sum_tokens(store.state.get(name, {}))
                    if sent or recv:
                        suffix += f" | ↑{_fmt_tokens(sent)} ↓{_fmt_tokens(recv)} tok"
                _console.print(
                    f"{indent}[ok]✓[/ok] [{color}]{icon}[/{color}]"
                    f" {name} [dim]{suffix}[/dim]"
                )

        except Exception:
            if self.verbose and not is_prompt:
                elapsed = time.monotonic() - t0
                suffix = _elapsed_str(elapsed)
                if isinstance(effect, DynamicDefinition):
                    sent, recv = _sum_tokens(store.state.get(name, {}))
                    if sent or recv:
                        suffix += f" | ↑{_fmt_tokens(sent)} ↓{_fmt_tokens(recv)} tok"
                _console.print(
                    f"{indent}[err]✗[/err] [{color}]{icon}[/{color}]"
                    f" {name} [dim]{suffix}[/dim]"
                )
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
    return type(effect).__name__.lower()


# (icon, rich color) per primitive type
_EFFECT_STYLE: dict[str, tuple[str, str]] = {
    "prompt": ("◆", "cyan"),
    "dynamic": ("⬡", "blue"),
    "loop": ("↻", "yellow"),
    "if": ("◇", "magenta"),
    "reflector": ("✺", "green"),
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
    """Renders concurrent effect statuses on a single animated line for tree flow."""

    _SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"

    def __init__(
        self,
        names: list[str],
        indent: str = "",
        icon: str = "◆",
        color: str = "cyan",
    ) -> None:
        self._lock = threading.Lock()
        self._names = list(names)
        self._states: dict[str, str] = {n: "pending" for n in names}
        self._start = time.monotonic()
        self._indent = indent
        self._icon = icon
        self._color = color
        self.done_lines: list[str] = []

    def on_start(self, name: str) -> None:
        with self._lock:
            self._states[name] = "running"

    def on_done(self, name: str, line: str) -> None:
        with self._lock:
            self._states[name] = "done"
            self.done_lines.append(line)

    def on_error(self, name: str, line: str) -> None:
        with self._lock:
            self._states[name] = "error"
            self.done_lines.append(line)

    def __rich__(self) -> str:
        elapsed = time.monotonic() - self._start
        spinner_char = self._SPINNER[int(elapsed * 8) % len(self._SPINNER)]
        parts: list[str] = []
        with self._lock:
            states = dict(self._states)
        ic = self._icon
        co = self._color
        for name in self._names:
            state = states.get(name, "pending")
            if state == "running":
                parts.append(f"[info]{spinner_char}[/info] [{co}]{ic}[/{co}] {name}")
            elif state == "done":
                parts.append(f"[ok]✓[/ok] [{co}]{ic}[/{co}] {name}")
            elif state == "error":
                parts.append(f"[err]✗[/err] [{co}]{ic}[/{co}] {name}")
            else:
                parts.append(f"[dim]· {ic} {name}[/dim]")
        return self._indent + "   ".join(parts)

