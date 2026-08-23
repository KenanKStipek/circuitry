"""The headless driving story must stay runnable and stay true.

`docs/wizard.md` documents `cof wizard` and the `drive_conversation` loop it
wraps, neither of which hard-code a state path or a source-tree path — that
coupling (a `src/circuitry/curation/...` path that only resolves from a
checkout) was exactly what the old, since-replaced `scripts/wizard-chat`
implementation got wrong. What is still worth checking against the wizard's
own interface is the turn contract table; what is worth checking about the
script is that it now delegates rather than reimplementing the loop.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path

import yaml

#: Rich styles each dash of an option separately (bold/dim spans mid-token),
#: which splits a literal "--goal" across escape codes; stripping ANSI SGR
#: sequences before asserting is what makes a plain substring check reliable.
_ANSI = re.compile(r"\x1b\[[0-9;]*m")
_NO_COLOR_ENV = {**os.environ, "NO_COLOR": "1"}


def _plain(text: str) -> str:
    return _ANSI.sub("", text)

DOC_PATH = Path("docs/wizard.md")
SCRIPT_PATH = Path("scripts/wizard-chat")
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


# ── The wrapper script ───────────────────────────────────────────────────────


def test_wizard_chat_is_a_thin_wrapper_over_cof_wizard() -> None:
    """`scripts/wizard-chat` only ever worked from a checkout. It now forwards
    to `cof wizard` instead of reimplementing the loop, the state-path digging,
    or the source-tree resolution a second time."""
    source = SCRIPT_PATH.read_text(encoding="utf-8")
    assert "circuitry.cli.app" in source
    assert str(WIZARD_PATH) not in source
    assert "prime.turn.decide" not in source


def test_wizard_chat_delegates_argument_handling_to_cof_wizard() -> None:
    """Run the script for real, with a bad flag `cof wizard` alone would
    reject — proof this is dispatch, not a parallel argument parser."""
    result = subprocess.run(
        [sys.executable, str(SCRIPT_PATH), "--not-a-real-flag"],
        capture_output=True,
        text=True,
        env=_NO_COLOR_ENV,
        check=False,
    )
    assert result.returncode != 0
    assert "--not-a-real-flag" in _plain(result.stdout + result.stderr)


def test_wizard_chat_help_is_cof_wizards_help() -> None:
    result = subprocess.run(
        [sys.executable, str(SCRIPT_PATH), "--help"],
        capture_output=True,
        text=True,
        check=True,
        env=_NO_COLOR_ENV,
    )
    plain = _plain(result.stdout)
    assert "--goal" in plain
    assert "--reply" in plain
