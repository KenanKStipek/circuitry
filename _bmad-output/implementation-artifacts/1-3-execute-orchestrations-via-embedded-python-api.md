# Story 1.3: Execute Orchestrations via Embedded Python API

Status: ready-for-dev

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a developer,
I want to execute orchestrations from Python package APIs,
so that I can embed Circuitry in other Python systems without shelling out to CLI.

## Acceptance Criteria

1. **Given** an installed Circuitry package in a Python project  
   **When** I invoke orchestration execution through public API methods  
   **Then** execution behavior matches CLI semantics for outputs and state writes.
2. **Given** API consumers integrating the runtime  
   **When** they follow official usage guidance  
   **Then** API usage is documented with runnable examples.
3. **Given** equivalent orchestration + inputs  
   **When** executed via CLI and embedded API  
   **Then** state shape and deterministic path mapping are functionally equivalent.

## Tasks / Subtasks

- [ ] Define/confirm stable embedded API entrypoints for run/validate/inspect use cases (AC: 1, 2)
- [ ] Ensure API execution path reuses core compile/runtime flow used by CLI (AC: 1, 3)
- [ ] Add API-level tests comparing CLI and embedded execution state outputs on the same fixture orchestration (AC: 1, 3)
- [ ] Add documentation snippet for package consumers with minimal runnable examples (AC: 2)
- [ ] Ensure API errors surface actionable exceptions without hiding core compiler/runtime context (AC: 1)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 1 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/cli/`, `src/circuitry/core/`, `src/circuitry/core/store/store.py`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
  - Programmatic building blocks exist: `compile_orchestration`, `DynamicRuntime`, `Store` under `src/circuitry/core/`.
  - Reusable execution orchestrator exists via `run(req)` in `src/circuitry/cli/runtime_shim.py` (importable, not CLI-only).
  - `src/circuitry/core/__init__.py` exports core runtime primitives for direct use.
- Gaps to close for this story:
  - No explicit first-class embedded API module (service-layer API) for host applications.
  - README programmatic snippet references `create_adapter`, but the implemented factory is `build_adapter` (`src/circuitry/adapters/factory.py`), so docs/API contract are currently inconsistent.
  - No parity tests currently prove CLI vs embedded execution/state equivalence on the same orchestration fixtures.
  - Error semantics for embedded consumers are not documented as a stable integration contract.

### Source Code Anchors

- `src/circuitry/core/compiler.py`
- `src/circuitry/core/dynamic.py`
- `src/circuitry/core/__init__.py`
- `src/circuitry/cli/runtime_shim.py`
- `src/circuitry/adapters/factory.py`

### Technical Requirements

- Expected touch points: `src/circuitry/core/*`, possible export surface in `src/circuitry/core/__init__.py` and/or new API module.
- Do not create divergent execution semantics for embedded usage; share compile/runtime path with CLI.
- Preserve State Path Contract behavior and metadata layout expected by operators/support tooling.

### Architecture Compliance

- Enforce deterministic orchestration-to-state mapping per State Path Contract.
- Do not introduce hidden/non-deterministic path segments.
- Maintain backward compatibility posture for state path format unless explicitly versioned.

### Library / Framework Requirements

- Python package layout under `src/`.
- Orchestration parsing via `PyYAML` from `requirements.txt`.
- CLI UX stack remains Typer + Rich.
- Quality gates remain pytest + ruff + mypy.

### File Structure Requirements

- Keep changes scoped to existing module boundaries (`cli`, `core`, `store`, docs/examples as needed).
- Avoid introducing duplicate orchestration execution paths.
- Add tests in a dedicated `tests/` tree when implementing this story.

### Testing Requirements

- Add unit/integration tests directly tied to story acceptance criteria.
- Include at least one deterministic state-path assertion in relevant tests.
- Verify no regressions to existing example orchestration behavior.

### Project Structure Notes

- Current repo has no committed `tests/` tree; implementation should introduce one deliberately with focused scope.
- Preserve existing CLI command naming and output style patterns unless story explicitly changes them.

### References

- `_bmad-output/planning-artifacts/epics.md`
- `_bmad-output/planning-artifacts/architecture.md`
- `docs/architecture.md`
- `src/circuitry/cli/app.py`
- `src/circuitry/cli/runtime_shim.py`
- `src/circuitry/core/compiler.py`

## Dev Agent Record

### Agent Model Used

GPT-5 Codex

### Debug Log References

- Workflow: `_bmad/bmm/workflows/4-implementation/create-story/workflow.yaml`
- Instructions: `_bmad/bmm/workflows/4-implementation/create-story/instructions.xml`

### Completion Notes List

- Story context generated from Epic 1 source and architecture constraints.
- Deterministic state-path guardrails included where applicable.
- Ready for `dev-story` implementation workflow.

### File List

- `_bmad-output/implementation-artifacts/1-3-execute-orchestrations-via-embedded-python-api.md`
