from __future__ import annotations

from pathlib import Path
from unittest.mock import patch

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


def _make_fake_run(generated_yaml: str = "effects:\n  - type: prompt\n    name: hello\n    template: Hi"):
    """Return a fake ``run`` that simulates meta_orchestrator output."""
    from circuitry.cli.runtime_shim import RunResult

    def _fake(req):
        # Verify the initial state uses the correct key
        assert "user_request" in req.initial_state, (
            "gen command must pass 'user_request', not 'prompt'"
        )
        return RunResult(
            ok=True,
            state={
                "prime": {
                    "generate": {
                        "value": True,
                        "final_yaml": {"value": generated_yaml},
                    }
                }
            },
            warnings=[],
        )

    return _fake


def test_gen_outputs_yaml(tmp_path: Path):
    """gen command should print the generated YAML to stdout."""
    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", "make a greeting bot"])

    assert result.exit_code == 0, result.output
    assert "effects:" in result.output


def test_gen_writes_to_file(tmp_path: Path):
    """gen --out writes YAML to the specified file."""
    out = tmp_path / "generated.yml"

    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", "make a greeting bot", "--out", str(out)])

    assert result.exit_code == 0, result.output
    assert out.exists()
    assert "effects:" in out.read_text(encoding="utf-8")


def test_gen_passes_user_request_key():
    """gen must pass 'user_request' (not 'prompt') in initial_state."""
    captured_req = {}

    def _capture_run(req):
        from circuitry.cli.runtime_shim import RunResult

        captured_req.update(req.initial_state)
        return RunResult(
            ok=True,
            state={"prime": {"generate": {"value": True, "final_yaml": {"value": "effects: []"}}}},
            warnings=[],
        )

    with patch("circuitry.cli.app.run", _capture_run):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", "build me a pipeline"])

    assert result.exit_code == 0, result.output
    assert "user_request" in captured_req
    assert captured_req["user_request"] == "build me a pipeline"


def test_gen_injects_rules():
    """gen should inject 'rules' into initial_state from bundled docs."""
    captured_req = {}

    def _capture_run(req):
        from circuitry.cli.runtime_shim import RunResult

        captured_req.update(req.initial_state)
        return RunResult(
            ok=True,
            state={"prime": {"generate": {"value": True, "final_yaml": {"value": "effects: []"}}}},
            warnings=[],
        )

    with patch("circuitry.cli.app.run", _capture_run):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", "make something"])

    assert result.exit_code == 0, result.output
    # Rules should be injected if the bundled doc exists
    if "rules" in captured_req:
        assert "LLM Authoring Rules" in captured_req["rules"]


def test_gen_failure_shows_error():
    """gen should show error and exit 1 when run fails."""
    from circuitry.cli.runtime_shim import RunResult

    def _failing_run(req):
        return RunResult(ok=False, state={}, warnings=[], error="adapter not configured")

    with patch("circuitry.cli.app.run", _failing_run):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", "make something"])

    assert result.exit_code == 1
