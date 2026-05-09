## Summary

<!-- A short description of what this PR does and why. -->

## Type

- [ ] Bug fix (`fix:`)
- [ ] New feature (`feat:`)
- [ ] Refactor (`refactor:`)
- [ ] Documentation (`docs:`)
- [ ] Test (`test:`)
- [ ] Chore / build / CI (`chore:` / `build:` / `ci:`)
- [ ] Breaking change (`!` suffix; include `BREAKING CHANGE:` footer)

## Linked issues

<!-- e.g. Fixes #123, Refs #456 -->

## Test plan

<!-- How a reviewer can verify this works. -->

- [ ] `pytest -q` passes locally
- [ ] `ruff check .` is clean
- [ ] `mypy src` is clean
- [ ] Manual verification: <describe what you ran>

## Checklist

- [ ] Conventional Commits subject (`<type>(<scope>): <summary>`)
- [ ] Tests added or updated for the behavior change
- [ ] `CHANGELOG.md` updated under `## [Unreleased]`
- [ ] If bundled orchestrations changed, `scripts/sync-bundled` re-run
- [ ] Public-API impact considered (`docs/stability.md`); breaking change called out above if applicable
- [ ] Documentation updated (`docs/`, README, inline help)

## Notes for reviewers

<!-- Anything reviewers should pay particular attention to, alternatives considered, follow-ups planned, etc. -->
