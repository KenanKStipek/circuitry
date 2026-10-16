"""`resume: restart` on `dynamic` and `use` (#590).

Under `cof run --resume`, a scope with ``resume: restart`` whose saved node
finished without error is skipped whole, as before. Otherwise it runs from
its first effect in a fresh state, its saved children not reused, and its
``finally:`` runs as usual at its end. A ``use`` runs its child from its
start, even when a saved child record (``runtime.state.record_children``)
matches. This holds at any depth. ``resume: continue`` (the default) keeps
the earlier behaviour: continue at the first unfinished effect.

Each resumed run starts from the first run's state as saved (`--out`'s JSON
form), the way `cof run --resume` reads it back.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.interrupts import SigTermInterrupt
from circuitry.cli.runtime_shim import RunRequest, RunResult, run, validate
from circuitry.core.compiler import compile_orchestration
from circuitry.core.saved_state import dumps_saved_state

RECORD = {"state": {"record_children": True}}


class LoggingAdapter:
    """Logs what each prompt does, the way the issue's shell commands do.

    A prompt starting with ``work`` logs ``work-start`` and ``work-end``
    around its work; any other logs its own text. A prompt in ``interrupt``
    (or ``fail``) is stopped by SIGTERM (or fails) once, after ``work-start``.
    """

    name = "logging"

    def __init__(self) -> None:
        self.log: list[str] = []
        self.interrupt: set[str] = set()
        self.fail: set[str] = set()

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        work = prompt.startswith("work")
        self.log.append(prompt.replace("work", "work-start", 1) if work else prompt)
        if prompt in self.interrupt:
            self.interrupt.discard(prompt)
            raise SigTermInterrupt()
        if prompt in self.fail:
            self.fail.discard(prompt)
            raise RuntimeError(f"boom: {prompt}")
        if work:
            self.log.append(prompt.replace("work", "work-end", 1))
        return GenerateResult(text=f"ok:{prompt}", raw={})


def _write(tmp_path: Path, name: str, content: dict[str, Any]) -> Path:
    path = tmp_path / name
    path.write_text(yaml.safe_dump(content, sort_keys=False), encoding="utf-8")
    return path


def _prompt(name: str, template: str) -> dict[str, Any]:
    return {"type": "prompt", "name": name, "template": template}


def _session(suffix: str = "", *, resume: str | None = "restart") -> dict[str, Any]:
    """The issue's scope: acquire, work, and a finally that releases."""
    session: dict[str, Any] = {
        "type": "dynamic",
        "name": "session",
        "effects": [
            _prompt("acquire", f"acquire{suffix}"),
            _prompt("work", f"work{suffix}"),
        ],
        "finally": [_prompt("release", f"release{suffix}")],
    }
    if resume is not None:
        session["resume"] = resume
    return session


def _run(
    orch: Path,
    adapter: LoggingAdapter,
    *,
    inputs: dict[str, Any] | None = None,
    saved: dict[str, Any] | None = None,
) -> RunResult:
    return run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state={"input": dict(inputs or {})} if saved is None else saved,
            resume=saved is not None,
        )
    )


def _saved(result: RunResult) -> dict[str, Any]:
    """*result*'s state as `--out` writes it and `--resume` reads it back."""
    loaded: dict[str, Any] = json.loads(dumps_saved_state(result.state))
    return loaded


def _resume(orch: Path, adapter: LoggingAdapter, first: RunResult) -> RunResult:
    second = _run(orch, adapter, saved=_saved(first))
    assert second.ok is True, second.error
    return second


# -- the issue's reproduction --------------------------------------------------


def test_a_restart_dynamic_stopped_mid_scope_runs_again_from_its_first_effect(
    tmp_path: Path,
) -> None:
    orch = _write(tmp_path, "probe.yml", {"effects": [_session()]})
    adapter = LoggingAdapter()
    adapter.interrupt = {"work"}

    first = _run(orch, adapter)
    assert first.interrupted is True
    _resume(orch, adapter, first)

    assert adapter.log == [
        "acquire", "work-start", "release",
        "acquire", "work-start", "work-end", "release",
    ]


@pytest.mark.parametrize("resume", [None, "continue"])
def test_without_restart_a_stopped_dynamic_continues_at_its_first_unfinished_effect(
    tmp_path: Path, resume: str | None
) -> None:
    orch = _write(tmp_path, "probe.yml", {"effects": [_session(resume=resume)]})
    adapter = LoggingAdapter()
    adapter.interrupt = {"work"}

    first = _run(orch, adapter)
    assert first.interrupted is True
    _resume(orch, adapter, first)

    assert adapter.log == [
        "acquire", "work-start", "release",
        "work-start", "work-end", "release",
    ]


def test_a_restart_dynamic_that_failed_runs_again_from_its_first_effect(
    tmp_path: Path,
) -> None:
    orch = _write(tmp_path, "probe.yml", {"effects": [_session()]})
    adapter = LoggingAdapter()
    adapter.fail = {"work"}

    first = _run(orch, adapter)
    assert first.ok is False
    _resume(orch, adapter, first)

    assert adapter.log == [
        "acquire", "work-start", "release",
        "acquire", "work-start", "work-end", "release",
    ]


def test_a_finished_restart_dynamic_is_skipped(tmp_path: Path) -> None:
    orch = _write(
        tmp_path, "probe.yml", {"effects": [_session(), _prompt("after", "after")]}
    )
    adapter = LoggingAdapter()
    adapter.fail = {"after"}

    first = _run(orch, adapter)
    assert first.ok is False
    second = _resume(orch, adapter, first)

    assert adapter.log == ["acquire", "work-start", "work-end", "release", "after", "after"]
    assert second.state["prime"]["session"]["work"]["value"] == "ok:work"


def test_a_restart_dynamic_stopped_in_its_own_finally_runs_again_from_its_first_effect(
    tmp_path: Path,
) -> None:
    orch = _write(tmp_path, "probe.yml", {"effects": [_session()]})
    adapter = LoggingAdapter()
    adapter.interrupt = {"release"}

    first = _run(orch, adapter)
    assert first.interrupted is True
    _resume(orch, adapter, first)

    assert adapter.log == [
        "acquire", "work-start", "work-end", "release",
        "acquire", "work-start", "work-end", "release",
    ]


def test_a_restarted_dynamic_does_not_read_what_its_last_run_saved(tmp_path: Path) -> None:
    session = {
        "type": "dynamic",
        "name": "session",
        "resume": "restart",
        "effects": [
            _prompt("a", "a"),
            _prompt("b", "b-[{{prime.session.c.value}}]"),
            _prompt("c", "c"),
        ],
        "finally": [_prompt("release", "release")],
    }
    orch = _write(tmp_path, "probe.yml", {"effects": [session]})
    adapter = LoggingAdapter()
    adapter.fail = {"release"}

    first = _run(orch, adapter)
    assert first.ok is False
    second = _resume(orch, adapter, first)

    assert adapter.log == ["a", "b-[]", "c", "release", "a", "b-[]", "c", "release"]
    assert set(second.state["prime"]["session"]) == {"value", "meta", "a", "b", "c", "release"}


# -- use -----------------------------------------------------------------------


def _use_parent(tmp_path: Path, *, resume: str | None) -> Path:
    child = _write(
        tmp_path,
        "child.yml",
        {
            "interface": {"outputs": {"done": {"path": "prime.work.value"}}},
            "effects": [_prompt("acquire", "acquire"), _prompt("work", "work")],
        },
    )
    use: dict[str, Any] = {"type": "use", "name": "sub", "path": str(child)}
    if resume is not None:
        use["resume"] = resume
    return _write(tmp_path, "parent.yml", {"runtime": RECORD, "effects": [use]})


def test_a_restart_use_runs_its_recorded_child_from_its_start(tmp_path: Path) -> None:
    orch = _use_parent(tmp_path, resume="restart")
    adapter = LoggingAdapter()
    adapter.interrupt = {"work"}

    first = _run(orch, adapter)
    assert first.interrupted is True
    assert "acquire" in first.state["prime"]["sub"]
    second = _resume(orch, adapter, first)

    assert adapter.log == ["acquire", "work-start", "acquire", "work-start", "work-end"]
    assert second.state["prime"]["sub"]["value"] == {"done": "ok:work"}


def test_without_restart_a_use_continues_inside_its_recorded_child(tmp_path: Path) -> None:
    orch = _use_parent(tmp_path, resume=None)
    adapter = LoggingAdapter()
    adapter.interrupt = {"work"}

    first = _run(orch, adapter)
    assert first.interrupted is True
    _resume(orch, adapter, first)

    assert adapter.log == ["acquire", "work-start", "work-start", "work-end"]


# -- at any depth ----------------------------------------------------------------


def test_a_restart_dynamic_in_a_resumed_loop_pass_restarts_there(tmp_path: Path) -> None:
    loop = {
        "type": "loop",
        "name": "frames",
        "flow": "chain",
        "each": {"in": "input.items", "as": "item"},
        "body": [_prompt("prep", "prep-{{item}}"), _session("-{{item}}")],
    }
    orch = _write(tmp_path, "loop.yml", {"effects": [loop]})
    adapter = LoggingAdapter()
    adapter.interrupt = {"work-1"}

    first = _run(orch, adapter, inputs={"items": ["0", "1", "2"]})
    assert first.interrupted is True
    before = len(adapter.log)
    second = _resume(orch, adapter, first)

    # Pass 1's `prep` is reused; its session runs from its start.
    assert adapter.log[before:] == [
        "acquire-1", "work-start-1", "work-end-1", "release-1",
        "prep-2", "acquire-2", "work-start-2", "work-end-2", "release-2",
    ]
    assert second.state["prime"]["frames"]["meta"]["completed_passes"] == [0, 1, 2]


def test_a_restart_dynamic_in_a_resumed_if_branch_restarts_there(tmp_path: Path) -> None:
    branch = {
        "type": "if",
        "name": "gate",
        "if": {"mode": "cel", "expr": "true"},
        "then": [_prompt("prep", "prep"), _session()],
    }
    orch = _write(tmp_path, "if.yml", {"effects": [branch]})
    adapter = LoggingAdapter()
    adapter.interrupt = {"work"}

    first = _run(orch, adapter)
    assert first.interrupted is True
    before = len(adapter.log)
    _resume(orch, adapter, first)

    assert adapter.log[before:] == ["acquire", "work-start", "work-end", "release"]


def test_a_restart_dynamic_in_a_resumed_use_child_restarts_there(tmp_path: Path) -> None:
    child = _write(
        tmp_path,
        "child.yml",
        {
            "interface": {"outputs": {"done": {"path": "prime.session.work.value"}}},
            "effects": [_prompt("prep", "prep"), _session()],
        },
    )
    orch = _write(
        tmp_path,
        "parent.yml",
        {"runtime": RECORD, "effects": [{"type": "use", "name": "sub", "path": str(child)}]},
    )
    adapter = LoggingAdapter()
    adapter.interrupt = {"work"}

    first = _run(orch, adapter)
    assert first.interrupted is True
    before = len(adapter.log)
    second = _resume(orch, adapter, first)

    assert adapter.log[before:] == ["acquire", "work-start", "work-end", "release"]
    assert second.state["prime"]["sub"]["value"] == {"done": "ok:work"}


# -- cof check -------------------------------------------------------------------


def _check(tmp_path: Path, effect: dict[str, Any]) -> dict[str, Any]:
    child = _write(tmp_path, "child.yml", {"effects": [_prompt("x", "x")]})
    if effect.get("type") == "use":
        effect = {**effect, "path": str(child)}
    orch = _write(tmp_path, "doc.yml", {"effects": [effect]})
    return validate(orch, skip_preflight=True, trust_document=True)


@pytest.mark.parametrize("value", ["continue", "restart"])
@pytest.mark.parametrize(
    "effect",
    [
        {"type": "dynamic", "name": "d", "effects": [_prompt("x", "x")]},
        {"type": "use", "name": "u", "path": "c.yml"},
    ],
    ids=["dynamic", "use"],
)
def test_cof_check_accepts_resume_on_dynamic_and_use(
    tmp_path: Path, effect: dict[str, Any], value: str
) -> None:
    result = _check(tmp_path, {**effect, "resume": value})
    assert result["ok"] is True, result["errors"]
    assert not any("resume" in warning for warning in result["warnings"])

    definition = compile_orchestration(orch={"effects": [{**effect, "resume": value}]})
    assert definition.effects[0].resume == value


@pytest.mark.parametrize(
    ("effect", "label"),
    [
        (_prompt("p", "p"), "prompt"),
        ({"type": "tool", "name": "t", "provider": "regex", "params": {}}, "tool"),
        ({"type": "if", "if": {"mode": "cel", "expr": "true"}, "then": [_prompt("x", "x")]}, "if"),
        (
            {"type": "loop", "name": "l", "while": {"mode": "cel", "expr": "false"},
             "max_iterations": 1, "body": [_prompt("x", "x")]},
            "loop",
        ),
        ({"type": "reflector", "name": "r", "effects": [_prompt("propose_steps", "x")]}, "reflector"),
    ],
    ids=lambda value: value if isinstance(value, str) else None,
)
def test_cof_check_rejects_resume_on_any_other_effect_type(
    tmp_path: Path, effect: dict[str, Any], label: str
) -> None:
    result = _check(tmp_path, {**effect, "resume": "restart"})
    assert result["ok"] is False
    assert result["errors"] == [
        (
            f"{label} effect at 'prime.effects[0]': 'resume' is only allowed on a "
            "'dynamic' or 'use' effect."
        )
    ]


def test_cof_check_rejects_resume_on_a_yield_through_its_closed_schema(tmp_path: Path) -> None:
    result = _check(tmp_path, {"type": "yield", "name": "y", "template": "x", "resume": "restart"})
    assert result["ok"] is False
    assert result["errors"] == [
        "effects[0]: Additional properties are not allowed ('resume' was unexpected)"
    ]


@pytest.mark.parametrize("value", ["again", "Restart", True, None])
@pytest.mark.parametrize(
    ("effect", "label"),
    [
        ({"type": "dynamic", "name": "d", "effects": [_prompt("x", "x")]}, "Dynamic"),
        ({"type": "use", "name": "u", "path": "c.yml"}, "Use"),
    ],
    ids=["dynamic", "use"],
)
def test_any_other_resume_value_is_rejected_naming_the_effect_and_the_key(
    tmp_path: Path, effect: dict[str, Any], label: str, value: Any
) -> None:
    result = _check(tmp_path, {**effect, "resume": value})
    assert result["ok"] is False
    assert result["errors"] == [
        f"effects[0].resume: {value!r} is not one of ['continue', 'restart']"
    ]

    # A document that skips the schema (a `use` child with `validate: false`)
    # still meets the compiler's own check.
    with pytest.raises(ValueError) as excinfo:
        compile_orchestration(orch={"effects": [{**effect, "resume": value}]})
    assert str(excinfo.value) == (
        f"{label} effect at 'prime.effects[0]': 'resume' must be 'continue' or "
        f"'restart', got {value!r}."
    )
