"""`use` inputs by reference (`{from: <path>}`) and the opt-in complete child record (#218)."""

from __future__ import annotations

import hashlib
from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest
import yaml

from circuitry.api import run_orchestration
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store
from circuitry.core.use import _resolve_reference, reference_path

# ── Helpers ──────────────────────────────────────────────────────────────────


def _write(tmp_path: Path, name: str, content: dict) -> Path:
    path = tmp_path / name
    path.write_text(yaml.dump(content), encoding="utf-8")
    return path


def _run(
    orch: dict,
    *,
    inputs: dict[str, Any] | None = None,
    runtime_config: dict[str, Any] | None = None,
) -> dict[str, Any]:
    root = compile_orchestration(orch=orch)
    store = Store(state={"input": dict(inputs or {})})
    # Non-empty on purpose: `Runtime.__init__` swaps an empty dict for a fresh
    # one, and a real run's runtime_config is never empty.
    DynamicRuntime(
        root,
        adapter=MagicMock(),
        model="test",
        runtime_config={"_test_marker": True, **(runtime_config or {})},
    ).execute(store=store)
    return store.state


def _json(name: str, text: str) -> dict[str, Any]:
    """A tool effect that parses *text* as JSON: a value source with no model call."""
    return {"type": "tool", "name": name, "provider": "json", "params": {"mode": "parse", "input": text}}


#: A child that hands back, typed, whatever it received as `got`.
ECHO = {
    "interface": {
        "inputs": {"got": {"required": False}},
        "outputs": {"got": {"path": "input.got"}},
    },
    "effects": [_json("noop", "true")],
}

RECORD = {"state": {"record_children": True}}


# ── reference_path / _resolve_reference ──────────────────────────────────────


def test_reference_path_accepts_only_the_single_from_key() -> None:
    assert reference_path({"from": "prime.x.value"}) == "prime.x.value"
    assert reference_path({"from": "  input.items  "}) == "input.items"
    assert reference_path({"from": "a", "other": 1}) is None
    assert reference_path({"from": 3}) is None
    assert reference_path("{{input.items}}") is None
    assert reference_path(["from"]) is None


def test_resolve_reference_walks_mappings_and_list_indices() -> None:
    ctx = {"a": {"b": [10, {"c": 5}]}}
    assert _resolve_reference(ctx, "a.b.1.c") == 5
    assert _resolve_reference(ctx, "a.b.0") == 10
    assert _resolve_reference(ctx, "a.b.2") is None
    assert _resolve_reference(ctx, "a.b.x") is None
    assert _resolve_reference(ctx, "a.missing") is None
    assert _resolve_reference(ctx, "a.b.0.deeper") is None


# ── A: inputs by reference ───────────────────────────────────────────────────


@pytest.mark.parametrize(
    "value",
    [[1, 2, 3], {"k": [1, {"n": True}]}, 7, 2.5, True, "text", ["nnedi", "nnedi_crisp"]],
)
def test_from_passes_structured_values_unchanged(tmp_path: Path, value: Any) -> None:
    import json

    child = _write(tmp_path, "echo.yml", ECHO)
    orch = {
        "effects": [
            _json("src", json.dumps(value)),
            {"type": "use", "name": "sub", "path": str(child), "inputs": {"got": {"from": "prime.src.value"}}},
        ]
    }
    state = _run(orch)
    got = state["prime"]["sub"]["value"]["got"]
    assert got == value
    assert type(got) is type(value)


def test_from_reads_input_namespace_and_list_indices(tmp_path: Path) -> None:
    child = _write(tmp_path, "echo.yml", ECHO)
    orch = {
        "effects": [
            {"type": "use", "name": "whole", "path": str(child), "inputs": {"got": {"from": "input.rungs"}}},
            {"type": "use", "name": "second", "path": str(child), "inputs": {"got": {"from": "input.rungs.1.name"}}},
        ]
    }
    rungs = [{"name": "4k", "factor": 2}, {"name": "1080p", "factor": 4}]
    state = _run(orch, inputs={"rungs": rungs})
    assert state["prime"]["whole"]["value"]["got"] == rungs
    assert state["prime"]["second"]["value"]["got"] == "1080p"


@pytest.mark.parametrize("flow", ["chain", "tree"])
def test_from_accepts_loop_bindings(tmp_path: Path, flow: str) -> None:
    child = _write(tmp_path, "echo.yml", ECHO)
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "ladder",
                "flow": flow,
                "max_concurrency": 4,
                "each": {"in": "input.rungs", "as": "r"},
                "body": [
                    {"type": "use", "name": "whole", "path": str(child), "inputs": {"got": {"from": "r"}}},
                    {"type": "use", "name": "field", "path": str(child), "inputs": {"got": {"from": "r.factor"}}},
                ],
            }
        ]
    }
    rungs = [{"name": "4k", "factor": 2}, {"name": "1080p", "factor": 4}, {"name": "480p", "factor": 9}]
    state = _run(orch, inputs={"rungs": rungs})
    for i, rung in enumerate(rungs):
        node = state["prime"]["ladder"][f"iter_{i}"]
        assert node["whole"]["value"]["got"] == rung
        assert node["field"]["value"]["got"] == rung["factor"]


def test_from_never_aliases_parent_state(tmp_path: Path) -> None:
    child = _write(tmp_path, "echo.yml", ECHO)
    orch = {
        "effects": [
            _json("src", '{"items": [1, 2]}'),
            {"type": "use", "name": "sub", "path": str(child), "inputs": {"got": {"from": "prime.src.value"}}},
        ]
    }
    state = _run(orch)
    parent_value = state["prime"]["src"]["value"]
    assert state["prime"]["sub"]["value"]["got"] == parent_value
    assert state["prime"]["sub"]["value"]["got"] is not parent_value
    assert state["prime"]["sub"]["meta"]["inputs"]["got"] is not parent_value


def test_from_unresolved_optional_input_passes_null(tmp_path: Path) -> None:
    child = _write(tmp_path, "echo.yml", ECHO)
    orch = {
        "effects": [
            {"type": "use", "name": "sub", "path": str(child), "inputs": {"got": {"from": "prime.missing.value"}}},
        ]
    }
    state = _run(orch)
    assert state["prime"]["sub"]["value"]["got"] is None
    assert state["prime"]["sub"]["meta"]["error"] is None


def test_from_unresolved_required_input_fails_clearly(tmp_path: Path) -> None:
    required = {**ECHO, "interface": {**ECHO["interface"], "inputs": {"got": {"required": True}}}}
    child = _write(tmp_path, "echo.yml", required)
    orch = {
        "effects": [
            {"type": "use", "name": "sub", "path": str(child), "inputs": {"got": {"from": "prime.missing.value"}}},
        ]
    }
    with pytest.raises(Exception, match=r"required input 'got' resolved to nothing from 'prime.missing.value'"):
        _run(orch)


@pytest.mark.parametrize(
    ("path", "message"),
    [
        ("items", "not rooted at a state namespace or an enclosing loop binding"),
        ("state.prime.src.value", "Write 'prime.src.value' instead of 'state.prime.src.value'"),
        ("r.name", "loop bindings in scope: none here"),
        ("", "needs a non-empty path"),
    ],
)
def test_from_rejects_bad_roots_at_compile_time(tmp_path: Path, path: str, message: str) -> None:
    child = _write(tmp_path, "echo.yml", ECHO)
    orch = {"effects": [{"type": "use", "name": "sub", "path": str(child), "inputs": {"got": {"from": path}}}]}
    with pytest.raises(ValueError, match=message):
        compile_orchestration(orch=orch)


def test_from_rejects_a_binding_outside_its_loop(tmp_path: Path) -> None:
    child = _write(tmp_path, "echo.yml", ECHO)
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "ladder",
                "each": {"in": "input.rungs", "as": "r"},
                "body": [_json("noop", "true")],
            },
            {"type": "use", "name": "after", "path": str(child), "inputs": {"got": {"from": "r"}}},
        ]
    }
    with pytest.raises(ValueError, match="loop bindings in scope: none here"):
        compile_orchestration(orch=orch)


def test_from_inputs_leave_a_library_ref_intact() -> None:
    """Validating by-reference inputs must not disturb the use effect's own `ref:`."""
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "sub",
                "ref": "utilities/critique",
                "inputs": {"content": {"from": "input.text"}, "criteria": "clarity"},
            }
        ]
    }
    root = compile_orchestration(orch=orch)
    use = root.effects[0]
    assert use.ref == "utilities/critique"
    assert use.inputs == {"content": {"from": "input.text"}, "criteria": "clarity"}


def test_string_and_literal_inputs_are_unchanged(tmp_path: Path) -> None:
    child = _write(tmp_path, "echo.yml", ECHO)
    orch = {
        "effects": [
            {"type": "use", "name": "templated", "path": str(child), "inputs": {"got": "{{input.n}}"}},
            {"type": "use", "name": "literal", "path": str(child), "inputs": {"got": {"a": 1, "b": [2]}}},
            {"type": "use", "name": "lookalike", "path": str(child), "inputs": {"got": {"from": "x", "also": 1}}},
        ]
    }
    state = _run(orch, inputs={"n": 5})
    assert state["prime"]["templated"]["value"]["got"] == "5"
    assert state["prime"]["literal"]["value"]["got"] == {"a": 1, "b": [2]}
    assert state["prime"]["lookalike"]["value"]["got"] == {"from": "x", "also": 1}


# ── Always-on meta: inputs and the child's YAML digest ───────────────────────


def test_meta_records_inputs_and_orchestration_sha256(tmp_path: Path) -> None:
    child = _write(tmp_path, "echo.yml", ECHO)
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "sub",
                "path": str(child),
                "inputs": {"got": {"from": "input.rungs"}, "label": "rung {{input.n}}"},
            }
        ]
    }
    state = _run(orch, inputs={"rungs": [1, 2], "n": 3})
    meta = state["prime"]["sub"]["meta"]
    assert meta["inputs"] == {"got": [1, 2], "label": "rung 3"}
    assert meta["orchestration_sha256"] == hashlib.sha256(child.read_bytes()).hexdigest()


def test_meta_digest_for_inline_children_is_of_the_rendered_yaml() -> None:
    orch = {
        "effects": [
            {"type": "use", "name": "sub", "inline": yaml.dump({"effects": [_json("noop", "true")]})},
        ]
    }
    state = _run(orch)
    digest = state["prime"]["sub"]["meta"]["orchestration_sha256"]
    assert isinstance(digest, str) and len(digest) == 64


# ── B: the complete record (opt-in) ──────────────────────────────────────────


def _child_with_steps(tmp_path: Path) -> Path:
    return _write(
        tmp_path,
        "steps.yml",
        {
            "interface": {"outputs": {"total": {"path": "prime.total.value"}}},
            "effects": [_json("first", "[1, 2]"), _json("total", "3")],
        },
    )


def test_record_is_off_by_default(tmp_path: Path) -> None:
    child = _child_with_steps(tmp_path)
    state = _run({"effects": [{"type": "use", "name": "sub", "path": str(child)}]})
    assert set(state["prime"]["sub"]) == {"value", "meta"}
    assert state["prime"]["sub"]["value"] == {"total": 3}


def test_record_children_keeps_every_child_effect(tmp_path: Path) -> None:
    child = _child_with_steps(tmp_path)
    state = _run({"effects": [{"type": "use", "name": "sub", "path": str(child)}]}, runtime_config=RECORD)
    node = state["prime"]["sub"]
    assert node["value"] == {"total": 3}
    assert node["first"]["value"] == [1, 2]
    assert node["first"]["meta"]["params_rendered"] == {"mode": "parse", "input": "[1, 2]"}
    assert node["total"]["value"] == 3


def test_record_children_composes_through_nested_uses(tmp_path: Path) -> None:
    leaf = _child_with_steps(tmp_path)
    mid = _write(
        tmp_path,
        "mid.yml",
        {
            "interface": {"outputs": {"total": {"path": "prime.leaf_call.value.total"}}},
            "effects": [{"type": "use", "name": "leaf_call", "path": str(leaf)}],
        },
    )
    state = _run({"effects": [{"type": "use", "name": "mid_call", "path": str(mid)}]}, runtime_config=RECORD)
    mid_node = state["prime"]["mid_call"]
    assert mid_node["value"] == {"total": 3}
    leaf_node = mid_node["leaf_call"]
    assert leaf_node["value"] == {"total": 3}
    assert leaf_node["first"]["meta"]["params_rendered"]["input"] == "[1, 2]"
    assert leaf_node["meta"]["orchestration_sha256"] == hashlib.sha256(leaf.read_bytes()).hexdigest()


def test_record_children_under_a_tree_loop(tmp_path: Path) -> None:
    child = _write(
        tmp_path,
        "square.yml",
        {
            "interface": {"inputs": {"n": {"required": True}}, "outputs": {"n": {"path": "input.n"}}},
            "effects": [_json("seen", "{{input.n}}")],
        },
    )
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "each_n",
                "flow": "tree",
                "max_concurrency": 4,
                "each": {"in": "input.ns", "as": "n"},
                "body": [{"type": "use", "name": "call", "path": str(child), "inputs": {"n": {"from": "n"}}}],
            }
        ]
    }
    state = _run(orch, inputs={"ns": [1, 2, 3, 4]}, runtime_config=RECORD)
    for i, n in enumerate([1, 2, 3, 4]):
        call = state["prime"]["each_n"][f"iter_{i}"]["call"]
        assert call["value"] == {"n": n}
        assert call["seen"]["value"] == n


def test_record_children_keeps_a_failed_childs_partial_record(tmp_path: Path) -> None:
    child = _write(
        tmp_path,
        "breaks.yml",
        {
            "interface": {"outputs": {"never": {"path": "prime.second.value"}}},
            "effects": [_json("first", "[1, 2]"), _json("second", "not json")],
        },
    )
    orch = {"effects": [{"type": "use", "name": "sub", "path": str(child), "on_error": "continue"}]}
    state = _run(orch, runtime_config=RECORD)
    node = state["prime"]["sub"]
    assert node["meta"]["error"]
    assert node["first"]["value"] == [1, 2]
    assert node["second"]["meta"]["error"]


def test_record_children_from_the_orchestrations_own_runtime_block(tmp_path: Path) -> None:
    child = _child_with_steps(tmp_path)
    parent = _write(
        tmp_path,
        "parent.yml",
        {
            "runtime": {"state": {"record_children": True}},
            "effects": [{"type": "use", "name": "sub", "path": "steps.yml"}],
        },
    )
    assert child.parent == parent.parent  # resolved relative to the parent file (#217)
    result = run_orchestration(orchestration_path=parent, state={}, adapter=MagicMock())
    node = result.state["prime"]["sub"]
    assert node["value"] == {"total": 3}
    assert node["first"]["value"] == [1, 2]
