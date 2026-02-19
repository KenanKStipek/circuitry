# Testing Policy

## Purpose

This policy defines the minimum test and quality requirements for every implementation story.

## Definition of Done for Any Story

A story is not complete unless all of the following are true:

1. New or changed behavior has automated tests.
2. Existing tests that cover adjacent behavior are updated when needed.
3. All quality gates pass locally:
   - `pytest`
   - `ruff check .`
   - `mypy src`
4. If state-path behavior is touched, tests include deterministic path assertions.
5. If user-facing behavior is touched (CLI/API/docs examples), at least one integration-style validation is included.

## Per-Story Test Planning Checklist

Before coding:

1. List acceptance criteria.
2. Map each acceptance criterion to at least one test case.
3. Identify regression risk areas and add targeted tests.

During coding:

1. Add tests with the code change, not after.
2. Keep fixtures minimal and focused on behavior under test.
3. Prefer deterministic assertions over snapshot-only assertions.

Before moving story status to `review`:

1. Run `pytest`.
2. Run `ruff check .`.
3. Run `mypy src`.
4. Confirm all new tests pass.
5. Confirm no unrelated failures were introduced.

## Required Story Record Update

When finishing a story, include in the completion notes:

1. Which tests were added/updated.
2. Which quality gates were run.
3. Whether deterministic path assertions were included (if applicable).

## Current Priority for This Repo

Given current runtime goals, priority order is:

1. Compiler/runtime correctness tests.
2. Deterministic state-path contract tests.
3. CLI/API behavior tests.
4. Documentation/example drift checks.
