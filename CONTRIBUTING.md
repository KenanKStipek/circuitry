# Contributing to Circuitry

Thanks for your interest in contributing! Circuitry is in early alpha, so we are happy to receive bug reports, small fixes, additional bundled orchestrations, and adapter or tool plugins.

Please skim this document before opening your first PR.

## Ground rules

- Be patient and constructive. The project follows our [Code of Conduct](CODE_OF_CONDUCT.md).
- Open an issue or a Discussion before starting any non-trivial PR — this avoids duplicated work and helps confirm scope.
- Questions, ideas, and "show and tell" belong in [GitHub Discussions](https://github.com/kenankstipek/circuitry/discussions). There is no Discord or Slack at this time.

## Development setup

Circuitry targets Python 3.9–3.13.

```bash
git clone https://github.com/kenankstipek/circuitry.git
cd circuitry
python -m venv .venv
source .venv/bin/activate
pip install -e .
pip install -r requirements-dev.txt
```

Verify the install:

```bash
cof version
cof doctor
```

## Running checks

The full quality gate matches CI:

```bash
pytest -q --cov=circuitry          # tests + coverage
ruff check .                       # lint
mypy src                           # static types
```

If you only changed one area, run the targeted suite first (e.g. `pytest tests/cli -q`) and run the full suite before pushing.

## Changelog fragments

Do **not** edit `CHANGELOG.md`'s `## [Unreleased]` section. With several PRs in
flight, every one of them appends to the same hunk and every one of them
conflicts the moment the first merges. Instead, add a **new file** under
[`changelog.d/`](changelog.d/README.md):

```
changelog.d/<issue-or-pr-number>.<type>.md
```

`<type>` is one of `added`, `changed`, `deprecated`, `removed`, `fixed`,
`security`. The file holds just the entry you would otherwise have written:

```markdown
- **`cof run --profile`.** Run-level adapter/model defaults from `profiles/<name>.yml`.
```

Because each PR writes its own path, parallel PRs merge in any order without
conflicts. Verify locally:

```bash
python scripts/build-changelog.py --check     # fragment names + contents
python scripts/build-changelog.py --dry-run   # the section your entry lands in
```

CI enforces this: a PR that changes code under `src/` or `scripts/` (or bumps
`pyproject.toml`) must add a fragment, and no PR may edit the Unreleased section
directly. If a change genuinely has nothing to announce, add the `no-changelog`
label or put `[skip-changelog]` in the PR title.

At release time a maintainer runs `python scripts/build-changelog.py`, which
merges every fragment into `## [Unreleased]` (section order, then filename) and
deletes them. See [RELEASING.md](RELEASING.md).

Same reasoning applies to other shared lists — add new `docs/index.md` links at
the **end** of their section rather than in the middle.

## Commit message format

We use [Conventional Commits](https://www.conventionalcommits.org/) for the subject line. The release pipeline derives the next version from these prefixes, so the format is enforced.

```
<type>(<optional scope>): <short summary>

[optional body]

[optional footer(s)]
```

Common types: `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, `perf`, `ci`, `build`.

Breaking changes get a `!` after the type/scope, e.g. `feat(adapter)!: drop legacy positional args`, plus a `BREAKING CHANGE:` footer.

## Pull requests

Before opening a PR:

- [ ] All tests pass locally (`pytest -q`)
- [ ] `ruff check .` and `mypy src` are clean
- [ ] You added or updated tests for any behavior change
- [ ] You added a changelog fragment — a **new file** `changelog.d/<issue-or-pr>.<type>.md` — rather than editing [`CHANGELOG.md`](CHANGELOG.md) (see [Changelog fragments](#changelog-fragments))
- [ ] If you changed the public API surface (see [`docs/stability.md`](docs/stability.md)), the change is intentional and called out in the PR description
- [ ] If you changed bundled orchestrations under `src/circuitry/bundled/orchestrations/`, you re-ran `scripts/sync-bundled` so the repo-root copies match

Use the PR template that appears when you open a pull request — it covers the same items.

## Areas that welcome contribution

- Additional adapters (Mistral direct, Cohere, etc.)
- Additional tool plugins (HTTP, file I/O, image utilities)
- Additional bundled orchestrations
- Documentation, examples, typo fixes, and asciinema recordings
- Issues labeled [`good first issue`](https://github.com/kenankstipek/circuitry/labels/good%20first%20issue)

## Reporting a security issue

Please do **not** open a public issue. See [SECURITY.md](SECURITY.md) for the private disclosure process.

## License

By contributing, you agree that your contributions will be licensed under the MIT License (see [LICENSE](LICENSE)).
