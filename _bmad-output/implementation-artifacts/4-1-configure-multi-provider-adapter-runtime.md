# Story 4.1: Configure Multi-Provider Adapter Runtime

Status: done

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a developer,
I want to configure multiple upstream providers and adapter selection rules,
so that I can choose the best model path per environment and workload.

## Acceptance Criteria

1. **Given** environment-specific runtime configuration  \
   **When** an orchestration run is executed  \
   **Then** adapter/provider resolution follows configured selection rules.
2. **Given** resolved adapter/model configuration  \
   **When** execution is recorded  \
   **Then** runtime metadata includes provider identity and configuration context used.
3. **Given** invalid or missing adapter configuration  \
   **When** runtime initialization occurs  \
   **Then** failures are explicit and actionable.

## Tasks / Subtasks

- [x] Define explicit adapter-selection policy precedence and document edge cases (AC: 1, 3)
- [x] Ensure runtime metadata consistently records resolved adapter/model/source across all run outcomes (AC: 2)
- [x] Improve diagnostics for unknown/missing adapter and model configuration paths (AC: 3)
- [x] Add tests for config precedence (CLI/orchestration/config/default) and adapter resolution behavior (AC: 1, 3)
- [x] Add operator-facing docs for multi-provider runtime configuration examples (AC: 1, 2)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 4 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/adapters/`, `src/circuitry/cli/`, `src/circuitry/core/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- Adapter factory supports multiple providers (`ollama`, `openai`, `anthropic`, `litellm`) via `build_adapter`.
- Effective settings resolution with precedence logic already exists in `resolve_effective_settings`.
- Runtime persists `runtime.effective_settings` and source metadata on each run.
- Gaps to close for this story:
- Selection policy is mainly precedence-based; advanced routing/selection rules per workload are not implemented.
- No committed test suite currently validates full precedence matrix and multi-adapter config behavior.
- Operator documentation for multi-provider runtime patterns is still thin and partially drift-prone.

### Source Code Anchors

- `src/circuitry/adapters/factory.py:12`
- `src/circuitry/cli/effective_settings.py:28`
- `src/circuitry/cli/effective_settings.py:52`
- `src/circuitry/cli/runtime_shim.py:68`
- `src/circuitry/cli/runtime_shim.py:80`

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

- Improved unknown adapter diagnostics in `src/circuitry/adapters/factory.py` with supported adapter list and actionable runtime/config hints.
- Added runtime initialization validation in `src/circuitry/cli/runtime_shim.py` to produce explicit missing adapter/model resolution errors with source context.
- Preserved and validated runtime metadata behavior (`runtime.effective_settings`) including adapter/model/runtime source traceability.
- Added precedence and resolution tests in `tests/cli/test_effective_settings_precedence.py` covering CLI/orchestration/config ordering and actionable failure paths.
- Added operator-facing multi-provider configuration guidance and precedence docs in `README.md` and `docs/development-guide.md`.
- Verified quality gates: `pytest -q`, `ruff check .`, and `mypy src` all passing.

### File List

- `_bmad-output/implementation-artifacts/4-1-configure-multi-provider-adapter-runtime.md`
- `src/circuitry/adapters/factory.py`
- `src/circuitry/cli/runtime_shim.py`
- `tests/cli/test_effective_settings_precedence.py`
- `README.md`
- `docs/development-guide.md`
