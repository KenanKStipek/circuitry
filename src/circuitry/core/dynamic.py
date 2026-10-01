from __future__ import annotations

import logging
import threading
import time
from collections.abc import Callable, Mapping, Sequence
from concurrent.futures import ThreadPoolExecutor, as_completed
from contextlib import nullcontext
from copy import deepcopy
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import TYPE_CHECKING, Any, Literal, Union

from ..adapters import Adapter
from ..output import console as _console
from .disabled import is_enabled, write_disabled_node
from .prompt import PromptDefinition, PromptRuntime
from .store import Store

logger = logging.getLogger(__name__)

if TYPE_CHECKING:
    # Type-only imports to avoid circular imports at runtime
    from .conditional import ConditionalDefinition
    from .loop import LoopDefinition
    from .reflector import ReflectorDefinition
    from .tool import ToolDefinition
    from .use import UseDefinition


class TreeExecutionError(RuntimeError):
    """Raised when one or more effects fail during parallel (tree) execution."""

    def __init__(self, errors: list[Exception]) -> None:
        self.errors = errors
        super().__init__(self._format())

    def _format(self) -> str:
        if len(self.errors) == 1:
            return str(self.errors[0])
        parts = [f"{len(self.errors)} effects failed in parallel:"]
        for i, err in enumerate(self.errors, 1):
            # Each entry is wrapped in a RuntimeError to carry the failing
            # child's path (#289); show the original error's own type,
            # which is what the author's exception handling actually cares
            # about, not the wrapper's.
            original = err.__cause__ or err
            parts.append(f"  [{i}] {type(original).__name__}: {err}")
        return "\n".join(parts)


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


EffectDef = Union[
    "DynamicDefinition",
    PromptDefinition,
    "ReflectorDefinition",
    "ConditionalDefinition",
    "LoopDefinition",
    "ToolDefinition",
    "UseDefinition",
]


@dataclass
class AncestorContext:
    """Tracks a running parent/grandparent container for live elapsed-time display."""

    name: str
    icon: str
    color: str
    start: float  # time.monotonic()
    indent: str


def _render_ancestors(ancestors: list[AncestorContext], spinner_chars: str) -> list[str]:
    """Render ancestor context lines with live count-up timers."""
    now = time.monotonic()
    char = spinner_chars[int(now * 8) % len(spinner_chars)]
    lines: list[str] = []
    for a in ancestors:
        elapsed = _elapsed_str(now - a.start)
        lines.append(
            f"{a.indent}[info]{char}[/info] [{a.color}]{a.icon}[/{a.color}]"
            f" {a.name} [dim]{elapsed}[/dim]"
        )
    return lines


@dataclass(frozen=True)
class DynamicDefinition:
    name: str
    effects: Sequence[EffectDef]
    flow: Literal["chain", "tree"] = "chain"

    # Tree flow only. None (default) runs every child at once, as before
    # this field existed; set, it bounds the worker pool.
    max_concurrency: int | None = None

    # Tree flow only. True cancels children that have not yet started once
    # one fails; chain flow already stops at the failing child (see
    # ``execute``) so this has no effect there.
    stop_on_error: bool = False

    # Error behavior for a failure anywhere inside this dynamic that is not
    # itself absorbed by a child's own on_error: 'fail' propagates to this
    # dynamic's own parent (the default); 'skip'/'continue' record the error
    # on this dynamic's own meta and let the parent carry on, the same
    # degradation a leaf effect's on_error gives (see guidebook ch. 5).
    on_error: Literal["fail", "skip", "continue"] = "fail"

    # Free-form metadata, recorded on this dynamic's own meta. Observability
    # tagging, not behavior.
    labels: Mapping[str, Any] | None = None

    # False = skip execution (whole subtree) and write a disabled node.
    enabled: bool = True


class DynamicRuntime:
    def __init__(
        self,
        definition: DynamicDefinition,
        *,
        adapter: Adapter,
        model: str,
        model_locked: bool = False,
        runtime_config: dict[str, Any] | None = None,
        dry_run: bool = False,
        timeout_seconds: int = 120,
        verbose: bool = False,
        depth: int = 0,
        ancestors: list[AncestorContext] | None = None,
        label_prefix: str | None = None,
    ):
        self.defn = definition
        self.adapter = adapter
        self.model = model
        # See PromptRuntime: the run default was pinned by ``--model`` or a
        # profile's run-level ``model:``, so the complexity router defers to
        # it. Containers only carry the flag down to the prompts they run.
        self.model_locked = model_locked
        self.runtime_config = runtime_config or {}
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds
        self.verbose = verbose
        self.depth = depth
        self._ancestors = ancestors or []
        # Set by an enclosing ``use`` effect so this dynamic's own leaf
        # effects (prompt/tool/use) can be traced back to the invocation
        # that spawned them — see ``_child_display_name`` and
        # ``UseRuntime.execute``.
        self._label_prefix = label_prefix

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
                "labels": dict(self.defn.labels) if self.defn.labels else None,
            }
        )

        if self.defn.flow not in ("chain", "tree"):
            meta["error"] = f"Unsupported flow: {self.defn.flow}"
            meta["completed_at"] = _now_iso()
            dyn["value"] = False
            raise ValueError(meta["error"])

        # Below the unsupported-flow guard, which rejects the container
        # before it ever dispatches: a container that cannot run announces
        # neither start nor complete.
        store.fire_effect_start(self.defn.name, dyn)

        # A container parent (loop body, conditional branch, enclosing dynamic)
        # hands down the context its own children render against — iteration
        # bindings, root inputs, absolute state paths.  Layer this dynamic's
        # store namespace on top of it so the short sibling paths inside this
        # dynamic keep resolving, with local nodes winning name collisions
        # (same precedence the loop body uses for its own siblings).
        ctx = store.state if ctx_override is None else {**ctx_override, **store.state}
        child_store = store.child(self.defn.name)

        # Build ancestor context for children (this dynamic is now a parent).
        # Skip depth 0 — that's the invisible root "prime" container.
        t0 = time.monotonic()
        self._child_ancestors = list(self._ancestors)
        if self.depth > 0:
            icon, color = _EFFECT_STYLE.get(f"dynamic:{self.defn.flow}", ("⬡", "blue"))
            self._child_ancestors.append(AncestorContext(
                name=self.defn.name,
                icon=icon,
                color=color,
                start=t0,
                indent="  " * (self.depth - 1),
            ))

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
                            store.on_write(store.root_state)
            else:
                # Tree semantics: all effects run concurrently against the same
                # deterministic snapshot from dynamic start, not sibling writes.
                tree_ctx = deepcopy(ctx)

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
                        ancestors=self._child_ancestors,
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

                # Give each thread its own isolated Store so concurrent
                # effects never mutate shared dicts.  Results are merged
                # back into child_store sequentially after all futures
                # complete; observers see each branch as it runs (see
                # Store.parallel_branches).
                isolated_stores = child_store.parallel_branches(
                    len(self.defn.effects)
                )

                # An empty tree runs nothing, like an empty chain;
                # ThreadPoolExecutor itself refuses max_workers=0. Unset
                # max_concurrency keeps every child running at once, as
                # before this field existed; set, it bounds the pool.
                if self.defn.max_concurrency is None:
                    max_workers = max(1, len(self.defn.effects))
                else:
                    max_workers = max(
                        1, min(self.defn.max_concurrency, len(self.defn.effects))
                    )

                # Set by a worker that just failed, before it re-raises
                # (see _execute_branch), so the *same* worker thread checks
                # it before taking its next queued child — no race against
                # the main thread's own cancellation below, which only
                # reaches children a free worker hasn't already started.
                stop_event = (
                    threading.Event() if self.defn.stop_on_error else None
                )

                with live_ctx:
                    with ThreadPoolExecutor(max_workers=max_workers) as executor:
                        futures: dict = {
                            executor.submit(
                                self._execute_branch,
                                effect,
                                store=isolated_stores[idx],
                                ctx=tree_ctx,
                                stop_event=stop_event,
                                tracker=tree_tracker,
                            ): idx
                            for idx, effect in enumerate(self.defn.effects)
                        }
                        for future in as_completed(futures):
                            idx = futures[future]
                            try:
                                future.result()
                            except Exception as e:
                                # Name the failing child's path, same as chain
                                # flow does (#289), so a tree container's own
                                # meta.error is a breadcrumb into the child
                                # either way.
                                effect_path = self._effect_path(
                                    effect=self.defn.effects[idx], index=idx
                                )
                                wrapped = RuntimeError(f"{effect_path}: {e}")
                                wrapped.__cause__ = e
                                tree_errors.append(wrapped)
                                if self.defn.stop_on_error:
                                    # stop_event (set in _execute_branch) is
                                    # what actually keeps a freed worker from
                                    # picking up the next queued child; this
                                    # cancel() is a second line of defense
                                    # for a future the pool hasn't dequeued
                                    # at all yet. Neither can kill an
                                    # already-running thread — the executor's
                                    # own shutdown, below, still waits for
                                    # whatever was already running to finish
                                    # before the isolated stores are merged.
                                    for pending in futures:
                                        if pending is not future:
                                            pending.cancel()
                                    break

                # Merge isolated stores back into child_store sequentially
                for idx in range(len(self.defn.effects)):
                    for key, value in isolated_stores[idx].state.items():
                        child_store.state[key] = value

                if store.on_write:
                    store.on_write(store.root_state)

                if tree_errors:
                    exc = TreeExecutionError(tree_errors)
                    exc.__cause__ = tree_errors[0]
                    raise exc

            dyn["value"] = True
            meta["completed_at"] = _now_iso()
            store.fire_effect_complete(self.defn.name, dyn)

        except Exception as e:
            dyn["value"] = False
            meta["error"] = str(e)
            meta["completed_at"] = _now_iso()
            # Balances the start fired above: a container that failed still
            # closes its pair, carrying value False and meta.error.
            store.fire_effect_complete(self.defn.name, dyn)
            if self.defn.on_error == "fail":
                raise
            # skip/continue: the same degradation a leaf effect's on_error
            # gives — the failure is recorded on this dynamic's own meta.error
            # (above) and the exception stops here instead of propagating to
            # this dynamic's parent, which runs its next effect normally.
            logger.warning(
                "Dynamic %r: %s; on_error=%s, continuing with the next effect",
                self.defn.name,
                e,
                self.defn.on_error,
            )

    def _execute_branch(
        self,
        effect: EffectDef,
        *,
        store: Store,
        ctx: dict[str, Any],
        stop_event: threading.Event | None = None,
        tracker: _TreeStatus | None = None,
    ) -> None:
        """Run one tree branch, then republish its isolated store.

        The branch's writes stay in its own store until the dynamic merges
        them, so it publishes them itself — how ``--live-state`` shows a
        parallel branch that landed while its siblings still run.

        ``stop_event`` is set once ``stop_on_error`` sees a sibling fail; a
        branch that has not started yet when it checks returns without
        running at all, rather than racing the main thread's own
        cancellation of futures a free worker hasn't picked up yet.
        """
        if stop_event is not None and stop_event.is_set():
            return
        try:
            self._execute_effect(effect, store=store, ctx=ctx, tracker=tracker)
        except Exception:
            if stop_event is not None:
                stop_event.set()
            raise
        finally:
            if store.on_write:
                store.on_write(store.root_state)

    def _execute_effect(
        self,
        effect: EffectDef,
        *,
        store: Store,
        ctx: dict[str, Any],
        cb_start: Callable[[], None] | None = None,
        cb_done: Callable[[str], None] | None = None,
        cb_error: Callable[[str], None] | None = None,
        tracker: _TreeStatus | None = None,
    ) -> None:
        """Execute a single effect within the dynamic."""
        # Local imports to avoid circular imports at module load time
        from .conditional import ConditionalDefinition, ConditionalRuntime
        from .loop import LoopDefinition, LoopRuntime
        from .reflector import ReflectorDefinition, ReflectorRuntime
        from .tool import ToolDefinition, ToolRuntime
        from .use import UseDefinition, UseRuntime

        indent = "  " * self.depth
        type_label = _effect_type_label(effect)
        icon, color = _EFFECT_STYLE.get(type_label, ("·", "white"))
        name = getattr(effect, "name", None) or "?"
        is_prompt = isinstance(effect, PromptDefinition)
        is_tool = isinstance(effect, ToolDefinition)
        is_use = isinstance(effect, UseDefinition)

        # If a tracker is provided, derive all callbacks from it
        _cb_running: Callable[[str, int], None] | None = None
        if tracker is not None:
            _n = name
            def cb_start(_k=_n):
                return tracker.on_start(_k)
            def cb_done(line, _k=_n):
                return tracker.on_done(_k, line)
            def cb_error(line, _k=_n):
                return tracker.on_error(_k, line)
            def _cb_running(t, e, _k=_n):
                return tracker.on_running(_k, t, e)

        if not is_enabled(effect):
            _skip_disabled_effect(
                effect,
                store=store,
                indent=indent,
                icon=icon,
                color=color,
                verbose=self.verbose,
                cb_done=cb_done,
            )
            return

        if self.verbose and not is_prompt and not is_tool and not is_use:
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
                    model_locked=self.model_locked,
                    runtime_config=self.runtime_config,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                    verbose=self.verbose,
                    depth=self.depth,
                    cb_start=cb_start,
                    cb_done=cb_done,
                    cb_error=cb_error,
                    cb_running=_cb_running,
                    display_name=_child_display_name(name, label_prefix=self._label_prefix),
                    ancestors=self._child_ancestors if tracker is None else None,
                ).execute(store=store, ctx=ctx)

            elif isinstance(effect, DynamicDefinition):
                DynamicRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    model_locked=self.model_locked,
                    runtime_config=self.runtime_config,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                    verbose=self.verbose,
                    depth=self.depth + 1,
                    ancestors=self._child_ancestors,
                    label_prefix=self._label_prefix,
                ).execute(store=store, ctx_override=ctx)

            elif isinstance(effect, ReflectorDefinition):
                ReflectorRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    model_locked=self.model_locked,
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
                    model_locked=self.model_locked,
                    runtime_config=self.runtime_config,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                    verbose=self.verbose,
                    depth=self.depth,
                    ancestors=self._child_ancestors,
                ).execute(store=store, ctx=ctx)

            elif isinstance(effect, LoopDefinition):
                LoopRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    model_locked=self.model_locked,
                    runtime_config=self.runtime_config,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                    verbose=self.verbose,
                    depth=self.depth,
                    ancestors=self._child_ancestors,
                    label_prefix=self._label_prefix,
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
                    display_name=_child_display_name(name, label_prefix=self._label_prefix),
                    ancestors=self._child_ancestors if tracker is None else None,
                ).execute(store=store, ctx=ctx)

            elif isinstance(effect, UseDefinition):
                UseRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    model_locked=self.model_locked,
                    runtime_config=self.runtime_config,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                    verbose=self.verbose,
                    depth=self.depth,
                    cb_start=cb_start,
                    cb_done=cb_done,
                    cb_error=cb_error,
                    display_name=_child_display_name(name, label_prefix=self._label_prefix),
                    ancestors=self._child_ancestors if tracker is None else None,
                ).execute(store=store, ctx=ctx)

            else:
                raise TypeError(f"Unsupported effect type: {type(effect)}")

            if self.verbose and not is_prompt and not is_tool and not is_use:
                elapsed = time.monotonic() - t0
                suffix = _elapsed_str(elapsed)
                # A dynamic with on_error: skip/continue absorbs its own
                # failure and returns normally (no exception reaches here),
                # the same degradation a leaf effect's on_error gives — so
                # it is reported the same way a leaf effect's own failure
                # is, not as a success.
                absorbed_error = None
                if isinstance(effect, DynamicDefinition):
                    node = store.state.get(name, {})
                    sent, recv = _sum_tokens(node)
                    if sent or recv:
                        suffix += f" | ↑{_fmt_tokens(sent)} ↓{_fmt_tokens(recv)} tok"
                    if isinstance(node, dict):
                        absorbed_error = node.get("meta", {}).get("error")
                if absorbed_error is not None:
                    line = (
                        f"{indent}[err]✗[/err] [{color}]{icon}[/{color}]"
                        f" {name} [dim]{suffix}[/dim]"
                    )
                    if cb_error is not None:
                        cb_error(line)
                    else:
                        _console.print(line)
                else:
                    line = (
                        f"{indent}[ok]✓[/ok] [{color}]{icon}[/{color}]"
                        f" {name} [dim]{suffix}[/dim]"
                    )
                    if cb_done is not None:
                        cb_done(line)
                    else:
                        _console.print(line)

        except Exception:
            if self.verbose and not is_prompt and not is_tool and not is_use:
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
        # An unnamed loop/conditional has no state node of its own and
        # contributes no path segment — same convention
        # `cli.profiles.collect_orchestration_effect_paths` documents for
        # addressing them. `type(effect).__name__` (e.g. `LoopDefinition[0]`)
        # is a Python class name, not a state path, and was surfacing
        # verbatim in error messages (#269 item 12 follow-up).
        return self.defn.name


def _skip_disabled_effect(
    effect: Any,
    *,
    store: Store,
    indent: str,
    icon: str,
    color: str,
    verbose: bool,
    cb_done: Callable[[str], None] | None = None,
) -> None:
    """Write the disabled node for *effect* and report the skip.

    Shared by every container runtime (dynamic/conditional/loop) so the skip
    node shape and the verbose line stay identical wherever an effect is
    dispatched. An anonymous effect (transparent conditional/loop) has no
    state node to write, so it is simply not executed.
    """
    name = getattr(effect, "name", None)
    if isinstance(name, str) and name:
        write_disabled_node(store=store, name=name)
    if verbose:
        label = name or "?"
        line = (
            f"{indent}[dim]⊘[/dim] [{color}]{icon}[/{color}]"
            f" {label} [dim]disabled[/dim]"
        )
        if cb_done is not None:
            cb_done(line)
        else:
            _console.print(line)


def _effect_type_label(effect: Any) -> str:
    """Return a short human-readable type label for verbose output.

    For dynamic and loop effects the label includes the flow topology
    (e.g. ``"dynamic:tree"``, ``"loop:chain"``) so callers can pick
    distinct icons for parallel vs sequential execution.
    """
    # Deferred imports to avoid circular dependency at module level
    from .conditional import ConditionalDefinition
    from .loop import LoopDefinition
    from .reflector import ReflectorDefinition
    from .tool import ToolDefinition
    from .use import UseDefinition

    if isinstance(effect, PromptDefinition):
        return "prompt"
    if isinstance(effect, DynamicDefinition):
        return f"dynamic:{effect.flow}"
    if isinstance(effect, ConditionalDefinition):
        return "if"
    if isinstance(effect, LoopDefinition):
        return f"loop:{effect.flow}"
    if isinstance(effect, ReflectorDefinition):
        return "reflector"
    if isinstance(effect, ToolDefinition):
        return "tool"
    if isinstance(effect, UseDefinition):
        return "use"
    return type(effect).__name__.lower()


# (icon, rich color) per primitive type.
# Dynamic and loop have distinct icons for chain (sequential) vs tree (parallel).
_EFFECT_STYLE: dict[str, tuple[str, str]] = {
    "prompt": ("◆", "cyan"),
    "dynamic:chain": ("⬡", "blue"),
    "dynamic:tree": ("⬢", "blue"),
    "loop:chain": ("↻", "yellow"),
    "loop:tree": ("⇶", "yellow"),
    "if": ("◇", "magenta"),
    "reflector": ("✺", "green"),
    "tool": ("⚙", "white"),
    "use": ("⊕", "green"),
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


def _child_display_name(
    name: str, *, label_prefix: str | None = None, iter_label: str | None = None
) -> str | None:
    """Combine an effect's own name with an inherited ``use`` prefix and/or loop tag.

    ``label_prefix`` traces a line back to the ``use`` invocation that spawned
    it (see ``UseRuntime.execute``); ``iter_label`` is the ``[N]`` a loop body
    adds for its own iteration. Returns ``None`` when neither applies, so
    callers can pass the result straight through to ``display_name=`` and
    keep each runtime's own "no override" default of the bare effect name —
    this is what keeps unaffected call sites (no loop, no enclosing ``use``)
    byte-identical to before.
    """
    parts = [name]
    if label_prefix:
        parts.append(label_prefix)
    if iter_label:
        parts.append(iter_label)
    if len(parts) == 1:
        return None
    return " ".join(parts)


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
        ancestors: list[AncestorContext] | None = None,
    ) -> None:
        self._lock = threading.Lock()
        self._items = list(items)  # (name, icon, color)
        self._names = [name for name, _, _ in items]
        self._states: dict[str, str] = {name: "pending" for name, _, _ in items}
        self._targets: dict[str, str] = {name: "" for name, _, _ in items}
        self._estimated: dict[str, int] = {name: 0 for name, _, _ in items}
        self._item_starts: dict[str, float] = {}
        self._start = time.monotonic()
        self._indent = indent
        self._ancestors = ancestors or []

    def on_start(self, name: str) -> None:
        with self._lock:
            self._states[name] = "running"
            self._item_starts[name] = time.monotonic()

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
        now = time.monotonic()
        spinner_char = self._SPINNER[int(now * 8) % len(self._SPINNER)]
        with self._lock:
            states = dict(self._states)
            targets = dict(self._targets)
            estimated = dict(self._estimated)
            item_starts = dict(self._item_starts)

        # Render ancestor context lines above the tree items
        lines = _render_ancestors(self._ancestors, self._SPINNER)

        for name, icon, color in self._items:
            state = states.get(name, "pending")
            if state == "running":
                t = targets.get(name, "")
                e = estimated.get(name, 0)
                parts: list[str] = []
                if t:
                    parts.append(t)
                if name in item_starts:
                    parts.append(_elapsed_str(now - item_starts[name]))
                if e:
                    parts.append(f"~{e}tok ↑")
                dim_suffix = f" [dim]{' | '.join(parts)}[/dim]" if parts else ""
                lines.append(
                    f"{self._indent}[info]{spinner_char}[/info] [{color}]{icon}[/{color}] {name}{dim_suffix}"
                )
            elif state == "pending":
                lines.append(f"{self._indent}[dim]· {icon} {name}[/dim]")
            # done/error: already printed via on_done/on_error; omit from live display
        return "\n".join(lines)

