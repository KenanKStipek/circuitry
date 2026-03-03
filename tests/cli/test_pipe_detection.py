from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import patch

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


def _write(tmp_path: Path, name: str, content: str) -> Path:
    path = tmp_path / name
    path.write_text(content, encoding="utf-8")
    return path


SIMPLE_ORCH = """\
effects:
  - type: prompt
    name: greet
    template: "Hello"
"""


def _make_fake_run(ok: bool = True, state: dict | None = None):
    """Return a fake ``run`` function that returns a mock RunResult."""
    from circuitry.cli.runtime_shim import RunResult

    def _fake(req):
        return RunResult(
            ok=ok,
            state=state or {"prime": {"greet": {"value": "hi"}}},
            warnings=[],
        )

    return _fake


def test_pipe_detection_produces_json(tmp_path: Path):
    """When stdout is not a tty (piped) and no --out, run should output state as JSON."""
    orch = _write(tmp_path, "orch.yml", SIMPLE_ORCH)

    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            # CliRunner has non-tty stdout, so auto-pipe triggers json mode.
            # Without --out, json mode prints state to stdout.
            result = runner.invoke(app, ["run", str(orch)])

    assert result.exit_code == 0, result.output
    # Output should be valid JSON state (auto-pipe triggers --json mode)
    payload = json.loads(result.output.strip())
    assert "prime" in payload


def test_tail_wins_over_pipe_detection(tmp_path: Path):
    """--tail should produce raw value even when piped (non-tty)."""
    orch = _write(tmp_path, "orch.yml", SIMPLE_ORCH)

    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            result = runner.invoke(app, ["run", str(orch), "--tail"])

    assert result.exit_code == 0, result.output
    # --tail should output the raw value, not a JSON envelope
    assert "hi" in result.output
    # Verify it's NOT a JSON envelope (no "ok" key)
    try:
        parsed = json.loads(result.output.strip())
        assert "ok" not in parsed  # Should not be an envelope
    except json.JSONDecodeError:
        pass  # Not JSON at all, which is fine for --tail with header text


def test_explicit_json_flag_outputs_json(tmp_path: Path):
    """Explicit --json flag without --out should print state as JSON."""
    orch = _write(tmp_path, "orch.yml", SIMPLE_ORCH)

    with patch("circuitry.cli.app.run", _make_fake_run()):
        with patch("circuitry.cli.app.resolve_config") as mock_cfg:
            from circuitry.cli.config import CircuitryConfig

            mock_cfg.return_value = CircuitryConfig()
            # --json without --out prints state to stdout
            result = runner.invoke(app, ["run", str(orch), "--json"])

    assert result.exit_code == 0, result.output
    payload = json.loads(result.output.strip())
    assert "prime" in payload
