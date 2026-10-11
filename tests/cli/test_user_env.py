"""`load_user_env` — the `.env` `cof setup` writes, loaded at every host
entry point (#348).

The autouse `_hermetic_global_config` fixture (`tests/conftest.py`) already
points `circuitry.cli.config.GLOBAL_CONFIG_DIR` at a per-test temp dir, so
writing `.env` there never touches the developer's real
``~/.config/circuitry``.
"""

from __future__ import annotations

import logging
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
def _clean_canary_env(monkeypatch: pytest.MonkeyPatch):
    """`load_user_env` mutates `os.environ` directly (mirroring real
    `load_dotenv(override=False)` behaviour), bypassing `monkeypatch`'s own
    tracking of the values it set — but `monkeypatch.delenv` here still
    records whatever value was really present and restores exactly that at
    teardown (the lane's shell exports a real `OPENAI_API_KEY`; a test must
    not delete it for the rest of the process).
    """
    for name in ("OPENAI_API_KEY", "CIRCUITRY_DOTENV_CANARY"):
        monkeypatch.delenv(name, raising=False)


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


def test_windows_has_no_getuid_but_still_loads(monkeypatch: pytest.MonkeyPatch):
    """`os.getuid` doesn't exist on Windows; `load_user_env` must skip the
    POSIX ownership/mode checks there rather than raising `AttributeError`
    for every `cof` command once `.env` exists (#349 review finding 2)."""
    monkeypatch.delattr(os, "getuid", raising=False)
    _write_env(f"OPENAI_API_KEY={_CANARY}\n")

    result = load_user_env()

    assert result.loaded is True
    assert result.supplied == ("OPENAI_API_KEY",)
    assert result.warning is None
    assert os.environ["OPENAI_API_KEY"] == _CANARY


def test_unreadable_env_file_does_not_crash():
    path = _write_env(f"OPENAI_API_KEY={_CANARY}\n", mode=0o000)
    if os.access(path, os.R_OK):
        pytest.skip("running as a user that can read a mode-0 file (e.g. root)")

    try:
        result = load_user_env()
    finally:
        path.chmod(0o600)

    assert result.loaded is False
    assert result.warning is not None
    assert "OPENAI_API_KEY" not in os.environ


def test_bare_key_with_no_value_is_not_reported_as_supplied():
    _write_env("BARE_KEY\nOPENAI_API_KEY=" + _CANARY + "\n")

    try:
        result = load_user_env()
        assert "BARE_KEY" not in result.supplied
        assert "BARE_KEY" not in os.environ
        assert result.supplied == ("OPENAI_API_KEY",)
    finally:
        os.environ.pop("BARE_KEY", None)


def test_equals_in_single_quoted_key_is_skipped_with_one_warning(caplog):
    """A single-quoted key can contain `=` (`'A=B'=...`); `os.environ[key]
    = value` would raise `ValueError` ("illegal environment variable
    name") for it (#473)."""
    path = _write_env("'A=B'=" + _CANARY + "\nOPENAI_API_KEY=" + _CANARY + "\n")

    with caplog.at_level(logging.WARNING, logger="circuitry.cli.config"):
        result = load_user_env()

    assert result.invalid == ("A=B",)
    assert result.supplied == ("OPENAI_API_KEY",)
    assert "A=B" not in os.environ
    assert os.environ["OPENAI_API_KEY"] == _CANARY
    assert _CANARY not in caplog.text
    expected = f"{path}: skipping 'A=B': not a valid environment variable name or value."
    assert expected in caplog.text
    assert caplog.text.count("not a valid environment variable name or value") == 1


def test_nul_byte_in_value_is_skipped_with_one_warning(caplog):
    """A raw NUL byte decodes as valid UTF-8 inside a value, but
    `os.environ[key] = value` would raise `ValueError` ("embedded null
    byte") for it (#473)."""
    secret = "s3cr3t_nul\x00tail_473"
    path = _write_env("NUL_KEY=" + secret + "\nOPENAI_API_KEY=" + _CANARY + "\n")

    with caplog.at_level(logging.WARNING, logger="circuitry.cli.config"):
        result = load_user_env()

    assert result.invalid == ("NUL_KEY",)
    assert result.supplied == ("OPENAI_API_KEY",)
    assert "NUL_KEY" not in os.environ
    assert os.environ["OPENAI_API_KEY"] == _CANARY
    assert "s3cr3t_nul" not in caplog.text
    assert "tail_473" not in caplog.text
    expected = f"{path}: skipping 'NUL_KEY': not a valid environment variable name or value."
    assert expected in caplog.text


def test_empty_key_is_skipped_with_one_warning(monkeypatch, caplog):
    """`python-dotenv` never actually hands `load_user_env` an empty key
    (its own line grammar has no way to produce one), but the check is
    defensive, matching electricity's port (#473)."""
    path = _write_env("OPENAI_API_KEY=" + _CANARY + "\n")
    import dotenv

    monkeypatch.setattr(
        dotenv, "dotenv_values", lambda _path: {"": "something", "OPENAI_API_KEY": _CANARY}
    )

    with caplog.at_level(logging.WARNING, logger="circuitry.cli.config"):
        result = load_user_env()

    assert result.invalid == ("",)
    assert result.supplied == ("OPENAI_API_KEY",)
    expected = f"{path}: skipping '': not a valid environment variable name or value."
    assert expected in caplog.text


def test_doctor_reports_names_only_never_a_value(tmp_path):
    import json

    import typer.testing

    from circuitry.cli.app import app

    invalid_canary = "sk-invalid-canary-should-never-print-2b7e"
    _write_env(f"OPENAI_API_KEY={_CANARY}\n'A=B'={invalid_canary}\n")
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
    # Proves the CLI root callback actually wired up `load_user_env()` —
    # without that call, `OPENAI_API_KEY` would never reach `os.environ` and
    # this would still pass on `"OPENAI_API_KEY"` appearing in doctor's own
    # "missing: env:OPENAI_API_KEY" extension-check output.
    assert os.environ["OPENAI_API_KEY"] == _CANARY
    assert "OPENAI_API_KEY" in result.output
    assert "User .env" in result.output
    assert _CANARY not in result.output
    assert "A=B" in result.output
    assert invalid_canary not in result.output


def test_doctor_renders_without_error_for_markup_breaking_key(tmp_path):
    """A key containing Rich markup syntax (e.g. from a single-quoted
    `'[/x]=1'=...` entry) must not break `cof doctor`'s own rendering —
    the key name is escaped before being interpolated into a Rich-markup
    string (#473 P2)."""
    import json

    import typer.testing

    from circuitry.cli.app import app

    _write_env(f"'[/x]=1'={_CANARY}\nOPENAI_API_KEY={_CANARY}\n")
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
    assert result.exception is None
    assert "[/x]=1" in result.output
    assert _CANARY not in result.output
