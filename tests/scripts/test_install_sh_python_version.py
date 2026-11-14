"""scripts/install.sh's own Python-version gate must match pyproject.toml's
``requires-python`` (issue #776) -- it used to accept 3.9, one minor version
below what the package itself needs."""

from __future__ import annotations

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]


def test_install_sh_min_python_matches_pyproject_requires_python() -> None:
    pyproject = (REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    match = re.search(r'requires-python\s*=\s*">=(\d+)\.(\d+)"', pyproject)
    assert match, "pyproject.toml has no requires-python"
    major, minor = match.groups()

    install_sh = (REPO_ROOT / "scripts" / "install.sh").read_text(encoding="utf-8")
    min_python_match = re.search(r'MIN_PYTHON="(\d+\.\d+)"', install_sh)
    assert min_python_match, "scripts/install.sh has no MIN_PYTHON"
    assert min_python_match.group(1) == f"{major}.{minor}"

    assert f'-ge {minor} ]; }}' in install_sh, (
        "scripts/install.sh's version-comparison check doesn't match MIN_PYTHON"
    )
