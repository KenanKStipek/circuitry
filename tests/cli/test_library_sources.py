"""Tests for the library source registry (`runtime.library.sources`).

Covers the zero-config regression guarantee, the folder source (with and
without a `manifest.json`), metadata derivation, source precedence, the
ambiguity warning, and source-qualified resolution.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.library_sources import (
    CurationSource,
    FolderSource,
    LibraryRegistry,
    LibrarySourceError,
)
from circuitry.cli.registry import load_index

runner = CliRunner()


# ── fixtures ─────────────────────────────────────────────────────────────────


PIPELINE_YML = """\
# My local pipeline — summarises a document.
#
# Longer prose that should not end up in the description.

interface:
  inputs:
    document:
      type: string
      required: true
      description: The document to summarise.

effects:
  - type: prompt
    name: summarise
    template: "Summarise this: {{document}}"
"""

NO_COMMENT_YML = """\
effects:
  - type: prompt
    name: greet
    template: |
      Greet the user warmly.
      This second line is not the description.
"""

NESTED_YML = """\
# A nested orchestration.

effects:
  - type: prompt
    name: think
    template: "Think about {{topic}}."
"""


def _write_folder(root: Path, *, manifest: bool) -> Path:
    root.mkdir(parents=True, exist_ok=True)
    (root / "my_pipeline.yml").write_text(PIPELINE_YML, encoding="utf-8")
    (root / "no_comment.yml").write_text(NO_COMMENT_YML, encoding="utf-8")
    nested = root / "nested"
    nested.mkdir(exist_ok=True)
    (nested / "deep.yml").write_text(NESTED_YML, encoding="utf-8")

    if manifest:
        (root / "manifest.json").write_text(
            json.dumps(
                {
                    "entries": [
                        {
                            "name": "my_pipeline",
                            "file": "my_pipeline.yml",
                            "category": "local",
                            "description": "Manifest-provided description.",
                            "backends": ["llm"],
                            "inputs": {
                                "document": {"type": "string", "required": True}
                            },
                        }
                    ]
                }
            ),
            encoding="utf-8",
        )
    return root


def _write_config(path: Path, sources: list[dict[str, Any]]) -> Path:
    path.write_text(
        json.dumps({"runtime": {"library": {"sources": sources}}}), encoding="utf-8"
    )
    return path


@pytest.fixture()
def folder_config(tmp_path: Path) -> Path:
    """Config with curation first, then a `local` folder source."""
    folder = _write_folder(tmp_path / "orchestrations", manifest=False)
    return _write_config(
        tmp_path / "circuitry.config.json",
        [{"type": "curation"}, {"type": "folder", "name": "local", "path": str(folder)}],
    )


# ── zero-config regression ───────────────────────────────────────────────────


def test_default_registry_is_curation_only() -> None:
    registry = LibraryRegistry.from_config(CircuitryConfig())
    assert registry.source_names == ["curation"]
    assert registry.is_multi_source is False


def test_absent_sources_key_defaults_to_curation() -> None:
    cfg = CircuitryConfig(runtime={"library": {"backend": "filesystem"}})
    assert LibraryRegistry.from_config(cfg).source_names == ["curation"]


def test_zero_config_list_json_is_unchanged() -> None:
    """`cof list --json` must be byte-identical to the pre-registry output."""
    result = runner.invoke(app, ["list", "--json"])
    assert result.exit_code == 0
    assert json.loads(result.output) == load_index()
    # No source key leaks into single-source output.
    assert all("source" not in entry for entry in json.loads(result.output))


def test_zero_config_list_has_no_source_column() -> None:
    result = runner.invoke(app, ["list"])
    assert result.exit_code == 0
    assert "Source" not in result.output


def test_zero_config_info_json_is_unchanged() -> None:
    result = runner.invoke(app, ["info", "learn/hello", "--json"])
    assert result.exit_code == 0
    data = json.loads(result.output)
    assert "source" not in data
    assert data["name"] == "learn/hello"


def test_zero_config_run_learn_hello() -> None:
    result = runner.invoke(app, ["run", "learn/hello", "--dry-run", "-e", "name=Test"])
    assert result.exit_code == 0


def test_curation_source_matches_load_index() -> None:
    entries = CurationSource().list_entries()
    assert [e.metadata for e in entries] == load_index()
    assert all(e.source == "curation" for e in entries)
    assert all(e.path is not None and e.path.exists() for e in entries)


# ── folder source: metadata derivation ───────────────────────────────────────


def test_folder_source_derives_description_from_leading_comment(tmp_path: Path) -> None:
    folder = _write_folder(tmp_path / "lib", manifest=False)
    entries = {e.name: e for e in FolderSource("local", folder).list_entries()}
    assert entries["my_pipeline"].metadata["description"] == (
        "My local pipeline — summarises a document."
    )


def test_folder_source_falls_back_to_first_prompt(tmp_path: Path) -> None:
    folder = _write_folder(tmp_path / "lib", manifest=False)
    entries = {e.name: e for e in FolderSource("local", folder).list_entries()}
    assert entries["no_comment"].metadata["description"] == "Greet the user warmly."


def test_folder_source_derives_inputs_from_interface(tmp_path: Path) -> None:
    folder = _write_folder(tmp_path / "lib", manifest=False)
    entries = {e.name: e for e in FolderSource("local", folder).list_entries()}
    inputs = entries["my_pipeline"].metadata["inputs"]
    assert inputs == [
        {
            "name": "document",
            "type": "string",
            "required": True,
            "description": "The document to summarise.",
        }
    ]


def test_folder_source_scans_recursively(tmp_path: Path) -> None:
    folder = _write_folder(tmp_path / "lib", manifest=False)
    entries = {e.name: e for e in FolderSource("local", folder).list_entries()}
    assert set(entries) == {"my_pipeline", "no_comment", "nested/deep"}
    assert entries["nested/deep"].category == "nested"


def test_folder_source_honours_manifest(tmp_path: Path) -> None:
    folder = _write_folder(tmp_path / "lib", manifest=True)
    entries = {e.name: e for e in FolderSource("local", folder).list_entries()}
    manifested = entries["my_pipeline"]
    assert manifested.metadata["description"] == "Manifest-provided description."
    assert manifested.metadata["backends"] == ["llm"]
    assert manifested.category == "local"
    # Files absent from the manifest still get derived metadata.
    assert entries["no_comment"].metadata["description"] == "Greet the user warmly."


def test_folder_source_resolves_by_bare_and_nested_name(tmp_path: Path) -> None:
    folder = _write_folder(tmp_path / "lib", manifest=False)
    source = FolderSource("local", folder)
    assert source.resolve("my_pipeline") == folder / "my_pipeline.yml"
    assert source.resolve("nested/deep") == folder / "nested" / "deep.yml"
    # Last-segment match for a nested entry.
    assert source.resolve("deep") == folder / "nested" / "deep.yml"
    assert source.resolve("does_not_exist") is None


def test_folder_source_missing_directory_is_empty(tmp_path: Path) -> None:
    source = FolderSource("local", tmp_path / "nope")
    assert source.list_entries() == []
    assert source.resolve("anything") is None


def test_folder_source_refresh_picks_up_new_files(tmp_path: Path) -> None:
    folder = _write_folder(tmp_path / "lib", manifest=False)
    source = FolderSource("local", folder)
    assert source.resolve("late_addition") is None
    (folder / "late_addition.yml").write_text(NO_COMMENT_YML, encoding="utf-8")
    source.refresh()
    assert source.resolve("late_addition") == folder / "late_addition.yml"


# ── precedence, ambiguity, qualified refs ────────────────────────────────────


def _dual_folder_registry(tmp_path: Path) -> tuple[LibraryRegistry, Path, Path]:
    alpha = tmp_path / "alpha"
    beta = tmp_path / "beta"
    alpha.mkdir()
    beta.mkdir()
    (alpha / "dup.yml").write_text("# Alpha copy.\n\n" + NO_COMMENT_YML, encoding="utf-8")
    (beta / "dup.yml").write_text("# Beta copy.\n\n" + NO_COMMENT_YML, encoding="utf-8")
    (beta / "beta_only.yml").write_text(NO_COMMENT_YML, encoding="utf-8")
    cfg = CircuitryConfig(
        runtime={
            "library": {
                "sources": [
                    {"type": "folder", "name": "alpha", "path": str(alpha)},
                    {"type": "folder", "name": "beta", "path": str(beta)},
                ]
            }
        }
    )
    return (LibraryRegistry.from_config(cfg), alpha, beta)


def test_precedence_first_source_wins(tmp_path: Path) -> None:
    registry, alpha, _ = _dual_folder_registry(tmp_path)
    resolution = registry.resolve("dup")
    assert resolution is not None
    assert resolution.entry.source == "alpha"
    assert resolution.path == alpha / "dup.yml"


def test_ambiguity_is_reported(tmp_path: Path) -> None:
    registry, _, _ = _dual_folder_registry(tmp_path)
    resolution = registry.resolve("dup")
    assert resolution is not None
    assert resolution.is_ambiguous
    assert resolution.ambiguous_sources == ["beta"]
    warning = resolution.ambiguity_warning("dup")
    assert "alpha:dup" in warning and "beta:dup" in warning


def test_unambiguous_name_has_no_warning(tmp_path: Path) -> None:
    registry, _, _ = _dual_folder_registry(tmp_path)
    resolution = registry.resolve("beta_only")
    assert resolution is not None
    assert not resolution.is_ambiguous


def test_source_qualified_ref_bypasses_precedence(tmp_path: Path) -> None:
    registry, _, beta = _dual_folder_registry(tmp_path)
    resolution = registry.resolve("beta:dup")
    assert resolution is not None
    assert resolution.entry.source == "beta"
    assert resolution.path == beta / "dup.yml"
    assert not resolution.is_ambiguous


def test_qualified_ref_for_unknown_source_is_not_split(tmp_path: Path) -> None:
    registry, _, _ = _dual_folder_registry(tmp_path)
    assert registry.split_ref("gamma:dup") == (None, "gamma:dup")
    assert registry.resolve("gamma:dup") is None
    # A colon that isn't a configured source name stays part of the name.
    assert registry.split_ref("C:/tmp/x.yml") == (None, "C:/tmp/x.yml")


def test_list_entries_filtered_by_source(tmp_path: Path) -> None:
    registry, _, _ = _dual_folder_registry(tmp_path)
    names = [e.name for e in registry.list_entries(source="beta")]
    assert sorted(names) == ["beta_only", "dup"]


# ── config validation ────────────────────────────────────────────────────────


def test_unknown_source_type_is_rejected() -> None:
    cfg = CircuitryConfig(runtime={"library": {"sources": [{"type": "github"}]}})
    with pytest.raises(LibrarySourceError, match="Unknown library source type"):
        LibraryRegistry.from_config(cfg)


def test_folder_source_requires_path() -> None:
    cfg = CircuitryConfig(runtime={"library": {"sources": [{"type": "folder"}]}})
    with pytest.raises(LibrarySourceError, match="requires a 'path'"):
        LibraryRegistry.from_config(cfg)


def test_source_requires_type() -> None:
    cfg = CircuitryConfig(runtime={"library": {"sources": [{"name": "x"}]}})
    with pytest.raises(LibrarySourceError, match="missing required field 'type'"):
        LibraryRegistry.from_config(cfg)


def test_empty_sources_list_is_rejected() -> None:
    cfg = CircuitryConfig(runtime={"library": {"sources": []}})
    with pytest.raises(LibrarySourceError, match="non-empty list"):
        LibraryRegistry.from_config(cfg)


def test_folder_source_name_defaults_to_directory_name(tmp_path: Path) -> None:
    folder = _write_folder(tmp_path / "my_lib", manifest=False)
    cfg = CircuitryConfig(
        runtime={"library": {"sources": [{"type": "folder", "path": str(folder)}]}}
    )
    assert LibraryRegistry.from_config(cfg).source_names == ["my_lib"]


# ── CLI aggregation ──────────────────────────────────────────────────────────


def test_cli_list_aggregates_sources(folder_config: Path) -> None:
    result = runner.invoke(app, ["list", "--json", "-c", str(folder_config)])
    assert result.exit_code == 0
    data = json.loads(result.output)
    sources = {entry["source"] for entry in data}
    assert sources == {"curation", "local"}
    assert "my_pipeline" in [e["name"] for e in data]


def test_cli_list_shows_source_column_when_multi_source(folder_config: Path) -> None:
    result = runner.invoke(app, ["list", "-c", str(folder_config)])
    assert result.exit_code == 0
    assert "Source" in result.output


def test_cli_list_source_filter(folder_config: Path) -> None:
    result = runner.invoke(
        app, ["list", "--json", "--source", "local", "-c", str(folder_config)]
    )
    assert result.exit_code == 0
    data = json.loads(result.output)
    assert {e["source"] for e in data} == {"local"}
    assert sorted(e["name"] for e in data) == ["my_pipeline", "nested/deep", "no_comment"]


def test_cli_list_unknown_source(folder_config: Path) -> None:
    result = runner.invoke(app, ["list", "--source", "nope", "-c", str(folder_config)])
    assert result.exit_code == 1
    assert "Unknown library source" in result.output


def test_cli_info_folder_entry(folder_config: Path) -> None:
    result = runner.invoke(
        app, ["info", "local:my_pipeline", "--json", "-c", str(folder_config)]
    )
    assert result.exit_code == 0
    data = json.loads(result.output)
    assert data["source"] == "local"
    assert data["description"] == "My local pipeline — summarises a document."


def test_cli_run_folder_entry(folder_config: Path) -> None:
    result = runner.invoke(
        app,
        [
            "run",
            "local:my_pipeline",
            "--dry-run",
            "-e",
            "document=hello",
            "-c",
            str(folder_config),
        ],
    )
    assert result.exit_code == 0


def test_cli_run_bare_folder_name(folder_config: Path) -> None:
    result = runner.invoke(
        app,
        ["run", "my_pipeline", "--dry-run", "-e", "document=hello", "-c", str(folder_config)],
    )
    assert result.exit_code == 0


def test_cli_eject_folder_entry(folder_config: Path, tmp_path: Path) -> None:
    dest = tmp_path / "ejected.yml"
    result = runner.invoke(
        app,
        ["eject", "local:my_pipeline", "--out", str(dest), "-c", str(folder_config)],
    )
    assert result.exit_code == 0
    assert "effects:" in dest.read_text(encoding="utf-8")


def test_cli_warns_on_ambiguous_bare_name(tmp_path: Path) -> None:
    alpha = tmp_path / "alpha"
    beta = tmp_path / "beta"
    alpha.mkdir()
    beta.mkdir()
    (alpha / "dup.yml").write_text(NO_COMMENT_YML, encoding="utf-8")
    (beta / "dup.yml").write_text(NO_COMMENT_YML, encoding="utf-8")
    cfg = _write_config(
        tmp_path / "config.json",
        [
            {"type": "folder", "name": "alpha", "path": str(alpha)},
            {"type": "folder", "name": "beta", "path": str(beta)},
        ],
    )

    result = runner.invoke(app, ["info", "dup", "-c", str(cfg)])
    assert result.exit_code == 0
    assert "matched multiple sources" in result.output.replace("\n", " ")


def test_cli_malformed_sources_is_a_clean_error(tmp_path: Path) -> None:
    cfg = _write_config(tmp_path / "config.json", [{"type": "nope"}])
    result = runner.invoke(app, ["list", "-c", str(cfg)])
    assert result.exit_code == 1
    assert "Unknown library source type" in result.output.replace("\n", " ")
