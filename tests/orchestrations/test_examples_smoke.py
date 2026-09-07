from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.api import (
    inspect_orchestration,
    run_orchestration,
    validate_orchestration,
)
from circuitry.cli.config import CircuitryConfig

EXAMPLES_DIR = Path("src/circuitry/curation")
MANIFEST_PATH = EXAMPLES_DIR / "manifest.json"


def _curated_files() -> list[Path]:
    """All YAML files anywhere under the curation tree, sorted for stable order."""
    return sorted(p for p in EXAMPLES_DIR.rglob("*.yml") if p.is_file())


CURATED_EXAMPLES = [str(p.relative_to(EXAMPLES_DIR)) for p in _curated_files()]


def _load_doc(rel: str) -> dict[str, Any]:
    return yaml.safe_load((EXAMPLES_DIR / rel).read_text(encoding="utf-8"))


def _declared_inputs(rel: str) -> dict[str, Any]:
    interface = _load_doc(rel).get("interface")
    if not isinstance(interface, dict):
        return {}
    inputs = interface.get("inputs")
    return inputs if isinstance(inputs, dict) else {}


#: Files whose interface declares at least one input — the population this
#: regression covers (issue #192: bare {{name}} silently rendering empty
#: against real `-e KEY=VALUE`/`use.inputs` caller input).
EXAMPLES_WITH_INPUTS = [rel for rel in CURATED_EXAMPLES if _declared_inputs(rel)]

#: Only string/number/boolean inputs are checked for non-empty render: they
#: are always interpolated directly, so a correct reference always leaves a
#: recognizable trace in the rendered text. array/object inputs are typically
#: consumed via Mustache section iteration over caller-shaped fields we can't
#: safely synthesize (e.g. utilities/route.yml's `{{#input.routes}} -
#: {{key}}: {{description}}{{/input.routes}}`), so asserting on their render
#: would produce false failures unrelated to this bug class.
_SCALAR_TYPES = {"string", "number", "boolean"}


def _synthetic_input_namespace(declared: dict[str, Any]) -> dict[str, Any]:
    """Build an `input` namespace with a unique, greppable value per input."""
    namespace: dict[str, Any] = {}
    for name, spec in declared.items():
        if not isinstance(spec, dict):
            continue
        marker = f"__INPUT_{name.upper()}_MARKER__"
        input_type = spec.get("type", "string")
        if input_type == "array":
            namespace[name] = [marker]
        elif input_type == "object":
            namespace[name] = {"_marker": marker}
        elif input_type == "number":
            namespace[name] = 123456
        elif input_type == "boolean":
            namespace[name] = True
        else:
            namespace[name] = marker
    return namespace


def _all_strings(node: Any) -> list[str]:
    if isinstance(node, str):
        return [node]
    if isinstance(node, dict):
        return [s for v in node.values() for s in _all_strings(v)]
    if isinstance(node, list):
        return [s for v in node for s in _all_strings(v)]
    return []


def _references_input_name(text: str, name: str) -> bool:
    """Whether *text* contains a Mustache tag naming *name*, bare or namespaced.

    Deliberately matches both the correct ``{{input.name}}`` spelling and the
    buggy bare ``{{name}}`` spelling — the whole point of this regression is
    to catch the bare form, so "is this input referenced at all" must not
    already assume the fix is in place.
    """
    pattern = re.compile(
        r"\{\{\{?\s*[#^/&]?\s*(?:input\.)?" + re.escape(name) + r"\b"
    )
    return pattern.search(text) is not None


@pytest.mark.parametrize("rel", EXAMPLES_WITH_INPUTS)
def test_examples_render_declared_inputs(rel: str) -> None:
    """Every declared scalar input, once supplied, must show up in a render.

    Regression for issue #192: a template that reads a declared input via
    the bare pre-namespace spelling (``{{name}}`` instead of
    ``{{input.name}}``) compiles fine but silently renders empty against
    real ``-e KEY=VALUE``/``use.inputs`` input, because caller input only
    ever lands under the ``input`` namespace.
    """
    import chevron

    doc = _load_doc(rel)
    declared = _declared_inputs(rel)
    synthetic_input = _synthetic_input_namespace(declared)
    state = {"input": synthetic_input}
    effects = doc.get("effects") or doc.get("steps") or []
    all_text = "\n".join(_all_strings(effects))

    for name, spec in declared.items():
        if not isinstance(spec, dict) or spec.get("type", "string") not in _SCALAR_TYPES:
            continue
        if not _references_input_name(all_text, name):
            continue  # declared but unused by this document's effects
        marker = str(synthetic_input[name])
        rendered = "\n".join(
            chevron.render(text, state) for text in _all_strings(effects) if "{{" in text
        )
        assert marker in rendered, (
            f"{rel}: declared input '{name}' is referenced as input.{name} "
            f"but rendered empty against a real 'input' namespace — check "
            f"for a stray bare '{{{{{name}}}}}' reference."
        )


def test_example_manifest_covers_curated_set() -> None:
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    listed = {entry["file"] for entry in manifest["entries"]}
    on_disk = set(CURATED_EXAMPLES)
    assert listed == on_disk, (
        f"Manifest/disk mismatch.\n"
        f"  in manifest only: {listed - on_disk}\n"
        f"  on disk only: {on_disk - listed}"
    )


@pytest.mark.parametrize("rel", CURATED_EXAMPLES)
def test_examples_validate(rel: str) -> None:
    result = validate_orchestration(orchestration_path=EXAMPLES_DIR / rel)
    assert result["ok"] is True, result.get("errors")
    assert result["errors"] == []


@pytest.mark.parametrize("rel", CURATED_EXAMPLES)
def test_examples_inspect(rel: str) -> None:
    summary = inspect_orchestration(orchestration_path=EXAMPLES_DIR / rel)
    assert summary["effects_count"] >= 1
    assert isinstance(summary["effect_names"], list)


@pytest.mark.parametrize("rel", CURATED_EXAMPLES)
def test_examples_dry_run_smoke(rel: str) -> None:
    result = run_orchestration(
        orchestration_path=EXAMPLES_DIR / rel,
        state={},
        dry_run=True,
        config=CircuitryConfig(default_adapter="ollama", default_model="phi3:mini"),
    )
    assert result.ok is True
    assert "runtime" in result.state
    assert "prime" in result.state
    assert result.state["runtime"]["last_run"]["completed_at"] is not None
