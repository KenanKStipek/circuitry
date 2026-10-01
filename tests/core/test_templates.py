"""Malformed Mustache is an error, never raw text sent on (#261).

At compile time every template an orchestration renders is tokenized, so
`cof check` and `cof run` reject an unclosed or mismatched tag before
anything runs. At run time a template that still fails to render raises into
the effect's own ``on_error`` instead of falling back to the unrendered text.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.cli.runtime_shim import validate
from circuitry.core import compiler
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.prompt import PromptRuntime
from circuitry.core.store import Store
from circuitry.core.templates import (
    TemplateError,
    render_template,
    template_syntax_error,
)

BROKEN = "topic={{input.topic}"


@dataclass
class RecordingAdapter:
    name: str = "recording"
    prompts: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)
        return GenerateResult(text="yes", raw={})


def _tool(**params: Any) -> dict[str, Any]:
    return {"type": "tool", "name": "t", "provider": "json", "params": params}


def test_syntax_error_reasons() -> None:
    assert template_syntax_error("hello {{input.x}}") is None
    assert template_syntax_error(BROKEN) == "unclosed tag at line 1"
    assert "Trying to close tag" in (template_syntax_error("{{#a}}x{{/b}}") or "")
    assert template_syntax_error("{{}}") is not None


def test_render_template_raises_instead_of_returning_raw_text() -> None:
    assert render_template("hi {{a}}", {"a": 1}) == "hi 1"
    with pytest.raises(TemplateError, match=r"^params\.input: malformed Mustache template"):
        render_template(BROKEN, {}, label="params.input")


@pytest.mark.parametrize(
    ("effect", "field_path"),
    [
        (_tool(mode="stringify", input=BROKEN), "params.input"),
        (_tool(nested={"list": ["ok", BROKEN]}), "params.nested.list[1]"),
        ({**_tool(), "params_json": '{"a": "{{x}"}'}, "params_json"),
        ({**_tool(), "prompt": BROKEN}, "prompt"),
        ({"type": "prompt", "name": "p", "template": BROKEN}, "template"),
        (
            {"type": "prompt", "name": "p", "messages": [{"role": "user", "content": BROKEN}]},
            "messages[0].content",
        ),
        (
            {
                "type": "prompt",
                "name": "p",
                "template": "Describe it.",
                "assets": [{"kind": "image", "ref": BROKEN}],
            },
            "assets[0].ref",
        ),
        (
            {"type": "if", "if": {"mode": "model", "template": BROKEN}, "then": [_tool()]},
            "if.template",
        ),
        (
            {"type": "loop", "while": {"mode": "model", "template": BROKEN}, "body": [_tool()]},
            "while.template",
        ),
        ({"type": "use", "name": "u", "path": "x.yml", "inputs": {"q": BROKEN}}, "inputs.q"),
        ({"type": "use", "name": "u", "inline": "effects: {{x}"}, "inline"),
    ],
)
def test_compile_rejects_malformed_template(effect: dict, field_path: str) -> None:
    with pytest.raises(
        ValueError,
        match=re.escape(f"prime.effects[0].{field_path}: malformed Mustache template"),
    ):
        compile_orchestration(orch={"effects": [effect]})


def test_use_reference_input_is_not_a_template() -> None:
    effect = {"type": "use", "name": "u", "path": "x.yml", "inputs": {"q": {"from": "input.x"}}}
    compile_orchestration(orch={"effects": [effect]})


def test_cof_check_reports_the_issue_repro(tmp_path: Path) -> None:
    path = tmp_path / "broken.yml"
    path.write_text(
        "effects:\n"
        "  - {type: tool, name: t, provider: json,"
        " params: {mode: stringify, input: 'topic={{input.topic}'}}\n",
        encoding="utf-8",
    )
    result = validate(path)
    assert result["ok"] is False
    assert result["errors"] == [
        "prime.effects[0].params.input: malformed Mustache template: unclosed tag at line 1"
    ]


# --- run time: a template the compiler never vetted -------------------------


@pytest.fixture
def unchecked_templates(monkeypatch: pytest.MonkeyPatch) -> None:
    """Compile without the template check, as a directly-built definition would."""
    monkeypatch.setattr(compiler, "template_syntax_error", lambda _text: None)


def _run(orch: dict[str, Any], adapter: RecordingAdapter | None = None) -> Store:
    store = Store({"input": {"topic": "x"}})
    DynamicRuntime(
        compile_orchestration(orch=orch),
        adapter=adapter or RecordingAdapter(),
        model="m",
    ).execute(store=store)
    return store


@pytest.mark.usefixtures("unchecked_templates")
def test_tool_param_that_fails_to_render_raises_under_on_error_fail() -> None:
    with pytest.raises(RuntimeError, match=r"prime\.t: params\.input: malformed Mustache"):
        _run({"effects": [_tool(mode="stringify", input=BROKEN)]})


@pytest.mark.usefixtures("unchecked_templates")
def test_tool_param_that_fails_to_render_honours_on_error_continue() -> None:
    store = _run(
        {
            "effects": [
                {**_tool(mode="stringify", input=BROKEN), "on_error": "continue"},
                {**_tool(mode="stringify", input="after"), "name": "after"},
            ]
        }
    )
    assert store.get("prime.t.value") is None
    assert "malformed Mustache template" in store.get("prime.t.meta.error")
    assert store.get("prime.after.value") == '"after"'


@pytest.mark.usefixtures("unchecked_templates")
def test_prompt_that_fails_to_render_never_reaches_the_model() -> None:
    adapter = RecordingAdapter()
    with pytest.raises(RuntimeError, match=r"prime\.p: template: malformed Mustache"):
        _run({"effects": [{"type": "prompt", "name": "p", "template": BROKEN}]}, adapter)
    assert adapter.prompts == []


@pytest.mark.usefixtures("unchecked_templates")
def test_prompt_that_fails_to_render_honours_on_error_skip() -> None:
    adapter = RecordingAdapter()
    store = _run(
        {"effects": [{"type": "prompt", "name": "p", "template": BROKEN, "on_error": "skip"}]},
        adapter,
    )
    assert adapter.prompts == []
    assert store.get("prime.p.value") is None
    assert "template: malformed Mustache template" in store.get("prime.p.meta.error")


@pytest.mark.usefixtures("unchecked_templates")
def test_prompt_message_that_fails_to_render_honours_on_error_skip() -> None:
    adapter = RecordingAdapter()
    store = _run(
        {
            "effects": [
                {
                    "type": "prompt",
                    "name": "p",
                    "messages": [{"role": "user", "content": BROKEN}],
                    "on_error": "skip",
                }
            ]
        },
        adapter,
    )
    assert adapter.prompts == []
    assert store.get("prime.p.value") is None
    assert "messages[0].content: malformed Mustache template" in store.get("prime.p.meta.error")


@pytest.mark.usefixtures("unchecked_templates")
def test_prompt_asset_ref_that_fails_to_render_honours_on_error_skip() -> None:
    adapter = RecordingAdapter()
    store = _run(
        {
            "effects": [
                {
                    "type": "prompt",
                    "name": "p",
                    "template": "Describe it.",
                    "assets": [{"kind": "image", "ref": BROKEN}],
                    "on_error": "skip",
                }
            ]
        },
        adapter,
    )
    assert adapter.prompts == []
    assert store.get("prime.p.value") is None
    assert "assets[0].ref: malformed Mustache template" in store.get("prime.p.meta.error")


@pytest.mark.usefixtures("unchecked_templates")
def test_if_model_template_that_fails_to_render_honours_on_error() -> None:
    adapter = RecordingAdapter()
    store = _run(
        {
            "effects": [
                {
                    "type": "if",
                    "name": "gate",
                    "if": {"mode": "model", "template": BROKEN},
                    "on_error": "skip",
                    "then": [_tool(input="1")],
                }
            ]
        },
        adapter,
    )
    assert adapter.prompts == []
    assert "if.template: malformed Mustache template" in store.get("prime.gate.meta.error")


@pytest.mark.usefixtures("unchecked_templates")
def test_while_model_template_that_fails_to_render_raises() -> None:
    adapter = RecordingAdapter()
    with pytest.raises(RuntimeError, match=r"prime\.poll: while\.template: malformed"):
        _run(
            {
                "effects": [
                    {
                        "type": "loop",
                        "name": "poll",
                        "while": {"mode": "model", "template": BROKEN},
                        "max_iterations": 2,
                        "body": [_tool(input="1")],
                    }
                ]
            },
            adapter,
        )
    assert adapter.prompts == []


@pytest.mark.usefixtures("unchecked_templates")
def test_prompt_that_fails_to_render_still_reports_its_failure_line() -> None:
    lines: list[str] = []
    PromptRuntime(
        compile_orchestration(
            orch={"effects": [{"type": "prompt", "name": "p", "template": BROKEN, "on_error": "skip"}]}
        ).effects[0],
        adapter=RecordingAdapter(),
        model="m",
        verbose=True,
        cb_error=lines.append,
    ).execute(store=Store({}), ctx={})
    (line,) = lines
    assert "✗" in line and " p " in line


@pytest.mark.usefixtures("unchecked_templates")
def test_tool_prompt_that_fails_to_render_honours_on_error_continue() -> None:
    store = _run(
        {"effects": [{**_tool(mode="stringify", input="1"), "prompt": BROKEN, "on_error": "continue"}]}
    )
    assert store.get("prime.t.value") is None
    assert "prompt: malformed Mustache template" in store.get("prime.t.meta.error")


CHILD_INLINE = "effects: [{type: tool, name: c, provider: json, params: {input: '1'}}]"


@pytest.mark.usefixtures("unchecked_templates")
def test_use_input_that_fails_to_render_honours_on_error_continue() -> None:
    store = _run(
        {
            "effects": [
                {
                    "type": "use",
                    "name": "u",
                    "inline": CHILD_INLINE,
                    "inputs": {"topic": BROKEN},
                    "on_error": "continue",
                },
                {**_tool(mode="stringify", input="after"), "name": "after"},
            ]
        }
    )
    assert "inputs.topic: malformed Mustache template" in store.get("prime.u.meta.error")
    assert store.get("prime.u.c") is None
    assert store.get("prime.after.value") == '"after"'


@pytest.mark.usefixtures("unchecked_templates")
def test_use_inline_that_fails_to_render_honours_on_error_skip() -> None:
    store = _run(
        {"effects": [{"type": "use", "name": "u", "inline": CHILD_INLINE + BROKEN, "on_error": "skip"}]}
    )
    assert store.get("prime.u.value") is None
    assert "inline: malformed Mustache template" in store.get("prime.u.meta.error")
