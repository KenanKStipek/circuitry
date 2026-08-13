"""Cross-source resolution for `use ref:`, with recorded commit pins.

A `use` effect's `ref:` used to be a curation-only lookup. It now resolves
through the same :class:`~circuitry.cli.library_sources.LibraryRegistry` the
CLI uses, so an orchestration can compose across every configured source:

* bare refs (`utilities/critique`) search sources in precedence order;
* source-qualified refs (`hub:utilities/critique`) hit exactly that source.

Remote (github) sources are served from a SHA-pinned local cache, so every
resolution against one is recorded as a **pin** — source, ref, commit SHA, and
the cache path it resolved to. Pins land in the run state at
``runtime.library_refs``, which is what makes a re-run reproducible from cache
with the network unplugged.

A ref whose source has never been refreshed is a *configuration* failure, not a
runtime one: :func:`check_use_refs` walks the static `use` graph at
validate/preflight time and reports the exact ``cof library refresh <source>``
command needed, so a run never dies half-way through for a missing cache.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import TYPE_CHECKING, Any, Optional

if TYPE_CHECKING:  # pragma: no cover - typing only
    from ..cli.library_sources import LibraryRegistry, LibrarySource

#: Key under which pins accumulate on the shared mutable runtime config.
#: Private (leading underscore) by the same convention as `_use_call_stack`.
PINS_KEY = "_library_pins"

#: Where pins surface in the serialised run state.
STATE_KEY = "library_refs"


class LibraryRefError(ValueError):
    """A `ref:` that cannot resolve *yet* — the fix is a command, not an edit."""


@dataclass(frozen=True)
class ResolvedRef:
    """A resolved `use ref:`, plus everything needed to reproduce it."""

    ref: str
    source: str
    path: Path
    sha: Optional[str] = None
    cache_path: Optional[str] = None
    ambiguous_sources: list[str] = field(default_factory=list)

    def as_pin(self) -> dict[str, Any]:
        """The state-shaped record written to ``runtime.library_refs``."""
        return {
            "ref": self.ref,
            "source": self.source,
            "path": str(self.path),
            "sha": self.sha,
            "cache_path": self.cache_path,
            "resolved_at": datetime.now(timezone.utc).isoformat(),
        }


def build_registry(runtime: Optional[dict[str, Any]] = None) -> "LibraryRegistry":
    """A registry for the given runtime config (curation-only when absent)."""
    from ..cli.library_sources import LibraryRegistry

    return LibraryRegistry.from_runtime(runtime)


def resolve_ref(
    ref: str,
    *,
    runtime: Optional[dict[str, Any]] = None,
    registry: Optional["LibraryRegistry"] = None,
) -> Optional[ResolvedRef]:
    """Resolve a bare or source-qualified `ref:` across configured sources.

    Returns ``None`` when nothing matches, and raises
    :class:`LibraryRefError` when the only reason it could not match is a
    source that has never been fetched — that distinction is what lets
    preflight fail loudly on a stale cache while leaving genuine typos to the
    caller's own "not found" message.
    """
    registry = registry if registry is not None else build_registry(runtime)
    source_name, _bare = registry.split_ref(ref)

    if source_name is not None:
        source = registry.get_source(source_name)
        if source is not None:
            _require_fetched(ref, source)

    resolution = registry.resolve(ref)
    path = resolution.path if resolution is not None else None
    if resolution is None or path is None:
        # Nothing matched. If any candidate source cannot serve entries yet,
        # that is the actionable cause — surface it instead of "not found".
        unfetched = _unfetched(registry, only=source_name)
        if unfetched:
            raise LibraryRefError(_refresh_message(ref, unfetched))
        return None

    entry = resolution.entry
    source = registry.get_source(entry.source)
    sha, cache_path = _pin_fields(source, path)
    return ResolvedRef(
        ref=ref,
        source=entry.source,
        path=path,
        sha=sha,
        cache_path=cache_path,
        ambiguous_sources=list(resolution.ambiguous_sources),
    )


def record_pin(runtime_config: Optional[dict[str, Any]], resolved: ResolvedRef) -> None:
    """Append a pin to the run's shared runtime config, deduped by identity.

    The runtime config dict is threaded through every nested `use`, so pins
    recorded deep in a chain still reach the root run state.
    """
    if runtime_config is None:
        return
    pins = runtime_config.setdefault(PINS_KEY, [])
    if not isinstance(pins, list):  # pragma: no cover - defensive
        return
    identity = (resolved.ref, resolved.source, str(resolved.path))
    for existing in pins:
        if not isinstance(existing, dict):
            continue
        if (existing.get("ref"), existing.get("source"), existing.get("path")) == identity:
            return
    pins.append(resolved.as_pin())


def collect_pins(runtime_config: Optional[dict[str, Any]]) -> list[dict[str, Any]]:
    """Every pin recorded during a run, in resolution order."""
    if not runtime_config:
        return []
    pins = runtime_config.get(PINS_KEY)
    if not isinstance(pins, list):
        return []
    return [dict(p) for p in pins if isinstance(p, dict)]


def check_use_refs(
    orch: dict[str, Any],
    *,
    root_path: Optional[Path] = None,
    runtime: Optional[dict[str, Any]] = None,
) -> list[tuple[str, str]]:
    """Walk the static `use` graph, reporting refs that need a refresh.

    Returns ``(ref, message)`` pairs — empty when every reachable `ref:` can be
    served from what is on disk right now. Unresolvable refs are *not* reported
    here: only a never-fetched source is, because that is the failure a command
    can fix. Traversal follows resolved children across sources (cache paths
    included) so a ref buried three orchestrations deep still fails early.
    """
    from .cycle_check import collect_use_refs, load_orch, resolve_reference

    registry = build_registry(runtime)
    problems: list[tuple[str, str]] = []
    seen_refs: set[str] = set()
    visited: set[str] = set()

    def walk(node: dict[str, Any], parent_dir: Optional[Path]) -> None:
        for kind, value in collect_use_refs(node):
            if kind == "ref":
                if value in seen_refs:
                    continue
                seen_refs.add(value)
                try:
                    resolved = resolve_ref(value, registry=registry)
                except LibraryRefError as exc:
                    problems.append((value, str(exc)))
                    continue
                child = resolved.path if resolved is not None else None
            else:
                child = resolve_reference(
                    kind, value, parent_dir=parent_dir, registry=registry
                )
            if child is None:
                continue
            key = str(child.resolve())
            if key in visited:
                continue
            visited.add(key)
            child_orch = load_orch(child)
            if child_orch:
                walk(child_orch, child.parent)

    walk(orch, root_path.parent if root_path is not None else None)
    return problems


# ── internals ────────────────────────────────────────────────────────────────


def _require_fetched(ref: str, source: "LibrarySource") -> None:
    notice = _notice(source)
    if notice:
        raise LibraryRefError(_refresh_message(ref, [(source.name, notice)]))


def _unfetched(
    registry: "LibraryRegistry", *, only: Optional[str] = None
) -> list[tuple[str, str]]:
    out: list[tuple[str, str]] = []
    for source in registry.sources:
        if only is not None and source.name != only:
            continue
        notice = _notice(source)
        if notice:
            out.append((source.name, notice))
    return out


def _notice(source: "LibrarySource") -> Optional[str]:
    notice = getattr(source, "notice", None)
    if not callable(notice):
        return None
    message = notice()
    return str(message) if message else None


def _refresh_message(ref: str, unfetched: list[tuple[str, str]]) -> str:
    commands = " ".join(f"`cof library refresh {name}`" for name, _ in unfetched)
    detail = " ".join(notice for _, notice in unfetched)
    return (
        f"use ref {ref!r} cannot be resolved: {detail} "
        f"Run {commands} before this orchestration runs."
    )


def _pin_fields(
    source: Optional["LibrarySource"], path: Path
) -> tuple[Optional[str], Optional[str]]:
    """Commit SHA and cache directory, for sources that pin one (github)."""
    if source is None:
        return (None, None)
    sha = getattr(source, "cached_sha", None)
    if not isinstance(sha, str) or not sha:
        return (None, None)
    entries_dir = getattr(source, "entries_dir", None)
    cache_path = str(entries_dir) if isinstance(entries_dir, Path) else str(path.parent)
    return (sha, cache_path)
