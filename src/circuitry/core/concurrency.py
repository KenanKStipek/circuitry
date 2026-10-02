"""Run-wide concurrency limits: a global cap plus named resource groups (#274).

Only leaf effects (tool, prompt) ever hold a slot — see
:class:`RunConcurrencyLimiter`. A single instance is built once per run (see
``cli.runtime_shim.run``) and threaded through every ``*Runtime`` the same
way ``runtime_config`` already is: stashed under the private
``_concurrency_limiter`` key of that same dict, which every container
(dynamic/loop/conditional/reflector/use) already passes down to its children
by reference, including across a ``use`` effect's child orchestration. A
``runtime_config`` with no such key means no run-wide limiter is configured
— a leaf effect dispatches immediately, exactly as it did before this
module existed.
"""

from __future__ import annotations

import threading
from collections.abc import Callable, Iterator, Mapping
from contextlib import contextmanager
from typing import Any

#: The dict key under which the run's one :class:`RunConcurrencyLimiter` (if
#: any) rides inside every ``runtime_config`` — see the module docstring.
RUNTIME_CONFIG_KEY = "_concurrency_limiter"

#: The ``on_wait``/``on_acquired`` label used for the run-wide cap, as
#: opposed to a named group's own name.
GLOBAL_RESOURCE_LABEL = "global"


class UnknownConcurrencyGroupError(ValueError):
    """A ``group:`` names a group ``runtime.concurrency_groups`` doesn't define.

    Raised at ``cof check`` time for a statically known document (see
    ``core.compiler.unknown_concurrency_group_errors``) and, as a backstop,
    here at dispatch time for anything that reaches a leaf effect without
    having gone through that check first (a ``use`` child, inline-generated
    YAML, or a definition built directly rather than compiled from a
    document).
    """


def parse_max_concurrency(value: Any) -> tuple[int | None, list[str]]:
    """Validate ``runtime.max_concurrency``. Returns ``(value, errors)``."""
    if value is None:
        return None, []
    if isinstance(value, bool) or not isinstance(value, int) or value < 1:
        return None, [
            f"runtime.max_concurrency must be a positive integer, got {value!r}."
        ]
    return value, []


def parse_concurrency_groups(value: Any) -> tuple[dict[str, int], list[str]]:
    """Validate ``runtime.concurrency_groups``. Returns ``(groups, errors)``."""
    if value is None:
        return {}, []
    if not isinstance(value, Mapping):
        return {}, [
            (
                "runtime.concurrency_groups must be a mapping of group name to a "
                f"positive integer limit, got {type(value).__name__}."
            )
        ]
    groups: dict[str, int] = {}
    errors: list[str] = []
    for name, limit in value.items():
        if not isinstance(name, str) or not name.strip():
            errors.append(
                f"runtime.concurrency_groups has a non-string or empty group "
                f"name: {name!r}."
            )
            continue
        if isinstance(limit, bool) or not isinstance(limit, int) or limit < 1:
            errors.append(
                f"runtime.concurrency_groups.{name} must be a positive "
                f"integer, got {limit!r}."
            )
            continue
        groups[name] = limit
    return groups, errors


class RunConcurrencyLimiter:
    """Run-wide concurrency gate: a global cap plus named resource groups.

    Shared by every leaf effect (tool, prompt) dispatched anywhere in a
    run — across parallel tree loops, parallel dynamic branches, and ``use``
    children. Containers (loop, dynamic, use, if/conditional, reflector)
    never acquire a slot themselves, only the leaves they eventually
    dispatch — so a parent can never hold a slot one of its own children is
    waiting on, which is what makes nesting safe from deadlock: every thread
    blocked in :meth:`acquire` is waiting only on another leaf's own
    eventual, unconditional release, never on a container that is itself
    waiting on that thread.

    Acquisition order is always the same — the global slot first, then the
    named group's — so two leaves can never hold the two resources in
    opposite order and deadlock on each other.
    """

    def __init__(
        self,
        *,
        max_concurrency: int | None = None,
        groups: Mapping[str, int] | None = None,
    ) -> None:
        self._global_sem = (
            threading.Semaphore(max_concurrency) if max_concurrency else None
        )
        self._group_sems: dict[str, threading.Semaphore] = {
            name: threading.Semaphore(limit) for name, limit in (groups or {}).items()
        }

    @classmethod
    def from_runtime_config(cls, runtime_config: Mapping[str, Any]) -> RunConcurrencyLimiter:
        """Build from a merged ``runtime_config`` dict's own keys.

        Raises ``ValueError`` (not :class:`UnknownConcurrencyGroupError` —
        there is no document here to name an unknown group against) if
        ``max_concurrency``/``concurrency_groups`` is malformed.
        """
        max_concurrency, mc_errors = parse_max_concurrency(
            runtime_config.get("max_concurrency")
        )
        groups, group_errors = parse_concurrency_groups(
            runtime_config.get("concurrency_groups")
        )
        errors = [*mc_errors, *group_errors]
        if errors:
            raise ValueError(
                "Invalid runtime concurrency configuration:\n"
                + "\n".join(f"  - {e}" for e in errors)
            )
        return cls(max_concurrency=max_concurrency, groups=groups)

    @property
    def group_names(self) -> frozenset[str]:
        return frozenset(self._group_sems)

    @contextmanager
    def acquire(
        self,
        *,
        group: str | None = None,
        on_wait: Callable[[str], None] | None = None,
        on_acquired: Callable[[], None] | None = None,
    ) -> Iterator[None]:
        """Block until a slot is free, hold it for the ``with`` body, release it.

        *on_wait* fires (at most twice: once per resource) with the
        resource's label — ``"global"`` or the group name — the moment this
        call actually has to block for it, not before; *on_acquired* fires
        right after that same resource is granted. Neither fires at all for
        a slot that was immediately free, which is the common case and
        should not spam an observer with waits that never happened.
        """
        if group is not None and group not in self._group_sems:
            known = ", ".join(sorted(self._group_sems)) or "(none configured)"
            raise UnknownConcurrencyGroupError(
                f"group {group!r} is not defined in runtime.concurrency_groups "
                f"— known groups: {known}."
            )
        held: list[threading.Semaphore] = []
        try:
            if self._global_sem is not None:
                self._acquire_one(
                    self._global_sem, GLOBAL_RESOURCE_LABEL, on_wait, on_acquired
                )
                held.append(self._global_sem)
            if group is not None:
                sem = self._group_sems[group]
                self._acquire_one(sem, group, on_wait, on_acquired)
                held.append(sem)
            yield
        finally:
            for sem in reversed(held):
                sem.release()

    @staticmethod
    def _acquire_one(
        sem: threading.Semaphore,
        label: str,
        on_wait: Callable[[str], None] | None,
        on_acquired: Callable[[], None] | None,
    ) -> None:
        if sem.acquire(blocking=False):
            return
        if on_wait is not None:
            on_wait(label)
        sem.acquire(blocking=True)
        if on_acquired is not None:
            on_acquired()
