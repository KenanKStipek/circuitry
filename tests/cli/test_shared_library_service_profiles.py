from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


def _write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def _shape_paths(value: Any, prefix: str = "") -> set[str]:
    paths: set[str] = set()
    if isinstance(value, dict):
        for key, child in value.items():
            current = f"{prefix}.{key}" if prefix else str(key)
            paths.add(current)
            paths.update(_shape_paths(child, current))
    elif isinstance(value, list):
        for idx, child in enumerate(value):
            current = f"{prefix}[{idx}]"
            paths.add(current)
            paths.update(_shape_paths(child, current))
    return paths


def test_run_library_service_profiles_preserve_shared_asset_semantics(
    tmp_path: Path,
) -> None:
    lib_root = tmp_path / "library"
    config_path = tmp_path / "config.json"
    state_path = tmp_path / "in.json"
    out_a = tmp_path / "a.json"
    out_b = tmp_path / "b.json"

    _write(
        lib_root / "welcome" / "1.0.0.yml",
        (
            "effects:\n"
            "  - type: prompt\n"
            "    name: greet\n"
            "    template: \"hello {{input.name}}\"\n"
        ),
    )
    _write(state_path, json.dumps({"input": {"name": "Elena"}}) + "\n")

    cfg = {
        "runtime": {
            "library": {
                "backend": "filesystem",
                "local_root": str(lib_root),
                "service_profiles": {
                    "svc-a": {
                        "default_adapter": "openai",
                        "default_model": "gpt-4o-mini",
                        "runtime": {
                            "adapters": {"openai": {"timeout_seconds": 15}},
                            "service": {"name": "svc-a"},
                        },
                    },
                    "svc-b": {
                        "default_adapter": "anthropic",
                        "default_model": "claude-sonnet-4-20250514",
                        "runtime": {
                            "adapters": {"anthropic": {"timeout_seconds": 25}},
                            "service": {"name": "svc-b"},
                        },
                    },
                },
            }
        }
    }
    _write(config_path, json.dumps(cfg, indent=2) + "\n")

    result_a = runner.invoke(
        app,
        [
            "run-library",
            "welcome",
            "--version",
            "1.0.0",
            "--service-profile",
            "svc-a",
            "--config",
            str(config_path),
            "--state",
            str(state_path),
            "--dry-run",
            "--out",
            str(out_a),
        ],
    )
    result_b = runner.invoke(
        app,
        [
            "run-library",
            "welcome",
            "--version",
            "1.0.0",
            "--service-profile",
            "svc-b",
            "--config",
            str(config_path),
            "--state",
            str(state_path),
            "--dry-run",
            "--out",
            str(out_b),
        ],
    )

    assert result_a.exit_code == 0
    assert result_b.exit_code == 0

    state_a = json.loads(out_a.read_text(encoding="utf-8"))
    state_b = json.loads(out_b.read_text(encoding="utf-8"))

    assert state_a["prime"]["greet"]["meta"]["prompt_sent"] == "hello Elena"
    assert state_b["prime"]["greet"]["meta"]["prompt_sent"] == "hello Elena"

    assert _shape_paths(state_a.get("prime", {})) == _shape_paths(state_b.get("prime", {}))

    assert state_a["runtime"]["effective_settings"]["adapter"] == "openai"
    assert state_b["runtime"]["effective_settings"]["adapter"] == "anthropic"

    assert state_a["runtime"]["shared_library"]["service_profile"] == "svc-a"
    assert state_b["runtime"]["shared_library"]["service_profile"] == "svc-b"
