"""The saved form of run state: each loop's final pass written once.

In memory a named loop's ``last`` is an alias of its final completed pass —
``node["last"] is node["iter_<N>"]`` (see ``LoopRuntime._link_last``). JSON
has no aliases, so a plain ``json.dumps`` writes that pass twice, and nested
loops compound it: an inner loop's ``last`` is copied again inside the outer
loop's ``last``.

Saved state (``--out``, ``--print``, the ``--live-state`` mirror) therefore
writes ``last`` as a reference to the sibling key it aliases::

    "last": {"$ref": "iter_3"}

and every path that reads saved state back into a run (``--state``, an
``initial_state`` built from a previous run's output, a persisted snapshot)
or into the TUI relinks it to the pass it names, so ``prime.<loop>.last``
is the same alias again. A state file written before this form existed holds
``last`` as a full copy; that still loads, as the copy it always was.
"""

from __future__ import annotations

import datetime
import json
import re
from typing import Any

#: The one key of a saved ``last`` reference; its value names the sibling
#: ``iter_<N>`` key holding the pass ``last`` aliased.
LAST_REF_KEY = "$ref"

_ITER_KEY = re.compile(r"iter_\d+")


def compact_last_aliases(state: Any) -> Any:
    """*state* with every aliased ``last`` replaced by ``{"$ref": "iter_<N>"}``.

    Never mutates *state*: dicts and lists on the path to a replaced alias are
    shallow-copied, everything else is shared with the original, so a state
    without loops comes back as the very same object. Only a ``last`` that *is*
    one of its siblings (identity, not equality) is replaced — a user effect
    named ``last`` is left alone.
    """
    if isinstance(state, dict):
        # Snapshot the items first: a tree-flow branch may add keys to a dict
        # while a live-state mirror is being written.
        items = list(state.items())
        alias_key = _aliased_iter_key(state.get("last"), items)
        changed: dict[str, Any] | None = None
        for key, child in items:
            if alias_key is not None and key == "last":
                new_child: Any = {LAST_REF_KEY: alias_key}
            else:
                new_child = compact_last_aliases(child)
            if new_child is not child:
                if changed is None:
                    changed = dict(items)
                changed[key] = new_child
        return state if changed is None else changed
    if isinstance(state, list):
        entries = list(state)
        rebuilt: list[Any] | None = None
        for index, child in enumerate(entries):
            new_child = compact_last_aliases(child)
            if new_child is not child:
                if rebuilt is None:
                    rebuilt = entries
                rebuilt[index] = new_child
        return state if rebuilt is None else rebuilt
    return state


def _aliased_iter_key(last: Any, items: list[tuple[str, Any]]) -> str | None:
    """The ``iter_<N>`` key whose value *is* ``last``, if any."""
    if not isinstance(last, dict):
        return None
    # The final completed pass is almost always the last iter key written.
    for key, child in reversed(items):
        if child is last and key != "last" and _ITER_KEY.fullmatch(key):
            return key
    return None


def link_last_refs(state: Any) -> Any:
    """Relink every saved ``last`` reference to the pass it names, in place.

    The inverse of :func:`compact_last_aliases`: ``{"$ref": "iter_<N>"}``
    next to an ``iter_<N>`` dict becomes that very dict again. A reference
    whose target is missing is left as it is, and so is a ``last`` holding a
    full copy of the pass (state saved before references existed). Returns
    *state* for chaining.
    """
    stack: list[Any] = [state]
    while stack:
        current = stack.pop()
        if isinstance(current, dict):
            target = _ref_target(current)
            if target is not None:
                current["last"] = current[target]
            stack.extend(
                child for key, child in current.items() if key != "last" or target is None
            )
        elif isinstance(current, list):
            stack.extend(current)
    return state


def _ref_target(node: dict[str, Any]) -> str | None:
    """The sibling key *node*'s ``last`` references, when it resolves."""
    last = node.get("last")
    if not isinstance(last, dict) or len(last) != 1:
        return None
    ref = last.get(LAST_REF_KEY)
    if not isinstance(ref, str) or not _ITER_KEY.fullmatch(ref):
        return None
    return ref if isinstance(node.get(ref), dict) else None


def json_safe_default(value: Any) -> str:
    """``json.dumps(..., default=str)``, but only for two of the YAML 1.1
    types a loaded orchestration document can produce that plain JSON can't
    encode: an unquoted date or timestamp (``datetime.date``,
    ``datetime.datetime`` — a subclass of ``date``) and ``!!binary``
    (``bytes``). These can reach state several ways — a reflector's own
    ``meta.iterations[i].parsed``, a tool's ``meta.params_rendered``, a
    ``use`` effect's literal (non-``{from: ...}``) inputs, an
    ``interface.inputs`` default with no declared ``type:`` (or nested
    inside an ``object``/``array`` default) — all fixed here, at the
    serialisation boundary, rather than at each entry point. A CLI ``-e``
    value never needs this: it's always a string. Every
    ``json.dumps(..., default=...)`` call writing run state uses this
    (``--out``/``--print``/``--live-state`` below; the ``core.store``
    persistence backends ``--resume <run-id>`` reads from). Anything else
    json.dumps can't encode — including ``!!set`` (``set``), the one other
    YAML 1.1 type a document can produce that plain JSON can't encode —
    still raises, same as a plain ``json.dumps`` with no ``default=`` at
    all.
    """
    if isinstance(value, (datetime.date, bytes)):
        return str(value)
    raise TypeError(f"Object of type {type(value).__name__} is not JSON serializable")


#: The tiebreak order :func:`_json_key_sort_key` uses when two keys of
#: different types write the same JSON text (e.g. the string ``"true"``
#: beside the bool ``True``) -- arbitrary but fixed, so the order is total
#: and the same on both engines.
_KEY_TYPE_RANK = {bool: 0, int: 1, float: 2, type(None): 3, str: 4}


def _json_key_text(key: Any) -> str:
    """The JSON string ``json.dumps`` would write for a dict *key* --
    ``"true"``/``"false"``/``"null"`` for a bool/``None``, decimal text
    for an int or float, or the string itself.
    """
    if isinstance(key, str):
        return key
    if key is True:
        return "true"
    if key is False:
        return "false"
    if key is None:
        return "null"
    if isinstance(key, int):
        return str(key)
    if isinstance(key, float):
        return json.dumps(key)
    raise TypeError(f"keys must be str, int, float, bool or None, not {type(key).__name__}")


def _json_key_sort_key(key: Any) -> tuple[str, int]:
    """*key*'s own sort key for :func:`canonical_key_order`: the JSON
    text it would write, then its type's rank in :data:`_KEY_TYPE_RANK`
    as a tiebreak for two keys whose JSON text is the same.

    The text is computed first: a key of a type JSON can't write at all
    (a YAML 1.1 date, ``!!set``'s own ``set``, ...) then fails exactly as
    it already does for the un-pre-sorted ``--out`` write --
    :func:`_json_key_text`'s own ``TypeError`` -- instead of a
    ``KeyError`` from looking its type up in :data:`_KEY_TYPE_RANK`
    first.
    """
    text = _json_key_text(key)
    rank = _KEY_TYPE_RANK[bool if isinstance(key, bool) else type(key)]
    return (text, rank)


def canonical_key_order(value: Any) -> Any:
    """*value* with every dict's keys reordered by the JSON text each key
    would write, not by the keys themselves -- so a dict mixing key types
    that ``<`` can't compare (e.g. a YAML 1.1 ``on:`` key read as the bool
    ``True`` beside a string key) still sorts, deterministically, instead
    of ``json.dumps(..., sort_keys=True)``'s own ``TypeError`` for that
    case (#810; ``yaml.dump``, which this replaced in one caller, tolerated
    it). For a dict whose keys are all the same type and all strings --
    the common case -- the JSON text of a key is the key itself, so this
    sorts exactly as ``sort_keys=True`` always did. The sort is stable, so
    two keys with the same JSON text (and the same type) keep their
    relative order.

    This is the order the reflector-plan digest (``core/use.py::
    _canonical_json_digest``, #810) hashes: an all-int-keyed plan must
    keep hashing the same way it always has, so this never changes for
    that shape, even though an all-int dict's keys compare, and sort
    numerically, with plain ``sorted()`` -- see :func:`pretty_key_order`
    for the ``--pretty``-only order that prefers that numeric sort.
    """
    if isinstance(value, dict):
        items = sorted(value.items(), key=lambda kv: _json_key_sort_key(kv[0]))
        return {k: canonical_key_order(v) for k, v in items}
    if isinstance(value, list):
        return [canonical_key_order(v) for v in value]
    return value


def pretty_key_order(value: Any) -> Any:
    """*value* with every dict's keys sorted by the keys themselves --
    ``sorted(d.items())``, exactly what ``json.dumps(..., sort_keys=True)``
    already did, so an all-string dict sorts by text and an all-int (or
    all-numeric, bools included: ``bool`` is an ``int`` subclass) dict
    sorts numerically (``1, 2, 10``), never by each int's own decimal text
    (``"1", "10", "2"``).

    Only when that raises ``TypeError`` -- a dict mixing key types ``<``
    can't compare, e.g. a YAML 1.1 ``on:`` key read as the bool ``True``
    beside a string key -- does this fall back to :func:`canonical_key_order`'s
    own order: the JSON text each key would write, then its type's rank, as
    a deterministic order ``<`` itself has no opinion on (#778; a date key
    still fails the same way an un-pre-sorted ``--out`` write already does --
    the same ``TypeError`` :func:`canonical_key_order` itself raises for one).

    Used only by :func:`dumps_saved_state`'s ``--pretty`` write -- never by
    the reflector-plan digest, which must keep :func:`canonical_key_order`'s
    own order so an existing plan's digest never changes (#810).
    """
    if isinstance(value, dict):
        try:
            items = sorted(value.items(), key=lambda kv: kv[0])
        except TypeError:
            items = sorted(value.items(), key=lambda kv: _json_key_sort_key(kv[0]))
        return {k: pretty_key_order(v) for k, v in items}
    if isinstance(value, list):
        return [pretty_key_order(v) for v in value]
    return value


def dumps_saved_state(state: Any, *, pretty: bool = False) -> str:
    """Serialise run state in its saved form (see the module docstring)."""
    saved = compact_last_aliases(state)
    if pretty:
        # Pre-sorted by `pretty_key_order`, then written with
        # `sort_keys=False`: `json.dumps(..., sort_keys=True)` raises
        # `TypeError` on a dict whose keys aren't all mutually orderable
        # (e.g. a YAML 1.1 bool key beside a string key) -- state can
        # carry exactly that shape (#778).
        ordered = pretty_key_order(saved)
        return json.dumps(
            ordered, indent=2, sort_keys=False, default=json_safe_default
        )
    return json.dumps(saved, default=json_safe_default)
