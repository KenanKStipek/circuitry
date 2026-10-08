#!/usr/bin/env python3
"""Lane B's golden corpus: `load_document`, `structural_errors`/
`unknown_key_warnings`, and the concurrency-configuration errors
(issue #408's lane B section).

Every case here fails before `compile_orchestration` -- a load error, a
structural error, or (for `check_for_run` specifically) a concurrency-
configuration error -- so every case is fully checkable against this
lane alone, with one narrow exception: `validate()`'s own concurrency-
configuration check runs *after* a document compiles (confirmed
directly against `cli/runtime_shim.py`), so the two
`*_concurrency_config_error` cases' `check_report` side still needs a
real compile (lane C) even though their `check_for_run` side -- which
checks the same configuration *before* structural checks even start --
needs nothing further; `check_report_needs` marks that.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI). Usage: python3 generate_compiler_load_corpus.py [--check]
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

from _compiler_corpus import render_corpus, run_case, write_or_check

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-compiler"
    / "tests"
    / "golden"
    / "load.json"
)

_TOOL_YML = (
    "    name: t\n"
    "    provider: json\n"
    "    params: {mode: stringify, input: 1}\n"
)

#: A one-line flow-style tool effect, for embedding inside `[...]`/`{...}`
#: flow syntax (a `loop`'s `body:`) where block style doesn't apply.
_TOOL_FLOW = "{type: tool, name: t, provider: json, params: {mode: stringify, input: 1}}"

CASES: list[dict[str, Any]] = [
    {
        "name": "unsupported_suffix_md",
        "files": {"doc.md": "effects: []\n"},
        "entry": "doc.md",
    },
    {
        "name": "toon_refused",
        "files": {"doc.toon": "effects[0]: 1\n"},
        "entry": "doc.toon",
        # Circuitry's own message (`toon-format` isn't installed here)
        # and electricity's are a documented divergence (issue #408's
        # Scope section) — deliberately different text, so neither side
        # is compared against the other; `golden_load.rs` asserts
        # electricity's own wording directly instead.
        "error_modes": {"validate_errors": ["location"], "run_error": "location"},
    },
    {
        "name": "empty_yaml_file",
        "files": {"doc.yml": ""},
        "entry": "doc.yml",
        # `check_report`'s own "Orchestration file is empty." is
        # Circuitry's own text (exact); `check_for_run` loads an empty
        # YAML file as `{}` and then fails the schema's own "'effects'
        # is a required property" check -- third-party `jsonschema`
        # text (location only).
        "error_modes": {"validate_errors": ["exact"], "run_error": "location"},
    },
    {
        "name": "empty_json_file",
        "files": {"doc.json": ""},
        "entry": "doc.json",
        # `load_json("")` has no YAML-style `or {}` fallback, so
        # `check_for_run` fails with the JSON reader's own syntax
        # message instead -- also third-party-shaped text.
        "error_modes": {"validate_errors": ["exact"], "run_error": "location"},
    },
    {
        "name": "whitespace_only_file",
        "files": {"doc.yml": "   \t\n  \n"},
        "entry": "doc.yml",
        "error_modes": {"validate_errors": ["exact"], "run_error": "location"},
    },
    {
        "name": "non_mapping_root_yaml_truthy",
        "files": {"doc.yml": "- 1\n- 2\n"},
        "entry": "doc.yml",
        # A truthy non-mapping root fails to load outright on both
        # surfaces, with Circuitry's own exact message either way.
        "error_modes": {"validate_errors": ["exact"], "run_error": "exact"},
    },
    {
        "name": "non_mapping_root_yaml_falsy_becomes_empty_dict",
        "files": {"doc.yml": "false\n"},
        "entry": "doc.yml",
        # `load_yaml("false\n") or {}` -- a falsy root becomes `{}`
        # (Python truthiness, not just `is None`), so this loads fine
        # and only then fails the schema's required-property check --
        # third-party `jsonschema` text on both surfaces here (`check_
        # report` has no "Orchestration file is empty." short circuit
        # to beat it to, unlike `empty_yaml_file`'s own case).
        "error_modes": {"validate_errors": ["location"], "run_error": "location"},
    },
    {
        "name": "non_mapping_root_json",
        "files": {"doc.json": "[]\n"},
        "entry": "doc.json",
        # JSON has no such `or {}` fallback: an empty (falsy) list is
        # still a load error, Circuitry's own exact message.
        "error_modes": {"validate_errors": ["exact"], "run_error": "exact"},
    },
    {
        "name": "duplicate_key_yaml",
        "files": {
            "doc.yml": "name: a\neffects: []\nname: b\n",
        },
        "entry": "doc.yml",
    },
    {
        "name": "duplicate_key_yaml_crlf",
        "files": {
            "doc.yml": "name: a\r\neffects: []\r\nname: b\r\n",
        },
        "entry": "doc.yml",
    },
    {
        "name": "duplicate_key_json",
        "files": {
            "doc.json": '{"name": "a", "effects": [], "name": "b"}',
        },
        "entry": "doc.json",
    },
    {
        "name": "lone_cr_near_miss_key",
        "files": {
            "doc.yml": (
                "effects:\r"
                "  - type: loop\r"
                "    name: poll\r"
                "    each: {in: 'input.xs'}\r"
                "    whlie: {mode: cel, expr: 'true'}\r"
                f"    body: [{_TOOL_FLOW}]\r"
            ),
        },
        "entry": "doc.yml",
    },
    {
        "name": "near_miss_key_error_and_unrelated_key_warning",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: tool\n"
                "    name: a\n"
                "    provider: json\n"
                "    prams: {mode: stringify, input: 1}\n"
                "  - type: tool\n"
                "    name: b\n"
                "    provider: json\n"
                "    params: {mode: stringify, input: 1}\n"
                "    owner: ops\n"
            ),
        },
        "entry": "doc.yml",
    },
    {
        "name": "schema_violation_loop_needs_exactly_one_of_while_or_each",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: loop\n"
                "    name: poll\n"
                f"    body: [{_TOOL_FLOW}]\n"
            ),
        },
        "entry": "doc.yml",
        "error_modes": {"validate_errors": ["location"], "run_error": "location"},
    },
    {
        "name": "group_field_on_a_container_effect",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: dynamic\n"
                "    name: d\n"
                "    group: g\n"
                "    effects:\n"
                "      - type: tool\n"
                "        name: t\n"
                "        provider: json\n"
                "        params: {mode: stringify, input: 1}\n"
            ),
        },
        "entry": "doc.yml",
    },
    {
        "name": "interface_inputs_unknown_type",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: stringg}\n"
                "effects: []\n"
            ),
        },
        "entry": "doc.yml",
    },
    {
        "name": "interface_inputs_default_type_mismatch_with_unquote_hint",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: integer, default: '3'}\n"
                "effects: []\n"
            ),
        },
        "entry": "doc.yml",
    },
    {
        "name": "negative_max_concurrency_config_error",
        "files": {
            "doc.yml": (
                "runtime: {max_concurrency: -1}\n"
                "effects:\n"
                "  - type: tool\n"
                f"{_TOOL_YML}"
            ),
        },
        "entry": "doc.yml",
        "check_report_needs": "C",
    },
    {
        "name": "invalid_concurrency_groups_config_error",
        "files": {
            "doc.yml": (
                "runtime: {concurrency_groups: {io: -1}}\n"
                "effects:\n"
                "  - type: tool\n"
                f"{_TOOL_YML}"
            ),
        },
        "entry": "doc.yml",
        "check_report_needs": "C",
    },
]


def main() -> int:
    results = []
    for case in CASES:
        check_report_needs = case.pop("check_report_needs", None)
        result = run_case(case)
        if check_report_needs is not None:
            result["check_report_needs"] = check_report_needs
        results.append(result)
    text = render_corpus(results)
    return write_or_check(OUTPUT, text, check="--check" in sys.argv[1:])


if __name__ == "__main__":
    raise SystemExit(main())
