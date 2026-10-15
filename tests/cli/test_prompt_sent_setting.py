"""``runtime.state.prompt_sent`` end to end: ``cof check``, saved state, plugins (#577)."""

from __future__ import annotations

import json
import sqlite3
from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, RunResult, run, validate

runner = CliRunner()

MARKER = "PROMPT-ONLY-MARKER-9c1d"

DOCUMENT = """\
adapter: scripted
model: test
runtime:
  state:
    prompt_sent: "{setting}"
effects:
  - type: prompt
    name: single
    template: "{marker} template for {{{{input.who}}}}"
  - type: prompt
    name: chat
    messages:
      - {{role: system, content: "{marker} system"}}
      - {{role: user, content: "{marker} user"}}
"""

REPLIES = """\
prime.single:
  - text: "first reply"
prime.chat:
  - text: "second reply"
"""


def _setup(tmp_path: Path, setting: str) -> Path:
    orch = tmp_path / "orch.yml"
    orch.write_text(DOCUMENT.format(setting=setting, marker=MARKER), encoding="utf-8")
    (tmp_path / "scripted-replies.yaml").write_text(REPLIES, encoding="utf-8")
    return orch


def _run(orch: Path, tmp_path: Path, **kwargs: Any) -> RunResult:
    return run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=tmp_path / "out.json",
            dry_run=False,
            validate_only=False,
            initial_state={"who": "ada"},
            config=CircuitryConfig(),
            **kwargs,
        )
    )


@pytest.mark.parametrize("setting", ["head:0", "head:-1", "head:", "everything", "true", "5"])
def test_check_rejects_any_other_value_naming_the_key(tmp_path: Path, setting: str) -> None:
    orch = _setup(tmp_path, setting)
    report = validate(orch, config=CircuitryConfig(), skip_preflight=True, trust_document=True)
    assert report["ok"] is False
    assert any(e.startswith("runtime.state.prompt_sent must be") for e in report["errors"])


@pytest.mark.parametrize("setting", ["full", "sha256", "head:12"])
def test_check_accepts_the_three_forms(tmp_path: Path, setting: str) -> None:
    orch = _setup(tmp_path, setting)
    report = validate(orch, config=CircuitryConfig(), skip_preflight=True, trust_document=True)
    assert report["ok"] is True, report["errors"]


def test_a_run_with_a_bad_value_fails_before_any_effect_dispatches(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    result = _run(_setup(tmp_path, "head:0"), tmp_path, trust_document=True)
    assert result.ok is False
    assert "runtime.state.prompt_sent" in (result.error or "")
    assert "single" not in result.state.get("prime", {})


def test_the_setting_applies_from_config_too(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    orch = _setup(tmp_path, "full")
    orch.write_text(orch.read_text().replace("runtime:\n  state:\n    prompt_sent: \"full\"\n", ""))
    cfg = CircuitryConfig(runtime={"state": {"prompt_sent": "sha256"}})
    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={"who": "ada"},
            config=cfg,
        )
    )
    assert result.ok, result.error
    assert result.state["prime"]["single"]["meta"]["prompt_sent"] is None


def _search(root: Path) -> list[str]:
    """Every file under *root* that holds the marker, as raw bytes."""
    needle = MARKER.encode()
    return sorted(
        str(path.relative_to(root))
        for path in root.rglob("*")
        if path.is_file() and needle in path.read_bytes()
    )


def _cli_run(tmp_path: Path, setting: str, *extra: str) -> Any:
    orch = _setup(tmp_path, setting)
    return runner.invoke(
        app,
        ["run", str(orch), "-e", "who=ada", "--out", str(tmp_path / "out.json"), *extra],
    )


def test_sha256_leaves_no_prompt_text_in_any_saved_state(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    db = tmp_path / "runs.db"
    monkeypatch.setenv("CIRCUITRY_SQLITE_PATH", str(db))
    monkeypatch.setenv("CIRCUITRY_ENV", "dev")
    config = tmp_path / "config.json"
    config.write_text(
        json.dumps(
            {
                "plugins": [
                    "circuitry.runtime_plugins.sqlite",
                    "circuitry.runtime_plugins.jsonl_file",
                ],
                "runtime": {
                    "persistence": {"enabled": True, "backend": "jsonl-file", "path": "snapshots.jsonl"},
                    "runtime_plugins": {"jsonl-file": {"path": "effects.jsonl"}},
                },
            }
        ),
        encoding="utf-8",
    )
    result = _cli_run(
        tmp_path,
        "sha256",
        "--config",
        str(config),
        "--live-state",
        str(tmp_path / "live.json"),
        "--events",
        str(tmp_path / "events.jsonl"),
    )
    assert result.exit_code == 0, result.stdout

    saved = json.loads((tmp_path / "out.json").read_text(encoding="utf-8"))
    single = saved["prime"]["single"]["meta"]
    assert single["prompt_sent"] is None
    assert single["prompt_chars"] == len(f"{MARKER} template for ada")
    # Every place a run saves state or effect records exists, and none holds the text.
    saved_files = ["out.json", "live.json", "events.jsonl", "snapshots.jsonl", "effects.jsonl"]
    for name in saved_files:
        assert (tmp_path / name).is_file(), name
        assert MARKER.encode() not in (tmp_path / name).read_bytes(), name
    assert MARKER.encode() not in db.read_bytes()
    with sqlite3.connect(db) as conn:
        assert conn.execute("SELECT COUNT(*) FROM effect_results").fetchone()[0] > 0


def test_full_still_records_the_text_in_saved_state(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    result = _cli_run(tmp_path, "full")
    assert result.exit_code == 0, result.stdout
    assert MARKER in (tmp_path / "out.json").read_text(encoding="utf-8")


def test_the_two_modes_send_the_same_input_and_produce_the_same_values(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    seen: dict[str, Any] = {}
    for setting in ("full", "sha256"):
        sub = tmp_path / setting
        sub.mkdir()
        monkeypatch.chdir(sub)
        result = _run(_setup(sub, setting), sub, trust_document=True)
        assert result.ok, result.error
        prime = result.state["prime"]
        seen[setting] = {
            name: (prime[name]["value"], prime[name]["meta"]["prompt_chars"],
                   prime[name]["meta"]["prompt_sent_sha256"],
                   prime[name]["meta"]["tokens_sent"])
            for name in ("single", "chat")
        }
    assert seen["full"] == seen["sha256"]
