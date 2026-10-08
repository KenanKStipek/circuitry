# Releasing circuitry-cof

Releases ship to PyPI as `circuitry-cof` (the import name is `circuitry`).
The release process is fully automated by `.github/workflows/release.yml` —
pushing a tag matching `v<major>.<minor>.<patch>` (with optional `rcN`
suffix for pre-releases) triggers the workflow, which runs the test
matrix, builds the wheel + sdist, publishes to PyPI via OIDC trusted
publishing, and creates a GitHub Release with the changelog excerpt.

The same tag also covers `electricity/`, the preview Rust runner in this
repository, and `oscilloscope/`, the preview `osp` TUI: the workflow builds
their release archives (Linux x86_64/aarch64 musl, macOS aarch64) in
parallel with the Python build, and attaches them to the same GitHub
release, marked as a **preview**. The container image
(`ghcr.io/kenankstipek/electricity`) is pushed by a dedicated
`publish-image` job only after the Python wheel/sdist and every
electricity and osp archive have built and the PyPI upload has succeeded —
nothing publishes anywhere (PyPI, ghcr.io, the GitHub release) unless
every artifact built, and a failed PyPI upload leaves no image behind. osp
has no container image.

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

The first push to `ghcr.io/kenankstipek/electricity` creates that package
as **private** by default; make it public once from the package's GitHub
settings (Package settings → Change visibility) so `docker pull` works
without authentication.

## Cutting a release

The release process is a 4-step ritual:

### 1. Verify CI is green on `main`

```bash
gh run list --branch main --limit 1
# look for the most recent quality.yml run; should be 'success'
```

If quality is red, fix it on `main` before tagging.

### 2. Bump the version

Edit `pyproject.toml`, `electricity/Cargo.toml` **and**
`oscilloscope/Cargo.toml` — one version covers all three, and the release
workflow's tag check fails if any of them disagree (so does
`tests/test_electricity_version.py`):

```toml
# pyproject.toml
[project]
version = "0.2.0"   # was 0.1.0
```

```toml
# electricity/Cargo.toml
[workspace.package]
version = "0.2.0"   # was 0.1.0
```

```toml
# oscilloscope/Cargo.toml
[workspace.package]
version = "0.2.0"   # was 0.1.0
```

Then refresh `oscilloscope/Cargo.lock` the same way you would
`electricity/Cargo.lock` — `cd oscilloscope && cargo update --workspace`,
or just let the next `cargo build`/`cargo test` there rewrite it, since no
dependency version changed, only the workspace's own.

For a release-candidate tag, the Cargo files spell the version differently
from `pyproject.toml`: it uses PEP 440 (`0.2.0rc1`), but Cargo requires
valid semver, so `electricity/Cargo.toml` and `oscilloscope/Cargo.toml` both
use `0.2.0-rc.1`. The release workflow's `verify-version` job and
`tests/test_electricity_version.py` both map one form to the other before
comparing — see the "Pre-releases" section below.

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
git add pyproject.toml electricity/Cargo.toml electricity/Cargo.lock oscilloscope/Cargo.toml oscilloscope/Cargo.lock CHANGELOG.md changelog.d   # includes the consumed fragments
git commit -m "release v0.2.0"
git tag v0.2.0
git push origin main
git push origin v0.2.0
```

The workflow then:

1. Runs the test matrix on Python 3.9–3.13.
2. Verifies the tag matches `pyproject.toml`'s, `electricity/Cargo.toml`'s
   **and** `oscilloscope/Cargo.toml`'s version (catches forgot-to-bump tags
   before they ship).
3. Builds wheel + sdist and runs `twine check` on them, and in parallel
   builds electricity's and osp's release archives — the same reusable
   workflow builds both — and validates electricity's container image
   (without pushing it yet; osp has none).
4. Once every build succeeds: uploads to PyPI via OIDC trusted publishing,
   then pushes the container image to ghcr.io.
5. Creates a GitHub Release with the changelog excerpt, the Python
   artifacts, and electricity's and osp's archives/checksums, all attached.

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

Bump `pyproject.toml` to `0.2.0rc1` and `electricity/Cargo.toml` and
`oscilloscope/Cargo.toml` to `0.2.0-rc.1` (see step 2 above) — the tag
itself always matches `pyproject.toml`'s spelling.

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
| `.github/workflows/electricity.yml` | electricity's own fmt/clippy/test/MSRV CI, plus the release build without publishing |
| `.github/workflows/oscilloscope.yml` | osp's own fmt/clippy/test/MSRV CI, plus the release build without publishing |
| `.github/workflows/rust-release-build.yml` | Reusable workflow: a Rust workspace's release archives, and electricity's container image (`inputs.build-image`) |
| `pyproject.toml` | Python version source of truth |
| `electricity/Cargo.toml` | electricity's version (`[workspace.package]`), kept equal to `pyproject.toml`'s |
| `oscilloscope/Cargo.toml` | osp's version (`[workspace.package]`), kept equal to `pyproject.toml`'s |
| `CHANGELOG.md` | Per-release notes; the workflow extracts from here |
| `changelog.d/` | Per-PR changelog fragments, compiled into `CHANGELOG.md` at release |
| `scripts/build-changelog.py` | The fragment compiler |
| `scripts/check-changelog.py` | CI gate: fragment required, no direct Unreleased edits |
| `RELEASING.md` | This document |
