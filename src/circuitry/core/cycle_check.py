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
    chain the run's own `path:` field uses (`core.use.resolve_filesystem_use_path`,
    shared here so this walk can't drift from it; only the legacy
    `orchestration:` field falls back to the library, and doing so there
    makes the run treat the child as untrusted). An unfetched remote source
    resolves to None here — a missing cache is preflight's error to raise,
    not a cycle.
    """
    from .library_ref import LibraryRefError, build_registry, resolve_ref
    from .use import resolve_filesystem_use_path

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
    resolved_on_disk = resolve_filesystem_use_path(value, document_dir=parent_dir)
    if resolved_on_disk is not None:
        return resolved_on_disk.resolve()

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


#: Effect type -> the `on_error` values under which it absorbs a failure
#: anywhere in its own subtree, rather than letting it propagate to
#: whatever encloses it -- matching each one's own run code: a `use`
#: (`core.use.UseRuntime.execute`'s own `except`, wrapping everything from
#: resolving the child through running it) and a `dynamic`
#: (`core.dynamic.DynamicRuntime`) under `skip`/`continue`; a `loop`
#: (`core.loop.LoopRuntime`) under `break`/`continue`. An `if`/`conditional`
#: is deliberately absent: its own `on_error` only ever covers a failed
#: *condition* (`core.conditional.ConditionalRuntime._decide_and_run`'s own
#: `try`/`except` wraps the condition eval alone) -- a failure inside the
#: branch it takes propagates regardless. A `reflector` has no `on_error`
#: field at all, so it can never absorb either.
_ABSORBING_ON_ERROR = {
    "use": frozenset({"skip", "continue"}),
    "dynamic": frozenset({"skip", "continue"}),
    "loop": frozenset({"break", "continue"}),
}

#: Keys, on any effect, whose value is a list of child effects -- the same
#: set `_walk_effects` already recurses into.
_CONTAINER_CHILD_KEYS = ("effects", "then", "else", "body", "finally")


def _use_path_effects(
    orch: dict[str, Any], *, tolerant: bool = False
) -> list[tuple[dict[str, Any], bool]]:
    """Every `use` effect anywhere in *orch* naming a `path:`/legacy
    `orchestration:` child -- never a `ref:` or `inline:` one, each its own
    document (a library entry, or generated at run time) rather than a
    child of this one -- paired with whether a failure reaching it is
    absorbed rather than failing the root's own check (#832).

    *tolerant*, true when *orch* itself was reached through a `use` effect
    (possibly in an enclosing document) whose own `on_error` already
    absorbs anything inside it: carried into every effect found here, and
    OR'd, going down, with every `dynamic`/`loop` on the path whose own
    `on_error` absorbs its own subtree (see :data:`_ABSORBING_ON_ERROR`) --
    once any of them does, every effect further down inherits it, the same
    width the run's own `try`/`except` gives that container. An
    `if`/`conditional` or `reflector` never changes it, matching the run
    (see :data:`_ABSORBING_ON_ERROR`'s own docstring); a `use` effect found
    here is also never itself recursed into for further containers --
    `UseEffect` carries no child-effect-list key of its own.
    """
    out: list[tuple[dict[str, Any], bool]] = []

    def walk(effects: Any, tolerant: bool) -> None:
        if not isinstance(effects, list):
            return
        for effect in effects:
            if not isinstance(effect, dict):
                continue
            effect_type = (effect.get("type") or "").strip().lower()
            if effect_type == "use":
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
                    on_error = str(effect.get("on_error") or "fail").strip().lower()
                    out.append((effect, tolerant or on_error in _ABSORBING_ON_ERROR["use"]))
                continue
            absorbing = _ABSORBING_ON_ERROR.get(effect_type)
            if absorbing is not None:
                on_error = str(effect.get("on_error") or "fail").strip().lower()
                child_tolerant = tolerant or on_error in absorbing
            else:
                child_tolerant = tolerant
            for child_key in _CONTAINER_CHILD_KEYS:
                walk(effect.get(child_key), child_tolerant)

    walk(orch.get("effects") or orch.get("steps") or [], tolerant)
    walk(orch.get("finally"), tolerant)
    return out


def check_use_path_children(
    root_orch: dict[str, Any],
    *,
    root_path: Path | None = None,
    trusted: bool = False,
    runtime: dict[str, Any] | None = None,
    known_groups: frozenset[str] = frozenset(),
) -> tuple[list[tuple[str, str]], list[tuple[str, str]]]:
    """Every `use: path:`/legacy `orchestration:` child of *root_orch*,
    recursively, compiled the way a run of the root would compile it --
    the same resolution chain (`core.use.resolve_filesystem_use_path`),
    the same structural check a file named directly gets
    (`core.document_check.structural_errors`), the same project boundary
    and trust `core.compiler.compile_orchestration` enforces for a run's
    own child, and the same ambient `runtime.concurrency_groups` table
    (`core.compiler.unknown_concurrency_group_errors`) -- as
    ``(failures, warnings)``, each a list of ``(label, message)`` pairs,
    in discovery order. Both empty when every reachable child resolves,
    parses, validates and compiles; `cof check` fails the root on
    anything in *failures*, the same way the run itself would fail on
    reaching that child. A failure absorbed by any effect on the path
    from the root's own effect list down to it -- the failing `use`
    itself, or an enclosing `use`/`dynamic`/`loop`, in the same document
    or in one above it, whose own `on_error` absorbs a failure anywhere
    in its own subtree (see :data:`_ABSORBING_ON_ERROR`; an
    `if`/`conditional`'s own `on_error` never does, since it only ever
    covers a failed condition, never the branch it takes) -- lands in
    *warnings* instead: the run swallows that same failure there too, so
    it shouldn't fail a check the run itself would pass. A child reached
    more than once -- a diamond dependency, or two separate `use`
    effects naming the same file under a different `on_error`/`validate:`
    of their own -- is revisited once per distinct (path, tolerant,
    validate) combination it's reached under, the same way the run
    itself resolves and compiles each `use` effect's own child
    independently rather than reusing an earlier one's outcome; the same
    underlying failure found this way more than once is still reported
    exactly once, as an error if any reference to it is itself not
    tolerant, a warning only when every one of them is -- never as both.

    Unlike :func:`iter_use_path_children` (#611's secrets walk, tolerant
    of an unreachable child so a child the walk can't resolve never fails
    the root's own check on its own): that walk resolves through
    :func:`resolve_reference` rather than calling
    `resolve_filesystem_use_path` directly, swallows every load/compile
    error as "no secrets to report" rather than naming it, never applies
    `validate: false`/a schema check of its own, and always compiles with
    the root's own trust even for a legacy `orchestration:` child that
    falls back to the library -- this function's own, stricter walk is
    the one to extend if a future caller needs the two to fully agree.
    A failure here is this function's whole point (#832): resolving,
    parsing, validating and compiling a child are exactly what the real
    run does before running it, so skipping any of them silently would
    let `cof check` pass a root whose run fails. A legacy
    `orchestration:` child the filesystem can't resolve is still followed
    through the library, same as the run's own fallback, but checked
    limited (``trusted=False``) rather than inheriting the root's, since
    it is then someone else's document the moment it falls back
    (`core.use.UseRuntime.execute`'s own `child_trusted` check) -- reported
    the same way the run reports it rather than skipped silently for
    being library-resolved, and confined, when pinned, to that source's
    own cached tree (``lib.cache_path``) rather than the project
    containing the cache -- the same boundary
    `UseRuntime._confinement_root_for` gives a remote-pinned child.

    The library registry (only ever needed for that legacy fallback) is
    built lazily, on the first child that actually needs it -- building
    it up front would make a document with a malformed
    `runtime.library.sources` of its own fail this check even when
    nothing in it ever reaches the library (#345's own tolerance, which
    this walk must keep).
    """
    from ..cli.library_sources import LibrarySourceError
    from ..cli.orchestration_loader import load_orchestration_file
    from .compiler import compile_orchestration, unknown_concurrency_group_errors
    from .document_check import structural_errors
    from .library_ref import LibraryRefError, build_registry, resolve_ref
    from .prompt_files import default_project_root
    from .use import (
        orchestration_not_found_message,
        path_not_found_message,
        resolve_filesystem_use_path,
    )

    root_identity = str(root_path.resolve()) if root_path is not None else None
    seen: set[tuple[str, bool, bool]] = set()
    failures: list[tuple[str, str]] = []
    warnings: list[tuple[str, str]] = []
    #: (label, message) -> whether it is currently reported as an error --
    #: the same underlying failure, reached again through a stricter or
    #: looser `use` on another path to it, is reported exactly once, as an
    #: error if any reference to it is itself not tolerant (#832).
    reported: dict[tuple[str, str], bool] = {}
    registry_box: list[LibraryRegistry] = []

    def get_registry() -> LibraryRegistry:
        if not registry_box:
            registry_box.append(build_registry(runtime))
        return registry_box[0]

    def report(tolerant: bool, label: str, message: str) -> None:
        rkey = (label, message)
        already_error = reported.get(rkey)
        if already_error:
            return
        if already_error is False:
            if not tolerant:
                warnings.remove((label, message))
                failures.append((label, message))
                reported[rkey] = True
            return
        if tolerant:
            warnings.append((label, message))
            reported[rkey] = False
        else:
            failures.append((label, message))
            reported[rkey] = True

    def visit(
        orch: dict[str, Any],
        document_dir: Path | None,
        doc_trusted: bool,
        document_label: str | None,
        tolerant: bool,
    ) -> None:
        for effect, effect_tolerant in _use_path_effects(orch, tolerant=tolerant):
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
            confinement_override: Path | None = None
            if resolved is None and not is_path:
                try:
                    lib = resolve_ref(value, registry=get_registry())
                except (LibraryRefError, LibrarySourceError) as exc:
                    report(
                        effect_tolerant,
                        document_label or value,
                        f"Use effect '{name}': {exc}",
                    )
                    continue
                if lib is not None:
                    resolved = lib.path
                    child_trusted = False
                    if lib.cache_path:
                        confinement_override = Path(lib.cache_path)

            if resolved is None:
                message = (
                    path_not_found_message(name, value)
                    if is_path
                    else orchestration_not_found_message(name, value)
                )
                report(effect_tolerant, document_label or value, message)
                continue

            # Normalized once here, same as the run's own `identity`
            # (`UseRuntime.execute`'s `str(resolved_path.resolve())`) --
            # every later use of *resolved*, including the text embedded
            # in a validation-failure message below, is this same
            # normalized path, not the possibly cwd-relative or
            # `..`-containing one `resolve_filesystem_use_path` returned,
            # so the embedded text stays the same regardless of the
            # caller's own cwd.
            resolved = resolved.resolve()
            key = str(resolved)
            if key == root_identity:
                continue
            # Keyed on (path, tolerant, validate), not the path alone: the
            # same child reached a second time through a stricter (or
            # looser) `use`, or with `validate:` set differently, is a
            # distinct reachability context the run re-evaluates from
            # scratch every time it takes that `use` -- so this check must
            # too, rather than silently skipping the second reference
            # because the first one already resolved fine (#832). At most
            # four visits per document (two `tolerant` values by two
            # `validate` values).
            check_validate = bool(effect.get("validate", True))
            seen_key = (key, effect_tolerant, check_validate)
            if seen_key in seen:
                continue
            seen.add(seen_key)

            try:
                child_orch = load_orchestration_file(resolved)
            except Exception as exc:
                report(effect_tolerant, key, str(exc))
                continue
            if not isinstance(child_orch, dict):
                continue

            if check_validate:
                schema_errors = structural_errors(child_orch)
                if schema_errors:
                    report(
                        effect_tolerant,
                        key,
                        f"Orchestration {resolved} validation failed:\n"
                        + "\n".join(f"  - {e}" for e in schema_errors),
                    )
                    continue

            child_document_dir = resolved.parent
            try:
                child_def = compile_orchestration(
                    orch=child_orch,
                    root_name="prime",
                    document_dir=child_document_dir,
                    confinement_root=(
                        confinement_override
                        or default_project_root(child_document_dir)
                    ),
                    trusted=child_trusted,
                )
            except Exception as exc:
                report(effect_tolerant, key, str(exc))
                continue

            if known_groups:
                group_errors = unknown_concurrency_group_errors(child_def, known_groups)
                if group_errors:
                    report(
                        effect_tolerant,
                        key,
                        f"Orchestration {resolved} validation failed:\n"
                        + "\n".join(f"  - {e}" for e in group_errors),
                    )
                    continue

            visit(child_orch, child_document_dir, child_trusted, key, effect_tolerant)

    visit(
        root_orch,
        root_path.parent if root_path is not None else None,
        trusted,
        str(root_path.resolve()) if root_path is not None else None,
        False,
    )
    return failures, warnings

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
