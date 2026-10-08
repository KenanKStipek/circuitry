"""`cof run --resume` — the CLI entry point (#270, resume part).

Covers the three saved-state sources (explicit `--state`, `--resume last`,
`--resume <run-id>` via a jsonl-file persistence backend) and the two safety
checks (document content hash, explicit inputs on the no-stash run-id path).
The engine's own skip/reuse behavior (no re-dispatch of a finished effect)
is covered in `tests/core/test_resume.py`; these use the builtin `uuid`/
`json` tool providers rather than `prompt` so nothing here needs a real
adapter.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


def _write(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def _two_step_orch_body() -> str:
    """A `uuid` step that always succeeds, then a `json` step whose params
    always fail to parse — a known plugin (so preflight doesn't reject it
    before anything runs), deterministically wrong every time it runs."""
    return """
effects:
  - type: tool
    name: step1
    provider: uuid
  - type: tool
    name: step2
    provider: json
    params:
      mode: parse
      input: "not valid json"
""".lstrip()


def _two_step_orch(tmp_path: Path) -> Path:
    return _write(
        tmp_path / "chain.yml",
        _two_step_orch_body(),
    )


def test_resume_from_explicit_state_file_skips_completed_steps(
    tmp_path: Path,
) -> None:
    orch = _two_step_orch(tmp_path)
    out = tmp_path / "run.json"

    first = runner.invoke(app, ["run", str(orch), "--out", str(out)])
    assert first.exit_code == 1, first.stdout
    state = json.loads(out.read_text(encoding="utf-8"))
    assert state["prime"]["step1"]["meta"]["completed_at"]
    assert not state["prime"]["step1"]["meta"]["error"]
    assert state["prime"]["step2"]["meta"]["error"]
    step1_uuid = state["prime"]["step1"]["value"]

    second = runner.invoke(
        app, ["run", str(orch), "--state", str(out), "--resume", "whatever"]
    )
    # step2's input is always invalid JSON, so resuming still fails there —
    # the error names step2, proving the run reached it again rather than
    # stopping (or erroring) at step1. step1's own uuid being unchanged (not
    # just its error-free meta) proves it wasn't re-dispatched at all.
    assert second.exit_code == 1
    assert "step2" in second.stdout
    second_state = json.loads(out.read_text(encoding="utf-8"))
    assert second_state["prime"]["step1"]["value"] == step1_uuid


def test_resume_rejects_a_run_that_was_never_recorded(tmp_path: Path) -> None:
    orch = _write(
        tmp_path / "solo.yml", "effects:\n  - type: tool\n    name: s\n    provider: uuid\n"
    )

    result = runner.invoke(app, ["run", str(orch), "--resume", "last"])
    assert result.exit_code == 1
    assert "No previous run found" in result.stdout


def test_resume_last_requires_matching_orchestration(tmp_path: Path) -> None:
    orch_a = _write(
        tmp_path / "a.yml", "effects:\n  - type: tool\n    name: s\n    provider: uuid\n"
    )
    orch_b = _write(
        tmp_path / "b.yml", "effects:\n  - type: tool\n    name: s\n    provider: uuid\n"
    )
    out = tmp_path / "a.json"

    first = runner.invoke(app, ["run", str(orch_a), "--out", str(out)])
    assert first.exit_code == 0, first.stdout

    result = runner.invoke(app, ["run", str(orch_b), "--resume", "last"])
    assert result.exit_code == 1
    assert "last run was" in result.stdout


def test_resume_last_requires_the_last_run_used_out(tmp_path: Path) -> None:
    orch = _write(
        tmp_path / "solo.yml", "effects:\n  - type: tool\n    name: s\n    provider: uuid\n"
    )

    first = runner.invoke(app, ["run", str(orch)])
    assert first.exit_code == 0, first.stdout

    result = runner.invoke(app, ["run", str(orch), "--resume", "last"])
    assert result.exit_code == 1
    assert "did not use --out" in result.stdout


def test_resume_last_resumes_a_failed_run(tmp_path: Path) -> None:
    orch = _two_step_orch(tmp_path)
    out = tmp_path / "run.json"

    first = runner.invoke(app, ["run", str(orch), "--out", str(out)])
    assert first.exit_code == 1, first.stdout

    second = runner.invoke(app, ["run", str(orch), "--resume", "last"])
    # step2's input is always invalid JSON, so this still fails — proving
    # the *source resolution* (finding `out` via the --last stash) worked,
    # which is this test's job; the engine's no-rerun behavior is covered in
    # tests/core/test_resume.py.
    assert second.exit_code == 1
    assert "step2" in second.stdout


def test_resume_refuses_a_changed_document_unless_forced(tmp_path: Path) -> None:
    orch = _write(
        tmp_path / "chain.yml", "effects:\n  - type: tool\n    name: s\n    provider: uuid\n"
    )
    out = tmp_path / "run.json"

    first = runner.invoke(app, ["run", str(orch), "--out", str(out)])
    assert first.exit_code == 0, first.stdout

    _write(
        orch,
        "effects:\n  - type: tool\n    name: s\n    provider: uuid\n"
        "  - type: tool\n    name: s2\n    provider: uuid\n",
    )

    blocked = runner.invoke(app, ["run", str(orch), "--state", str(out), "--resume", "x"])
    assert blocked.exit_code == 1
    assert "content hash differs" in blocked.stdout

    forced = runner.invoke(
        app, ["run", str(orch), "--state", str(out), "--resume", "x", "--force"]
    )
    assert forced.exit_code == 0, forced.stdout


def test_resume_by_run_id_needs_persistence_configured(tmp_path: Path) -> None:
    orch = _write(
        tmp_path / "chain.yml", "effects:\n  - type: tool\n    name: s\n    provider: uuid\n"
    )

    result = runner.invoke(app, ["run", str(orch), "--resume", "some-run-id"])
    assert result.exit_code == 1
    assert "runtime.persistence" in result.stdout


def test_resume_by_run_id_refuses_silent_inputs_without_minus_e(tmp_path: Path) -> None:
    db_path = tmp_path / "runs.jsonl"
    orch = _write(
        tmp_path / "chain.yml",
        f"""
runtime:
  persistence:
    enabled: true
    backend: jsonl-file
    path: "{db_path}"
interface:
  inputs:
    topic: {{type: string, required: true}}
effects:
  - type: tool
    name: s
    provider: json
    params:
      mode: stringify
      input: "{{{{input.topic}}}}"
""".lstrip(),
    )

    first = runner.invoke(app, ["run", str(orch), "-e", "topic=widgets"])
    assert first.exit_code == 0, first.stdout

    record = json.loads(db_path.read_text(encoding="utf-8").splitlines()[-1])
    run_id = record["run_id"]

    no_inputs = runner.invoke(app, ["run", str(orch), "--resume", run_id])
    assert no_inputs.exit_code == 1
    assert "explicit inputs" in no_inputs.stdout

    with_inputs = runner.invoke(
        app, ["run", str(orch), "--resume", run_id, "-e", "topic=widgets"]
    )
    assert with_inputs.exit_code == 0, with_inputs.stdout


def test_resume_by_run_id_scoped_to_its_own_orchestration(tmp_path: Path) -> None:
    db_path = tmp_path / "runs.jsonl"
    orch_a = _write(
        tmp_path / "a.yml",
        f"""
runtime:
  persistence:
    enabled: true
    backend: jsonl-file
    path: "{db_path}"
effects:
  - type: tool
    name: s
    provider: uuid
""".lstrip(),
    )
    orch_b = _write(
        tmp_path / "b.yml",
        f"""
runtime:
  persistence:
    enabled: true
    backend: jsonl-file
    path: "{db_path}"
effects:
  - type: tool
    name: s
    provider: uuid
""".lstrip(),
    )

    first = runner.invoke(app, ["run", str(orch_a)])
    assert first.exit_code == 0, first.stdout
    record = json.loads(db_path.read_text(encoding="utf-8").splitlines()[-1])
    run_id = record["run_id"]

    cross = runner.invoke(app, ["run", str(orch_b), "--resume", run_id])
    assert cross.exit_code == 1
    assert "No persisted state found" in cross.stdout


def test_resume_and_last_are_mutually_exclusive(tmp_path: Path) -> None:
    orch = _write(
        tmp_path / "chain.yml", "effects:\n  - type: tool\n    name: s\n    provider: uuid\n"
    )

    result = runner.invoke(app, ["run", str(orch), "--resume", "last", "--last"])
    assert result.exit_code == 1
    assert "mutually exclusive" in result.stdout


def test_resume_by_run_id_finds_a_failed_run(tmp_path: Path) -> None:
    """#270 F1: the run store must persist a failed run too, or `--resume
    <run-id>` can never find one — only runs that happened to succeed."""
    db_path = tmp_path / "runs.jsonl"
    orch = _write(
        tmp_path / "chain.yml",
        f"""
runtime:
  persistence:
    enabled: true
    backend: jsonl-file
    path: "{db_path}"
""".lstrip()
        + _two_step_orch_body(),
    )

    first = runner.invoke(app, ["run", str(orch)])
    assert first.exit_code == 1, first.stdout

    records = [json.loads(line) for line in db_path.read_text(encoding="utf-8").splitlines()]
    assert records, "the failed run must still be persisted"
    record = records[-1]
    assert record["ok"] is False
    run_id = record["run_id"]

    second = runner.invoke(app, ["run", str(orch), "--resume", run_id])
    # Found and resumed (not "No persisted state found") — step2 fails the
    # same deterministic way every time, proving the run reached it again.
    assert second.exit_code == 1
    assert "No persisted state found" not in second.stdout
    assert "step2" in second.stdout


def test_resume_refuses_when_only_a_referenced_prompt_file_changed(tmp_path: Path) -> None:
    """The document content hash (#396) folds in every `{file: ...}` prompt
    source the document references, not just the orchestration YAML's own
    bytes — editing only the prompt file must refuse resume exactly like
    editing the YAML itself already does.
    """
    _write(tmp_path / "brief.md", "Say hi.")
    orch = _write(
        tmp_path / "chain.yml",
        "prompts:\n  brief: {file: brief.md}\n"
        "effects:\n  - type: yield\n    name: y\n    template: \"{{> brief}}\"\n",
    )
    out = tmp_path / "run.json"

    first = runner.invoke(app, ["run", str(orch), "--out", str(out)])
    assert first.exit_code == 0, first.stdout

    _write(tmp_path / "brief.md", "Say hi, differently.")

    blocked = runner.invoke(app, ["run", str(orch), "--state", str(out), "--resume", "x"])
    assert blocked.exit_code == 1
    assert "content hash differs" in blocked.stdout

    forced = runner.invoke(
        app, ["run", str(orch), "--state", str(out), "--resume", "x", "--force"]
    )
    assert forced.exit_code == 0, forced.stdout


def test_document_hash_changes_when_bytes_move_between_prompt_files(tmp_path: Path) -> None:
    """`runtime.last_run.document_hash` is `document_content_digest` (#407):
    moving bytes across a prompt-file boundary (same total content, same
    file set, different split) must still change it — concatenating each
    file's bytes with nothing between them (the pre-#407 algorithm) could
    not tell that apart."""
    _write(tmp_path / "a.md", "AB")
    _write(tmp_path / "b.md", "CD")
    orch = _write(
        tmp_path / "chain.yml",
        "prompts:\n  a: {file: a.md}\n  b: {file: b.md}\n"
        "effects:\n  - type: yield\n    name: y\n    template: \"{{> a}}{{> b}}\"\n",
    )

    out1 = tmp_path / "run1.json"
    first = runner.invoke(app, ["run", str(orch), "--out", str(out1)])
    assert first.exit_code == 0, first.stdout
    hash_before = json.loads(out1.read_text(encoding="utf-8"))["runtime"]["last_run"][
        "document_hash"
    ]

    # Same four bytes overall ("ABCD"), same two files, moved across the
    # boundary instead of either file's own content changing.
    _write(tmp_path / "a.md", "A")
    _write(tmp_path / "b.md", "BCD")

    out2 = tmp_path / "run2.json"
    second = runner.invoke(app, ["run", str(orch), "--out", str(out2)])
    assert second.exit_code == 0, second.stdout
    hash_after = json.loads(out2.read_text(encoding="utf-8"))["runtime"]["last_run"][
        "document_hash"
    ]

    assert hash_before != hash_after


def test_resume_accepts_an_unchanged_prompt_file_document_under_the_new_hash(
    tmp_path: Path,
) -> None:
    """The only end-to-end proof that the recompute in `app.py` matches
    the stamp in `runtime_shim.py` for a document with a `{file: ...}`
    prompt, confinement root included: an unchanged document resumes
    without `--force` when the saved state carries the current-algorithm
    hash, not just the legacy one."""
    _write(tmp_path / "brief.md", "Say hi.")
    orch = _write(
        tmp_path / "chain.yml",
        "prompts:\n  brief: {file: brief.md}\n"
        "effects:\n  - type: yield\n    name: y\n    template: \"{{> brief}}\"\n",
    )
    out = tmp_path / "run.json"

    first = runner.invoke(app, ["run", str(orch), "--out", str(out)])
    assert first.exit_code == 0, first.stdout

    resumed = runner.invoke(app, ["run", str(orch), "--state", str(out), "--resume", "x"])
    assert resumed.exit_code == 0, resumed.stdout


def test_resume_refuses_moved_prompt_file_bytes_under_the_new_hash(tmp_path: Path) -> None:
    """The user-visible fix from #407: under the current algorithm, moving
    bytes across a prompt-file boundary (same total content, same file
    set, different split) must still be refused as a content change, not
    just produce a different hash in isolation."""
    _write(tmp_path / "a.md", "AB")
    _write(tmp_path / "b.md", "CD")
    orch = _write(
        tmp_path / "chain.yml",
        "prompts:\n  a: {file: a.md}\n  b: {file: b.md}\n"
        "effects:\n  - type: yield\n    name: y\n    template: \"{{> a}}{{> b}}\"\n",
    )
    out = tmp_path / "run.json"

    first = runner.invoke(app, ["run", str(orch), "--out", str(out)])
    assert first.exit_code == 0, first.stdout

    # Same four bytes overall ("ABCD"), same two files, moved across the
    # boundary instead of either file's own content changing.
    _write(tmp_path / "a.md", "A")
    _write(tmp_path / "b.md", "BCD")

    blocked = runner.invoke(app, ["run", str(orch), "--state", str(out), "--resume", "x"])
    assert blocked.exit_code == 1
    assert "content hash differs" in blocked.stdout

    forced = runner.invoke(
        app, ["run", str(orch), "--state", str(out), "--resume", "x", "--force"]
    )
    assert forced.exit_code == 0, forced.stdout


def test_document_hash_equals_document_content_digest_for_the_same_document(
    tmp_path: Path,
) -> None:
    """`document_hash` and the consent digest (`cof trust`, the pre-run
    gate, `use`) must be the exact same value for the same document (#407)
    — one algorithm, not two that happen to agree on bytes alone."""
    from circuitry.core.prompt_compose import document_content_digest
    from circuitry.core.yaml_load import load_yaml

    _write(tmp_path / "brief.md", "Say hi.")
    orch_path = _write(
        tmp_path / "chain.yml",
        "prompts:\n  brief: {file: brief.md}\n"
        "effects:\n  - type: yield\n    name: y\n    template: \"{{> brief}}\"\n",
    )
    out = tmp_path / "run.json"

    result = runner.invoke(app, ["run", str(orch_path), "--out", str(out)])
    assert result.exit_code == 0, result.stdout
    document_hash = json.loads(out.read_text(encoding="utf-8"))["runtime"]["last_run"][
        "document_hash"
    ]

    orch = load_yaml(orch_path.read_text(encoding="utf-8"))
    expected = document_content_digest(orch_path, orch, confinement_root=tmp_path)
    assert document_hash == expected


def test_resume_accepts_a_pre_407_document_hash(tmp_path: Path) -> None:
    """A state saved by a release before #407 recorded `document_hash` with
    the old algorithm (document bytes, then each referenced prompt file's
    bytes, sorted, with no path label or length) — `--resume` must still
    accept that without `--force` when the document hasn't actually
    changed."""
    from circuitry.core.resume import _legacy_document_sha256

    _write(tmp_path / "brief.md", "Say hi.")
    orch = _write(
        tmp_path / "chain.yml",
        "prompts:\n  brief: {file: brief.md}\n"
        "effects:\n  - type: yield\n    name: y\n    template: \"{{> brief}}\"\n",
    )
    out = tmp_path / "run.json"

    first = runner.invoke(app, ["run", str(orch), "--out", str(out)])
    assert first.exit_code == 0, first.stdout

    state = json.loads(out.read_text(encoding="utf-8"))
    legacy_hash = _legacy_document_sha256(orch)
    # A document with a referenced prompt file is exactly where the two
    # algorithms diverge (the new one tags each file with its path and
    # byte length) — otherwise this fixture would not exercise the
    # fallback at all.
    assert legacy_hash != state["runtime"]["last_run"]["document_hash"]
    state["runtime"]["last_run"]["document_hash"] = legacy_hash
    out.write_text(json.dumps(state), encoding="utf-8")

    resumed = runner.invoke(app, ["run", str(orch), "--state", str(out), "--resume", "x"])
    assert resumed.exit_code == 0, resumed.stdout


def test_resume_refuses_a_changed_document_under_the_legacy_hash_too(tmp_path: Path) -> None:
    """A saved state carrying a pre-#407 hash is accepted when the document
    is unchanged (see above) — but a document that genuinely changed since
    is still refused, exactly like a state carrying the current hash."""
    from circuitry.core.resume import _legacy_document_sha256

    orch = _write(
        tmp_path / "chain.yml", "effects:\n  - type: tool\n    name: s\n    provider: uuid\n"
    )
    out = tmp_path / "run.json"

    first = runner.invoke(app, ["run", str(orch), "--out", str(out)])
    assert first.exit_code == 0, first.stdout

    state = json.loads(out.read_text(encoding="utf-8"))
    state["runtime"]["last_run"]["document_hash"] = _legacy_document_sha256(orch)
    out.write_text(json.dumps(state), encoding="utf-8")

    _write(
        orch,
        "effects:\n  - type: tool\n    name: s\n    provider: uuid\n"
        "  - type: tool\n    name: s2\n    provider: uuid\n",
    )

    blocked = runner.invoke(app, ["run", str(orch), "--state", str(out), "--resume", "x"])
    assert blocked.exit_code == 1
    assert "content hash differs" in blocked.stdout

    forced = runner.invoke(
        app, ["run", str(orch), "--state", str(out), "--resume", "x", "--force"]
    )
    assert forced.exit_code == 0, forced.stdout


def test_resume_refuses_a_changed_prompt_file_under_the_legacy_hash_too(tmp_path: Path) -> None:
    """Same as above, but the thing that changed is a referenced prompt
    file rather than the orchestration YAML itself."""
    from circuitry.core.resume import _legacy_document_sha256

    _write(tmp_path / "brief.md", "Say hi.")
    orch = _write(
        tmp_path / "chain.yml",
        "prompts:\n  brief: {file: brief.md}\n"
        "effects:\n  - type: yield\n    name: y\n    template: \"{{> brief}}\"\n",
    )
    out = tmp_path / "run.json"

    first = runner.invoke(app, ["run", str(orch), "--out", str(out)])
    assert first.exit_code == 0, first.stdout

    state = json.loads(out.read_text(encoding="utf-8"))
    state["runtime"]["last_run"]["document_hash"] = _legacy_document_sha256(orch)
    out.write_text(json.dumps(state), encoding="utf-8")

    _write(tmp_path / "brief.md", "Say hi, differently.")

    blocked = runner.invoke(app, ["run", str(orch), "--state", str(out), "--resume", "x"])
    assert blocked.exit_code == 1
    assert "content hash differs" in blocked.stdout

    forced = runner.invoke(
        app, ["run", str(orch), "--state", str(out), "--resume", "x", "--force"]
    )
    assert forced.exit_code == 0, forced.stdout


def test_resume_refuses_a_state_with_no_document_hash_unless_forced(tmp_path: Path) -> None:
    """#270 F8: a state with no `runtime.last_run.document_hash` (never
    written by `cof run`, or predating this field) must refuse the same
    way a genuinely changed document does, not silently skip the check."""
    orch = _write(
        tmp_path / "chain.yml", "effects:\n  - type: tool\n    name: s\n    provider: uuid\n"
    )
    handwritten = tmp_path / "handwritten.json"
    handwritten.write_text(json.dumps({"prime": {}}), encoding="utf-8")

    blocked = runner.invoke(
        app, ["run", str(orch), "--state", str(handwritten), "--resume", "x"]
    )
    assert blocked.exit_code == 1
    assert "document_hash" in blocked.stdout

    forced = runner.invoke(
        app, ["run", str(orch), "--state", str(handwritten), "--resume", "x", "--force"]
    )
    assert forced.exit_code == 0, forced.stdout


def test_resume_defaults_out_to_the_state_file_when_not_given(tmp_path: Path) -> None:
    """#270 F10: a resumed run with neither --out nor a profile `out:` must
    still save its own progress somewhere — back to the file its state
    came from — rather than risking losing it all again."""
    orch = _two_step_orch(tmp_path)
    run_json = tmp_path / "run.json"

    first = runner.invoke(app, ["run", str(orch), "--out", str(run_json)])
    assert first.exit_code == 1, first.stdout
    before = json.loads(run_json.read_text(encoding="utf-8"))
    assert before["prime"]["step1"]["meta"]["completed_at"]

    second = runner.invoke(
        app, ["run", str(orch), "--state", str(run_json), "--resume", "x"]
    )
    assert second.exit_code == 1, second.stdout
    # Written back to run.json even though --out wasn't passed this time.
    after = json.loads(run_json.read_text(encoding="utf-8"))
    assert after["prime"]["step2"]["meta"]["error"]


def test_resume_last_does_not_replay_a_redacted_secret_as_a_literal_value(
    tmp_path: Path,
) -> None:
    """#270 F9: `--resume last` must not feed the stash's redaction marker
    back in as if it were the real `-e` value — the resumed state (loaded
    straight from --out) already carries the real one."""
    orch = _write(
        tmp_path / "chain.yml",
        """
interface:
  inputs:
    api_key: {type: string, required: true}
effects:
  - type: tool
    name: step1
    provider: json
    params:
      mode: parse
      input: "not valid json"
""".lstrip(),
    )
    run_json = tmp_path / "run.json"

    first = runner.invoke(
        app,
        ["run", str(orch), "-e", "api_key=realsecret123", "--out", str(run_json)],
    )
    assert first.exit_code == 1, first.stdout
    before = json.loads(run_json.read_text(encoding="utf-8"))
    assert before["input"]["api_key"] == "realsecret123"

    second = runner.invoke(app, ["run", str(orch), "--resume", "last"])
    assert second.exit_code == 1, second.stdout
    after = json.loads(run_json.read_text(encoding="utf-8"))
    assert after["input"]["api_key"] == "realsecret123"
