# Circuitry — Agent Briefing

## What it is
Cybernetic orchestration framework (Python). Core library in `src/`, tests in
`tests/`, bundled orchestration curation in `scripts/`, docs in `docs/`.

## Minimum verification for any change
```
pip install -e ".[tools]" && pip install -r requirements-dev.txt
pytest -q -m 'not integration'
ruff check .
mypy src
bash scripts/smoke-curation.sh
```
CI is `.github/workflows/quality.yml` (pytest matrix 3.10–3.13, ruff, mypy,
smoke). A PR is shippable when every check is green.

## Changelog — write a fragment, never edit `CHANGELOG.md`
Every change ships its release note as a **new file**, `changelog.d/<issue-or-pr>.<type>.md`
(`type` ∈ added / changed / deprecated / removed / fixed / security), containing
just the entry's markdown bullet. New file per PR = parallel PRs never conflict.

- Do **not** touch `CHANGELOG.md`'s `## [Unreleased]` section — CI rejects it.
  It is compiled from fragments at release time by `python scripts/build-changelog.py`.
- Nothing to announce (pure refactor, CI-only)? Add the `no-changelog` label or
  put `[skip-changelog]` in the PR title.
- Check your work locally: `python scripts/build-changelog.py --check` (validates
  fragments) and `--dry-run` (prints the compiled section).
- Same conflict logic for other shared lists: append new `docs/index.md` links at
  the **END** of their section, never mid-list.

See `changelog.d/README.md`.

## Agent workflow
Work is managed via GitHub issues; labels drive state and autonomy
(same scheme as CyberDiner).

State labels:
- `agent-ready` — spec complete, queued for autonomous pickup (dispatcher runs every 3h)
- `working` — a workflow run is executing right now. Harness-managed ONLY — never set or clear it manually
- `ready-for-review` — agent finished its part; human's turn
- `needs-human` — escalated; stop and wait
- `blocked` / `hold` — do not work the issue at all (also workflow-gated)

Autonomy labels:
- *(none)* — Direct to PR (default): state the plan in-session, implement, verify, open a PR
- `needs-plan` — post an implementation plan as an issue comment, add
  `ready-for-review`, and STOP. Resume only on a human @claude approval comment.

Label lifecycle: the moment the deliverable exists, swap `agent-ready` →
`ready-for-review` in one `gh issue edit`. Strip lifecycle labels when issues
close. Stale labels are bugs.

## Pull request protocol — open the PR FIRST, not last
Runs can die at any moment; a pushed branch with no PR is invisible work.
1. Push the first meaningful commit early, then immediately
   `gh pr create --draft` with `Closes #<n>`, one line of scope, and a
   `**WIP — run in progress**` marker in the body.
2. Keep committing and pushing small increments so partial progress survives.
3. On completion, finalize with `gh pr edit` (full summary, acceptance-criteria
   status, test plan, deviations — remove the WIP marker), then `gh pr ready`.
   Comment on the issue and swap its labels in the same breath.
4. If a previous run already opened a PR for this issue, resume that branch —
   never open a duplicate.
Never merge a PR. Never force-push. Branch naming: `issue-<number>-<short-slug>`.
