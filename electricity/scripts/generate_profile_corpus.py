#!/usr/bin/env python3
"""Generate electricity-compiler's golden corpus for profile loading and
validation (lane M1-J1, issue #488) from Circuitry's own real functions:
``cli/profiles.py``'s ``parse_profile_document``, ``_validate_effect_
paths``, ``collect_orchestration_effect_paths``/``collect_orchestration_
condition_paths``, ``condition_target_message``, ``validate_profile_
routing_pins``, ``_settings_from_raw``, plus ``cli/complexity_config.py``'s
``band_named``/``resolve_complexity_settings`` for the routing-pin cases'
own ``RoutingSettings`` fixtures.

electricity has no profile *discovery*: ``--profile <path>`` names the
file directly (DESIGN.md §6.11, §1), where Circuitry's own ``load_profile``
takes a *name* and discovers the path. Every "end to end" case below
therefore calls Circuitry's own ``parse_profile_document``/
``_validate_effect_paths``/``_settings_from_raw`` directly, deriving
*name* the same way electricity's own ``profile_name_from_path`` does
(the file's own basename with exactly one trailing ``.yml``/``.yaml``
suffix stripped) rather than through ``discover_profile_path`` -- for a
given file this is the identical string discovery would have used
(DESIGN.md §6.11's own ruling), so this is a narrowing of Circuitry's
real code path, not a reimplementation of it.

Every value that can't be prose-level JSON (a `date`/`bytes`/non-string
dict key) is recorded with the same tagged encoding
`generate_value_corpus.py`/`generate_store_corpus.py` use (`{"t": ...}`);
every other value is left as plain JSON. Every error records its
exception class and text, with the generator's own temporary profile
path replaced by the literal placeholder `{path}` -- the Rust replay
substitutes its own path at the same position before comparing.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI, `-c electricity/scripts/generator-constraints.txt`, as CI does).
Usage: python3 generate_profile_corpus.py [--check]
"""

from __future__ import annotations

import datetime
import json
import struct
import sys
import tempfile
from pathlib import Path
from typing import Any

from circuitry.cli import profiles
from circuitry.cli.complexity_config import (
    ComplexityBand,
    RoutingSettings,
    resolve_complexity_settings,
)

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-compiler"
    / "tests"
    / "golden"
    / "profile_corpus.json"
)

#: electricity's own derivation (`profile_name_from_path`), narrowed from
#: Circuitry's name-based discovery (DESIGN.md §6.11) -- used throughout
#: this generator in place of `discover_profile_path`.
_PROFILE_SUFFIXES = (".yml", ".yaml")


def _profile_name_from_path(path: Path) -> str:
    file_name = path.name
    for suffix in _PROFILE_SUFFIXES:
        if file_name.endswith(suffix):
            return file_name[: -len(suffix)]
    return file_name


# ---------------------------------------------------------------------
# Tagged value encoding (matches generate_value_corpus.py's)
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
    if isinstance(value, (bytes, bytearray)):
        return {"t": "bytes", "v": bytes(value).hex()}
    if isinstance(value, list):
        return {"t": "list", "v": [encode(item) for item in value]}
    if isinstance(value, dict):
        return {"t": "dict", "v": [[encode(k), encode(v)] for k, v in value.items()]}
    if isinstance(value, datetime.datetime):
        naive = value.replace(tzinfo=None)
        offset = value.utcoffset()
        return {
            "t": "datetime",
            "naive": naive.isoformat(timespec="microseconds"),
            "offset_seconds": None if offset is None else round(offset.total_seconds()),
        }
    if isinstance(value, datetime.date):
        return {"t": "date", "v": value.isoformat()}
    raise TypeError(f"no tagged encoding for {type(value)!r}")


def _redact_path(text: str, path: Path) -> str:
    return text.replace(str(path), "{path}")


def _error_result(
    exc: Exception, path: Path | None, *, mode: str = "exact"
) -> dict[str, Any]:
    """*mode* says how the Rust replay must compare `error` text:

    - ``"exact"``: Circuitry's own text, byte for byte (after `{path}`
      substitution) -- the default.
    - ``"electricity_own"``: not reproduced from Circuitry at all
      (not-found/unreadable text, DESIGN.md §6.11's own ruling) --
      the Rust replay checks only that this case fails.
    - ``"location"``: third-party text (a YAML parser's own `str()`) --
      the Rust replay checks only that this case fails with a
      non-empty message.
    - ``"schema"``: Circuitry's own header and each line's own location
      are exact; the per-line message past that is third-party
      `jsonschema` text, and multi-error ordering is not reproducible
      (Python sorts by `str(err)`) -- the Rust replay compares the
      header plus the *set* of locations, never the message text or
      line order.
    """
    message = str(exc)
    if path is not None:
        message = _redact_path(message, path)
    return {"ok": False, "error_class": type(exc).__name__, "error": message, "mode": mode}


# ---------------------------------------------------------------------
# parse_profile_document
# ---------------------------------------------------------------------


def parse_case(
    name: str,
    *,
    root: Path,
    file_text: str | None = None,
    file_bytes: bytes | None = None,
    missing: bool = False,
    directory: bool = False,
    profile_name: str = "demo",
    mode: str = "exact",
) -> dict[str, Any]:
    case_dir = root / name
    case_dir.mkdir(parents=True)
    path = case_dir / f"{profile_name}.yml"
    if missing:
        pass
    elif directory:
        path.mkdir()
    elif file_bytes is not None:
        path.write_bytes(file_bytes)
    else:
        path.write_text(file_text or "", encoding="utf-8")

    try:
        raw = profiles.parse_profile_document(path, name=profile_name)
    except Exception as exc:
        expect = _error_result(exc, path, mode=mode)
    else:
        expect = {"ok": True, "value": encode(raw)}

    return {
        "name": name,
        "kind": "parse_profile_document",
        "input": _file_input(
            profile_name=profile_name,
            file_text=file_text,
            file_bytes=file_bytes,
            missing=missing,
            directory=directory,
        ),
        "expect": expect,
    }


def _file_input(
    *,
    profile_name: str,
    file_text: str | None,
    file_bytes: bytes | None,
    missing: bool,
    directory: bool,
) -> dict[str, Any]:
    """What the Rust replay needs to recreate this case's own file under
    its own fresh temporary directory -- the generator's own temp root
    is never recorded (a fresh, incomparable path every run)."""
    return {
        "profile_name": profile_name,
        "missing": missing,
        "directory": directory,
        "file_text": file_text,
        "file_bytes_hex": file_bytes.hex() if file_bytes is not None else None,
    }


# ---------------------------------------------------------------------
# collect_orchestration_effect_paths / collect_orchestration_condition_paths
# ---------------------------------------------------------------------


def collect_case(name: str, *, document: dict[str, Any]) -> dict[str, Any]:
    effect_paths = sorted(profiles.collect_orchestration_effect_paths(document))
    condition_paths = sorted(profiles.collect_orchestration_condition_paths(document))
    return {
        "name": name,
        "kind": "collect_effect_paths",
        "input": {"document": document},
        "expect": {
            "effect_paths": effect_paths,
            "condition_paths": condition_paths,
        },
    }


# ---------------------------------------------------------------------
# _validate_effect_paths
# ---------------------------------------------------------------------


def validate_effect_paths_case(
    name: str, *, effects: Any, document: dict[str, Any], profile_name: str = "demo"
) -> dict[str, Any]:
    try:
        profiles._validate_effect_paths(effects, orch=document, profile_name=profile_name)
    except Exception as exc:
        expect = _error_result(exc, None)
    else:
        expect = {"ok": True}
    return {
        "name": name,
        "kind": "validate_effect_paths",
        "input": {
            "effects": encode(effects),
            "document": document,
            "profile_name": profile_name,
        },
        "expect": expect,
    }


# ---------------------------------------------------------------------
# validate_profile_routing_pins
# ---------------------------------------------------------------------


def _encode_routing(routing: RoutingSettings) -> dict[str, Any]:
    return {
        "enabled": routing.enabled,
        "bands": [
            {"model": b.model, "max": b.max, "name": b.name} for b in routing.bands
        ],
    }


def routing_pins_case(
    name: str, *, effects: dict[str, Any], bands: list[ComplexityBand], profile_name: str = "demo"
) -> dict[str, Any]:
    routing = RoutingSettings(enabled=True, bands=tuple(bands))
    return _routing_pins_case_with(name, effects=effects, routing=routing, profile_name=profile_name)


def routing_pins_case_via_resolve(
    name: str,
    *,
    effects: dict[str, Any],
    complexity_block: dict[str, Any],
    profile_name: str = "demo",
) -> dict[str, Any]:
    """Like :func:`routing_pins_case`, but *routing* comes from a synthetic
    ``runtime.complexity`` block run through ``resolve_complexity_settings`` --
    the real path the run wiring (lane R) takes, rather than a
    hand-built `RoutingSettings`."""
    settings = resolve_complexity_settings({"complexity": complexity_block})
    return _routing_pins_case_with(
        name, effects=effects, routing=settings.routing, profile_name=profile_name
    )


def _routing_pins_case_with(
    name: str, *, effects: dict[str, Any], routing: RoutingSettings, profile_name: str
) -> dict[str, Any]:
    try:
        profiles.validate_profile_routing_pins(effects, routing=routing, profile_name=profile_name)
    except Exception as exc:
        expect = _error_result(exc, None)
    else:
        expect = {"ok": True}
    return {
        "name": name,
        "kind": "validate_profile_routing_pins",
        "input": {
            "effects": encode(effects),
            "routing": _encode_routing(routing),
            "profile_name": profile_name,
        },
        "expect": expect,
    }


# ---------------------------------------------------------------------
# _settings_from_raw
# ---------------------------------------------------------------------


def settings_from_raw_case(
    name: str, *, raw: dict[str, Any], profile_name: str = "demo", path: str = "/profiles/demo.yml"
) -> dict[str, Any]:
    settings = profiles._settings_from_raw(raw, name=profile_name, path=Path(path))
    return {
        "name": name,
        "kind": "settings_from_raw",
        "input": {"raw": encode(raw), "profile_name": profile_name, "path": path},
        "expect": {
            "name": settings.name,
            "adapter": settings.adapter,
            "model": settings.model,
            "out": settings.out,
            "inputs": encode(settings.inputs),
            "effects": encode(settings.effects),
            "persistence": encode(settings.persistence) if settings.persistence is not None else None,
            "raw": encode(settings.raw),
        },
    }


# ---------------------------------------------------------------------
# load_profile, end to end (no discovery -- see module docstring)
# ---------------------------------------------------------------------


def load_profile_case(
    name: str,
    *,
    root: Path,
    file_text: str | None = None,
    file_bytes: bytes | None = None,
    document: dict[str, Any],
    profile_name: str = "demo",
    missing: bool = False,
    mode: str = "exact",
) -> dict[str, Any]:
    case_dir = root / name
    case_dir.mkdir(parents=True)
    path = case_dir / f"{profile_name}.yml"
    if missing:
        pass
    elif file_bytes is not None:
        path.write_bytes(file_bytes)
    else:
        path.write_text(file_text or "", encoding="utf-8")
    derived_name = _profile_name_from_path(path)

    try:
        raw = profiles.parse_profile_document(path, name=derived_name)
        effects_raw = raw.get("effects") or {}
        profiles._validate_effect_paths(effects_raw, orch=document, profile_name=derived_name)
        settings = profiles._settings_from_raw(raw, name=derived_name, path=path)
    except Exception as exc:
        expect = _error_result(exc, path, mode=mode)
    else:
        expect = {
            "ok": True,
            "name": settings.name,
            "adapter": settings.adapter,
            "model": settings.model,
            "out": settings.out,
            "inputs": encode(settings.inputs),
            "effects": encode(settings.effects),
            "persistence": encode(settings.persistence) if settings.persistence is not None else None,
            "raw": encode(settings.raw),
        }

    return {
        "name": name,
        "kind": "load_profile",
        "input": {
            "document": document,
            "profile_name": profile_name,
            "missing": missing,
            "file_text": file_text,
            "file_bytes_hex": file_bytes.hex() if file_bytes is not None else None,
        },
        "expect": expect,
    }


# ---------------------------------------------------------------------
# Cases
# ---------------------------------------------------------------------


def _tool(path_name: str) -> dict[str, Any]:
    return {"type": "tool", "name": path_name, "provider": "json", "params": {"mode": "stringify", "input": 1}}


def build_cases(root: Path) -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []

    # --- parse_profile_document: locating/reading/YAML/root/schema ---
    cases.append(parse_case("missing_file", root=root, missing=True, mode="electricity_own"))
    cases.append(parse_case("path_is_a_directory", root=root, directory=True, mode="electricity_own"))
    cases.append(parse_case("empty_file_becomes_empty_mapping", root=root, file_text=""))
    cases.append(parse_case("whitespace_only_file_becomes_empty_mapping", root=root, file_text="   \n   \n"))
    cases.append(parse_case("comment_only_file_becomes_empty_mapping", root=root, file_text="# just a comment\n"))
    cases.append(parse_case("null_root_becomes_empty_mapping", root=root, file_text="null\n"))
    cases.append(parse_case("false_root_becomes_empty_mapping", root=root, file_text="false\n"))
    cases.append(parse_case("empty_list_root_becomes_empty_mapping", root=root, file_text="[]\n"))
    cases.append(parse_case("zero_root_becomes_empty_mapping", root=root, file_text="0\n"))
    cases.append(parse_case("empty_string_root_becomes_empty_mapping", root=root, file_text="''\n"))
    cases.append(parse_case("empty_mapping_root", root=root, file_text="{}\n"))
    cases.append(parse_case("non_empty_list_root_is_rejected", root=root, file_text="- 1\n- 2\n"))
    cases.append(parse_case("scalar_root_is_rejected", root=root, file_text="hello\n"))
    cases.append(
        parse_case(
            "duplicate_key_last_wins",
            root=root,
            file_text="model: a\nmodel: b\n",
        )
    )
    cases.append(
        parse_case(
            "crlf_duplicate_key_last_wins",
            root=root,
            file_text="model: a\r\nmodel: b\r\n",
        )
    )
    cases.append(
        parse_case(
            "lone_cr_duplicate_key_last_wins",
            root=root,
            file_text="model: a\rmodel: b\r",
        )
    )
    cases.append(
        parse_case(
            "yaml_1_1_scalars_yes_and_octal_and_date",
            root=root,
            file_text="adapter: 'x'\ninputs:\n  flag: yes\n  legacy_octal: 017\n  day: 2024-01-01\n",
        )
    )
    cases.append(
        parse_case(
            "merge_key_is_applied",
            root=root,
            file_text=(
                "inputs:\n"
                "  base: &base\n"
                "    x: 1\n"
                "  merged:\n"
                "    <<: *base\n"
                "    y: 2\n"
            ),
        )
    )
    cases.append(
        parse_case(
            "leading_bom_is_kept_and_skipped_by_the_yaml_loader",
            root=root,
            file_bytes=b"\xef\xbb\xbfadapter: openai\n",
        )
    )
    cases.append(
        parse_case(
            "invalid_utf8_at_a_start_byte",
            root=root,
            file_bytes=b"adapter: \xff\n",
        )
    )
    cases.append(
        parse_case(
            "invalid_utf8_at_a_continuation_byte",
            root=root,
            file_bytes=b"adapter: \xe2\x28\n",
        )
    )
    cases.append(
        parse_case(
            "invalid_utf8_at_the_end_of_data",
            root=root,
            file_bytes=b"adapter: \xe2\x82",
        )
    )
    cases.append(
        parse_case(
            "malformed_yaml_syntax",
            root=root,
            file_text="adapter: [unterminated\n",
            mode="location",
        )
    )
    cases.append(
        parse_case(
            "schema_unknown_root_key",
            root=root,
            file_text="bogus_key: 1\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_type_error_on_adapter",
            root=root,
            file_text="adapter: 5\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_effects_entry_min_properties",
            root=root,
            file_text="effects:\n  summarize: {}\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_effects_entry_additional_properties",
            root=root,
            file_text="effects:\n  summarize:\n    bogus: 1\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_routing_false_is_valid",
            root=root,
            file_text="effects:\n  summarize:\n    routing: false\n",
        )
    )
    cases.append(
        parse_case(
            "schema_routing_true_is_invalid",
            root=root,
            file_text="effects:\n  summarize:\n    routing: true\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_routing_empty_string_is_invalid",
            root=root,
            file_text="effects:\n  summarize:\n    routing: ''\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_persistence_missing_backend",
            root=root,
            file_text="persistence:\n  path: x.jsonl\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_several_errors_at_once",
            root=root,
            file_text="adapter: 5\nmodel: 6\nbogus: 1\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_non_json_date_at_bare_object_position",
            root=root,
            file_text="inputs: 2024-01-01\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_non_string_key_under_effects",
            root=root,
            file_text="effects:\n  1:\n    model: x\n",
        )
    )
    cases.append(
        parse_case(
            "schema_non_string_key_under_inputs_is_valid",
            root=root,
            file_text="inputs:\n  yes: 1\n",
        )
    )

    # --- schema: `type` on every property, not just `adapter` ---
    cases.append(
        parse_case("schema_type_error_on_model", root=root, file_text="model: 5\n", mode="schema")
    )
    cases.append(
        parse_case("schema_type_error_on_out", root=root, file_text="out: 5\n", mode="schema")
    )
    cases.append(
        parse_case("schema_type_error_on_inputs", root=root, file_text="inputs: 5\n", mode="schema")
    )
    cases.append(
        parse_case("schema_type_error_on_effects", root=root, file_text="effects: 5\n", mode="schema")
    )
    cases.append(
        parse_case(
            "schema_type_error_on_persistence",
            root=root,
            file_text="persistence: 5\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_routing_int_is_invalid",
            root=root,
            file_text="effects:\n  summarize:\n    routing: 1\n",
            mode="schema",
        )
    )

    # --- schema markers: the spurious-marker filter under `effects`/
    # `persistence`, not just the `inputs` case above; a non-JSON scalar
    # at a `type: string` position; non-string-key locations ---
    cases.append(
        parse_case(
            "schema_date_marker_under_persistence_fails_type_not_required",
            root=root,
            file_text="persistence: 2024-01-01\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_date_marker_under_an_effects_path_fails_type_not_additional_properties",
            root=root,
            file_text="effects:\n  x: 2024-01-01\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_bytes_marker_under_inputs_fails_type_not_object",
            root=root,
            file_text="inputs: !!binary aGk=\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_nan_marker_on_adapter_fails_type_not_string",
            root=root,
            file_text="adapter: .nan\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_date_marker_on_model_fails_type_not_string",
            root=root,
            file_text="model: 2024-01-01\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_non_string_int_key_location_under_effects",
            root=root,
            file_text="effects:\n  1: {}\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_non_string_date_key_location_under_effects",
            root=root,
            file_text="effects:\n  2024-01-01: {}\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_non_string_naive_datetime_key_location_under_effects",
            root=root,
            file_text="effects:\n  2024-01-01 00:00:00: {}\n",
            mode="schema",
        )
    )
    cases.append(
        parse_case(
            "schema_non_string_aware_datetime_key_location_under_effects",
            root=root,
            file_text="effects:\n  2024-01-01T00:00:00Z: {}\n",
            mode="schema",
        )
    )

    # --- collect_orchestration_effect_paths / condition_paths ---
    cases.append(
        collect_case(
            "named_dynamic_with_named_child",
            document={
                "effects": [
                    {"type": "dynamic", "name": "d", "effects": [_tool("t")]},
                ]
            },
        )
    )
    cases.append(
        collect_case(
            "unnamed_dynamic_contributes_no_segment",
            document={
                "effects": [
                    {"type": "dynamic", "effects": [_tool("t")]},
                ]
            },
        )
    )
    cases.append(
        collect_case(
            "steps_key_in_place_of_effects",
            document={"steps": [_tool("t")]},
        )
    )
    cases.append(
        collect_case(
            "named_if_then_and_else_with_conditions",
            document={
                "effects": [
                    {
                        "type": "if",
                        "name": "c",
                        "if": {"mode": "cel", "expr": "true"},
                        "then": [_tool("a")],
                        "else": [_tool("b")],
                    }
                ]
            },
        )
    )
    cases.append(
        collect_case(
            "unnamed_if_contributes_no_condition",
            document={
                "effects": [
                    {
                        "type": "if",
                        "if": {"mode": "cel", "expr": "true"},
                        "then": [_tool("a")],
                    }
                ]
            },
        )
    )
    cases.append(
        collect_case(
            "named_loop_body_with_conditions",
            document={
                "effects": [
                    {
                        "type": "loop",
                        "name": "l",
                        "while": {"mode": "cel", "expr": "true"},
                        "body": [_tool("t")],
                    }
                ]
            },
        )
    )
    cases.append(
        collect_case(
            "named_reflector_recurses_like_dynamic",
            document={
                "effects": [
                    {"type": "reflector", "name": "r", "effects": [_tool("t")]},
                ]
            },
        )
    )
    cases.append(
        collect_case(
            "unnamed_reflector_contributes_no_segment",
            document={
                "effects": [
                    {"type": "reflector", "effects": [_tool("t")]},
                ]
            },
        )
    )
    cases.append(
        collect_case(
            "unnamed_loop_contributes_no_segment_or_condition",
            document={
                "effects": [
                    {"type": "loop", "while": {"mode": "cel", "expr": "true"}, "body": [_tool("t")]},
                ]
            },
        )
    )
    cases.append(
        collect_case(
            "conditional_alias_behaves_like_if",
            document={
                "effects": [
                    {
                        "type": "conditional",
                        "name": "c",
                        "if": {"mode": "cel", "expr": "true"},
                        "then": [_tool("a")],
                        "else": [_tool("b")],
                    }
                ]
            },
        )
    )
    cases.append(
        collect_case(
            "use_children_are_never_entered",
            document={
                "effects": [
                    {"type": "use", "name": "u", "path": "child.yml", "effects": [_tool("t")]},
                ]
            },
        )
    )
    cases.append(
        collect_case(
            "top_level_finally_is_never_walked",
            document={
                "effects": [_tool("t")],
                "finally": [_tool("f")],
            },
        )
    )
    cases.append(
        collect_case(
            "nested_finally_inside_dynamic_is_never_walked",
            document={
                "effects": [
                    {
                        "type": "dynamic",
                        "name": "d",
                        "effects": [_tool("t")],
                        "finally": [_tool("f")],
                    }
                ]
            },
        )
    )
    cases.append(
        collect_case(
            "whitespace_only_name_counts_as_unnamed",
            document={"effects": [{"type": "tool", "name": "   ", "provider": "json"}]},
        )
    )
    cases.append(
        collect_case(
            "non_string_name_counts_as_unnamed",
            document={"effects": [{"type": "tool", "name": 5, "provider": "json"}]},
        )
    )
    cases.append(
        collect_case(
            "upper_case_type_is_normalized",
            document={"effects": [{"type": "DYNAMIC", "name": "d", "effects": [_tool("t")]}]},
        )
    )
    cases.append(
        collect_case(
            "non_string_type_is_stringified_and_then_never_matches",
            document={"effects": [{"type": 5, "name": "x", "effects": [_tool("t")]}]},
        )
    )
    cases.append(
        collect_case(
            "non_mapping_list_elements_are_skipped",
            document={"effects": ["not a mapping", _tool("t")]},
        )
    )
    cases.append(
        collect_case(
            "deeply_nested_named_containers",
            document={
                "effects": [
                    {
                        "type": "dynamic",
                        "name": "outer",
                        "effects": [
                            {
                                "type": "if",
                                "name": "inner",
                                "if": {"mode": "cel", "expr": "true"},
                                "then": [_tool("leaf")],
                            }
                        ],
                    }
                ]
            },
        )
    )

    # --- _validate_effect_paths ---
    base_document = {
        "effects": [
            _tool("summarize"),
            {
                "type": "if",
                "name": "c",
                "if": {"mode": "cel", "expr": "true"},
                "then": [_tool("a")],
            },
        ]
    }
    cases.append(
        validate_effect_paths_case(
            "valid_known_path_is_accepted",
            effects={"summarize": {"model": "x"}},
            document=base_document,
        )
    )
    cases.append(
        validate_effect_paths_case(
            "condition_target_alone",
            effects={"c.if": {"enabled": False}},
            document=base_document,
        )
    )
    cases.append(
        validate_effect_paths_case(
            "condition_target_several",
            effects={"c.if": {"enabled": False}, "c.condition": {"enabled": False}},
            document=base_document,
        )
    )
    cases.append(
        validate_effect_paths_case(
            "unknown_path_with_named_effects_present",
            effects={"bogus": {"enabled": False}},
            document=base_document,
        )
    )
    cases.append(
        validate_effect_paths_case(
            "unknown_path_with_no_named_effects",
            effects={"bogus": {"enabled": False}},
            document={"effects": []},
        )
    )
    cases.append(
        validate_effect_paths_case(
            "conditions_win_over_unknown_keys",
            effects={"c.if": {"enabled": False}, "bogus": {"enabled": False}},
            document=base_document,
        )
    )
    cases.append(
        validate_effect_paths_case(
            "empty_effects_is_skipped",
            effects={},
            document=base_document,
        )
    )
    cases.append(
        validate_effect_paths_case(
            "non_mapping_effects_is_skipped",
            effects=["not", "a", "mapping"],
            document=base_document,
        )
    )
    cases.append(
        validate_effect_paths_case(
            "one_non_string_unknown_key",
            effects={1: {"enabled": False}},
            document={"effects": []},
        )
    )
    cases.append(
        validate_effect_paths_case(
            "several_non_string_unknown_keys_of_one_type",
            effects={1: {"enabled": False}, 2: {"enabled": False}},
            document={"effects": []},
        )
    )
    cases.append(
        validate_effect_paths_case(
            "mixed_string_and_int_unknown_keys_raise_sort_type_error",
            effects={"a": {"enabled": False}, 1: {"enabled": False}},
            document={"effects": []},
        )
    )
    cases.append(
        validate_effect_paths_case(
            "nested_condition_target_uses_the_last_dot_rsplit",
            effects={"d.c.if": {"enabled": False}},
            document={
                "effects": [
                    {
                        "type": "dynamic",
                        "name": "d",
                        "effects": [
                            {
                                "type": "if",
                                "name": "c",
                                "if": {"mode": "cel", "expr": "true"},
                                "then": [_tool("t")],
                            }
                        ],
                    }
                ]
            },
        )
    )

    # --- validate_profile_routing_pins ---
    cases.append(
        routing_pins_case(
            "matching_named_band",
            effects={"summarize": {"routing": "heavy"}},
            bands=[ComplexityBand(model="big", name="heavy")],
        )
    )
    cases.append(
        routing_pins_case(
            "unmatched_single_pin",
            effects={"summarize": {"routing": "heavy"}},
            bands=[ComplexityBand(model="big", name="light")],
        )
    )
    cases.append(
        routing_pins_case(
            "unmatched_several_pins_sorted_across_paths",
            effects={
                "b_path": {"routing": "nope"},
                "a_path": {"routing": "also_nope"},
            },
            bands=[ComplexityBand(model="big", name="light")],
        )
    )
    cases.append(
        routing_pins_case(
            "unnamed_band_never_matches",
            effects={"summarize": {"routing": "heavy"}},
            bands=[ComplexityBand(model="big")],
        )
    )
    cases.append(
        routing_pins_case(
            "no_bands_at_all",
            effects={"summarize": {"routing": "heavy"}},
            bands=[],
        )
    )
    cases.append(
        routing_pins_case(
            "routing_false_is_skipped",
            effects={"summarize": {"routing": False}},
            bands=[],
        )
    )
    cases.append(
        routing_pins_case(
            "non_dict_override_is_skipped",
            effects={"summarize": "not-a-dict"},
            bands=[],
        )
    )
    cases.append(
        routing_pins_case_via_resolve(
            "pin_checked_through_resolve_complexity_settings_even_when_routing_disabled",
            effects={"summarize": {"routing": "missing"}},
            complexity_block={
                "routing": {
                    "enabled": False,
                    "bands": [{"model": "big", "name": "heavy"}],
                }
            },
        )
    )

    # --- _settings_from_raw ---
    cases.append(
        settings_from_raw_case(
            "every_field_present",
            raw={
                "adapter": "openai",
                "model": "gpt-4",
                "out": "result.json",
                "inputs": {"x": 1},
                "effects": {"summarize": {"model": "x"}},
                "persistence": {"backend": "sqlite", "path": "a.db"},
            },
        )
    )
    cases.append(
        settings_from_raw_case(
            "colliding_stringified_keys_first_position_last_value_wins",
            raw={"effects": {1: {"model": "a"}, "1": {"model": "b"}, "z": {"model": "c"}}},
        )
    )
    cases.append(
        settings_from_raw_case(
            "non_dict_effects_values_are_dropped",
            raw={"effects": {"a": "not-a-dict", "b": {"model": "x"}}},
        )
    )
    cases.append(
        settings_from_raw_case(
            "non_dict_persistence_is_none",
            raw={"persistence": False},
        )
    )
    cases.append(
        settings_from_raw_case(
            "truthy_non_dict_adapter_stringifies",
            raw={"adapter": True},
        )
    )
    cases.append(
        settings_from_raw_case(
            "absent_fields_default_empty",
            raw={},
        )
    )
    cases.append(
        settings_from_raw_case(
            "falsy_inputs_becomes_empty_dict",
            raw={"inputs": {}},
        )
    )

    # --- load_profile, end to end ---
    cases.append(
        load_profile_case(
            "end_to_end_success",
            root=root,
            file_text="adapter: openai\nmodel: gpt-4\ninputs:\n  x: 1\neffects:\n  summarize:\n    model: override\n",
            document={"effects": [_tool("summarize")]},
        )
    )
    cases.append(
        load_profile_case(
            "end_to_end_succeeds_with_an_empty_profile",
            root=root,
            file_text="",
            document={"effects": []},
        )
    )
    cases.append(
        load_profile_case(
            "end_to_end_fails_at_not_found",
            root=root,
            missing=True,
            document={"effects": []},
            mode="electricity_own",
        )
    )
    cases.append(
        load_profile_case(
            "end_to_end_fails_at_schema",
            root=root,
            file_text="adapter: 5\n",
            document={"effects": []},
            mode="schema",
        )
    )
    cases.append(
        load_profile_case(
            "end_to_end_fails_at_effect_path_validation",
            root=root,
            file_text="effects:\n  bogus:\n    model: x\n",
            document={"effects": [_tool("summarize")]},
        )
    )
    cases.append(
        load_profile_case(
            "end_to_end_fails_at_malformed_yaml",
            root=root,
            file_text="adapter: [unterminated\n",
            document={"effects": []},
            mode="location",
        )
    )
    cases.append(
        load_profile_case(
            "end_to_end_fails_at_invalid_utf8",
            root=root,
            file_bytes=b"adapter: \xff\n",
            document={"effects": []},
        )
    )
    cases.append(
        load_profile_case(
            "end_to_end_fails_at_a_non_mapping_root",
            root=root,
            file_text="- 1\n- 2\n",
            document={"effects": []},
        )
    )
    cases.append(
        load_profile_case(
            "end_to_end_fails_at_condition_target",
            root=root,
            file_text="effects:\n  c.if:\n    enabled: false\n",
            document={
                "effects": [
                    {
                        "type": "if",
                        "name": "c",
                        "if": {"mode": "cel", "expr": "true"},
                        "then": [_tool("a")],
                    }
                ]
            },
        )
    )

    return cases


def render(cases: list[dict[str, Any]]) -> str:
    return json.dumps(cases, indent=2, ensure_ascii=False, sort_keys=False) + "\n"


def main() -> int:
    check = "--check" in sys.argv[1:]
    with tempfile.TemporaryDirectory(prefix="electricity-profile-corpus-") as tmp:
        root = Path(tmp).resolve()
        cases = build_cases(root)
        text = render(cases)

    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    if check:
        current = OUTPUT.read_text(encoding="utf-8") if OUTPUT.exists() else ""
        if current != text:
            print(f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr)
            return 1
        return 0
    OUTPUT.write_text(text, encoding="utf-8")
    print(f"wrote {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
