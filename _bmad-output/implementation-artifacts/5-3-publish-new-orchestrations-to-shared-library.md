# Story 5.3: Publish New Orchestrations to Shared Library

Status: ready-for-dev

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a contributor,
I want to add new orchestrations to the shared library,
so that other teams can discover and reuse them.

## Acceptance Criteria

1. **Given** a valid orchestration artifact and publication workflow  
   **When** I publish the asset  
   **Then** it is stored with searchable metadata and version information.
2. **Given** publication validation requirements  
   **When** an asset is published  
   **Then** publication validation ensures minimum quality requirements are met.

## Tasks / Subtasks

- [ ] Define publish workflow contract (artifact payload, metadata schema, versioning policy, auth) (AC: 1)
- [ ] Implement publish interface and storage integration for orchestration assets and metadata index records (AC: 1)
- [ ] Add metadata/search indexing fields for discoverability (name, tags, owner, version, compatibility) (AC: 1)
- [ ] Implement publication quality gates (schema/compile checks, deterministic path checks, lint policy) (AC: 2)
- [ ] Add tests for publish success, duplicate/version conflict, validation failure, and retrieval after publish (AC: 1, 2)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 5 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/cli/`, `src/circuitry/core/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- Local validation and compile entrypoints exist and can be reused as part of publication gates.
- Compiler and loader enforce basic orchestration shape/type checks before execution.
- Gaps to close for this story:
- No publish command/workflow currently exists in CLI or runtime integration.
- No shared-library storage/index component exists for metadata/versioned assets.
- Current validate path is minimal and does not enforce publication-grade quality checks.

### Source Code Anchors

- `src/circuitry/cli/app.py:118`
- `src/circuitry/cli/runtime_shim.py:130`
- `src/circuitry/cli/orchestration_loader.py:9`
- `src/circuitry/core/compiler.py:32`
- `src/circuitry/core/compiler.py:37`

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
- `src/circuitry/cli/runtime_shim.py`
- `src/circuitry/cli/orchestration_loader.py`
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

- `_bmad-output/implementation-artifacts/5-3-publish-new-orchestrations-to-shared-library.md`
