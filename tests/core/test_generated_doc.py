"""The generated-document ceiling (#772): telling an author's own
``inline:`` (literal structure, values templated in) from a document whose
structure only exists because it was rendered, and the tool-provider/shell
rules a generated document is held to.
"""

from __future__ import annotations

from circuitry.core.compiler import apply_effect_overrides, compile_orchestration
from circuitry.generated_doc import (
    DECOMPOSITION_SAFE_PROVIDERS,
    current_shell_allowlist,
    generated_document_errors,
    is_generated_inline,
    resolved_shell_allowlist,
    static_shell_allowed_commands,
    static_tool_providers,
    static_tool_providers_from_definitions,
)

# ── is_generated_inline ──────────────────────────────────────────────────


def test_whole_template_as_one_tag_is_generated() -> None:
    rendered = {"effects": [{"type": "tool", "name": "t", "provider": "shell"}]}
    assert is_generated_inline("{{{prime.gen.value}}}", rendered) is True


def test_literal_structure_with_templated_scalar_is_not_generated() -> None:
    raw = (
        "effects:\n"
        "  - type: tool\n"
        "    name: step1\n"
        "    provider: shell\n"
        "    params:\n"
        "      command: echo\n"
        "      args: ['{{msg}}']\n"
        "      allowed_commands: [echo]\n"
    )
    rendered = {
        "effects": [
            {
                "type": "tool",
                "name": "step1",
                "provider": "shell",
                "params": {
                    "command": "echo",
                    "args": ["hello"],
                    "allowed_commands": ["echo"],
                },
            }
        ]
    }
    assert is_generated_inline(raw, rendered) is False


def test_rendered_structure_with_an_extra_effect_is_generated() -> None:
    raw = "effects:\n  - type: prompt\n    name: a\n    template: hi\n"
    rendered = {
        "effects": [
            {"type": "prompt", "name": "a", "template": "hi"},
            {"type": "tool", "name": "b", "provider": "shell"},
        ]
    }
    assert is_generated_inline(raw, rendered) is True


def test_rendered_list_longer_than_template_is_generated() -> None:
    raw = "effects:\n  - type: prompt\n    name: a\n    template: '{{#each items}}{{this}}{{/each}}'\n"
    rendered = {
        "effects": [
            {"type": "prompt", "name": "a", "template": "1"},
            {"type": "prompt", "name": "b", "template": "2"},
        ]
    }
    assert is_generated_inline(raw, rendered) is True


def test_a_templated_provider_is_generated_even_when_it_renders_to_the_same_name() -> None:
    """A Mustache tag in a security-sensitive field (#772 rule 6) is
    generated even when it happens to render to exactly what the author
    could have written literally -- a tag that renders to 'shell' today
    can render to anything else tomorrow."""
    raw = (
        "effects:\n"
        "  - type: tool\n"
        "    name: t\n"
        "    provider: '{{prime.pick.value}}'\n"
        "    params:\n"
        "      command: touch\n"
        "      allowed_commands: [touch]\n"
    )
    rendered = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "shell",
                "params": {"command": "touch", "allowed_commands": ["touch"]},
            }
        ]
    }
    assert is_generated_inline(raw, rendered) is True


def test_a_templated_allowed_commands_entry_is_generated() -> None:
    raw = (
        "effects:\n"
        "  - type: tool\n"
        "    name: t\n"
        "    provider: shell\n"
        "    params:\n"
        "      command: touch\n"
        "      allowed_commands: ['{{prime.pick.value}}']\n"
    )
    rendered = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "shell",
                "params": {"command": "touch", "allowed_commands": ["touch"]},
            }
        ]
    }
    assert is_generated_inline(raw, rendered) is True


def test_a_templated_allow_tools_entry_is_generated() -> None:
    raw = (
        "effects:\n"
        "  - type: use\n"
        "    name: child\n"
        "    allow_tools: ['{{prime.pick.value}}']\n"
        "    inline: 'effects: []'\n"
    )
    rendered = {
        "effects": [
            {
                "type": "use",
                "name": "child",
                "allow_tools": ["docker"],
                "inline": "effects: []",
            }
        ]
    }
    assert is_generated_inline(raw, rendered) is True


# ── static_tool_providers ────────────────────────────────────────────────


def test_static_tool_providers_collects_direct_tool_effects() -> None:
    orch = {
        "effects": [
            {"type": "tool", "name": "a", "provider": "shell"},
            {"type": "dynamic", "name": "d", "effects": [{"type": "tool", "name": "b", "provider": "git"}]},
        ]
    }
    assert static_tool_providers(orch) == frozenset({"shell", "git"})


def test_static_tool_providers_recurses_into_literal_inline_use_child() -> None:
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "child",
                "inline": "effects:\n  - type: tool\n    name: x\n    provider: docker\n",
            }
        ]
    }
    assert static_tool_providers(orch) == frozenset({"docker"})


def test_static_tool_providers_does_not_follow_path_or_ref() -> None:
    orch = {"effects": [{"type": "use", "name": "child", "path": "other.yml"}]}
    assert static_tool_providers(orch) == frozenset()


# ── generated_document_errors ────────────────────────────────────────────


def test_allowed_commands_on_generated_shell_effect_is_always_refused() -> None:
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "shell",
                "params": {"command": "touch", "allowed_commands": ["touch"]},
            }
        ]
    }
    errors = generated_document_errors(
        orch, allowed_providers=frozenset({"shell"}), generating_effect_path="prime.plan_it"
    )
    assert errors
    assert "allowed_commands" in errors[0]
    assert "shell" in errors[0]
    assert "prime.plan_it" in errors[0]


def test_service_shell_true_is_always_refused() -> None:
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "service",
                "params": {"command": "touch x", "shell": True},
            }
        ]
    }
    errors = generated_document_errors(
        orch, allowed_providers=frozenset({"service"}), generating_effect_path="prime.plan_it"
    )
    assert errors
    assert "shell: true" in errors[0]
    assert "prime.plan_it" in errors[0]


def test_provider_outside_the_allowed_set_is_refused() -> None:
    orch = {"effects": [{"type": "tool", "name": "t", "provider": "git"}]}
    errors = generated_document_errors(
        orch, allowed_providers=frozenset({"shell"}), generating_effect_path="prime.plan_it"
    )
    assert errors
    assert "git" in errors[0]
    assert "allow_tools" in errors[0]
    assert "prime.plan_it" in errors[0]
    assert "ceiling" not in errors[0]


def test_an_unknown_provider_is_not_flagged_here() -> None:
    """A provider that isn't a real plugin at all fails the usual way
    ("Unknown plugin") at dispatch, never this check -- adding an unknown
    name to allow_tools would itself be a check error."""
    orch = {"effects": [{"type": "tool", "name": "t", "provider": "nonexistent_tool"}]}
    errors = generated_document_errors(
        orch, allowed_providers=frozenset({"shell"}), generating_effect_path="prime.plan_it"
    )
    assert errors == []


def test_provider_within_ceiling_is_clean() -> None:
    orch = {"effects": [{"type": "tool", "name": "t", "provider": "shell", "params": {"command": "echo"}}]}
    errors = generated_document_errors(orch, allowed_providers=frozenset({"shell"}))
    assert errors == []


def test_provider_name_is_matched_case_insensitively() -> None:
    """'provider: Shell' is the same tool as 'provider: shell' (#772 rule
    1) -- matching the exact normalization build_plugin applies before
    dispatch, so this check can't be bypassed just by changing case."""
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "Shell",
                "params": {"command": "touch", "allowed_commands": ["touch"]},
            }
        ]
    }
    errors = generated_document_errors(
        orch, allowed_providers=frozenset({"shell"}), generating_effect_path="prime.plan_it"
    )
    assert errors
    assert "allowed_commands" in errors[0]


def test_provider_outside_the_allowed_set_is_refused_case_insensitively() -> None:
    orch = {"effects": [{"type": "tool", "name": "t", "provider": "GIT"}]}
    errors = generated_document_errors(
        orch, allowed_providers=frozenset({"shell"}), generating_effect_path="prime.plan_it"
    )
    assert errors
    assert "'git'" in errors[0]


def test_effect_type_is_matched_case_insensitively() -> None:
    orch = {
        "effects": [
            {
                "type": "Dynamic",
                "name": "d",
                "effects": [{"type": "TOOL", "name": "t", "provider": "git"}],
            }
        ]
    }
    errors = generated_document_errors(
        orch, allowed_providers=frozenset({"shell"}), generating_effect_path="prime.plan_it"
    )
    assert errors
    assert "git" in errors[0]


# ── shell allowlist union (rule 2) ───────────────────────────────────────


def test_static_shell_allowed_commands_collects_literal_shell_effects() -> None:
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "a",
                "provider": "shell",
                "params": {"command": "node", "allowed_commands": ["node", "jq"]},
            }
        ]
    }
    assert static_shell_allowed_commands(orch) == frozenset({"node", "jq"})


def test_resolved_shell_allowlist_unions_default_and_document_literal() -> None:
    allowed = resolved_shell_allowlist(frozenset({"node"}), host_pin=None)
    assert "node" in allowed
    assert "ls" in allowed  # the default allowlist


def test_resolved_shell_allowlist_intersects_with_host_pin() -> None:
    allowed = resolved_shell_allowlist(
        frozenset({"node", "jq"}), host_pin=frozenset({"node", "ls"})
    )
    assert allowed == frozenset({"node", "ls"})


def test_current_shell_allowlist_falls_back_to_the_host_default_with_no_key_set() -> None:
    assert "ls" in current_shell_allowlist({})
    assert "node" not in current_shell_allowlist({})


def test_a_generated_shell_command_within_the_document_literal_allowlist_is_accepted() -> None:
    """Rule 2: a generated shell step may run what the enclosing document's
    own literal shell effects (and the host config) already allow."""
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "shell",
                "params": {"command": "node", "allowed_commands": ["node"]},
            }
        ]
    }
    errors = generated_document_errors(
        orch,
        allowed_providers=frozenset({"shell"}),
        generating_effect_path="prime.plan_it",
        allowed_shell_commands=resolved_shell_allowlist(frozenset({"node"}), host_pin=None),
    )
    assert errors == []


def test_a_generated_shell_command_beyond_the_document_literal_allowlist_is_refused() -> None:
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "shell",
                "params": {"command": "rm", "allowed_commands": ["node", "rm"]},
            }
        ]
    }
    errors = generated_document_errors(
        orch,
        allowed_providers=frozenset({"shell"}),
        generating_effect_path="prime.plan_it",
        allowed_shell_commands=resolved_shell_allowlist(frozenset({"node"}), host_pin=None),
    )
    assert errors
    assert "'rm'" in errors[0]
    assert "node" not in errors[0]
    assert "runtime.plugins.shell.allowed_commands" in errors[0]


# ── static_tool_providers_from_definitions (rule 9) ──────────────────────


def test_static_tool_providers_from_definitions_matches_the_raw_walk() -> None:
    orch = {
        "effects": [
            {"type": "tool", "name": "a", "provider": "shell"},
            {
                "type": "use",
                "name": "child",
                "inline": "effects:\n  - type: tool\n    name: x\n    provider: docker\n",
            },
        ]
    }
    root_def = compile_orchestration(orch=orch, root_name="prime")
    assert static_tool_providers_from_definitions(root_def) == frozenset({"shell", "docker"})


def test_static_tool_providers_from_definitions_reflects_a_profile_provider_override() -> None:
    """The root document's own providers are computed from the *effective*
    document, after a profile's per-effect provider override already
    applied -- the same compiled-tree semantics electricity's own
    static_tool_providers_from_op uses (#772 rule 9), not the raw,
    pre-override YAML."""
    orch = {
        "effects": [
            {"type": "tool", "name": "t", "provider": "shell", "params": {"command": "echo"}},
        ]
    }
    root_def = compile_orchestration(orch=orch, root_name="prime")
    assert static_tool_providers_from_definitions(root_def) == frozenset({"shell"})

    overridden, matched = apply_effect_overrides(root_def, {"t": {"provider": "json"}})
    assert matched == {"t"}
    assert static_tool_providers_from_definitions(overridden) == frozenset({"json"})


def test_decomposition_safe_providers_are_side_effect_free_data_tools() -> None:
    # hash and csv are deliberately not here: both support from_path, which
    # reads any file on disk -- a plan cannot be held to a data-only ceiling
    # while one of its always-allowed tools can read ~/.ssh/id_rsa (#772).
    assert frozenset(
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
    ) == DECOMPOSITION_SAFE_PROVIDERS


# ── the provider ceiling runs independently of the shell/service checks ──


def test_shell_effect_within_the_shell_allowlist_but_outside_the_provider_ceiling_is_still_refused() -> None:
    """A shell effect whose own allowed_commands is in order must still be
    refused when 'shell' itself isn't in the provider ceiling -- the three
    checks are independent, never an if/elif chain where the allowlist
    check matching skips the provider check entirely."""
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "shell",
                "params": {"command": "touch", "allowed_commands": ["touch"]},
            }
        ]
    }
    errors = generated_document_errors(
        orch,
        allowed_providers=frozenset({"json"}),
        generating_effect_path="prime.plan_it",
        allowed_shell_commands=frozenset({"touch"}),
    )
    assert errors
    assert "shell" in errors[0]
    assert "allow_tools" in errors[0]


def test_service_shell_true_outside_the_provider_ceiling_is_still_refused_for_the_provider_too() -> None:
    """A 'service' effect's shell: true is refused on its own terms; the
    provider-ceiling violation is reported too, not swallowed by the
    shell: true branch matching first."""
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "service",
                "params": {"command": "touch x", "shell": True},
            }
        ]
    }
    errors = generated_document_errors(
        orch,
        allowed_providers=frozenset({"json"}),
        generating_effect_path="prime.plan_it",
    )
    assert len(errors) == 2
    assert any("shell: true" in e for e in errors)
    assert any("service" in e and "allow_tools" in e for e in errors)


# ── service shell: true uses the real plugin's own boolean coercion ──


def test_service_shell_set_to_a_string_the_old_gate_missed_is_refused() -> None:
    """plugins.base._as_bool treats every non-empty string except
    false/0/no/off/n as true -- a gate that only recognised
    1/true/yes/on let a string like 'y' or 'on please' through as
    falsy. This must be refused the same as shell: true."""
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "service",
                "params": {"command": "touch x", "shell": "y"},
            }
        ]
    }
    errors = generated_document_errors(
        orch, allowed_providers=frozenset({"service"}), generating_effect_path="prime.plan_it"
    )
    assert errors
    assert "shell: true" in errors[0]


# ── a generated service start's own executable is checked like shell's command ──


def test_generated_service_start_with_an_allowed_executable_is_clean() -> None:
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "service",
                "params": {"action": "start", "name": "svc", "command": ["node", "server.js"]},
            }
        ]
    }
    errors = generated_document_errors(
        orch,
        allowed_providers=frozenset({"service"}),
        generating_effect_path="prime.plan_it",
        allowed_shell_commands=frozenset({"node"}),
    )
    assert errors == []


def test_generated_service_start_with_a_disallowed_executable_is_refused() -> None:
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "service",
                "params": {"action": "start", "name": "svc", "command": ["rm", "-rf", "/"]},
            }
        ]
    }
    errors = generated_document_errors(
        orch,
        allowed_providers=frozenset({"service"}),
        generating_effect_path="prime.plan_it",
        allowed_shell_commands=frozenset({"node"}),
    )
    assert errors
    assert "'rm'" in errors[0]


def test_generated_service_start_with_a_path_executable_is_refused() -> None:
    """A path is never 'in' the shell allowlist, however it compares --
    the same rule the shell tool itself applies to params.command."""
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "service",
                "params": {"action": "start", "name": "svc", "command": ["/usr/bin/node"]},
            }
        ]
    }
    errors = generated_document_errors(
        orch,
        allowed_providers=frozenset({"service"}),
        generating_effect_path="prime.plan_it",
        allowed_shell_commands=frozenset({"node"}),
    )
    assert errors
    assert "/usr/bin/node" in errors[0]


# ── a generated service start's env can't redirect how the executable resolves ──


def test_generated_service_start_setting_env_path_is_refused() -> None:
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "service",
                "params": {
                    "action": "start",
                    "name": "svc",
                    "command": ["node", "server.js"],
                    "env": {"PATH": "/tmp/evil"},
                },
            }
        ]
    }
    errors = generated_document_errors(
        orch,
        allowed_providers=frozenset({"service"}),
        generating_effect_path="prime.plan_it",
        allowed_shell_commands=frozenset({"node"}),
    )
    assert errors
    assert "'PATH'" in errors[0]


def test_generated_service_start_setting_env_ld_preload_is_refused() -> None:
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "service",
                "params": {
                    "action": "start",
                    "name": "svc",
                    "command": ["node", "server.js"],
                    "env": {"LD_PRELOAD": "/tmp/evil.so"},
                },
            }
        ]
    }
    errors = generated_document_errors(
        orch,
        allowed_providers=frozenset({"service"}),
        generating_effect_path="prime.plan_it",
        allowed_shell_commands=frozenset({"node"}),
    )
    assert errors
    assert "'LD_PRELOAD'" in errors[0]


def test_generated_service_start_with_an_unrelated_env_key_is_clean() -> None:
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "service",
                "params": {
                    "action": "start",
                    "name": "svc",
                    "command": ["node", "server.js"],
                    "env": {"PORT": "8080"},
                },
            }
        ]
    }
    errors = generated_document_errors(
        orch,
        allowed_providers=frozenset({"service"}),
        generating_effect_path="prime.plan_it",
        allowed_shell_commands=frozenset({"node"}),
    )
    assert errors == []


# ── a nested, still-templated inline inside a literal inline is generated ──


def test_a_rendered_inline_nested_inside_a_literal_inline_is_generated() -> None:
    """'inline' is a security-sensitive shape key: a nested use effect's
    own inline text must be byte-identical between the unrendered
    template and the rendered document, or the outer child counts as
    generated -- closing the gap where a model's own nested document,
    once rendered into an inner inline:, could pass as the author's own
    merely because the outer structure still matched."""
    raw = (
        "effects:\n"
        "  - type: use\n"
        "    name: wrapper\n"
        "    inline: |\n"
        "      effects:\n"
        "        - type: use\n"
        "          name: run\n"
        "          inline: \"{{{prime.plan.value}}}\"\n"
    )
    rendered = {
        "effects": [
            {
                "type": "use",
                "name": "wrapper",
                "inline": (
                    "effects:\n- {type: tool, name: x, provider: shell, "
                    "params: {command: curl, allowed_commands: [curl]}}\n"
                ),
            }
        ]
    }
    assert is_generated_inline(raw, rendered)
