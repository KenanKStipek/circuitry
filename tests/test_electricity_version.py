"""electricity's Cargo workspace version must always equal pyproject.toml's
(#359) — regex rather than ``tomllib``/a TOML dependency to parse, since
``tomllib`` is stdlib only from Python 3.11 and CI tests 3.10 too (see
``tests/test_py_typed.py`` for the same pattern).
"""

from __future__ import annotations

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent


def _pyproject_version() -> str:
    pyproject = (REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    match = re.search(r'(?m)^version\s*=\s*"([^"]+)"', pyproject)
    assert match is not None, "pyproject.toml: no top-level version = \"...\" found"
    return match.group(1)


def _cargo_workspace_version() -> str:
    cargo_toml = (REPO_ROOT / "electricity" / "Cargo.toml").read_text(encoding="utf-8")
    workspace_package_match = re.search(
        r"(?ms)^\[workspace\.package\]\s*(.*?)(?:\n\[|\Z)", cargo_toml
    )
    assert workspace_package_match is not None, (
        "electricity/Cargo.toml: no [workspace.package] table found"
    )
    version_match = re.search(r'(?m)^version\s*=\s*"([^"]+)"', workspace_package_match.group(1))
    assert version_match is not None, (
        "electricity/Cargo.toml: [workspace.package] has no version = \"...\""
    )
    return version_match.group(1)


def test_electricity_workspace_version_matches_pyproject() -> None:
    assert _cargo_workspace_version() == _pyproject_version()
