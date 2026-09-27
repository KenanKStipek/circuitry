from __future__ import annotations

from typing import Any


def scope_ctx(ctx: dict[str, Any], local: dict[str, Any]) -> dict[str, Any]:
    """Layer a container's own writes over the context its children render against.

    A step reading the step before it — inside a loop iteration or an ``if``
    branch — is a *within-container* reference, and it resolves through a
    scope chain: the container's own writes so far first, then the enclosing
    scope, then root state. The overlay lands in two places so both taught
    spellings mean the same node:

    * top level, for the bare ``{{step.value}}`` form
    * inside ``prime``, for the canonical ``{{prime.step.value}}`` form (and
      its CEL twin ``state.prime.step.value``)

    Shallow by design. Nested nodes stay shared by reference, so every path
    that already resolved against the enclosing scope — ``{{prime.<dynamic>.
    <name>.value}}``, a loop's own ``prime.<loop>.iter_<N>`` subtree, root
    inputs — keeps resolving. Only the container's own names are shadowed.
    """
    if not local:
        return ctx
    merged = {**ctx, **local}
    parent = ctx.get("prime")
    merged["prime"] = {**parent, **local} if isinstance(parent, dict) else dict(local)
    return merged


def local_writes(
    state: dict[str, Any], baseline: frozenset[str], own_names: frozenset[str]
) -> dict[str, Any]:
    """The nodes written into *state* since *baseline*.

    A name already present in *baseline* still counts as local if it is one
    of *own_names* — a step that shadows an enclosing name of the same spelling
    keeps its slot: inside the container, that name means the container's own
    step, not the pre-existing one.
    """
    return {
        key: value
        for key, value in state.items()
        if key not in baseline or key in own_names
    }
