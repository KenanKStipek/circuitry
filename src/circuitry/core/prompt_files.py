"""``{file: <path>}`` prompt sources, and the project they must stay inside (#396).

A declared prompt, a ``prompt``/``yield`` effect's ``template``, or a message's
``content`` may name a file instead of carrying its text inline. The file is
read once, when the document is loaded and compiled — never mid-run — and its
text is used exactly as inline text would be.

Confinement: the path is resolved against the directory of the document that
names it, then (after symlinks are resolved) must stay inside *that
document's project* — the directory of the nearest ``circuitry.config.json``/
``config.json`` at or above the document (see
:func:`circuitry.cli.config.DEFAULT_CONFIG_FILENAMES`, duplicated here rather
than imported so this module, like the rest of ``core/``, never imports
``cli/`` at module scope), or the document's own directory when there is
none. A library document (``use: ref:``, ``cof run-library``) resolves the
same way but is confined to its source's own tree instead — the caller passes
that tree's root as *confinement_root* directly, skipping the config-file
walk documented here.

A document with no file of its own — generated at run time (a reflector or
decomposition plan, a ``use: inline`` child), or with no file at all (stdin,
an SDK string) — has no *document_dir* to resolve against, so ``file:`` is a
compile error for it; see ``core.prompt_compose`` for where that is raised.
"""

from __future__ import annotations

from pathlib import Path

#: Mirrors ``circuitry.cli.config.DEFAULT_CONFIG_FILENAMES`` — see module
#: docstring for why this isn't imported.
_CONFIG_FILENAMES = ("circuitry.config.json", "config.json")

#: A prompt file larger than this is rejected at ``cof check``/compile time.
MAX_PROMPT_FILE_BYTES = 1024 * 1024


class PromptFileError(ValueError):
    """A ``{file: <path>}`` prompt source could not be used, named by field."""


def default_project_root(document_dir: Path) -> Path:
    """The confinement ceiling for *document_dir*'s own ``file:`` references.

    Walks upward from *document_dir* for the nearest directory holding a
    ``circuitry.config.json``/``config.json``; falls back to *document_dir*
    itself when none is found anywhere above it.
    """
    current = document_dir
    while True:
        for name in _CONFIG_FILENAMES:
            if (current / name).is_file():
                return current
        if current.parent == current:
            return document_dir
        current = current.parent


def resolve_prompt_file_path(
    path_str: str,
    *,
    document_dir: Path,
    confinement_root: Path,
    field: str,
) -> Path:
    """The resolved, confined filesystem path a ``{file: <path_str>}`` names.

    *path_str* is resolved against *document_dir* (never *confinement_root*
    itself — ``../shared/x.md`` is a sibling of the document, not of the
    project root), then checked, after symlinks are resolved, against
    *confinement_root*. Raises :class:`PromptFileError`, naming *field*, for
    an absolute path, a ``{{ }}`` template tag, or a path that escapes the
    project — existence/readability is :func:`resolve_prompt_file`'s own
    concern, once it has a path to read.
    """
    if not isinstance(path_str, str) or not path_str.strip():
        raise PromptFileError(f"{field}: 'file' must be a non-empty string path.")
    if "{{" in path_str or "}}" in path_str:
        raise PromptFileError(
            f"{field}: 'file' must be a literal path — it cannot contain a "
            f"template tag: {path_str!r}."
        )
    raw = Path(path_str)
    if raw.is_absolute():
        raise PromptFileError(
            f"{field}: 'file' must be a relative path, got absolute path "
            f"{path_str!r}."
        )

    candidate = document_dir / raw
    try:
        resolved = candidate.resolve(strict=False)
    except OSError as exc:
        raise PromptFileError(f"{field}: could not resolve 'file' {path_str!r}: {exc}") from exc
    confinement_resolved = confinement_root.resolve(strict=False)

    if resolved != confinement_resolved and not resolved.is_relative_to(
        confinement_resolved
    ):
        raise PromptFileError(
            f"{field}: 'file' {path_str!r} resolves outside the project "
            f"({confinement_resolved}) — 'file:' paths must stay inside it."
        )
    return resolved


def resolve_prompt_file(
    path_str: str,
    *,
    document_dir: Path,
    confinement_root: Path,
    field: str,
) -> str:
    """Read and return the text of a ``{file: <path_str>}`` prompt source.

    Raises :class:`PromptFileError`, naming *field*, for anything
    :func:`resolve_prompt_file_path` rejects, or a file that is missing,
    unreadable, not UTF-8, or too large.
    """
    resolved = resolve_prompt_file_path(
        path_str, document_dir=document_dir, confinement_root=confinement_root, field=field
    )

    if not resolved.exists():
        raise PromptFileError(f"{field}: 'file' {path_str!r} does not exist.")
    if not resolved.is_file():
        raise PromptFileError(f"{field}: 'file' {path_str!r} is not a regular file.")

    size = resolved.stat().st_size
    if size > MAX_PROMPT_FILE_BYTES:
        raise PromptFileError(
            f"{field}: 'file' {path_str!r} is {size} bytes, over the "
            f"{MAX_PROMPT_FILE_BYTES}-byte limit for a prompt file."
        )

    try:
        return resolved.read_text(encoding="utf-8")
    except UnicodeDecodeError as exc:
        raise PromptFileError(f"{field}: 'file' {path_str!r} is not valid UTF-8: {exc}") from exc
    except OSError as exc:
        raise PromptFileError(f"{field}: 'file' {path_str!r} could not be read: {exc}") from exc


def resolve_text_or_file(
    value: object,
    *,
    field: str,
    document_dir: Path | None,
    confinement_root: Path | None,
) -> str:
    """A template/content field's text: *value* itself, or a ``{file: ...}``'s.

    *value* must be a non-empty string, or a mapping with exactly the one key
    ``file`` naming a path. Raises :class:`PromptFileError` for a ``file:``
    source when *document_dir* is ``None`` — this document was generated at
    run time or has no file of its own (see the module docstring) — and for
    any ``file:`` violation :func:`resolve_prompt_file` itself raises.
    """
    if isinstance(value, str):
        return value
    if isinstance(value, dict) and set(value) == {"file"}:
        if document_dir is None or confinement_root is None:
            raise PromptFileError(
                f"{field}: 'file:' cannot be used here — this document has no "
                "file of its own (it was generated at run time, or has no "
                "path: stdin, an SDK string, a 'use: inline' child)."
            )
        return resolve_prompt_file(
            value["file"],
            document_dir=document_dir,
            confinement_root=confinement_root,
            field=field,
        )
    raise PromptFileError(f"{field} must be a string or {{file: <path>}}.")
