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


#: Keywords whose own value is literal instance data, never a subschema —
#: a key named ``multipleOf`` written *inside* one of these (``const: {
#: multipleOf: 0}``) is someone's own data, not this module's keyword.
_LITERAL_DATA_KEYS = frozenset({"const", "enum", "examples", "default"})

#: Keywords whose own value maps a *name* (a property name, a pattern, a
#: definition name) to a subschema: the mapping's own keys are names, not
#: keywords, so a mapping with a key literally spelled ``multipleOf``
#: (``properties: {multipleOf: {type: number}}``) is a property named
#: "multipleOf", not an occurrence of the keyword — only each *value* is
#: itself a schema worth walking.
_NAME_MAP_SCHEMA_KEYS = frozenset(
    {"properties", "patternProperties", "dependentSchemas", "$defs", "definitions"}
)


def _multiple_of_well_formedness_error(node: Any) -> str | None:
    """:data:`MULTIPLE_OF_ERROR` if some ``multipleOf`` anywhere in *node*
    (recursively) isn't a number greater than zero; ``None`` otherwise."""
    if isinstance(node, Mapping):
        if "multipleOf" in node and _multiple_of_is_malformed(node["multipleOf"]):
            return MULTIPLE_OF_ERROR
        for key, value in node.items():
            if key in _LITERAL_DATA_KEYS:
                continue
            if key in _NAME_MAP_SCHEMA_KEYS:
                if isinstance(value, Mapping):
                    for subschema in value.values():
                        found = _multiple_of_well_formedness_error(subschema)
                        if found is not None:
                            return found
                continue
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


#: :func:`_resolve_same_document_ref`'s own return value for a pointer
#: that does not resolve at all — distinct from a pointer that resolves
#: to the literal JSON ``null``, which is a found value like any other.
_UNRESOLVED = object()


def _resolve_same_document_ref(root: Any, ref: str) -> Any:
    """The node *ref* (a same-document JSON pointer) names in *root*, or
    :data:`_UNRESOLVED` when it does not resolve."""
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
                return _UNRESOLVED
            if 0 <= index < len(node):
                node = node[index]
                continue
        return _UNRESOLVED
    return node

#: Keywords whose own subschema applies to a *different* part of the
#: instance than the keyword's own location, each reached directly (not
#: through a name map). An edge through one of these resets the running
#: `$ref` chain to zero — the same distinction a recursive tree/linked-
#: list schema relies on to stay finite for any real instance despite its
#: own schema graph being cyclic. ``items`` is handled separately: its
#: value is either one of these (a single schema) or a list, like
#: :data:`_INSTANCE_DESCENDING_LIST_SCHEMA_KEYS`'s own.
_INSTANCE_DESCENDING_SINGLE_SCHEMA_KEYS = frozenset(
    {"additionalProperties", "contains", "propertyNames"}
)

#: Keywords whose own value is a list of subschemas, each applied to a
#: different part of the instance (each list element); see
#: :data:`_INSTANCE_DESCENDING_SINGLE_SCHEMA_KEYS`.
_INSTANCE_DESCENDING_LIST_SCHEMA_KEYS = frozenset({"prefixItems"})

#: Keywords whose own value maps a name (a property name, a pattern) to
#: a subschema, each applied to a different part of the instance (the
#: value or property the name picks out); see
#: :data:`_INSTANCE_DESCENDING_SINGLE_SCHEMA_KEYS`.
_INSTANCE_DESCENDING_MAPPING_SCHEMA_KEYS = frozenset({"properties", "patternProperties"})

#: Keywords whose own subschema applies at the *same* instance location
#: as the keyword itself (an in-place applicator): an edge through one of
#: these carries the running `$ref` chain through unchanged, rather than
#: resetting it the way an instance-descending edge does.
_IN_PLACE_LIST_SCHEMA_KEYS = frozenset({"allOf", "anyOf", "oneOf"})
_IN_PLACE_SINGLE_SCHEMA_KEYS = frozenset({"not", "if", "then", "else"})
_IN_PLACE_MAPPING_SCHEMA_KEYS = frozenset({"dependentSchemas"})


def ref_chain_depth_exceeds_cap(schema: Any, *, limit: int = MAX_REF_CHAIN_HOPS) -> bool:
    """Whether *schema* chains more than *limit* consecutive bare `$ref`
    resolutions at one instance location, never descending into the
    instance in between. A chain of exactly *limit* resolutions is fine;
    one more is not.

    An edge through an instance-descending keyword (``properties``,
    ``items``, ...) resets the running chain to zero, since evaluating it
    moves to a different part of the instance — the same distinction a
    recursive tree/linked-list schema relies on to stay finite for any
    real instance despite its own schema graph being cyclic. An edge
    through an in-place applicator (``allOf``, ``not``, ``if``, ...)
    carries the running chain through unchanged instead, since it applies
    at the very same instance location as the keyword it came from — a
    schema that cycles back to a node already active in the *current*
    chain (whether through `$ref` hops alone or mixed with in-place
    applicators) can never move to a different instance location at all,
    so it counts as exceeding the cap immediately once found, rather than
    being mistaken for one of those legitimate, instance-descending
    cycles. A `$ref` that does not resolve still consumes one hop, the
    same as a hop that lands on an ordinary leaf schema.

    *memo* caches ``longest(node, ref_hops, ...)`` by ``(node id,
    ref_hops)`` once computed, reused by a later call reaching the same
    node at the same hop count by a different path — without it, a
    `$defs` graph where many nodes `$ref` the same, deeper shared targets
    revisits those targets once per distinct path to them, which grows
    exponentially with depth even though the graph itself has few nodes.
    A cycle within the *current* run is still caught first, before the
    cache is ever consulted, by *ref_run_active*/*structural_active*
    (both scoped to one real call path, never memoized) — the cache only
    ever serves a call already known clean of that.
    """
    structural_active: set[int] = set()
    memo: dict[tuple[int, int], int] = {}

    def longest(node: Any, ref_hops: int, ref_run_active: frozenset[int]) -> int:
        if not isinstance(node, Mapping):
            return ref_hops
        node_id = id(node)
        if node_id in ref_run_active:
            return limit + 1
        if node_id in structural_active:
            return 0
        if ref_hops > limit:
            return ref_hops
        cache_key = (node_id, ref_hops)
        if cache_key in memo:
            return memo[cache_key]
        structural_active.add(node_id)
        try:
            result = ref_hops
            run_active = ref_run_active | {node_id}
            ref = node.get("$ref")
            if isinstance(ref, str) and _is_same_document_pointer(ref):
                target = _resolve_same_document_ref(schema, ref)
                if target is _UNRESOLVED:
                    result = max(result, ref_hops + 1)
                else:
                    result = max(result, longest(target, ref_hops + 1, run_active))
            for key, value in node.items():
                if key == "$ref" or key in _NON_SCHEMA_STRUCTURAL_KEYS:
                    continue
                if key == "items":
                    if isinstance(value, Mapping):
                        result = max(result, longest(value, 0, frozenset()))
                    elif isinstance(value, list):
                        for item in value:
                            result = max(result, longest(item, 0, frozenset()))
                    continue
                if key in _INSTANCE_DESCENDING_SINGLE_SCHEMA_KEYS:
                    if isinstance(value, Mapping):
                        result = max(result, longest(value, 0, frozenset()))
                    continue
                if key in _INSTANCE_DESCENDING_LIST_SCHEMA_KEYS:
                    if isinstance(value, list):
                        for item in value:
                            result = max(result, longest(item, 0, frozenset()))
                    continue
                if key in _INSTANCE_DESCENDING_MAPPING_SCHEMA_KEYS:
                    if isinstance(value, Mapping):
                        for subschema in value.values():
                            result = max(result, longest(subschema, 0, frozenset()))
                    continue
                if key in _IN_PLACE_LIST_SCHEMA_KEYS:
                    if isinstance(value, list):
                        for item in value:
                            result = max(result, longest(item, ref_hops, run_active))
                    continue
                if key in _IN_PLACE_SINGLE_SCHEMA_KEYS:
                    if isinstance(value, Mapping):
                        result = max(result, longest(value, ref_hops, run_active))
                    continue
                if key in _IN_PLACE_MAPPING_SCHEMA_KEYS:
                    if isinstance(value, Mapping):
                        for subschema in value.values():
                            result = max(result, longest(subschema, ref_hops, run_active))
                    continue
                # Any other key (an unrecognized/custom keyword) is
                # treated the same as an instance-descending one: the
                # same conservative default the walk always used, which
                # keeps a chain hiding behind one bounded, rather than
                # carried through as if it applied at the same location.
                if isinstance(value, Mapping):
                    result = max(result, longest(value, 0, frozenset()))
                elif isinstance(value, list):
                    for item in value:
                        result = max(result, longest(item, 0, frozenset()))
            memo[cache_key] = result
            return result
        finally:
            structural_active.discard(node_id)

    return longest(schema, 0, frozenset()) > limit


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


def _is_nan(value: float) -> bool:
    return value != value


def _canonical_key(value: Any) -> Any:
    """A hashable key such that two JSON values are equal under this
    module's own rule for ``uniqueItems`` (numbers by value across int/float — including ``Infinity``, equal to
    another ``Infinity`` of the same sign — a boolean distinct from any
    number, ``NaN`` never equal to anything, deep structural equality for
    arrays/objects) iff their canonical keys are equal — or ``None`` when
    *value* contains a ``NaN`` anywhere, which this rule never treats as a
    duplicate of anything, including itself."""
    if isinstance(value, bool):
        return ("bool", value)
    if isinstance(value, (int, float)):
        if isinstance(value, float) and _is_nan(value):
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


#: A placeholder passed to ``validator.descend``'s own ``path=`` in place
#: of a literal ``None`` property name: `jsonschema`'s own ``descend``
#: (`jsonschema/validators.py`) treats ``path=None`` as its own sentinel
#: for “no path segment to add” (`if path is not None: error.path.
#: appendleft(path)`), which collides with a real document key that IS
#: ``None`` (a YAML ``null`` mapping key) — silently dropping that
#: segment rather than recording it. Every caller below swaps this in
#: for a ``None`` property name, then rewrites it back to the real
#: ``None`` on each error ``descend`` actually yields.
_NULL_PATH_MARKER = object()


def _properties_validator(validator: Any, properties: Any, instance: Any, schema: Any) -> Any:
    """`jsonschema`'s own ``properties`` keyword (`_keywords.properties`),
    with a literal ``None`` property name kept in each error's own path
    instead of silently dropped."""
    if not validator.is_type(instance, "object"):
        return
    for property_name, subschema in properties.items():
        if property_name in instance:
            path = _NULL_PATH_MARKER if property_name is None else property_name
            for error in validator.descend(
                instance[property_name],
                subschema,
                path=path,
                schema_path=property_name,
            ):
                if property_name is None:
                    error.path[0] = None
                yield error


def _additional_properties_validator(validator: Any, aP: Any, instance: Any, schema: Any) -> Any:
    """`jsonschema`'s own ``additionalProperties`` keyword
    (`_keywords.additionalProperties`), with a literal ``None`` property
    name kept in each error's own path instead of silently dropped. The
    ``aP`` boolean/no-match-found branches never consult an instance key
    as a path segment at all, so only the subschema branch needs this."""
    from jsonschema._utils import (  # type: ignore[import-untyped]
        extras_msg,
        find_additional_properties,
    )

    if not validator.is_type(instance, "object"):
        return
    extra = set(find_additional_properties(instance, schema))
    if validator.is_type(aP, "object"):
        for extra_name in extra:
            path = _NULL_PATH_MARKER if extra_name is None else extra_name
            for error in validator.descend(instance[extra_name], aP, path=path):
                if extra_name is None:
                    error.path[0] = None
                yield error
    elif not aP and extra:
        import jsonschema  # type: ignore[import-untyped]

        if "patternProperties" in schema:
            verb = "does" if len(extra) == 1 else "do"
            joined = ", ".join(repr(each) for each in sorted(extra, key=str))
            patterns = ", ".join(repr(each) for each in sorted(schema["patternProperties"]))
            yield jsonschema.exceptions.ValidationError(
                f"{joined} {verb} not match any of the regexes: {patterns}"
            )
        else:
            joined, verb = extras_msg(sorted(extra, key=str))
            yield jsonschema.exceptions.ValidationError(
                f"Additional properties are not allowed ({joined} {verb} unexpected)"
            )


def extended_validator_class(schema: Mapping[str, Any]) -> Any:
    """The validator class `jsonschema` would pick for *schema*, with its
    own ``uniqueItems`` keyword replaced by one comparing every pair, and
    its own ``properties``/``additionalProperties`` keywords replaced by
    ones that keep a literal ``None`` property name in an error's own
    path (see :data:`_NULL_PATH_MARKER`)."""
    import jsonschema  # type: ignore[import-untyped]

    base = jsonschema.validators.validator_for(schema)
    return jsonschema.validators.extend(
        base,
        {
            "uniqueItems": _unique_items_validator,
            "properties": _properties_validator,
            "additionalProperties": _additional_properties_validator,
        },
    )


def extended_validator(schema: Mapping[str, Any]) -> Any:
    """A validator instance for *schema*, built from
    :func:`extended_validator_class`."""
    return extended_validator_class(schema)(schema)
