"""electricity's and oscilloscope's Cargo workspace versions must always
equal pyproject.toml's (#359, #420) — regex rather than ``tomllib``/a TOML
dependency to parse, since ``tomllib`` is stdlib only from Python 3.11 and
CI tests 3.10 too (see ``tests/test_py_typed.py`` for the same pattern).

A pre-release version differs in spelling between the two: pyproject.toml
uses PEP 440 (``0.2.0rc1``), but Cargo requires valid semver
(``0.2.0-rc.1``) — see RELEASING.md's "Pre-releases" section, and the same
mapping in ``.github/workflows/release.yml``'s ``verify-version`` job.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
WORKSPACES = ("electricity", "oscilloscope")


def _pyproject_version() -> str:
    pyproject = (REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    match = re.search(r'(?m)^version\s*=\s*"([^"]+)"', pyproject)
    assert match is not None, "pyproject.toml: no top-level version = \"...\" found"
    return match.group(1)


def _expected_cargo_version(pyproject_version: str) -> str:
    match = re.fullmatch(r"(\d+\.\d+\.\d+)rc(\d+)", pyproject_version)
    if match is None:
        return pyproject_version
    return f"{match.group(1)}-rc.{match.group(2)}"


def _cargo_workspace_version(workspace: str) -> str:
    cargo_toml = (REPO_ROOT / workspace / "Cargo.toml").read_text(encoding="utf-8")
    workspace_package_match = re.search(
        r"(?ms)^\[workspace\.package\]\s*(.*?)(?:\n\[|\Z)", cargo_toml
    )
    assert workspace_package_match is not None, (
        f"{workspace}/Cargo.toml: no [workspace.package] table found"
    )
    version_match = re.search(r'(?m)^version\s*=\s*"([^"]+)"', workspace_package_match.group(1))
    assert version_match is not None, (
        f"{workspace}/Cargo.toml: [workspace.package] has no version = \"...\""
    )
    return version_match.group(1)


@pytest.mark.parametrize("workspace", WORKSPACES)
def test_cargo_workspace_version_matches_pyproject(workspace: str) -> None:
    assert _cargo_workspace_version(workspace) == _expected_cargo_version(_pyproject_version())


def test_expected_cargo_version_maps_rc_suffix_to_semver_prerelease() -> None:
    assert _expected_cargo_version("0.2.0rc1") == "0.2.0-rc.1"
    assert _expected_cargo_version("0.2.0") == "0.2.0"
