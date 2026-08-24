"""Grammar matrix for the input./prime./runtime. state-namespace contract (#86).

Two layers:

* Direct unit tests against ``core.state_ns`` — the hard-error validators
  and the lift-on-hydrate helper in isolation.
* Compile/runtime tests through ``compile_orchestration`` — the same
  validators wired into the real compile path, plus one case
  (``input.items``) proving the *legal* spelling actually resolves at
  runtime, not just that illegal ones raise.
"""

from __future__ import annotations

from dataclasses import dataclass

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.state_ns import (
    NAMESPACES,
    declared_input_names,
    migrate_legacy_state,
    validate_bare_input_refs,
    validate_cel_expr,
    validate_each_in_path,
)
from circuitry.core.store import Store


@dataclass(frozen=True)
class EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"model": model, "prompt": prompt})


# ── migrate_legacy_state ─────────────────────────────────────────────────


def test_migrate_legacy_state_lifts_bare_root_keys() -> None:
    state = {"topic": "widgets", "count": 3}
    result = migrate_legacy_state(state)
    assert result["input"] == {"topic": "widgets", "count": 3}
    assert "topic" not in result
    assert "count" not in result


def test_migrate_legacy_state_is_idempotent_once_input_is_present() -> None:
    """Lift-on-hydrate is a one-time move: an already-namespaced state short
    circuits, so a second pass through the same choke point is a no-op."""
    state = {"input": {"topic": "widgets"}, "extra": "stray"}
    result = migrate_legacy_state(state)
    assert result["input"] == {"topic": "widgets"}
    assert result["extra"] == "stray"


def test_migrate_legacy_state_leaves_namespaces_and_builtins_at_root() -> None:
    state = {
        "prime": {"x": 1},
        "runtime": {"y": 2},
        "_run_id": "abc",
        "topic": "widgets",
    }
    result = migrate_legacy_state(state)
    assert result["prime"] == {"x": 1}
    assert result["runtime"] == {"y": 2}
    assert result["_run_id"] == "abc"
    assert result["input"] == {"topic": "widgets"}


def test_migrate_legacy_state_on_empty_state_yields_empty_input() -> None:
    assert migrate_legacy_state({}) == {"input": {}}


# ── validate_each_in_path ────────────────────────────────────────────────


def test_validate_each_in_path_accepts_every_namespace_root() -> None:
    for root in NAMESPACES:
        validate_each_in_path(f"{root}.items", effect_path="prime.loop")


def test_validate_each_in_path_rejects_bare_key() -> None:
    with pytest.raises(ValueError, match="not rooted at a state namespace"):
        validate_each_in_path("items", effect_path="prime.loop")


def test_validate_each_in_path_rejects_empty_path() -> None:
    with pytest.raises(ValueError, match="must be a dot path rooted"):
        validate_each_in_path("", effect_path="prime.loop")


def test_validate_each_in_path_rejects_state_dot_namespace_as_cel_only() -> None:
    with pytest.raises(ValueError, match="CEL-only binding"):
        validate_each_in_path("state.input.items", effect_path="prime.loop")


def test_validate_each_in_path_rejects_state_dot_non_namespace_key() -> None:
    with pytest.raises(ValueError, match="not rooted at a state namespace"):
        validate_each_in_path("state.items", effect_path="prime.loop")


# ── validate_cel_expr ────────────────────────────────────────────────────


def test_validate_cel_expr_accepts_every_namespace_binding() -> None:
    validate_cel_expr("state.input.ok == true", effect_path="prime.gate")
    validate_cel_expr("state.prime.step.value > 0", effect_path="prime.gate")
    validate_cel_expr("state.runtime.last_run.run_id != ''", effect_path="prime.gate")


def test_validate_cel_expr_rejects_state_dot_unknown_key() -> None:
    with pytest.raises(ValueError, match="does not name a state namespace"):
        validate_cel_expr("state.topic == 'x'", effect_path="prime.gate")


def test_validate_cel_expr_ignores_identifiers_that_are_not_state_dot() -> None:
    # `item.name` is a within-loop binding, not a `state.` reference — no error.
    validate_cel_expr("item.name == 'x' && 1 == 1", effect_path="prime.gate")


# ── declared_input_names / validate_bare_input_refs ──────────────────────


def test_declared_input_names_reads_interface_inputs() -> None:
    orch = {
        "interface": {
            "inputs": {"topic": {"type": "string"}, "count": {"type": "number"}}
        }
    }
    assert declared_input_names(orch) == frozenset({"topic", "count"})


def test_declared_input_names_empty_without_an_interface() -> None:
    assert declared_input_names({}) == frozenset()


def test_validate_bare_input_refs_rejects_bare_ref_to_declared_input() -> None:
    orch = {
        "interface": {"inputs": {"topic": {"type": "string"}}},
        "effects": [{"type": "prompt", "name": "step", "template": "about {{topic}}"}],
    }
    with pytest.raises(ValueError, match="declared interface input 'topic'"):
        validate_bare_input_refs(orch)


def test_validate_bare_input_refs_accepts_namespaced_ref() -> None:
    orch = {
        "interface": {"inputs": {"topic": {"type": "string"}}},
        "effects": [
            {"type": "prompt", "name": "step", "template": "about {{input.topic}}"}
        ],
    }
    validate_bare_input_refs(orch)


def test_validate_bare_input_refs_leaves_undeclared_bare_keys_legal() -> None:
    """Loop `as` vars, `_loop_index`, and sibling shorthand aren't policed —
    only bare refs matching this document's own declared inputs are."""
    orch = {
        "interface": {"inputs": {"topic": {"type": "string"}}},
        "effects": [
            {
                "type": "loop",
                "name": "each_item",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "prompt",
                        "name": "step",
                        "template": "{{item}} #{{_loop_index}} {{input.topic}}",
                    }
                ],
            }
        ],
    }
    validate_bare_input_refs(orch)


# ── Compile-time grammar matrix (wired through compile_orchestration) ────


def test_compile_orchestration_input_items_resolves_and_iterates() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "each_item",
                "each": {"in": "input.items", "as": "item"},
                "body": [{"type": "prompt", "name": "step", "template": "{{item}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": ["a", "b"]}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    loop_value = store.get("prime.each_item.value")
    assert loop_value["termination"]["reason"] == "collection_exhausted"
    assert loop_value["iterations"] == 2


def test_compile_orchestration_rejects_bare_each_in() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "x",
                "each": {"in": "items", "as": "item"},
                "body": [{"type": "prompt", "name": "s", "template": "{{item}}"}],
            }
        ]
    }
    with pytest.raises(ValueError, match="not rooted at a state namespace"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_orchestration_rejects_state_prefixed_each_in() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "x",
                "each": {"in": "state.input.items", "as": "item"},
                "body": [{"type": "prompt", "name": "s", "template": "{{item}}"}],
            }
        ]
    }
    with pytest.raises(ValueError, match="CEL-only binding"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_orchestration_cel_state_input_resolves() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "name": "gate",
                "if": {"mode": "cel", "expr": "state.input.ok == true"},
                "then": [{"type": "prompt", "name": "yes", "template": "y"}],
                "else": [{"type": "prompt", "name": "no", "template": "n"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"ok": True}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    assert store.get("prime.gate.value")["branch"] == "then"


def test_compile_orchestration_rejects_cel_state_dot_unknown_key() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "name": "gate",
                "if": {"mode": "cel", "expr": "state.ok == true"},
                "then": [{"type": "prompt", "name": "yes", "template": "y"}],
            }
        ]
    }
    with pytest.raises(ValueError, match="does not name a state namespace"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_orchestration_rejects_bare_ref_to_declared_input() -> None:
    orch = {
        "interface": {"inputs": {"topic": {"type": "string"}}},
        "effects": [{"type": "prompt", "name": "step", "template": "about {{topic}}"}],
    }
    with pytest.raises(ValueError, match="declared interface input 'topic'"):
        compile_orchestration(orch=orch, root_name="prime")
