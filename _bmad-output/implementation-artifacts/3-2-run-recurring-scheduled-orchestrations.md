# Story 3.2: Run Recurring Scheduled Orchestrations

Status: review

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As an operator,
I want orchestrations to run on recurring schedules,
so that periodic workflows execute consistently without manual intervention.

## Acceptance Criteria

1. **Given** a configured recurring schedule and orchestration target  \n   **When** scheduled trigger time is reached  \n   **Then** runtime dispatch occurs within target latency and run outcome is persisted.
2. **Given** missed or failed schedules  \n   **When** scheduler evaluates run outcomes  \n   **Then** explicit diagnostic records are produced for investigation.
3. **Given** repeated scheduled runs  \n   **When** operators inspect state/history  \n   **Then** run metadata is traceable per invocation.

## Tasks / Subtasks

- [x] Define scheduler abstraction and job definition format for orchestration targets (AC: 1)
- [x] Implement recurring dispatch worker that invokes existing runtime pipeline reliably (AC: 1, 3)
- [x] Record schedule metadata (planned_at, triggered_at, status, errors) per run (AC: 2, 3)
- [x] Add handling/reporting for missed, delayed, and failed schedule events (AC: 2)
- [x] Add tests for recurring dispatch behavior, failure handling, and metadata traceability (AC: 1, 2, 3)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 3 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/core/`, `src/circuitry/cli/`, `src/circuitry/adapters/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- Runtime execution and metadata persistence primitives exist and can be reused by a scheduler dispatch layer.
- Current `runtime.last_run` metadata structure can be extended as foundation for scheduled run records.
- Gaps to close for this story:
- No scheduler service/module exists in `src/` (no cron parser, job store, dispatcher loop, or schedule config schema).
- No persisted schedule history model currently exists; only last run metadata is tracked.
- No automated tests currently cover recurring execution or dispatch latency behavior.

### Source Code Anchors

- `src/circuitry/cli/runtime_shim.py:59`
- `src/circuitry/cli/runtime_shim.py:68`
- `src/circuitry/cli/runtime_shim.py:117`
- `src/circuitry/core/store/store.py:43`

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

- Implemented `src/circuitry/service/scheduler.py` with `ScheduledJob`, `ScheduledDispatchRecord`, and `RecurringScheduler.tick()` for deterministic recurring dispatch.
- Reused `src/circuitry/cli/runtime_shim.py:run` for job execution by generating `RunRequest` per invocation, preserving a single orchestration runtime path.
- Added schedule invocation metadata at `runtime.schedule` and append-only per-run records at `runtime.schedule_history` including `planned_at`, `triggered_at`, `completed_at`, `delay_seconds`, `status`, and `error`.
- Added delayed/missed diagnostics via `allowed_lateness_seconds` and persisted `delayed` with measured dispatch latency.
- Added scheduler tests in `tests/service/test_scheduler.py` for due dispatch, delayed diagnostics, failure reporting, and repeated invocation traceability.
- Verified quality gates: `pytest -q`, `ruff check .`, and `mypy src` all passing after implementation.

### File List

- `_bmad-output/implementation-artifacts/3-2-run-recurring-scheduled-orchestrations.md`
- `src/circuitry/service/scheduler.py`
- `src/circuitry/service/__init__.py`
- `tests/service/test_scheduler.py`
