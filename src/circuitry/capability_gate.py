"""Run-time ceiling for a generated plan's tool capabilities (#275).

:mod:`circuitry.cli.document_consent` is the static half: it works out what
capabilities a document (and its ``use`` children) need and gates the first
run of one that did not come from the user's own disk on an explicit yes.
This module is the run-time half, the same split
:mod:`circuitry.allowlist_gate` makes for adapter/tool allowlists: once a
document's capabilities are settled, a plan an LLM generates inside it
(reflector, decomposition) must stay inside that same set — it never gets
its own consent prompt, only a ceiling it cannot exceed.

The ceiling lives in the run's shared ``runtime_config`` under
:data:`CAPABILITY_CEILING_KEY`, installed once by
:func:`circuitry.cli.document_consent.enforce_consent` for a document that
went through the consent gate (``None`` for one that did not — a path-run
document stays unrestricted, matching the rest of #275's scope).
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

#: ``runtime_config`` key holding the run's capability ceiling: a list of
#: capability names, or absent entirely when no ceiling applies. Mirrors
#: ``allowlist_gate.ALLOWLISTS_KEY`` — set by the consent gate, never by a
#: document.
CAPABILITY_CEILING_KEY = "_capability_ceiling"

_UNSET = object()


class CapabilityConsentError(ValueError):
    """A document or generated plan needs a capability nobody consented to."""


def install_capability_ceiling(
    runtime_config: dict[str, Any], ceiling: frozenset[str] | None
) -> None:
    """Record the run's capability ceiling. ``None`` means unrestricted."""
    if ceiling is not None:
        runtime_config[CAPABILITY_CEILING_KEY] = sorted(ceiling)


def capability_ceiling(runtime_config: Mapping[str, Any] | None) -> frozenset[str] | None:
    """The installed ceiling, or ``None`` when this run has none."""
    entry = (runtime_config or {}).get(CAPABILITY_CEILING_KEY, _UNSET)
    if entry is _UNSET or entry is None:
        return None
    return frozenset(entry) if isinstance(entry, list) else None


def capabilities_beyond_ceiling(
    required: frozenset[str], runtime_config: Mapping[str, Any] | None
) -> frozenset[str]:
    """*required* capabilities this run's ceiling (if any) does not cover."""
    ceiling = capability_ceiling(runtime_config)
    if ceiling is None:
        return frozenset()
    return required - ceiling


def require_within_ceiling(
    label: str, required: frozenset[str], runtime_config: Mapping[str, Any] | None
) -> None:
    """Raise :class:`CapabilityConsentError` unless *required* fits the ceiling."""
    beyond = capabilities_beyond_ceiling(required, runtime_config)
    if beyond:
        ceiling = capability_ceiling(runtime_config) or frozenset()
        raise CapabilityConsentError(
            f"{label} needs {', '.join(sorted(beyond))}, beyond this document's "
            f"consented capabilities ({', '.join(sorted(ceiling)) or 'none'})."
        )
