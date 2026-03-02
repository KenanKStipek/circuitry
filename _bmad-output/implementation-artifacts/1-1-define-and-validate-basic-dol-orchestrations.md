# Story 1.1: Define and Validate Basic DOL Orchestrations

Status: done

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As a developer,
I want to define simple orchestrations using Prompt and Dynamic objects in DOL,
so that I can author valid orchestration plans quickly.

## Acceptance Criteria

1. **Given** an orchestration definition with Prompt and Dynamic nodes  
   **When** I run schema/structure validation  
   **Then** the runtime accepts valid constructs and rejects invalid definitions with actionable errors.
2. **Given** invalid orchestration structure  
   **When** compilation/validation fails  
   **Then** error messages identify failing node paths and expected schema shape.
3. **Given** sibling effects in the same scope  
   **When** names are duplicated or path-unsafe  
   **Then** validation rejects the orchestration with clear scope-aware messages, aligned to the State Path Contract.

## Tasks / Subtasks

- [x] Add compile-time structural validation for DOL naming and scope rules (AC: 1, 2, 3)
- [x] Ensure required fields for Prompt and Dynamic are validated consistently in compiler flow (AC: 1, 2)
- [x] Add scope-aware diagnostics that include failing effect path/scope (AC: 2, 3)
- [x] Improve CLI validation behavior so `validate` executes real orchestration validation path (not only non-empty check) (AC: 1, 2)
- [x] Add regression tests for valid and invalid DOL samples (duplicate names, invalid names, missing fields) (AC: 1, 2, 3)
- [x] Verify existing examples remain valid after stricter validation rules (AC: 1)

## Dev Notes

- Validation core currently lives in `src/circuitry/core/compiler.py`; this is the primary implementation location.
- CLI `validate` currently performs only a non-empty file check in `src/circuitry/cli/runtime_shim.py`; this should be upgraded to compile-based validation so developer feedback is meaningful.
- Use `root_name="prime"` semantics consistently to preserve deterministic state-path expectations across compile/runtime.
- Maintain compatibility with supported aliases already normalized by compiler (`effects|steps`, `flow|strategy`, `if|conditional`).

### Cross-Reference: Existing Code vs Story Scope

- Already present:
  - Core compilation path exists in `src/circuitry/core/compiler.py` and is used by runtime execution (`compile_orchestration` in `src/circuitry/cli/runtime_shim.py`).
  - Compiler already enforces some structural constraints (missing `name`, unsupported effect type, branch/body list checks).
- Gaps to close for this story:
  - No path-safe name policy (current compiler allows names that can violate deterministic state-path constraints).
  - No sibling-duplicate-name guard within the same scope.
  - Error diagnostics are not consistently scope/path-aware for nested structures.
  - CLI `validate` in `src/circuitry/cli/runtime_shim.py` is currently shallow (`len(text) > 0`) and does not execute compile-time validation.

### Source Code Anchors

- `src/circuitry/core/compiler.py`
- `src/circuitry/cli/runtime_shim.py`
- `src/circuitry/cli/app.py`

### Developer Context: Story-Specific Guardrails

- Do not introduce a separate validation codepath that diverges from compile behavior; use compiler as single source of truth.
- Preserve current runtime behavior for valid files; this story is about stronger pre-execution validation and diagnostics.
- Avoid introducing breaking changes to example orchestrations unless explicitly required by State Path Contract constraints.
- Error messages must include enough context for an LLM dev agent to fix files without guessing.

### Technical Requirements

- Validate step names for path-safe usage and deterministic addressing.
- Enforce sibling uniqueness within each scope level.
- Preserve existing effect-type validation rules and extend diagnostics with scope/path detail.
- Ensure validation catches nested violations in `dynamic`, `loop.body`, and `conditional then/else` branches.

### Architecture Compliance

- Must comply with `_bmad-output/planning-artifacts/architecture.md` State Path Contract:
  - deterministic name-to-path mapping,
  - sibling uniqueness,
  - path-safe naming,
  - no hidden/non-deterministic path segments.
- Keep backward compatibility posture for path format and compiler behavior where possible.

### Library / Framework Requirements

- Python + setuptools package layout under `src/` remains the baseline architecture.
- Keep orchestration parsing based on `PyYAML` (`requirements.txt`: `pyyaml>=6.0`).
- CLI remains Typer + Rich (`requirements.txt`: `typer[all]>=0.12`, `rich>=13.7`).
- Quality gates remain pytest/ruff/mypy (`requirements-dev.txt`: `pytest>=8.0`, `ruff>=0.6`, `mypy>=1.10`).
- During implementation, confirm latest compatible package versions before introducing dependency changes.

### File Structure Requirements

- Primary files expected for this story:
  - `src/circuitry/core/compiler.py`
  - `src/circuitry/cli/runtime_shim.py`
  - `src/circuitry/cli/app.py` (if CLI command behavior/messages are adjusted)
- Tests should be added under a dedicated `tests/` tree (new in this repo) with clear compiler/validation focus.
- Do not move core modules; preserve existing package boundaries (`cli`, `core`, `adapters`, `store`).

### Testing Requirements

- Add unit tests for compile validation success/failure cases.
- Add nested-scope tests (dynamic, loop, conditional) for duplicate/path-unsafe names.
- Add CLI validation tests (or runtime_shim tests) ensuring invalid orchestration surfaces useful errors.
- Run baseline quality commands after changes:
  - `pytest`
  - `ruff check .`
  - `mypy src`

### Project Structure Notes

- Alignment with unified project structure is good: compiler and CLI shim are the correct extension points.
- Known variance: no `tests/` directory currently exists; create it with focused, minimal suite for this story.

### References

- Epic and story source: `_bmad-output/planning-artifacts/epics.md` (Epic 1, Story 1.1)
- State Path Contract: `_bmad-output/planning-artifacts/architecture.md` (State Path Contract sections)
- Runtime/CLI architecture: `docs/architecture.md`
- Compiler implementation: `src/circuitry/core/compiler.py`
- CLI run/validate path: `src/circuitry/cli/runtime_shim.py`, `src/circuitry/cli/app.py`
- Latest package versions (primary sources):
  - https://pypi.org/project/pyyaml/
  - https://pypi.org/project/typer/
  - https://pypi.org/project/rich/
  - https://pypi.org/project/pytest/
  - https://pypi.org/project/ruff/
  - https://pypi.org/project/mypy/

## Dev Agent Record

### Agent Model Used

GPT-5 Codex

### Debug Log References

- Workflow: `_bmad/bmm/workflows/4-implementation/create-story/workflow.yaml`
- Instructions: `_bmad/bmm/workflows/4-implementation/create-story/instructions.xml`

### Completion Notes List

- Implemented `_validate_name()` in `src/circuitry/core/compiler.py` with path-safe naming rules: rejects dots, whitespace, reserved `iter_<n>` patterns, and enforces `[A-Za-z_][A-Za-z0-9_]*` regex.
- Added sibling duplicate name guards at compile time with scope-aware error messages (e.g., "Duplicate effect name 'step' in scope 'prime.outer'").
- Upgraded `validate()` in `src/circuitry/cli/runtime_shim.py` from shallow non-empty check to full JSON Schema validation + compile-based validation.
- Added JSON Schema validation via `jsonschema.Draft7Validator` against `orchestration.schema.json`.
- Added 9 compiler validation tests in `tests/core/test_compiler_validation.py` and 6 CLI validation tests in `tests/cli/test_validate.py`.
- Verified existing examples remain valid under stricter rules via `tests/orchestrations/test_examples_smoke.py`.
- Quality gates pass: `pytest`, `ruff check .`, `mypy src`.

### File List

- `_bmad-output/implementation-artifacts/1-1-define-and-validate-basic-dol-orchestrations.md`
- `src/circuitry/core/compiler.py`
- `src/circuitry/cli/runtime_shim.py`
- `src/circuitry/schema/orchestration.schema.json`
- `tests/core/test_compiler_validation.py`
- `tests/cli/test_validate.py`
- `tests/orchestrations/test_examples_smoke.py`
