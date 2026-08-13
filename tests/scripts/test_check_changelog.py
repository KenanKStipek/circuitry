"""Tests for ``scripts/check-changelog.py`` — the CI gate."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from types import ModuleType

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT_PATH = REPO_ROOT / "scripts" / "check-changelog.py"


def _load(path: Path, name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


check_changelog = _load(SCRIPT_PATH, "check_changelog")


CHANGELOG = """# Changelog

Preamble.

## [Unreleased]

### Added

- An unreleased entry.

## [0.1.0] — 2026-05-08

### Added

- First release.
"""


# --------------------------------------------------------------------------
# Escape hatches
# --------------------------------------------------------------------------


def test_no_changelog_label_skips() -> None:
    assert check_changelog.is_skipped(["bug", "no-changelog"], "feat: thing") is True


def test_skip_changelog_title_marker_skips_case_insensitively() -> None:
    assert check_changelog.is_skipped([], "chore: bump deps [SKIP-CHANGELOG]") is True


def test_ordinary_pr_is_not_skipped() -> None:
    assert check_changelog.is_skipped(["enhancement"], "feat: thing") is False


# --------------------------------------------------------------------------
# Which changes need a fragment
# --------------------------------------------------------------------------


@pytest.mark.parametrize(
    "path",
    ["src/circuitry/core/loop.py", "scripts/build-changelog.py", "pyproject.toml"],
)
def test_code_changes_require_a_fragment(path: str) -> None:
    assert check_changelog.requires_fragment([path]) is True


@pytest.mark.parametrize(
    "path",
    [
        "docs/index.md",
        "tests/scripts/test_build_changelog.py",
        ".github/workflows/quality.yml",
        "changelog.d/56.added.md",
        "README.md",
        "CHANGELOG.md",
    ],
)
def test_docs_and_test_only_changes_do_not(path: str) -> None:
    assert check_changelog.requires_fragment([path]) is False


def test_a_mixed_pr_still_requires_a_fragment() -> None:
    assert check_changelog.requires_fragment(["docs/index.md", "src/circuitry/cli/app.py"]) is True


def test_has_fragment_ignores_the_readme() -> None:
    assert check_changelog.has_fragment(["changelog.d/README.md"]) is False
    assert check_changelog.has_fragment(["changelog.d/56.added.md"]) is True
    assert check_changelog.has_fragment(["src/circuitry/cli/app.py"]) is False


# --------------------------------------------------------------------------
# Direct Unreleased edits
# --------------------------------------------------------------------------


def test_unreleased_line_range_covers_only_that_section() -> None:
    first, last = check_changelog.unreleased_line_range(CHANGELOG)
    lines = CHANGELOG.split("\n")

    assert lines[first - 1].strip() == "## [Unreleased]"
    assert "- An unreleased entry." in lines[first - 1 : last]
    assert "## [0.1.0] — 2026-05-08" not in lines[first - 1 : last]


def test_unreleased_line_range_when_absent() -> None:
    first, last = check_changelog.unreleased_line_range("# Changelog\n\n## [0.1.0]\n")
    assert last < first


def test_changed_line_numbers_parses_hunks() -> None:
    diff = "@@ -5,0 +6,2 @@\n+one\n+two\n@@ -20 +21 @@\n-old\n+new\n"
    assert check_changelog.changed_line_numbers(diff) == {6, 7, 21}


def test_changed_line_numbers_records_deletion_only_hunks() -> None:
    assert check_changelog.changed_line_numbers("@@ -9,2 +8,0 @@\n-gone\n-also gone\n") == {8}


def test_touches_unreleased_detects_an_added_entry() -> None:
    # Line 9 is "- An unreleased entry." — inside the Unreleased section.
    assert check_changelog.touches_unreleased("@@ -8,0 +9 @@\n+- sneaky\n", CHANGELOG) is True


def test_touches_unreleased_ignores_released_sections() -> None:
    # Line 15 is inside the 0.1.0 section.
    assert check_changelog.touches_unreleased("@@ -14,0 +15 @@\n+- historical fix\n", CHANGELOG) is False


def test_touches_unreleased_ignores_the_header_preamble() -> None:
    assert check_changelog.touches_unreleased("@@ -3 +3 @@\n-Preamble.\n+Preamble, reworded.\n", CHANGELOG) is False


# --------------------------------------------------------------------------
# End to end, against a throwaway git repo
# --------------------------------------------------------------------------


@pytest.fixture()
def repo(tmp_path: Path) -> Path:
    import subprocess

    root = tmp_path / "repo"
    root.mkdir()

    def git(*args: str) -> None:
        result = subprocess.run(
            ["git", "-c", "user.email=t@e.com", "-c", "user.name=T", "-c", "commit.gpgsign=false", *args],
            cwd=root,
            capture_output=True,
            text=True,
        )
        assert result.returncode == 0, result.stderr

    git("init", "-q", "-b", "main")
    (root / "CHANGELOG.md").write_text(CHANGELOG, encoding="utf-8")
    (root / "changelog.d").mkdir()
    (root / "changelog.d" / "README.md").write_text("# fragments\n", encoding="utf-8")
    (root / "src").mkdir()
    (root / "src" / "mod.py").write_text("x = 1\n", encoding="utf-8")
    git("add", "-A")
    git("commit", "-qm", "base")
    git("checkout", "-q", "-b", "feature")
    return root


def _commit(repo: Path, message: str) -> None:
    import subprocess

    for args in (["add", "-A"], ["commit", "-qm", message]):
        result = subprocess.run(
            ["git", "-c", "user.email=t@e.com", "-c", "user.name=T", "-c", "commit.gpgsign=false", *args],
            cwd=repo,
            capture_output=True,
            text=True,
        )
        assert result.returncode == 0, result.stderr


def _run(repo: Path, **kwargs: str) -> int:
    argv = ["--base-ref", "main", "--head-ref", "HEAD", "--repo-root", str(repo)]
    for key, value in kwargs.items():
        argv += [f"--{key.replace('_', '-')}", value]
    return check_changelog.main(argv)


def test_code_change_without_fragment_fails(repo: Path) -> None:
    (repo / "src" / "mod.py").write_text("x = 2\n", encoding="utf-8")
    _commit(repo, "feat: change code")

    assert _run(repo) == 1


def test_code_change_with_fragment_passes(repo: Path) -> None:
    (repo / "src" / "mod.py").write_text("x = 2\n", encoding="utf-8")
    (repo / "changelog.d" / "77.added.md").write_text("- A thing.\n", encoding="utf-8")
    _commit(repo, "feat: change code")

    assert _run(repo) == 0


def test_no_changelog_label_lets_a_code_change_through(repo: Path) -> None:
    (repo / "src" / "mod.py").write_text("x = 2\n", encoding="utf-8")
    _commit(repo, "chore: change code")

    assert _run(repo, labels="bug,no-changelog") == 0
    assert _run(repo, pr_title="chore: internal [skip-changelog]") == 0


def test_editing_the_unreleased_section_fails(repo: Path) -> None:
    (repo / "CHANGELOG.md").write_text(
        CHANGELOG.replace("- An unreleased entry.", "- An unreleased entry.\n- A sneaky one."),
        encoding="utf-8",
    )
    _commit(repo, "docs: edit changelog")

    assert _run(repo) == 1


def test_editing_a_released_section_is_allowed(repo: Path) -> None:
    (repo / "CHANGELOG.md").write_text(
        CHANGELOG.replace("- First release.", "- First release (typo fixed)."),
        encoding="utf-8",
    )
    _commit(repo, "docs: fix a typo in 0.1.0 notes")

    assert _run(repo) == 0


def test_release_prs_can_rewrite_unreleased_with_the_label(repo: Path) -> None:
    (repo / "CHANGELOG.md").write_text(
        CHANGELOG.replace("## [Unreleased]", "## [Unreleased]\n\n## [0.2.0] — 2026-08-13"),
        encoding="utf-8",
    )
    _commit(repo, "release v0.2.0")

    assert _run(repo, labels="no-changelog") == 0


def test_docs_only_change_passes(repo: Path) -> None:
    (repo / "README.md").write_text("hello\n", encoding="utf-8")
    _commit(repo, "docs: readme")

    assert _run(repo) == 0
