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


def dumps_saved_state(state: Any, *, pretty: bool = False) -> str:
    """Serialise run state in its saved form (see the module docstring)."""
    saved = compact_last_aliases(state)
    if pretty:
        return json.dumps(saved, indent=2, sort_keys=True)
    return json.dumps(saved)
