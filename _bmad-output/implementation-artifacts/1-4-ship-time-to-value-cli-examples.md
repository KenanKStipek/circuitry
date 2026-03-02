# Story 1.4: Ship Time-to-Value CLI Examples

Status: done

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a new user,
I want built-in examples that run immediately,
so that I can confirm the framework works before writing custom orchestration logic.

## Acceptance Criteria

1. **Given** a fresh local setup with documented prerequisites  
   **When** I run the quick-start example command  
   **Then** a successful orchestration run starts within the NFR target and produces expected output.
2. **Given** README quick-start documentation  
   **When** new users follow it  
   **Then** they can complete first run without external tribal knowledge.
3. **Given** shipped example orchestrations  
   **When** run by users and CI checks  
   **Then** examples remain aligned with current runtime behavior and state path semantics.

## Tasks / Subtasks

- [x] Define a canonical quick-start command and ensure it uses a reliable local example orchestration (AC: 1, 2)
- [x] Tighten README quick-start sequence with prerequisites, expected outputs, and state file inspection path (AC: 2)
- [x] Validate and refresh `examples/*.yml` so they execute under current compiler/runtime behavior (AC: 1, 3)
- [x] Add lightweight automated smoke checks for representative examples (prompt, dynamic, conditional, loop) (AC: 3)
- [x] Document expected completion time and common setup pitfalls with direct remediation steps (AC: 1, 2)

## Dev Notes

- Story source: `_bmad-output/planning-artifacts/epics.md` (Epic 1 details and acceptance criteria).
- Architecture constraints: `_bmad-output/planning-artifacts/architecture.md` State Path Contract.
- Baseline architecture references: `docs/architecture.md`, `docs/project-overview.md`.
- Existing implementation paths: `src/circuitry/cli/`, `src/circuitry/core/`, `src/circuitry/core/store/store.py`.

### Cross-Reference: Existing Code vs Story Scope

- Already present:
  - README includes quick-start CLI commands (`circuitry run examples/hello.yml` and dry-run variants).
  - Example orchestration set exists under `examples/` (`hello`, `dynamic_hello`, `conditional_example`, `loop_example`, `typed_prompt_example`, `reflector_v1`).
  - CLI commands `run`, `validate`, `inspect`, and `doctor` are wired in `src/circuitry/cli/app.py`.
- Gaps to close for this story:
  - Example readiness is uneven: some examples omit model/adapter defaults and depend on external config resolution.
  - There is no committed smoke-test suite currently validating examples end-to-end in CI.
  - Local execution prerequisites are under-specified for fresh environments (module path + required packages like Typer/Rich/PyYAML not guaranteed unless setup is completed).
  - README “programmatic” section currently has API drift (`create_adapter` vs actual `build_adapter`), which weakens time-to-value onboarding.

### Source Code Anchors

- `README.md`
- `examples/hello.yml`
- `examples/dynamic_hello.yml`
- `examples/conditional_example.yml`
- `examples/loop_example.yml`
- `src/circuitry/cli/app.py`

### Technical Requirements

- Primary files: `README.md`, `examples/*.yml`, `src/circuitry/cli/app.py` (if command UX adjustments needed).
- Keep examples deterministic enough for reproducible state-shape validation.
- Preserve compatibility with adapter configuration behavior in `runtime_shim` and config resolution flow.

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

- Example orchestrations under `examples/` validated and refreshed to execute under current compiler/runtime.
- README quick-start updated with canonical run command, prerequisites, expected outputs, and state inspection path. Fixed API drift (`create_adapter` → `build_adapter`).
- Added parameterized smoke tests in `tests/orchestrations/test_examples_smoke.py` covering 7 example orchestrations (validate, inspect, dry-run).
- Added documentation contract tests in `tests/docs/test_documentation_contracts.py` verifying README quick-start and API reference symbol accuracy.
- Quality gates pass: `pytest`, `ruff check .`, `mypy src`.

### File List

- `_bmad-output/implementation-artifacts/1-4-ship-time-to-value-cli-examples.md`
- `README.md`
- `examples/hello.yml`
- `examples/dynamic_hello.yml`
- `examples/conditional_example.yml`
- `examples/loop_example.yml`
- `tests/orchestrations/test_examples_smoke.py`
- `tests/docs/test_documentation_contracts.py`
