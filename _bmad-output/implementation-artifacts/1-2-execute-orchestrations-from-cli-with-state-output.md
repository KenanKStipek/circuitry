# Story 1.2: Execute Orchestrations from CLI with State Output

Status: done

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a user,
I want to run an orchestration from the CLI,
so that I can see execution outputs and metadata persisted in deterministic state paths.

## Acceptance Criteria

1. **Given** a valid orchestration file and CLI entrypoint  
   **When** I execute a run command  
   **Then** the orchestration completes and writes both value and metadata to deterministic state paths.
2. **Given** successful or failed execution  
   **When** CLI output is rendered  
   **Then** output includes success/failure summary and artifact location for state inspection.
3. **Given** at least one documented CLI walkthrough  
   **When** users inspect generated state  
   **Then** path examples map directly to orchestration composition including named loop `iter_<n>` segments.

## Tasks / Subtasks

- [x] Ensure `run` command persistently writes complete state JSON to `--out` and supports readable inspection flow (AC: 1, 2)
- [x] Standardize runtime metadata capture in `runtime.last_run` and `runtime.effective_settings` for every run outcome (AC: 1, 2)
- [x] Ensure error path still writes usable runtime metadata and clear CLI error output (AC: 2)
- [x] Add/update docs/examples showing exact path mapping from orchestration names to state keys (AC: 3)
- [x] Add regression tests for CLI run state-output behavior with dynamic/conditional/loop examples (AC: 1, 3)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 1 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/cli/`, `src/circuitry/core/`, `src/circuitry/core/store/store.py`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
  - CLI `run` command and options exist in `src/circuitry/cli/app.py` (`--out`, `--print`, `--dry-run`, `--json`).
  - Runtime writes `runtime.last_run` and `runtime.effective_settings` in `src/circuitry/cli/runtime_shim.py`.
  - Core execution persists hierarchical state through `Store` + `DynamicRuntime`.
- Gaps to close for this story:
  - `inspect_orchestration` currently reads `steps` only; many examples use `effects`, so inspection metadata can be misleading.
  - CLI output does not provide a strong, explicit “artifact location” summary unless the user infers from flags.
  - On failed runs, state metadata is updated internally, but `app.py` exits before any optional state write path is surfaced to user output.
  - State-path walkthrough examples are not yet documented in a single canonical CLI-focused guide.

### Source Code Anchors

- `src/circuitry/cli/app.py`
- `src/circuitry/cli/runtime_shim.py`
- `src/circuitry/core/dynamic.py`
- `src/circuitry/core/store/store.py`

### Technical Requirements

- Primary implementation files: `src/circuitry/cli/app.py`, `src/circuitry/cli/runtime_shim.py`, `src/circuitry/core/dynamic.py`.
- Preserve deterministic root name `prime` and existing nested write semantics in `Store` usage.
- Keep CLI UX consistent with Typer + Rich patterns already used in `app.py`.

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

- CLI `run` command writes complete state JSON via `--out` with `runtime.last_run` and `runtime.effective_settings` metadata on every outcome (success and failure).
- Error paths write usable runtime metadata before surfacing CLI error output.
- State-path mapping documented via `docs/troubleshooting-state-paths.md` and example orchestrations under `examples/`.
- Added CLI run/inspect tests in `tests/cli/test_run_and_inspect.py` covering state-output behavior and parity between CLI and API execution.
- Quality gates pass: `pytest`, `ruff check .`, `mypy src`.

### File List

- `_bmad-output/implementation-artifacts/1-2-execute-orchestrations-from-cli-with-state-output.md`
- `src/circuitry/cli/app.py`
- `src/circuitry/cli/runtime_shim.py`
- `src/circuitry/core/dynamic.py`
- `src/circuitry/core/store/store.py`
- `tests/cli/test_run_and_inspect.py`
- `docs/troubleshooting-state-paths.md`
