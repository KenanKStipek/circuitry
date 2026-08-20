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

Warnings surface through ``cof validate`` / ``cof check`` and the MCP
``validate_orchestration`` tool. Exit codes are unaffected.
"""

from __future__ import annotations

from typing import Any, Mapping, Sequence

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
    _walk(effects, path="effects", warnings=warnings)

    return warnings


def _walk(effects: Any, *, path: str, warnings: list[str]) -> None:
    if not isinstance(effects, Sequence) or isinstance(effects, (str, bytes)):
        return
    for index, effect in enumerate(effects):
        if not isinstance(effect, Mapping):
            continue
        here = f"{path}[{index}]"
        _check_effect(effect, where=here, warnings=warnings)
        for key in _CHILD_KEYS:
            _walk(effect.get(key), path=f"{here}.{key}", warnings=warnings)


def _check_effect(effect: Mapping[str, Any], *, where: str, warnings: list[str]) -> None:
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


def _check_flow(flow: Any, *, where: str, warnings: list[str]) -> None:
    if not isinstance(flow, str):
        return
    canonical = DEPRECATED_FLOW_ALIASES.get(flow.strip().lower())
    if canonical is not None:
        warnings.append(
            f"{where}: flow '{flow}' is a deprecated alias — write "
            f"'flow: {canonical}'. Both parse; only '{canonical}' is documented."
        )
