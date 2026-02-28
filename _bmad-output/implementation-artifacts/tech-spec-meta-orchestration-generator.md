---
title: 'Meta-Orchestration Generator'
slug: 'meta-orchestration-generator'
created: '2026-02-28'
status: 'Completed'
stepsCompleted: [1, 2, 3, 4]
tech_stack: ['Python', 'PyYAML', 'Chevron/Mustache', 'Typer', 'Circuitry DSL', 'CEL (Common Expression Language)']
files_to_modify:
  - 'examples/meta_orchestration_generator.yml'
  - 'examples/README.md'
code_patterns:
  - 'dynamic(chain) for sequential multi-step chains'
  - 'loop(each) over json-typed prompt output arrays'
  - 'if(cel) with same effect name in both branches for consistent downstream state path'
  - 'Mustache interpolation: {{key}} for initial state, {{prime.name.value}} for effect outputs'
  - 'CEL expressions use state.prime.* prefix'
  - 'YAML comments used for documentation and usage instructions'
test_patterns:
  - 'examples/manifest.json version bump if behavior changes'
  - 'pytest -q tests/examples/test_examples_smoke.py smoke test'
---

# Tech-Spec: Meta-Orchestration Generator

**Created:** 2026-02-28

## Overview

### Problem Statement

No example exists demonstrating circuitry being used to generate new orchestration YAML from natural language — the "use circuitry to build circuitry" pattern. Users who want to scaffold a new orchestration have no reference for how to decompose that reasoning into the framework's own primitives.

### Solution

A multi-step `dynamic/chain` orchestration that accepts a user's natural language prompt as initial state, analyzes intent, elaborates each planned step (via loop), branches on complexity (via if), drafts the YAML, and self-refines — outputting a ready-to-use `.yml` as a text value in the state store. The user retrieves the generated YAML from `out.json` and saves it as a new orchestration file.

### Scope

**In Scope:**
- New example file: `examples/meta_orchestration_generator.yml`
- Accepts user prompt via initial state file (`--state input.json` where `input.json` contains `{"user_prompt": "..."}`)
- 5-step chain using `dynamic`, `loop`, `if`, and `prompt` primitives — no `reflector`
- Model-agnostic; recommended minimum capability documented in YAML header comments
- Output: final generated YAML stored at `prime.generate.final_yaml.value` in state
- New entry added to `examples/README.md` index table

**Out of Scope:**
- Auto-writing generated YAML file to disk (user extracts from state)
- State-file input mode (using existing `out.json` as seed input) — V2
- Formal schema validation of the generated YAML output

## Context for Development

### Codebase Patterns

- All orchestration files live in `examples/` as `.yml` files
- **Initial state**: `--state <filepath>` flag only — no inline JSON. File content keys become root-level template variables: `{"user_prompt": "..."}` → `{{user_prompt}}`
- Effect outputs: `prime.<effect_name>.value` (top-level), `prime.<dynamic_name>.<effect>.value` (nested inside a dynamic)
- Typed prompt outputs: `text` (default), `json`, `boolean`, `number`. Use `prompt_type: json` with `schema:` block for validated structured output
- `dynamic` with `flow: chain` — sequential; each effect can reference prior outputs via `{{prime.name.value}}`
- `loop` with `each.in: prime.<name>.value` — iterates over a JSON array output from a prior `prompt_type: json` effect; loop variable accessed as `{{as_variable_name}}`; outputs stored at `prime.<loop_name>.iter_N.<body_effect>.value`
- `if` with `mode: cel` — deterministic CEL expression; **using the same effect name in both `then` and `else` branches produces a consistent downstream state path**: `prime.<if_name>.<shared_name>.value`
- CEL expressions use `state.prime.<name>.value` (full prefix required — not just `prime.*`)
- Mustache templates use `{{variable}}` — works for both initial state keys and state paths
- YAML `#` comments used extensively for usage docs in example files

### Files to Reference

| File | Purpose |
| ---- | ------- |
| `examples/multi_primitive_story.yml` | Primary reference: dynamic+loop+if+prompt composition |
| `examples/typed_prompt_example.yml` | `prompt_type: json` with `schema:` block |
| `examples/loop_example.yml` | `loop` with `each` — canonical loop syntax |
| `examples/conditional_example.yml` | `if` with `mode: cel` — canonical branch syntax |
| `examples/dynamic_hello.yml` | Simplest `dynamic/chain` baseline |
| `examples/README.md` | Example index — add new row to table |
| `src/circuitry/cli/app.py` | CLI surface — `--state` flag at line 52-54 |
| `src/circuitry/cli/runtime_shim.py` | State loading from file at lines 51-65 |

### Technical Decisions

- **No reflector**: This orchestration is the "approachable alternative" to reflector — explicit chain-of-thought reasoning via prompt primitives only.
- **Output to state only**: Consistent with all examples. User retrieves generated YAML from `prime.generate.final_yaml.value` in `out.json`.
- **`--state` flag = file path only**: No inline JSON. User must create a JSON file. Input convention: `{"user_prompt": "Build an orchestration that..."}`. Run: `./scripts/circuitry run --state input.json examples/meta_orchestration_generator.yml`
- **Model-agnostic, recommended minimum noted in comments**: `phi3:mini` set as default but will produce inconsistent YAML for multi-step generation. Recommend 7B+ instruction-tuned (llama3, mistral-7b-instruct), or hosted (claude-haiku, gpt-4o-mini) for reliable output. Document this prominently in the YAML header.
- **Orchestration shape — 5-step chain inside a top-level `dynamic`:**
  1. `analyze_intent` — `prompt_type: json` + `schema`, extracts `goal`, `steps[]`, `requires_loop`, `requires_branching`, `input_schema`, `output_path`
  2. `elaborate_steps` — `loop(each)` over `prime.generate.analyze_intent.value.steps`; each iteration produces a `yaml_stub` prompt (stubs stored at `prime.generate.elaborate_steps.iter_N.yaml_stub.value`)
  3. `branching_guidance` — `if(cel)` on `state.prime.generate.analyze_intent.value.requires_branching == true`; both branches name their prompt `guidance` → consistent downstream path `prime.generate.branching_guidance.guidance.value`
  4. `draft_yaml` — `prompt(text)` assembles complete YAML using `analyze_intent` JSON fields + `branching_guidance.guidance` as context
  5. `final_yaml` — `prompt(text)` self-review pass that checks for common errors and corrects them; **this is the value the user retrieves**
- **Loop elaborations not directly referenced by `draft_yaml`**: Loop is a demonstrative primitive showing circuitry's iteration capability; `draft_yaml` uses the structured `analyze_intent` JSON for assembly context. Loop outputs remain accessible in state at `iter_N.yaml_stub.value` for user inspection.
- **`if` branch naming convention**: Both `then` and `else` branches use effect name `guidance` → `prime.generate.branching_guidance.guidance.value` is always populated regardless of which branch ran.

## Implementation Plan

### Tasks

- [x] Task 1: Create `examples/meta_orchestration_generator.yml`
  - File: `examples/meta_orchestration_generator.yml` (new file)
  - Action: Write the complete orchestration YAML with the following structure:
    - Header comment block: description, usage instructions (create input.json → run with --state → extract from state), input schema, all notable state output paths, model recommendation
    - Top-level: `adapter: ollama`, `model: phi3:mini`
    - Single top-level `dynamic` named `generate` with `flow: chain` containing 5 effects in order:
      1. `prompt` named `analyze_intent`: `prompt_type: json`, `schema` block (object with properties: `goal` string, `steps` array of strings, `requires_loop` bool, `requires_branching` bool, `suggested_primitives` string array, `input_schema` string, `output_path` string; all required; `additionalProperties: false`), template instructs model to analyze `{{user_prompt}}` and return structured JSON
      2. `loop` named `elaborate_steps`: `each.in: prime.generate.analyze_intent.value.steps`, `each.as: step`, body contains single `prompt` named `yaml_stub` whose template asks the model to write a YAML effect block for `{{step}}` given `{{prime.generate.analyze_intent.value.goal}}`
      3. `if` named `branching_guidance`: `mode: cel`, `expr: "state.prime.generate.analyze_intent.value.requires_branching == true"`, `then` branch has `prompt` named `guidance` producing a note about including `if` effects, `else` branch has `prompt` named `guidance` producing a note about sequential chain — **both branches must use the exact name `guidance`**
      4. `prompt` named `draft_yaml`: template references `{{user_prompt}}`, `{{prime.generate.analyze_intent.value.goal}}`, `{{prime.generate.analyze_intent.value.steps}}`, `{{prime.generate.analyze_intent.value.requires_loop}}`, `{{prime.generate.analyze_intent.value.requires_branching}}`, `{{prime.generate.analyze_intent.value.input_schema}}`, `{{prime.generate.analyze_intent.value.output_path}}`, `{{prime.generate.branching_guidance.guidance.value}}`; includes a full Circuitry YAML rules reference in the prompt; instructs model to return ONLY valid YAML with no markdown fences
      5. `prompt` named `final_yaml`: template references `{{user_prompt}}` and `{{prime.generate.draft_yaml.value}}`; instructs model to check for 7 specific common errors (missing adapter/model, wrong state path format, loop `in` not pointing to json array, CEL missing `state.` prefix, undefined Mustache vars, missing required fields, wrong nested scope references); return corrected or unchanged YAML only
    - Each section separated by a `#` comment explaining its purpose
  - Notes: Model `phi3:mini` is the YAML default for consistency with examples, but the header comment must clearly state recommended minimums. Use `|` (literal block scalar) for all multi-line templates.

- [x] Task 2: Add entry to `examples/README.md`
  - File: `examples/README.md`
  - Action: Add a new row to the Example Index table for `meta_orchestration_generator.yml` with:
    - File: `meta_orchestration_generator.yml`
    - Intent: "Generate a new orchestration YAML from a natural language prompt"
    - Primitives: `dynamic(chain), prompt(json), loop(each), if(cel), prompt`
    - Prerequisites: `--state input.json required (see file header for format)`
    - Difficulty: `advanced`
    - Expected State Highlights: `` `prime.generate.final_yaml.value` (generated YAML), `prime.generate.analyze_intent.value` (intent analysis), `prime.generate.elaborate_steps.iter_N.yaml_stub.value` (per-step stubs) ``
  - Notes: Table row must follow the exact existing column order. Do not modify any other content in README.md.

### Acceptance Criteria

- [ ] AC 1: Given `examples/meta_orchestration_generator.yml` exists and `input.json` contains `{"user_prompt": "Build an orchestration that summarizes a list of articles"}`, when `./scripts/circuitry run --dry-run --state input.json examples/meta_orchestration_generator.yml` is run, then the command exits with code 0 and produces no errors.

- [ ] AC 2: Given the orchestration is validated with `./scripts/circuitry inspect examples/meta_orchestration_generator.yml`, when inspect runs, then it reports a valid orchestration with no schema or compilation errors.

- [ ] AC 3: Given a dry-run completes successfully, when the output state is inspected, then `prime.generate.analyze_intent` exists in state and its `value` contains the keys `goal`, `steps`, `requires_loop`, `requires_branching`.

- [ ] AC 4: Given `analyze_intent` returns `requires_branching: false`, when the orchestration runs, then `prime.generate.branching_guidance.guidance.value` is populated with a note referencing sequential chain (else branch ran).

- [ ] AC 5: Given `analyze_intent` returns `requires_branching: true`, when the orchestration runs, then `prime.generate.branching_guidance.guidance.value` is populated with a note referencing `if` effects (then branch ran).

- [ ] AC 6: Given a live run with a 7B+ instruction-tuned model or hosted model, when the orchestration completes, then `prime.generate.final_yaml.value` contains a non-empty string that begins with `adapter:` and is parseable as YAML.

- [ ] AC 7: Given the smoke test suite runs (`pytest -q tests/examples/test_examples_smoke.py`), when `meta_orchestration_generator.yml` is included in the examples directory, then all smoke tests pass.

- [ ] AC 8: Given `examples/README.md` is updated, when the index table is read, then `meta_orchestration_generator.yml` appears as a row with correct primitive list, difficulty level of `advanced`, and state paths documented.

## Additional Context

### Dependencies

- No new Python dependencies required.
- Runtime: any configured adapter (ollama, openai, anthropic, litellm) — file uses `ollama` as default for consistency with existing examples.
- For live validation: a 7B+ instruction-tuned model (e.g. `ollama pull llama3` or a hosted API key configured).

### Testing Strategy

- **Dry-run validation** (no model required):
  ```bash
  echo '{"user_prompt": "Build an orchestration that summarizes a list of articles"}' > /tmp/meta-input.json
  ./scripts/circuitry run --dry-run --state /tmp/meta-input.json examples/meta_orchestration_generator.yml
  ```
- **Schema inspection** (no model required):
  ```bash
  ./scripts/circuitry inspect examples/meta_orchestration_generator.yml
  ```
- **Smoke test suite**:
  ```bash
  pytest -q tests/examples/test_examples_smoke.py
  ```
- **Live run** (requires 7B+ model):
  ```bash
  echo '{"user_prompt": "Build an orchestration that summarizes a list of articles"}' > /tmp/meta-input.json
  ./scripts/circuitry run --state /tmp/meta-input.json examples/meta_orchestration_generator.yml
  cat out.json | python3 -c "import sys,json; print(json.load(sys.stdin)['prime']['generate']['final_yaml']['value'])"
  ```
- **Extract generated YAML** and save as a new file to manually verify it is syntactically valid YAML.

### Notes

- **Pre-mortem risk — model output quality**: The biggest risk is the model producing non-YAML text or malformed YAML despite the prompt instructions. The self-review step (Step 5) mitigates but does not eliminate this. Mitigation in the spec: header prominently recommends 7B+ models; prompt templates include explicit "return ONLY the YAML, no markdown fences" instructions.
- **Pre-mortem risk — CEL path correctness**: Nested state paths inside a `dynamic` use `state.prime.<dynamic_name>.<effect>.value` in CEL expressions. This is easy to get wrong. The `branching_guidance` if expression must use the full path `state.prime.generate.analyze_intent.value.requires_branching`.
- **Pre-mortem risk — loop `in` path**: The `elaborate_steps` loop's `each.in` must point to the `steps` array field inside the JSON object output, not the raw `value`. Correct: `prime.generate.analyze_intent.value.steps`. If `analyze_intent` returns a top-level array instead of an object containing `steps`, this breaks. The schema enforces the object shape.
- **V2 scope**: Accept an existing `out.json` state file as seed input so users can pipe from a previous orchestration run into the generator.
- **`--state-file` flag does not exist**: The CLI flag is `--state` (or `-s`) and takes a file path. The in-scope item in Overview has been updated accordingly.
- **manifest.json**: After creating the example, bump `examples/manifest.json` if required by the maintenance rule in `examples/README.md`.

## Review Notes

- Adversarial review completed
- Findings: 12 total, 11 fixed, 1 skipped (noise)
- Resolution approach: auto-fix
- Fixed: F1 (out.json documented), F2 (self-review disclaimer + structured checklist), F3 (array serialization hint in template), F4 (stubs reference note in draft_yaml), F5 (adapter-specific model recommendations), F6 (--- delimiter replaced), F7 (prompt_type: text explicit on guidance prompts), F8 (systematic per-check review prompt), F9 (README Prerequisites model caveat), F10 (suggested_primitives field removed), F11 (minItems/maxItems on steps schema)
- Skipped: F12 (loop variable scoping — confirmed working pattern in existing examples)
