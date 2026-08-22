"""Tests for curation library registry and name resolution."""

from __future__ import annotations

from circuitry.cli.registry import load_index, resolve_bundled


def test_load_index_returns_entries() -> None:
    entries = load_index()
    assert isinstance(entries, list)
    assert len(entries) > 0


def test_load_index_entries_have_required_fields() -> None:
    entries = load_index()
    for entry in entries:
        assert "name" in entry, f"Entry missing 'name': {entry}"
        assert "file" in entry, f"Entry missing 'file': {entry}"
        assert "description" in entry, f"Entry missing 'description': {entry}"
        assert "category" in entry, f"Entry missing 'category': {entry}"
        assert "backends" in entry, f"Entry missing 'backends': {entry}"


def test_load_index_files_exist() -> None:
    """Every file referenced in the index must exist on disk."""
    entries = load_index()
    for entry in entries:
        resolved = resolve_bundled(entry["name"])
        assert resolved is not None, f"Missing file for {entry['name']}: {entry['file']}"
        assert resolved.exists(), f"File does not exist: {resolved}"


def test_resolve_bundled_by_slash_name() -> None:
    path = resolve_bundled("learn/hello")
    assert path is not None
    assert path.name == "hello.yml"
    assert path.exists()


def test_resolve_bundled_by_hyphenated_name() -> None:
    path = resolve_bundled("recipes/article-summarizer")
    assert path is not None
    assert path.name == "article_summarizer.yml"
    assert path.exists()


def test_resolve_bundled_by_underscore_name() -> None:
    path = resolve_bundled("recipes/article_summarizer")
    assert path is not None
    assert path.name == "article_summarizer.yml"


def test_resolve_bundled_by_last_segment() -> None:
    """Bare names match by last segment of slash-delimited entries."""
    path = resolve_bundled("article_summarizer")
    assert path is not None
    assert path.name == "article_summarizer.yml"


def test_resolve_bundled_nonexistent_returns_none() -> None:
    assert resolve_bundled("nonexistent-orchestration") is None


def test_resolve_bundled_learn_prompt() -> None:
    path = resolve_bundled("learn/prompt")
    assert path is not None
    assert path.name == "prompt.yml"
    assert path.exists()
