# Story 6.3: Provide Curated Example Orchestrations

Status: review

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a developer evaluating Circuitry,
I want example orchestrations for real usage patterns,
so that I can adapt working patterns instead of starting from scratch.

## Acceptance Criteria

1. **Given** example orchestration artifacts in repository docs or examples  
   **When** users run and inspect them  
   **Then** examples demonstrate core primitives and operational modes clearly.
2. **Given** runtime evolution  
   **When** examples are maintained  
   **Then** examples are versioned and validated against current runtime behavior.

## Tasks / Subtasks

- [x] Curate example set to explicitly cover core primitives and practical composition patterns (AC: 1)
- [x] Add example metadata/readme index (intent, expected outputs, prerequisites, difficulty) (AC: 1)
- [x] Add versioning policy for examples and compatibility notes per runtime release changes (AC: 2)
- [x] Add automated checks to validate examples execute/inspect successfully in CI or local quality gate (AC: 2)
- [x] Add at least one multi-primitive scenario example demonstrating realistic orchestration composition (AC: 1, 2)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 6 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/development-guide.md`.
- Existing implementation/documentation paths: `examples/`, `README.md`, `src/circuitry/cli/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- Repository already contains six example orchestration YAMLs spanning core primitives.
- README and development guide already reference examples and demonstrate run commands.
- Gaps to close for this story:
- Example set needed explicit curation metadata and compatibility/version policy.
- Needed a broader automated run+inspect validation pass tied to curated set.

### Source Code Anchors

- `examples/hello.yml`
- `examples/dynamic_hello.yml`
- `examples/conditional_example.yml`
- `examples/loop_example.yml`
- `examples/typed_prompt_example.yml`
- `examples/reflector_v1.yml`
- `examples/multi_primitive_story.yml`
- `examples/README.md`
- `examples/manifest.json`
- `tests/examples/test_examples_smoke.py`

### Technical Requirements

- Keep compile/runtime behavior deterministic and aligned with declared DOL semantics.
- Preserve root and nested path composition under `prime` with named child segments.
- Any behavior change must include deterministic state-path assertions and failure-path diagnostics.

### Architecture Compliance

- Enforce deterministic orchestration-to-state mapping per State Path Contract.
- Do not introduce hidden/non-deterministic path segments.
- Maintain backward compatibility posture for state path format unless explicitly versioned.

### Library / Framework Requirements

- Python package layout under `src/`.
- Orchestration parsing via `PyYAML` from `requirements.txt`.
- CLI UX stack remains Typer + Rich where CLI behavior is involved.
- Quality gates remain pytest + ruff + mypy.

### File Structure Requirements

- Keep changes scoped to existing module boundaries (`examples`, `docs`, tests/tooling as needed).
- Avoid introducing duplicate orchestration execution paths.
- Add tests in a dedicated `tests/` tree when implementing executable example checks.

### Testing Requirements

- Add checks tied to story acceptance criteria (run + inspect for curated examples).
- Include deterministic state-path assertions and failure-diagnostics checks where runtime behavior is touched.
- Verify no regressions to existing example orchestration behavior.

### Project Structure Notes

- Current repo has no committed `tests/` tree; implementation should introduce one deliberately with focused scope.
- Preserve current runtime semantics where unchanged by explicit story acceptance criteria.

### References

- `_bmad-output/planning-artifacts/epics.md`
- `_bmad-output/planning-artifacts/architecture.md`
- `README.md`
- `docs/development-guide.md`
- `examples/`
- `examples/README.md`
- `examples/manifest.json`
- `tests/examples/test_examples_smoke.py`
- `src/circuitry/cli/app.py`

## Dev Agent Record

### Agent Model Used

GPT-5 Codex

### Debug Log References

- Workflow: `_bmad/bmm/workflows/4-implementation/create-story/workflow.yaml`
- Instructions: `_bmad/bmm/workflows/4-implementation/create-story/instructions.xml`

### Completion Notes List

- Added curated examples index with intent, prerequisites, difficulty, and expected state-path outcomes.
- Added example manifest with versioning/compatibility metadata.
- Added multi-primitive example and expanded automated validate/inspect/dry-run coverage.

### File List

- `_bmad-output/implementation-artifacts/6-3-provide-curated-example-orchestrations.md`
- `examples/README.md`
- `examples/manifest.json`
- `examples/multi_primitive_story.yml`
- `tests/examples/test_examples_smoke.py`
- `README.md`
- `docs/development-guide.md`
