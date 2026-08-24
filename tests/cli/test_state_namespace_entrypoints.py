"""Every caller-state entry point wraps bare root keys under `input` and
nowhere else — the CLI half of the #86 stage-1 contract. `-e` KEY=VALUE and
a `--state` JSON file both flow through RunRequest.initial_state /
state_path into the single choke point (runtime_shim._load_state ->
migrate_legacy_state); this pins that each source actually reaches it.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path

import pytest

pytest.importorskip("typer")

from circuitry.adapters.base import GenerateResult
from circuitry.cli.app import _parse_env_vars
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run


def _write_orch(path: Path) -> None:
    path.write_text(
        "effects:\n"
        "  - type: prompt\n"
        "    name: greet\n"
        '    template: "hi {{input.name}}"\n',
        encoding="utf-8",
    )


@dataclass(frozen=True)
class RecordingAdapter:
    name: str = "primary"
    calls: list = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append(prompt)
        return GenerateResult(text=prompt, raw={})


def test_dash_e_bare_key_lands_under_input(tmp_path: Path) -> None:
    orch_path = tmp_path / "orch.yml"
    _write_orch(orch_path)

    # Mirrors cli.app.run_cmd: no --state file, so `-e` becomes initial_state
    # verbatim (bare, unwrapped) — the run's own choke point wraps it.
    initial_state = _parse_env_vars(["name=World"])
    assert initial_state == {"name": "World"}

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state=initial_state,
            config=CircuitryConfig(),
            adapter=RecordingAdapter(),
        )
    )

    assert result.ok is True, result.error
    assert result.state["input"] == {"name": "World"}
    assert "name" not in result.state
    assert result.state["prime"]["greet"]["value"] == "hi World"


def test_state_file_bare_keys_land_under_input(tmp_path: Path) -> None:
    orch_path = tmp_path / "orch.yml"
    _write_orch(orch_path)

    state_path = tmp_path / "state.json"
    state_path.write_text(json.dumps({"name": "Elena"}), encoding="utf-8")

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=state_path,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state=None,
            config=CircuitryConfig(),
            adapter=RecordingAdapter(),
        )
    )

    assert result.ok is True, result.error
    assert result.state["input"] == {"name": "Elena"}
    assert "name" not in result.state
    assert result.state["prime"]["greet"]["value"] == "hi Elena"


def test_state_file_already_namespaced_is_left_untouched(tmp_path: Path) -> None:
    """A --state file that is itself a prior --out snapshot (already
    input/prime/runtime-shaped) is trusted as-is — lift-on-hydrate is a
    no-op once `input` is present."""
    orch_path = tmp_path / "orch.yml"
    _write_orch(orch_path)

    state_path = tmp_path / "state.json"
    state_path.write_text(json.dumps({"input": {"name": "Priya"}}), encoding="utf-8")

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=state_path,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state=None,
            config=CircuitryConfig(),
            adapter=RecordingAdapter(),
        )
    )

    assert result.ok is True, result.error
    assert result.state["input"] == {"name": "Priya"}
    assert result.state["prime"]["greet"]["value"] == "hi Priya"
