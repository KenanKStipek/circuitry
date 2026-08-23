"""`cof run --out` as a profile-selectable default (issue #134).

`--out` has no orchestration/config layer — only `--out` flag > profile
`out:` > default (no file written). These boundary tests pin that ladder and
the relative-path resolution rule: a profile's `out:` resolves relative to
the working directory at run time, exactly like the `--out` flag does, not
relative to the profile file's own location.
"""

from __future__ import annotations

import json
from pathlib import Path

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


def _write_orch(path: Path) -> Path:
    return _write(
        path,
        """
effects:
  - type: prompt
    name: greet
    template: "Hello, {{name}}."
""".lstrip(),
    )


def _write_config(path: Path) -> Path:
    return _write(
        path,
        json.dumps(
            {
                "default_adapter": "ollama",
                "default_model": "cfg-model",
                "runtime": {"adapters": {"ollama": {"base_url": "http://localhost:11434"}}},
            }
        ),
    )


def test_profile_out_is_used_when_no_cli_flag(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    orch = _write_orch(tmp_path / "recipe.yml")
    cfg = _write_config(tmp_path / "config.json")
    _write(tmp_path / "profiles" / "fast.yml", "out: runs/from-profile.json\n")

    result = runner.invoke(
        app,
        ["run", str(orch), "--config", str(cfg), "--profile", "fast", "--dry-run"],
    )
    assert result.exit_code == 0, result.stdout

    written = tmp_path / "runs" / "from-profile.json"
    assert written.exists()
    state = json.loads(written.read_text(encoding="utf-8"))
    settings = state["runtime"]["effective_settings"]
    assert settings["out"] == "runs/from-profile.json"
    assert settings["sources"]["out"] == "profile"


def test_cli_out_flag_beats_profile_out(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    orch = _write_orch(tmp_path / "recipe.yml")
    cfg = _write_config(tmp_path / "config.json")
    _write(tmp_path / "profiles" / "fast.yml", "out: runs/from-profile.json\n")

    result = runner.invoke(
        app,
        [
            "run",
            str(orch),
            "--config",
            str(cfg),
            "--profile",
            "fast",
            "--dry-run",
            "--out",
            "runs/from-cli.json",
        ],
    )
    assert result.exit_code == 0, result.stdout

    assert not (tmp_path / "runs" / "from-profile.json").exists()
    written = tmp_path / "runs" / "from-cli.json"
    assert written.exists()
    state = json.loads(written.read_text(encoding="utf-8"))
    settings = state["runtime"]["effective_settings"]
    assert settings["out"] == "runs/from-cli.json"
    assert settings["sources"]["out"] == "cli"


def test_no_cli_flag_and_no_profile_out_writes_nothing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    orch = _write_orch(tmp_path / "recipe.yml")
    cfg = _write_config(tmp_path / "config.json")
    _write(tmp_path / "profiles" / "fast.yml", "adapter: ollama\n")

    result = runner.invoke(
        app,
        ["run", str(orch), "--config", str(cfg), "--profile", "fast", "--dry-run"],
    )
    assert result.exit_code == 0, result.stdout
    assert not (tmp_path / "runs").exists()

    payload = json.loads(result.stdout)
    settings = payload["runtime"]["effective_settings"]
    assert settings["out"] is None
    assert settings["sources"]["out"] == "default"


def test_profile_out_relative_path_resolves_against_cwd_not_profile_dir(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A relative `out:` must resolve like the `--out` flag does: relative to
    the working directory at run time, not relative to profiles/<name>.yml."""
    project_dir = tmp_path / "project"
    orch_dir = project_dir / "orchestrations"
    orch = _write_orch(orch_dir / "recipe.yml")
    cfg = _write_config(project_dir / "config.json")
    # Orchestration-scoped profile — lives under orch_dir/profiles/, several
    # directories away from where the run is invoked.
    _write(orch_dir / "profiles" / "fast.yml", "out: runs/from-profile.json\n")

    monkeypatch.chdir(project_dir)
    result = runner.invoke(
        app,
        ["run", str(orch), "--config", str(cfg), "--profile", "fast", "--dry-run"],
    )
    assert result.exit_code == 0, result.stdout

    # Written relative to the cwd (project_dir), not relative to orch_dir.
    assert (project_dir / "runs" / "from-profile.json").exists()
    assert not (orch_dir / "runs" / "from-profile.json").exists()


def test_last_replay_respects_profile_out_not_a_stale_resolved_path(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`--last` must not freeze a profile-resolved --out as if it were an
    explicit flag — replaying should re-resolve through the same profile."""
    fake_home = tmp_path / "home"
    fake_home.mkdir()
    monkeypatch.setattr(app_module, "GLOBAL_CONFIG_DIR", fake_home)
    monkeypatch.setattr(app_module, "_LAST_RUN_PATH", fake_home / "last-run.json")
    monkeypatch.chdir(tmp_path)
    orch = _write_orch(tmp_path / "recipe.yml")
    cfg = _write_config(tmp_path / "config.json")
    _write(tmp_path / "profiles" / "fast.yml", "out: runs/from-profile.json\n")

    first = runner.invoke(
        app,
        ["run", str(orch), "--config", str(cfg), "--profile", "fast", "--dry-run"],
    )
    assert first.exit_code == 0, first.stdout
    stash = json.loads((fake_home / "last-run.json").read_text(encoding="utf-8"))
    # The stash keeps the CLI flag's own value (absent here), not a resolved
    # profile path, so a later profile edit is still honoured on replay.
    assert stash["out"] is None
    assert stash["profile"] == "fast"

    (tmp_path / "runs" / "from-profile.json").unlink()
    _write(tmp_path / "profiles" / "fast.yml", "out: runs/edited.json\n")

    replay = runner.invoke(app, ["run", "--last"])
    assert replay.exit_code == 0, replay.stdout
    assert (tmp_path / "runs" / "edited.json").exists()
