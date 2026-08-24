"""`cof run --scoring/--routing/--decompose` — the CLI tier of the three
complexity switches.

Mirrors `test_run_adapter_model_flags.py`'s shape: flags win over config in
both directions, `--last` replays them, and the prerequisite error a flag
combination violates reads identically to the config path. The
`--no-routing`-beats-a-profile-pin composition (issue #110, following #148's
per-effect routing pins) is covered at the `runtime_shim.run` level in
`test_run_complexity_flags_composition.py`.
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


def _write_orch(tmp_path: Path) -> Path:
    return _write(
        tmp_path / "noop.yml",
        """
effects:
  - type: prompt
    name: greet
    template: "Hello, {{name}}."
""".lstrip(),
    )


def _write_config(tmp_path: Path, *, complexity: dict | None = None) -> Path:
    runtime: dict = {"adapters": {"ollama": {"base_url": "http://localhost:11434"}}}
    if complexity is not None:
        runtime["complexity"] = complexity
    return _write(
        tmp_path / "config.json",
        json.dumps(
            {
                "default_adapter": "ollama",
                "default_model": "cfg-model",
                "runtime": runtime,
            }
        ),
    )


def _run(tmp_path: Path, *args: str) -> dict:
    """Dry-run the orchestration and return the recorded effective settings."""
    out = tmp_path / "state.json"
    result = runner.invoke(app, ["run", *args, "--dry-run", "--out", str(out)])
    assert result.exit_code == 0, result.stdout
    state = json.loads(out.read_text(encoding="utf-8"))
    settings = state["runtime"]["effective_settings"]
    assert isinstance(settings, dict)
    return settings


def _run_expect_failure(tmp_path: Path, *args: str) -> str:
    out = tmp_path / "state.json"
    result = runner.invoke(app, ["run", *args, "--dry-run", "--out", str(out)])
    assert result.exit_code == 1, result.stdout
    return result.stdout


_BANDS = [{"name": "cheap", "max": 40, "model": "small"}, {"model": "large"}]


def test_no_scoring_flag_forces_scoring_on_over_config(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path)
    cfg = _write_config(tmp_path, complexity={"scoring": {"enabled": False}})

    settings = _run(tmp_path, str(orch), "--config", str(cfg), "--scoring")

    assert settings["runtime"]["complexity"]["scoring"]["enabled"] is True
    assert settings["sources"]["complexity.scoring"] == "cli"


def test_no_scoring_flag_forces_scoring_off_over_config(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path)
    cfg = _write_config(tmp_path, complexity={"scoring": {"enabled": True}})

    settings = _run(tmp_path, str(orch), "--config", str(cfg), "--no-scoring")

    assert settings["runtime"]["complexity"]["scoring"]["enabled"] is False
    assert settings["sources"]["complexity.scoring"] == "cli"


def test_routing_flag_forces_routing_on_reusing_configured_bands(
    tmp_path: Path,
) -> None:
    orch = _write_orch(tmp_path)
    cfg = _write_config(
        tmp_path,
        complexity={
            "scoring": {"enabled": True},
            "routing": {"enabled": False, "bands": _BANDS},
        },
    )

    settings = _run(tmp_path, str(orch), "--config", str(cfg), "--routing")

    assert settings["runtime"]["complexity"]["routing"]["enabled"] is True
    assert settings["runtime"]["complexity"]["routing"]["bands"] == _BANDS
    assert settings["sources"]["complexity.routing"] == "cli"


def test_no_routing_flag_forces_routing_off_over_config(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path)
    cfg = _write_config(
        tmp_path,
        complexity={
            "scoring": {"enabled": True},
            "routing": {"enabled": True, "bands": _BANDS},
        },
    )

    settings = _run(tmp_path, str(orch), "--config", str(cfg), "--no-routing")

    assert settings["runtime"]["complexity"]["routing"]["enabled"] is False
    assert settings["sources"]["complexity.routing"] == "cli"


def test_decompose_flag_forces_decomposition_on_over_config(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path)
    cfg = _write_config(
        tmp_path,
        complexity={"scoring": {"enabled": True}, "decomposition": {"enabled": False}},
    )

    settings = _run(tmp_path, str(orch), "--config", str(cfg), "--decompose")

    assert settings["runtime"]["complexity"]["decomposition"]["enabled"] is True
    assert settings["sources"]["complexity.decomposition"] == "cli"


def test_no_decompose_flag_forces_decomposition_off_over_config(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path)
    cfg = _write_config(
        tmp_path,
        complexity={"scoring": {"enabled": True}, "decomposition": {"enabled": True}},
    )

    settings = _run(tmp_path, str(orch), "--config", str(cfg), "--no-decompose")

    assert settings["runtime"]["complexity"]["decomposition"]["enabled"] is False
    assert settings["sources"]["complexity.decomposition"] == "cli"


def test_all_three_flags_together(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path)
    cfg = _write_config(tmp_path, complexity={"routing": {"bands": _BANDS}})

    settings = _run(
        tmp_path, str(orch), "--config", str(cfg), "--scoring", "--routing", "--decompose"
    )

    complexity = settings["runtime"]["complexity"]
    assert complexity["scoring"]["enabled"] is True
    assert complexity["routing"]["enabled"] is True
    assert complexity["decomposition"]["enabled"] is True
    assert settings["sources"]["complexity.scoring"] == "cli"
    assert settings["sources"]["complexity.routing"] == "cli"
    assert settings["sources"]["complexity.decomposition"] == "cli"


def test_no_flags_preserves_existing_complexity_precedence(tmp_path: Path) -> None:
    """Regression: without the flags, the config's own switches are unaffected."""
    orch = _write_orch(tmp_path)
    cfg = _write_config(tmp_path, complexity={"scoring": {"enabled": True}})

    settings = _run(tmp_path, str(orch), "--config", str(cfg))

    assert settings["runtime"]["complexity"]["scoring"]["enabled"] is True
    assert settings["sources"]["complexity.scoring"] == "config"


# -- prerequisite error, same as the config path -----------------------------


def test_routing_flag_without_scoring_names_the_missing_prerequisite(
    tmp_path: Path,
) -> None:
    orch = _write_orch(tmp_path)
    cfg = _write_config(tmp_path, complexity={"routing": {"bands": _BANDS}})

    stdout = _run_expect_failure(tmp_path, str(orch), "--config", str(cfg), "--routing")

    assert "runtime.complexity.routing.enabled is true" in stdout
    assert "runtime.complexity.scoring.enabled" in stdout


def test_decompose_flag_without_scoring_names_the_missing_prerequisite(
    tmp_path: Path,
) -> None:
    orch = _write_orch(tmp_path)
    cfg = _write_config(tmp_path)

    stdout = _run_expect_failure(tmp_path, str(orch), "--config", str(cfg), "--decompose")

    assert "runtime.complexity.decomposition.enabled is true" in stdout
    assert "runtime.complexity.scoring.enabled" in stdout


def test_no_scoring_with_routing_flag_also_names_the_prerequisite(
    tmp_path: Path,
) -> None:
    """`--no-scoring --routing` — the flag path can violate the prerequisite
    against itself, not just against a config that left scoring off."""
    orch = _write_orch(tmp_path)
    cfg = _write_config(
        tmp_path,
        complexity={"scoring": {"enabled": True}, "routing": {"bands": _BANDS}},
    )

    stdout = _run_expect_failure(
        tmp_path, str(orch), "--config", str(cfg), "--no-scoring", "--routing"
    )

    assert "runtime.complexity.routing.enabled is true" in stdout
    assert "runtime.complexity.scoring.enabled" in stdout


# -- --last replay -------------------------------------------------------------


def test_last_replays_the_complexity_flags(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake_dir = tmp_path / "config-home"
    fake_dir.mkdir(parents=True, exist_ok=True)
    monkeypatch.setattr(app_module, "GLOBAL_CONFIG_DIR", fake_dir)
    monkeypatch.setattr(app_module, "_LAST_RUN_PATH", fake_dir / "last-run.json")

    orch = _write_orch(tmp_path)
    cfg = _write_config(
        tmp_path,
        complexity={"scoring": {"enabled": True}, "routing": {"bands": _BANDS}},
    )

    _run(tmp_path, str(orch), "--config", str(cfg), "--routing", "--no-decompose")

    stashed = json.loads((fake_dir / "last-run.json").read_text(encoding="utf-8"))
    assert stashed["routing"] is True
    assert stashed["decompose"] is False
    assert stashed["scoring"] is None

    replayed = _run(tmp_path, "--last")
    complexity = replayed["runtime"]["complexity"]
    assert complexity["routing"]["enabled"] is True
    assert complexity["decomposition"]["enabled"] is False
    assert replayed["sources"]["complexity.routing"] == "cli"
    assert replayed["sources"]["complexity.decomposition"] == "cli"
