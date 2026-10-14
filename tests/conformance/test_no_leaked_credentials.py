"""Case 22's own static twin (tools-adapters-plugins.md §7 case 22, the
redaction blanket; issue #448's acceptance criterion, closed by #537):
every committed `expected.*` file across the whole suite -- not just
`c-http-redaction-blanket`'s own -- must never contain a real-looking
credential, `harness.FAKE_CREDENTIAL_MARKER`, or one of its own case's
`fake_credentials` values. `test_no_leaked_paths.py` is the equivalent
check for a leaked local filesystem path; this is the same idea for a
leaked credential.

`expected.http_requests.json` is deliberately excluded: nine cases record
a real-looking credential there on purpose (`tests/conformance/README.md`'s
own case-22 section) -- what the mock server actually received is the
point of the header/URL redaction those cases check (display-only, never
a what-gets-sent change), so that one file is the one declared exemption,
exactly as `assert_no_leaked_credentials` never scans it either.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from . import harness

#: The `expected.*` filenames case 22's static check covers (`tests/
#: conformance/README.md`'s own layout table) -- deliberately not
#: `expected.http_requests.json` (the one declared exemption above), and
#: not an input file (`orchestration.yml`, `config.json`, a scripted
#: replies file, a mock-HTTP fixture, `case.json` itself): those describe
#: what a case *sends*, which can legitimately carry a credential-shaped
#: literal (`c-http-redaction-blanket`'s own `Authorization: Bearer
#: sk-live-...` header) -- only what a case's own run *produced* matters
#: here.
_CHECKED_FILENAMES = frozenset(
    {
        "expected.json",
        "expected.out.json",
        "expected.pretty.json",
        "expected.events.jsonl",
        "expected.warnings.txt",
        "expected.run_warnings.txt",
    }
)


def _checked_files() -> list[Path]:
    return sorted(
        p
        for p in harness.CASES_DIR.rglob("*")
        if p.is_file() and p.name in _CHECKED_FILENAMES
    )


def _checked_file_ids() -> list[str]:
    return [str(p.relative_to(harness.CASES_DIR)) for p in _checked_files()]


@pytest.mark.parametrize("path", _checked_files(), ids=_checked_file_ids())
def test_no_committed_expected_file_leaks_a_credential(path: Path) -> None:
    case_dir = path.parent
    metadata = harness.load_case(case_dir)
    text = path.read_text(encoding="utf-8")
    harness.assert_no_leaked_credentials(
        {path.name: text},
        case_name=case_dir.name,
        fake_credentials=metadata.get("fake_credentials"),
    )
