---
title: 'Circuitry Orchestration Schema & Reference Doc'
slug: 'orchestration-schema-reference'
created: '2026-02-28'
status: 'ready-for-dev'
stepsCompleted: [1, 2, 3, 4]
tech_stack:
  - Python 3.x
  - jsonschema>=4.0 (new dependency)
  - PyYAML
  - Typer / Rich
  - pytest (tests)
files_to_modify:
  - src/circuitry/schema/__init__.py (new)
  - src/circuitry/schema/orchestration.schema.json (new)
  - docs/orchestration-reference.md (new)
  - requirements.txt
  - src/circuitry/cli/runtime_shim.py
  - orchestrations/meta_orchestrator.yml
  - orchestrations/orchestration_improver.yml
  - scripts/improve-orchestration
  - scripts/run-orchestration-ideas
  - tests/cli/test_validate.py (extend existing)
code_patterns:
  - validate() returns dict {"ok": bool, "errors": list[str]}
  - Schema loaded via Path(__file__).parent.parent / "schema" / "orchestration.schema.json" from runtime_shim.py
  - Tests use tmp_path fixture; import directly from circuitry.cli.runtime_shim
  - Scripts inject state as dict to run_orchestration(state={...})
test_patterns:
  - pytest with tmp_path fixture
  - _write(tmp_path, "name.yml", content) helper already exists in test_validate.py
  - New schema tests follow same _write + validate() + assert pattern as existing tests
---

# Tech-Spec: Circuitry Orchestration Schema & Reference Doc

**Created:** 2026-02-28

## Overview

### Problem Statement

Rules for writing valid Circuitry orchestrations are duplicated inline in `meta_orchestrator.yml` and `orchestration_improver.yml`. `circuitry validate` only runs the Python compiler — there is no JSON Schema pre-validation layer. LLMs authoring orchestrations have an incomplete, duplicated rule set with no single source of truth, causing drift and making it harder to keep all tooling aligned.

### Solution

Create a canonical `src/circuitry/schema/orchestration.schema.json` covering the full DOL type system, and a comprehensive `docs/orchestration-reference.md` human+LLM reference. Wire the JSON Schema into `circuitry validate` as a pre-compilation pass. Update `meta_orchestrator.yml` and `orchestration_improver.yml` to consume a `{{rules}}` Mustache variable injected by the calling scripts — scripts extract the `## LLM Authoring Rules` section from the reference doc and pass it as input state, eliminating inline rule duplication entirely.

### Scope

**In Scope:**
- `src/circuitry/schema/orchestration.schema.json` — JSON Schema for the full orchestration DOL, derived from `compiler.py` and the type system doc; bundled inside the Python package
- `docs/orchestration-reference.md` — comprehensive reference (all fields, effect types, state path rules, patterns/antipatterns, annotated examples per primitive, plus a delimited `## LLM Authoring Rules` section)
- Add `jsonschema>=4.0` to `requirements.txt`
- Update `src/circuitry/cli/runtime_shim.py:validate()` to run schema validation before compiler
- Update `orchestrations/meta_orchestrator.yml` — replace inline rules with `{{rules}}` variable
- Update `orchestrations/orchestration_improver.yml` — same
- Update `scripts/improve-orchestration` and `scripts/run-orchestration-ideas` to inject the `## LLM Authoring Rules` section as `rules` input state
- Extend `tests/cli/test_validate.py` with schema-specific tests

**Out of Scope:**
- GBNF grammar for constrained generation
- IDE rules files (CLAUDE.md, .cursorrules, copilot-instructions)
- Changes to compiler structural validation logic
- New runtime primitives or execution engine changes

## Context for Development

### Codebase Patterns

- `validate()` in `runtime_shim.py:269` currently: loads YAML → calls `compile_orchestration()` → returns `{"ok": bool, "errors": [str]}`. New schema validation runs before the compiler in this same function, returning the same shape on failure.
- `jsonschema` is not yet in `requirements.txt` — must be added.
- Effect names must match `^[A-Za-z_][A-Za-z0-9_]*$` and `iter_\d+` is a reserved pattern (compiler enforces this; schema enforces it too via `"pattern"` constraint for early detection).
- The two orchestration files both embed the same 7-point rule list inside their `draft_yaml` and `final_yaml` prompt templates. These blocks become `{{rules}}` references.
- `scripts/improve-orchestration` passes state as `{"input_yaml": ..., "original_prompt": ...}` at line 228 — `"rules"` is a new third key.
- `scripts/run-orchestration-ideas` passes state as `{"user_request": ...}` to meta_orchestrator at line 111 — `"rules"` is a new second key.
- Flow aliases: `chain_of_thought`, `cot` normalize to `chain`; `tree_of_thought`, `tot` normalize to `tree`. Schema `enum` for `flow` must include all six values.
- **CRITICAL**: Existing `test_validate_ok_for_valid_orchestration` uses a YAML with no `adapter` or `model` fields and expects `ok: True`. Schema must NOT require `adapter` or `model` at the document level — only `effects` is required.
- `test_examples_validate` in `tests/orchestrations/test_examples_smoke.py` validates all 7 curated examples — once schema is wired into `validate()`, this test automatically covers schema correctness for all examples.

### Files to Reference

| File | Purpose |
| ---- | ------- |
| `src/circuitry/cli/runtime_shim.py:269-279` | `validate()` — insert schema pass before `compile_orchestration()` |
| `src/circuitry/cli/runtime_shim.py:1-25` | Existing import pattern to follow |
| `src/circuitry/core/compiler.py` | Ground truth for all field constraints, name rules, defaults |
| `docs/Circuitry Type System 2f34435ec2e0808394e7ddbb86d14a89.md` | TypeScript-typed DOL — maps 1:1 to JSON Schema |
| `orchestrations/meta_orchestrator.yml:196-259` | `draft_yaml` and `final_yaml` prompts containing inline rules to replace |
| `orchestrations/orchestration_improver.yml:136-211` | `draft_yaml` and `final_yaml` prompts containing inline rules to replace |
| `scripts/improve-orchestration:60-70` | `PROJECT_ROOT` definition — add `RULES` constant extraction after this |
| `scripts/improve-orchestration:228` | `run_orchestration(state={...})` improver call — add `"rules": RULES` |
| `scripts/improve-orchestration:302` | `run_orchestration(state={...})` judge call — add `"rules": RULES` |
| `scripts/run-orchestration-ideas:38-45` | `PROJECT_ROOT` definition — add `RULES` constant extraction after this |
| `scripts/run-orchestration-ideas:111` | `run_orchestration(state={...})` meta_orchestrator call — add `"rules": RULES` |
| `tests/cli/test_validate.py` | Existing validate tests — follow exact same `_write` + `validate()` + assert pattern |
| `requirements.txt` | Add `jsonschema>=4.0` after PyYAML entry |

### Technical Decisions

- **Schema location**: Bundled inside the Python package at `src/circuitry/schema/orchestration.schema.json`. Loaded from `runtime_shim.py` via `Path(__file__).parent.parent / "schema" / "orchestration.schema.json"`. Works both from repo root and after `pip install` (unlike placing it in `docs/`).
- **Schema draft**: JSON Schema draft-07 for broadest `jsonschema` library compatibility (`"$schema": "http://json-schema.org/draft-07/schema#"`).
- **Schema top-level**: `effects` is the only required field. `adapter`, `model`, `flow`, `version` are optional. `additionalProperties: true` — runtime reads other keys (`runtime`, `plugins`, etc.) the schema must not reject.
- **Rules injection**: Scripts extract only the `## LLM Authoring Rules` section from `docs/orchestration-reference.md` (not the full doc) to keep token count manageable. Use a simple string split on `## LLM Authoring Rules` — everything after that heading is the rules text.
- **Graceful degradation**: If `orchestration.schema.json` is not found at load time (e.g. non-standard install), `validate()` skips the schema step and falls through to the compiler. This avoids breaking existing workflows during a partial deployment.
- **Error collection**: Use `jsonschema.Draft7Validator(schema).iter_errors(orch)` to collect ALL errors (not just the first), consistent with compiler reporting behavior.

## Implementation Plan

### Tasks

- [ ] Task 1: Create `src/circuitry/schema/__init__.py` and `src/circuitry/schema/orchestration.schema.json`
  - File: `src/circuitry/schema/__init__.py`
  - Action: Create empty file to make `schema` a Python package
  - File: `src/circuitry/schema/orchestration.schema.json`
  - Action: Write JSON Schema draft-07 covering the full DOL:
    - Top-level: `required: ["effects"]`, `properties`: `effects` (array of EffectDef, minItems 0), `adapter` (string), `model` (string), `flow` (enum: chain/chain_of_thought/cot/tree/tree_of_thought/tot), `version` (number). `additionalProperties: true`.
    - `EffectDef`: `oneOf` five types using JSON Schema `if/then` discrimination on `type` field:
      - `type: "prompt"` → required: `name` (pattern `^[A-Za-z_][A-Za-z0-9_]*$`), one-of `template`|`messages`; optional: `prompt_type` (enum: text/json/boolean/tool/number/array/object), `schema`, `model`, `provider`, `provider_fallbacks`, `params`, `timeout_ms`, `deterministic`, `inputs`, `assets`, `retries` (object with `max_attempts`, `backoff_ms`), `on_error` (enum: fail/skip/continue), `description`
      - `type: "dynamic"` → required: `name` (pattern), `effects` (array EffectDef, minItems 1); optional: `flow`, `max_concurrency`, `stop_on_error`, `on_error` (fail/skip/continue), `labels`, `description`
      - `type: "if"` or `"conditional"` → optional: `name`; required: `if` (object: `mode` enum model/cel + one-of `template`|`expr`), `then` (array); optional: `else` (array), `threshold`, `on_error`, `labels`, `description`
      - `type: "loop"` → optional: `name`; required: `body` (array, minItems 1); one-of `while` (object: `mode` + `template`|`expr`) | `each` (object: required `in` string, optional `as` string); optional: `max_iterations`, `min_iterations`, `on_error` (enum: fail/break/continue), `labels`, `description`
      - `type: "reflector"` → required: `name` (pattern), `effects` (array, minItems 1); optional: `flow`, `plan_from_step`, `max_iterations`, `generated_key`, `stop_on_done`, `max_effects`, `max_steps`, `prime_template`
  - Notes: Use `$defs` for `EffectDef` and `NamePattern` to avoid repetition. The `if/then` discrimination pattern in JSON Schema draft-07 produces cleaner error messages than `oneOf` alone.

- [ ] Task 2: Create `docs/orchestration-reference.md`
  - File: `docs/orchestration-reference.md`
  - Action: Write comprehensive reference with the following sections:
    1. **Overview** — DOL vs runtime split, declarative/deterministic design, two-world model
    2. **File Structure** — top-level fields table: `adapter` (string, optional, default from config), `model` (string, optional, default from config), `effects` (array, required), `flow` (string, optional, default chain), `version` (number, optional)
    3. **Effect Types** — one subsection per type, each with: field reference table (field / type / required / default / constraints), state output path pattern, annotated YAML example:
       - `prompt` — outputs at `prime.<name>.value`; note `messages` vs `template` mutual exclusivity
       - `dynamic` — chain (sequential, sees prior outputs) vs tree (parallel, sees input snapshot); outputs at `prime.<dyn_name>.<child_name>.value`
       - `if` / `conditional` — CEL mode (`state.prime.<name>.value` prefix required) vs model mode; same effect name required in both branches
       - `loop` — `each` (over JSON array from prior prompt_type:json effect) vs `while` (model or CEL continuation); iteration paths: `prime.<loop_name>.iter_N.<body_effect>.value`
       - `reflector` — planning-time effect that generates a Dynamic; inner effects path: `prime.<reflector_name>.plan.*`
    4. **State Path Addressing** — Mustache rules: `{{key}}` for initial state, `{{prime.name.value}}` for top-level effect, `{{prime.dyn.child.value}}` for nested effect inside dynamic; CEL: always `state.prime.<name>.value` (not `prime.<name>.value`); loop body references: `{{item}}` for current `each` element (or named binding)
    5. **Patterns & Antipatterns** — chain vs tree choice, naming named vs transparent loops/conditionals, staged prompts over single-shot complex generation, same inner name in if/else branches, loop `each.in` must point to a `prompt_type: json` array output
    6. **`## LLM Authoring Rules`** — concise rule set (10–15 rules) covering: top-level fields, effect type syntax, state path format for Mustache, CEL prefix requirement, loop each.in constraint, name pattern, same name in if/else branches, flow options. This section is extracted verbatim by scripts and injected as `{{rules}}`.
  - Notes: Keep the `## LLM Authoring Rules` section self-contained with no cross-references so it reads correctly when injected into a prompt without surrounding context.

- [ ] Task 3: Add `jsonschema>=4.0` to `requirements.txt`
  - File: `requirements.txt`
  - Action: Add `jsonschema>=4.0` on the line after `pyyaml>=6.0`

- [ ] Task 4: Wire schema validation into `validate()` in `runtime_shim.py`
  - File: `src/circuitry/cli/runtime_shim.py`
  - Action:
    1. Add at top of file alongside existing imports: `import json` (already imported inline in `_load_state` — move to top-level), `import jsonschema`
    2. Define a module-level helper `_load_schema() -> dict | None` that loads `Path(__file__).parent.parent / "schema" / "orchestration.schema.json"` and returns the parsed dict, or returns `None` if the file does not exist (graceful degradation)
    3. In `validate()` at line 269, after the empty-file check and YAML parse, before `compile_orchestration()`:
       ```python
       schema = _load_schema()
       if schema is not None:
           validator = jsonschema.Draft7Validator(schema)
           schema_errors = sorted(validator.iter_errors(orch), key=str)
           if schema_errors:
               return {"ok": False, "errors": [e.message for e in schema_errors]}
       ```
    4. Leave the `compile_orchestration()` call and its exception handler unchanged below
  - Notes: `json` is currently imported inside `_load_state` — move the import to the top of the file as part of this task to keep it consistent.

- [ ] Task 5: Update `orchestrations/meta_orchestrator.yml` to use `{{rules}}`
  - File: `orchestrations/meta_orchestrator.yml`
  - Action:
    1. In the `draft_yaml` prompt template: replace the `CIRCUITRY YAML RULES — follow these exactly:` block and its bullet list with a single line: `Authoring rules to follow exactly:\n{{rules}}`
    2. In the `final_yaml` prompt template: replace the inline rules block with `Use the authoring rules in {{rules}} as your reference for what constitutes a structurally correct file.` — keep the seven numbered review CHECK items unchanged (they are review questions, not rules)
    3. In the header comment block, add `rules` to the `─── INPUT STATE ───` table: `rules  string  LLM authoring rules — injected by scripts/run-orchestration-ideas or pass manually`
  - Notes: The `{{rules}}` Mustache variable is populated by the calling script. If run manually without passing `rules` in state, Mustache renders it as empty string — the existing seven checks in `final_yaml` still catch structural issues.

- [ ] Task 6: Update `orchestrations/orchestration_improver.yml` to use `{{rules}}`
  - File: `orchestrations/orchestration_improver.yml`
  - Action:
    1. In `draft_yaml` prompt: replace `CIRCUITRY YAML RULES — follow exactly:` block with `Authoring rules to follow exactly:\n{{rules}}`
    2. In `final_yaml` prompt: replace inline rules block with `Use the authoring rules in {{rules}} as your reference.` — keep review checklist items unchanged
    3. In header comment: add `rules` to input state table
  - Notes: Same approach as Task 5.

- [ ] Task 7: Update `scripts/improve-orchestration` to inject `rules`
  - File: `scripts/improve-orchestration`
  - Action:
    1. Define a module-level helper function `_load_rules(project_root: Path) -> str` that:
       - Reads `project_root / "docs" / "orchestration-reference.md"`
       - Splits on `## LLM Authoring Rules` and returns everything after that heading stripped
       - Returns `""` if file does not exist or heading not found (graceful fallback)
    2. After `PROJECT_ROOT` is defined (~line 61), add: `RULES = _load_rules(PROJECT_ROOT)`
    3. At the `run_orchestration(state={"input_yaml": current_yaml, "original_prompt": prompt_for_iter}, ...)` call (~line 228): add `"rules": RULES` to the state dict
    4. At the judge `run_orchestration(state={"yaml_a": ..., "yaml_b": ...}, ...)` call (~line 302): add `"rules": RULES` (harmless extra key, judge ignores it)
  - Notes: The `_load_rules` helper should be defined near the top of the file, after the imports section.

- [ ] Task 8: Update `scripts/run-orchestration-ideas` to inject `rules`
  - File: `scripts/run-orchestration-ideas`
  - Action:
    1. Add same `_load_rules(project_root: Path) -> str` helper function (or import from a shared location if preferred — but since these are standalone scripts, duplicate the small helper)
    2. After `PROJECT_ROOT` is defined (~line 39), add: `RULES = _load_rules(PROJECT_ROOT)`
    3. At the `run_orchestration(state={"user_request": user_request}, ...)` call (~line 111): add `"rules": RULES` to the state dict
  - Notes: `_load_rules` is small enough that duplicating it across two scripts is acceptable. No shared utility module needed.

- [ ] Task 9: Add schema validation tests to `tests/cli/test_validate.py`
  - File: `tests/cli/test_validate.py`
  - Action: Append three new test functions after the existing tests, using the existing `_write` helper:
    ```python
    def test_validate_schema_rejects_missing_effects(tmp_path: Path) -> None:
        path = _write(tmp_path, "no_effects.yml", "adapter: ollama\nmodel: llama3\n")
        result = validate(path)
        assert result["ok"] is False
        assert len(result["errors"]) >= 1

    def test_validate_schema_rejects_invalid_effect_type(tmp_path: Path) -> None:
        path = _write(tmp_path, "bad_type.yml",
            "effects:\n  - type: not_a_real_type\n    name: x\n    template: hello\n")
        result = validate(path)
        assert result["ok"] is False
        assert len(result["errors"]) >= 1

    def test_validate_schema_accepts_minimal_valid(tmp_path: Path) -> None:
        # adapter and model must NOT be required by the schema
        path = _write(tmp_path, "minimal.yml",
            "effects:\n  - type: prompt\n    name: greet\n    template: Hello\n")
        result = validate(path)
        assert result["ok"] is True
        assert result["errors"] == []
    ```
  - Notes: These tests verify the schema boundary conditions. `test_examples_validate` in `test_examples_smoke.py` covers AC6 (all curated examples pass) automatically once the schema is wired in.

### Acceptance Criteria

- [ ] AC1: Given an orchestration YAML with no `effects` key (e.g. `adapter: ollama\nmodel: llama3`), when `circuitry validate` is run, then `ok` is `False` and `errors` is non-empty with a schema-level message.

- [ ] AC2: Given any of the 7 curated orchestrations in `orchestrations/`, when `circuitry validate orchestrations/<file>.yml` is run, then `ok` is `True` and `errors` is empty.

- [ ] AC3: Given an orchestration YAML with only `effects` and no `adapter` or `model`, when `circuitry validate` is run, then `ok` is `True` — confirming `adapter` and `model` are not required by the schema.

- [ ] AC4: Given `docs/orchestration-reference.md` exists and contains `## LLM Authoring Rules`, when `scripts/improve-orchestration --orchestration <file> --prompt "..."` is run, then the state dict passed to `run_orchestration` contains a non-empty `"rules"` key.

- [ ] AC5: Given `orchestrations/meta_orchestrator.yml` and `orchestrations/orchestration_improver.yml`, when their `draft_yaml` prompt templates are inspected, then they contain `{{rules}}` and do NOT contain the literal string `CIRCUITRY YAML RULES — follow`.

- [ ] AC6: Given `pytest tests/orchestrations/test_examples_smoke.py` is run after this spec is implemented, then all `test_examples_validate` parametrize cases pass (schema + compiler both accept all 7 curated examples).

- [ ] AC7: Given `pytest tests/cli/test_validate.py` is run, then all existing tests still pass (no regressions) and all three new schema tests pass.

## Additional Context

### Dependencies

- `jsonschema>=4.0` — new runtime dependency; add to `requirements.txt`. No other new dependencies.

### Testing Strategy

**Unit tests (automated):**
- `pytest tests/cli/test_validate.py` — runs existing + 3 new schema tests (AC1, AC3, AC7)
- `pytest tests/orchestrations/test_examples_smoke.py` — validates all 7 curated examples against schema + compiler (AC2, AC6)

**Manual verification:**
- Run `pytest -q` from repo root to confirm zero regressions across all test suites
- `grep -n 'CIRCUITRY YAML RULES' orchestrations/meta_orchestrator.yml orchestrations/orchestration_improver.yml` — should return no matches (AC5)
- `grep -n '{{rules}}' orchestrations/meta_orchestrator.yml orchestrations/orchestration_improver.yml` — should show matches in both files
- `scripts/improve-orchestration --orchestration orchestrations/_prompt.yml --prompt "test" --dry-run` — confirm it runs without error (AC4)

### Notes

- **Schema `if/then` discrimination**: Prefer JSON Schema `if/then` over raw `oneOf` for effect type discrimination — cleaner error messages. Structure: top-level `if: {properties: {type: {const: "prompt"}}}` → `then: {$ref: "#/$defs/PromptEffect"}` repeated for each type.
- **`json` import**: Currently `runtime_shim.py` imports `json` inside the `_load_state` function body. Move it to the top-level imports as part of Task 4 for consistency.
- **`iter_N` name reservation**: The compiler rejects names matching `iter_\d+` (reserved for loop iteration segments). The schema can enforce `not: {pattern: "^iter_\\d+$"}` on name fields — catches this before the compiler, with a clearer error.
- **`scripts/run-orchestration-ideas` also calls `improve-orchestration`**: It does so via `subprocess.run` (line 181) — no state injection needed there since `improve-orchestration` handles its own `RULES` injection.
- **Future**: If the reference doc grows very large, consider extracting only the `## LLM Authoring Rules` section into a standalone `docs/llm-rules.md` file. For now, a single well-structured `orchestration-reference.md` with a delimited section is cleaner.
