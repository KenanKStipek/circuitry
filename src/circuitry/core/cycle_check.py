"""Static cycle detection for `use` references in orchestration graphs.

Walks `ref:` and `path:` references at validate time, building an adjacency
map and detecting cycles via DFS with three-color marking. Inline-mode subs
are excluded — their content renders at runtime, so the static graph cannot
see them.

`ref:` edges resolve through the library registry, so the graph crosses
sources: a folder orchestration referencing a github entry is followed into
that source's SHA-pinned cache path, and a cycle that closes through a remote
source is caught before anything runs.
"""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING, Any

import yaml  # type: ignore[import-untyped]

if TYPE_CHECKING:  # pragma: no cover - typing only
    from ..cli.library_sources import LibraryRegistry


class CycleDetected(ValueError):
    """Raised when a use-reference cycle is found at validate time."""


def _walk_effects(effects: Any) -> list[dict[str, Any]]:
    """Yield every effect dict found anywhere in a recursive effects tree."""
    out: list[dict[str, Any]] = []
    if not isinstance(effects, list):
        return out
    for effect in effects:
        if not isinstance(effect, dict):
            continue
        out.append(effect)
        # Recurse into nested containers
        for child_key in ("effects", "then", "else", "body"):
            child = effect.get(child_key)
            if isinstance(child, list):
                out.extend(_walk_effects(child))
    return out


def collect_use_refs(orch: dict[str, Any]) -> list[tuple[str, str]]:
    """Return a list of (kind, value) for every `use` effect referencing a file/library entry.

    Skips inline-mode uses. Kind is 'ref' or 'path'; for the deprecated
    'orchestration' field, kind is 'path' (we treat it as filesystem-or-bundled).
    """
    out: list[tuple[str, str]] = []
    effects = orch.get("effects") or orch.get("steps") or []
    for effect in _walk_effects(effects):
        if (effect.get("type") or "").strip().lower() != "use":
            continue
        ref = effect.get("ref")
        path_field = effect.get("path")
        legacy = effect.get("orchestration")
        if isinstance(ref, str) and ref.strip():
            out.append(("ref", ref.strip()))
        elif isinstance(path_field, str) and path_field.strip():
            out.append(("path", path_field.strip()))
        elif isinstance(legacy, str) and legacy.strip():
            out.append(("path", legacy.strip()))
    return out


def resolve_reference(
    kind: str,
    value: str,
    *,
    parent_dir: Path | None,
    registry: LibraryRegistry | None = None,
    runtime: dict[str, Any] | None = None,
) -> Path | None:
    """Resolve a `ref:` / `path:` value to an absolute file path, or None if unresolvable.

    `ref:` goes through the library registry (cross-source, cache paths
    included); `path:` keeps its filesystem-first chain with a library lookup
    as the final fallback. An unfetched remote source resolves to None here —
    a missing cache is preflight's error to raise, not a cycle.
    """
    from .library_ref import LibraryRefError, build_registry, resolve_ref

    if registry is None:
        registry = build_registry(runtime)

    def library_lookup(ref: str) -> Path | None:
        try:
            resolved = resolve_ref(ref, registry=registry)
        except LibraryRefError:
            return None
        return resolved.path.resolve() if resolved is not None else None

    if kind == "ref":
        return library_lookup(value)

    # path / legacy orchestration: try filesystem then the library as fallback
    candidate = Path(value)
    if candidate.exists() and candidate.is_file():
        return candidate.resolve()

    if parent_dir is not None:
        relative = parent_dir / value
        if relative.exists() and relative.is_file():
            return relative.resolve()

    return library_lookup(value)


def load_orch(path: Path) -> dict[str, Any]:
    """Parse an orchestration file, treating unreadable content as empty."""
    try:
        data = yaml.safe_load(path.read_text(encoding="utf-8"))
    except (OSError, yaml.YAMLError):
        return {}
    return data if isinstance(data, dict) else {}


def detect_cycles(
    root_orch: dict[str, Any],
    *,
    root_path: Path | None = None,
    runtime: dict[str, Any] | None = None,
) -> list[str] | None:
    """Walk the static `use:` graph from root_orch.

    Returns None if no cycle is found. Returns a list of orchestration paths
    forming the cycle if one is detected (e.g. ['/A.yml', '/B.yml', '/A.yml']).

    `root_path` is the path of the root orchestration if it was loaded from disk;
    used both as a node identity for the root and to scope parent-relative paths.
    `runtime` is the resolved runtime config; its `library.sources` decide which
    sources `ref:` edges may traverse (curation-only when omitted).
    """
    from .library_ref import build_registry

    registry = build_registry(runtime)
    cache: dict[str, dict[str, Any]] = {}

    def load(path: Path) -> dict[str, Any]:
        key = str(path)
        if key not in cache:
            cache[key] = load_orch(path)
        return cache[key]

    # Three-color DFS: 0=unvisited, 1=on-stack, 2=done.
    color: dict[str, int] = {}
    parent_chain: list[str] = []

    def visit(orch: dict[str, Any], identity: str, parent_dir: Path | None) -> list[str] | None:
        state = color.get(identity, 0)
        if state == 1:
            # Cycle: find where identity appears in chain and slice
            try:
                idx = parent_chain.index(identity)
            except ValueError:
                idx = 0
            return [*parent_chain[idx:], identity]
        if state == 2:
            return None

        color[identity] = 1
        parent_chain.append(identity)
        try:
            for kind, value in collect_use_refs(orch):
                resolved = resolve_reference(
                    kind, value, parent_dir=parent_dir, registry=registry
                )
                if resolved is None:
                    # Unresolvable reference — not a cycle issue. Skip.
                    continue
                child_identity = str(resolved)
                child_orch = load(resolved)
                child_parent_dir = resolved.parent
                cycle = visit(child_orch, child_identity, child_parent_dir)
                if cycle is not None:
                    return cycle
        finally:
            parent_chain.pop()
            color[identity] = 2

        return None

    root_identity = str(root_path.resolve()) if root_path is not None else "<root>"
    root_parent_dir = root_path.parent if root_path is not None else None
    return visit(root_orch, root_identity, root_parent_dir)
