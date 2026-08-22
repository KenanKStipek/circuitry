"""Loop iteration context must survive every container nested in the body.

A loop binds its iteration variable into the context it hands to body effects.
Container effects (dynamic, if, nested loop) re-derive the context they pass to
their own children, so each one has to carry the binding forward — otherwise a
structurally valid orchestration renders ``{{item.*}}`` empty and the run
"succeeds" with garbage prompts.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class RecordingAdapter:
    """Echoes the rendered prompt back and records everything it received."""

    name: str = "recording"
    prompts: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)
        return GenerateResult(text=prompt, raw={"model": model, "prompt": prompt})


def _run(orch: dict, state: dict) -> RecordingAdapter:
    root = compile_orchestration(orch=orch, root_name="prime")
    adapter = RecordingAdapter()
    DynamicRuntime(root, adapter=adapter, model="unit-test").execute(
        store=Store(state)
    )
    return adapter


def test_loop_var_reaches_prompts_through_every_nested_container() -> None:
    """Regression for #85: direct, if-nested and dynamic-nested all see the item."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "each": {"in": "items", "as": "item"},
                "body": [
                    {
                        "type": "prompt",
                        "name": "direct",
                        "template": "DIRECT sees: {{item.name}}",
                    },
                    {
                        "type": "if",
                        "name": "branch",
                        "if": {"mode": "cel", "expr": "1 == 1"},
                        "then": [
                            {
                                "type": "prompt",
                                "name": "nested",
                                "template": "NESTED-IF sees: {{item.name}}",
                            }
                        ],
                    },
                    {
                        "type": "dynamic",
                        "name": "wrap",
                        "flow": "chain",
                        "effects": [
                            {
                                "type": "prompt",
                                "name": "nested_dyn",
                                "template": "NESTED-DYN sees: {{item.name}}",
                            }
                        ],
                    },
                ],
            }
        ]
    }

    adapter = _run(orch, {"items": [{"name": "alpha"}]})

    assert adapter.prompts == [
        "DIRECT sees: alpha",
        "NESTED-IF sees: alpha",
        "NESTED-DYN sees: alpha",
    ]


def test_loop_var_survives_if_wrapping_a_dynamic() -> None:
    """The reported authoring shape: loop -> if -> dynamic -> prompt."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "each": {"in": "items", "as": "item"},
                "body": [
                    {
                        "type": "if",
                        "name": "branch",
                        "if": {"mode": "cel", "expr": "1 == 1"},
                        "then": [
                            {
                                "type": "dynamic",
                                "name": "wrap",
                                "effects": [
                                    {
                                        "type": "prompt",
                                        "name": "deep",
                                        "template": "DEEP sees: {{item.name}} #{{_loop_index}}",
                                    }
                                ],
                            }
                        ],
                    }
                ],
            }
        ]
    }

    adapter = _run(orch, {"items": [{"name": "alpha"}, {"name": "beta"}]})

    assert adapter.prompts == ["DEEP sees: alpha #0", "DEEP sees: beta #1"]


def test_loop_var_survives_tree_flow_dynamic_in_loop_body() -> None:
    """Parallel iterations each carry their own binding into the nested dynamic."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "flow": "tree",
                "each": {"in": "items", "as": "item"},
                "body": [
                    {
                        "type": "dynamic",
                        "name": "wrap",
                        "effects": [
                            {
                                "type": "prompt",
                                "name": "nested_dyn",
                                "template": "sees: {{item.name}}",
                            }
                        ],
                    }
                ],
            }
        ]
    }

    adapter = _run(orch, {"items": [{"name": "alpha"}, {"name": "beta"}]})

    assert sorted(adapter.prompts) == ["sees: alpha", "sees: beta"]


def test_nested_dynamic_in_loop_still_sees_root_inputs_and_siblings() -> None:
    """Widening the context must not drop the paths that already resolved.

    ``{{topic}}`` is a root input, ``{{prime.outer.iter_0.first.value}}`` is the
    absolute path of a sibling body effect, and ``{{wrap.inner_first.value}}``
    is a sibling *inside* the dynamic — all three worked (or should have) before
    and must keep working.
    """
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "each": {"in": "items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "first", "template": "FIRST"},
                    {
                        "type": "dynamic",
                        "name": "wrap",
                        "effects": [
                            {
                                "type": "prompt",
                                "name": "inner_first",
                                "template": "INNER",
                            },
                            {
                                "type": "prompt",
                                "name": "inner_second",
                                "template": (
                                    "{{topic}}|{{item.name}}"
                                    "|{{prime.outer.iter_0.first.value}}"
                                    "|{{wrap.inner_first.value}}"
                                ),
                            },
                        ],
                    },
                ],
            }
        ]
    }

    adapter = _run(orch, {"items": [{"name": "alpha"}], "topic": "cybernetics"})

    assert adapter.prompts[-1] == "cybernetics|alpha|FIRST|INNER"


def test_conditional_nested_dynamic_sees_root_context() -> None:
    """A dynamic inside an `if` keeps the root inputs its siblings can see."""
    orch = {
        "effects": [
            {
                "type": "if",
                "name": "gate",
                "if": {"mode": "cel", "expr": "1 == 1"},
                "then": [
                    {
                        "type": "dynamic",
                        "name": "wrap",
                        "effects": [
                            {
                                "type": "prompt",
                                "name": "deep",
                                "template": "topic: {{topic}}",
                            }
                        ],
                    }
                ],
            }
        ]
    }

    adapter = _run(orch, {"topic": "cybernetics"})

    assert adapter.prompts == ["topic: cybernetics"]
