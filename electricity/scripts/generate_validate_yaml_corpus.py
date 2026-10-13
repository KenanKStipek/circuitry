#!/usr/bin/env python3
"""Generate the golden corpus for lane L2a of issue #448 (#521):
`electricity-tools::validate_yaml`, a close port of
`plugins/validate_yaml.py::ValidateYamlPlugin`, by actually running
that real plugin against synthetic params.

Every circuitry-own error text (a near-miss key, `group:` placement, an
interface error, a duplicate key, a compile error) is recorded `exact`
and compared word for word on the Rust side. A PyYAML scan error or a
JSON-Schema violation is third-party text (DESIGN.md §1, §12): recorded
`third_party` with only its fixed prefix (`"YAML parse error: "` or the
schema error's own `"<location>: "`), never its full wording -- the
Rust side only checks the prefix matches and the rest is non-empty.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI, `-c electricity/scripts/generator-constraints.txt`).
Usage: python3 generate_validate_yaml_corpus.py [--check]
"""

from __future__ import annotations

import datetime
import json
import struct
import sys
from pathlib import Path
from typing import Any

from circuitry.plugins.validate_yaml import ValidateYamlPlugin

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-tools"
    / "tests"
    / "golden"
    / "validate_yaml_corpus.json"
)


# ---------------------------------------------------------------------
# Tagged value encoding, params only (matches generate_tool_runtime_
# corpus.py's `encode`, widened with a `bytes` tag this corpus alone
# needs -- `yaml: b"..."` is one of the non-string-type cases).
# ---------------------------------------------------------------------


def encode(value: object) -> dict:
    if value is None:
        return {"t": "none"}
    if isinstance(value, bool):
        return {"t": "bool", "v": value}
    if isinstance(value, int):
        return {"t": "int", "v": str(value)}
    if isinstance(value, float):
        bits = struct.unpack("<Q", struct.pack("<d", value))[0]
        return {"t": "float", "bits": format(bits, "016x")}
    if isinstance(value, str):
        return {"t": "str", "v": value}
    if isinstance(value, bytes):
        return {"t": "bytes", "hex": value.hex()}
    # `datetime.datetime` is a `datetime.date` subclass -- checked first,
    # or every datetime would also match the `date` branch below.
    if isinstance(value, datetime.datetime):
        offset = value.utcoffset()
        return {
            "t": "datetime",
            "v": value.replace(tzinfo=None).isoformat(),
            "offset_seconds": int(offset.total_seconds()) if offset is not None else None,
        }
    if isinstance(value, datetime.date):
        return {"t": "date", "v": value.isoformat()}
    if isinstance(value, list):
        return {"t": "list", "v": [encode(item) for item in value]}
    if isinstance(value, dict):
        return {"t": "dict", "v": [[encode(k), encode(v)] for k, v in value.items()]}
    raise TypeError(f"no tagged encoding for {type(value)!r}")


def encode_params(params: dict[str, Any]) -> dict[str, dict]:
    return {key: encode(value) for key, value in params.items()}


# ---------------------------------------------------------------------
# Error tagging: `exact` (Circuitry's own text, compared word for
# word) or `third_party` (a fixed prefix only, non-empty rest).
# ---------------------------------------------------------------------


def exact(text: str) -> dict[str, str]:
    return {"kind": "exact", "text": text}


def third_party(text: str, prefix: str) -> dict[str, str]:
    assert text.startswith(prefix), f"{text!r} does not start with {prefix!r}"
    rest = text[len(prefix) :]
    assert rest, f"{text!r} has an empty rest after prefix {prefix!r}"
    return {"kind": "third_party", "prefix": prefix}


def tag_scan_error(text: str) -> dict[str, str]:
    return third_party(text, "YAML parse error: ")


def tag_schema_error(text: str) -> dict[str, str]:
    location, _, _ = text.partition(": ")
    return third_party(text, f"{location}: ")


# ---------------------------------------------------------------------
# Cases
# ---------------------------------------------------------------------


def run(params: dict[str, Any]) -> dict[str, Any]:
    try:
        result = ValidateYamlPlugin().execute(params=params)
    except Exception as exc:
        return {"raised": True, "error": str(exc), "py_class": type(exc).__name__}
    return {
        "raised": False,
        "tool_ok": result.ok,
        "value_ok": result.value["ok"],
        "yaml_echo": result.value["yaml"],
        "stderr": result.stderr,
        "errors": result.value["errors"],
    }


def succeeding_case(
    name: str,
    params: dict[str, Any],
    *,
    expect_ok: bool,
    errors_tagged: list[dict[str, str]],
    unordered: bool = False,
) -> dict[str, Any]:
    outcome = run(params)
    assert not outcome["raised"], f"{name}: expected success, raised {outcome.get('error')!r}"
    assert outcome["value_ok"] is expect_ok, f"{name}: value['ok'] mismatch: {outcome}"
    # The generator asserts its own expected texts directly (never only via
    # the Rust replay's comparison, per DESIGN.md's own L2 standard,
    # reviewed as missing on PR #524's finding 3): an `exact` tag's own
    # *text* must equal the real plugin's own output, word for word, not
    # just a hand-typed guess that happens to look right.
    assert len(errors_tagged) == len(outcome["errors"]), (
        f"{name}: errors_tagged has {len(errors_tagged)} entries, outcome has "
        f"{len(outcome['errors'])}: {outcome['errors']!r}"
    )
    if not unordered:
        for tag, actual in zip(errors_tagged, outcome["errors"], strict=True):
            if tag["kind"] == "exact":
                assert tag["text"] == actual, (
                    f"{name}: hand-typed text {tag['text']!r} does not match the plugin's own "
                    f"output {actual!r}"
                )
    return {
        "name": name,
        "params": encode_params(params),
        "raised": False,
        "error": None,
        "tool_ok": outcome["tool_ok"],
        "value_ok": outcome["value_ok"],
        "yaml_echo": outcome["yaml_echo"],
        "stderr": outcome["stderr"],
        "errors": errors_tagged,
        "errors_unordered": unordered,
    }


def raising_case(name: str, params: dict[str, Any]) -> dict[str, Any]:
    outcome = run(params)
    assert outcome["raised"], f"{name}: expected a raise, got {outcome}"
    return {
        "name": name,
        "params": encode_params(params),
        "raised": True,
        "error": outcome["error"],
        "py_class": outcome["py_class"],
        "tool_ok": None,
        "value_ok": None,
        "yaml_echo": None,
        "stderr": None,
        "errors": None,
    }


_NEAR_MISS_NO_SCHEMA = """
effects:
  - type: tool
    name: a
    provider: json
    discription: oops
    params: {mode: parse, input: "1"}
""".strip()

_GROUP_ON_DYNAMIC = """
effects:
  - type: dynamic
    name: d
    group: mygroup
    effects:
      - type: tool
        name: a
        provider: json
        params: {mode: parse, input: "1"}
""".strip()

_UNKNOWN_INTERFACE_TYPE = """
interface:
  inputs:
    x:
      type: wat
effects:
  - type: tool
    name: a
    provider: json
    params: {mode: parse, input: "1"}
""".strip()

_QUOTED_DEFAULT = """
interface:
  inputs:
    x:
      type: number
      default: "5"
effects:
  - type: tool
    name: a
    provider: json
    params: {mode: parse, input: "1"}
""".strip()

_DUPLICATE_KEY = """
effects:
  - type: tool
    name: a
    provider: json
    params: {mode: parse, input: "1"}
effects:
  - type: tool
    name: b
    provider: json
    params: {mode: parse, input: "1"}
""".strip()

_DUPLICATE_SIBLING_NAMES = """
effects:
  - type: tool
    name: a
    provider: json
    params: {mode: parse, input: "1"}
  - type: tool
    name: a
    provider: json
    params: {mode: parse, input: "1"}
""".strip()

_BARE_INTERFACE_INPUT_REF = """
interface:
  inputs:
    x: {type: string}
effects:
  - type: yield
    name: a
    template: "{{x}}"
""".strip()

_SCAN_ERROR_DOC = "effects: [{a: [1, 2}]"

_SCHEMA_ERROR_DOC = """
effects:
  - type: tool
    provider: json
""".strip()

_SCHEMA_ERRORS_MULTI = """
effects:
  - type: tool
    provider: json
  - type: tool
    provider: json
""".strip()

_THREE_NEAR_MISS_ERRORS = """
effects:
  - type: tool
    name: a
    provider: json
    discription: x
    params: {mode: parse, input: "1"}
  - type: tool
    name: b
    provider: json
    discription: x
    params: {mode: parse, input: "1"}
  - type: tool
    name: c
    provider: json
    discription: x
    params: {mode: parse, input: "1"}
""".strip()


def build_cases() -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []

    # --- every branch of `_validate_document`, in order --------------
    cases.append(succeeding_case("valid_document", {"yaml": "effects: []"}, expect_ok=True, errors_tagged=[]))
    cases.append(
        succeeding_case(
            "valid_document_fenced",
            {"yaml": "```yaml\neffects: []\n```"},
            expect_ok=True,
            errors_tagged=[],
        )
    )
    cases.append(
        succeeding_case(
            "strip_fences_false_on_fence_free_document",
            {"yaml": "effects: []", "strip_fences": "no"},
            expect_ok=True,
            errors_tagged=[],
        )
    )
    outcome = run({"yaml": "```yaml\neffects: []\n```", "strip_fences": "false"})
    assert not outcome["raised"] and outcome["value_ok"] is False
    cases.append(
        {
            "name": "strip_fences_false_leaves_fences_for_the_parser",
            "params": encode_params({"yaml": "```yaml\neffects: []\n```", "strip_fences": "false"}),
            "raised": False,
            "error": None,
            "tool_ok": outcome["tool_ok"],
            "value_ok": outcome["value_ok"],
            "yaml_echo": outcome["yaml_echo"],
            "stderr": outcome["stderr"],
            "errors": [tag_scan_error(outcome["errors"][0])],
        }
    )
    cases.append(
        succeeding_case(
            "blank_document_python_whitespace",
            {"yaml": "  \x1c "},
            expect_ok=False,
            errors_tagged=[exact("Empty document \u2014 expected a Circuitry orchestration YAML.")],
        )
    )
    for label, doc, type_name in [
        ("not_a_mapping_date", "2024-01-01", "date"),
        ("not_a_mapping_comment_only", "# only a comment", "NoneType"),
        ("not_a_mapping_list", "- a", "list"),
    ]:
        cases.append(
            succeeding_case(
                label,
                {"yaml": doc},
                expect_ok=False,
                errors_tagged=[
                    exact(
                        "Orchestration must be a YAML mapping with an 'effects' key, "
                        f"got {type_name}."
                    )
                ],
            )
        )
    cases.append(
        succeeding_case(
            "missing_effects_key",
            {"yaml": "name: x"},
            expect_ok=False,
            errors_tagged=[exact("Orchestration is missing the required top-level 'effects' key.")],
        )
    )
    cases.append(
        succeeding_case(
            "near_miss_key_no_schema_error",
            {"yaml": _NEAR_MISS_NO_SCHEMA},
            expect_ok=False,
            errors_tagged=[
                exact(
                    "effects[0]: unknown key 'discription' on a 'tool' effect "
                    "\u2014 did you mean 'description'? As written it is ignored."
                )
            ],
        )
    )
    cases.append(
        succeeding_case(
            "group_on_non_empty_dynamic",
            {"yaml": _GROUP_ON_DYNAMIC},
            expect_ok=False,
            errors_tagged=[
                exact(
                    "effects[0]: 'group' is only allowed on tool/prompt effects "
                    "(it names a concurrency-group slot a leaf effect holds at "
                    "dispatch) \u2014 found on a 'dynamic' effect."
                )
            ],
        )
    )
    cases.append(
        succeeding_case(
            "unknown_interface_type",
            {"yaml": _UNKNOWN_INTERFACE_TYPE},
            expect_ok=False,
            errors_tagged=[
                exact(
                    "interface.inputs.x.type: 'wat' is not a recognized type \u2014 "
                    "expected one of string, number, integer, boolean, array, object."
                )
            ],
        )
    )
    cases.append(
        succeeding_case(
            "quoted_interface_default_unquote_hint",
            {"yaml": _QUOTED_DEFAULT},
            expect_ok=False,
            errors_tagged=[
                exact(
                    "interface.inputs.x.default: declared type 'number' but '5' is "
                    "str \u2014 quoting it makes it a string; remove the quotes to "
                    "declare it as number."
                )
            ],
        )
    )
    outcome = run({"yaml": _DUPLICATE_KEY})
    assert not outcome["raised"] and outcome["value_ok"] is False
    cases.append(
        {
            "name": "duplicate_key",
            "params": encode_params({"yaml": _DUPLICATE_KEY}),
            "raised": False,
            "error": None,
            "tool_ok": outcome["tool_ok"],
            "value_ok": outcome["value_ok"],
            "yaml_echo": outcome["yaml_echo"],
            "stderr": outcome["stderr"],
            "errors": [exact(outcome["errors"][0])],
        }
    )
    cases.append(
        succeeding_case(
            "duplicate_sibling_names_compile_error",
            {"yaml": _DUPLICATE_SIBLING_NAMES},
            expect_ok=False,
            errors_tagged=[
                exact(
                    "Compile error: Duplicate effect name 'a' in scope 'prime'. "
                    "Seen at 'prime.effects[0]' and 'prime.effects[1]'."
                )
            ],
        )
    )
    cases.append(
        succeeding_case(
            "duplicate_sibling_names_compile_off",
            {"yaml": _DUPLICATE_SIBLING_NAMES, "compile": "off"},
            expect_ok=True,
            errors_tagged=[],
        )
    )
    cases.append(
        succeeding_case(
            "bare_interface_input_reference_compile_error",
            {"yaml": _BARE_INTERFACE_INPUT_REF},
            expect_ok=False,
            errors_tagged=[
                exact(
                    "Compile error: Template at 'effects[0].template': bare "
                    "'{{x}}' refers to declared interface input 'x'. Caller "
                    "inputs live under the 'input' namespace; write '{{input.x}}'."
                )
            ],
        )
    )

    # --- third-party texts (location only) ---------------------------
    outcome = run({"yaml": _SCAN_ERROR_DOC})
    assert not outcome["raised"] and outcome["value_ok"] is False
    cases.append(
        {
            "name": "pyyaml_scan_error",
            "params": encode_params({"yaml": _SCAN_ERROR_DOC}),
            "raised": False,
            "error": None,
            "tool_ok": outcome["tool_ok"],
            "value_ok": outcome["value_ok"],
            "yaml_echo": outcome["yaml_echo"],
            "stderr": outcome["stderr"],
            "errors": [tag_scan_error(outcome["errors"][0])],
        }
    )
    outcome = run({"yaml": _SCHEMA_ERROR_DOC})
    assert not outcome["raised"] and outcome["value_ok"] is False
    cases.append(
        {
            "name": "jsonschema_error",
            "params": encode_params({"yaml": _SCHEMA_ERROR_DOC}),
            "raised": False,
            "error": None,
            "tool_ok": outcome["tool_ok"],
            "value_ok": outcome["value_ok"],
            "yaml_echo": outcome["yaml_echo"],
            "stderr": outcome["stderr"],
            "errors": [tag_schema_error(outcome["errors"][0])],
        }
    )
    # Several schema errors: compared as a multiset of locations, since
    # Python sorts by `str(err)` and electricity sorts by `(location,
    # message)` -- the kept order (and, under a low `max_errors`, the kept
    # *subset*) can differ even though the error set itself agrees
    # (DESIGN.md's own Hazards section; review finding 3 on PR #524).
    outcome = run({"yaml": _SCHEMA_ERRORS_MULTI})
    assert not outcome["raised"] and len(outcome["errors"]) == 2
    cases.append(
        {
            "name": "multiple_schema_errors_as_a_multiset",
            "params": encode_params({"yaml": _SCHEMA_ERRORS_MULTI}),
            "raised": False,
            "error": None,
            "tool_ok": outcome["tool_ok"],
            "value_ok": outcome["value_ok"],
            "yaml_echo": outcome["yaml_echo"],
            "stderr": None,
            "errors": [tag_schema_error(e) for e in outcome["errors"]],
            "errors_unordered": True,
        }
    )

    # --- the cap boundary: exactly max_errors, and one more -----------
    outcome = run({"yaml": _THREE_NEAR_MISS_ERRORS, "max_errors": 3})
    assert not outcome["raised"] and len(outcome["errors"]) == 3
    cases.append(
        {
            "name": "cap_boundary_exactly_max_errors",
            "params": encode_params({"yaml": _THREE_NEAR_MISS_ERRORS, "max_errors": 3}),
            "raised": False,
            "error": None,
            "tool_ok": outcome["tool_ok"],
            "value_ok": outcome["value_ok"],
            "yaml_echo": outcome["yaml_echo"],
            "stderr": outcome["stderr"],
            "errors": [exact(e) for e in outcome["errors"]],
        }
    )
    outcome = run({"yaml": _THREE_NEAR_MISS_ERRORS, "max_errors": 2})
    assert not outcome["raised"] and len(outcome["errors"]) == 3
    assert outcome["errors"][-1] == "... and 1 more error(s)."
    cases.append(
        {
            "name": "cap_boundary_one_more_than_max_errors",
            "params": encode_params({"yaml": _THREE_NEAR_MISS_ERRORS, "max_errors": 2}),
            "raised": False,
            "error": None,
            "tool_ok": outcome["tool_ok"],
            "value_ok": outcome["value_ok"],
            "yaml_echo": outcome["yaml_echo"],
            "stderr": outcome["stderr"],
            "errors": [exact(e) for e in outcome["errors"]],
        }
    )

    # --- `_as_bool` string forms (strip_fences and compile) -----------
    for form, expected_bool in [
        ("true", True),
        ("True", True),
        ("false", False),
        ("False", False),
        ("0", False),
        ("1", True),
        ("no", False),
        ("No", False),
        ("off", False),
        ("Off", False),
        ("n", False),
        ("N", False),
        ("", False),
        ("yes", True),
        ("anything-else", True),
        ("  false  ", False),
        ("  FALSE  ", False),
    ]:
        doc = "effects: []" if expected_bool else "```yaml\neffects: []\n```"
        outcome = run({"yaml": doc, "strip_fences": form})
        assert not outcome["raised"], (form, outcome)
        expect_ok = bool(expected_bool)
        errors_tagged = [] if expected_bool else [tag_scan_error(outcome["errors"][0])]
        assert outcome["value_ok"] is expect_ok, (form, outcome)
        cases.append(
            {
                "name": f"as_bool_strip_fences_{form!r}",
                "params": encode_params({"yaml": doc, "strip_fences": form}),
                "raised": False,
                "error": None,
                "tool_ok": outcome["tool_ok"],
                "value_ok": outcome["value_ok"],
                "yaml_echo": outcome["yaml_echo"],
                "stderr": outcome["stderr"],
                "errors": errors_tagged,
            }
        )

    # --- `_as_bool` string forms for `compile` too (review finding 3 on
    # PR #524: only `strip_fences` was covered) -- `_DUPLICATE_SIBLING_NAMES`
    # only fails under compile, so a truthy/falsy form flips `ok` the same
    # way `strip_fences`'s own loop does for a fence-free/fenced document.
    for form, expected_bool in [
        ("true", True),
        ("false", False),
        ("0", False),
        ("1", True),
        ("no", False),
        ("off", False),
        ("yes", True),
        ("anything-else", True),
        ("  false  ", False),
    ]:
        outcome = run({"yaml": _DUPLICATE_SIBLING_NAMES, "compile": form})
        assert not outcome["raised"], (form, outcome)
        expect_ok = not bool(expected_bool)
        assert outcome["value_ok"] is expect_ok, (form, outcome)
        errors_tagged = (
            []
            if expect_ok
            else [
                exact(
                    "Compile error: Duplicate effect name 'a' in scope 'prime'. "
                    "Seen at 'prime.effects[0]' and 'prime.effects[1]'."
                )
            ]
        )
        cases.append(
            {
                "name": f"as_bool_compile_{form!r}",
                "params": encode_params({"yaml": _DUPLICATE_SIBLING_NAMES, "compile": form}),
                "raised": False,
                "error": None,
                "tool_ok": outcome["tool_ok"],
                "value_ok": outcome["value_ok"],
                "yaml_echo": outcome["yaml_echo"],
                "stderr": outcome["stderr"],
                "errors": errors_tagged,
            }
        )

    # --- `max_errors` non-integer/wrong-type/below-one forms -----------
    for label, value in [
        ("string_x", "x"),
        ("list_one", [1]),
    ]:
        cases.append(raising_case(f"max_errors_{label}_raises", {"yaml": "effects: []", "max_errors": value}))
    for label, value in [("zero", 0), ("negative", -1)]:
        cases.append(raising_case(f"max_errors_{label}_raises", {"yaml": "effects: []", "max_errors": value}))

    # --- `max_errors` accepted forms ------------------------------------
    for label, value in [
        ("string_3", "3"),
        ("string_padded_3", " 3 "),
        ("float_truncates", 2.9),
        ("bool_true", True),
    ]:
        cases.append(
            succeeding_case(
                f"max_errors_{label}",
                {"yaml": "effects: []", "max_errors": value},
                expect_ok=True,
                errors_tagged=[],
            )
        )

    # --- non-string `yaml` values of every type raise -------------------
    for label, value in [
        ("int", 5),
        ("float", 5.5),
        ("bool", True),
        ("list", ["a"]),
        ("dict", {"a": 1}),
        ("bytes", b"effects: []"),
        # Settled 6 singles out `date`/`datetime` for `type(x).__name__`
        # (never `datetime.date`/`datetime.datetime`) -- review finding 3
        # on PR #524: these two were missing from this loop.
        ("date", datetime.date(2024, 1, 1)),
        # Deliberately naive (no `tzinfo`) -- `yaml.dump`'s own implicit
        # timestamp resolver produces a naive `datetime.datetime` just the
        # same for a scalar with no explicit offset (DESIGN.md §3.2), the
        # one shape this corpus needs to exercise `type(x).__name__`'s own
        # "datetime" branch.
        ("datetime", datetime.datetime(2024, 1, 1, 12, 30, 0, tzinfo=None)),  # noqa: DTZ001
    ]:
        cases.append(raising_case(f"yaml_type_{label}_raises", {"yaml": value}))
    cases.append(
        raising_case(
            "max_errors_infinity_raises_overflow_error",
            {"yaml": "effects: []", "max_errors": float("inf")},
        )
    )

    # --- no `yaml` param at all: empty document, not raised -------------
    cases.append(
        succeeding_case(
            "no_yaml_param_is_empty_document",
            {},
            expect_ok=False,
            errors_tagged=[exact("Empty document \u2014 expected a Circuitry orchestration YAML.")],
        )
    )
    cases.append(
        succeeding_case(
            "null_yaml_param_is_empty_document",
            {"yaml": None},
            expect_ok=False,
            errors_tagged=[exact("Empty document \u2014 expected a Circuitry orchestration YAML.")],
        )
    )

    return cases


def render(cases: list[dict[str, Any]]) -> str:
    return json.dumps({"cases": cases}, indent=2, ensure_ascii=False, sort_keys=True) + "\n"


def write_or_check(path: Path, text: str, *, check: bool) -> bool:
    path.parent.mkdir(parents=True, exist_ok=True)
    if check:
        current = path.read_text() if path.exists() else ""
        if current != text:
            print(f"{path} is stale; run without --check to regenerate", file=sys.stderr)
            return False
        return True
    path.write_text(text)
    print(f"wrote {path}")
    return True


def main() -> int:
    check = "--check" in sys.argv[1:]
    text = render(build_cases())
    return 0 if write_or_check(OUTPUT, text, check=check) else 1


if __name__ == "__main__":
    raise SystemExit(main())
