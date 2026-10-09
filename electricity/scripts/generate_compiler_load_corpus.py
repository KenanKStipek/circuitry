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
        # #408's acceptance audit: a root key YAML reads as a non-string
        # (an unquoted `1:`) -- `key_label`'s own "(YAML read the
        # unquoted key as a <type>)" suffix is otherwise pinned only by
        # `structural.rs`'s unit tests, never by this golden corpus.
        "name": "non_string_root_key_warns_with_its_yaml_read_type",
        "files": {
            "doc.yml": "1: unexpected\neffects: []\n",
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
        # Issue #429: the CLI's `-e` inputs reach `check_for_run` --
        # these cases pass `CheckOptions.inputs`/`RunRequest.initial_
        # state["input"]` the same text a `-e` flag would carry.
        # `string` is the one declared type a CLI-shaped text value can
        # never fail against (`isinstance(str)` always matches once
        # `_coerce`/the int-float-bool-to-string branch run) -- these
        # four cases are JSON-sniffable shapes `_parse_env_vars` would
        # turn into a non-string value first; `5`/`true` recover back
        # to the same text either way (the int/float/bool-to-string
        # branch in `check_interface_inputs` already handles that, with
        # or without `_restore_raw_text_for_string_inputs`), so they
        # exercise the sniff-then-recover path without the restore
        # itself mattering; `1e3` recovers to a *different* text
        # (`"1000.0"`) either way, unobservable here since this corpus
        # only compares `run_error`, not the resulting state.
        "name": "interface_input_e_string_value_int_shaped_text_is_recovered",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: string, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "5"},
    },
    {
        "name": "interface_input_e_string_value_boolean_shaped_text_is_recovered",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: string, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "true"},
    },
    {
        "name": "interface_input_e_string_value_exponent_shaped_text_is_recovered",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: string, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "1e3"},
    },
    {
        # Without `_restore_raw_text_for_string_inputs`, `-e x=null`
        # JSON-sniffs to `None` before `check_interface_inputs` ever
        # sees it, which counts as absent -- same as not passing `-e x`
        # at all -- and a required, undefaulted input would report
        # missing. The restore puts the literal text `"null"` back for
        # a declared `string` input, so it stays present instead: this
        # case's pass/fail verdict genuinely depends on the restore.
        "name": "interface_input_e_string_value_null_text_stays_present_via_restore",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: string, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "null"},
    },
    {
        # Without the restore, `-e x=[1]` JSON-sniffs to the list `[1]`;
        # a declared `string` input rejects a list outright (the
        # int/float/bool-to-string branch doesn't cover it, and a list
        # is never a `str`). The restore puts the literal text `"[1]"`
        # back, which satisfies `type: string` directly -- another case
        # whose verdict genuinely depends on the restore.
        "name": "interface_input_e_string_value_array_shaped_text_passes_only_via_restore",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: string, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "[1]"},
    },
    {
        # A JSON `null` given to a *non-string* required input counts
        # as absent, same as the key never being passed at all -- the
        # restore above only ever applies to a declared `type: string`.
        "name": "interface_input_e_required_non_string_input_given_null_is_still_missing",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: integer, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "null"},
    },
    {
        # `-e x=1` JSON-sniffs to the int `1` before `check_interface_
        # inputs` ever sees it -- `1` is one of `_TRUE_WORDS`, but that
        # word list is only consulted for text `_coerce` actually
        # receives, and an int never reaches `_coerce` at all.
        "name": "interface_input_e_boolean_word_that_json_sniffs_to_an_int",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: boolean, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "1"},
    },
    {
        "name": "interface_input_e_number_word_that_json_sniffs_to_a_bool",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: number, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "true"},
    },
    {
        "name": "interface_input_e_array_value_that_json_sniffs_to_an_object",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: array, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": '{"a": 1}'},
    },
    {
        # `core/state_ns.py::migrate_legacy_state` never lifts a
        # `_`-prefixed root key under `input` -- a document that
        # declares `_token` as a required input can't be satisfied by
        # `-e _token=...` at all.
        "name": "interface_input_e_underscore_prefixed_key_is_never_lifted",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    _token: {type: string, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"_token": "x"},
    },
    {
        # `-e input={"name": "W"}` JSON-sniffs to a dict under the
        # literal key `input` -- `migrate_legacy_state` treats that key
        # as already namespaced and uses its value directly.
        "name": "interface_input_e_input_namespace_key_satisfies_required_input",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    name: {type: string, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"input": '{"name": "W"}'},
    },
    {
        # `-e input=5` JSON-sniffs to a non-dict value; `migrate_legacy_
        # state` trusts the `input` key as already namespaced and
        # leaves it alone, so `cli/runtime_shim.py::run`'s own
        # `if not isinstance(input_ns, dict): input_ns = {}` resets it
        # to an empty namespace right before `check_interface_inputs`.
        "name": "interface_input_e_non_dict_input_key_value_becomes_an_empty_namespace",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    name: {type: string, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"input": "5"},
    },
    {
        # Once an `input` key is present among the `-e` entries, no
        # other key is lifted under it at all -- `name` here stays a
        # bare, unused root key, not `state["input"]["name"]`.
        "name": "interface_input_e_input_key_present_other_e_keys_are_not_lifted",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    name: {type: string, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"input": "{}", "name": "W"},
    },
    {
        # `prime` is a namespace name, so `migrate_legacy_state` never
        # lifts it under `input` either -- same as an underscore-prefixed
        # key.
        "name": "interface_input_e_declared_input_named_prime_cannot_be_satisfied",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    prime: {type: integer, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"prime": "5"},
    },
    {
        "name": "interface_input_e_number_valid",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: number, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "5.5"},
    },
    {
        "name": "interface_input_e_number_invalid",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: number, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "notanumber"},
    },
    {
        # `int()`'s own underscore digit separator (PEP 515): `1_000`
        # isn't valid JSON, so it stays raw text through the CLI's own
        # JSON-sniffing pre-pass and reaches `_coerce` as text either way.
        "name": "interface_input_e_integer_with_underscores",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: integer, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "1_000"},
    },
    {
        "name": "interface_input_e_integer_invalid",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: integer, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "abc"},
    },
    {
        # `_TRUE_WORDS`/`_FALSE_WORDS`: `cli/app.py`'s own lenient
        # boolean words, not just `true`/`false`.
        "name": "interface_input_e_boolean_word_valid",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: boolean, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "yes"},
    },
    {
        "name": "interface_input_e_boolean_invalid",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: boolean, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "nope"},
    },
    {
        "name": "interface_input_e_array_valid",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: array, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "[1,2,3]"},
    },
    {
        # The failure text embeds plain `json.loads`'s own third-party
        # message (`_coerce`'s `array`/`object` arm) past Circuitry's own
        # "could not be converted: " -- `error_modes` below only pins
        # the Circuitry-owned prefix (`location_prefix`'s own location
        # is text up to the first ": ", which lands exactly there).
        "name": "interface_input_e_array_invalid",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: array, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "[1,2,"},
        "error_modes": {"run_error": "location_prefix"},
    },
    {
        "name": "interface_input_e_object_valid",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: object, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": '{"a": 1}'},
    },
    {
        "name": "interface_input_e_object_invalid",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    x: {type: object, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"x": "notjson"},
        "error_modes": {"run_error": "location_prefix"},
    },
    {
        "name": "required_interface_input_supplied_via_e_run_succeeds",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    name: {type: string, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"name": "World"},
    },
    {
        # A document's own declared `default:` is checked structurally
        # (its own, unconditional, static type check -- `interface_inputs_
        # default_type_mismatch_with_unquote_hint`'s own case) regardless
        # of whether `-e` ever overrides it, so a *bad* default can never
        # prove an override took effect: it fails `check_for_run` either
        # way. A *valid* default (`3`, structurally fine) with an
        # *invalid* `-e` override instead proves the override replaced
        # it, not merely coexisted alongside it -- a silently-kept
        # default would pass, where this fails on the override's own
        # text.
        "name": "default_overridden_by_provided_e_value",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    count: {type: integer, default: 3}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"count": "abc"},
    },
    {
        # An input `interface.inputs` never declares stays allowed
        # (issue #429: "Undeclared extra inputs stay allowed").
        "name": "undeclared_extra_e_input_is_allowed",
        "files": {
            "doc.yml": (
                "interface:\n"
                "  inputs:\n"
                "    name: {type: string, required: true}\n"
                "effects: []\n"
            )
        },
        "entry": "doc.yml",
        "inputs": {"name": "World", "unrelated": "1"},
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
        # #408's acceptance audit: `parse_concurrency_groups`'s own
        # non-string/empty-group-name branch (`pipeline.rs`'s
        # "runtime.concurrency_groups has a non-string or empty group
        # name: ..." message) was ported with no corpus case pinning
        # it -- YAML reads an unquoted `1:` as the int `1`, not a string.
        "name": "concurrency_groups_non_string_key_config_error",
        "files": {
            "doc.yml": "runtime: {concurrency_groups: {1: 3}}\neffects: []\n",
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
