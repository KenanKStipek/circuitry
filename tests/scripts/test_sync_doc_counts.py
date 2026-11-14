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


def test_electricity_tool_names_reads_only_between_markers(tmp_path: Path) -> None:
    rs = tmp_path / "registry.rs"
    rs.write_text(
        "fn builders() {\n"
        "    // doc-counts:electricity-tools:start\n"
        '    map.insert("json", f);\n'
        '    map.insert("shell", f);\n'
        "    // doc-counts:electricity-tools:end\n"
        '    map.insert("sleep", f); // test-only, outside the markers\n'
        "}\n",
        encoding="utf-8",
    )
    original = sync_doc_counts.ELECTRICITY_REGISTRY_RS
    sync_doc_counts.ELECTRICITY_REGISTRY_RS = rs
    try:
        assert sync_doc_counts._electricity_tool_names() == ["json", "shell"]
    finally:
        sync_doc_counts.ELECTRICITY_REGISTRY_RS = original


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
