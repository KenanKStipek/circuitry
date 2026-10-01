"""Tests for orchestration interface declarations with use effects."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest
import yaml

from circuitry.core.store import Store
from circuitry.core.use import UseDefinition, UseRuntime


def _write_orch(tmp_path: Path, name: str, content: dict) -> Path:
    path = tmp_path / name
    path.write_text(yaml.dump(content), encoding="utf-8")
    return path


def _mock_adapter(response: str = "mock") -> MagicMock:
    adapter = MagicMock()
    adapter.name = "mock"
    result = MagicMock()
    result.text = response
    result.raw = {}
    result.tokens_sent = 10
    result.tokens_received = 5
    adapter.generate.return_value = result
    return adapter


# ── Interface: required input validation ─────────────────────────────────────


def test_interface_validates_required_inputs(tmp_path: Path) -> None:
    """Missing required input declared in interface raises ValueError."""
    child_orch = {
        "interface": {
            "inputs": {
                "article_text": {"type": "string", "required": True},
            }
        },
        "effects": [{"type": "prompt", "name": "step", "template": "Summarize: {{input.article_text}}"}],
    }
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(
        name="sub",
        orchestration=str(child_path),
        inputs={},  # missing article_text
    )

    adapter = _mock_adapter()
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")

    with pytest.raises(RuntimeError, match=r"missing required input.*article_text"):
        runtime.execute(store=store, ctx=store.state)


def test_interface_passes_when_required_inputs_provided(tmp_path: Path) -> None:
    """Required inputs present → no error."""
    child_orch = {
        "interface": {
            "inputs": {
                "article_text": {"type": "string", "required": True},
            }
        },
        "effects": [{"type": "prompt", "name": "step", "template": "Summarize: {{input.article_text}}"}],
    }
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(
        name="sub",
        orchestration=str(child_path),
        inputs={"article_text": "Some article"},
    )

    adapter = _mock_adapter("Summary")
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["sub"]["step"]["value"] is not None


def test_interface_optional_inputs_not_required(tmp_path: Path) -> None:
    """Optional inputs don't raise when missing."""
    child_orch = {
        "interface": {
            "inputs": {
                "text": {"type": "string", "required": True},
                "max_words": {"type": "number", "required": False},
            }
        },
        "effects": [{"type": "prompt", "name": "step", "template": "Do: {{input.text}}"}],
    }
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(
        name="sub",
        orchestration=str(child_path),
        inputs={"text": "hello"},  # max_words omitted — that's fine
    )

    adapter = _mock_adapter("done")
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["sub"]["step"]["value"] is not None


# ── Interface: auto-generated output mapping ─────────────────────────────────


def test_interface_auto_generates_outputs(tmp_path: Path) -> None:
    """When use has no explicit outputs and interface declares outputs, auto-map."""
    child_orch = {
        "interface": {
            "outputs": {
                "summary": {"type": "string", "path": "prime.step.value"},
            }
        },
        "effects": [{"type": "prompt", "name": "step", "template": "Summarize"}],
    }
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(
        name="sub",
        orchestration=str(child_path),
        # no explicit outputs — interface should auto-generate
    )

    adapter = _mock_adapter("The summary")
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    # Auto-mapped: summary → prime.step.value
    assert store.state["sub"]["value"] == {"summary": "The summary"}


def test_interface_explicit_outputs_override_auto(tmp_path: Path) -> None:
    """Explicit outputs on use effect take precedence over interface auto-mapping."""
    child_orch = {
        "interface": {
            "outputs": {
                "summary": {"type": "string", "path": "prime.step.value"},
            }
        },
        "effects": [{"type": "prompt", "name": "step", "template": "Summarize"}],
    }
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(
        name="sub",
        orchestration=str(child_path),
        outputs={"custom_key": "prime.step.value"},  # explicit — overrides interface
    )

    adapter = _mock_adapter("The summary")
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["sub"]["value"] == {"custom_key": "The summary"}


def test_interface_no_interface_works_normally(tmp_path: Path) -> None:
    """Orchestrations without interface work exactly as before."""
    child_orch = {
        "effects": [{"type": "prompt", "name": "step", "template": "Hello"}],
    }
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(name="sub", orchestration=str(child_path))

    adapter = _mock_adapter("Hi")
    store = Store(state={})
    runtime = UseRuntime(defn, adapter=adapter, model="test-model")
    runtime.execute(store=store, ctx=store.state)

    assert store.state["sub"]["step"]["value"] is not None


# ── Interface: default:, type coercion for a use: child's own inputs ───────


def test_interface_child_default_applied_when_input_omitted(tmp_path: Path) -> None:
    """A declared `default:` fills in a use: child input the caller omits."""
    child_orch = {
        "interface": {"inputs": {"max_words": {"type": "number", "default": 42}}},
        "effects": [
            {
                "type": "tool",
                "name": "echo",
                "provider": "json",
                "params": {"mode": "stringify", "input": "{{input.max_words}}"},
            }
        ],
    }
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(name="sub", orchestration=str(child_path), inputs={})
    store = Store(state={})
    UseRuntime(defn, adapter=_mock_adapter(), model="test-model").execute(
        store=store, ctx=store.state
    )

    assert store.state["sub"]["echo"]["value"] == '"42"'


def test_interface_child_rendered_string_coerced_to_declared_number(tmp_path: Path) -> None:
    """A use: input renders as text even when it's meant to be a number —
    the declared `type` still coerces it, same as a top-level run's `-e`."""
    child_orch = {
        "interface": {"inputs": {"max_words": {"type": "number"}}},
        "effects": [
            {
                "type": "tool",
                "name": "echo",
                "provider": "json",
                "params": {"mode": "stringify", "input": "{{input.max_words}}"},
            }
        ],
    }
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(name="sub", orchestration=str(child_path), inputs={"max_words": "7"})
    store = Store(state={})
    UseRuntime(defn, adapter=_mock_adapter(), model="test-model").execute(
        store=store, ctx=store.state
    )

    # A value truly coerced to int 7 stringifies to "7"; left as text "7" it
    # would render identically here, so this alone isn't conclusive — the
    # top-level `-e` test (test_cli_dash_e_string_coerced_to_declared_number)
    # checks the leading-zero case that does distinguish them. This test's
    # job is only to confirm the child path doesn't reject a declared-number
    # input that crosses as text.
    assert store.state["sub"]["echo"]["value"] == '"7"'


def test_interface_child_array_input_rendered_as_mustache_string_fails_conversion(
    tmp_path: Path,
) -> None:
    """A plain (non-`{from:}`) Mustache value for a declared `array` input
    renders via Python repr, not JSON, so the declared-type coercion fails
    with a clear error rather than silently passing the child a malformed
    string."""
    child_orch = {
        "interface": {"inputs": {"items": {"type": "array"}}},
        "effects": [
            {
                "type": "tool",
                "name": "echo",
                "provider": "json",
                "params": {"mode": "stringify", "input": "{{input.items}}"},
            }
        ],
    }
    child_path = _write_orch(tmp_path, "child.yml", child_orch)

    defn = UseDefinition(
        name="sub", orchestration=str(child_path), inputs={"items": "{{parent_list}}"}
    )
    store = Store(state={"parent_list": ["a", "b"]})

    with pytest.raises(RuntimeError, match=r"input 'items'.*could not be converted"):
        UseRuntime(defn, adapter=_mock_adapter(), model="test-model").execute(
            store=store, ctx=store.state
        )


# ── Schema validation ────────────────────────────────────────────────────────


def test_schema_validates_interface_declaration() -> None:
    """Orchestration with interface field passes schema validation."""
    import tempfile

    from circuitry.cli.runtime_shim import validate

    orch = {
        "interface": {
            "inputs": {
                "text": {"type": "string", "required": True},
            },
            "outputs": {
                "result": {"type": "string", "path": "prime.step.value"},
            },
        },
        "effects": [{"type": "prompt", "name": "step", "template": "Do: {{input.text}}"}],
    }

    with tempfile.NamedTemporaryFile(suffix=".yml", mode="w", delete=False) as f:
        yaml.dump(orch, f)
        path = Path(f.name)

    result = validate(path)
    assert result["ok"] is True
