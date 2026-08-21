"""Loader for structured rule files in the rules/ directory.

Rule files are YAML documents designed for LLM consumption — compact,
structured descriptions of each Circuitry effect type. The loader reads
them as plain text (not parsed YAML) and concatenates them for injection
into orchestration templates via Mustache state variables.

Usage:
    from circuitry.rules import load_all_rules, load_rules_for

    # Full ruleset (replaces old _load_rules() markdown extraction)
    rules = load_all_rules(Path("rules"))

    # Per-type: common + specific type(s)
    rules_prompt = load_rules_for("prompt", rules_dir=Path("rules"))
    rules_loop = load_rules_for("loop", rules_dir=Path("rules"))
"""

from __future__ import annotations

from pathlib import Path

# Files loaded by load_all_rules(), in order.
# common and patterns bookend the per-type files.
_ALL_FILES = [
    "common",
    "prompt",
    "dynamic",
    "loop",
    "conditional",
    "tool",
    "reflector",
    "use",
    "interface",
    "patterns",
]

# Per-type files (excludes common, interface, and patterns which are section files).
EFFECT_TYPES = ["prompt", "dynamic", "loop", "conditional", "tool", "reflector", "use"]


def load_rule_file(rules_dir: Path, name: str) -> str:
    """Load a single rule file as text. Returns empty string if not found."""
    path = rules_dir / f"{name}.yml"
    if not path.is_file():
        return ""
    return path.read_text(encoding="utf-8").strip()


def load_rules_for(*effect_types: str, rules_dir: Path) -> str:
    """Load common.yml + specific type files, concatenated with --- separators.

    Always includes common.yml first. Does not include patterns.yml
    (design patterns are separate context, not per-type constraints).
    """
    parts: list[str] = []

    common = load_rule_file(rules_dir, "common")
    if common:
        parts.append(common)

    for etype in effect_types:
        content = load_rule_file(rules_dir, etype)
        if content:
            parts.append(content)

    return "\n\n---\n\n".join(parts)


def load_all_rules(rules_dir: Path) -> str:
    """Load all rule files in canonical order, concatenated with --- separators.

    Replaces the old _load_rules() that extracted markdown from
    docs/orchestration-reference.md.
    """
    if not rules_dir.is_dir():
        return ""

    parts: list[str] = []
    for name in _ALL_FILES:
        content = load_rule_file(rules_dir, name)
        if content:
            parts.append(content)

    return "\n\n---\n\n".join(parts)
