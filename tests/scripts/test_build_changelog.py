"""Tests for ``scripts/build-changelog.py`` — the fragment compiler."""

from __future__ import annotations

import importlib.util
import shutil
import subprocess
import sys
from pathlib import Path
from types import ModuleType

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT_PATH = REPO_ROOT / "scripts" / "build-changelog.py"


def _load(path: Path, name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


build_changelog = _load(SCRIPT_PATH, "build_changelog")


BASE_CHANGELOG = """# Changelog

Preamble prose.

## [Unreleased]

### Added

- An entry that predates fragments.

### Changed

- A changed entry.

## [0.1.0] — 2026-05-08

### Added

- First release.
"""


def _write(directory: Path, name: str, text: str) -> Path:
    path = directory / name
    path.write_text(text, encoding="utf-8")
    return path


@pytest.fixture()
def fragments_dir(tmp_path: Path) -> Path:
    directory = tmp_path / "changelog.d"
    directory.mkdir()
    _write(directory, "README.md", "# not a fragment\n")
    return directory


# --------------------------------------------------------------------------
# Fragment discovery
# --------------------------------------------------------------------------


def test_parse_fragment_name_accepts_every_section_type() -> None:
    for type_ in build_changelog.SECTION_ORDER:
        assert build_changelog.parse_fragment_name(f"42.{type_}.md") == ("42", type_)


@pytest.mark.parametrize(
    "name",
    ["README.md", "42.md", "42.nonsense.md", ".42.added.md", "notes.txt", ".added.md"],
)
def test_parse_fragment_name_rejects_non_fragments(name: str) -> None:
    assert build_changelog.parse_fragment_name(name) is None


def test_collect_fragments_skips_readme_and_sorts(fragments_dir: Path) -> None:
    _write(fragments_dir, "9.fixed.md", "- fixed nine\n")
    _write(fragments_dir, "12.added.md", "- added twelve\n")
    _write(fragments_dir, "3.added.md", "- added three\n")

    names = [fragment.path.name for fragment in build_changelog.collect_fragments(fragments_dir)]

    # Section order first (added before fixed), then filename within a section.
    assert names == ["12.added.md", "3.added.md", "9.fixed.md"]


def test_collect_fragments_on_missing_directory(tmp_path: Path) -> None:
    assert build_changelog.collect_fragments(tmp_path / "nope") == []


def test_unknown_fragment_files_flags_typos(fragments_dir: Path) -> None:
    _write(fragments_dir, "42.added.md", "- fine\n")
    _write(fragments_dir, "42.add.md", "- typo in the type\n")

    unknown = [path.name for path in build_changelog.unknown_fragment_files(fragments_dir)]

    assert unknown == ["42.add.md"]


# --------------------------------------------------------------------------
# Entry normalisation
# --------------------------------------------------------------------------


def test_normalise_entry_passes_bullets_through() -> None:
    text = "- **Thing.** Did a thing.\n- And another.\n"
    assert build_changelog.normalise_entry(text) == ["- **Thing.** Did a thing.", "- And another."]


def test_normalise_entry_bullets_plain_prose_and_indents_continuations() -> None:
    text = "**Thing.** Did a thing\nthat wrapped onto a second line.\n"
    assert build_changelog.normalise_entry(text) == [
        "- **Thing.** Did a thing",
        "  that wrapped onto a second line.",
    ]


def test_normalise_entry_on_empty_fragment() -> None:
    assert build_changelog.normalise_entry("\n  \n") == []


# --------------------------------------------------------------------------
# Compilation
# --------------------------------------------------------------------------


def _fragment(type_: str, identifier: str, text: str) -> object:
    return build_changelog.Fragment(
        path=Path(f"changelog.d/{identifier}.{type_}.md"),
        identifier=identifier,
        type=type_,
        text=text,
    )


def test_compile_appends_to_existing_sections() -> None:
    result = build_changelog.compile_changelog(
        BASE_CHANGELOG,
        [_fragment("added", "56", "- Fragments.\n"), _fragment("changed", "57", "- Tweak.\n")],
    )

    assert "- An entry that predates fragments.\n- Fragments." in result
    assert "- A changed entry.\n- Tweak." in result
    # Released sections are untouched.
    assert "## [0.1.0] — 2026-05-08\n\n### Added\n\n- First release." in result


def test_compile_creates_missing_sections_in_canonical_order() -> None:
    result = build_changelog.compile_changelog(
        BASE_CHANGELOG,
        [_fragment("fixed", "58", "- A fix.\n"), _fragment("removed", "59", "- A removal.\n")],
    )

    unreleased = result.split("## [0.1.0]")[0]
    assert [line for line in unreleased.split("\n") if line.startswith("### ")] == [
        "### Added",
        "### Changed",
        "### Removed",
        "### Fixed",
    ]


def test_compile_is_deterministic_regardless_of_input_order() -> None:
    fragments = [
        _fragment("fixed", "9", "- nine\n"),
        _fragment("added", "12", "- twelve\n"),
        _fragment("added", "3", "- three\n"),
    ]

    first = build_changelog.compile_changelog(BASE_CHANGELOG, fragments)
    second = build_changelog.compile_changelog(BASE_CHANGELOG, list(reversed(fragments)))

    assert first == second
    assert first.index("- twelve") < first.index("- three") < first.index("- nine")


def test_compile_with_no_fragments_is_a_no_op() -> None:
    assert build_changelog.compile_changelog(BASE_CHANGELOG, []) == BASE_CHANGELOG


def test_compile_without_unreleased_heading_raises() -> None:
    with pytest.raises(build_changelog.ChangelogError):
        build_changelog.compile_changelog("# Changelog\n\n## [0.1.0]\n", [_fragment("added", "1", "- x\n")])


def test_compile_into_an_empty_unreleased_section() -> None:
    changelog = "# Changelog\n\n## [Unreleased]\n\n## [0.1.0] — 2026-05-08\n\n- old\n"

    result = build_changelog.compile_changelog(changelog, [_fragment("added", "1", "- new\n")])

    assert result == "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- new\n\n## [0.1.0] — 2026-05-08\n\n- old\n"


# --------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------


def test_main_compiles_and_deletes_fragments(tmp_path: Path, fragments_dir: Path, capsys) -> None:
    changelog = _write(tmp_path, "CHANGELOG.md", BASE_CHANGELOG)
    _write(fragments_dir, "56.added.md", "- Fragments.\n")
    _write(fragments_dir, "57.fixed.md", "- A fix.\n")

    exit_code = build_changelog.main(
        ["--fragments-dir", str(fragments_dir), "--changelog", str(changelog)]
    )

    assert exit_code == 0
    text = changelog.read_text(encoding="utf-8")
    assert "- Fragments." in text and "- A fix." in text
    assert not (fragments_dir / "56.added.md").exists()
    assert not (fragments_dir / "57.fixed.md").exists()
    # README survives the sweep.
    assert (fragments_dir / "README.md").exists()


def test_main_keep_leaves_fragments_in_place(tmp_path: Path, fragments_dir: Path) -> None:
    changelog = _write(tmp_path, "CHANGELOG.md", BASE_CHANGELOG)
    _write(fragments_dir, "56.added.md", "- Fragments.\n")

    exit_code = build_changelog.main(
        ["--fragments-dir", str(fragments_dir), "--changelog", str(changelog), "--keep"]
    )

    assert exit_code == 0
    assert "- Fragments." in changelog.read_text(encoding="utf-8")
    assert (fragments_dir / "56.added.md").exists()


def test_main_dry_run_writes_nothing(tmp_path: Path, fragments_dir: Path, capsys) -> None:
    changelog = _write(tmp_path, "CHANGELOG.md", BASE_CHANGELOG)
    _write(fragments_dir, "56.added.md", "- Fragments.\n")

    exit_code = build_changelog.main(
        ["--fragments-dir", str(fragments_dir), "--changelog", str(changelog), "--dry-run"]
    )

    assert exit_code == 0
    assert changelog.read_text(encoding="utf-8") == BASE_CHANGELOG
    assert (fragments_dir / "56.added.md").exists()
    out = capsys.readouterr().out
    assert "## [Unreleased]" in out and "- Fragments." in out
    assert "## [0.1.0]" not in out


def test_main_with_no_fragments_succeeds(tmp_path: Path, fragments_dir: Path) -> None:
    changelog = _write(tmp_path, "CHANGELOG.md", BASE_CHANGELOG)

    exit_code = build_changelog.main(
        ["--fragments-dir", str(fragments_dir), "--changelog", str(changelog)]
    )

    assert exit_code == 0
    assert changelog.read_text(encoding="utf-8") == BASE_CHANGELOG


def test_main_check_passes_and_fails(tmp_path: Path, fragments_dir: Path) -> None:
    _write(fragments_dir, "56.added.md", "- Fragments.\n")
    assert build_changelog.main(["--fragments-dir", str(fragments_dir), "--check"]) == 0

    _write(fragments_dir, "57.add.md", "- typo'd type\n")
    assert build_changelog.main(["--fragments-dir", str(fragments_dir), "--check"]) == 1

    (fragments_dir / "57.add.md").unlink()
    _write(fragments_dir, "58.fixed.md", "\n")
    assert build_changelog.main(["--fragments-dir", str(fragments_dir), "--check"]) == 1


def test_repo_fragments_are_valid() -> None:
    """The fragments actually sitting in the repo compile without complaint."""
    assert build_changelog.unknown_fragment_files(REPO_ROOT / "changelog.d") == []
    for fragment in build_changelog.collect_fragments(REPO_ROOT / "changelog.d"):
        assert build_changelog.normalise_entry(fragment.text), f"{fragment.path} is empty"


# --------------------------------------------------------------------------
# The point of the whole exercise: parallel PRs must not conflict
# --------------------------------------------------------------------------


def _git(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            "git",
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "-c",
            "commit.gpgsign=false",
            *args,
        ],
        cwd=repo,
        capture_output=True,
        text=True,
    )


def _init_repo(repo: Path) -> None:
    repo.mkdir(parents=True, exist_ok=True)
    assert _git(repo, "init", "-q", "-b", "main").returncode == 0
    (repo / "CHANGELOG.md").write_text(BASE_CHANGELOG, encoding="utf-8")
    (repo / "changelog.d").mkdir()
    (repo / "changelog.d" / "README.md").write_text("# fragments\n", encoding="utf-8")
    assert _git(repo, "add", "-A").returncode == 0
    assert _git(repo, "commit", "-qm", "base").returncode == 0


def _branch_adding(repo: Path, branch: str, relative: str, text: str) -> None:
    assert _git(repo, "checkout", "-q", "-b", branch, "main").returncode == 0
    path = repo / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    assert _git(repo, "add", "-A").returncode == 0
    assert _git(repo, "commit", "-qm", f"work on {branch}").returncode == 0
    assert _git(repo, "checkout", "-q", "main").returncode == 0


@pytest.mark.skipif(shutil.which("git") is None, reason="git is not installed")
@pytest.mark.parametrize("order", [("pr-a", "pr-b"), ("pr-b", "pr-a")])
def test_two_fragment_prs_merge_cleanly_in_either_order(tmp_path: Path, order: tuple[str, str]) -> None:
    repo = tmp_path / "repo"
    _init_repo(repo)
    _branch_adding(repo, "pr-a", "changelog.d/101.added.md", "- Feature from PR A.\n")
    _branch_adding(repo, "pr-b", "changelog.d/102.fixed.md", "- Fix from PR B.\n")

    for branch in order:
        merge = _git(repo, "merge", "--no-edit", "-q", branch)
        assert merge.returncode == 0, f"merging {branch} conflicted:\n{merge.stdout}{merge.stderr}"

    # Both fragments survive, and compiling produces one combined section.
    exit_code = build_changelog.main(
        [
            "--fragments-dir",
            str(repo / "changelog.d"),
            "--changelog",
            str(repo / "CHANGELOG.md"),
        ]
    )
    assert exit_code == 0

    text = (repo / "CHANGELOG.md").read_text(encoding="utf-8")
    assert "- Feature from PR A." in text
    assert "- Fix from PR B." in text
    assert list((repo / "changelog.d").iterdir()) == [repo / "changelog.d" / "README.md"]


@pytest.mark.skipif(shutil.which("git") is None, reason="git is not installed")
def test_direct_changelog_edits_are_what_conflict(tmp_path: Path) -> None:
    """Control case: the old workflow — two PRs editing Unreleased — does conflict."""
    repo = tmp_path / "repo"
    _init_repo(repo)
    for branch, entry in (("pr-a", "- Feature from PR A."), ("pr-b", "- Fix from PR B.")):
        _branch_adding(
            repo,
            branch,
            "CHANGELOG.md",
            BASE_CHANGELOG.replace("### Added\n\n", f"### Added\n\n{entry}\n"),
        )

    assert _git(repo, "merge", "--no-edit", "-q", "pr-a").returncode == 0
    assert _git(repo, "merge", "--no-edit", "-q", "pr-b").returncode != 0
