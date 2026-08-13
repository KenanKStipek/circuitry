# Releasing circuitry-cof

Releases ship to PyPI as `circuitry-cof` (the import name is `circuitry`).
The release process is fully automated by `.github/workflows/release.yml` —
pushing a tag matching `v<major>.<minor>.<patch>` (with optional `rcN`
suffix for pre-releases) triggers the workflow, which runs the test
matrix, builds the wheel + sdist, publishes to PyPI via OIDC trusted
publishing, and creates a GitHub Release with the changelog excerpt.

## One-time setup (per repository)

PyPI trusted publishing must be configured **once**. After it's set
up, every release publishes without storing any secret in this repo.

1. Create the PyPI project (publish v0.1.0 manually the first time, OR
   register the name first via `pypi.org` UI). Once the project exists:

2. Visit <https://pypi.org/manage/project/circuitry-cof/settings/publishing/>
   and add a "GitHub Actions" trusted publisher with:

   | Field | Value |
   | --- | --- |
   | Owner | `KenanKStipek` |
   | Repository name | `circuitry` |
   | Workflow filename | `release.yml` |
   | Environment name | `pypi` |

3. Verify the GitHub `pypi` environment exists (it's referenced by
   `release.yml`). It will be created on the first run automatically;
   no protection rules required.

That's it — no API tokens, no GitHub secrets needed.

## Cutting a release

The release process is a 4-step ritual:

### 1. Verify CI is green on `main`

```bash
gh run list --branch main --limit 1
# look for the most recent quality.yml run; should be 'success'
```

If quality is red, fix it on `main` before tagging.

### 2. Bump the version

Edit `pyproject.toml`:

```toml
[project]
version = "0.2.0"   # was 0.1.0
```

Decide the bump per [SemVer](https://semver.org/) — with the alpha
caveat documented in [`docs/stability.md`](docs/stability.md) that 0.x
minor bumps may include breaking changes:

* `MAJOR` — once we hit 1.0, breaking changes to the public API.
* `MINOR` (0.x) — new features OR breaking changes within the alpha
  contract.
* `PATCH` — bug fixes and additions that strictly preserve the public
  API surface.

### 3. Compile the changelog fragments, then cut the version section

Contributors add release notes as fragments under
[`changelog.d/`](changelog.d/README.md) — one new file per PR, so parallel PRs
never conflict on `CHANGELOG.md`. Compile them first:

```bash
python scripts/build-changelog.py --dry-run   # preview the combined section
python scripts/build-changelog.py             # merge into Unreleased, delete the fragments
```

The compiler is deterministic (section order, then filename) and only touches
the `## [Unreleased]` section; released sections are left alone.

Then move `Unreleased` to the new version — rename the heading and add
today's date:

```markdown
## [Unreleased]

### Added

(empty — keep this section here for the next round of work)

## [0.2.0] — 2026-05-09

### Added

- ... (the entries that were under Unreleased)
```

The exact heading format `## [<version>] — <YYYY-MM-DD>` is what the
release workflow extracts to populate the GitHub Release notes.

### 4. Commit, tag, push

```bash
git add pyproject.toml CHANGELOG.md changelog.d   # includes the consumed fragments
git commit -m "release v0.2.0"
git tag v0.2.0
git push origin main
git push origin v0.2.0
```

The workflow then:

1. Runs the test matrix on Python 3.9–3.13.
2. Verifies the tag matches `pyproject.toml`'s version (catches
   forgot-to-bump tags before they ship).
3. Builds wheel + sdist and runs `twine check` on them.
4. Uploads to PyPI via OIDC trusted publishing.
5. Creates a GitHub Release with the changelog excerpt and the built
   artifacts attached.

Watch progress at:

```bash
gh run watch
```

A typical end-to-end takes 4–6 minutes.

## Pre-releases

Use an `rcN` suffix for release candidates:

```bash
git tag v0.2.0rc1
git push origin v0.2.0rc1
```

The workflow marks these as `--prerelease` on GitHub. PyPI accepts the
release but `pip install circuitry-cof` won't pick it up by default
(consumers need `pip install --pre`).

## Yanking a bad release

If a published version has a critical issue, **yank** it on PyPI
(`pypi.org/manage/project/circuitry-cof/release/<version>/`) rather
than re-tagging the same version (PyPI rejects re-uploads).

Cut a `PATCH` bump for the fix. Yanking keeps users who already
installed the bad version working, but blocks fresh `pip install`
from selecting it.

## Local pre-flight (optional)

To smoke-test the build before tagging:

```bash
python -m pip install --upgrade build twine
python -m build
python -m twine check dist/*
ls -la dist/   # circuitry_cof-<version>.tar.gz + .whl
```

## Files involved

| File | Purpose |
| --- | --- |
| `.github/workflows/release.yml` | The automation |
| `.github/workflows/quality.yml` | Test / lint / typecheck on every push and PR |
| `pyproject.toml` | Version source of truth |
| `CHANGELOG.md` | Per-release notes; the workflow extracts from here |
| `changelog.d/` | Per-PR changelog fragments, compiled into `CHANGELOG.md` at release |
| `scripts/build-changelog.py` | The fragment compiler |
| `scripts/check-changelog.py` | CI gate: fragment required, no direct Unreleased edits |
| `RELEASING.md` | This document |
