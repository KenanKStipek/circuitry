# Story 2.1: Implement Chain and Tree Effect Topologies

Status: ready-for-dev

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a developer,
I want to express chain and tree orchestration flow patterns,
so that I can encode different execution structures explicitly in DOL.

## Acceptance Criteria

1. **Given** orchestration definitions using chain and tree patterns  \
   **When** the compiler normalizes these definitions for execution  \
   **Then** runtime executes effects in the expected deterministic structure.
2. **Given** dynamic flow declarations with aliases (`chain_of_thought`, `tree_of_thought`, etc.)  \
   **When** orchestration is compiled  \
   **Then** flow is normalized to canonical chain/tree semantics consistently.
3. **Given** executed chain/tree dynamics  \
   **When** state is inspected  \
   **Then** resulting paths and metadata reflect declared topology consistently.

## Tasks / Subtasks

- [ ] Validate and harden flow normalization behavior for root and nested dynamics (AC: 1, 2)
- [ ] Ensure runtime semantics for chain/tree are explicit and testable (AC: 1)
- [ ] Ensure dynamic metadata records canonical flow value and execution outcome (AC: 3)
- [ ] Add test fixtures covering chain/tree aliases and nested flow combinations (AC: 1, 2, 3)
- [ ] Document topology behavior with at least one chain and one tree example path walk (AC: 3)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 2 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/core/`, `src/circuitry/cli/`, `src/circuitry/core/store/store.py`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- Flow alias normalization exists in compiler via `_normalize_flow` with `chain/tree` aliases (`src/circuitry/core/compiler.py:22`).
- Root and nested dynamic flow values are normalized during compile (`src/circuitry/core/compiler.py:44`, `src/circuitry/core/compiler.py:66`).
- Runtime persists canonical `flow` in dynamic metadata and rejects unsupported values (`src/circuitry/core/dynamic.py:71`, `src/circuitry/core/dynamic.py:76`).
- Gaps to close for this story:
- Runtime execution loop currently processes effects the same way regardless of `chain` vs `tree` (`src/circuitry/core/dynamic.py:85`); topology-specific behavior is not differentiated yet.
- No committed test harness currently validates chain/tree semantics and state-path outcomes.
- Docs/examples do not currently provide a canonical side-by-side chain vs tree state inspection walkthrough.

### Source Code Anchors

- `src/circuitry/core/compiler.py`
- `src/circuitry/core/dynamic.py`
- `docs/architecture.md`
- `examples/dynamic_hello.yml`

### Technical Requirements

- Keep compile/runtime behavior deterministic and aligned with declared DOL semantics.
- Preserve root and nested path composition under `prime` with named child segments.
- Any behavior change must include corresponding deterministic state-path assertions.

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

- Keep changes scoped to existing module boundaries (`core`, `cli`, `store`, docs/examples/tests as needed).
- Avoid introducing duplicate orchestration execution paths.
- Add tests in a dedicated `tests/` tree when implementing this story.

### Testing Requirements

- Add unit/integration tests directly tied to story acceptance criteria.
- Include deterministic state-path assertions for relevant execution patterns.
- Verify no regressions to existing example orchestration behavior.

### Project Structure Notes

- Current repo has no committed `tests/` tree; implementation should introduce one deliberately with focused scope.
- Preserve current runtime semantics where unchanged by explicit story acceptance criteria.

### References

- `_bmad-output/planning-artifacts/epics.md`
- `_bmad-output/planning-artifacts/architecture.md`
- `docs/architecture.md`
- `src/circuitry/core/compiler.py`
- `src/circuitry/core/dynamic.py`
- `src/circuitry/core/conditional.py`
- `src/circuitry/core/loop.py`

## Dev Agent Record

### Agent Model Used

GPT-5 Codex

### Debug Log References

- Workflow: `_bmad/bmm/workflows/4-implementation/create-story/workflow.yaml`
- Instructions: `_bmad/bmm/workflows/4-implementation/create-story/instructions.xml`

### Completion Notes List

- Story context generated from Epic 2 source and architecture constraints.
- Included existing-code cross-reference with present capabilities and concrete gaps.
- Ready for `dev-story` implementation workflow.

### File List

- `_bmad-output/implementation-artifacts/2-1-implement-chain-and-tree-effect-topologies.md`
