"""The complexity score recorded on a prompt effect's state node.

The score is written in the pre-dispatch meta block, which is what gives it the
two properties worth testing: it is on the node before the adapter is ever
called, and it is therefore still there when the adapter blows up. The other
half of the contract is negative — with scoring off the key does not exist at
all, so the state tree is unchanged from a build without the feature.
"""

from __future__ import annotations

import copy
import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store

SCORING_ON: dict[str, Any] = {"complexity": {"scoring": {"enabled": True}}}
SCORING_OFF: dict[str, Any] = {"complexity": {"scoring": {"enabled": False}}}


@dataclass(frozen=True)
class EchoAdapter:
    name: str = "primary"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=f"echo:{prompt}", raw={})


@dataclass(frozen=True)
class AlwaysFailAdapter:
    name: str = "primary"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        raise RuntimeError(f"{self.name} outage")


def _prompt_orch(**overrides: Any) -> dict[str, Any]:
    effect: dict[str, Any] = {
        "type": "prompt",
        "name": "task",
        "template": "Analyze {{topic}} and cross-reference it against {{source}}.",
    }
    effect.update(overrides)
    return {"effects": [effect]}


def _run(
    orch: dict[str, Any],
    *,
    adapter: Any = None,
    runtime_config: dict[str, Any] | None = None,
    store: Store | None = None,
) -> Store:
    root = compile_orchestration(orch=orch, root_name="prime")
    store = store if store is not None else Store({})
    DynamicRuntime(
        root,
        adapter=adapter or EchoAdapter(),
        model="primary-model",
        runtime_config=runtime_config,
    ).execute(store=store)
    return store


def _strip_timestamps(value: Any) -> Any:
    """The state tree minus the two keys that legitimately differ per run."""
    if isinstance(value, dict):
        return {
            key: _strip_timestamps(item)
            for key, item in value.items()
            if key not in ("created_at", "completed_at")
        }
    if isinstance(value, list):
        return [_strip_timestamps(item) for item in value]
    return value


def test_score_and_breakdown_land_on_the_node_before_dispatch() -> None:
    """``on_effect_start`` fires after the pre-dispatch meta block and before
    any execution branch, so what it sees is exactly what was written first."""
    seen: dict[str, Any] = {}

    def capture(path: str, node: dict[str, Any]) -> None:
        seen[path] = copy.deepcopy(node)

    _run(
        _prompt_orch(),
        runtime_config=SCORING_ON,
        store=Store({}, effect_start=capture),
    )

    complexity = seen["prime.task"]["meta"]["complexity"]
    assert complexity["mode"] == "rendered"
    assert 0.0 < complexity["score"] <= complexity["max_score"] == 100.0

    # The breakdown is keyed by signal name and reconstructs the total, which
    # is the property that makes a surprising score arguable from state alone.
    signals = complexity["signals"]
    assert set(signals) == {
        "prompt_size",
        "state_references",
        "output_type",
        "schema_shape",
        "output_size",
        "structure",
        "keywords",
    }
    total = sum(signal["contribution"] for signal in signals.values())
    assert total == pytest.approx(complexity["score"], abs=1e-6)

    # No adapter result has been recorded yet — this really is pre-dispatch.
    assert seen["prime.task"]["meta"]["tokens_received"] is None
    assert seen["prime.task"]["value"] is None


def test_score_survives_effect_failure() -> None:
    """The diagnostic has to outlive the thing it diagnoses."""
    store = Store({})
    with pytest.raises(RuntimeError):
        _run(
            _prompt_orch(),
            adapter=AlwaysFailAdapter(),
            runtime_config=SCORING_ON,
            store=store,
        )

    assert isinstance(store.get("prime.task.meta.error"), str)
    assert store.get("prime.task.value") is None
    assert store.get("prime.task.meta.complexity.score") > 0.0
    assert store.get("prime.task.meta.complexity.signals.keywords.note")


def test_configured_weights_are_translated_to_scorer_signals() -> None:
    """``runtime.complexity.scoring.weights`` names signals in the config's
    vocabulary; a weight set there must actually move the score."""
    orch = _prompt_orch()
    baseline = _run(orch, runtime_config=SCORING_ON)

    weighted = _run(
        orch,
        runtime_config={
            "complexity": {
                "scoring": {
                    "enabled": True,
                    # Config spelling of the scorer's ``state_references``
                    # neighbours: only ``structural_position`` survives, so a
                    # top-level effect's score collapses toward zero.
                    "weights": {
                        "prompt_size": 0.0,
                        "state_references": 0.0,
                        "prompt_type": 0.0,
                        "output_schema": 0.0,
                        "output_size": 0.0,
                        "structural_position": 1.0,
                        "keywords": 0.0,
                    },
                }
            }
        },
    )

    assert baseline.get("prime.task.meta.complexity.score") > 0.0
    assert weighted.get("prime.task.meta.complexity.score") == 0.0
    signals = weighted.get("prime.task.meta.complexity.signals")
    assert signals["structure"]["weight"] == 1.0
    assert signals["prompt_size"]["weight"] == 0.0


@pytest.mark.parametrize("runtime_config", [None, {}, SCORING_OFF])
def test_no_key_is_added_when_scoring_is_disabled(
    runtime_config: dict[str, Any] | None,
) -> None:
    """Disabled means absent — not null, not an empty object."""
    reference = _run(_prompt_orch())
    store = _run(_prompt_orch(), runtime_config=runtime_config)

    meta = store.get("prime.task.meta")
    assert "complexity" not in meta

    # Byte-identical to a run with no complexity config at all, timestamps
    # aside. Serializing catches a key appearing anywhere in the tree, not just
    # on the node this test happens to look at.
    serialized = json.dumps(_strip_timestamps(store.state), sort_keys=True)
    assert serialized == json.dumps(
        _strip_timestamps(reference.state), sort_keys=True
    )
    assert "complexity" not in serialized


def test_cel_condition_branches_on_the_recorded_score() -> None:
    """The acceptance criterion that a dict lookup cannot stand in for: the
    score has to resolve through the real CEL evaluator, addressed the way an
    orchestration addresses state."""
    orch = {
        "effects": [
            _prompt_orch()["effects"][0],
            {
                "type": "conditional",
                "name": "gate",
                "if": {
                    "mode": "cel",
                    "expr": "state.prime.task.meta.complexity.score > 5.0",
                },
                "then": [
                    {"type": "prompt", "name": "deep_review", "template": "deep"}
                ],
                "else": [
                    {"type": "prompt", "name": "quick_pass", "template": "quick"}
                ],
            },
        ]
    }

    store = _run(orch, runtime_config=SCORING_ON)

    score = store.get("prime.task.meta.complexity.score")
    assert score > 5.0
    assert store.get("prime.gate.meta.condition_result") is True
    assert store.get("prime.gate.meta.branch") == "then"
    assert store.get("prime.gate.deep_review.value") == "echo:deep"
    assert store.get("prime.gate.quick_pass") is None

    # And the same expression against the same recorded score takes the other
    # branch when the threshold moves above it — the branch tracks the score
    # rather than the expression being trivially true.
    other = copy.deepcopy(orch)
    other["effects"][1]["if"]["expr"] = (
        f"state.prime.task.meta.complexity.score > {score + 1.0}"
    )
    flipped = _run(other, runtime_config=SCORING_ON)

    assert flipped.get("prime.gate.meta.branch") == "else"
    assert flipped.get("prime.gate.quick_pass.value") == "echo:quick"
    assert flipped.get("prime.gate.deep_review") is None


def test_config_block_reaches_the_runtime_through_a_real_run(
    tmp_path: Path,
) -> None:
    """The demo path end to end: `runtime.complexity` in a config file, a run
    through the CLI shim, a score on the state written to disk, and a
    conditional in the same orchestration branching on it.

    The unit tests above hand `PromptRuntime` a runtime dict directly; this one
    exists to prove that config actually arrives there.
    """
    orch = {
        "effects": [
            _prompt_orch()["effects"][0],
            {
                "type": "conditional",
                "name": "gate",
                "if": {
                    "mode": "cel",
                    "expr": "state.prime.task.meta.complexity.score > 5.0",
                },
                "then": [
                    {"type": "prompt", "name": "deep_review", "template": "deep"}
                ],
            },
        ]
    }
    orch_path = tmp_path / "orch.yml"
    orch_path.write_text(yaml.dump(orch, sort_keys=False), encoding="utf-8")

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=EchoAdapter(name="echo"),
            skip_preflight=True,
            config=CircuitryConfig(runtime=copy.deepcopy(SCORING_ON)),
        )
    )

    assert result.ok, result.error

    # Round-tripped through JSON: whatever is written here has to survive
    # serialization to a state file.
    written = json.loads(json.dumps(result.state))
    complexity = written["prime"]["task"]["meta"]["complexity"]
    assert complexity["score"] > 5.0
    assert complexity["signals"]["state_references"]["raw"] == 2.0
    assert written["prime"]["gate"]["meta"]["branch"] == "then"
