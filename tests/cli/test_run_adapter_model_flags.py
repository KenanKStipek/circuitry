"""`cof run --adapter/--model` — the CLI tier of effective-settings resolution.

These flags are the top layer of the documented precedence chain, so the
boundary tests here pin them against every layer they must beat: environment
variables (which overlay the config layer), `--profile`, and the
orchestration's own `adapter:`/`model:`.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import ClassVar

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli import app as app_module
from circuitry.cli.app import app

runner = CliRunner()


def _write(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def _write_orch(tmp_path: Path, *, pinned: bool = False) -> Path:
    pins = "adapter: openai\nmodel: orch-model\n" if pinned else ""
    return _write(
        tmp_path / "noop.yml",
        pins
        + """
effects:
  - type: prompt
    name: greet
    template: "Hello, {{name}}."
""".lstrip(),
    )


def _write_config(tmp_path: Path) -> Path:
    return _write(
        tmp_path / "config.json",
        json.dumps(
            {
                "default_adapter": "ollama",
                "default_model": "cfg-model",
                "runtime": {"adapters": {"ollama": {"base_url": "http://localhost:11434"}}},
            }
        ),
    )


def _clear_env(monkeypatch: pytest.MonkeyPatch) -> None:
    for name in ("CIRCUITRY_MODEL", "CIRCUITRY_ADAPTER", "CIRCUITRY_ADAPTER_URL"):
        monkeypatch.delenv(name, raising=False)


def _run(tmp_path: Path, *args: str) -> dict:
    """Dry-run the orchestration and return the recorded effective settings."""
    out = tmp_path / "state.json"
    result = runner.invoke(app, ["run", *args, "--dry-run", "--out", str(out)])
    assert result.exit_code == 0, result.stdout
    state = json.loads(out.read_text(encoding="utf-8"))
    settings = state["runtime"]["effective_settings"]
    assert isinstance(settings, dict)
    return settings


def test_flags_win_over_env_vars(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    _clear_env(monkeypatch)
    monkeypatch.setenv("CIRCUITRY_MODEL", "phi3:mini")
    monkeypatch.setenv("CIRCUITRY_ADAPTER", "openai")
    orch = _write_orch(tmp_path)
    cfg = _write_config(tmp_path)

    settings = _run(
        tmp_path,
        str(orch),
        "--config",
        str(cfg),
        "--adapter",
        "ollama",
        "--model",
        "gpt-oss:20b",
    )

    assert settings["model"] == "gpt-oss:20b"
    assert settings["adapter"] == "ollama"
    assert settings["sources"]["model"] == "cli"
    assert settings["sources"]["adapter"] == "cli"


def test_flags_win_over_profile(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    _clear_env(monkeypatch)
    orch = _write_orch(tmp_path)
    cfg = _write_config(tmp_path)
    _write(
        tmp_path / "profiles" / "fast.yml",
        "adapter: openai\nmodel: profile-model\n",
    )

    settings = _run(
        tmp_path,
        str(orch),
        "--config",
        str(cfg),
        "--profile",
        "fast",
        "--adapter",
        "ollama",
        "--model",
        "gpt-oss:20b",
    )

    assert settings["model"] == "gpt-oss:20b"
    assert settings["adapter"] == "ollama"
    assert settings["sources"]["model"] == "cli"
    assert settings["sources"]["adapter"] == "cli"


def test_flags_win_over_orchestration_pins(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _clear_env(monkeypatch)
    orch = _write_orch(tmp_path, pinned=True)
    cfg = _write_config(tmp_path)

    settings = _run(
        tmp_path, str(orch), "--config", str(cfg), "--adapter", "ollama", "--model", "cli-model"
    )

    assert settings["model"] == "cli-model"
    assert settings["adapter"] == "ollama"
    assert settings["sources"]["model"] == "cli"
    assert settings["sources"]["adapter"] == "cli"


def test_one_flag_leaves_the_other_layer_intact(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`--model` alone must not silently claim the adapter slot too."""
    _clear_env(monkeypatch)
    orch = _write_orch(tmp_path, pinned=True)
    cfg = _write_config(tmp_path)

    settings = _run(tmp_path, str(orch), "--config", str(cfg), "--model", "cli-model")

    assert settings["model"] == "cli-model"
    assert settings["sources"]["model"] == "cli"
    assert settings["adapter"] == "openai"
    assert settings["sources"]["adapter"] == "orchestration"


def test_no_flags_preserves_existing_precedence(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Regression: without the flags, env still beats config and orch still wins."""
    _clear_env(monkeypatch)
    monkeypatch.setenv("CIRCUITRY_MODEL", "env-model")
    orch = _write_orch(tmp_path)
    cfg = _write_config(tmp_path)

    settings = _run(tmp_path, str(orch), "--config", str(cfg))

    assert settings["model"] == "env-model"
    assert settings["adapter"] == "ollama"
    assert settings["sources"]["model"] == "config"
    assert settings["sources"]["adapter"] == "config"


def test_last_replays_the_flags(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    _clear_env(monkeypatch)
    fake_dir = tmp_path / "config-home"
    fake_dir.mkdir(parents=True, exist_ok=True)
    monkeypatch.setattr(app_module, "GLOBAL_CONFIG_DIR", fake_dir)
    monkeypatch.setattr(app_module, "_LAST_RUN_PATH", fake_dir / "last-run.json")

    orch = _write_orch(tmp_path)
    cfg = _write_config(tmp_path)

    _run(
        tmp_path,
        str(orch),
        "--config",
        str(cfg),
        "--adapter",
        "ollama",
        "--model",
        "gpt-oss:20b",
    )

    stashed = json.loads((fake_dir / "last-run.json").read_text(encoding="utf-8"))
    assert stashed["adapter"] == "ollama"
    assert stashed["model"] == "gpt-oss:20b"

    replayed = _run(tmp_path, "--last")
    assert replayed["model"] == "gpt-oss:20b"
    assert replayed["adapter"] == "ollama"
    assert replayed["sources"]["model"] == "cli"


def test_run_library_flags_are_recorded_as_cli(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _clear_env(monkeypatch)
    monkeypatch.setenv("CIRCUITRY_MODEL", "phi3:mini")
    orch = _write_orch(tmp_path, pinned=True)
    cfg = _write_config(tmp_path)
    out = tmp_path / "state.json"

    class _Asset:
        asset_id = "demo"
        version = "1.0.0"
        source = "test"
        file_path = orch
        metadata: ClassVar[dict] = {}

    monkeypatch.setattr(
        app_module, "fetch_shared_orchestration", lambda **kwargs: _Asset()
    )

    result = runner.invoke(
        app,
        [
            "run-library",
            "demo",
            "--config",
            str(cfg),
            "--adapter",
            "ollama",
            "--model",
            "gpt-oss:20b",
            "--dry-run",
            "--out",
            str(out),
        ],
    )
    assert result.exit_code == 0, result.stdout

    settings = json.loads(out.read_text(encoding="utf-8"))["runtime"]["effective_settings"]
    assert settings["model"] == "gpt-oss:20b"
    assert settings["adapter"] == "ollama"
    assert settings["sources"]["model"] == "cli"
    assert settings["sources"]["adapter"] == "cli"
