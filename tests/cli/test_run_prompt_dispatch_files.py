"""``file: {from: ...}`` on a prompt, through ``cof run``/``cof check``/``--resume`` (#578)."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run, validate
from circuitry.core.prompt_compose import document_content_digest

runner = CliRunner()

ORCHESTRATION = """\
adapter: scripted
model: test
effects:
  - type: prompt
    name: ask
    template:
      file: {from: input.prompt_file}
      within: {from: input.work}
"""


def _write(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def test_resume_proceeds_and_reads_the_new_text_when_the_file_changed_after_a_failed_run(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    orch = _write(tmp_path / "doc.yml", ORCHESTRATION)
    prompt_file = _write(tmp_path / "work" / "p.txt", "first text {{literal}}")
    # No reply scripted for `prime.ask`: the first run fails after reading the file.
    _write(tmp_path / "scripted-replies.yaml", "{}\n")
    out = tmp_path / "run.json"
    args = ["-e", "prompt_file=p.txt", "-e", f"work={tmp_path / 'work'}"]

    first = runner.invoke(app, ["run", str(orch), "--out", str(out), *args])
    assert first.exit_code == 1, first.stdout
    failed = json.loads(out.read_text(encoding="utf-8"))
    assert failed["prime"]["ask"]["meta"]["error"]
    assert failed["prime"]["ask"]["meta"]["prompt_sent"] == "first text {{literal}}"

    prompt_file.write_text("second text, rewritten after the failure", encoding="utf-8")
    _write(tmp_path / "scripted-replies.yaml", "prime.ask:\n  - text: done\n")

    second = runner.invoke(
        app, ["run", str(orch), "--state", str(out), "--resume", "whatever", "--out", str(out), *args]
    )
    assert second.exit_code == 0, second.stdout
    resumed = json.loads(out.read_text(encoding="utf-8"))
    meta = resumed["prime"]["ask"]["meta"]
    assert resumed["prime"]["ask"]["value"] == "done"
    assert meta["prompt_sent"] == "second text, rewritten after the failure"
    assert meta["template_files"][0]["chars"] == len("second text, rewritten after the failure")


def test_the_document_hash_does_not_depend_on_the_files_content(tmp_path: Path) -> None:
    orch = _write(tmp_path / "doc.yml", ORCHESTRATION)
    prompt_file = _write(tmp_path / "work" / "p.txt", "one")
    from circuitry.cli.orchestration_loader import load_orchestration_file

    before = document_content_digest(orch, load_orchestration_file(orch))
    prompt_file.write_text("two, different", encoding="utf-8")
    prompt_file.unlink()
    assert document_content_digest(orch, load_orchestration_file(orch)) == before


def _check(orch: Path, *, trust_document: bool) -> dict[str, Any]:
    return validate(
        orch, config=CircuitryConfig(), skip_preflight=True, trust_document=trust_document
    )


LITERAL_WITHIN = """\
effects:
  - type: prompt
    name: ask
    template:
      file: {{from: input.prompt_file}}
      within: {within}
"""


def test_check_accepts_the_form_and_rejects_a_missing_within(tmp_path: Path) -> None:
    assert _check(_write(tmp_path / "ok.yml", ORCHESTRATION), trust_document=True)["ok"] is True
    bad = _write(
        tmp_path / "bad.yml",
        "effects:\n  - type: prompt\n    name: ask\n    template:\n"
        "      file: {from: input.prompt_file}\n",
    )
    report = _check(bad, trust_document=True)
    assert report["ok"] is False
    assert "'within' is required" in report["errors"][0]
    assert "template" in report["errors"][0]


def test_a_limited_document_names_a_within_only_inside_its_project_and_a_trusted_one_anywhere(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    outside = tmp_path / "elsewhere"
    outside.mkdir()
    orch = _write(project / "doc.yml", LITERAL_WITHIN.format(within=str(outside)))

    limited = _check(orch, trust_document=False)
    assert limited["ok"] is False
    assert "resolves outside the project" in limited["errors"][0]

    assert _check(orch, trust_document=True)["ok"] is True

    inside = _write(project / "inside.yml", LITERAL_WITHIN.format(within=str(project / "data")))
    assert _check(inside, trust_document=False)["ok"] is True


def test_run_applies_the_same_trust_rule(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.chdir(tmp_path)
    project = tmp_path / "project"
    outside = tmp_path / "elsewhere"
    _write(outside / "p.txt", "elsewhere text")
    orch = _write(project / "doc.yml", LITERAL_WITHIN.format(within=str(outside)))
    orch.write_text("adapter: scripted\nmodel: test\n" + orch.read_text(encoding="utf-8"))
    _write(tmp_path / "scripted-replies.yaml", "prime.ask:\n  - text: ok\n")

    def go(trust: bool) -> Any:
        return run(
            RunRequest(
                orchestration_path=orch,
                state_path=None,
                out_path=None,
                dry_run=False,
                validate_only=False,
                initial_state={"prompt_file": "p.txt"},
                config=CircuitryConfig(),
                trust_document=trust,
            )
        )

    limited = go(False)
    assert limited.ok is False
    assert "resolves outside the project" in (limited.error or "")

    trusted = go(True)
    assert trusted.ok, trusted.error
    assert trusted.state["prime"]["ask"]["meta"]["prompt_sent"] == "elsewhere text"
