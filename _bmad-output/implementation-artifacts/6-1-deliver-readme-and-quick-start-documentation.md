# Story 6.1: Deliver README and Quick-Start Documentation

Status: review

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a new user,
I want clear README onboarding documentation,
so that I can install, configure, and run first orchestration quickly.

## Acceptance Criteria

1. **Given** a fresh user with baseline prerequisites  
   **When** they follow README onboarding instructions  
   **Then** they can run example orchestrations successfully without external guidance.
2. **Given** onboarding documentation  
   **When** users learn execution paths  
   **Then** documentation clearly covers CLI and embedded Python usage.

## Tasks / Subtasks

- [x] Tighten README quick-start flow to be executable end-to-end for a first run (AC: 1)
- [x] Align programmatic Python snippets with actual exported API names and signatures (AC: 2)
- [x] Add explicit prerequisites and environment expectations for dry-run vs live adapter execution (AC: 1)
- [x] Add verification checklist for CLI + embedded path completion in onboarding docs (AC: 1, 2)
- [x] Add doc validation checks to catch drift between README examples and runtime behavior (AC: 1, 2)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 6 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/development-guide.md`.
- Existing implementation/documentation paths: `README.md`, `docs/`, `src/circuitry/cli/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- README includes installation, CLI usage, embedded usage, and examples overview.
- Development guide includes prerequisites and concrete local commands for run/validate/inspect.
- Gaps to close for this story:
- No automated doc-example verification currently ensures README snippets remain executable against code.
- Onboarding flow did not explicitly separate no-network dry-run first success from live provider setup path.

### Source Code Anchors

- `README.md:108`
- `README.md:132`
- `README.md:172`
- `docs/development-guide.md:70`
- `tests/docs/test_documentation_contracts.py:1`
- `src/circuitry/__init__.py:1`

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

- Keep changes scoped to existing module boundaries (`README`, `docs`, and minimal runtime API touchpoints only if needed).
- Avoid introducing duplicate orchestration execution paths.
- Add tests in a dedicated `tests/` tree when implementing executable doc-validation checks.

### Testing Requirements

- Add checks tied to story acceptance criteria (CLI quick-start and embedded path).
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
- `docs/api-reference.md`
- `tests/docs/test_documentation_contracts.py`
- `src/circuitry/cli/app.py`

## Dev Agent Record

### Agent Model Used

GPT-5 Codex

### Debug Log References

- Workflow: `_bmad/bmm/workflows/4-implementation/create-story/workflow.yaml`
- Instructions: `_bmad/bmm/workflows/4-implementation/create-story/instructions.xml`

### Completion Notes List

- Tightened README onboarding with explicit dry-run/live expectations and a quick verification checklist.
- Added explicit embedded API surface guidance with link to API reference.
- Added docs conformance tests to detect onboarding/symbol drift.

### File List

- `_bmad-output/implementation-artifacts/6-1-deliver-readme-and-quick-start-documentation.md`
- `README.md`
- `docs/development-guide.md`
- `docs/api-reference.md`
- `tests/docs/test_documentation_contracts.py`
