from __future__ import annotations

import pytest

from circuitry.core.compiler import compile_orchestration


def test_compile_accepts_valid_basic_orchestration() -> None:
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "group",
                "effects": [
                    {
                        "type": "prompt",
                        "name": "ask_name",
                        "template": "Hello {{input}}",
                    }
                ],
            }
        ]
    }
    result = compile_orchestration(orch=orch, root_name="prime")
    assert result.name == "prime"
    assert len(result.effects) == 1


def test_compile_rejects_missing_prompt_name() -> None:
    orch = {"effects": [{"type": "prompt", "template": "hello"}]}
    with pytest.raises(ValueError, match="missing required field 'name'"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_rejects_name_with_dot() -> None:
    orch = {"effects": [{"type": "prompt", "name": "bad.name", "template": "hello"}]}
    with pytest.raises(ValueError, match=r"'\.' is not allowed in names"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_rejects_duplicate_sibling_names_root_scope() -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "step", "template": "a"},
            {"type": "prompt", "name": "step", "template": "b"},
        ]
    }
    with pytest.raises(
        ValueError, match="Duplicate effect name 'step' in scope 'prime'"
    ):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_rejects_duplicate_sibling_names_nested_scope() -> None:
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "outer",
                "effects": [
                    {"type": "prompt", "name": "step", "template": "a"},
                    {"type": "prompt", "name": "step", "template": "b"},
                ],
            }
        ]
    }
    with pytest.raises(
        ValueError, match="Duplicate effect name 'step' in scope 'prime.outer'"
    ):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_rejects_non_list_conditional_then() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "name": "gate",
                "if": {"mode": "cel", "expr": "state.input.ok == true"},
                "then": {"type": "prompt", "name": "a", "template": "a"},
            }
        ]
    }
    with pytest.raises(ValueError, match=r"prime\.effects\[0\]\.then must be a list"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_rejects_non_list_loop_body() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "repeat",
                "each": {"in": "prime.items.value", "as": "item"},
                "body": {"type": "prompt", "name": "a", "template": "a"},
            }
        ]
    }
    with pytest.raises(ValueError, match=r"prime\.effects\[0\]\.body must be a list"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_rejects_non_mapping_effect_entry() -> None:
    orch = {"effects": ["not-an-effect"]}
    with pytest.raises(ValueError, match="must be an object/mapping"):
        compile_orchestration(orch=orch, root_name="prime")
