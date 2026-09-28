"""The README and the guidebook share one running example: an issue-to-fix agent.

These tests hold the pieces that span files together: the README's first
orchestration and the chapter 14 capstone validate as the files a reader would
save, the book build lists every chapter, and every relative link and anchor in
the README, the docs index and the guidebook resolves (chapter 14 was renamed,
and a stale link fails silently on GitHub).
"""

from __future__ import annotations

import importlib.util
import re
from pathlib import Path

import pytest

from circuitry import validate_orchestration

README = Path("README.md")
GUIDEBOOK = Path("docs/guidebook")
CAPSTONE = GUIDEBOOK / "14-issue-to-pull-request.md"
FENCE = re.compile(r"^```yaml[^\n]*\n(.*?)^```", re.MULTILINE | re.DOTALL)
ANY_FENCE = re.compile(r"^```.*?^```", re.MULTILINE | re.DOTALL)
LINK = re.compile(r"\]\(([^)\s]+)\)")
HEADING = re.compile(r"^#{1,6}\s+(.+?)\s*$", re.MULTILINE)


def _yaml_blocks(path: Path) -> list[str]:
    return [m.group(1) for m in FENCE.finditer(path.read_text(encoding="utf-8"))]


def _named_block(path: Path, filename: str) -> str:
    """The yaml block whose first line is ``# <filename>`` (optionally followed by a comment)."""
    for block in _yaml_blocks(path):
        first = block.splitlines()[0] if block.strip() else ""
        if re.match(rf"#\s*{re.escape(filename)}(\s|$)", first):
            return block
    raise AssertionError(f"{path}: no yaml block starts with '# {filename}'")


def test_readme_first_orchestration_is_triage_and_validates(tmp_path: Path) -> None:
    target = tmp_path / "triage.yml"
    target.write_text(_named_block(README, "triage.yml"), encoding="utf-8")
    report = validate_orchestration(orchestration_path=target)
    assert report["ok"], report["errors"]
    assert "cof run triage.yml" in README.read_text(encoding="utf-8")


def test_capstone_documents_validate_side_by_side(tmp_path: Path) -> None:
    """issue_to_pr.yml calls run_tests.yml by path; saved together, both validate."""
    for name in ("run_tests.yml", "issue_to_pr.yml"):
        (tmp_path / name).write_text(_named_block(CAPSTONE, name), encoding="utf-8")
    for name in ("run_tests.yml", "issue_to_pr.yml"):
        report = validate_orchestration(orchestration_path=tmp_path / name)
        assert report["ok"], (name, report["errors"])


def _build_script_parts() -> list[str]:
    spec = importlib.util.spec_from_file_location(
        "build_guidebook", Path("scripts/build-guidebook.py")
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return [name for _, chapters in module.PARTS for name in chapters]


def test_book_build_lists_every_chapter_in_order() -> None:
    chapters = sorted(p.name for p in GUIDEBOOK.glob("[0-9][0-9]-*.md"))
    assert _build_script_parts() == chapters


def _slug(text: str) -> str:
    """GitHub's heading anchor: lower-case, punctuation dropped, spaces to hyphens."""
    text = re.sub(r"[^\w\- ]", "", text.strip().lower())
    return re.sub(r" ", "-", text)


def _anchors(path: Path) -> set[str]:
    text = ANY_FENCE.sub("", path.read_text(encoding="utf-8"))
    seen: dict[str, int] = {}
    anchors: set[str] = set()
    for heading in HEADING.findall(text):
        slug = _slug(heading)
        count = seen.get(slug, 0)
        seen[slug] = count + 1
        anchors.add(slug if count == 0 else f"{slug}-{count}")
    return anchors


_LINKED_DOCS = [README, Path("docs/index.md"), *sorted(GUIDEBOOK.glob("*.md"))]


@pytest.mark.parametrize("doc", _LINKED_DOCS, ids=[str(p) for p in _LINKED_DOCS])
def test_relative_links_and_anchors_resolve(doc: Path) -> None:
    text = ANY_FENCE.sub("", doc.read_text(encoding="utf-8"))
    broken: list[str] = []
    for target in LINK.findall(text):
        if re.match(r"[a-z][a-z0-9+.-]*:", target):  # http:, https:, mailto:
            continue
        file_part, _, anchor = target.partition("#")
        resolved = (doc.parent / file_part).resolve() if file_part else doc.resolve()
        if not resolved.exists():
            broken.append(f"{target} (no such file)")
            continue
        if anchor and resolved.suffix == ".md" and anchor not in _anchors(resolved):
            broken.append(f"{target} (no such heading)")
    assert not broken, f"{doc}: {broken}"
