# Story 4.3: Persist Runtime State Through Postgres Tooling

Status: review

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a developer,
I want orchestration state persisted through a Postgres integration path,
so that run data is durable and queryable for downstream usage.

## Acceptance Criteria

1. **Given** Postgres persistence configuration and secure transport settings  \
   **When** orchestration runs write and read state data  \
   **Then** persisted state preserves deterministic path structure and integrity.
2. **Given** persistence failures  \
   **When** write/read operations fail  \
   **Then** failures are reported with non-silent error metadata.
3. **Given** persisted runtime history  \
   **When** operators inspect run outcomes  \
   **Then** state and metadata remain auditable and reconstructable.

## Tasks / Subtasks

- [x] Design persistence abstraction for state snapshot/write/read operations with deterministic key structure (AC: 1)
- [x] Implement Postgres-backed storage adapter/tooling and secure connection requirements (AC: 1)
- [x] Integrate persistence lifecycle into runtime execution path with robust error handling (AC: 2)
- [x] Add tests for persistence round-trip integrity and failure metadata behavior (AC: 1, 2, 3)
- [x] Document operational setup (schema, migrations, connectivity, security) for Postgres persistence (AC: 1, 3)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 4 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/adapters/`, `src/circuitry/cli/`, `src/circuitry/core/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- State abstraction exists (`Store`) and supports JSON serialization output via `dump_json`.
- Runtime metadata model is already rich enough to persist run diagnostics and path-derived records.
- Architecture docs explicitly call out persistence/plugin direction.
- Gaps to close for this story:
- No Postgres persistence implementation currently exists in `src/`.
- No persistence interface/plugin contract is implemented for runtime hooks.
- No schema/migration or encrypted transport handling is present for database-backed state.

### Source Code Anchors

- `src/circuitry/core/store/store.py:10`
- `src/circuitry/core/store/store.py:54`
- `src/circuitry/cli/runtime_shim.py:97`
- `docs/Circuitry Terminology 2c94435ec2e0808daeeff76f7ed1ed25.md:208`
- `docs/architecture.md:58`

### Technical Requirements

- Keep compile/runtime behavior deterministic and aligned with declared DOL semantics.
- Preserve root and nested path composition under `prime` with named child segments.
- Any behavior change must include deterministic state-path assertions and operational diagnostics.

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

- Keep changes scoped to existing module boundaries (`adapters`, `cli`, `core`, docs/tests as needed).
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
- `src/circuitry/adapters/factory.py`
- `src/circuitry/cli/effective_settings.py`
- `src/circuitry/cli/runtime_shim.py`
- `src/circuitry/core/store/store.py`

## Dev Agent Record

### Agent Model Used

GPT-5 Codex

### Debug Log References

- Workflow: `_bmad/bmm/workflows/4-implementation/create-story/workflow.yaml`
- Instructions: `_bmad/bmm/workflows/4-implementation/create-story/instructions.xml`

### Completion Notes List

- Added persistence abstraction in `src/circuitry/core/store/persistence.py` with runtime backend resolution and protocol shape for state load/save lifecycle.
- Implemented Postgres backend in `src/circuitry/core/store/postgres.py` including schema bootstrap, snapshot load/save, SSL mode safeguards, and actionable diagnostics.
- Integrated persistence lifecycle into `src/circuitry/cli/runtime_shim.py`:
  - optional state hydration from latest persisted snapshot
  - per-run snapshot persistence
  - non-silent failure propagation via `runtime.persistence` metadata and run errors
  - auditable run IDs under `runtime.last_run.run_id`
- Added tests for persistence round-trip and failure behavior:
  - `tests/cli/test_postgres_persistence.py`
  - `tests/core/test_postgres_persistence_config.py`
- Added operational documentation in `docs/postgres-persistence.md` and linked from `docs/index.md`; referenced in `docs/development-guide.md`.
- Verified quality gates: `pytest -q`, `ruff check src tests`, and `mypy src` all passing.

### File List

- `_bmad-output/implementation-artifacts/4-3-persist-runtime-state-through-postgres-tooling.md`
- `src/circuitry/core/store/persistence.py`
- `src/circuitry/core/store/postgres.py`
- `src/circuitry/core/store/__init__.py`
- `src/circuitry/cli/runtime_shim.py`
- `tests/cli/test_postgres_persistence.py`
- `tests/core/test_postgres_persistence_config.py`
- `docs/postgres-persistence.md`
- `docs/index.md`
- `docs/development-guide.md`
