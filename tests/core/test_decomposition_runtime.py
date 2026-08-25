"""Runtime decomposition: trigger, execute, write back, fail safe.

The contract under test is substitution: when an effect scores over the
threshold, the planner's emitted orchestration runs *in its place* and the
merged result lands at the original effect's own state path, so downstream
consumers run unchanged. Then the guarantees around it: the trigger is strict
(`score > threshold`), an invalid plan never runs, `max_depth` bounds the
recursion, and none of the four failure paths — planner failure, invalid plan,
child execution failure, depth exhaustion — can leave partial state at the
original path or (under the default `on_failure: route_up`) turn a working run
into a failed one.

The planner is a stub orchestration (one prompt effect, same interface output
names as the bundled planner) driven by a scripted adapter, so every plan —
valid, invalid, or absent — is a test input rather than a model's mood.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store

#: Same interface output names as the bundled planner; the runtime reads the
#: plan through the interface, so this stands in for `agents/decompose.yml`
#: with a single scripted model call.
STUB_PLANNER = """
interface:
  outputs:
    say: {path: prime.plan.value.say}
    chunks: {path: prime.plan.value.chunks}
    yaml: {path: prime.plan.value.yaml}
    done: {path: prime.plan.value.done}
    result_path: {path: prime.plan.value.result_path}
effects:
  - type: prompt
    name: plan
    prompt_type: object
    schema:
      type: object
      properties:
        say: {type: string}
        chunks: {type: array}
        yaml: {type: string}
        done: {type: boolean}
        result_path: {type: string}
      required: [say, chunks, yaml]
    template: "PLANNER {{source_template}}"
"""

#: What the planner emits for the source effect: two chunks and a merge, per
#: the merge contract (top-level effect named `merge`, result at
#: prime.merge.value). The `analyze` keyword keeps chunk scores comfortably
#: above the tiny thresholds these tests use, so nested-trigger behavior is
#: deterministic.
EMITTED_YAML = """\
effects:
  - type: prompt
    name: chunk_a
    template: "CHUNK_A analyze {{topic}}"
  - type: prompt
    name: chunk_b
    template: "CHUNK_B analyze {{topic}}"
  - type: prompt
    name: merge
    template: "MERGE {{prime.chunk_a.value}} {{prime.chunk_b.value}}"
"""

#: A second-level plan for CHUNK_A, so nested decomposition is observable in
#: the final merged value (SUB_A shows up only when depth 1 decomposed).
SUB_YAML = """\
effects:
  - type: prompt
    name: sub_a
    template: "SUB_A analyze {{topic}}"
  - type: prompt
    name: sub_b
    template: "SUB_B analyze {{topic}}"
  - type: prompt
    name: merge
    template: "SUBMERGE {{prime.sub_a.value}} {{prime.sub_b.value}}"
"""


def _plan_payload(
    yaml_text: str,
    *,
    chunks: list[dict[str, str]] | None = None,
    done: Any = True,
    result_path: str = "prime.merge.value",
    say: str = "split on the natural units",
) -> dict[str, Any]:
    return {
        "say": say,
        "chunks": chunks
        if chunks is not None
        else [{"name": "chunk_a", "job": "a"}, {"name": "chunk_b", "job": "b"}],
        "yaml": yaml_text,
        "done": done,
        "result_path": result_path,
    }


@dataclass
class ScriptedAdapter:
    """Answers planner prompts from a marker->plan script, echoes the rest.

    A planner prompt (the stub's template starts with ``PLANNER``) returns the
    JSON plan whose marker appears in the source template; no matching marker
    is a planner outage. Every other prompt echoes ``gen[<model>]:<prompt>``,
    which is what lets assertions read the model *and* the interpolated
    context straight out of any downstream value.
    """

    plans: dict[str, dict[str, Any]] = field(default_factory=dict)
    fail_planner: bool = False
    fail_prefix: str | None = None
    name: str = "primary"
    calls: list[tuple[str, str]] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append((model, prompt))
        if prompt.startswith("PLANNER"):
            if self.fail_planner:
                raise RuntimeError("planner outage")
            for marker, plan in self.plans.items():
                if marker in prompt:
                    return GenerateResult(text=json.dumps(plan), raw={})
            raise RuntimeError("no plan scripted for this source")
        if self.fail_prefix and prompt.startswith(self.fail_prefix):
            raise RuntimeError("chunk outage")
        return GenerateResult(text=f"gen[{model}]:{prompt}", raw={})

    def planner_calls(self) -> list[str]:
        return [prompt for _, prompt in self.calls if prompt.startswith("PLANNER")]


#: Scores > 5 under default weights (two references plus keyword matches, per
#: the recorded-score tests), so single-digit thresholds trigger reliably.
TASK_TEMPLATE = "TASK analyze {{topic}} and cross-reference it against {{source}}."


def _orch(*extra_effects: dict[str, Any]) -> dict[str, Any]:
    return {
        "effects": [
            {"type": "prompt", "name": "task", "template": TASK_TEMPLATE},
            *extra_effects,
        ]
    }


def _config(
    planner: Path,
    *,
    threshold: float = 1.0,
    max_depth: int = 1,
    max_chunks: int = 8,
    on_failure: str = "route_up",
    routing: dict[str, Any] | None = None,
) -> dict[str, Any]:
    complexity: dict[str, Any] = {
        "scoring": {"enabled": True},
        "decomposition": {
            "enabled": True,
            "threshold": threshold,
            "max_depth": max_depth,
            "max_chunks": max_chunks,
            "on_failure": on_failure,
        },
    }
    if routing is not None:
        complexity["routing"] = routing
    return {
        "complexity": complexity,
        "_decomposition_planner_path": str(planner),
    }


ROUTING_BIG = {"enabled": True, "bands": [{"name": "top", "model": "big-model"}]}

STATE = {"topic": "volcanoes", "source": "the archive"}


def _run(
    orch: dict[str, Any],
    *,
    adapter: ScriptedAdapter,
    runtime_config: dict[str, Any],
    store: Store | None = None,
) -> Store:
    root = compile_orchestration(orch=orch, root_name="prime")
    store = store if store is not None else Store(dict(STATE))
    DynamicRuntime(
        root,
        adapter=adapter,
        model="primary-model",
        runtime_config=runtime_config,
    ).execute(store=store)
    return store


@pytest.fixture()
def planner(tmp_path: Path) -> Path:
    path = tmp_path / "stub_planner.yml"
    path.write_text(STUB_PLANNER, encoding="utf-8")
    return path


# --------------------------------------------------------------------------
# trigger
# --------------------------------------------------------------------------


def _recorded_score(planner: Path) -> float:
    """The score the trigger will compare — from a run that cannot trigger."""
    adapter = ScriptedAdapter()
    store = _run(
        _orch(),
        adapter=adapter,
        runtime_config={"complexity": {"scoring": {"enabled": True}}},
    )
    score = store.get("prime.task.meta.complexity.score")
    assert isinstance(score, float) and score > 1.0
    return score


def test_score_equal_to_threshold_does_not_trigger(planner: Path) -> None:
    """The trigger is strictly greater-than; the boundary effect is untouched."""
    score = _recorded_score(planner)
    adapter = ScriptedAdapter(plans={"TASK": _plan_payload(EMITTED_YAML)})
    store = _run(
        _orch(),
        adapter=adapter,
        runtime_config=_config(planner, threshold=score),
    )

    assert "decomposition" not in store.get("prime.task.meta")
    assert store.get("prime.task.value") == f"gen[primary-model]:{store.get('prime.task.meta.prompt_sent')}"
    assert adapter.planner_calls() == []


def test_score_above_threshold_triggers(planner: Path) -> None:
    score = _recorded_score(planner)
    adapter = ScriptedAdapter(plans={"TASK": _plan_payload(EMITTED_YAML)})
    store = _run(
        _orch(),
        adapter=adapter,
        runtime_config=_config(planner, threshold=score - 0.25),
    )

    assert store.get("prime.task.meta.decomposition.decomposed") is True
    assert len(adapter.planner_calls()) == 1


def test_below_threshold_effect_is_untouched(planner: Path) -> None:
    adapter = ScriptedAdapter(plans={"TASK": _plan_payload(EMITTED_YAML)})
    store = _run(
        _orch(),
        adapter=adapter,
        runtime_config=_config(planner, threshold=100.0),
    )

    meta = store.get("prime.task.meta")
    assert "decomposition" not in meta
    assert store.get("prime.task.value").startswith("gen[primary-model]:TASK")
    assert adapter.planner_calls() == []


# --------------------------------------------------------------------------
# write-back and metadata
# --------------------------------------------------------------------------


def test_merged_result_lands_at_the_original_path_and_downstream_consumes_it(
    planner: Path,
) -> None:
    """The load-bearing criterion: substitution is invisible downstream.

    The downstream effect references ``{{prime.task.value}}`` exactly as it
    would against the undecomposed effect — its rendered prompt must contain
    the child's merged result, asserted from state, not by inspection.
    """
    adapter = ScriptedAdapter(plans={"TASK": _plan_payload(EMITTED_YAML)})
    store = _run(
        _orch(
            {
                "type": "prompt",
                "name": "downstream",
                "template": "DOWN {{prime.task.value}}",
            }
        ),
        adapter=adapter,
        runtime_config=_config(planner),
    )

    merged = store.get("prime.task.value")
    # The merge step saw both chunks' outputs, each of which interpolated the
    # parent's own context — the copied-context seam and the write-back seam
    # in one string.
    assert merged.startswith("gen[primary-model]:MERGE")
    assert "CHUNK_A analyze volcanoes" in merged
    assert "CHUNK_B analyze volcanoes" in merged

    assert store.get("prime.downstream.meta.prompt_sent") == f"DOWN {merged}"
    assert store.get("prime.downstream.value") == f"gen[primary-model]:DOWN {merged}"

    # The child ran in scratch state: no chunk nodes leak into the parent tree.
    assert set(store.get("prime.task").keys()) == {"value", "meta"}
    assert store.get("prime.chunk_a") is None
    assert store.get("prime.merge") is None


def test_decomposition_metadata_is_recorded_on_the_node(planner: Path) -> None:
    adapter = ScriptedAdapter(plans={"TASK": _plan_payload(EMITTED_YAML)})
    store = _run(_orch(), adapter=adapter, runtime_config=_config(planner))

    recorded = store.get("prime.task.meta.decomposition")
    assert recorded["decomposed"] is True
    assert recorded["outcome"] == "decomposed"
    assert recorded["reason"] is None
    assert recorded["depth"] == 0
    assert recorded["max_depth"] == 1
    assert recorded["chunk_count"] == 2
    assert recorded["plan"]["say"] == "split on the natural units"
    assert [chunk["name"] for chunk in recorded["plan"]["chunks"]] == [
        "chunk_a",
        "chunk_b",
    ]
    assert recorded["result_path"] == "prime.merge.value"
    assert recorded["yaml"] == EMITTED_YAML.strip()
    assert recorded["score"] > recorded["threshold"] == 1.0


def test_child_effects_announce_under_the_parent_node(planner: Path) -> None:
    """#127's bargain holds for decomposition children: isolated state, shared
    observation — planner and chunk effects fire lifecycle callbacks namespaced
    under the decomposing effect's own path."""
    seen: list[str] = []
    adapter = ScriptedAdapter(plans={"TASK": _plan_payload(EMITTED_YAML)})
    _run(
        _orch(),
        adapter=adapter,
        runtime_config=_config(planner),
        store=Store(dict(STATE), effect_start=lambda path, node: seen.append(path)),
    )

    assert "prime.task" in seen
    assert "prime.task.plan" in seen  # the stub planner's own prompt
    for child in ("chunk_a", "chunk_b", "merge"):
        assert f"prime.task.{child}" in seen


# --------------------------------------------------------------------------
# failure paths — planner failure, invalid plan, child execution failure
# --------------------------------------------------------------------------


def test_planner_failure_routes_up_to_the_capable_model(planner: Path) -> None:
    adapter = ScriptedAdapter(fail_planner=True)
    store = _run(
        _orch(),
        adapter=adapter,
        runtime_config=_config(planner, routing=ROUTING_BIG),
    )

    recorded = store.get("prime.task.meta.decomposition")
    assert recorded["outcome"] == "route_up"
    assert recorded["reason"] == "planner_failed"
    assert recorded["fallback_model"] == "big-model"
    # The original prompt ran, on the catch-all band's model, and the run lived.
    assert store.get("prime.task.value").startswith("gen[big-model]:TASK")
    assert store.get("prime.task.meta.model") == "big-model"
    assert store.get("prime.task.meta.error") is None


def test_planner_failure_with_on_failure_fail_propagates(planner: Path) -> None:
    adapter = ScriptedAdapter(fail_planner=True)
    store = Store(dict(STATE))
    with pytest.raises(RuntimeError, match="planner_failed"):
        _run(
            _orch(),
            adapter=adapter,
            runtime_config=_config(planner, on_failure="fail"),
            store=store,
        )

    assert store.get("prime.task.value") is None
    assert "planner_failed" in store.get("prime.task.meta.error")
    assert store.get("prime.task.meta.decomposition.outcome") == "failed"
    assert set(store.get("prime.task").keys()) == {"value", "meta"}


def test_invalid_plan_never_runs_and_routes_up(planner: Path) -> None:
    """Unparseable YAML with a confident `done: true` — the runtime's own
    validation is the gate, not the planner's word."""
    adapter = ScriptedAdapter(plans={"TASK": _plan_payload("effects: [", done=True)})
    store = _run(_orch(), adapter=adapter, runtime_config=_config(planner))

    recorded = store.get("prime.task.meta.decomposition")
    assert recorded["outcome"] == "run_as_is"  # routing off: degrade gracefully
    assert recorded["reason"] == "invalid_plan"
    # No chunk ever dispatched: the only non-planner call is the original
    # prompt running as-is.
    non_planner = [p for _, p in adapter.calls if not p.startswith("PLANNER")]
    assert len(non_planner) == 1 and non_planner[0].startswith("TASK")
    assert store.get("prime.task.value").startswith("gen[primary-model]:TASK")


def test_single_chunk_plan_is_invalid(planner: Path) -> None:
    """A "decomposition" into one chunk is the original prompt with extra
    scaffolding; the validator refuses it before anything runs."""
    one_chunk = _plan_payload(
        EMITTED_YAML, chunks=[{"name": "chunk_a", "job": "everything"}]
    )
    adapter = ScriptedAdapter(plans={"TASK": one_chunk})
    store = _run(_orch(), adapter=adapter, runtime_config=_config(planner))

    recorded = store.get("prime.task.meta.decomposition")
    assert recorded["reason"] == "invalid_plan"
    assert "at least 2" in recorded["error"]


def test_over_budget_plan_is_invalid(planner: Path) -> None:
    """`max_chunks` is a hard ceiling on the plan, enforced by the runtime's
    validation — the same plan that decomposes under a budget of 2 is refused
    under a budget it exceeds."""
    within = ScriptedAdapter(plans={"TASK": _plan_payload(EMITTED_YAML)})
    store = _run(
        _orch(), adapter=within, runtime_config=_config(planner, max_chunks=2)
    )
    assert store.get("prime.task.meta.decomposition.decomposed") is True

    three_chunks = _plan_payload(
        EMITTED_YAML,
        chunks=[
            {"name": "chunk_a", "job": "a"},
            {"name": "chunk_b", "job": "b"},
            {"name": "chunk_c", "job": "c"},
        ],
    )
    over = ScriptedAdapter(plans={"TASK": three_chunks})
    refused = _run(
        _orch(), adapter=over, runtime_config=_config(planner, max_chunks=2)
    )
    recorded = refused.get("prime.task.meta.decomposition")
    assert recorded["reason"] == "invalid_plan"
    assert "budget is 2" in recorded["error"]


def test_plan_missing_the_merge_effect_is_invalid(planner: Path) -> None:
    """The merge contract is enforced structurally: nothing in the emitted
    document writes `result_path`, so running it could only ever read back
    stale or absent state."""
    no_merge = (
        "effects:\n"
        '  - type: prompt\n'
        "    name: chunk_a\n"
        '    template: "CHUNK_A analyze {{topic}}"\n'
        "  - type: prompt\n"
        "    name: assemble\n"
        '    template: "ASSEMBLE {{prime.chunk_a.value}}"\n'
    )
    adapter = ScriptedAdapter(plans={"TASK": _plan_payload(no_merge)})
    store = _run(_orch(), adapter=adapter, runtime_config=_config(planner))

    recorded = store.get("prime.task.meta.decomposition")
    assert recorded["reason"] == "invalid_plan"
    assert "merge" in recorded["error"]


def test_child_execution_failure_routes_up_with_no_partial_state(
    planner: Path,
) -> None:
    adapter = ScriptedAdapter(
        plans={"TASK": _plan_payload(EMITTED_YAML)}, fail_prefix="CHUNK_A"
    )
    store = _run(
        _orch(),
        adapter=adapter,
        runtime_config=_config(planner, routing=ROUTING_BIG),
    )

    recorded = store.get("prime.task.meta.decomposition")
    assert recorded["outcome"] == "route_up"
    assert recorded["reason"] == "execution_failed"
    assert store.get("prime.task.value").startswith("gen[big-model]:TASK")
    # The dead child's scratch state was discarded whole.
    assert set(store.get("prime.task").keys()) == {"value", "meta"}
    assert store.get("prime.chunk_a") is None


def test_child_execution_failure_with_on_failure_fail_propagates(
    planner: Path,
) -> None:
    adapter = ScriptedAdapter(
        plans={"TASK": _plan_payload(EMITTED_YAML)}, fail_prefix="CHUNK_A"
    )
    store = Store(dict(STATE))
    with pytest.raises(RuntimeError, match="execution_failed"):
        _run(
            _orch(),
            adapter=adapter,
            runtime_config=_config(planner, on_failure="fail"),
            store=store,
        )

    assert store.get("prime.task.value") is None
    assert set(store.get("prime.task").keys()) == {"value", "meta"}
    assert store.get("prime.task.meta.decomposition.reason") == "execution_failed"


# --------------------------------------------------------------------------
# max_depth
# --------------------------------------------------------------------------


def test_max_depth_zero_routes_up_before_the_planner_ever_runs(
    planner: Path,
) -> None:
    adapter = ScriptedAdapter(plans={"TASK": _plan_payload(EMITTED_YAML)})
    store = _run(
        _orch(),
        adapter=adapter,
        runtime_config=_config(planner, max_depth=0, routing=ROUTING_BIG),
    )

    recorded = store.get("prime.task.meta.decomposition")
    assert recorded["outcome"] == "route_up"
    assert recorded["reason"] == "max_depth"
    assert recorded["depth"] == 0
    assert recorded["max_depth"] == 0
    assert adapter.planner_calls() == []
    assert store.get("prime.task.value").startswith("gen[big-model]:TASK")


def test_max_depth_one_stops_chunks_from_decomposing_again(planner: Path) -> None:
    """Chunks score above the threshold too — at `max_depth: 1` they hit the
    ceiling and run as-is (routing off), so exactly one plan is ever made."""
    adapter = ScriptedAdapter(
        plans={
            "TASK": _plan_payload(EMITTED_YAML),
            "CHUNK_A": _plan_payload(SUB_YAML),
        }
    )
    store = _run(
        _orch(),
        adapter=adapter,
        runtime_config=_config(planner, max_depth=1),
    )

    merged = store.get("prime.task.value")
    assert "CHUNK_A analyze volcanoes" in merged
    assert "SUB_A" not in merged
    assert len(adapter.planner_calls()) == 1


def test_max_depth_two_decomposes_the_chunk_then_holds_the_ceiling(
    planner: Path,
) -> None:
    """At `max_depth: 2` the chunk decomposes once more — its value is the
    sub-merge — and the depth-2 grandchildren run as-is: the ceiling holds one
    level further down, observable in the final merged string."""
    adapter = ScriptedAdapter(
        plans={
            "TASK": _plan_payload(EMITTED_YAML),
            "CHUNK_A": _plan_payload(SUB_YAML),
        }
    )
    store = _run(
        _orch(),
        adapter=adapter,
        runtime_config=_config(planner, max_depth=2),
    )

    merged = store.get("prime.task.value")
    assert "SUBMERGE" in merged
    assert "SUB_A analyze volcanoes" in merged
    # Depth-2 effects (sub_a/sub_b/the sub-merge) never plan again: the only
    # planner calls are the depth-0 task and the depth-1 chunks.
    assert all(
        "SUB" not in prompt.split("PLANNER", 1)[1]
        for prompt in adapter.planner_calls()
    )


def test_a_plan_that_reemits_the_running_orchestration_is_cut_off(
    planner: Path,
) -> None:
    """The `use` cycle guard covers decomposition children: a chunk whose plan
    is the very orchestration already executing fails that one decomposition
    (execution path) and runs as-is — the run completes."""
    adapter = ScriptedAdapter(
        plans={
            "TASK": _plan_payload(EMITTED_YAML),
            "CHUNK_A": _plan_payload(EMITTED_YAML),
        }
    )
    store = _run(
        _orch(),
        adapter=adapter,
        runtime_config=_config(planner, max_depth=3),
    )

    merged = store.get("prime.task.value")
    assert store.get("prime.task.meta.decomposition.decomposed") is True
    assert "CHUNK_A analyze volcanoes" in merged


# --------------------------------------------------------------------------
# routing off
# --------------------------------------------------------------------------


def test_routing_off_chunks_run_on_the_default_model(planner: Path) -> None:
    adapter = ScriptedAdapter(plans={"TASK": _plan_payload(EMITTED_YAML)})
    _run(_orch(), adapter=adapter, runtime_config=_config(planner))

    assert adapter.calls and all(model == "primary-model" for model, _ in adapter.calls)


def test_routing_off_route_up_degrades_to_running_as_is(planner: Path) -> None:
    adapter = ScriptedAdapter(fail_planner=True)
    store = _run(_orch(), adapter=adapter, runtime_config=_config(planner))

    recorded = store.get("prime.task.meta.decomposition")
    assert recorded["outcome"] == "run_as_is"
    assert recorded["fallback_model"] is None
    assert store.get("prime.task.value").startswith("gen[primary-model]:TASK")
    assert store.get("prime.task.meta.model") == "primary-model"


def test_route_up_respects_an_explicit_model(planner: Path) -> None:
    """`routing.respect_explicit` (the default) covers the fallback too: an
    effect that pins its own model keeps it when decomposition falls back."""
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "task",
                "template": TASK_TEMPLATE,
                "model": "pinned-model",
            }
        ]
    }
    adapter = ScriptedAdapter(fail_planner=True)
    store = _run(
        orch,
        adapter=adapter,
        runtime_config=_config(planner, routing=ROUTING_BIG),
    )

    assert store.get("prime.task.meta.decomposition.outcome") == "run_as_is"
    assert store.get("prime.task.value").startswith("gen[pinned-model]:TASK")


# --------------------------------------------------------------------------
# input inference
# --------------------------------------------------------------------------


def test_describe_inputs_prefers_the_namespaced_input_root() -> None:
    """When ``ctx`` carries the post-#86 ``input`` namespace, only its keys
    are declared — ``prime``/``runtime`` siblings and ``_``-prefixed
    builtins at the same top level are never genuine caller inputs.

    Regression test for #161: run c69cfb8d's emitted plan declared
    ``runtime`` as a required input because the framework namespace sits at
    the same top level as ``input`` in the render context and leaked into
    `_describe_inputs`'s enumeration.
    """
    from circuitry.core.decompose import _describe_inputs

    ctx = {
        "input": {"topic": "volcanoes", "source": "the archive"},
        "prime": {"task": {"value": "..."}},
        "runtime": {"effective_settings": {}},
        "_loop_index": 0,
    }

    description = _describe_inputs(ctx)

    assert "topic" in description
    assert "source" in description
    assert "runtime" not in description
    assert "prime" not in description
    assert "_loop_index" not in description


def test_describe_inputs_falls_back_to_excluding_known_namespaces() -> None:
    """A context built without the ``input`` wrapper (e.g. a ``Store``
    constructed directly, as the rest of this test file does) still
    excludes ``prime``/``runtime``/``_``-prefixed keys from the inferred
    inputs list — the same fallback ``_sql_persistence.extract_inputs``
    uses for a pre-namespace state snapshot.
    """
    from circuitry.core.decompose import _describe_inputs

    ctx = {
        "topic": "volcanoes",
        "source": "the archive",
        "runtime": {"effective_settings": {}},
        "prime": {"task": {"value": "..."}},
        "_loop_index": 0,
    }

    description = _describe_inputs(ctx)

    assert "topic" in description
    assert "source" in description
    assert "runtime" not in description
    assert "prime" not in description
    assert "_loop_index" not in description


def test_decomposition_planner_prompt_never_declares_runtime_as_an_input(
    tmp_path: Path,
) -> None:
    """End to end: a `runtime` state key sits alongside `topic`/`source` in
    the effect's render context, but the planner prompt's source_interface
    must only ever mention genuine caller inputs.
    """
    planner_path = tmp_path / "stub_planner_with_interface.yml"
    planner_path.write_text(
        STUB_PLANNER.replace(
            'template: "PLANNER {{source_template}}"',
            'template: "PLANNER {{source_template}} :: {{source_interface}}"',
        ),
        encoding="utf-8",
    )

    adapter = ScriptedAdapter(plans={"TASK": _plan_payload(EMITTED_YAML)})
    state = {**STATE, "runtime": {"effective_settings": {"adapter": "ollama"}}}
    store = _run(
        _orch(),
        adapter=adapter,
        runtime_config=_config(planner_path),
        store=Store(state),
    )

    assert store.get("prime.task.meta.decomposition.outcome") == "decomposed"
    [planner_prompt] = adapter.planner_calls()
    assert "topic" in planner_prompt
    assert "runtime" not in planner_prompt


# --------------------------------------------------------------------------
# housekeeping
# --------------------------------------------------------------------------


def test_the_bundled_planner_is_the_default(planner: Path) -> None:
    from circuitry.core.decompose import _planner_path

    bundled = _planner_path({})
    assert bundled.is_file()
    assert bundled.name == "decompose.yml"
    assert _planner_path({"_decomposition_planner_path": str(planner)}) == planner
