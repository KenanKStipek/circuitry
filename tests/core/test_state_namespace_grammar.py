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


def test_validate_cel_expr_rejects_loop_binding_without_extra_names() -> None:
    """`state.item` is only legal where a loop declared it — the default
    empty allow-list rejects it just like any other unknown key."""
    with pytest.raises(ValueError, match="does not name a state namespace"):
        validate_cel_expr("state.item == 'x'", effect_path="prime.gate")


def test_validate_cel_expr_accepts_names_in_extra_names() -> None:
    validate_cel_expr(
        "state.item == 'x' && state.iter.index > 0",
        effect_path="prime.gate",
        extra_names=frozenset({"item", "iter"}),
    )


def test_validate_cel_expr_rejects_name_outside_the_extra_names_allow_list() -> None:
    """A name declared by a sibling/unrelated loop is still undeclared here."""
    with pytest.raises(ValueError, match="does not name a state namespace"):
        validate_cel_expr(
            "state.other == 'x'", effect_path="prime.gate", extra_names=frozenset({"item"})
        )


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


def test_compile_orchestration_cel_sees_the_enclosing_loops_each_binding() -> None:
    """A CEL condition inside a loop body may reference the loop's own
    ``each.as`` name and ``iter.index`` directly — the acceptance case for
    issue #186."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "each_item",
                "collect": "keep",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "if",
                        # Unnamed: then/else effects merge into the iteration's
                        # own scope, so `collect: keep` can find them directly.
                        "if": {
                            "mode": "cel",
                            "expr": "state.item > 1 && state.iter.index >= 0",
                        },
                        "then": [{"type": "prompt", "name": "keep", "template": "{{item}}"}],
                        "else": [{"type": "prompt", "name": "keep", "template": "skip"}],
                    }
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": [1, 2, 3]}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    assert store.get("prime.each_item.collected.value") == ["skip", "2", "3"]


def test_compile_orchestration_rejects_cel_loop_binding_outside_a_loop() -> None:
    """``state.item`` is only legal where a loop declared ``item`` — outside
    any loop it is just another undeclared key."""
    orch = {
        "effects": [
            {
                "type": "if",
                "name": "gate",
                "if": {"mode": "cel", "expr": "state.item == 'x'"},
                "then": [{"type": "prompt", "name": "yes", "template": "y"}],
            }
        ]
    }
    with pytest.raises(ValueError, match="does not name a state namespace"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_orchestration_rejects_undeclared_name_inside_a_loop_body() -> None:
    """Being inside a loop only allow-lists *that* loop's own bindings — an
    unrelated name is still a hard error, with a message naming what is in
    scope."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "each_item",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "if",
                        "name": "gate",
                        "if": {"mode": "cel", "expr": "state.other == 'x'"},
                        "then": [{"type": "prompt", "name": "yes", "template": "y"}],
                    }
                ],
            }
        ]
    }
    with pytest.raises(ValueError, match="does not name a state namespace") as exc_info:
        compile_orchestration(orch=orch, root_name="prime")
    assert "Names bound by an enclosing loop here" in str(exc_info.value)
    assert "item" in str(exc_info.value)


def test_compile_orchestration_nested_loop_cel_binding_shadows_the_outer() -> None:
    """An inner loop's ``as`` name of the same spelling as the outer loop's
    shadows it for CEL inside the inner body, and reverts once back in the
    outer body — the same rule Mustache already follows for templates.

    Outer binds ``item`` to a string, inner binds it (same name) to a number;
    a CEL condition inside the inner body reads the inner (numeric) binding,
    and a CEL condition back in the outer body, after the inner loop returns,
    reads the outer (string) binding again.
    """
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "collect": "after_inner",
                "each": {"in": "input.outer_items", "as": "item"},
                "body": [
                    {
                        "type": "loop",
                        "name": "inner",
                        "collect": "classify",
                        "each": {"in": "input.inner_items", "as": "item"},
                        "body": [
                            {
                                "type": "if",
                                # Inner `item` (a number) shadows outer `item` here.
                                "if": {"mode": "cel", "expr": "state.item > 1"},
                                "then": [
                                    {"type": "prompt", "name": "classify", "template": "big"}
                                ],
                                "else": [
                                    {"type": "prompt", "name": "classify", "template": "small"}
                                ],
                            }
                        ],
                    },
                    {
                        "type": "if",
                        # Back in the outer body, `item` means the outer (string) binding again.
                        "if": {"mode": "cel", "expr": "state.item == 'A'"},
                        "then": [
                            {"type": "prompt", "name": "after_inner", "template": "A-match"}
                        ],
                        "else": [
                            {"type": "prompt", "name": "after_inner", "template": "no-match"}
                        ],
                    },
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"outer_items": ["A"], "inner_items": [1, 2, 3]}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    assert store.get("prime.outer.iter_0.inner.collected.value") == [
        "small",
        "big",
        "big",
    ]
    assert store.get("prime.outer.collected.value") == ["A-match"]


def test_compile_orchestration_rejects_bare_ref_to_declared_input() -> None:
    orch = {
        "interface": {"inputs": {"topic": {"type": "string"}}},
        "effects": [{"type": "prompt", "name": "step", "template": "about {{topic}}"}],
    }
    with pytest.raises(ValueError, match="declared interface input 'topic'"):
        compile_orchestration(orch=orch, root_name="prime")
