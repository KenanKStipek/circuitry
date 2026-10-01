from __future__ import annotations

import logging
import threading
import time
from collections.abc import Mapping, Sequence
from concurrent.futures import ThreadPoolExecutor, as_completed
from contextlib import nullcontext
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import TYPE_CHECKING, Any, Literal, Union

from ..adapters import Adapter
from ..output import console as _console
from .answers import parse_boolean_answer
from .disabled import is_disabled_node, is_enabled
from .scope import local_writes as _local_writes_state
from .scope import scope_ctx as _scope_ctx
from .store import Store
from .templates import render_template

logger = logging.getLogger(__name__)

if TYPE_CHECKING:
    from .conditional import ConditionalDefinition
    from .dynamic import DynamicDefinition
    from .prompt import PromptDefinition
    from .reflector import ReflectorDefinition
    from .tool import ToolDefinition


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


EffectDef = Union[
    "DynamicDefinition",
    "PromptDefinition",
    "ConditionalDefinition",
    "LoopDefinition",
    "ReflectorDefinition",
    "ToolDefinition",
]


@dataclass(frozen=True)
class LoopWhileDef:
    """Defines continuation condition for while loops."""

    mode: Literal["model", "cel"] = "model"
    template: str | None = None  # for mode: model
    expr: str | None = None  # for mode: cel
    #: mode: cel — raise instead of reading an unset ``state.`` path as
    #: false, so a missing field cannot quietly end (or extend) the loop.
    strict: bool = False


@dataclass(frozen=True)
class LoopEachDef:
    """Defines collection iteration for each loops."""

    in_path: str  # dot-delimited path into effective context
    as_name: str = "item"  # binding name for current element
    #: An each-loop's collection length is known before the first pass runs,
    #: so by default a collection larger than max_iterations fails the loop
    #: at start rather than silently running a truncated subset (see
    #: LoopBoundsError). Setting this opts into the old truncate-and-continue
    #: behavior, recording max_iterations_reached + unvisited on the node.
    truncate: bool = False


class LoopBoundsError(ValueError):
    """Raised when an ``each`` loop's collection outgrows ``max_iterations``.

    Unlike ``while``, an each-loop's bound is known up front — hitting the
    cap here is never a runaway, it is a caller error. Raise
    ``max_iterations``, bound the collection, or opt into
    ``each.truncate: true`` to process the first N and record the rest as
    unvisited.
    """


@dataclass(frozen=True)
class LoopDefinition:
    """
    A Loop repeats execution of a body while a condition is true or over a collection.

    Per the spec:
    - type: loop
    - while: LoopWhileDef (mode + template/expr) OR
    - each: LoopEachDef (in + as)
    - body: list of effects
    - name: optional (named loop vs transparent control)
    """

    name: str | None
    body: Sequence[EffectDef]

    # Continuation strategy (exactly one of these should be set)
    while_def: LoopWhileDef | None = None
    each_def: LoopEachDef | None = None

    # Iteration bounds. None (the default) means no cap: an each loop runs
    # every item, a while loop runs until its condition is false.
    max_iterations: int | None = None
    min_iterations: int = 0

    # Error behavior
    on_error: Literal["fail", "break", "continue"] = "fail"

    # Collection output: if set, aggregate this body effect's .value across all
    # iterations into an array written to prime.<loop_name>.collected.value
    collect: str | None = None

    # Execution topology for each-loops: "chain" = sequential (default),
    # "tree" = parallel iterations via ThreadPoolExecutor.
    # while-loops always run sequentially regardless of this setting.
    flow: Literal["chain", "tree"] = "chain"

    # Maximum parallel workers when flow="tree". None = unbounded.
    max_concurrency: int | None = None

    # False = skip execution (whole body, every iteration) and write a
    # disabled node.
    enabled: bool = True

    # Free-form metadata, recorded on this loop's own meta (named only — an
    # unnamed loop has no node to carry it). Observability tagging, not
    # behavior — see DynamicDefinition.labels.
    labels: Mapping[str, Any] | None = None


class LoopRuntime:
    """
    Executes a LoopDefinition:
      1) Determine continuation strategy (while/each)
      2) For each iteration:
         - Check continuation condition
         - Execute body effects
         - Update state
      3) Record outcomes according to recording mode
    """

    def __init__(
        self,
        definition: LoopDefinition,
        *,
        adapter: Adapter,
        model: str,
        model_locked: bool = False,
        runtime_config: dict[str, Any] | None = None,
        dry_run: bool = False,
        timeout_seconds: int = 120,
        verbose: bool = False,
        depth: int = 0,
        ancestors: list | None = None,
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
        # Set by an enclosing ``use`` effect — see ``_child_display_name``.
        self._label_prefix = label_prefix
        # Raw reply from the last `mode: model` while-condition evaluation,
        # success or failure — set inside _evaluate_model and read back in
        # execute() to record meta["answer"] alongside the parsed result.
        # Token counts ride the same path: this call bypasses PromptRuntime
        # entirely, so nowhere else ever sees what it spent. `_tokens_sent`/
        # `_tokens_received` are the last check's own count (what meta's
        # `tokens_sent`/`tokens_received` always mean); `_..._total` sum
        # every check a `while` made before it stopped — a ten-pass loop's
        # condition doesn't cost one check's worth of tokens, it costs ten.
        # Reset in execute(), not here: this same Loop instance re-executes
        # once per outer pass when it sits inside another loop's body or a
        # --state resume (see completed_indices's comment below), and a
        # prior pass's total must not bleed into this one's (#260).
        self._model_answer: str | None = None
        self._model_tokens_sent: int | None = None
        self._model_tokens_received: int | None = None
        self._model_tokens_sent_total: int | None = None
        self._model_tokens_received_total: int | None = None

    def execute(self, *, store: Store, ctx: dict[str, Any]) -> None:
        # Reset at the top of every execute() call, not just __init__: this
        # same Loop instance re-executes once per outer pass when it sits
        # inside another loop's body or a --state resume, and a prior pass's
        # while-condition token total must not bleed into this one's (#260).
        self._model_tokens_sent_total = None
        self._model_tokens_received_total = None

        # Named loop: create a node for this loop
        # Transparent control: effects merge directly into parent
        is_named = bool(self.defn.name)

        if is_named:
            assert self.defn.name is not None
            node = store.ensure_dict(self.defn.name)
            node.setdefault("value", None)
            meta = node.get("meta")
            if not isinstance(meta, dict):
                meta = {}
                node["meta"] = meta
            meta["created_at"] = _now_iso()
            if self.defn.max_iterations is not None:
                meta["max_iterations"] = self.defn.max_iterations
            meta["min_iterations"] = self.defn.min_iterations
            meta["labels"] = dict(self.defn.labels) if self.defn.labels else None
            child_store = store.child(self.defn.name)
            iterations_effects: list[dict[str, Any]] = []
            # Before the first iteration, so the loop's own start brackets
            # every start/complete pair its body produces.
            store.fire_effect_start(self.defn.name, node)
        else:
            node = None
            meta = None
            child_store = store
            iterations_effects = []

        # Keys the loop's store already carried before the first pass. An
        # unnamed (transparent) loop writes body effects straight into the
        # enclosing node, so this is what separates "the enclosing scope" from
        # "what this iteration produced" when the scope overlay is built.
        baseline = frozenset(child_store.state)

        iteration_count = 0
        # Index of the final pass that ran to completion — what `last` will
        # alias. Tracked separately from iteration_count because a pass that
        # errored under on_error: continue/break leaves a partial iter node
        # (and, in while mode, still advances the count).
        last_completed: int | None = None
        termination_reason = "max_iterations_reached"
        # Set only when an each-loop's `truncate: true` actually cut the
        # collection short — the count of elements the loop never visited.
        unvisited: int | None = None
        # Set only by the each-loop bounds check (a collection longer than
        # max_iterations under on_error: break/continue): the one "error"
        # termination with no failed pass of its own to carry the detail —
        # see LoopBoundsError and the ch.7 termination-reasons table.
        termination_detail: str | None = None
        # The previous *completed* pass's own writes (same shape as
        # _local_writes), exposed to the next pass's body as
        # prime.<loop>.prev — chain flow only (each and while), named loops
        # only (there is no spelling without the loop's own name). None
        # before the first completed pass, so the key is absent rather than
        # an empty node: a template renders empty and CEL's has() reads
        # false, the same "absent" every other unset path gets (#243).
        prev_writes: dict[str, Any] | None = None
        # Indices (pass order) of every pass that raised under on_error:
        # break/continue/fail. Excluded from `collected` regardless of
        # whether the collect target itself produced a value before the
        # pass's later failure, and surfaced on the node as
        # meta.failed_passes — see #239.
        failed_passes: list[int] = []
        # Indices (pass order) of every pass THIS execute() call actually
        # completed. Collection reads only from this list, never from the
        # iter_<N> keys left on *node* — a named loop's node is reused
        # across outer passes (it sits inside an unnamed outer loop) or
        # across a --state/persistence resume, so a prior run's iter_<N>
        # keys can still be sitting on the node when this run starts.
        # Walking those keys collected stale passes alongside this run's
        # own; this list can't, since it only ever grows during this call.
        completed_indices: list[int] = []

        # Build ancestor context for children (this loop is now a parent)
        from .dynamic import _EFFECT_STYLE as _ES
        from .dynamic import AncestorContext

        _loop_t0 = time.monotonic()
        _loop_icon, _loop_color = _ES.get(f"loop:{self.defn.flow}", ("↻", "yellow"))
        self._child_ancestors = list(self._ancestors)
        self._child_ancestors.append(AncestorContext(
            name=self.defn.name or "loop",
            icon=_loop_icon,
            color=_loop_color,
            start=_loop_t0,
            indent="  " * self.depth,
        ))

        try:
            if self.defn.each_def:
                # Collection iteration mode
                if meta:
                    meta["mode"] = "each"
                    meta["each_in_path"] = self.defn.each_def.in_path
                    meta["each_as"] = self.defn.each_def.as_name

                collection, each_error = self._resolve_collection(ctx)

                if each_error is not None:
                    # An unresolved path is not an exhausted collection: the
                    # caller pointed at nothing, and silently running zero
                    # iterations would mask the misspelled path.
                    termination_reason = "collection_unresolved"
                    if meta:
                        meta["each_in_error"] = each_error
                elif not collection:
                    termination_reason = "collection_exhausted"
                elif (
                    self.defn.max_iterations is not None
                    and len(collection) > self.defn.max_iterations
                    and not self.defn.each_def.truncate
                ):
                    bounds_error = LoopBoundsError(
                        f"each loop {self.defn.name or '<unnamed>'!r} "
                        f"({self.defn.each_def.in_path}): collection "
                        f"has {len(collection)} items but max_iterations is "
                        f"{self.defn.max_iterations} — raise max_iterations, bound "
                        f"the collection, or set each.truncate: true to process "
                        f"the first {self.defn.max_iterations} and record the "
                        f"rest as unvisited"
                    )
                    if self.defn.on_error == "fail":
                        raise bounds_error
                    # break/continue: gated the same way a pass-level failure
                    # is (see below) — the loop never starts a pass, same as
                    # collection_unresolved, but the bound violation is still
                    # recorded since there is no per-pass node to carry it.
                    # An unnamed loop has no node (meta is None) to carry
                    # that record at all, so log loudly here or the failure
                    # leaves no trace anywhere.
                    logger.warning(
                        "Loop %r: %s; on_error=%s, stopping the loop",
                        self.defn.name or "<unnamed>",
                        bounds_error,
                        self.defn.on_error,
                    )
                    termination_reason = "error"
                    termination_detail = str(bounds_error)
                    if meta:
                        meta["error"] = str(bounds_error)
                elif self.defn.flow == "tree":
                    # Parallel iteration: submit all at once, collect results in order.
                    # Each thread gets a shallow overlay of ctx, the same
                    # {**ctx, ...} the chain path below builds, not a
                    # deepcopy — cheap regardless of how large the run state
                    # or the collection is (#268). This is safe because
                    # nothing a body effect does can write back through a
                    # shared nested dict: every write lands in the thread's
                    # own isolated Store (below), never in ctx itself, and
                    # every value a body effect reads out of ctx and hands to
                    # a tool/prompt is already a fresh copy by the time it
                    # leaves this process — template rendering stringifies it
                    # (core.templates), and params_json round-trips it through
                    # json.loads. A nested dict an iteration's own item shares
                    # with a sibling (or with ctx itself) is read, never
                    # mutated in place, so sharing it by reference costs
                    # nothing. See test_loop_tree_each_shallow_overlay for the
                    # isolation this relies on.
                    capped = (
                        collection
                        if self.defn.max_iterations is None
                        else collection[: self.defn.max_iterations]
                    )
                    total = len(capped)

                    # The real ceiling on how many iterations can ever be
                    # concurrently pending — bounded by max_concurrency when
                    # set, else every iteration runs at once.
                    effective_concurrency = (
                        total
                        if self.defn.max_concurrency is None
                        else max(1, min(self.defn.max_concurrency, total))
                    )

                    # One signal, before any branch starts, naming how many
                    # can ever be concurrently pending — lets a listener
                    # (MCP's RunManager) wait for a real completion/pause
                    # count instead of guessing from a debounce window (#237).
                    store.fire_concurrent_dispatch(self.defn.name, effective_concurrency)

                    iter_ctxs: list[tuple[int, dict[str, Any]]] = []
                    for idx, item in enumerate(capped):
                        iter_ctx = {
                            **ctx,
                            self.defn.each_def.as_name: item,
                            "_loop_index": idx,
                            "iter": {"index": idx},
                        }
                        iter_ctxs.append((idx, iter_ctx))

                    # Per-thread isolated stores: each thread writes into its own
                    # state dict, so no iteration sees another's writes, while
                    # every effect inside still reports to the run's observers
                    # at its full path (see Store.parallel_branches).
                    isolated_stores = child_store.parallel_branches(total)

                    results: dict[int, dict[str, Any]] = {}
                    errors: dict[int, Exception] = {}

                    # Build animated per-iteration tracker for verbose display
                    tree_tracker: _LoopIterTracker | None = None
                    if self.verbose and total > 0 and self.defn.body:
                        from .dynamic import _EFFECT_STYLE, _effect_type_label
                        _tl = _effect_type_label(self.defn.body[0])
                        _icon, _color = _EFFECT_STYLE.get(_tl, ("◆", "cyan"))
                        _bname = getattr(self.defn.body[0], "name", None) or "?"
                        tree_tracker = _LoopIterTracker(
                            total=total,
                            name=_bname,
                            indent="  " * (self.depth + 1),
                            icon=_icon,
                            color=_color,
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

                    with live_ctx:
                        with ThreadPoolExecutor(
                            max_workers=self.defn.max_concurrency
                        ) as executor:
                            future_to_idx = {
                                executor.submit(
                                    self._execute_body,
                                    store=isolated_stores[idx],
                                    ctx=iter_ctx,
                                    iteration=idx,
                                    baseline=baseline,
                                    parallel=True,
                                    tracker=tree_tracker,
                                    iter_label=f"[{idx}]",
                                ): idx
                                for idx, iter_ctx in iter_ctxs
                            }
                            for future in as_completed(future_to_idx):
                                i = future_to_idx[future]
                                try:
                                    results[i] = future.result()[0]
                                except Exception as exc:
                                    errors[i] = exc
                                finally:
                                    # This iteration is done, whether or not
                                    # it ever registered a prompt — one fewer
                                    # settle point a listener still needs to
                                    # see (#237).
                                    store.fire_branch_settled(self.defn.name)

                    # Merge isolated stores back into child_store sequentially
                    for idx in range(total):
                        for key, value in isolated_stores[idx].state.items():
                            child_store.state[key] = value

                    # Fire on_write once after merge
                    if store.on_write:
                        store.on_write(store.root_state)

                    # Assemble results in original order
                    for idx in range(total):
                        if idx in results:
                            iterations_effects.append(results[idx])
                            iteration_count += 1
                            last_completed = idx
                            completed_indices.append(idx)

                    # Truncation status is independent of whether any
                    # iteration errored — an on_error: continue run with
                    # failures is still exhausted (or still truncated) on its
                    # own terms, not silently relabeled by the errors.
                    if total < len(collection):
                        # each.truncate: true cut the collection short.
                        termination_reason = "max_iterations_reached"
                        unvisited = len(collection) - total
                    else:
                        termination_reason = "collection_exhausted"

                    if errors:
                        failed_passes.extend(sorted(errors))
                        if self.defn.on_error == "fail":
                            termination_reason = "error"
                            raise next(iter(errors.values()))
                        if self.defn.on_error == "break":
                            termination_reason = "error"
                        # continue: keep the truncation/exhaustion reason
                        # computed above — errors don't change it.
                else:
                    # Sequential iteration (default)
                    total = len(collection)
                    for idx, item in enumerate(collection):
                        if (
                            self.defn.max_iterations is not None
                            and idx >= self.defn.max_iterations
                        ):
                            # Only reachable under each.truncate: true — the
                            # fail-fast check above already stopped the
                            # untruncated case before the first pass.
                            termination_reason = "max_iterations_reached"
                            unvisited = total - idx
                            break

                        # Bind current item to context
                        iter_ctx = dict(ctx)
                        iter_ctx[self.defn.each_def.as_name] = item
                        iter_ctx["_loop_index"] = idx
                        iter_ctx["iter"] = {"index": idx}
                        iter_ctx = self._with_prev(iter_ctx, prev_writes)

                        try:
                            iter_effects, iter_writes = self._execute_body(
                                store=child_store,
                                ctx=iter_ctx,
                                iteration=idx,
                                baseline=baseline,
                                iter_label=f"[{idx}]",
                            )
                            iterations_effects.append(iter_effects)
                            iteration_count += 1
                            last_completed = idx
                            completed_indices.append(idx)
                            prev_writes = iter_writes
                            # Publish after every completed pass, the same
                            # path the tree flow uses since #291 — otherwise
                            # --live-state and other state observers see
                            # nothing of a chain loop until it finishes (#299).
                            if store.on_write:
                                store.on_write(store.root_state)
                        except Exception:
                            failed_passes.append(idx)
                            if self.defn.on_error == "fail":
                                termination_reason = "error"
                                raise
                            if self.defn.on_error == "break":
                                termination_reason = "error"
                                break
                            # continue: skip this iteration
                    else:
                        termination_reason = "collection_exhausted"

            elif self.defn.while_def:
                # Condition-based iteration mode
                if meta:
                    meta["mode"] = self.defn.while_def.mode

                # What the pass that just finished wrote. The condition is
                # checked *between* passes, so it reads the previous
                # iteration's output under the same within-iteration names the
                # body itself uses — a grammar a body template can use but a
                # condition template cannot would send control flow wrong
                # rather than merely rendering a prompt empty.
                last_writes: dict[str, Any] = {}

                while (
                    self.defn.max_iterations is None
                    or iteration_count < self.defn.max_iterations
                ):
                    # A pass `min_iterations` already forces runs without
                    # consulting the condition at all — not evaluating it and
                    # discarding the answer, never evaluating it (#298). A
                    # `mode: cel` condition that reads state only the body
                    # itself sets would otherwise warn about an unset path on
                    # every one of these passes; a `mode: model` condition
                    # would otherwise make — and throw away — a model call.
                    if iteration_count < self.defn.min_iterations:
                        should_continue = True
                    else:
                        # Check continuation condition. A CEL expression that
                        # cannot be evaluated raises (see ``cel_eval``) rather
                        # than answering False — a broken condition used to be
                        # indistinguishable from an exhausted loop.
                        # A per-check overlay, not a mutation of ctx: a while
                        # loop must not leak its own _loop_index/iter onto the
                        # caller's dict (see #260). The values match what the
                        # old leaking mutation left behind for this same check,
                        # on purpose — the condition sees the *last finished*
                        # pass's index (-1 before the first pass), not the pass
                        # about to run, because that is the spelling every
                        # existing `+ 1`-compensated condition already assumes.
                        # `iter.count` (0 before the first pass) is the
                        # uncompensated equivalent for new conditions:
                        # `state.iter.count < N` reads the same as
                        # `state.iter.index + 1 < N`.
                        cond_ctx = {
                            **ctx,
                            "iter": {"index": iteration_count - 1, "count": iteration_count},
                        }
                        try:
                            should_continue = self._evaluate_condition(
                                ctx=_scope_ctx(cond_ctx, last_writes)
                            )
                            if meta and self.defn.while_def.mode == "model":
                                meta["answer"] = self._model_answer
                                meta["adapter"] = getattr(self.adapter, "name", "unknown")
                                meta["model"] = self.model
                                meta["tokens_sent"] = self._model_tokens_sent
                                meta["tokens_received"] = self._model_tokens_received
                                meta["tokens_sent_total"] = self._model_tokens_sent_total
                                meta["tokens_received_total"] = self._model_tokens_received_total
                        except Exception as exc:
                            if meta and self.defn.while_def.mode == "model":
                                meta["answer"] = self._model_answer
                                meta["adapter"] = getattr(self.adapter, "name", "unknown")
                                meta["model"] = self.model
                                meta["tokens_sent"] = self._model_tokens_sent
                                meta["tokens_received"] = self._model_tokens_received
                                meta["tokens_sent_total"] = self._model_tokens_sent_total
                                meta["tokens_received_total"] = self._model_tokens_received_total
                            if self.defn.on_error == "fail":
                                termination_reason = "error"
                                raise
                            # break/continue: a condition we cannot evaluate can
                            # never become false, so continuing would spin to
                            # max_iterations. Both stop the loop, loudly.
                            logger.warning(
                                "Loop %r: while-condition failed (%s); on_error=%s, "
                                "stopping the loop",
                                self.defn.name or "<unnamed>",
                                exc,
                                self.defn.on_error,
                            )
                            termination_reason = "condition_error"
                            if meta:
                                meta["error"] = str(exc)
                            break

                    if not should_continue and iteration_count >= self.defn.min_iterations:
                        termination_reason = "condition_false"
                        break

                    # Per-pass overlay, like each already builds — never a
                    # mutation of the caller's ctx (see #260): at the root
                    # ctx IS the run state, and nested inside another loop's
                    # body it is that outer pass's own dict.
                    iter_ctx = {
                        **ctx,
                        "_loop_index": iteration_count,
                        "iter": {"index": iteration_count},
                    }
                    iter_ctx = self._with_prev(iter_ctx, prev_writes)
                    try:
                        iter_effects, last_writes = self._execute_body(
                            store=child_store,
                            ctx=iter_ctx,
                            iteration=iteration_count,
                            baseline=baseline,
                            iter_label=f"[{iteration_count}]",
                        )
                        iterations_effects.append(iter_effects)
                        last_completed = iteration_count
                        completed_indices.append(iteration_count)
                        iteration_count += 1
                        prev_writes = last_writes
                        # Publish after every completed pass — see the each
                        # (chain) branch above and #299.
                        if store.on_write:
                            store.on_write(store.root_state)
                    except Exception:
                        failed_passes.append(iteration_count)
                        if self.defn.on_error == "fail":
                            termination_reason = "error"
                            raise
                        if self.defn.on_error == "break":
                            termination_reason = "error"
                            break
                        # continue: skip this iteration
                        iteration_count += 1

                if termination_reason == "max_iterations_reached":
                    # The while python-loop above exited on its own condition
                    # (iteration_count reached the cap) without any break
                    # setting a different reason — the cap stopped the loop,
                    # not the while-condition converging. Distinguishable from
                    # a converged run only here, so surface it loudly — via
                    # the console under --verbose (nicer formatting, already
                    # the user-facing channel for this flag), or `logger`
                    # otherwise so it still reaches the CLI's stderr handler;
                    # never both, or the same message doubles up on screen.
                    if self.verbose:
                        _console.print(
                            f"[warn]⚠[/warn] Loop {self.defn.name or '<unnamed>'!r} "
                            f"stopped after {iteration_count} iterations: "
                            f"max_iterations ({self.defn.max_iterations}) reached "
                            f"without the while-condition becoming false"
                        )
                    else:
                        logger.warning(
                            "Loop %r: stopped after %d iterations because "
                            "max_iterations (%d) was reached, not because the "
                            "while-condition became false",
                            self.defn.name or "<unnamed>",
                            iteration_count,
                            self.defn.max_iterations,
                        )

            if node:
                termination: dict[str, Any] = {"reason": termination_reason}
                if unvisited is not None:
                    termination["unvisited"] = unvisited
                if termination_detail is not None:
                    termination["detail"] = termination_detail
                node["value"] = {
                    "iterations": iteration_count,
                    "termination": termination,
                    "effects_by_iteration": iterations_effects,
                }
                if meta:
                    meta["completed_at"] = _now_iso()
                    if failed_passes:
                        meta["failed_passes"] = list(failed_passes)

                # collect: aggregate the named body effect's .value across all iterations
                if self.defn.collect:
                    node["collected"] = {
                        "value": self._collect_values(node, completed_indices)
                    }

                self._link_last(node, last_completed)

            if is_named and self.defn.name:
                store.fire_effect_complete(self.defn.name, node or {})

        except Exception as e:
            if meta:
                meta["error"] = str(e)
                meta["completed_at"] = _now_iso()
            if node:
                node["value"] = {
                    "iterations": iteration_count,
                    "termination": {
                        "reason": "error",
                        "detail": str(e),
                    },
                    "effects_by_iteration": iterations_effects,
                }
                if meta and failed_passes:
                    meta["failed_passes"] = list(failed_passes)
                if self.defn.collect:
                    node["collected"] = {
                        "value": self._collect_values(node, completed_indices)
                    }
                self._link_last(node, last_completed)
            if is_named and self.defn.name:
                # Balances the start fired before the first iteration — a
                # loop that blew up still closes its pair.
                store.fire_effect_complete(self.defn.name, node or {})
            raise

    def _link_last(
        self, node: dict[str, Any], last_completed: int | None
    ) -> None:
        """Expose the final *completed* pass at ``last``.

        An alias, not a copy: ``last`` and ``iter_<N>`` share the same dict,
        so ``prime.<loop>.last.<step>.value`` and every deeper field path
        resolve exactly as the iter node does. A pass that errored under
        ``on_error: continue``/``break`` is skipped in favor of the last one
        that finished, and a zero-iteration loop writes no ``last`` key at
        all — reads fall through/render empty exactly like a missing
        ``iter_<N>``. Saved state writes the alias as a reference to the
        pass, not a second copy (:mod:`circuitry.core.saved_state`).
        """
        if last_completed is None:
            return
        iter_node = node.get(f"iter_{last_completed}")
        if isinstance(iter_node, dict):
            node["last"] = iter_node

    def _collect_values(
        self, node: dict[str, Any], completed_indices: list[int]
    ) -> list[Any]:
        """Aggregate the ``collect`` target's value across every pass that
        produced one, in pass order.

        Reads *completed_indices* — the passes this ``execute()`` call
        itself completed, tracked as they happen — rather than scanning
        ``node`` for ``iter_<N>`` keys. A named loop's node is reused across
        calls (the loop sits inside an unnamed outer loop, so every outer
        pass writes to the same node; or the run was seeded with
        ``--state``/a persistence resume), so an *older* run's ``iter_<N>``
        keys can already be sitting on the node when this call starts —
        scanning the node's keys would collect those stale passes alongside
        this run's own (#239 follow-up). A failed pass is left out entirely
        — the same contract a disabled collect target already has (its node
        exists, value ``None``, ``meta.disabled``, elided here rather than
        surfacing as a run of ``None`` entries) — even if the collect target
        itself produced a value before a later body effect in that pass
        failed.
        """
        key = self.defn.collect
        if not key:
            return []
        collected: list[Any] = []
        for i in completed_indices:
            iter_node = node.get(f"iter_{i}")
            if not isinstance(iter_node, dict):
                continue
            effect_node = iter_node.get(key)
            if not isinstance(effect_node, dict):
                continue
            if is_disabled_node(effect_node):
                continue
            collected.append(effect_node.get("value"))
        return collected

    def _resolve_collection(
        self, ctx: dict[str, Any]
    ) -> tuple[list[Any], str | None]:
        """Resolve the collection path to an actual list.

        Returns ``(collection, error)``. *error* is ``None`` only when the
        path resolved to an actual list (possibly empty); otherwise it says
        why resolution failed, so the loop can terminate with
        ``collection_unresolved`` instead of masquerading as exhausted.
        """
        if not self.defn.each_def:
            return [], None

        path = self.defn.each_def.in_path

        # Navigate the path
        current: Any = ctx
        for part in path.split("."):
            if not part:
                continue
            if isinstance(current, dict):
                current = current.get(part)
            else:
                error = f"path {path!r} hit non-dict at segment {part!r}"
                logger.warning("Loop collection %s; running zero iterations", error)
                return [], error
            if current is None:
                error = f"path {path!r} resolved to None at segment {part!r}"
                logger.warning("Loop collection %s; running zero iterations", error)
                return [], error

        if isinstance(current, list):
            return current, None
        error = (
            f"path {path!r} resolved to {type(current).__name__} instead of list"
        )
        logger.warning("Loop collection %s; running zero iterations", error)
        return [], error

    def _evaluate_condition(self, *, ctx: dict[str, Any]) -> bool:
        """Evaluate the while condition and return a boolean result."""
        if not self.defn.while_def:
            return False

        if self.defn.while_def.mode == "cel":
            return self._evaluate_cel(ctx=ctx)
        return self._evaluate_model(ctx=ctx)

    def _evaluate_model(self, *, ctx: dict[str, Any]) -> bool:
        """Cybernetic evaluation: invoke model with rendered template."""
        self._model_answer = None
        self._model_tokens_sent = None
        self._model_tokens_received = None
        if self.dry_run:
            return False  # Stop loop in dry run after first iteration

        template = self.defn.while_def.template if self.defn.while_def else ""

        # Render template against context; a failure raises into the
        # caller's on_error handling rather than asking about raw text.
        rendered = render_template(template, ctx, label="while.template")

        # Invoke model to get yes/no decision
        prompt = f"""Evaluate the following condition and respond with ONLY 'yes' or 'no':

{rendered}

Should the loop continue? Answer (yes/no):"""

        res = self.adapter.generate(
            model=self.model,
            prompt=prompt,
            timeout_seconds=self.timeout_seconds,
        )
        self._model_answer = res.text
        self._model_tokens_sent = res.tokens_sent
        self._model_tokens_received = res.tokens_received
        if res.tokens_sent is not None:
            self._model_tokens_sent_total = (
                self._model_tokens_sent_total or 0
            ) + res.tokens_sent
        if res.tokens_received is not None:
            self._model_tokens_received_total = (
                self._model_tokens_received_total or 0
            ) + res.tokens_received

        # Parse response as a lenient yes/no; raises on an answer that
        # doesn't unambiguously read as one (see core.answers).
        return parse_boolean_answer(res.text or "")

    def _evaluate_cel(self, *, ctx: dict[str, Any]) -> bool:
        """Deterministic evaluation: evaluate CEL expression against state."""
        from .cel_eval import evaluate_cel

        if not self.defn.while_def:
            return False

        return evaluate_cel(
            self.defn.while_def.expr or "",
            ctx,
            strict=self.defn.while_def.strict,
        )

    def _body_names(self) -> frozenset[str]:
        """Names of this loop's own body effects."""
        return frozenset(
            name
            for name in (getattr(e, "name", None) for e in self.defn.body)
            if isinstance(name, str) and name
        )

    def _local_writes(
        self, iter_store: Store, baseline: frozenset[str]
    ) -> dict[str, Any]:
        """The nodes the current iteration has written so far.

        For a named loop the iteration store is fresh, so everything in it is
        local. For an unnamed one the body writes into the enclosing node, so
        *baseline* (the keys present when the loop started) is subtracted —
        otherwise the whole parent scope would be re-exported as if this pass
        had produced it. A body effect that shadows an enclosing name keeps
        its slot either way: inside the body, that name means the body's own
        effect.
        """
        return _local_writes_state(iter_store.state, baseline, self._body_names())

    def _with_prev(
        self, iter_ctx: dict[str, Any], prev_writes: dict[str, Any] | None
    ) -> dict[str, Any]:
        """Layer ``prime.<loop>.prev`` onto *iter_ctx* for the pass about to run.

        Chain flow only (both ``each`` and ``while`` build *iter_ctx* this
        way; tree flow never calls this — ``cof check`` rejects a
        ``prime.<loop>.prev`` reference in a tree loop's body instead, since
        tree passes run in parallel and there is no previous one). A no-op
        for an unnamed loop (nothing to spell ``prime.<loop>`` with) or
        before the first completed pass (*prev_writes* is ``None``) — the
        key stays absent rather than an empty node, so a template renders
        empty and CEL's ``has()`` reads false, same as any other unset path.

        Merges at the ``prime`` dict level, and again one level down at this
        loop's own name, rather than replacing either — ``prime.<name>``
        there is this loop's own live node (``ctx["prime"]`` is the run
        state itself at the root, mutated in place as the loop writes), so
        replacing it wholesale would drop ``iter_<N>``/``meta`` out of a
        body's view, and replacing ``prime`` wholesale would drop an outer
        loop's own already-layered ``prime.<outer>.prev`` — each loop's
        ``prev`` is its own (#243).
        """
        if not self.defn.name or prev_writes is None:
            return iter_ctx
        prime = dict(iter_ctx.get("prime") or {})
        own_node = prime.get(self.defn.name)
        own_node = dict(own_node) if isinstance(own_node, dict) else {}
        own_node["prev"] = prev_writes
        prime[self.defn.name] = own_node
        iter_ctx = dict(iter_ctx)
        iter_ctx["prime"] = prime
        return iter_ctx

    def _execute_body(
        self,
        *,
        store: Store,
        ctx: dict[str, Any],
        iteration: int,
        baseline: frozenset[str] = frozenset(),
        parallel: bool = False,
        tracker: _LoopIterTracker | None = None,
        iter_label: str | None = None,
    ) -> tuple[dict[str, Any], dict[str, Any]]:
        """Execute all effects in the loop body for one iteration.

        Returns the iteration's effect record and the nodes it wrote, the
        latter so a while-condition can be evaluated against the pass that
        just finished.
        """
        from .conditional import ConditionalDefinition, ConditionalRuntime
        from .dynamic import DynamicDefinition, DynamicRuntime, _effect_type_label
        from .prompt import PromptDefinition, PromptRuntime
        from .reflector import ReflectorDefinition, ReflectorRuntime
        from .tool import ToolDefinition, ToolRuntime
        from .use import UseDefinition, UseRuntime

        # Create iteration-specific store if named
        if self.defn.name:
            iter_key = f"iter_{iteration}"
            iter_store = store.child(iter_key)
        else:
            iter_store = store

        from .dynamic import (
            _EFFECT_STYLE,
            _child_display_name,
            _elapsed_str,
            _skip_disabled_effect,
        )

        body_indent = "  " * (self.depth + 1)
        executed: list[dict[str, Any]] = []
        # Overlays are always rebuilt from the context the iteration started
        # with, never from the previous overlay — layering copies on copies
        # would freeze whatever the enclosing scope looked like one step ago.
        base_ctx = ctx
        for effect in self.defn.body:
            effect_record = {
                "type": type(effect).__name__,
                "name": getattr(effect, "name", None),
            }
            type_label = _effect_type_label(effect)
            icon, color = _EFFECT_STYLE.get(type_label, ("·", "white"))
            name = getattr(effect, "name", None) or "?"
            is_prompt = isinstance(effect, PromptDefinition)
            is_tool = isinstance(effect, ToolDefinition)
            is_use = isinstance(effect, UseDefinition)

            if not is_enabled(effect):
                _skip_disabled_effect(
                    effect,
                    store=iter_store,
                    indent=body_indent,
                    icon=icon,
                    color=color,
                    verbose=self.verbose,
                )
                effect_record["disabled"] = True
                executed.append(effect_record)
                # Expose the skip node to later body effects on the same terms
                # as a produced one (see the sibling merge below).
                ctx = _scope_ctx(base_ctx, self._local_writes(iter_store, baseline))
                continue

            if self.verbose and not is_prompt and not is_tool and not is_use:
                _console.print(
                    f"{body_indent}[info]→[/info] [{color}]{icon}[/{color}]"
                    f" {name}"
                )

            t0 = time.monotonic()
            try:
                if is_prompt:
                    if tracker is not None:
                        def _cb_start(_i=iteration):
                            return (tracker.on_start(_i))
                        def _cb_done(line, _i=iteration):
                            return (tracker.on_done(_i, line))
                        def _cb_error(line, _i=iteration):
                            return (tracker.on_error(_i, line))
                        def _cb_running(t, e, _i=iteration):
                            return (tracker.on_running(_i, t, e))
                    elif parallel:
                        def _cb_start(_n=name, _ico=icon, _col=color, _ind=body_indent):
                            return (_console.print(
                                                        f"{_ind}[info]→[/info] [{_col}]{_ico}[/{_col}] {_n}"
                                                    ))
                        _cb_done = _console.print
                        _cb_error = _console.print
                        _cb_running = None
                    else:
                        _cb_start = None
                        _cb_done = None
                        _cb_error = None
                        _cb_running = None

                    PromptRuntime(
                        effect,
                        adapter=self.adapter,
                        model=self.model,
                        model_locked=self.model_locked,
                        runtime_config=self.runtime_config,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                        verbose=self.verbose,
                        depth=self.depth + 1,
                        cb_start=_cb_start,
                        cb_done=_cb_done,
                        cb_error=_cb_error,
                        cb_running=_cb_running,
                        display_name=_child_display_name(
                            name, label_prefix=self._label_prefix, iter_label=iter_label
                        ),
                        ancestors=self._child_ancestors if tracker is None else None,
                    ).execute(store=iter_store, ctx=ctx)

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
                        depth=self.depth + 2,
                        ancestors=self._child_ancestors,
                        label_prefix=self._label_prefix,
                    ).execute(store=iter_store, ctx_override=ctx)

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
                        depth=self.depth + 1,
                        ancestors=self._child_ancestors,
                    ).execute(store=iter_store, ctx=ctx)

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
                        depth=self.depth + 1,
                        ancestors=self._child_ancestors,
                        label_prefix=self._label_prefix,
                    ).execute(store=iter_store, ctx=ctx)

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
                    ).execute(store=iter_store)

                elif is_tool:
                    ToolRuntime(
                        effect,
                        runtime_config=self.runtime_config,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                        verbose=self.verbose,
                        depth=self.depth + 1,
                        display_name=_child_display_name(
                            name, label_prefix=self._label_prefix, iter_label=iter_label
                        ),
                        ancestors=self._child_ancestors if tracker is None else None,
                    ).execute(store=iter_store, ctx=ctx)

                elif isinstance(effect, UseDefinition):
                    use_display_name = _child_display_name(
                        name, label_prefix=self._label_prefix, iter_label=iter_label
                    )
                    if tracker is not None:
                        def _cb_start(_i=iteration):
                            return (tracker.on_start(_i))
                        def _cb_done(line, _i=iteration):
                            return (tracker.on_done(_i, line))
                        def _cb_error(line, _i=iteration):
                            return (tracker.on_error(_i, line))
                    elif parallel:
                        def _cb_start(_n=use_display_name or name, _ind=body_indent):
                            return (_console.print(
                                                        f"{_ind}[info]→[/info] [green]⊕[/green] {_n}"
                                                    ))
                        _cb_done = _console.print
                        _cb_error = _console.print
                    else:
                        _cb_start = None
                        _cb_done = None
                        _cb_error = None

                    UseRuntime(
                        effect,
                        adapter=self.adapter,
                        model=self.model,
                        model_locked=self.model_locked,
                        runtime_config=self.runtime_config,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                        verbose=self.verbose,
                        depth=self.depth + 1,
                        cb_start=_cb_start,
                        cb_done=_cb_done,
                        cb_error=_cb_error,
                        display_name=use_display_name,
                        ancestors=self._child_ancestors if tracker is None else None,
                    ).execute(store=iter_store, ctx=ctx)

                else:
                    raise TypeError(f"Unsupported effect type: {type(effect)}")

                if self.verbose and not is_prompt and not is_tool and not is_use:
                    elapsed = time.monotonic() - t0
                    _console.print(
                        f"{body_indent}[ok]✓[/ok] [{color}]{icon}[/{color}]"
                        f" {name} [dim]{_elapsed_str(elapsed)}[/dim]"
                    )

            except Exception:
                if self.verbose and not is_prompt and not is_tool and not is_use:
                    elapsed = time.monotonic() - t0
                    _console.print(
                        f"{body_indent}[err]✗[/err] [{color}]{icon}[/{color}]"
                        f" {name} [dim]{_elapsed_str(elapsed)}[/dim]"
                    )
                raise
            finally:
                # A tree iteration's writes stay in its own store until the
                # loop merges them, so it republishes after every step — how
                # --live-state shows a parallel loop's progress while it runs.
                if parallel and iter_store.on_write:
                    iter_store.on_write(iter_store.root_state)

            executed.append(effect_record)

            # Make prior body effects' outputs available to subsequent body
            # effects under the canonical within-iteration names — both
            # {{prime.<step>.value}} and the bare {{<step>.value}}.  Named and
            # unnamed loops go through the same overlay, so adding or removing
            # a loop's `name:` no longer silently changes which spelling
            # resolves.
            ctx = _scope_ctx(base_ctx, self._local_writes(iter_store, baseline))

        return (
            {
                "executed_effects": executed,
                "count": len(executed),
            },
            self._local_writes(iter_store, baseline),
        )


class _LoopIterTracker:
    """
    Tracks running state for parallel loop iterations.
    Rendered as a multi-line animated block inside a single rich.live.Live context.
    After Live exits (transient), done_lines are printed as static output.
    """

    _SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"

    def __init__(
        self,
        total: int,
        name: str,
        indent: str,
        icon: str,
        color: str,
        ancestors: list | None = None,
    ) -> None:
        self._lock = threading.Lock()
        self._total = total
        self._name = name
        self._states: list[str] = ["pending"] * total
        self._targets: list[str] = [""] * total
        self._estimated: list[int] = [0] * total
        self._item_starts: list[float | None] = [None] * total
        self._start = time.monotonic()
        self._indent = indent
        self._icon = icon
        self._color = color
        self._ancestors = ancestors or []

    def on_start(self, idx: int) -> None:
        with self._lock:
            if idx < self._total:
                self._states[idx] = "running"
                self._item_starts[idx] = time.monotonic()

    def on_running(self, idx: int, target: str, estimated_out: int) -> None:
        with self._lock:
            if idx < self._total:
                self._targets[idx] = target
                self._estimated[idx] = estimated_out

    def on_done(self, idx: int, line: str) -> None:
        with self._lock:
            if idx < self._total:
                self._states[idx] = "done"
        _console.print(line)

    def on_error(self, idx: int, line: str) -> None:
        with self._lock:
            if idx < self._total:
                self._states[idx] = "error"
        _console.print(line)

    def __rich__(self) -> str:
        from .dynamic import _elapsed_str, _render_ancestors

        now = time.monotonic()
        spinner_char = self._SPINNER[int(now * 8) % len(self._SPINNER)]
        with self._lock:
            states = list(self._states)
            targets = list(self._targets)
            estimated = list(self._estimated)
            item_starts = list(self._item_starts)
        ic = self._icon
        co = self._color

        # Render ancestor context lines above the iteration items
        lines = _render_ancestors(self._ancestors, self._SPINNER)

        for idx, state in enumerate(states):
            label = self._name if self._total == 1 else f"{self._name} [{idx}]"
            if state == "running":
                t = targets[idx]
                e = estimated[idx]
                parts: list[str] = []
                if t:
                    parts.append(t)
                if item_starts[idx] is not None:
                    parts.append(_elapsed_str(now - item_starts[idx]))
                if e:
                    parts.append(f"~{e}tok ↑")
                dim_suffix = f" [dim]{' | '.join(parts)}[/dim]" if parts else ""
                lines.append(
                    f"{self._indent}[info]{spinner_char}[/info] [{co}]{ic}[/{co}] {label}{dim_suffix}"
                )
            elif state == "pending":
                lines.append(f"{self._indent}[dim]· {ic} {label}[/dim]")
            # done/error: already printed above via on_done/on_error; omit from live display
        return "\n".join(lines)
