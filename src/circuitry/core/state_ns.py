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
inside a loop body) are not namespaces and are left alone by the
migration helper — a bare ``{{as_name}}``/``{{_loop_index}}`` template ref
is always legal, no ``interface.inputs`` declaration required.

CEL sees the same loop-scope bindings, but only where they are actually in
lexical scope. ``validate_cel_expr``'s ``extra_names`` parameter is the
compile-time allow-list an enclosing loop pushes onto ``state.<key>`` for
CEL expressions inside its own body: the loop's ``each.as`` name (if it
is an ``each`` loop) and ``iter`` — the loop-metadata namespace holding
``iter.index``, the current 0-based iteration count, for both ``each``
and ``while`` loops. A ``while`` loop's own condition (``while.expr``) also
sees ``iter``, but there ``iter.index``/``iter.count`` describe the pass
that just ran rather than the one about to — see ``LoopRuntime.execute``
in :mod:`circuitry.core.loop`. Nested loops accumulate: an expression two loops
deep sees both loops' bindings, with a repeated ``as`` name resolving to
the innermost (shadowing) binding at runtime. Outside any loop, or
referencing a name no enclosing loop declared, ``state.<key>`` is a hard
error — same as any other undeclared namespace.

``validate_cel_expr`` also rejects a bare (free) name in CEL — one
written without the ``state.`` prefix at all. ``state`` itself is always
allowed as a root (plus ``value`` and ``meta`` for ``expect:``, via the
``roots`` parameter), as are CEL's own type names and a macro variable
within its own macro's body (``all``, ``exists``, ``exists_one``, ``map``
and ``filter``, in their exactly-two-argument form). Everything else is a
hard error, reported as whichever of three cases it is: an enclosing
loop's own binding written bare, a state namespace written bare, or an
otherwise undefined name.
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
        # DEBUG, not INFO: a bare `-e KEY=VALUE` run with no `--state` file
        # lifts its own root keys exactly the same way a genuinely legacy
        # (pre-namespace) snapshot does, so this fires on effectively every
        # `-e` invocation — INFO-under-`--verbose` noise for completely
        # ordinary use, not something worth flagging each time.
        logger.debug(
            "Lifted legacy root state keys under 'input': %s",
            ", ".join(sorted(lifted)),
        )
    return state


def validate_reference_path(
    path: str,
    *,
    label: str,
    effect_path: str,
    loop_names: frozenset[str] = frozenset(),
) -> None:
    """Hard-error unless a by-reference ``{from: <path>}`` leaf has a legal root.

    Shared by every ``{from: <path>}`` leaf — a ``use`` effect's ``inputs``
    and a tool effect's ``params`` (#234) alike: a path resolves against the
    same scope templates see: rooted at ``input.``/``prime.``/``runtime.``, or
    a binding of an enclosing loop (``each.as``, ``iter``). ``state.``-prefixed
    spellings and bare keys raise, with the canonical spelling named in the
    message. *label* names the specific leaf (e.g. "Use effect 'sub' input
    'got'", "Tool effect 'call' param 'symbols'") for the error text.
    """
    where = f"{label} at '{effect_path}'"
    if not path:
        raise ValueError(f"{where}: '{{from: ...}}' needs a non-empty path.")
    root, _, rest = path.partition(".")
    if root in NAMESPACES or root in loop_names:
        return
    if root == "state" and rest.partition(".")[0] in NAMESPACES:
        raise ValueError(
            f"{where}: 'state.' is a CEL-only binding; outside CEL, paths are "
            f"root-relative. Write '{rest}' instead of '{path}'."
        )
    bindings = ", ".join(sorted(loop_names)) if loop_names else "none here"
    raise ValueError(
        f"{where}: '{path}' is not rooted at a state namespace or an enclosing "
        f"loop binding (loop bindings in scope: {bindings}). Write "
        f"'input.{path}' for caller-supplied values or 'prime.<effect>.value' "
        f"for effect outputs."
    )


def validate_each_in_path(
    path: str, *, effect_path: str, loop_names: frozenset[str] = frozenset()
) -> None:
    """Hard-error unless a loop ``each.in`` path is rooted at a namespace.

    Legal roots are ``input.``/``prime.``/``runtime.``, or a binding of an
    enclosing loop (``each.as``) — the same scope ``{from: ...}`` resolves
    against (see ``validate_reference_path``). Bare keys and
    ``state.``-prefixed spellings both raise, with the canonical spelling
    named in the message.
    """
    root, _, rest = path.partition(".")
    if root in NAMESPACES or root in loop_names:
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
    bindings = ", ".join(sorted(loop_names)) if loop_names else "none here"
    if not path:
        raise ValueError(
            f"Loop each.in at '{effect_path}' must be a dot path rooted at "
            "'input.', 'prime.', or 'runtime.' (e.g. 'input.items'), or an "
            f"enclosing loop binding (loop bindings in scope: {bindings})."
        )
    raise ValueError(
        f"Loop each.in at '{effect_path}': bare key '{path}' is not rooted "
        f"at a state namespace or an enclosing loop binding (loop bindings "
        f"in scope: {bindings}). Write 'input.{path}' for caller-supplied "
        f"values or 'prime.{path}' for effect outputs."
    )


#: CEL's own type names — always legal bare, in every expression.
_TYPE_NAMES = frozenset(
    {
        "bool",
        "bytes",
        "double",
        "int",
        "list",
        "map",
        "null_type",
        "string",
        "type",
        "uint",
    }
)

#: Display order for a ``roots`` set in the "is not defined" message
#: ("'state', 'value' and 'meta' are the only roots", never alphabetical).
_ROOT_ORDER = ("state", "value", "meta")


def _format_roots(roots: frozenset[str]) -> str:
    quoted = [f"'{root}'" for root in _ROOT_ORDER if root in roots]
    if len(quoted) == 1:
        return f"{quoted[0]} is the only root"
    return f"{', '.join(quoted[:-1])} and {quoted[-1]} are the only roots"


def validate_cel_expr(
    expr: str,
    *,
    effect_path: str,
    extra_names: frozenset[str] = frozenset(),
    roots: frozenset[str] = frozenset({"state"}),
) -> None:
    """Hard-error on ``state.<key>`` where ``<key>`` is not a namespace, or
    on a bare (free) name CEL cannot resolve.

    The paths and names come off the CEL parse tree, so a quoted
    ``'state.topic'`` is a string literal and not a violation. An
    expression that does not parse is left alone here —
    ``cel_eval.validate_cel_syntax`` reports that with the parser's own
    message.

    ``extra_names`` is the compile-time allow-list an enclosing loop
    contributes for its own body: its ``each.as`` name and/or ``iter``
    (see the module docstring). It has no effect outside a loop body,
    where the caller passes the empty default.

    ``roots`` is every bare name this CEL site itself binds — ``state``
    everywhere, plus ``value`` and ``meta`` for ``expect:``. Beyond
    those, CEL's own type names and a macro variable (inside its own
    macro's body) are always legal bare; every other free name is a hard
    error, naming the first such name in source order.
    """
    from .cel_eval import _MAX_EXPR_LENGTH, CelError, free_identifiers, state_paths

    try:
        paths = state_paths(expr or "")
    except CelError:
        return
    for path in paths:
        key = path.split(".")[1]
        if key not in NAMESPACES and key not in extra_names:
            raise ValueError(
                f"CEL expression at '{effect_path}': 'state.{key}' does not "
                f"name a state namespace ('state' binds to the state root). "
                f"Write 'state.input.{key}' for caller-supplied values or "
                f"'state.prime.{key}' for effect outputs."
                + (
                    f" Names bound by an enclosing loop here: "
                    f"{', '.join(sorted(extra_names))}."
                    if extra_names
                    else ""
                )
            )

    # An over-long expression is left to validate_cel_syntax's own
    # length error, the same way a parse failure is left alone above:
    # free_identifiers has no length cap of its own (matching
    # state_paths), so without this an expression that happens to
    # contain a bare name would report that instead of being too long.
    if len(expr or "") > _MAX_EXPR_LENGTH:
        return
    try:
        names = free_identifiers(expr or "")
    except CelError:
        return
    allowed = roots | _TYPE_NAMES
    for name in names:
        if name in allowed:
            continue
        if name in extra_names:
            raise ValueError(
                f"CEL expression at '{effect_path}': '{name}' is a loop "
                f"binding: in CEL, write 'state.{name}'."
            )
        if name in NAMESPACES:
            raise ValueError(
                f"CEL expression at '{effect_path}': '{name}' is a state "
                f"namespace: in CEL, write 'state.{name}'."
            )
        raise ValueError(
            f"CEL expression at '{effect_path}': '{name}' is not defined "
            f"({_format_roots(roots)}). Write 'state.input.{name}' for "
            f"caller-supplied values or 'state.prime.{name}' for effect "
            f"outputs."
            + (
                f" Names bound by an enclosing loop here: "
                f"{', '.join(sorted(extra_names))}."
                if extra_names
                else ""
            )
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
    finally_effects = orch.get("finally")
    if isinstance(finally_effects, list):
        _walk_bare_refs(finally_effects, declared, container_path="finally")


#: Container fields whose values are lists of child effect dicts.
_CHILD_LISTS = ("effects", "steps", "body", "then", "else", "finally")


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
    for field in ("template", "prompt", "inline", "params_json"):
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
