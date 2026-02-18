# Story 4.2: Integrate LiteLLM and Direct Provider Conformance

Status: ready-for-dev

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a platform engineer,
I want LiteLLM and direct providers validated against conformance scenarios,
so that provider behavior is portable and predictable.

## Acceptance Criteria

1. **Given** at least LiteLLM and two direct provider adapters  \
   **When** adapter conformance tests are executed  \
   **Then** all required capabilities pass defined scenarios.
2. **Given** adapter capability or contract mismatches  \
   **When** conformance checks fail  \
   **Then** incompatibilities are surfaced with actionable diagnostics.
3. **Given** normalized adapter contract requirements  \
   **When** new providers are added  \
   **Then** they can be validated with the same conformance suite.

## Tasks / Subtasks

- [ ] Define adapter conformance contract and required capabilities (response shape, tokens, error behavior) (AC: 1, 3)
- [ ] Build conformance fixtures spanning LiteLLM + direct providers (OpenAI/Anthropic/Ollama) (AC: 1)
- [ ] Add failure diagnostics for contract mismatches (AC: 2)
- [ ] Add test harness support for provider stubs/mocks to avoid flaky network dependence (AC: 1, 2)
- [ ] Document conformance onboarding steps for future adapters (AC: 3)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 4 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/adapters/`, `src/circuitry/cli/`, `src/circuitry/core/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- Adapter implementations exist for LiteLLM and direct providers with normalized `generate()` return contract.
- `GenerateResult` already includes normalized fields (`text`, `raw`, `tokens_sent`, `tokens_received`).
- Adapter build path is centralized through factory wiring.
- Gaps to close for this story:
- No conformance test suite currently exists to prove cross-provider behavior equivalence.
- Adapter error and token edge-case consistency is not currently regression-locked by tests.
- Provider integration diagnostics are implementation-specific; no unified conformance report format yet.

### Source Code Anchors

- `src/circuitry/adapters/base.py:8`
- `src/circuitry/adapters/litellm.py:11`
- `src/circuitry/adapters/openai.py:12`
- `src/circuitry/adapters/anthropic.py:12`
- `src/circuitry/adapters/factory.py:12`

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

- Story context generated from Epic 4 source and architecture constraints.
- Included existing-code cross-reference with present capabilities and concrete gaps.
- Ready for `dev-story` implementation workflow.

### File List

- `_bmad-output/implementation-artifacts/4-2-integrate-litellm-and-direct-provider-conformance.md`
