"""The ``agent`` tool's ``prompt: {file: {from: ...}, within: ...}`` through ``cof run``/``cof check`` (#591)."""

from __future__ import annotations

import json
import sqlite3
from pathlib import Path
from typing import Any

import pytest
from agent_cli_test_support import write_fake_cli

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import validate

runner = CliRunner()

MARKER = "AGENT-PROMPT-CLI-MARKER-c41f"

#: pi's event stream for one reply; the attached prompt file is copied to
#: ``FAKE_PROMPT_COPY`` so the test can see what the session received.
FAKE_PI = """\
for arg in sys.argv[1:]:
    if arg.startswith("@"):
        with open(arg[1:], encoding="utf-8") as src, open(os.environ["FAKE_PROMPT_COPY"], "w", encoding="utf-8") as dst:
            dst.write(src.read())
for event in [
    {"type": "session", "id": "sess-1", "cwd": os.getcwd()},
    {"type": "message_end", "message": {"role": "assistant",
        "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "cost": {"total": 0}},
        "content": [{"type": "text", "text": "done"}], "stopReason": "stop"}},
    {"type": "agent_end"},
]:
    sys.stdout.write(json.dumps(event) + "\\n")
"""

DOCUMENT = """\
effects:
  - type: tool
    name: implement
    provider: agent
    timeout_ms: 30000
    params:
      prompt:
        file: {{from: input.prompt_file}}
        within: {within}
      cwd: {{from: input.work}}
      env:
        FAKE_PROMPT_COPY: {copy}
"""


def _write(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def _check(orch: Path, *, trust_document: bool) -> dict[str, Any]:
    return validate(
        orch, config=CircuitryConfig(), skip_preflight=True, trust_document=trust_document
    )


def test_the_text_reaches_the_session_and_no_saved_state_holds_it(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    work = tmp_path / "work"
    _write(work / "task.md", f"{MARKER}: do the task.\n")
    copy = tmp_path / "received.md"
    orch = _write(
        tmp_path / "doc.yml",
        DOCUMENT.format(within="{from: input.work}", copy=json.dumps(str(copy))),
    )
    fake = write_fake_cli(tmp_path, "pi", body=FAKE_PI)
    db = tmp_path / "runs.db"
    monkeypatch.setenv("CIRCUITRY_SQLITE_PATH", str(db))
    config = _write(
        tmp_path / "config.json",
        json.dumps(
            {
                "plugins": [
                    "circuitry.runtime_plugins.sqlite",
                    "circuitry.runtime_plugins.jsonl_file",
                ],
                "runtime": {
                    "plugins": {"agent": {"pi": {"binary": str(fake)}}},
                    "persistence": {
                        "enabled": True,
                        "backend": "jsonl-file",
                        "path": "snapshots.jsonl",
                    },
                    "runtime_plugins": {"jsonl-file": {"path": "effects.jsonl"}},
                },
            }
        ),
    )
    result = runner.invoke(
        app,
        [
            "run",
            str(orch),
            "-e",
            "prompt_file=task.md",
            "-e",
            f"work={work}",
            "--config",
            str(config),
            "--out",
            str(tmp_path / "out.json"),
            "--live-state",
            str(tmp_path / "live.json"),
            "--events",
            str(tmp_path / "events.jsonl"),
        ],
    )
    assert result.exit_code == 0, result.stdout
    assert copy.read_text(encoding="utf-8") == f"{MARKER}: do the task.\n"

    saved = json.loads((tmp_path / "out.json").read_text(encoding="utf-8"))
    meta = saved["prime"]["implement"]["meta"]
    assert meta["template_files"][0]["field"] == "params.prompt"
    assert meta["template_files"][0]["chars"] == len(f"{MARKER}: do the task.\n")
    assert meta["params_rendered"]["prompt"]["file"] == str((work / "task.md").resolve())
    for name in ("out.json", "live.json", "events.jsonl", "snapshots.jsonl", "effects.jsonl"):
        assert (tmp_path / name).is_file(), name
        assert MARKER.encode() not in (tmp_path / name).read_bytes(), name
    assert MARKER.encode() not in db.read_bytes()
    with sqlite3.connect(db) as conn:
        assert conn.execute("SELECT COUNT(*) FROM effect_results").fetchone()[0] > 0


def test_check_accepts_the_form_and_rejects_a_missing_within(tmp_path: Path) -> None:
    ok = _write(tmp_path / "ok.yml", DOCUMENT.format(within="{from: input.work}", copy="x"))
    assert _check(ok, trust_document=True)["ok"] is True
    bad = _write(
        tmp_path / "bad.yml",
        "effects:\n  - type: tool\n    name: implement\n    provider: agent\n"
        "    params:\n      prompt:\n        file: {from: input.prompt_file}\n",
    )
    report = _check(bad, trust_document=True)
    assert report["ok"] is False
    assert "params.prompt: 'within' is required" in report["errors"][0]


def test_a_limited_document_names_a_within_only_inside_its_project(tmp_path: Path) -> None:
    project = tmp_path / "project"
    outside = tmp_path / "elsewhere"
    outside.mkdir()
    orch = _write(
        project / "doc.yml", DOCUMENT.format(within=json.dumps(str(outside)), copy="x")
    )
    limited = _check(orch, trust_document=False)
    assert limited["ok"] is False
    assert "params.prompt: 'within'" in limited["errors"][0]
    assert "resolves outside the project" in limited["errors"][0]
    assert _check(orch, trust_document=True)["ok"] is True


def test_check_rejects_the_form_in_params_json(tmp_path: Path) -> None:
    orch = _write(
        tmp_path / "doc.yml",
        "effects:\n  - type: tool\n    name: implement\n    provider: agent\n"
        "    params_json: '{\"prompt\": {\"file\": {\"from\": \"input.f\"}, \"within\": \"/srv\"}}'\n",
    )
    report = _check(orch, trust_document=True)
    assert report["ok"] is False
    assert "params_json must not set params.prompt" in report["errors"][0]
