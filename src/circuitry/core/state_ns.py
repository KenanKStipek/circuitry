"""Root-namespace contract for orchestration state (issue #86, Option C).

State has exactly three root namespaces:

* ``input``   — caller-supplied values (CLI ``-e``/``--state``, profile
  ``inputs``, ``use.inputs``, REST/scheduler ``state`` payloads)
* ``prime``   — effect outputs, rooted at the compiled orchestration root
* ``runtime`` — framework metadata (last_run, plugins, persistence, …)

Outside CEL, paths are root-relative (``input.items``, ``prime.x.value``).
Inside CEL, ``state`` binds to the state root (``state.input.items``).
There is no sugar layer: legacy spellings are hard errors, raised from
``core.compiler`` so ``cof check``/``validate()`` surface them before a
run ever dispatches.

Framework builtins injected at the root (``_run_id``, ``_timestamp``) and
loop-scope bindings (``as`` names, ``_loop_index``, sibling shorthand
inside a loop body) are not namespaces and are left alone by both the
migration helper and the validators.
"""

from __future__ import annotations

import logging
import re
from typing import Any

logger = logging.getLogger(__name__)

#: The only legal root namespaces.
NAMESPACES: tuple[str, ...] = ("input", "prime", "runtime")

#: The namespace caller-supplied values live under.
INPUT_NS = "input"

#: ``state.<key>`` references inside a CEL expression.
_CEL_STATE_KEY = re.compile(r"\bstate\.([A-Za-z_][A-Za-z0-9_]*)")

#: Mustache tag keys: ``{{name}}``, ``{{{name}}}``, ``{{&name}}``, and the
#: section forms ``{{#name}}``/``{{^name}}``/``{{/name}}``. Comments and
#: partials never match because ``!``/``>`` are in neither character class.
_MUSTACHE_TAG = re.compile(r"\{\{\{?\s*[#^/&]?\s*([A-Za-z0-9_.\-]+)\s*\}?\}\}")


def migrate_legacy_state(state: dict[str, Any]) -> dict[str, Any]:
    """Wrap caller-supplied root keys under the ``input`` namespace.

    Lift-on-hydrate: a state dict that already carries an ``input`` key is
    trusted as namespaced and returned untouched, which makes the helper
    idempotent and safe to apply at every hydration choke point. Otherwise
    every root key that is neither a namespace nor a framework builtin
    (``_``-prefixed) is moved under ``input`` and the lift is logged once.

    Mutates *state* in place and returns it.
    """
    if INPUT_NS in state:
        return state
    lifted = [
        key
        for key in state
        if key not in NAMESPACES and not key.startswith("_")
    ]
    input_ns: dict[str, Any] = {}
    for key in lifted:
        input_ns[key] = state.pop(key)
    state[INPUT_NS] = input_ns
    if lifted:
        logger.info(
            "Lifted legacy root state keys under 'input': %s",
            ", ".join(sorted(lifted)),
        )
    return state


def validate_each_in_path(path: str, *, effect_path: str) -> None:
    """Hard-error unless a loop ``each.in`` path is rooted at a namespace.

    Legal roots are ``input.``/``prime.``/``runtime.``. Bare keys and
    ``state.``-prefixed spellings both raise, with the canonical spelling
    named in the message.
    """
    root, _, rest = path.partition(".")
    if root in NAMESPACES:
        return
    if root == "state":
        if rest.partition(".")[0] in NAMESPACES:
            raise ValueError(
                f"Loop each.in at '{effect_path}': 'state.' is a CEL-only "
                f"binding; outside CEL, paths are root-relative. "
                f"Write '{rest}' instead of '{path}'."
            )
        suffix = rest or "<key>"
        raise ValueError(
            f"Loop each.in at '{effect_path}': '{path}' is not rooted at a "
            f"state namespace. Write 'input.{suffix}' for caller-supplied "
            f"values or 'prime.{suffix}' for effect outputs."
        )
    if not path:
        raise ValueError(
            f"Loop each.in at '{effect_path}' must be a dot path rooted at "
            "'input.', 'prime.', or 'runtime.' (e.g. 'input.items')."
        )
    raise ValueError(
        f"Loop each.in at '{effect_path}': bare key '{path}' is not rooted "
        f"at a state namespace. Write 'input.{path}' for caller-supplied "
        f"values or 'prime.{path}' for effect outputs."
    )


def validate_cel_expr(expr: str, *, effect_path: str) -> None:
    """Hard-error on ``state.<key>`` where ``<key>`` is not a namespace."""
    for match in _CEL_STATE_KEY.finditer(expr or ""):
        key = match.group(1)
        if key not in NAMESPACES:
            raise ValueError(
                f"CEL expression at '{effect_path}': 'state.{key}' does not "
                f"name a state namespace ('state' binds to the state root). "
                f"Write 'state.input.{key}' for caller-supplied values or "
                f"'state.prime.{key}' for effect outputs."
            )


def declared_input_names(orch: dict[str, Any]) -> frozenset[str]:
    """The input names an orchestration document declares in its interface."""
    interface = orch.get("interface")
    if not isinstance(interface, dict):
        return frozenset()
    inputs = interface.get("inputs")
    if not isinstance(inputs, dict):
        return frozenset()
    return frozenset(key for key in inputs if isinstance(key, str))


def validate_bare_input_refs(orch: dict[str, Any]) -> None:
    """Hard-error on bare ``{{name}}`` refs to this document's declared inputs.

    Only names declared in the document's own ``interface.inputs`` are
    policed — undeclared bare keys (loop ``as`` vars, ``_loop_index``,
    sibling shorthand) stay legal. Scans every template-bearing string
    field the runtime renders with Mustache.
    """
    declared = declared_input_names(orch)
    if not declared:
        return
    effects = orch.get("effects") or orch.get("steps") or []
    if isinstance(effects, list):
        _walk_bare_refs(effects, declared, container_path="effects")


#: Container fields whose values are lists of child effect dicts.
_CHILD_LISTS = ("effects", "steps", "body", "then", "else")


def _walk_bare_refs(
    effects: list[Any], declared: frozenset[str], *, container_path: str
) -> None:
    for idx, effect in enumerate(effects):
        if not isinstance(effect, dict):
            continue
        effect_path = f"{container_path}[{idx}]"
        for field, text in _iter_template_strings(effect):
            _check_bare_refs(text, declared, location=f"{effect_path}.{field}")
        for field in _CHILD_LISTS:
            children = effect.get(field)
            if isinstance(children, list):
                _walk_bare_refs(
                    children, declared, container_path=f"{effect_path}.{field}"
                )


def _iter_template_strings(effect: dict[str, Any]) -> list[tuple[str, str]]:
    """Every (field-label, text) pair the runtime Mustache-renders."""
    found: list[tuple[str, str]] = []
    for field in ("template", "prompt", "inline"):
        value = effect.get(field)
        if isinstance(value, str):
            found.append((field, value))
    messages = effect.get("messages")
    if isinstance(messages, list):
        for i, message in enumerate(messages):
            if isinstance(message, dict) and isinstance(
                message.get("content"), str
            ):
                found.append((f"messages[{i}].content", message["content"]))
    for field in ("inputs", "params"):
        value = effect.get(field)
        if isinstance(value, dict):
            found.extend(_nested_strings(value, prefix=field))
    for field in ("while", "if"):
        value = effect.get(field)
        if isinstance(value, dict) and isinstance(value.get("template"), str):
            found.append((f"{field}.template", value["template"]))
    return found


def _nested_strings(value: Any, *, prefix: str) -> list[tuple[str, str]]:
    if isinstance(value, str):
        return [(prefix, value)]
    if isinstance(value, dict):
        return [
            pair
            for key, child in value.items()
            for pair in _nested_strings(child, prefix=f"{prefix}.{key}")
        ]
    if isinstance(value, list):
        return [
            pair
            for i, child in enumerate(value)
            for pair in _nested_strings(child, prefix=f"{prefix}[{i}]")
        ]
    return []


def _check_bare_refs(
    text: str, declared: frozenset[str], *, location: str
) -> None:
    for match in _MUSTACHE_TAG.finditer(text):
        key = match.group(1)
        root = key.partition(".")[0]
        if root in NAMESPACES:
            continue
        if root in declared:
            raise ValueError(
                f"Template at '{location}': bare '{{{{{key}}}}}' refers to "
                f"declared interface input '{root}'. Caller inputs live "
                f"under the 'input' namespace; write "
                f"'{{{{input.{key}}}}}'."
            )
