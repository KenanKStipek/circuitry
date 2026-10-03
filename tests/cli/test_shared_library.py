from __future__ import annotations

import json
from pathlib import Path

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli import app as app_module
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


def test_run_library_tail_can_be_piped(tmp_path: Path) -> None:
    """``--tail`` is exempt from the auto-``--json`` pipe detection, the same
    way ``cof run --tail`` is — its own help text says "Ideal for piping"
    (#265 part 2)."""
    lib_root = tmp_path / "library"
    config_path = tmp_path / "config.json"
    _write_library_asset(lib_root, "welcome", "1.0.0", "hello-tail")
    _write_config(config_path, lib_root)

    result = runner.invoke(
        app,
        [
            "run-library", "welcome", "--version", "1.0.0",
            "--config", str(config_path), "--dry-run", "--tail",
        ],
    )

    assert result.exit_code == 0, result.output
    assert "mutually exclusive" not in result.output


def test_run_library_accepts_skip_preflight_and_profile_flags(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Flag parity with `cof run` (#265 part 2): these must parse, not 'no
    such option' — and `--profile` must behave like `cof run`'s, including
    writing the profile's own `out:` when `--out` isn't also given, the same
    as `run_cmd` does via `result.out_path` (#265 part 2 follow-up)."""
    monkeypatch.chdir(tmp_path)
    lib_root = tmp_path / "library"
    config_path = tmp_path / "config.json"
    _write_library_asset(lib_root, "welcome", "1.0.0", "hello")
    _write_config(config_path, lib_root)
    _write(
        lib_root / "welcome" / "profiles" / "fast.yml",
        "out: runs/from-profile.json\n",
    )

    result = runner.invoke(
        app,
        [
            "run-library", "welcome", "--version", "1.0.0",
            "--config", str(config_path), "--dry-run",
            "--skip-preflight", "--no-scoring", "--no-routing", "--no-decompose",
            "--profile", "fast",
        ],
    )

    assert result.exit_code == 0, result.output
    written = tmp_path / "runs" / "from-profile.json"
    assert written.exists()
    state = json.loads(written.read_text(encoding="utf-8"))
    assert state["runtime"]["effective_settings"]["out"] == "runs/from-profile.json"


def test_run_library_stashes_for_last_with_no_trust(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A successful run-library invocation stashes for `cof run --last`, same
    as `cof run` does — and a library asset is never a trusted document
    (#265 part 2)."""
    fake_home = tmp_path / "home"
    fake_home.mkdir()
    monkeypatch.setattr(app_module, "GLOBAL_CONFIG_DIR", fake_home)
    monkeypatch.setattr(app_module, "_LAST_RUN_PATH", fake_home / "last-run.json")
    lib_root = tmp_path / "library"
    config_path = tmp_path / "config.json"
    _write_library_asset(lib_root, "welcome", "1.0.0", "hello-{{name}}")
    _write_config(config_path, lib_root)

    first = runner.invoke(
        app,
        [
            "run-library", "welcome", "--version", "1.0.0",
            "--config", str(config_path), "--dry-run", "-e", "name=world",
        ],
    )
    assert first.exit_code == 0, first.output
    stash = json.loads((fake_home / "last-run.json").read_text(encoding="utf-8"))
    assert stash["orchestration"] == str(lib_root / "welcome" / "1.0.0.yml")
    assert stash["trust_document"] is False
    # #337: a replay via `cof run --last` must apply the same whole-document
    # capability gate the original `run-library` invocation did.
    assert stash["remote_library_source"] is True

    second = runner.invoke(app, ["run", "--last"])
    assert second.exit_code == 0, second.output


def test_run_library_replay_via_last_carries_the_capability_gate(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """#337: replaying a `cof run-library` stash with `cof run --last` must
    go through the same whole-document capability gate the original run
    applied, not skip it — the hole this closes: before `run-library`'s own
    stash carried `remote_library_source`, a replay ran a shell-using
    fetched asset with no consent at all.
    """
    fake_home = tmp_path / "home"
    fake_home.mkdir()
    monkeypatch.setattr(app_module, "GLOBAL_CONFIG_DIR", fake_home)
    monkeypatch.setattr(app_module, "_LAST_RUN_PATH", fake_home / "last-run.json")
    lib_root = tmp_path / "library"
    config_path = tmp_path / "config.json"
    _write(
        lib_root / "welcome" / "1.0.0.yml",
        (
            "effects:\n"
            "  - type: tool\n"
            "    name: t\n"
            "    provider: shell\n"
            "    params:\n"
            "      command: echo\n"
            "      args: [hi]\n"
        ),
    )
    _write(lib_root / "welcome" / "1.0.0.json", json.dumps({"title": "welcome-1.0.0"}) + "\n")
    _write_config(config_path, lib_root)

    first = runner.invoke(
        app,
        [
            "run-library", "welcome", "--version", "1.0.0",
            "--config", str(config_path), "--dry-run",
            "--allow-capabilities", "shell",
        ],
    )
    assert first.exit_code == 0, first.output
    stash = json.loads((fake_home / "last-run.json").read_text(encoding="utf-8"))
    assert stash["remote_library_source"] is True
    # The one-time `--allow-capabilities` grant is never persisted — the
    # replay below must refuse rather than silently reuse it.
    assert "allow_capabilities" not in stash

    replayed = runner.invoke(app, ["run", "--last", "--json"])

    assert replayed.exit_code == 1
    payload = json.loads(replayed.stdout)
    assert payload["ok"] is False
    assert "shell" in payload["error"]
    assert "cof trust" in payload["error"]


def test_run_library_service_profile_reapplied_on_last_replay(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A run-library run made with `--service-profile` stashes the profile
    name, and `cof run --last` reapplies it — otherwise the profile's
    adapter/model/runtime/plugin overrides silently vanish on replay (#265
    part 2 follow-up)."""
    monkeypatch.chdir(tmp_path)
    lib_root = tmp_path / "library"
    config_path = tmp_path / "config.json"
    # No document-level `adapter:`/`model:` here (unlike `_write_library_asset`'s
    # fixed template) so the service profile's `default_model` is the layer
    # actually under test, not shadowed by a higher-precedence document value.
    _write(
        lib_root / "welcome" / "1.0.0.yml",
        "effects:\n  - type: prompt\n    name: greet\n    template: \"hello\"\n",
    )
    cfg = {
        "default_adapter": "openai",
        "default_model": "gpt-4o-mini",
        "runtime": {
            "library": {
                "backend": "filesystem",
                "local_root": str(lib_root),
                "service_profiles": {
                    "svc-a": {
                        "default_model": "gpt-4o",
                    },
                },
            },
        },
    }
    _write(config_path, json.dumps(cfg, indent=2) + "\n")

    first = runner.invoke(
        app,
        [
            "run-library", "welcome", "--version", "1.0.0",
            "--config", str(config_path), "--dry-run",
            "--service-profile", "svc-a", "--json",
        ],
    )
    assert first.exit_code == 0, first.output
    first_state = json.loads(first.stdout)
    assert (
        first_state["runtime"]["effective_settings"]["model"] == "gpt-4o"
    )

    second = runner.invoke(app, ["run", "--last"])
    assert second.exit_code == 0, second.output
    second_state = json.loads(second.stdout)
    assert second_state["runtime"]["effective_settings"]["model"] == "gpt-4o"


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
    pytest.importorskip(
        "toon_format", reason="toon-format optional extra not installed"
    )
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


def test_run_library_failure_surfaces_an_untrusted_project_config_warning(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """An untrusted cwd config that would have set runtime.library is skipped,
    so the asset lookup fails — and the failure still names the skip (#278)."""
    lib_root = tmp_path / "library"
    _write_library_asset(lib_root, "welcome", "1.0.0", "hello")
    _write_config(tmp_path / "circuitry.config.json", lib_root)
    monkeypatch.chdir(tmp_path)

    result = runner.invoke(app, ["run-library", "welcome", "--version", "1.0.0"])

    assert result.exit_code == 1
    assert "Shared library is not configured" in result.stdout
    assert "Skipped project config" in result.stderr
