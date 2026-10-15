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

``{file: {from: <state path>}, within: <dir>}`` (#578) is the run-time
counterpart, for a ``prompt`` effect's ``template`` and a message's
``content`` only: the path is read from state and the file when the effect
dispatches, never when the document loads. See :class:`DispatchFileSource`.
"""

from __future__ import annotations

import hashlib
import re
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any

#: Mirrors ``circuitry.cli.config.DEFAULT_CONFIG_FILENAMES`` — see module
#: docstring for why this isn't imported.
_CONFIG_FILENAMES = ("circuitry.config.json", "config.json")

#: A prompt file larger than this is rejected at ``cof check``/compile time,
#: and a dispatch-time ``file: {from: ...}`` file at the effect's dispatch.
MAX_PROMPT_FILE_BYTES = 1024 * 1024

_NO_FILE_OF_ITS_OWN = (
    "{field}: 'file:' cannot be used here — this document has no "
    "file of its own (it was generated at run time, or has no "
    "path: stdin, an SDK string, a 'use: inline' child)."
)

#: ``within: {from: input.<name>}``'s path: one name under ``input``.
_WITHIN_INPUT_PATH = re.compile(r"input\.[^.\s{}]+")


class PromptFileError(ValueError):
    """A ``{file: <path>}`` prompt source could not be used, named by field."""


def path_is_inside(path: Path, root: Path) -> bool:
    """True when *path* is *root* itself or lies under it; both already resolved."""
    return path == root or path.is_relative_to(root)


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

    if not path_is_inside(resolved, confinement_resolved):
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
    if is_dispatch_file(value):
        raise PromptFileError(
            f"{field}: 'file: {{from: <state path>}}' reads a file when the effect "
            "dispatches, and is only for a prompt effect's 'template' and "
            "messages[].content."
        )
    if isinstance(value, dict) and set(value) == {"file"}:
        if document_dir is None or confinement_root is None:
            raise PromptFileError(_NO_FILE_OF_ITS_OWN.format(field=field))
        return resolve_prompt_file(
            value["file"],
            document_dir=document_dir,
            confinement_root=confinement_root,
            field=field,
        )
    raise PromptFileError(f"{field} must be a string or {{file: <path>}}.")


@dataclass(frozen=True)
class DispatchFileSource:
    """A ``template``/``content`` of ``{file: {from: <path>}, within: <dir>}`` (#578).

    *path_from* is the state path holding the file's path. *within* (a literal
    absolute directory) or *within_from* (``input.<name>``) names the directory
    the file must stay inside. *confine_to* is, for a limited document, the
    project the resolved ``within`` itself must stay inside — the same root a
    ``{file: <path>}`` is confined to; ``None`` for a document run by path,
    which can name any directory. Kept as text so a compiled definition dumps
    to JSON.
    """

    path_from: str
    within: str | None = None
    within_from: str | None = None
    confine_to: str | None = None


def is_dispatch_file(value: object) -> bool:
    """True for the ``{file: {from: ...}, ...}`` shape, valid or not."""
    return isinstance(value, Mapping) and isinstance(value.get("file"), Mapping)


def compile_dispatch_file(
    value: Mapping[str, Any],
    *,
    field: str,
    effect_path: str,
    loop_names: frozenset[str],
    document_dir: Path | None,
    confinement_root: Path | None,
    trusted: bool,
) -> DispatchFileSource:
    """Check a ``{file: {from: <path>}, within: <dir>}`` and compile it.

    Raises :class:`PromptFileError`, naming *field*. A document with no file of
    its own (generated at run time) cannot use it, the same compile error
    ``{file: <path>}`` gives. A limited document (*trusted* false) can name a
    literal ``within`` only inside *confinement_root*; a trusted one, run by
    path, can name any directory.
    """
    # Imported here: ``state_ns`` has no business at this module's import time.
    from .state_ns import validate_reference_path

    if document_dir is None or confinement_root is None:
        raise PromptFileError(_NO_FILE_OF_ITS_OWN.format(field=field))
    if "within" not in value:
        raise PromptFileError(
            f"{field}: 'within' is required with 'file: {{from: <path>}}' — the "
            "directory the file must stay inside."
        )
    extra = sorted(str(key) for key in value if key not in ("file", "within"))
    if extra:
        raise PromptFileError(
            f"{field}: 'file: {{from: <path>}}' takes only 'file' and 'within'; got {extra}."
        )

    file_ref = value["file"]
    if set(file_ref) != {"from"} or not isinstance(file_ref["from"], str) or not file_ref["from"].strip():
        raise PromptFileError(
            f"{field}: 'file' must be a path, or {{from: <state path>}}; got {dict(file_ref)!r}."
        )
    path_from = file_ref["from"].strip()
    try:
        validate_reference_path(
            path_from, label=f"Prompt {field} file", effect_path=effect_path, loop_names=loop_names
        )
    except ValueError as exc:
        raise PromptFileError(str(exc)) from exc

    within_raw = value["within"]
    within: str | None = None
    within_from: str | None = None
    if isinstance(within_raw, str):
        if "{{" in within_raw or "}}" in within_raw:
            raise PromptFileError(
                f"{field}: 'within' must be a literal path — it cannot contain a "
                f"template tag: {within_raw!r}."
            )
        if not within_raw.strip() or not Path(within_raw).is_absolute():
            raise PromptFileError(
                f"{field}: 'within' must be an absolute path, or {{from: input.<name>}}; "
                f"got {within_raw!r}."
            )
        within = within_raw
    elif (
        isinstance(within_raw, Mapping)
        and set(within_raw) == {"from"}
        and isinstance(within_raw["from"], str)
        and _WITHIN_INPUT_PATH.fullmatch(within_raw["from"].strip())
    ):
        within_from = within_raw["from"].strip()
    else:
        raise PromptFileError(
            f"{field}: 'within' must be an absolute path, or {{from: input.<name>}} — "
            f"never an effect's output; got {within_raw!r}."
        )

    confine_to = None if trusted else str(confinement_root)
    if within is not None and confine_to is not None:
        try:
            resolved_within = Path(within).resolve(strict=False)
            resolved_root = Path(confine_to).resolve(strict=False)
        except (OSError, RuntimeError) as exc:
            raise PromptFileError(f"{field}: could not resolve 'within' {within!r}: {exc}") from exc
        if not path_is_inside(resolved_within, resolved_root):
            raise PromptFileError(
                f"{field}: 'within' {within!r} resolves outside the project "
                f"({resolved_root}) — a limited document's 'within' must stay inside it."
            )
    return DispatchFileSource(
        path_from=path_from, within=within, within_from=within_from, confine_to=confine_to
    )


@dataclass(frozen=True)
class DispatchFileText:
    """What :func:`read_dispatch_file` read: the text and its ``meta.template_files`` record."""

    text: str
    record: dict[str, Any]


def read_dispatch_file(
    source: DispatchFileSource,
    ctx: Mapping[str, Any],
    *,
    field: str,
    effect_label: str,
) -> DispatchFileText:
    """Read a :class:`DispatchFileSource` against the state *ctx* the effect sees.

    The text is used verbatim: bytes decoded as UTF-8, no newline translation,
    no rendering. Raises :class:`PromptFileError` naming *effect_label*,
    *field* and the path, for each of: a state value that is not a path
    string, a ``within`` that is not an absolute directory (or, for a limited
    document, is outside its project), no such file, not a regular file,
    outside ``within``, unreadable, not UTF-8, larger than
    :data:`MAX_PROMPT_FILE_BYTES`.
    """
    # Imported here: ``core.use`` imports ``core.prompt``, which imports this module.
    from .use import _resolve_reference

    where = f"{effect_label}: {field}"

    def fail(problem: str) -> PromptFileError:
        return PromptFileError(f"{where}: {problem}")

    raw_path = _resolve_reference(ctx, source.path_from)
    if not isinstance(raw_path, str) or not raw_path.strip():
        raise fail(
            f"'file: {{from: {source.path_from}}}' resolved to {raw_path!r}; "
            "expected a file path string."
        )

    if source.within is not None:
        within_raw: Any = source.within
        within_label = repr(source.within)
    else:
        assert source.within_from is not None
        within_raw = _resolve_reference(ctx, source.within_from)
        within_label = f"{{from: {source.within_from}}}"
    if not isinstance(within_raw, str) or not Path(within_raw).is_absolute():
        raise fail(
            f"'within' {within_label} resolved to {within_raw!r}; expected an absolute directory path."
        )
    try:
        within_dir = Path(within_raw).resolve(strict=False)
    except (OSError, RuntimeError, ValueError) as exc:
        raise fail(f"could not resolve 'within' {within_raw!r}: {exc}") from exc
    if source.confine_to is not None:
        try:
            project = Path(source.confine_to).resolve(strict=False)
        except (OSError, RuntimeError) as exc:
            raise fail(f"could not resolve the project directory: {exc}") from exc
        if not path_is_inside(within_dir, project):
            raise fail(
                f"'within' {within_raw!r} resolves outside the project ({project}) — a "
                "limited document's 'within' must stay inside it."
            )

    try:
        candidate = Path(raw_path)
        resolved = (candidate if candidate.is_absolute() else within_dir / candidate).resolve(
            strict=False
        )
    except (OSError, RuntimeError, ValueError) as exc:
        raise fail(f"could not resolve file {raw_path!r}: {exc}") from exc
    if not path_is_inside(resolved, within_dir):
        raise fail(f"file {raw_path!r} resolves outside 'within' ({within_dir}).")

    try:
        if not resolved.exists():
            raise fail(f"file {raw_path!r} does not exist.")
        if not resolved.is_file():
            raise fail(f"file {raw_path!r} is not a regular file.")
        with resolved.open("rb") as handle:
            data = handle.read(MAX_PROMPT_FILE_BYTES + 1)
    except OSError as exc:
        raise fail(f"file {raw_path!r} could not be read: {exc}") from exc
    if len(data) > MAX_PROMPT_FILE_BYTES:
        raise fail(
            f"file {raw_path!r} is over the {MAX_PROMPT_FILE_BYTES}-byte limit for a prompt file."
        )
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise fail(f"file {raw_path!r} is not valid UTF-8: {exc}") from exc
    return DispatchFileText(
        text=text,
        record={
            "field": field,
            "path": str(resolved),
            "sha256": hashlib.sha256(data).hexdigest(),
            "chars": len(text),
        },
    )
