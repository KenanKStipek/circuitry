from __future__ import annotations

from circuitry.core.lint import splitting_merge_warnings

_SPLITTING_ON = {"complexity": {"splitting": {"enabled": True, "max_input_tokens": 1000}}}


def test_no_warning_when_splitting_is_off() -> None:
    orch = {"effects": [{"type": "prompt", "name": "a", "prompt_type": "object"}]}
    assert splitting_merge_warnings(orch, {}) == []


def test_text_output_needs_no_merge_template() -> None:
    orch = {"effects": [{"type": "prompt", "name": "a", "prompt_type": "text"}]}
    assert splitting_merge_warnings(orch, _SPLITTING_ON) == []


def test_array_output_needs_no_merge_template() -> None:
    orch = {"effects": [{"type": "prompt", "name": "a", "prompt_type": "array"}]}
    assert splitting_merge_warnings(orch, _SPLITTING_ON) == []


def test_json_with_array_schema_needs_no_merge_template() -> None:
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "a",
                "prompt_type": "json",
                "schema": {"type": "array"},
            }
        ]
    }
    assert splitting_merge_warnings(orch, _SPLITTING_ON) == []


def test_object_output_without_merge_template_warns() -> None:
    orch = {"effects": [{"type": "prompt", "name": "a", "prompt_type": "object"}]}
    warnings = splitting_merge_warnings(orch, _SPLITTING_ON)
    assert len(warnings) == 1
    assert "effects[0] ('a')" in warnings[0]
    assert "split.merge.template" in warnings[0]


def test_object_output_with_merge_template_is_silent() -> None:
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "a",
                "prompt_type": "object",
                "split": {"merge": {"template": "{{#parts}}{{.}}{{/parts}}"}},
            }
        ]
    }
    assert splitting_merge_warnings(orch, _SPLITTING_ON) == []


def test_json_with_object_schema_warns() -> None:
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "a",
                "prompt_type": "json",
                "schema": {"type": "object"},
            }
        ]
    }
    warnings = splitting_merge_warnings(orch, _SPLITTING_ON)
    assert len(warnings) == 1


def test_own_budget_tokens_warns_even_with_the_runtime_switch_off() -> None:
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "a",
                "prompt_type": "object",
                "split": {"budget_tokens": 500},
            }
        ]
    }
    warnings = splitting_merge_warnings(orch, {})
    assert len(warnings) == 1
    assert "split.budget_tokens" in warnings[0]


def test_split_enabled_false_is_never_warned_about_even_with_the_switch_on() -> None:
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "a",
                "prompt_type": "object",
                "split": {"enabled": False},
            }
        ]
    }
    assert splitting_merge_warnings(orch, _SPLITTING_ON) == []


def test_split_enabled_false_is_never_warned_about_even_with_its_own_budget_tokens() -> None:
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "a",
                "prompt_type": "object",
                "split": {"enabled": False, "budget_tokens": 500},
            }
        ]
    }
    assert splitting_merge_warnings(orch, _SPLITTING_ON) == []


def test_walks_nested_effects() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "name": "gate",
                "then": [{"type": "prompt", "name": "a", "prompt_type": "boolean"}],
            }
        ]
    }
    warnings = splitting_merge_warnings(orch, _SPLITTING_ON)
    assert len(warnings) == 1
    assert "effects[0].then[0]" in warnings[0]
