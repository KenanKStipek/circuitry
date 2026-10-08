"""Declared prompts and ``{{> name}}`` composition (#396).

``{{> name}}`` is expanded by this module, in Circuitry's own code, before a
template ever reaches :func:`circuitry.core.templates.render_template` —
which keeps rejecting an unexpanded partial tag exactly as it does today
(chevron's own partial support is never used; see ``core.templates``'
module docstring). *name* is either:

* **A declared prompt** (``prompts:`` at the document root). Its own text is
  a template, expanded and rendered — recursively, so a declared prompt may
  include another — against the same context as the surrounding template,
  always unescaped (its tags "never escape, wherever it is included", #397),
  then spliced in.
* **An effect** — a ``yield``, or a ``prompt`` whose reply is text. Spliced
  in exactly what ``{{{prime.<name>.value}}}`` would insert at that point: a
  dotted name reaches into a nested/composed effect's own state the same way
  a bare template reference already does, and a missing/skipped/absorbed-
  failure value renders as ``""``.

Either way, the resolved text is substituted as an opaque value bound to a
synthetic context key and spliced in with a triple-brace tag — never by
pasting the resolved text into the template's own source and re-tokenizing
it — so a model reply (or a declared prompt's own text) containing literal
``{{...}}`` is inserted verbatim and never parsed as more Mustache, the same
guarantee an ordinary ``{{{x}}}`` already gives today. One trailing line
break (``\\n``/``\\r\\n``) is dropped from the resolved text before it is
spliced in, so ``{{> voice}}`` alone on its own line does not add a blank
one; nothing else about surrounding whitespace changes (no Mustache
standalone-partial re-indentation — see issue #396).
"""

from __future__ import annotations

import re
from collections.abc import Mapping
from pathlib import Path
from typing import Any

from .templates import TemplateError, render_template

__all__ = [
    "RUNTIME_CONFIG_KEY",
    "PromptCompositionError",
    "check_prompt_composition",
    "compile_declared_prompts",
    "declared_prompts",
    "referenced_prompt_file_paths",
    "render_with_composition",
]

#: The ``runtime_config`` key a document's own compiled ``prompts:`` rides
#: under — same private-key convention as
#: ``core.concurrency.RUNTIME_CONFIG_KEY``/``_orchestration_dir``. Rebuilt
#: fresh for every ``use`` child (never inherited) so "a child document run
#: with ``use`` sees only its own declared prompts" holds without any
#: special-casing at the ``use`` boundary itself.
RUNTIME_CONFIG_KEY = "_prompts"


def declared_prompts(runtime_config: Mapping[str, Any] | None) -> dict[str, str]:
    """The current document's declared prompts, from *runtime_config*."""
    if not runtime_config:
        return {}
    value = runtime_config.get(RUNTIME_CONFIG_KEY)
    return value if isinstance(value, dict) else {}


class PromptCompositionError(ValueError):
    """A ``prompts:``/``{{> name}}`` document is malformed — a ``cof check`` error."""


#: A valid ``{{> name}}`` name: a declared prompt or effect name, optionally
#: dotted (``pipeline.outline``) to reach a nested/composed effect's state.
_NAME_SHAPE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*$")

#: ``{{> name}}`` tags in a template. Mirrors chevron's own partial grammar
#: (the sigil must immediately follow ``{{``, with no leading whitespace —
#: see ``core.templates``), and the negative lookbehind excludes a stray
#: ``{{{>x}}}`` (which chevron tokenizes as an oddly-named "no escape"
#: variable, not a partial — see ``core.templates._reject_partials``).
_PARTIAL_TAG = re.compile(r"(?<!\{)\{\{>([^}]*)\}\}")

#: Effect types whose value is prose text a declared prompt/``{{> name}}``
#: may splice in. A ``prompt`` only qualifies when its reply is actually
#: text (``prompt_type`` unset or ``"text"``, and ``template``/``messages``
#: rather than something that decodes to JSON/boolean/number/array/object).
_TEXT_PRODUCING_TYPES = frozenset({"yield", "prompt"})


def compile_declared_prompts(
    orch: Mapping[str, Any],
    *,
    document_dir: Any = None,
    confinement_root: Any = None,
) -> dict[str, str]:
    """The document's ``prompts:`` map, as ``{name: raw template text}``.

    Each value is a string or ``{file: <path>}``; a file's content is read
    here (compile time), via ``core.prompt_files``. Declared-prompt text is
    not rendered here — only extracted — since it can include other
    declared prompts and effects that don't have values yet.
    """
    from .prompt_files import resolve_text_or_file

    raw = orch.get("prompts")
    if raw is None:
        return {}
    if not isinstance(raw, dict):
        raise PromptCompositionError("'prompts' must be a mapping of name to template.")

    declared: dict[str, str] = {}
    for name, value in raw.items():
        if not isinstance(name, str) or not _NAME_SHAPE.match(name) or "." in name:
            raise PromptCompositionError(
                f"'prompts' key {name!r} must be a valid name "
                "([A-Za-z_][A-Za-z0-9_]*, no '.')."
            )
        declared[name] = resolve_text_or_file(
            value,
            field=f"prompts.{name}",
            document_dir=document_dir,
            confinement_root=confinement_root,
        )
    return declared


def referenced_prompt_file_paths(
    orch: Mapping[str, Any], *, document_dir: Path, confinement_root: Path
) -> list[Path]:
    """Every ``{file: <path>}`` prompt source *orch* resolves to, best-effort.

    For anything that identifies a document by its content (``--resume``'s
    document-hash check, capability consent) — their content is as much a
    part of the document's identity as the orchestration YAML's own bytes
    (#396). Swallows every :class:`~circuitry.core.prompt_files.PromptFileError`
    rather than raising: this walk is a best-effort hashing aid, not a
    validation pass (``compile_declared_prompts``/``cof check`` already
    raise on a genuine violation, with the field name a caller here has
    lost).
    """
    from .prompt_files import PromptFileError, resolve_prompt_file_path

    paths: list[Path] = []

    def resolve(value: Any) -> None:
        if (
            isinstance(value, Mapping)
            and set(value) == {"file"}
            and isinstance(value.get("file"), str)
        ):
            try:
                paths.append(
                    resolve_prompt_file_path(
                        value["file"],
                        document_dir=document_dir,
                        confinement_root=confinement_root,
                        field="file",
                    )
                )
            except PromptFileError:
                pass

    prompts = orch.get("prompts")
    if isinstance(prompts, Mapping):
        for value in prompts.values():
            resolve(value)

    def walk(effects: Any) -> None:
        if not isinstance(effects, list):
            return
        for effect in effects:
            if not isinstance(effect, Mapping):
                continue
            resolve(effect.get("template"))
            messages = effect.get("messages")
            if isinstance(messages, list):
                for message in messages:
                    if isinstance(message, Mapping):
                        resolve(message.get("content"))
            for field in _CHILD_LISTS:
                walk(effect.get(field))

    walk(orch.get("effects") or orch.get("steps") or [])
    walk(orch.get("finally"))
    return paths


# ── static (cof check) validation ───────────────────────────────────────────


#: Container fields whose values are lists of child effect dicts, walked
#: when collecting every effect name/type in the document (not into a
#: ``use`` effect's own child document — that compiles separately).
_CHILD_LISTS = ("effects", "steps", "body", "then", "else", "finally")


def _collect_effect_records(
    effects: Any, out: dict[str, str]
) -> None:
    if not isinstance(effects, list):
        return
    for effect in effects:
        if not isinstance(effect, dict):
            continue
        name = effect.get("name")
        etype = str(effect.get("type") or "").strip().lower()
        if isinstance(name, str) and name:
            out[name] = etype
        for field in _CHILD_LISTS:
            _collect_effect_records(effect.get(field), out)


#: The exact fields ``{{> name}}`` is composed in — "prompt templates and
#: messages, yield templates, declared prompts, tool params and prompt, use
#: inputs and inline" (#396). Everything else a template renders
#: (``if``/``while`` model templates, ``expect.template``, an asset's
#: ``ref``, ``retries``, ...) is deliberately excluded here: it keeps going
#: through the unconditional partial rejection ``core.templates`` already
#: gave every template, unaffected by this module.
_COMPOSABLE_SCALAR_FIELDS = ("template", "prompt", "inline", "params_json")


def _iter_composable_strings(effect: Mapping[str, Any]) -> list[str]:
    """Every string in *effect* that ``{{> name}}`` composition actually sees.

    ``inputs`` is only ever rendered — and so only ever scanned here — on a
    ``use`` effect (``core.use._render_inputs``); a ``prompt``/``yield``
    effect's own ``inputs`` are prompt-local *values* merged into context
    as-is, never templates in their own right, so prose that happens to
    contain ``{{> ...}}``-looking text there (an example in a generated
    meta-prompt, say) is not a composition tag to validate. ``params`` is a
    ``tool`` effect's own field, so it never collides with this.
    """
    found: list[str] = []
    for field in _COMPOSABLE_SCALAR_FIELDS:
        value = effect.get(field)
        if isinstance(value, str):
            found.append(value)
    messages = effect.get("messages")
    if isinstance(messages, list):
        found.extend(
            message["content"]
            for message in messages
            if isinstance(message, Mapping) and isinstance(message.get("content"), str)
        )
    params = effect.get("params")
    if params is not None:
        found.extend(_walk_strings(params))
    etype = str(effect.get("type") or "").strip().lower()
    if etype == "use":
        inputs = effect.get("inputs")
        if inputs is not None:
            found.extend(_walk_strings(inputs))
    return found


def _walk_strings(value: Any) -> list[str]:
    found: list[str] = []
    if isinstance(value, str):
        found.append(value)
    elif isinstance(value, Mapping):
        for sub in value.values():
            found.extend(_walk_strings(sub))
    elif isinstance(value, (list, tuple)):
        for sub in value:
            found.extend(_walk_strings(sub))
    return found


def _effect_is_text_producing(effect_type: str, effect: Mapping[str, Any]) -> bool:
    if effect_type == "yield":
        return True
    if effect_type != "prompt":
        return False
    prompt_type = effect.get("prompt_type")
    return prompt_type is None or prompt_type == "text"


def partial_references(text: str) -> set[str]:
    """Every name a ``{{> name}}`` tag in *text* names, for reference analysis
    (tree-flow sibling warnings, ``cof check``, ``--resume``) alongside the
    existing ``prime.<name>`` scan those already do."""
    return {m.group(1).strip() for m in _PARTIAL_TAG.finditer(text)}


def check_prompt_composition(
    orch: Mapping[str, Any], *, declared: Mapping[str, str]
) -> list[str]:
    """``cof check`` errors for ``prompts:``/``{{> name}}`` in *orch*.

    Checks, across every template-bearing string in the document (effects
    and declared prompts alike): an unknown name, a name that is both
    declared and an effect, a named effect that is neither a ``yield`` nor a
    text ``prompt``, and a cycle among declared prompts. A dotted name
    (``pipeline.outline``) is only checked by its first segment — the named
    top-level effect — the same shallow-path trust ``{{prime.x.y}}`` already
    gets elsewhere in this codebase; what a composed/nested effect exposes
    under that segment is a run-time concern, same as any other dotted
    state reference.
    """
    errors: list[str] = []

    effect_records: dict[str, str] = {}
    _collect_effect_records(orch.get("effects") or orch.get("steps") or [], effect_records)
    _collect_effect_records(orch.get("finally"), effect_records)

    overlap = sorted(set(declared) & set(effect_records))
    errors.extend(
        f"'{name}' is both a declared prompt and an effect name — "
        "'{{> " + name + "}}' would be ambiguous."
        for name in overlap
    )

    def check_name(name: str, *, where: str) -> None:
        head = name.split(".", 1)[0]
        if head in declared:
            if head in overlap:
                return  # already reported once, above
            return
        if head in effect_records:
            if head in overlap:
                return
            etype = effect_records[head]
            if etype not in _TEXT_PRODUCING_TYPES or (
                etype == "prompt"
                and not _effect_is_text_producing(etype, _find_effect(orch, head))
            ):
                errors.append(
                    f"{where}: '{{{{> {name}}}}}' names effect '{head}' "
                    f"(type '{etype}'), which is neither a 'yield' nor a "
                    "text 'prompt'."
                )
            return
        errors.append(f"{where}: '{{{{> {name}}}}}' does not name a declared prompt or effect.")

    def walk(effects: Any, container_path: str) -> None:
        if not isinstance(effects, list):
            return
        for idx, effect in enumerate(effects):
            if not isinstance(effect, dict):
                continue
            effect_path = f"{container_path}[{idx}]"
            for text in _iter_composable_strings(effect):
                for name in sorted(partial_references(text)):
                    if not _NAME_SHAPE.match(name):
                        errors.append(
                            f"{effect_path}: '{{{{> {name}}}}}' is not a valid name."
                        )
                        continue
                    check_name(name, where=effect_path)
            for field in _CHILD_LISTS:
                walk(effect.get(field), f"{effect_path}.{field}")

    walk(orch.get("effects") or orch.get("steps") or [], "effects")
    walk(orch.get("finally"), "finally")

    for prompt_name, text in declared.items():
        for name in sorted(partial_references(text)):
            if not _NAME_SHAPE.match(name):
                errors.append(f"prompts.{prompt_name}: '{{{{> {name}}}}}' is not a valid name.")
                continue
            check_name(name, where=f"prompts.{prompt_name}")

    errors.extend(_declared_prompt_cycles(declared))
    return errors


def _find_effect(orch: Mapping[str, Any], name: str) -> dict[str, Any]:
    found: dict[str, Any] = {}

    def walk(effects: Any) -> None:
        if not isinstance(effects, list) or found:
            return
        for effect in effects:
            if not isinstance(effect, dict) or found:
                continue
            if effect.get("name") == name:
                found.update(effect)
                return
            for field in _CHILD_LISTS:
                walk(effect.get(field))

    walk(orch.get("effects") or orch.get("steps") or [])
    if not found:
        walk(orch.get("finally"))
    return found


def _dfs_find_cycle(
    node: str,
    *,
    graph: Mapping[str, set[str]],
    visited: set[str],
    on_path: set[str],
    path: list[str],
) -> str | None:
    """The node that closes a cycle reachable from *node*, or ``None``.

    ``visited``/``on_path``/``path`` are mutated in place — a plain
    depth-first search, with ``on_path``/``path`` unwound on the way back
    out of a branch that didn't find one.
    """
    if node in on_path:
        return node
    if node in visited:
        return None
    visited.add(node)
    on_path.add(node)
    path.append(node)
    for neighbor in sorted(graph.get(node, ())):
        cycle_at = _dfs_find_cycle(
            neighbor, graph=graph, visited=visited, on_path=on_path, path=path
        )
        if cycle_at is not None:
            return cycle_at
    path.pop()
    on_path.discard(node)
    return None


def _declared_prompt_cycles(declared: Mapping[str, str]) -> list[str]:
    """One error per cycle found among declared prompts' own ``{{> name}}`` refs."""
    graph: dict[str, set[str]] = {
        name: {ref for ref in partial_references(text) if ref in declared}
        for name, text in declared.items()
    }
    errors: list[str] = []
    visited: set[str] = set()

    for start in sorted(graph):
        if start in visited:
            continue
        path: list[str] = []
        cycle_start = _dfs_find_cycle(
            start, graph=graph, visited=visited, on_path=set(), path=path
        )
        if cycle_start is not None:
            idx = path.index(cycle_start) if cycle_start in path else 0
            cycle = [*path[idx:], cycle_start]
            errors.append("cycle among declared prompts: " + " -> ".join(cycle))
    return errors


# ── runtime expansion ────────────────────────────────────────────────────────


def _drop_one_trailing_newline(text: str) -> str:
    if text.endswith("\r\n"):
        return text[:-2]
    if text.endswith("\n"):
        return text[:-1]
    return text


def _dotted_get(root: Any, path: list[str]) -> Any:
    current = root
    for part in path:
        if isinstance(current, Mapping):
            if part not in current:
                return None
            current = current[part]
        elif isinstance(current, (list, tuple)):
            try:
                current = current[int(part)]
            except (ValueError, IndexError):
                return None
        else:
            return None
    return current


def _resolve_partial_text(
    name: str,
    *,
    ctx: Mapping[str, Any],
    declared: Mapping[str, str],
    label: str,
    seen: frozenset[str],
) -> str:
    if not _NAME_SHAPE.match(name):
        raise TemplateError(f"{label}: '{{{{> {name}}}}}' is not a valid name.")

    head = name.split(".", 1)[0]
    if "." not in name and head in declared:
        if name in seen:
            raise TemplateError(
                f"{label}: cycle among declared prompts at '{name}'."
            )
        resolved, extra = _expand(
            declared[name], ctx=ctx, declared=declared, label=f"prompts.{name}", seen=seen | {name}
        )
        merged_ctx = {**ctx, **extra} if extra else ctx
        text = render_template(resolved, merged_ctx, label=f"prompts.{name}", escape=False)
    else:
        # Not a declared prompt: must be an effect reference. ``head`` has
        # to at least exist as a node in ``ctx['prime']`` — written the
        # moment an effect starts (``Store.fire_effect_start``), including a
        # disabled/skipped one or one absorbed by its own ``on_error`` — for
        # the rest of the path to legitimately resolve to "" (a skipped
        # effect, one absorbed by ``on_error: continue``, per #396). A name
        # that was never anything is a run-time "unknown name" failure
        # (``cof check`` catches it first for anything static; this is the
        # backstop for a generated plan or a ``use: inline`` child's own
        # dynamically built text, which can carry a name that is nobody's
        # effect at all).
        prime_ns = ctx.get("prime") if isinstance(ctx, Mapping) else None
        if not (isinstance(prime_ns, Mapping) and head in prime_ns):
            raise TemplateError(
                f"{label}: '{{{{> {name}}}}}' does not name a declared prompt or effect."
            )
        value = _dotted_get(ctx, ["prime", *name.split("."), "value"])
        text = "" if value is None else str(value)
    return _drop_one_trailing_newline(text)


def _expand(
    template: str,
    *,
    ctx: Mapping[str, Any],
    declared: Mapping[str, str],
    label: str,
    seen: frozenset[str],
) -> tuple[str, dict[str, str]]:
    extra: dict[str, str] = {}
    counter = 0

    def replace(match: re.Match[str]) -> str:
        nonlocal counter
        name = match.group(1).strip()
        text = _resolve_partial_text(
            name, ctx=ctx, declared=declared, label=label, seen=seen
        )
        key = f"__circuitry_partial_{counter}__"
        counter += 1
        extra[key] = text
        return f"{{{{{{{key}}}}}}}"

    rewritten = _PARTIAL_TAG.sub(replace, template)
    return rewritten, extra


def render_with_composition(
    template: str,
    ctx: Mapping[str, Any],
    *,
    declared: Mapping[str, str] | None = None,
    label: str = "template",
    escape: bool = True,
) -> str:
    """Render *template* with every ``{{> name}}`` expanded first (#396).

    *declared* is the document's own ``prompts:`` registry (empty/``None``
    outside a document that declares any, or for a child ``use`` document
    with none of its own — see the ``_prompts`` runtime-config key). Every
    other tag in *template* keeps rendering exactly as :func:`render_template`
    already does, including *escape*.
    """
    rewritten, extra = _expand(
        template, ctx=ctx, declared=declared or {}, label=label, seen=frozenset()
    )
    merged_ctx = {**ctx, **extra} if extra else ctx
    return render_template(rewritten, merged_ctx, label=label, escape=escape)
