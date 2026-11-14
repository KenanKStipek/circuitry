#!/usr/bin/env python3
"""Reject built distributions whose metadata carries a direct-URL dependency.

PyPI refuses an upload whose ``Requires-Dist`` metadata uses PEP 508's direct
reference form (``name @ url``), including inside an extra — and
``twine check`` does not catch it (#774). This script reads the wheel's
``METADATA`` and the sdist's ``PKG-INFO`` and fails, naming the offending
line, if either contains one.

    python scripts/check-direct-references.py dist/*.whl dist/*.tar.gz

With no arguments, it checks every ``*.whl`` and ``*.tar.gz`` under ``dist/``.

Standard library only.
"""

from __future__ import annotations

import argparse
import re
import sys
import tarfile
import zipfile
from collections.abc import Iterable, Sequence
from email import message_from_string
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

#: PEP 508's direct reference grammar: a name (with optional extras), then
#: `` @ `` followed by a URL. The marker clause (after `;`) is stripped
#: before this is applied, so a legitimate environment marker containing an
#: unrelated `@` can't trip it.
DIRECT_REFERENCE_RE = re.compile(r"^[A-Za-z0-9][\w.-]*\s*(?:\[[^\]]*\])?\s*@\s*\S")


def requires_dist_from_metadata(text: str) -> list[str]:
    """``Requires-Dist`` header values from an RFC 822 metadata blob."""
    message = message_from_string(text)
    return message.get_all("Requires-Dist") or []


def requires_dist_from_wheel(path: Path) -> list[str]:
    with zipfile.ZipFile(path) as archive:
        metadata_names = [n for n in archive.namelist() if n.endswith(".dist-info/METADATA")]
        if not metadata_names:
            raise ValueError(f"{path}: no *.dist-info/METADATA found")
        text = archive.read(metadata_names[0]).decode("utf-8")
    return requires_dist_from_metadata(text)


def requires_dist_from_sdist(path: Path) -> list[str]:
    with tarfile.open(path) as archive:
        pkg_info_names = [
            n for n in archive.getnames() if n.endswith("/PKG-INFO") and n.count("/") == 1
        ]
        if not pkg_info_names:
            raise ValueError(f"{path}: no top-level PKG-INFO found")
        member = archive.extractfile(pkg_info_names[0])
        assert member is not None
        text = member.read().decode("utf-8")
    return requires_dist_from_metadata(text)


def requires_dist_from_dist(path: Path) -> list[str]:
    if path.suffixes[-2:] == [".tar", ".gz"] or path.suffix == ".tgz":
        return requires_dist_from_sdist(path)
    if path.suffix == ".whl":
        return requires_dist_from_wheel(path)
    raise ValueError(f"{path}: unrecognized distribution file type")


def direct_references(lines: Iterable[str]) -> list[str]:
    """``Requires-Dist`` lines that use PEP 508's direct reference form."""
    offenders = []
    for line in lines:
        clause = line.split(";", 1)[0].strip()
        if DIRECT_REFERENCE_RE.match(clause):
            offenders.append(line)
    return offenders


def default_dist_files() -> list[Path]:
    dist_dir = REPO_ROOT / "dist"
    return sorted(dist_dir.glob("*.whl")) + sorted(dist_dir.glob("*.tar.gz"))


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "dists",
        nargs="*",
        type=Path,
        help="wheel/sdist paths to check (default: dist/*.whl, dist/*.tar.gz)",
    )
    args = parser.parse_args(argv)

    dist_files = args.dists or default_dist_files()
    if not dist_files:
        print("error: no distribution files given and none found under dist/", file=sys.stderr)
        return 1

    failures: list[str] = []
    for dist_file in dist_files:
        lines = requires_dist_from_dist(dist_file)
        failures.extend(
            f"{dist_file}: direct reference in Requires-Dist: {offender}"
            for offender in direct_references(lines)
        )

    for failure in failures:
        print(f"error: {failure}", file=sys.stderr)
    if failures:
        return 1

    print(f"direct-reference check OK ({len(dist_files)} distribution(s)).")
    return 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
