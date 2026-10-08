"""Declared prompts and ``{{> name}}`` composition (#396).

``{{> name}}`` is expanded by this module, in Circuitry's own code, before a
template ever reaches :func:`circuitry.core.templates.render_template` —
which keeps rejecting an unexpanded partial tag exactly as it does today
(chevron's own partial support is never used; see ``core.templates``'
module docstring). *name* is either:

* **A declared prompt** (``prompts:`` at the document root). Its own TEXT —
  not a rendered value — is spliced in, after its own ``{{> other}}`` tags
  are recursively expanded the same way (cycle-checked) and its own escaped
  variable tags (``{{x}}``) are rewritten to explicit no-escape tags
  (``{{&x}}``, #397), directly into the including template's source, before
  that whole string ever reaches chevron. Chevron then renders the spliced
  text's own tags against whatever scope they land in — a Mustache section
  the ``{{> name}}`` tag sat inside included — exactly like any other text
  that was always there; a fragment reused inside a loop-body section (one
  line per item) sees each item, not one shared top-level context. (The
  alternative — splicing a pre-rendered VALUE, bound to an opaque context
  key — was tried and rejected: it cannot see enclosing section scope at
  all, since it is rendered once, in isolation, before the surrounding
  template's sections ever run.)
* **An effect** — a ``yield``, or a ``prompt`` whose reply is text. Spliced
  in exactly what ``{{{prime.<name>.value}}}`` would insert at that point: a
  dotted name reaches into a nested/composed effect's own state the same way
  a bare template reference already does, and a missing/skipped/absorbed-
  failure value renders as ``""``. Unlike a declared prompt, this IS a
  resolved value, bound to a synthetic context key and spliced in with a
  triple-brace tag, never re-rendered — a model reply containing literal
  ``{{...}}`` is inserted verbatim, the same guarantee an ordinary
  ``{{{x}}}`` already gives today.

One trailing line break (``\\n``/``\\r\\n``) is dropped from a declared
prompt's own text and from a resolved effect value before either is spliced
in, so ``{{> voice}}`` alone on its own line does not add a blank one;
nothing else about surrounding whitespace changes (no Mustache
standalone-partial re-indentation — see issue #396). A declared prompt may
not use a set-delimiter tag (``{{=...=}}``): it would change the delimiters
of whatever template it lands in, which is never checked at that template's
own compile time.
"""

from __future__ import annotations

import hashlib
import itertools
import re
from collections.abc import Iterator, Mapping
from pathlib import Path
from typing import Any

import chevron.tokenizer  # type: ignore[import-untyped]

from .templates import TemplateError, render_template, template_syntax_error

__all__ = [
    "EFFECT_NAMES_RUNTIME_KEY",
    "RUNTIME_CONFIG_KEY",
    "PromptCompositionError",
    "all_effect_names",
    "check_prompt_composition",
    "compile_declared_prompts",
    "declared_prompts",
    "document_content_digest",
    "known_effect_names",
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

#: The ``runtime_config`` key a document's own compiled effect-name set
#: rides under (see :data:`circuitry.core.dynamic.DynamicDefinition.effect_names`
#: and :func:`known_effect_names`) — same lifecycle as ``RUNTIME_CONFIG_KEY``.
EFFECT_NAMES_RUNTIME_KEY = "_prompt_effect_names"


def declared_prompts(runtime_config: Mapping[str, Any] | None) -> dict[str, str]:
    """The current document's declared prompts, from *runtime_config*."""
    if not runtime_config:
        return {}
    value = runtime_config.get(RUNTIME_CONFIG_KEY)
    return value if isinstance(value, dict) else {}


def known_effect_names(runtime_config: Mapping[str, Any] | None) -> frozenset[str]:
    """Every effect name the current document's tree contains anywhere.

    Lets the runtime side of ``{{> name}}`` (:func:`_resolve_effect_text`)
    tell a real effect that simply has not written its value yet — an
    untaken ``if`` branch, a ``flow: tree`` sibling, one later in the chain
    — apart from a genuinely unknown name.
    """
    if not runtime_config:
        return frozenset()
    value = runtime_config.get(EFFECT_NAMES_RUNTIME_KEY)
    if isinstance(value, frozenset):
        return value
    if isinstance(value, (set, list, tuple)):
        return frozenset(value)
    return frozenset()


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

#: Effect types whose named children nest under the container's own name in
#: real state (``prime.<name>.<child>.value``), so a bare ``{{> child}}``
#: NEVER resolves to one — not even from a sibling inside the very same
#: container — only the dotted form (``{{> name.child}}``) does, from
#: anywhere in the document (``core.dynamic``'s own ``ctx`` is always
#: rooted at the document's real ``prime``, never rebound per container;
#: only a loop's body gets the ``ctx_override`` that rebinds "this pass's
#: own sibling" bare — see ``core.loop``/``core.primes``' "WITHIN A LOOP
#: BODY"). A named ``loop`` is excluded from this set for exactly that
#: reason: its own body children stay bare-visible, the same "short
#: sibling path" shorthand its own runtime container gives them. ``use`` is
#: excluded too: its children are a wholly separate document, compiled on
#: its own; nothing in *this* document's effect tree corresponds to its
#: internal effect names.
_SCOPE_INTRODUCING_TYPES = frozenset({"if", "dynamic"})


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


#: Container fields whose values are lists of child effect dicts, walked
#: when collecting every effect name/type in the document (not into a
#: ``use`` effect's own child document — that compiles separately).
_CHILD_LISTS = ("effects", "steps", "body", "then", "else", "finally")


def all_effect_names(orch: Mapping[str, Any]) -> frozenset[str]:
    """Every effect name anywhere in *orch*, flattened regardless of nesting.

    Used for the declared-prompt/effect-name collision check (ambiguous
    everywhere a name could be bare-referenced) and for the runtime
    "known but not written yet" set (:func:`known_effect_names`) — both
    deliberately permissive, unlike :func:`check_prompt_composition`'s own
    scope-aware name resolution.
    """
    names: set[str] = set()

    def walk(effects: Any) -> None:
        if not isinstance(effects, list):
            return
        for effect in effects:
            if not isinstance(effect, dict):
                continue
            name = effect.get("name")
            if isinstance(name, str) and name:
                names.add(name)
            for field in _CHILD_LISTS:
                walk(effect.get(field))

    walk(orch.get("effects") or orch.get("steps") or [])
    walk(orch.get("finally"))
    return frozenset(names)


def document_content_digest(
    resolved_path: Path, orch: Mapping[str, Any], *, confinement_root: Path | None = None
) -> str:
    """SHA-256 of *resolved_path*'s bytes, plus every prompt file it
    references (#396) — shared by every surface that treats a document's
    bytes as its identity: capability consent (``cof trust``,
    ``cli.document_consent``) and ``core.use``'s own content digest
    (``UseRuntime._content_digest``). *confinement_root* is the project a
    ``{file: ...}`` reference inside *orch* must stay inside — the caller's
    own confinement rule (a library source's cached tree, say); the nearest
    ``circuitry.config.json``/``config.json`` when omitted. Best-effort,
    like ``core.resume.document_sha256``: a document that fails to parse
    here (it will fail again, loudly, moments later) just falls back to the
    file's own bytes.
    """
    from .prompt_files import default_project_root

    hasher = hashlib.sha256()
    hasher.update(resolved_path.read_bytes())
    try:
        document_dir = resolved_path.resolve().parent
        root = (
            confinement_root
            if confinement_root is not None
            else default_project_root(document_dir)
        )
        paths = referenced_prompt_file_paths(
            orch, document_dir=document_dir, confinement_root=root
        )
        for prompt_file in sorted(set(paths)):
            hasher.update(prompt_file.read_bytes())
    except Exception:
        pass
    return hasher.hexdigest()


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


#: The exact fields ``{{> name}}`` is composed in — "prompt templates and
#: messages, yield templates, declared prompts, tool params and prompt, use
#: inputs and inline" (#396). Everything else a template renders
#: (``if``/``while`` model templates, ``expect.template``, an asset's
#: ``ref``, ``retries``, ...) is deliberately excluded here: it keeps going
#: through the unconditional partial rejection ``core.templates`` already
#: gave every template, unaffected by this module. Only ``template`` and a
#: message's ``content`` may be ``{file: ...}``-shaped (#396 §3); the rest
#: are always plain strings.
_COMPOSABLE_SCALAR_FIELDS = ("template", "prompt", "inline", "params_json")


def _resolve_file_field_text(
    value: Any, *, document_dir: Path | None, confinement_root: Path | None
) -> str | None:
    """*value*'s text, if it is a ``{file: ...}`` this document can read —
    best-effort, same swallow-and-skip precedent as
    :func:`referenced_prompt_file_paths`: a genuine ``file:`` violation is
    ``compile_declared_prompts``/the effect's own compile step's to raise,
    with the field name this best-effort scan has lost.
    """
    if document_dir is None or confinement_root is None:
        return None
    if not (
        isinstance(value, Mapping)
        and set(value) == {"file"}
        and isinstance(value.get("file"), str)
    ):
        return None
    from .prompt_files import PromptFileError, resolve_prompt_file

    try:
        return resolve_prompt_file(
            value["file"],
            document_dir=document_dir,
            confinement_root=confinement_root,
            field="file",
        )
    except PromptFileError:
        return None


def _iter_composable_strings(
    effect: Mapping[str, Any],
    *,
    document_dir: Path | None = None,
    confinement_root: Path | None = None,
) -> list[str]:
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
        elif field == "template":
            file_text = _resolve_file_field_text(
                value, document_dir=document_dir, confinement_root=confinement_root
            )
            if file_text is not None:
                found.append(file_text)
    messages = effect.get("messages")
    if isinstance(messages, list):
        for message in messages:
            if not isinstance(message, Mapping):
                continue
            content = message.get("content")
            if isinstance(content, str):
                found.append(content)
            else:
                file_text = _resolve_file_field_text(
                    content, document_dir=document_dir, confinement_root=confinement_root
                )
                if file_text is not None:
                    found.append(file_text)
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
    (``cof check``, ``--resume``) alongside the existing ``prime.<name>``
    scan those already do."""
    return {m.group(1).strip() for m in _PARTIAL_TAG.finditer(text)}


def _declared_prompt_syntax_errors(declared: Mapping[str, str]) -> list[str]:
    """Each declared prompt validated on its own, before it is ever spliced
    anywhere (#396): a malformed fragment (an unbalanced section, say) is
    reported against the prompt that owns it, not whatever template
    happens to include it first. Also forbids a set-delimiter tag
    (``{{=...=}}``) inside a declared prompt — it would silently change the
    delimiters of the template it lands in, which is never checked at that
    template's own compile time.
    """
    errors: list[str] = []
    for name, text in declared.items():
        reason = template_syntax_error(text, allow_partials=True)
        if reason is not None:
            errors.append(f"prompts.{name}: malformed Mustache template: {reason}")
            continue
        for token_type, _value in chevron.tokenizer.tokenize(text):
            if token_type == "set delimiter":
                errors.append(
                    f"prompts.{name}: '{{{{=...=}}}}' (set-delimiter) is not "
                    "allowed inside a declared prompt — it would change the "
                    "delimiters of the template that includes it."
                )
                break
    return errors


def check_prompt_composition(
    orch: Mapping[str, Any],
    *,
    declared: Mapping[str, str],
    document_dir: Path | None = None,
    confinement_root: Path | None = None,
) -> list[str]:
    """``cof check`` errors for ``prompts:``/``{{> name}}`` in *orch*.

    Checks, across every template-bearing string in the document (effects
    and declared prompts alike, including ``{file: ...}``-sourced effect
    templates/message content): a malformed declared prompt, an unknown
    name, a name that is both declared and an effect, a named effect that is
    neither a ``yield`` nor a text ``prompt``, and a cycle among declared
    prompts.

    Name resolution mirrors the one real distinction this codebase's own
    runtime state tree makes (``core.dynamic``/``core.loop``): a named
    ``if``/``dynamic`` nests its children's VALUES under its own name
    (``prime.<name>.<child>``), and that nesting is never undone by
    position — a bare ``{{> child}}`` does not resolve even from a sibling
    inside the very same named container, only the dotted form
    (``{{> name.child}}``) does, from anywhere in the document. Everything
    else (an unnamed ``if``/``dynamic``, a ``loop``, a ``reflector``) writes
    at whatever level it itself sits at, so its own children stay
    bare-visible there — including a loop's documented "this pass's own
    sibling" shorthand inside its body. A dotted name that resolves past a
    container this module does not follow that way (a loop's own iteration
    wrapping, say) is trusted rather than rejected — the same shallow-path
    trust ``{{prime.x.y}}`` already gets elsewhere in this codebase; what it
    exposes at run time is that effect's own concern.
    """
    errors = _declared_prompt_syntax_errors(declared)

    overlap = sorted(set(declared) & all_effect_names(orch))
    errors.extend(
        f"'{name}' is both a declared prompt and an effect name — "
        "'{{> " + name + "}}' would be ambiguous."
        for name in overlap
    )

    root: dict[str, dict[str, Any]] = {}

    def check_name(
        name: str, *, where: str, bare_chain: tuple[Mapping[str, dict[str, Any]], ...]
    ) -> None:
        head = name.split(".", 1)[0]
        if head in declared:
            if "." in name:
                errors.append(
                    f"{where}: '{{{{> {name}}}}}' names declared prompt '{head}', "
                    "which has no nested state — a declared prompt is plain text, "
                    "never dotted."
                )
                return
            return  # declared always wins; collision already reported once, above
        if "." not in name:
            # Bare lookup: innermost scope first (a named `if`'s own branch
            # may have pushed its own children here — see the
            # `_SCOPE_INTRODUCING_TYPES`/bare_chain comment below), then
            # `root` for everything bare-visible document-wide.
            for scope in reversed(bare_chain):
                if head in scope:
                    node = scope[head]
                    if not node["text_producing"]:
                        errors.append(
                            f"{where}: '{{{{> {name}}}}}' names effect '{head}' "
                            f"(type '{node['type']}'), which is neither a 'yield' "
                            "nor a text 'prompt'."
                        )
                    return
            errors.append(f"{where}: '{{{{> {name}}}}}' does not name a declared prompt or effect.")
            return
        # Dotted lookup always resolves from `root`, regardless of where the
        # reference itself sits — a dotted name reaches a nested/composed
        # effect's own state "from anywhere in the document" (#396 §2).
        if head not in root:
            errors.append(f"{where}: '{{{{> {name}}}}}' does not name a declared prompt or effect.")
            return
        node = root[head]
        segments = name.split(".")
        for seg in segments[1:]:
            children = node.get("children") or {}
            if seg not in children:
                if node["type"] in _SCOPE_INTRODUCING_TYPES:
                    # Unlike `loop`/`use` below, every child of a named
                    # `if`/`dynamic` IS tracked here (`walk` populates
                    # `children` for exactly these types) — a missing
                    # segment under one is a genuine unknown name, not a
                    # container this model simply doesn't follow into.
                    errors.append(
                        f"{where}: '{{{{> {name}}}}}' does not name a declared "
                        "prompt or effect."
                    )
                return  # shallow-path trust: a container this model doesn't follow
            node = children[seg]
        if not node["text_producing"]:
            errors.append(
                f"{where}: '{{{{> {name}}}}}' names effect '{segments[-1]}' "
                f"(type '{node['type']}'), which is neither a 'yield' nor a "
                "text 'prompt'."
            )

    def walk(
        effects: Any,
        container_path: str,
        out: dict[str, dict[str, Any]],
        bare_chain: tuple[Mapping[str, dict[str, Any]], ...],
    ) -> None:
        if not isinstance(effects, list):
            return
        for effect in effects:
            if not isinstance(effect, dict):
                continue
            name = effect.get("name")
            etype = str(effect.get("type") or "").strip().lower()
            if isinstance(name, str) and name:
                out[name] = {
                    "type": etype,
                    "children": {},
                    "text_producing": _effect_is_text_producing(etype, effect),
                }
        for idx, effect in enumerate(effects):
            if not isinstance(effect, dict):
                continue
            effect_path = f"{container_path}[{idx}]"
            for text in _iter_composable_strings(
                effect, document_dir=document_dir, confinement_root=confinement_root
            ):
                for name in sorted(partial_references(text)):
                    if not _NAME_SHAPE.match(name):
                        errors.append(
                            f"{effect_path}: '{{{{> {name}}}}}' is not a valid name."
                        )
                        continue
                    check_name(name, where=effect_path, bare_chain=bare_chain)
            name = effect.get("name")
            etype = str(effect.get("type") or "").strip().lower()
            # Only the nested dict a named if/dynamic was just given above
            # (its own real state namespace) is NOT where its own children's
            # checks continue from — those are walked with THAT dict as
            # `out`, so nested names land there (dotted-reachable from
            # anywhere), never merged back up into the enclosing scope.
            next_out = out
            next_bare_chain = bare_chain
            if isinstance(name, str) and name and etype in _SCOPE_INTRODUCING_TYPES:
                next_out = out[name]["children"]
                if etype == "if":
                    # A named `if`'s own branch gets `ConditionalRuntime`'s
                    # documented within-branch shorthand at run time (an
                    # earlier branch step is visible to a later one both as
                    # `prime.<step>` and bare — `core.conditional`/
                    # `core.scope`): push this branch's own children onto
                    # the bare-visible chain too, so a sibling can bare-
                    # reference another sibling in the *same* branch without
                    # making them visible outside it (the chain is never
                    # threaded back up to the caller). `dynamic` gets no
                    # such shorthand at run time — its own `ctx` never
                    # rebinds per container — so its children stay off the
                    # bare chain, exactly as the existing "rejected even
                    # from inside it" test already pins.
                    next_bare_chain = (*bare_chain, next_out)
            for field in _CHILD_LISTS:
                walk(effect.get(field), f"{effect_path}.{field}", next_out, next_bare_chain)

    walk(orch.get("effects") or orch.get("steps") or [], "effects", root, (root,))
    walk(orch.get("finally") or [], "finally", root, (root,))

    for prompt_name, text in declared.items():
        for name in sorted(partial_references(text)):
            if not _NAME_SHAPE.match(name):
                errors.append(f"prompts.{prompt_name}: '{{{{> {name}}}}}' is not a valid name.")
                continue
            check_name(name, where=f"prompts.{prompt_name}", bare_chain=(root,))

    errors.extend(_declared_prompt_cycles(declared))
    return errors


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


def _unescape_variable_tags(text: str, *, label: str) -> str:
    """Rebuild *text* — a declared prompt's own resolved text, recursively
    including any fragment it itself spliced in — with every escaped
    variable tag (``{{x}}``) rewritten to an explicit no-escape tag
    (``{{&x}}``), so the fragment's own tags never escape wherever they
    land once spliced into an including template (#397), even one that
    keeps escaping everything else (a tool param, say).

    Rebuilds from chevron's own tokenizer rather than a regex, so section/
    comment/no-escape tags are reproduced correctly regardless of nesting.
    chevron.tokenizer classifies 'variable' and 'no escape' tags identically
    for its standalone-whitespace trimming (neither ever qualifies), so
    converting one into the other here never disturbs surrounding
    whitespace. A set-delimiter tag is rejected rather than honoured — a
    declared prompt may not carry one (``cof check`` already rejects it for
    a checked document; this is the backstop for a generated/``use: inline``
    one, #396 §2's "fails the same way, under on_error").
    """
    parts: list[str] = []
    try:
        tokens = list(chevron.tokenizer.tokenize(text))
    except Exception as exc:
        raise TemplateError(f"{label}: malformed Mustache template: {exc}") from exc
    for token_type, value in tokens:
        if token_type == "literal":
            parts.append(value)
        elif token_type in ("variable", "no escape"):
            parts.append(f"{{{{&{value}}}}}")
        elif token_type == "section":
            parts.append(f"{{{{#{value}}}}}")
        elif token_type == "inverted section":
            parts.append(f"{{{{^{value}}}}}")
        elif token_type == "end":
            parts.append(f"{{{{/{value}}}}}")
        elif token_type == "partial":
            # Should never occur: every `{{> name}}` is fully expanded,
            # recursively, before this runs. Reproduced as-is so
            # `render_template`'s own unconditional rejection catches it
            # with its usual message, rather than silently mangling it.
            parts.append(f"{{{{>{value}}}}}")
        elif token_type == "set delimiter":
            raise TemplateError(
                f"{label}: a declared prompt may not use '{{{{=...=}}}}' "
                "(set-delimiter) — it would change the delimiters of the "
                "template that includes it."
            )
        # comments are dropped — they render as nothing anyway.
    return "".join(parts)


def _resolve_effect_text(
    name: str,
    head: str,
    *,
    ctx: Mapping[str, Any],
    known_names: frozenset[str],
    label: str,
) -> str:
    """The resolved text ``{{> name}}`` splices for an effect reference —
    exactly what ``{{{prime.<name>.value}}}`` would insert.

    *head* already exists in ``ctx['prime']`` (it ran, even if skipped or
    absorbed by its own ``on_error``) — dotted-get it, "" for ``None``. Not
    there yet, but a real effect in the document (*known_names*, #396) — an
    untaken ``if`` branch, a ``flow: tree`` sibling, one later in the chain
    — also "", the same as a bare miss. Genuinely unknown — not even a name
    in the document — is the run-time backstop error for a generated
    document that skipped ``cof check``'s own, scope-aware validation.
    """
    prime_ns = ctx.get("prime") if isinstance(ctx, Mapping) else None
    if isinstance(prime_ns, Mapping) and head in prime_ns:
        value = _dotted_get(ctx, ["prime", *name.split("."), "value"])
        return "" if value is None else str(value)
    if head in known_names:
        return ""
    raise TemplateError(
        f"{label}: '{{{{> {name}}}}}' does not name a declared prompt or effect."
    )


def _expand(
    template: str,
    *,
    ctx: Mapping[str, Any],
    declared: Mapping[str, str],
    known_names: frozenset[str],
    label: str,
    seen: frozenset[str],
    counter: Iterator[int],
    extra: dict[str, str],
) -> str:
    """*template*, with every ``{{> name}}`` tag replaced, mutating *extra*
    in place with one entry per resolved effect reference.

    *counter* is a single shared sequence (``itertools.count()``), passed
    down unchanged through every recursive call this expansion makes — a
    declared prompt nested inside another template is expanded by its own
    recursive ``_expand`` call, and a fresh ``counter``/``extra`` pair per
    call would let two sibling references (one inside a nested declared
    prompt, one beside it) mint the same synthetic key and collide in the
    single ``extra`` dict the whole render ultimately shares.
    """

    def replace(match: re.Match[str]) -> str:
        name = match.group(1).strip()
        if not _NAME_SHAPE.match(name):
            raise TemplateError(f"{label}: '{{{{> {name}}}}}' is not a valid name.")
        head = name.split(".", 1)[0]
        if "." not in name and head in declared:
            if name in seen:
                raise TemplateError(f"{label}: cycle among declared prompts at '{name}'.")
            fragment = _expand(
                declared[name],
                ctx=ctx,
                declared=declared,
                known_names=known_names,
                label=f"prompts.{name}",
                seen=seen | {name},
                counter=counter,
                extra=extra,
            )
            fragment = _unescape_variable_tags(fragment, label=f"prompts.{name}")
            return _drop_one_trailing_newline(fragment)
        text = _resolve_effect_text(name, head, ctx=ctx, known_names=known_names, label=label)
        text = _drop_one_trailing_newline(text)
        key = f"__circuitry_partial_{next(counter)}__"
        extra[key] = text
        return f"{{{{{{{key}}}}}}}"

    return _PARTIAL_TAG.sub(replace, template)


def render_with_composition(
    template: str,
    ctx: Mapping[str, Any],
    *,
    declared: Mapping[str, str] | None = None,
    known_effect_names: frozenset[str] = frozenset(),
    label: str = "template",
    escape: bool = True,
) -> str:
    """Render *template* with every ``{{> name}}`` expanded first (#396).

    *declared* is the document's own ``prompts:`` registry (empty/``None``
    outside a document that declares any, or for a child ``use`` document
    with none of its own — see the ``_prompts`` runtime-config key).
    *known_effect_names* is the document's own effect-name set (see
    :func:`known_effect_names`), used only to tell a real effect that has
    not written its value yet apart from a genuinely unknown name. Every
    other tag in *template* — including one a declared prompt's own text
    textually spliced in here — keeps rendering exactly as
    :func:`render_template` already does, including *escape*; a spliced
    declared-prompt fragment's own tags are pre-rewritten to never escape
    regardless (#397).
    """
    extra: dict[str, str] = {}
    rewritten = _expand(
        template,
        ctx=ctx,
        declared=declared or {},
        known_names=known_effect_names,
        label=label,
        seen=frozenset(),
        counter=itertools.count(),
        extra=extra,
    )
    merged_ctx = {**ctx, **extra} if extra else ctx
    return render_template(rewritten, merged_ctx, label=label, escape=escape)
