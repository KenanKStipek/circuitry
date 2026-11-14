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
the template never wrote down, a template that fails to parse as YAML at
all (the common case: the whole ``inline:`` is one ``{{{...}}}`` tag), or a
scalar at a security-sensitive field (``type``, ``provider``,
``allow_tools``, ``params.allowed_commands``, ``params.shell``) that only
matches *by value* rather than being the same literal text — the document
counts as generated. A reflector's plan has no "author's own template" to
compare against at all: it is always generated. **Generated-ness is also
inherited**: every effect textually inside an already-generated document is
itself generated too, whatever its own shape looks like, and a generated
document's own ``use``/``reflector`` effect can never grant its nested
documents a tool provider beyond what it itself was allowed (see
``core.use.UseRuntime._check_generated_document``, the caller that threads
that inheritance through ``runtime_config``).

**The ceiling.** A generated document may use only the tool providers its
enclosing document uses itself, statically (:func:`static_tool_providers`),
plus whatever the generating ``use``/``reflector`` effect's own, literal
``allow_tools:`` lists. :func:`generated_document_errors` checks a parsed,
generated document against that ceiling and against the shell-specific
rules — ``allowed_commands`` only ever as wide as the host config plus the
enclosing document's own literal shell effects (:func:`current_shell_allowlist`),
``service``'s ``shell: true`` never honoured — and returns every violation
found, before any of the generated document's effects run, so the caller
can fail the generating effect the same way any other compile-time error
does.

Effect *type* and tool *provider* names are matched case-insensitively
everywhere in this module, the same way :func:`circuitry.plugins.factory.build_plugin`
normalizes a provider name before dispatch (``provider: Shell`` and
``provider: shell`` are the same tool), so a generated document cannot slip
past a comparison that only checked the exact-case string.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

#: ``runtime_config`` key holding the *current* document's own, statically
#: referenced tool-plugin provider names (a list of strings, JSON/state-safe
#: like every other ``_``-prefixed runtime_config key) — set fresh for every
#: document a run compiles and executes (the root document, and each ``use``/
#: decomposition child), so a nested ``use``/``reflector`` effect's generated
#: child is judged against *its own* enclosing document, never an ancestor's.
GENERATED_DOCUMENT_PROVIDERS_KEY = "_document_tool_providers"

#: ``runtime_config`` key holding the *current* document's own, resolved
#: shell allowlist (host config's default/pinned commands union the
#: enclosing document's own literal ``shell`` effects' ``allowed_commands``,
#: already intersected with any host pin) — the ceiling a generated ``shell``
#: effect's own ``params.allowed_commands`` is checked against (#772 rule 2).
GENERATED_DOCUMENT_SHELL_COMMANDS_KEY = "_document_shell_allowed_commands"

#: ``runtime_config`` key holding whether the document currently executing
#: (the one that holds whatever ``use``/``reflector`` effect is about to be
#: checked) is itself generated — set by the same caller that sets the two
#: keys above. ``True`` means every nested document inside it is generated
#: too, whatever its own shape looks like, and its own ``allow_tools`` can
#: never grant anything the enclosing ceiling didn't already.
GENERATED_DOCUMENT_IS_GENERATED_KEY = "_document_is_generated"

#: ``runtime_config`` key holding the dotted state path of the ``use``/
#: ``reflector`` effect that generated the document currently executing
#: (or "" for a decomposition plan, which has no single generating effect
#: of its own) -- named in a dispatch-time refusal (:func:`dispatch_errors`)
#: the same way the compile-time walk already names it, so a refusal found
#: much later, after the child has been running for a while, still points
#: at the effect whose ``allow_tools`` (or lack of one) would fix it.
GENERATED_DOCUMENT_GENERATING_PATH_KEY = "_document_generating_effect_path"

#: ``runtime_config`` key holding whether a provider-ceiling refusal in the
#: current document should name ``allow_tools`` as the fix (``True``, a
#: ``use``/``reflector`` child) or the fixed decomposition safe set
#: (``False``, a decomposition plan, which has no ``allow_tools`` field of
#: its own to name) -- read by :func:`dispatch_errors` the same way
#: :func:`generated_document_errors`'s callers already pass
#: ``allow_tools_available`` explicitly.
GENERATED_DOCUMENT_ALLOW_TOOLS_AVAILABLE_KEY = "_document_allow_tools_available"

#: A decomposition plan has no generating ``use``/``reflector`` effect of its
#: own to carry a literal ``allow_tools:`` (#772 rule 3) — its own merge step
#: commonly needs a tool the document it serves doesn't otherwise use, so
#: every decomposition plan is additionally allowed this fixed set of
#: tool providers, none of which run a command or reach anything outside the
#: process: pure data transforms, never a side effect.
DECOMPOSITION_SAFE_PROVIDERS: frozenset[str] = frozenset(
    {
        "json",
        "regex",
        "math",
        "cel",
        "clock",
        "base64",
        "hex",
        "uuid",
        "validate_yaml",
    }
)

#: Security-sensitive keys (#772 rule 6): a scalar at one of these, read
#: anywhere in an effect's own dict (an exact-key match, any depth — never
#: narrowed to e.g. only ``params.allowed_commands``, since that is strictly
#: the safer direction), must be byte-identical between the unrendered
#: template and the rendered document for :func:`is_generated_inline` to
#: still call it the author's own; a templated value there (even one that
#: renders to the same literal text the author could have written) counts
#: as generated.
_SENSITIVE_SHAPE_KEYS = frozenset(
    {
        "type",
        "provider",
        "allow_tools",
        "allowed_commands",
        "shell",
        "inline",
        "path",
        "ref",
        "orchestration",
    }
)


def _normalize_name(value: Any) -> str | None:
    """*value* as a lowercase, stripped provider/effect-type name, or
    ``None`` when it isn't a (non-blank) string — matching the exact
    normalization :func:`circuitry.plugins.factory.build_plugin` applies to
    a ``provider:`` field before dispatch, so a comparison here can never
    disagree with what actually runs (#772).
    """
    if not isinstance(value, str):
        return None
    normalized = value.strip().lower()
    return normalized or None


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
        etype = _normalize_name(effect.get("type"))
        if etype == "tool":
            prov = _normalize_name(effect.get("provider"))
            if prov:
                providers.add(prov)
        elif etype == "dynamic":
            _walk_tool_providers(
                effect.get("effects") or effect.get("steps"), providers
            )
            _walk_tool_providers(effect.get("finally"), providers)
        elif etype in ("if", "conditional"):
            _walk_tool_providers(effect.get("then"), providers)
            _walk_tool_providers(effect.get("else"), providers)
        elif etype == "loop":
            _walk_tool_providers(effect.get("body"), providers)
        elif etype == "reflector":
            _walk_tool_providers(
                effect.get("effects") or effect.get("steps"), providers
            )
        elif etype == "use":
            inline = effect.get("inline")
            if isinstance(inline, str):
                parsed = _best_effort_yaml(inline)
                if isinstance(parsed, dict):
                    _walk_tool_providers(
                        parsed.get("effects") or parsed.get("steps"), providers
                    )
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


def _walk_shell_allowed_commands(effects: Any, commands: set[str]) -> None:
    if not isinstance(effects, list):
        return
    for effect in effects:
        if not isinstance(effect, dict):
            continue
        etype = _normalize_name(effect.get("type"))
        if etype == "tool":
            prov = _normalize_name(effect.get("provider"))
            params = effect.get("params")
            if prov == "shell" and isinstance(params, dict):
                allowed = params.get("allowed_commands")
                if isinstance(allowed, list):
                    commands.update(c for c in allowed if isinstance(c, str))
        elif etype == "dynamic":
            _walk_shell_allowed_commands(
                effect.get("effects") or effect.get("steps"), commands
            )
            _walk_shell_allowed_commands(effect.get("finally"), commands)
        elif etype in ("if", "conditional"):
            _walk_shell_allowed_commands(effect.get("then"), commands)
            _walk_shell_allowed_commands(effect.get("else"), commands)
        elif etype == "loop":
            _walk_shell_allowed_commands(effect.get("body"), commands)
        elif etype == "reflector":
            _walk_shell_allowed_commands(
                effect.get("effects") or effect.get("steps"), commands
            )
        elif etype == "use":
            inline = effect.get("inline")
            if isinstance(inline, str):
                parsed = _best_effort_yaml(inline)
                if isinstance(parsed, dict):
                    _walk_shell_allowed_commands(
                        parsed.get("effects") or parsed.get("steps"), commands
                    )
                    _walk_shell_allowed_commands(parsed.get("finally"), commands)


def static_shell_allowed_commands(orch: Any) -> frozenset[str]:
    """Every command this document's own literal ``shell`` effects allow
    (#772 rule 2) — every ``params.allowed_commands`` entry on a literal
    ``type: tool, provider: shell`` effect anywhere in the document's own
    text, recursing the same way :func:`static_tool_providers` does. A
    generated ``shell`` effect's own ``allowed_commands`` is never collected
    here — only an *enclosing*, non-generated document's own text counts.
    """
    commands: set[str] = set()
    if isinstance(orch, dict):
        _walk_shell_allowed_commands(orch.get("effects") or orch.get("steps"), commands)
        _walk_shell_allowed_commands(orch.get("finally"), commands)
    return frozenset(commands)


def _walk_definition_tool_providers(effects: Any, providers: set[str]) -> None:
    from .core.conditional import ConditionalDefinition
    from .core.dynamic import DynamicDefinition
    from .core.loop import LoopDefinition
    from .core.reflector import ReflectorDefinition
    from .core.tool import ToolDefinition
    from .core.use import UseDefinition

    for effect in effects or ():
        if isinstance(effect, ToolDefinition):
            prov = _normalize_name(effect.provider)
            if prov:
                providers.add(prov)
        elif isinstance(effect, DynamicDefinition):
            _walk_definition_tool_providers(effect.effects, providers)
            _walk_definition_tool_providers(effect.finally_effects, providers)
        elif isinstance(effect, ConditionalDefinition):
            _walk_definition_tool_providers(effect.then_effects, providers)
            _walk_definition_tool_providers(effect.else_effects, providers)
        elif isinstance(effect, LoopDefinition):
            _walk_definition_tool_providers(effect.body, providers)
        elif isinstance(effect, ReflectorDefinition):
            _walk_definition_tool_providers(effect.inner.effects, providers)
        elif isinstance(effect, UseDefinition):
            if effect.inline is not None:
                parsed = _best_effort_yaml(effect.inline)
                if isinstance(parsed, dict):
                    _walk_tool_providers(
                        parsed.get("effects") or parsed.get("steps"), providers
                    )
                    _walk_tool_providers(parsed.get("finally"), providers)


def static_tool_providers_from_definitions(root: Any) -> frozenset[str]:
    """Every ``type: tool`` provider the compiled root document references,
    walked off the compiled ``DynamicDefinition`` tree rather than the raw
    YAML dict (#772 rule 9) -- so a profile's own per-effect ``provider``
    override (:func:`circuitry.core.compiler.apply_effect_overrides`,
    applied before this runs) is reflected the same way electricity's own
    :func:`static_tool_providers_from_op` already reflects it off the
    compiled bytecode. Walks the same shape :func:`static_tool_providers`
    walks over raw YAML, plus a literal ``use: inline:`` leaf's own text
    (best-effort re-parsed, the same as :func:`static_tool_providers`)
    rather than a ``path:``/``orchestration:`` leaf's compiled child, which
    is a separate file judged on its own terms.
    """
    providers: set[str] = set()
    _walk_definition_tool_providers(getattr(root, "effects", None), providers)
    _walk_definition_tool_providers(getattr(root, "finally_effects", None), providers)
    return frozenset(providers)


def host_shell_pin(runtime_config: Mapping[str, Any] | None) -> frozenset[str] | None:
    """``runtime.plugins.shell.allowed_commands`` — the host's own ceiling
    (see ``plugins/shell.py``'s module doc comment), or ``None`` when the
    host sets no pin at all.
    """
    if not isinstance(runtime_config, Mapping):
        return None
    plugins = runtime_config.get("plugins")
    shell_cfg = plugins.get("shell") if isinstance(plugins, Mapping) else None
    pin = shell_cfg.get("allowed_commands") if isinstance(shell_cfg, Mapping) else None
    if isinstance(pin, list) and all(isinstance(p, str) for p in pin):
        return frozenset(pin)
    return None


def resolved_shell_allowlist(
    document_literal_commands: frozenset[str], *, host_pin: frozenset[str] | None
) -> frozenset[str]:
    """The host's default allowlist, plus *document_literal_commands*
    (:func:`static_shell_allowed_commands` of some enclosing, non-generated
    document), intersected with *host_pin* when the host sets one — the
    ceiling a generated ``shell`` effect's own ``allowed_commands`` may never
    exceed (#772 rule 2). Idempotent: re-resolving an already-resolved set
    against the same *host_pin* returns it unchanged, which is what lets a
    generated document's own ceiling simply carry this set forward
    unexamined for its own nested generated children (#772 rule 2's
    inheritance — see ``core.use.UseRuntime._check_generated_document``).
    """
    from .plugins.shell import _DEFAULT_ALLOWED as _SHELL_DEFAULT_ALLOWED

    allowed = frozenset(_SHELL_DEFAULT_ALLOWED) | document_literal_commands
    if host_pin is not None:
        allowed &= host_pin
    return allowed


def current_shell_allowlist(runtime_config: Mapping[str, Any] | None) -> frozenset[str]:
    """The resolved shell allowlist already stored under
    :data:`GENERATED_DOCUMENT_SHELL_COMMANDS_KEY`, or the host's own
    default/pinned allowlist with no document-literal commands when that key
    isn't set yet (a caller that hasn't wired the key in at all — never
    narrower than what every shell effect, generated or not, could already
    reach before #772).
    """
    if isinstance(runtime_config, Mapping):
        stored = runtime_config.get(GENERATED_DOCUMENT_SHELL_COMMANDS_KEY)
        if stored is not None:
            return frozenset(stored)
    return resolved_shell_allowlist(
        frozenset(), host_pin=host_shell_pin(runtime_config)
    )


def _same_shape(a: Any, b: Any, *, sensitive: bool = False) -> bool:
    """Whether *a* (parsed from the unrendered template) and *b* (the
    rendered, final document) agree on structure at this node and below.

    A scalar is compatible with a scalar — rendering a value in is exactly
    what "only values templated in" means — unless *sensitive* says this
    position is one of :data:`_SENSITIVE_SHAPE_KEYS` (#772 rule 6): there, a
    rendered value that merely happens to equal what the author could have
    written is still generated, because a Mustache tag that renders to
    ``shell`` today can render to anything else tomorrow. A dict/list is
    compatible only with a dict/list of the same keys/length, recursively:
    anything a rendered node added or removed relative to the unrendered
    template did not come from the author's own text.
    """
    if isinstance(a, dict) or isinstance(b, dict):
        if not (isinstance(a, dict) and isinstance(b, dict)):
            return False
        if set(a) != set(b):
            return False
        return all(
            _same_shape(a[key], b[key], sensitive=key in _SENSITIVE_SHAPE_KEYS)
            for key in a
        )
    if isinstance(a, list) or isinstance(b, list):
        if not (isinstance(a, list) and isinstance(b, list)):
            return False
        if len(a) != len(b):
            return False
        return all(
            _same_shape(x, y, sensitive=sensitive) for x, y in zip(a, b, strict=True)
        )
    return not (sensitive and a != b)


def is_generated_inline(raw_template: str, rendered: Any) -> bool:
    """Whether a ``use: inline:`` child counts as generated (#772).

    ``raw_template`` is the effect's own ``inline:`` field text, *before*
    Mustache rendering; ``rendered`` is the same child after rendering and
    YAML-parsing — what will actually run. ``False`` means the unrendered
    template already has the rendered document's exact shape, with only
    non-sensitive scalar values templated in (see :func:`_same_shape`), so
    this is the author's own content, same as always. ``True`` — including
    when the unrendered text does not parse as YAML at all, the common case
    for a template that is just one ``{{{...}}}`` tag — means some or all of
    this document's structure (or a security-sensitive field's value) only
    exists because it was rendered, so it is generated.
    """
    raw_parsed = _best_effort_yaml(raw_template)
    if not isinstance(raw_parsed, dict):
        return True
    return not _same_shape(raw_parsed, rendered)


def _tool_effect_errors(
    *,
    effect_label: str,
    provider_name: str | None,
    params: Mapping[str, Any],
    allowed_providers: frozenset[str] | None,
    known_providers: frozenset[str] | None,
    allowed_shell_commands: frozenset[str],
    generating_effect_path: str,
    allow_tools_available: bool,
) -> list[str]:
    """Every violation a single tool effect's *final* provider and params
    commit (#772). Shared by the compile-time walk
    (:func:`_walk_generated_errors`) and the dispatch-time check
    (:func:`dispatch_errors`), so the two can never disagree on what a rule
    means -- only on what they can see (dispatch sees ``params_json``'s
    merge and every retry's fully rendered params; the walk sees only the
    parsed document). The checks below are independent ifs, not an
    if/elif chain: the provider-ceiling check runs for every tool effect
    regardless of whether the shell/service-specific checks also fire, so
    a 'shell' or 'service' effect outside the ceiling is still refused
    even when its own allowlist/shell value is in order.
    """
    from .plugins.base import _as_bool
    from .plugins.shell import is_bare_command_name

    errors: list[str] = []
    if provider_name == "shell" and "allowed_commands" in params:
        requested = params.get("allowed_commands")
        requested_list = requested if isinstance(requested, list) else [requested]
        bad = sorted(
            {
                c
                for c in requested_list
                if isinstance(c, str) and c not in allowed_shell_commands
            }
        )
        if not isinstance(requested, list) and not bad:
            bad = ["<invalid>"]
        if bad:
            errors.append(
                f"generated effect '{effect_label}' sets params.allowed_commands "
                f"to include {bad!r} on a 'shell' tool, generated by "
                f"{generating_effect_path}; the host config and this document's "
                f"own shell effects don't already allow {bad!r} -- add it to a "
                "literal shell effect's allowed_commands in the document, or to "
                "runtime.plugins.shell.allowed_commands."
            )
    if provider_name == "service":
        if _as_bool(params.get("shell")):
            errors.append(
                f"generated effect '{effect_label}' sets params.shell: true on "
                f"a 'service' tool, generated by {generating_effect_path}; a "
                "document generated at run time can never do that."
            )
        elif _normalize_name(params.get("action")) == "start":
            env = params.get("env")
            if isinstance(env, Mapping):
                bad_env = sorted(
                    key
                    for key in env
                    if isinstance(key, str)
                    and (key == "PATH" or key.startswith(("LD_", "DYLD_")))
                )
                if bad_env:
                    errors.append(
                        f"generated effect '{effect_label}' starts a 'service' "
                        f"tool whose env sets {bad_env!r}, generated by "
                        f"{generating_effect_path}; a document generated at run "
                        "time can never change how its executable resolves or "
                        "loads this way."
                    )
            command = params.get("command")
            if isinstance(command, list) and command:
                executable = command[0]
                executable_name = (
                    executable if isinstance(executable, str) else str(executable)
                )
                if (
                    not is_bare_command_name(executable_name)
                    or executable_name not in allowed_shell_commands
                ):
                    errors.append(
                        f"generated effect '{effect_label}' starts a 'service' tool "
                        f"whose command begins with {executable_name!r}, generated "
                        f"by {generating_effect_path}; the host config and this "
                        f"document's own shell effects don't already allow "
                        f"{executable_name!r} -- add it to a literal shell effect's "
                        "allowed_commands in the document, or to "
                        "runtime.plugins.shell.allowed_commands."
                    )
    if (
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
        if allow_tools_available:
            errors.append(
                f"generated effect '{effect_label}' uses tool provider "
                f"'{provider_name}', which the document that generated it "
                f"doesn't use; add '{provider_name}' to allow_tools on "
                f"{generating_effect_path} to allow it."
            )
        else:
            errors.append(
                f"generated effect '{effect_label}' uses tool provider "
                f"'{provider_name}', generated by {generating_effect_path}; "
                "it isn't among the tool providers the document it serves "
                "already uses, plus the fixed set a generated plan is always "
                "allowed (json, regex, math, cel, clock, base64, hex, "
                "uuid, validate_yaml)."
            )
    return errors


def _walk_generated_errors(
    effects: Any,
    *,
    path: str,
    allowed_providers: frozenset[str] | None,
    known_providers: frozenset[str] | None,
    allowed_shell_commands: frozenset[str],
    generating_effect_path: str,
    allow_tools_available: bool,
    errors: list[str],
) -> None:
    if not isinstance(effects, list):
        return
    for effect in effects:
        if not isinstance(effect, dict):
            continue
        etype = _normalize_name(effect.get("type"))
        raw_name = effect.get("name")
        name = raw_name if isinstance(raw_name, str) and raw_name else "<unnamed>"
        effect_label = f"{path}.{name}" if path else name
        if etype == "tool":
            provider_name = _normalize_name(effect.get("provider"))
            params = effect.get("params")
            params = params if isinstance(params, dict) else {}
            errors.extend(
                _tool_effect_errors(
                    effect_label=effect_label,
                    provider_name=provider_name,
                    params=params,
                    allowed_providers=allowed_providers,
                    known_providers=known_providers,
                    allowed_shell_commands=allowed_shell_commands,
                    generating_effect_path=generating_effect_path,
                    allow_tools_available=allow_tools_available,
                )
            )
        elif etype == "dynamic":
            _walk_generated_errors(
                effect.get("effects") or effect.get("steps"),
                path=effect_label,
                allowed_providers=allowed_providers,
                known_providers=known_providers,
                allowed_shell_commands=allowed_shell_commands,
                generating_effect_path=generating_effect_path,
                allow_tools_available=allow_tools_available,
                errors=errors,
            )
            _walk_generated_errors(
                effect.get("finally"),
                path=effect_label,
                allowed_providers=allowed_providers,
                known_providers=known_providers,
                allowed_shell_commands=allowed_shell_commands,
                generating_effect_path=generating_effect_path,
                allow_tools_available=allow_tools_available,
                errors=errors,
            )
        elif etype in ("if", "conditional"):
            _walk_generated_errors(
                effect.get("then"),
                path=effect_label,
                allowed_providers=allowed_providers,
                known_providers=known_providers,
                allowed_shell_commands=allowed_shell_commands,
                generating_effect_path=generating_effect_path,
                allow_tools_available=allow_tools_available,
                errors=errors,
            )
            _walk_generated_errors(
                effect.get("else"),
                path=effect_label,
                allowed_providers=allowed_providers,
                known_providers=known_providers,
                allowed_shell_commands=allowed_shell_commands,
                generating_effect_path=generating_effect_path,
                allow_tools_available=allow_tools_available,
                errors=errors,
            )
        elif etype == "loop":
            _walk_generated_errors(
                effect.get("body"),
                path=effect_label,
                allowed_providers=allowed_providers,
                known_providers=known_providers,
                allowed_shell_commands=allowed_shell_commands,
                generating_effect_path=generating_effect_path,
                allow_tools_available=allow_tools_available,
                errors=errors,
            )
        elif etype == "reflector":
            _walk_generated_errors(
                effect.get("effects") or effect.get("steps"),
                path=effect_label,
                allowed_providers=allowed_providers,
                known_providers=known_providers,
                allowed_shell_commands=allowed_shell_commands,
                generating_effect_path=generating_effect_path,
                allow_tools_available=allow_tools_available,
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
                        allowed_shell_commands=allowed_shell_commands,
                        generating_effect_path=generating_effect_path,
                        allow_tools_available=allow_tools_available,
                        errors=errors,
                    )
                    _walk_generated_errors(
                        parsed.get("finally"),
                        path=effect_label,
                        allowed_providers=allowed_providers,
                        known_providers=known_providers,
                        allowed_shell_commands=allowed_shell_commands,
                        generating_effect_path=generating_effect_path,
                        allow_tools_available=allow_tools_available,
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
    allowed_shell_commands: frozenset[str] | None = None,
    allow_tools_available: bool = True,
) -> list[str]:
    """Every rule a generated document violates (#772); empty means clean.

    Checked statically against the parsed document — before any of its
    effects run — so the caller can refuse up front, the same way a schema
    or allowlist violation already does. *allowed_providers* is the set of
    tool providers this generated document may use: the document that
    generated it uses itself, statically (:func:`static_tool_providers`),
    plus the generating effect's own literal ``allow_tools:``. ``None``
    skips the provider check (rule 3) entirely while still enforcing the
    shell-specific rules (rule 2). *allowed_shell_commands* is the resolved
    ceiling a generated ``shell`` effect's own ``params.allowed_commands``
    may draw from (:func:`current_shell_allowlist`); ``None`` falls back to
    the bare host default (no document-literal commands, no pin).
    *generating_effect_path* is the dotted state path of the ``use``/
    ``reflector`` effect that produced this document, named in a refusal so
    the fix ("add it to allow_tools on ...") is actionable; ignored when
    *allowed_providers* is ``None``. *allow_tools_available* is ``False``
    for a decomposition plan (:func:`circuitry.core.decompose._check_generated_tool_ceiling`):
    it has no generating ``use``/``reflector`` effect of its own to carry a
    literal ``allow_tools:``, so a provider-outside-the-ceiling refusal
    there names the fixed safe set instead of a field that doesn't exist.
    """
    errors: list[str] = []
    known_providers = None if allowed_providers is None else _known_providers()
    shell_commands = (
        allowed_shell_commands
        if allowed_shell_commands is not None
        else current_shell_allowlist(None)
    )
    if isinstance(orch, dict):
        _walk_generated_errors(
            orch.get("effects") or orch.get("steps"),
            path="",
            allowed_providers=allowed_providers,
            known_providers=known_providers,
            allowed_shell_commands=shell_commands,
            generating_effect_path=generating_effect_path,
            allow_tools_available=allow_tools_available,
            errors=errors,
        )
        _walk_generated_errors(
            orch.get("finally"),
            path="",
            allowed_providers=allowed_providers,
            known_providers=known_providers,
            allowed_shell_commands=shell_commands,
            generating_effect_path=generating_effect_path,
            allow_tools_available=allow_tools_available,
            errors=errors,
        )
    return errors


def dispatch_errors(
    *,
    provider: Any,
    params: Any,
    runtime_config: Mapping[str, Any] | None,
    effect_label: str,
) -> list[str]:
    """Rule 3: the authoritative check, run right before a tool effect
    dispatches, on every attempt -- against its *final* provider and
    params, after Mustache rendering and the ``params_json`` merge.

    A no-op (returns ``[]``) when the document currently executing carries
    no limits: ``runtime_config`` has no truthy
    :data:`GENERATED_DOCUMENT_IS_GENERATED_KEY` -- a literal document's own
    tool effects are never checked here, only by the allowlist/capability
    gates that already cover every document. Otherwise calls the same
    :func:`_tool_effect_errors` the compile-time walk
    (:func:`generated_document_errors`) calls, so the two can never
    disagree on what a rule means -- only on what each can see: this sees
    whatever ``params_json`` merged in and every retry's own rendered
    values; the walk only ever sees the parsed, pre-``params_json``
    document.
    """
    if not isinstance(runtime_config, Mapping) or not runtime_config.get(
        GENERATED_DOCUMENT_IS_GENERATED_KEY
    ):
        return []
    allowed_providers = frozenset(
        runtime_config.get(GENERATED_DOCUMENT_PROVIDERS_KEY) or ()
    )
    generating_effect_path = str(
        runtime_config.get(GENERATED_DOCUMENT_GENERATING_PATH_KEY) or ""
    )
    allow_tools_available = bool(
        runtime_config.get(GENERATED_DOCUMENT_ALLOW_TOOLS_AVAILABLE_KEY, True)
    )
    return _tool_effect_errors(
        effect_label=effect_label,
        provider_name=_normalize_name(provider),
        params=params if isinstance(params, Mapping) else {},
        allowed_providers=allowed_providers,
        known_providers=_known_providers(),
        allowed_shell_commands=current_shell_allowlist(runtime_config),
        generating_effect_path=generating_effect_path,
        allow_tools_available=allow_tools_available,
    )
