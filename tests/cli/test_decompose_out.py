"""``cof run --decompose-out <dir>`` — persisting generated decomposition plans.

``meta.decomposition`` already carries the whole story (see
``tests/core/test_decomposition_runtime.py``); these tests cover the
disk-writing layer on top of it: naming, what gets written for a succeeded vs.
a failed plan, that a run with no decomposition writes nothing, and the
load-bearing guarantee — a written plan is a valid orchestration a fresh
``cof run`` can execute standalone to the same result.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.decompose_out import (
    make_decompose_out_observer,
    plan_filename,
    write_decomposition_plan,
)
from circuitry.cli.orchestration_loader import load_orchestration_file
from circuitry.cli.runtime_shim import RunRequest, run

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

TASK_TEMPLATE = "TASK analyze {{topic}} and cross-reference it against {{source}}."
STATE = {"topic": "volcanoes", "source": "the archive"}


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
    """Same contract as the core decomposition tests' double: a planner prompt
    (starts with ``PLANNER``) returns the scripted plan; everything else
    echoes ``gen[<model>]:<prompt>``."""

    name: str = "scripted"
    plans: dict[str, Any] = field(default_factory=dict)
    calls: list[tuple[str, str]] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append((model, prompt))
        if prompt.startswith("PLANNER"):
            for marker, plan in self.plans.items():
                if marker in prompt:
                    return GenerateResult(text=json.dumps(plan), raw={})
            raise RuntimeError("no plan scripted for this source")
        return GenerateResult(text=f"gen[{model}]:{prompt}", raw={})


def _write(path: Path, content: str) -> Path:
    path.write_text(content, encoding="utf-8")
    return path


def _config(planner: Path, **decomposition_overrides: Any) -> CircuitryConfig:
    decomposition: dict[str, Any] = {
        "enabled": True,
        "threshold": 1.0,
        "max_depth": 1,
        "max_chunks": 8,
        "on_failure": "route_up",
        **decomposition_overrides,
    }
    return CircuitryConfig(
        runtime={
            "complexity": {"scoring": {"enabled": True}, "decomposition": decomposition},
            "_decomposition_planner_path": str(planner),
        }
    )


def _run_decomposing_task(
    tmp_path: Path, *, decompose_out: Path | None, adapter: ScriptedAdapter, planner: Path
) -> Any:
    orch = _write(
        tmp_path / "task.yml",
        "effects:\n  - type: prompt\n    name: task\n    template: "
        f'"{TASK_TEMPLATE}"\n',
    )
    return run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state=dict(STATE),
            adapter=adapter,
            model_override="primary-model",
            config=_config(planner),
            skip_preflight=True,
            decompose_out=decompose_out,
        )
    )


# --------------------------------------------------------------------------
# unit: naming and the low-level writer
# --------------------------------------------------------------------------


def test_plan_filename_is_deterministic_and_sanitizes_the_path() -> None:
    name = plan_filename("run-1", "prime.task")
    assert name == plan_filename("run-1", "prime.task")
    assert name == "run-1__prime.task.yml"


def test_plan_filename_differs_across_effects_and_runs() -> None:
    a = plan_filename("run-1", "prime.task")
    b = plan_filename("run-1", "prime.other")
    c = plan_filename("run-2", "prime.task")
    assert len({a, b, c}) == 3


def test_write_decomposition_plan_returns_none_without_yaml(tmp_path: Path) -> None:
    written = write_decomposition_plan(
        tmp_path,
        run_id="run-1",
        effect_path="prime.task",
        decomposition={"decomposed": False, "reason": "planner_failed"},
    )
    assert written is None
    assert list(tmp_path.iterdir()) == []


def test_write_decomposition_plan_writes_a_headed_file(tmp_path: Path) -> None:
    written = write_decomposition_plan(
        tmp_path,
        run_id="run-1",
        effect_path="prime.task",
        decomposition={
            "decomposed": True,
            "reason": None,
            "result_path": "prime.merge.value",
            "chunk_count": 2,
            "yaml": EMITTED_YAML,
        },
    )
    assert written == tmp_path / "run-1__prime.task.yml"
    text = written.read_text(encoding="utf-8")
    assert text.startswith("# Generated by circuitry decomposition")
    assert "# status: succeeded" in text
    assert "# run_id: run-1" in text
    assert "# effect: prime.task" in text
    assert "effects:" in text
    assert "chunk_a" in text


def test_write_decomposition_plan_marks_a_failed_plan(tmp_path: Path) -> None:
    written = write_decomposition_plan(
        tmp_path,
        run_id="run-1",
        effect_path="prime.task",
        decomposition={
            "decomposed": False,
            "reason": "invalid_plan",
            "error": "the plan has 1 chunk(s); a decomposition needs at least 2",
            "yaml": EMITTED_YAML,
        },
    )
    text = written.read_text(encoding="utf-8")
    assert "# status: failed" in text
    assert "# reason: invalid_plan" in text
    assert "at least 2" in text


def test_write_decomposition_plan_writes_a_rejected_payload_when_one_exists(
    tmp_path: Path,
) -> None:
    """`planner_failed` with a `raw_payload` (the planner returned something,
    it just failed the envelope schema) is worth reading and gets written."""
    written = write_decomposition_plan(
        tmp_path,
        run_id="run-1",
        effect_path="prime.task",
        decomposition={
            "decomposed": False,
            "reason": "planner_failed",
            "error": "Schema validation failed: [...] is not of type 'object'",
            "raw_payload": json.dumps(
                [{"name": "chunk_a", "job": "a"}, {"name": "chunk_b", "job": "b"}]
            ),
        },
    )
    assert written == tmp_path / "run-1__prime.task.rejected.yml"
    text = written.read_text(encoding="utf-8")
    assert "# status: failed" in text
    assert "# reason: planner_failed" in text
    assert "chunk_a" in text


def test_write_decomposition_plan_uses_txt_when_payload_does_not_parse(
    tmp_path: Path,
) -> None:
    written = write_decomposition_plan(
        tmp_path,
        run_id="run-1",
        effect_path="prime.task",
        decomposition={
            "decomposed": False,
            "reason": "planner_failed",
            "error": "boom",
            "raw_payload": "{unterminated",
        },
    )
    assert written == tmp_path / "run-1__prime.task.rejected.txt"


def test_write_decomposition_plan_skips_a_genuinely_empty_planner_failure(
    tmp_path: Path,
) -> None:
    """No `raw_payload` at all (timeout, adapter error) — nothing to write."""
    written = write_decomposition_plan(
        tmp_path,
        run_id="run-1",
        effect_path="prime.task",
        decomposition={
            "decomposed": False,
            "reason": "planner_failed",
            "error": "All adapter attempts failed: [...]",
        },
    )
    assert written is None
    assert list(tmp_path.iterdir()) == []


def test_write_decomposition_plan_skips_max_depth(tmp_path: Path) -> None:
    written = write_decomposition_plan(
        tmp_path,
        run_id="run-1",
        effect_path="prime.task",
        decomposition={
            "decomposed": False,
            "reason": "max_depth",
            "outcome": "route_up",
        },
    )
    assert written is None
    assert list(tmp_path.iterdir()) == []


def test_observer_ignores_nodes_without_decomposition_meta(tmp_path: Path) -> None:
    observe = make_decompose_out_observer(tmp_path, "run-1")
    observe("prime.other", {"value": "x", "meta": {"model": "m"}})
    observe("prime.no_meta", {"value": "x"})
    assert list(tmp_path.iterdir()) == []


# --------------------------------------------------------------------------
# integration: a real decomposing run
# --------------------------------------------------------------------------


def test_decompose_out_writes_the_plan_for_a_triggered_decomposition(
    tmp_path: Path,
) -> None:
    planner = _write(tmp_path / "planner.yml", STUB_PLANNER)
    out_dir = tmp_path / "plans"
    adapter = ScriptedAdapter(plans={"TASK": _plan_payload(EMITTED_YAML)})

    result = _run_decomposing_task(
        tmp_path, decompose_out=out_dir, adapter=adapter, planner=planner
    )

    assert result.ok
    assert result.state["prime"]["task"]["meta"]["decomposition"]["decomposed"] is True

    written = list(out_dir.iterdir())
    assert len(written) == 1
    run_id = result.state["runtime"]["last_run"]["run_id"]
    assert written[0].name == f"{run_id}__prime.task.yml"
    text = written[0].read_text(encoding="utf-8")
    assert "# status: succeeded" in text
    assert "chunk_a" in text


def test_no_decompose_out_writes_nothing(tmp_path: Path) -> None:
    planner = _write(tmp_path / "planner.yml", STUB_PLANNER)
    adapter = ScriptedAdapter(plans={"TASK": _plan_payload(EMITTED_YAML)})

    result = _run_decomposing_task(
        tmp_path, decompose_out=None, adapter=adapter, planner=planner
    )

    assert result.ok
    assert result.state["prime"]["task"]["meta"]["decomposition"]["decomposed"] is True


def test_a_run_that_never_decomposes_writes_nothing(tmp_path: Path) -> None:
    planner = _write(tmp_path / "planner.yml", STUB_PLANNER)
    out_dir = tmp_path / "plans"
    # Threshold far above any score this template can reach.
    orch = _write(
        tmp_path / "task.yml",
        "effects:\n  - type: prompt\n    name: task\n    template: \"hi\"\n",
    )
    adapter = ScriptedAdapter()

    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state=dict(STATE),
            adapter=adapter,
            model_override="primary-model",
            config=_config(planner, threshold=100.0),
            skip_preflight=True,
            decompose_out=out_dir,
        )
    )

    assert result.ok
    assert "decomposition" not in result.state["prime"]["task"]["meta"]
    assert not out_dir.exists()


def test_a_failed_plan_is_persisted_and_marked(tmp_path: Path) -> None:
    """`invalid_plan` still emits YAML worth reading — a single-chunk plan is
    refused before anything runs, but the attempt is written to disk anyway."""
    planner = _write(tmp_path / "planner.yml", STUB_PLANNER)
    out_dir = tmp_path / "plans"
    one_chunk = _plan_payload(
        EMITTED_YAML, chunks=[{"name": "chunk_a", "job": "everything"}]
    )
    adapter = ScriptedAdapter(plans={"TASK": one_chunk})

    result = _run_decomposing_task(
        tmp_path, decompose_out=out_dir, adapter=adapter, planner=planner
    )

    assert result.ok
    recorded = result.state["prime"]["task"]["meta"]["decomposition"]
    assert recorded["reason"] == "invalid_plan"

    written = list(out_dir.iterdir())
    assert len(written) == 1
    text = written[0].read_text(encoding="utf-8")
    assert "# status: failed" in text
    assert "# reason: invalid_plan" in text
    assert "at least 2" in text


def test_a_planner_envelope_failure_is_persisted_as_a_rejected_payload(
    tmp_path: Path,
) -> None:
    """The exact field-evidence shape from issue #160: the planner emits a
    conceptually-correct plan as a bare JSON array instead of the required
    ``{say, chunks, yaml}`` envelope. That fails the planner's own schema —
    ``reason: planner_failed`` — but the array it returned is still worth
    reading, so it lands on disk instead of being silently dropped."""
    planner = _write(tmp_path / "planner.yml", STUB_PLANNER)
    out_dir = tmp_path / "plans"
    bare_array = [{"name": "chunk_a", "job": "a"}, {"name": "chunk_b", "job": "b"}]
    adapter = ScriptedAdapter(plans={"TASK": bare_array})

    result = _run_decomposing_task(
        tmp_path, decompose_out=out_dir, adapter=adapter, planner=planner
    )

    assert result.ok
    recorded = result.state["prime"]["task"]["meta"]["decomposition"]
    assert recorded["reason"] == "planner_failed"

    written = list(out_dir.iterdir())
    assert len(written) == 1
    run_id = result.state["runtime"]["last_run"]["run_id"]
    assert written[0].name == f"{run_id}__prime.task.rejected.yml"
    text = written[0].read_text(encoding="utf-8")
    assert "# status: failed" in text
    assert "# reason: planner_failed" in text
    assert "chunk_a" in text


def test_a_genuinely_empty_planner_failure_writes_nothing(tmp_path: Path) -> None:
    """No plan scripted for this source — the adapter raises outright, so
    there is no payload to persist, unlike an envelope-schema rejection."""
    planner = _write(tmp_path / "planner.yml", STUB_PLANNER)
    out_dir = tmp_path / "plans"
    adapter = ScriptedAdapter()  # no plans scripted -> raises for PLANNER prompts

    result = _run_decomposing_task(
        tmp_path, decompose_out=out_dir, adapter=adapter, planner=planner
    )

    assert result.ok
    recorded = result.state["prime"]["task"]["meta"]["decomposition"]
    assert recorded["reason"] == "planner_failed"
    assert "raw_payload" not in recorded
    assert not out_dir.exists() or list(out_dir.iterdir()) == []


def test_two_decomposing_effects_in_one_run_do_not_collide(tmp_path: Path) -> None:
    planner = _write(tmp_path / "planner.yml", STUB_PLANNER)
    out_dir = tmp_path / "plans"
    orch = _write(
        tmp_path / "orch.yml",
        "effects:\n"
        "  - type: prompt\n"
        "    name: task\n"
        f'    template: "{TASK_TEMPLATE}"\n'
        "  - type: prompt\n"
        "    name: other\n"
        f'    template: "{TASK_TEMPLATE}"\n',
    )
    adapter = ScriptedAdapter(plans={"TASK": _plan_payload(EMITTED_YAML)})

    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state=dict(STATE),
            adapter=adapter,
            model_override="primary-model",
            config=_config(planner),
            skip_preflight=True,
            decompose_out=out_dir,
        )
    )

    assert result.ok
    written = {p.name for p in out_dir.iterdir()}
    run_id = result.state["runtime"]["last_run"]["run_id"]
    assert written == {f"{run_id}__prime.task.yml", f"{run_id}__prime.other.yml"}


def test_a_written_plan_re_runs_standalone_to_the_same_result(tmp_path: Path) -> None:
    """The round-trip acceptance criterion: a file `--decompose-out` writes is
    itself a valid orchestration a fresh `cof run` executes directly, and it
    reproduces the same merged value the original decomposition produced."""
    planner = _write(tmp_path / "planner.yml", STUB_PLANNER)
    out_dir = tmp_path / "plans"
    adapter = ScriptedAdapter(plans={"TASK": _plan_payload(EMITTED_YAML)})

    original = _run_decomposing_task(
        tmp_path, decompose_out=out_dir, adapter=adapter, planner=planner
    )
    assert original.ok
    original_value = original.state["prime"]["task"]["value"]

    written = next(out_dir.iterdir())
    # A written plan is loadable through the normal orchestration loader —
    # the header is plain YAML comments, invisible to the parser.
    parsed = load_orchestration_file(written)
    assert "effects" in parsed

    standalone_adapter = ScriptedAdapter()
    standalone = run(
        RunRequest(
            orchestration_path=written,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state=dict(STATE),
            adapter=standalone_adapter,
            model_override="primary-model",
            config=CircuitryConfig(),
            skip_preflight=True,
        )
    )

    assert standalone.ok
    assert standalone.state["prime"]["merge"]["value"] == original_value
