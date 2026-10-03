"""Structural checks every runnable orchestration passes before it executes.

``cof check`` (``runtime_shim.validate``), ``cof run`` (``runtime_shim.run``)
and a ``use`` effect loading its child all call :func:`structural_errors`, so
a document one of them rejects the others reject too. Three parts:

* the JSON schema (``schema/orchestration.schema.json``), reported with the
  path of the offending node;
* unknown keys. The schema leaves effects and the top level open, so a
  misspelled key would otherwise be dropped without a word. A key that is a
  near miss of a key the effect type knows (``whlie``, ``max_iteration``,
  ``adapter`` on a prompt) is an error — the author meant the known key and
  it is not being applied. Any other unknown key is a warning
  (:func:`unknown_key_warnings`): it is ignored, and may be deliberate;
* an ``interface.inputs`` default that doesn't match its declared ``type``
  (:func:`interface_default_type_errors`) — see #301. Unlike a CLI ``-e``
  value or a ``use:`` child input, a YAML/JSON default is already a typed
  value, not text to coerce: a quoted ``"3"`` for an ``integer`` input is
  the author's own mistake, so it is an error, not a silent coercion.
"""

from __future__ import annotations

import difflib
import json
from collections.abc import Mapping, Sequence
from functools import cache, lru_cache
from pathlib import Path
from typing import Any

from .interface_inputs import _TYPE_NAMES, _matches_type

__all__ = [
    "interface_default_type_errors",
    "interface_unknown_type_errors",
    "orchestration_schema",
    "schema_errors",
    "structural_errors",
    "unknown_key_errors",
    "unknown_key_warnings",
]

_SCHEMA_PATH = Path(__file__).parent.parent / "schema" / "orchestration.schema.json"

#: Effect type -> the schema definition that lists its keys.
_EFFECT_DEFS = {
    "prompt": "PromptEffect",
    "dynamic": "DynamicEffect",
    "if": "ConditionalEffect",
    "conditional": "ConditionalEffect",
    "loop": "LoopEffect",
    "reflector": "ReflectorEffect",
    "tool": "ToolEffect",
    "use": "UseEffect",
}

#: Keys the compiler still reads that the schema does not list: the legacy
#: ``steps``/``strategy`` spellings of ``effects``/``flow``.
_LEGACY_CONTAINER_KEYS = frozenset({"steps", "strategy"})

#: Top-level keys a document may carry besides the schema's own properties —
#: ``description`` is free text, ``runtime``/``plugins`` feed the run's
#: configuration (see the reference's File Structure section).
_EXTRA_TOP_LEVEL_KEYS = frozenset({"description", "runtime", "plugins"}) | _LEGACY_CONTAINER_KEYS

#: Keys that are not misspellings but are still a mistake for a known one.
#: ``adapter`` is a real top-level key; on an effect it is ``provider``.
_MISTAKEN_FOR = {"adapter": "provider"}

#: Keys whose value is a list of child effects. ``finally`` is only legal on
#: ``dynamic`` (and the document root, walked separately below), but a
#: near-miss key inside one is still a document author's typo worth catching
#: here rather than only at runtime (#272 review).
_CHILD_KEYS = ("effects", "steps", "then", "else", "body", "finally")


@lru_cache(maxsize=1)
def orchestration_schema() -> dict[str, Any]:
    """The orchestration JSON schema (read once per process)."""
    return json.loads(_SCHEMA_PATH.read_text(encoding="utf-8"))


def _describe_schema_error(err: Any) -> str:
    """``<path>: <message>``, plus the deepest sub-error when there is one.

    The path is spelled the way lint spells it (``effects[0].each``).

    ``EffectDef`` dispatches on ``type`` through nested if/then and several
    definitions use ``oneOf``, so the top-level message is often a bare "is
    not valid under any of the given schemas"; the best-matching sub-error
    says what is actually wrong.
    """
    import jsonschema  # type: ignore[import-untyped]

    message = err.message
    # "{...the whole effect...} is not valid under any of the given schemas":
    # the path already says which node, and a loop's repr includes its body.
    instance_repr = repr(err.instance)
    if isinstance(err.instance, (dict, list)) and message.startswith(instance_repr):
        message = "value" + message[len(instance_repr) :]
    if err.context:
        sub = jsonschema.exceptions.best_match(err.context)
        if sub is not None and sub.message != err.message:
            message = f"{message} ({sub.message})"
    where = err.json_path.removeprefix("$").removeprefix(".") or "top level"
    return f"{where}: {message}"


def schema_errors(orch: Any) -> list[str]:
    """Every schema violation in *orch*; empty when it conforms."""
    import jsonschema  # type: ignore[import-untyped]

    validator = jsonschema.Draft7Validator(orchestration_schema())
    return [
        _describe_schema_error(err)
        for err in sorted(validator.iter_errors(orch), key=str)
    ]


@cache
def _known_effect_keys(effect_type: str) -> frozenset[str]:
    definition = orchestration_schema()["$defs"][_EFFECT_DEFS[effect_type]]
    keys = frozenset(definition.get("properties", {}))
    if effect_type in ("dynamic", "reflector"):
        keys |= _LEGACY_CONTAINER_KEYS
    return keys


@lru_cache(maxsize=1)
def _known_top_level_keys() -> frozenset[str]:
    return frozenset(orchestration_schema()["properties"]) | _EXTRA_TOP_LEVEL_KEYS


def _near_miss(key: str, known: frozenset[str]) -> str | None:
    """The known key *key* was probably meant to be, or ``None``."""
    normalized = key.strip().lower().replace("-", "_")
    if normalized in known:
        return normalized
    mistaken_for = _MISTAKEN_FOR.get(normalized)
    if mistaken_for in known:
        return mistaken_for
    matches = difflib.get_close_matches(normalized, sorted(known), n=1, cutoff=0.8)
    return matches[0] if matches else None


def _unknown_keys(orch: Any) -> tuple[list[str], list[str]]:
    errors: list[str] = []
    warnings: list[str] = []
    if not isinstance(orch, Mapping):
        return errors, warnings

    def report(key: Any, known: frozenset[str], where: str, owner: str) -> None:
        label = repr(key) if isinstance(key, str) else f"{key!r} (YAML read the unquoted key as a {type(key).__name__})"
        match = _near_miss(key, known) if isinstance(key, str) else None
        if match is not None:
            errors.append(
                f"{where}: unknown key {label} on {owner} — did you mean '{match}'? "
                "As written it is ignored."
            )
        else:
            warnings.append(
                f"{where}: unknown key {label} on {owner} is ignored. "
                f"Known keys: {', '.join(sorted(known))}."
            )

    top_known = _known_top_level_keys()
    for key in orch:
        if key not in top_known:
            report(key, top_known, "top level", "the document")

    def walk(effects: Any, path: str) -> None:
        if not isinstance(effects, Sequence) or isinstance(effects, (str, bytes)):
            return
        for index, effect in enumerate(effects):
            if not isinstance(effect, Mapping):
                continue
            here = f"{path}[{index}]"
            raw_type = effect.get("type")
            effect_type = raw_type.strip().lower() if isinstance(raw_type, str) else ""
            if effect_type in _EFFECT_DEFS:
                known = _known_effect_keys(effect_type)
                for key in effect:
                    if key not in known:
                        report(key, known, here, f"a '{effect_type}' effect")
            for child_key in _CHILD_KEYS:
                walk(effect.get(child_key), f"{here}.{child_key}")

    effects = orch.get("effects")
    walk(effects if effects is not None else orch.get("steps"), "effects")
    walk(orch.get("finally"), "finally")
    return errors, warnings


def unknown_key_errors(orch: Any) -> list[str]:
    """Unknown keys that are near misses of a known key — the author's typo."""
    return _unknown_keys(orch)[0]


def unknown_key_warnings(orch: Any) -> list[str]:
    """Unknown keys that resemble no known key: ignored, possibly on purpose."""
    return _unknown_keys(orch)[1]


def _unquote_hint(value: str, declared_type: str) -> str:
    """A hint if *value*, unquoted, would itself satisfy *declared_type*.

    Checked against how YAML itself would read the unquoted text — not
    ``_coerce``'s lenient CLI word list (``y``/``t``/``1`` for ``boolean``):
    those stay a string or become an int when actually unquoted in YAML, so
    the hint would tell the author to make a change that doesn't fix
    anything.
    """
    import yaml

    try:
        coerced = yaml.safe_load(value)
    except yaml.YAMLError:
        return ""
    if not _matches_type(coerced, declared_type):
        return ""
    return f" — quoting it makes it a string; remove the quotes to declare it as {declared_type}"


def interface_unknown_type_errors(orch: Any) -> list[str]:
    """An ``interface.inputs`` declared ``type`` that isn't one of the six
    recognized names (#301 note 7).

    An unrecognized ``type`` is otherwise silently unchecked — not by the
    schema (``type`` is a free string there) and not by
    :func:`interface_default_type_errors` or ``check_interface_inputs``,
    which both skip a type they don't recognize rather than reject it.
    """
    if not isinstance(orch, Mapping):
        return []
    interface = orch.get("interface")
    if not isinstance(interface, Mapping):
        return []
    iface_inputs = interface.get("inputs")
    if not isinstance(iface_inputs, Mapping):
        return []

    errors: list[str] = []
    allowed = ", ".join(_TYPE_NAMES)
    for key, spec in iface_inputs.items():
        if not isinstance(spec, Mapping) or "type" not in spec:
            continue
        declared_type = spec["type"]
        if isinstance(declared_type, str) and declared_type in _TYPE_NAMES:
            continue
        errors.append(
            f"interface.inputs.{key}.type: {declared_type!r} is not a recognized "
            f"type — expected one of {allowed}."
        )
    return errors


def interface_default_type_errors(orch: Any) -> list[str]:
    """An ``interface.inputs`` default that doesn't match its declared ``type``.

    Applies the same ``_matches_type`` rules :func:`interface_inputs.check_interface_inputs`
    applies at run time, but without its string coercion: a default is
    already a real YAML/JSON value, not a CLI ``-e``/``use:`` value crossing
    as text, so a string default for a non-string type is always wrong — a
    quoted numeral gets a hint to unquote it, naming the input, the type and
    the value either way.
    """
    if not isinstance(orch, Mapping):
        return []
    interface = orch.get("interface")
    if not isinstance(interface, Mapping):
        return []
    iface_inputs = interface.get("inputs")
    if not isinstance(iface_inputs, Mapping):
        return []

    errors: list[str] = []
    for key, spec in iface_inputs.items():
        if not isinstance(spec, Mapping) or "default" not in spec:
            continue
        declared_type = spec.get("type")
        if not isinstance(declared_type, str) or declared_type not in _TYPE_NAMES:
            continue
        value = spec["default"]
        if _matches_type(value, declared_type):
            continue
        hint = _unquote_hint(value, declared_type) if isinstance(value, str) else ""
        errors.append(
            f"interface.inputs.{key}.default: declared type {declared_type!r} "
            f"but {value!r} is {type(value).__name__}{hint}."
        )
    return errors


def structural_errors(orch: Any) -> list[str]:
    """Near-miss unknown keys first (the likelier cause), then schema errors,
    then an ``interface.inputs`` unrecognized type, then a default that
    doesn't match its (recognized) type."""
    return [
        *unknown_key_errors(orch),
        *schema_errors(orch),
        *interface_unknown_type_errors(orch),
        *interface_default_type_errors(orch),
    ]
