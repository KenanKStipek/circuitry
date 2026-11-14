"""electricity/scripts/generator-constraints.txt pins exact versions of
every library whose text or behaviour the conformance suite compares
against (issue #778); pyproject.toml's own dependency ranges must each
cover the pinned version, narrowed to its minor release, so an ordinary
user's install can never resolve a library version CI's suite never ran
against.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
CONSTRAINTS_PATH = REPO_ROOT / "electricity" / "scripts" / "generator-constraints.txt"

# The libraries issue #778's plan names: every one whose behaviour the
# conformance suite compares, plus the libraries underneath them.
EXPECTED_PINNED_LIBRARIES = frozenset(
    {
        "jsonschema",
        "referencing",
        "jsonschema-specifications",
        "pyyaml",
        "chevron",
        "cel-python",
        "lark",
        "google-re2",
        "pendulum",
        "python-dotenv",
    }
)


def _normalize(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def _parse_version(version: str) -> tuple[int, ...]:
    return tuple(int(part) for part in version.split("."))


def _load_constraints() -> dict[str, str]:
    constraints: dict[str, str] = {}
    for raw_line in CONSTRAINTS_PATH.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        name, _, version = line.partition("==")
        assert version, f"generator-constraints.txt: not an exact pin: {line!r}"
        constraints[_normalize(name)] = version
    return constraints


def _load_pyproject_dependency_specifiers() -> dict[str, str]:
    pyproject = (REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    match = re.search(r"(?ms)^dependencies = \[(.*?)\n\]", pyproject)
    assert match is not None, "pyproject.toml: no top-level dependencies = [...] found"
    specifiers: dict[str, str] = {}
    dependency_lines = [
        line.strip() for line in match.group(1).splitlines() if line.strip().startswith('"')
    ]
    for line in dependency_lines:
        entry_match = re.match(r'^"([^"]+)"', line)
        assert entry_match is not None, f"pyproject.toml: unparseable dependency line {line!r}"
        entry = entry_match.group(1)
        name_match = re.match(r"^([A-Za-z0-9][A-Za-z0-9._-]*)(.*)$", entry)
        assert name_match is not None, f"pyproject.toml: unparseable dependency {entry!r}"
        specifiers[_normalize(name_match.group(1))] = name_match.group(2)
    return specifiers


def _satisfies(version: str, specifier: str) -> bool:
    version_t = _parse_version(version)
    for raw_clause in specifier.split(","):
        clause = raw_clause.strip()
        if clause.startswith(">="):
            if version_t < _parse_version(clause[2:]):
                return False
        elif clause.startswith("<="):
            if version_t > _parse_version(clause[2:]):
                return False
        elif clause.startswith("<"):
            if version_t >= _parse_version(clause[1:]):
                return False
        else:
            raise AssertionError(f"unsupported specifier clause: {clause!r}")
    return True


def test_every_expected_library_is_pinned() -> None:
    pinned = _load_constraints()
    assert pinned.keys() >= EXPECTED_PINNED_LIBRARIES


@pytest.mark.parametrize("library", sorted(EXPECTED_PINNED_LIBRARIES))
def test_pinned_version_satisfies_pyproject_range(library: str) -> None:
    pinned = _load_constraints()
    specifiers = _load_pyproject_dependency_specifiers()
    assert library in specifiers, f"pyproject.toml: no dependency entry for {library!r}"
    assert _satisfies(pinned[library], specifiers[library]), (
        f"{library}=={pinned[library]} (generator-constraints.txt) does not satisfy "
        f"pyproject.toml's {library}{specifiers[library]!r}"
    )


@pytest.mark.parametrize("library", sorted(EXPECTED_PINNED_LIBRARIES))
def test_pyproject_range_stops_at_the_pins_next_minor(library: str) -> None:
    """Satisfying the pin isn't enough: the range must end at the pin's next
    minor release, so a plain install can't resolve a version the suite never
    ran against."""
    pinned = _load_constraints()
    specifiers = _load_pyproject_dependency_specifiers()
    major, minor = _parse_version(pinned[library])[:2]
    upper_bounds = [
        clause.strip()[1:]
        for clause in specifiers[library].split(",")
        if clause.strip().startswith("<") and not clause.strip().startswith("<=")
    ]
    assert upper_bounds == [f"{major}.{minor + 1}"], (
        f"pyproject.toml's {library}{specifiers[library]!r} should end at <{major}.{minor + 1}, "
        f"the minor release after the pinned {pinned[library]}"
    )
