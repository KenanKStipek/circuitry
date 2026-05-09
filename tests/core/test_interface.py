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
        "effects": [{"type": "prompt", "name": "step", "template": "Summarize: {{article_text}}"}],
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

    with pytest.raises(RuntimeError, match="missing required input.*article_text"):
        runtime.execute(store=store, ctx=store.state)


def test_interface_passes_when_required_inputs_provided(tmp_path: Path) -> None:
    """Required inputs present → no error."""
    child_orch = {
        "interface": {
            "inputs": {
                "article_text": {"type": "string", "required": True},
            }
        },
        "effects": [{"type": "prompt", "name": "step", "template": "Summarize: {{article_text}}"}],
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
        "effects": [{"type": "prompt", "name": "step", "template": "Do: {{text}}"}],
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


# ── Schema validation ────────────────────────────────────────────────────────


def test_schema_validates_interface_declaration() -> None:
    """Orchestration with interface field passes schema validation."""
    from circuitry.cli.runtime_shim import validate
    import tempfile

    orch = {
        "interface": {
            "inputs": {
                "text": {"type": "string", "required": True},
            },
            "outputs": {
                "result": {"type": "string", "path": "prime.step.value"},
            },
        },
        "effects": [{"type": "prompt", "name": "step", "template": "Do: {{text}}"}],
    }

    with tempfile.NamedTemporaryFile(suffix=".yml", mode="w", delete=False) as f:
        yaml.dump(orch, f)
        path = Path(f.name)

    result = validate(path)
    assert result["ok"] is True
