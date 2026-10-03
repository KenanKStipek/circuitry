"""CLI surface for capability consent (#275): `cof run` / `cof run-library`
refuse non-interactively by default, `--allow-capabilities` is a scripted
escape hatch, and `cof trust <document.yml>` approves one permanently.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import yaml
from typer.testing import CliRunner

from circuitry.cli import app as app_module
from circuitry.cli.app import (
    _capability_prompt,
    _confirm_capabilities,
    _parse_allow_capabilities,
    _status_pausing_prompt,
    app,
)
from circuitry.cli.config import trust_store_path
from circuitry.cli.document_consent import consented_capabilities, document_digest

runner = CliRunner()


def _write_yaml(path: Path, orch: dict[str, Any]) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(yaml.safe_dump(orch), encoding="utf-8")
    return path


def _config_file(tmp_path: Path, **runtime: Any) -> Path:
    path = tmp_path / "config.json"
    path.write_text(json.dumps({"runtime": runtime} if runtime else {}), encoding="utf-8")
    return path


def _shell_orch(path: Path) -> Path:
    return _write_yaml(
        path,
        {
            "effects": [
                {
                    "type": "tool",
                    "name": "t",
                    "provider": "shell",
                    "params": {"command": "echo", "args": ["hi"]},
                }
            ]
        },
    )


# ── _parse_allow_capabilities ────────────────────────────────────────────────


def test_parse_allow_capabilities_splits_and_strips() -> None:
    assert _parse_allow_capabilities("shell, network") == {"shell", "network"}


def test_parse_allow_capabilities_of_none_or_empty_is_none() -> None:
    assert _parse_allow_capabilities(None) is None
    assert _parse_allow_capabilities("") is None
    assert _parse_allow_capabilities(" , ") is None


# ── _capability_prompt ───────────────────────────────────────────────────────


def test_capability_prompt_is_none_when_quiet_or_json(monkeypatch: Any) -> None:
    monkeypatch.setattr("sys.stdin.isatty", lambda: True)
    monkeypatch.setattr("sys.stdout.isatty", lambda: True)
    assert _capability_prompt(quiet=True, json_out=False) is None
    assert _capability_prompt(quiet=False, json_out=True) is None
    assert _capability_prompt(quiet=False, json_out=False) is _confirm_capabilities


def test_capability_prompt_is_none_without_a_real_tty(monkeypatch: Any) -> None:
    monkeypatch.setattr("sys.stdin.isatty", lambda: False)
    monkeypatch.setattr("sys.stdout.isatty", lambda: True)
    assert _capability_prompt(quiet=False, json_out=False) is None


# ── _status_pausing_prompt ───────────────────────────────────────────────────


def test_status_pausing_prompt_is_none_when_base_prompt_is_none() -> None:
    assert _status_pausing_prompt(None, {}) is None


def test_status_pausing_prompt_stops_and_restarts_the_live_status() -> None:
    calls: list[str] = []

    class _FakeStatus:
        def stop(self) -> None:
            calls.append("stop")

        def start(self) -> None:
            calls.append("start")

    def base_prompt(label: str, capabilities: frozenset[str]) -> bool:
        calls.append("prompt")
        return True

    status_holder: dict[str, Any] = {"status": _FakeStatus()}
    wrapped = _status_pausing_prompt(base_prompt, status_holder)
    assert wrapped is not None

    assert wrapped("doc.yml", frozenset({"shell"})) is True
    assert calls == ["stop", "prompt", "start"]


def test_status_pausing_prompt_tolerates_no_status_installed_yet() -> None:
    wrapped = _status_pausing_prompt(lambda label, caps: True, {})
    assert wrapped is not None
    assert wrapped("doc.yml", frozenset({"shell"})) is True


def test_confirm_capabilities_prints_and_defers_to_typer_confirm(monkeypatch: Any) -> None:
    seen = {}

    def fake_confirm(message: str, default: bool = False) -> bool:
        seen["message"] = message
        return True

    monkeypatch.setattr(app_module.typer, "confirm", fake_confirm)
    assert _confirm_capabilities("my.yml", frozenset({"shell"})) is True
    assert "recorded" in seen["message"].lower()


# ── cof run-library: non-interactive refusal / --allow-capabilities ────────


def _library_config(tmp_path: Path, lib_root: Path) -> Path:
    return _config_file(
        tmp_path, library={"backend": "filesystem", "local_root": str(lib_root)}
    )


def test_run_library_refuses_a_shell_tool_asset_non_interactively(tmp_path: Path) -> None:
    lib_root = tmp_path / "library"
    _shell_orch(lib_root / "welcome" / "1.0.0.yml")
    (lib_root / "welcome" / "1.0.0.json").write_text("{}", encoding="utf-8")

    result = runner.invoke(
        app, ["run-library", "welcome", "-c", str(_library_config(tmp_path, lib_root)), "--json"]
    )

    assert result.exit_code == 1
    payload = json.loads(result.stdout)
    assert payload["ok"] is False
    assert "shell" in payload["error"]
    assert "cof trust" in payload["error"]


def test_run_library_allow_capabilities_runs_it(tmp_path: Path) -> None:
    lib_root = tmp_path / "library"
    _shell_orch(lib_root / "welcome" / "1.0.0.yml")
    (lib_root / "welcome" / "1.0.0.json").write_text("{}", encoding="utf-8")

    result = runner.invoke(
        app,
        [
            "run-library", "welcome",
            "-c", str(_library_config(tmp_path, lib_root)),
            "--json", "--dry-run", "--allow-capabilities", "shell",
        ],
    )

    assert result.exit_code == 0, result.output
    assert "runtime" in json.loads(result.stdout)


def test_run_library_allow_capabilities_does_not_persist(tmp_path: Path) -> None:
    lib_root = tmp_path / "library"
    asset = _shell_orch(lib_root / "welcome" / "1.0.0.yml")
    (lib_root / "welcome" / "1.0.0.json").write_text("{}", encoding="utf-8")

    runner.invoke(
        app,
        [
            "run-library", "welcome",
            "-c", str(_library_config(tmp_path, lib_root)),
            "--json", "--dry-run", "--allow-capabilities", "shell",
        ],
    )

    assert consented_capabilities(
        document_digest(asset.read_bytes()), store_path=trust_store_path()
    ) is None


# ── cof run: a trusted path with a use: ref: child still needs consent ─────


def test_cof_run_refuses_a_ref_child_even_though_the_parent_is_trusted(
    tmp_path: Path,
) -> None:
    folder = tmp_path / "lib"
    _shell_orch(folder / "helper.yml")
    orch = _write_yaml(
        tmp_path / "root.yml",
        {"effects": [{"type": "use", "name": "u", "ref": "helper"}]},
    )
    config = _config_file(
        tmp_path, library={"sources": [{"type": "folder", "name": "local", "path": str(folder)}]}
    )

    result = runner.invoke(app, ["run", str(orch), "-c", str(config), "--json"])

    assert result.exit_code == 1
    payload = json.loads(result.stdout)
    assert "shell" in payload["error"]
    assert "cof trust" in payload["error"]


def test_cof_run_resume_still_refuses_a_ref_child_without_repeated_consent(
    tmp_path: Path,
) -> None:
    """#330's `--resume` must not let a `use: ref:` child skip the gate just
    because the resumed run's content-hash/safety checks pass —
    `--allow-capabilities` approves one run only (never persisted, see
    `test_run_library_allow_capabilities_does_not_persist` above), so a
    `--resume` that doesn't repeat it hits the same refusal a fresh run
    would.
    """
    folder = tmp_path / "lib"
    _shell_orch(folder / "helper.yml")
    orch = _write_yaml(
        tmp_path / "root.yml",
        {
            "effects": [
                {"type": "tool", "name": "step1", "provider": "uuid"},
                {"type": "use", "name": "u", "ref": "helper"},
            ]
        },
    )
    config = _config_file(
        tmp_path, library={"sources": [{"type": "folder", "name": "local", "path": str(folder)}]}
    )
    out = tmp_path / "run.json"

    first = runner.invoke(
        app,
        [
            "run", str(orch), "-c", str(config), "--out", str(out),
            "--allow-capabilities", "shell",
        ],
    )
    assert first.exit_code == 0, first.output

    resumed = runner.invoke(
        app, ["run", str(orch), "-c", str(config), "--resume", "last"]
    )

    assert resumed.exit_code == 1
    assert "shell" in resumed.stdout
    assert "cof trust" in resumed.stdout


def test_cof_run_a_tool_only_document_by_path_is_unaffected(tmp_path: Path) -> None:
    """#275's owner check: no use: ref:, so nothing about this changes — with
    a gated provider (`shell`, not the untagged `uuid`), so a regression that
    started gating a by-path run would actually fail this test.
    """
    orch = _shell_orch(tmp_path / "orch.yml")

    result = runner.invoke(
        app, ["run", str(orch), "-c", str(_config_file(tmp_path)), "--json", "--skip-preflight"]
    )

    assert result.exit_code == 0, result.output
    assert "runtime" in json.loads(result.stdout)


# ── cof trust <document.yml> ─────────────────────────────────────────────────


def test_cof_trust_document_records_consent_and_unblocks_the_run(tmp_path: Path) -> None:
    folder = tmp_path / "lib"
    helper = _shell_orch(folder / "helper.yml")
    orch = _write_yaml(
        tmp_path / "root.yml",
        {"effects": [{"type": "use", "name": "u", "ref": "helper"}]},
    )
    config = _config_file(
        tmp_path, library={"sources": [{"type": "folder", "name": "local", "path": str(folder)}]}
    )

    trusted = runner.invoke(app, ["trust", str(helper), "-c", str(config), "--yes"])
    assert trusted.exit_code == 0, trusted.output
    assert "Consented" in trusted.output

    result = runner.invoke(app, ["run", str(orch), "-c", str(config), "--json"])
    assert result.exit_code == 0, result.output
    assert "runtime" in json.loads(result.stdout)


def test_cof_trust_document_declining_leaves_it_unconsented(
    tmp_path: Path, monkeypatch: Any
) -> None:
    helper = _shell_orch(tmp_path / "helper.yml")
    monkeypatch.setattr("typer.confirm", lambda *a, **k: False)

    result = runner.invoke(app, ["trust", str(helper)])

    assert result.exit_code == 1
    assert consented_capabilities(
        document_digest(helper.read_bytes()), store_path=trust_store_path()
    ) is None


def test_cof_trust_document_with_no_gated_capabilities_needs_no_confirmation(
    tmp_path: Path,
) -> None:
    orch = _write_yaml(tmp_path / "orch.yml", {"effects": [{"type": "tool", "name": "t", "provider": "uuid"}]})

    result = runner.invoke(app, ["trust", str(orch)])

    assert result.exit_code == 0, result.output
    assert "none of the gated capabilities" in result.output
