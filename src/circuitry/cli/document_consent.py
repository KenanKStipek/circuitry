"""Capability consent for a document that did not come from the user's own
disk (#275).

A file a user names by path is trusted like a script they run (#284,
``docs/threat-model.md`` §6) and is never asked about here. But Circuitry
also ships first-class ways to run a document someone else wrote —
``cof run-library`` / ``run_shared_orchestration`` (a fetched shared-library
asset), and a ``use: ref:`` child reached from *any* document, trusted or
not. Before such a document's effects can shell out, evaluate Python, write
or delete a file, or reach the network, the user gets a one-time prompt
naming the capabilities involved; a yes is recorded by the document's own
content digest, in the same store ``cof trust`` uses
(:mod:`circuitry.cli.config_trust`) under a different top-level key, so an
edited document (a changed digest) asks again.

Two call sites install the result as this run's capability ceiling
(:mod:`circuitry.capability_gate`): :func:`enforce_consent`, run once up
front for the whole document (mirrors
:func:`circuitry.cli.allowlist.check_allowlist`'s static walk — the fail-fast
half, with an optional interactive prompt), and
:meth:`circuitry.core.use.UseRuntime._check_capability_consent`, which
re-checks a ``ref:`` child as it loads (covers a ``ref:`` value only known at
render time — the run-time half, never interactive, same split the
allowlist gate makes between :mod:`circuitry.cli.allowlist` and
:mod:`circuitry.allowlist_gate`).
"""

from __future__ import annotations

import hashlib
import json
import os
import shlex
import tempfile
from collections.abc import Callable
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from ..core.cycle_check import collect_use_refs, iter_use_children, resolve_reference
from ..plugins.capabilities import capabilities_of
from .allowlist import walk_orchestration_refs
from .config_trust import _read_raw_store

#: Top-level key this module's entries live under in the shared trust store
#: (``cof trust``'s ``trusted.json``) — see :func:`_read_raw_store`.
DOCUMENT_CONSENT_KEY = "document_capabilities"
STORE_VERSION = 1

#: A callback the CLI supplies for an interactive run: *(label, capabilities)
#: -> True* to consent, *False* to refuse. ``None`` means never prompt — the
#: non-interactive refusal every surface besides an interactive ``cof run`` /
#: ``cof run-library`` gets (#275's rule 5: MCP, REST, CI, no TTY never ask).
ConsentPrompt = Callable[[str, frozenset[str]], bool]


class DocumentConsentError(ValueError):
    """A document needs capabilities nobody has consented to yet."""


@dataclass(frozen=True)
class DocumentConsentEntry:
    sha256: str
    capabilities: tuple[str, ...]
    consented_at: str


def document_digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _read_entries(store_path: Path) -> dict[str, DocumentConsentEntry]:
    raw = _read_raw_store(store_path)
    section = raw.get(DOCUMENT_CONSENT_KEY)
    if not isinstance(section, dict):
        return {}
    entries: dict[str, DocumentConsentEntry] = {}
    for digest, value in section.items():
        if isinstance(value, dict) and isinstance(value.get("capabilities"), list):
            entries[str(digest)] = DocumentConsentEntry(
                sha256=str(digest),
                capabilities=tuple(sorted(str(c) for c in value["capabilities"])),
                consented_at=str(value.get("consented_at") or ""),
            )
    return entries


def consented_capabilities(digest: str, *, store_path: Path) -> frozenset[str] | None:
    """What *digest* was consented to, or ``None`` if it was never asked about."""
    entry = _read_entries(store_path).get(digest)
    return None if entry is None else frozenset(entry.capabilities)


def record_consent(
    digest: str, capabilities: frozenset[str], *, store_path: Path
) -> DocumentConsentEntry:
    """Record that *digest* may use *capabilities*, replacing any prior entry."""
    store_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    raw = _read_raw_store(store_path)
    section = raw.get(DOCUMENT_CONSENT_KEY)
    section = dict(section) if isinstance(section, dict) else {}
    entry = DocumentConsentEntry(
        sha256=digest,
        capabilities=tuple(sorted(capabilities)),
        consented_at=datetime.now(timezone.utc).replace(microsecond=0).isoformat(),
    )
    section[digest] = {
        "capabilities": list(entry.capabilities),
        "consented_at": entry.consented_at,
    }
    raw[DOCUMENT_CONSENT_KEY] = section
    raw.setdefault("version", STORE_VERSION)
    fd, tmp_name = tempfile.mkstemp(
        dir=store_path.parent, prefix=".trusted.", suffix=".tmp"
    )
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            handle.write(json.dumps(raw, indent=2) + "\n")
        os.replace(tmp_name, store_path)
    except BaseException:
        Path(tmp_name).unlink(missing_ok=True)
        raise
    return entry


def required_capabilities(
    orch: dict[str, Any], *, root_path: Path | None, runtime: dict[str, Any] | None
) -> frozenset[str]:
    """Every capability *orch* and its statically reachable ``use`` children
    (``ref:``/``path:``/``inline:`` alike) need — the "compiled document
    including use children" #275's design asks for. Mirrors
    :func:`circuitry.cli.allowlist.check_allowlist`'s own traversal.
    """
    _adapters, tools = walk_orchestration_refs(orch)
    required = {cap for tool in tools for cap in capabilities_of(tool)}
    for _label, child in iter_use_children(orch, root_path=root_path, runtime=runtime):
        _child_adapters, child_tools = walk_orchestration_refs(
            child, include_document_adapter=False
        )
        required.update(cap for tool in child_tools for cap in capabilities_of(tool))
    return frozenset(required)


@dataclass(frozen=True)
class RefChildRequirement:
    """One ``use: ref:`` child reachable from a document, and what it needs.

    *label* and *digest* identify the child itself (its own resolved path and
    content hash) — consent for a ``ref:`` child is recorded independently of
    whatever document pulled it in, so the same library entry used from two
    different parents is asked about once.
    """

    label: str
    path: Path
    digest: str
    required: frozenset[str]


def ref_child_requirements(
    orch: dict[str, Any], *, root_path: Path | None, runtime: dict[str, Any] | None
) -> list[RefChildRequirement]:
    """Every ``use: ref:`` child statically reachable from *orch*.

    Walks ``path:``/``inline:`` edges transparently (that content is the
    same document's own, not gated) and stops recursing at each ``ref:``
    edge, which gets its own :class:`RefChildRequirement` computed over its
    *own* whole reachable subtree (so a ref child that itself pulls in
    further children is still fully covered).
    """
    from ..core.library_ref import build_registry
    from .orchestration_loader import load_orchestration_file

    registry = build_registry(runtime)
    seen: set[str] = set()
    out: list[RefChildRequirement] = []

    def visit(node: dict[str, Any], parent_dir: Path | None) -> None:
        for kind, value in collect_use_refs(node):
            resolved = resolve_reference(
                kind, value, parent_dir=parent_dir, registry=registry
            )
            if resolved is None:
                continue
            key = str(resolved)
            if key in seen:
                continue
            seen.add(key)
            try:
                child_orch = load_orchestration_file(resolved)
            except Exception:
                # Unreadable or malformed — the run's own load of this same
                # child (`UseRuntime._load_child_orch`) reports it properly,
                # respecting that child's own `on_error`/`validate:`; this
                # static pre-scan just has nothing to add from it.
                continue
            if kind == "ref":
                out.append(
                    RefChildRequirement(
                        label=key,
                        path=resolved,
                        digest=document_digest(resolved.read_bytes()),
                        required=required_capabilities(
                            child_orch, root_path=resolved, runtime=runtime
                        ),
                    )
                )
                continue
            # A path: edge stays this document's own content — keep walking
            # through it for a ref: child nested further in.
            visit(child_orch, resolved.parent)

    visit(orch, root_path.parent if root_path is not None else None)
    return out


def refusal_message(label: str, missing: frozenset[str]) -> str:
    names = ", ".join(sorted(missing))
    return (
        f"'{label}' needs capabilities nobody has consented to: {names}. "
        f"Run `cof trust {shlex.quote(label)}` to review and approve it, or "
        f"pass `--allow-capabilities {names}` for a scripted/CI run."
    )


def resolve_consent(
    label: str,
    digest: str,
    required: frozenset[str],
    *,
    store_path: Path,
    allow_capabilities: frozenset[str] | None,
    prompt: ConsentPrompt | None,
) -> frozenset[str]:
    """Ensure *required* is consented for *digest*, prompting or raising as
    needed, and return the capability set now on record for it (at least
    *required*).

    Raises :class:`DocumentConsentError` when *required* is not already
    consented, not covered by *allow_capabilities*, and either no *prompt*
    was given (a non-interactive run) or the user declined.
    """
    consented = consented_capabilities(digest, store_path=store_path) or frozenset()
    allowed_override = allow_capabilities or frozenset()
    missing = required - consented - allowed_override
    if not missing:
        return consented | allowed_override
    if prompt is None:
        raise DocumentConsentError(refusal_message(label, missing))
    if not prompt(label, required):
        raise DocumentConsentError(
            f"Not consented: '{label}' was not approved to use "
            f"{', '.join(sorted(required))}; nothing ran."
        )
    recorded = record_consent(digest, consented | required, store_path=store_path)
    return frozenset(recorded.capabilities)


def enforce_consent(
    *,
    orch: dict[str, Any],
    orchestration_path: Path,
    gate_whole_document: bool,
    runtime: dict[str, Any] | None,
    store_path: Path,
    allow_capabilities: frozenset[str] | None,
    prompt: ConsentPrompt | None,
) -> frozenset[str] | None:
    """The capability consent gate for one run, called once before it starts.

    *gate_whole_document* is True only for a document that itself did not
    come from the user's disk — today, a ``cof fetch``/``cof run-library``
    asset (``RunRequest.shared_library_metadata is not None``); False (a
    path-run or plain library-name document) skips straight to the ``ref:``
    children, which are independently in scope regardless of the parent's
    own trust.

    Returns the capability ceiling to install for this run
    (:func:`circuitry.capability_gate.install_capability_ceiling`): the
    whole-document consent when *gate_whole_document*, else ``None``
    (unrestricted at top level — a path-run document's own effects are
    never gated, per #284).
    """
    ceiling: frozenset[str] | None = None
    if gate_whole_document:
        required = required_capabilities(orch, root_path=orchestration_path, runtime=runtime)
        if required:
            digest = document_digest(orchestration_path.read_bytes())
            ceiling = resolve_consent(
                str(orchestration_path),
                digest,
                required,
                store_path=store_path,
                allow_capabilities=allow_capabilities,
                prompt=prompt,
            )
        else:
            ceiling = frozenset()

    for child in ref_child_requirements(orch, root_path=orchestration_path, runtime=runtime):
        if not child.required:
            continue
        resolve_consent(
            child.label,
            child.digest,
            child.required,
            store_path=store_path,
            allow_capabilities=allow_capabilities,
            prompt=prompt,
        )

    return ceiling
