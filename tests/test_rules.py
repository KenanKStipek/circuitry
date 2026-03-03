"""Tests that rule files in rules/ stay in sync with orchestration.schema.json.

Verifies field coverage, enum consistency, required fields, and loader output.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
import yaml

PROJECT_ROOT = Path(__file__).resolve().parent.parent
RULES_DIR = PROJECT_ROOT / "rules"
SCHEMA_PATH = PROJECT_ROOT / "src" / "circuitry" / "schema" / "orchestration.schema.json"

# Map rule file names to JSON schema $defs keys.
_RULE_TO_SCHEMA = {
    "prompt": "PromptEffect",
    "dynamic": "DynamicEffect",
    "loop": "LoopEffect",
    "conditional": "ConditionalEffect",
    "tool": "ToolEffect",
    "reflector": "ReflectorEffect",
}


@pytest.fixture(scope="module")
def schema() -> dict:
    return json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))


@pytest.fixture(scope="module")
def schema_defs(schema: dict) -> dict:
    return schema["$defs"]


def _load_rule(name: str) -> dict:
    path = RULES_DIR / f"{name}.yml"
    return yaml.safe_load(path.read_text(encoding="utf-8"))


# ─── All effect types have a rule file ─────────────────────────────────────


def test_all_effect_types_have_rule_files():
    """Every effect type in the schema has a corresponding rule file."""
    for rule_name in _RULE_TO_SCHEMA:
        path = RULES_DIR / f"{rule_name}.yml"
        assert path.is_file(), f"Missing rule file: {path}"


def test_common_and_patterns_exist():
    assert (RULES_DIR / "common.yml").is_file()
    assert (RULES_DIR / "patterns.yml").is_file()


# ─── Field coverage: rule file fields exist in schema ──────────────────────


@pytest.mark.parametrize("rule_name,schema_key", list(_RULE_TO_SCHEMA.items()))
def test_rule_fields_exist_in_schema(rule_name: str, schema_key: str, schema_defs: dict):
    """Every field listed in a rule file exists in the corresponding schema definition."""
    rule = _load_rule(rule_name)
    rule_fields = set(rule.get("fields", {}).keys())

    schema_def = schema_defs[schema_key]
    schema_props = set(schema_def.get("properties", {}).keys())

    missing = rule_fields - schema_props
    assert not missing, (
        f"Rule file {rule_name}.yml lists fields not in schema {schema_key}: {missing}"
    )


# ─── Required fields match ─────────────────────────────────────────────────


@pytest.mark.parametrize("rule_name,schema_key", list(_RULE_TO_SCHEMA.items()))
def test_required_fields_match(rule_name: str, schema_key: str, schema_defs: dict):
    """Required fields in rule files match the schema's required array."""
    rule = _load_rule(rule_name)
    rule_required = set(rule.get("required", []))

    schema_def = schema_defs[schema_key]
    schema_required = set(schema_def.get("required", []))

    assert rule_required == schema_required, (
        f"Rule file {rule_name}.yml required={rule_required} vs schema {schema_key} required={schema_required}"
    )


# ─── Enum consistency ──────────────────────────────────────────────────────


def _collect_enums(rule: dict) -> dict[str, set[str]]:
    """Extract {field_name: set_of_enum_values} from a rule file."""
    result = {}
    for field_name, field_def in rule.get("fields", {}).items():
        if isinstance(field_def, dict) and "enum" in field_def:
            result[field_name] = set(field_def["enum"])
    return result


def _collect_schema_enums(schema_def: dict, schema_defs: dict) -> dict[str, set[str]]:
    """Extract {field_name: set_of_enum_values} from a schema definition."""
    result = {}
    for field_name, field_def in schema_def.get("properties", {}).items():
        if "enum" in field_def:
            result[field_name] = set(field_def["enum"])
        elif "$ref" in field_def:
            ref_name = field_def["$ref"].split("/")[-1]
            ref_def = schema_defs.get(ref_name, {})
            if "enum" in ref_def:
                result[field_name] = set(ref_def["enum"])
    return result


@pytest.mark.parametrize("rule_name,schema_key", list(_RULE_TO_SCHEMA.items()))
def test_enum_values_match(rule_name: str, schema_key: str, schema_defs: dict):
    """Enum values in rule files match the schema's enums."""
    rule = _load_rule(rule_name)
    rule_enums = _collect_enums(rule)

    schema_def = schema_defs[schema_key]
    schema_enums = _collect_schema_enums(schema_def, schema_defs)

    for field_name, rule_values in rule_enums.items():
        if field_name in schema_enums:
            schema_values = schema_enums[field_name]
            assert rule_values <= schema_values, (
                f"Rule {rule_name}.yml field '{field_name}' has enum values "
                f"{rule_values - schema_values} not in schema"
            )


# ─── Common rules: naming pattern matches schema ──────────────────────────


def test_naming_pattern_matches_schema(schema_defs: dict):
    common = _load_rule("common")
    rule_pattern = common["naming"]["pattern"]

    schema_name = schema_defs["NamePattern"]
    schema_patterns = [c["pattern"] for c in schema_name.get("allOf", []) if "pattern" in c]

    assert rule_pattern in schema_patterns, (
        f"Common naming pattern {rule_pattern!r} not found in schema NamePattern patterns {schema_patterns}"
    )


# ─── Common rules: valid flows match schema ────────────────────────────────


def test_flow_values_match_schema(schema_defs: dict):
    common = _load_rule("common")
    rule_flows = (
        set(common["file_structure"]["valid_flows"]["sequential"])
        | set(common["file_structure"]["valid_flows"]["parallel"])
    )

    schema_flows = set(schema_defs["FlowModel"]["enum"])

    assert rule_flows == schema_flows, (
        f"Common valid_flows {rule_flows} != schema FlowModel {schema_flows}"
    )


# ─── Loader tests ─────────────────────────────────────────────────────────


def test_load_all_rules():
    from circuitry.rules import load_all_rules

    result = load_all_rules(RULES_DIR)
    assert len(result) > 0
    assert "type: prompt" in result
    assert "type: loop" in result
    assert "type: tool" in result
    assert "section: common" in result
    assert "section: patterns" in result


def test_load_rules_for_prompt():
    from circuitry.rules import load_rules_for

    result = load_rules_for("prompt", rules_dir=RULES_DIR)
    assert "section: common" in result
    assert "type: prompt" in result
    # Should NOT include other effect type rule files (check for their summary lines)
    assert "\ntype: loop\n" not in result
    assert "\ntype: tool\n" not in result


def test_load_rules_for_multiple():
    from circuitry.rules import load_rules_for

    result = load_rules_for("prompt", "tool", rules_dir=RULES_DIR)
    assert "section: common" in result
    assert "type: prompt" in result
    assert "type: tool" in result
    assert "type: loop" not in result


def test_load_rules_for_missing_type():
    from circuitry.rules import load_rules_for

    result = load_rules_for("nonexistent", rules_dir=RULES_DIR)
    # Should still include common
    assert "section: common" in result
    # But nothing else
    assert "type: prompt" not in result


def test_load_rule_file():
    from circuitry.rules import load_rule_file

    result = load_rule_file(RULES_DIR, "prompt")
    assert "type: prompt" in result
    assert len(result) > 0


def test_load_rule_file_missing():
    from circuitry.rules import load_rule_file

    result = load_rule_file(RULES_DIR, "nonexistent")
    assert result == ""
