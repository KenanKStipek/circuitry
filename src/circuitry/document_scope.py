"""Which document a tool effect belongs to, for a tool that reads a path (#580).

A tool plugin sees only its own params, not the document that carries the
effect. A plugin that reads a file the document names (``systemone``'s
``state_file``) must tell a document the caller wrote and trusts from one
that reached ``cof`` indirectly (a library name, a fetched source, a REST or
MCP caller's document), and must know the project that document lives in.

The run records both in its shared ``runtime_config`` under
:data:`DOCUMENT_SCOPE_KEY`, like :mod:`circuitry.allowlist_gate` records the
allowlists: ``cli.runtime_shim.run`` for the root document once it is
compiled, ``core.use`` again for each file-backed ``use`` child. The project
is the same directory a ``{file: ...}`` prompt source is confined to
(``core.prompt_files.default_project_root``). A document's ``runtime:`` block
cannot set the key: it is installed after the settings are resolved.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any

#: ``runtime_config`` key holding ``{"limited": bool, "project_root": str | None}``.
#: Absent when no run installed one (a plugin called directly).
DOCUMENT_SCOPE_KEY = "_document_scope"


@dataclass(frozen=True)
class DocumentScope:
    #: True for a document the caller did not name by path, and for any
    #: ``use`` child resolved through the library.
    limited: bool
    #: The project the document lives in, or ``None`` when it has none.
    project_root: Path | None


def install_document_scope(runtime_config: dict[str, Any], scope: DocumentScope) -> None:
    """Record the scope of the document whose effects run next."""
    runtime_config[DOCUMENT_SCOPE_KEY] = {
        "limited": scope.limited,
        "project_root": None if scope.project_root is None else str(scope.project_root),
    }


def document_scope(runtime_config: Mapping[str, Any] | None) -> DocumentScope | None:
    """The installed scope, or ``None`` when no run installed one."""
    entry = (runtime_config or {}).get(DOCUMENT_SCOPE_KEY)
    if not isinstance(entry, Mapping):
        return None
    root = entry.get("project_root")
    return DocumentScope(
        limited=bool(entry.get("limited")),
        project_root=Path(root) if isinstance(root, str) else None,
    )
