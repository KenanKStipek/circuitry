"""An effect's ``timeout_ms`` read from state: ``{from: <path>, default: <int>}`` (#564).

A ``tool`` or ``prompt`` effect's ``timeout_ms`` is either an integer literal
or a by-reference mapping with the same ``{from: <path>}`` shape ``params``
and ``use`` inputs take, plus an optional ``default``. The compiler checks the
form (a path string, a positive integer default); the runtime resolves it at
each attempt's dispatch against the state the effect sees then, loop and
``use`` bindings included, and fails the effect when the value is not a
positive integer.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, TypeGuard

from .state_ns import validate_reference_path

REFERENCE_KEY = "from"
DEFAULT_KEY = "default"


@dataclass(frozen=True)
class TimeoutFrom:
    """A compiled ``timeout_ms: {from: <path>, default: <int>}``."""

    path: str
    default: int | None = None


def _is_positive_integer(value: Any) -> TypeGuard[int]:
    return isinstance(value, int) and not isinstance(value, bool) and value > 0


def compile_timeout_ms(
    raw: Any,
    *,
    effect_type: str,
    name: str,
    effect_path: str,
    loop_names: frozenset[str] = frozenset(),
) -> int | TimeoutFrom | None:
    """A literal ``timeout_ms`` as an int, a by-reference one as a :class:`TimeoutFrom`.

    A mapping must be exactly ``{from: <path>}`` or ``{from: <path>, default:
    <int>}``; the path is rooted like every other ``{from:}`` and the default
    must be a positive integer, the same rule the resolved value meets.
    """
    if raw is None:
        return None
    if not isinstance(raw, dict):
        return int(raw)
    where = f"{effect_type.capitalize()} effect '{name}' at '{effect_path}'"
    form = "an integer, or {from: <path>, default: <positive integer>}"
    keys = set(raw)
    if REFERENCE_KEY not in keys or not keys <= {REFERENCE_KEY, DEFAULT_KEY}:
        raise ValueError(
            f"{where}: 'timeout_ms' must be {form}; got keys "
            f"{sorted(str(key) for key in keys)}."
        )
    path = raw[REFERENCE_KEY]
    if not isinstance(path, str):
        raise ValueError(f"{where}: 'timeout_ms' 'from' must be a path string, got {path!r}.")
    path = path.strip()
    validate_reference_path(
        path,
        label=f"{effect_type.capitalize()} effect '{name}' timeout_ms",
        effect_path=effect_path,
        loop_names=loop_names,
    )
    default = raw.get(DEFAULT_KEY)
    if DEFAULT_KEY in raw and not _is_positive_integer(default):
        raise ValueError(
            f"{where}: 'timeout_ms' 'default' must be a positive integer, got {default!r}."
        )
    return TimeoutFrom(path=path, default=default)


def resolve_timeout_ms(
    timeout_ms: int | TimeoutFrom | None, ctx: Mapping[str, Any], *, effect_label: str
) -> int | None:
    """The effect's ``timeout_ms`` for one attempt: a literal as written, a reference read from *ctx*.

    A path that is missing (or holds ``null``) takes the ``default``, or fails
    when there is none. Anything other than a positive integer fails, naming
    the path.
    """
    if not isinstance(timeout_ms, TimeoutFrom):
        return timeout_ms
    # Imported here: ``core.use`` imports ``core.prompt``, which imports this module.
    from .use import _resolve_reference

    resolved = _resolve_reference(ctx, timeout_ms.path)
    if resolved is None:
        if timeout_ms.default is not None:
            return timeout_ms.default
        raise ValueError(
            f"{effect_label}: timeout_ms '{{from: {timeout_ms.path}}}' did not resolve to a "
            "value and has no default."
        )
    if not _is_positive_integer(resolved):
        raise ValueError(
            f"{effect_label}: timeout_ms '{{from: {timeout_ms.path}}}' resolved to "
            f"{resolved!r}; expected a positive integer."
        )
    return resolved
