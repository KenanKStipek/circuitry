# Story 5.2: Execute Shared Assets Across Services

Status: done

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a team integrating multiple services,
I want shared orchestrations to run consistently across contexts,
so that behavior is reusable without redefining logic per service.

## Acceptance Criteria

1. **Given** a shared orchestration asset used by multiple services  
   **When** each service executes the orchestration through supported runtime interfaces  
   **Then** execution semantics and state outputs remain consistent.
2. **Given** service-specific configuration requirements  
   **When** services execute shared assets  
   **Then** service-specific configuration can be applied without altering core asset logic.

## Tasks / Subtasks

- [x] Define execution contract for shared assets across service contexts (runtime inputs, metadata, invariants) (AC: 1)
- [x] Add shared-asset identity/version metadata to runtime run records for traceability across services (AC: 1)
- [x] Introduce service-specific override mechanism (adapter/model/runtime knobs) without mutating shared asset payloads (AC: 2)
- [x] Add cross-service conformance tests that assert equivalent state-path and semantic outputs (AC: 1, 2)
- [x] Document usage pattern for shared-asset execution in CLI and embedded API integrations (AC: 1, 2)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 5 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/core/`, `src/circuitry/cli/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- Compile/runtime path is deterministic and reused uniformly by CLI execution.
- Effective settings already support orchestration-plus-config merge semantics that can carry service-level runtime differences.
- Runtime metadata already records resolved effective settings, enabling baseline context tracing.
- Gaps to close for this story:
- No first-class shared-asset identity/version fields are captured in run metadata.
- No conformance test matrix exists for same-asset execution across multiple service profiles.
- No explicit service-profile abstraction currently exists beyond generic config/runtime overrides.

### Source Code Anchors

- `src/circuitry/core/compiler.py:32`
- `src/circuitry/core/dynamic.py:54`
- `src/circuitry/cli/effective_settings.py:28`
- `src/circuitry/cli/effective_settings.py:95`
- `src/circuitry/cli/runtime_shim.py:56`
- `src/circuitry/cli/runtime_shim.py:68`

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

- Keep changes scoped to existing module boundaries (`core`, `cli`, docs/tests as needed).
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
- `src/circuitry/core/compiler.py`
- `src/circuitry/core/dynamic.py`
- `src/circuitry/cli/effective_settings.py`
- `src/circuitry/cli/runtime_shim.py`

## Dev Agent Record

### Agent Model Used

GPT-5 Codex

### Debug Log References

- Workflow: `_bmad/bmm/workflows/4-implementation/create-story/workflow.yaml`
- Instructions: `_bmad/bmm/workflows/4-implementation/create-story/instructions.xml`

### Completion Notes List

- Extended shared-library execution contract in `src/circuitry/cli/shared_library.py` with `service_profiles` and deterministic runtime override application via `apply_service_profile(...)`.
- Added service-profile support to CLI `run-library` in `src/circuitry/cli/app.py` (`--service-profile`) without mutating shared asset payloads.
- Added embedded API support through `run_shared_orchestration(...)` in `src/circuitry/api.py`, including service profile override handling and shared metadata propagation.
- Preserved traceability by recording service profile identity in `runtime.shared_library.service_profile` (when applied), alongside asset/version/source metadata.
- Added cross-service conformance tests in `tests/cli/test_shared_library_service_profiles.py` asserting equivalent shared-asset semantic outputs across profiles plus profile-specific effective settings.
- Added embedded API shared-asset execution coverage in `tests/api/test_shared_library_api.py`.
- Updated usage docs in `docs/shared-library.md` and `docs/development-guide.md` with service-profile and CLI/API patterns.
- Verified quality gates: `pytest -q`, `ruff check src tests`, and `mypy src` all passing.

### File List

- `_bmad-output/implementation-artifacts/5-2-execute-shared-assets-across-services.md`
- `src/circuitry/cli/shared_library.py`
- `src/circuitry/cli/app.py`
- `src/circuitry/api.py`
- `src/circuitry/__init__.py`
- `tests/cli/test_shared_library_service_profiles.py`
- `tests/api/test_shared_library_api.py`
- `docs/shared-library.md`
- `docs/development-guide.md`
