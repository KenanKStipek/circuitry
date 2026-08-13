#!/usr/bin/env python3
"""Compile ``changelog.d/`` fragments into ``CHANGELOG.md``'s Unreleased section.

Every PR drops a new file ``changelog.d/<id>.<type>.md`` instead of editing
``CHANGELOG.md``, so two PRs in flight never touch the same path and never
conflict. At release time this script merges those fragments into
``## [Unreleased]`` and deletes them.

    python scripts/build-changelog.py             # compile + delete fragments
    python scripts/build-changelog.py --dry-run   # print the result, write nothing
    python scripts/build-changelog.py --keep      # compile but keep the fragments
    python scripts/build-changelog.py --check     # validate fragments only

Ordering is deterministic — Keep a Changelog section order, then filename
within a section — so the same fragments always compile to the same bytes.

Standard library only, by design: this runs in CI and at release time without
installing anything.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import NamedTuple, Sequence

REPO_ROOT = Path(__file__).resolve().parent.parent

#: Fragment types, in the order their sections appear in the changelog.
SECTION_ORDER = ("added", "changed", "deprecated", "removed", "fixed", "security")

#: ``<type>`` -> ``### <Title>``.
SECTION_TITLES = {name: name.capitalize() for name in SECTION_ORDER}

UNRELEASED_HEADING = "## [Unreleased]"

BULLET_PREFIXES = ("- ", "* ", "-\t", "*\t")


class Fragment(NamedTuple):
    """A single ``changelog.d/<identifier>.<type>.md`` file."""

    path: Path
    identifier: str
    type: str
    text: str

    @property
    def sort_key(self) -> tuple[int, str]:
        return (SECTION_ORDER.index(self.type), self.path.name)


class ChangelogError(Exception):
    """A fragment or changelog file the compiler refuses to guess about."""


# --------------------------------------------------------------------------
# Fragment discovery
# --------------------------------------------------------------------------


def parse_fragment_name(name: str) -> tuple[str, str] | None:
    """Split ``123.added.md`` into ``("123", "added")``.

    Returns ``None`` for anything that is not a fragment (``README.md``,
    dotfiles, non-markdown files), so the directory can hold documentation
    alongside the fragments.
    """
    if not name.endswith(".md") or name.startswith("."):
        return None
    identifier, _, type_ = name[: -len(".md")].rpartition(".")
    if not identifier or type_ not in SECTION_ORDER:
        return None
    return identifier, type_


def collect_fragments(fragments_dir: Path) -> list[Fragment]:
    """Return every fragment in ``fragments_dir``, in compile order."""
    if not fragments_dir.is_dir():
        return []
    fragments = []
    for path in sorted(fragments_dir.iterdir()):
        if not path.is_file():
            continue
        parsed = parse_fragment_name(path.name)
        if parsed is None:
            continue
        identifier, type_ = parsed
        fragments.append(
            Fragment(
                path=path,
                identifier=identifier,
                type=type_,
                text=path.read_text(encoding="utf-8"),
            )
        )
    return sorted(fragments, key=lambda fragment: fragment.sort_key)


def unknown_fragment_files(fragments_dir: Path) -> list[Path]:
    """Markdown files in ``fragments_dir`` that are neither fragments nor README."""
    if not fragments_dir.is_dir():
        return []
    return [
        path
        for path in sorted(fragments_dir.iterdir())
        if path.is_file()
        and path.name != "README.md"
        and not path.name.startswith(".")
        and parse_fragment_name(path.name) is None
    ]


def normalise_entry(text: str) -> list[str]:
    """Turn a fragment's body into changelog bullet lines.

    Already-bulleted fragments pass through verbatim. Plain prose gets a
    ``- `` prefix, with continuation lines indented so the markdown list
    stays well-formed.
    """
    lines = [line.rstrip() for line in text.strip("\n").splitlines()]
    while lines and not lines[0].strip():
        lines.pop(0)
    while lines and not lines[-1].strip():
        lines.pop()
    if not lines:
        return []
    if lines[0].startswith(BULLET_PREFIXES):
        return lines
    out = ["- " + lines[0].lstrip()]
    for line in lines[1:]:
        out.append("  " + line.lstrip() if line.strip() else "")
    return out


# --------------------------------------------------------------------------
# Changelog surgery
# --------------------------------------------------------------------------


class _Subsection(NamedTuple):
    title: str
    lines: list[str]

    @property
    def rank(self) -> int:
        key = self.title.strip().lower()
        if key in SECTION_ORDER:
            return SECTION_ORDER.index(key)
        return len(SECTION_ORDER)  # unknown sections sort last, order preserved


def _find_unreleased_bounds(lines: Sequence[str]) -> tuple[int, int]:
    """Return ``(heading_index, end_index)`` for the Unreleased section.

    ``end_index`` is exclusive and points at the next ``## `` heading (or the
    end of the file).
    """
    start = None
    for index, line in enumerate(lines):
        if line.strip() == UNRELEASED_HEADING:
            start = index
            break
    if start is None:
        raise ChangelogError(f"No {UNRELEASED_HEADING!r} heading found in the changelog")
    for index in range(start + 1, len(lines)):
        if lines[index].startswith("## "):
            return start, index
    return start, len(lines)


def _split_subsections(body: Sequence[str]) -> tuple[list[str], list[_Subsection]]:
    """Split an Unreleased body into (preamble, subsections)."""
    preamble: list[str] = []
    subsections: list[_Subsection] = []
    current: _Subsection | None = None
    for line in body:
        if line.startswith("### "):
            current = _Subsection(title=line[len("### ") :].strip(), lines=[])
            subsections.append(current)
        elif current is None:
            preamble.append(line)
        else:
            current.lines.append(line)
    return preamble, subsections


def _trimmed(lines: Sequence[str]) -> list[str]:
    out = list(lines)
    while out and not out[0].strip():
        out.pop(0)
    while out and not out[-1].strip():
        out.pop()
    return out


def _insertion_index(subsections: Sequence[_Subsection], rank: int) -> int:
    """Where a new subsection of ``rank`` belongs among existing ones."""
    for index, subsection in enumerate(subsections):
        if subsection.rank > rank:
            return index
    return len(subsections)


def compile_changelog(changelog_text: str, fragments: Sequence[Fragment]) -> str:
    """Return ``changelog_text`` with ``fragments`` merged into Unreleased.

    Existing entries are preserved; new entries are appended to the end of
    their section, and missing sections are created in canonical order.
    """
    if not fragments:
        return changelog_text

    lines = changelog_text.split("\n")
    start, end = _find_unreleased_bounds(lines)
    preamble, subsections = _split_subsections(lines[start + 1 : end])

    for fragment in sorted(fragments, key=lambda fragment: fragment.sort_key):
        entry = normalise_entry(fragment.text)
        if not entry:
            continue
        title = SECTION_TITLES[fragment.type]
        existing = next(
            (s for s in subsections if s.title.strip().lower() == fragment.type),
            None,
        )
        if existing is None:
            existing = _Subsection(title=title, lines=[])
            subsections.insert(_insertion_index(subsections, SECTION_ORDER.index(fragment.type)), existing)
        # No blank line between bullets — a blank line would turn the section
        # into a markdown "loose list" and change how existing entries render.
        body = _trimmed(existing.lines)
        body.extend(entry)
        existing.lines[:] = body

    rebuilt = [UNRELEASED_HEADING]
    preamble_body = _trimmed(preamble)
    if preamble_body:
        rebuilt.extend(["", *preamble_body])
    for subsection in subsections:
        body = _trimmed(subsection.lines)
        rebuilt.extend(["", f"### {subsection.title}"])
        if body:
            rebuilt.extend(["", *body])
    rebuilt.append("")

    return "\n".join([*lines[:start], *rebuilt, *lines[end:]])


# --------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------


def _check(fragments_dir: Path, fragments: Sequence[Fragment]) -> int:
    problems: list[str] = []
    for path in unknown_fragment_files(fragments_dir):
        problems.append(
            f"{path}: not a valid fragment name — expected <id>.<type>.md with "
            f"<type> in {', '.join(SECTION_ORDER)}"
        )
    for fragment in fragments:
        if not normalise_entry(fragment.text):
            problems.append(f"{fragment.path}: fragment is empty")
    for problem in problems:
        print(f"error: {problem}", file=sys.stderr)
    if problems:
        return 1
    print(f"{len(fragments)} fragment(s) OK in {fragments_dir}")
    return 0


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--fragments-dir",
        type=Path,
        default=REPO_ROOT / "changelog.d",
        help="directory holding the fragments (default: changelog.d/)",
    )
    parser.add_argument(
        "--changelog",
        type=Path,
        default=REPO_ROOT / "CHANGELOG.md",
        help="changelog to compile into (default: CHANGELOG.md)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print the compiled Unreleased section; write and delete nothing",
    )
    parser.add_argument(
        "--keep",
        action="store_true",
        help="write the changelog but keep the fragment files",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="validate fragment names and contents; write and delete nothing",
    )
    args = parser.parse_args(argv)

    fragments = collect_fragments(args.fragments_dir)

    if args.check:
        return _check(args.fragments_dir, fragments)

    if not fragments:
        print(f"No fragments in {args.fragments_dir} — nothing to compile.")
        return 0

    try:
        original = args.changelog.read_text(encoding="utf-8")
        compiled = compile_changelog(original, fragments)
    except (ChangelogError, OSError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    if args.dry_run:
        lines = compiled.split("\n")
        start, end = _find_unreleased_bounds(lines)
        print("\n".join(lines[start:end]).rstrip())
        return 0

    args.changelog.write_text(compiled, encoding="utf-8")
    print(f"Compiled {len(fragments)} fragment(s) into {args.changelog}:")
    for fragment in fragments:
        print(f"  {fragment.type:<10} {fragment.path.name}")
        if not args.keep:
            fragment.path.unlink()
    if args.keep:
        print("(fragments kept — rerun without --keep to delete them)")
    return 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
