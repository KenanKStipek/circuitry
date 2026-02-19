# Test Matrix

This matrix defines minimum regression coverage for orchestration execution patterns.

## Execution Patterns

1. `chain` dynamic flow:
- Sibling effects can consume prior sibling writes.
- Canonical metadata stores `flow: chain`.

2. `tree` dynamic flow:
- Sibling effects execute against deterministic start-of-dynamic snapshot.
- Canonical metadata stores `flow: tree`.

3. Conditional:
- Named conditional writes under wrapper path.
- Transparent conditional writes directly under parent path.
- Branch selection and executed effect metadata are recorded.

4. Loop:
- Named loop writes wrapper + `iter_<n>` segments.
- Transparent loop writes directly under parent path.
- Iteration count, termination reason, and per-iteration executed effects are recorded.

5. Nested composition:
- Deeply nested failures include hierarchical effect-path breadcrumbs.
- State-path hierarchy remains deterministic.

## Quality Gates

Every story implementation must pass:

1. `pytest -q`
2. `ruff check .`
3. `mypy src`

CI workflow: `.github/workflows/quality.yml`.
