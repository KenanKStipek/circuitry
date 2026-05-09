"""Tests for opt-in full-namespace mode in `use` effects.

When `outputs:` is not declared (and no `interface:` outputs), the child
orchestration's prime subtree is exposed at prime.<use_name>.<child_effect>.value
— matching `dynamic` namespacing.

When `outputs:` IS declared, behavior is unchanged: prime.<use_name>.value is
a flat dict of declared keys.
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import yaml

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


# ── AC 8: full-namespace mode ────────────────────────────────────────────────


def test_full_namespace_exposes_each_child_effect(tmp_path: Path) -> None:
    """Multiple child effects are each reachable at prime.<use>.<name>.value."""
    child = {
        "effects": [
            {"type": "prompt", "name": "first", "template": "f"},
            {"type": "prompt", "name": "second", "template": "s"},
            {"type": "prompt", "name": "third", "template": "t"},
        ]
    }
    child_path = _write(tmp_path, "child.yml", child)

    defn = UseDefinition(name="sub", path=str(child_path))
    adapter = _mock_adapter("response")
    store = Store(state={})
    UseRuntime(defn, adapter=adapter, model="test").execute(store=store, ctx=store.state)

    assert store.state["sub"]["first"]["value"] == "response"
    assert store.state["sub"]["second"]["value"] == "response"
    assert store.state["sub"]["third"]["value"] == "response"


def test_full_namespace_no_value_sentinel(tmp_path: Path) -> None:
    """The legacy value: True sentinel is gone in full-namespace mode."""
    child = {"effects": [{"type": "prompt", "name": "step", "template": "x"}]}
    child_path = _write(tmp_path, "child.yml", child)

    defn = UseDefinition(name="sub", path=str(child_path))
    adapter = _mock_adapter("x")
    store = Store(state={})
    UseRuntime(defn, adapter=adapter, model="test").execute(store=store, ctx=store.state)

    # value is None (initialized by setdefault) — not True.
    assert store.state["sub"]["value"] is None


# ── AC 9: declared-outputs mode preserves legacy behavior ───────────────────


def test_declared_outputs_returns_flat_dict(tmp_path: Path) -> None:
    """With outputs declared, prime.<use>.value is a flat dict of declared keys."""
    child = {
        "effects": [
            {"type": "prompt", "name": "step", "template": "x"},
            {"type": "prompt", "name": "other", "template": "y"},
        ]
    }
    child_path = _write(tmp_path, "child.yml", child)

    defn = UseDefinition(
        name="sub",
        path=str(child_path),
        outputs={"answer": "prime.step.value"},
    )
    adapter = _mock_adapter("answered")
    store = Store(state={})
    UseRuntime(defn, adapter=adapter, model="test").execute(store=store, ctx=store.state)

    # Flat-dict mode: only declared keys present in value.
    assert store.state["sub"]["value"] == {"answer": "answered"}
    # Not exposed at top of namespace (declared mode does not full-merge).
    assert "step" not in store.state["sub"]
    assert "other" not in store.state["sub"]


# ── Interface still wins ────────────────────────────────────────────────────


def test_interface_outputs_take_precedence_over_full_namespace(tmp_path: Path) -> None:
    """Interface-declared outputs auto-generate the mapping; full-namespace mode
    is NOT used even when the use effect has no explicit outputs."""
    child = {
        "interface": {
            "outputs": {
                "result": {"type": "string", "path": "prime.step.value"},
            }
        },
        "effects": [{"type": "prompt", "name": "step", "template": "x"}],
    }
    child_path = _write(tmp_path, "child.yml", child)

    defn = UseDefinition(name="sub", path=str(child_path))
    adapter = _mock_adapter("hello")
    store = Store(state={})
    UseRuntime(defn, adapter=adapter, model="test").execute(store=store, ctx=store.state)

    # Interface auto-mapping wins: flat dict, not full namespace.
    assert store.state["sub"]["value"] == {"result": "hello"}
    assert "step" not in store.state["sub"]
