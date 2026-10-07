"""The calling effect's stable state path, threaded through every model call.

``core.store.Store.effect_path`` already gives the canonical dotted path of
an effect *within one store* — but a `use` child and a decomposition's
generated plan each run in a fresh ``Store``, newly rooted at the universal
``_CHILD_ROOT`` container every compiled orchestration gets (observed only
by wrapping the lifecycle callbacks; see ``core.use``'s
``_namespaced_effect_cb``), so ``store.effect_path(name)`` alone is only
the path *relative to that fresh root*, not the globally unique path a
scripted model adapter needs to tell two calls apart — and it still carries
that root's own name, which the parent's observers never see (they already
represent the child as a node of their own).

``_PATH_PREFIX`` carries the absolute path of the innermost such container
across the call stack (empty outside one). :func:`call_path` composes it
with the current store's own relative path, stripping that leading
``_CHILD_ROOT`` segment whenever a prefix is active — the same rule
``core.use``'s ``_relative_child_path`` applies to what observers see, so a
replies-file path always matches the path a run's own observability and
final ``--out`` state report. :func:`nested_container` pushes a new prefix
around a `use`/decomposition child's or a `flow: tree` loop's/dynamic's
isolated branch's execution, computed the same way, so nesting composes to
arbitrary depth. A chain-flow loop pass needs no special handling here —
``Store.child`` already folds the iteration index into the store's own
prefix chain, which ``store.effect_path`` reads directly.
"""

from __future__ import annotations

from collections.abc import Iterator
from contextlib import contextmanager
from contextvars import ContextVar

from .store import Store

_PATH_PREFIX: ContextVar[str] = ContextVar("circuitry_effect_path_prefix", default="")
_CALL_PATH: ContextVar[str | None] = ContextVar("circuitry_model_call_path", default=None)

#: Every compiled orchestration — the top-level run and every `use`/
#: decomposition child alike — is rooted under this key
#: (``compiler.compile_orchestration(..., root_name="prime")``). A child's
#: own absolute path therefore always begins with it; stripped here, the
#: same way ``core.use``'s ``_relative_child_path`` strips it for observers,
#: so a model-call path never doubles it up (``prime.first.prime.answer``)
#: once an outer container has already pushed its own prefix.
_CHILD_ROOT = "prime"


def _without_child_root(path: str) -> str:
    """*path* with one leading ``_CHILD_ROOT`` segment stripped, if present."""
    if path == _CHILD_ROOT:
        return ""
    return path.removeprefix(f"{_CHILD_ROOT}.")


def call_path(store: Store, name: str | None) -> str:
    """The absolute dotted state path of effect *name* in *store*.

    *name* is ``None`` for a transparent (unnamed) conditional or loop —
    its own model-mode ``if``/``while`` decision is not a separately named
    effect, so its identity is the container's own path, ``store.own_path``,
    with nothing appended.
    """
    local = store.effect_path(name) if name is not None else store.own_path
    prefix = _PATH_PREFIX.get()
    if not prefix:
        return local
    local = _without_child_root(local)
    return f"{prefix}.{local}" if local else prefix


@contextmanager
def nested_container(store: Store, name: str | None) -> Iterator[None]:
    """Mark every effect dispatched below as nested under ``call_path(store, name)``.

    Wrap a `use`/decomposition child's execution, or a `flow: tree` loop's/
    dynamic's isolated branch dispatch, in this so a prompt effect reused
    across sibling containers (two `use` children each with their own
    `answer` prompt, say, or two branches of the same unnamed tree body)
    still gets a globally distinct path. Pass the *child* store already
    built for this container (``store.child(name)``, or the loop/dynamic's
    own ``child_store`` — the same store whose ``own_path`` the isolated
    branches below will otherwise have no way to recover) with ``name=None``
    when the caller already has it, rather than re-deriving it from *name*
    on the *outer* store, which breaks when *name* is ``None`` (an unnamed
    loop: ``store.effect_path(None)`` would format as the literal
    substring ``"None"``).
    """
    token = _PATH_PREFIX.set(call_path(store, name))
    try:
        yield
    finally:
        _PATH_PREFIX.reset(token)


@contextmanager
def model_call(store: Store, name: str | None) -> Iterator[None]:
    """Mark the model call *name* is about to make with its own absolute path.

    Wrap every adapter dispatch a model call can make — a prompt effect's
    attempt chain (including retries), a `model-mode` ``expect:`` re-ask, an
    `if`/`while` model-mode decision (``name=None`` when transparent) — so
    :func:`current_call_path` is set for the duration of that one call and
    reset immediately after, never leaking into an unrelated effect's
    dispatch on the same thread.
    """
    token = _CALL_PATH.set(call_path(store, name))
    try:
        yield
    finally:
        _CALL_PATH.reset(token)


def current_call_path() -> str | None:
    """The calling effect's absolute path, or ``None`` outside a :func:`model_call`."""
    return _CALL_PATH.get()
