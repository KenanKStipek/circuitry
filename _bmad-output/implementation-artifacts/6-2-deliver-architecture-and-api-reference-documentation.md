# Story 6.2: Deliver Architecture and API Reference Documentation

Status: review

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a developer integrator,
I want architecture and API references,
so that I can understand runtime boundaries and integrate correctly.

## Acceptance Criteria

1. **Given** official architecture and API docs  
   **When** a developer implements integration using those docs  
   **Then** they can discover core interfaces and expected behavior accurately.
2. **Given** runtime evolution  
   **When** architecture and API docs are maintained  
   **Then** documentation remains aligned with current runtime implementation.

## Tasks / Subtasks

- [x] Define and publish API reference for core integration surfaces (compiler/runtime/store/adapter boundary) (AC: 1)
- [x] Map architecture docs directly to concrete code paths and runtime flow checkpoints (AC: 1)
- [x] Add versioning/update guidance so API docs track runtime changes without drift (AC: 2)
- [x] Add examples showing canonical integration patterns and anti-patterns for API consumers (AC: 1)
- [x] Add doc quality gate to detect stale symbols/signatures in API reference content (AC: 2)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 6 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`, `docs/index.md`.
- Existing implementation/documentation paths: `docs/`, `src/circuitry/core/`, `src/circuitry/adapters/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- Architecture documentation exists with runtime flow and component boundaries.
- Project overview/index docs already provide top-level orientation and entry points.
- Core/adapters expose a basic import surface usable as API reference seed.
- Gaps to close for this story:
- API discoverability needed a single explicit reference source.
- No doc-to-code conformance check existed to catch symbol/signature drift.

### Source Code Anchors

- `docs/api-reference.md:1`
- `docs/architecture.md:35`
- `docs/index.md:17`
- `src/circuitry/__init__.py:1`
- `src/circuitry/adapters/__init__.py:1`
- `tests/docs/test_documentation_contracts.py:1`

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

- Keep changes scoped to existing module boundaries (`docs`, public module exports, tests/tooling as needed).
- Avoid introducing duplicate orchestration execution paths.
- Add tests in a dedicated `tests/` tree when implementing executable API doc conformance checks.

### Testing Requirements

- Add checks tied to story acceptance criteria (API discoverability and accuracy).
- Include deterministic state-path assertions and failure-diagnostics checks where runtime behavior is touched.
- Verify no regressions to existing example orchestration behavior.

### Project Structure Notes

- Current repo has no committed `tests/` tree; implementation should introduce one deliberately with focused scope.
- Preserve current runtime semantics where unchanged by explicit story acceptance criteria.

### References

- `_bmad-output/planning-artifacts/epics.md`
- `_bmad-output/planning-artifacts/architecture.md`
- `docs/architecture.md`
- `docs/project-overview.md`
- `docs/index.md`
- `docs/api-reference.md`
- `tests/docs/test_documentation_contracts.py`
- `src/circuitry/__init__.py`
- `src/circuitry/adapters/__init__.py`

## Dev Agent Record

### Agent Model Used

GPT-5 Codex

### Debug Log References

- Workflow: `_bmad/bmm/workflows/4-implementation/create-story/workflow.yaml`
- Instructions: `_bmad/bmm/workflows/4-implementation/create-story/instructions.xml`

### Completion Notes List

- Added dedicated API reference with stable exports, signatures, integration patterns, and anti-patterns.
- Added architecture runtime checkpoint map tied to concrete code paths.
- Added doc conformance test to enforce API symbol documentation drift checks.

### File List

- `_bmad-output/implementation-artifacts/6-2-deliver-architecture-and-api-reference-documentation.md`
- `docs/api-reference.md`
- `docs/architecture.md`
- `docs/index.md`
- `tests/docs/test_documentation_contracts.py`
