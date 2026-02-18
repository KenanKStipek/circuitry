# Story 5.1: Retrieve Shared Library Orchestrations

Status: ready-for-dev

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a developer,
I want to fetch orchestrations from a shared library,
so that I can reuse proven orchestration assets quickly.

## Acceptance Criteria

1. **Given** access to the shared orchestration library  
   **When** I request an orchestration asset  
   **Then** I can retrieve it with required metadata/version details.
2. **Given** retrieved shared assets  
   **When** I execute them through supported runtime interfaces  
   **Then** fetched assets are executable without manual restructuring.

## Tasks / Subtasks

- [ ] Define shared-library retrieval contract (asset identifier, version selector, metadata envelope, auth) (AC: 1)
- [ ] Implement retrieval path that materializes library assets into executable orchestration payloads (AC: 1, 2)
- [ ] Capture and expose required metadata/version details in CLI and runtime metadata (AC: 1)
- [ ] Add error handling for missing/unauthorized/invalid library assets with diagnosable messages (AC: 1, 2)
- [ ] Add tests for retrieval success, version pinning, and execution-readiness of fetched assets (AC: 1, 2)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 5 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/cli/`, `src/circuitry/core/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- CLI/runtime path can load YAML orchestration objects and execute them once provided locally.
- Existing run pipeline already compiles and executes the loaded orchestration document without additional reshaping.
- Gaps to close for this story:
- No shared-library client or retrieval protocol exists; orchestration input is local-path based.
- No library asset metadata/version identity model is persisted in runtime run metadata.
- No fetch command/subcommand exists to resolve shared assets into local executable inputs.

### Source Code Anchors

- `src/circuitry/cli/app.py:29`
- `src/circuitry/cli/orchestration_loader.py:9`
- `src/circuitry/cli/runtime_shim.py:54`
- `src/circuitry/cli/runtime_shim.py:94`

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

- Keep changes scoped to existing module boundaries (`cli`, `core`, docs/tests as needed).
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
- `src/circuitry/cli/app.py`
- `src/circuitry/cli/orchestration_loader.py`
- `src/circuitry/cli/runtime_shim.py`
- `src/circuitry/core/compiler.py`

## Dev Agent Record

### Agent Model Used

GPT-5 Codex

### Debug Log References

- Workflow: `_bmad/bmm/workflows/4-implementation/create-story/workflow.yaml`
- Instructions: `_bmad/bmm/workflows/4-implementation/create-story/instructions.xml`

### Completion Notes List

- Story context generated from Epic 5 source and architecture constraints.
- Included existing-code cross-reference with concrete retrieval/publishing gaps.
- Ready for `dev-story` implementation workflow.

### File List

- `_bmad-output/implementation-artifacts/5-1-retrieve-shared-library-orchestrations.md`
