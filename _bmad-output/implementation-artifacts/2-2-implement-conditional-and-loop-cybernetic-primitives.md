# Story 2.2: Implement Conditional and Loop Cybernetic Primitives

Status: done

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a developer,
I want to use conditionals and loops in orchestrations,
so that I can encode dynamic control flow for real-time decision and iteration behavior.

## Acceptance Criteria

1. **Given** orchestration definitions containing conditional and loop constructs  \
   **When** runtime executes control-flow evaluation and loop progression  \
   **Then** branching and iteration follow declared mode semantics with explicit termination behavior.
2. **Given** named conditional and loop constructs  \
   **When** state is recorded  \
   **Then** runtime stores branch/iteration metadata needed for diagnosis.
3. **Given** State Path Contract rules  \
   **When** conditionals and loops execute  \
   **Then** wrapper and iteration path rules are deterministic and consistent.

## Tasks / Subtasks

- [x] Tighten conditional mode behavior (`model` and `cel`) and branch selection metadata (AC: 1, 2)
- [x] Tighten loop mode behavior (`each` and `while`) including termination and bounds metadata (AC: 1, 2)
- [x] Ensure named vs transparent control writes match State Path Contract (AC: 3)
- [x] Add tests for branch selection, loop iteration indexing, and termination reasons (AC: 1, 2, 3)
- [x] Add troubleshooting notes for diagnosing branch and loop outcomes via state inspection (AC: 2, 3)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 2 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/core/`, `src/circuitry/cli/`, `src/circuitry/core/store/store.py`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- Conditional mode handling exists for `model` and `cel` (`src/circuitry/core/conditional.py:172`, `src/circuitry/core/conditional.py:211`).
- Conditional named-wrapper metadata exists (mode/threshold/branch/result) (`src/circuitry/core/conditional.py:93`, `src/circuitry/core/conditional.py:121`).
- Loop supports `each` and `while` modes with termination metadata (`src/circuitry/core/loop.py:127`, `src/circuitry/core/loop.py:168`, `src/circuitry/core/loop.py:203`).
- Named loop iteration path segments are created as `iter_<n>` (`src/circuitry/core/loop.py:350`).
- Gaps to close for this story:
- Conditional captures `executed_effects` but never populates it with branch execution detail (`src/circuitry/core/conditional.py:126`, `src/circuitry/core/conditional.py:161`).
- Loop `_execute_body` currently returns `{}`, so `effects_by_iteration` has weak diagnostic payload (`src/circuitry/core/loop.py:208`, `src/circuitry/core/loop.py:393`).
- Named vs transparent control behavior is implemented, but no committed regression suite currently locks these path semantics.

### Source Code Anchors

- `src/circuitry/core/conditional.py`
- `src/circuitry/core/loop.py`
- `src/circuitry/core/compiler.py`
- `src/circuitry/core/dynamic.py`

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

- Conditional mode behavior tightened for `model` and `cel` modes with branch selection metadata recording (`executed_effects`, mode, threshold, branch, result) in `src/circuitry/core/conditional.py`.
- Loop mode behavior tightened for `each` and `while` with termination metadata, iteration indexing (`iter_<n>`), and `effects_by_iteration` diagnostic payload in `src/circuitry/core/loop.py`.
- Named vs transparent control writes aligned to State Path Contract: named conditionals/loops create wrapper segments; transparent ones write directly.
- Added 8 tests in `tests/core/test_conditional_loop_metadata.py`: conditional metadata, loop iteration tracking, collect aggregation, parallel loop execution, order preservation, context sharing, max_concurrency.
- Added 6 state path contract tests in `tests/core/test_state_path_contract.py` for named/transparent conditional and loop wrapper segment behavior.
- Added troubleshooting guidance in `docs/troubleshooting-state-paths.md`.
- Quality gates pass: `pytest`, `ruff check .`, `mypy src`.

### File List

- `_bmad-output/implementation-artifacts/2-2-implement-conditional-and-loop-cybernetic-primitives.md`
- `src/circuitry/core/conditional.py`
- `src/circuitry/core/loop.py`
- `src/circuitry/core/compiler.py`
- `tests/core/test_conditional_loop_metadata.py`
- `tests/core/test_state_path_contract.py`
- `docs/troubleshooting-state-paths.md`
