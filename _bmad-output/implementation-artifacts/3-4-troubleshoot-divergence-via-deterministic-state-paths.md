# Story 3.4: Troubleshoot Divergence via Deterministic State Paths

Status: review

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a support user,
I want to inspect state and execution metadata deterministically,
so that I can isolate where orchestration behavior diverged from expectations.

## Acceptance Criteria

1. **Given** a completed or failed orchestration run  \n   **When** support inspects stored state paths and metadata  \n   **Then** divergence points can be identified to specific orchestration steps.
2. **Given** nested control flow execution  \n   **When** failures occur  \n   **Then** diagnostics provide clear hierarchical context for root-cause analysis.
3. **Given** support playbooks and docs  \n   **When** engineers troubleshoot incidents  \n   **Then** deterministic state-path inspection workflow is documented and reproducible.

## Tasks / Subtasks

- [x] Define standard divergence-inspection workflow using deterministic state-path traversal (AC: 1, 3)
- [x] Improve error metadata context to include hierarchical execution path breadcrumbs where feasible (AC: 1, 2)
- [x] Add helper utilities or docs for extracting failed node paths and related metadata quickly (AC: 1, 3)
- [x] Add regression tests asserting deterministic path layout and diagnosability under failure scenarios (AC: 1, 2)
- [x] Document troubleshooting examples for prompt/dynamic/conditional/loop failure modes (AC: 3)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 3 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/core/`, `src/circuitry/cli/`, `src/circuitry/adapters/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- Deterministic hierarchical state writing exists through `Store.ensure_dict` and runtime composition.
- Runtime metadata records (`created_at`, `completed_at`, `error`, adapter/model info) exist across prompt/dynamic/control primitives.
- Existing examples and `out.json` provide real state snapshots for troubleshooting pattern development.
- Gaps to close for this story:
- Exception/error messages often lack full hierarchical path breadcrumbs at point of failure.
- No dedicated support tooling/commands currently summarize divergence hotspots from state.
- No committed troubleshooting guide currently standardizes deterministic state-path debugging workflow.

### Source Code Anchors

- `src/circuitry/core/store/store.py:26`
- `src/circuitry/core/prompt.py:137`
- `src/circuitry/core/conditional.py:121`
- `src/circuitry/core/loop.py:203`
- `src/circuitry/core/dynamic.py:92`

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

- Added deterministic divergence helper `find_divergence_paths(...)` in `src/circuitry/core/diagnostics.py` to extract sorted failure-path records from runtime state metadata.
- Exposed the helper through embedded API surface as `inspect_divergence_paths(...)` (`src/circuitry/api.py`, `src/circuitry/__init__.py`).
- Added regression coverage in `tests/core/test_diagnostics.py` for deterministic sorting and nested failure breadcrumb discoverability from real runtime failures.
- Added embedded API coverage in `tests/api/test_embedded_api.py` for deterministic divergence extraction output ordering.
- Added operator-facing troubleshooting playbook in `docs/troubleshooting-state-paths.md` and indexed it in `docs/index.md`.
- Verified quality gates: `pytest -q`, `ruff check src tests`, and `mypy src` all passing after implementation.

### File List

- `_bmad-output/implementation-artifacts/3-4-troubleshoot-divergence-via-deterministic-state-paths.md`
- `src/circuitry/core/diagnostics.py`
- `src/circuitry/core/__init__.py`
- `src/circuitry/api.py`
- `src/circuitry/__init__.py`
- `tests/core/test_diagnostics.py`
- `tests/api/test_embedded_api.py`
- `docs/troubleshooting-state-paths.md`
- `docs/index.md`
