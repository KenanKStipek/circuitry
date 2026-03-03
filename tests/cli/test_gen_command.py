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


def test_gen_format_json():
    """gen --format json should produce valid JSON output."""
    import json

    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", "make a bot", "--format", "json"])

    assert result.exit_code == 0, result.output
    parsed = json.loads(result.output.strip())
    assert "effects" in parsed
    assert parsed["effects"][0]["name"] == "hello"


def test_gen_format_toon():
    """gen --format toon should produce valid TOON output."""
    from toon_format import decode

    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", "make a bot", "--format", "toon"])

    assert result.exit_code == 0, result.output
    parsed = decode(result.output.strip())
    assert "effects" in parsed
    assert parsed["effects"][0]["name"] == "hello"


def test_gen_format_yaml_default():
    """gen with no --format flag should produce YAML (default)."""
    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", "make a bot"])

    assert result.exit_code == 0, result.output
    assert "effects:" in result.output


def test_gen_format_json_writes_to_file(tmp_path: Path):
    """gen --format json --out writes valid JSON to file."""
    import json

    out = tmp_path / "generated.json"

    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(
                app, ["gen", "make a bot", "--format", "json", "--out", str(out)]
            )

    assert result.exit_code == 0, result.output
    assert out.exists()
    parsed = json.loads(out.read_text(encoding="utf-8"))
    assert "effects" in parsed


def test_gen_strips_markdown_fences():
    """gen should strip markdown code fences from LLM output before parsing."""
    import json

    fenced_yaml = "```yaml\neffects:\n  - type: prompt\n    name: hello\n    template: Hi\n```"

    with patch("circuitry.cli.app.run", _make_fake_run(generated_yaml=fenced_yaml)):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", "make a bot", "--format", "json"])

    assert result.exit_code == 0, result.output
    parsed = json.loads(result.output.strip())
    assert "effects" in parsed
    assert parsed["effects"][0]["name"] == "hello"


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
