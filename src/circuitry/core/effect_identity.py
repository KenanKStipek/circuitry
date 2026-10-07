"""The calling effect's stable state path, threaded through every model call.

``core.store.Store.effect_path`` already gives the canonical dotted path of
an effect *within one store* — but a `use` child and a decomposition's
generated plan each run in a fresh, unprefixed ``Store`` (isolated state,
observed only by wrapping the lifecycle callbacks; see ``core.use``'s
``_namespaced_effect_cb``), so ``store.effect_path(name)`` alone is only
the path *relative to that container*, not the globally unique path a
scripted model adapter needs to tell two calls apart.

``_PATH_PREFIX`` carries the absolute path of the innermost such container
across the call stack (empty outside one). :func:`call_path` composes it
with the current store's own relative path; :func:`nested_container` pushes
a new prefix around a `use`/decomposition child's execution, computed the
same way, so nesting composes to arbitrary depth. A `flow: tree`/`each`
loop pass and a named loop's `use` child need no special handling here —
``Store.child``/``parallel_branches`` already fold the iteration index into
the store's own prefix chain, which ``store.effect_path`` reads directly.
"""

from __future__ import annotations

from collections.abc import Iterator
from contextlib import contextmanager
from contextvars import ContextVar

from .store import Store

_PATH_PREFIX: ContextVar[str] = ContextVar("circuitry_effect_path_prefix", default="")
_CALL_PATH: ContextVar[str | None] = ContextVar("circuitry_model_call_path", default=None)


def call_path(store: Store, name: str) -> str:
    """The absolute dotted state path of effect *name* in *store*."""
    prefix = _PATH_PREFIX.get()
    local = store.effect_path(name)
    return f"{prefix}.{local}" if prefix else local


@contextmanager
def nested_container(store: Store, name: str) -> Iterator[None]:
    """Mark every effect dispatched below as nested under ``call_path(store, name)``.

    Wrap a `use`/decomposition child's execution in this so a prompt effect
    reused across sibling containers (two `use` children each with their
    own `answer` prompt, say) still gets a globally distinct path.
    """
    token = _PATH_PREFIX.set(call_path(store, name))
    try:
        yield
    finally:
        _PATH_PREFIX.reset(token)


@contextmanager
def model_call(store: Store, name: str) -> Iterator[None]:
    """Mark the model call *name* is about to make with its own absolute path.

    Wrap every adapter dispatch a model call can make — a prompt effect's
    attempt chain (including retries), a `model-mode` ``expect:`` re-ask —
    so :func:`current_call_path` is set for the duration of that one call
    and reset immediately after, never leaking into an unrelated effect's
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
