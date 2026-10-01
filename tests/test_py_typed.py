from __future__ import annotations

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent


def test_py_typed_marker_exists() -> None:
    assert (REPO_ROOT / "src" / "circuitry" / "py.typed").exists()


def test_py_typed_is_packaged() -> None:
    """``pyproject.toml`` must ship ``py.typed`` in the wheel, matching the
    ``Typing :: Typed`` classifier the package declares (#325).
    """
    pyproject = (REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    assert '"Typing :: Typed"' in pyproject

    package_data_match = re.search(
        r"\[tool\.setuptools\.package-data\]\s*circuitry\s*=\s*\[(.*?)\]",
        pyproject,
        re.DOTALL,
    )
    assert package_data_match is not None, (
        "pyproject.toml's [tool.setuptools.package-data] circuitry list not found"
    )
    assert '"py.typed"' in package_data_match.group(1)
