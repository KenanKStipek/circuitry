# Shared Library Contribution Workflow

This repository does not publish shared-library assets.

Shared-library publication is done by pull request in the separate library repository.  
This document defines the contract contributors should follow so assets can be retrieved and executed by Circuitry.

## Scope and Ownership

- Source-of-truth for shared assets: external shared-library repository.
- Consumer runtime (this repo): retrieval and execution only (`fetch`, `run-library`).
- Asset owners are responsible for metadata accuracy, semantic versioning, and compatibility notes.

## Asset Contract

Required layout in the shared-library repository:

- `<asset_id>/<version>.yml` or `<asset_id>/<version>.yaml`
- Optional metadata sidecar: `<asset_id>/<version>.json`

Recommended metadata fields in sidecar JSON:

- `name`: human-readable asset name
- `owner`: team or maintainer id
- `tags`: list of discovery tags
- `version`: semantic version string matching file version
- `compatibility`: compatibility notes (runtime/provider expectations)
- `summary`: short description

## Versioning Rules

- Use semantic versioning (`MAJOR.MINOR.PATCH`).
- Bump `MAJOR` for breaking state-path or behavior changes.
- Bump `MINOR` for backward-compatible features.
- Bump `PATCH` for backward-compatible fixes.
- Never rewrite an existing published version file; add a new version.

## PR Merge Gates

Before merge in the library repository, run these checks against the contributed orchestration:

1. Compile and shape validation:
   - `python -m circuitry.cli.app validate path/to/orchestration.yml`
2. Structural inspection:
   - `python -m circuitry.cli.app inspect path/to/orchestration.yml`
3. Repo quality gates:
   - `pytest`
   - `ruff check .`
   - `mypy src`
4. Retrieval readiness smoke check from consumer repo:
   - `python -m circuitry.cli.app fetch <asset_id> --version <version> --out /tmp/asset.yml`
   - `python -m circuitry.cli.app run-library <asset_id> --version <version> --dry-run`

## Review Checklist

- Asset file path and version format follow contract.
- Metadata sidecar fields are complete and searchable.
- State paths remain deterministic and rooted under `prime`.
- Compatibility notes reflect adapter/model/runtime assumptions.
- Quality gates pass and retrieval smoke check succeeds.

## Deprecation and Breaking Changes

- Keep prior major versions available unless policy explicitly retires them.
- Mark deprecated assets in metadata with migration guidance.
- Include replacement asset/version recommendations in PR description.
