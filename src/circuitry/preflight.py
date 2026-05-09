"""Preflight: dependency checks for adapters, tool plugins, and runtime plugins.

Each :class:`~circuitry.adapters.base.Adapter`,
:class:`~circuitry.plugins.base.ToolPlugin`, and
:class:`~circuitry.core.runtime_plugins.RuntimePlugin` may implement
``check() -> CheckResult`` to report whether its environment is ready
(env vars set, binaries on PATH, endpoints reachable, etc).

Implementations are optional (Story 1 — Task 8). The preflight walker
treats a missing ``check`` method as ``ok=True`` so external plugins
written before this hook existed continue to work.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass(frozen=True)
class CheckResult:
    """Outcome of a single extension's dependency check.

    ``missing`` items use a small grammar so the CLI can render
    actionable next-steps:

      * ``"env:VAR_NAME"`` — required environment variable not set
      * ``"binary:name"`` — executable not on ``PATH``
      * ``"library:dotted.path"`` — Python package not importable
      * ``"host:url"`` — endpoint not reachable

    ``message`` is freeform context shown alongside the missing list.
    """

    ok: bool
    missing: list[str] = field(default_factory=list)
    message: str | None = None


def call_check(instance: Any) -> CheckResult:
    """Invoke ``instance.check()`` if implemented, else default to ok.

    Backwards-compat shim for extensions that predate the preflight
    Protocol addition.
    """
    check_fn = getattr(instance, "check", None)
    if check_fn is None or not callable(check_fn):
        return CheckResult(ok=True, missing=[])
    try:
        result = check_fn()
    except Exception as exc:
        return CheckResult(
            ok=False,
            missing=[],
            message=f"check() raised {type(exc).__name__}: {exc}",
        )
    if not isinstance(result, CheckResult):
        return CheckResult(
            ok=False,
            missing=[],
            message=(
                f"check() returned {type(result).__name__}, "
                "expected CheckResult"
            ),
        )
    return result
