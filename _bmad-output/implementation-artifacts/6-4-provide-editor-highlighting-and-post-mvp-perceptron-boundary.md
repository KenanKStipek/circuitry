# Story 6.4: Provide Editor Highlighting and Post-MVP Perceptron Boundary

Status: ready-for-dev

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a developer,
I want editor highlighting support now and clear post-MVP Perceptron boundaries,
so that authoring experience improves without destabilizing MVP scope.

## Acceptance Criteria

1. **Given** editor extension support scope and roadmap notes  
   **When** users author orchestration files  
   **Then** syntax and highlighting support improves authoring clarity in MVP.
2. **Given** roadmap boundaries  
   **When** MVP commitments are documented  
   **Then** Perceptron implementation remains explicitly out of MVP delivery scope.

## Tasks / Subtasks

- [ ] Define MVP editor-highlighting scope (file extensions, token classes, packaging target, install instructions) (AC: 1)
- [ ] Deliver initial highlighting artifact (grammar/rules) and example fixtures for validation (AC: 1)
- [ ] Add docs for editor support setup, known limitations, and contribution workflow (AC: 1)
- [ ] Document explicit MVP vs post-MVP boundary for Perceptron with scope guardrails (AC: 2)
- [ ] Add verification checks for highlighting behavior against representative orchestration files (AC: 1)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 6 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline planning references: `_bmad-output/planning-artifacts/prd.md`.
- Existing implementation/documentation paths: `docs/`, `examples/`, `_bmad-output/planning-artifacts/`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
- MVP planning artifacts already call out editor highlighting as MVP and Perceptron as post-MVP.
- Example YAML artifacts exist and can be used as fixtures for highlighting token coverage.
- Gaps to close for this story:
- No editor extension/highlighting grammar or package currently exists in repo.
- No user-facing documentation currently explains how to install/use orchestration syntax highlighting.
- No explicit MVP boundary document currently operationalizes Perceptron deferral for implementation planning.

### Source Code Anchors

- `_bmad-output/planning-artifacts/epics.md:484`
- `_bmad-output/planning-artifacts/prd.md:376`
- `_bmad-output/planning-artifacts/prd.md:425`
- `_bmad-output/planning-artifacts/prd.md:433`
- `examples/hello.yml`
- `examples/typed_prompt_example.yml`

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

- Keep changes scoped to existing module boundaries (`docs`, `examples`, editor-extension folder if introduced, tests/tooling as needed).
- Avoid introducing duplicate orchestration execution paths.
- Add tests in a dedicated `tests/` tree when implementing executable highlighting checks.

### Testing Requirements

- Add checks tied to story acceptance criteria (highlighting behavior on representative YAML fixtures).
- Include deterministic state-path assertions and failure-diagnostics checks where runtime behavior is touched.
- Verify no regressions to existing example orchestration behavior.

### Project Structure Notes

- Current repo has no committed `tests/` tree; implementation should introduce one deliberately with focused scope.
- Preserve current runtime semantics where unchanged by explicit story acceptance criteria.

### References

- `_bmad-output/planning-artifacts/epics.md`
- `_bmad-output/planning-artifacts/prd.md`
- `_bmad-output/planning-artifacts/architecture.md`
- `examples/`
- `docs/index.md`

## Dev Agent Record

### Agent Model Used

GPT-5 Codex

### Debug Log References

- Workflow: `_bmad/bmm/workflows/4-implementation/create-story/workflow.yaml`
- Instructions: `_bmad/bmm/workflows/4-implementation/create-story/instructions.xml`

### Completion Notes List

- Story context generated from Epic 6 source and architecture constraints.
- Included existing-code cross-reference with concrete highlighting and Perceptron-boundary gaps.
- Ready for `dev-story` implementation workflow.

### File List

- `_bmad-output/implementation-artifacts/6-4-provide-editor-highlighting-and-post-mvp-perceptron-boundary.md`
