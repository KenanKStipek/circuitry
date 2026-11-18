"""Every YAML example in docs/guidebook/ is executable doctrine.

The guidebook's promise (docs/guidebook/README.md, "How to read the examples")
is that positive examples validate and anti-patterns provably do not. This
test holds the prose to it:

* a ```yaml block whose first line starts with ``# ✗`` (and is not
  ``# ✗ runtime``) must FAIL validation;
* a block whose first line starts with ``# ⚠`` must validate WITH warnings;
* a block whose first line starts with ``# ✗ runtime`` documents a shape the
  validator cannot catch — it must still validate cleanly, since the whole
  point is that it goes green;
* every other block that is an orchestration (has a top-level ``effects:``)
  or an effect fragment (starts with ``- type:``) must validate with no
  errors.

Blocks that are neither — profiles, config snippets, output declarations,
interface-only fragments — are skipped. A fragment is wrapped in ``effects:``
before validation; templates may reference effects that live elsewhere in the
chapter, which the validator does not cross-check, so a fragment validates on
its own structure.
"""

from __future__ import annotations

import re
import textwrap
from pathlib import Path

import pytest
import yaml

from circuitry import validate_orchestration

GUIDEBOOK = Path("docs/guidebook")
FENCE = re.compile(r"^```yaml[^\n]*\n(.*?)^```", re.MULTILINE | re.DOTALL)


def _blocks() -> list[tuple[str, int, str]]:
    found: list[tuple[str, int, str]] = []
    for path in sorted(GUIDEBOOK.glob("*.md")):
        text = path.read_text(encoding="utf-8")
        for match in FENCE.finditer(text):
            line = text[: match.start()].count("\n") + 2
            found.append((path.name, line, match.group(1)))
    return found


def _body(block: str) -> str:
    """The block without its leading marker/comment lines, dedented."""
    lines = textwrap.dedent(block).strip().splitlines()
    while lines and lines[0].lstrip().startswith("#"):
        lines.pop(0)
    return "\n".join(lines).strip()


def _classify(block: str) -> str | None:
    first = block.lstrip().splitlines()[0] if block.strip() else ""
    if first.startswith("# ✗ runtime"):
        return "runtime"
    if first.startswith("# ✗"):
        return "reject"
    if first.startswith("# ⚠"):
        return "warn"
    body = _body(block)
    if body.startswith("- type:"):
        return "fragment"
    try:
        doc = yaml.safe_load(body)
    except yaml.YAMLError:
        return None
    if isinstance(doc, dict) and isinstance(doc.get("effects"), list):
        return "orchestration"
    return None  # a profile, a config snippet, an outputs/interface fragment …


def _as_document(block: str) -> str:
    body = _body(block)
    if body.startswith("- type:"):
        return "effects:\n" + textwrap.indent(body, "  ")
    return body


def _stub_use_path_children(document_text: str, tmp_path: Path) -> None:
    """Write a trivially valid stand-in at every `use: path:`/legacy
    `orchestration:` reference *document_text* names, when nothing is
    there already.

    Each fenced example validates alone, in its own empty `tmp_path`
    (#832): a `path:` naming another block of the same chapter's own
    prose -- a real file in the project this guidebook describes, never
    one in the sandbox -- would otherwise fail `cof check`'s own
    compile of that child for a reason this test was never about: that
    the file does not exist here, rather than anything the example
    itself gets wrong.
    """
    try:
        parsed = yaml.safe_load(document_text)
    except yaml.YAMLError:
        return
    if not isinstance(parsed, dict):
        return
    from circuitry.core.cycle_check import collect_use_refs

    for kind, value in collect_use_refs(parsed):
        if kind != "path":
            continue
        candidate = tmp_path / value
        if candidate.exists():
            continue
        candidate.parent.mkdir(parents=True, exist_ok=True)
        candidate.write_text("effects: []\n", encoding="utf-8")


def _validate(block: str, tmp_path: Path) -> dict:
    document = _as_document(block)
    _stub_use_path_children(document, tmp_path)
    target = tmp_path / "example.yml"
    target.write_text(document, encoding="utf-8")
    return validate_orchestration(orchestration_path=target)


_CASES = [(name, line, block) for name, line, block in _blocks() if _classify(block)]


@pytest.mark.parametrize(
    ("name", "line", "block"),
    _CASES,
    ids=[f"{name}:{line}" for name, line, _ in _CASES],
)
def test_guidebook_example(name: str, line: int, block: str, tmp_path: Path) -> None:
    kind = _classify(block)
    report = _validate(block, tmp_path)
    where = f"{name}:{line}"

    if kind == "reject":
        assert not report["ok"], f"{where}: marked ✗ but validated cleanly"
    elif kind == "warn":
        assert report["ok"], f"{where}: marked ⚠ but failed: {report['errors']}"
        assert report["warnings"], f"{where}: marked ⚠ but produced no warning"
    else:  # runtime, fragment, orchestration
        assert report["ok"], f"{where}: {report['errors']}"


def test_guidebook_has_examples() -> None:
    assert len(_CASES) > 20, "the guidebook lost its examples"
