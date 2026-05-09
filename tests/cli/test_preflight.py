"""Tests for Story 1 — preflight dependency checks."""

from __future__ import annotations

import shutil
from pathlib import Path

import pytest
import typer.testing

from circuitry.cli.app import app
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import (
    RunRequest,
    format_preflight_errors,
    preflight,
    run,
    validate,
)
from circuitry.preflight import CheckResult, call_check


def _write(tmp_path: Path, name: str, content: str) -> Path:
    p = tmp_path / name
    p.write_text(content, encoding="utf-8")
    return p


# ---------- CheckResult shape ----------


def test_check_result_default_is_ok() -> None:
    """Required for AC 1.7 — extensions w/o explicit check() should default ok."""

    class StubAdapter:
        pass

    r = call_check(StubAdapter())
    assert r.ok is True
    assert r.missing == []


def test_check_result_handles_exceptions() -> None:
    class Exploder:
        def check(self) -> CheckResult:
            raise RuntimeError("boom")

    r = call_check(Exploder())
    assert r.ok is False
    assert "boom" in (r.message or "")


def test_check_result_rejects_wrong_return_type() -> None:
    class Liar:
        def check(self):
            return "ok"

    r = call_check(Liar())
    assert r.ok is False
    assert "expected CheckResult" in (r.message or "")


# ---------- adapter check() impls ----------


def test_openai_adapter_reports_missing_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """AC 1.1: env var unset → CheckResult with env:VAR_NAME marker."""
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    from circuitry.adapters.openai import OpenAIAdapter

    r = OpenAIAdapter().check()
    assert r.ok is False
    assert "env:OPENAI_API_KEY" in r.missing


def test_openai_adapter_ok_with_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("OPENAI_API_KEY", "sk-test")
    if shutil.which("curl") is None:
        pytest.skip("curl not available")
    from circuitry.adapters.openai import OpenAIAdapter

    r = OpenAIAdapter().check()
    assert r.ok is True
    assert r.missing == []


def test_anthropic_adapter_reports_missing_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    from circuitry.adapters.anthropic import AnthropicAdapter

    r = AnthropicAdapter().check()
    assert r.ok is False
    assert "env:ANTHROPIC_API_KEY" in r.missing


def test_litellm_adapter_reports_missing_library(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """AC 1.1 / library variant."""
    import importlib.util

    real = importlib.util.find_spec

    def fake_find_spec(name: str, *args, **kwargs):
        if name == "litellm":
            return None
        return real(name, *args, **kwargs)

    monkeypatch.setattr("importlib.util.find_spec", fake_find_spec)
    from circuitry.adapters.litellm import LiteLLMAdapter

    r = LiteLLMAdapter().check()
    assert r.ok is False
    assert "library:litellm" in r.missing


def test_ollama_adapter_reports_unreachable_host() -> None:
    """AC 1.3: unreachable endpoint → CheckResult with host:url marker."""
    from circuitry.adapters.ollama import OllamaAdapter

    # Port 9 is the discard service — actively refuses connections in most envs.
    r = OllamaAdapter(base_url="http://127.0.0.1:9").check()
    assert r.ok is False
    assert any(m.startswith("host:http://127.0.0.1:9") for m in r.missing)


# ---------- tool plugin check() impls ----------


def test_ffmpeg_plugin_reports_missing_binary(monkeypatch: pytest.MonkeyPatch) -> None:
    """AC 1.2: binary missing → CheckResult with binary:name marker."""
    monkeypatch.setattr("shutil.which", lambda _name: None)
    from circuitry.plugins.ffmpeg import FfmpegPlugin

    r = FfmpegPlugin().check()
    assert r.ok is False
    assert "binary:ffmpeg" in r.missing


def test_comfyui_plugin_reports_unreachable_host() -> None:
    from circuitry.plugins.comfyui import ComfyUIPlugin

    r = ComfyUIPlugin(base_url="http://127.0.0.1:9").check()
    assert r.ok is False
    assert any(m.startswith("host:http://127.0.0.1:9") for m in r.missing)


# ---------- preflight walker ----------


def test_preflight_walker_reports_per_extension(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    p = _write(
        tmp_path,
        "orch.yml",
        "adapter: openai\neffects:\n  - {type: prompt, name: g, template: x}\n",
    )
    cfg = CircuitryConfig()
    results = preflight(p, cfg)
    labels = {label for label, _ in results}
    assert labels == {"adapter:openai"}
    by_label = dict(results)
    assert by_label["adapter:openai"].ok is False


def test_preflight_walker_handles_host_claude_deferred(tmp_path: Path) -> None:
    """host_claude can't be built without a request_handler — must surface as
    deferred (ok=True), not a hard preflight failure."""
    p = _write(
        tmp_path,
        "orch.yml",
        "adapter: host_claude\neffects:\n  - {type: prompt, name: g, template: x}\n",
    )
    cfg = CircuitryConfig()
    results = preflight(p, cfg)
    by_label = dict(results)
    r = by_label["adapter:host_claude"]
    assert r.ok is True
    assert "deferred" in (r.message or "")


def test_format_preflight_errors_skips_ok() -> None:
    rs = [
        ("adapter:ollama", CheckResult(ok=True)),
        (
            "adapter:openai",
            CheckResult(ok=False, missing=["env:OPENAI_API_KEY"]),
        ),
    ]
    errors = format_preflight_errors(rs)
    assert len(errors) == 1
    assert "adapter:openai" in errors[0]
    assert "env:OPENAI_API_KEY" in errors[0]


# ---------- validate() integration ----------


def test_validate_runs_preflight_when_config_supplied(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    p = _write(
        tmp_path,
        "orch.yml",
        "adapter: openai\neffects:\n  - {type: prompt, name: g, template: x}\n",
    )
    result = validate(p, config=CircuitryConfig())
    assert result["ok"] is False
    assert any("OPENAI_API_KEY" in e for e in result["errors"])


def test_validate_no_preflight_when_config_none(tmp_path: Path) -> None:
    """Programmatic callers without config get default-open behavior."""
    p = _write(
        tmp_path,
        "orch.yml",
        "adapter: openai\neffects:\n  - {type: prompt, name: g, template: x}\n",
    )
    result = validate(p)  # no config
    assert result["ok"] is True


# ---------- run() gating ----------


def test_run_aborts_on_preflight_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC 1.4: missing dep → run aborts with actionable error."""
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    p = _write(
        tmp_path,
        "orch.yml",
        "adapter: openai\nmodel: gpt-4o-mini\neffects:\n  - {type: prompt, name: g, template: x}\n",
    )
    cfg = CircuitryConfig()
    result = run(
        RunRequest(
            orchestration_path=p,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            config=cfg,
        )
    )
    assert result.ok is False
    assert "Preflight failed" in (result.error or "")
    assert "env:OPENAI_API_KEY" in (result.error or "")


def test_run_skip_preflight_bypasses_gate(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC 1.5: --skip-preflight allows the run to proceed past preflight."""
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    p = _write(
        tmp_path,
        "orch.yml",
        "adapter: openai\nmodel: gpt-4o-mini\neffects:\n  - {type: prompt, name: g, template: x}\n",
    )
    cfg = CircuitryConfig()
    result = run(
        RunRequest(
            orchestration_path=p,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            config=cfg,
            skip_preflight=True,
        )
    )
    # Preflight didn't gate it — instead the actual call attempt failed.
    # We assert the error doesn't come from preflight.
    assert result.ok is False
    assert "Preflight failed" not in (result.error or "")


def test_run_dry_run_skips_preflight(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    p = _write(
        tmp_path,
        "orch.yml",
        "adapter: openai\nmodel: gpt-4o-mini\neffects:\n  - {type: prompt, name: g, template: x}\n",
    )
    cfg = CircuitryConfig()
    result = run(
        RunRequest(
            orchestration_path=p,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=False,
            initial_state={},
            config=cfg,
        )
    )
    assert result.ok is True


# ---------- doctor exit codes ----------


def test_doctor_exits_nonzero_on_missing_deps(monkeypatch: pytest.MonkeyPatch) -> None:
    """AC 1.6: doctor exits 1 when any check fails."""
    # Force the openai adapter to report missing env, and also force the
    # allowlist so we don't spend time on every adapter or hit a working host.
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    monkeypatch.setenv("CIRCUITRY_ENABLED_ADAPTERS", "openai")
    monkeypatch.setenv("CIRCUITRY_ENABLED_TOOLS", "")  # lockdown tools to skip
    monkeypatch.setenv("CIRCUITRY_ENABLED_PLUGINS", "")
    runner = typer.testing.CliRunner()
    result = runner.invoke(app, ["doctor"])
    assert result.exit_code == 1
