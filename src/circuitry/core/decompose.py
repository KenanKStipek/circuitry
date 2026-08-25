"""Runtime decomposition — split an over-complex prompt, land the result at its path.

When ``runtime.complexity.decomposition`` is enabled and a prompt effect's
recorded complexity score strictly exceeds ``threshold``, the runtime swaps the
single over-complex model call for three bounded steps:

1. **Plan** — the bundled planner orchestration
   (``curation/agents/decompose.yml``) runs in an isolated store and returns a
   chunk plan plus a complete emitted orchestration. The planner run itself has
   decomposition switched off: the planner's own planning prompt is
   deliberately enormous and would otherwise trigger the very feature it
   implements.
2. **Validate** — the emitted YAML must parse, pass the orchestration schema,
   keep the fan-out within ``[2, max_chunks]``, and honor the merge contract:
   a top-level effect that actually writes the planner-reported
   ``result_path``. An invalid plan never runs.
3. **Execute** — the emitted orchestration runs as a state-isolated child
   seeded with a *copy* of the effect's render context, exactly like a ``use``
   child: same inline-identity cycle guard (``_use_call_stack``), same
   namespaced observability (child effects announce under the original
   effect's node path, live state mirrors them there). The value at
   ``result_path`` is handed back to :class:`~circuitry.core.prompt.PromptRuntime`,
   which writes it at the original effect's ``value`` — so a downstream
   ``{{prime.<name>.value}}`` reference resolves unchanged and nothing else in
   the orchestration knows the substitution happened.

Recursion is bounded by ``max_depth``, threaded through
``runtime_config["_decomposition_depth"]``: the emitted orchestration runs one
level deeper, so a chunk that itself scores above the threshold decomposes
again until the ceiling. At the ceiling the effect *routes up* — the original
prompt runs once on the routing table's catch-all (most capable) model — or
simply runs as-is when routing is off.

Failure semantics: planner failure, an invalid plan, and child execution
failure are three distinct, recorded reasons, all governed by ``on_failure``.
``route_up`` (the default) falls back the same way the depth ceiling does, so
an attempt at decomposition can never turn a working run into a failed one;
``fail`` propagates like any other effect error. On every failure path the
child's scratch state is discarded whole — nothing partial ever lands at the
original effect's path.
"""

from __future__ import annotations

import copy
import hashlib
import logging
from collections.abc import Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any

from .store import Store
from .use import (
    _clean_yaml_fences,
    _grafted_snapshot,
    _namespaced_effect_cb,
    _resolve_dot_path,
    _validate_inline_yaml,
)

if TYPE_CHECKING:
    from ..adapters import Adapter
    from ..cli.complexity_config import ComplexitySettings, DecompositionSettings
    from .prompt import PromptDefinition

logger = logging.getLogger(__name__)

#: Decomposition nesting level, threaded through ``runtime_config`` the way
#: ``_use_call_stack`` is. The emitted child runs with this incremented, so a
#: chunk that decomposes again counts against ``max_depth``.
_DEPTH_KEY = "_decomposition_depth"

#: Test/host hook: absolute path of a planner orchestration to run instead of
#: the bundled one. Same contract — the planner's ``interface.outputs`` names
#: are what the runtime reads.
_PLANNER_PATH_KEY = "_decomposition_planner_path"

_DEFAULT_RESULT_PATH = "prime.merge.value"

#: Where each planner output lives when the planner's interface does not say —
#: the bundled planner's own paths, so a trimmed-down planner still reads.
_CANONICAL_PLANNER_OUTPUTS: dict[str, str] = {
    "say": "prime.plan.value.say",
    "chunks": "prime.plan.value.chunks",
    "yaml": "prime.check.value.yaml",
    "done": "prime.done.value",
    "result_path": "prime.result_path.value",
}


class DecompositionError(RuntimeError):
    """A decomposition failure under ``on_failure: fail`` — propagated."""


@dataclass(frozen=True)
class DecompositionResult:
    """What one decomposition attempt decided, for the prompt runtime to act on.

    ``meta`` always lands at ``meta.decomposition`` on the effect node — the
    record that it was attempted, at what depth, with which plan, and how it
    ended. Exactly one of three postures follows:

    * ``succeeded`` — write ``value`` at the effect's path and stop.
    * ``fail`` — raise :class:`DecompositionError` (``on_failure: fail``).
    * neither — run the original prompt, on ``fallback_model`` when set
      (route up) or on the already-resolved model when not (run as-is).
    """

    meta: dict[str, Any] = field(default_factory=dict)
    succeeded: bool = False
    value: Any = None
    failure: str | None = None
    error: str | None = None
    fail: bool = False
    fallback_model: str | None = None


@dataclass(frozen=True)
class _Plan:
    """The planner run's outputs, read through its interface."""

    say: Any
    chunks: Any
    yaml: str | None
    done: Any
    result_path: str


def maybe_decompose(
    defn: PromptDefinition,
    *,
    store: Store,
    node: dict[str, Any],
    ctx: Mapping[str, Any],
    score: float,
    source_template: str,
    adapter: Adapter,
    model: str,
    runtime_config: dict[str, Any],
    timeout_seconds: int = 120,
    verbose: bool = False,
    display_depth: int = 0,
) -> DecompositionResult | None:
    """Attempt to decompose *defn*, or ``None`` when decomposition does not apply.

    ``None`` means "not triggered" — the switch is off, the score does not
    strictly exceed the threshold, or the settings could not even be resolved
    (in which case the run matters more than the feature, same posture as
    scoring). Anything else comes back as a :class:`DecompositionResult` whose
    fields tell the caller exactly what to do; this function never raises.
    """
    from ..cli.complexity_config import resolve_complexity_settings

    try:
        settings = resolve_complexity_settings(runtime_config)
    except Exception:
        logger.warning(
            "Could not resolve runtime.complexity; skipping decomposition for "
            "prompt %r",
            defn.name,
            exc_info=True,
        )
        return None

    dset = settings.decomposition
    if not dset.enabled or score <= dset.threshold:
        return None

    depth = _current_depth(runtime_config)
    fallback = _route_up_model(settings, defn)
    base: dict[str, Any] = {
        "decomposed": False,
        "score": score,
        "threshold": dset.threshold,
        "depth": depth,
        "max_depth": dset.max_depth,
    }

    # The ceiling is not a failure: it always routes up (or runs as-is when
    # routing is off), independent of `on_failure` — and it is checked before
    # the planner ever runs, so a depth-limited effect costs no extra calls.
    if depth >= dset.max_depth:
        return DecompositionResult(
            meta={
                **base,
                "outcome": "route_up" if fallback else "run_as_is",
                "reason": "max_depth",
                "fallback_model": fallback,
            },
            fallback_model=fallback,
        )

    node_path = store.effect_path(defn.name)

    try:
        plan = _run_planner(
            defn,
            store=store,
            node=node,
            node_path=node_path,
            ctx=ctx,
            source_template=source_template,
            adapter=adapter,
            model=model,
            runtime_config=runtime_config,
            max_chunks=dset.max_chunks,
            timeout_seconds=timeout_seconds,
            verbose=verbose,
            display_depth=display_depth,
        )
    except Exception as exc:
        logger.warning(
            "Decomposition planner failed for prompt %r", defn.name, exc_info=True
        )
        return _failure(
            base,
            dset,
            "planner_failed",
            str(exc),
            fallback,
            raw_payload=_rejected_payload_text(exc),
        )

    base["plan"] = {"say": plan.say, "chunks": plan.chunks}
    base["chunk_count"] = len(plan.chunks) if isinstance(plan.chunks, list) else None
    base["result_path"] = plan.result_path
    base["yaml"] = plan.yaml

    problems = _validate_plan(plan, max_chunks=dset.max_chunks)
    if problems:
        return _failure(base, dset, "invalid_plan", "; ".join(problems), fallback)

    try:
        value = _execute_plan(
            plan,
            defn=defn,
            store=store,
            node=node,
            node_path=node_path,
            ctx=ctx,
            adapter=adapter,
            model=model,
            runtime_config=runtime_config,
            depth=depth,
            timeout_seconds=timeout_seconds,
            verbose=verbose,
            display_depth=display_depth,
        )
    except Exception as exc:
        logger.warning(
            "Decomposition child failed for prompt %r", defn.name, exc_info=True
        )
        return _failure(base, dset, "execution_failed", str(exc), fallback)

    return DecompositionResult(
        meta={**base, "decomposed": True, "outcome": "decomposed", "reason": None},
        succeeded=True,
        value=value,
    )


# --------------------------------------------------------------------------
# trigger plumbing
# --------------------------------------------------------------------------


def _current_depth(runtime_config: Mapping[str, Any]) -> int:
    raw = runtime_config.get(_DEPTH_KEY, 0)
    try:
        return max(0, int(raw))
    except (TypeError, ValueError):
        return 0


def _route_up_model(
    settings: ComplexitySettings, defn: PromptDefinition
) -> str | None:
    """The "more capable model" to fall back to, or ``None`` to run as-is.

    With routing on, that is the catch-all band's model — the row every score
    too high for the rest of the table lands in. With routing off there is no
    table to read, so route-up degrades to running the original prompt as-is.
    An effect that pins its own ``model:`` is respected the same way the router
    respects it (``routing.respect_explicit``).
    """
    routing = settings.routing
    if not routing.enabled or not routing.bands:
        return None
    if defn.model and routing.respect_explicit:
        return None
    return routing.bands[-1].model


def _failure(
    base: dict[str, Any],
    dset: DecompositionSettings,
    reason: str,
    error: str,
    fallback: str | None,
    *,
    raw_payload: str | None = None,
) -> DecompositionResult:
    fail = dset.on_failure == "fail"
    if fail:
        outcome, fallback = "failed", None
    else:
        outcome = "route_up" if fallback else "run_as_is"
    meta = {
        **base,
        "outcome": outcome,
        "reason": reason,
        "error": error,
        "fallback_model": fallback,
    }
    if raw_payload:
        meta["raw_payload"] = raw_payload
    return DecompositionResult(
        meta=meta,
        failure=reason,
        error=error,
        fail=fail,
        fallback_model=fallback,
    )


def _rejected_payload_text(exc: BaseException) -> str | None:
    """The planner's raw response text, if *exc* traces back to a schema
    rejection rather than a truly empty failure (timeout, adapter error).

    A planner failure crosses one ``RuntimeError(f"{effect_path}: {e}") from e``
    wrapper per container it exits through on its way out of the isolated
    planner run (see :class:`~.dynamic.DynamicRuntime`); each wrapper keeps the
    original on ``__cause__``, so the chain always bottoms out at whatever
    :class:`~.prompt.PromptRuntime` actually raised.
    """
    from .prompt import SchemaValidationError

    seen: set[int] = set()
    current: BaseException | None = exc
    while current is not None and id(current) not in seen:
        seen.add(id(current))
        if isinstance(current, SchemaValidationError) and current.raw_response_text:
            return current.raw_response_text
        current = current.__cause__
    return None


# --------------------------------------------------------------------------
# plan
# --------------------------------------------------------------------------


def _planner_path(runtime_config: Mapping[str, Any]) -> Path:
    override = runtime_config.get(_PLANNER_PATH_KEY)
    if override:
        return Path(str(override))
    import importlib.resources

    return Path(
        str(
            importlib.resources.files("circuitry")
            / "curation"
            / "agents"
            / "decompose.yml"
        )
    )


def _planner_runtime_config(runtime_config: dict[str, Any]) -> dict[str, Any]:
    """The planner's runtime: the caller's, with decomposition switched off.

    The planner's own planning prompt scores off the chart by construction —
    left on, decomposition would recurse into itself before the first plan
    ever came back. Scoring and routing are left as configured.
    """
    out = dict(runtime_config)
    complexity = runtime_config.get("complexity")
    if isinstance(complexity, Mapping):
        out["complexity"] = {**complexity, "decomposition": {"enabled": False}}
    return out


def _describe_inputs(ctx: Mapping[str, Any]) -> str:
    """A plain listing of the context keys the source template could read.

    The planner's ``source_interface`` input is free-form; the top-level keys
    (minus the effect namespace and runtime-internal keys) are what the
    emitted document should re-declare as its inputs.
    """
    keys = sorted(
        key
        for key in ctx
        if isinstance(key, str) and key != "prime" and not key.startswith("_")
    )
    if not keys:
        return "(no declared inputs)"
    import yaml as _yaml

    return str(_yaml.safe_dump({"inputs": keys}, sort_keys=False))


def _planner_outputs(planner_orch: Mapping[str, Any]) -> dict[str, str]:
    """name -> state path for each planner output, interface over canonical."""
    from .outputs import normalize_outputs

    declared: dict[str, str] = {}
    interface = planner_orch.get("interface")
    if isinstance(interface, Mapping):
        try:
            declared = (
                normalize_outputs(
                    interface.get("outputs"),
                    context="decomposition planner interface.outputs",
                )
                or {}
            )
        except Exception:
            logger.warning(
                "Decomposition planner declares malformed interface.outputs; "
                "reading canonical paths instead",
                exc_info=True,
            )
    return {
        name: declared.get(name, default)
        for name, default in _CANONICAL_PLANNER_OUTPUTS.items()
    }


def _run_planner(
    defn: PromptDefinition,
    *,
    store: Store,
    node: dict[str, Any],
    node_path: str,
    ctx: Mapping[str, Any],
    source_template: str,
    adapter: Adapter,
    model: str,
    runtime_config: dict[str, Any],
    max_chunks: int,
    timeout_seconds: int,
    verbose: bool,
    display_depth: int,
) -> _Plan:
    import yaml as _yaml

    from ..cli.orchestration_loader import load_orchestration_file

    planner_orch = load_orchestration_file(_planner_path(runtime_config))

    planner_state: dict[str, Any] = {
        "source_template": source_template,
        "source_interface": _describe_inputs(ctx),
        "source_output": str(
            _yaml.safe_dump(
                {"prompt_type": defn.prompt_type, "schema": defn.schema},
                sort_keys=False,
            )
        ),
        "max_chunks": max_chunks,
    }

    planner_store = _run_isolated(
        planner_orch,
        initial_state=planner_state,
        parent_store=store,
        node=node,
        node_path=node_path,
        adapter=adapter,
        model=model,
        runtime_config=_planner_runtime_config(runtime_config),
        timeout_seconds=timeout_seconds,
        verbose=verbose,
        display_depth=display_depth,
    )

    read = {
        name: _resolve_dot_path(planner_store.state, path)
        for name, path in _planner_outputs(planner_orch).items()
    }
    raw_yaml = read.get("yaml")
    cleaned = _clean_yaml_fences(raw_yaml) if isinstance(raw_yaml, str) else None
    result_path = read.get("result_path")
    if not isinstance(result_path, str) or not result_path.strip():
        result_path = _DEFAULT_RESULT_PATH
    return _Plan(
        say=read.get("say"),
        chunks=read.get("chunks"),
        yaml=cleaned,
        done=read.get("done"),
        result_path=result_path.strip(),
    )


# --------------------------------------------------------------------------
# validate
# --------------------------------------------------------------------------


def _validate_plan(plan: _Plan, *, max_chunks: int) -> list[str]:
    """Everything wrong with the plan; an empty list is the licence to run it."""
    problems: list[str] = []
    if not isinstance(plan.yaml, str) or not plan.yaml.strip():
        problems.append("the planner returned no orchestration YAML")
        return problems
    if plan.done is False:
        problems.append(
            "the planner's own done gate is false (its output never validated "
            "within budget)"
        )
    if not isinstance(plan.chunks, list) or not plan.chunks:
        problems.append("the planner returned no chunk list")
    elif len(plan.chunks) < 2:
        problems.append(
            f"the plan has {len(plan.chunks)} chunk(s); a decomposition needs "
            "at least 2"
        )
    elif len(plan.chunks) > max_chunks:
        problems.append(
            f"the plan has {len(plan.chunks)} chunks; the budget is {max_chunks}"
        )
    ok, errors = _validate_inline_yaml(plan.yaml)
    if not ok:
        problems.extend(errors)
        return problems
    problems.extend(_check_merge_contract(plan))
    return problems


def _check_merge_contract(plan: _Plan) -> list[str]:
    """The emitted document must actually write the path the caller will read.

    The child is seeded with a copy of the parent's context, which can already
    contain a node at ``result_path`` — so without a writer for that path in
    the emitted document, "execute and read the result back" would read stale
    parent state and call it a merge. Requiring a top-level effect named after
    the path's first segment under ``prime`` closes that hole before anything
    runs (and :func:`_execute_plan` clears the copied node besides).
    """
    import yaml as _yaml

    try:
        parsed = _yaml.safe_load(plan.yaml or "")
    except _yaml.YAMLError as exc:
        return [f"YAML parse error: {exc}"]
    if not isinstance(parsed, dict):
        return ["the emitted orchestration is not a YAML mapping"]
    parts = plan.result_path.split(".")
    if len(parts) < 2 or parts[0] != "prime":
        return [
            (
                f"result_path {plan.result_path!r} does not point into the "
                "emitted orchestration's own state (expected 'prime.<effect>...')"
            )
        ]
    effects = parsed.get("effects")
    names = (
        {entry.get("name") for entry in effects if isinstance(entry, Mapping)}
        if isinstance(effects, list)
        else set()
    )
    if parts[1] not in names:
        return [
            (
                f"the emitted orchestration has no top-level effect named "
                f"{parts[1]!r}, so nothing writes result_path {plan.result_path!r}"
            )
        ]
    return []


# --------------------------------------------------------------------------
# execute
# --------------------------------------------------------------------------


def _execute_plan(
    plan: _Plan,
    *,
    defn: PromptDefinition,
    store: Store,
    node: dict[str, Any],
    node_path: str,
    ctx: Mapping[str, Any],
    adapter: Adapter,
    model: str,
    runtime_config: dict[str, Any],
    depth: int,
    timeout_seconds: int,
    verbose: bool,
    display_depth: int,
) -> Any:
    import yaml as _yaml

    text = plan.yaml or ""
    parsed = _yaml.safe_load(text)
    if not isinstance(parsed, dict):
        raise ValueError("the emitted orchestration is not a YAML mapping")

    # Same guard, same stack, same identity scheme as an inline `use` child:
    # a plan that (transitively) re-emits an orchestration already executing
    # up-stack is cut off here rather than recursing.
    identity = f"inline:{hashlib.sha256(text.encode('utf-8')).hexdigest()[:16]}"
    call_stack: list[str] = runtime_config.setdefault("_use_call_stack", [])
    if identity in call_stack:
        cycle_path = " → ".join([*call_stack, identity])
        raise RecursionError(
            f"decomposition of '{defn.name}': cycle detected — {cycle_path}"
        )
    call_stack.append(identity)
    try:
        # A *copy* of the effect's own render context — loop overlays and
        # prompt-local inputs included — so chunk templates resolve exactly
        # the references the source template could, while every child write
        # stays in scratch state the parent never sees.
        child_state = copy.deepcopy(
            {key: value for key, value in ctx.items() if isinstance(key, str)}
        )
        # Clear any copied node at the result path: the merged value must be
        # something the child wrote, never stale parent state read back.
        result_parts = plan.result_path.split(".")
        copied_prime = child_state.get("prime")
        if isinstance(copied_prime, dict) and len(result_parts) >= 2:
            copied_prime.pop(result_parts[1], None)

        child_store = _run_isolated(
            parsed,
            initial_state=child_state,
            parent_store=store,
            node=node,
            node_path=node_path,
            adapter=adapter,
            model=model,
            runtime_config={**runtime_config, _DEPTH_KEY: depth + 1},
            timeout_seconds=timeout_seconds,
            verbose=verbose,
            display_depth=display_depth,
        )
    finally:
        if call_stack and call_stack[-1] == identity:
            call_stack.pop()

    value = _resolve_dot_path(child_store.state, plan.result_path)
    if value is None:
        raise ValueError(
            f"the decomposition child completed but left nothing at "
            f"result_path '{plan.result_path}'"
        )
    return value


def _run_isolated(
    orch: Mapping[str, Any],
    *,
    initial_state: dict[str, Any],
    parent_store: Store,
    node: dict[str, Any],
    node_path: str,
    adapter: Adapter,
    model: str,
    runtime_config: dict[str, Any],
    timeout_seconds: int,
    verbose: bool,
    display_depth: int,
) -> Store:
    """Run *orch* in an isolated store that observers still see.

    Isolated state, shared observation — the same bargain a ``use`` child
    strikes: the child keeps its own state dict, but inherits the parent's
    lifecycle callbacks with paths namespaced under the decomposing effect's
    node, its ``on_write`` via a grafted whole-run snapshot, and its lock.
    """
    from .compiler import compile_orchestration
    from .dynamic import DynamicRuntime

    root = compile_orchestration(orch=dict(orch), root_name="prime")
    child_store = Store(
        state=initial_state,
        on_write=_child_on_write(parent_store, node, node_path),
        effect_start=_namespaced_effect_cb(parent_store.effect_start, node_path),
        effect_complete=_namespaced_effect_cb(
            parent_store.effect_complete, node_path
        ),
        _lock=parent_store._lock,
    )
    DynamicRuntime(
        root,
        adapter=adapter,
        model=model,
        runtime_config=runtime_config,
        dry_run=False,
        timeout_seconds=timeout_seconds,
        verbose=verbose,
        depth=display_depth + 1,
    ).execute(store=child_store)
    return child_store


def _child_on_write(
    parent_store: Store, node: dict[str, Any], node_path: str
) -> Any:
    parent_on_write = parent_store.on_write
    if parent_on_write is None:
        return None
    root_state = parent_store.root_state

    def _publish(child_snapshot: dict[str, Any]) -> None:
        parent_on_write(
            _grafted_snapshot(root_state, node_path, node, child_snapshot)
        )

    return _publish
