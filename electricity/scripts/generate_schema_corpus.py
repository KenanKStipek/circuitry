#!/usr/bin/env python3
"""Generate electricity-schema's golden corpus from Circuitry's real validation.

For every synthetic document below, records whether Circuitry's own
``jsonschema.Draft7Validator`` against the real
``schema/orchestration.schema.json`` or ``schema/profile.schema.json``
accepts it and, when it does not, every violation:

* orchestration documents record ``core.document_check.schema_errors``'s
  own output verbatim (``described_errors``) -- text *and* order, both of
  which electricity's engine path now reproduces exactly (issue #528), not
  just the location (DESIGN.md §4 step 3, issue #380);
* profile documents still record only each violation's own *location*
  (``cli.profiles``'s own formula, ``"/".join(str(p) for p in err.path) or
  "<root>"`` from ``_validate_profile_schema``) -- that caller isn't one of
  #528's own four, and the Rust side is still never required to match its
  message text word for word (DESIGN.md §1, §12).

Documents are synthetic, hand-written against the schema/design, not drawn
from any real orchestration. Must be run with the Circuitry package
installed in the active interpreter (the lane venv locally; `pip install -e
.` in CI) so the schema and the ``jsonschema`` version are Circuitry's own.
Usage: python3 generate_schema_corpus.py [--check]
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

import jsonschema  # type: ignore[import-untyped]

from circuitry.cli import profiles  # type: ignore[import-untyped]
from circuitry.core import document_check  # type: ignore[import-untyped]

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-schema"
    / "tests"
    / "golden"
    / "corpus.json"
)


def _profile_locations(document: Any) -> list[str]:
    validator = jsonschema.Draft7Validator(profiles._load_profile_schema())
    return [
        "/".join(str(p) for p in err.path) or "<root>" for err in validator.iter_errors(document)
    ]


def _orchestration_case(name: str, document: Any) -> dict:
    described = document_check.schema_errors(document)
    return {
        "name": name,
        "schema": "orchestration",
        "document": document,
        "valid": not described,
        "described_errors": described,
    }


def _profile_case(name: str, document: Any) -> dict:
    locations = _profile_locations(document)
    return {
        "name": name,
        "schema": "profile",
        "document": document,
        "valid": not locations,
        "locations": sorted(locations),
    }


# ---------------------------------------------------------------------
# Orchestration documents
# ---------------------------------------------------------------------


def orchestration_cases() -> list[dict]:
    valid: list[tuple[str, Any]] = [
        ("minimal", {"effects": []}),
        ("tool_effect", {"effects": [{"type": "tool", "name": "fetch", "provider": "ffmpeg"}]}),
        (
            "prompt_text_effect",
            {"effects": [{"type": "prompt", "name": "ask", "template": "Ask a question"}]},
        ),
        (
            "prompt_json_effect_with_schema",
            {
                "effects": [
                    {
                        "type": "prompt",
                        "name": "ask",
                        "prompt_type": "json",
                        "template": "Ask a question",
                        "schema": {"type": "object"},
                    }
                ]
            },
        ),
        (
            "dynamic_tree_with_child",
            {
                "effects": [
                    {
                        "type": "dynamic",
                        "name": "d",
                        "flow": "tree",
                        "effects": [{"type": "tool", "name": "t", "provider": "ffmpeg"}],
                    }
                ]
            },
        ),
        (
            "conditional_if_then_else",
            {
                "effects": [
                    {
                        "type": "if",
                        "if": {"mode": "cel", "expr": "true"},
                        "then": [{"type": "tool", "name": "t", "provider": "ffmpeg"}],
                        "else": [],
                    }
                ]
            },
        ),
        (
            "loop_each",
            {
                "effects": [
                    {
                        "type": "loop",
                        "name": "l",
                        "each": {"in": "prime.x.value"},
                        "body": [{"type": "tool", "name": "t", "provider": "ffmpeg"}],
                    }
                ]
            },
        ),
        (
            "loop_while",
            {
                "effects": [
                    {
                        "type": "loop",
                        "while": {"mode": "cel", "expr": "true"},
                        "body": [{"type": "tool", "name": "t", "provider": "ffmpeg"}],
                    }
                ]
            },
        ),
        (
            "reflector_effect",
            {
                "effects": [
                    {
                        "type": "reflector",
                        "name": "r",
                        "effects": [{"type": "tool", "name": "t", "provider": "ffmpeg"}],
                    }
                ]
            },
        ),
        ("use_with_path", {"effects": [{"type": "use", "name": "u", "path": "./child.yml"}]}),
        ("use_with_ref", {"effects": [{"type": "use", "name": "u", "ref": "lib/thing"}]}),
        (
            "use_with_inline",
            {"effects": [{"type": "use", "name": "u", "inline": "effects: []"}]},
        ),
        (
            "tool_expect_bare_string",
            {
                "effects": [
                    {
                        "type": "tool",
                        "name": "t",
                        "provider": "ffmpeg",
                        "expect": "value != null",
                    }
                ]
            },
        ),
        (
            "tool_expect_model_object",
            {
                "effects": [
                    {
                        "type": "tool",
                        "name": "t",
                        "provider": "ffmpeg",
                        "expect": {"mode": "model", "template": "Did it work?"},
                    }
                ]
            },
        ),
        (
            "interface_inputs_and_outputs",
            {
                "interface": {
                    "inputs": {"topic": {"type": "string", "required": True}},
                    "outputs": {"summary": "prime.summarize.value"},
                },
                "effects": [{"type": "tool", "name": "t", "provider": "ffmpeg"}],
            },
        ),
        (
            "full_document_with_finally",
            {
                "adapter": "ollama",
                "model": "llama3",
                "version": "1.0.0",
                "flow": "chain",
                "effects": [{"type": "tool", "name": "t", "provider": "ffmpeg"}],
                "finally": [{"type": "tool", "name": "cleanup", "provider": "ffmpeg"}],
            },
        ),
        (
            # fancy-regex's "$" doesn't match just before a trailing "\n" the way
            # Python re's does; electricity-schema normalizes NamePattern's patterns
            # so this stays valid on both sides (a YAML "|" block scalar routinely
            # ends in exactly this).
            "name_with_trailing_newline_still_matches_pattern",
            {"effects": [{"type": "tool", "name": "fetch\n", "provider": "ffmpeg"}]},
        ),
        (
            "flow_enum_valid",
            {"flow": "tree_of_thought", "effects": []},
        ),
        (
            "retries_valid",
            {
                "effects": [
                    {
                        "type": "tool",
                        "name": "t",
                        "provider": "ffmpeg",
                        "retries": {"max_attempts": 3, "backoff_ms": 100},
                    }
                ]
            },
        ),
    ]

    invalid: list[tuple[str, Any]] = [
        ("missing_effects", {}),
        ("effects_wrong_type", {"effects": "nope"}),
        ("effect_missing_type", {"effects": [{}]}),
        (
            "tool_missing_provider",
            {"effects": [{"type": "tool", "name": "t"}]},
        ),
        (
            "prompt_json_missing_schema",
            {
                "effects": [
                    {
                        "type": "prompt",
                        "name": "p",
                        "prompt_type": "json",
                        "template": "Ask a question",
                    }
                ]
            },
        ),
        ("use_missing_ref_path_inline", {"effects": [{"type": "use", "name": "u"}]}),
        (
            "use_both_ref_and_path",
            {"effects": [{"type": "use", "name": "u", "ref": "a", "path": "b"}]},
        ),
        (
            "loop_missing_each_or_while",
            {
                "effects": [
                    {
                        "type": "loop",
                        "body": [{"type": "tool", "name": "t", "provider": "ffmpeg"}],
                    }
                ]
            },
        ),
        (
            "two_errors_different_locations",
            {"version": True, "effects": [{"type": "tool", "name": "t"}]},
        ),
        (
            "interface_inputs_weird_key_wrong_type",
            {
                "interface": {"inputs": {"weird key": {"type": 123}}},
                "effects": [],
            },
        ),
        (
            "interface_inputs_key_needing_escape",
            {
                "interface": {"inputs": {"it's": {"type": 123}}},
                "effects": [],
            },
        ),
        (
            # interface.inputs/outputs and use.outputs keys are free-form
            # (additionalProperties), so a digit-string key is a real,
            # reachable case -- it must be bracket-quoted ($.interface.inputs['1'])
            # rather than mistaken for an array index ($.interface.inputs[1]).
            "interface_inputs_digit_string_key",
            {
                "interface": {"inputs": {"1": {"type": 5}}},
                "effects": [],
            },
        ),
        (
            # Python jsonschema's own json_path-compatible-property check
            # (``_JSON_PATH_COMPATIBLE_PROPERTY_PATTERN``) ends in an
            # unescaped "$", which matches just before a single trailing
            # "\n" too -- a key ending in exactly one "\n" is still dotted
            # (``interface.inputs.abc\n.type``), not bracket-quoted.
            "interface_inputs_key_trailing_newline",
            {
                "interface": {"inputs": {"abc\n": {"type": 5}}},
                "effects": [],
            },
        ),
        (
            "name_bad_character",
            {"effects": [{"type": "tool", "name": "bad name", "provider": "ffmpeg"}]},
        ),
        (
            "name_leading_digit",
            {"effects": [{"type": "tool", "name": "1abc", "provider": "ffmpeg"}]},
        ),
        (
            "name_reserved_word",
            {"effects": [{"type": "tool", "name": "value", "provider": "ffmpeg"}]},
        ),
        (
            "name_iter_reserved",
            {"effects": [{"type": "tool", "name": "iter_3", "provider": "ffmpeg"}]},
        ),
        (
            # A non-ASCII (but Unicode-decimal) digit: Python re's \d matches it,
            # ASCII-only [A-Za-z0-9_] doesn't, so this trips *both* NamePattern
            # clauses -- two errors at the same location, not one.
            "name_iter_reserved_unicode_digit",
            {"effects": [{"type": "tool", "name": "iter_\u0663", "provider": "ffmpeg"}]},
        ),
        (
            # Exercises the "$" rewrite inside a "not" sub-schema: "value\n"
            # still matches the reserved-word pattern (Python re's "$" matches
            # just before a single trailing "\n"), so the reserved-word check
            # still rejects it on both sides.
            "name_reserved_word_with_trailing_newline",
            {"effects": [{"type": "tool", "name": "value\n", "provider": "ffmpeg"}]},
        ),
        (
            # Exercises the "$" rewrite inside a "not" sub-schema together with
            # the "\d" rewrite: "iter_3\n" still matches "^iter_\d+$", so the
            # iter_<N> reserved check still rejects it on both sides.
            "name_iter_reserved_with_trailing_newline",
            {"effects": [{"type": "tool", "name": "iter_3\n", "provider": "ffmpeg"}]},
        ),
        (
            # Over-acceptance guard: Python re's "$" matches before only a
            # *single* trailing "\n", so a second trailing "\n" must still be
            # rejected on both sides -- the rewritten "\n?$" must not become
            # "\n*$" by accident.
            "name_two_trailing_newlines_still_invalid",
            {"effects": [{"type": "tool", "name": "fetch\n\n", "provider": "ffmpeg"}]},
        ),
        (
            "flow_enum_invalid",
            {"flow": "bogus", "effects": []},
        ),
        (
            "retries_max_attempts_below_minimum",
            {
                "effects": [
                    {
                        "type": "tool",
                        "name": "t",
                        "provider": "ffmpeg",
                        "retries": {"max_attempts": 0},
                    }
                ]
            },
        ),
        (
            "loop_body_nested_missing_provider",
            {
                "effects": [
                    {
                        "type": "loop",
                        "each": {"in": "prime.x.value"},
                        "body": [{"type": "tool", "name": "t"}],
                    }
                ]
            },
        ),
        (
            # Issue #528's own worked example: an empty "body" (a minItems
            # violation) and a missing "each"/"while" (a oneOf failure with
            # context) on the same effect -- two different-message errors
            # at two different locations, ordered by message text.
            "loop_empty_body_and_missing_each_or_while",
            {"effects": [{"name": "a", "type": "loop", "body": []}]},
        ),
        (
            # Two effects each missing the same required keys: several
            # pairs of violations share the exact same message text at two
            # different locations, pinning that the sort breaks the tie by
            # location rather than leaving it to chance.
            "two_effects_with_identical_missing_required_errors",
            {"effects": [{}, {}]},
        ),
        (
            # Issue #528's own repro: a container missing
            # its own required field that holds a child also missing the
            # same field -- two identical-message, identical-keyword errors
            # whose schema paths differ only by the outer one being a strict
            # prefix of the inner one. Python's sort (`str(err)`, whose
            # schema-path component drops the firing keyword) puts the
            # shorter path first; pushing the keyword back onto the full
            # path before comparing reverses this pair.
            "nested_dynamic_each_missing_name",
            {
                "effects": [
                    {
                        "type": "dynamic",
                        "effects": [{"type": "tool", "provider": "ffmpeg"}],
                    }
                ]
            },
        ),
        (
            "dynamic_missing_effects",
            {"effects": [{"type": "dynamic", "name": "d"}]},
        ),
        (
            "if_missing_then",
            {
                "effects": [
                    {
                        "type": "if",
                        "name": "i",
                        "if": {"mode": "cel", "expr": "true"},
                    }
                ]
            },
        ),
        (
            "reflector_missing_effects",
            {"effects": [{"type": "reflector", "name": "r"}]},
        ),
        (
            "yield_missing_template",
            {"effects": [{"type": "yield", "name": "y"}]},
        ),
    ]

    return [_orchestration_case(name, doc) for name, doc in valid + invalid]


# ---------------------------------------------------------------------
# Profile documents
# ---------------------------------------------------------------------


def profile_cases() -> list[dict]:
    valid: list[tuple[str, Any]] = [
        ("empty", {}),
        (
            "adapter_model_out_inputs",
            {"adapter": "ollama", "model": "llama3", "out": "out.json", "inputs": {"a": 1}},
        ),
        (
            "per_effect_overrides",
            {"effects": {"summarize": {"model": "m", "provider": "p", "enabled": True}}},
        ),
        ("routing_false", {"effects": {"a.b": {"routing": False}}}),
        ("routing_named_band", {"effects": {"a.b": {"routing": "heavy"}}}),
        ("persistence_sqlite", {"persistence": {"backend": "sqlite"}}),
    ]

    invalid: list[tuple[str, Any]] = [
        ("unknown_top_level_key", {"bogus": 1}),
        ("effect_override_empty", {"effects": {"summarize": {}}}),
        (
            "effect_override_unknown_key",
            {"effects": {"summarize": {"model": "m", "bogus": 1}}},
        ),
        ("persistence_missing_backend", {"persistence": {"enabled": True}}),
        ("inputs_wrong_type", {"inputs": "nope"}),
        ("root_wrong_type", []),
        ("routing_wrong_oneof", {"effects": {"a.b": {"routing": 5}}}),
    ]

    return [_profile_case(name, doc) for name, doc in valid + invalid]


# ---------------------------------------------------------------------
# Structural documents -- unknown keys interleaved with schema errors
# ---------------------------------------------------------------------


def _structural_case(name: str, document: Any) -> dict:
    """Like `_orchestration_case`, but `structural_errors` rather than just
    `schema_errors` -- near-miss unknown keys, `group:` placement and
    `interface.inputs` type errors interleaved with schema violations, in
    `core/document_check.structural_errors`'s own order. Replayed on the Rust
    side against `electricity_compiler::structural_errors`, not
    `electricity_schema::orchestration_errors` (schema errors alone) --
    see `electricity-compiler/tests/structural_corpus.rs`'s own doc
    comment.
    """
    described = document_check.structural_errors(document)
    return {
        "name": name,
        "schema": "orchestration_structural",
        "document": document,
        "valid": not described,
        "described_errors": described,
    }


def structural_cases() -> list[dict]:
    invalid: list[tuple[str, Any]] = [
        (
            # Issue #528's review: a near-miss unknown key right
            # next to two schema errors on the same effect (the issue's own
            # acceptance bullet, "unknown keys next to schema errors") --
            # `structural_errors` reports the near-miss first (the likelier
            # cause), then the schema errors, in that order.
            "near_miss_key_next_to_schema_errors",
            {
                "effects": [
                    {
                        "type": "loop",
                        "whlie": {"mode": "cel", "expr": "true"},
                        "body": [],
                    }
                ]
            },
        ),
    ]
    return [_structural_case(name, doc) for name, doc in invalid]


def build_corpus() -> list[dict]:
    return orchestration_cases() + profile_cases() + structural_cases()


def render(cases: list[dict]) -> str:
    return json.dumps(cases, indent=2, ensure_ascii=False, sort_keys=True) + "\n"


def main() -> int:
    text = render(build_corpus())
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_text() if OUTPUT.exists() else ""
        if current != text:
            print(f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr)
            return 1
        return 0
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text(text)
    print(f"wrote {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
