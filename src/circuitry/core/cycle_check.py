"""Static cycle detection for `use` references in orchestration graphs.

Walks `ref:` and `path:` references at validate time, building an adjacency
map and detecting cycles via DFS with three-color marking. Inline-mode subs
are excluded — their content renders at runtime, so the static graph cannot
see them.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import yaml  # type: ignore[import-untyped]


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


def _collect_use_refs(orch: dict[str, Any]) -> list[tuple[str, str]]:
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


def _resolve_reference(
    kind: str, value: str, *, parent_dir: Path | None
) -> Path | None:
    """Resolve a `ref:` / `path:` value to an absolute file path, or None if unresolvable."""
    from ..cli.registry import resolve_bundled

    if kind == "ref":
        resolved = resolve_bundled(value)
        return resolved.resolve() if resolved is not None else None

    # path / legacy orchestration: try filesystem then registry as fallback
    candidate = Path(value)
    if candidate.exists() and candidate.is_file():
        return candidate.resolve()

    if parent_dir is not None:
        relative = parent_dir / value
        if relative.exists() and relative.is_file():
            return relative.resolve()

    bundled = resolve_bundled(value)
    if bundled is not None:
        return bundled.resolve()

    return None


def detect_cycles(
    root_orch: dict[str, Any],
    *,
    root_path: Path | None = None,
) -> list[str] | None:
    """Walk the static `use:` graph from root_orch.

    Returns None if no cycle is found. Returns a list of orchestration paths
    forming the cycle if one is detected (e.g. ['/A.yml', '/B.yml', '/A.yml']).

    `root_path` is the path of the root orchestration if it was loaded from disk;
    used both as a node identity for the root and to scope parent-relative paths.
    """
    cache: dict[str, dict[str, Any]] = {}

    def load(path: Path) -> dict[str, Any]:
        key = str(path)
        if key not in cache:
            cache[key] = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
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
            return parent_chain[idx:] + [identity]
        if state == 2:
            return None

        color[identity] = 1
        parent_chain.append(identity)
        try:
            for kind, value in _collect_use_refs(orch):
                resolved = _resolve_reference(kind, value, parent_dir=parent_dir)
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
