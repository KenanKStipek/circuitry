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


def main() -> int:
    results = [run_case(case) for case in CASES]
    text = render_corpus(results)
    return write_or_check(OUTPUT, text, check="--check" in sys.argv[1:])


if __name__ == "__main__":
    raise SystemExit(main())
