"""Tests for ``scripts/sync-doc-counts.py`` — the doc-counts CI gate (#777)."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from types import ModuleType

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT_PATH = REPO_ROOT / "scripts" / "sync-doc-counts.py"


def _load(path: Path, name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


sync_doc_counts = _load(SCRIPT_PATH, "sync_doc_counts")


def test_format_list_joins_with_and() -> None:
    assert sync_doc_counts._format_list(["a"]) == "`a`"
    assert sync_doc_counts._format_list(["a", "b"]) == "`a` and `b`"
    assert sync_doc_counts._format_list(["a", "b", "c"]) == "`a`, `b` and `c`"


def test_electricity_tool_names_reads_only_flags_true_in_current(tmp_path: Path) -> None:
    rs = tmp_path / "refusal.rs"
    rs.write_text(
        'const TOOL_REGEX_PROVIDERS: &[&str] = &["regex"];\n'
        'const TOOL_HTTP_PROVIDERS: &[&str] = &["http"];\n'
        "impl<'a> Supported<'a> {\n"
        "    pub fn current(providers: &'a [&'a str]) -> Self {\n"
        "        Supported {\n"
        "            tool_regex: true,\n"
        "            tool_http: false,\n"
        "            providers,\n"
        "        }\n"
        "    }\n"
        "\n"
        "    fn allows_provider(&self, name: &str) -> bool {\n"
        '        name == "json"\n'
        "            || (self.tool_regex && TOOL_REGEX_PROVIDERS.contains(&name))\n"
        "            || (self.tool_http && TOOL_HTTP_PROVIDERS.contains(&name))\n"
        "    }\n"
        "}\n",
        encoding="utf-8",
    )
    original = sync_doc_counts.ELECTRICITY_REFUSAL_RS
    sync_doc_counts.ELECTRICITY_REFUSAL_RS = rs
    try:
        assert sync_doc_counts._electricity_tool_names() == ["json", "regex"]
    finally:
        sync_doc_counts.ELECTRICITY_REFUSAL_RS = original


def test_rewrite_only_touches_recognized_markers() -> None:
    text = (
        "before\n"
        "<!-- doc-counts:start tool-plugin-count -->99<!-- doc-counts:end tool-plugin-count -->\n"
        "after, untouched\n"
    )
    updated = sync_doc_counts._rewrite(text, {"tool-plugin-count": "76"})
    assert "76" in updated
    assert "99" not in updated
    assert "after, untouched" in updated


def test_rewrite_rejects_unknown_marker_name() -> None:
    text = "<!-- doc-counts:start mystery -->1<!-- doc-counts:end mystery -->"
    with pytest.raises(KeyError):
        sync_doc_counts._rewrite(text, {})


def test_check_fails_when_a_doc_falls_out_of_sync(tmp_path: Path) -> None:
    doc = tmp_path / "README.md"
    doc.write_text(
        "<!-- doc-counts:start tool-plugin-count -->1<!-- doc-counts:end tool-plugin-count -->\n",
        encoding="utf-8",
    )
    original_docs = sync_doc_counts.DOC_FILES
    sync_doc_counts.DOC_FILES = [doc]
    try:
        assert sync_doc_counts.main(["--check"]) == 1
        assert sync_doc_counts.main([]) == 0
        assert sync_doc_counts.main(["--check"]) == 0
    finally:
        sync_doc_counts.DOC_FILES = original_docs


def test_repo_docs_are_already_in_sync() -> None:
    assert sync_doc_counts.main(["--check"]) == 0


def test_no_marker_starts_a_line() -> None:
    """A line beginning `<!--` opens a CommonMark HTML block that can
    interrupt the surrounding paragraph or list item (GitHub renders it
    that way), so every marker must sit mid-line."""
    for doc in sync_doc_counts.DOC_FILES:
        for lineno, line in enumerate(doc.read_text(encoding="utf-8").splitlines(), start=1):
            assert not line.lstrip().startswith("<!-- doc-counts:"), (
                f"{doc}:{lineno} starts with a doc-counts marker"
            )
