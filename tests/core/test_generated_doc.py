"""The generated-document ceiling (#772): telling an author's own
``inline:`` (literal structure, values templated in) from a document whose
structure only exists because it was rendered, and the tool-provider/shell
rules a generated document is held to.
"""

from __future__ import annotations

from circuitry.generated_doc import (
    generated_document_errors,
    is_generated_inline,
    static_tool_providers,
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
