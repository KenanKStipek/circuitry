"""Coverage guard: every schema property is documented in the rules/ ruleset.

`orchestration.schema.json` is the validation authority; `rules/*.yml` is the
authoring guide handed to models (via `load_all_rules`). When the schema grows
a field the rules never mention, models cannot discover it — and, worse, invent
their own spelling for it. This test fails CI in that case.

Documenting a field does not mean recommending it: a field that is runner or
config policy (`retries`, `provider_fallbacks`) is covered by an entry that says
so. Documented omission, not absence.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

import pytest

from circuitry.rules import load_all_rules

PROJECT_ROOT = Path(__file__).resolve().parent.parent
RULES_DIR = PROJECT_ROOT / "rules"
SCHEMA_PATH = PROJECT_ROOT / "src" / "circuitry" / "schema" / "orchestration.schema.json"

# JSON Schema keywords whose values are property-name -> subschema maps.
_PROPERTY_MAPS = ("properties", "patternProperties")

# Keywords whose values are name -> subschema maps of reusable definitions.
_DEFINITION_MAPS = ("$defs", "definitions")

# Keywords whose values are subschemas (or lists of subschemas) to descend into.
_SUBSCHEMA_KEYS = (
    "items",
    "additionalProperties",
    "if",
    "then",
    "else",
    "not",
    "allOf",
    "anyOf",
    "oneOf",
)


def _collect_property_names(node: object, acc: set[str]) -> None:
    """Recursively collect every property name declared anywhere in the schema."""
    if isinstance(node, list):
        for item in node:
            _collect_property_names(item, acc)
        return
    if not isinstance(node, dict):
        return

    for keyword in _PROPERTY_MAPS:
        props = node.get(keyword)
        if isinstance(props, dict):
            acc.update(props.keys())
            for sub in props.values():
                _collect_property_names(sub, acc)

    for keyword in _DEFINITION_MAPS:
        defs = node.get(keyword)
        if isinstance(defs, dict):
            for sub in defs.values():
                _collect_property_names(sub, acc)

    for keyword in _SUBSCHEMA_KEYS:
        if keyword in node:
            _collect_property_names(node[keyword], acc)


def _schema_property_names() -> set[str]:
    schema = json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))
    names: set[str] = set()
    _collect_property_names(schema, names)
    return names


def test_schema_enumeration_is_non_trivial():
    """Guard the guard: a broken collector must not silently pass everything."""
    names = _schema_property_names()
    assert len(names) > 40, f"Schema property collection looks broken: {sorted(names)}"
    # Spot-check one field per nesting level.
    assert {"effects", "prompt_type", "max_attempts", "min_iterations"} <= names


@pytest.mark.parametrize("prop", sorted(_schema_property_names()))
def test_schema_property_is_documented_in_rules(prop: str):
    """Every schema property name appears (word-boundary) in the assembled ruleset."""
    rules = load_all_rules(RULES_DIR)
    assert rules, f"No rules loaded from {RULES_DIR}"

    assert re.search(rf"\b{re.escape(prop)}\b", rules), (
        f"Schema property '{prop}' is not mentioned anywhere in rules/*.yml. "
        f"Add it to the relevant rule file — if it is runner/config policy rather "
        f"than author-facing, document it as such ('usually omitted')."
    )


def _rule_files() -> list[str]:
    return sorted(p.stem for p in RULES_DIR.glob("*.yml"))


@pytest.mark.parametrize("rule_name", _rule_files())
def test_rule_example_is_a_valid_orchestration(rule_name: str, tmp_path: Path):
    """Each rule file's example block validates as (part of) an orchestration.

    Effect-type examples are a one-element effects list and get wrapped in a
    minimal document; section examples (interface) are already document-shaped.
    """
    import yaml

    from circuitry.api import validate_orchestration

    rule = yaml.safe_load((RULES_DIR / f"{rule_name}.yml").read_text(encoding="utf-8"))
    example = rule.get("example")
    if example is None:
        pytest.skip(f"{rule_name}.yml has no example block")

    parsed = yaml.safe_load(example)
    doc: dict = {"adapter": "ollama", "model": "llama3"}
    if isinstance(parsed, list):
        doc["effects"] = parsed
    else:
        doc.update(parsed)

    doc_path = tmp_path / f"{rule_name}_example.yml"
    doc_path.write_text(yaml.safe_dump(doc, sort_keys=False), encoding="utf-8")

    result = validate_orchestration(orchestration_path=doc_path)
    assert result["ok"], f"{rule_name}.yml example failed validation: {result['errors']}"


def test_bundled_rules_match_repo_rules():
    """src/circuitry/bundled/rules/ is the packaged mirror of rules/ — keep it in sync.

    `cof gen` reads the bundled copy, so a rules/ edit that never reaches
    bundled/ ships an authoring guide nobody sees.
    """
    bundled_dir = PROJECT_ROOT / "src" / "circuitry" / "bundled" / "rules"
    repo_names = {p.name for p in RULES_DIR.glob("*.yml")}
    bundled_names = {p.name for p in bundled_dir.glob("*.yml")}

    assert repo_names == bundled_names, (
        f"rules/ and bundled/rules/ file sets differ: "
        f"only in rules/={repo_names - bundled_names}, "
        f"only in bundled/={bundled_names - repo_names}"
    )

    for name in sorted(repo_names):
        repo_text = (RULES_DIR / name).read_text(encoding="utf-8")
        bundled_text = (bundled_dir / name).read_text(encoding="utf-8")
        assert repo_text == bundled_text, (
            f"rules/{name} and bundled/rules/{name} differ — copy the updated file across."
        )


def test_use_and_interface_are_loaded():
    rules = load_all_rules(RULES_DIR)
    assert "type: use" in rules
    assert "section: interface" in rules


def test_load_rules_for_use():
    from circuitry.rules import load_rules_for

    result = load_rules_for("use", rules_dir=RULES_DIR)
    assert "section: common" in result
    assert "type: use" in result
    assert "\ntype: prompt\n" not in result
