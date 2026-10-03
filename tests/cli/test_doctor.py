"""CLI-level tests for ``cof doctor`` — independent of the TUI wrapper.

``tests/tui/test_doctor_view.py`` covers the Textual screen against a fake
``DiagnosticsSource``; these drive the standalone ``doctor`` Typer command
body itself (``cli/doctor.py``), including the ``--generate`` smoke call and
the adapter/tool/runtime-plugin ``check()`` rendering in ``_check_extensions``.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
import typer.testing

from circuitry.adapters.base import GenerateResult
from circuitry.cli.app import app
from circuitry.cli.detect import BackendStatus, DetectionResult

runner = typer.testing.CliRunner()


def _write_config(tmp_path: Path, **overrides: object) -> Path:
    config = {"default_adapter": "ollama", "default_model": "llama3:latest", **overrides}
    path = tmp_path / "config.json"
    path.write_text(json.dumps(config), encoding="utf-8")
    return path


def _lockdown_env(monkeypatch: pytest.MonkeyPatch, adapters: str = "") -> None:
    """Keep the extension-check tables small and offline for a fast test."""
    monkeypatch.setenv("CIRCUITRY_ENABLED_ADAPTERS", adapters)
    monkeypatch.setenv("CIRCUITRY_ENABLED_TOOLS", "")
    monkeypatch.setenv("CIRCUITRY_ENABLED_PLUGINS", "")


def test_doctor_reports_effective_adapter_and_model(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _lockdown_env(monkeypatch)
    config = _write_config(tmp_path)

    result = runner.invoke(app, ["doctor", "--config", str(config)])

    assert result.exit_code == 0
    assert "ollama" in result.output
    assert "llama3:latest" in result.output


def test_doctor_with_orch_option_loads_orchestration(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _lockdown_env(monkeypatch)
    config = _write_config(tmp_path)
    orch = tmp_path / "orch.yml"
    orch.write_text(
        "adapter: ollama\nmodel: llama3:latest\n"
        "effects:\n  - {type: prompt, name: g, template: x}\n",
        encoding="utf-8",
    )

    result = runner.invoke(
        app, ["doctor", "--config", str(config), "--orch", str(orch)]
    )

    assert result.exit_code == 0


def test_doctor_model_present_yes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _lockdown_env(monkeypatch)
    config = _write_config(tmp_path)

    fake = DetectionResult(
        backends=[
            BackendStatus(
                name="ollama",
                available=True,
                detail="http://localhost:11434 (1 models)",
                models=["llama3:latest"],
            )
        ]
    )
    monkeypatch.setattr("circuitry.cli.doctor.detect_all", lambda **_: fake)

    result = runner.invoke(app, ["doctor", "--config", str(config)])

    assert result.exit_code == 0
    assert "Model present" in result.output
    assert "YES" in result.output


def test_doctor_model_present_no(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _lockdown_env(monkeypatch)
    config = _write_config(tmp_path)

    fake = DetectionResult(
        backends=[
            BackendStatus(
                name="ollama",
                available=True,
                detail="http://localhost:11434 (0 models)",
                models=["some-other-model"],
            )
        ]
    )
    monkeypatch.setattr("circuitry.cli.doctor.detect_all", lambda **_: fake)

    result = runner.invoke(app, ["doctor", "--config", str(config)])

    assert result.exit_code == 0
    assert "NO (missing: llama3:latest)" in result.output


def test_doctor_never_shows_key_characters(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """#350: a canary key's characters never appear in `cof doctor` output."""
    _lockdown_env(monkeypatch)
    config = _write_config(tmp_path)
    canary = "sk-CANARYvalueNeverShown1234567890"
    monkeypatch.setenv("OPENAI_API_KEY", canary)
    monkeypatch.setenv("ANTHROPIC_API_KEY", canary)

    result = runner.invoke(app, ["doctor", "--config", str(config)])

    assert result.exit_code == 0
    assert canary not in result.output
    for i in range(len(canary) - 5):
        assert canary[i : i + 6] not in result.output


def test_doctor_generate_flag_success(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _lockdown_env(monkeypatch)
    config = _write_config(tmp_path)

    class FakeAdapter:
        def generate(self, *, model: str, prompt: str, timeout_seconds: int) -> GenerateResult:
            return GenerateResult(text="hello there", raw={})

    monkeypatch.setattr("circuitry.cli.doctor.build_adapter", lambda **_: FakeAdapter())

    result = runner.invoke(app, ["doctor", "--config", str(config), "--generate"])

    assert result.exit_code == 0
    assert "Generate test" in result.output
    assert "OK (hello there)" in result.output


def test_doctor_generate_flag_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _lockdown_env(monkeypatch)
    config = _write_config(tmp_path)

    class FakeAdapter:
        def generate(self, *, model: str, prompt: str, timeout_seconds: int) -> GenerateResult:
            raise RuntimeError("connection refused")

    monkeypatch.setattr("circuitry.cli.doctor.build_adapter", lambda **_: FakeAdapter())

    result = runner.invoke(app, ["doctor", "--config", str(config), "--generate"])

    assert result.exit_code == 1
    assert "FAIL (connection refused)" in result.output


def test_doctor_generate_flag_no_model_resolved(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _lockdown_env(monkeypatch)
    config = _write_config(tmp_path, default_model=None)

    class FakeAdapter:
        def generate(self, *, model: str, prompt: str, timeout_seconds: int) -> GenerateResult:
            raise AssertionError("should not be reached when no model is resolved")

    monkeypatch.setattr("circuitry.cli.doctor.build_adapter", lambda **_: FakeAdapter())

    result = runner.invoke(app, ["doctor", "--config", str(config), "--generate"])

    assert result.exit_code == 1
    assert "No model resolved" in result.output


def test_check_extensions_reports_unknown_adapter(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _lockdown_env(monkeypatch, adapters="nosuchadapter")
    config = _write_config(tmp_path)

    result = runner.invoke(app, ["doctor", "--config", str(config)])

    assert result.exit_code == 1
    assert "nosuchadapter" in result.output
    assert "unknown" in result.output


def test_check_extensions_reports_deferred_adapter(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """``host_claude`` can't be built from config — reported deferred, not failed."""
    _lockdown_env(monkeypatch, adapters="host_claude")
    config = _write_config(tmp_path)

    result = runner.invoke(app, ["doctor", "--config", str(config)])

    assert result.exit_code == 0
    assert "deferred" in result.output


def test_check_extensions_reports_tool_plugin_table(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("CIRCUITRY_ENABLED_ADAPTERS", "")
    monkeypatch.setenv("CIRCUITRY_ENABLED_TOOLS", "json")
    monkeypatch.setenv("CIRCUITRY_ENABLED_PLUGINS", "")
    config = _write_config(tmp_path)

    result = runner.invoke(app, ["doctor", "--config", str(config)])

    assert result.exit_code == 0
    assert "Tool plugins" in result.output
    assert "json" in result.output


def test_check_extensions_reports_unknown_tool(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("CIRCUITRY_ENABLED_ADAPTERS", "")
    monkeypatch.setenv("CIRCUITRY_ENABLED_TOOLS", "nosuchtool")
    monkeypatch.setenv("CIRCUITRY_ENABLED_PLUGINS", "")
    config = _write_config(tmp_path)

    result = runner.invoke(app, ["doctor", "--config", str(config)])

    assert result.exit_code == 1
    assert "nosuchtool" in result.output
    assert "unknown" in result.output


def test_check_extensions_reports_runtime_plugin_ok(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("CIRCUITRY_ENABLED_ADAPTERS", "")
    monkeypatch.setenv("CIRCUITRY_ENABLED_TOOLS", "")
    monkeypatch.delenv("CIRCUITRY_ENABLED_PLUGINS", raising=False)
    config = _write_config(
        tmp_path, plugins=["plugin_fixtures:make_recording_plugin"]
    )

    result = runner.invoke(app, ["doctor", "--config", str(config)])

    assert result.exit_code == 0
    assert "Runtime plugins" in result.output
    assert "recording-plugin" in result.output


def test_check_extensions_reports_runtime_plugin_load_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("CIRCUITRY_ENABLED_ADAPTERS", "")
    monkeypatch.setenv("CIRCUITRY_ENABLED_TOOLS", "")
    monkeypatch.delenv("CIRCUITRY_ENABLED_PLUGINS", raising=False)
    config = _write_config(tmp_path, plugins=["plugin_fixtures:does_not_exist"])

    result = runner.invoke(app, ["doctor", "--config", str(config)])

    assert result.exit_code == 1
    assert "load failed" in result.output
