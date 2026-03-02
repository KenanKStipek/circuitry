from __future__ import annotations

from pathlib import Path

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run


def _write(tmp_path: Path, name: str, content: str) -> Path:
    path = tmp_path / name
    path.write_text(content, encoding="utf-8")
    return path


def test_plugin_lifecycle_hooks_execute_and_are_observable(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "orch.yml",
        """
adapter: openai
model: gpt-4o-mini
effects:
  - type: prompt
    name: greet
    template: "hello"
""".strip()
        + "\n",
    )

    cfg = CircuitryConfig(
        plugins=["plugin_fixtures:make_recording_plugin"]
    )
    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=False,
            initial_state={},
            config=cfg,
        )
    )

    assert result.ok is True
    marker = result.state["runtime"]["plugin_marker"]
    assert marker["started"] is True
    assert marker["succeeded"] is True

    plugin_meta = result.state["runtime"]["plugins"]
    assert plugin_meta["contract_version"] == "1"
    hooks = [e["hook"] for e in plugin_meta["events"] if e["ok"] is True]
    assert "load" in hooks
    assert "on_run_start" in hooks
    assert "on_run_success" in hooks


def test_plugin_failures_are_isolated_and_visible_in_metadata(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "orch.yml",
        """
adapter: openai
model: gpt-4o-mini
effects:
  - type: prompt
    name: greet
    template: "hello"
""".strip()
        + "\n",
    )

    cfg = CircuitryConfig(plugins=["plugin_fixtures:make_failing_plugin"])
    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=False,
            initial_state={},
            config=cfg,
        )
    )

    assert result.ok is True
    plugin_meta = result.state["runtime"]["plugins"]
    failed_events = [e for e in plugin_meta["events"] if e["ok"] is False]
    assert any(e["hook"] == "on_run_start" for e in failed_events)
    assert any(e["hook"] == "on_run_success" for e in failed_events)


def test_plugin_load_failure_is_reported_and_run_continues(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "orch.yml",
        """
adapter: openai
model: gpt-4o-mini
effects:
  - type: prompt
    name: greet
    template: "hello"
""".strip()
        + "\n",
    )

    cfg = CircuitryConfig(plugins=["plugin_fixtures:does_not_exist"])
    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=False,
            initial_state={},
            config=cfg,
        )
    )

    assert result.ok is True
    plugin_meta = result.state["runtime"]["plugins"]
    load_failures = [
        e for e in plugin_meta["events"] if e["hook"] == "load" and e["ok"] is False
    ]
    assert len(load_failures) == 1
    assert load_failures[0]["error"]


def test_plugin_on_failure_hook_receives_runtime_error(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "bad.yml",
        """
effects:
  - type: prompt
    name: dup
    template: "a"
  - type: prompt
    name: dup
    template: "b"
""".strip()
        + "\n",
    )

    cfg = CircuitryConfig(
        plugins=["plugin_fixtures:make_recording_plugin"]
    )
    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=False,
            initial_state={},
            config=cfg,
        )
    )

    assert result.ok is False
    marker = result.state["runtime"]["plugin_marker"]
    assert marker["failed"] is True
    assert "Duplicate effect name 'dup'" in marker["error"]
