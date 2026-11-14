"""Runtime decomposition — split an over-complex prompt, land the result at its path.

Two independent triggers share this one seam, ``maybe_decompose`` below —
size first, then score (see the comment on ``maybe_decompose`` for why size
goes first) — and share one record, ``meta.decomposition``, distinguished
by its ``trigger`` key (``"score"``/``"size"``).

**Score trigger.** When ``runtime.complexity.decomposition`` is enabled and a
prompt effect's recorded complexity score strictly exceeds ``threshold``, the
runtime swaps the single over-complex model call for three bounded steps:

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

**Size trigger.** When the estimated size of the input a prompt effect would
send (``meta.prompt_sent``, the scorer's own estimator — see
:mod:`circuitry.core.split`) strictly exceeds a token budget, the runtime
replaces the single call with a deterministic fan-out instead: no planner, no
model call to produce the plan. The effect's single largest state reference
is sliced into whole units (list items, mapping entries, or text at its own
structure — :mod:`circuitry.core.split`'s job), a plan is *generated* (never
asked for) with one ``loop`` over the parts and a copy of the source prompt as
the loop body, and the parts' results are merged back — natively for
``text``/array-shaped output, through an author-supplied ``split.merge.template``
reduce prompt otherwise. The plan still runs through the very same validate/
execute path the score trigger's planner-written plan does (schema, the
allowlists, the capability ceiling, the merge contract, the isolated-child
execution with namespaced observability), and shares the one ``max_depth``
ceiling (``_decomposition_depth``) with the score trigger: a part is an
ordinary prompt at ``depth + 1``, scored and size-checked again like any
other. There is no route-up for size — a bigger model is not a bigger
context window — so ``on_failure`` is only ``run_as_is``/``fail``.
"""

from __future__ import annotations

import copy
import hashlib
import logging
import re
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any

from .effect_identity import nested_container
from .split import NoSplitPoint, split_list, split_mapping, split_text
from .split import estimate_tokens as _estimate_tokens
from .store import Store
from .use import (
    _clean_yaml_fences,
    _grafted_snapshot,
    _namespaced_effect_cb,
    _namespaced_open_instance_id_cb,
    _namespaced_progress_cb,
    _namespaced_report_cb,
    _record_children_enabled,
    _resolve_dot_path,
    _validate_inline_yaml,
)

if TYPE_CHECKING:
    from ..adapters import Adapter
    from ..cli.complexity_config import (
        ComplexitySettings,
        DecompositionSettings,
        SplittingSettings,
    )
    from .prompt import PromptDefinition, SplitDef

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
    score: float | None,
    source_template: str,
    prompt_sent: str,
    adapter: Adapter,
    model: str,
    resolved_model: str,
    runtime_config: dict[str, Any],
    timeout_seconds: int = 120,
    verbose: bool = False,
    display_depth: int = 0,
) -> DecompositionResult | None:
    """Attempt to decompose *defn* by size, then by score; ``None`` when
    neither trigger applies.

    Size runs first: a part is within budget by construction, a planned
    chunk is not, so checking size first means every leaf finally sent fits
    whatever the planner does — checking score first could leave an
    over-budget chunk at the depth ceiling, exactly the call this feature
    exists to prevent. The two triggers are otherwise independent: splitting
    needs no score at all, and a score trigger only ever gets its turn when
    size did not act (size is off, the prompt fits its budget, or no budget
    is configured for this effect/model at all).

    ``None`` means "neither trigger fired" — both switches are off, *score*
    does not strictly exceed the decomposition threshold and no size budget
    is exceeded, or the settings could not even be resolved (in which case
    the run matters more than the feature, same posture as scoring). Anything
    else comes back as a :class:`DecompositionResult` whose fields tell the
    caller exactly what to do; this function never raises.
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

    depth = _current_depth(runtime_config)

    size_result = _maybe_split(
        defn,
        store=store,
        node=node,
        ctx=ctx,
        prompt_sent=prompt_sent,
        resolved_model=resolved_model,
        settings=settings,
        depth=depth,
        adapter=adapter,
        model=model,
        runtime_config=runtime_config,
        timeout_seconds=timeout_seconds,
        verbose=verbose,
        display_depth=display_depth,
    )
    if size_result is not None:
        return size_result

    dset = settings.decomposition
    if score is None or not dset.enabled or score <= dset.threshold:
        return None

    fallback = _route_up_model(settings, defn)
    base: dict[str, Any] = {
        "decomposed": False,
        "trigger": "score",
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

    problems = _validate_plan(
        plan, max_chunks=dset.max_chunks, runtime_config=runtime_config, node_path=node_path
    )
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
    """The planner's runtime: the caller's, with both decomposition triggers
    switched off.

    The planner's own planning prompt scores off the chart by construction —
    left on, decomposition would recurse into itself before the first plan
    ever came back. Its rendered input is also enormous by construction (the
    source template plus the planning instructions), so splitting needs the
    same shield: a size-triggered split would hand the planner a plan built
    from a slice of its own instructions rather than the planner itself, and
    under ``splitting.on_failure: fail`` would turn every over-threshold
    decomposition attempt into a planner failure. Scoring and routing are
    left as configured.
    """
    out = dict(runtime_config)
    complexity = runtime_config.get("complexity")
    if isinstance(complexity, Mapping):
        out["complexity"] = {
            **complexity,
            "decomposition": {"enabled": False},
            "splitting": {"enabled": False},
        }
    return out


def _describe_inputs(ctx: Mapping[str, Any]) -> str:
    """A plain listing of the caller inputs the source template could read.

    The planner's ``source_interface`` input is free-form; these are what the
    emitted document should re-declare as its own ``interface.inputs``.

    Prefers the namespaced ``input`` root (the state contract in
    :mod:`circuitry.core.state_ns`) when present, since only that namespace
    ever holds genuine caller-supplied values — ``prime`` (other effects'
    outputs) and ``runtime`` (framework metadata: settings, persistence,
    plugins, ...) sit at the same top level and are never real inputs a
    human would supply when re-running an emitted plan standalone. Falls
    back to the legacy exclude-known-namespaces listing for a context built
    without the ``input`` wrapper (e.g. a ``Store`` constructed directly in
    a test), the same fallback :func:`extract_inputs
    <circuitry.runtime_plugins._sql_persistence.extract_inputs>` uses for a
    pre-namespace state snapshot.
    """
    from .state_ns import INPUT_NS, NAMESPACES

    input_ns = ctx.get(INPUT_NS)
    if isinstance(input_ns, Mapping):
        keys = sorted(key for key in input_ns if isinstance(key, str))
    else:
        keys = sorted(
            key
            for key in ctx
            if isinstance(key, str)
            and key not in NAMESPACES
            and not key.startswith("_")
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

    # The planner reads its inputs where every orchestration reads caller input,
    # under ``input`` ({{{input.source_template}}}, state.input.max_chunks);
    # seeded at the root, it saw an empty prompt and its gate never passed.
    with nested_container(store, defn.name):
        planner_store = _run_isolated(
            planner_orch,
            initial_state={"input": planner_state},
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


def _validate_plan(
    plan: _Plan, *, max_chunks: int, runtime_config: Mapping[str, Any], node_path: str = ""
) -> list[str]:
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
    problems.extend(
        _validate_generated_yaml(
            plan.yaml, plan.result_path, runtime_config=runtime_config, node_path=node_path
        )
    )
    return problems


def _validate_generated_yaml(
    yaml_text: str, result_path: str, *, runtime_config: Mapping[str, Any], node_path: str = ""
) -> list[str]:
    """The five checks every generated plan — planned (score trigger) or
    deterministically generated (size trigger) — must pass before it runs:
    the schema, the allowlists, the capability ceiling, the tool-provider
    ceiling (#772), and the merge contract (#672). Shared so the size
    trigger's own, never-planned plan runs through exactly the same gate as
    the score trigger's planner output.
    """
    ok, errors = _validate_inline_yaml(yaml_text)
    if not ok:
        return errors
    return [
        *_check_merge_contract(yaml_text, result_path),
        *_check_allowlists(yaml_text, runtime_config),
        *_check_capability_ceiling(yaml_text, runtime_config),
        *_check_generated_tool_ceiling(yaml_text, runtime_config, node_path=node_path),
    ]


def _check_allowlists(yaml_text: str, runtime_config: Mapping[str, Any]) -> list[str]:
    """A generated plan runs under the same allowlists as the document it serves."""
    import yaml as _yaml

    from ..allowlist_gate import allowed_adapters, allowed_tools
    from ..cli.allowlist import orchestration_denials

    try:
        parsed = _yaml.safe_load(yaml_text or "")
    except _yaml.YAMLError:
        return []  # already reported by the schema check
    if not isinstance(parsed, dict):
        return []
    return orchestration_denials(
        parsed,
        enabled_adapters=allowed_adapters(runtime_config),
        enabled_tools=allowed_tools(runtime_config),
        skip_templated=True,
    )


def _check_capability_ceiling(yaml_text: str, runtime_config: Mapping[str, Any]) -> list[str]:
    """A generated plan may only use capabilities already consented for the
    document it serves (#275 rule 4) — no ceiling installed (a path-run or
    plain library-name document) means unrestricted, unchanged from before.
    """
    import yaml as _yaml

    from ..capability_gate import capabilities_beyond_ceiling
    from ..cli.allowlist import walk_orchestration_refs
    from ..plugins.capabilities import capabilities_of

    try:
        parsed = _yaml.safe_load(yaml_text or "")
    except _yaml.YAMLError:
        return []  # already reported by the schema check
    if not isinstance(parsed, dict):
        return []
    _adapters, tools = walk_orchestration_refs(parsed, include_document_adapter=False)
    required = frozenset(cap for tool in tools for cap in capabilities_of(tool))
    beyond = capabilities_beyond_ceiling(required, runtime_config)
    if not beyond:
        return []
    return [
        "the plan needs capabilities beyond this document's consented ones: "
        + ", ".join(sorted(beyond))
    ]


def _check_generated_tool_ceiling(
    yaml_text: str, runtime_config: Mapping[str, Any], *, node_path: str = ""
) -> list[str]:
    """A decomposition plan is always generated (#772): its own
    'allowed_commands' never widen past the host config and the enclosing
    document's own literal shell effects, a 'service' effect's 'shell:
    true' is always refused, and it is held to the tool-provider ceiling
    (rule 3) like a 'use'/'reflector' generated child -- except that it has
    no generating effect of its own to carry a literal 'allow_tools:', so
    it is additionally always allowed ``DECOMPOSITION_SAFE_PROVIDERS``: the
    planner's own 'merge' step commonly needs a side-effect-free data tool
    (json, say) the enclosing document never uses itself, and none of that
    fixed set can reach anything outside the process.
    """
    import yaml as _yaml

    from ..generated_doc import (
        DECOMPOSITION_SAFE_PROVIDERS,
        GENERATED_DOCUMENT_PROVIDERS_KEY,
        current_shell_allowlist,
        generated_document_errors,
    )

    try:
        parsed = _yaml.safe_load(yaml_text or "")
    except _yaml.YAMLError:
        return []  # already reported by the schema check
    if not isinstance(parsed, dict):
        return []
    enclosing = frozenset(runtime_config.get(GENERATED_DOCUMENT_PROVIDERS_KEY) or ())
    allowed_providers = enclosing | DECOMPOSITION_SAFE_PROVIDERS
    return generated_document_errors(
        parsed,
        allowed_providers=allowed_providers,
        generating_effect_path=f"decomposition of '{node_path}'" if node_path else "",
        allowed_shell_commands=current_shell_allowlist(runtime_config),
        allow_tools_available=False,
    )


def _plan_tool_ceiling_overlay(runtime_config: Mapping[str, Any]) -> dict[str, Any]:
    """The ``runtime_config`` keys a decomposition plan's own execution
    needs for #772 rule 2's inheritance: the plan itself is generated, and
    becomes the "enclosing document" for any ``use``/``reflector`` effect
    of its own, allowed the tool providers it was just checked against in
    :func:`_check_generated_tool_ceiling` -- the enclosing document's own
    providers plus ``DECOMPOSITION_SAFE_PROVIDERS`` -- never recomputed
    from its own, model-written text. A further-nested decomposition (one
    level deeper, bounded by ``max_depth``) inherits this same ceiling
    unchanged, the same inheritance rule 2 gives a nested ``use``/
    ``reflector`` effect; the shell-allowlist key is left alone entirely,
    inheriting through the ``**runtime_config`` spread at each call site.
    """
    from ..generated_doc import (
        DECOMPOSITION_SAFE_PROVIDERS,
        GENERATED_DOCUMENT_IS_GENERATED_KEY,
        GENERATED_DOCUMENT_PROVIDERS_KEY,
    )

    allowed = (
        frozenset(runtime_config.get(GENERATED_DOCUMENT_PROVIDERS_KEY) or ())
        | DECOMPOSITION_SAFE_PROVIDERS
    )
    return {
        GENERATED_DOCUMENT_IS_GENERATED_KEY: True,
        GENERATED_DOCUMENT_PROVIDERS_KEY: sorted(allowed),
    }


def _check_merge_contract(yaml_text: str, result_path: str) -> list[str]:
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
        parsed = _yaml.safe_load(yaml_text or "")
    except _yaml.YAMLError as exc:
        return [f"YAML parse error: {exc}"]
    if not isinstance(parsed, dict):
        return ["the emitted orchestration is not a YAML mapping"]
    parts = result_path.split(".")
    if len(parts) < 2 or parts[0] != "prime":
        return [
            (
                f"result_path {result_path!r} does not point into the "
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
                f"{parts[1]!r}, so nothing writes result_path {result_path!r}"
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

    # Same guard, same identity scheme as an inline `use` child: a plan that
    # (transitively) re-emits an orchestration already executing up-stack is
    # cut off here rather than recursing. The stack is derived per call-path
    # rather than mutated in place — `runtime_config` is one dict shared by
    # every runtime in the run, and a decomposing prompt may itself run
    # inside a tree-flow loop body, so a shared, mutated list would let
    # concurrent sibling iterations see each other as ancestors.
    identity = f"inline:{hashlib.sha256(text.encode('utf-8')).hexdigest()[:16]}"
    parent_stack: list[str] = list(runtime_config.get("_use_call_stack", []))
    if identity in parent_stack:
        cycle_path = " → ".join([*parent_stack, identity])
        raise RecursionError(
            f"decomposition of '{defn.name}': cycle detected — {cycle_path}"
        )
    child_runtime_config = {
        **runtime_config,
        "_use_call_stack": [*parent_stack, identity],
        _DEPTH_KEY: depth + 1,
        **_plan_tool_ceiling_overlay(runtime_config),
    }

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

    declared_names = _declared_effect_names(parsed)
    child_store = _build_child_store(
        child_state, parent_store=store, node=node, node_path=node_path
    )
    try:
        with nested_container(store, defn.name):
            _execute_in_store(
                parsed,
                child_store,
                adapter=adapter,
                model=model,
                runtime_config=child_runtime_config,
                timeout_seconds=timeout_seconds,
                verbose=verbose,
                display_depth=display_depth,
            )
    finally:
        # Grafted on both success and failure (#672) — whatever the
        # child got to, even mid-way through a raised exception; the decomposed
        # node's own `value` stays all-or-nothing regardless (nothing below
        # this reads a partial result on the failure path, since the `raise`
        # implicit in `finally` propagates before any caller can).
        if _record_children_enabled(runtime_config):
            _graft_declared_children(node, child_store.state, declared_names)

    value = _resolve_dot_path(child_store.state, plan.result_path)
    if value is None:
        raise ValueError(
            f"the decomposition child completed but left nothing at "
            f"result_path '{plan.result_path}'"
        )
    return value


def _declared_effect_names(parsed: Mapping[str, Any]) -> list[str]:
    """The generated/planned orchestration's own top-level effect names, in
    document order — what :func:`_graft_declared_children` keeps, since the
    child's seeded state can otherwise carry whatever siblings the parent's
    own context copied in for template resolution, never actually run by
    this child dispatch.
    """
    effects = parsed.get("effects")
    if not isinstance(effects, list):
        return []
    return [
        entry["name"]
        for entry in effects
        if isinstance(entry, Mapping) and isinstance(entry.get("name"), str)
    ]


def _graft_declared_children(
    node: dict[str, Any], child_state: Mapping[str, Any], declared_names: Sequence[str]
) -> None:
    """``runtime.state.record_children`` (#672): keep the child's own
    declared effects under the decomposing node, beside ``value``/``meta``,
    in the shape live state already shows them — the same posture
    :func:`circuitry.core.use._graft_child_record` gives a ``use`` child.

    Filtered to *declared_names* rather than every key under the child's
    ``prime``: the child is seeded with a *copy* of the parent's own render
    context (so chunk/part templates can resolve the references the source
    template could), which can already hold sibling effects this dispatch
    never ran — only the generated plan's own effects belong on this node.
    """
    child_prime = child_state.get("prime") if isinstance(child_state, Mapping) else None
    if not isinstance(child_prime, dict):
        return
    for name in declared_names:
        if name in child_prime:
            node[name] = child_prime[name]


def _build_child_store(
    initial_state: dict[str, Any],
    *,
    parent_store: Store,
    node: dict[str, Any],
    node_path: str,
) -> Store:
    """An isolated child :class:`Store` that observers still see.

    Isolated state, shared observation — the same bargain a ``use`` child
    strikes: the child keeps its own state dict, but inherits the parent's
    lifecycle callbacks with paths namespaced under the decomposing effect's
    node, its ``on_write`` via a grafted whole-run snapshot, and its lock.
    Split from running the child (see :func:`_execute_in_store`) so a caller
    can still read *initial_state* — by then the child's state, partial or
    not — after a raised exception, which is what :func:`_graft_child_record`
    needs on a failed attempt.
    """
    return Store(
        state=initial_state,
        on_write=_child_on_write(parent_store, node, node_path),
        effect_start=_namespaced_effect_cb(parent_store.effect_start, node_path),
        effect_complete=_namespaced_effect_cb(
            parent_store.effect_complete, node_path
        ),
        report=_namespaced_report_cb(parent_store.report, node_path),
        progress=_namespaced_progress_cb(parent_store.progress, node_path),
        open_instance_id=_namespaced_open_instance_id_cb(
            parent_store.open_instance_id, node_path
        ),
        _lock=parent_store._lock,
    )


def _execute_in_store(
    orch: Mapping[str, Any],
    child_store: Store,
    *,
    adapter: Adapter,
    model: str,
    runtime_config: dict[str, Any],
    timeout_seconds: int,
    verbose: bool,
    display_depth: int,
) -> None:
    """Compile and run *orch* against the already-built *child_store*."""
    from .compiler import compile_orchestration
    from .dynamic import DynamicRuntime

    root = compile_orchestration(orch=dict(orch), root_name="prime")
    # A child document run with `use` sees only its own declared prompts
    # (#396) — same contract here: `root.prompts`/`root.effect_names`, just
    # compiled from THIS plan's own dict, never the parent's, so a
    # `{{> name}}` the parent happens to declare can't leak in. The planner
    # (`WIZARD_PRIME_V1`, embedded in both `wizard.yml` and `decompose.yml`)
    # IS taught `prompts:`/`{{> name}}`, so a generated plan may legitimately
    # declare and reference its own — `root.prompts` reflects whatever the
    # plan's own document actually has, rather than a hardcoded `{}`, so one
    # that is self-consistent (declares its own `prompts:` and only ever
    # references them) runs the way it compiled,
    # instead of failing at run time for a name `cof check` already accepted.
    from .prompt_compose import EFFECT_NAMES_RUNTIME_KEY as _EFFECT_NAMES_KEY
    from .prompt_compose import RUNTIME_CONFIG_KEY as _PROMPTS_KEY

    child_runtime_config = dict(runtime_config)
    child_runtime_config[_PROMPTS_KEY] = root.prompts
    child_runtime_config[_EFFECT_NAMES_KEY] = root.effect_names
    DynamicRuntime(
        root,
        adapter=adapter,
        model=model,
        runtime_config=child_runtime_config,
        dry_run=False,
        timeout_seconds=timeout_seconds,
        verbose=verbose,
        depth=display_depth + 1,
    ).execute(store=child_store)


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
    """Run *orch* in a fresh isolated store, returning it only on success —
    used by the planner run, which is never grafted onto any node
    (``record_children`` only ever applies to the *emitted plan's* child, not
    the planner call that produced it).
    """
    child_store = _build_child_store(
        initial_state, parent_store=parent_store, node=node, node_path=node_path
    )
    _execute_in_store(
        orch,
        child_store,
        adapter=adapter,
        model=model,
        runtime_config=runtime_config,
        timeout_seconds=timeout_seconds,
        verbose=verbose,
        display_depth=display_depth,
    )
    return child_store


def _child_on_write(
    parent_store: Store, node: dict[str, Any], node_path: str
) -> Any:
    parent_on_write = parent_store.on_write
    if parent_on_write is None:
        return None
    root_state = parent_store.root_state

    def _publish(child_snapshot: dict[str, Any]) -> None:
        # Deep-copied before grafting, the same protection
        # Store.parallel_branches gives a tree-flow pass's own snapshot:
        # this can fire while the generated plan's own thread is still
        # mutating its state underneath it (#634).
        parent_on_write(
            _grafted_snapshot(
                root_state, node_path, node, copy.deepcopy(child_snapshot)
            )
        )

    return _publish


# --------------------------------------------------------------------------
# size trigger (#672)
# --------------------------------------------------------------------------


class _NoSplitPointError(ValueError):
    """Nothing in the effect's render context can be sliced to fit the
    budget — the ``no_split_point`` failure reason."""


class _MergeInvalidError(ValueError):
    """The merged value failed its schema, or a part's own value was the
    wrong shape to merge at all — the ``merge_invalid`` failure reason."""


@dataclass(frozen=True)
class _SplitPrep:
    """Everything :func:`_prepare_split` worked out about *how* to split.

    *template_text*/*message_texts* are the source's own template/messages
    text, already run through ``{{> name}}`` composition expansion (#672)
    — what :func:`_generate_split_plan` rewrites the split reference's
    tags in, instead of the raw, unexpanded ``defn.template``/``defn.messages``:
    a declared prompt spliced in by the parent's own composition is spliced
    in here too, so the generated plan's child, which declares no
    ``prompts:`` of its own, never needs to resolve a ``{{> name}}`` tag at
    all. *message_texts* is ``None`` entry-for-entry where the original
    message is a dispatch-time file (nothing to expand or rewrite there;
    :func:`_generate_split_plan` re-serializes the file source instead).
    *extra_state* is the composition expansion's own resolved-effect-
    reference fragments (the synthetic ``__circuitry_partial_N__`` keys
    :func:`circuitry.core.prompt_compose._expand` mints) that the rewritten
    text's own tags still read — seeded into the child's state so they
    resolve exactly the same literal text there.
    """

    ref_path: str
    kind: str  # "list" | "mapping" | "string"
    parts: list[Any]
    sizes: list[int]
    template_text: str | None = None
    message_texts: list[str | None] | None = None
    extra_state: dict[str, str] = field(default_factory=dict)


def _resolve_split_budget(
    defn: PromptDefinition, sset: SplittingSettings, resolved_model: str
) -> int | None:
    """The token budget this effect splits against, or ``None`` when size
    does not apply at all (#672's precedence: the effect's own
    ``split.budget_tokens`` first — splitting even with the runtime switch
    off — then ``splitting.models.<resolved model>``, then
    ``splitting.max_input_tokens``, the last two read only when the runtime
    switch is on).
    """
    split_def = defn.split
    if split_def is not None and split_def.budget_tokens is not None:
        return split_def.budget_tokens
    if not sset.enabled:
        return None
    budget = sset.models.get(resolved_model)
    if budget is not None:
        return budget
    return sset.max_input_tokens


def _native_merge_mode(defn: PromptDefinition) -> str | None:
    """The merge rule by output type alone — ``None`` means the type needs
    ``split.merge.template`` (#672)."""
    if defn.prompt_type == "text":
        return "join"
    if defn.prompt_type == "array":
        return "concat"
    if (
        defn.prompt_type == "json"
        and isinstance(defn.schema, Mapping)
        and defn.schema.get("type") == "array"
    ):
        return "concat"
    return None


def _split_failure(
    base: dict[str, Any], sset: SplittingSettings, reason: str, error: str
) -> DecompositionResult:
    """Size-trigger failure posture (#672): never a route-up — there is
    no bigger context window — so ``fallback_model`` is never set at all
    (unlike the score trigger's own failure helper, :func:`_failure`).
    """
    fail = sset.on_failure == "fail"
    outcome = "failed" if fail else "run_as_is"
    return DecompositionResult(
        meta={**base, "outcome": outcome, "reason": reason, "error": error},
        failure=reason,
        error=error,
        fail=fail,
        fallback_model=None,
    )


def _maybe_split(
    defn: PromptDefinition,
    *,
    store: Store,
    node: dict[str, Any],
    ctx: Mapping[str, Any],
    prompt_sent: str,
    resolved_model: str,
    settings: ComplexitySettings,
    depth: int,
    adapter: Adapter,
    model: str,
    runtime_config: dict[str, Any],
    timeout_seconds: int,
    verbose: bool,
    display_depth: int,
) -> DecompositionResult | None:
    """The size trigger — see :func:`maybe_decompose`, which runs this first.

    ``None`` means size does not apply at all: no budget is configured for
    this effect/model (whether or not the runtime switch is on), an effect
    opted out with ``split.enabled: false``, or the estimate does not
    strictly exceed the budget. Everything else is a real attempt, which
    always returns a :class:`DecompositionResult` — this function never
    raises.
    """
    sset = settings.splitting
    split_def = defn.split

    if split_def is not None and split_def.enabled is False:
        return None

    budget = _resolve_split_budget(defn, sset, resolved_model)
    if budget is None:
        return None

    estimated = _estimate_tokens(prompt_sent)
    if estimated <= budget:
        return None

    max_parts = (
        split_def.max_parts
        if split_def is not None and split_def.max_parts is not None
        else sset.max_parts
    )

    base: dict[str, Any] = {
        "decomposed": False,
        "trigger": "size",
        "estimated_tokens": estimated,
        "budget": budget,
        "depth": depth,
        "max_depth": sset.max_depth,
    }

    # Checked before anything else — no planner here to skip calling, but a
    # ceiling hit still costs nothing beyond the budget comparison above
    # (#672).
    if depth >= sset.max_depth:
        return _split_failure(base, sset, "max_depth", "max depth reached before splitting")

    has_merge_template = bool(
        split_def is not None
        and split_def.merge is not None
        and split_def.merge.template.strip()
    )
    if has_merge_template:
        merge_mode = "reduce"
    else:
        native = _native_merge_mode(defn)
        if native is None:
            return _split_failure(
                base,
                sset,
                "no_merge",
                f"prompt_type {defn.prompt_type!r} needs split.merge.template to merge parts",
            )
        merge_mode = native
    base["merge"] = merge_mode

    try:
        prep = _prepare_split(defn, ctx=ctx, runtime_config=runtime_config, budget=budget)
    except _NoSplitPointError as exc:
        return _split_failure(base, sset, "no_split_point", str(exc))

    n_parts = len(prep.parts)
    if n_parts > max_parts:
        return _split_failure(
            base,
            sset,
            "too_many_parts",
            f"splitting produced {n_parts} parts; the budget is {max_parts}",
        )

    base["split"] = {
        "path": prep.ref_path,
        "kind": prep.kind,
        "parts": n_parts,
        "sizes": prep.sizes,
    }
    base["chunk_count"] = n_parts

    node_path = store.effect_path(defn.name)
    parts_key, plan_dict, result_path = _generate_split_plan(
        defn,
        prep=prep,
        merge_mode=merge_mode,
        split_def=split_def,
        n_parts=n_parts,
        max_concurrency=sset.max_concurrency,
        ctx=ctx,
    )
    import yaml as _yaml

    plan_yaml = _yaml.safe_dump(plan_dict, sort_keys=False)
    base["yaml"] = plan_yaml
    base["result_path"] = result_path

    problems = _validate_generated_yaml(
        plan_yaml, result_path, runtime_config=runtime_config, node_path=node_path
    )
    if problems:
        logger.warning(
            "Generated split plan for prompt %r failed validation: %s",
            defn.name,
            "; ".join(problems),
        )
        return _split_failure(base, sset, "execution_failed", "; ".join(problems))

    try:
        collected = _execute_split_plan(
            plan_dict,
            defn=defn,
            store=store,
            node=node,
            node_path=node_path,
            ctx=ctx,
            adapter=adapter,
            model=model,
            runtime_config=runtime_config,
            depth=depth,
            parts_key=parts_key,
            parts=prep.parts,
            extra_state=prep.extra_state,
            result_path=result_path,
            timeout_seconds=timeout_seconds,
            verbose=verbose,
            display_depth=display_depth,
        )
    except Exception as exc:
        logger.warning(
            "Decomposition (size trigger) child failed for prompt %r",
            defn.name,
            exc_info=True,
        )
        return _split_failure(base, sset, "execution_failed", str(exc))

    if merge_mode == "reduce":
        value = collected
    else:
        try:
            value = _apply_native_merge(
                merge_mode,
                collected,
                defn=defn,
                separator=(split_def.separator if split_def is not None else None),
            )
        except _MergeInvalidError as exc:
            return _split_failure(base, sset, "merge_invalid", str(exc))

    return DecompositionResult(
        meta={**base, "decomposed": True, "outcome": "decomposed", "reason": None},
        succeeded=True,
        value=value,
    )


# --- reference selection ---------------------------------------------------


def _template_sources(defn: PromptDefinition) -> list[tuple[str, str, str | None]]:
    """``(label, raw text, role)`` per analyzable source — the one template,
    or every ``messages[].content`` that isn't itself a dispatch-time file
    (which has no references to analyze).
    """
    if defn.template is not None:
        return [("template", defn.template, None)]
    return [
        (f"messages[{index}].content", msg.content, msg.role)
        for index, msg in enumerate(defn.messages or ())
        if msg.file is None
    ]


def _expand_sources(
    sources: Sequence[tuple[str, str, str | None]],
    ctx: Mapping[str, Any],
    runtime_config: Mapping[str, Any],
) -> tuple[list[str], dict[str, str]]:
    """Every *sources* text with ``{{> name}}`` expanded — the same expansion
    :func:`circuitry.core.prompt_compose.render_with_composition` performs
    (#672) — sharing one synthetic-key counter and one ``extra`` dict
    across every source, so a resolved-effect-reference fragment in one
    ``messages[]`` entry never mints the same key as one in a sibling entry.
    This expansion feeds both the split measurement (below) and the
    generated plan's own template/messages (:func:`_generate_split_plan`),
    so the keys it mints must resolve to the same literal text wherever they
    end up.
    """
    import itertools

    from .prompt_compose import _expand, declared_prompts, known_effect_names

    extra: dict[str, str] = {}
    counter = itertools.count()
    budget = [0]
    declared = declared_prompts(runtime_config)
    known_names = known_effect_names(runtime_config)
    expanded = [
        _expand(
            raw,
            ctx=ctx,
            declared=declared,
            known_names=known_names,
            label=label,
            seen=frozenset(),
            counter=counter,
            extra=extra,
            budget=budget,
        )
        for label, raw, _role in sources
    ]
    return expanded, extra


def _candidate_references(expanded_sources: Sequence[str]) -> list[str]:
    """Every distinct state reference (plain and section tags) across
    *expanded_sources*, in first-seen order.
    """
    from .complexity import _extract_references

    seen: set[str] = set()
    ordered: list[str] = []
    for text in expanded_sources:
        references, _sections = _extract_references(text)
        for reference in references:
            if reference not in seen:
                seen.add(reference)
                ordered.append(reference)
    return ordered


def _render_reference_alone(reference: str, ctx: Mapping[str, Any]) -> str:
    from .templates import render_template

    return render_template(
        "{{{" + reference + "}}}", ctx, label="split reference", escape=False
    )


def _tag_occurrences_and_extensions(
    expanded_sources: Sequence[str], reference: str
) -> tuple[int, list[str]]:
    """How many tags read exactly *reference*, and any tag that reaches
    further into it (``{{reference.sub}}``) — the latter is always a refusal
    (#672).
    """
    from .complexity import _TAG_RE

    count = 0
    extending: list[str] = []
    prefix = f"{reference}."
    for text in expanded_sources:
        for sigil, name in _TAG_RE.findall(text):
            if sigil in ("!", ">", "/"):
                continue
            cleaned = name.strip()
            if cleaned == reference:
                count += 1
            elif cleaned.startswith(prefix):
                extending.append(cleaned)
    return count, extending


def _file_message_fixed_tokens(defn: PromptDefinition, ctx: Mapping[str, Any]) -> int:
    """The token cost of every ``messages[].content: {file: ...}`` entry
    (#672) — excluded from ``_template_sources`` (nothing in it to
    split), but still sent on every part, so it belongs in the fixed-text
    budget alongside the template's own fixed text.
    """
    if defn.template is not None:
        return 0
    from .prompt_files import read_dispatch_file

    total = 0
    for index, msg in enumerate(defn.messages or ()):
        if msg.file is None:
            continue
        read = read_dispatch_file(
            msg.file,
            ctx,
            field=f"messages[{index}].content",
            effect_label=f"Prompt effect '{defn.name}'",
        )
        total += _estimate_tokens(f"{msg.role}: {read.text}")
    return total


def _serialize_message_file(source: Any) -> dict[str, Any]:
    """The generated map prompt's own copy of a ``content: {file: ...}``
    message (#672): the file's text was never the split reference, so
    every part still reads the same file, at its own dispatch time — the
    same :class:`~circuitry.core.prompt_files.DispatchFileSource` the source
    prompt compiled, written back out in the one shape the grammar accepts.
    """
    out: dict[str, Any] = {"file": {"from": source.path_from}}
    out["within"] = source.within if source.within is not None else {"from": source.within_from}
    return out


def _set_dotted_copy(ctx: Mapping[str, Any], path: Sequence[str], value: Any) -> dict[str, Any]:
    """A shallow-structural copy of *ctx* with *value* set at the dotted
    *path* — only the branch actually being modified is rebuilt, everything
    else stays shared by reference (cheap: the real deep copy for the child
    state happens separately, later, only once splitting is decided).
    """
    if not path:
        return dict(ctx) if isinstance(ctx, Mapping) else {}
    head, *rest = path
    new_ctx = dict(ctx) if isinstance(ctx, Mapping) else {}
    if not rest:
        new_ctx[head] = value
        return new_ctx
    current = ctx.get(head) if isinstance(ctx, Mapping) else None
    new_ctx[head] = _set_dotted_copy(current if isinstance(current, Mapping) else {}, rest, value)
    return new_ctx


def _prepare_split(
    defn: PromptDefinition,
    *,
    ctx: Mapping[str, Any],
    runtime_config: Mapping[str, Any],
    budget: int,
) -> _SplitPrep:
    """Pick the split reference, measure the room each part has, and cut its
    value into whole-unit parts — or raise :class:`_NoSplitPointError`.
    """
    from .prompt_compose import _dotted_get

    if defn.template_file is not None:
        # A dispatch-time file template has no references: the file text
        # itself is split (#672).
        from .prompt_files import read_dispatch_file

        read = read_dispatch_file(
            defn.template_file, ctx, field="template", effect_label=f"Prompt effect '{defn.name}'"
        )
        if budget < 1:
            raise _NoSplitPointError("the budget leaves no room for any part")
        file_parts: list[Any]
        try:
            file_parts = split_text(read.text, room=budget)
        except NoSplitPoint as exc:
            raise _NoSplitPointError(str(exc)) from exc
        file_sizes = [_estimate_tokens(part) for part in file_parts]
        return _SplitPrep(
            ref_path="template", kind="string", parts=file_parts, sizes=file_sizes
        )

    sources = _template_sources(defn)
    if not sources:
        raise _NoSplitPointError("the prompt has no template text to split")

    expanded_texts, extra_state = _expand_sources(sources, ctx, runtime_config)

    references = _candidate_references(expanded_texts)
    if not references:
        raise _NoSplitPointError(
            "the template has no state references to split — its fixed text alone is over budget"
        )

    best_reference: str | None = None
    best_tokens = -1
    for reference in references:
        tokens = _estimate_tokens(_render_reference_alone(reference, ctx))
        if tokens > best_tokens:
            best_tokens = tokens
            best_reference = reference
    assert best_reference is not None
    reference = best_reference

    occurrences, extending = _tag_occurrences_and_extensions(expanded_texts, reference)
    if extending:
        raise _NoSplitPointError(
            f"the template reaches into the split reference {reference!r} with a "
            f"further path: {extending[0]!r}"
        )
    if occurrences < 1:
        raise _NoSplitPointError(f"the split reference {reference!r} does not occur in the template")

    value = _dotted_get(ctx, reference.split("."))
    if isinstance(value, str):
        kind = "string"
        empty: Any = ""
    elif isinstance(value, Mapping):
        kind = "mapping"
        empty = {}
    elif isinstance(value, list):
        kind = "list"
        empty = []
    else:
        raise _NoSplitPointError(
            f"the split reference {reference!r} is not a list, mapping, or string"
        )

    from .templates import render_template

    fixed_ctx = _set_dotted_copy(ctx, reference.split("."), empty)
    rendered_fixed: list[str] = [
        render_template(text, {**fixed_ctx, **extra_state}, label=label, escape=False)
        for (label, _raw, _role), text in zip(sources, expanded_texts, strict=True)
    ]
    if defn.template is not None:
        fixed_text = rendered_fixed[0]
        template_text: str | None = expanded_texts[0]
        message_texts: list[str | None] | None = None
    else:
        fixed_text = "\n\n".join(
            f"{role}: {text}"
            for (_label, _raw, role), text in zip(sources, rendered_fixed, strict=True)
        )
        template_text = None
        expanded_iter = iter(expanded_texts)
        message_texts = [
            None if msg.file is not None else next(expanded_iter)
            for msg in (defn.messages or ())
        ]
    # A dispatch-time file message's text never varies with the split
    # reference, but it is still sent on every part, so it counts toward
    # the fixed text the budget must leave room beside (#672) — even
    # though it is excluded from `sources` above (nothing in it to split).
    fixed_tokens = _estimate_tokens(fixed_text) + _file_message_fixed_tokens(defn, ctx)

    room = (budget - fixed_tokens) // occurrences
    if room < 1:
        raise _NoSplitPointError(
            "the template's fixed text (everything except the split reference) "
            "does not leave room for a part within the budget"
        )

    parts: list[Any]
    try:
        if kind == "list":
            parts = split_list(value, room=room)
        elif kind == "mapping":
            parts = split_mapping(value, room=room)
        else:
            parts = split_text(value, room=room)
    except NoSplitPoint as exc:
        raise _NoSplitPointError(str(exc)) from exc

    sizes = [
        _estimate_tokens(str(part)) if not isinstance(part, str) else _estimate_tokens(part)
        for part in parts
    ]
    return _SplitPrep(
        ref_path=reference,
        kind=kind,
        parts=parts,
        sizes=sizes,
        template_text=template_text,
        message_texts=message_texts,
        extra_state=extra_state,
    )


# --- plan generation --------------------------------------------------------


_SIGIL_TAG_FORMATS: tuple[tuple[str, str], ...] = (
    (r"\{\{\{\s*%s\s*\}\}\}", "{{{%s}}}"),
    (r"\{\{&\s*%s\s*\}\}", "{{&%s}}"),
    (r"\{\{#\s*%s\s*\}\}", "{{#%s}}"),
    (r"\{\{\^\s*%s\s*\}\}", "{{^%s}}"),
    (r"\{\{/\s*%s\s*\}\}", "{{/%s}}"),
    (r"\{\{\s*%s\s*\}\}", "{{%s}}"),
)


def _rewrite_exact(text: str, old_path: str, new_name: str) -> str:
    """Rewrite every tag reading exactly *old_path*, every sigil
    (``{{x}}``/``{{{x}}}``/``{{&x}}``/``{{#x}}``/``{{^x}}``/``{{/x}}``), to
    *new_name* (#672). Extensions were already refused before generation
    ever runs, so none remain to worry about here.
    """
    escaped = re.escape(old_path)
    result = text
    for pattern_template, replacement_template in _SIGIL_TAG_FORMATS:
        pattern = re.compile(pattern_template % escaped)
        result = pattern.sub(replacement_template % new_name, result)
    return result


def _rewrite_prefix(text: str, old_prefix: str, new_prefix: str) -> str:
    """Rewrite every tag whose name is exactly *old_prefix*, or extends it
    with a further dotted path, replacing just the leading segment with
    *new_prefix* — every sigil, extensions kept verbatim (#672's merge-
    template rewrite is the one case an extension is rewritten, not refused).
    """
    from .complexity import _TAG_RE

    def _sub(match: re.Match[str]) -> str:
        whole = match.group(0)
        sigil = match.group(1)
        name = match.group(2).strip()
        if name == old_prefix:
            new_name = new_prefix
        elif name.startswith(f"{old_prefix}."):
            new_name = new_prefix + name[len(old_prefix) :]
        else:
            return whole
        if sigil in ("!", ">"):
            return whole
        if whole.startswith("{{{") and whole.endswith("}}}"):
            return "{{{" + new_name + "}}}"
        return "{{" + sigil + new_name + "}}"

    return _TAG_RE.sub(_sub, text)


def _pick_parts_key(ctx: Mapping[str, Any]) -> str:
    """``split_parts``, with ``_`` appended until it collides with no key of
    the parent's declared inputs (#672).
    """
    from .state_ns import INPUT_NS

    input_ns = ctx.get(INPUT_NS) if isinstance(ctx, Mapping) else None
    existing = set(input_ns.keys()) if isinstance(input_ns, Mapping) else set()
    key = "split_parts"
    while key in existing:
        key += "_"
    return key


def _pick_effect_name(base: str, ctx: Mapping[str, Any]) -> str:
    """*base* (``"parts"``/``"merge"``), with ``_`` appended until it
    collides with no key already written under the parent's own ``prime``
    namespace.

    The generated plan's loop/reduce effect is named after *base*; a source
    template that happens to read some unrelated, already-existing effect by
    that same name (never the split reference) would otherwise see the
    split machinery's own, still-in-flight node instead — silently, since
    that read renders empty rather than raising.
    """
    prime_ns = ctx.get("prime") if isinstance(ctx, Mapping) else None
    existing = set(prime_ns.keys()) if isinstance(prime_ns, Mapping) else set()
    name = base
    while name in existing:
        name += "_"
    return name


def _serialize_timeout_ms(timeout_ms: Any) -> Any:
    from .timeout_ref import TimeoutFrom

    if isinstance(timeout_ms, TimeoutFrom):
        out: dict[str, Any] = {"from": timeout_ms.path}
        if timeout_ms.default is not None:
            out["default"] = timeout_ms.default
        return out
    return timeout_ms


def _copy_source_fields(defn: PromptDefinition) -> dict[str, Any]:
    """The source prompt's own fields a generated part/merge prompt copies
    verbatim (#672) — every optional one only when the source actually
    set it.
    """
    out: dict[str, Any] = {"prompt_type": defn.prompt_type}
    if defn.schema is not None:
        out["schema"] = defn.schema
    if defn.model is not None:
        out["model"] = defn.model
    if defn.provider is not None:
        out["provider"] = defn.provider
    if defn.provider_fallbacks:
        out["provider_fallbacks"] = list(defn.provider_fallbacks)
    if defn.params is not None:
        out["params"] = defn.params
    if defn.timeout_ms is not None:
        out["timeout_ms"] = _serialize_timeout_ms(defn.timeout_ms)
    out["deterministic"] = defn.deterministic
    if defn.retries is not None:
        out["retries"] = {
            "max_attempts": defn.retries.max_attempts,
            "backoff_ms": defn.retries.backoff_ms,
        }
    if defn.inputs is not None:
        out["inputs"] = defn.inputs
    if defn.assets:
        out["assets"] = [{"kind": asset.kind, "ref": asset.ref} for asset in defn.assets]
    if defn.group is not None:
        out["group"] = defn.group
    if defn.description is not None:
        out["description"] = defn.description
    return out


def _forwarded_split_config(split_def: SplitDef | None) -> dict[str, Any] | None:
    """The source's own ``split`` block, forwarded onto both generated
    prompts (#672's hierarchical reduce): every field the source set,
    unchanged. ``merge.template`` keeps its original, unrewritten form
    (every level rewrites its own copy against its own loop's collected
    value, in :func:`_generate_split_plan` itself) — so a part that is
    itself still over budget, or a reduce whose own input is, splits again,
    bounded by the same ``max_depth`` every part already shares. ``None``
    when the source carried no ``split:`` block worth forwarding at all.
    """
    if split_def is None:
        return None
    out: dict[str, Any] = {}
    if split_def.merge is not None:
        out["merge"] = {"template": split_def.merge.template}
    if split_def.separator is not None:
        out["separator"] = split_def.separator
    if split_def.max_parts is not None:
        out["max_parts"] = split_def.max_parts
    if split_def.budget_tokens is not None:
        out["budget_tokens"] = split_def.budget_tokens
    return out or None


def _generate_split_plan(
    defn: PromptDefinition,
    *,
    prep: _SplitPrep,
    merge_mode: str,
    split_def: SplitDef | None,
    n_parts: int,
    max_concurrency: int | None,
    ctx: Mapping[str, Any],
) -> tuple[str, dict[str, Any], str]:
    """The generated plan (#672): ``(parts_key, plan dict, result_path)``."""
    ref_path = prep.ref_path
    parts_key = _pick_parts_key(ctx)
    # The loop (and, for a reduce, the merge prompt) is named after the
    # parent's own already-written effects, never the other way around
    # (#672) — a source template that reads some unrelated existing
    # effect called "parts"/"merge" (not the split reference) keeps reading
    # it rather than silently seeing this split's own in-flight node.
    loop_name = _pick_effect_name("parts", ctx)

    map_prompt: dict[str, Any] = {"type": "prompt", "name": "map"}
    map_prompt.update(_copy_source_fields(defn))
    if defn.template is not None:
        assert prep.template_text is not None
        map_prompt["template"] = _rewrite_exact(prep.template_text, ref_path, "part")
    elif defn.template_file is not None:
        map_prompt["template"] = "{{{part}}}"
    else:
        assert prep.message_texts is not None
        map_prompt["messages"] = [
            {
                "role": msg.role,
                "content": (
                    _rewrite_exact(text, ref_path, "part")
                    if msg.file is None and text is not None
                    else _serialize_message_file(msg.file)
                ),
            }
            for msg, text in zip(defn.messages or (), prep.message_texts, strict=True)
        ]
    map_prompt["on_error"] = "fail"
    forwarded_split = _forwarded_split_config(split_def)
    if forwarded_split is not None:
        map_prompt["split"] = dict(forwarded_split)

    loop_effect: dict[str, Any] = {
        "type": "loop",
        "name": loop_name,
        "flow": "tree",
        "each": {"in": f"input.{parts_key}", "as": "part"},
        "max_iterations": n_parts,
    }
    if max_concurrency is not None:
        loop_effect["max_concurrency"] = max_concurrency
    loop_effect["collect"] = "map"
    loop_effect["on_error"] = "fail"
    loop_effect["body"] = [map_prompt]

    effects: list[dict[str, Any]] = [loop_effect]
    if merge_mode == "reduce":
        if split_def is None or split_def.merge is None:
            raise AssertionError("merge_mode == 'reduce' without split.merge.template")
        merge_name = _pick_effect_name("merge", ctx)
        merge_prompt: dict[str, Any] = {"type": "prompt", "name": merge_name}
        merge_prompt.update(_copy_source_fields(defn))
        merge_prompt["template"] = _rewrite_prefix(
            split_def.merge.template, "parts", f"prime.{loop_name}.collected.value"
        )
        merge_prompt["on_error"] = "fail"
        # A hierarchical reduce (#672): the merge prompt's own input
        # (the collected parts) can itself be over budget, so it keeps the
        # same split configuration the source carried, same as the map
        # prompt above.
        if forwarded_split is not None:
            merge_prompt["split"] = dict(forwarded_split)
        effects.append(merge_prompt)
        result_path = f"prime.{merge_name}.value"
    else:
        result_path = f"prime.{loop_name}.collected.value"

    plan: dict[str, Any] = {
        "interface": {"inputs": {parts_key: {"type": "array", "required": True}}},
        "effects": effects,
    }
    return parts_key, plan, result_path


# --- execution and native merge --------------------------------------------


def _execute_split_plan(
    plan_dict: Mapping[str, Any],
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
    parts_key: str,
    parts: list[Any],
    extra_state: Mapping[str, str],
    result_path: str,
    timeout_seconds: int,
    verbose: bool,
    display_depth: int,
) -> Any:
    import yaml as _yaml

    text = _yaml.safe_dump(plan_dict, sort_keys=False)

    # Same cycle guard, same identity scheme as the score trigger's own
    # `_execute_plan` — see its comment.
    identity = f"inline:{hashlib.sha256(text.encode('utf-8')).hexdigest()[:16]}"
    parent_stack: list[str] = list(runtime_config.get("_use_call_stack", []))
    if identity in parent_stack:
        cycle_path = " → ".join([*parent_stack, identity])
        raise RecursionError(
            f"split of '{defn.name}': cycle detected — {cycle_path}"
        )
    child_runtime_config = {
        **runtime_config,
        "_use_call_stack": [*parent_stack, identity],
        _DEPTH_KEY: depth + 1,
        **_plan_tool_ceiling_overlay(runtime_config),
    }

    # A *copy* of the effect's own render context, exactly like the score
    # trigger's child — plus the parts themselves, seeded only into the
    # child's `input` namespace: the parts never appear in the plan YAML
    # itself (#672), only here, in scratch state the parent never sees.
    child_state = copy.deepcopy(
        {key: value for key, value in ctx.items() if isinstance(key, str)}
    )
    input_ns = child_state.get("input")
    if not isinstance(input_ns, dict):
        input_ns = {}
        child_state["input"] = input_ns
    input_ns[parts_key] = copy.deepcopy(parts)
    # The resolved-effect-reference fragments `{{> name}}` composition
    # expansion minted while measuring the split (#672) — the generated
    # map/merge template's own tags still read these synthetic top-level
    # keys, so they must resolve here to the same literal text they did
    # when the budget was measured.
    child_state.update(extra_state)

    declared_names = _declared_effect_names(plan_dict)
    # Every name this child's own generated plan declares is cleared from
    # the copied `prime` first (#672/R12): the loop/merge effect names
    # are chosen not to collide with the parent's own already-written
    # effects, but clearing them here too means the merged value, at
    # whichever of them `result_path` points into, can only ever be
    # something this child dispatch actually produced — never stale parent
    # state read back.
    copied_prime = child_state.get("prime")
    if isinstance(copied_prime, dict):
        for name in declared_names:
            copied_prime.pop(name, None)

    child_store = _build_child_store(
        child_state, parent_store=store, node=node, node_path=node_path
    )
    try:
        with nested_container(store, defn.name):
            _execute_in_store(
                plan_dict,
                child_store,
                adapter=adapter,
                model=model,
                runtime_config=child_runtime_config,
                timeout_seconds=timeout_seconds,
                verbose=verbose,
                display_depth=display_depth,
            )
    finally:
        if _record_children_enabled(runtime_config):
            _graft_declared_children(node, child_store.state, declared_names)

    value = _resolve_dot_path(child_store.state, result_path)
    if value is None:
        raise ValueError(
            f"the split child completed but left nothing at result_path '{result_path}'"
        )
    return value


def _apply_native_merge(
    merge_mode: str, parts_value: Any, *, defn: PromptDefinition, separator: str | None
) -> Any:
    """``join``/``concat`` (#672) — ``reduce`` never reaches here, its own
    merge prompt already wrote the final value.
    """
    if not isinstance(parts_value, list):
        raise _MergeInvalidError("the collected parts are not a list")

    if merge_mode == "join":
        texts: list[str] = []
        for item in parts_value:
            if not isinstance(item, str):
                raise _MergeInvalidError(
                    f"a part's value is {type(item).__name__}, not a string, and the "
                    "output type is text"
                )
            texts.append(item)
        return (separator if separator is not None else "\n\n").join(texts)

    if merge_mode == "concat":
        merged: list[Any] = []
        for item in parts_value:
            if not isinstance(item, list):
                raise _MergeInvalidError(
                    f"a part's value is {type(item).__name__}, not an array, and the "
                    "output type is an array"
                )
            merged.extend(item)
        if defn.schema is not None:
            _validate_merged_against_schema(merged, defn.schema)
        return merged

    raise _MergeInvalidError(f"unknown merge mode {merge_mode!r}")


def _validate_merged_against_schema(value: Any, schema: Mapping[str, Any]) -> None:
    try:
        import jsonschema  # type: ignore[import-untyped]

        jsonschema.validate(value, schema)
    except ImportError:
        pass
    except jsonschema.ValidationError as exc:
        raise _MergeInvalidError(f"the merged value failed its schema: {exc.message}") from exc
