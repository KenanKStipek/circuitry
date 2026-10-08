# Circuitry — Agent Briefing

## What it is
Cybernetic orchestration framework (Python). Core library in `src/`, tests in
`tests/`, bundled orchestration curation in `src/circuitry/curation/`, docs in
`docs/`. `electricity/` is a preview Rust runner for the same orchestrations
(its own Cargo workspace; see `electricity/README.md` and `electricity/DESIGN.md`).

## Minimum verification for any change
```
pip install -e ".[tools]" && pip install -r requirements-dev.txt
pytest -q -m 'not integration'
ruff check .
mypy src
bash scripts/smoke-curation.sh
python scripts/sync-bundled-docs.py --check
python scripts/generate-conformance-cases.py --check
for g in electricity/scripts/generate_*.py; do python "$g" --check; done
```
CI is `.github/workflows/quality.yml` (pytest matrix 3.10–3.13, ruff, mypy,
smoke). A PR is shippable when every check is green.

The last line checks the files `electricity/` copies from, or generates with,
Circuitry's own code (its schemas, and expected outputs from the YAML, JSON,
template, CEL and schema code). A change to that code can make one stale even
when nothing under `electricity/` changed; CI checks it in
`.github/workflows/electricity-generated.yml`. Run the stale generator without
`--check` and commit the result; if electricity's Rust tests then fail, the
change altered behaviour electricity reproduces, so say so in the PR.

Install from `requirements-dev.txt` before running any of the above — don't
trust a `ruff`/`mypy` already on `PATH`. Tool versions are pinned there
specifically so "green locally" means "green in CI"; a stray unpinned
install can pass or fail on findings CI won't reproduce.

The suite is hermetic to your machine's `~/.config/circuitry/config.json` —
an autouse fixture in `tests/conftest.py` redirects global-config discovery
to a per-test temp dir, so a real global config (e.g. a restrictive
`enabled_adapters` allowlist) can't change test outcomes. A test that
intentionally exercises config discovery tiers opts out with
`@pytest.mark.real_config_discovery` and constructs its own layering
explicitly.

No test can reach a live, metered third-party API by accident. Ordinary
integration tests (local/offline-reachable backends — Ollama, SurrealDB, a
test Postgres, ...) stay gated behind `CIRCUITRY_RUN_INTEGRATION=1` plus
`-m integration` as before. `tests/integration/test_cyberdiner_live.py`
(the only tests that spend a real request against a live production service)
need a further, distinct `CIRCUITRY_LIVE_TESTS=1` — credentials
(`CYBERDINER_TOKEN`/`CYBERDINER_EXPO_URL`) being present in the environment
is deliberately never enough on its own to run them (#266). Never set
`CIRCUITRY_LIVE_TESTS` for a routine test run.

## Rust (`electricity/`)
Any change under `electricity/` additionally needs, run from `electricity/`:
```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```
Use the installed stable toolchain as-is — never `cargo install`, `rustup
target add/toolchain install`, or any other global install; cross-compiles
(musl targets, other OSes) are verified in CI only (`.github/workflows/electricity.yml`).
The workspace `version` in `electricity/Cargo.toml` must always equal
`pyproject.toml`'s (checked by a Python test and by the release workflow's tag
check); bump both together. Build output stays in `electricity/target/`
(gitignored). A test that runs the `electricity` binary gets a temporary HOME
and the credential variables removed, exactly like `cof` subprocesses.

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
Work is managed via GitHub issues and done in local agent sessions (Pi).
The GitHub-hosted agent loop is off: the `dispatch`, `claude`, `finalize` and
`claude-code-review` workflows (vendored from agent-loop) are disabled in
Actions, so nothing on GitHub picks up issues, answers `@claude`, or reviews
PRs. `gh workflow enable <file>` turns one back on. `accept.yml` (a `/accept`
comment or the `accepted` label merges a green PR) and `rollup.yml` still run.

Labels:
- `ready-for-review` — the issue's PR is done; the owner's turn
- `needs-human` — blocked on a decision; stop and ask
- `blocked` / `hold` — do not work the issue at all

Strip these when an issue closes. `agent-ready`, `working`, `needs-plan` and
`in-loop` belonged to the GitHub loop and no longer trigger anything.

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
Never merge a PR unless the owner has said to. Never force-push. Branch naming: `issue-<number>-<short-slug>`.
