#!/usr/bin/env python3
"""Golden corpus for lane C's `compile_document` (issue #408's Test
strategy section) -- every effect type's happy path, every compile
error message `core/compiler.py`/`core/state_ns.py` raises that this
lane ports, a CRLF document, and `use` cycles.

Deliberately out of scope (lane D's own corpus,
`generate_compiler_compose_corpus.py`): `prompts:`, `{{> name}}`
partials, and anything else from the compile-time half of #406.

Must be run with Python 3.11 (the lane venv locally;
`actions/setup-python` 3.11 in CI). Usage:
    python3 generate_compiler_compile_corpus.py [--check]
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from _compiler_corpus import render_corpus, run_case, write_or_check

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-compiler"
    / "tests"
    / "golden"
    / "compile.json"
)


def doc(*, name: str = "doc.yml", text: str) -> dict:
    return {"name": name.rsplit(".", 1)[0], "files": {name: text}, "entry": name}


CASES: list[dict] = []


def add(case: dict) -> None:
    CASES.append(case)


# --- Happy paths: every effect type -----------------------------------

add(doc(
    text=(
        "effects:\n"
        "  - type: prompt\n"
        "    name: greet\n"
        "    template: 'Hello, {{input.name}}!'\n"
    )
))

add({
    "name": "prompt_with_messages",
    "files": {"doc.yml": (
        "effects:\n"
        "  - type: prompt\n"
        "    name: chat\n"
        "    messages:\n"
        "      - role: system\n"
        "        content: 'You are helpful.'\n"
        "      - role: user\n"
        "        content: 'Hi {{input.name}}'\n"
    )},
    "entry": "doc.yml",
})

add({
    "name": "prompt_json_with_schema",
    "files": {"doc.yml": (
        "effects:\n"
        "  - type: prompt\n"
        "    name: extract\n"
        "    template: 'Extract JSON'\n"
        "    prompt_type: json\n"
        "    schema:\n"
        "      type: object\n"
    )},
    "entry": "doc.yml",
})

add({
    "name": "tool_happy_path",
    "files": {"doc.yml": (
        "effects:\n"
        "  - type: tool\n"
        "    name: fetch\n"
        "    provider: shell\n"
        "    params:\n"
        "      cmd: 'echo {{input.name}}'\n"
        "      allowed_commands: ['echo']\n"
        "    retries:\n"
        "      max_attempts: 3\n"
        "      backoff_ms: 500\n"
        "    expect: 'value.ok == true'\n"
    )},
    "entry": "doc.yml",
})

add({
    "name": "tool_from_reference_param",
    "files": {"doc.yml": (
        "effects:\n"
        "  - type: tool\n"
        "    name: fetch\n"
        "    provider: shell\n"
        "    params:\n"
        "      count: {from: 'input.n', default: 1}\n"
    )},
    "entry": "doc.yml",
})

add({
    "name": "use_path_happy_path",
    "files": {
        "doc.yml": (
            "effects:\n"
            "  - type: use\n"
            "    name: sub\n"
            "    path: child.yml\n"
            "    inputs:\n"
            "      topic: '{{input.topic}}'\n"
            "    outputs:\n"
            "      result: {path: 'prime.sub.value'}\n"
        ),
        "child.yml": "effects: []\n",
    },
    "entry": "doc.yml",
})

add({
    "name": "use_inline_happy_path",
    "files": {"doc.yml": (
        "effects:\n"
        "  - type: use\n"
        "    name: sub\n"
        "    inline: 'effects: []'\n"
    )},
    "entry": "doc.yml",
})

add({
    "name": "yield_happy_path",
    "files": {"doc.yml": (
        "effects:\n"
        "  - type: yield\n"
        "    name: out\n"
        "    template: 'Value is {{input.n}}'\n"
    )},
    "entry": "doc.yml",
})

add({
    "name": "reflector_happy_path",
    "files": {"doc.yml": (
        "effects:\n"
        "  - type: reflector\n"
        "    name: think\n"
        "    effects:\n"
        "      - type: prompt\n"
        "        name: propose_steps\n"
        "        template: 'propose'\n"
    )},
    "entry": "doc.yml",
})

add({
    "name": "dynamic_nested_happy_path",
    "files": {"doc.yml": (
        "effects:\n"
        "  - type: dynamic\n"
        "    name: group\n"
        "    flow: tree\n"
        "    effects:\n"
        "      - type: prompt\n"
        "        name: a\n"
        "        template: 'a'\n"
        "      - type: prompt\n"
        "        name: b\n"
        "        template: 'b'\n"
        "    finally:\n"
        "      - type: prompt\n"
        "        name: cleanup\n"
        "        template: 'done'\n"
    )},
    "entry": "doc.yml",
})

add({
    "name": "conditional_cel_happy_path",
    "files": {"doc.yml": (
        "effects:\n"
        "  - type: if\n"
        "    name: branch\n"
        "    if:\n"
        "      mode: cel\n"
        "      expr: 'state.input.ok == true'\n"
        "    then:\n"
        "      - type: prompt\n"
        "        name: yes_branch\n"
        "        template: 'yes'\n"
        "    else:\n"
        "      - type: prompt\n"
        "        name: no_branch\n"
        "        template: 'no'\n"
    )},
    "entry": "doc.yml",
})

add({
    "name": "conditional_model_happy_path",
    "files": {"doc.yml": (
        "effects:\n"
        "  - type: if\n"
        "    if:\n"
        "      template: 'Is this true?'\n"
        "    then:\n"
        "      - type: prompt\n"
        "        name: yes_branch\n"
        "        template: 'yes'\n"
    )},
    "entry": "doc.yml",
})

add({
    "name": "loop_each_chain_happy_path",
    "files": {"doc.yml": (
        "effects:\n"
        "  - type: loop\n"
        "    name: iterate\n"
        "    each:\n"
        "      in: input.items\n"
        "      as: item\n"
        "    collect: handle\n"
        "    body:\n"
        "      - type: prompt\n"
        "        name: handle\n"
        "        template: 'item is {{item}}'\n"
    )},
    "entry": "doc.yml",
})

add({
    "name": "loop_while_happy_path",
    "files": {"doc.yml": (
        "effects:\n"
        "  - type: loop\n"
        "    name: poll\n"
        "    while:\n"
        "      mode: cel\n"
        "      expr: 'state.iter.index < 3'\n"
        "    body:\n"
        "      - type: prompt\n"
        "        name: tick\n"
        "        template: 'tick {{_loop_index}}'\n"
    )},
    "entry": "doc.yml",
})

add({
    "name": "loop_each_tree_happy_path",
    "files": {"doc.yml": (
        "effects:\n"
        "  - type: loop\n"
        "    name: parallel_iterate\n"
        "    flow: tree\n"
        "    each:\n"
        "      in: input.items\n"
        "    body:\n"
        "      - type: prompt\n"
        "        name: handle\n"
        "        template: 'item is {{item}}'\n"
    )},
    "entry": "doc.yml",
})

add({
    "name": "unnamed_loop_is_transparent",
    "files": {"doc.yml": (
        "effects:\n"
        "  - type: loop\n"
        "    each:\n"
        "      in: input.items\n"
        "    body:\n"
        "      - type: prompt\n"
        "        name: handle\n"
        "        template: 'item is {{item}}'\n"
    )},
    "entry": "doc.yml",
})

add({
    "name": "crlf_document",
    "files": {"doc.yml": (
        "effects:\r\n"
        "  - type: prompt\r\n"
        "    name: greet\r\n"
        "    template: |\r\n"
        "      line one\r\n"
        "      line two {{input.name}}\r\n"
    )},
    "entry": "doc.yml",
})


# --- Compile errors: _validate_name -------------------------------------

add(doc(name="bad.yml", text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: 123\n"
    "    provider: shell\n"
)))
CASES[-1]["name"] = "name_must_be_string"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: '  '\n"
    "    provider: shell\n"
)))
CASES[-1]["name"] = "name_empty_whitespace"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: ' fetch'\n"
    "    provider: shell\n"
)))
CASES[-1]["name"] = "name_leading_whitespace"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: 'a.b'\n"
    "    provider: shell\n"
)))
CASES[-1]["name"] = "name_contains_dot"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: 'a b'\n"
    "    provider: shell\n"
)))
CASES[-1]["name"] = "name_contains_whitespace"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: 'iter_3'\n"
    "    provider: shell\n"
)))
CASES[-1]["name"] = "name_reserved_iter_pattern"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: 'value'\n"
    "    provider: shell\n"
)))
CASES[-1]["name"] = "name_reserved_structural_slot"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: '1abc'\n"
    "    provider: shell\n"
)))
CASES[-1]["name"] = "name_bad_pattern"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: dup\n"
    "    provider: shell\n"
    "  - type: tool\n"
    "    name: dup\n"
    "    provider: shell\n"
)))
CASES[-1]["name"] = "duplicate_effect_name_in_scope"


# --- Compile errors: structure / dispatch -------------------------------

add(doc(text="effects:\n  - name: x\n"))
CASES[-1]["name"] = "effect_missing_type"

add(doc(text="effects:\n  - type: bogus\n    name: x\n"))
CASES[-1]["name"] = "unsupported_effect_type"

add(doc(text="effects:\n  - type: tool\n    provider: shell\n"))
CASES[-1]["name"] = "tool_missing_name"

add(doc(text="effects:\n  - type: dynamic\n    effects: []\n"))
CASES[-1]["name"] = "dynamic_missing_name"

add(doc(text="effects:\n  - type: use\n    path: child.yml\n"))
CASES[-1]["name"] = "use_missing_name"

add(doc(text="effects:\n  - type: yield\n    template: x\n"))
CASES[-1]["name"] = "yield_missing_name"

add(doc(text="effects:\n  - type: reflector\n    effects: []\n"))
CASES[-1]["name"] = "reflector_missing_name"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    finally: []\n"
)))
CASES[-1]["name"] = "finally_not_allowed_on_tool"


# --- Compile errors: conditional/loop ------------------------------------

add(doc(text="effects:\n  - type: if\n    then: []\n"))
CASES[-1]["name"] = "conditional_missing_if"

add(doc(text=(
    "effects:\n"
    "  - type: if\n"
    "    if: {mode: cel}\n"
    "    then: []\n"
)))
CASES[-1]["name"] = "conditional_cel_missing_expr"

add(doc(text=(
    "effects:\n"
    "  - type: if\n"
    "    if: {mode: model}\n"
    "    then: []\n"
)))
CASES[-1]["name"] = "conditional_model_missing_template"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    name: bad\n"
    "    while: {mode: cel, expr: 'true'}\n"
    "    each: {in: input.items}\n"
    "    body: []\n"
)))
CASES[-1]["name"] = "loop_both_while_and_each"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    name: bad\n"
    "    body: []\n"
)))
CASES[-1]["name"] = "loop_neither_while_nor_each"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    each: {in: input.items}\n"
    "    collect: handle\n"
    "    body:\n"
    "      - type: prompt\n"
    "        name: handle\n"
    "        template: x\n"
)))
CASES[-1]["name"] = "loop_collect_needs_a_name"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    name: shots\n"
    "    flow: tree\n"
    "    each: {in: input.items}\n"
    "    body:\n"
    "      - type: prompt\n"
    "        name: handle\n"
    "        template: 'prev was {{prime.shots.prev.value}}'\n"
)))
CASES[-1]["name"] = "tree_loop_rejects_prev_reference"


# --- Compile errors: tool -------------------------------------------------

add(doc(text="effects:\n  - type: tool\n    name: x\n"))
CASES[-1]["name"] = "tool_missing_provider"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    params_json: {not: a string}\n"
)))
CASES[-1]["name"] = "tool_params_json_wrong_type"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    params:\n"
    "      allowed_commands: {from: 'input.cmds'}\n"
)))
CASES[-1]["name"] = "tool_security_sensitive_param_rejects_reference"


# --- Compile errors: use --------------------------------------------------

add(doc(text="effects:\n  - type: use\n    name: x\n"))
CASES[-1]["name"] = "use_requires_one_reference_field"

add(doc(text=(
    "effects:\n"
    "  - type: use\n"
    "    name: x\n"
    "    path: a.yml\n"
    "    inline: 'effects: []'\n"
)))
CASES[-1]["name"] = "use_multiple_reference_fields"

add(doc(text="effects:\n  - type: use\n    name: x\n    ref: some/library\n"))
CASES[-1]["name"] = "use_ref_rejected_as_electricity_divergence"


# --- Compile errors: yield -------------------------------------------------

add(doc(text=(
    "effects:\n"
    "  - type: yield\n"
    "    name: x\n"
    "    template: 'hi'\n"
    "    schema: {type: object}\n"
)))
CASES[-1]["name"] = "yield_forbidden_key"

add(doc(text="effects:\n  - type: yield\n    name: x\n"))
CASES[-1]["name"] = "yield_missing_template"


# --- Compile errors: prompt -------------------------------------------------

add(doc(text=(
    "effects:\n"
    "  - type: prompt\n"
    "    name: x\n"
    "    template: 'hi'\n"
    "    prompt_type: image\n"
)))
CASES[-1]["name"] = "prompt_image_type_rejected"

add(doc(text=(
    "effects:\n"
    "  - type: prompt\n"
    "    name: x\n"
    "    template: 'hi'\n"
    "    prompt_type: json\n"
)))
CASES[-1]["name"] = "prompt_json_requires_schema"

add(doc(text="effects:\n  - type: prompt\n    name: x\n"))
CASES[-1]["name"] = "prompt_missing_template_or_messages"


# --- Compile errors: templates --------------------------------------------

add(doc(text=(
    "effects:\n"
    "  - type: prompt\n"
    "    name: x\n"
    "    template: '{{#unclosed}}'\n"
)))
CASES[-1]["name"] = "malformed_template_is_rejected"


# --- Compile errors: state_ns ----------------------------------------------

add(doc(text=(
    "effects:\n"
    "  - type: if\n"
    "    if: {mode: cel, expr: 'state.bogus.x == 1'}\n"
    "    then: []\n"
)))
CASES[-1]["name"] = "cel_expr_rejects_unknown_namespace"

add(doc(text=(
    "effects:\n"
    "  - type: if\n"
    "    if: {mode: cel, expr: 'state.a =='}\n"
    "    then: []\n"
)))
CASES[-1]["name"] = "cel_expr_does_not_parse"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    name: it\n"
    "    each: {in: 'bogus'}\n"
    "    body: []\n"
)))
CASES[-1]["name"] = "each_in_path_rejects_bare_key"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    params:\n"
    "      n: {from: 'bogus'}\n"
)))
CASES[-1]["name"] = "from_reference_rejects_bare_key"

add({
    "name": "bare_input_ref_collides_with_declared_input",
    "files": {"doc.yml": (
        "interface:\n"
        "  inputs:\n"
        "    topic: {type: string}\n"
        "effects:\n"
        "  - type: prompt\n"
        "    name: x\n"
        "    template: 'about {{topic}}'\n"
    )},
    "entry": "doc.yml",
})

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    params:\n"
    "      n: {from: 'state.topic'}\n"
)))
CASES[-1]["name"] = "from_reference_rejects_state_prefixed"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    params:\n"
    "      n: {from: ''}\n"
)))
CASES[-1]["name"] = "from_reference_rejects_empty"

add(doc(text=(
    "effects:\n"
    "  - type: use\n"
    "    name: sub\n"
    "    path: child.yml\n"
    "    inputs:\n"
    "      n: {from: 'bogus'}\n"
)))
CASES[-1]["files"]["child.yml"] = "effects: []\n"
CASES[-1]["name"] = "use_input_from_reference_rejects_bare_key"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    name: it\n"
    "    each: {in: 'state.input.items'}\n"
    "    body:\n"
    "      - type: prompt\n"
    "        name: handle\n"
    "        template: x\n"
)))
CASES[-1]["name"] = "each_in_path_rejects_state_prefixed"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    name: it\n"
    "    each: {in: ''}\n"
    "    body:\n"
    "      - type: prompt\n"
    "        name: handle\n"
    "        template: x\n"
)))
CASES[-1]["name"] = "each_in_path_rejects_empty"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    name: outer\n"
    "    each: {in: input.items, as: item}\n"
    "    body:\n"
    "      - type: if\n"
    "        if: {mode: cel, expr: 'state.bogus == 1'}\n"
    "        then: []\n"
)))
CASES[-1]["name"] = "cel_expr_unknown_namespace_lists_loop_bindings"

add(doc(text=(
    "effects:\n"
    "  - type: if\n"
    "    if: {mode: cel, expr: '   '}\n"
    "    then: []\n"
)))
CASES[-1]["name"] = "conditional_cel_whitespace_only_expression_is_empty"

add(doc(text=(
    "effects:\n"
    "  - type: if\n"
    "    if: {mode: cel, expr: '" + ("x" * 5000) + "'}\n"
    "    then: []\n"
)))
CASES[-1]["name"] = "cel_expression_too_long"


# --- Compile errors: structure / dispatch, more shapes ---------------------

add(doc(text="effects: not-a-list\n"))
CASES[-1]["name"] = "effects_must_be_a_list"

add(doc(text="effects:\n  - - not-a-mapping\n"))
CASES[-1]["name"] = "effect_must_be_a_mapping"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    name: it\n"
    "    each: not-a-mapping\n"
    "    body: []\n"
)))
CASES[-1]["name"] = "loop_each_must_be_a_mapping"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    name: it\n"
    "    while: {mode: model}\n"
    "    body: []\n"
)))
CASES[-1]["name"] = "loop_while_model_missing_template"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    name: poll\n"
    "    while: {mode: cel}\n"
    "    body: []\n"
)))
CASES[-1]["name"] = "loop_while_cel_missing_expr"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    name: poll\n"
    "    while: {mode: cel, expr: '   '}\n"
    "    body: []\n"
)))
CASES[-1]["name"] = "loop_while_cel_empty_expression_has_its_own_label"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    name: poll\n"
    "    while: {mode: cel, expr: 'state.a =='}\n"
    "    body: []\n"
)))
CASES[-1]["name"] = "loop_while_cel_parse_error_has_its_own_label"
CASES[-1]["error_modes"] = {"run_error": "location"}


# --- Compile errors: expect (tool/use) --------------------------------------

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    expect: '   '\n"
)))
CASES[-1]["name"] = "expect_bare_string_empty"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    expect: {mode: cel}\n"
)))
CASES[-1]["name"] = "expect_cel_missing_expr"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    expect: {mode: model}\n"
)))
CASES[-1]["name"] = "expect_model_missing_template"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    expect: 1\n"
)))
CASES[-1]["name"] = "expect_invalid_shape"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    expect:\n"
    "      mode: model\n"
    "      template: |\n"
    "        Is it ok?\n"
)))
CASES[-1]["name"] = "expect_model_template_keeps_trailing_newline"


# --- Compile errors: prompt (more shapes) -----------------------------------

add(doc(text="effects:\n  - type: prompt\n    template: x\n"))
CASES[-1]["name"] = "prompt_missing_name"

add(doc(text=(
    "effects:\n"
    "  - type: prompt\n"
    "    name: x\n"
    "    template: 123\n"
)))
CASES[-1]["name"] = "prompt_template_wrong_type"

add(doc(text=(
    "effects:\n"
    "  - type: yield\n"
    "    name: x\n"
    "    template: '   '\n"
)))
CASES[-1]["name"] = "yield_template_must_not_be_empty"


# --- Compile errors: templates (more fields) --------------------------------

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    params_json: '{{#bad'\n"
)))
CASES[-1]["name"] = "malformed_params_json_template"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    prompt: '{{#bad'\n"
)))
CASES[-1]["name"] = "malformed_tool_prompt_template"

add(doc(text=(
    "effects:\n"
    "  - type: use\n"
    "    name: x\n"
    "    inline: '{{#bad'\n"
)))
CASES[-1]["name"] = "malformed_inline_template"

add(doc(text=(
    "effects:\n"
    "  - type: prompt\n"
    "    name: x\n"
    "    messages:\n"
    "      - role: user\n"
    "        content: '{{#bad'\n"
)))
CASES[-1]["name"] = "malformed_message_content_template"

add(doc(text=(
    "effects:\n"
    "  - type: prompt\n"
    "    name: x\n"
    "    template: hi\n"
    "    assets:\n"
    "      - kind: image\n"
    "        ref: '{{#bad'\n"
)))
CASES[-1]["name"] = "malformed_asset_ref_template"

add(doc(text=(
    "effects:\n"
    "  - type: if\n"
    "    if: {template: '{{#bad'}\n"
    "    then: []\n"
)))
CASES[-1]["name"] = "malformed_if_template"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    name: it\n"
    "    while: {template: '{{#bad'}\n"
    "    body: []\n"
)))
CASES[-1]["name"] = "malformed_while_template"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    expect: {mode: model, template: '{{#bad'}\n"
)))
CASES[-1]["name"] = "malformed_expect_template"


# --- Compile errors: outputs -------------------------------------------------

add(doc(text=(
    "effects:\n"
    "  - type: use\n"
    "    name: sub\n"
    "    path: child.yml\n"
    "    outputs: not-a-mapping\n"
)))
CASES[-1]["files"]["child.yml"] = "effects: []\n"
CASES[-1]["name"] = "outputs_must_be_a_mapping"

add(doc(text=(
    "effects:\n"
    "  - type: use\n"
    "    name: sub\n"
    "    path: child.yml\n"
    "    outputs:\n"
    "      result: ''\n"
)))
CASES[-1]["files"]["child.yml"] = "effects: []\n"
CASES[-1]["name"] = "outputs_entry_empty_string"

add(doc(text=(
    "effects:\n"
    "  - type: use\n"
    "    name: sub\n"
    "    path: child.yml\n"
    "    outputs:\n"
    "      result: {}\n"
)))
CASES[-1]["files"]["child.yml"] = "effects: []\n"
CASES[-1]["name"] = "outputs_entry_missing_path"

add(doc(text=(
    "effects:\n"
    "  - type: use\n"
    "    name: sub\n"
    "    path: child.yml\n"
    "    outputs:\n"
    "      result: 5\n"
)))
CASES[-1]["files"]["child.yml"] = "effects: []\n"
CASES[-1]["name"] = "outputs_entry_invalid_type"


# --- Compile errors: duplicate names / bare refs, more shapes --------------

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: dup\n"
    "    provider: shell\n"
    "finally:\n"
    "  - type: tool\n"
    "    name: dup\n"
    "    provider: shell\n"
)))
CASES[-1]["name"] = "duplicate_name_between_body_and_finally"

add({
    "name": "bare_input_ref_in_finally",
    "files": {"doc.yml": (
        "interface:\n"
        "  inputs:\n"
        "    topic: {type: string}\n"
        "effects: []\n"
        "finally:\n"
        "  - type: prompt\n"
        "    name: x\n"
        "    template: 'about {{topic}}'\n"
    )},
    "entry": "doc.yml",
})

add({
    "name": "bare_input_ref_in_nested_params",
    "files": {"doc.yml": (
        "interface:\n"
        "  inputs:\n"
        "    topic: {type: string}\n"
        "effects:\n"
        "  - type: tool\n"
        "    name: x\n"
        "    provider: shell\n"
        "    params:\n"
        "      nested:\n"
        "        deep: 'about {{topic}}'\n"
    )},
    "entry": "doc.yml",
})


# --- Compile errors: Unknown flow value -------------------------------------

add(doc(text="effects: []\nflow: bogus\n"))
CASES[-1]["name"] = "unknown_flow_value"


# --- Finding 18: an explicit `null` name/template is "absent", not --
# --- "the wrong type" ---------------------------------------------

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: null\n"
    "    provider: shell\n"
)))
CASES[-1]["name"] = "explicit_null_name_on_a_required_name_type"

add(doc(text=(
    "effects:\n"
    "  - type: if\n"
    "    name: null\n"
    "    if: {mode: cel, expr: 'true'}\n"
    "    then: []\n"
)))
CASES[-1]["name"] = "explicit_null_name_on_a_conditional_is_unnamed"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    name: null\n"
    "    each: {in: input.items}\n"
    "    body:\n"
    "      - type: prompt\n"
    "        name: handle\n"
    "        template: x\n"
)))
CASES[-1]["name"] = "explicit_null_name_on_a_loop_is_unnamed"

add(doc(text=(
    "effects:\n"
    "  - type: yield\n"
    "    name: x\n"
    "    template: null\n"
)))
CASES[-1]["name"] = "explicit_null_yield_template_must_have_template"


# --- Finding 20: `inline`'s syntax check runs against the raw, --------
# --- untrimmed text, so a leading blank line doesn't shift the --------
# --- "at line N" a malformed-template message carries -----------------

add(doc(text=(
    "effects:\n"
    "  - type: use\n"
    "    name: x\n"
    "    inline: \"\\n\\n{{#a}}\"\n"
)))
CASES[-1]["name"] = "inline_checked_against_the_untrimmed_text"


# --- Findings 15/16: is-not-None coercions and integer saturation ----

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    timeout_ms: 0\n"
)))
CASES[-1]["name"] = "tool_timeout_ms_zero_is_kept_not_absent"

add(doc(text=(
    "effects:\n"
    "  - type: prompt\n"
    "    name: x\n"
    "    template: hi\n"
    "    timeout_ms: 0\n"
)))
CASES[-1]["name"] = "prompt_timeout_ms_zero_is_kept_not_absent"

add(doc(text=(
    "effects:\n"
    "  - type: dynamic\n"
    "    name: g\n"
    "    flow: tree\n"
    "    max_concurrency: 0\n"
    "    effects: []\n"
)))
CASES[-1]["name"] = "dynamic_max_concurrency_zero_is_kept_not_absent"


# --- Decision 2: {file:} prompt/yield sources, now that lane D's own ---
# --- prompt_files::resolve_text_or_file is wired in ---------------------

add({
    "name": "prompt_template_from_file",
    "files": {
        "doc.yml": (
            "effects:\n"
            "  - type: prompt\n"
            "    name: greet\n"
            "    template: {file: template.txt}\n"
        ),
        "template.txt": "Hello, {{input.name}}!",
    },
    "entry": "doc.yml",
})

add({
    "name": "prompt_message_content_from_file",
    "files": {
        "doc.yml": (
            "effects:\n"
            "  - type: prompt\n"
            "    name: chat\n"
            "    messages:\n"
            "      - role: user\n"
            "        content: {file: msg.txt}\n"
        ),
        "msg.txt": "Hi {{input.name}}",
    },
    "entry": "doc.yml",
})

add({
    "name": "yield_template_from_file",
    "files": {
        "doc.yml": (
            "effects:\n"
            "  - type: yield\n"
            "    name: out\n"
            "    template: {file: yield.txt}\n"
        ),
        "yield.txt": "Value is {{input.n}}",
    },
    "entry": "doc.yml",
})

add(doc(text=(
    "effects:\n"
    "  - type: prompt\n"
    "    name: greet\n"
    "    template: {file: does-not-exist.txt}\n"
)))
CASES[-1]["name"] = "prompt_template_file_missing"


# --- Findings 5/6: check order within a single effect -----------------------

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    params:\n"
    "      allowed_commands: {from: 'bogus'}\n"
)))
CASES[-1]["name"] = "tool_param_leaves_checked_before_security_sensitive_reference"

add(doc(text=(
    "effects:\n"
    "  - type: tool\n"
    "    name: x\n"
    "    provider: shell\n"
    "    params:\n"
    "      a: '{{#bad'\n"
    "      allowed_commands: {from: 'input.c'}\n"
)))
CASES[-1]["name"] = "tool_param_leaves_checked_before_security_sensitive_literal"

add(doc(text=(
    "effects:\n"
    "  - type: use\n"
    "    name: x\n"
    "    path: child.yml\n"
    "    inputs:\n"
    "      a: {from: 'bogus'}\n"
    "      b: '{{#bad'\n"
)))
CASES[-1]["files"]["child.yml"] = "effects: []\n"
CASES[-1]["name"] = "use_inputs_templates_checked_before_references"


# --- Finding 7: collect '' on an unnamed loop is not rejected ---------------

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    each: {in: input.items}\n"
    "    collect: ''\n"
    "    body:\n"
    "      - type: prompt\n"
    "        name: handle\n"
    "        template: x\n"
)))
CASES[-1]["name"] = "collect_empty_string_on_unnamed_loop_is_fine"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    each: {in: input.items}\n"
    "    collect: '   '\n"
    "    body:\n"
    "      - type: prompt\n"
    "        name: handle\n"
    "        template: x\n"
)))
CASES[-1]["name"] = "collect_whitespace_only_on_unnamed_loop_is_fine"


# --- Finding 8: prompt/yield params/inputs are raw, never checked ----------

add(doc(text=(
    "effects:\n"
    "  - type: prompt\n"
    "    name: x\n"
    "    template: hi\n"
    "    inputs:\n"
    "      v: '{{#raw'\n"
    "      w: {from: 'bogus'}\n"
    "    params:\n"
    "      stop: ['{{']\n"
)))
CASES[-1]["name"] = "prompt_params_and_inputs_are_raw_values"

add(doc(text=(
    "effects:\n"
    "  - type: yield\n"
    "    name: x\n"
    "    template: hi\n"
    "    inputs:\n"
    "      v: '{{#raw'\n"
    "      w: {from: 'bogus'}\n"
)))
CASES[-1]["name"] = "yield_inputs_are_raw_values"

add(doc(text=(
    "effects:\n"
    "  - type: loop\n"
    "    name: it\n"
    "    each: {in: input.items, as: item}\n"
    "    body:\n"
    "      - type: prompt\n"
    "        name: handle\n"
    "        template: hi\n"
    "        inputs:\n"
    "          v: {from: 'item.x'}\n"
)))
CASES[-1]["name"] = "prompt_inputs_in_a_loop_body_are_not_reference_checked"


# --- Finding 9: a custom prime_template is never Mustache-checked ----------

add(doc(text=(
    "effects:\n"
    "  - type: reflector\n"
    "    name: think\n"
    "    prime_template: 'Plan {goal}: {{#unclosed'\n"
    "    effects:\n"
    "      - type: prompt\n"
    "        name: propose_steps\n"
    "        template: x\n"
)))
CASES[-1]["name"] = "reflector_prime_template_is_never_mustache_checked"


# --- Finding 4: a reflector's inner scope is scope_child(scope, name) ------

add(doc(text=(
    "effects:\n"
    "  - type: reflector\n"
    "    name: think\n"
    "    effects:\n"
    "      - type: prompt\n"
    "        name: a\n"
    "        template: x\n"
    "      - type: prompt\n"
    "        name: a\n"
    "        template: y\n"
)))
CASES[-1]["name"] = "reflector_inner_duplicate_name_reports_the_inner_scope"


# --- Finding 12: group references, checked against Circuitry ---------------

add({
    "name": "concurrency_group_references_everywhere_an_effect_can_set_one",
    "files": {"doc.yml": (
        "runtime:\n"
        "  concurrency_groups:\n"
        "    io: 2\n"
        "effects:\n"
        "  - type: tool\n"
        "    name: fetch\n"
        "    provider: shell\n"
        "    group: io\n"
        "  - type: loop\n"
        "    name: it\n"
        "    each: {in: input.items}\n"
        "    body:\n"
        "      - type: prompt\n"
        "        name: ask\n"
        "        template: x\n"
        "        group: llm\n"
        "  - type: if\n"
        "    if: {mode: cel, expr: 'true'}\n"
        "    then:\n"
        "      - type: prompt\n"
        "        name: ask2\n"
        "        template: x\n"
        "        group: \"it's\"\n"
        "  - type: reflector\n"
        "    name: think\n"
        "    effects:\n"
        "      - type: prompt\n"
        "        name: propose_steps\n"
        "        template: x\n"
        "        group: llm\n"
        "finally:\n"
        "  - type: prompt\n"
        "    name: cleanup\n"
        "    template: x\n"
        "    group: llm\n"
    )},
    "entry": "doc.yml",
})


# --- use cycles -------------------------------------------------------------

add({
    "name": "use_cycle_two_hop",
    "files": {
        "a.yml": "effects:\n  - type: use\n    name: s\n    path: b.yml\n",
        "b.yml": "effects:\n  - type: use\n    name: s\n    path: a.yml\n",
    },
    "entry": "a.yml",
})

add({
    "name": "use_acyclic_chain",
    "files": {
        "a.yml": "effects:\n  - type: use\n    name: s\n    path: b.yml\n",
        "b.yml": "effects: []\n",
    },
    "entry": "a.yml",
})

add({
    # Decision 1: a `use` cycle that loops back through the *entry*
    # document itself, entered from the a.yml side.
    "name": "use_cycle_through_entry_document_entered_from_a",
    "files": {
        "a.yml": "effects:\n  - type: use\n    name: s\n    path: b.yml\n",
        "b.yml": "effects:\n  - type: use\n    name: s\n    path: a.yml\n",
    },
    "entry": "a.yml",
})

add({
    # Same two files, entered from the other side -- the cycle's own
    # chain (and its rotation) differs by entry point.
    "name": "use_cycle_through_entry_document_entered_from_b",
    "files": {
        "a.yml": "effects:\n  - type: use\n    name: s\n    path: b.yml\n",
        "b.yml": "effects:\n  - type: use\n    name: s\n    path: a.yml\n",
    },
    "entry": "b.yml",
})

add({
    "name": "use_cycle_document_uses_itself",
    "files": {
        "a.yml": "effects:\n  - type: use\n    name: s\n    path: a.yml\n",
    },
    "entry": "a.yml",
})

add({
    # Finding 13: a duplicate key in a `use` *child* (never the entry
    # document, which Circuitry's main loader -- not
    # `core/cycle_check.py::load_orch`'s plain `yaml.safe_load` --
    # reads) must not hide a cycle through it.
    "name": "use_cycle_through_a_child_with_a_duplicate_key",
    "files": {
        "entry.yml": "effects:\n  - type: use\n    name: s\n    path: a.yml\n",
        "a.yml": (
            "effects:\n"
            "  - type: use\n"
            "    name: s\n"
            "    name: s\n"
            "    path: b.yml\n"
        ),
        "b.yml": "effects:\n  - type: use\n    name: s\n    path: a.yml\n",
    },
    "entry": "entry.yml",
})


def main() -> int:
    results = [run_case(case) for case in CASES]
    text = render_corpus(results)
    return write_or_check(OUTPUT, text, check="--check" in sys.argv[1:])


if __name__ == "__main__":
    raise SystemExit(main())
