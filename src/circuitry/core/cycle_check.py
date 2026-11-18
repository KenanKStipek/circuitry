"""Static cycle detection for `use` references in orchestration graphs.

Walks `ref:` and `path:` references at validate time, building an adjacency
map and detecting cycles via DFS with three-color marking. Inline-mode subs
are excluded — their content renders at runtime, so the static graph cannot
see them.

`ref:` edges resolve through the library registry, so the graph crosses
sources: a folder orchestration referencing a github entry is followed into
that source's SHA-pinned cache path, and a cycle that closes through a remote
source is caught before anything runs.

`iter_use_children` walks the same graph for callers that need every child
document rather than a cycle — the allowlist check in `cof check`.
`iter_use_path_children` walks only the `path:`/`orchestration:` subset —
the secrets-listing check in `cof check` (#611), since a `ref:` or `inline:`
child is always limited and so never resolves the root's own secrets.
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
        # Recurse into nested containers. 'finally' is only legal on dynamic
        # (and the document root, handled by each call site below), but a
        # use effect living in one is a real edge in the static use-graph.
        for child_key in ("effects", "then", "else", "body", "finally"):
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
    for effect in [*_walk_effects(effects), *_walk_effects(orch.get("finally"))]:
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
    allow_library_fallback: bool = True,
) -> Path | None:
    """Resolve a `ref:` / `path:` value to an absolute file path, or None if unresolvable.

    `ref:` goes through the library registry (cross-source, cache paths
    included); `path:` keeps its filesystem-first chain with a library lookup
    as the final fallback, unless *allow_library_fallback* is false — the
    chain the run's own `path:` field uses, which never consults the
    library (`core.use.UseRuntime._resolve_orchestration`; only the legacy
    `orchestration:` field falls back to it, and doing so there makes the
    run treat the child as untrusted). An unfetched remote source resolves
    to None here — a missing cache is preflight's error to raise, not a
    cycle.
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

    if not allow_library_fallback:
        return None

    return library_lookup(value)


def load_orch(path: Path) -> dict[str, Any]:
    """Parse an orchestration file, treating unreadable content as empty.

    Deliberately `yaml.safe_load`, not the run's own loader -- a `use`
    cycle through a child with a duplicate YAML key must still be found
    even though the run's own loader (`cli.orchestration_loader`) rejects
    one (#415); the cycle check reads ahead of that rejection, last-value-
    wins like plain YAML, rather than let a document hide a cycle behind a
    parse error the run hasn't reached yet.
    """
    try:
        data = yaml.safe_load(path.read_text(encoding="utf-8"))
    except (OSError, yaml.YAMLError):
        return {}
    return data if isinstance(data, dict) else {}


def _load_path_child_orch(path: Path) -> dict[str, Any]:
    """Parse a `use: path:`/`orchestration:` child the way the run loads it
    (`cli.orchestration_loader.load_orchestration_file`, which also handles
    `.json`/`.toon`), for :func:`iter_use_path_children` -- unlike
    :func:`load_orch`, this walk is never fooled by a cycle hiding behind a
    parse error (it only follows `path:`/`orchestration:` edges, which the
    compiler itself loads the same way), so matching the run's own format
    support, and its duplicate-key rejection, matters more here than
    tolerating what the run would refuse: a child the run can't load names
    no secrets either, so it's skipped exactly like an unresolvable path.
    """
    from ..cli.orchestration_loader import load_orchestration_file

    try:
        data = load_orchestration_file(path)
    except Exception:
        return {}
    return data if isinstance(data, dict) else {}


def iter_use_children(
    root_orch: dict[str, Any],
    *,
    root_path: Path | None = None,
    runtime: dict[str, Any] | None = None,
) -> list[tuple[str, dict[str, Any]]]:
    """Every orchestration statically reachable from root_orch through `use`.

    Returns ``(label, orch)`` pairs, the root excluded, each file once.
    `ref:` / `path:` children resolve exactly as :func:`detect_cycles` resolves
    them; an unresolvable reference is skipped (the run reports it). An
    `inline:` child is included when its unrendered text parses as YAML; what
    its Mustache tags render to only exists at run time, where `UseRuntime`
    checks the rendered document again.
    """
    from .library_ref import build_registry

    registry = build_registry(runtime)
    seen: set[str] = {str(root_path.resolve())} if root_path is not None else set()
    out: list[tuple[str, dict[str, Any]]] = []

    def visit(orch: dict[str, Any], parent_dir: Path | None) -> None:
        for kind, value in collect_use_refs(orch):
            resolved = resolve_reference(
                kind, value, parent_dir=parent_dir, registry=registry
            )
            if resolved is None or str(resolved) in seen:
                continue
            seen.add(str(resolved))
            child = load_orch(resolved)
            out.append((str(resolved), child))
            visit(child, resolved.parent)

        effects = orch.get("effects") or orch.get("steps") or []
        for effect in [*_walk_effects(effects), *_walk_effects(orch.get("finally"))]:
            if (effect.get("type") or "").strip().lower() != "use":
                continue
            text = effect.get("inline")
            if not isinstance(text, str) or text in seen:
                continue
            seen.add(text)
            try:
                child = yaml.safe_load(text)
            except (yaml.YAMLError, ValueError):
                # A calendar-invalid timestamp (`2024-02-30`) raises a bare
                # `ValueError`, never a `yaml.YAMLError` (`core/yaml_load.py`'s
                # own module doc) — just as unwalkable as a real parse error
                # for this scanner's own purposes, so it's skipped the same way.
                continue
            if not isinstance(child, dict):
                continue
            out.append((f"inline '{effect.get('name') or 'use'}'", child))
            # Inline children have no directory of their own; nested `path:`
            # references resolve against the enclosing document's.
            visit(child, parent_dir)

    visit(root_orch, root_path.parent if root_path is not None else None)
    return out


def iter_use_path_children(
    root_orch: dict[str, Any],
    *,
    root_path: Path | None = None,
    runtime: dict[str, Any] | None = None,
) -> list[tuple[str, dict[str, Any], Path | None]]:
    """Every orchestration reachable from root_orch by following only
    `path:`/legacy `orchestration:` `use` edges, recursively.

    Returns ``(label, orch, parent_dir)`` triples, the root excluded, each
    file once, in the same order ``iter_use_children`` would visit them.
    Never follows a ``ref:`` edge (a library entry is always limited,
    whatever its parent's trust, so it never resolves the root's own
    secrets) or an ``inline:`` one (generated at run time, also always
    limited) -- a child named by `path:` inherits its parent's trust and so
    is the only kind this walk needs for a caller that resolves what a
    trusted root's own run would (`cof check`'s secrets listing). Resolves
    both the `path:` field and the legacy `orchestration:` one from the
    filesystem only, with no library fallback -- the chain the run's own
    `path:` field uses (`core.use.UseRuntime._resolve_orchestration`). A
    legacy `orchestration:` child the filesystem can't resolve is a library
    child once the run falls back for it, pinned and so untrusted
    (`UseRuntime._pin`) -- out of scope the same way a `ref:` child is,
    rather than walked as if trusted. An unresolvable `path:` is skipped,
    same as `iter_use_children`.
    """
    from .library_ref import build_registry

    registry = build_registry(runtime)
    seen: set[str] = {str(root_path.resolve())} if root_path is not None else set()
    out: list[tuple[str, dict[str, Any], Path | None]] = []

    def visit(orch: dict[str, Any], parent_dir: Path | None) -> None:
        for kind, value in collect_use_refs(orch):
            if kind != "path":
                continue
            resolved = resolve_reference(
                kind,
                value,
                parent_dir=parent_dir,
                registry=registry,
                allow_library_fallback=False,
            )
            if resolved is None or str(resolved) in seen:
                continue
            seen.add(str(resolved))
            child = _load_path_child_orch(resolved)
            out.append((str(resolved), child, resolved.parent))
            visit(child, resolved.parent)

    visit(root_orch, root_path.parent if root_path is not None else None)
    return out


def _use_path_effects(orch: dict[str, Any]) -> list[dict[str, Any]]:
    """Every `use` effect anywhere in *orch* naming a `path:`/legacy
    `orchestration:` child -- never a `ref:` or `inline:` one, each its own
    document (a library entry, or generated at run time) rather than a
    child of this one.
    """
    out: list[dict[str, Any]] = []
    effects = orch.get("effects") or orch.get("steps") or []
    for effect in [*_walk_effects(effects), *_walk_effects(orch.get("finally"))]:
        if (effect.get("type") or "").strip().lower() != "use":
            continue
        ref = effect.get("ref")
        inline = effect.get("inline")
        if isinstance(ref, str) and ref.strip():
            continue
        if isinstance(inline, str) and inline.strip():
            continue
        path_field = effect.get("path")
        legacy = effect.get("orchestration")
        if (isinstance(path_field, str) and path_field.strip()) or (
            isinstance(legacy, str) and legacy.strip()
        ):
            out.append(effect)
    return out


def check_use_path_children(
    root_orch: dict[str, Any],
    *,
    root_path: Path | None = None,
    trusted: bool = False,
    runtime: dict[str, Any] | None = None,
) -> list[tuple[str, str]]:
    """Every `use: path:`/legacy `orchestration:` child of *root_orch*,
    recursively, compiled the way a run of the root would compile it -- the
    same resolution chain (`core.use.resolve_filesystem_use_path`), the
    same structural check a file named directly gets
    (`core.document_check.structural_errors`), and the same project
    boundary and trust `core.compiler.compile_orchestration` enforces for a
    run's own child -- as a list of ``(label, message)`` failures, in
    discovery order. Empty when every reachable child resolves, parses,
    validates and compiles; `cof check` fails the root on anything in this
    list, the same way the run itself would fail on reaching that child.

    Unlike :func:`iter_use_path_children` (#611's secrets walk, tolerant of
    an unreachable child so a child the walk can't resolve never fails the
    root's own check on its own), a failure here is this function's whole
    point (#832): resolving, parsing, validating and compiling a child are
    exactly what the real run does before running it, so skipping any of
    them silently would let `cof check` pass a root whose run fails. A
    legacy `orchestration:` child the filesystem can't resolve is still
    followed through the library, same as the run's own fallback, but
    checked limited (``trusted=False``) rather than inheriting the root's,
    since it is then someone else's document the moment it falls back
    (`core.use.UseRuntime.execute`'s own `child_trusted` check) -- reported
    the same way the run reports it rather than skipped silently for being
    library-resolved.
    """
    from ..cli.orchestration_loader import load_orchestration_file
    from .compiler import compile_orchestration
    from .document_check import structural_errors
    from .library_ref import LibraryRefError, build_registry, resolve_ref
    from .prompt_files import default_project_root
    from .use import resolve_filesystem_use_path

    registry = build_registry(runtime)
    seen: set[str] = {str(root_path.resolve())} if root_path is not None else set()
    failures: list[tuple[str, str]] = []

    def visit(orch: dict[str, Any], document_dir: Path | None, doc_trusted: bool) -> None:
        for effect in _use_path_effects(orch):
            name = effect.get("name") or "use"
            path_field = effect.get("path")
            if isinstance(path_field, str) and path_field.strip():
                is_path = True
                value = path_field.strip()
            else:
                is_path = False
                value = str(effect.get("orchestration")).strip()

            resolved = resolve_filesystem_use_path(value, document_dir=document_dir)
            child_trusted = doc_trusted
            if resolved is None and not is_path:
                try:
                    lib = resolve_ref(value, registry=registry)
                except LibraryRefError as exc:
                    failures.append((value, f"Use effect '{name}': {exc}"))
                    continue
                if lib is not None:
                    resolved = lib.path.resolve()
                    child_trusted = False

            if resolved is None:
                message = (
                    f"Use effect '{name}': path '{value}' not found "
                    "(checked absolute, cwd-relative, and parent-orchestration-relative)."
                    if is_path
                    else (
                        f"Use effect '{name}': orchestration '{value}' not found. "
                        "Provide a valid file path or a bundled orchestration name "
                        "(run `cof list` to see available names)."
                    )
                )
                failures.append((value, message))
                continue

            resolved = resolved.resolve()
            key = str(resolved)
            if key in seen:
                continue
            seen.add(key)

            try:
                child_orch = load_orchestration_file(resolved)
            except Exception as exc:
                failures.append((key, str(exc)))
                continue
            if not isinstance(child_orch, dict):
                continue

            if bool(effect.get("validate", True)):
                schema_errors = structural_errors(child_orch)
                if schema_errors:
                    failures.append(
                        (
                            key,
                            f"Orchestration {resolved} validation failed:\n"
                            + "\n".join(f"  - {e}" for e in schema_errors),
                        )
                    )
                    continue

            try:
                compile_orchestration(
                    orch=child_orch,
                    root_name="prime",
                    document_dir=resolved.parent,
                    confinement_root=default_project_root(resolved.parent),
                    trusted=child_trusted,
                )
            except Exception as exc:
                failures.append((key, str(exc)))
                continue

            visit(child_orch, resolved.parent, child_trusted)

    visit(root_orch, root_path.parent if root_path is not None else None, trusted)
    return failures


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
