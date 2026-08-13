#!/usr/bin/env python3
"""CI gate for the ``changelog.d/`` fragment workflow.

Two rules, both aimed at keeping parallel PRs conflict-free:

1. A PR that changes shipped code must add a fragment (``changelog.d/*.md``).
2. No PR may edit ``CHANGELOG.md``'s ``## [Unreleased]`` section directly —
   that is compiled from fragments at release time.

Escape hatches, for release PRs and for changes with nothing to announce:
a ``no-changelog`` label, or ``[skip-changelog]`` in the PR title.

    python scripts/check-changelog.py --base-ref origin/main \\
        --labels "$LABELS" --pr-title "$TITLE"

Standard library only; shells out to ``git`` for the diff.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from collections.abc import Iterable, Sequence
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

SKIP_LABEL = "no-changelog"
SKIP_TITLE_MARKER = "[skip-changelog]"

CHANGELOG_PATH = "CHANGELOG.md"
FRAGMENTS_DIR = "changelog.d"
UNRELEASED_HEADING = "## [Unreleased]"

#: Changing any of these means the release notes need an entry.
CODE_PATH_PREFIXES = ("src/", "scripts/")
CODE_PATH_FILES = ("pyproject.toml",)

#: ...unless the change is only to these (docs, tests, CI, the fragments).
EXEMPT_PATH_PREFIXES = (f"{FRAGMENTS_DIR}/", "docs/", "tests/", ".github/", "_bmad")

HUNK_RE = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@")


def _git(args: Sequence[str], cwd: Path) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout


# --------------------------------------------------------------------------
# Pure predicates (unit-tested without a git repo)
# --------------------------------------------------------------------------


def is_skipped(labels: Iterable[str], pr_title: str) -> bool:
    """True when the PR opted out of the fragment requirement."""
    if any(label.strip() == SKIP_LABEL for label in labels):
        return True
    return SKIP_TITLE_MARKER in pr_title.lower()


def requires_fragment(paths: Iterable[str]) -> bool:
    """True when any changed path is shipped code that needs release notes."""
    for path in paths:
        if path.startswith(EXEMPT_PATH_PREFIXES):
            continue
        if path.startswith(CODE_PATH_PREFIXES) or path in CODE_PATH_FILES:
            return True
    return False


def has_fragment(paths: Iterable[str]) -> bool:
    """True when the change adds or updates at least one fragment."""
    prefix = f"{FRAGMENTS_DIR}/"
    return any(
        path.startswith(prefix) and path.endswith(".md") and not path.endswith("/README.md")
        for path in paths
    )


def unreleased_line_range(changelog_text: str) -> tuple[int, int]:
    """1-indexed ``(first, last)`` lines of the Unreleased section.

    Returns ``(0, -1)`` (an empty range) when there is no such section.
    """
    lines = changelog_text.split("\n")
    start = None
    for index, line in enumerate(lines, start=1):
        if line.strip() == UNRELEASED_HEADING:
            start = index
            break
    if start is None:
        return (0, -1)
    for index in range(start, len(lines)):
        if lines[index].startswith("## "):
            return (start, index)
    return (start, len(lines))


def changed_line_numbers(diff_text: str) -> set[int]:
    """Post-image line numbers touched by a unified diff.

    Pure deletions report the line they were removed from, so removing an
    Unreleased entry is caught too.
    """
    touched: set[int] = set()
    for line in diff_text.split("\n"):
        match = HUNK_RE.match(line)
        if not match:
            continue
        start = int(match.group(1))
        count = int(match.group(2)) if match.group(2) is not None else 1
        if count == 0:  # deletion-only hunk
            touched.add(start)
            continue
        touched.update(range(start, start + count))
    return touched


def touches_unreleased(diff_text: str, changelog_text: str) -> bool:
    """True when the diff edits lines inside the Unreleased section."""
    first, last = unreleased_line_range(changelog_text)
    if last < first:
        return False
    return any(first <= line <= last for line in changed_line_numbers(diff_text))


# --------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--base-ref", default="origin/main", help="merge base to diff against")
    parser.add_argument("--head-ref", default="HEAD", help="the PR head")
    parser.add_argument("--labels", default="", help="comma- or newline-separated PR labels")
    parser.add_argument("--pr-title", default="", help="the PR title")
    parser.add_argument("--repo-root", type=Path, default=REPO_ROOT)
    args = parser.parse_args(argv)

    labels = [part for part in re.split(r"[,\n]", args.labels) if part.strip()]
    if is_skipped(labels, args.pr_title):
        print(f"changelog check skipped ({SKIP_LABEL} label or {SKIP_TITLE_MARKER} in title).")
        return 0

    diff_range = f"{args.base_ref}...{args.head_ref}"
    try:
        changed = [
            path
            for path in _git(["diff", "--name-only", diff_range], args.repo_root).split("\n")
            if path.strip()
        ]
    except subprocess.CalledProcessError as exc:
        print(f"error: git diff {diff_range} failed: {exc.stderr.strip()}", file=sys.stderr)
        return 1

    failures: list[str] = []

    if CHANGELOG_PATH in changed:
        diff_text = _git(["diff", "-U0", diff_range, "--", CHANGELOG_PATH], args.repo_root)
        changelog_text = (args.repo_root / CHANGELOG_PATH).read_text(encoding="utf-8")
        if touches_unreleased(diff_text, changelog_text):
            failures.append(
                f"{CHANGELOG_PATH}'s '{UNRELEASED_HEADING}' section was edited directly. "
                f"Add a fragment under {FRAGMENTS_DIR}/ instead — see {FRAGMENTS_DIR}/README.md. "
                f"(Release PRs: add the '{SKIP_LABEL}' label.)"
            )

    if requires_fragment(changed) and not has_fragment(changed):
        failures.append(
            f"This PR changes code but adds no changelog fragment. Create "
            f"{FRAGMENTS_DIR}/<issue-or-pr>.<type>.md (added/changed/deprecated/removed/"
            f"fixed/security) — see {FRAGMENTS_DIR}/README.md. "
            f"Nothing to announce? Add the '{SKIP_LABEL}' label or put "
            f"'{SKIP_TITLE_MARKER}' in the PR title."
        )

    for failure in failures:
        print(f"error: {failure}", file=sys.stderr)
    if failures:
        return 1

    print("changelog check OK.")
    return 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
