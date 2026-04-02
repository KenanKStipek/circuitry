"""Tests for the use (sub-orchestration) effect."""

from __future__ import annotations

from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest
import yaml

from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store
from circuitry.core.use import (
    UseDefinition,
    UseRuntime,
    _clean_yaml_fences,
    _resolve_dot_path,
    _validate_inline_yaml,
)


# ── Helpers ──────────────────────────────────────────────────────────────────


def _write_orch(tmp_path: Path, name: str, content: dict) -> Path:
    path = tmp_path / name
    path.write_text(yaml.dump(content), encoding="utf-8")
    return path


def _mock_adapter(response: str = "mock response") -> MagicMock:
    adapter = MagicMock()
    adapter.name = "mock"
    result = MagicMock()
    result.text = response
    result.raw = {}
    result.tokens_sent = 10
    result.tokens_received = 5
    adapter.generate.return_value = result
    return adapter


# ── _resolve_dot_path ────────────────────────────────────────────────────────


def test_resolve_dot_path_simple() -> None:
    state = {"prime": {"greet": {"value": "hello"}}}
    assert _resolve_dot_path(state, "prime.greet.value") == "hello"


def test_resolve_dot_path_missing() -> None:
    state = {"prime": {}}
    assert _resolve_dot_path(state, "prime.greet.value") is None


def test_resolve_dot_path_top_level() -> None:
    state = {"name": "World"}
    assert _resolve_dot_path(state, "name") == "World"


# ── Compiler tests ───────────────────────────────────────────────────────────


def test_compile_use_basic() -> None:
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "sub",
                "orchestration": "hello",
            }
        ]
    }
    root = compile_orchestration(orch=orch)
    assert len(root.effects) == 1
    effect = root.effects[0]
    assert isinstance(effect, UseDefinition)
    assert effect.name == "sub"
    assert effect.orchestration == "hello"
    assert effect.inputs is None
    assert effect.outputs is None
    assert effect.on_error == "fail"


def test_compile_use_with_inputs_outputs() -> None:
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "sub",
                "orchestration": "article-summarizer",
                "inputs": {"article_text": "{{prime.fetch.value}}", "max_words": 50},
                "outputs": {"summary": "prime.summarize.value"},
            }
        ]
    }
    root = compile_orchestration(orch=orch)
    effect = root.effects[0]
    assert isinstance(effect, UseDefinition)
    assert effect.inputs == {"article_text": "{{prime.fetch.value}}", "max_words": 50}
    assert effect.outputs == {"summary": "prime.summarize.value"}


def test_compile_use_missing_name() -> None:
    orch = {"effects": [{"type": "use", "orchestration": "hello"}]}
    with pytest.raises(ValueError, match="missing required field 'name'"):
        compile_orchestration(orch=orch)


def test_compile_use_missing_orchestration_and_inline() -> None:
    orch = {"effects": [{"type": "use", "name": "sub"}]}
    with pytest.raises(ValueError, match="requires either"):
        compile_orchestration(orch=orch)


def test_compile_use_invalid_name_with_dot() -> None:
    orch = {"effects": [{"type": "use", "name": "a.b", "orchestration": "hello"}]}
    with pytest.raises(ValueError, match="not allowed"):
        compile_orchestration(orch=orch)


def test_compile_use_on_error_variants() -> None:
    for on_error in ("fail", "skip", "continue"):
        orch = {
            "effects": [
                {
                    "type": "use",
                    "name": "sub",
                    "orchestration": "hello",
                    "on_error": on_error,
                }
            ]
        }
        root = compile_orchestration(orch=orch)
        assert root.effects[0].on_error == on_error


def test_compile_use_duplicate_name() -> None:
    orch = {
        "effects": [
            {"type": "use", "name": "sub", "orchestration": "hello"},
            {"type": "use", "name": "sub", "orchestration": "hello"},
        ]
    }
    with pytest.raises(ValueError, match="Duplicate effect name"):
        compile_orchestration(orch=orch)


# ── Runtime tests ────────────────────────────────────────────────────────────


def test_use_runtime_resolves_file_path(tmp_path: Path) -> None:
    """UseRuntime can resolve a local file path."""
    child_orch = {"effects": [{"type": "prompt", "name": "greet", "template": "Hello {{name}}"}]}
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(
        name="sub",
        orchestration=str(child_path),
        inputs={"name": "World"},
        outputs={"greeting": "prime.greet.value"},
    )

    adapter = _mock_adapter("Hello World!")
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["sub"]["value"]["greeting"] == "Hello World!"
    assert store.state["sub"]["meta"]["error"] is None


def test_use_runtime_resolves_bundled_name() -> None:
    """UseRuntime can resolve a bundled orchestration name."""
    defn = UseDefinition(
        name="sub",
        orchestration="hello",
        inputs={"name": "Tester"},
    )

    adapter = _mock_adapter("Hi Tester!")
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    # Without outputs mapping, value is True on success
    assert store.state["sub"]["value"] is True


def test_use_runtime_resolution_failure() -> None:
    """Non-existent orchestration raises a clear error."""
    defn = UseDefinition(name="sub", orchestration="nonexistent_orch_12345")

    adapter = _mock_adapter()
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")

    with pytest.raises(RuntimeError, match="not found"):
        runtime.execute(store=store, ctx=store.state)


def test_use_runtime_state_isolation(tmp_path: Path) -> None:
    """Parent state is not visible in child; child state is not leaked to parent."""
    child_orch = {"effects": [{"type": "prompt", "name": "echo", "template": "Echo"}]}
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(
        name="sub",
        orchestration=str(child_path),
        inputs={"child_key": "child_value"},
    )

    adapter = _mock_adapter("echoed")
    parent_state = {"parent_secret": "should_not_leak"}
    store = Store(state=parent_state)
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    # Parent state should not have child's internal state keys
    assert "echo" not in store.state
    # Parent should still have its own keys
    assert store.state["parent_secret"] == "should_not_leak"
    # sub.value should be True (no outputs mapping)
    assert store.state["sub"]["value"] is True


def test_use_runtime_input_mapping(tmp_path: Path) -> None:
    """Input values with Mustache templates are rendered against parent context."""
    child_orch = {"effects": [{"type": "prompt", "name": "greet", "template": "Hello {{who}}"}]}
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(
        name="sub",
        orchestration=str(child_path),
        inputs={"who": "{{parent_name}}"},
    )

    adapter = _mock_adapter("Hello Alice!")
    store = Store(state={"parent_name": "Alice"})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    # Check the prompt was rendered with the input
    call_args = adapter.generate.call_args
    assert "Alice" in call_args.kwargs.get("prompt", "")


def test_use_runtime_output_mapping(tmp_path: Path) -> None:
    """Specific child state paths are extracted to parent's .value dict."""
    child_orch = {"effects": [{"type": "prompt", "name": "result", "template": "42"}]}
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(
        name="sub",
        orchestration=str(child_path),
        outputs={"answer": "prime.result.value"},
    )

    adapter = _mock_adapter("42")
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["sub"]["value"] == {"answer": "42"}


def test_use_runtime_no_output_mapping(tmp_path: Path) -> None:
    """Without outputs, .value is True on success."""
    child_orch = {"effects": [{"type": "prompt", "name": "step", "template": "ok"}]}
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(name="sub", orchestration=str(child_path))

    adapter = _mock_adapter("ok")
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["sub"]["value"] is True


def test_use_runtime_on_error_skip() -> None:
    """on_error: skip sets value to None on failure."""
    defn = UseDefinition(
        name="sub",
        orchestration="nonexistent_orch_12345",
        on_error="skip",
    )

    adapter = _mock_adapter()
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["sub"]["value"] is None
    assert store.state["sub"]["meta"]["error"] is not None


def test_use_runtime_on_error_continue() -> None:
    """on_error: continue records error but doesn't raise."""
    defn = UseDefinition(
        name="sub",
        orchestration="nonexistent_orch_12345",
        on_error="continue",
    )

    adapter = _mock_adapter()
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["sub"]["meta"]["error"] is not None


def test_use_runtime_dry_run(tmp_path: Path) -> None:
    """dry_run flag is respected — no child execution."""
    child_orch = {"effects": [{"type": "prompt", "name": "step", "template": "ok"}]}
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(name="sub", orchestration=str(child_path))

    adapter = _mock_adapter()
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model", dry_run=True)
    runtime.execute(store=store, ctx=store.state)

    assert store.state["sub"]["value"] is None
    assert store.state["sub"]["meta"]["dry_run"] is True
    adapter.generate.assert_not_called()


def test_use_runtime_non_string_inputs(tmp_path: Path) -> None:
    """Non-string input values are passed through without Mustache rendering."""
    child_orch = {"effects": [{"type": "prompt", "name": "step", "template": "count: {{count}}"}]}
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(
        name="sub",
        orchestration=str(child_path),
        inputs={"count": 42, "flag": True, "items": [1, 2, 3]},
    )

    adapter = _mock_adapter("counted")
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["sub"]["value"] is True


# ── Integration: use inside dynamic ──────────────────────────────────────────


def test_use_inside_dynamic_chain(tmp_path: Path) -> None:
    """A use effect works as a child of a dynamic chain."""
    child_orch = {"effects": [{"type": "prompt", "name": "greet", "template": "Hi"}]}
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "pipeline",
                "flow": "chain",
                "effects": [
                    {"type": "prompt", "name": "step1", "template": "First"},
                    {
                        "type": "use",
                        "name": "child_step",
                        "orchestration": str(child_path),
                    },
                ],
            }
        ]
    }

    root = compile_orchestration(orch=orch)
    adapter = _mock_adapter("response")
    store = Store(state={})

    DynamicRuntime(
        root, adapter=adapter, model="test-model"
    ).execute(store=store)

    assert store.state["prime"]["pipeline"]["step1"]["value"] == "response"
    assert store.state["prime"]["pipeline"]["child_step"]["value"] is True


# ── Schema validation tests ──────────────────────────────────────────────────


def test_schema_validates_use_effect() -> None:
    from circuitry.cli.runtime_shim import validate
    import tempfile

    orch = {
        "effects": [
            {
                "type": "use",
                "name": "sub",
                "orchestration": "hello",
                "inputs": {"name": "World"},
                "outputs": {"greeting": "prime.greet.value"},
            }
        ]
    }

    with tempfile.NamedTemporaryFile(suffix=".yml", mode="w", delete=False) as f:
        yaml.dump(orch, f)
        path = Path(f.name)

    result = validate(path)
    assert result["ok"] is True


def test_schema_rejects_use_without_orchestration() -> None:
    from circuitry.cli.runtime_shim import validate
    import tempfile

    orch = {"effects": [{"type": "use", "name": "sub"}]}

    with tempfile.NamedTemporaryFile(suffix=".yml", mode="w", delete=False) as f:
        yaml.dump(orch, f)
        path = Path(f.name)

    result = validate(path)
    assert result["ok"] is False


# ── Inline YAML helpers ──────────────────────────────────────────────────────


def test_clean_yaml_fences() -> None:
    text = "```yaml\neffects:\n  - type: prompt\n```"
    cleaned = _clean_yaml_fences(text)
    assert "```" not in cleaned
    assert "effects:" in cleaned


def test_clean_yaml_fences_with_document_separator() -> None:
    text = "---\neffects:\n  - type: prompt"
    cleaned = _clean_yaml_fences(text)
    assert "---" not in cleaned
    assert "effects:" in cleaned


def test_validate_inline_yaml_valid() -> None:
    yaml_text = "effects:\n  - type: prompt\n    name: greet\n    template: Hello\n"
    ok, errors = _validate_inline_yaml(yaml_text)
    assert ok is True
    assert errors == []


def test_validate_inline_yaml_invalid_yaml() -> None:
    ok, errors = _validate_inline_yaml("not: [valid: yaml: {{")
    assert ok is False
    assert any("parse error" in e.lower() or "YAML" in e for e in errors)


def test_validate_inline_yaml_missing_effects() -> None:
    ok, errors = _validate_inline_yaml("adapter: ollama\n")
    assert ok is False
    assert any("effects" in e.lower() for e in errors)


# ── Compiler: inline ────────────────────────────────────────────────────────


def test_compile_use_inline() -> None:
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "run_plan",
                "inline": "{{prime.plan.value}}",
            }
        ]
    }
    root = compile_orchestration(orch=orch)
    effect = root.effects[0]
    assert isinstance(effect, UseDefinition)
    assert effect.orchestration is None
    assert effect.inline == "{{prime.plan.value}}"


def test_compile_use_both_orchestration_and_inline_fails() -> None:
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "sub",
                "orchestration": "hello",
                "inline": "effects: []",
            }
        ]
    }
    with pytest.raises(ValueError, match="both"):
        compile_orchestration(orch=orch)


def test_compile_use_neither_orchestration_nor_inline_fails() -> None:
    orch = {"effects": [{"type": "use", "name": "sub"}]}
    with pytest.raises(ValueError, match="requires either"):
        compile_orchestration(orch=orch)


def test_compile_use_validate_flag() -> None:
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "run_plan",
                "inline": "{{plan}}",
                "validate": False,
            }
        ]
    }
    root = compile_orchestration(orch=orch)
    assert root.effects[0].validate is False


# ── Runtime: inline ─────────────────────────────────────────────────────────


def test_use_inline_executes_yaml_string() -> None:
    """Inline YAML is parsed, compiled, and executed."""
    inline_yaml = yaml.dump({
        "effects": [{"type": "prompt", "name": "greet", "template": "Hello inline"}]
    })

    defn = UseDefinition(
        name="run_plan",
        inline=inline_yaml,  # literal YAML, no Mustache
    )

    adapter = _mock_adapter("Hello from inline!")
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["run_plan"]["value"] is True
    assert store.state["run_plan"]["meta"]["inline"] is True
    adapter.generate.assert_called_once()


def test_use_inline_with_mustache_rendering() -> None:
    """Inline template is Mustache-rendered against parent context before parsing."""
    defn = UseDefinition(
        name="run_plan",
        inline="{{plan_yaml}}",
    )

    plan_yaml = yaml.dump({
        "effects": [{"type": "prompt", "name": "step", "template": "Rendered!"}]
    })

    adapter = _mock_adapter("Rendered!")
    store = Store(state={"plan_yaml": plan_yaml})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["run_plan"]["value"] is True


def test_use_inline_with_output_mapping() -> None:
    """Output mapping works with inline orchestrations."""
    inline_yaml = yaml.dump({
        "effects": [{"type": "prompt", "name": "answer", "template": "42"}]
    })

    defn = UseDefinition(
        name="run_plan",
        inline=inline_yaml,
        outputs={"result": "prime.answer.value"},
    )

    adapter = _mock_adapter("42")
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["run_plan"]["value"] == {"result": "42"}


def test_use_inline_validation_rejects_bad_yaml() -> None:
    """Invalid inline YAML (missing effects) is caught by validation."""
    defn = UseDefinition(
        name="run_plan",
        inline="adapter: ollama",  # no effects key
        validate=True,
    )

    adapter = _mock_adapter()
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")

    with pytest.raises(RuntimeError, match="validation failed"):
        runtime.execute(store=store, ctx=store.state)


def test_use_inline_validation_skip() -> None:
    """validate: false skips schema validation — only YAML parsing required."""
    # This YAML is structurally valid YAML but doesn't have 'effects' at top level.
    # With validate=False, the compiler will still fail, but we get past validation.
    defn = UseDefinition(
        name="run_plan",
        inline="effects:\n  - type: prompt\n    name: ok\n    template: works\n",
        validate=False,
    )

    adapter = _mock_adapter("works")
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["run_plan"]["value"] is True


def test_use_inline_strips_code_fences() -> None:
    """LLM output with markdown code fences is cleaned before parsing."""
    fenced = "```yaml\neffects:\n  - type: prompt\n    name: step\n    template: fenced\n```"

    defn = UseDefinition(name="run_plan", inline=fenced)

    adapter = _mock_adapter("fenced!")
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["run_plan"]["value"] is True


def test_use_inline_on_error_skip_for_validation_failure() -> None:
    """on_error: skip handles validation failures gracefully."""
    defn = UseDefinition(
        name="run_plan",
        inline="not_valid_orchestration: true",
        on_error="skip",
    )

    adapter = _mock_adapter()
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["run_plan"]["value"] is None
    assert store.state["run_plan"]["meta"]["error"] is not None


# ── Schema: inline ──────────────────────────────────────────────────────────


def test_schema_validates_use_inline_effect() -> None:
    from circuitry.cli.runtime_shim import validate
    import tempfile

    orch = {
        "effects": [
            {
                "type": "use",
                "name": "run_plan",
                "inline": "{{prime.plan.value}}",
            }
        ]
    }

    with tempfile.NamedTemporaryFile(suffix=".yml", mode="w", delete=False) as f:
        yaml.dump(orch, f)
        path = Path(f.name)

    result = validate(path)
    assert result["ok"] is True


def test_schema_validates_use_with_validate_false() -> None:
    from circuitry.cli.runtime_shim import validate
    import tempfile

    orch = {
        "effects": [
            {
                "type": "use",
                "name": "run_plan",
                "inline": "{{plan}}",
                "validate": False,
            }
        ]
    }

    with tempfile.NamedTemporaryFile(suffix=".yml", mode="w", delete=False) as f:
        yaml.dump(orch, f)
        path = Path(f.name)

    result = validate(path)
    assert result["ok"] is True
