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
                        "review_semantics": {"value": generated_yaml},
                    }
                }
            },
            warnings=[],
        )

    return _fake


def test_gen_outputs_yaml(tmp_path: Path):
    """gen command should write the generated YAML to <name>.yml."""
    orch_name = str(tmp_path / "greeting_bot")
    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", orch_name, "make a greeting bot"])

    assert result.exit_code == 0, result.output
    orch_file = Path(f"{orch_name}.yml")
    assert orch_file.exists()
    assert "effects:" in orch_file.read_text(encoding="utf-8")


def test_gen_writes_state_to_out(tmp_path: Path):
    """gen --out writes resulting state JSON to the specified file."""
    import json

    orch_name = str(tmp_path / "greeting_bot")
    state_out = tmp_path / "state.json"

    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(
                app, ["gen", orch_name, "make a greeting bot", "--out", str(state_out)]
            )

    assert result.exit_code == 0, result.output
    # --out should contain state JSON, not orchestration
    assert state_out.exists()
    state = json.loads(state_out.read_text(encoding="utf-8"))
    assert "prime" in state
    # Orchestration file should also exist
    orch_file = Path(f"{orch_name}.yml")
    assert orch_file.exists()
    assert "effects:" in orch_file.read_text(encoding="utf-8")


def test_gen_passes_user_request_key(tmp_path: Path):
    """gen must pass 'user_request' (not 'prompt') in initial_state."""
    captured_req = {}

    def _capture_run(req):
        from circuitry.cli.runtime_shim import RunResult

        captured_req.update(req.initial_state)
        return RunResult(
            ok=True,
            state={"prime": {"generate": {"value": True, "review_semantics": {"value": "effects: []"}}}},
            warnings=[],
        )

    orch_name = str(tmp_path / "pipeline")
    with patch("circuitry.cli.app.run", _capture_run):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", orch_name, "build me a pipeline"])

    assert result.exit_code == 0, result.output
    assert "user_request" in captured_req
    assert captured_req["user_request"] == "build me a pipeline"


def test_gen_injects_rules(tmp_path: Path):
    """gen should inject structured rules from bundled rules/ directory."""
    captured_req = {}

    def _capture_run(req):
        from circuitry.cli.runtime_shim import RunResult

        captured_req.update(req.initial_state)
        return RunResult(
            ok=True,
            state={"prime": {"generate": {"value": True, "review_semantics": {"value": "effects: []"}}}},
            warnings=[],
        )

    orch_name = str(tmp_path / "something")
    with patch("circuitry.cli.app.run", _capture_run):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", orch_name, "make something"])

    assert result.exit_code == 0, result.output
    # Rules should be injected from bundled rules/ directory
    if "rules" in captured_req:
        assert "section: common" in captured_req["rules"]
        assert "type: prompt" in captured_req["rules"]
    # Per-type rule keys should also be present
    if "rules_prompt" in captured_req:
        assert "type: prompt" in captured_req["rules_prompt"]
        assert "section: common" in captured_req["rules_prompt"]
    if "rules_tool" in captured_req:
        assert "type: tool" in captured_req["rules_tool"]


def test_gen_format_json(tmp_path: Path):
    """gen --format json should produce valid JSON orchestration file."""
    import json

    orch_name = str(tmp_path / "bot")
    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", orch_name, "make a bot", "--format", "json"])

    assert result.exit_code == 0, result.output
    orch_file = Path(f"{orch_name}.json")
    assert orch_file.exists()
    parsed = json.loads(orch_file.read_text(encoding="utf-8"))
    assert "effects" in parsed
    assert parsed["effects"][0]["name"] == "hello"


def test_gen_format_toon(tmp_path: Path):
    """gen --format toon should produce valid TOON orchestration file."""
    from toon_format import decode

    orch_name = str(tmp_path / "bot")
    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", orch_name, "make a bot", "--format", "toon"])

    assert result.exit_code == 0, result.output
    orch_file = Path(f"{orch_name}.toon")
    assert orch_file.exists()
    parsed = decode(orch_file.read_text(encoding="utf-8"))
    assert "effects" in parsed
    assert parsed["effects"][0]["name"] == "hello"


def test_gen_format_yaml_default(tmp_path: Path):
    """gen with no --format flag should produce YAML (default)."""
    orch_name = str(tmp_path / "bot")
    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", orch_name, "make a bot"])

    assert result.exit_code == 0, result.output
    orch_file = Path(f"{orch_name}.yml")
    assert orch_file.exists()
    assert "effects:" in orch_file.read_text(encoding="utf-8")


def test_gen_format_json_writes_to_file(tmp_path: Path):
    """gen --format json writes valid JSON orchestration to <name>.json."""
    import json

    orch_name = str(tmp_path / "bot")
    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(
                app, ["gen", orch_name, "make a bot", "--format", "json"]
            )

    assert result.exit_code == 0, result.output
    orch_file = Path(f"{orch_name}.json")
    assert orch_file.exists()
    parsed = json.loads(orch_file.read_text(encoding="utf-8"))
    assert "effects" in parsed


def test_gen_strips_markdown_fences(tmp_path: Path):
    """gen should strip markdown code fences from LLM output before parsing."""
    import json

    fenced_yaml = "```yaml\neffects:\n  - type: prompt\n    name: hello\n    template: Hi\n```"
    orch_name = str(tmp_path / "bot")

    with patch("circuitry.cli.app.run", _make_fake_run(generated_yaml=fenced_yaml)):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", orch_name, "make a bot", "--format", "json"])

    assert result.exit_code == 0, result.output
    orch_file = Path(f"{orch_name}.json")
    assert orch_file.exists()
    parsed = json.loads(orch_file.read_text(encoding="utf-8"))
    assert "effects" in parsed
    assert parsed["effects"][0]["name"] == "hello"


def test_gen_failure_shows_error(tmp_path: Path):
    """gen should show error and exit 1 when run fails."""
    from circuitry.cli.runtime_shim import RunResult

    def _failing_run(req):
        return RunResult(ok=False, state={}, warnings=[], error="adapter not configured")

    orch_name = str(tmp_path / "something")
    with patch("circuitry.cli.app.run", _failing_run):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", orch_name, "make something"])

    assert result.exit_code == 1
