"""Optional model enumeration: adapters that can say what they offer.

An adapter may implement ``list_models() -> list[str]`` to name the models
a user can actually pick — Ollama's installed tags, CyberDiner's tiers, a
hosted provider's current model strings. It is optional in exactly the
same sense as :func:`~circuitry.preflight.call_check`'s ``check()``:
adapters written before this hook existed keep working, and a missing
method means "I don't know", not an error.

Callers go through :func:`call_list_models`, which never raises and never
returns anything but a clean, de-duplicated list of non-empty strings. A
model picker is a convenience — an unreachable daemon or a third-party
adapter returning nonsense must degrade to "no suggestions", never to a
crash in the UI that asked.
"""

from __future__ import annotations

from typing import Any, Protocol, runtime_checkable

__all__ = ["MAX_MODELS", "ModelLister", "call_list_models", "list_adapter_models"]

#: Upper bound on suggestions handed back to a UI. A dropdown is not a
#: catalogue browser, and a misbehaving endpoint should not be able to
#: paste ten thousand rows into the TUI.
MAX_MODELS = 500


@runtime_checkable
class ModelLister(Protocol):
    """Structural type for adapters that enumerate their models.

    Deliberately separate from :class:`~circuitry.adapters.base.Adapter`:
    folding ``list_models`` into that Protocol would make it mandatory for
    every adapter in the tree and for every out-of-tree one.
    """

    def list_models(self) -> list[str]: ...


def call_list_models(instance: Any) -> list[str]:
    """Model names ``instance`` offers, or ``[]`` if it cannot say.

    Backwards-compat shim mirroring
    :func:`~circuitry.preflight.call_check`. Absent method, wrong return
    type, or any exception (an unreachable local daemon is the common
    case) all collapse to the empty list.
    """
    list_fn = getattr(instance, "list_models", None)
    if list_fn is None or not callable(list_fn):
        return []
    try:
        result = list_fn()
    except Exception:
        # A picker asking "what have you got?" must not be able to fail.
        return []
    if not isinstance(result, (list, tuple)):
        return []

    seen: set[str] = set()
    names: list[str] = []
    for item in result:
        if not isinstance(item, str):
            continue
        name = item.strip()
        if not name or name in seen:
            continue
        seen.add(name)
        names.append(name)
        if len(names) >= MAX_MODELS:
            break
    return names


def list_adapter_models(*, adapter_name: str, runtime: dict[str, Any]) -> list[str]:
    """Build ``adapter_name`` from config and ask it for its models.

    Same forgiving contract as :func:`call_list_models`: an unknown name,
    an adapter that cannot be built from config alone (``host_claude``),
    or a backend that is simply not running yields ``[]``.
    """
    from .factory import build_adapter

    try:
        adapter = build_adapter(adapter_name=adapter_name, runtime=runtime)
    except Exception:
        return []
    return call_list_models(adapter)
