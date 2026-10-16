"""``runtime.state.parsed_stdout`` end to end: ``cof check``, saved state, ``--resume`` (#592)."""

from __future__ import annotations

import hashlib
import json
import os
import stat
import sys
import textwrap
from pathlib import Path

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, RunResult, run, validate
from circuitry.core.parsed_stdout import drops_parsed_stdout, parsed_stdout_errors

runner = CliRunner()

STDOUT = '{"ok": true, "items": [1, 2.5, "café"]}\n'

DOCUMENT = """\
runtime:
  state:
    parsed_stdout: {setting}
effects:
  - type: tool
    name: report
    provider: shell
    params:
      command: emit
      allowed_commands: [emit, plain]
      parse: json
  - type: tool
    name: text
    provider: shell
    params:
      command: plain
      allowed_commands: [emit, plain]
"""


def _bin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A PATH directory holding ``emit`` (prints STDOUT, appends to a counter)
    and ``plain`` (prints the same text, never parsed)."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    for name in ("emit", "plain"):
        path = bin_dir / name
        path.write_text(
            textwrap.dedent(
                f"""\
                #!{sys.executable}
                import sys
                with open({str(tmp_path / (name + '.count'))!r}, "a") as f:
                    f.write("x")
                sys.stdout.write({STDOUT!r})
                """
            ),
            encoding="utf-8",
        )
        path.chmod(path.stat().st_mode | stat.S_IXUSR)
    monkeypatch.setenv("PATH", f"{bin_dir}{os.pathsep}{os.environ['PATH']}")
    return bin_dir


def _write(tmp_path: Path, setting: str, document: str = DOCUMENT) -> Path:
    orch = tmp_path / "orch.yml"
    orch.write_text(document.format(setting=setting), encoding="utf-8")
    return orch


def _run(orch: Path, *, config: CircuitryConfig | None = None) -> RunResult:
    return run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            config=config or CircuitryConfig(),
            trust_document=True,
        )
    )


def test_the_setting_reads_full_and_drop() -> None:
    assert drops_parsed_stdout(None) is False
    assert drops_parsed_stdout({"state": {"parsed_stdout": "full"}}) is False
    assert drops_parsed_stdout({"state": {"parsed_stdout": "drop"}}) is True
    assert drops_parsed_stdout({"state": "not a mapping"}) is False
    assert parsed_stdout_errors({"state": {"parsed_stdout": None}}) == []
    with pytest.raises(ValueError, match=r"runtime\.state\.parsed_stdout must be"):
        drops_parsed_stdout({"state": {"parsed_stdout": "sha256"}})


@pytest.mark.parametrize("setting", ["sha256", "Drop", "true", "5", "'head:10'"])
def test_check_rejects_any_other_value_naming_the_key(tmp_path: Path, setting: str) -> None:
    orch = _write(tmp_path, setting)
    report = validate(orch, config=CircuitryConfig(), skip_preflight=True, trust_document=True)
    assert report["ok"] is False
    assert any(
        e.startswith("runtime.state.parsed_stdout must be 'full' or 'drop', got ")
        for e in report["errors"]
    ), report["errors"]


@pytest.mark.parametrize("setting", ["full", "drop"])
def test_check_accepts_full_and_drop(tmp_path: Path, setting: str) -> None:
    report = validate(
        _write(tmp_path, setting), config=CircuitryConfig(), skip_preflight=True, trust_document=True
    )
    assert report["ok"] is True, report["errors"]


def test_a_run_with_a_bad_value_fails_before_any_effect_dispatches(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    _bin(tmp_path, monkeypatch)
    result = _run(_write(tmp_path, "sha256"))
    assert result.ok is False
    assert "runtime.state.parsed_stdout must be 'full' or 'drop'" in (result.error or "")
    assert not (tmp_path / "emit.count").exists()


def test_drop_keeps_a_parsed_steps_stdout_out_of_state(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    _bin(tmp_path, monkeypatch)
    result = _run(_write(tmp_path, "drop"))
    assert result.ok, result.error
    report = result.state["prime"]["report"]
    assert report["value"] == {"ok": True, "items": [1, 2.5, "café"]}
    assert report["meta"]["stdout"] is None
    assert report["meta"]["stdout_chars"] == len(STDOUT)
    assert report["meta"]["stdout_sha256"] == hashlib.sha256(STDOUT.encode()).hexdigest()
    assert report["meta"]["exit_code"] == 0
    # A step without `parse: json` keeps its stdout, and gains no new field.
    text = result.state["prime"]["text"]
    assert text["value"] == STDOUT
    assert text["meta"]["stdout"] == STDOUT
    assert "stdout_chars" not in text["meta"]
    assert "stdout_sha256" not in text["meta"]


def test_full_and_the_default_keep_todays_record(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    _bin(tmp_path, monkeypatch)
    default_doc = DOCUMENT.replace("runtime:\n  state:\n    parsed_stdout: {setting}\n", "")
    for orch in (_write(tmp_path, "full"), _write(tmp_path, "", default_doc)):
        result = _run(orch)
        assert result.ok, result.error
        meta = result.state["prime"]["report"]["meta"]
        assert meta["stdout"] == STDOUT
        assert "stdout_chars" not in meta
        assert "stdout_sha256" not in meta


def test_the_setting_applies_from_config_too(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    _bin(tmp_path, monkeypatch)
    default_doc = DOCUMENT.replace("runtime:\n  state:\n    parsed_stdout: {setting}\n", "")
    result = _run(
        _write(tmp_path, "", default_doc),
        config=CircuitryConfig(runtime={"state": {"parsed_stdout": "drop"}}),
    )
    assert result.ok, result.error
    assert result.state["prime"]["report"]["meta"]["stdout"] is None


@pytest.mark.parametrize("allow_nonzero", [False, True])
@pytest.mark.parametrize("stdout", ["", "warning: not json\n"])
def test_a_failed_parse_is_recorded_the_same_under_drop_and_full(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, stdout: str, allow_nonzero: bool
) -> None:
    monkeypatch.chdir(tmp_path)
    bin_dir = _bin(tmp_path, monkeypatch)
    (bin_dir / "emit").write_text(
        f"#!{sys.executable}\nimport sys\nsys.stdout.write({stdout!r})\n"
        f"sys.exit({3 if allow_nonzero else 0})\n",
        encoding="utf-8",
    )
    document = DOCUMENT.replace(
        "      parse: json\n",
        f"      parse: json\n      allow_nonzero: {str(allow_nonzero).lower()}\n",
    )
    metas = {}
    for setting in ("full", "drop"):
        result = _run(_write(tmp_path, setting, document))
        assert result.ok is False
        metas[setting] = result.state["prime"]["report"]["meta"]
    for meta in metas.values():
        assert meta["stdout"] is None
        assert "stdout_chars" not in meta
        assert "stdout_sha256" not in meta
        assert "parse: json: stdout is" in meta["error"]
    assert metas["drop"]["error"] == metas["full"]["error"]


def test_resume_works_on_a_state_written_with_drop(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    bin_dir = _bin(tmp_path, monkeypatch)
    gate = tmp_path / "gate-open"
    (bin_dir / "gate").write_text(
        f"#!{sys.executable}\nimport os, sys\nsys.exit(0 if os.path.exists({str(gate)!r}) else 1)\n",
        encoding="utf-8",
    )
    (bin_dir / "gate").chmod(0o755)
    document = (
        DOCUMENT
        + """\
  - type: tool
    name: gate
    provider: shell
    params:
      command: gate
      allowed_commands: [gate]
  - type: tool
    name: second
    provider: json
    params:
      mode: extract
      input: {{from: prime.report.value}}
      path: items[2]
"""
    )
    orch = _write(tmp_path, "drop", document)
    out = tmp_path / "out.json"

    first = runner.invoke(app, ["run", str(orch), "--out", str(out)])
    assert first.exit_code != 0
    saved = json.loads(out.read_text(encoding="utf-8"))
    assert saved["prime"]["report"]["meta"]["stdout"] is None
    first_completed_at = saved["prime"]["report"]["meta"]["completed_at"]

    gate.write_text("open", encoding="utf-8")
    second = runner.invoke(
        app, ["run", str(orch), "--state", str(out), "--resume", "x", "--out", str(out)]
    )
    assert second.exit_code == 0, second.output
    resumed = json.loads(out.read_text(encoding="utf-8"))
    report = resumed["prime"]["report"]
    # Skipped whole: the step ran once, and its saved record is reused as is.
    assert (tmp_path / "emit.count").read_text() == "x"
    assert report["meta"]["completed_at"] == first_completed_at
    assert report["meta"]["stdout"] is None
    assert report["meta"]["stdout_chars"] == len(STDOUT)
    assert resumed["prime"]["second"]["value"] == "café"
