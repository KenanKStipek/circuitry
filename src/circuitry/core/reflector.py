"""Reflector — planning primitive that generates and executes effects at runtime.

Internally delegates to use(inline) for YAML validation, compilation, and
execution of LLM-generated orchestrations. The reflector is syntax sugar that
adds: prime directive rendering, iteration control (done flag), and automatic
wiring between the inner planning prompt and the generated orchestration.
"""

from __future__ import annotations

import copy
import json
import logging
from dataclasses import dataclass, replace
from datetime import datetime, timezone
from typing import TYPE_CHECKING, Any

import yaml as _yaml  # type: ignore[import-untyped]

from ..adapters import Adapter
from ..cli.redaction import redact
from .primes import REFLECTOR_PRIME_V1
from .store import Store
from .templates import render_template
from .use import UseDefinition, UseRuntime, _clean_yaml_fences

#: {context} is a summary, not a state dump — capped well under a model's
#: context window so a run with a large plugin/runtime config never crowds
#: out the actual planning instructions.
_CONTEXT_MAX_CHARS = 2000

logger = logging.getLogger(__name__)

if TYPE_CHECKING:
    from .dynamic import DynamicDefinition


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _store_root(store: Store) -> dict[str, Any]:
    for attr in ("data", "state", "root", "_data", "_state", "_root"):
        v = getattr(store, attr, None)
        if isinstance(v, dict):
            return v
    if isinstance(store, dict):
        return store  # type: ignore[return-value]
    raise AttributeError("Store does not expose its underlying dict.")


def _run_root_state(store: Store) -> dict[str, Any]:
    """The whole-run state dict, regardless of how deep *store* is nested.

    A reflector anywhere in the tree receives a ``Store`` already scoped to
    its container (``prime`` at top level, deeper under a loop/dynamic/
    conditional, or isolated inside a ``flow: tree`` dynamic or parallel
    loop branch) — its own ``.state`` is never the run root. ``goal`` and
    ``context`` read from ``input``/``prime``/``runtime`` at the true root,
    so they go through ``Store.true_root_state`` (which survives tree/
    parallel branch isolation, unlike ``Store.root_state``) instead of
    :func:`_store_root`.
    """
    true_root_state = getattr(store, "true_root_state", None)
    if isinstance(true_root_state, dict):
        return true_root_state
    root_state = getattr(store, "root_state", None)
    if isinstance(root_state, dict):
        return root_state
    return _store_root(store)


@dataclass(frozen=True)
class ReflectorDefinition:
    name: str
    inner: DynamicDefinition

    plan_from_step: str = "propose_steps"

    # v1 loop control
    max_iterations: int = 1

    # where generated effects are executed under the reflector node
    generated_key: str = "generated"

    stop_on_done: bool = True

    # Prime directive support
    prime_template: str = REFLECTOR_PRIME_V1
    max_effects: int = 8

    # False = skip planning and generated execution entirely, writing a
    # disabled node ("turn agentic planning off for this run").
    enabled: bool = True

    # Generated-document ceiling (#772): tool providers a generated plan may
    # use beyond the enclosing document's own static providers. Literal only
    # -- compiler._compile_allow_tools rejects a rendered value, params_json
    # or state at compile time.
    allow_tools: frozenset[str] = frozenset()


class ReflectorRuntime:
    def __init__(
        self,
        definition: ReflectorDefinition,
        *,
        adapter: Adapter,
        model: str,
        model_locked: bool = False,
        runtime_config: dict[str, Any] | None = None,
        dry_run: bool = False,
        timeout_seconds: int = 120,
        verbose: bool = False,
        # See DynamicRuntime: gates the loop-progress line, forwarded the
        # same way ``verbose`` is to the inner and generated runs.
        progress_display: bool = False,
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
        self.progress_display = progress_display

    def execute(self, *, store: Store) -> None:
        node = store.ensure_dict(self.defn.name)
        node.setdefault("value", None)
        meta = _ensure_dict(node, "meta")
        meta.setdefault("iterations", [])
        node.setdefault(self.defn.generated_key, {})
        # A rerun (resumed after an earlier failed attempt, or any other
        # reason this reflector runs again) starts with a clean slate:
        # `effect_completed_ok` cares about *this* attempt's own outcome,
        # not a stale `meta.error` a prior one left behind -- left
        # untouched, a reflector that failed, then succeeded on resume,
        # would still read as failed on every later resume.
        meta["error"] = None

        reflector_store = store.child(self.defn.name)

        # Before planning, so the reflector's own start brackets every
        # start/complete pair its inner dynamic and generated effects produce.
        store.fire_effect_start(self.defn.name, node)
        try:
            iterations = max(1, int(self.defn.max_iterations or 1))
            for i in range(iterations):
                rec = self._run_iteration(
                    i=i, store=store, reflector_store=reflector_store
                )
                meta["iterations"].append(rec)

                if rec.get("error"):
                    node["value"] = False
                    # Lifecycle observers judge a completion failed by
                    # ``meta.error``; the iteration record alone is invisible
                    # to them.
                    meta["error"] = rec["error"]
                    meta["completed_at"] = _now_iso()
                    raise RuntimeError(rec["error"])

                if rec.get("stop", False):
                    node["value"] = True
                    meta["completed_at"] = _now_iso()
                    return

            node["value"] = True
            meta["completed_at"] = _now_iso()
        except Exception as e:
            node["value"] = False
            meta.setdefault("error", str(e))
            meta.setdefault("completed_at", _now_iso())
            raise
        finally:
            # Balances the start on every exit, a failed iteration included.
            store.fire_effect_complete(self.defn.name, node)

    def _run_iteration(
        self, *, i: int, store: Store, reflector_store: Store
    ) -> dict[str, Any]:
        """One reflector iteration: prime → plan → validate → execute via use(inline)."""
        from .dynamic import DynamicRuntime

        rec: dict[str, Any] = {
            "i": i,
            "plan_from_step": self.defn.plan_from_step,
            "done": False,
            "stop": False,
            "parsed": None,
            "error": None,
            "plan_text": "",
        }

        # ── Phase 1: Render prime directive ──────────────────────────────
        prime_text = _render_reflector_prime(
            prime_template=self.defn.prime_template,
            max_effects=self.defn.max_effects,
            goal=_best_effort_goal(store),
            context=_best_effort_context(store),
        )

        # ��─ Phase 2: Execute inner dynamic (with prime prepended) ────────
        inner = _prepend_prime_to_plan_prompt(
            inner=self.defn.inner,
            plan_from_step=self.defn.plan_from_step,
            prime_text=prime_text,
        )

        try:
            DynamicRuntime(
                inner,
                adapter=self.adapter,
                model=self.model,
                model_locked=self.model_locked,
                runtime_config=self.runtime_config,
                dry_run=self.dry_run,
                timeout_seconds=self.timeout_seconds,
                verbose=self.verbose,
                progress_display=self.progress_display,
            ).execute(store=reflector_store)
        except Exception as e:
            rec["error"] = f"inner_failed: {e}"
            return rec

        # ── Phase 3: Extract plan text from inner step output ────────────
        plan_text = _get_plan_text(
            node=_store_root(reflector_store), plan_from_step=self.defn.plan_from_step
        )
        rec["plan_text"] = plan_text

        if self.dry_run:
            rec["done"] = True
            rec["stop"] = True
            return rec

        if self.verbose:
            rec["raw_plan_text"] = plan_text

        # ── Phase 4: Extract done flag and check stop conditions ─────────
        try:
            done, parsed_plan = _extract_done_flag(plan_text)
            rec["done"] = done

            if parsed_plan is None:
                rec["stop"] = True
                return rec

            # A plan that stops the reflector here (done, with
            # stop_on_done) is never executed via use(inline), so the cap
            # below — which only matters for a plan that will actually run
            # — does not apply to it; it still gets parsed so rec["parsed"]
            # is recorded the same way for every plan.
            stopping = done and self.defn.stop_on_done

            # Check for empty effects list — nothing to execute
            effects = (
                parsed_plan.get("effects", []) if isinstance(parsed_plan, dict) else []
            )
            rec["parsed"] = parsed_plan
            if not effects:
                rec["stop"] = True
                return rec
            # A plan over the cap is invalid, same as one that fails to
            # parse — never truncated silently (#251 part 2).
            if (
                not stopping
                and isinstance(effects, list)
                and len(effects) > self.defn.max_effects
            ):
                rec["error"] = (
                    f"invalid_plan: generated plan has {len(effects)} "
                    f"top-level effects, exceeds max_effects "
                    f"({self.defn.max_effects})"
                )
                return rec

            if stopping:
                rec["stop"] = True
                return rec

        except Exception as e:
            rec["error"] = f"parse_failed: {e}"
            return rec

        # ── Phase 5: Validate + compile + execute via use(inline) ────────
        iteration_key = f"iter_{i}"

        try:
            use_defn = UseDefinition(
                name=iteration_key,
                # The parsed, normalized document handed straight to the
                # child -- never dumped to YAML and reparsed (#778). A deep
                # copy: `rec["parsed"]` above and whatever the child's own
                # compile step does with this dict stay independent.
                inline_document=copy.deepcopy(parsed_plan),
                validate=True,
                on_error="fail",
                allow_tools=self.defn.allow_tools,
                # A reflector's plan has no author template of its own to
                # compare against -- it is unconditionally generated (#772).
                force_generated=True,
                # Named in a refusal as the effect to add allow_tools to --
                # the reflector's own path, not the synthetic use's.
                generating_effect_path_override=store.effect_path(self.defn.name),
            )

            gen_parent = reflector_store.child(self.defn.generated_key)
            gen_store = gen_parent

            UseRuntime(
                use_defn,
                adapter=self.adapter,
                model=self.model,
                model_locked=self.model_locked,
                runtime_config=self.runtime_config,
                dry_run=self.dry_run,
                timeout_seconds=self.timeout_seconds,
                verbose=self.verbose,
                progress_display=self.progress_display,
            ).execute(store=gen_store, ctx=_store_root(store))

        except Exception as e:
            rec["error"] = f"generated_exec_failed: {e}"
            return rec

        return rec


# ── Helpers ──────────────────────────────────────────────────────────────────


def _ensure_dict(parent: dict[str, Any], key: str) -> dict[str, Any]:
    node = parent.get(key)
    if not isinstance(node, dict):
        node = {}
        parent[key] = node
    return node


def _render_reflector_prime(
    *, prime_template: str, max_effects: int, goal: str, context: str
) -> str:
    """Render *prime_template* with Mustache, seeing exactly ``goal``,
    ``context`` and ``max_effects`` -- the same rule as any other prompt
    template (no HTML escaping). A malformed template never reaches here:
    it is a document error at check/compile time
    (:func:`prime_template_str_format_error`, ``core.compiler``). There is
    no fallback to the raw template.
    """
    critical_warning = (
        "CRITICAL: The next system will parse your response as YAML. "
        "If you include ANY markdown (like **bold**, *italics*, backticks, or headings), parsing fails.\n"
        "Do not output any '*' characters except as plain text INSIDE a YAML quoted string.\n"
        "Never use *, **, backticks, headings, or bullet prose outside YAML.\n"
        "Every line must be valid YAML.\n"
    )
    rendered = render_template(
        prime_template or "",
        {"max_effects": max_effects, "goal": goal, "context": context},
        label="reflector prime_template",
        escape=False,
    ).strip()
    return critical_warning + rendered + "\n\n"


#: ``goal``, ``context`` and ``max_effects`` -- the only names
#: :func:`_render_reflector_prime` ever saw rendered with ``str.format``.
_STR_FORMAT_PLACEHOLDER_NAMES = ("goal", "context", "max_effects")


def prime_template_str_format_error(prime_template: str) -> str | None:
    """None if *prime_template* contains no leftover ``str.format``
    placeholder; otherwise a message naming the fix, identical on both
    engines (``core.compiler``, ``electricity-compiler``'s own port).

    A single-brace ``{goal}``/``{context}``/``{max_effects}`` once rendered
    with ``str.format``; now that ``prime_template`` renders with Mustache,
    it would render as literal text instead of the planner's goal, its
    context, or the effect cap -- almost certainly not what the author
    wants, so it is refused rather than silently kept. A double-braced
    ``{{goal}}`` (the Mustache form, or a literal ``{{{{goal}}}}``-shaped
    escape) never matches: the brace immediately before or after it is
    checked and excluded.
    """
    best_pos: int | None = None
    best_name: str | None = None
    for name in _STR_FORMAT_PLACEHOLDER_NAMES:
        token = "{" + name + "}"
        start = 0
        while True:
            idx = prime_template.find(token, start)
            if idx == -1:
                break
            before_ok = idx == 0 or prime_template[idx - 1] != "{"
            after = idx + len(token)
            after_ok = after >= len(prime_template) or prime_template[after] != "}"
            if before_ok and after_ok:
                if best_pos is None or idx < best_pos:
                    best_pos = idx
                    best_name = name
                break
            start = idx + 1
    if best_name is None:
        return None
    old = "{" + best_name + "}"
    new = "{{" + best_name + "}}"
    return f"uses the str.format placeholder '{old}'; write '{new}' instead."


def _best_effort_goal(store: Store) -> str:
    """The root ``goal`` effect's value: ``prime.goal.value`` at the run root.

    A top-level ``prompt`` (or any effect) named ``goal`` is the documented
    source — there is no dedicated goal effect type. Missing or non-string,
    this renders as an empty slot rather than raising.
    """
    try:
        root = _run_root_state(store)
        prime = root.get("prime")
        if isinstance(prime, dict):
            goal = prime.get("goal")
            if isinstance(goal, dict):
                v = goal.get("value")
                if isinstance(v, str) and v.strip():
                    return v.strip()
    except Exception:
        logger.debug("Best-effort goal extraction failed", exc_info=True)
    return ""


def _best_effort_context(store: Store) -> str:
    """A concise, redacted summary of ``runtime.effective_settings``.

    Only the knobs a planner can act on — the model/adapter it is running
    under and the tool plugins it may target with a ``type: tool`` effect —
    not the full settings object (which also carries the resolved runtime
    config, profile content, and provenance, and can be arbitrarily large).
    Redacted again here, in addition to the redaction already applied when
    ``effective_settings`` was embedded in state: a prime directive is a
    model-facing surface, so it gets the same defense-in-depth as any other
    output artifact. Size-capped so a long plugin list can't crowd out the
    rest of the prime directive.
    """
    try:
        root = _run_root_state(store)
        runtime = root.get("runtime")
        if not isinstance(runtime, dict):
            return ""
        eff = runtime.get("effective_settings")
        if not isinstance(eff, dict):
            return ""
        summary = {
            "model": eff.get("model"),
            "adapter": eff.get("adapter"),
            "plugins": eff.get("plugins"),
        }
        text = json.dumps(redact(summary), indent=2, sort_keys=True)
        if len(text) > _CONTEXT_MAX_CHARS:
            text = text[:_CONTEXT_MAX_CHARS].rstrip() + "\n... (truncated)"
        return text
    except Exception:
        logger.debug("Best-effort context extraction failed", exc_info=True)
    return ""


def _prepend_prime_to_plan_prompt(
    *, inner: DynamicDefinition, plan_from_step: str, prime_text: str
) -> DynamicDefinition:
    """Shallow-clone DynamicDefinition, prepending prime to the plan prompt's template."""
    from .prompt import PromptDefinition

    new_effects: list[Any] = []
    for s in inner.effects:
        if isinstance(s, PromptDefinition) and s.name == plan_from_step:
            new_effects.append(replace(s, template=prime_text + (s.template or "")))
        else:
            new_effects.append(s)

    return replace(inner, effects=new_effects)


def _get_plan_text(*, node: dict[str, Any], plan_from_step: str) -> str:
    inner = node.get("inner")
    if not isinstance(inner, dict):
        return ""
    step_node = inner.get(plan_from_step)
    if not isinstance(step_node, dict):
        return ""
    val = step_node.get("value")
    return val if isinstance(val, str) else ""


def _extract_done_flag(plan_text: str) -> tuple[bool, Any]:
    """Extract the done flag from plan text and return (done, parsed_plan).

    The plan text from the LLM may include a `done: true/false` field alongside
    `effects: [...]`. We extract the done flag for iteration control, then ensure
    the parsed document has a valid `effects` key for use(inline) to process --
    never dumped back to YAML and reparsed, so the plan runs with its keys in
    the order the model wrote them (#778).

    Returns:
        (done, parsed_plan) where parsed_plan is the parsed document (a dict
        with an `effects` key, once normalization can find one), or whatever
        else the plan text parsed to when it couldn't -- a bare list becomes
        ``{"effects": [...]}``, anything else is returned as-is for
        use(inline)'s own validation to reject. ``None`` means the plan text
        was blank.
    """
    cleaned = _clean_yaml_fences(plan_text)
    if not cleaned.strip():
        return True, None

    # Unparseable YAML (syntax or semantic, e.g. an invalid calendar date)
    # propagates to the caller as `parse_failed: {e}` -- there is no raw
    # text left to hand use(inline) to diagnose instead.
    parsed = _yaml.safe_load(cleaned)

    if not isinstance(parsed, dict):
        # A bare list → treat as effects, not done
        if isinstance(parsed, list):
            return False, {"effects": parsed}
        return False, parsed

    done = bool(parsed.get("done", False))

    # Ensure effects key exists for the orchestration schema
    if "effects" not in parsed:
        # Check for legacy 'steps' key
        steps = parsed.get("steps")
        if isinstance(steps, list):
            parsed["effects"] = steps
            del parsed["steps"]
        # Check for 'plan' wrapper
        elif isinstance(parsed.get("plan"), dict) and "effects" in parsed["plan"]:
            parsed["effects"] = parsed["plan"]["effects"]
            del parsed["plan"]
        elif isinstance(parsed.get("plan"), list):
            parsed["effects"] = parsed["plan"]
            del parsed["plan"]
        else:
            return done, parsed

    # Remove done from the dict — it's not part of the orchestration schema
    # (additionalProperties: true allows it, but cleaner to strip)
    parsed.pop("done", None)

    return done, parsed
