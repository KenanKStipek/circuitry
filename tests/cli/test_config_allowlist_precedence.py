"""A discovered project config may narrow, never widen, a global allowlist (#245).

Plus the reporting half: `resolve_config` records the layers it applied, and
`cof run` / `cof check` / `cof doctor` print them.
"""

from __future__ import annotations

import json
import logging
import re
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

pytest.importorskip("typer")
from rich.console import Console
from typer.testing import CliRunner

from circuitry.cli import app as app_module
from circuitry.cli import doctor as doctor_module
from circuitry.cli.app import app
from circuitry.cli.config import (
    CONFIG_ENV_VARS,
    ConfigSource,
    describe_config_sources,
    resolve_config,
)

# Each test builds its own global/project layering.
pytestmark = pytest.mark.real_config_discovery

runner = CliRunner()


@pytest.fixture()
def layers(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> tuple[Path, Path]:
    """(global config path, project dir) with every config env var cleared."""
    for name in (*CONFIG_ENV_VARS, "CIRCUITRY_CONFIG"):
        monkeypatch.delenv(name, raising=False)
    global_path = tmp_path / "home" / "config.json"
    global_path.parent.mkdir()
    monkeypatch.setattr("circuitry.cli.config.GLOBAL_CONFIG_PATH", global_path)
    project = tmp_path / "project"
    project.mkdir()
    return global_path, project


def _write_json(path: Path, data: dict) -> Path:
    path.write_text(json.dumps(data), encoding="utf-8")
    return path


def test_project_list_is_intersected_with_the_global_list(
    layers: tuple[Path, Path], caplog: pytest.LogCaptureFixture
) -> None:
    global_path, project = layers
    _write_json(global_path, {"enabled_tools": ["json", "http"]})
    _write_json(project / "circuitry.config.json", {"enabled_tools": ["json", "shell"]})

    with caplog.at_level(logging.WARNING, logger="circuitry.cli.config"):
        cfg = resolve_config(cwd=project)

    assert cfg.enabled_tools == ["json"]
    assert "cannot widen enabled_tools" in caplog.text
    assert "['shell']" in caplog.text


def test_intersection_is_case_insensitive_for_tools_and_adapters(
    layers: tuple[Path, Path]
) -> None:
    """`enabled_tools`/`enabled_adapters` normalize to lowercase, so an
    uppercase entry in either layer still narrows correctly."""
    global_path, project = layers
    _write_json(global_path, {"enabled_tools": ["JSON", "HTTP"], "enabled_adapters": ["OLLAMA"]})
    _write_json(
        project / "circuitry.config.json",
        {"enabled_tools": ["json", "shell"], "enabled_adapters": ["ollama"]},
    )

    cfg = resolve_config(cwd=project)

    assert cfg.enabled_tools == ["json"]
    assert cfg.enabled_adapters == ["ollama"]


def test_project_null_does_not_reopen_a_global_list(
    layers: tuple[Path, Path], caplog: pytest.LogCaptureFixture
) -> None:
    global_path, project = layers
    _write_json(global_path, {"enabled_tools": ["json"], "enabled_adapters": ["ollama"]})
    _write_json(project / "config.json", {"enabled_tools": None, "enabled_adapters": None})

    with caplog.at_level(logging.WARNING, logger="circuitry.cli.config"):
        cfg = resolve_config(cwd=project)

    assert cfg.enabled_tools == ["json"]
    assert cfg.enabled_adapters == ["ollama"]
    assert "cannot re-open enabled_tools" in caplog.text


def test_project_without_the_key_keeps_the_global_list(layers: tuple[Path, Path]) -> None:
    global_path, project = layers
    _write_json(global_path, {"enabled_plugins": ["sqlite"]})
    _write_json(project / "circuitry.config.json", {"default_model": "m"})

    cfg = resolve_config(cwd=project)

    assert cfg.enabled_plugins == ["sqlite"]
    assert cfg.default_model == "m"


def test_project_may_lock_down_further(layers: tuple[Path, Path]) -> None:
    global_path, project = layers
    _write_json(global_path, {"enabled_tools": ["json", "http"]})
    _write_json(project / "circuitry.config.json", {"enabled_tools": []})

    assert resolve_config(cwd=project).enabled_tools == []


def test_project_list_applies_when_global_sets_none(layers: tuple[Path, Path]) -> None:
    _, project = layers
    _write_json(project / "circuitry.config.json", {"enabled_tools": ["json"]})

    assert resolve_config(cwd=project).enabled_tools == ["json"]


def test_circuitry_config_file_keeps_its_precedence(
    layers: tuple[Path, Path], tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A file the caller named is trusted as given — it layers over global."""
    global_path, project = layers
    _write_json(global_path, {"enabled_tools": ["json"]})
    named = _write_json(tmp_path / "named.json", {"enabled_tools": ["json", "shell"]})
    monkeypatch.setenv("CIRCUITRY_CONFIG", str(named))

    cfg = resolve_config(cwd=project)

    assert cfg.enabled_tools == ["json", "shell"]
    assert [s.kind for s in cfg.sources] == ["global", "CIRCUITRY_CONFIG"]


def test_explicit_config_replaces_discovery(layers: tuple[Path, Path], tmp_path: Path) -> None:
    global_path, project = layers
    _write_json(global_path, {"enabled_tools": ["json"]})
    explicit = _write_json(tmp_path / "explicit.json", {"enabled_tools": None})

    cfg = resolve_config(explicit_path=explicit, cwd=project)

    assert cfg.enabled_tools is None
    assert cfg.sources == (ConfigSource("--config", str(explicit)),)


def test_env_var_still_overrides_the_file_layers(
    layers: tuple[Path, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    global_path, project = layers
    _write_json(global_path, {"enabled_tools": ["json"]})
    monkeypatch.setenv("CIRCUITRY_ENABLED_TOOLS", "json,http")

    cfg = resolve_config(cwd=project)

    assert cfg.enabled_tools == ["json", "http"]
    assert cfg.sources[-1] == ConfigSource("env", "CIRCUITRY_ENABLED_TOOLS")


def test_sources_list_every_applied_layer_in_order(
    layers: tuple[Path, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    global_path, project = layers
    _write_json(global_path, {})
    local = _write_json(project / "circuitry.config.json", {})
    monkeypatch.setenv("CIRCUITRY_ENABLED_TOOLS", "")
    monkeypatch.setenv("CIRCUITRY_MODEL", "")  # empty: not applied, not listed

    cfg = resolve_config(cwd=project)

    assert cfg.sources == (
        ConfigSource("global", str(global_path)),
        ConfigSource("project", str(local)),
        ConfigSource("env", "CIRCUITRY_ENABLED_TOOLS"),
    )
    assert describe_config_sources(cfg.sources) == (
        f"{global_path} (global), {local} (project), CIRCUITRY_ENABLED_TOOLS (env)"
    )


def test_malformed_layer_is_not_listed(layers: tuple[Path, Path]) -> None:
    global_path, project = layers
    global_path.write_text("{not json", encoding="utf-8")

    cfg = resolve_config(cwd=project)

    assert cfg.sources == ()
    assert describe_config_sources(cfg.sources) == "— (built-in defaults)"


# ---------- headers ----------


def test_check_and_run_headers_name_the_config_sources(
    layers: tuple[Path, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    global_path, project = layers
    _write_json(global_path, {"enabled_tools": ["json"]})
    local = _write_json(project / "circuitry.config.json", {})
    doc = project / "doc.yml"
    doc.write_text(
        "effects:\n  - type: tool\n    name: t\n    provider: json\n"
        "    params: {op: parse, input: '{}'}\n",
        encoding="utf-8",
    )
    monkeypatch.chdir(project)
    # `cof run` goes quiet when stdout is not a terminal; CliRunner's is not.
    monkeypatch.setattr(
        app_module,
        "sys",
        SimpleNamespace(stdout=SimpleNamespace(isatty=lambda: True), stdin=sys.stdin),
    )
    monkeypatch.setattr(app_module, "console", Console(width=1000))
    expected = f"Config: {global_path} (global), {local} (project)"

    check = runner.invoke(app, ["check", str(doc), "--skip-preflight"])
    assert check.exit_code == 0, check.stdout
    assert expected in check.stdout

    out = project / "state.json"
    ran = runner.invoke(
        app, ["run", str(doc), "--dry-run", "--skip-preflight", "--out", str(out)]
    )
    assert expected in ran.stdout


def test_doctor_names_the_config_sources(
    layers: tuple[Path, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    global_path, project = layers
    _write_json(global_path, {})
    monkeypatch.chdir(project)
    monkeypatch.setattr(doctor_module, "console", Console(width=1000))
    monkeypatch.setattr(
        "circuitry.cli.doctor.detect_all",
        lambda **_: type("R", (), {"backends": [], "get": lambda self, _n: None})(),
    )

    result = runner.invoke(app, ["doctor"])

    row = rf"Config sources\s+│ {re.escape(str(global_path))} \(global\)"
    assert re.search(row, result.stdout), result.stdout
