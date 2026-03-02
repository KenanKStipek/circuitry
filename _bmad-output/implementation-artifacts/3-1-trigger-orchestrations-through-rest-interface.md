# Story 3.1: Trigger Orchestrations Through REST Interface

Status: done

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As an integration engineer,
I want to invoke orchestrations through a REST endpoint,
so that external services can trigger runtime execution programmatically.

## Acceptance Criteria

1. **Given** a deployed trigger interface with authentication enabled  \n   **When** a valid orchestration request is submitted  \n   **Then** the service acknowledges the request within the defined NFR target.
2. **Given** accepted trigger requests  \n   **When** orchestration execution is started  \n   **Then** request IDs and execution status are traceable for follow-up inspection.
3. **Given** failed or invalid trigger requests  \n   **When** errors occur  \n   **Then** responses and runtime metadata provide diagnosable failure detail.

## Tasks / Subtasks

- [x] Design minimal REST trigger contract (request payload, auth, response envelope, correlation IDs) (AC: 1, 2)
- [x] Reuse existing runtime execution path (`run(req)`) behind REST handler to avoid duplicate orchestration logic (AC: 2)
- [x] Add authentication and request validation guardrails aligned with NFR-S3 (AC: 1, 3)
- [x] Persist request/trace metadata into runtime state for post-run inspection (AC: 2, 3)
- [x] Add API tests for success, auth failure, validation failure, and runtime failure paths (AC: 1, 2, 3)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 3 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/core/`, `src/circuitry/cli/`, `src/circuitry/adapters/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- Execution primitive already exists in `src/circuitry/cli/runtime_shim.py` (`run(req)`), including `runtime.last_run` and `runtime.effective_settings` metadata writes.
- Orchestration loading, settings resolution, compile, and execute pipeline is already centralized (`orchestration_loader.py`, `effective_settings.py`, `compiler.py`, `dynamic.py`).
- Gaps to close for this story:
- No REST server/router layer currently exists in `src/` (no endpoint handler, request schema, or auth middleware).
- No request correlation ID model is currently persisted in state metadata for external trigger observability.
- No HTTP contract tests currently exist for trigger lifecycle and error semantics.

### Source Code Anchors

- `src/circuitry/cli/runtime_shim.py:48`
- `src/circuitry/cli/runtime_shim.py:56`
- `src/circuitry/cli/runtime_shim.py:68`
- `src/circuitry/cli/orchestration_loader.py:9`
- `src/circuitry/cli/effective_settings.py:28`

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

- Keep changes scoped to existing module boundaries (`core`, `cli`, `adapters`, docs/tests as needed).
- Avoid introducing duplicate orchestration execution paths.
- Add tests in a dedicated `tests/` tree when implementing this story.

### Testing Requirements

- Add unit/integration tests directly tied to story acceptance criteria.
- Include deterministic state-path assertions and failure-diagnostics checks.
- Verify no regressions to existing example orchestration behavior.

### Project Structure Notes

- Current repo has no committed `tests/` tree; implementation should introduce one deliberately with focused scope.
- Preserve current runtime semantics where unchanged by explicit story acceptance criteria.

### References

- `_bmad-output/planning-artifacts/epics.md`
- `_bmad-output/planning-artifacts/architecture.md`
- `docs/architecture.md`
- `src/circuitry/cli/runtime_shim.py`
- `src/circuitry/core/compiler.py`
- `src/circuitry/core/prompt.py`
- `src/circuitry/core/dynamic.py`

## Dev Agent Record

### Agent Model Used

GPT-5 Codex

### Debug Log References

- Workflow: `_bmad/bmm/workflows/4-implementation/create-story/workflow.yaml`
- Instructions: `_bmad/bmm/workflows/4-implementation/create-story/instructions.xml`

### Completion Notes List

- Implemented `src/circuitry/service/rest.py` with a minimal `POST /v1/triggers/run` contract, bearer token guard, request ID correlation, and response envelope metadata.
- Reused existing execution path by constructing `RunRequest` and invoking `src/circuitry/cli/runtime_shim.py:run`, avoiding duplicate orchestration runtime logic.
- Added runtime trace metadata injection at `runtime.trigger` (`interface`, `request_id`, `status`, timestamps, `error`) to support follow-up inspection.
- Added REST contract tests covering success, authentication failure, payload validation failure, and runtime failure in `tests/service/test_rest_trigger.py`.
- Verified quality gates: `pytest -q`, `ruff check .`, and `mypy src` all passing after implementation.

### File List

- `_bmad-output/implementation-artifacts/3-1-trigger-orchestrations-through-rest-interface.md`
- `src/circuitry/service/__init__.py`
- `src/circuitry/service/rest.py`
- `tests/service/test_rest_trigger.py`
