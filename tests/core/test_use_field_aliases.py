"""Tests for `use` reference fields: ref / path / orchestration (deprecated) / inline."""

from __future__ import annotations

import warnings
from pathlib import Path
from unittest.mock import MagicMock

import pytest
import yaml

from circuitry.core.compiler import compile_orchestration
from circuitry.core.store import Store
from circuitry.core.use import UseDefinition, UseRuntime


def _write(tmp_path: Path, name: str, content: dict) -> Path:
    path = tmp_path / name
    path.write_text(yaml.dump(content), encoding="utf-8")
    return path


def _mock_adapter(response: str = "ok") -> MagicMock:
    adapter = MagicMock()
    adapter.name = "mock"
    result = MagicMock()
    result.text = response
    result.raw = {}
    result.tokens_sent = 0
    result.tokens_received = 0
    adapter.generate.return_value = result
    return adapter


# ── AC 1: ref ─────────────────────────────────────────────────────────────────


def test_ref_resolves_curation_entry() -> None:
    """`ref:` looks up an entry by slash-delimited curation name."""
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "sub",
                "ref": "learn/hello",
                "inputs": {"name": "world"},
            }
        ]
    }
    root = compile_orchestration(orch=orch)
    effect = root.effects[0]
    assert effect.ref == "learn/hello"
    assert effect.path is None
    assert effect.orchestration is None
    assert effect.inline is None


def test_ref_runs_end_to_end() -> None:
    """A use with ref: actually executes the resolved orchestration."""
    defn = UseDefinition(name="sub", ref="learn/hello", inputs={"name": "world"})
    adapter = _mock_adapter("Hi world")
    store = Store(state={})
    UseRuntime(defn, adapter=adapter, model="test").execute(store=store, ctx=store.state)
    # learn/hello has a single effect named 'greet'.
    assert store.state["sub"]["greet"]["value"] == "Hi world"


def test_ref_unresolvable_raises() -> None:
    """`ref:` to a missing entry raises a clear error at runtime."""
    defn = UseDefinition(name="sub", ref="no/such/entry")
    adapter = _mock_adapter()
    store = Store(state={})
    with pytest.raises(RuntimeError, match="ref 'no/such/entry' did not resolve"):
        UseRuntime(defn, adapter=adapter, model="test").execute(store=store, ctx=store.state)


# ── AC 2: path ───────────────────────────────────────────────────────────────


def test_path_absolute(tmp_path: Path) -> None:
    """`path:` accepts an absolute filesystem path."""
    child = {"effects": [{"type": "prompt", "name": "step", "template": "x"}]}
    p = _write(tmp_path, "child.yml", child)
    orch = {"effects": [{"type": "use", "name": "sub", "path": str(p)}]}
    root = compile_orchestration(orch=orch)
    assert root.effects[0].path == str(p)


def test_path_runs_end_to_end(tmp_path: Path) -> None:
    child = {"effects": [{"type": "prompt", "name": "step", "template": "x"}]}
    p = _write(tmp_path, "child.yml", child)
    defn = UseDefinition(name="sub", path=str(p))
    adapter = _mock_adapter("done")
    store = Store(state={})
    UseRuntime(defn, adapter=adapter, model="test").execute(store=store, ctx=store.state)
    assert store.state["sub"]["step"]["value"] == "done"


def test_path_unresolvable_raises(tmp_path: Path) -> None:
    """An unresolvable path raises a clear runtime error."""
    bogus = tmp_path / "does_not_exist.yml"
    defn = UseDefinition(name="sub", path=str(bogus))
    adapter = _mock_adapter()
    store = Store(state={})
    with pytest.raises(RuntimeError, match="not found"):
        UseRuntime(defn, adapter=adapter, model="test").execute(store=store, ctx=store.state)


# ── AC 3: deprecated orchestration field ─────────────────────────────────────


def test_orchestration_field_emits_deprecation_warning() -> None:
    """Compiling a use effect with `orchestration:` emits a DeprecationWarning."""
    orch = {"effects": [{"type": "use", "name": "sub", "orchestration": "learn/hello"}]}

    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        compile_orchestration(orch=orch)

    deprecations = [w for w in caught if issubclass(w.category, DeprecationWarning)]
    assert deprecations, "Expected a DeprecationWarning for `orchestration:` field"
    msg = str(deprecations[0].message)
    assert "deprecated" in msg.lower()
    assert "sub" in msg
    assert "ref" in msg or "path" in msg


def test_orchestration_field_still_runs() -> None:
    """The legacy field still resolves and runs end-to-end."""
    defn = UseDefinition(name="sub", orchestration="learn/hello", inputs={"name": "x"})
    adapter = _mock_adapter("Hi x")
    store = Store(state={})
    UseRuntime(defn, adapter=adapter, model="test").execute(store=store, ctx=store.state)
    assert store.state["sub"]["greet"]["value"] == "Hi x"


# ── AC 4: mutual exclusion ───────────────────────────────────────────────────


def test_two_reference_fields_errors_with_field_names() -> None:
    """Setting two of {ref, path, orchestration, inline} fails with a structured error."""
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "sub",
                "ref": "learn/hello",
                "path": "/tmp/foo.yml",
            }
        ]
    }
    with pytest.raises(ValueError) as exc_info:
        compile_orchestration(orch=orch)
    msg = str(exc_info.value)
    assert "multiple reference fields" in msg
    assert "ref" in msg
    assert "path" in msg
    assert "sub" in msg


def test_three_reference_fields_errors_listing_all() -> None:
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "sub",
                "ref": "x",
                "path": "/tmp/y.yml",
                "inline": "effects: []",
            }
        ]
    }
    with pytest.raises(ValueError) as exc_info:
        compile_orchestration(orch=orch)
    msg = str(exc_info.value)
    for field in ("ref", "path", "inline"):
        assert field in msg


def test_no_reference_fields_errors() -> None:
    """A use effect with none of ref/path/orchestration/inline fails compile."""
    orch = {"effects": [{"type": "use", "name": "sub"}]}
    with pytest.raises(ValueError, match="requires exactly one"):
        compile_orchestration(orch=orch)


# ── inline still works ───────────────────────────────────────────────────────


def test_inline_still_compiles_and_runs() -> None:
    """`inline:` (which was already the spec-canonical alternative) still works."""
    inline_yaml = yaml.dump({
        "effects": [{"type": "prompt", "name": "step", "template": "inline"}]
    })
    defn = UseDefinition(name="sub", inline=inline_yaml)
    adapter = _mock_adapter("inline-output")
    store = Store(state={})
    UseRuntime(defn, adapter=adapter, model="test").execute(store=store, ctx=store.state)
    assert store.state["sub"]["step"]["value"] == "inline-output"
