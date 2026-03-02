# Story 5.3: Publish New Orchestrations to Shared Library

Status: done

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a contributor,
I want to add new orchestrations to the shared library via pull request to the library repository,
so that other teams can discover and reuse them.

## Acceptance Criteria

1. **Given** a valid orchestration artifact and contribution workflow  
   **When** I submit a pull request to the shared-library repository  
   **Then** the asset includes searchable metadata and version information required for retrieval.
2. **Given** publication validation requirements  
   **When** a contribution pull request is reviewed  
   **Then** validation gates ensure minimum quality requirements are met before merge.

## Tasks / Subtasks

- [x] Define contribution contract for library repo PRs (artifact layout, metadata schema, versioning policy, ownership fields) (AC: 1)
- [x] Document repository boundary: this repo retrieves/executes assets only; publish happens in library repo (AC: 1)
- [x] Define required metadata/search fields for discoverability (name, tags, owner, version, compatibility) (AC: 1)
- [x] Define publication quality gates using existing validation paths (schema/compile checks, deterministic path checks, lint/type/test policy) (AC: 2)
- [x] Add contribution checklist that verifies retrieval readiness from this repo after merge (AC: 1, 2)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 5 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/cli/`, `src/circuitry/core/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- Local validation and compile entrypoints exist and can be reused as part of PR contribution gates.
- Compiler and loader enforce orchestration shape/type checks before execution.
- Shared library retrieval + version selection exists (`fetch`, `run-library`) and defines the consumer-side contract.
- Gaps to close for this story:
- Publish process for shared assets must be documented as external repository workflow.
- Governance contract for metadata/version expectations must be explicit.
- Contribution checklist needs to map existing tooling commands to merge gates.

### Source Code Anchors

- `src/circuitry/cli/app.py:320`
- `src/circuitry/cli/app.py:368`
- `src/circuitry/cli/shared_library.py:27`
- `src/circuitry/cli/orchestration_loader.py:9`
- `src/circuitry/core/compiler.py:32`

### Technical Requirements

- Keep compile/runtime behavior deterministic and aligned with declared DOL semantics.
- Preserve root and nested path composition under `prime` with named child segments.
- Any contribution guidance must preserve deterministic validation requirements and failure-path diagnostics.

### Architecture Compliance

- Enforce deterministic orchestration-to-state mapping per State Path Contract.
- Do not introduce hidden/non-deterministic path segments.
- Maintain backward compatibility posture for state path format unless explicitly versioned in the library repo.

### Library / Framework Requirements

- Python package layout under `src/`.
- Orchestration parsing via `PyYAML` from `requirements.txt`.
- CLI UX stack remains Typer + Rich where consumer behavior is involved.
- Contribution gates rely on `validate`/`inspect` plus `pytest` + `ruff` + `mypy`.

### File Structure Requirements

- Keep changes scoped to docs and existing consumer interfaces (`cli`, docs).
- Avoid introducing duplicate orchestration execution paths.
- Keep publish mechanics outside this repository.

### Testing Requirements

- Validate documented process via existing retrieval and runtime tests.
- Ensure contribution checklist references deterministic state-path assertions and failure diagnostics.
- Verify no regressions to existing shared-library retrieval behavior.

### Project Structure Notes

- Shared-library publication source-of-truth is a separate repository by design.
- Preserve runtime semantics in this repo; only retrieval/execution lives here.

### References

- `_bmad-output/planning-artifacts/epics.md`
- `_bmad-output/planning-artifacts/architecture.md`
- `docs/shared-library.md`
- `docs/shared-library-contributions.md`
- `docs/architecture.md`
- `src/circuitry/cli/app.py`
- `src/circuitry/cli/shared_library.py`
- `src/circuitry/cli/orchestration_loader.py`
- `src/circuitry/core/compiler.py`

## Dev Agent Record

### Agent Model Used

GPT-5 Codex

### Debug Log References

- Workflow: `_bmad/bmm/workflows/4-implementation/create-story/workflow.yaml`
- Instructions: `_bmad/bmm/workflows/4-implementation/create-story/instructions.xml`

### Completion Notes List

- Implemented contribution workflow documentation for external shared-library repository publication.
- Defined metadata/version contract and merge-gate checklist aligned with existing CLI/tooling.
- Confirmed in-repo scope boundary remains retrieval/execution only.

### File List

- `_bmad-output/implementation-artifacts/5-3-publish-new-orchestrations-to-shared-library.md`
- `docs/shared-library.md`
- `docs/shared-library-contributions.md`
