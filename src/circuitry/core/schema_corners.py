"""`jsonschema` corners `cof` and `electricity` agree on, rather than each
reproducing an accident of the Python library or its own imitation of it
(issue #778, step 5):

* ``uniqueItems`` compares every pair by real JSON equality (numbers by
  value, a boolean never equal to a number, ``NaN`` never equal to
  anything) instead of `jsonschema`'s own sort-then-compare-neighbours,
  which can miss a real duplicate a merely-tied element separates.
* A ``multipleOf`` that isn't a number greater than zero — including
  ``NaN``/``Infinity``, which pass `jsonschema`'s own metaschema check for
  it (``{"type": "number", "exclusiveMinimum": 0}``, which neither value
  violates) — makes the schema invalid.
* A `$ref` chain is capped at :data:`MAX_REF_CHAIN_HOPS` consecutive
  resolutions at one instance location, well under the point a bare
  ``jsonschema.validate`` call's own Python recursion can actually reach
  (that point varies with the process's call-stack depth and even with
  whether a CEL expression has compiled yet in this process — `cel-python`
  raises the recursion limit itself — so relying on it is itself the kind
  of accident this module replaces).

Used wherever a document embeds a JSON Schema: a ``prompt`` effect's
``schema:``, the ``cel`` tool's ``params.schema``, and the ``agent`` tool's
``params.result_schema``.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from typing import Any
from urllib.parse import unquote

#: How many consecutive `$ref` resolutions, at one instance location and
#: never descending into the instance, a schema may chain before both
#: engines refuse it. Measured against a bare ``jsonschema.validate`` call
#: with the default (not yet `cel-python`-raised) recursion limit and a
#: shallow call stack: that combination's own real `RecursionError` starts
#: at 484-495 consecutive hops. This stays comfortably under that in every
#: case, including a deeper call stack or a smaller process-wide limit than
#: this process happens to have.
MAX_REF_CHAIN_HOPS = 200

#: Schema keys a `$ref` chain is never read through: the two definition-only
#: containers (reached only by an actual `$ref` into them, never
#: structurally) and the keywords whose own value is literal instance data,
#: never a subschema — a ``"$ref"`` key written *inside* one of these is
#: someone's own data, not this module's keyword.
_NON_SCHEMA_STRUCTURAL_KEYS = frozenset(
    {"$defs", "definitions", "const", "enum", "examples", "default"}
)

#: The one message both engines use for a `multipleOf` that isn't a number
#: greater than zero, replacing `jsonschema`'s own varied metaschema text
#: for the same mistake.
MULTIPLE_OF_ERROR = "multipleOf must be a number greater than 0"


def _multiple_of_is_malformed(value: Any) -> bool:
    if isinstance(value, bool):
        return True
    if not isinstance(value, (int, float)):
        return True
    if isinstance(value, float) and not _is_finite(value):
        return True
    return value <= 0


def _is_finite(value: float) -> bool:
    return value == value and value not in (float("inf"), float("-inf"))


def _multiple_of_well_formedness_error(node: Any) -> str | None:
    """:data:`MULTIPLE_OF_ERROR` if some ``multipleOf`` anywhere in *node*
    (recursively) isn't a number greater than zero; ``None`` otherwise."""
    if isinstance(node, Mapping):
        if "multipleOf" in node and _multiple_of_is_malformed(node["multipleOf"]):
            return MULTIPLE_OF_ERROR
        for value in node.values():
            found = _multiple_of_well_formedness_error(value)
            if found is not None:
                return found
    elif isinstance(node, list):
        for item in node:
            found = _multiple_of_well_formedness_error(item)
            if found is not None:
                return found
    return None


def _is_same_document_pointer(ref: str) -> bool:
    return ref == "#" or ref.startswith("#/")


def _resolve_same_document_ref(root: Any, ref: str) -> Any:
    """The node *ref* (a same-document JSON pointer) names in *root*, or
    ``None`` when it does not resolve."""
    pointer = unquote(ref[1:])
    if pointer == "":
        return root
    node = root
    for raw_segment in pointer.split("/")[1:]:
        segment = raw_segment.replace("~1", "/").replace("~0", "~")
        if isinstance(node, Mapping) and segment in node:
            node = node[segment]
            continue
        if isinstance(node, list):
            try:
                index = int(segment)
            except ValueError:
                return None
            if 0 <= index < len(node):
                node = node[index]
                continue
        return None
    return node


def ref_chain_depth_exceeds_cap(schema: Any, *, limit: int = MAX_REF_CHAIN_HOPS) -> bool:
    """Whether *schema* chains more than *limit* consecutive bare `$ref`
    resolutions at one instance location.

    Only an actual `$ref`-to-`$ref` hop, with nothing else evaluated at
    that same location in between, counts: a hop reached through an
    ordinary structural key (``properties``, ``items``, ...) resets the
    count, since evaluating it moves to a different part of the instance —
    the same distinction a recursive tree/linked-list schema relies on to
    stay finite for any real instance despite its own schema graph being
    cyclic. A genuine cycle with no such structural key anywhere in it (two
    schemas each only `$ref`-ing the other) can never move to a different
    instance location at all, so it counts as exceeding the cap immediately
    once found, rather than being mistaken for one of those legitimate
    cycles.
    """
    structural_active: set[int] = set()

    def longest(node: Any, ref_hops: int, ref_run_active: frozenset[int]) -> int:
        if not isinstance(node, Mapping):
            return ref_hops
        node_id = id(node)
        if node_id in structural_active:
            return 0
        if ref_hops >= limit:
            return ref_hops
        structural_active.add(node_id)
        try:
            result = ref_hops
            ref = node.get("$ref")
            if isinstance(ref, str) and _is_same_document_pointer(ref):
                target = _resolve_same_document_ref(schema, ref)
                target_id = id(target)
                if target_id in ref_run_active:
                    result = max(result, limit)
                else:
                    result = max(
                        result,
                        longest(target, ref_hops + 1, ref_run_active | {node_id}),
                    )
            for key, value in node.items():
                if key == "$ref" or key in _NON_SCHEMA_STRUCTURAL_KEYS:
                    continue
                if isinstance(value, Mapping):
                    result = max(result, longest(value, 0, frozenset()))
                elif isinstance(value, list):
                    for item in value:
                        result = max(result, longest(item, 0, frozenset()))
            return result
        finally:
            structural_active.discard(node_id)

    return longest(schema, 0, frozenset()) >= limit


def ref_chain_error(schema: Any, *, limit: int = MAX_REF_CHAIN_HOPS) -> str | None:
    if ref_chain_depth_exceeds_cap(schema, limit=limit):
        return f"$ref chain is too long (limit {limit} consecutive references)"
    return None


def schema_well_formedness_error(schema: Mapping[str, Any]) -> str | None:
    """``None`` if *schema* is a well-formed JSON Schema, else one message
    naming why — this module's own text for a `multipleOf`/`$ref`-chain
    corner, `jsonschema`'s own ``SchemaError`` text for anything else."""
    import jsonschema  # type: ignore[import-untyped]

    found = _multiple_of_well_formedness_error(schema)
    if found is not None:
        return found
    found = ref_chain_error(schema)
    if found is not None:
        return found
    try:
        jsonschema.validators.validator_for(schema).check_schema(schema)
    except jsonschema.SchemaError as exc:
        return exc.message
    return None


def _canonical_key(value: Any) -> Any:
    """A hashable key such that two JSON values are equal under J1's rule
    (numbers by value across int/float, a boolean distinct from any
    number, ``NaN`` never equal to anything, deep structural equality for
    arrays/objects) iff their canonical keys are equal — or ``None`` when
    *value* contains a ``NaN``/non-finite float anywhere, which this rule
    never treats as a duplicate of anything, including itself."""
    if isinstance(value, bool):
        return ("bool", value)
    if isinstance(value, (int, float)):
        if isinstance(value, float) and not _is_finite(value):
            return None
        return ("num", value)
    if isinstance(value, str):
        return ("str", value)
    if value is None:
        return ("null",)
    if isinstance(value, list):
        keys = []
        for item in value:
            key = _canonical_key(item)
            if key is None:
                return None
            keys.append(key)
        return ("list", tuple(keys))
    if isinstance(value, Mapping):
        entries = {}
        for k, v in value.items():
            key = _canonical_key(v)
            if key is None:
                return None
            entries[k] = key
        return ("dict", frozenset(entries.items()))
    return None


def _has_duplicate(items: Sequence[Any]) -> bool:
    seen: set[Any] = set()
    for item in items:
        key = _canonical_key(item)
        if key is None:
            continue
        if key in seen:
            return True
        seen.add(key)
    return False


def _unique_items_validator(validator: Any, uI: Any, instance: Any, schema: Any) -> Any:
    import jsonschema  # type: ignore[import-untyped]

    if uI and validator.is_type(instance, "array") and _has_duplicate(instance):
        yield jsonschema.exceptions.ValidationError(f"{instance!r} has non-unique elements")


def extended_validator_class(schema: Mapping[str, Any]) -> Any:
    """The validator class `jsonschema` would pick for *schema*, with its
    own ``uniqueItems`` keyword replaced by one comparing every pair."""
    import jsonschema  # type: ignore[import-untyped]

    base = jsonschema.validators.validator_for(schema)
    return jsonschema.validators.extend(base, {"uniqueItems": _unique_items_validator})


def extended_validator(schema: Mapping[str, Any]) -> Any:
    """A validator instance for *schema*, built from
    :func:`extended_validator_class`."""
    return extended_validator_class(schema)(schema)
