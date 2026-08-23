"""Advisory lint: things that parse fine but that the language no longer teaches.

Circuitry's parser is deliberately forgiving — every historical spelling still
compiles and runs, and nothing here ever turns a valid document invalid. What
this module does is name the drift, so authors (and the models trained on
authored orchestrations) converge on one spelling per construct:

* deprecated effect-type aliases (``conditional`` → ``if``)
* deprecated flow aliases (``cot``/``chain_of_thought`` → ``chain``, and the
  ``tot``/``tree_of_thought`` → ``tree`` pair)
* effects named after an effect *type* (``use``, ``loop``, ``if``, ``dynamic``)
  — generic names read as structure rather than intent, and duplicates of them
  collide in sibling scope
* loop-body references that do not mean what they look like: a fixed
  ``iter_<N>`` path (stale data, not an error) and ``prime.<loop>.<step>``
  (never resolves), both of which have the same fix — the canonical
  within-iteration form ``{{prime.<step>.value}}``

Warnings surface through ``cof validate`` / ``cof check`` and the MCP
``validate_orchestration`` tool. Exit codes are unaffected.
"""

from __future__ import annotations

import re
from collections.abc import Mapping, Sequence
from typing import Any, NamedTuple

__all__ = [
    "CANONICAL_EFFECT_TYPES",
    "DEPRECATED_EFFECT_TYPE_ALIASES",
    "DEPRECATED_FLOW_ALIASES",
    "TYPE_KEYWORDS",
    "lint_orchestration",
]

#: Effect types as the docs, rules, and examples spell them.
CANONICAL_EFFECT_TYPES = frozenset(
    {"prompt", "dynamic", "if", "loop", "reflector", "tool", "use"}
)

#: Still parsed, no longer taught. alias -> canonical.
DEPRECATED_EFFECT_TYPE_ALIASES = {"conditional": "if"}

#: Still parsed, no longer taught. alias -> canonical.
DEPRECATED_FLOW_ALIASES = {
    "chain_of_thought": "chain",
    "cot": "chain",
    "tree_of_thought": "tree",
    "tot": "tree",
}

#: Words that name a *kind* of effect rather than a job. Poor effect names.
TYPE_KEYWORDS = CANONICAL_EFFECT_TYPES | set(DEPRECATED_EFFECT_TYPE_ALIASES)

#: Keys whose value is a list of child effects, in walk order.
_CHILD_KEYS = ("effects", "then", "else", "body")

#: A fixed-iteration path segment: ``prime.rank.iter_0.score.value``.
_ITER_SEGMENT = re.compile(r"\biter_\d+\b")


class _LoopScope(NamedTuple):
    """A loop whose iteration is in progress at the point being linted."""

    name: str | None
    body_names: frozenset[str]


def lint_orchestration(orch: Any) -> list[str]:
    """Return advisory warnings for an orchestration document.

    Never raises: a malformed document is the schema validator's problem, and
    lint just declines to say anything about the parts it cannot read.
    """
    warnings: list[str] = []
    if not isinstance(orch, Mapping):
        return warnings

    _check_flow(orch.get("flow"), where="top level", warnings=warnings)

    effects = orch.get("effects")
    if not isinstance(effects, Sequence) or isinstance(effects, (str, bytes)):
        effects = orch.get("steps")
    _walk(effects, path="effects", warnings=warnings, loops=())

    return warnings


def _walk(
    effects: Any,
    *,
    path: str,
    warnings: list[str],
    loops: tuple[_LoopScope, ...],
) -> None:
    if not isinstance(effects, Sequence) or isinstance(effects, (str, bytes)):
        return
    for index, effect in enumerate(effects):
        if not isinstance(effect, Mapping):
            continue
        here = f"{path}[{index}]"

        # A loop's own `while` / `each` blocks are evaluated once per pass, so
        # they live in the loop's own iteration scope alongside its body.
        inner = (*loops, _loop_scope(effect)) if _is_loop(effect) else loops

        _check_effect(effect, where=here, warnings=warnings, loops=inner)
        for key in _CHILD_KEYS:
            _walk(effect.get(key), path=f"{here}.{key}", warnings=warnings, loops=inner)


def _is_loop(effect: Mapping[str, Any]) -> bool:
    raw_type = effect.get("type")
    return isinstance(raw_type, str) and raw_type.strip().lower() == "loop"


def _loop_scope(effect: Mapping[str, Any]) -> _LoopScope:
    body = effect.get("body")
    names: set[str] = set()
    if isinstance(body, Sequence) and not isinstance(body, (str, bytes)):
        for child in body:
            if isinstance(child, Mapping):
                child_name = child.get("name")
                if isinstance(child_name, str) and child_name:
                    names.add(child_name)
    name = effect.get("name")
    return _LoopScope(
        name=name if isinstance(name, str) and name else None,
        body_names=frozenset(names),
    )


def _reference_strings(effect: Mapping[str, Any]) -> list[str]:
    """Every string an effect renders or evaluates, minus its child effects.

    Templates, message contents, tool params, CEL expressions, `use` inputs —
    they are all just strings carrying state paths, and none of them can be
    told apart usefully here. Child effect lists are excluded because the walk
    visits those in their own right (and under their own loop scope).
    """
    found: list[str] = []

    def _collect(value: Any) -> None:
        if isinstance(value, str):
            found.append(value)
        elif isinstance(value, Mapping):
            for sub in value.values():
                _collect(sub)
        elif isinstance(value, Sequence) and not isinstance(value, (str, bytes)):
            for sub in value:
                _collect(sub)

    for key, value in effect.items():
        if key in _CHILD_KEYS:
            continue
        _collect(value)
    return found


def _check_loop_references(
    effect: Mapping[str, Any],
    *,
    where: str,
    warnings: list[str],
    loops: tuple[_LoopScope, ...],
) -> None:
    """Flag the two loop-body reference forms that silently do the wrong thing.

    ``iter_<N>`` is the dangerous one: inside the body of the very loop it
    names it does not fail, it resolves to whichever pass ``N`` names —
    iteration 0's output, read during iteration 3. An empty render is at least
    visible; plausible stale data is not. Only *enclosing* loops are flagged;
    a fixed pass of some other, already-finished loop is a real reference.

    ``prime.<loop>.<step>`` is the merely-empty one: ``prime.<loop>`` is the
    loop's own node, holding ``iter_<N>``, ``collected`` and ``meta``, so a
    body step's name is never a key there.
    """
    named = [scope for scope in loops if scope.name is not None]
    if not named:
        return

    seen: set[str] = set()
    for text in _reference_strings(effect):
        for scope in named:
            pattern = rf"\bprime\.{re.escape(scope.name or '')}\.([A-Za-z_]\w*)"
            for match in re.finditer(pattern, text):
                segment = match.group(1)
                key = f"{scope.name}.{segment}"
                if key in seen:
                    continue

                if _ITER_SEGMENT.fullmatch(segment):
                    seen.add(key)
                    warnings.append(
                        f"{where}: 'prime.{scope.name}.{segment}' names a fixed "
                        f"pass of the loop it sits inside. It does not fail — it "
                        f"resolves to pass {segment.removeprefix('iter_')}'s "
                        f"output in every iteration, so every later pass reads "
                        f"stale data. Use '{{{{prime.<step>.value}}}}' for the "
                        f"current pass; iter_<N> is only meaningful outside the "
                        f"loop."
                    )
                elif segment in scope.body_names:
                    seen.add(key)
                    warnings.append(
                        f"{where}: 'prime.{scope.name}.{segment}' never resolves "
                        f"— 'prime.{scope.name}' is the loop's own node (iter_<N>, "
                        f"collected, meta), not a scope its body steps live in. "
                        f"Use '{{{{prime.{segment}.value}}}}' for the current "
                        f"pass, or 'prime.{scope.name}.collected.value' after the "
                        f"loop."
                    )


def _check_effect(
    effect: Mapping[str, Any],
    *,
    where: str,
    warnings: list[str],
    loops: tuple[_LoopScope, ...] = (),
) -> None:
    raw_type = effect.get("type")
    effect_type = raw_type.strip().lower() if isinstance(raw_type, str) else ""

    canonical = DEPRECATED_EFFECT_TYPE_ALIASES.get(effect_type)
    if canonical is not None:
        warnings.append(
            f"{where}: type '{effect_type}' is a deprecated alias — "
            f"write 'type: {canonical}'. Both parse; only '{canonical}' is documented."
        )

    _check_flow(effect.get("flow"), where=where, warnings=warnings)

    name = effect.get("name")
    if isinstance(name, str) and name.strip().lower() in TYPE_KEYWORDS:
        warnings.append(
            f"{where}: effect is named '{name}', which is an effect-type keyword. "
            "Name effects after the job they do (e.g. 'summarize_article'), not "
            "after their type — generic names collide when two of them end up "
            "siblings in the same scope."
        )

    _check_loop_references(effect, where=where, warnings=warnings, loops=loops)


def _check_flow(flow: Any, *, where: str, warnings: list[str]) -> None:
    if not isinstance(flow, str):
        return
    canonical = DEPRECATED_FLOW_ALIASES.get(flow.strip().lower())
    if canonical is not None:
        warnings.append(
            f"{where}: flow '{flow}' is a deprecated alias — write "
            f"'flow: {canonical}'. Both parse; only '{canonical}' is documented."
        )
