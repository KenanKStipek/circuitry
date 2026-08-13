# `changelog.d/` — changelog fragments

One file per pull request, so two PRs in flight never touch the same line of
`CHANGELOG.md` and never conflict with each other.

## Writing a fragment

Create **a new file** named `<issue-or-pr-number>.<type>.md`:

```
changelog.d/56.added.md
changelog.d/57.fixed.md
changelog.d/58.changed.md
```

`<type>` is one of the [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
sections:

| type | section |
| --- | --- |
| `added` | `### Added` |
| `changed` | `### Changed` |
| `deprecated` | `### Deprecated` |
| `removed` | `### Removed` |
| `fixed` | `### Fixed` |
| `security` | `### Security` |

The file contains just the entry's markdown — the same bullet you would
otherwise have written under `## [Unreleased]`:

```markdown
- **Changelog fragments.** Each PR now writes `changelog.d/<id>.<type>.md`
  instead of editing `CHANGELOG.md` directly.
```

A leading `- ` is optional; the compiler adds one (and indents continuation
lines) when the fragment is plain prose. Multiple bullets in one fragment are
fine.

Need several kinds of change in one PR? Write several fragments —
`56.added.md` *and* `56.fixed.md`.

## Do not edit `CHANGELOG.md`

The `## [Unreleased]` section is compiled from these fragments at release
time. Editing it directly re-introduces the conflict this directory exists to
prevent, and CI rejects it. Entries that predate this directory stay where
they are.

## Compiling (release time only)

```sh
python scripts/build-changelog.py            # merge fragments into CHANGELOG.md, delete them
python scripts/build-changelog.py --dry-run  # print the resulting Unreleased section, change nothing
python scripts/build-changelog.py --check    # validate fragment names/contents, change nothing
```

Ordering is deterministic: section order above, then filename within a
section. Existing `## [Unreleased]` entries are preserved; new entries are
appended to the end of their section.

This file is not a fragment — the compiler only reads `*.<type>.md`.
