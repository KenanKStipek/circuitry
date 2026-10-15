"""A prompt's text read at dispatch from a file named in state (#578)."""

from __future__ import annotations

import hashlib
import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

from circuitry.adapters.base import GenerateOptions, GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store

MARKER = "DISPATCH-FILE-MARKER-31bc"


@dataclass
class RecordingAdapter:
    name: str = "rec"
    fail_first: int = 0
    before_reply: Any = None
    calls: list[dict[str, Any]] = field(default_factory=list)

    def generate(
        self,
        *,
        model: str,
        prompt: str,
        timeout_seconds: int = 120,
        options: GenerateOptions | None = None,
    ) -> GenerateResult:
        self.calls.append({"prompt": prompt, "options": options})
        if self.before_reply is not None:
            self.before_reply()
        if len(self.calls) <= self.fail_first:
            raise TimeoutError("boom")
        return GenerateResult(text="ok", raw={})


def _compile(
    effect: dict[str, Any],
    tmp_path: Path,
    *,
    trusted: bool = True,
    root: Path | None = None,
    document: bool = True,
) -> Any:
    return compile_orchestration(
        orch={"effects": [{"type": "prompt", "name": "ask", **effect}]},
        root_name="prime",
        document_dir=tmp_path if document else None,
        confinement_root=(root or tmp_path) if document else None,
        trusted=trusted,
    )


def _run(
    effect: dict[str, Any],
    tmp_path: Path,
    state: dict[str, Any],
    *,
    adapter: RecordingAdapter | None = None,
    runtime_config: dict[str, Any] | None = None,
    **compile_kwargs: Any,
) -> tuple[dict[str, Any], RecordingAdapter]:
    adapter = adapter or RecordingAdapter()
    store = Store(state)
    DynamicRuntime(
        _compile(effect, tmp_path, **compile_kwargs),
        adapter=adapter,
        model="m",
        runtime_config=runtime_config or {},
    ).execute(store=store)
    return store.get("prime.ask"), adapter


def _from(path_key: str = "input.prompt_file", within: Any = None) -> dict[str, Any]:
    return {"file": {"from": path_key}, "within": within if within is not None else {"from": "input.work"}}


def _state(tmp_path: Path, file: str = "p.txt") -> dict[str, Any]:
    return {"input": {"prompt_file": file, "work": str(tmp_path)}}


# ---------- reading ----------


def test_a_template_file_is_read_at_dispatch_and_sent_verbatim(tmp_path: Path) -> None:
    text = f"line one {{{{not rendered}}}} {MARKER}\r\nline two é🙂\n"
    (tmp_path / "p.txt").write_bytes(text.encode("utf-8"))
    node, adapter = _run({"template": _from()}, tmp_path, _state(tmp_path))

    assert adapter.calls[0]["prompt"] == text
    meta = node["meta"]
    assert meta["prompt_sent"] == text
    assert meta["template_files"] == [
        {
            "field": "template",
            "path": str((tmp_path / "p.txt").resolve()),
            "sha256": hashlib.sha256(text.encode("utf-8")).hexdigest(),
            "chars": len(text),
        }
    ]


def test_the_file_is_read_when_the_effect_dispatches_not_when_the_document_loads(tmp_path: Path) -> None:
    effect = {"template": _from()}
    definition = _compile(effect, tmp_path)  # no file exists yet: compiles fine
    (tmp_path / "p.txt").write_text("written later", encoding="utf-8")
    adapter = RecordingAdapter()
    store = Store(_state(tmp_path))
    DynamicRuntime(definition, adapter=adapter, model="m").execute(store=store)
    assert adapter.calls[0]["prompt"] == "written later"


def test_the_path_comes_from_state_written_by_an_earlier_effect(tmp_path: Path) -> None:
    (tmp_path / "built.txt").write_text("built at run time", encoding="utf-8")
    definition = compile_orchestration(
        orch={
            "effects": [
                {"type": "yield", "name": "prep", "template": "built.txt"},
                {
                    "type": "prompt",
                    "name": "ask",
                    "template": _from("prime.prep.value"),
                },
            ]
        },
        document_dir=tmp_path,
        confinement_root=tmp_path,
        trusted=True,
    )
    adapter = RecordingAdapter()
    DynamicRuntime(definition, adapter=adapter, model="m").execute(store=Store(_state(tmp_path)))
    assert adapter.calls[0]["prompt"] == "built at run time"


def test_a_relative_path_resolves_against_within_and_an_absolute_one_is_used_as_is(tmp_path: Path) -> None:
    (tmp_path / "sub").mkdir()
    (tmp_path / "sub" / "p.txt").write_text("in sub", encoding="utf-8")
    state = {"input": {"prompt_file": "p.txt", "work": str(tmp_path / "sub")}}
    _, adapter = _run({"template": _from()}, tmp_path, state)
    assert adapter.calls[0]["prompt"] == "in sub"

    state["input"]["prompt_file"] = str(tmp_path / "sub" / "p.txt")
    _, adapter = _run({"template": _from()}, tmp_path, state)
    assert adapter.calls[0]["prompt"] == "in sub"


def test_within_may_be_a_literal_absolute_path(tmp_path: Path) -> None:
    (tmp_path / "p.txt").write_text("literal", encoding="utf-8")
    _, adapter = _run(
        {"template": _from(within=str(tmp_path))},
        tmp_path,
        {"input": {"prompt_file": "p.txt"}},
    )
    assert adapter.calls[0]["prompt"] == "literal"


def test_message_contents_read_files_beside_rendered_ones_and_flatten(tmp_path: Path) -> None:
    (tmp_path / "user.txt").write_text("raw {{x}} body", encoding="utf-8")
    node, adapter = _run(
        {
            "messages": [
                {"role": "system", "content": "be {{input.tone}}"},
                {"role": "user", "content": _from("input.user_file")},
            ]
        },
        tmp_path,
        {"input": {"tone": "brief", "user_file": "user.txt", "work": str(tmp_path)}},
    )
    assert adapter.calls[0]["prompt"] == "system: be brief\n\nuser: raw {{x}} body"
    assert [m.content for m in adapter.calls[0]["options"].messages] == ["be brief", "raw {{x}} body"]
    records = node["meta"]["template_files"]
    assert [r["field"] for r in records] == ["messages[1].content"]
    assert records[0]["chars"] == len("raw {{x}} body")


def test_nothing_is_recorded_when_no_file_is_read(tmp_path: Path) -> None:
    node, _ = _run({"template": "plain"}, tmp_path, {})
    assert "template_files" not in node["meta"]


def test_retries_reuse_the_text_read_before_the_first_attempt(tmp_path: Path) -> None:
    target = tmp_path / "p.txt"
    target.write_text("first read", encoding="utf-8")
    adapter = RecordingAdapter(
        fail_first=1, before_reply=lambda: target.write_text("changed mid-run", encoding="utf-8")
    )
    node, _ = _run(
        {"template": _from(), "retries": {"max_attempts": 2, "backoff_ms": 1}},
        tmp_path,
        _state(tmp_path),
        adapter=adapter,
    )
    assert [call["prompt"] for call in adapter.calls] == ["first read", "first read"]
    assert node["meta"]["template_files"][0]["chars"] == len("first read")


def test_with_sha256_a_prompt_built_in_a_file_leaves_no_copy_in_state(tmp_path: Path) -> None:
    (tmp_path / "p.txt").write_text(f"{MARKER} body", encoding="utf-8")
    node, adapter = _run(
        {"template": _from()},
        tmp_path,
        _state(tmp_path),
        runtime_config={"state": {"prompt_sent": "sha256"}},
    )
    assert adapter.calls[0]["prompt"] == f"{MARKER} body"
    assert MARKER not in repr(node)


# ---------- errors ----------


def _fails(tmp_path: Path, state: dict[str, Any], effect: dict[str, Any] | None = None) -> str:
    node, adapter = _run(
        {"template": _from(), "on_error": "continue", **(effect or {})}, tmp_path, state
    )
    assert node["value"] is None
    assert adapter.calls == []
    assert "template_files" not in node["meta"]
    return str(node["meta"]["error"])


def test_a_missing_file_fails_the_effect_naming_effect_field_and_path(tmp_path: Path) -> None:
    error = _fails(tmp_path, _state(tmp_path, "nope.txt"))
    assert error == "Prompt effect 'ask': template: file 'nope.txt' does not exist."


def test_a_directory_is_not_a_regular_file(tmp_path: Path) -> None:
    (tmp_path / "d").mkdir()
    error = _fails(tmp_path, _state(tmp_path, "d"))
    assert error == "Prompt effect 'ask': template: file 'd' is not a regular file."


def test_a_file_outside_within_is_refused_even_through_dot_dot_or_a_symlink(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    (tmp_path / "secret.txt").write_text("outside", encoding="utf-8")
    (work / "link.txt").symlink_to(tmp_path / "secret.txt")
    state = {"input": {"prompt_file": "../secret.txt", "work": str(work)}}
    assert "outside 'within'" in _fails(tmp_path, state)
    state["input"]["prompt_file"] = "link.txt"
    assert "outside 'within'" in _fails(tmp_path, state)
    state["input"]["prompt_file"] = str(tmp_path / "secret.txt")
    assert "outside 'within'" in _fails(tmp_path, state)


def test_a_file_over_one_mebibyte_is_refused_and_exactly_one_is_read(tmp_path: Path) -> None:
    (tmp_path / "big.txt").write_bytes(b"a" * (1024 * 1024 + 1))
    assert "over the 1048576-byte limit" in _fails(tmp_path, _state(tmp_path, "big.txt"))
    (tmp_path / "ok.txt").write_bytes(b"a" * (1024 * 1024))
    node, adapter = _run({"template": _from()}, tmp_path, _state(tmp_path, "ok.txt"))
    assert len(adapter.calls[0]["prompt"]) == 1024 * 1024
    assert node["meta"]["template_files"][0]["chars"] == 1024 * 1024


def test_a_file_that_is_not_utf8_is_refused(tmp_path: Path) -> None:
    (tmp_path / "bin.txt").write_bytes(b"\xff\xfe\x00bad")
    assert "is not valid UTF-8" in _fails(tmp_path, _state(tmp_path, "bin.txt"))


@pytest.mark.skipif(os.geteuid() == 0, reason="root reads any file")
def test_an_unreadable_file_is_refused(tmp_path: Path) -> None:
    target = tmp_path / "locked.txt"
    target.write_text("x", encoding="utf-8")
    target.chmod(0)
    try:
        assert "could not be read" in _fails(tmp_path, _state(tmp_path, "locked.txt"))
    finally:
        target.chmod(0o600)


@pytest.mark.parametrize("value", [None, 5, ["p.txt"], "", "   "])
def test_a_state_value_that_is_not_a_path_string_fails(tmp_path: Path, value: Any) -> None:
    state = {"input": {"prompt_file": value, "work": str(tmp_path)}}
    error = _fails(tmp_path, state)
    assert error.startswith(
        "Prompt effect 'ask': template: 'file: {from: input.prompt_file}' resolved to "
    )
    assert "expected a file path string" in error


@pytest.mark.parametrize("within", [None, "relative/dir", 5])
def test_a_within_that_is_not_an_absolute_path_fails(tmp_path: Path, within: Any) -> None:
    state = {"input": {"prompt_file": "p.txt", "work": within}}
    assert "expected an absolute directory path" in _fails(tmp_path, state)


def test_a_failure_in_a_messages_prompt_names_that_message(tmp_path: Path) -> None:
    node, _ = _run(
        {
            "messages": [
                {"role": "system", "content": "ok"},
                {"role": "user", "content": _from()},
            ],
            "on_error": "continue",
        },
        tmp_path,
        _state(tmp_path, "gone.txt"),
    )
    assert node["meta"]["error"] == (
        "Prompt effect 'ask': messages[1].content: file 'gone.txt' does not exist."
    )


def test_under_on_error_fail_the_run_stops(tmp_path: Path) -> None:
    with pytest.raises(RuntimeError, match=r"template: file 'gone\.txt' does not exist"):
        _run({"template": _from()}, tmp_path, _state(tmp_path, "gone.txt"))


# ---------- checking the form ----------


@pytest.mark.parametrize(
    ("template", "message"),
    [
        ({"file": {"from": "input.f"}}, "'within' is required"),
        ({"file": {"from": "input.f"}, "within": "relative"}, "'within' must be an absolute path"),
        ({"file": {"from": "input.f"}, "within": "/srv/{{x}}"}, "cannot contain a template tag"),
        ({"file": {"from": "input.f"}, "within": {"from": "prime.prep.value"}}, "never an effect's output"),
        ({"file": {"from": "input.f"}, "within": {"from": "input.a.b"}}, "never an effect's output"),
        ({"file": {"from": "input.f"}, "within": {"from": "input.d", "x": 1}}, "never an effect's output"),
        ({"file": {"from": "input.f"}, "within": 5}, "'within' must be an absolute path"),
        ({"file": {"from": ""}, "within": {"from": "input.d"}}, "'file' must be a path"),
        ({"file": {"from": 5}, "within": {"from": "input.d"}}, "'file' must be a path"),
        ({"file": {}, "within": {"from": "input.d"}}, "'file' must be a path"),
        ({"file": {"from": "input.f", "x": 1}, "within": {"from": "input.d"}}, "'file' must be a path"),
        ({"file": {"from": "bare"}, "within": {"from": "input.d"}}, "is not rooted at a state namespace"),
        ({"file": {"from": "input.f"}, "within": {"from": "input.d"}, "x": 1}, "takes only 'file' and 'within'"),
    ],
)
def test_check_rejects_a_malformed_source_naming_the_field(
    tmp_path: Path, template: dict[str, Any], message: str
) -> None:
    with pytest.raises(ValueError, match=r"prime\.effects") as excinfo:
        _compile({"template": template}, tmp_path)
    assert ".template" in str(excinfo.value)
    assert message in str(excinfo.value)


def test_check_names_the_message_field_in_a_messages_prompt(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match=r"messages\[1\]\.content: 'within' is required"):
        _compile(
            {"messages": [{"role": "user", "content": "hi"}, {"role": "user", "content": {"file": {"from": "input.f"}}}]},
            tmp_path,
        )


def test_it_is_not_allowed_in_a_yield_or_the_prompts_map(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="only for a prompt effect"):
        compile_orchestration(
            orch={"effects": [{"type": "yield", "name": "y", "template": _from()}]},
            document_dir=tmp_path,
            confinement_root=tmp_path,
            trusted=True,
        )
    with pytest.raises(ValueError, match="only for a prompt effect"):
        compile_orchestration(
            orch={"prompts": {"p": _from()}, "effects": [{"type": "prompt", "name": "a", "template": "{{> p}}"}]},
            document_dir=tmp_path,
            confinement_root=tmp_path,
            trusted=True,
        )


@pytest.mark.parametrize(
    ("plan_prompt", "field"),
    [
        ({"template": _from()}, "template"),
        (
            {"messages": [{"role": "system", "content": "plan"}, {"role": "user", "content": _from()}]},
            r"messages\[1\]\.content",
        ),
    ],
)
def test_a_reflectors_plan_prompt_cannot_read_its_text_from_a_file_in_state(
    tmp_path: Path, plan_prompt: dict[str, Any], field: str
) -> None:
    reflector = {
        "type": "reflector",
        "name": "r",
        "effects": [{"type": "prompt", "name": "propose_steps", **plan_prompt}],
    }
    with pytest.raises(ValueError, match=rf"Reflector effect 'r' .*plan prompt 'propose_steps' reads {field} "):
        compile_orchestration(
            orch={"effects": [reflector]}, document_dir=tmp_path, confinement_root=tmp_path, trusted=True
        )
    # Another prompt in the reflector may still read a file at dispatch.
    reflector["effects"] = [
        {"type": "prompt", "name": "propose_steps", "template": "plan"},
        {"type": "prompt", "name": "other", "template": _from()},
    ]
    compile_orchestration(
        orch={"effects": [reflector]}, document_dir=tmp_path, confinement_root=tmp_path, trusted=True
    )


def test_a_generated_document_cannot_use_it(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="this document has no file of its own"):
        _compile({"template": _from()}, tmp_path, document=False)


def test_the_form_is_accepted_for_a_loop_binding_path(tmp_path: Path) -> None:
    (tmp_path / "a.txt").write_text("A", encoding="utf-8")
    definition = compile_orchestration(
        orch={
            "effects": [
                {
                    "type": "loop",
                    "name": "each_file",
                    "each": {"in": "input.files", "as": "f"},
                    "body": [{"type": "prompt", "name": "ask", "template": _from("f")}],
                }
            ]
        },
        document_dir=tmp_path,
        confinement_root=tmp_path,
        trusted=True,
    )
    adapter = RecordingAdapter()
    DynamicRuntime(definition, adapter=adapter, model="m").execute(
        store=Store({"input": {"files": ["a.txt"], "work": str(tmp_path)}})
    )
    assert adapter.calls[0]["prompt"] == "A"


# ---------- trust ----------


def test_a_limited_document_can_name_a_literal_within_only_inside_its_project(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    outside = tmp_path / "elsewhere"
    outside.mkdir()
    _compile({"template": _from(within=str(project / "data"))}, project, trusted=False)
    with pytest.raises(ValueError, match="resolves outside the project"):
        _compile({"template": _from(within=str(outside))}, project, trusted=False)
    with pytest.raises(ValueError, match="resolves outside the project"):
        _compile({"template": _from(within=str(project / ".." / "elsewhere"))}, project, trusted=False)


def test_a_document_run_by_path_can_name_any_directory(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    outside = tmp_path / "elsewhere"
    outside.mkdir()
    (outside / "p.txt").write_text("elsewhere", encoding="utf-8")
    _, adapter = _run(
        {"template": _from(within=str(outside))},
        project,
        {"input": {"prompt_file": "p.txt"}},
        trusted=True,
    )
    assert adapter.calls[0]["prompt"] == "elsewhere"


def test_a_limited_documents_input_within_is_checked_against_its_project_at_dispatch(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    (project / "data").mkdir(parents=True)
    (project / "data" / "p.txt").write_text("inside", encoding="utf-8")
    outside = tmp_path / "elsewhere"
    outside.mkdir()
    (outside / "p.txt").write_text("outside", encoding="utf-8")

    state = {"input": {"prompt_file": "p.txt", "work": str(project / "data")}}
    _, adapter = _run({"template": _from()}, project, state, trusted=False)
    assert adapter.calls[0]["prompt"] == "inside"

    state["input"]["work"] = str(outside)
    node, adapter = _run(
        {"template": _from(), "on_error": "continue"}, project, state, trusted=False
    )
    assert adapter.calls == []
    assert "resolves outside the project" in node["meta"]["error"]
