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


# --- Conditional validation: prompt_type requires schema ---


def test_compile_rejects_json_prompt_without_schema() -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "x", "template": "hi", "prompt_type": "json"},
        ]
    }
    with pytest.raises(ValueError, match="requires a 'schema' field"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_rejects_object_prompt_without_schema() -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "x", "template": "hi", "prompt_type": "object"},
        ]
    }
    with pytest.raises(ValueError, match="requires a 'schema' field"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_rejects_array_prompt_without_schema() -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "x", "template": "hi", "prompt_type": "array"},
        ]
    }
    with pytest.raises(ValueError, match="requires a 'schema' field"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_accepts_json_prompt_with_schema() -> None:
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "x",
                "template": "hi",
                "prompt_type": "json",
                "schema": {"type": "object"},
            },
        ]
    }
    compile_orchestration(orch=orch, root_name="prime")


def test_compile_accepts_text_prompt_without_schema() -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "x", "template": "hi", "prompt_type": "text"},
        ]
    }
    compile_orchestration(orch=orch, root_name="prime")


# --- Conditional validation: mode model/cel requires template/expr ---


def test_compile_rejects_model_condition_without_template() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "if": {"mode": "model"},
                "then": [{"type": "prompt", "name": "a", "template": "hi"}],
            },
        ]
    }
    with pytest.raises(ValueError, match="mode 'model' requires a 'template' field"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_rejects_cel_condition_without_expr() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "if": {"mode": "cel"},
                "then": [{"type": "prompt", "name": "a", "template": "hi"}],
            },
        ]
    }
    with pytest.raises(ValueError, match="mode 'cel' requires an 'expr' field"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_accepts_model_condition_with_template() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "if": {"mode": "model", "template": "Is it true?"},
                "then": [{"type": "prompt", "name": "a", "template": "hi"}],
            },
        ]
    }
    compile_orchestration(orch=orch, root_name="prime")


def test_compile_accepts_cel_condition_with_expr() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "if": {"mode": "cel", "expr": "state.prime.x.value == true"},
                "then": [{"type": "prompt", "name": "a", "template": "hi"}],
            },
        ]
    }
    compile_orchestration(orch=orch, root_name="prime")


# --- Loop while validation ---


def test_compile_rejects_loop_while_model_without_template() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "while": {"mode": "model"},
                "body": [{"type": "prompt", "name": "a", "template": "hi"}],
            },
        ]
    }
    with pytest.raises(ValueError, match="mode 'model' requires a 'template' field"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_rejects_loop_while_cel_without_expr() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "while": {"mode": "cel"},
                "body": [{"type": "prompt", "name": "a", "template": "hi"}],
            },
        ]
    }
    with pytest.raises(ValueError, match="mode 'cel' requires an 'expr' field"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_accepts_loop_while_model_with_template() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "while": {"mode": "model", "template": "Continue?"},
                "body": [{"type": "prompt", "name": "a", "template": "hi"}],
            },
        ]
    }
    compile_orchestration(orch=orch, root_name="prime")


def test_compile_accepts_loop_while_cel_with_expr() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "while": {"mode": "cel", "expr": "state.prime.x.value < 5"},
                "body": [{"type": "prompt", "name": "a", "template": "hi"}],
            },
        ]
    }
    compile_orchestration(orch=orch, root_name="prime")
