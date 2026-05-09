"""Tests that the curation manifest matches what's on disk and conforms to its schema."""

from __future__ import annotations

import json
from pathlib import Path

import jsonschema
import pytest

CURATION_DIR = Path("src/circuitry/curation")
MANIFEST_PATH = CURATION_DIR / "manifest.json"
SCHEMA_PATH = Path("src/circuitry/schema/curation-manifest.schema.json")


@pytest.fixture(scope="module")
def manifest() -> dict:
    return json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))


@pytest.fixture(scope="module")
def schema() -> dict:
    return json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))


# ── AC 14: Manifest schema enforced ─────────────────────────────────────────


def test_manifest_validates_against_schema(manifest: dict, schema: dict) -> None:
    validator = jsonschema.Draft7Validator(schema)
    errors = sorted(validator.iter_errors(manifest), key=str)
    assert not errors, "\n".join(e.message for e in errors)


def test_every_yaml_has_a_manifest_entry(manifest: dict) -> None:
    """No orphan YAML files on disk (every YAML must be listed in manifest)."""
    on_disk = {
        str(p.relative_to(CURATION_DIR))
        for p in CURATION_DIR.rglob("*.yml")
        if p.is_file()
    }
    listed = {entry["file"] for entry in manifest["entries"]}
    orphans = on_disk - listed
    assert not orphans, f"YAML files on disk but missing from manifest: {sorted(orphans)}"


def test_every_manifest_entry_resolves_to_a_file(manifest: dict) -> None:
    """No stale manifest entries (every listed file must exist)."""
    missing = []
    for entry in manifest["entries"]:
        path = CURATION_DIR / entry["file"]
        if not path.exists():
            missing.append(entry["file"])
    assert not missing, f"Manifest entries with no file on disk: {missing}"


def test_manifest_entry_categories_match_subdirs(manifest: dict) -> None:
    """The category field must match the subdirectory the file lives in."""
    for entry in manifest["entries"]:
        first_segment = entry["file"].split("/", 1)[0]
        assert entry["category"] == first_segment, (
            f"Entry {entry['name']} has category={entry['category']!r} "
            f"but file is in subdir {first_segment!r}"
        )


def test_manifest_entry_name_matches_file_stem(manifest: dict) -> None:
    """The slash-name should be `<category>/<stem>`."""
    for entry in manifest["entries"]:
        file_path = Path(entry["file"])
        expected = f"{file_path.parent.as_posix()}/{file_path.stem}"
        assert entry["name"] == expected, (
            f"Entry name {entry['name']!r} doesn't match file path {entry['file']!r} "
            f"(expected {expected!r})"
        )
