"""The headless driving story must stay runnable and stay true.

`docs/wizard.md` documents `cof wizard` and the `drive_conversation` loop it
wraps, neither of which hard-code a state path or a source-tree path — that
coupling (a `src/circuitry/curation/...` path that only resolves from a
checkout) is exactly what this suite guards against. What is worth checking
against the wizard's own interface is the turn contract table.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

import yaml

DOC_PATH = Path("docs/wizard.md")
WIZARD_PATH = Path("src/circuitry/curation/agents/wizard.yml")


def _declared_paths() -> dict[str, str]:
    interface = yaml.safe_load(WIZARD_PATH.read_text(encoding="utf-8"))["interface"]
    return {name: spec["path"] for name, spec in interface["outputs"].items()}


def _python_blocks(markdown: str) -> list[str]:
    return re.findall(r"```python\n(.*?)```", markdown, flags=re.DOTALL)


# ── The doc example ──────────────────────────────────────────────────────────


def test_doc_example_is_valid_python() -> None:
    (example,) = _python_blocks(DOC_PATH.read_text(encoding="utf-8"))
    compile(example, "docs/wizard.md", "exec")


def test_doc_example_drives_the_shared_headless_loop() -> None:
    """The example calls `drive_conversation` rather than digging state paths
    itself — a second implementation of the turn contract is what the earlier
    version of this doc had, and it is what drifted."""
    (example,) = _python_blocks(DOC_PATH.read_text(encoding="utf-8"))
    assert "drive_conversation" in example
    assert "wizard_host" in example


def test_doc_contract_table_matches_the_interface() -> None:
    doc = DOC_PATH.read_text(encoding="utf-8")
    for output, path in _declared_paths().items():
        assert f"`{path}`" in doc, f"contract table is missing {output!r}"


def test_doc_never_hardcodes_the_source_tree_path() -> None:
    """The wizard is resolved through `wizard_path()` -> `resolve_bundled()`
    everywhere in this doc, which is what makes it work from an installed
    wheel; a literal `src/circuitry/curation/...` path would only work from a
    checkout, which is the bug this page used to teach by example."""
    doc = DOC_PATH.read_text(encoding="utf-8")
    assert str(WIZARD_PATH) not in doc


def test_doc_documents_cof_wizard_and_distinguishes_it_from_gen() -> None:
    doc = DOC_PATH.read_text(encoding="utf-8")
    assert "cof wizard" in doc
    assert "cof gen" in doc


def test_manifest_documents_the_same_contract() -> None:
    manifest = json.loads(
        Path("src/circuitry/curation/manifest.json").read_text(encoding="utf-8")
    )
    entry = next(e for e in manifest["entries"] if e["name"] == "agents/wizard")
    assert {k: v["path"] for k, v in entry["outputs"].items()} == _declared_paths()
