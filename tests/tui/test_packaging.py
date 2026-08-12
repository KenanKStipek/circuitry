"""Packaging: the ``[tui]`` extra is declared and resolves to textual."""

from __future__ import annotations

import re
from importlib.metadata import PackageNotFoundError, metadata
from pathlib import Path

import pytest

PYPROJECT = Path(__file__).resolve().parents[2] / "pyproject.toml"


def _dist_metadata():
    for dist in ("circuitry-cof", "circuitry"):
        try:
            return metadata(dist)
        except PackageNotFoundError:
            continue
    pytest.skip("circuitry is not installed; run `pip install -e '.[tui]'`")


def test_pyproject_declares_the_tui_extra() -> None:
    text = PYPROJECT.read_text(encoding="utf-8")
    assert re.search(r'^tui = \["textual>=1\.0"\]$', text, re.MULTILINE), (
        "pyproject.toml must declare a `tui` extra pinning textual>=1.0"
    )


def test_installed_metadata_exposes_the_tui_extra() -> None:
    meta = _dist_metadata()
    assert "tui" in meta.get_all("Provides-Extra", [])


def test_tui_extra_requires_textual() -> None:
    meta = _dist_metadata()
    requires = meta.get_all("Requires-Dist", [])
    tui_reqs = [r for r in requires if 'extra == "tui"' in r or "extra == 'tui'" in r]
    assert tui_reqs, f"no Requires-Dist gated on the tui extra: {requires}"
    assert any(r.startswith("textual") for r in tui_reqs), tui_reqs


def test_textual_is_not_a_base_dependency() -> None:
    """The base install must stay dependency-light."""
    meta = _dist_metadata()
    base = [r for r in meta.get_all("Requires-Dist", []) if "extra ==" not in r]
    assert not any(r.startswith("textual") for r in base), base
