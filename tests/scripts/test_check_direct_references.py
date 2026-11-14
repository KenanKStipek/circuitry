"""Tests for ``scripts/check-direct-references.py``."""

from __future__ import annotations

import importlib.util
import shutil
import subprocess
import sys
from pathlib import Path
from types import ModuleType

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT_PATH = REPO_ROOT / "scripts" / "check-direct-references.py"


def _load(path: Path, name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


check_direct_references = _load(SCRIPT_PATH, "check_direct_references")


# ---------------------------------------------------------------------------
# Pure predicate
# ---------------------------------------------------------------------------


def test_direct_references_flags_name_at_url():
    lines = [
        "typer>=0.12",
        'toon-format @ git+https://github.com/toon-format/toon-python.git ; extra == "toon"',
    ]
    offenders = check_direct_references.direct_references(lines)
    assert offenders == [
        'toon-format @ git+https://github.com/toon-format/toon-python.git ; extra == "toon"'
    ]


def test_direct_references_allows_version_specifiers_and_markers():
    lines = [
        "typer>=0.12",
        'toon-format<2,>=1.2; extra == "toon"',
        'requests>=2.28; python_version >= "3.10"',
    ]
    assert check_direct_references.direct_references(lines) == []


def test_direct_references_flags_direct_reference_with_extras():
    lines = ['somepkg[extra] @ https://example.com/somepkg-1.0.tar.gz']
    assert check_direct_references.direct_references(lines) == lines


def test_direct_references_flags_no_space_after_at():
    # PEP 508 allows no whitespace around `@` at all.
    lines = ['somepkg@https://example.com/somepkg-1.0.tar.gz']
    assert check_direct_references.direct_references(lines) == lines


# ---------------------------------------------------------------------------
# Built distributions
# ---------------------------------------------------------------------------


def _build_throwaway_project(project_dir: Path, dist_dir: Path) -> None:
    (project_dir / "src" / "tinypkg").mkdir(parents=True)
    (project_dir / "src" / "tinypkg" / "__init__.py").write_text("", encoding="utf-8")
    (project_dir / "pyproject.toml").write_text(
        """\
[build-system]
requires = ["setuptools>=77"]
build-backend = "setuptools.build_meta"

[project]
name = "tinypkg"
version = "0.0.1"
dependencies = []

[project.optional-dependencies]
extra = ["somepkg @ git+https://example.com/somepkg.git"]

[tool.setuptools.packages.find]
where = ["src"]
""",
        encoding="utf-8",
    )
    subprocess.run(
        ["uv", "build", "-o", str(dist_dir), str(project_dir)],
        check=True,
        capture_output=True,
        text=True,
    )


@pytest.mark.skipif(shutil.which("uv") is None, reason="uv not installed")
def test_check_fails_on_built_dist_with_direct_reference(tmp_path: Path):
    project_dir = tmp_path / "throwaway"
    dist_dir = tmp_path / "dist"
    _build_throwaway_project(project_dir, dist_dir)

    dist_files = sorted(dist_dir.glob("*.whl")) + sorted(dist_dir.glob("*.tar.gz"))
    assert len(dist_files) == 2

    failures: list[str] = []
    for dist_file in dist_files:
        lines = check_direct_references.requires_dist_from_dist(dist_file)
        failures.extend(check_direct_references.direct_references(lines))
    assert len(failures) == 2
    assert all("somepkg @ git+https://example.com/somepkg.git" in f for f in failures)

    exit_code = check_direct_references.main([str(f) for f in dist_files])
    assert exit_code == 1


@pytest.mark.skipif(shutil.which("uv") is None, reason="uv not installed")
def test_check_passes_on_built_dist_without_direct_reference(tmp_path: Path):
    project_dir = tmp_path / "throwaway"
    dist_dir = tmp_path / "dist"
    (project_dir / "src" / "tinypkg").mkdir(parents=True)
    (project_dir / "src" / "tinypkg" / "__init__.py").write_text("", encoding="utf-8")
    (project_dir / "pyproject.toml").write_text(
        """\
[build-system]
requires = ["setuptools>=77"]
build-backend = "setuptools.build_meta"

[project]
name = "tinypkg"
version = "0.0.1"
dependencies = ["somepkg>=1,<2"]

[tool.setuptools.packages.find]
where = ["src"]
""",
        encoding="utf-8",
    )
    subprocess.run(
        ["uv", "build", "-o", str(dist_dir), str(project_dir)],
        check=True,
        capture_output=True,
        text=True,
    )

    dist_files = sorted(dist_dir.glob("*.whl")) + sorted(dist_dir.glob("*.tar.gz"))
    assert len(dist_files) == 2

    exit_code = check_direct_references.main([str(f) for f in dist_files])
    assert exit_code == 0
