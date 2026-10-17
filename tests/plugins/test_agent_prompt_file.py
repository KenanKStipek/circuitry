"""The ``agent`` tool reads ``params.prompt`` from a file named in state at dispatch (#591).

``prompt: {file: {from: <state path>}, within: <dir>}`` takes #578's form and
rules: read once before the first attempt, sent verbatim, recorded in
``meta.template_files``, never in state. Every CLI here is a fake script; no
test runs a real CLI or calls a real model.
"""

from __future__ import annotations

import hashlib
import json
import os
import tempfile
from pathlib import Path
from typing import Any

import pytest
from agent_cli_test_support import write_fake_cli

from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store

MARKER = "AGENT-PROMPT-FILE-MARKER-7e2a"
TEXT = f"{MARKER}: rename the module. Keep {{{{literal}}}} braces.\n"

#: Logs each call (argv, stdin, the text of every ``@<file>``) to
#: ``FAKE_AGENT_LOG``. With ``FAKE_FAIL_FIRST`` set, the first call rewrites
#: that file and exits 1. On a resumed call it writes ``FAKE_RESULT_REPAIR``
#: to ``FAKE_RESULT_PATH`` when both are set. Then prints pi's event stream
#: (``--mode`` in argv) or Claude Code's stream-json.
FAKE_BODY = """\
argv = sys.argv[1:]
is_pi = "--mode" in argv
resume_flag = "--session" if is_pi else "--resume"
resumed = argv[argv.index(resume_flag) + 1] if resume_flag in argv else None
attachments = []
for arg in argv:
    if arg.startswith("@"):
        with open(arg[1:], encoding="utf-8") as handle:
            attachments.append(handle.read())
log = os.environ["FAKE_AGENT_LOG"]
earlier = 0
if os.path.exists(log):
    with open(log, encoding="utf-8") as handle:
        earlier = len(handle.read().splitlines())
with open(log, "a", encoding="utf-8") as handle:
    handle.write(json.dumps({"argv": argv, "stdin": stdin, "attachments": attachments}) + "\\n")
if earlier == 0 and os.environ.get("FAKE_FAIL_FIRST"):
    with open(os.environ["FAKE_FAIL_FIRST"], "w", encoding="utf-8") as handle:
        handle.write("rewritten after the first attempt")
    sys.stderr.write("transient failure")
    sys.exit(1)
if resumed and os.environ.get("FAKE_RESULT_REPAIR"):
    with open(os.environ["FAKE_RESULT_PATH"], "w", encoding="utf-8") as handle:
        handle.write(os.environ["FAKE_RESULT_REPAIR"])
session = resumed or "sess-abc"
if is_pi:
    usage = {"input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0, "cost": {"total": 0.01}}
    events = [
        {"type": "session", "id": session, "cwd": os.getcwd()},
        {"type": "message_end", "message": {"role": "assistant", "usage": usage,
            "content": [{"type": "text", "text": "done"}], "stopReason": "stop"}},
        {"type": "agent_end"},
    ]
else:
    events = [
        {"type": "system", "subtype": "init", "session_id": session},
        {"type": "assistant", "message": {"content": [{"type": "text", "text": "done"}]}},
        {"type": "result", "subtype": "success", "is_error": False, "result": "done",
         "session_id": session, "num_turns": 1, "total_cost_usd": 0.02,
         "usage": {"input_tokens": 7, "output_tokens": 3, "cache_read_input_tokens": 0,
                   "cache_creation_input_tokens": 0}},
    ]
for event in events:
    sys.stdout.write(json.dumps(event) + "\\n")
sys.exit(0)
"""


@pytest.fixture(autouse=True)
def _scratch_in_tmp_path(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """The plugin's scratch dirs (prompt files, transcript) land in tmp_path."""
    scratch = tmp_path / "scratch"
    scratch.mkdir()
    monkeypatch.setattr(tempfile, "tempdir", str(scratch))


@pytest.fixture
def work(tmp_path: Path) -> Path:
    """The agent's cwd, which also holds the prompt file."""
    path = tmp_path / "work"
    path.mkdir()
    (path / "task.md").write_text(TEXT, encoding="utf-8")
    return path


def _prompt_from(within: Any = None) -> dict[str, Any]:
    return {
        "file": {"from": "input.prompt_file"},
        "within": within if within is not None else {"from": "input.work"},
    }


def _effect(work: Path, tmp_path: Path, **overrides: Any) -> dict[str, Any]:
    params = {
        "prompt": _prompt_from(),
        "cwd": str(work),
        "env": {
            "FAKE_AGENT_LOG": str(tmp_path / "calls.jsonl"),
            "FAKE_RESULT_PATH": str(work / "result.json"),
        },
    }
    params.update(overrides.pop("params", {}))
    return {
        "type": "tool",
        "name": "implement",
        "provider": "agent",
        "timeout_ms": 30000,
        "params": params,
        **overrides,
    }


def _compile(
    effects: list[dict[str, Any]],
    project: Path,
    *,
    trusted: bool = True,
    document: bool = True,
) -> Any:
    return compile_orchestration(
        orch={"effects": effects},
        document_dir=project if document else None,
        confinement_root=project if document else None,
        trusted=trusted,
    )


def _run(
    effect: dict[str, Any],
    tmp_path: Path,
    state: dict[str, Any],
    *,
    engine: str = "pi",
    trusted: bool = True,
    project: Path | None = None,
) -> dict[str, Any]:
    fake = write_fake_cli(tmp_path, "pi" if engine == "pi" else "claude", body=FAKE_BODY)
    store = Store(state)
    DynamicRuntime(
        _compile([effect], project or tmp_path, trusted=trusted),
        adapter=None,
        model="test",
        runtime_config={"plugins": {"agent": {"engine": engine, engine: {"binary": str(fake)}}}},
    ).execute(store=store)
    node: dict[str, Any] = store.state["prime"]["implement"]
    return node


def _state(work: Path, prompt_file: Any = "task.md") -> dict[str, Any]:
    return {"input": {"prompt_file": prompt_file, "work": str(work)}}


def _calls(tmp_path: Path) -> list[dict[str, Any]]:
    log = tmp_path / "calls.jsonl"
    if not log.exists():
        return []
    return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]


# ---------- the text reaches the session, and stays out of state ----------


def test_pi_receives_the_files_text_verbatim_and_state_records_only_the_file(
    tmp_path: Path, work: Path
) -> None:
    node = _run(_effect(work, tmp_path), tmp_path, _state(work))
    assert node["meta"]["error"] is None
    assert node["value"] == "done"
    assert _calls(tmp_path)[0]["attachments"] == [TEXT]
    resolved = str((work / "task.md").resolve())
    assert node["meta"]["template_files"] == [
        {
            "field": "params.prompt",
            "path": resolved,
            "sha256": hashlib.sha256(TEXT.encode("utf-8")).hexdigest(),
            "chars": len(TEXT),
        }
    ]
    assert node["meta"]["params_rendered"]["prompt"] == {
        "file": resolved,
        "within": str(work.resolve()),
    }
    assert MARKER not in json.dumps(node, default=str)


def test_claude_code_receives_the_files_text_on_stdin(tmp_path: Path, work: Path) -> None:
    node = _run(_effect(work, tmp_path), tmp_path, _state(work), engine="claude_code")
    assert node["meta"]["error"] is None
    assert _calls(tmp_path)[0]["stdin"] == TEXT
    assert MARKER not in json.dumps(node, default=str)


def test_the_path_can_come_from_an_earlier_effect_and_a_literal_within(
    tmp_path: Path, work: Path
) -> None:
    prep = {
        "type": "tool",
        "name": "prep",
        "provider": "json",
        "params": {"mode": "parse", "input": '{"prompt_file": "task.md"}'},
    }
    effect = _effect(
        work,
        tmp_path,
        params={"prompt": {"file": {"from": "prime.prep.value.prompt_file"}, "within": str(work)}},
    )
    fake = write_fake_cli(tmp_path, "pi", body=FAKE_BODY)
    store = Store({"input": {}})
    DynamicRuntime(
        _compile([prep, effect], tmp_path),
        adapter=None,
        model="test",
        runtime_config={"plugins": {"agent": {"pi": {"binary": str(fake)}}}},
    ).execute(store=store)
    assert store.state["prime"]["implement"]["meta"]["error"] is None
    assert _calls(tmp_path)[0]["attachments"] == [TEXT]


def test_a_retry_uses_the_text_read_before_the_first_attempt(tmp_path: Path, work: Path) -> None:
    effect = _effect(work, tmp_path, retries={"max_attempts": 2, "backoff_ms": 1})
    effect["params"]["env"]["FAKE_FAIL_FIRST"] = str(work / "task.md")
    node = _run(effect, tmp_path, _state(work))
    assert node["meta"]["error"] is None
    assert node["meta"]["retries_used"] == 1
    calls = _calls(tmp_path)
    assert len(calls) == 2
    # The first attempt rewrote the file; the retry still sends the text read before it.
    assert (work / "task.md").read_text(encoding="utf-8") == "rewritten after the first attempt"
    assert calls[1]["attachments"] == [calls[0]["attachments"][0]]
    assert TEXT in calls[1]["attachments"][0]
    assert node["meta"]["template_files"][0]["chars"] == len(TEXT)


def test_the_repair_turn_continues_the_same_session(tmp_path: Path, work: Path) -> None:
    effect = _effect(work, tmp_path, params={"result_file": "result.json"})
    effect["params"]["env"]["FAKE_RESULT_REPAIR"] = '{"ok": true}'
    node = _run(effect, tmp_path, _state(work))
    assert node["meta"]["error"] is None
    assert node["value"] == {"ok": True}
    first, repair = _calls(tmp_path)
    assert TEXT.rstrip("\n") in first["attachments"][0]
    assert repair["argv"][repair["argv"].index("--session") + 1] == "sess-abc"
    assert MARKER not in json.dumps(node, default=str)


def test_a_string_prompt_is_unchanged(tmp_path: Path, work: Path) -> None:
    effect = _effect(work, tmp_path, params={"prompt": "Task: {{input.task}}"})
    node = _run(effect, tmp_path, {"input": {"task": "rename"}})
    assert node["meta"]["error"] is None
    assert _calls(tmp_path)[0]["attachments"] == ["Task: rename"]
    assert node["meta"]["params_rendered"]["prompt"] == "Task: rename"
    assert "template_files" not in node["meta"]


# ---------- errors fail the effect, before the CLI runs, and are not retried ----------


def _fails(
    tmp_path: Path, work: Path, state: dict[str, Any], **run_kwargs: Any
) -> str:
    effect = _effect(
        work, tmp_path, on_error="continue", retries={"max_attempts": 3, "backoff_ms": 1}
    )
    node = _run(effect, tmp_path, state, **run_kwargs)
    assert node["value"] is None
    assert _calls(tmp_path) == []
    assert "template_files" not in node["meta"]
    assert "retries_used" not in node["meta"]
    return str(node["meta"]["error"])


def test_a_missing_file_names_the_effect_the_field_and_the_path(tmp_path: Path, work: Path) -> None:
    error = _fails(tmp_path, work, _state(work, "nope.md"))
    assert error == "Tool effect 'implement': params.prompt: file 'nope.md' does not exist."


def test_a_directory_is_not_a_regular_file(tmp_path: Path, work: Path) -> None:
    (work / "dir").mkdir()
    error = _fails(tmp_path, work, _state(work, "dir"))
    assert error == "Tool effect 'implement': params.prompt: file 'dir' is not a regular file."


def test_a_file_outside_within_is_refused_even_through_dot_dot_or_a_symlink(
    tmp_path: Path, work: Path
) -> None:
    (tmp_path / "secret.md").write_text("outside", encoding="utf-8")
    (work / "link.md").symlink_to(tmp_path / "secret.md")
    for name in ("../secret.md", "link.md", str(tmp_path / "secret.md")):
        error = _fails(tmp_path, work, _state(work, name))
        assert error.startswith("Tool effect 'implement': params.prompt: file ")
        assert f"{name!r} resolves outside 'within'" in error


def test_a_file_over_one_mebibyte_is_refused(tmp_path: Path, work: Path) -> None:
    (work / "big.md").write_bytes(b"a" * (1024 * 1024 + 1))
    error = _fails(tmp_path, work, _state(work, "big.md"))
    assert error == (
        "Tool effect 'implement': params.prompt: file 'big.md' is over the "
        "1048576-byte limit for a prompt file."
    )


def test_a_file_that_is_not_utf8_is_refused(tmp_path: Path, work: Path) -> None:
    (work / "bin.md").write_bytes(b"\xff\xfe\x00bad")
    error = _fails(tmp_path, work, _state(work, "bin.md"))
    assert error.startswith("Tool effect 'implement': params.prompt: file 'bin.md' is not valid UTF-8")


@pytest.mark.skipif(os.geteuid() == 0, reason="root reads any file")
def test_an_unreadable_file_is_refused(tmp_path: Path, work: Path) -> None:
    locked = work / "locked.md"
    locked.write_text("x", encoding="utf-8")
    locked.chmod(0)
    try:
        error = _fails(tmp_path, work, _state(work, "locked.md"))
    finally:
        locked.chmod(0o600)
    assert error.startswith("Tool effect 'implement': params.prompt: file 'locked.md' could not be read")


@pytest.mark.parametrize("value", [None, 5, ""])
def test_a_state_value_that_is_not_a_path_string_fails(
    tmp_path: Path, work: Path, value: Any
) -> None:
    error = _fails(tmp_path, work, _state(work, value))
    assert error.startswith(
        "Tool effect 'implement': params.prompt: 'file: {from: input.prompt_file}' resolved to "
    )


def test_under_on_error_fail_the_run_stops(tmp_path: Path, work: Path) -> None:
    with pytest.raises(RuntimeError, match=r"Tool effect .implement.: params\.prompt: file .nope\.md."):
        _run(_effect(work, tmp_path), tmp_path, _state(work, "nope.md"))


# ---------- trust ----------


def test_a_limited_documents_within_stays_inside_its_project(tmp_path: Path, work: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    with pytest.raises(ValueError, match="resolves outside the project"):
        _compile(
            [_effect(work, tmp_path, params={"prompt": _prompt_from(str(work))})],
            project,
            trusted=False,
        )
    # Run by path, the same document may name any directory.
    _compile(
        [_effect(work, tmp_path, params={"prompt": _prompt_from(str(work))})], project
    )
    # An input `within` is checked against the project when the effect dispatches.
    error = _fails(tmp_path, work, _state(work), trusted=False, project=project)
    assert "resolves outside the project" in error


def test_a_generated_document_cannot_use_it(tmp_path: Path, work: Path) -> None:
    with pytest.raises(ValueError, match="this document has no file of its own"):
        _compile([_effect(work, tmp_path)], tmp_path, document=False)


@pytest.mark.parametrize(
    ("within", "message"),
    [
        (None, "'within' is required"),
        ("relative/dir", "'within' must be an absolute path"),
        ("/srv/{{input.x}}", "cannot contain a template tag"),
        ({"from": "prime.prep.value"}, "never an effect's output"),
    ],
)
def test_check_rejects_a_bad_within_naming_params_prompt(
    tmp_path: Path, work: Path, within: Any, message: str
) -> None:
    prompt = {"file": {"from": "input.prompt_file"}}
    if within is not None:
        prompt["within"] = within
    with pytest.raises(ValueError) as excinfo:
        _compile([_effect(work, tmp_path, params={"prompt": prompt})], tmp_path)
    assert "params.prompt" in str(excinfo.value)
    assert message in str(excinfo.value)


def test_another_provider_keeps_a_prompt_mapping_as_a_plain_param(tmp_path: Path) -> None:
    definition = _compile(
        [
            {
                "type": "tool",
                "name": "t",
                "provider": "json",
                "params": {"mode": "stringify", "input": 1, "prompt": _prompt_from()},
            }
        ],
        tmp_path,
    )
    tool = definition.effects[0]
    assert tool.prompt_file is None
    assert tool.params["prompt"] == _prompt_from()


# ---------- params_json never supplies the file form ----------

_FILE_FORM_JSON = json.dumps(
    {"prompt": {"file": {"from": "input.prompt_file"}, "within": "/srv/work"}}
)


def test_check_rejects_a_params_json_that_supplies_the_file_form(
    tmp_path: Path, work: Path
) -> None:
    effect = _effect(work, tmp_path, params_json=_FILE_FORM_JSON)
    effect["params"].pop("prompt")
    with pytest.raises(ValueError) as excinfo:
        _compile([effect], tmp_path)
    assert "params_json must not set params.prompt to {file:" in str(excinfo.value)


def test_check_rejects_a_params_json_that_replaces_a_prompt_read_from_a_file(
    tmp_path: Path, work: Path
) -> None:
    effect = _effect(work, tmp_path, params_json='{"prompt": "{{input.task}}"}')
    with pytest.raises(ValueError) as excinfo:
        _compile([effect], tmp_path)
    assert "params_json must not set params.prompt: this effect reads params.prompt" in str(
        excinfo.value
    )


def test_a_rendered_params_json_that_supplies_the_file_form_fails_at_dispatch(
    tmp_path: Path, work: Path
) -> None:
    # Not JSON before rendering, so `cof check` cannot see the form.
    effect = _effect(work, tmp_path, params_json="{{{input.overlay}}}", on_error="continue")
    effect["params"].pop("prompt")
    state = {"input": {"overlay": _FILE_FORM_JSON, "prompt_file": "task.md", "work": str(work)}}
    node = _run(effect, tmp_path, state)
    assert "params_json must not set params.prompt to {file:" in node["meta"]["error"]
    assert _calls(tmp_path) == []


def test_a_rendered_params_json_cannot_replace_a_prompt_read_from_a_file(
    tmp_path: Path, work: Path
) -> None:
    effect = _effect(work, tmp_path, params_json="{{{input.overlay}}}", on_error="continue")
    state = {**_state(work), "input": {**_state(work)["input"], "overlay": '{"prompt": "other"}'}}
    node = _run(effect, tmp_path, state)
    assert "params_json must not set params.prompt: this effect reads" in node["meta"]["error"]
    assert _calls(tmp_path) == []


def test_params_json_may_supply_the_other_params_next_to_a_prompt_file(
    tmp_path: Path, work: Path
) -> None:
    effect = _effect(work, tmp_path, params_json="{{{prime.plan.value}}}")
    cwd = effect["params"].pop("cwd")
    plan = {
        "type": "tool",
        "name": "plan",
        "provider": "json",
        "params": {
            "mode": "stringify",
            "input": {"engine": "claude_code", "cwd": cwd, "model": "sonnet", "tools": ["Read"]},
        },
    }
    claude = write_fake_cli(tmp_path, "claude", body=FAKE_BODY)
    store = Store(_state(work))
    DynamicRuntime(
        _compile([plan, effect], tmp_path),
        adapter=None,
        model="test",
        runtime_config={"plugins": {"agent": {"claude_code": {"binary": str(claude)}}}},
    ).execute(store=store)
    node = store.state["prime"]["implement"]
    assert node["meta"]["error"] is None
    call = _calls(tmp_path)[0]
    assert call["stdin"] == TEXT
    assert "--tools" in call["argv"]
    assert node["meta"]["params_rendered"]["engine"] == "claude_code"
    assert node["meta"]["params_rendered"]["prompt"]["file"] == str((work / "task.md").resolve())


def test_a_string_prompt_from_params_json_is_unchanged(tmp_path: Path, work: Path) -> None:
    effect = _effect(work, tmp_path, params_json='{"prompt": "Task from JSON"}')
    effect["params"].pop("prompt")
    node = _run(effect, tmp_path, {"input": {}})
    assert node["meta"]["error"] is None
    assert _calls(tmp_path)[0]["attachments"] == ["Task from JSON"]
