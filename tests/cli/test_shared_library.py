from __future__ import annotations

import json
from pathlib import Path

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


def _write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def _write_library_asset(lib_root: Path, asset_id: str, version: str, template: str) -> None:
    _write(
        lib_root / asset_id / f"{version}.yml",
        (
            "adapter: openai\n"
            "model: gpt-4o-mini\n"
            "effects:\n"
            "  - type: prompt\n"
            "    name: greet\n"
            f"    template: \"{template}\"\n"
        ),
    )
    _write(
        lib_root / asset_id / f"{version}.json",
        json.dumps({"title": f"{asset_id}-{version}"}) + "\n",
    )


def _write_config(path: Path, lib_root: Path, token: str | None = None) -> None:
    library_cfg: dict[str, object] = {
        "backend": "filesystem",
        "local_root": str(lib_root),
    }
    if token is not None:
        library_cfg["auth_token"] = token

    cfg = {
        "default_adapter": "openai",
        "default_model": "gpt-4o-mini",
        "runtime": {"library": library_cfg},
    }
    _write(path, json.dumps(cfg, indent=2) + "\n")


def test_fetch_shared_library_asset_supports_latest_and_pinned_versions(
    tmp_path: Path,
) -> None:
    lib_root = tmp_path / "library"
    config_path = tmp_path / "config.json"
    out_latest = tmp_path / "latest.yml"
    out_pinned = tmp_path / "pinned.yml"

    _write_library_asset(lib_root, "welcome", "1.0.0", "hello-v1")
    _write_library_asset(lib_root, "welcome", "1.1.0", "hello-v1-1")
    _write_config(config_path, lib_root)

    latest = runner.invoke(
        app,
        [
            "fetch",
            "welcome",
            "--config",
            str(config_path),
            "--out",
            str(out_latest),
        ],
    )
    assert latest.exit_code == 0
    assert "hello-v1-1" in out_latest.read_text(encoding="utf-8")

    pinned = runner.invoke(
        app,
        [
            "fetch",
            "welcome",
            "--version",
            "1.0.0",
            "--config",
            str(config_path),
            "--out",
            str(out_pinned),
        ],
    )
    assert pinned.exit_code == 0
    assert "hello-v1" in out_pinned.read_text(encoding="utf-8")


def test_run_library_executes_retrieved_asset_and_records_metadata(tmp_path: Path) -> None:
    lib_root = tmp_path / "library"
    config_path = tmp_path / "config.json"
    out_state = tmp_path / "out.json"

    _write_library_asset(lib_root, "welcome", "1.0.0", "hello")
    _write_config(config_path, lib_root)

    result = runner.invoke(
        app,
        [
            "run-library",
            "welcome",
            "--version",
            "1.0.0",
            "--config",
            str(config_path),
            "--dry-run",
            "--out",
            str(out_state),
        ],
    )

    assert result.exit_code == 0
    state = json.loads(out_state.read_text(encoding="utf-8"))
    shared = state["runtime"]["shared_library"]
    assert shared["asset_id"] == "welcome"
    assert shared["version"] == "1.0.0"
    assert shared["source"].startswith("filesystem:")
    assert state["runtime"]["last_run"]["completed_at"] is not None


def _write_library_asset_json(lib_root: Path, asset_id: str, version: str) -> None:
    """Write a JSON orchestration asset (no metadata sidecar — would collide)."""
    orch = {
        "adapter": "openai",
        "model": "gpt-4o-mini",
        "effects": [
            {"type": "prompt", "name": "greet", "template": "hello-json", "format": "text"}
        ],
    }
    path = lib_root / asset_id / f"{version}.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(orch, indent=2), encoding="utf-8")


def _write_library_asset_toon(lib_root: Path, asset_id: str, version: str) -> None:
    """Write a TOON orchestration asset."""
    from toon_format import encode

    orch = {
        "adapter": "openai",
        "model": "gpt-4o-mini",
        "effects": [
            {"type": "prompt", "name": "greet", "template": "hello-toon", "format": "text"}
        ],
    }
    path = lib_root / asset_id / f"{version}.toon"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(encode(orch), encoding="utf-8")


def test_fetch_json_orchestration_asset(tmp_path: Path) -> None:
    lib_root = tmp_path / "library"
    config_path = tmp_path / "config.json"
    out_path = tmp_path / "out.json"

    _write_library_asset_json(lib_root, "welcome", "1.0.0")
    _write_config(config_path, lib_root)

    result = runner.invoke(
        app,
        ["fetch", "welcome", "--config", str(config_path), "--out", str(out_path)],
    )
    assert result.exit_code == 0
    content = out_path.read_text(encoding="utf-8")
    assert "hello-json" in content


def test_fetch_toon_orchestration_asset(tmp_path: Path) -> None:
    lib_root = tmp_path / "library"
    config_path = tmp_path / "config.json"
    out_path = tmp_path / "out.toon"

    _write_library_asset_toon(lib_root, "welcome", "2.0.0")
    _write_config(config_path, lib_root)

    result = runner.invoke(
        app,
        ["fetch", "welcome", "--config", str(config_path), "--out", str(out_path)],
    )
    assert result.exit_code == 0


def test_json_orchestration_skips_metadata_sidecar(tmp_path: Path) -> None:
    """A .json orchestration must not load itself as its own metadata sidecar."""
    from circuitry.cli.shared_library import _load_metadata_sidecar

    orch_file = tmp_path / "1.0.0.json"
    orch_file.write_text('{"effects": []}', encoding="utf-8")
    metadata = _load_metadata_sidecar(orch_file)
    assert metadata == {}


def test_shared_library_discovers_mixed_formats(tmp_path: Path) -> None:
    """Asset directory with .yml and .json should both be candidates."""
    lib_root = tmp_path / "library"
    _write_library_asset(lib_root, "multi", "1.0.0", "yaml-version")
    _write_library_asset_json(lib_root, "multi", "2.0.0")

    config_path = tmp_path / "config.json"
    _write_config(config_path, lib_root)

    # Latest should be 2.0.0 (JSON)
    out_path = tmp_path / "latest.json"
    result = runner.invoke(
        app,
        ["fetch", "multi", "--config", str(config_path), "--out", str(out_path)],
    )
    assert result.exit_code == 0
    content = out_path.read_text(encoding="utf-8")
    assert "hello-json" in content


def test_fetch_reports_unauthorized_and_missing_assets(tmp_path: Path) -> None:
    lib_root = tmp_path / "library"
    config_path = tmp_path / "config.json"
    out_path = tmp_path / "asset.yml"

    _write_library_asset(lib_root, "welcome", "1.0.0", "hello")
    _write_config(config_path, lib_root, token="secret")

    unauthorized = runner.invoke(
        app,
        [
            "fetch",
            "welcome",
            "--config",
            str(config_path),
            "--out",
            str(out_path),
        ],
    )
    assert unauthorized.exit_code == 1
    assert "Unauthorized shared library access" in unauthorized.stdout

    missing = runner.invoke(
        app,
        [
            "fetch",
            "missing-asset",
            "--config",
            str(config_path),
            "--auth-token",
            "secret",
            "--out",
            str(out_path),
        ],
    )
    assert missing.exit_code == 1
    assert "Shared library asset not found" in missing.stdout
