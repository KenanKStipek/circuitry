"""Reflector — planning primitive that generates and executes effects at runtime.

Internally delegates to use(inline) for YAML validation, compilation, and
execution of LLM-generated orchestrations. The reflector is syntax sugar that
adds: prime directive rendering, iteration control (done flag), and automatic
wiring between the inner planning prompt and the generated orchestration.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, replace
from typing import TYPE_CHECKING, Any

import yaml as _yaml  # type: ignore[import-untyped]

from ..adapters import Adapter
from .primes import REFLECTOR_PRIME_V1
from .store import Store
from .use import UseDefinition, UseRuntime, _clean_yaml_fences

logger = logging.getLogger(__name__)

if TYPE_CHECKING:
    from .dynamic import DynamicDefinition


def _store_root(store: Store) -> dict[str, Any]:
    for attr in ("data", "state", "root", "_data", "_state", "_root"):
        v = getattr(store, attr, None)
        if isinstance(v, dict):
            return v
    if isinstance(store, dict):
        return store  # type: ignore[return-value]
    raise AttributeError("Store does not expose its underlying dict.")


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


class ReflectorRuntime:
    def __init__(
        self,
        definition: ReflectorDefinition,
        *,
        adapter: Adapter,
        model: str,
        runtime_config: dict[str, Any] | None = None,
        dry_run: bool = False,
        timeout_seconds: int = 120,
        verbose: bool = False,
    ):
        self.defn = definition
        self.adapter = adapter
        self.model = model
        self.runtime_config = runtime_config or {}
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds
        self.verbose = verbose

    def execute(self, *, store: Store) -> None:
        node = store.ensure_dict(self.defn.name)
        node.setdefault("value", None)
        meta = _ensure_dict(node, "meta")
        meta.setdefault("iterations", [])
        node.setdefault(self.defn.generated_key, {})

        reflector_store = store.child(self.defn.name)

        iterations = max(1, int(self.defn.max_iterations or 1))
        for i in range(iterations):
            rec = self._run_iteration(i=i, store=store, reflector_store=reflector_store)
            meta["iterations"].append(rec)

            if rec.get("error"):
                node["value"] = False
                raise RuntimeError(rec["error"])

            if rec.get("stop", False):
                node["value"] = True
                return

        node["value"] = True

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
                runtime_config=self.runtime_config,
                dry_run=self.dry_run,
                timeout_seconds=self.timeout_seconds,
                verbose=self.verbose,
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
            done, plan_yaml = _extract_done_flag(plan_text)
            rec["done"] = done

            if not plan_yaml.strip():
                rec["stop"] = True
                return rec

            # Check for empty effects list — nothing to execute
            try:
                parsed_check = _yaml.safe_load(plan_yaml)
                effects = parsed_check.get("effects", []) if isinstance(parsed_check, dict) else []
                rec["parsed"] = parsed_check
                if not effects:
                    rec["stop"] = True
                    return rec
            except Exception:
                pass  # let use(inline) handle parse errors

            if done and self.defn.stop_on_done:
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
                inline=plan_yaml,
                validate=True,
                on_error="fail",
            )

            gen_parent = reflector_store.child(self.defn.generated_key)
            gen_store = gen_parent

            UseRuntime(
                use_defn,
                adapter=self.adapter,
                model=self.model,
                runtime_config=self.runtime_config,
                dry_run=self.dry_run,
                timeout_seconds=self.timeout_seconds,
                verbose=self.verbose,
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
    critical_warning = (
        "CRITICAL: The next system will parse your response as YAML. "
        "If you include ANY markdown (like **bold**, *italics*, backticks, or headings), parsing fails.\n"
        "Do not output any '*' characters except as plain text INSIDE a YAML quoted string.\n"
        "Never use *, **, backticks, headings, or bullet prose outside YAML.\n"
        "Every line must be valid YAML.\n"
    )
    try:
        rendered = (
            (prime_template or "")
            .format(
                max_effects=max_effects,
                goal=goal,
                context=context,
            )
            .strip()
        )
        return critical_warning + rendered + "\n\n"
    except Exception:
        logger.warning("Reflector prime template formatting failed; using raw template", exc_info=True)
        return critical_warning + (prime_template or "").strip() + "\n\n"


def _best_effort_goal(store: Store) -> str:
    try:
        root = _store_root(store)
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
    try:
        root = _store_root(store)
        runtime = root.get("runtime")
        if isinstance(runtime, dict):
            eff = runtime.get("effective_settings")
            if isinstance(eff, dict):
                import json

                return json.dumps(eff, indent=2, sort_keys=True)
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


def _extract_done_flag(plan_text: str) -> tuple[bool, str]:
    """Extract the done flag from plan text and return (done, cleaned_yaml).

    The plan text from the LLM may include a `done: true/false` field alongside
    `effects: [...]`. We extract the done flag for iteration control, then ensure
    the YAML has a valid `effects` key for use(inline) to process.

    Returns:
        (done, cleaned_yaml) where cleaned_yaml is valid orchestration YAML.
    """
    cleaned = _clean_yaml_fences(plan_text)
    if not cleaned.strip():
        return True, ""

    try:
        parsed = _yaml.safe_load(cleaned)
    except _yaml.YAMLError:
        # If YAML is unparseable, let use(inline) handle the error
        return False, cleaned

    if not isinstance(parsed, dict):
        # A bare list → treat as effects, not done
        if isinstance(parsed, list):
            return False, _yaml.dump({"effects": parsed}, default_flow_style=False)
        return False, cleaned

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
            return done, cleaned

    # Remove done from the dict — it's not part of the orchestration schema
    # (additionalProperties: true allows it, but cleaner to strip)
    parsed.pop("done", None)

    return done, _yaml.dump(parsed, default_flow_style=False)
