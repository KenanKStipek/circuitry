"""A broken ``--config`` must read like a sentence, not a traceback.

Covers every subcommand that accepts ``-c/--config``: the loader raises a
typed :class:`ConfigError` and the root command group turns it into one
``Error: …`` line on stderr with exit code 1.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.config import (
    ConfigError,
    _load_json_file,
    load_config,
    resolve_config,
    trust_store_path,
)
from circuitry.cli.config_trust import record_trust

runner = CliRunner()


# Every command that takes a config path, with the minimum extra args needed
# to get past argument parsing. ``gen`` spells the flag ``--config`` only.
CONFIG_COMMANDS: list[tuple[str, list[str], str]] = [
    ("run", ["run", "learn/hello", "--dry-run"], "-c"),
    ("doctor", ["doctor"], "-c"),
    ("check", ["check", "{orch}"], "-c"),
    ("validate", ["validate", "{orch}"], "-c"),
    ("list", ["list"], "-c"),
    ("info", ["info", "learn/hello"], "-c"),
    ("eject", ["eject", "learn/hello"], "-c"),
    ("run-library", ["run-library", "asset-id"], "-c"),
    ("fetch", ["fetch", "asset-id", "-o", "{out}"], "-c"),
    ("library refresh", ["library", "refresh"], "-c"),
    ("gen", ["gen", "demo", "a prompt"], "--config"),
]

# name -> (file contents or None for "do not create", expected message fragment)
BROKEN_CONFIGS: dict[str, tuple[str | None, str]] = {
    "missing": (None, "Config file not found"),
    "malformed": ("{\n  \"default_model\": \n", "is not valid JSON"),
    "non_dict_root": ('["not", "an", "object"]', "must contain a JSON object at the root"),
}


@pytest.fixture()
def orch_file(tmp_path: Path) -> Path:
    path = tmp_path / "orch.yml"
    path.write_text("name: demo\nsteps: []\n", encoding="utf-8")
    return path


def _config_path(tmp_path: Path, kind: str) -> Path:
    contents, _ = BROKEN_CONFIGS[kind]
    path = tmp_path / f"{kind}.config.json"
    if contents is not None:
        path.write_text(contents, encoding="utf-8")
    return path


@pytest.mark.parametrize("kind", sorted(BROKEN_CONFIGS))
@pytest.mark.parametrize(
    "argv,flag", [pytest.param(a, f, id=n) for n, a, f in CONFIG_COMMANDS]
)
def test_broken_config_is_one_actionable_line(
    kind: str, argv: list[str], flag: str, tmp_path: Path, orch_file: Path
) -> None:
    cfg_path = _config_path(tmp_path, kind)
    args = [
        a.format(orch=orch_file, out=tmp_path / "fetched.yml") for a in argv
    ] + [flag, str(cfg_path)]

    result = runner.invoke(app, args)

    assert result.exit_code == 1
    assert result.exception is None or isinstance(result.exception, SystemExit)
    # No traceback leaked to the user.
    assert "Traceback" not in result.output
    assert "FileNotFoundError" not in result.output
    assert "JSONDecodeError" not in result.output

    stderr_lines = [line for line in result.stderr.splitlines() if line.strip()]
    assert len(stderr_lines) == 1, result.stderr
    line = stderr_lines[0]
    assert line.startswith("Error: ")
    assert BROKEN_CONFIGS[kind][1] in line
    assert str(cfg_path) in line


# name -> reason fragment the CIRCUITRY_CONFIG-sourced message names (#326).
CIRCUITRY_CONFIG_REASONS: dict[str, str] = {
    "missing": "does not exist",
    "malformed": "is not valid JSON",
    "non_dict_root": "must contain a JSON object at the root",
}


@pytest.mark.parametrize("kind", sorted(BROKEN_CONFIGS))
@pytest.mark.parametrize(
    "argv", [pytest.param(a, id=n) for n, a, _ in CONFIG_COMMANDS]
)
def test_broken_circuitry_config_env_names_the_variable(
    kind: str,
    argv: list[str],
    tmp_path: Path,
    orch_file: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A missing/malformed file from ``CIRCUITRY_CONFIG`` must name the
    variable, not just the path — unlike ``--config``, a stale env var isn't
    visible in the command line (#326)."""
    cfg_path = _config_path(tmp_path, kind)
    args = [a.format(orch=orch_file, out=tmp_path / "fetched.yml") for a in argv]
    monkeypatch.setenv("CIRCUITRY_CONFIG", str(cfg_path))

    result = runner.invoke(app, args)

    assert result.exit_code == 1
    stderr_lines = [line for line in result.stderr.splitlines() if line.strip()]
    assert len(stderr_lines) == 1, result.stderr
    line = stderr_lines[0]
    assert line.startswith(f"Error: CIRCUITRY_CONFIG points to {cfg_path}, which ")
    assert CIRCUITRY_CONFIG_REASONS[kind] in line
    assert "unset it or fix the path" in line


def test_circuitry_config_env_missing_exact_wording(
    tmp_path: Path, orch_file: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Pins the exact wording from the issue (#326)."""
    cfg_path = tmp_path / "absent.config.json"
    monkeypatch.setenv("CIRCUITRY_CONFIG", str(cfg_path))

    result = runner.invoke(app, ["doctor"])

    assert result.exit_code == 1
    assert result.stderr.strip() == (
        f"Error: CIRCUITRY_CONFIG points to {cfg_path}, which does not exist; "
        "unset it or fix the path"
    )


def test_config_flag_wording_unchanged_when_circuitry_config_also_set(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """``--config`` wins over ``CIRCUITRY_CONFIG`` and keeps its own wording
    (#326) — the env var is irrelevant once an explicit flag is given."""
    monkeypatch.setenv("CIRCUITRY_CONFIG", str(tmp_path / "from-env.json"))
    flag_path = tmp_path / "from-flag.json"

    result = runner.invoke(app, ["doctor", "-c", str(flag_path)])

    assert result.exit_code == 1
    assert result.stderr.strip() == f"Error: Config file not found: {flag_path}"


def test_valid_config_still_works(tmp_path: Path) -> None:
    cfg_path = tmp_path / "circuitry.config.json"
    cfg_path.write_text(json.dumps({"default_model": "test-model"}), encoding="utf-8")

    result = runner.invoke(app, ["list", "-c", str(cfg_path)])

    assert result.exit_code == 0
    assert "Error:" not in result.output


# ---------------------------------------------------------------------------
# Loader-level behaviour
# ---------------------------------------------------------------------------


def test_load_json_file_missing_raises_config_error(tmp_path: Path) -> None:
    with pytest.raises(ConfigError, match="Config file not found"):
        _load_json_file(tmp_path / "absent.json")


def test_load_json_file_malformed_reports_line_and_column(tmp_path: Path) -> None:
    path = tmp_path / "bad.json"
    path.write_text('{"a": 1,\n  "b": }\n', encoding="utf-8")
    with pytest.raises(ConfigError, match=r"is not valid JSON: .*line 2, column"):
        _load_json_file(path)


@pytest.mark.parametrize(
    "body,found", [("[]", "an array"), ('"x"', "a string"), ("3", "a number"), ("null", "null")]
)
def test_load_json_file_non_object_root(tmp_path: Path, body: str, found: str) -> None:
    path = tmp_path / "root.json"
    path.write_text(body, encoding="utf-8")
    with pytest.raises(ConfigError, match=f"must contain a JSON object at the root; found {found}"):
        _load_json_file(path)


def test_load_json_file_directory(tmp_path: Path) -> None:
    with pytest.raises(ConfigError, match="is a directory"):
        _load_json_file(tmp_path)


def test_config_error_is_value_error() -> None:
    """Discovery layers catch ValueError to degrade to a warning — keep that."""
    assert issubclass(ConfigError, ValueError)


def test_load_config_raises_config_error(tmp_path: Path) -> None:
    with pytest.raises(ConfigError, match="Config file not found"):
        load_config(tmp_path / "absent.json")


def test_resolve_config_explicit_path_raises(tmp_path: Path) -> None:
    with pytest.raises(ConfigError, match="Config file not found"):
        resolve_config(explicit_path=tmp_path / "absent.json")


def test_discovered_config_stays_a_warning(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    """An auto-discovered broken config warns and falls back — it is not fatal."""
    monkeypatch.delenv("CIRCUITRY_CONFIG", raising=False)
    discovered = tmp_path / "circuitry.config.json"
    discovered.write_text("{oops", encoding="utf-8")
    # Trusted, so it is parsed at all (an untrusted file is skipped unread).
    record_trust(discovered, discovered.read_bytes(), store_path=trust_store_path())

    cfg = resolve_config(cwd=tmp_path)

    assert cfg.default_model  # sane defaults survived
    assert any("Skipping malformed" in rec.message for rec in caplog.records)
