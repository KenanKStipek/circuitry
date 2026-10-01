from __future__ import annotations

from pathlib import Path
from unittest.mock import patch

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


def _make_fake_run(
    generated_yaml: str = "effects:\n  - type: prompt\n    name: hello\n    template: Hi",
    warnings: list[str] | None = None,
):
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
            warnings=warnings or [],
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


def test_gen_prints_the_run_warnings(tmp_path: Path) -> None:
    """cof gen must surface result.warnings — e.g. a skipped project config —
    not swallow them (#278)."""
    orch_name = str(tmp_path / "greeting_bot")
    warning = "Skipped project config /tmp/circuitry.config.json: it is not trusted."

    with patch("circuitry.cli.app.run", _make_fake_run(warnings=[warning])):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", orch_name, "make a greeting bot"])

    assert result.exit_code == 0, result.output
    assert warning in result.stderr


def test_gen_out_writes_the_orchestration_document(tmp_path: Path):
    """gen --out writes the generated orchestration itself (#258), not a
    side artifact — matching `wizard --out`."""
    orch_name = str(tmp_path / "greeting_bot")
    out_path = tmp_path / "custom_name.yml"

    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(
                app, ["gen", orch_name, "make a greeting bot", "--out", str(out_path)]
            )

    assert result.exit_code == 0, result.output
    assert out_path.exists()
    assert "effects:" in out_path.read_text(encoding="utf-8")
    # The default <name>.yml path is NOT also written when --out redirects.
    assert not Path(f"{orch_name}.yml").exists()


def test_gen_live_state_flag_reaches_run_request(tmp_path: Path):
    """gen --live-state is passed through as RunRequest.live_state_path,
    decoupled from --out (#258 part 1)."""
    from circuitry.cli.runtime_shim import RunResult

    captured = {}
    orch_name = str(tmp_path / "greeting_bot")
    live_state_path = tmp_path / "live.json"

    def _capture_run(req):
        captured["live_state_path"] = req.live_state_path
        return RunResult(
            ok=True,
            state={
                "prime": {
                    "generate": {"value": True, "review_semantics": {"value": "effects: []"}}
                }
            },
            warnings=[],
        )

    with patch("circuitry.cli.app.run", _capture_run):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(
                app,
                ["gen", orch_name, "make a greeting bot", "--live-state", str(live_state_path)],
            )

    assert result.exit_code == 0, result.output
    assert captured["live_state_path"] == live_state_path
    orch_file = Path(f"{orch_name}.yml")
    assert orch_file.exists()


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
    decode = pytest.importorskip(
        "toon_format", reason="toon-format optional extra not installed"
    ).decode

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


def test_gen_handles_hash_commented_preamble(tmp_path: Path):
    """A model echoing the bundled examples' own `# effects:`-commented
    header (#258 part 1) must still parse — the boundary scan used to only
    match an uncommented `effects:`/`adapter:` line at column 0."""
    commented_yaml = (
        "# effects:\n"
        "#   - type: prompt\n"
        "#     name: fetch\n"
        "effects:\n"
        "  - type: prompt\n"
        "    name: hello\n"
        "    template: Hi\n"
    )
    orch_name = str(tmp_path / "bot")

    with patch("circuitry.cli.app.run", _make_fake_run(generated_yaml=commented_yaml)):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", orch_name, "make a bot"])

    assert result.exit_code == 0, result.output
    orch_file = Path(f"{orch_name}.yml")
    assert orch_file.exists()
    content = orch_file.read_text(encoding="utf-8")
    assert "raw:" not in content
    assert "name: hello" in content


def test_gen_rejects_unparseable_output_and_writes_nothing(tmp_path: Path):
    """When the model's response has no recoverable YAML document, gen must
    error out and must not fall back to writing `{raw: ...}` (#258 part 1)."""
    garbage = "I can't help with that right now, sorry!"
    orch_name = str(tmp_path / "bot")

    with patch("circuitry.cli.app.run", _make_fake_run(generated_yaml=garbage)):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", orch_name, "make a bot"])

    assert result.exit_code == 1
    assert not Path(f"{orch_name}.yml").exists()


def test_gen_rejects_structurally_invalid_output_and_writes_nothing(tmp_path: Path):
    """A document that parses as YAML but fails schema/structural checks
    (missing required `effects`) must fail the same way `cof check` would,
    and nothing should land on disk (#258 part 1)."""
    invalid_yaml = "adapter: ollama\nname: not_an_orchestration\n"
    orch_name = str(tmp_path / "bot")

    with patch("circuitry.cli.app.run", _make_fake_run(generated_yaml=invalid_yaml)):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["gen", orch_name, "make a bot"])

    assert result.exit_code == 1
    assert "failed" in result.output.lower() or "error" in result.output.lower()
    assert not Path(f"{orch_name}.yml").exists()
    assert not Path(f"{orch_name}.yml.tmp").exists()
