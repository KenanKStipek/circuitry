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

from typer.testing import CliRunner

from circuitry.adapters.base import GenerateResult
from circuitry.cli.app import _apply_inline_overrides, _parse_env_vars, app
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run

cli_runner = CliRunner()


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


def test_apply_inline_overrides_lands_under_input_of_namespaced_snapshot() -> None:
    """Issue #163: a --state file that is itself a namespaced snapshot (e.g.
    a prior --out) already has an `input` key, so `migrate_legacy_state`
    short-circuits on it. -e overrides must be merged into that existing
    `input` dict at the CLI merge site, or they'd stay stranded at the root,
    unreachable via `{{input.<key>}}`."""
    loaded = {"input": {"name": "Elena", "keep": "me"}, "prime": {}, "runtime": {}}

    merged = _apply_inline_overrides(loaded, {"name": "Priya"})

    assert merged["input"] == {"name": "Priya", "keep": "me"}
    assert "name" not in merged


def test_apply_inline_overrides_legacy_file_falls_through_to_root() -> None:
    """A --state file that is NOT yet namespaced (no `input` key) keeps its
    inline overrides at the root, exactly like its other bare keys, so the
    usual lift-on-hydrate wraps everything together."""
    loaded = {"name": "Elena"}

    merged = _apply_inline_overrides(loaded, {"count": 3})

    assert merged == {"name": "Elena", "count": 3}


def test_dash_e_overrides_input_key_in_namespaced_state_file(tmp_path: Path) -> None:
    """End-to-end regression for issue #163: `-e` combined with an
    already-namespaced --state file (e.g. a prior --out snapshot) must land
    the override under `input`, overriding any existing `input.<key>`."""
    orch_path = tmp_path / "orch.yml"
    _write_orch(orch_path)

    state_path = tmp_path / "state.json"
    state_path.write_text(
        json.dumps({"input": {"name": "Elena", "keep": "me"}}), encoding="utf-8"
    )

    inline = _parse_env_vars(["name=Priya"])
    initial_state = json.loads(state_path.read_text(encoding="utf-8"))
    initial_state = _apply_inline_overrides(initial_state, inline)

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
    assert result.state["input"] == {"name": "Priya", "keep": "me"}
    assert "name" not in result.state
    assert result.state["prime"]["greet"]["value"] == "hi Priya"


def test_cli_dash_e_overrides_input_key_of_a_resumed_out_snapshot(tmp_path: Path) -> None:
    """Full CLI regression for issue #163: `cof run <orch> --state
    <namespaced-snapshot> -e k=v` must land `k` at state["input"]["k"],
    overriding any existing input.k from the file — not stall at the root."""
    orch_path = tmp_path / "orch.yml"
    _write_orch(orch_path)

    first_out = tmp_path / "first.json"
    first = cli_runner.invoke(
        app,
        ["run", str(orch_path), "-e", "name=Elena", "--dry-run", "--out", str(first_out)],
    )
    assert first.exit_code == 0, first.stdout
    first_state = json.loads(first_out.read_text(encoding="utf-8"))
    assert first_state["input"] == {"name": "Elena"}

    second_out = tmp_path / "second.json"
    second = cli_runner.invoke(
        app,
        [
            "run",
            str(orch_path),
            "--state",
            str(first_out),
            "-e",
            "name=Priya",
            "--dry-run",
            "--out",
            str(second_out),
        ],
    )
    assert second.exit_code == 0, second.stdout
    second_state = json.loads(second_out.read_text(encoding="utf-8"))
    assert second_state["input"] == {"name": "Priya"}
    assert "name" not in second_state
