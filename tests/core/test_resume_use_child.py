"""`cof run --resume` inside a `use` child (#567): with
``runtime.state.record_children: true`` an unfinished `use` continues at its
child's first unfinished effect, recursively for nested `use`, as long as the
child would run the same document with the same inputs. Without
``record_children`` a `use` stays all-or-nothing.

Each resumed run starts from the first run's state as saved (`--out`'s JSON
form), the way `cof run --resume` reads it back.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.runtime_shim import RunRequest, RunResult, run
from circuitry.core.saved_state import dumps_saved_state

RECORD = {"state": {"record_children": True}}


class CountingAdapter:
    """Records every prompt it's asked to generate; fails or interrupts on demand."""

    name = "counting"

    def __init__(self) -> None:
        self.calls: list[str] = []
        self.fail_prompts: set[str] = set()
        self.interrupt_prompts: set[str] = set()

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append(prompt)
        if prompt in self.interrupt_prompts:
            raise KeyboardInterrupt
        if prompt in self.fail_prompts:
            raise RuntimeError(f"boom: {prompt}")
        return GenerateResult(text=f"ok:{prompt}", raw={})


def _write(tmp_path: Path, name: str, content: dict[str, Any]) -> Path:
    path = tmp_path / name
    path.write_text(yaml.safe_dump(content, sort_keys=False), encoding="utf-8")
    return path


def _prompt(name: str, template: str) -> dict[str, Any]:
    return {"type": "prompt", "name": name, "template": template}


def _run(
    orch: Path,
    adapter: CountingAdapter,
    *,
    inputs: dict[str, Any] | None = None,
    saved: dict[str, Any] | None = None,
) -> RunResult:
    if saved is None:
        initial_state: dict[str, Any] | None = {"input": dict(inputs or {})}
    else:
        initial_state = saved
    return run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=initial_state,
            resume=saved is not None,
        )
    )


def _saved(result: RunResult) -> dict[str, Any]:
    """*result*'s state as `--out` writes it and `--resume` reads it back."""
    loaded: dict[str, Any] = json.loads(dumps_saved_state(result.state))
    return loaded


def _child(tmp_path: Path) -> Path:
    """A three-step child; `topic` is an input, `tags` a structured one."""
    return _write(
        tmp_path,
        "child.yml",
        {
            "interface": {
                "inputs": {"topic": {"required": True}, "tags": {"required": False}},
                "outputs": {"last": {"path": "prime.c.value"}},
            },
            "effects": [
                _prompt("a", "a {{input.topic}}"),
                _prompt("b", "b {{input.topic}}"),
                _prompt("c", "c {{input.topic}}"),
            ],
        },
    )


def _parent(
    tmp_path: Path, child: Path, *, runtime: dict[str, Any] | None = RECORD
) -> Path:
    doc: dict[str, Any] = {
        "effects": [
            _prompt("before", "before"),
            {
                "type": "use",
                "name": "sub",
                "path": str(child),
                "inputs": {"topic": "{{input.topic}}", "tags": {"from": "input.tags"}},
            },
            _prompt("after", "after {{prime.sub.value.last}}"),
        ]
    }
    if runtime is not None:
        doc["runtime"] = runtime
    return _write(tmp_path, "parent.yml", doc)


INPUTS = {"topic": "x", "tags": ["one", {"two": 2}]}


def test_resume_continues_a_failed_use_child_at_its_first_unfinished_effect(
    tmp_path: Path,
) -> None:
    orch = _parent(tmp_path, _child(tmp_path))
    adapter = CountingAdapter()
    adapter.fail_prompts = {"c x"}

    first = _run(orch, adapter, inputs=INPUTS)
    assert first.ok is False
    assert adapter.calls == ["before", "a x", "b x", "c x"]

    adapter.fail_prompts = set()
    second = _run(orch, adapter, saved=_saved(first))
    assert second.ok is True, second.error
    # `before`, and the child's `a` and `b`, are reused, not run again.
    assert adapter.calls[4:] == ["c x", "after ok:c x"]
    sub = second.state["prime"]["sub"]
    assert sub["value"] == {"last": "ok:c x"}
    assert sub["meta"]["error"] is None
    assert [sub[k]["value"] for k in ("a", "b", "c")] == ["ok:a x", "ok:b x", "ok:c x"]


def test_resume_continues_an_interrupted_use_child_at_its_first_unfinished_effect(
    tmp_path: Path,
) -> None:
    orch = _parent(tmp_path, _child(tmp_path))
    adapter = CountingAdapter()
    adapter.interrupt_prompts = {"b x"}

    first = _run(orch, adapter, inputs=INPUTS)
    assert first.ok is False
    assert first.interrupted is True
    # The interrupted child's record is saved: `a` finished, `b` did not.
    sub = first.state["prime"]["sub"]
    assert sub["a"]["value"] == "ok:a x"
    assert not sub["b"]["meta"].get("completed_at")

    adapter.interrupt_prompts = set()
    second = _run(orch, adapter, saved=_saved(first))
    assert second.ok is True, second.error
    assert adapter.calls[3:] == ["b x", "c x", "after ok:c x"]


def test_resume_continues_inside_nested_use_children(tmp_path: Path) -> None:
    leaf = _write(
        tmp_path,
        "leaf.yml",
        {
            "interface": {
                "inputs": {"topic": {"required": True}},
                "outputs": {"y": {"path": "prime.y.value"}},
            },
            "effects": [_prompt("x", "x {{input.topic}}"), _prompt("y", "y {{input.topic}}")],
        },
    )
    mid = _write(
        tmp_path,
        "mid.yml",
        {
            "interface": {
                "inputs": {"topic": {"required": True}},
                "outputs": {"m2": {"path": "prime.m2.value"}},
            },
            "effects": [
                _prompt("m1", "m1 {{input.topic}}"),
                {
                    "type": "use",
                    "name": "inner",
                    "path": str(leaf),
                    "inputs": {"topic": "{{input.topic}}"},
                },
                _prompt("m2", "m2 {{prime.inner.value.y}}"),
            ],
        },
    )
    orch = _write(
        tmp_path,
        "parent.yml",
        {
            "runtime": RECORD,
            "effects": [
                {"type": "use", "name": "outer", "path": str(mid), "inputs": {"topic": "t"}},
            ],
        },
    )
    adapter = CountingAdapter()
    adapter.fail_prompts = {"y t"}

    first = _run(orch, adapter)
    assert first.ok is False
    assert adapter.calls == ["m1 t", "x t", "y t"]

    adapter.fail_prompts = set()
    second = _run(orch, adapter, saved=_saved(first))
    assert second.ok is True, second.error
    # `m1` (the outer child) and `x` (the inner child) are reused.
    assert adapter.calls[3:] == ["y t", "m2 ok:y t"]
    outer = second.state["prime"]["outer"]
    assert outer["inner"]["x"]["value"] == "ok:x t"
    assert outer["inner"]["y"]["value"] == "ok:y t"


def test_resume_continues_a_loop_inside_a_use_child_at_its_first_unfinished_pass(
    tmp_path: Path,
) -> None:
    child = _write(
        tmp_path,
        "child.yml",
        {
            "interface": {"inputs": {"items": {"required": True}}},
            "effects": [
                {
                    "type": "loop",
                    "name": "frames",
                    "flow": "chain",
                    "each": {"in": "input.items", "as": "item"},
                    "body": [_prompt("render", "frame-{{item}}")],
                }
            ],
        },
    )
    orch = _write(
        tmp_path,
        "parent.yml",
        {
            "runtime": RECORD,
            "effects": [
                {
                    "type": "use",
                    "name": "sub",
                    "path": str(child),
                    "inputs": {"items": {"from": "input.items"}},
                    "outputs": {"n": "prime.frames.value.iterations"},
                }
            ],
        },
    )
    adapter = CountingAdapter()
    adapter.fail_prompts = {"frame-2"}

    first = _run(orch, adapter, inputs={"items": ["0", "1", "2", "3"]})
    assert first.ok is False
    assert adapter.calls == ["frame-0", "frame-1", "frame-2"]

    adapter.fail_prompts = set()
    second = _run(orch, adapter, saved=_saved(first))
    assert second.ok is True, second.error
    assert adapter.calls[3:] == ["frame-2", "frame-3"]
    assert second.state["prime"]["sub"]["value"] == {"n": 4}


def test_resume_runs_a_use_child_from_its_start_when_its_inputs_changed(
    tmp_path: Path,
) -> None:
    orch = _parent(tmp_path, _child(tmp_path))
    adapter = CountingAdapter()
    adapter.fail_prompts = {"c x"}

    first = _run(orch, adapter, inputs=INPUTS)
    assert first.ok is False

    adapter.fail_prompts = set()
    saved = _saved(first)
    saved["input"]["tags"] = ["one", {"two": 3}]
    second = _run(orch, adapter, saved=saved)
    assert second.ok is True, second.error
    # Same rendered prompts, but the child's `tags` input changed: the whole
    # child runs again.
    assert adapter.calls[4:] == ["a x", "b x", "c x", "after ok:c x"]
    assert second.state["prime"]["sub"]["meta"]["inputs"]["tags"] == ["one", {"two": 3}]


def test_resume_runs_a_use_child_from_its_start_when_its_document_changed(
    tmp_path: Path,
) -> None:
    child = _child(tmp_path)
    orch = _parent(tmp_path, child)
    adapter = CountingAdapter()
    adapter.fail_prompts = {"c x"}

    first = _run(orch, adapter, inputs=INPUTS)
    assert first.ok is False

    doc = yaml.safe_load(child.read_text(encoding="utf-8"))
    doc["effects"][0]["template"] = "A {{input.topic}}"
    child.write_text(yaml.safe_dump(doc, sort_keys=False), encoding="utf-8")

    adapter.fail_prompts = set()
    second = _run(orch, adapter, saved=_saved(first))
    assert second.ok is True, second.error
    assert adapter.calls[4:] == ["A x", "b x", "c x", "after ok:c x"]
    assert second.state["prime"]["sub"]["a"]["value"] == "ok:A x"


def test_resume_without_record_children_runs_a_use_child_from_its_start(
    tmp_path: Path,
) -> None:
    orch = _parent(tmp_path, _child(tmp_path), runtime=None)
    adapter = CountingAdapter()
    adapter.fail_prompts = {"c x"}

    first = _run(orch, adapter, inputs=INPUTS)
    assert first.ok is False
    assert "a" not in first.state["prime"]["sub"]

    adapter.fail_prompts = set()
    second = _run(orch, adapter, saved=_saved(first))
    assert second.ok is True, second.error
    assert adapter.calls[4:] == ["a x", "b x", "c x", "after ok:c x"]
    assert "a" not in second.state["prime"]["sub"]


def test_resume_without_record_children_keeps_no_record_of_an_interrupted_child(
    tmp_path: Path,
) -> None:
    orch = _parent(tmp_path, _child(tmp_path), runtime=None)
    adapter = CountingAdapter()
    adapter.interrupt_prompts = {"b x"}

    first = _run(orch, adapter, inputs=INPUTS)
    assert first.interrupted is True
    assert set(first.state["prime"]["sub"]) == {"value", "meta"}


def test_a_retry_of_a_resumed_use_child_runs_it_from_its_start(tmp_path: Path) -> None:
    child = _child(tmp_path)
    orch = _write(
        tmp_path,
        "parent.yml",
        {
            "runtime": RECORD,
            "effects": [
                {
                    "type": "use",
                    "name": "sub",
                    "path": str(child),
                    "inputs": {"topic": "x"},
                    "retries": {"max_attempts": 2, "backoff_ms": 0},
                }
            ],
        },
    )
    adapter = CountingAdapter()
    adapter.fail_prompts = {"c x"}

    first = _run(orch, adapter)
    assert first.ok is False
    assert adapter.calls == ["a x", "b x", "c x", "a x", "b x", "c x"]
    assert len(first.state["prime"]["sub"]["meta"]["attempts"]) == 2

    # Resumed: the first attempt continues at `c` and fails again; the retry
    # runs the whole child, then succeeds.

    class FailOnce(CountingAdapter):
        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            if prompt == "c x" and "c x" not in self.calls:
                self.calls.append(prompt)
                raise RuntimeError("boom once")
            return super().generate(
                model=model, prompt=prompt, timeout_seconds=timeout_seconds
            )

    retrying = FailOnce()
    second = _run(orch, retrying, saved=_saved(first))
    assert second.ok is True, second.error
    assert retrying.calls == ["c x", "a x", "b x", "c x"]
    # Only the resumed run's own two attempts -- the interrupted first run's
    # attempts stay behind in the state it resumed from, not merged in here.
    assert len(second.state["prime"]["sub"]["meta"]["attempts"]) == 2


def _expect_parent(tmp_path: Path, expect: Any) -> Path:
    return _write(
        tmp_path,
        "parent.yml",
        {
            "runtime": RECORD,
            "effects": [
                {
                    "type": "use",
                    "name": "sub",
                    "path": str(_child(tmp_path)),
                    "inputs": {"topic": "x"},
                    "expect": expect,
                }
            ],
        },
    )


def test_resume_runs_a_use_child_from_its_start_when_its_expect_failed(
    tmp_path: Path,
) -> None:
    """The child finished, but the `use`'s own `expect` judged its output
    false: reusing the child whole would hand the same output to the same
    check, and the run could never get past it."""
    orch = _expect_parent(tmp_path, "value.last == 'good'")

    class Replies(CountingAdapter):
        def __init__(self) -> None:
            super().__init__()
            self.last_reply = "bad"

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            super().generate(model=model, prompt=prompt, timeout_seconds=timeout_seconds)
            return GenerateResult(text=self.last_reply if prompt == "c x" else "ok", raw={})

    adapter = Replies()
    first = _run(orch, adapter)
    assert first.ok is False
    assert first.state["prime"]["sub"]["meta"]["expect"]["result"] is False
    assert adapter.calls == ["a x", "b x", "c x"]

    adapter.last_reply = "good"
    second = _run(orch, adapter, saved=_saved(first))
    assert second.ok is True, second.error
    assert adapter.calls[3:] == ["a x", "b x", "c x"]
    assert second.state["prime"]["sub"]["value"] == {"last": "good"}


def test_resume_reuses_a_finished_use_child_when_its_expect_raised(
    tmp_path: Path,
) -> None:
    """A model-mode `expect` whose own adapter call failed recorded no
    result: the child's output was never judged, so the finished child is
    reused and only the check runs again."""
    orch = _expect_parent(tmp_path, {"mode": "model", "template": "is {{value.last}} fine?"})

    class Judge(CountingAdapter):
        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            if prompt.startswith("Evaluate the following"):
                self.calls.append("judge")
                if self.fail_prompts:
                    raise RuntimeError("judge unavailable")
                return GenerateResult(text="yes", raw={})
            return super().generate(
                model=model, prompt=prompt, timeout_seconds=timeout_seconds
            )

    adapter = Judge()
    adapter.fail_prompts = {"judge"}
    first = _run(orch, adapter)
    assert first.ok is False
    assert "expect" not in first.state["prime"]["sub"]["meta"]
    assert adapter.calls == ["a x", "b x", "c x", "judge"]

    adapter.fail_prompts = set()
    second = _run(orch, adapter, saved=_saved(first))
    assert second.ok is True, second.error
    assert adapter.calls[4:] == ["judge"]
    assert second.state["prime"]["sub"]["meta"]["expect"]["result"] is True
