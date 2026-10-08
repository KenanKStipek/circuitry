"""Guards against a committed conformance fixture leaking the generating
machine's own filesystem layout (electricity/DESIGN.md §12; the generator
redacts the known absolute-path fields before writing, see `normalize.py`
and `scripts/generate-conformance-cases.py` — this test is the regression
check that redaction never gets silently bypassed, by a new field, a new
field path, or hand-editing a fixture)."""

from __future__ import annotations

import getpass
import re
from pathlib import Path

import pytest

from . import harness

_LEAK_PATTERNS = [
    re.compile(r"/Users/"),
    re.compile(r"/home/"),
    re.compile(r"/var/folders/"),
    re.compile(r"/private/"),
    re.compile(r"/tmp/"),
    re.compile(r"\.pi/"),
    # A real Windows path, JSON-encoded, has a *doubled* backslash
    # (`"C:\\Users\\..."`) — single-backslash `d:\n` inside ordinary
    # error text (an escaped newline right after a word ending in a letter)
    # must not false-positive here.
    re.compile(r"[A-Za-z]:\\\\"),
]

# Only flag the username embedded in a path (`/home/runner/...`,
# `/Users/<name>/...`) — a bare substring match is unsafe for a
# common word used as a username (CI's own `runner`), and the path
# patterns above already catch a leak that includes it this way.
_USERNAME_PATTERN = (
    re.compile(r"/" + re.escape(getpass.getuser()) + r"/") if getpass.getuser() else None
)


def _fixture_files() -> list[Path]:
    cases_dir = harness.CASES_DIR
    return sorted(p for p in cases_dir.rglob("*") if p.is_file())


def _fixture_ids() -> list[str]:
    cases_dir = harness.CASES_DIR
    return [str(p.relative_to(cases_dir)) for p in _fixture_files()]


@pytest.mark.parametrize("path", _fixture_files(), ids=_fixture_ids())
def test_no_local_filesystem_paths_committed(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    for pattern in _LEAK_PATTERNS:
        match = pattern.search(text)
        if match is not None:
            pytest.fail(
                f"{path}: committed fixture contains a local-filesystem path "
                f"({pattern.pattern!r} matched {match.group(0)!r}) — regenerate with "
                f"`python scripts/generate-conformance-cases.py`"
            )
    if _USERNAME_PATTERN is not None and _USERNAME_PATTERN.search(text):
        pytest.fail(
            f"{path}: committed fixture contains the current username in a path "
            f"({getpass.getuser()!r}) — regenerate with "
            f"`python scripts/generate-conformance-cases.py`"
        )
