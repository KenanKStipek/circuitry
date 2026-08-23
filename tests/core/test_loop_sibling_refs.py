"""A loop body step must be able to read the step before it, portably.

Regression suite for #88. A chained body — step 2 reads step 1 — is the most
natural multi-step loop shape, and it used to have no reference form that
worked in every loop: ``{{prime.<step>.value}}`` resolved in unnamed loops
only, the bare ``{{<step>.value}}`` in named loops only, and the one absolute
escape hatch (``iter_<N>``) was either empty or, worse, silently stale.

Everything here asserts on the string the adapter actually received — the same
text a run records as ``meta.prompt_sent`` — because that is the only place an
unresolved reference is visible. The run itself succeeds either way.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import apply_effect_overrides, compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.lint import lint_orchestration
from circuitry.core.store import Store

#: Every within-iteration spelling under test, in one template.
PROBE = (
    "canonical=[{{prime.first.value}}] "
    "bare=[{{first.value}}] "
    "loopname=[{{prime.outer.first.value}}] "
    "iter0=[{{prime.outer.iter_0.first.value}}]"
)


@dataclass
class RecordingAdapter:
    """Records every rendered prompt and echoes it straight back.

    Echoing keeps assertions readable — a step's value *is* its template — and
    avoids wrapper characters, which Mustache would HTML-escape on the way
    back into the next template.
    """

    name: str = "recording"
    prompts: list[str] = field(default_factory=list)
    replies: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)
        text = self.replies.pop(0) if self.replies else prompt
        return GenerateResult(text=text, raw={"model": model})


def _run(
    orch: dict[str, Any],
    state: dict[str, Any],
    *,
    replies: list[str] | None = None,
    overrides: dict[str, dict[str, Any]] | None = None,
) -> tuple[RecordingAdapter, dict[str, Any]]:
    root = compile_orchestration(orch=orch, root_name="prime")
    if overrides:
        root, _ = apply_effect_overrides(root, overrides)
    adapter = RecordingAdapter(replies=list(replies or []))
    DynamicRuntime(root, adapter=adapter, model="unit-test").execute(store=Store(state))
    return adapter, state


def _chained_loop(
    *,
    name: str | None,
    flow: str = "chain",
    template: str = PROBE,
) -> dict[str, Any]:
    loop: dict[str, Any] = {
        "type": "loop",
        "flow": flow,
        "each": {"in": "items", "as": "item"},
        "body": [
            {"type": "prompt", "name": "first", "template": "FIRST {{item}}"},
            {"type": "prompt", "name": "second", "template": template},
        ],
    }
    if name:
        loop["name"] = name
    return {"effects": [loop]}


def _probes(adapter: RecordingAdapter) -> list[str]:
    return [p for p in adapter.prompts if p.startswith("canonical=")]


# --------------------------------------------------------------------------
# The reported reproduction, across every loop shape.
# --------------------------------------------------------------------------


@pytest.mark.parametrize("loop_name", ["outer", None], ids=["named", "unnamed"])
@pytest.mark.parametrize("flow", ["chain", "tree"])
def test_body_step_reads_its_sibling_in_the_same_pass(
    loop_name: str | None, flow: str
) -> None:
    """The canonical and bare forms both resolve, in every loop shape.

    Named vs unnamed used to be exactly inverted — one form worked in each —
    so adding a ``name:`` to a working loop silently emptied every template.
    """
    adapter, _ = _run(
        _chained_loop(name=loop_name, flow=flow),
        {"items": ["alpha", "beta"]},
    )

    rendered = sorted(_probes(adapter))
    assert len(rendered) == 2
    for probe, item in zip(rendered, ["alpha", "beta"], strict=True):
        assert f"canonical=[FIRST {item}]" in probe
        assert f"bare=[FIRST {item}]" in probe


def test_each_pass_reads_its_own_output_not_the_first_ones() -> None:
    """Iteration N reads iteration N — the whole point of the scope chain."""
    adapter, _ = _run(
        _chained_loop(name="outer"),
        {"items": ["alpha", "beta", "gamma"]},
    )

    assert [p.split("]")[0] for p in _probes(adapter)] == [
        "canonical=[FIRST alpha",
        "canonical=[FIRST beta",
        "canonical=[FIRST gamma",
    ]


def test_loop_qualified_sibling_path_still_does_not_resolve() -> None:
    """``prime.<loop>.<step>`` is deliberately not made to work.

    ``prime.outer`` is the loop's own node — ``iter_<N>``, ``collected``,
    ``meta``. Grafting body step names into it would collide with the reserved
    ``iter_<N>`` namespace and make one path mean two things. Authors get a
    validation warning pointing at the canonical form instead.
    """
    adapter, _ = _run(_chained_loop(name="outer"), {"items": ["alpha"]})

    assert "loopname=[]" in _probes(adapter)[0]


def test_fixed_iteration_path_inside_a_body_is_stale_not_empty() -> None:
    """Documents the trap the validation warning exists for.

    ``iter_0`` renders iteration 0's output during iteration 1. Empty is
    visible; plausible-but-wrong is not — which is why the warning matters
    more than the grammar change.
    """
    adapter, _ = _run(_chained_loop(name="outer"), {"items": ["alpha", "beta"]})

    first, second = _probes(adapter)
    assert "iter0=[FIRST alpha]" in first
    assert "iter0=[FIRST alpha]" in second  # <- iteration 1, alpha's output


# --------------------------------------------------------------------------
# Scope chain: what the overlay must NOT break.
# --------------------------------------------------------------------------


def test_scope_chain_falls_through_to_enclosing_scope_and_root() -> None:
    """A name the iteration has not written resolves the way it always did."""
    orch = {
        "effects": [
            {"type": "prompt", "name": "before", "template": "BEFORE"},
            {
                "type": "loop",
                "name": "outer",
                "each": {"in": "items", "as": "item"},
                "body": [
                    {
                        "type": "prompt",
                        "name": "probe",
                        "template": (
                            "root=[{{topic}}] "
                            "outer=[{{prime.before.value}}] "
                            "item=[{{item}}] "
                            "idx=[{{_loop_index}}]"
                        ),
                    }
                ],
            },
        ]
    }

    adapter, _ = _run(orch, {"items": ["alpha"], "topic": "cybernetics"})

    assert adapter.prompts[-1] == (
        "root=[cybernetics] outer=[BEFORE] item=[alpha] idx=[0]"
    )


def test_body_step_shadows_an_enclosing_effect_of_the_same_name() -> None:
    """Iteration-local wins, and only for the length of the body."""
    orch = {
        "effects": [
            {"type": "prompt", "name": "note", "template": "OUTER"},
            {
                "type": "loop",
                "name": "outer",
                "each": {"in": "items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "note", "template": "INNER"},
                    {
                        "type": "prompt",
                        "name": "probe",
                        "template": "sees=[{{prime.note.value}}]",
                    },
                ],
            },
            {"type": "prompt", "name": "after", "template": "after=[{{prime.note.value}}]"},
        ]
    }

    adapter, _ = _run(orch, {"items": ["alpha"]})

    assert "sees=[INNER]" in adapter.prompts[-2]
    assert adapter.prompts[-1] == "after=[OUTER]"


def test_collect_still_aggregates_a_chained_body() -> None:
    """The overlay must not disturb per-iteration node writes."""
    orch = _chained_loop(name="outer")
    orch["effects"][0]["collect"] = "second"

    _, state = _run(orch, {"items": ["alpha", "beta"]})

    collected = state["prime"]["outer"]["collected"]["value"]
    assert len(collected) == 2
    assert "canonical=[FIRST alpha]" in collected[0]
    assert "canonical=[FIRST beta]" in collected[1]


def test_disabled_body_step_is_visible_to_later_siblings() -> None:
    """A skip node is exposed on the same terms as a produced one."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "each": {"in": "items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "skipped", "template": "NEVER"},
                    {
                        "type": "prompt",
                        "name": "probe",
                        "template": "sees=[{{prime.skipped.value}}]",
                    },
                ],
            }
        ]
    }

    adapter, state = _run(
        orch,
        {"items": ["alpha"]},
        overrides={"outer.skipped": {"enabled": False}},
    )

    # The node exists (value None), so the reference resolves to empty rather
    # than falling through to some unrelated outer name.
    assert adapter.prompts[-1] == "sees=[]"
    assert state["prime"]["outer"]["iter_0"]["skipped"]["value"] is None


# --------------------------------------------------------------------------
# The chain has to reach every nested container.
# --------------------------------------------------------------------------


@pytest.mark.parametrize(
    "wrapper",
    [
        pytest.param(
            lambda inner: {
                "type": "if",
                "name": "gate",
                "if": {"mode": "cel", "expr": "1 == 1"},
                "then": [inner],
            },
            id="conditional",
        ),
        pytest.param(
            lambda inner: {"type": "dynamic", "name": "wrap", "effects": [inner]},
            id="dynamic",
        ),
        pytest.param(
            lambda inner: {
                "type": "loop",
                "name": "inner_loop",
                "each": {"in": "one", "as": "sub"},
                "body": [inner],
            },
            id="nested-loop",
        ),
    ],
)
def test_sibling_reference_reaches_nested_containers(wrapper) -> None:
    """A grammar with a hole in it is not a grammar authors can rely on."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "each": {"in": "items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "first", "template": "FIRST {{item}}"},
                    wrapper(
                        {
                            "type": "prompt",
                            "name": "deep",
                            "template": "deep=[{{prime.first.value}}] item=[{{item}}]",
                        }
                    ),
                ],
            }
        ]
    }

    adapter, _ = _run(orch, {"items": ["alpha"], "one": [1]})

    assert adapter.prompts[-1] == "deep=[FIRST alpha] item=[alpha]"


def test_body_cel_predicate_sees_the_current_pass() -> None:
    """The same overlay feeds CEL, so ``state.prime.<step>.value`` works too."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "each": {"in": "items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "verdict", "template": "V {{item}}"},
                    {
                        "type": "if",
                        "name": "gate",
                        "if": {
                            "mode": "cel",
                            "expr": "state.prime.verdict.value == 'V alpha'",
                        },
                        "then": [
                            {"type": "prompt", "name": "yes", "template": "TAKEN"}
                        ],
                        "else": [
                            {"type": "prompt", "name": "yes", "template": "NOT-TAKEN"}
                        ],
                    },
                ],
            }
        ]
    }

    adapter, _ = _run(orch, {"items": ["alpha", "beta"]})

    assert [p for p in adapter.prompts if "TAKEN" in p] == ["TAKEN", "NOT-TAKEN"]


# --------------------------------------------------------------------------
# while conditions use the same grammar the body does.
# --------------------------------------------------------------------------


def test_while_cel_condition_sees_the_pass_that_just_finished() -> None:
    """A condition that could not read the body would misroute control flow.

    The body writes ``go`` / ``go`` / ``stop``; the condition keeps going while
    the last pass said ``go``, so it must run exactly three passes.
    """
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "max_iterations": 8,
                "min_iterations": 1,
                "while": {
                    "mode": "cel",
                    "expr": "state.prime.step.value != 'stop'",
                },
                "body": [{"type": "prompt", "name": "step", "template": "STEP"}],
            }
        ]
    }

    adapter, state = _run(orch, {}, replies=["go", "go", "stop"])

    assert len(adapter.prompts) == 3
    assert state["prime"]["spin"]["value"]["iterations"] == 3
    assert state["prime"]["spin"]["value"]["termination"]["reason"] == "condition_false"


def test_while_model_condition_template_renders_the_last_pass() -> None:
    """Same for a model-mode condition: the rendered ask carries the output."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "max_iterations": 2,
                "min_iterations": 1,
                "while": {
                    "mode": "model",
                    "template": "Improve further? [{{prime.step.value}}]",
                },
                "body": [{"type": "prompt", "name": "step", "template": "STEP"}],
            }
        ]
    }

    adapter, _ = _run(orch, {}, replies=["no", "draft-1", "no"])

    asks = [p for p in adapter.prompts if "Improve further?" in p]
    # First check happens before any pass, so the name falls through to empty;
    # the second sees pass 0's output.
    assert "Improve further? []" in asks[0]
    assert "Improve further? [draft-1]" in asks[1]


# --------------------------------------------------------------------------
# Validation warnings — the stale-data one is doing the most work here.
# --------------------------------------------------------------------------


def test_validate_warns_on_fixed_iteration_inside_the_loop_it_names() -> None:
    warnings = lint_orchestration(_chained_loop(name="outer"))

    stale = [w for w in warnings if "fixed pass" in w]
    assert len(stale) == 1
    assert "prime.outer.iter_0" in stale[0]
    assert "{{prime.<step>.value}}" in stale[0]


def test_validate_warns_on_the_loop_qualified_sibling_path() -> None:
    warnings = lint_orchestration(_chained_loop(name="outer"))

    dead = [w for w in warnings if "never resolves" in w]
    assert len(dead) == 1
    assert "prime.outer.first" in dead[0]
    assert "{{prime.first.value}}" in dead[0]


def test_validate_warns_on_a_while_condition_pinned_to_one_pass() -> None:
    """A loop's own condition lives in its iteration scope, so it is checked."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "while": {
                    "mode": "cel",
                    "expr": "state.prime.spin.iter_0.step.value != 'stop'",
                },
                "body": [{"type": "prompt", "name": "step", "template": "STEP"}],
            }
        ]
    }

    assert any("fixed pass" in w for w in lint_orchestration(orch))


def test_validate_is_quiet_on_the_canonical_form() -> None:
    orch = _chained_loop(name="outer", template="{{prime.first.value}}")

    assert lint_orchestration(orch) == []


def test_validate_is_quiet_on_references_outside_the_loop() -> None:
    """After the loop, ``iter_<N>`` and ``collected`` are the right answers."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "collect": "first",
                "each": {"in": "items", "as": "item"},
                "body": [{"type": "prompt", "name": "first", "template": "F"}],
            },
            {
                "type": "prompt",
                "name": "after",
                "template": (
                    "{{prime.outer.iter_0.first.value}} "
                    "{{prime.outer.collected.value}}"
                ),
            },
        ]
    }

    assert lint_orchestration(orch) == []


def test_validate_does_not_warn_on_another_loops_fixed_iteration() -> None:
    """A finished sibling loop's pass 0 is a real, stable reference."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "earlier",
                "each": {"in": "items", "as": "item"},
                "body": [{"type": "prompt", "name": "seed", "template": "S"}],
            },
            {
                "type": "loop",
                "name": "later",
                "each": {"in": "items", "as": "item"},
                "body": [
                    {
                        "type": "prompt",
                        "name": "use_seed",
                        "template": "{{prime.earlier.iter_0.seed.value}}",
                    }
                ],
            },
        ]
    }

    assert lint_orchestration(orch) == []


def test_warnings_reach_the_validate_result(tmp_path) -> None:
    """End to end: the warning an author actually sees from ``cof validate``."""
    import yaml

    from circuitry.api import validate_orchestration

    doc = {"adapter": "ollama", "model": "llama3"}
    doc.update(_chained_loop(name="outer"))
    path = tmp_path / "chained.yml"
    path.write_text(yaml.safe_dump(doc, sort_keys=False), encoding="utf-8")

    result = validate_orchestration(orchestration_path=path)

    assert result["ok"] is True
    assert any("fixed pass" in w for w in result["warnings"])
    assert any("never resolves" in w for w in result["warnings"])
