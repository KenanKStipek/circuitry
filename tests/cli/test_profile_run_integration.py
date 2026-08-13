from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run


def _write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def _write_orch(tmp_path: Path) -> Path:
    orch_path = tmp_path / "recipe.yml"
    _write(
        orch_path,
        """
effects:
  - type: prompt
    name: summarize
    template: "summarize {{topic}}"
  - type: dynamic
    name: sub
    effects:
      - type: prompt
        name: deep_analysis
        template: "analyze {{topic}}"
""".strip()
        + "\n",
    )
    return orch_path


@dataclass(frozen=True)
class RecordingAdapter:
    name: str
    calls: list = field(default_factory=list)

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        self.calls.append((model, prompt))
        return GenerateResult(text=f"{model}:{prompt}", raw={})


def test_run_with_profile_applies_overlay_inputs_and_records_redacted(
    tmp_path: Path,
) -> None:
    orch_path = _write_orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "fast.yml",
        """
inputs:
  topic: "widgets"
effects:
  summarize:
    model: tier-1
  sub.deep_analysis:
    model: tier-4
""".strip()
        + "\n",
    )

    adapter = RecordingAdapter(name="primary")
    req = RunRequest(
        orchestration_path=orch_path,
        state_path=None,
        out_path=None,
        dry_run=False,
        validate_only=False,
        config=CircuitryConfig(),
        adapter=adapter,
        profile_name="fast",
    )
    result = run(req)

    assert result.ok is True, result.error
    assert result.state["prime"]["summarize"]["value"] == "tier-1:summarize widgets"
    assert result.state["prime"]["summarize"]["meta"]["model"] == "tier-1"
    assert (
        result.state["prime"]["sub"]["deep_analysis"]["meta"]["model"] == "tier-4"
    )

    profile_record = result.state["runtime"]["effective_settings"]["profile"]
    assert profile_record["name"] == "fast"
    assert profile_record["content"]["inputs"] == {"topic": "widgets"}
    assert profile_record["content"]["effects"]["summarize"]["model"] == "tier-1"


def test_run_with_profile_cli_e_wins_over_profile_inputs(tmp_path: Path) -> None:
    orch_path = _write_orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "fast.yml",
        "inputs:\n  topic: \"widgets\"\n",
    )

    adapter = RecordingAdapter(name="primary")
    req = RunRequest(
        orchestration_path=orch_path,
        state_path=None,
        out_path=None,
        dry_run=False,
        validate_only=False,
        config=CircuitryConfig(),
        adapter=adapter,
        profile_name="fast",
        initial_state={"topic": "gadgets"},
    )
    result = run(req)

    assert result.ok is True, result.error
    assert result.state["prime"]["summarize"]["meta"]["prompt_sent"] == "summarize gadgets"


def test_run_with_unknown_profile_effect_path_fails_with_actionable_error(
    tmp_path: Path,
) -> None:
    orch_path = _write_orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "fast.yml",
        "effects:\n  does_not_exist:\n    model: tier-1\n",
    )

    req = RunRequest(
        orchestration_path=orch_path,
        state_path=None,
        out_path=None,
        dry_run=True,
        validate_only=False,
        config=CircuitryConfig(),
        profile_name="fast",
    )
    result = run(req)

    assert result.ok is False
    assert result.error is not None
    assert "does_not_exist" in result.error
    assert "summarize" in result.error
    assert "sub.deep_analysis" in result.error


def _write_reflector_orch(tmp_path: Path) -> Path:
    """The demo orchestration: agentic planning followed by a fixed effect."""
    orch_path = tmp_path / "recipe.yml"
    _write(
        orch_path,
        """
effects:
  - type: reflector
    name: planner
    effects:
      - type: prompt
        name: propose_steps
        template: "plan {{topic}}"
  - type: prompt
    name: report
    template: "report on <{{planner.value}}>"
""".strip()
        + "\n",
    )
    return orch_path


@dataclass(frozen=True)
class PlanningAdapter:
    """Returns a plan the reflector accepts as 'nothing left to do'."""

    name: str = "primary"
    calls: list = field(default_factory=list)

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        self.calls.append((model, prompt))
        return GenerateResult(text="done: true\neffects: []\n", raw={})


def _run_reflector_demo(orch_path: Path, profile_name: str | None) -> dict:
    adapter = PlanningAdapter()
    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            adapter=adapter,
            initial_state={"topic": "widgets"},
            profile_name=profile_name,
        )
    )
    assert result.ok is True, result.error
    return result.state


def test_run_with_profile_disabling_reflector_skips_planning(tmp_path: Path) -> None:
    """Demo: same orchestration with the reflector on vs off."""
    orch_path = _write_reflector_orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "no-planning.yml",
        "effects:\n  planner:\n    enabled: false\n",
    )

    on_state = _run_reflector_demo(orch_path, None)
    off_state = _run_reflector_demo(orch_path, "no-planning")

    # Reflector on: it planned, and the node carries no disabled marker.
    assert on_state["prime"]["planner"]["value"] is True
    assert "disabled" not in on_state["prime"]["planner"]["meta"]
    assert "inner" in on_state["prime"]["planner"]

    # Reflector off: skip node, no planning subtree at all.
    planner_off = off_state["prime"]["planner"]
    assert planner_off["value"] is None
    assert planner_off["meta"]["disabled"] is True
    assert set(planner_off["meta"]) == {"disabled", "created_at", "completed_at"}
    assert "inner" not in planner_off

    # Downstream stays coherent: the template renders the disabled node empty.
    assert off_state["prime"]["report"]["meta"]["prompt_sent"] == "report on <>"
    assert off_state["prime"]["report"]["value"] is not None


def test_run_without_profile_is_unaffected_by_profile_plumbing(tmp_path: Path) -> None:
    """Regression guard: identical RunRequests with/without profile_name=None
    must produce byte-identical state (minus timestamps/run_id)."""
    orch_path = _write_orch(tmp_path)

    def _run_once() -> dict:
        adapter = RecordingAdapter(name="primary")
        req = RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            adapter=adapter,
            initial_state={"topic": "widgets"},
        )
        result = run(req)
        assert result.ok is True, result.error
        return result.state

    state_default = _run_once()

    def _run_with_explicit_none() -> dict:
        adapter = RecordingAdapter(name="primary")
        req = RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            adapter=adapter,
            initial_state={"topic": "widgets"},
            profile_name=None,
        )
        result = run(req)
        assert result.ok is True, result.error
        return result.state

    state_explicit_none = _run_with_explicit_none()

    def _scrub(node: object) -> object:
        """Replace every timestamp-ish key anywhere in the tree.

        Every effect node carries its own meta timestamps — including the
        `dynamic` wrapper — so this walks rather than naming nodes.
        """
        if isinstance(node, dict):
            return {
                key: "T"
                if key in {"created_at", "completed_at", "started_at"}
                else "RID"
                if key == "run_id"
                else "PATH"
                if key == "orchestration_path"
                else _scrub(value)
                for key, value in node.items()
            }
        if isinstance(node, list):
            return [_scrub(item) for item in node]
        return node

    def _strip_volatile(state: dict) -> dict:
        clone = json.loads(json.dumps(state, default=str))
        clone.pop("_run_id", None)
        clone.pop("_timestamp", None)
        return _scrub(clone)  # type: ignore[return-value]

    assert "profile" not in state_default["runtime"]["effective_settings"]
    assert "profile" not in state_explicit_none["runtime"]["effective_settings"]
    assert _strip_volatile(state_default) == _strip_volatile(state_explicit_none)
