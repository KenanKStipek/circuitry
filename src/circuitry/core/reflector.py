from __future__ import annotations

from dataclasses import dataclass, replace
from typing import TYPE_CHECKING, Any

from .store import Store
from ..adapters import Adapter
from .primes import REFLECTOR_PRIME_V1

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
    inner: "DynamicDefinition"

    plan_from_step: str = "propose_steps"

    # v1 loop control
    max_iterations: int = 1

    # where generated effects are executed under the reflector node
    generated_key: str = "generated"

    stop_on_done: bool = True

    # Prime directive support
    prime_template: str = REFLECTOR_PRIME_V1
    max_effects: int = 8


class ReflectorRuntime:
    def __init__(
        self,
        definition: ReflectorDefinition,
        *,
        adapter: Adapter,
        model: str,
        dry_run: bool = False,
        timeout_seconds: int = 120,
        verbose: bool = False,
    ):
        self.defn = definition
        self.adapter = adapter
        self.model = model
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds
        self.verbose = verbose

    def execute(self, *, store: Store) -> None:
        # Local imports to avoid circulars
        from .dynamic import DynamicRuntime

        node = store.ensure_dict(self.defn.name)
        node.setdefault("value", None)
        meta = _ensure_dict(node, "meta")
        meta.setdefault("iterations", [])
        node.setdefault(self.defn.generated_key, {})

        # Execute everything under reflector namespace
        reflector_store = Store(node, on_write=store.on_write)

        iterations = max(1, int(self.defn.max_iterations or 1))
        for i in range(iterations):
            rec = self._run_iteration(i=i, store=store, reflector_store=reflector_store)
            meta["iterations"].append(rec)

            # stop conditions
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
        """
        One reflector loop iteration:
          1) build prime text
          2) run inner dynamic (primed)
          3) read plan text
          4) parse/validate plan
          5) compile + execute generated effects under reflector.generated.iter_{i}
        """
        from .dynamic import DynamicRuntime
        from .compiler import compile_orchestration

        rec: dict[str, Any] = {
            "i": i,
            "plan_from_step": self.defn.plan_from_step,
            "done": False,
            "stop": False,
            "parsed": None,
            "error": None,
            "plan_text": "",
        }

        # --- Phase 1: prime text ---
        prime_text = _render_reflector_prime(
            prime_template=self.defn.prime_template,
            max_effects=self.defn.max_effects,
            goal=_best_effort_goal(store),
            context=_best_effort_context(store),
        )

        # --- Phase 2: execute inner dynamic (with prime prepended) ---
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
                dry_run=self.dry_run,
                timeout_seconds=self.timeout_seconds,
            ).execute(store=reflector_store)
        except Exception as e:
            rec["error"] = f"inner_failed: {e}"
            return rec

        # --- Phase 3: pull plan text from inner.<plan_from_step>.value ---
        plan_text = _get_plan_text(
            node=_store_root(reflector_store), plan_from_step=self.defn.plan_from_step
        )
        rec["plan_text"] = plan_text

        if self.dry_run:
            rec["done"] = True
            rec["stop"] = True
            return rec

        # --- Phase 4: parse + validate plan ---
        try:
            if self.verbose:
                rec["raw_plan_text"] = plan_text
                print("\n[Reflector] Raw plan text:")
                print(plan_text)
                print("[/Reflector]\n")

            parsed = _parse_plan_yaml(plan_text)
            rec["parsed"] = parsed

            if self.verbose:
                print("[Reflector] Parsed plan object:")
                from pprint import pprint

                pprint(parsed)

            done = bool(parsed.get("done", False))
            effects = parsed.get("effects") or []
            if not isinstance(effects, list):
                raise ValueError("Reflector plan 'effects' must be a list.")

            rec["done"] = done

            # nothing to do
            if len(effects) == 0:
                rec["stop"] = True
                return rec

            # stop-on-done semantics
            if done and self.defn.stop_on_done:
                rec["stop"] = True
                return rec

            _validate_generated_effects(effects)
        except Exception as e:
            rec["error"] = f"parse_failed: {e}"
            return rec

        # --- Phase 5: compile + execute generated effects ---
        iteration_key = f"iter_{i}"
        gen_orch = {"effects": effects}

        try:
            gen_def = compile_orchestration(orch=gen_orch, root_name=iteration_key)
        except Exception as e:
            rec["error"] = f"compile_failed: {e}"
            return rec

        try:
            gen_root = reflector_store.ensure_dict(self.defn.generated_key)
            iter_container = _ensure_dict(gen_root, iteration_key)
            gen_store = Store(iter_container, on_write=store.on_write)

            DynamicRuntime(
                gen_def,
                adapter=self.adapter,
                model=self.model,
                dry_run=self.dry_run,
                timeout_seconds=self.timeout_seconds,
            ).execute(store=gen_store)

        except Exception as e:
            rec["error"] = f"generated_exec_failed: {e}"
            return rec

        return rec


def _ensure_dict(parent: dict[str, Any], key: str) -> dict[str, Any]:
    node = parent.get(key)
    if not isinstance(node, dict):
        node = {}
        parent[key] = node
    return node


def _render_reflector_prime(
    *, prime_template: str, max_effects: int, goal: str, context: str
) -> str:
    # Add explicit warning to the user in the prime
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
        return critical_warning + (prime_template or "").strip() + "\n\n"


def _best_effort_goal(store: Store) -> str:
    # prime.goal.value is the convention
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
        pass
    return ""


def _best_effort_context(store: Store) -> str:
    # keep it small but useful; you can swap this for "selector context" later
    try:
        root = _store_root(store)
        runtime = root.get("runtime")
        if isinstance(runtime, dict):
            eff = runtime.get("effective_settings")
            if isinstance(eff, dict):
                import json

                return json.dumps(eff, indent=2, sort_keys=True)
    except Exception:
        pass
    return ""


def _prepend_prime_to_plan_prompt(
    *, inner: "DynamicDefinition", plan_from_step: str, prime_text: str
) -> "DynamicDefinition":
    """
    Shallow-clone DynamicDefinition where plan prompt template is:
      prime_text + original_template
    """
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


def _strip_code_fences(text: str) -> str:
    """
    Remove markdown code fences anywhere in the text.
    This is intentionally blunt: if the model emits ```yaml ... ``` or stray ```
    lines inside the payload, we strip them so yaml.safe_load can succeed.
    """
    t = (text or "").strip()
    if not t:
        return ""

    lines = []
    for line in t.splitlines():
        if line.strip().startswith("```"):
            continue
        lines.append(line)

    return "\n".join(lines).strip()


def _extract_yaml(text: str) -> str:
    """
    Extracts YAML content from mixed markdown/prose by selecting from
    the first likely YAML line onward.
    """
    t = (text or "").strip()
    if not t:
        return ""

    # 1) If fenced, strip the fence and keep interior
    t = _strip_code_fences(t)

    # 2) If model included prose, find first likely-yaml line and slice from there
    lines = t.splitlines()
    # Support both 'effects' (spec) and 'steps' (legacy)
    starters = ("done:", "effects:", "steps:", "- type:", "type:", "plan:")
    for idx, line in enumerate(lines):
        s = line.lstrip()
        if any(s.startswith(x) for x in starters):
            return "\n".join(lines[idx:]).strip()

    # 3) Otherwise return as-is (parser will fail, but error will be clearer)
    return t


def _parse_plan_yaml(text: str) -> dict[str, Any]:
    """
    Accepts any of:
      - [ {type,name,...}, ... ]                       -> effects list
      - { done: bool, effects: [...] }                 -> canonical
      - { effects: [...] }                             -> canonical w/ default done
      - { plan: { effects: [...] } }                   -> recover from "plan:" wrappers
      - { plan: [...] }                                -> recover from "plan:" list
      - Legacy 'steps' key is also supported for backwards compatibility
    Also tolerates multi-document YAML (---) by selecting the first parseable doc.
    """
    import yaml

    cleaned = _extract_yaml(text)
    if not cleaned.strip():
        return {"done": True, "effects": []}

    docs = list(yaml.safe_load_all(cleaned)) if cleaned else []
    docs = [d for d in docs if d is not None]

    # pick the first doc that looks like something we can use
    for data in docs:
        if isinstance(data, list):
            return {"done": False, "effects": data}

        if isinstance(data, dict):
            # canonical: prefer 'effects', fallback to 'steps'
            if "effects" in data:
                return {
                    "done": bool(data.get("done", False)),
                    "effects": data.get("effects", []),
                }
            if "steps" in data:
                return {
                    "done": bool(data.get("done", False)),
                    "effects": data.get("steps", []),
                }

            # recovery: plan wrapper
            plan = data.get("plan")
            if isinstance(plan, dict):
                if isinstance(plan.get("effects"), list):
                    return {
                        "done": bool(data.get("done", False)),
                        "effects": plan["effects"],
                    }
                if isinstance(plan.get("steps"), list):
                    return {
                        "done": bool(data.get("done", False)),
                        "effects": plan["steps"],
                    }
            if isinstance(plan, list):
                return {"done": bool(data.get("done", False)), "effects": plan}

    return {"done": False, "effects": []}


def _validate_generated_effects(effects: list[Any]) -> None:
    allowed = {"prompt", "dynamic", "reflector"}

    for idx, s in enumerate(effects):
        if not isinstance(s, dict):
            raise ValueError(f"Generated effect[{idx}] must be an object.")

        t = (s.get("type") or "").strip().lower()
        n = s.get("name")

        if t not in allowed:
            raise ValueError(
                f"""Generated effect[{idx}] has invalid type. Expected one of: {sorted(allowed)} Actual effect: {repr(s)}"""
            )

        if not isinstance(n, str) or not n.strip():
            raise ValueError(f"Generated effect[{idx}] missing non-empty 'name'.")

        if t == "prompt":
            tpl = s.get("template")
            if not isinstance(tpl, str) or not tpl.strip():
                raise ValueError(
                    f"Generated prompt '{n}' missing non-empty 'template'."
                )

        if t in {"dynamic", "reflector"}:
            # Support both 'effects' (spec) and 'steps' (legacy)
            child = s.get("effects") or s.get("steps")
            if child is None:
                s["effects"] = []
            elif not isinstance(child, list):
                raise ValueError(f"Generated {t} '{n}' effects must be a list.")
            else:
                s["effects"] = child
