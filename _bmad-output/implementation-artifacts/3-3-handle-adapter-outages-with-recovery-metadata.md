# Story 3.3: Handle Adapter Outages with Recovery Metadata

Status: ready-for-dev

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As an operator,
I want adapter failures to produce structured failure and fallback records,
so that I can recover runs and minimize downtime impact.

## Acceptance Criteria

1. **Given** an orchestration execution where the primary adapter fails  \n   **When** runtime applies configured fallback/recovery behavior  \n   **Then** failure context, fallback path, and final outcome are written to execution metadata.
2. **Given** adapter/runtime failure scenarios  \n   **When** execution terminates  \n   **Then** no failure is silent and diagnosable records are preserved.
3. **Given** multi-adapter configuration  \n   **When** fallback policy is enabled  \n   **Then** runtime attempts configured alternate adapter/model paths deterministically.

## Tasks / Subtasks

- [ ] Define adapter fallback policy model at runtime configuration level (AC: 1, 3)
- [ ] Implement retry/fallback orchestration around adapter invocation boundaries (AC: 1, 3)
- [ ] Persist structured failure metadata including adapter/model/error/fallback-attempts (AC: 1, 2)
- [ ] Ensure all failure modes surface through CLI/API responses and state records (AC: 2)
- [ ] Add tests for outage simulation, fallback success, and fallback exhaustion paths (AC: 1, 2, 3)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 3 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/core/`, `src/circuitry/cli/`, `src/circuitry/adapters/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- Adapter interfaces return token/response metadata and raise runtime errors on transport/provider failures.
- Prompt/dynamic/loop/conditional runtimes already record `meta.error` and completion timestamps on failure paths.
- Factory supports multiple adapters (`ollama`, `openai`, `anthropic`, `litellm`) as a baseline for fallback sequencing.
- Gaps to close for this story:
- No implemented adapter fallback orchestration currently exists despite `provider_fallbacks` field in prompt definition/compiler.
- Failure metadata is present but not yet standardized for outage/fallback attempt chains.
- No outage/fallback conformance tests currently exist.

### Source Code Anchors

- `src/circuitry/core/prompt.py:77`
- `src/circuitry/core/compiler.py:289`
- `src/circuitry/adapters/factory.py:12`
- `src/circuitry/core/prompt.py:178`
- `src/circuitry/core/dynamic.py:94`

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

- Story context generated from Epic 3 source and architecture constraints.
- Included existing-code cross-reference with present capabilities and concrete gaps.
- Ready for `dev-story` implementation workflow.

### File List

- `_bmad-output/implementation-artifacts/3-3-handle-adapter-outages-with-recovery-metadata.md`
