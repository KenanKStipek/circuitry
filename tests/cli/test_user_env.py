"""`load_user_env` — the `.env` `cof setup` writes, loaded at every host
entry point (#348).

The autouse `_hermetic_global_config` fixture (`tests/conftest.py`) already
points `circuitry.cli.config.GLOBAL_CONFIG_DIR` at a per-test temp dir, so
writing `.env` there never touches the developer's real
``~/.config/circuitry``.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from circuitry.cli import config as config_module
from circuitry.cli.config import load_user_env

_CANARY = "sk-canary-should-never-print-9f3a1c"


def _env_path() -> Path:
    return config_module.GLOBAL_CONFIG_DIR / ".env"


def _write_env(content: str, *, mode: int = 0o600) -> Path:
    path = _env_path()
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    path.chmod(mode)
    return path


@pytest.fixture(autouse=True)
def _clean_canary_env():
    """`load_user_env` mutates `os.environ` directly (mirroring real
    `load_dotenv(override=False)` behaviour), bypassing `monkeypatch`'s
    tracking — clean up by hand so a canary set by one test can never leak
    into the next.
    """
    for name in ("OPENAI_API_KEY", "CIRCUITRY_DOTENV_CANARY"):
        os.environ.pop(name, None)
    yield
    for name in ("OPENAI_API_KEY", "CIRCUITRY_DOTENV_CANARY"):
        os.environ.pop(name, None)


def test_missing_file_is_not_loaded():
    result = load_user_env()

    assert result.loaded is False
    assert result.supplied == ()
    assert result.skipped == ()
    assert result.warning is None


def test_key_in_env_file_reaches_adapter_env_lookup():
    _write_env(f"OPENAI_API_KEY={_CANARY}\n")

    result = load_user_env()

    assert result.loaded is True
    assert result.supplied == ("OPENAI_API_KEY",)
    assert os.environ["OPENAI_API_KEY"] == _CANARY

    from circuitry.adapters.openai import OpenAIAdapter

    check = OpenAIAdapter().check()
    assert check.ok is True


def test_environment_wins_over_env_file():
    os.environ["OPENAI_API_KEY"] = "from-real-environment"
    _write_env(f"OPENAI_API_KEY={_CANARY}\n")

    result = load_user_env()

    assert result.loaded is True
    assert result.skipped == ("OPENAI_API_KEY",)
    assert result.supplied == ()
    assert os.environ["OPENAI_API_KEY"] == "from-real-environment"


def test_working_directory_env_file_is_not_loaded(tmp_path, monkeypatch):
    cwd_env = tmp_path / "project"
    cwd_env.mkdir()
    (cwd_env / ".env").write_text(f"CIRCUITRY_DOTENV_CANARY={_CANARY}\n", encoding="utf-8")
    monkeypatch.chdir(cwd_env)

    result = load_user_env()

    assert result.loaded is False
    assert "CIRCUITRY_DOTENV_CANARY" not in os.environ


def test_group_writable_env_file_is_refused():
    path = _write_env(f"OPENAI_API_KEY={_CANARY}\n", mode=0o660)

    result = load_user_env()

    assert result.loaded is False
    assert result.warning is not None
    assert "chmod 600" in result.warning
    assert str(path) in result.warning
    assert "OPENAI_API_KEY" not in os.environ


def test_world_writable_env_file_is_refused():
    _write_env(f"OPENAI_API_KEY={_CANARY}\n", mode=0o606)

    result = load_user_env()

    assert result.loaded is False
    assert "OPENAI_API_KEY" not in os.environ


def test_group_readable_env_file_loads_with_warning():
    _write_env(f"OPENAI_API_KEY={_CANARY}\n", mode=0o640)

    result = load_user_env()

    assert result.loaded is True
    assert result.supplied == ("OPENAI_API_KEY",)
    assert result.warning is not None
    assert "chmod 600" in result.warning
    assert os.environ["OPENAI_API_KEY"] == _CANARY


def test_doctor_reports_names_only_never_a_value(tmp_path):
    import json

    import typer.testing

    from circuitry.cli.app import app

    _write_env(f"OPENAI_API_KEY={_CANARY}\n")
    config_path = tmp_path / "config.json"
    config_path.write_text(
        json.dumps({"default_adapter": "ollama", "default_model": "llama3:latest"}),
        encoding="utf-8",
    )

    runner = typer.testing.CliRunner()
    result = runner.invoke(
        app,
        ["doctor", "--config", str(config_path)],
        env={
            "CIRCUITRY_ENABLED_ADAPTERS": "",
            "CIRCUITRY_ENABLED_TOOLS": "",
            "CIRCUITRY_ENABLED_PLUGINS": "",
        },
    )

    assert result.exit_code == 0
    assert "OPENAI_API_KEY" in result.output
    assert "User .env" in result.output
    assert _CANARY not in result.output
