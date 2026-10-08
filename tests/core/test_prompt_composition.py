"""Prompt composition: `type: yield`, declared `prompts:`, `{{> name}}`, and
prompt files (#396) — plus prompt text's no-HTML-escaping rule (#397), which
rides the same render path.
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest
import yaml

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.prompt_compose import EFFECT_NAMES_RUNTIME_KEY, RUNTIME_CONFIG_KEY
from circuitry.core.store import Store


def _mock_adapter(response: str = "mock reply") -> MagicMock:
    adapter = MagicMock()
    adapter.name = "mock"
    result = MagicMock()
    result.text = response
    result.raw = {}
    result.tokens_sent = 1
    result.tokens_received = 1
    adapter.generate.return_value = result
    return adapter


def _run(
    orch: dict,
    *,
    adapter: MagicMock | None = None,
    initial_state: dict | None = None,
    document_dir: Path | None = None,
    confinement_root: Path | None = None,
) -> Store:
    root = compile_orchestration(
        orch=orch, document_dir=document_dir, confinement_root=confinement_root
    )
    runtime_config = {
        RUNTIME_CONFIG_KEY: root.prompts,
        EFFECT_NAMES_RUNTIME_KEY: root.effect_names,
    }
    store = Store(initial_state or {"input": {}})
    DynamicRuntime(
        root, adapter=adapter or _mock_adapter(), model="m", runtime_config=runtime_config
    ).execute(store=store)
    return store


# ── `type: yield` ────────────────────────────────────────────────────────────


def test_yield_renders_a_template_with_no_model_call() -> None:
    adapter = _mock_adapter()
    store = _run(
        {"effects": [{"type": "yield", "name": "brief", "template": "Topic: {{input.topic}}"}]},
        adapter=adapter,
        initial_state={"input": {"topic": "circuitry"}},
    )
    assert store.get("prime.brief.value") == "Topic: circuitry"
    assert adapter.generate.call_count == 0


def test_yield_needs_no_model_so_rendering_is_testable_without_one() -> None:
    """A yield has no adapter dependency at all — ``None`` works."""
    root = compile_orchestration(orch={"effects": [{"type": "yield", "name": "y", "template": "x"}]})
    store = Store({"input": {}})
    DynamicRuntime(root, adapter=None, model="m").execute(store=store)
    assert store.get("prime.y.value") == "x"


def test_yield_on_error_continue_absorbs_a_render_failure() -> None:
    orch = {
        "prompts": {},
        "effects": [
            {"type": "yield", "name": "bad", "template": "{{> missing}}", "on_error": "continue"},
            {"type": "yield", "name": "after", "template": "ok"},
        ],
    }
    # `{{> missing}}` is a static `cof check` error (unknown name) — exercise the
    # run-time path instead, the way a generated document would reach it.
    import circuitry.core.compiler as compiler_mod

    original = compiler_mod.check_prompt_composition
    compiler_mod.check_prompt_composition = lambda *a, **k: []  # type: ignore[assignment]
    try:
        store = _run(orch)
    finally:
        compiler_mod.check_prompt_composition = original
    assert store.get("prime.bad.value") is None
    assert "does not name" in store.get("prime.bad.meta.error")
    assert store.get("prime.after.value") == "ok"


def test_yield_rejects_model_only_keys_at_compile_time() -> None:
    with pytest.raises(ValueError, match="does not accept"):
        compile_orchestration(
            orch={"effects": [{"type": "yield", "name": "y", "template": "x", "model": "gpt"}]}
        )


# ── declared prompts + `{{> name}}` ─────────────────────────────────────────


def test_declared_prompt_is_spliced_into_a_prompt_template() -> None:
    orch = {
        "prompts": {"voice": "Plain, direct sentences."},
        "effects": [
            {"type": "prompt", "name": "draft", "template": "{{> voice}}\nTopic: {{input.topic}}"},
        ],
    }
    adapter = _mock_adapter()
    _run(orch, adapter=adapter, initial_state={"input": {"topic": "x"}})
    sent = adapter.generate.call_args.kwargs["prompt"]
    assert sent == "Plain, direct sentences.\nTopic: x"


def test_several_prompts_concatenate_in_one_string() -> None:
    orch = {
        "prompts": {"look": "wide shot", "shot": "35mm"},
        "effects": [
            {"type": "prompt", "name": "p", "template": "{{> look}}, {{> shot}}"},
        ],
    }
    adapter = _mock_adapter()
    _run(orch, adapter=adapter)
    assert adapter.generate.call_args.kwargs["prompt"] == "wide shot, 35mm"


def test_several_prompts_concatenate_in_a_tool_param() -> None:
    orch = {
        "prompts": {"look": "wide shot", "shot": "35mm"},
        "effects": [
            {
                "type": "tool",
                "name": "gen",
                "provider": "json",
                "params": {"mode": "stringify", "input": "{{> look}}, {{> shot}}"},
            },
        ],
    }
    store = _run(orch)
    assert store.get("prime.gen.value") == '"wide shot, 35mm"'


def test_declared_prompt_may_include_another() -> None:
    orch = {
        "prompts": {
            "base": "Be concise.",
            "voice": "{{> base}} Plain sentences.",
        },
        "effects": [{"type": "yield", "name": "y", "template": "{{> voice}}"}],
    }
    store = _run(orch)
    assert store.get("prime.y.value") == "Be concise. Plain sentences."


def test_declared_prompt_trailing_newline_is_dropped_once() -> None:
    orch = {
        "prompts": {"voice": "Plain, direct.\n"},
        "effects": [{"type": "yield", "name": "y", "template": "{{> voice}}\nNext line"}],
    }
    store = _run(orch)
    assert store.get("prime.y.value") == "Plain, direct.\nNext line"


# ── section scope: text splicing, not a pre-rendered value (decision C) ─────


def test_a_declared_prompt_inside_a_list_section_sees_each_items_own_scope() -> None:
    """The fragment's own text is spliced in before chevron ever renders —
    so a section wrapped AROUND `{{> line}}` iterates normally, rendering
    the fragment's own tags fresh against each item, not one shared
    top-level context."""
    orch = {
        "prompts": {"line": "-{{title}}"},
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "json",
                "params": {
                    "mode": "stringify",
                    "input": "{{#input.items}}{{> line}}{{/input.items}}",
                },
            }
        ],
    }
    store = _run(
        orch, initial_state={"input": {"items": [{"title": "a"}, {"title": "b"}]}}
    )
    assert store.get("prime.t.value") == '"-a-b"'


def test_a_declared_prompt_inside_nested_sections_sees_each_level() -> None:
    orch = {
        "prompts": {"line": "[{{name}}]"},
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "json",
                "params": {
                    "mode": "stringify",
                    "input": (
                        "{{#input.groups}}({{#items}}{{> line}}{{/items}})"
                        "{{/input.groups}}"
                    ),
                },
            }
        ],
    }
    store = _run(
        orch,
        initial_state={
            "input": {
                "groups": [
                    {"items": [{"name": "a"}, {"name": "b"}]},
                    {"items": [{"name": "c"}]},
                ]
            }
        },
    )
    assert store.get("prime.t.value") == '"([a][b])([c])"'


def test_a_declared_prompt_inside_an_inverted_section() -> None:
    orch = {
        "prompts": {"empty_note": "NOTHING HERE"},
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "json",
                "params": {
                    "mode": "stringify",
                    "input": "{{^input.items}}{{> empty_note}}{{/input.items}}",
                },
            }
        ],
    }
    empty = _run(orch, initial_state={"input": {"items": []}})
    nonempty = _run(orch, initial_state={"input": {"items": ["x"]}})
    assert empty.get("prime.t.value") == '"NOTHING HERE"'
    assert nonempty.get("prime.t.value") == '""'


def test_a_declared_prompt_used_both_at_top_level_and_inside_a_section() -> None:
    orch = {
        "prompts": {"tag": "#{{input.x}}"},
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "json",
                "params": {
                    "mode": "stringify",
                    "input": "{{> tag}} {{#input.items}}{{> tag}}{{/input.items}}",
                },
            }
        ],
    }
    store = _run(orch, initial_state={"input": {"x": "root", "items": [1, 2]}})
    assert store.get("prime.t.value") == '"#root #root#root"'


def test_a_set_delimiter_tag_inside_a_declared_prompt_is_a_compile_error() -> None:
    with pytest.raises(ValueError, match="set-delimiter"):
        compile_orchestration(
            orch={
                "prompts": {"bad": "{{=<% %>=}}hi"},
                "effects": [{"type": "yield", "name": "y", "template": "{{> bad}}"}],
            }
        )


def test_a_set_delimiter_tag_inside_a_prompt_file_is_a_compile_error(
    tmp_path: Path,
) -> None:
    _write(tmp_path / "bad.md", "{{=<% %>=}}hi")
    with pytest.raises(ValueError, match="set-delimiter"):
        compile_orchestration(
            orch={
                "prompts": {"bad": {"file": "bad.md"}},
                "effects": [{"type": "yield", "name": "y", "template": "{{> bad}}"}],
            },
            document_dir=tmp_path,
            confinement_root=tmp_path,
        )


def test_an_unbalanced_section_inside_a_declared_prompt_is_reported_against_it() -> None:
    """Each declared prompt is validated on its own, so a malformed
    fragment is reported against the prompt that owns it, not the template
    that happens to include it first."""
    with pytest.raises(ValueError, match=r"prompts\.bad: malformed Mustache template"):
        compile_orchestration(
            orch={
                "prompts": {"bad": "{{#items}}unclosed"},
                "effects": [{"type": "yield", "name": "y", "template": "{{> bad}}"}],
            }
        )


def test_yield_text_is_spliced_into_another_effects_template() -> None:
    orch = {
        "effects": [
            {"type": "yield", "name": "brief", "template": "Audience: devs"},
            {"type": "prompt", "name": "draft", "template": "{{> brief}}\nGo."},
        ],
    }
    adapter = _mock_adapter()
    _run(orch, adapter=adapter)
    assert adapter.generate.call_args.kwargs["prompt"] == "Audience: devs\nGo."


def test_a_text_prompts_reply_is_spliced_verbatim_including_literal_braces() -> None:
    """A model reply containing `{{...}}` is inserted verbatim, never re-rendered."""
    orch = {
        "effects": [
            {"type": "prompt", "name": "gen", "template": "Say something."},
            {"type": "yield", "name": "wrap", "template": "Reply: {{> gen}}"},
        ],
    }
    adapter = _mock_adapter(response="here is {{input.secret}} and {{> nope}}")
    store = _run(orch, adapter=adapter, initial_state={"input": {"secret": "s"}})
    assert store.get("prime.wrap.value") == (
        "Reply: here is {{input.secret}} and {{> nope}}"
    )


def test_a_declared_prompt_that_includes_a_yield_inside_a_loop_body() -> None:
    """Inside a loop body, `{{> name}}` resolves to the current pass's own
    sibling, the same way a bare `prime.<name>.value` reference already does."""
    orch = {
        "prompts": {"wrap": "[{{> step}}]"},
        "effects": [
            {
                "type": "loop",
                "name": "per_item",
                "collect": "line",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "yield", "name": "step", "template": "{{item}}"},
                    {"type": "yield", "name": "line", "template": "{{> wrap}}"},
                ],
            }
        ],
    }
    store = _run(orch, initial_state={"input": {"items": ["a", "b"]}})
    assert store.get("prime.per_item.collected.value") == ["[a]", "[b]"]


def test_use_inline_supports_composition() -> None:
    orch = {
        "prompts": {"greeting": "Hello"},
        "effects": [
            {
                "type": "use",
                "name": "u",
                "inline": "effects: [{type: yield, name: c, template: '{{> greeting}}!'}]",
            }
        ],
    }
    store = _run(orch)
    assert store.get("prime.u.c.value") == "Hello!"


def test_use_inputs_support_composition() -> None:
    # `use.inline` renders once, against the *parent's* context, before the
    # child's own `inputs:` is rendered — so the rendered input itself is
    # checked via `meta.inputs`, not by having the child echo it back.
    orch = {
        "prompts": {"greeting": "Hello"},
        "effects": [
            {
                "type": "use",
                "name": "u",
                "inline": "effects: []",
                "inputs": {"msg": "{{> greeting}}!"},
            }
        ],
    }
    store = _run(orch)
    assert store.get("prime.u.meta.inputs") == {"msg": "Hello!"}


# ── cof check: unknown names, collisions, cycles ────────────────────────────


def test_unknown_partial_name_is_a_compile_error() -> None:
    with pytest.raises(ValueError, match=r"'\{\{> nope\}\}' does not name"):
        compile_orchestration(
            orch={"effects": [{"type": "yield", "name": "y", "template": "{{> nope}}"}]}
        )


def test_declared_and_effect_name_collision_is_a_compile_error() -> None:
    with pytest.raises(ValueError, match="both a declared prompt and an effect name"):
        compile_orchestration(
            orch={
                "prompts": {"brief": "x"},
                "effects": [{"type": "yield", "name": "brief", "template": "hi"}],
            }
        )


def test_referencing_a_non_text_effect_is_a_compile_error() -> None:
    with pytest.raises(ValueError, match="neither a 'yield' nor a text 'prompt'"):
        compile_orchestration(
            orch={
                "effects": [
                    {"type": "tool", "name": "fetcher", "provider": "json", "params": {}},
                    {"type": "yield", "name": "y", "template": "{{> fetcher}}"},
                ]
            }
        )


def test_referencing_a_json_prompt_is_a_compile_error() -> None:
    """A `prompt` effect only qualifies when its reply is text."""
    with pytest.raises(ValueError, match="neither a 'yield' nor a text 'prompt'"):
        compile_orchestration(
            orch={
                "effects": [
                    {
                        "type": "prompt",
                        "name": "plan",
                        "prompt_type": "json",
                        "schema": {"type": "array"},
                        "template": "List steps.",
                    },
                    {"type": "yield", "name": "y", "template": "{{> plan}}"},
                ]
            }
        )


def test_cycle_among_declared_prompts_is_a_compile_error() -> None:
    with pytest.raises(ValueError, match="cycle among declared prompts"):
        compile_orchestration(
            orch={
                "prompts": {"a": "{{> b}}", "b": "{{> a}}"},
                "effects": [{"type": "yield", "name": "y", "template": "{{> a}}"}],
            }
        )


def test_a_dotted_name_reaches_into_a_named_dynamics_own_child() -> None:
    """`{{> pipeline.outline}}` where `pipeline` is a named `dynamic` names
    the nested yield at `prime.pipeline.outline`, not `pipeline` itself (a
    container, neither a yield nor a text prompt) -- the dotted form must
    not be type-checked against the container's own type."""
    store = _run(
        {
            "effects": [
                {
                    "type": "dynamic",
                    "name": "pipeline",
                    "effects": [
                        {"type": "yield", "name": "outline", "template": "The plan."}
                    ],
                },
                {"type": "yield", "name": "summary", "template": "{{> pipeline.outline}}"},
            ]
        }
    )
    assert store.get("prime.summary.value") == "The plan."


def test_a_bare_name_nested_inside_a_named_dynamic_is_not_visible_outside_it() -> None:
    """The inverse of the above: a bare `{{> outline}}`, written OUTSIDE the
    named dynamic that nests it, must not resolve -- `outline` only exists at
    `prime.pipeline.outline`, never at the document's own top level."""
    with pytest.raises(ValueError, match=r"'\{\{> outline\}\}' does not name"):
        compile_orchestration(
            orch={
                "effects": [
                    {
                        "type": "dynamic",
                        "name": "pipeline",
                        "effects": [
                            {"type": "yield", "name": "outline", "template": "The plan."}
                        ],
                    },
                    {"type": "yield", "name": "summary", "template": "{{> outline}}"},
                ]
            }
        )


def test_a_bare_name_nested_inside_a_named_dynamic_is_rejected_even_from_inside_it() -> None:
    """A named `dynamic`'s children are never bare-visible, even to a
    sibling inside the very same dynamic -- `core.dynamic`'s own `ctx` is
    always rooted at the document's real `prime`, never rebound per
    container (unlike a loop body's documented shorthand), so a value
    written at `prime.pipeline.outline` is only ever reachable, from
    anywhere, as `{{> pipeline.outline}}`."""
    with pytest.raises(ValueError, match=r"'\{\{> outline\}\}' does not name"):
        compile_orchestration(
            orch={
                "effects": [
                    {
                        "type": "dynamic",
                        "name": "pipeline",
                        "effects": [
                            {"type": "yield", "name": "outline", "template": "The plan."},
                            {"type": "yield", "name": "recap", "template": "{{> outline}}!"},
                        ],
                    }
                ]
            }
        )


def test_a_dotted_name_resolves_the_same_way_from_inside_its_own_container() -> None:
    """The dotted form works from anywhere, including from a sibling
    inside the very same named dynamic it names."""
    store = _run(
        {
            "effects": [
                {
                    "type": "dynamic",
                    "name": "pipeline",
                    "effects": [
                        {"type": "yield", "name": "outline", "template": "The plan."},
                        {
                            "type": "yield",
                            "name": "recap",
                            "template": "{{> pipeline.outline}}!",
                        },
                    ],
                }
            ]
        }
    )
    assert store.get("prime.pipeline.recap.value") == "The plan.!"


def test_a_dotted_name_into_a_declared_prompt_is_a_compile_error() -> None:
    """A declared prompt is plain text, never a container — `{{> voice.x}}`
    must fail at `cof check`, not pass there and fail only at run time."""
    with pytest.raises(ValueError, match=r"declared prompt 'voice'.*no nested state"):
        compile_orchestration(
            orch={
                "prompts": {"voice": "Plain, direct."},
                "effects": [{"type": "yield", "name": "y", "template": "{{> voice.x}}"}],
            }
        )


def test_an_untaken_if_branchs_effect_renders_empty_not_an_error() -> None:
    """`{{> name}}` names a real effect in the document, but the branch that
    would have written it never ran -- renders "", exactly like a bare
    `{{{prime.name.value}}}` miss, not a run-time error."""
    store = _run(
        {
            "effects": [
                {
                    "type": "if",
                    "if": {"mode": "cel", "expr": "false"},
                    "then": [{"type": "yield", "name": "only_if_true", "template": "x"}],
                    "else": [],
                },
                {"type": "yield", "name": "after", "template": "[{{> only_if_true}}]"},
            ]
        }
    )
    assert store.get("prime.after.value") == "[]"


def test_an_effect_referenced_before_it_runs_renders_empty_not_an_error() -> None:
    """A forward reference -- valid as a NAME (checked at compile time
    regardless of position) -- but not yet written when the referencing
    effect runs, also renders ""."""
    store = _run(
        {
            "effects": [
                {"type": "yield", "name": "before", "template": "[{{> later}}]"},
                {"type": "yield", "name": "later", "template": "x"},
            ]
        }
    )
    assert store.get("prime.before.value") == "[]"
    assert store.get("prime.later.value") == "x"


def test_an_unknown_name_inside_a_file_sourced_template_is_a_compile_error(
    tmp_path: Path,
) -> None:
    _write(tmp_path / "draft.md", "{{> nope}}")
    with pytest.raises(ValueError, match=r"'\{\{> nope\}\}' does not name"):
        compile_orchestration(
            orch={
                "effects": [
                    {"type": "yield", "name": "y", "template": {"file": "draft.md"}}
                ]
            },
            document_dir=tmp_path,
            confinement_root=tmp_path,
        )


def test_a_valid_name_inside_a_file_sourced_message_content_is_accepted(
    tmp_path: Path,
) -> None:
    _write(tmp_path / "draft.md", "{{> voice}}")
    store = _run(
        {
            "prompts": {"voice": "Plain, direct."},
            "effects": [
                {
                    "type": "prompt",
                    "name": "p",
                    "messages": [{"role": "user", "content": {"file": "draft.md"}}],
                }
            ],
        },
        document_dir=tmp_path,
        confinement_root=tmp_path,
    )
    assert store.get("prime.p.meta.error") is None


# ── prompt files: `{file: <path>}` ──────────────────────────────────────────


def _write(path: Path, text: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    return path


def test_prompt_file_loads_declared_prompt_text(tmp_path: Path) -> None:
    _write(tmp_path / "prompts" / "voice.md", "Plain, direct.")
    orch = {
        "prompts": {"voice": {"file": "prompts/voice.md"}},
        "effects": [{"type": "yield", "name": "y", "template": "{{> voice}}"}],
    }
    store = _run(orch, document_dir=tmp_path, confinement_root=tmp_path)
    assert store.get("prime.y.value") == "Plain, direct."


def test_prompt_file_loads_a_yield_template(tmp_path: Path) -> None:
    _write(tmp_path / "brief.md", "Audience: {{input.audience}}")
    orch = {"effects": [{"type": "yield", "name": "y", "template": {"file": "brief.md"}}]}
    store = _run(
        orch,
        document_dir=tmp_path,
        confinement_root=tmp_path,
        initial_state={"input": {"audience": "devs"}},
    )
    assert store.get("prime.y.value") == "Audience: devs"


def test_prompt_file_outside_the_project_is_a_compile_error(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    outside = tmp_path / "outside.md"
    _write(outside, "nope")
    orch = {"prompts": {"x": {"file": "../outside.md"}}, "effects": []}
    with pytest.raises(ValueError, match="resolves outside the project"):
        compile_orchestration(orch=orch, document_dir=project, confinement_root=project)


def test_prompt_file_inside_the_project_via_dotdot_is_allowed(tmp_path: Path) -> None:
    root = tmp_path
    (root / "circuitry.config.json").write_text("{}", encoding="utf-8")
    doc_dir = root / "docs"
    doc_dir.mkdir()
    _write(root / "shared" / "voice.md", "Shared voice.")
    orch = {
        "prompts": {"voice": {"file": "../shared/voice.md"}},
        "effects": [{"type": "yield", "name": "y", "template": "{{> voice}}"}],
    }
    store = _run(orch, document_dir=doc_dir, confinement_root=root)
    assert store.get("prime.y.value") == "Shared voice."


def test_prompt_file_absolute_path_is_a_compile_error(tmp_path: Path) -> None:
    target = _write(tmp_path / "x.md", "x")
    orch = {"prompts": {"x": {"file": str(target)}}, "effects": []}
    with pytest.raises(ValueError, match="must be a relative path"):
        compile_orchestration(orch=orch, document_dir=tmp_path, confinement_root=tmp_path)


def test_prompt_file_symlink_pointing_out_is_a_compile_error(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    outside = tmp_path / "outside.md"
    _write(outside, "nope")
    link = project / "link.md"
    link.symlink_to(outside)
    orch = {"prompts": {"x": {"file": "link.md"}}, "effects": []}
    with pytest.raises(ValueError, match="resolves outside the project"):
        compile_orchestration(orch=orch, document_dir=project, confinement_root=project)


def test_missing_prompt_file_is_a_compile_error(tmp_path: Path) -> None:
    orch = {"prompts": {"x": {"file": "nope.md"}}, "effects": []}
    with pytest.raises(ValueError, match="does not exist"):
        compile_orchestration(orch=orch, document_dir=tmp_path, confinement_root=tmp_path)


def test_a_document_with_no_file_of_its_own_cannot_use_file(tmp_path: Path) -> None:
    """`document_dir=None` (the default) is what a generated document, or one
    with no path at all, compiles with — `file:` is a compile error there."""
    with pytest.raises(ValueError, match="generated at run time"):
        compile_orchestration(orch={"prompts": {"x": {"file": "x.md"}}, "effects": []})


def test_use_inline_child_cannot_use_file(tmp_path: Path) -> None:
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "u",
                "inline": "prompts: {x: {file: x.md}}\neffects: []",
                "on_error": "continue",
            }
        ]
    }
    store = _run(orch, document_dir=tmp_path, confinement_root=tmp_path)
    assert "generated at run time" in store.get("prime.u.meta.error")


def test_a_library_documents_prompt_file_stays_inside_its_source(tmp_path: Path) -> None:
    lib = tmp_path / "lib"
    _write(lib / "voice.md", "Library voice.")
    _write(
        lib / "helper.yml",
        yaml.safe_dump(
            {
                "prompts": {"voice": {"file": "voice.md"}},
                "effects": [{"type": "yield", "name": "y", "template": "{{> voice}}"}],
            }
        ),
    )
    outside_project = tmp_path / "caller.yml"
    _write(
        outside_project,
        yaml.safe_dump(
            {"effects": [{"type": "use", "name": "helper", "ref": "helper"}]}
        ),
    )
    cfg = CircuitryConfig(
        runtime={"library": {"sources": [{"type": "folder", "name": "local", "path": str(lib)}]}}
    )
    result = run(
        RunRequest(
            orchestration_path=outside_project,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=_mock_adapter(),
            skip_preflight=True,
            config=cfg,
        )
    )
    assert result.ok, result.error
    assert result.state["prime"]["helper"]["y"]["value"] == "Library voice."


# ── escaping (#397) rides the same render path ──────────────────────────────


def test_prompt_text_does_not_html_escape() -> None:
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "p",
                "template": 'Review: {{x}}',
                "inputs": {"x": 'Say "hi" & use <context> tags'},
            }
        ]
    }
    adapter = _mock_adapter()
    _run(orch, adapter=adapter)
    assert adapter.generate.call_args.kwargs["prompt"] == (
        'Review: Say "hi" & use <context> tags'
    )


def test_prompt_messages_content_does_not_html_escape() -> None:
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "p",
                "messages": [{"role": "user", "content": "{{x}}"}],
                "inputs": {"x": '<b>&"'},
            }
        ]
    }
    store = _run(orch)
    assert store.get("prime.p.meta.prompt_sent") == 'user: <b>&"'


def test_yield_template_does_not_html_escape() -> None:
    store = _run(
        {"effects": [{"type": "yield", "name": "y", "template": "{{x}}", "inputs": {"x": "<b>"}}]}
    )
    assert store.get("prime.y.value") == "<b>"


def test_declared_prompts_own_tags_never_escape_even_inside_a_tool_param() -> None:
    orch = {
        "prompts": {"voice": "{{input.x}}"},
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "json",
                "params": {"mode": "stringify", "input": "{{> voice}}"},
            }
        ],
    }
    store = _run(orch, initial_state={"input": {"x": "<b>"}})
    assert store.get("prime.t.value") == '"<b>"'


def test_tool_param_still_escapes_its_own_tags() -> None:
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "json",
                "params": {"mode": "stringify", "input": "{{input.x}}"},
            }
        ]
    }
    store = _run(orch, initial_state={"input": {"x": "<b>"}})
    assert store.get("prime.t.value") == '"&lt;b&gt;"'
