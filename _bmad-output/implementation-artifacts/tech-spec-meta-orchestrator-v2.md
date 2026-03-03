---
title: 'Meta Orchestrator v2 — Atomic Jobs, Tool Awareness, and Rules Alignment'
slug: 'meta-orchestrator-v2'
created: '2026-03-02'
status: 'ready-for-dev'
stepsCompleted: [1, 2, 3, 4]
tech_stack:
  - Circuitry DSL (YAML)
  - Python
  - Chevron/Mustache
  - JSON Schema draft-07
files_to_modify:
  - orchestrations/meta_orchestrator.yml
  - docs/orchestration-reference.md
  - scripts/improve-orchestration
  - scripts/run-orchestration-ideas
files_to_create:
  - docs/plugins/ffmpeg.md
  - docs/plugins/comfyui.md
code_patterns:
  - 'dynamic(chain) for sequential multi-step chains'
  - 'loop(each) + collect for parallel atomic generation'
  - 'if(cel) for state-based branching'
  - 'tool effect with provider for non-LLM side-effects'
  - 'Mustache interpolation: {{key}} for initial state, {{prime.name.value}} for effect outputs'
  - 'comic_strip.yml pattern: prompt(json) for planning → tool for execution → if(cel) for branching on state'
  - 'Each LLM call produces exactly one atomic primitive; results merged via state interpolation'
test_patterns:
  - 'circuitry validate orchestrations/meta_orchestrator.yml'
  - 'Live run with --state input.json → read output → assess quality → iterate → check in with user'
  - 'scripts/run-orchestration-ideas for batch testing generated orchestrations'
---

# Tech-Spec: Meta Orchestrator v2 — Atomic Jobs, Tool Awareness, and Rules Alignment

**Created:** 2026-03-02

## Overview

### Problem Statement

The meta orchestrator generates orchestrations that don't fully leverage Circuitry's strengths — splitting work into atomic LLM calls, interpolating results via state, and making decisions based on state. It also has no awareness of the `tool` effect type or available plugins (`ffmpeg`, `comfyui`). The LLM Authoring Rules injected as `{{rules}}` are out of date with the current system.

### Solution

Restructure the meta orchestrator itself into smaller atomic steps (practicing what it preaches), update it to generate tool-aware orchestrations that follow the atomic-jobs philosophy, and bring the LLM Authoring Rules in `docs/orchestration-reference.md` into alignment with the current system.

### Scope

**In Scope:**
- Restructure `meta_orchestrator.yml` into more granular atomic steps
- Dynamic plugin discovery: plugin description files in `docs/plugins/`, auto-loaded and injected as `{{plugins}}`
- Update the `## LLM Authoring Rules` section in `docs/orchestration-reference.md`
- Ensure generated orchestrations lean into small atomic jobs, state interpolation, and state-based branching
- Fix `_extract_yaml` in scripts to also accept `effects:` as start marker

**Out of Scope:**
- Adding new plugins beyond the existing two (ffmpeg, comfyui)
- Changes to the runtime engine or compiler
- Changes to the tool effect plugin architecture itself

## Context for Development

### Codebase Patterns

- The `{{rules}}` variable is injected by `_load_rules()` in `scripts/improve-orchestration` and `scripts/run-orchestration-ideas` — it extracts everything from `## LLM Authoring Rules` to EOF in `docs/orchestration-reference.md`
- The meta orchestrator uses `dynamic(chain)` as a top-level container with 5 sequential steps inside
- `loop` + `collect` + `flow: tree` pattern runs parallel atomic generation (one effect block per LLM call)
- `tool` effects use `type: tool, provider: <name>` with params dict; available plugins: `ffmpeg`, `comfyui`
- Plugin details: ffmpeg requires `params.input` + `params.output`; comfyui requires top-level `prompt` + `model` with optional `params` for sampler settings
- All orchestration YAML files omit `model:` and `adapter:` — inherited from `config.json`

### Files to Reference

| File | Purpose |
| ---- | ------- |
| `orchestrations/meta_orchestrator.yml` | Current meta orchestrator — primary file to restructure |
| `docs/orchestration-reference.md` | Canonical reference; contains `## LLM Authoring Rules` (line 559+) |
| `src/circuitry/plugins/ffmpeg.py` | ffmpeg plugin implementation — params reference for docs/plugins/ffmpeg.md |
| `src/circuitry/plugins/comfyui.py` | ComfyUI plugin implementation — params reference for docs/plugins/comfyui.md |
| `src/circuitry/plugins/base.py` | ToolPlugin Protocol + ToolResult |
| `src/circuitry/schema/orchestration.schema.json` | JSON Schema for validation |
| `orchestrations/comic_strip.yml` | Reference for tool effect usage in a real orchestration |
| `scripts/improve-orchestration` | `_load_rules()` extraction logic; `_extract_yaml()` start marker |
| `scripts/run-orchestration-ideas` | `_load_rules()` extraction logic; `_extract_yaml()` start marker |

### Technical Decisions

1. **Meta orchestrator restructure — 8 atomic steps inside `dynamic(chain)`:**
   - Current: 5 steps (analyze_intent → plan_details → draft_effects → assemble_yaml → final_yaml)
   - New: 8 steps, each producing exactly one focused output:
     1. `classify_request` (json) — goal, complexity, requires_tools flag, input_schema, output_path
     2. `select_plugins` (json) — given `{{plugins}}`, which plugins are relevant and why? Always runs (returns empty array if no tools needed)
     3. `plan_steps` (json) — break goal into 2-8 atomic steps, each tagged with effect type; references selected plugins
     4. `plan_state_and_schemas` (tree/parallel) — two sub-steps in parallel:
        - `plan_state_flow` — map reads/writes for each step
        - `plan_schemas` — which steps need prompt_type:json + draft schemas
     5. `draft_effects` (loop+collect+tree) — one YAML effect block per step (unchanged pattern)
     6. `assemble_yaml` — mechanical assembly of collected blocks into complete file
     7. `review_structure` — structural check against `{{rules}}`
     8. `review_semantics` — semantic check: does it solve the problem atomically?

2. **Dynamic plugin discovery — not hardcoded:**
   - Plugin description files in `docs/plugins/<name>.md`
   - Each file: title, description, when to use, required params, optional params, example YAML block
   - Runner scripts add `_load_plugins()` that scans `docs/plugins/*.md`, concatenates all files with `---` separators
   - Injected as `"plugins"` key in state alongside `"rules"`
   - Meta orchestrator templates reference `{{plugins}}` — zero hardcoded plugin names

3. **LLM Authoring Rules updates:**
   - Add atomic-jobs design philosophy section
   - Add design patterns section: prompt+tool combos, state-based branching, loop+collect aggregation
   - Tool effect rules already present (rule 11) — no plugin-specific additions (those belong in plugin docs)
   - Keep compact for context-window-limited models

4. **`_extract_yaml` fix:**
   - Both scripts' `_extract_yaml()` functions currently only look for `adapter:` as start marker
   - Update to also accept `effects:` — tool-only orchestrations have no adapter field
   - Simple: check for either `adapter:` or `effects:` as start line

5. **Testing workflow — iterative quality loop:**
   - Run meta orchestrator with a test prompt → extract YAML → validate → read output → assess quality → iterate
   - Check in with user after each iteration with findings
   - Use `scripts/run-orchestration-ideas` for batch testing across multiple prompts

## Implementation Plan

### Tasks

- [ ] Task 1: Create `docs/plugins/ffmpeg.md`
  - File: `docs/plugins/ffmpeg.md` (new)
  - Action: Write the ffmpeg plugin description file. Include:
    - **Name:** ffmpeg
    - **Description:** Runs local ffmpeg commands for audio/video/image processing. Subprocess-based (no shell layer).
    - **When to use:** Transcoding, format conversion, image compositing, adding text overlays, combining media files, extracting frames, applying filters.
    - **Required params:** `params.input` (string — input file path), `params.output` (string — output file path)
    - **Optional params:** `params.flags` (string — raw ffmpeg flags), `params.extra_inputs` (array of strings — additional `-i` inputs), `params.filter_complex` (string — filter_complex expression for multi-input operations), `params.vf_drawtext` (object — drawtext filter config: `text`, `fontfile`, `x`, `y`, `fontsize`, `fontcolor`, `box`, `boxcolor`, `boxborderw`), `params.map` (string — output stream mapping)
    - **Safety:** Shell metacharacters (`&&`, `||`, `;`, `>`, `<`, `` ` ``, `$(`) are rejected. `-y` (overwrite) is always injected.
    - **Example YAML block:** A simple transcode and a drawtext overlay example
  - Notes: Content derived from `src/circuitry/plugins/ffmpeg.py` implementation. Keep concise — this is injected into LLM context.

- [ ] Task 2: Create `docs/plugins/comfyui.md`
  - File: `docs/plugins/comfyui.md` (new)
  - Action: Write the ComfyUI plugin description file. Include:
    - **Name:** comfyui
    - **Description:** Generates images via ComfyUI REST API. Supports txt2img and img2img workflows.
    - **When to use:** Image generation, illustration, visual content creation, style transfer, image-to-image transformation.
    - **Required fields:** `prompt` (top-level string — image generation prompt, Mustache-rendered), `model` (top-level string — checkpoint filename e.g. `flux1-dev-fp8.safetensors`)
    - **Optional params:** `params.image_output` (path|base64|url, default path), `params.image_dir` (output directory), `params.width`/`params.height` (integers), `params.steps` (int, default 20), `params.cfg` (float, default 7.0), `params.sampler_name` (string, default euler), `params.scheduler` (string, default normal), `params.seed` (int, auto if absent), `params.negative_prompt` (string), `params.reference_image` (string path — triggers img2img workflow), `params.denoise` (float 0-1, used with reference_image)
    - **Example YAML block:** A txt2img example and an img2img example with reference_image
  - Notes: Content derived from `src/circuitry/plugins/comfyui.py` implementation.

- [ ] Task 3: Add `_load_plugins()` to `scripts/improve-orchestration`
  - File: `scripts/improve-orchestration`
  - Action:
    1. Add `_load_plugins()` function after `_load_rules()` (around line 86):
       ```python
       def _load_plugins(project_root: Path) -> str:
           """Load all plugin description files from docs/plugins/."""
           plugins_dir = project_root / "docs" / "plugins"
           if not plugins_dir.is_dir():
               return ""
           parts = []
           for md_file in sorted(plugins_dir.glob("*.md")):
               parts.append(md_file.read_text(encoding="utf-8").strip())
           return "\n\n---\n\n".join(parts)
       ```
    2. Add `PLUGINS = _load_plugins(PROJECT_ROOT)` after `RULES = _load_rules(PROJECT_ROOT)`.
    3. In `main()`, update the `run_orchestration()` call's `state=` dict to include `"plugins": PLUGINS` alongside `"rules": RULES`.
  - Notes: The `improve-orchestration` script runs `orchestration_improver.yml`, not `meta_orchestrator.yml` directly. But the improver also benefits from plugin context when improving tool-using orchestrations.

- [ ] Task 4: Add `_load_plugins()` to `scripts/run-orchestration-ideas`
  - File: `scripts/run-orchestration-ideas`
  - Action:
    1. Add the same `_load_plugins()` function after `_load_rules()` (around line 62).
    2. Add `PLUGINS = _load_plugins(PROJECT_ROOT)` after `RULES`.
    3. In `_generate_orchestration()`, update the `run_orchestration()` call's `state=` dict to include `"plugins": PLUGINS` alongside `"rules": RULES` and `"user_request"`.
  - Notes: This is the primary script that invokes `meta_orchestrator.yml`.

- [ ] Task 5: Fix `_extract_yaml()` in both scripts
  - Files: `scripts/improve-orchestration` (line 97-116), `scripts/run-orchestration-ideas` (line 77-95)
  - Action: Update the `_extract_yaml()` function to also accept `effects:` as a valid YAML start marker. Currently only checks for `adapter:`. Change the loop:
    ```python
    for i, line in enumerate(lines):
        if line.startswith("adapter:") or line.startswith("effects:"):
            return "\n".join(lines[i:]).strip()
    ```
  - Notes: Tool-only orchestrations start with `effects:` (no adapter/model needed). Without this fix, generated tool-only orchestrations would be silently discarded.

- [ ] Task 6: Update `## LLM Authoring Rules` in `docs/orchestration-reference.md`
  - File: `docs/orchestration-reference.md` (line 559+)
  - Action: Expand the LLM Authoring Rules section with:
    1. **Atomic design philosophy (new section, after rule 17):**
       - Prefer many small effects over few large ones — each LLM call should do one focused thing
       - Use `prompt_type: json` to produce structured data that downstream effects consume via state interpolation
       - Use `dynamic(chain)` to sequence dependent atomic steps; `dynamic(tree)` for independent parallel work
       - Use `loop(each) + collect` to process items individually and aggregate results
       - Use `if(cel)` to make decisions based on state values from prior effects
    2. **Design patterns (new section):**
       - **Prompt → Tool pipeline:** Use a prompt to generate parameters (e.g. ffmpeg flags, image prompts), then a tool effect to execute with those parameters. The prompt's output feeds the tool's params via Mustache interpolation.
       - **Staged decomposition:** Break complex generation into: plan (json) → per-item generation (loop+collect) → assembly (prompt) → review (prompt). Each stage is small and focused.
       - **State-based branching:** Use `if(cel)` with `state.prime.<name>.value.<field>` to branch on structured output from prior effects. Same effect name in both branches for consistent downstream paths.
    3. Keep existing rules 1-17 intact. Number new content as rules 18+.
  - Notes: These additions should be concise — they're injected into every LLM call via `{{rules}}`. Total section should stay under ~100 lines.

- [ ] Task 7: Restructure `orchestrations/meta_orchestrator.yml`
  - File: `orchestrations/meta_orchestrator.yml`
  - Action: Complete rewrite of the orchestration. Preserve the top-level `dynamic(chain)` named `generate`. Replace the 5 internal steps with 8 atomic steps:

    **Step 1: `classify_request`** (prompt, json)
    - Input: `{{user_request}}`, `{{plugins}}`
    - Output schema: `{goal, complexity, requires_tools, input_schema, output_path}`
    - Template: Analyze request, determine if tools from `{{plugins}}` are relevant
    - Atomic focus: classification only, no step planning

    **Step 2: `select_plugins`** (prompt, json)
    - Input: `{{user_request}}`, `{{plugins}}`, classification from step 1
    - Output schema: `{relevant_plugins: [{name, reason, use_case}], tool_guidance}`
    - Template: Given available plugins and the request, which plugins apply and how?
    - Returns empty `relevant_plugins: []` if no tools needed

    **Step 3: `plan_steps`** (prompt, json)
    - Input: classification, selected plugins, `{{user_request}}`
    - Output schema: `{steps: [{name, type, purpose, needs_tool, plugin}]}`
    - Template: Break the goal into 2-8 atomic steps. Each step is one effect. Tag with effect type. Reference selected plugins for tool steps.
    - Atomic focus: step planning only, no YAML generation

    **Step 4: `plan_details`** (dynamic, tree — parallel)
    - Two parallel sub-steps:
      - **`plan_state_flow`** (prompt, text): Map data dependencies. For each step: what it reads, what it writes. Verify no effect reads from a later step.
      - **`plan_schemas`** (prompt, text): Identify which steps need `prompt_type: json` and draft minimal schemas.

    **Step 5: `draft_effects`** (loop, each+collect+tree)
    - Iterates over `plan_steps` output array
    - Each iteration: one prompt producing one YAML effect block
    - Template includes: `{{rules}}`, `{{plugins}}`, state flow map, schema plan, selected plugins
    - Unchanged pattern from current — already atomic

    **Step 6: `assemble_yaml`** (prompt, text)
    - Mechanical: wraps collected blocks in `effects:` header with comments
    - Template instructs: do NOT rewrite blocks, only add header + indentation + section comments
    - Atomic focus: assembly only

    **Step 7: `review_structure`** (prompt, text)
    - Structural check against `{{rules}}`: naming, state paths, required fields, effect types
    - Silently fix violations; output corrected YAML
    - Atomic focus: structural correctness only

    **Step 8: `review_semantics`** (prompt, text)
    - Semantic check: does the orchestration actually solve `{{user_request}}`? Are steps atomic enough? Is state interpolation used correctly? Are tools used when the plugins would help?
    - Silently fix; output final YAML
    - PRIMARY OUTPUT: `prime.generate.review_semantics.value`

  - Notes:
    - Update header comment block with new step descriptions, state paths, and input keys (`user_request`, `rules`, `plugins`)
    - Primary output path changes from `prime.generate.final_yaml.value` to `prime.generate.review_semantics.value`
    - All templates use `|` (literal block scalar) for multi-line
    - No `adapter:` or `model:` at top level — inherited from config
    - Each template should include explicit "Return ONLY..." instructions to keep model output clean

- [ ] Task 8: Update scripts for new primary output path
  - Files: `scripts/run-orchestration-ideas` (line 166-172)
  - Action: Update the state path used to extract the generated YAML. Change from:
    ```python
    .get("final_yaml", {})
    ```
    to:
    ```python
    .get("review_semantics", {})
    ```
  - Notes: The `improve-orchestration` script reads from `orchestration_improver.yml`, not `meta_orchestrator.yml` directly, so it doesn't need this change. Only `run-orchestration-ideas` calls `meta_orchestrator.yml` and extracts the output.

- [ ] Task 9: Iterative testing and quality assessment
  - Action: Run the updated meta orchestrator against test prompts and iterate:
    1. Validate: `circuitry validate orchestrations/meta_orchestrator.yml`
    2. Run with test prompt: `circuitry run orchestrations/meta_orchestrator.yml --state /tmp/meta-input.json --out /tmp/meta-out.json`
    3. Extract and validate generated YAML
    4. Assess quality: atomicity, tool usage, state interpolation, correctness
    5. Report findings to user
    6. Iterate on prompts based on findings
    7. Repeat until quality is satisfactory
  - Test prompts to use:
    - Pure prompt: "Build an orchestration that summarizes a list of articles"
    - Tool-using: "Build an orchestration that generates a set of product images with descriptions"
    - Mixed: "Build an orchestration that writes a script, generates images for each scene, and composites them"
  - Notes: Check in with user after each iteration round.

### Acceptance Criteria

- [ ] AC 1: Given `docs/plugins/ffmpeg.md` and `docs/plugins/comfyui.md` exist, when `scripts/run-orchestration-ideas` runs, then the `plugins` state key contains the concatenated contents of both files.

- [ ] AC 2: Given a user request that involves image generation (e.g., "generate product photos"), when the meta orchestrator runs, then `classify_request` sets `requires_tools: true` and `select_plugins` identifies `comfyui` as relevant — without any hardcoded plugin names in the orchestration YAML.

- [ ] AC 3: Given a user request that is purely text-based (e.g., "summarize articles"), when the meta orchestrator runs, then `select_plugins` returns `relevant_plugins: []` and the generated orchestration contains no `type: tool` effects.

- [ ] AC 4: Given the meta orchestrator completes, when the generated YAML is extracted from `prime.generate.review_semantics.value`, then `circuitry validate` accepts it as structurally valid.

- [ ] AC 5: Given the generated YAML from a tool-using request, when inspected, then it contains `type: tool` effects with correct `provider` and `params` fields matching the plugin description files.

- [ ] AC 6: Given the generated YAML from any request, when inspected, then each LLM call (prompt effect) does exactly one focused thing — no single prompt tries to analyze, plan, generate, and review simultaneously.

- [ ] AC 7: Given `_extract_yaml()` receives YAML starting with `effects:` (no `adapter:` line), when called, then it returns the YAML correctly rather than returning empty string.

- [ ] AC 8: Given the `## LLM Authoring Rules` section is read, when inspected, then it includes atomic design philosophy guidance and design patterns for prompt+tool pipelines.

- [ ] AC 9: Given a new plugin description file is added to `docs/plugins/` (e.g., `docs/plugins/hypothetical.md`), when the meta orchestrator runs, then `{{plugins}}` automatically includes the new plugin's description without any code changes.

## Additional Context

### Dependencies

No new package dependencies.

- `pathlib.Path.glob("*.md")` for plugin discovery (stdlib)
- Existing `circuitry validate` for generated YAML validation
- Test prompts require a running LLM adapter (ollama with configured model)

### Testing Strategy

**Iterative quality loop (manual, with user check-ins):**

1. Validate meta orchestrator structure:
   ```bash
   source .venv/bin/activate
   circuitry validate orchestrations/meta_orchestrator.yml
   ```

2. Run with test prompt:
   ```bash
   echo '{"user_request": "Build an orchestration that summarizes a list of articles"}' > /tmp/meta-input.json
   circuitry run orchestrations/meta_orchestrator.yml --state /tmp/meta-input.json --out /tmp/meta-out.json
   ```

3. Extract and validate generated YAML:
   ```bash
   python3 -c "import sys,json; print(json.load(sys.stdin)['prime']['generate']['review_semantics']['value'])" < /tmp/meta-out.json > /tmp/generated.yml
   circuitry validate /tmp/generated.yml
   ```

4. Assess quality dimensions:
   - Does each effect do exactly one thing?
   - Are state paths correct (prime.<name>.value)?
   - Are tools used when appropriate?
   - Is the orchestration self-contained and runnable?

5. Report findings to user, iterate, repeat.

**Test prompts (escalating complexity):**

| Prompt | Expected behavior |
|--------|-------------------|
| "Summarize a list of articles" | Pure prompt orchestration, loop+collect, no tools |
| "Generate product images with descriptions" | Tool-using: comfyui for images, prompt for descriptions |
| "Write a script, generate scene images, composite into video" | Mixed: prompts for writing, comfyui for images, ffmpeg for video |

**Batch testing:**
```bash
# After manual iteration is complete, run against orchestration-ideas.txt
scripts/run-orchestration-ideas --skip-improve
```

### Notes

- **Primary output path change:** `prime.generate.final_yaml.value` → `prime.generate.review_semantics.value`. The `run-orchestration-ideas` script must be updated (Task 8).
- **Plugin description file format:** Keep concise. Each file is concatenated and injected into every LLM call. Aim for ~30-50 lines per plugin. Include one YAML example block per plugin — models learn better from examples than from parameter tables alone.
- **`comic_strip.yml` as reference architecture:** 10 steps mixing prompt (json) for creative planning with tool (comfyui/ffmpeg) for execution, chained via state, if (cel) for branching on dynamic panel count. This is the pattern the meta orchestrator should be able to reproduce.
- **Risk — model quality:** Small models may struggle with 8-step orchestration generation. The atomic-jobs approach mitigates this (each call is simpler), but the draft_effects loop template becomes the critical prompt. Quality depends heavily on the rules and plugin descriptions being clear and concise.
- **Risk — `{{plugins}}` context size:** If many plugins are added, the concatenated descriptions could consume significant context. Current two plugins should be ~80 lines total. Monitor this as plugins grow.
- **Current gaps in `analyze_intent`:** Lists "Available effect types: prompt, dynamic, loop, if" — missing `tool` and `reflector`. The new `classify_request` step must list all 6 effect types.
