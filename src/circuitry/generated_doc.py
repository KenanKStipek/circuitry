"""The generated-document ceiling (#772).

A document produced while a run executes — a reflector's plan, or a ``use``
whose ``inline:`` is rendered from state — can carry a tool effect that
*looks* exactly like a document the author wrote by hand: the same
``params.allowed_commands`` on a ``shell`` effect, the same ``provider:``
tokens. ``allowlist_gate``/``capability_gate`` already stop a generated
document from reaching an adapter, tool category or capability class the
run itself does not allow; this module adds the one check those two do
not: a *specific tool provider* a generated document's enclosing document
does not itself use, and the shell allowlist/``service`` ``shell: true``
widening ``docs/threat-model.md`` says a rendered list must never grant.

**Telling a generated document from an author's own.** An author-written
``use: inline:``/reflector ``effects:`` template whose *structure* is
literal, with only scalar values templated in, keeps working exactly as
before: :func:`is_generated_inline` parses the *unrendered* template text as
YAML and compares its shape (dict keys, list lengths, recursively) against
the rendered-and-parsed document. Anywhere the two disagree — a Mustache
tag that expanded into a whole effect, a block helper that produced a list
the template never wrote down, or a template that fails to parse as YAML at
all (the common case: the whole ``inline:`` is one ``{{{...}}}`` tag) —  the
document counts as generated. A reflector's plan has no "author's own
template" to compare against at all: it is always generated.

**The ceiling.** A generated document may use only the tool providers its
enclosing document uses itself, statically (:func:`static_tool_providers`),
plus whatever the generating ``use``/``reflector`` effect's own, literal
``allow_tools:`` lists. :func:`generated_document_errors` checks a parsed,
generated document against that ceiling and against the shell-specific
rules (``allowed_commands`` never honoured, ``service``'s ``shell: true``
never honoured) and returns every violation found — before any of the
generated document's effects run, so the caller can fail the generating
effect the same way any other compile-time error does.
"""

from __future__ import annotations

from typing import Any

#: ``runtime_config`` key holding the *current* document's own, statically
#: referenced tool-plugin provider names (a list of strings, JSON/state-safe
#: like every other ``_``-prefixed runtime_config key) — set fresh for every
#: document a run compiles and executes (the root document, and each ``use``/
#: decomposition child), so a nested ``use``/``reflector`` effect's generated
#: child is judged against *its own* enclosing document, never an ancestor's.
GENERATED_DOCUMENT_PROVIDERS_KEY = "_document_tool_providers"


def _best_effort_yaml(text: str) -> Any:
    """Parse *text* as YAML, or ``None`` on any failure.

    Used only to look for literal structure — never to execute or trust the
    result as a real document; a parse failure (the common case for a
    Mustache tag spanning a whole node) just means "no literal structure
    found here".
    """
    from .core.yaml_load import load_yaml

    try:
        return load_yaml(text)
    except Exception:
        return None


def _walk_tool_providers(effects: Any, providers: set[str]) -> None:
    if not isinstance(effects, list):
        return
    for effect in effects:
        if not isinstance(effect, dict):
            continue
        etype = effect.get("type")
        if etype == "tool":
            prov = effect.get("provider")
            if isinstance(prov, str) and prov.strip():
                providers.add(prov.strip())
        elif etype == "dynamic":
            _walk_tool_providers(effect.get("effects") or effect.get("steps"), providers)
            _walk_tool_providers(effect.get("finally"), providers)
        elif etype in ("if", "conditional"):
            _walk_tool_providers(effect.get("then"), providers)
            _walk_tool_providers(effect.get("else"), providers)
        elif etype == "loop":
            _walk_tool_providers(effect.get("body"), providers)
        elif etype == "reflector":
            _walk_tool_providers(effect.get("effects") or effect.get("steps"), providers)
        elif etype == "use":
            inline = effect.get("inline")
            if isinstance(inline, str):
                parsed = _best_effort_yaml(inline)
                if isinstance(parsed, dict):
                    _walk_tool_providers(parsed.get("effects") or parsed.get("steps"), providers)
                    _walk_tool_providers(parsed.get("finally"), providers)


def static_tool_providers(orch: Any) -> frozenset[str]:
    """Every ``type: tool`` provider this document's own text references.

    Recurses into ``dynamic``/``if``/``conditional``/``loop``/``reflector``
    bodies, and into a literal ``use: inline:`` child's own text (best-effort
    parsed as YAML) — a document written to use a tool only inside a literal
    inline child still "uses it itself" (#772 rule 3). Never follows
    ``path:``/``ref:``: that is a separate file, judged on its own terms by
    the allowlist/capability-consent gates that already cover it.
    """
    providers: set[str] = set()
    if isinstance(orch, dict):
        _walk_tool_providers(orch.get("effects") or orch.get("steps"), providers)
        _walk_tool_providers(orch.get("finally"), providers)
    return frozenset(providers)


def _same_shape(a: Any, b: Any) -> bool:
    """Whether *a* (parsed from the unrendered template) and *b* (the
    rendered, final document) agree on structure at this node and below.

    A scalar is always compatible with a scalar — rendering a value in is
    exactly what "only values templated in" means. A dict/list is compatible
    only with a dict/list of the same keys/length, recursively: anything a
    rendered node added or removed relative to the unrendered template did
    not come from the author's own text.
    """
    if isinstance(a, dict) or isinstance(b, dict):
        if not (isinstance(a, dict) and isinstance(b, dict)):
            return False
        if set(a) != set(b):
            return False
        return all(_same_shape(a[key], b[key]) for key in a)
    if isinstance(a, list) or isinstance(b, list):
        if not (isinstance(a, list) and isinstance(b, list)):
            return False
        if len(a) != len(b):
            return False
        return all(_same_shape(x, y) for x, y in zip(a, b, strict=True))
    return True


def is_generated_inline(raw_template: str, rendered: Any) -> bool:
    """Whether a ``use: inline:`` child counts as generated (#772).

    ``raw_template`` is the effect's own ``inline:`` field text, *before*
    Mustache rendering; ``rendered`` is the same child after rendering and
    YAML-parsing — what will actually run. ``False`` means the unrendered
    template already has the rendered document's exact shape (only scalar
    values differ), so this is the author's own content, with values
    templated in, same as always. ``True`` — including when the unrendered
    text does not parse as YAML at all, the common case for a template that
    is just one ``{{{...}}}`` tag — means some or all of this document's
    structure only exists because it was rendered, so it is generated.
    """
    raw_parsed = _best_effort_yaml(raw_template)
    if not isinstance(raw_parsed, dict):
        return True
    return not _same_shape(raw_parsed, rendered)


def _as_bool(value: Any) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        return value.strip().lower() in ("1", "true", "yes", "on")
    return bool(value)


def _walk_generated_errors(
    effects: Any,
    *,
    path: str,
    allowed_providers: frozenset[str] | None,
    known_providers: frozenset[str] | None,
    generating_effect_path: str,
    errors: list[str],
) -> None:
    if not isinstance(effects, list):
        return
    for effect in effects:
        if not isinstance(effect, dict):
            continue
        etype = effect.get("type")
        raw_name = effect.get("name")
        name = raw_name if isinstance(raw_name, str) and raw_name else "<unnamed>"
        effect_label = f"{path}.{name}" if path else name
        if etype == "tool":
            provider = effect.get("provider")
            provider_name = provider.strip() if isinstance(provider, str) else None
            params = effect.get("params")
            params = params if isinstance(params, dict) else {}
            if provider_name == "shell" and "allowed_commands" in params:
                errors.append(
                    f"generated effect '{effect_label}' sets params.allowed_commands "
                    "on a 'shell' tool, generated by "
                    f"{generating_effect_path}; a document generated at run time "
                    "can never widen the shell allowlist this way."
                )
            elif provider_name == "service" and _as_bool(params.get("shell")):
                errors.append(
                    f"generated effect '{effect_label}' sets params.shell: true on "
                    f"a 'service' tool, generated by {generating_effect_path}; a "
                    "document generated at run time can never do that."
                )
            elif (
                provider_name is not None
                and allowed_providers is not None
                and provider_name not in allowed_providers
                # A provider that isn't even a known plugin fails the usual way
                # (an "Unknown plugin" error at dispatch) instead of this one --
                # adding an unknown name to allow_tools would itself be a check
                # error, so this refusal is only for a real plugin the document
                # that generated it simply doesn't use.
                and (known_providers is None or provider_name in known_providers)
            ):
                errors.append(
                    f"generated effect '{effect_label}' uses tool provider "
                    f"'{provider_name}', which the document that generated it "
                    f"doesn't use; add '{provider_name}' to allow_tools on "
                    f"{generating_effect_path} to allow it."
                )
        elif etype == "dynamic":
            _walk_generated_errors(
                effect.get("effects") or effect.get("steps"),
                path=effect_label,
                allowed_providers=allowed_providers,
                known_providers=known_providers,
                generating_effect_path=generating_effect_path,
                errors=errors,
            )
            _walk_generated_errors(
                effect.get("finally"),
                path=effect_label,
                allowed_providers=allowed_providers,
                known_providers=known_providers,
                generating_effect_path=generating_effect_path,
                errors=errors,
            )
        elif etype in ("if", "conditional"):
            _walk_generated_errors(
                effect.get("then"),
                path=effect_label,
                allowed_providers=allowed_providers,
                known_providers=known_providers,
                generating_effect_path=generating_effect_path,
                errors=errors,
            )
            _walk_generated_errors(
                effect.get("else"),
                path=effect_label,
                allowed_providers=allowed_providers,
                known_providers=known_providers,
                generating_effect_path=generating_effect_path,
                errors=errors,
            )
        elif etype == "loop":
            _walk_generated_errors(
                effect.get("body"),
                path=effect_label,
                allowed_providers=allowed_providers,
                known_providers=known_providers,
                generating_effect_path=generating_effect_path,
                errors=errors,
            )
        elif etype == "reflector":
            _walk_generated_errors(
                effect.get("effects") or effect.get("steps"),
                path=effect_label,
                allowed_providers=allowed_providers,
                known_providers=known_providers,
                generating_effect_path=generating_effect_path,
                errors=errors,
            )
        elif etype == "use":
            inline = effect.get("inline")
            if isinstance(inline, str):
                parsed = _best_effort_yaml(inline)
                if isinstance(parsed, dict):
                    _walk_generated_errors(
                        parsed.get("effects") or parsed.get("steps"),
                        path=effect_label,
                        allowed_providers=allowed_providers,
                        known_providers=known_providers,
                        generating_effect_path=generating_effect_path,
                        errors=errors,
                    )
                    _walk_generated_errors(
                        parsed.get("finally"),
                        path=effect_label,
                        allowed_providers=allowed_providers,
                        known_providers=known_providers,
                        generating_effect_path=generating_effect_path,
                        errors=errors,
                    )


def _known_providers() -> frozenset[str]:
    from .plugins.factory import SUPPORTED_PLUGINS

    return frozenset(SUPPORTED_PLUGINS)


def generated_document_errors(
    orch: Any,
    *,
    allowed_providers: frozenset[str] | None,
    generating_effect_path: str = "",
) -> list[str]:
    """Every rule a generated document violates (#772); empty means clean.

    Checked statically against the parsed document — before any of its
    effects run — so the caller can refuse up front, the same way a schema
    or allowlist violation already does. *allowed_providers* is the set of
    tool providers this generated document may use: the document that
    generated it uses itself, statically (:func:`static_tool_providers`),
    plus the generating effect's own
    literal ``allow_tools:``. ``None`` skips the provider check (rule 3)
    entirely while still enforcing the shell-specific rules (rule 2) -- a
    decomposition plan's case: it has no generating effect of its own to
    carry an ``allow_tools:``, and is already held to the enclosing
    document's consented capability classes by the pre-existing capability
    ceiling (:mod:`circuitry.capability_gate`). *generating_effect_path* is
    the dotted state path of the ``use``/``reflector`` effect that produced
    this document, named in the provider-outside-the-allowed-set message so
    the fix ("add it to allow_tools on ...") is actionable; ignored when
    *allowed_providers* is ``None``.
    """
    errors: list[str] = []
    known_providers = None if allowed_providers is None else _known_providers()
    if isinstance(orch, dict):
        _walk_generated_errors(
            orch.get("effects") or orch.get("steps"),
            path="",
            allowed_providers=allowed_providers,
            known_providers=known_providers,
            generating_effect_path=generating_effect_path,
            errors=errors,
        )
        _walk_generated_errors(
            orch.get("finally"),
            path="",
            allowed_providers=allowed_providers,
            known_providers=known_providers,
            generating_effect_path=generating_effect_path,
            errors=errors,
        )
    return errors
