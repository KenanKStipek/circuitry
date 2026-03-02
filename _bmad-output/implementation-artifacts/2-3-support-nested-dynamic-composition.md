# Story 2.3: Support Nested Dynamic Composition

Status: done

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a developer,
I want to compose nested dynamics and mixed effect types,
so that I can model complex orchestration logic while preserving readability and predictability.

## Acceptance Criteria

1. **Given** a nested orchestration with prompts, dynamics, conditionals, and loops  \
   **When** execution runs through nested levels  \
   **Then** each node resolves context and state paths deterministically.
2. **Given** failures in nested execution paths  \
   **When** runtime captures the failure  \
   **Then** diagnostics identify hierarchical location for troubleshooting.
3. **Given** mixed nested effect composition  \
   **When** outputs are inspected  \
   **Then** path composition is derivable from orchestration object names with no hidden segments.

## Tasks / Subtasks

- [x] Validate nested composition execution for prompt/dynamic/conditional/loop combinations (AC: 1, 3)
- [x] Improve error context propagation to include nested scope/path where failures occur (AC: 2)
- [x] Ensure context handling across nested loops/conditionals is deterministic and non-leaky (AC: 1)
- [x] Add deep nested fixture tests with path assertions for all nested levels (AC: 1, 3)
- [x] Document nested composition patterns and anti-patterns for implementation consistency (AC: 2, 3)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 2 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/core/`, `src/circuitry/cli/`, `src/circuitry/core/store/store.py`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- Nested execution dispatch is implemented through `DynamicRuntime._execute_effect` across prompt/dynamic/conditional/loop/reflector (`src/circuitry/core/dynamic.py:98`).
- Nested store composition uses child stores plus `ensure_dict`, which forms deterministic hierarchical paths (`src/circuitry/core/dynamic.py:55`, `src/circuitry/core/dynamic.py:83`, `src/circuitry/core/store/store.py:26`).
- Nested loop composition is supported, including loop-in-loop execution (`src/circuitry/core/loop.py:384`).
- Gaps to close for this story:
- Error propagation records raw exception strings but does not consistently include full nested scope path context (`src/circuitry/core/dynamic.py:94`, `src/circuitry/core/loop.py:215`, `src/circuitry/core/conditional.py:168`).
- Unsupported-type errors are local type messages without composition path breadcrumbs (`src/circuitry/core/dynamic.py:152`).
- No committed deep mixed-nesting test suite currently validates path determinism and hierarchical diagnostics.

### Source Code Anchors

- `src/circuitry/core/dynamic.py`
- `src/circuitry/core/conditional.py`
- `src/circuitry/core/loop.py`
- `src/circuitry/core/store/store.py`

### Technical Requirements

- Keep compile/runtime behavior deterministic and aligned with declared DOL semantics.
- Preserve root and nested path composition under `prime` with named child segments.
- Any behavior change must include corresponding deterministic state-path assertions.

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

- Keep changes scoped to existing module boundaries (`core`, `cli`, `store`, docs/examples/tests as needed).
- Avoid introducing duplicate orchestration execution paths.
- Add tests in a dedicated `tests/` tree when implementing this story.

### Testing Requirements

- Add unit/integration tests directly tied to story acceptance criteria.
- Include deterministic state-path assertions for relevant execution patterns.
- Verify no regressions to existing example orchestration behavior.

### Project Structure Notes

- Current repo has no committed `tests/` tree; implementation should introduce one deliberately with focused scope.
- Preserve current runtime semantics where unchanged by explicit story acceptance criteria.

### References

- `_bmad-output/planning-artifacts/epics.md`
- `_bmad-output/planning-artifacts/architecture.md`
- `docs/architecture.md`
- `src/circuitry/core/compiler.py`
- `src/circuitry/core/dynamic.py`
- `src/circuitry/core/conditional.py`
- `src/circuitry/core/loop.py`

## Dev Agent Record

### Agent Model Used

GPT-5 Codex

### Debug Log References

- Workflow: `_bmad/bmm/workflows/4-implementation/create-story/workflow.yaml`
- Instructions: `_bmad/bmm/workflows/4-implementation/create-story/instructions.xml`

### Completion Notes List

- Nested composition execution validated across prompt/dynamic/conditional/loop combinations with deterministic store composition via child stores + `ensure_dict`.
- Error context propagation improved with hierarchical path breadcrumbs in `DynamicRuntime._execute_effect` (e.g., `"prime.outer.inner: <error>"`).
- Context handling across nested loops/conditionals verified as deterministic and non-leaky via `deepcopy` isolation in tree mode.
- Added nested error path test in `tests/core/test_nested_error_paths.py` asserting hierarchical error diagnostics.
- State path contract tests in `tests/core/test_state_path_contract.py` verify path composition for nested levels.
- Quality gates pass: `pytest`, `ruff check .`, `mypy src`.

### File List

- `_bmad-output/implementation-artifacts/2-3-support-nested-dynamic-composition.md`
- `src/circuitry/core/dynamic.py`
- `src/circuitry/core/conditional.py`
- `src/circuitry/core/loop.py`
- `src/circuitry/core/store/store.py`
- `tests/core/test_nested_error_paths.py`
- `tests/core/test_state_path_contract.py`
