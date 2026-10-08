#!/usr/bin/env python3
"""Lane B's golden corpus: `load_document`, `structural_errors`/
`unknown_key_warnings`, and the concurrency-configuration errors
(issue #408's lane B section).

Every case here fails before `compile_orchestration` -- a load error, a
structural error, or (for `check_for_run` specifically) a concurrency-
configuration error -- so every case is fully checkable against this
lane alone. `validate()`'s own concurrency-configuration check runs
*after* a document compiles (confirmed directly against
`cli/runtime_shim.py`), so the two `*_concurrency_config_error` cases'
`check_report` side is recorded against a real compile (lane C has
landed) even though their `check_for_run` side -- which checks the
same configuration *before* structural checks even start -- never
needed one.

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
        # is a required property" check -- a schema error, so the
        # location ("top level") is Circuitry's own and compared
        # exactly; the message past it is third-party `jsonschema` text.
        "error_modes": {"validate_errors": ["exact"], "run_error": "location_prefix"},
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
        # a schema error on both surfaces here (`check_report` has no
        # "Orchestration file is empty." short circuit to beat it to,
        # unlike `empty_yaml_file`'s own case): the location ("top
        # level") is Circuitry's own and compared exactly, the message
        # past it is third-party `jsonschema` text.
        "error_modes": {"validate_errors": ["location_prefix"], "run_error": "location_prefix"},
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
        # Another schema error (`"value is not valid under any of the
        # given schemas"`): the location ("effects[0]") is Circuitry's
        # own and compared exactly, the message past it is third-party.
        "error_modes": {"validate_errors": ["location_prefix"], "run_error": "location_prefix"},
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
        "name": "runtime_not_an_object_is_a_run_only_error",
        # `resolve_effective_settings`'s own shape check -- the schema
        # doesn't type `runtime`, so `validate()` (and so `check_report`)
        # passes outright; only `run()` (`check_for_run`) reaches this,
        # before structural checks even start (F1).
        "files": {"doc.yml": "runtime: 5\neffects: []\n"},
        "entry": "doc.yml",
    },
    {
        "name": "plugins_not_a_list_is_a_run_only_error",
        "files": {"doc.yml": "plugins: foo\neffects: []\n"},
        "entry": "doc.yml",
    },
    {
        "name": "plugins_entry_not_a_string_is_a_run_only_error",
        "files": {"doc.yml": "plugins: [1]\neffects: []\n"},
        "entry": "doc.yml",
    },
    {
        "name": "missing_required_interface_input_is_a_run_only_error",
        # `check_interface_inputs` against the top-level `interface.
        # inputs` -- also only reached by `run()`, after the
        # concurrency limiter and before structural checks (F1). The
        # schema itself never requires an input actually be supplied.
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: string, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
    },
    {
        "name": "missing_entry_file_with_an_unsupported_suffix",
        # The suffix is checked before any read (F2): `check_for_run`
        # reports the unsupported-format message without ever touching
        # the (nonexistent) file; `validate()` reads the raw file text
        # itself first and raises `FileNotFoundError` uncaught --
        # recorded by `_compiler_corpus.py`'s own `except Exception`
        # around that call, not Circuitry's usual `{ok, errors,
        # warnings}` shape, so the `validate_errors` side is `location`
        # (the OS's own message text, not Circuitry's).
        "files": {},
        "entry": "missing.txt",
        "error_modes": {"validate_errors": ["location"], "run_error": "exact"},
    },
    {
        "name": "non_utf8_file_with_an_unsupported_suffix",
        # Same shape as the case above, through the other path `load_
        # document`'s suffix-first check guards against: a `.png`
        # (unsupported either way) that also isn't valid UTF-8 --
        # `check_for_run` never reads it at all, where the pre-fix code
        # reported a UTF-8 decode error instead of the suffix one.
        "files": {"doc.png": {"bytes_hex": "89504e47ff"}},
        "entry": "doc.png",
        "error_modes": {"validate_errors": ["location"], "run_error": "exact"},
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
    },
    {
        # PR #417's second review: a schema `maximum`/`minimum` check
        # against an arbitrary-precision integer, well past `i64`/`u64`
        # range (the `jsonschema` crate panic this lane's F3 fix
        # clamps against) -- a 401-digit positive integer violates
        # `if.threshold`'s own `maximum: 1`.
        "name": "if_threshold_huge_int_is_a_maximum_error",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: if\n"
                "    name: c\n"
                "    if: {mode: cel, expr: 'true'}\n"
                f"    then: [{_TOOL_FLOW}]\n"
                f"    threshold: {'1' + '0' * 400}\n"
            ),
        },
        "entry": "doc.yml",
        "error_modes": {"validate_errors": ["location_prefix"], "run_error": "location_prefix"},
    },
    {
        # Same arbitrary-precision boundary, the `minimum` side: a
        # 401-digit negative integer violates a tree loop's own
        # `max_concurrency`'s `minimum: 1`.
        "name": "tree_loop_max_concurrency_huge_negative_int_is_a_minimum_error",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: loop\n"
                "    name: poll\n"
                "    flow: tree\n"
                "    each: {in: 'input.xs'}\n"
                f"    max_concurrency: -{'1' + '0' * 400}\n"
                f"    body: [{_TOOL_FLOW}]\n"
            ),
        },
        "entry": "doc.yml",
        "error_modes": {"validate_errors": ["location_prefix"], "run_error": "location_prefix"},
    },
    {
        # Same boundary on the valid side: a 401-digit positive integer
        # satisfies `prompt.timeout_ms`'s own `minimum: 0` (no `maximum`
        # of its own), so the document is valid end to end.
        "name": "prompt_timeout_ms_huge_int_is_valid",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: prompt\n"
                "    name: p\n"
                "    template: 'hi'\n"
                f"    timeout_ms: {'1' + '0' * 400}\n"
            ),
        },
        "entry": "doc.yml",
    },
]


def main() -> int:
    results = [run_case(case) for case in CASES]
    text = render_corpus(results)
    return write_or_check(OUTPUT, text, check="--check" in sys.argv[1:])


if __name__ == "__main__":
    raise SystemExit(main())
