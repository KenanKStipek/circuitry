# Shared Library Growth and Governance

This document defines the long-term workflow for scaling shared-library contributions.

## Contribution Lifecycle

1. Proposal
   - Contributor proposes new asset or version update with use case and compatibility notes.
2. Authoring
   - Contributor adds asset files using the shared contract (`<asset_id>/<version>.yml` and optional `.json` metadata).
3. Validation
   - Run required checks from `docs/shared-library-contributions.md`.
4. Review
   - Maintainers review metadata quality, deterministic state-path behavior, and compatibility claims.
5. Merge and Release
   - PR merge publishes the new version in the shared-library repository.
6. Consumption Verification
   - Consumer teams verify retrieval/execution via `fetch` and `run-library`.
7. Deprecation
   - Deprecations are announced with migration guidance; prior versions remain available per policy.

## Governance Policy

- Ownership:
  - Every asset must include an owning team or maintainer.
- Compatibility:
  - Metadata must state runtime/provider compatibility expectations.
- Breaking Changes:
  - Breaking behavior requires a major version bump and migration notes.
- Review Authority:
  - At least one maintainer approval required before merge.

## Quality Gates

- Required checks:
  - `python -m circuitry.cli.app validate <orchestration.yml>`
  - `python -m circuitry.cli.app inspect <orchestration.yml>`
  - `pytest`
  - `ruff check .`
  - `mypy src`
- Determinism requirement:
  - State-path structure must remain deterministic and rooted at `prime`.

## Post-MVP Automation Roadmap

Phase 1:
- Enforce metadata schema in CI.
- Enforce version format and duplicate-version prevention checks.
- Auto-run retrieval smoke tests against consumer repo.

Phase 2:
- Policy-as-code checks for ownership, compatibility fields, and changelog presence.
- Security/dependency gates for plugin/tool references.
- Automated release notes for new or deprecated assets.

Phase 3:
- Cross-repo compatibility matrix checks (consumer/runtime versions vs library assets).
- Drift detection for stale compatibility declarations.
- Automated deprecation reminders and migration health reports.

## Maintainership Metrics

Track at minimum:
- PR lead time (open to merge)
- Validation failure rate
- Rework rate (follow-up fixes within 7 days)
- Adoption rate (number of consumer services per asset)
- Deprecation completion rate

## Operating Cadence

- Weekly:
  - Triage open contribution PRs and failed validations.
- Monthly:
  - Review adoption and deprecation metrics.
- Quarterly:
  - Revisit compatibility and versioning policy effectiveness.
