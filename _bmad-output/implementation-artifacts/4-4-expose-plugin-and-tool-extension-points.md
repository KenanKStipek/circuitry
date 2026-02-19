# Story 4.4: Expose Plugin and Tool Extension Points

Status: review

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As an advanced user,
I want extension interfaces for plugins and tools,
so that I can add custom runtime capabilities without patching core execution engine internals.

## Acceptance Criteria

1. **Given** documented extension contracts  \
   **When** a plugin/tool integration is registered and invoked  \
   **Then** runtime executes extension behavior through supported interfaces.
2. **Given** extension failures  \
   **When** plugin/tool execution fails  \
   **Then** failures are isolated and observable in run metadata.
3. **Given** core runtime evolution  \
   **When** extensions are maintained across versions  \
   **Then** extension contracts remain stable and testable.

## Tasks / Subtasks

- [x] Define stable plugin/tool extension interfaces and lifecycle hooks (AC: 1, 3)
- [x] Implement plugin registration/loading/invocation path in runtime integration layer (AC: 1)
- [x] Add error-isolation boundaries so extension failures do not corrupt core runtime state (AC: 2)
- [x] Add conformance tests for extension hook contracts and failure observability (AC: 2, 3)
- [x] Document extension authoring guide with compatibility/versioning rules (AC: 1, 3)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 4 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/adapters/`, `src/circuitry/cli/`, `src/circuitry/core/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- Config model already includes `plugins` list and runtime settings plumbing.
- Effective settings resolution merges and deduplicates plugin identifiers.
- Runtime metadata pipeline is available to record extension-related execution context/errors.
- Gaps to close for this story:
- No implemented plugin loader/registry/hook execution pipeline exists in current runtime.
- `plugins` currently flow through config metadata but are not functionally invoked.
- No extension conformance tests or versioning policy enforcement currently exists.

### Source Code Anchors

- `src/circuitry/cli/config.py:25`
- `src/circuitry/cli/effective_settings.py:66`
- `src/circuitry/cli/runtime_shim.py:71`
- `docs/Circuitry Terminology 2c94435ec2e0808daeeff76f7ed1ed25.md:206`
- `docs/Circuitry Terminology 2c94435ec2e0808daeeff76f7ed1ed25.md:216`

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

- Added stable plugin contract in `src/circuitry/core/plugins.py` with contract versioning, lifecycle hooks (`on_run_start`, `on_run_success`, `on_run_failure`), plugin loading, and invocation helpers.
- Integrated plugin registration/loading/invocation into runtime path in `src/circuitry/cli/runtime_shim.py` with runtime metadata under `runtime.plugins`.
- Implemented failure isolation boundaries: plugin load/hook failures are non-fatal and recorded in `runtime.plugins.events` with per-hook diagnostics.
- Added conformance and observability tests in `tests/cli/test_plugins_runtime.py` covering success hooks, load failure visibility, hook-failure isolation, and failure-hook behavior on runtime errors.
- Added extension authoring guide in `docs/plugins.md` and linked from `docs/index.md`; added plugin test command to `docs/development-guide.md`.
- Included deterministic plugin fixtures for tests in `src/circuitry/dev_plugin_fixtures.py`.
- Verified quality gates: `pytest -q`, `ruff check src tests`, and `mypy src` all passing.

### File List

- `_bmad-output/implementation-artifacts/4-4-expose-plugin-and-tool-extension-points.md`
- `src/circuitry/core/plugins.py`
- `src/circuitry/cli/runtime_shim.py`
- `src/circuitry/dev_plugin_fixtures.py`
- `tests/cli/test_plugins_runtime.py`
- `docs/plugins.md`
- `docs/index.md`
- `docs/development-guide.md`
