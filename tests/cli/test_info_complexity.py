"""Tests for the resolved-complexity provenance display in `cof info` (#111).

Covers each layer `cof info` can source a complexity value from
(config / orchestration / profile / cli / default), band-table rendering
(ordered, catch-all marked), and that redaction stays intact.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()

_BANDS_A = [
    {"name": "cheap", "max": 40, "model": "small-model"},
    {"name": "mid", "max": 75, "model": "mid-model"},
    {"model": "big-model"},
]

_BANDS_B = [
    {"name": "only", "model": "orch-model"},
]

_EFFECTS = [{"type": "prompt", "name": "greet", "template": "Say hello to {{name}}."}]


def _write_folder(root: Path, *, orch_complexity: dict[str, Any] | None = None) -> Path:
    root.mkdir(parents=True, exist_ok=True)
    doc: dict[str, Any] = {"effects": _EFFECTS}
    if orch_complexity is not None:
        doc["runtime"] = {"complexity": orch_complexity}
    # JSON is valid YAML — avoids hand-merging two YAML fragments.
    (root / "pipeline.yml").write_text(json.dumps(doc), encoding="utf-8")
    return root


def _write_config(
    path: Path,
    *,
    folder: Path,
    complexity: dict[str, Any] | None = None,
    extra_runtime: dict[str, Any] | None = None,
) -> Path:
    runtime: dict[str, Any] = {
        "library": {"sources": [{"type": "folder", "name": "local", "path": str(folder)}]}
    }
    if complexity is not None:
        runtime["complexity"] = complexity
    if extra_runtime:
        runtime.update(extra_runtime)
    path.write_text(json.dumps({"runtime": runtime}), encoding="utf-8")
    return path


def _complexity_json(args: list[str]) -> dict[str, Any]:
    result = runner.invoke(app, ["info", *args])
    assert result.exit_code == 0, result.output
    return json.loads(result.output)["complexity"]


# ── default: no config, no orchestration override ───────────────────────────


def test_info_default_sources_are_default() -> None:
    complexity = _complexity_json(["learn/hello", "--json"])

    assert complexity["scoring"]["enabled"] == {"value": False, "source": "default"}
    assert complexity["routing"]["enabled"] == {"value": False, "source": "default"}
    assert complexity["routing"]["bands"] == {"value": [], "source": "default"}
    assert complexity["decomposition"]["enabled"] == {"value": False, "source": "default"}
    assert complexity["decomposition"]["threshold"] == {"value": 80.0, "source": "default"}
    assert complexity["decomposition"]["max_depth"] == {"value": 2, "source": "default"}
    assert complexity["decomposition"]["on_failure"] == {
        "value": "route_up",
        "source": "default",
    }


# ── config layer ─────────────────────────────────────────────────────────────


def test_info_config_layer_sources(tmp_path: Path) -> None:
    folder = _write_folder(tmp_path / "orchestrations")
    config = _write_config(
        tmp_path / "circuitry.config.json",
        folder=folder,
        complexity={
            "scoring": {"enabled": True},
            "routing": {"enabled": True, "bands": _BANDS_A},
            "decomposition": {
                "enabled": True,
                "threshold": 65,
                "max_depth": 3,
                "on_failure": "fail",
            },
        },
    )

    complexity = _complexity_json(["pipeline", "-c", str(config), "--json"])

    for section, key in (
        ("scoring", "enabled"),
        ("routing", "enabled"),
        ("routing", "bands"),
        ("decomposition", "enabled"),
        ("decomposition", "threshold"),
        ("decomposition", "max_depth"),
        ("decomposition", "on_failure"),
    ):
        assert complexity[section][key]["source"] == "config", (section, key)

    assert complexity["decomposition"]["threshold"]["value"] == 65.0
    assert complexity["decomposition"]["max_depth"]["value"] == 3
    assert complexity["decomposition"]["on_failure"]["value"] == "fail"

    bands = complexity["routing"]["bands"]["value"]
    assert [b["model"] for b in bands] == ["small-model", "mid-model", "big-model"]
    assert [b["catch_all"] for b in bands] == [False, False, True]


# ── orchestration layer beats config ─────────────────────────────────────────


def test_info_orchestration_layer_beats_config(tmp_path: Path) -> None:
    folder = _write_folder(
        tmp_path / "orchestrations",
        orch_complexity={
            "scoring": {"enabled": True},
            "routing": {"enabled": True, "bands": _BANDS_B},
        },
    )
    config = _write_config(
        tmp_path / "circuitry.config.json",
        folder=folder,
        complexity={
            "scoring": {"enabled": True},
            "routing": {"enabled": True, "bands": _BANDS_A},
        },
    )

    complexity = _complexity_json(["pipeline", "-c", str(config), "--json"])

    assert complexity["routing"]["enabled"]["source"] == "orchestration"
    assert complexity["routing"]["bands"]["source"] == "orchestration"
    bands = complexity["routing"]["bands"]["value"]
    assert [b["model"] for b in bands] == ["orch-model"]

    # decomposition was untouched by either layer: falls back to default.
    assert complexity["decomposition"]["threshold"]["source"] == "default"


# ── cli flag beats config ────────────────────────────────────────────────────


def test_info_cli_flag_beats_config(tmp_path: Path) -> None:
    folder = _write_folder(tmp_path / "orchestrations")
    config = _write_config(
        tmp_path / "circuitry.config.json",
        folder=folder,
        complexity={
            "scoring": {"enabled": True},
            "decomposition": {"enabled": True, "threshold": 65},
        },
    )

    complexity = _complexity_json(
        ["pipeline", "-c", str(config), "--no-decompose", "--json"]
    )

    assert complexity["decomposition"]["enabled"] == {"value": False, "source": "cli"}
    # A flag flips only `enabled` — the scalar it left untouched still
    # inherits from whichever layer actually defined it.
    assert complexity["decomposition"]["threshold"] == {"value": 65.0, "source": "config"}
    assert complexity["scoring"]["enabled"]["source"] == "config"


def test_info_cli_flag_prerequisite_error_is_a_clean_cli_error(tmp_path: Path) -> None:
    """--decompose without scoring must not crash with a raw traceback."""
    folder = _write_folder(tmp_path / "orchestrations")
    config = _write_config(tmp_path / "circuitry.config.json", folder=folder)

    result = runner.invoke(app, ["info", "pipeline", "-c", str(config), "--decompose"])

    assert result.exit_code == 1
    assert "scoring" in result.output.lower()


# ── profile layer is accepted (complexity has no profile-level field) ───────


def test_info_profile_flag_does_not_disturb_config_provenance(tmp_path: Path) -> None:
    folder = _write_folder(tmp_path / "orchestrations")
    (folder / "profiles").mkdir()
    (folder / "profiles" / "fast.yml").write_text("model: fast-model\n", encoding="utf-8")
    config = _write_config(
        tmp_path / "circuitry.config.json",
        folder=folder,
        complexity={"scoring": {"enabled": True}},
    )

    complexity = _complexity_json(
        ["pipeline", "-c", str(config), "--profile", "fast", "--json"]
    )

    assert complexity["scoring"]["enabled"] == {"value": True, "source": "config"}


def test_info_unknown_profile_is_a_clean_cli_error(tmp_path: Path) -> None:
    folder = _write_folder(tmp_path / "orchestrations")
    config = _write_config(tmp_path / "circuitry.config.json", folder=folder)

    result = runner.invoke(
        app, ["info", "pipeline", "-c", str(config), "--profile", "does-not-exist"]
    )

    assert result.exit_code == 1
    assert "not found" in result.output.lower()


# ── human-readable rendering: ordered band table, catch-all marked ──────────


def test_info_human_readable_band_table_is_ordered_and_marks_catch_all(
    tmp_path: Path,
) -> None:
    folder = _write_folder(tmp_path / "orchestrations")
    config = _write_config(
        tmp_path / "circuitry.config.json",
        folder=folder,
        complexity={
            "scoring": {"enabled": True},
            "routing": {"enabled": True, "bands": _BANDS_A},
        },
    )

    result = runner.invoke(app, ["info", "pipeline", "-c", str(config)])

    assert result.exit_code == 0
    output = result.output
    assert "Routing bands" in output
    assert "catch-all" in output
    # Ordering: cheap's row precedes mid's, which precedes the catch-all row.
    assert output.index("small-model") < output.index("mid-model") < output.index(
        "big-model"
    )
    # Provenance surfaced for the switches and the decomposition scalars too.
    assert "Complexity" in output
    assert "Decomposition" in output
    assert "config" in output
    assert "default" in output


# ── redaction: complexity display must not leak secrets from elsewhere ──────


def test_info_json_does_not_leak_adapter_secrets(tmp_path: Path) -> None:
    folder = _write_folder(tmp_path / "orchestrations")
    secret = "sk-super-secret-value-should-never-print"
    config = _write_config(
        tmp_path / "circuitry.config.json",
        folder=folder,
        complexity={"scoring": {"enabled": True}},
        extra_runtime={"adapters": {"openai": {"api_key": secret}}},
    )

    result = runner.invoke(app, ["info", "pipeline", "-c", str(config), "--json"])

    assert result.exit_code == 0
    assert secret not in result.output
