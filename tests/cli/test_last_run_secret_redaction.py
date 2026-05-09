"""Verify last-run.json never persists raw secret values, and that --last
refuses to silently replay redacted runs.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from circuitry.cli import app as app_module
from circuitry.cli.app import app
from circuitry.cli.redaction import REDACTED, redact_env_pairs


def _redirect_global_config(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> Path:
    """Redirect ~/.config/circuitry to a tmp dir so tests don't touch real state."""
    fake_dir = tmp_path / "config-home"
    fake_dir.mkdir(parents=True, exist_ok=True)
    fake_last_run = fake_dir / "last-run.json"
    monkeypatch.setattr(app_module, "GLOBAL_CONFIG_DIR", fake_dir)
    monkeypatch.setattr(app_module, "_LAST_RUN_PATH", fake_last_run)
    return fake_last_run


def _write_noop(tmp_path: Path) -> Path:
    """A single prompt that --dry-run skips. Needs an adapter to resolve, so
    we always pair it with `_write_config()`."""
    orch = tmp_path / "noop.yml"
    orch.write_text(
        """
effects:
  - type: prompt
    name: greet
    template: "Hello, {{name}}."
""".strip()
        + "\n",
        encoding="utf-8",
    )
    return orch


def _write_config(tmp_path: Path) -> Path:
    """An ollama config that --dry-run never actually contacts."""
    cfg = tmp_path / "config.json"
    cfg.write_text(
        json.dumps(
            {
                "default_adapter": "ollama",
                "default_model": "llama3.1:8b",
                "runtime": {
                    "adapters": {
                        "ollama": {"base_url": "http://localhost:11434"}
                    }
                },
            }
        ),
        encoding="utf-8",
    )
    return cfg


def test_secret_env_var_is_redacted_in_last_run(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    last_run_path = _redirect_global_config(monkeypatch, tmp_path)
    orch_path = _write_noop(tmp_path)
    cfg_path = _write_config(tmp_path)

    runner = CliRunner()
    result = runner.invoke(
        app,
        [
            "run",
            str(orch_path),
            "--config",
            str(cfg_path),
            "-e",
            "name=World",
            "-e",
            "OPENAI_API_KEY=sk-test-1234567890abcdef1234567890abcdef",
            "--dry-run",
            "--quiet",
        ],
    )
    assert result.exit_code == 0, result.output

    payload = json.loads(last_run_path.read_text(encoding="utf-8"))
    env_vars = payload["env_vars"] or []
    name_pair = next(p for p in env_vars if p.startswith("name="))
    secret_pair = next(p for p in env_vars if p.startswith("OPENAI_API_KEY="))

    assert name_pair == "name=World"
    assert secret_pair == f"OPENAI_API_KEY={REDACTED}"
    # The raw secret never reaches disk.
    assert "sk-test-1234567890abcdef1234567890abcdef" not in last_run_path.read_text(
        encoding="utf-8"
    )


def test_non_secret_env_var_passes_through(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    last_run_path = _redirect_global_config(monkeypatch, tmp_path)
    orch_path = _write_noop(tmp_path)
    cfg_path = _write_config(tmp_path)

    runner = CliRunner()
    result = runner.invoke(
        app,
        [
            "run",
            str(orch_path),
            "--config",
            str(cfg_path),
            "-e",
            "name=World",
            "-e",
            "topic=cats",
            "--dry-run",
            "--quiet",
        ],
    )
    assert result.exit_code == 0, result.output

    payload = json.loads(last_run_path.read_text(encoding="utf-8"))
    env_vars = payload["env_vars"] or []
    assert "name=World" in env_vars
    assert "topic=cats" in env_vars


def test_last_replay_aborts_when_redacted_secret_present(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    last_run_path = _redirect_global_config(monkeypatch, tmp_path)
    orch_path = _write_noop(tmp_path)

    last_run_path.write_text(
        json.dumps(
            {
                "orchestration": str(orch_path),
                "config": None,
                "state": None,
                "out": None,
                "pretty": False,
                "print_state": False,
                "dry_run": True,
                "json_out": False,
                "quiet": True,
                "verbose": False,
                "live_state": None,
                "env_vars": ["name=World", f"OPENAI_API_KEY={REDACTED}"],
                "tail": False,
            }
        )
        + "\n",
        encoding="utf-8",
    )

    runner = CliRunner()
    result = runner.invoke(app, ["run", "--last"])
    assert result.exit_code == 1
    assert "redacted secrets" in result.output


def test_redact_env_pairs_helper_round_trips() -> None:
    pairs = [
        "name=World",
        "OPENAI_API_KEY=sk-test-1234567890abcdef1234567890abcdef",
        "ANTHROPIC_API_KEY=sk-ant-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "auth_token=abc.def.ghi",
        "password=hunter2hunter2hunter2hunter2hunter2",
        "topic=cats",
        "MY_TOKEN=plain",
        "shaped_value=sk-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ]
    redacted = redact_env_pairs(pairs)
    assert redacted is not None
    assert "name=World" in redacted
    assert "topic=cats" in redacted
    assert f"OPENAI_API_KEY={REDACTED}" in redacted
    assert f"ANTHROPIC_API_KEY={REDACTED}" in redacted
    assert f"auth_token={REDACTED}" in redacted
    assert f"password={REDACTED}" in redacted
    # Suffix-matched secret name
    assert f"MY_TOKEN={REDACTED}" in redacted
    # Value shaped like an API key, redacted even though the key isn't sensitive.
    assert f"shaped_value={REDACTED}" in redacted


def test_redact_env_pairs_passes_none_through() -> None:
    assert redact_env_pairs(None) is None
    assert redact_env_pairs([]) == []
