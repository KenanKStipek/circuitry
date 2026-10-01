"""`cof init` — project scaffolding, interactive and non-interactive (#258 part 3)."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


def test_init_yes_is_fully_non_interactive(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """`cof init --yes` must not prompt — no stdin needed at all."""
    monkeypatch.chdir(tmp_path)
    result = runner.invoke(app, ["init", "--yes"], input="")
    assert result.exit_code == 0, result.output
    assert (tmp_path / "circuitry.config.json").exists()
    assert (tmp_path / "hello.yml").exists()


def test_init_no_tty_without_yes_aborts_cleanly(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Without --yes and without input, `cof init` still fails (prompts),
    matching prior behavior \u2014 --yes is what makes it scriptable."""
    monkeypatch.chdir(tmp_path)
    result = runner.invoke(app, ["init"], input="")
    assert result.exit_code != 0


def test_init_yes_honours_explicit_overrides(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    result = runner.invoke(
        app,
        [
            "init",
            "--yes",
            "--adapter",
            "openai",
            "--model",
            "gpt-4o-mini",
            "--adapter-url",
            "https://api.openai.com/v1",
        ],
    )
    assert result.exit_code == 0, result.output
    config = json.loads((tmp_path / "circuitry.config.json").read_text(encoding="utf-8"))
    assert config["default_adapter"] == "openai"
    assert config["default_model"] == "gpt-4o-mini"
    assert config["runtime"]["adapters"]["openai"]["base_url"] == "https://api.openai.com/v1"


def test_init_hello_yml_passes_cof_check(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.chdir(tmp_path)
    runner.invoke(app, ["init", "--yes"])
    result = runner.invoke(app, ["check", str(tmp_path / "hello.yml")])
    assert result.exit_code == 0, result.output
    assert "Valid" in result.output


def test_init_hello_yml_runs_with_no_api_key_and_no_network(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The generated hello.yml must actually work: no model, no network,
    no API key \u2014 only a bundled zero-dependency tool plugin (#258 part 3)."""
    monkeypatch.chdir(tmp_path)
    for var in ("OPENAI_API_KEY", "ANTHROPIC_API_KEY", "CYBERDINER_TOKEN", "CYBERDINER_EXPO_URL"):
        monkeypatch.delenv(var, raising=False)

    runner.invoke(app, ["init", "--yes"])
    result = runner.invoke(
        app, ["run", str(tmp_path / "hello.yml"), "-e", "name=World", "--tail"]
    )
    assert result.exit_code == 0, result.output
    assert "World" in result.output


def test_init_config_already_exists_aborts(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.chdir(tmp_path)
    (tmp_path / "circuitry.config.json").write_text("{}", encoding="utf-8")
    result = runner.invoke(app, ["init", "--yes"])
    assert result.exit_code == 1
    assert "already exists" in result.output
