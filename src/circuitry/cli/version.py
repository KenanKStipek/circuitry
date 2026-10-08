"""The installed package version, as reported by `cof --version`/`version`
and by anything else that needs to name this engine's own build (e.g. the
``--events`` stream's ``run_start.engine`` field).
"""

from __future__ import annotations


def resolve_version() -> str:
    from importlib.metadata import PackageNotFoundError
    from importlib.metadata import version as pkg_version

    # Distribution name is `circuitry-cof` on PyPI; the legacy `circuitry`
    # lookup is kept as a fallback for editable installs that pre-date the
    # rename.
    for dist in ("circuitry-cof", "circuitry"):
        try:
            return pkg_version(dist)
        except PackageNotFoundError:
            continue
    return "0.1.0+unknown"
