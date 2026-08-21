"""Disabled-effect semantics.

An effect carries ``enabled: bool`` on its ``*Definition``. The flag is set by
a profile's per-effect map (``effects.<path>.enabled: false``) via
``compiler.apply_effect_overrides``; orchestration YAML itself never sets it,
so a run without ``--profile`` behaves exactly as before.

A disabled effect is **not executed**. In its place the runtime writes a
uniform skip node::

    {"value": None, "meta": {"disabled": True, "created_at": ..., "completed_at": ...}}

which deliberately mirrors the shape ``on_error: skip`` leaves behind, so
downstream template/CEL handling is uniform. ``fire_effect_start`` /
``fire_effect_complete`` still fire for the node — observability sees the
skip rather than a gap.

Disabling a container (dynamic/loop/conditional/reflector) disables its whole
subtree: the container node is written as disabled and nothing inside it runs.
"""

from __future__ import annotations

from datetime import datetime, timezone
from typing import Any

from .store import Store

#: Marker key written into a skipped effect's ``meta``.
DISABLED_META_KEY = "disabled"


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def is_enabled(effect: Any) -> bool:
    """Return whether *effect* should execute. Missing flag means enabled."""
    return bool(getattr(effect, "enabled", True))


def is_disabled_node(node: Any) -> bool:
    """Return whether a state node was written by :func:`write_disabled_node`."""
    if not isinstance(node, dict):
        return False
    meta = node.get("meta")
    return isinstance(meta, dict) and meta.get(DISABLED_META_KEY) is True


def write_disabled_node(*, store: Store, name: str) -> dict[str, Any]:
    """Write the skip node for a disabled effect named *name* and notify hooks.

    Any pre-existing content at *name* (e.g. a value restored from persisted
    state) is replaced — the effect did not run in *this* run, so its node must
    not advertise a stale value.
    """
    node = store.ensure_dict(name)
    now = _now_iso()
    node["value"] = None
    node["meta"] = {
        DISABLED_META_KEY: True,
        "created_at": now,
        "completed_at": now,
    }
    # A skip is a zero-length effect: it still opens and closes its pair so
    # an observer counting starts against completes stays balanced.
    store.fire_effect_start(name, node)
    store.fire_effect_complete(name, node)
    if store.on_write:
        store.on_write(store.state)
    return node
