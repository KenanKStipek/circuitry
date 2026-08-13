# Circuitry Orchestration Reference

This document is the canonical reference for writing Circuitry orchestration YAML files. It covers every effect type, all fields, state path addressing rules, patterns, and antipatterns. The `## LLM Authoring Rules` section at the end is a self-contained rule set injected into LLM prompts by the toolchain.

The machine-readable counterpart is `src/circuitry/schema/orchestration.schema.json`, which is used by `circuitry validate` to enforce structural correctness.

---

## Overview

Circuitry separates two worlds:

- **DOL (Domain Object Language)** — the declarative YAML plan. Defines topology, control flow, and model invocations. Does not execute anything directly.
- **Runtime** — deterministic, append-only execution. Reads the DOL, executes it exactly, and records results into hierarchical state.

This means an orchestration YAML is a pure data structure. The runtime is the engine.

---

## File Structure

Top-level fields of an orchestration YAML file:

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `effects` | array | **yes** | — | Ordered list of top-level effects to execute |
| `adapter` | string | no | from config.json | Adapter: e.g. `ollama`, `openai`, `anthropic`, `litellm`, `cyberdiner` (job queue; `model:` is a tier), `host_claude` (MCP-only — see below). Full compiled-in list: `cof list --extensions` |
| `model` | string | no | from config.json | Model identifier (e.g. `llama3`, `gpt-4o`, `claude-haiku-20240307`) |
| `flow` | string | no | `chain` | Top-level flow for the implicit root dynamic |
| `version` | number | no | — | Optional compatibility version |

Additional top-level keys (e.g. `runtime`, `plugins`) are allowed and used by the runtime configuration layer.

**Minimal valid file:**
```yaml
adapter: ollama
model: llama3

effects:
  - type: prompt
    name: greet
    template: "Say hello!"
```

---

## Effect Types

### `prompt`

The atomic execution unit. Performs exactly one model invocation and writes a typed result to state.

**State output path:** `prime.<name>.value`

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `type` | `"prompt"` | yes | — | |
| `name` | string | yes | — | Pattern `^[A-Za-z_][A-Za-z0-9_]*$`; `iter_<N>` reserved |
| `template` | string | one-of | — | Mustache template; mutually exclusive with `messages` |
| `messages` | array | one-of | — | Role-based messages; mutually exclusive with `template` |
| `prompt_type` | string | no | `text` | `text`, `json`, `boolean`, `number`, `array`, `object`, `tool` |
| `schema` | object | no | — | JSON Schema for validating structured output |
| `description` | string | no | — | Human-readable description |
| `model` | string | no | — | Per-effect model override |
| `provider` | string | no | — | Per-effect provider override |
| `provider_fallbacks` | array | no | — | Ordered fallback providers |
| `params` | object | no | — | Provider-specific generation params (e.g. temperature) |
| `timeout_ms` | integer | no | — | Per-effect timeout in milliseconds |
| `deterministic` | boolean | no | `false` | Use temperature=0 or equivalent |
| `inputs` | object | no | — | Prompt-local key/value pairs for template rendering |
| `assets` | array | no | — | Non-text inputs: `[{kind: "image", ref: "path/to/img"}]` |
| `retries` | object | no | — | `{max_attempts: N, backoff_ms: M}` |
| `on_error` | string | no | `fail` | `fail`, `skip`, `continue` |

**Example — text output:**
```yaml
- type: prompt
  name: summarize
  template: "Summarize this article in one sentence: {{article_text}}"
```

**Example — JSON output with schema:**
```yaml
- type: prompt
  name: extract_items
  prompt_type: json
  schema:
    type: array
    items:
      type: string
  template: "Extract a JSON array of key items from: {{text}}"
```

**Example — role-based messages:**
```yaml
- type: prompt
  name: classify
  prompt_type: boolean
  messages:
    - role: system
      content: "You are a classifier. Reply with only true or false."
    - role: user
      content: "Is this text positive? {{text}}"
```

---

### `dynamic`

A named container that executes child effects sequentially (`chain`) or in parallel (`tree`). Groups related effects and records aggregated metadata.

**State output path:** `prime.<name>.<child_name>.value`

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `type` | `"dynamic"` | yes | — | |
| `name` | string | yes | — | Name pattern; must be unique among siblings |
| `flow` | string | no | `chain` | `chain`, `chain_of_thought`, `cot`, `tree`, `tree_of_thought`, `tot` |
| `effects` | array | yes | — | Non-empty list of child effects |
| `description` | string | no | — | |
| `max_concurrency` | integer | no | — | Max parallel executions for tree flow |
| `stop_on_error` | boolean | no | `false` | Stop all parallel effects on first error |
| `on_error` | string | no | `fail` | `fail`, `skip`, `continue` |
| `labels` | object | no | — | Arbitrary metadata annotations |

**Flow semantics:**
- `chain` — sequential: each effect executes after the previous, and sees all prior outputs in state
- `tree` — parallel: all effects execute concurrently against the same input snapshot; none see each other's outputs

**Example — chain (sequential pipeline):**
```yaml
- type: dynamic
  name: pipeline
  flow: chain
  effects:
    - type: prompt
      name: outline
      template: "Outline an essay on: {{topic}}"
    - type: prompt
      name: draft
      template: "Write the essay based on this outline:\n{{prime.pipeline.outline.value}}"
```

**Example — tree (parallel analysis):**
```yaml
- type: dynamic
  name: analysis
  flow: tree
  effects:
    - type: prompt
      name: summary
      template: "Summarize: {{text}}"
    - type: prompt
      name: sentiment
      prompt_type: boolean
      template: "Is this text positive? {{text}}"
```

---

### `if` / `conditional`

Evaluates a condition against state and executes exactly one branch (`then` or `else`). Non-selected branches produce no effects. Name is optional — named conditionals record decision metadata to state.

**State output path (named):** `prime.<name>.<branch_effect_name>.value`

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `type` | `"if"` or `"conditional"` | yes | — | Both accepted |
| `name` | string | no | — | Optional; enables state recording of the decision |
| `if` | object | yes | — | Condition definition |
| `if.mode` | string | no | `model` | `model` or `cel` |
| `if.template` | string | model only | — | LLM evaluates and returns boolean |
| `if.expr` | string | cel only | — | CEL expression; must use `state.prime.<name>.value` prefix |
| `then` | array | yes | — | Effects when condition is true |
| `else` | array | no | `[]` | Effects when condition is false |
| `threshold` | number | no | `0.5` | Confidence threshold for model mode |
| `on_error` | string | no | `fail` | `fail`, `continue`, `skip` |
| `labels` | object | no | — | |

**CEL mode example:**
```yaml
- type: if
  name: check_role
  if:
    mode: cel
    expr: "state.prime.get_role.value == 'admin'"
  then:
    - type: prompt
      name: response
      template: "Admin dashboard: {{prime.get_role.value}}"
  else:
    - type: prompt
      name: response
      template: "User view: {{prime.get_role.value}}"
```

**Model mode example:**
```yaml
- type: if
  name: quality_gate
  if:
    mode: model
    template: "Is this output high quality? Reply true or false.\n\n{{prime.draft.value}}"
  then:
    - type: prompt
      name: result
      template: "Approved: {{prime.draft.value}}"
  else:
    - type: prompt
      name: result
      template: "Rejected — rewrite: {{prime.draft.value}}"
```

> **Note:** Use the same `name` for inner effects in both `then` and `else` branches so that state paths are deterministic regardless of which branch executes.

---

### `loop`

Repeats a `body` of effects for each element of a collection (`each`) or while a condition holds (`while`). Name is optional.

**State output paths (named each loop):**
- Per-iteration: `prime.<name>.iter_0.<body_effect>.value`, `prime.<name>.iter_1.<body_effect>.value`, ...
- Aggregated (when `collect` is set): `prime.<name>.collected.value` — array of every iteration's collected effect value

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `type` | `"loop"` | yes | — | |
| `name` | string | no | — | Optional; enables wrapper metadata recording and `collect` |
| `collect` | string | no | — | Name of a body effect; aggregates its `.value` across all iterations into `prime.<name>.collected.value`. Requires a named loop. |
| `flow` | `"chain"` \| `"tree"` | no | `chain` | `chain` = sequential (default). `tree` = all `each` iterations run in parallel via `ThreadPoolExecutor`. `while` loops always run sequentially. |
| `max_concurrency` | integer | no | unbounded | Max parallel workers when `flow: tree`. |
| `body` | array | yes | — | Non-empty list of effects to execute per iteration |
| `each` | object | one-of | — | Collection iteration; mutually exclusive with `while` |
| `each.in` | string | yes (each) | — | State path to a JSON array (must be `prompt_type: json` output) |
| `each.as` | string | no | `item` | Variable name for current element in body templates |
| `while` | object | one-of | — | Continuation condition; mutually exclusive with `each` |
| `while.mode` | string | no | `model` | `model` or `cel` |
| `while.template` | string | model only | — | LLM returns boolean for continuation decision |
| `while.expr` | string | cel only | — | CEL expression against state |
| `max_iterations` | integer | no | `100` | Hard cap on iterations |
| `min_iterations` | integer | no | `0` | Minimum iterations before condition is checked |
| `on_error` | string | no | `fail` | `fail`, `break`, `continue` |
| `labels` | object | no | — | |

**Each loop example:**
```yaml
- type: prompt
  name: items
  prompt_type: json
  schema:
    type: array
    items:
      type: string
  template: "List 3 topics about {{subject}} as a JSON array."

- type: loop
  name: explain
  each:
    in: prime.items.value
    as: topic
  body:
    - type: prompt
      name: summary
      template: "Explain {{topic}} in one sentence."
```

**Each loop with `collect` example** — aggregates every `summary` output into one array at `prime.explain.collected.value`:
```yaml
- type: loop
  name: explain
  collect: summary
  each:
    in: prime.items.value
    as: topic
  body:
    - type: prompt
      name: summary
      template: "Explain {{topic}} in one sentence."

# prime.explain.collected.value → ["Explanation of topic 0", "Explanation of topic 1", ...]
```

**Parallel each loop with `flow: tree`** — all iterations run concurrently; results are assembled in original order:
```yaml
- type: loop
  name: draft_effects
  flow: tree          # run all iterations in parallel
  collect: draft_effect
  each:
    in: prime.steps.value
    as: step
  body:
    - type: prompt
      name: draft_effect
      template: "Write a YAML effect block for: {{step}}"

# prime.draft_effects.collected.value → [block_0, block_1, ...]  (order preserved)
```

**While loop example:**
```yaml
- type: loop
  name: refine
  while:
    mode: model
    template: "Does this draft need more improvement? Reply true or false.\n\n{{prime.draft.value}}"
  max_iterations: 5
  body:
    - type: prompt
      name: draft
      template: "Improve this text:\n{{prime.draft.value}}"
```

---

### `reflector`

A planning-time effect. Instead of executing a fixed set of effects, the reflector reads state, generates a Dynamic plan, and executes it. It can run multiple planning cycles (`max_iterations`). Used for adaptive, open-ended tasks where the number of steps is not known ahead of time.

**State output path:** `prime.<name>.plan.*` (runtime-generated keys)

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `type` | `"reflector"` | yes | — | |
| `name` | string | yes | — | Name pattern |
| `effects` | array | yes | — | Inner effects template used as the base dynamic per planning cycle |
| `flow` | string | no | `chain` | Flow for inner dynamic |
| `plan_from_step` | string | no | `propose_steps` | Inner effect whose output drives the plan |
| `max_iterations` | integer | no | `1` | Maximum planning cycles |
| `generated_key` | string | no | `generated` | State key for generated effects |
| `stop_on_done` | boolean | no | `true` | Stop when plan signals completion |
| `max_effects` | integer | no | `8` | Max effects per planning cycle |
| `max_steps` | integer | no | `8` | Alias for `max_effects` |
| `prime_template` | string | no | built-in | Custom prime template for planning |

**Example:**
```yaml
- type: reflector
  name: goal
  max_iterations: 3
  effects:
    - type: prompt
      name: propose_steps
      prompt_type: json
      template: "Given the goal '{{user_goal}}', what are the next steps?"
    - type: prompt
      name: execute
      template: "Execute: {{prime.goal.propose_steps.value}}"
```

A reflector is the effect most worth switching off for a single run: a profile
with `effects.goal.enabled: false` turns agentic planning off while leaving the
rest of the orchestration intact — see [Disabling Effects](#disabling-effects).

---

### `tool`

Executes a non-LLM side-effect via a named plugin. The plugin runs synchronously and writes its result to state. Tool effects do not require an `adapter` or `model` at the orchestration level — those fields only need to be set if the orchestration also contains `prompt` effects.

**State output path:** `prime.<name>.value`

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `type` | `"tool"` | yes | — | |
| `name` | string | yes | — | Pattern `^[A-Za-z_][A-Za-z0-9_]*$`; `iter_<N>` reserved |
| `provider` | string | yes | — | Plugin name: `ffmpeg`, `comfyui` |
| `prompt` | string | no | — | Primary input text. Mustache-rendered. For comfyui: the image generation prompt |
| `model` | string | no | — | Model/checkpoint name. For comfyui: checkpoint filename |
| `params` | object | no | `{}` | Plugin-specific parameters. All string values support Mustache rendering. Takes precedence over top-level `prompt`/`model` |
| `timeout_ms` | integer | no | — | Per-effect timeout in milliseconds |
| `on_error` | string | no | `fail` | `fail`, `skip`, `continue` |
| `description` | string | no | — | |

**Supported providers:**

| Provider | Description | Required inputs | Result value |
|----------|-------------|-----------------|--------------|
| `ffmpeg` | Run an ffmpeg command | `params.input` (path), `params.output` (path), `params.flags` (optional) | Output file path |
| `comfyui` | Generate an image via ComfyUI REST API | `prompt` (text), `model` (checkpoint filename) | Image path, base64, or URL |

**ffmpeg example:**
```yaml
- type: tool
  name: transcode
  provider: ffmpeg
  params:
    input: "{{prime.download.value}}"
    output: ./output/video.mp4
    flags: "-c:v libx264 -crf 23"
```

**comfyui example:**
```yaml
- type: tool
  name: generate_image
  provider: comfyui
  prompt: "a red apple on a wooden table, photorealistic"
  model: flux1-schnell-fp8.safetensors
  params:
    image_output: path
    image_dir: ./output/images
    width: 512
    height: 512
    steps: 4
```

**comfyui params reference:**

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `prompt` (top-level) | string | — | Image generation prompt text. Mustache-rendered |
| `model` (top-level) | string | plugin default | Checkpoint filename (e.g. `flux1-schnell-fp8.safetensors`) |
| `image_output` | string | `path` | `path`, `base64`, or `url` |
| `image_dir` | string | `./output/images` | Directory for `image_output: path` |
| `width` | integer | `512` | Image width in pixels |
| `height` | integer | `512` | Image height in pixels |
| `steps` | integer | `20` | Sampling steps |
| `cfg` | float | `7.0` | CFG scale |
| `sampler_name` | string | `euler` | Sampler name |
| `scheduler` | string | `normal` | Noise scheduler |
| `seed` | integer | random | Seed; auto-generated if absent or negative |
| `negative_prompt` | string | `""` | Negative prompt |
| `workflow` | object | — | Optional: full custom ComfyUI workflow (overrides built-in) |

---

### `use`

Runs another orchestration as an isolated sub-step. State is fully isolated: declared `inputs` are passed in as initial state; the parent does not see the child's working state directly. Outputs land at `prime.<name>` according to the namespacing mode (see below).

**State output path:** `prime.<name>.value` (declared-outputs mode) or `prime.<name>.<child_effect>.value` (full-namespace mode)

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `type` | `"use"` | yes | — | |
| `name` | string | yes | — | Pattern `^[A-Za-z_][A-Za-z0-9_]*$` |
| `ref` | string | * | — | Library lookup across configured sources. Bare (`utilities/critique`) or source-qualified (`hub:utilities/critique`) |
| `path` | string | * | — | Filesystem path (absolute, cwd-relative, or parent-orchestration-relative) |
| `inline` | string | * | — | Mustache template that renders to orchestration YAML at runtime |
| `orchestration` | string | * | — | **DEPRECATED** — use `ref` or `path` instead. Still accepted; emits `DeprecationWarning` |
| `validate` | bool | no | `true` | Schema-validate inline YAML before execution |
| `inputs` | object | no | `{}` | Map of name → value passed to child as initial state. String values are Mustache-rendered |
| `outputs` | object | no | — | Map of result-key → child dot-path. When present, switches to declared-outputs mode |
| `on_error` | string | no | `fail` | `fail`, `skip`, `continue` |
| `description` | string | no | — | |

\* Exactly one of `ref` / `path` / `inline` / `orchestration` must be present.

**Reference fields:**

- **`ref`** — library lookup across every source configured in [`runtime.library.sources`](./library-sources.md). With the default configuration (curation only) a slash-delimited name resolves under `src/circuitry/curation/`: `ref: utilities/critique` → `src/circuitry/curation/utilities/critique.yml`. With more sources configured, refs come in two forms:
  - **Bare** — `ref: utilities/critique` searches sources in precedence order and takes the first match. If more than one source matches, the run logs a warning naming the others.
  - **Source-qualified** — `ref: hub:utilities/critique` resolves in exactly the source named `hub` and never falls through to another source. A colon only qualifies when the prefix names a configured source, so a value like `C:/tmp/orch.yml` is never mistaken for one.
- **`path`** — filesystem path. Absolute, cwd-relative, or relative to the parent orchestration's directory. Use this for orchestrations that aren't in the curation library (e.g. project-local helpers).
- **`inline`** — Mustache template that renders to YAML at runtime. The template typically references a prior prompt's structured output (`{{prime.plan.value}}`) and is fully validated against the schema before execution unless `validate: false`.
- **`orchestration`** — **deprecated**. Functionally a hybrid of `path` and `ref` with a fallback chain. Existing files still work but emit a deprecation warning at compile time. Migrate to `ref` (for library entries) or `path` (for filesystem references).

**Output namespacing modes:**

| Mode | Trigger | Behavior |
|------|---------|----------|
| Declared-outputs | `outputs:` is set, OR child orchestration declares an `interface:` with `outputs` | `prime.<use_name>.value` is a flat dict of declared keys mapped to dot-path values from the child state |
| Full-namespace | No `outputs:` set, no interface outputs | The entire child `prime` subtree is exposed at `prime.<use_name>.<child_effect>.value` (matches `dynamic` namespacing) |

**Recorded pins (`runtime.library_refs`):**

Every `ref:` that resolves through a library source is recorded in the run state under `runtime.library_refs`, and mirrored onto the effect's own `meta.library_ref`:

```json
{
  "ref": "hub:utilities/critique",
  "source": "hub",
  "path": "/home/me/.cache/circuitry/library/hub/6f1c…/utilities/critique.yml",
  "sha": "6f1c9a2…",
  "cache_path": "/home/me/.cache/circuitry/library/hub/6f1c9a2…",
  "resolved_at": "2026-01-30T12:00:00+00:00"
}
```

For a `github` source, `sha` is the pinned commit the cache was fetched at and `cache_path` is the directory it was served from — which is what makes a later run reproduce the identical tree with **no network access at all** (only `cof library refresh` ever fetches). Local sources (`curation`, `folder`) have no commit to pin, so `sha` and `cache_path` are `null` and the resolved `path` is the record. Pins are recorded at every nesting depth, so a ref three orchestrations deep still shows up in the root run state.

**Unfetched sources fail early:**

A `ref:` pointing at a source whose cache has never been populated fails at **validate / preflight** time — never mid-run — with the command that fixes it:

```
library_ref:hub:utilities/critique: not ready — use ref 'hub:utilities/critique' cannot be
resolved: Library source 'hub' (owner/name@main) has not been fetched yet — run
`cof library refresh hub`. Run `cof library refresh hub` before this orchestration runs.
```

The check follows the static `use` graph, so a ref reached transitively through other orchestrations is caught just as early. Genuinely unknown refs (a typo, an entry that no source carries) are not a preflight failure — they surface as the `use` effect's own error at run time.

**Cycle detection:**

The runtime tracks a per-execution call stack of resolved orchestration paths. If A invokes B which invokes A, a `RecursionError` is raised before infinite recursion happens. The same check runs statically during `validate()` — cycles are detected at validate time before any execution. The static walk follows `ref:` edges **across sources**, resolving remote entries to their cache paths, so a cycle that closes through a `github` source (A in a folder → B in the hub cache → A) is caught statically too. Inline-mode subs are excluded from the static check (their content renders at runtime) but the runtime hashes the rendered YAML, so an inline template producing identical YAML in a parent of itself will still be caught.

**Example — library reference, declared outputs:**
```yaml
- type: use
  name: critique_step
  ref: utilities/critique
  inputs:
    content: "{{prime.draft.value}}"
    criteria: "clarity, accuracy, brevity"
  outputs:
    score: prime.score.value
    notes: prime.notes.value
```

**Example — source-qualified library reference:**
```yaml
- type: use
  name: hub_critique
  ref: hub:utilities/critique   # exactly the source named 'hub'; never another
  inputs:
    content: "{{prime.draft.value}}"
    criteria: "clarity, accuracy"
```

**Example — filesystem path, full-namespace mode:**
```yaml
- type: use
  name: research
  path: ./local/research-helper.yml
  inputs:
    topic: "{{prime.intake.value}}"
# child has effects 'gather' and 'synthesize';
# both are reachable as prime.research.gather.value and prime.research.synthesize.value
```

**Example — inline, LLM-generated plan:**
```yaml
- type: prompt
  name: plan
  template: "Output a Circuitry orchestration YAML to ..."
- type: use
  name: run_plan
  inline: "{{prime.plan.value}}"
  validate: true
```

---

## Disabling Effects

Any named effect can be switched off for a single run from a
[profile](profiles.md), without editing the orchestration:

```yaml
# profiles/no-planning.yml
effects:
  goal:
    enabled: false
```

```bash
cof run recipe --profile no-planning
```

A disabled effect **is not executed**. In its place the runtime writes a skip
node — deliberately the same shape `on_error: skip` leaves behind, so
downstream handling is uniform:

```json
{
  "value": null,
  "meta": { "disabled": true, "created_at": "...", "completed_at": "..." }
}
```

The per-effect `on_effect_complete` hook still fires for the node, so
observability sees the skip rather than a gap.

**Rules:**

| Situation | Behavior |
|-----------|----------|
| Disabled container (`dynamic` / `loop` / `conditional` / `reflector`) | The whole subtree is disabled — the container's node is written as disabled and nothing inside it runs (no child nodes appear at all) |
| Disabled `loop` body effect | Skipped in every iteration; `prime.<loop>.iter_<n>.<name>` is a skip node |
| Disabled `loop` `collect` target | Each iteration's slot is a skip node, and `prime.<loop>.collected.value` omits it — `collected` reports only values that were actually produced |
| Disabled effect inside an `if`/`else` branch | Skipped when that branch is selected; the branch's other effects run normally |
| A container's *condition* (`if:` on a conditional, `while:` on a loop) | Not disableable — it is a condition, not an effect, and a container with no condition has no defined branch. Targeting `<name>.if`, `<name>.while`, or `<name>.condition` fails profile validation with an actionable error. Disable the whole container instead |
| Unknown effect path | Fails profile validation, listing the orchestration's valid paths |
| Anonymous (transparent) `conditional`/`loop` | Contributes no path segment, so it has no address to disable — disable its individual named children instead |

**Downstream coherence.** A disabled node's `value` is `null`, so:

- Mustache templates referencing it render empty:
  `"report on <{{prime.goal.value}}>"` → `"report on <>"`
- CEL conditions that test for a value evaluate `False` — both by comparison
  (`state.prime.goal.value == "yes"`) and via the existing error→`False`
  behavior (`size(state.prime.goal.value) > 0`).
- Negative tests are the exception worth knowing: `state.prime.goal.value != ""`
  is `True` against `null`, exactly as it would be for any unset value. Prefer
  positive tests in conditions that may read a disableable effect.

A run with no `--profile` is unaffected: `enabled` is never set from
orchestration YAML, so every effect compiles as enabled.

---

## State Path Addressing

### Mustache Template Interpolation

Templates use Mustache syntax (`{{...}}`). Two kinds of references:

| Reference type | Syntax | Example |
|---------------|--------|---------|
| Initial state key | `{{key}}` | `{{user_input}}` |
| Top-level effect output | `{{prime.<name>.value}}` | `{{prime.summarize.value}}` |
| Nested effect inside dynamic | `{{prime.<dynamic_name>.<child_name>.value}}` | `{{prime.pipeline.outline.value}}` |
| Loop iteration element | `{{<each.as>}}` or `{{item}}` | `{{topic}}` (when `as: topic`) |
| Loop iteration index | `{{_loop_index}}` | Zero-based index, available in both `each` and `while` loop bodies |

**Rules:**
- Never use `{{<name>.value}}` or `{{<name>}}` alone for effect outputs — always include the `prime.` prefix
- Nested effects always include their parent dynamic name in the path

### CEL Expressions

CEL expressions (in `if.expr` and `while.expr`) evaluate against a root object named `state`.

| ✅ Correct | ❌ Wrong |
|-----------|---------|
| `state.prime.my_effect.value == "yes"` | `prime.my_effect.value == "yes"` |
| `state.prime.pipeline.step.value != ""` | `my_effect.value != ""` |

**Always use the full `state.prime.<name>.value` prefix in CEL expressions.**

### Loop Iteration Paths

Each loop iteration writes to an indexed path:
```
prime.<loop_name>.iter_0.<body_effect_name>.value
prime.<loop_name>.iter_1.<body_effect_name>.value
...
```

Reference a specific iteration from outside the loop:
```yaml
template: "First result: {{prime.explain.iter_0.summary.value}}"
```

---

## Patterns & Antipatterns

### Chain vs Tree

| Use `chain` when... | Use `tree` when... |
|--------------------|-------------------|
| Later effects need prior outputs | Effects are independent |
| Sequential processing pipeline | Parallel analysis of same input |
| Order matters | Order doesn't matter |

### Named vs Transparent Control

- **Named** loops and conditionals record wrapper metadata (iteration count, decision result) to state — use when you need to inspect or reference the control structure itself
- **Unnamed** (transparent) loops and conditionals execute without wrapper records — simpler, lower overhead

### Staged Prompts Over Single-Shot

For complex outputs, break into stages rather than asking the model to do everything at once:

```yaml
# Good: staged
- type: prompt
  name: outline
  template: "Outline the key points of: {{text}}"
- type: prompt
  name: expand
  template: "Expand each point: {{prime.outline.value}}"
- type: prompt
  name: finalize
  template: "Polish and finalize: {{prime.expand.value}}"

# Avoid: single-shot complex generation
- type: prompt
  name: result
  template: "Read, outline, expand, and polish this text in one shot: {{text}}"
```

### Same Name in Both If/Else Branches

Use the same inner effect name in both `then` and `else` so the consuming template works regardless of which branch ran:

```yaml
# Good
then:
  - type: prompt
    name: response        # same name
    template: "Admin: ..."
else:
  - type: prompt
    name: response        # same name
    template: "User: ..."

# Use downstream
- type: prompt
  name: display
  template: "{{prime.check_role.response.value}}"   # always resolves
```

### Loop `each.in` Must Point to a JSON Array

The state path in `each.in` must resolve to an array at runtime. This means it must point to a `prompt_type: json` effect whose output is a JSON array:

```yaml
# Good: source is prompt_type: json producing an array
- type: prompt
  name: topics
  prompt_type: json
  schema: {type: array, items: {type: string}}
  template: "List topics as a JSON array."

- type: loop
  each:
    in: prime.topics.value   # resolves to an array
    as: topic
  body: [...]

# Bad: source is prompt_type: text — will fail at runtime
- type: prompt
  name: topics
  template: "List topics."   # returns text, not array

- type: loop
  each:
    in: prime.topics.value   # not an array
  body: [...]
```

---

## LLM Authoring Rules

The following rules are sufficient for generating structurally correct Circuitry orchestration YAML. Apply all of them exactly.

**File structure:**
1. Top-level fields: `adapter` (string), `model` (string), `effects` (array). Only `effects` is required. Additional top-level keys are allowed.
2. `adapter` and `model` are only required when the orchestration contains `prompt` or `reflector` effects. Tool-only orchestrations (`type: tool` effects only) do not need `adapter` or `model`.
3. Valid `adapter` values: any name in the compiled-in adapter registry (`cof list --extensions`) — e.g. `ollama`, `openai`, `anthropic`, `litellm`, `cyberdiner`, `host_claude`. Two need special handling. `cyberdiner` is a job-queue broker: `model:` must be a capability tier (`cheap`, `fast-cheap`, `fast`, `good-cheap`, `good`, `good-fast`, `alpha` — the network owns the list), not a provider model name, and `runtime.adapters.cyberdiner.expo_url` / `token` must be set in config — never in the YAML. `host_claude` is MCP-only (the host Claude session generates each prompt — set via `circuitry-mcp` rather than config.json. By default rejects non-Claude `model:` pins; pass `override_model=True` to `run_orchestration` to ignore the pin and run through Claude regardless).
4. Valid `flow` values: `chain`, `chain_of_thought`, `cot` (all sequential); `tree`, `tree_of_thought`, `tot` (all parallel).

**Effect types and required fields:**
5. Valid `type` values: `prompt`, `dynamic`, `if`, `conditional`, `loop`, `reflector`, `tool`.
6. `prompt`: requires `name` and exactly one of `template` or `messages`. Optional: `prompt_type` (default `text`), `schema` (required when `prompt_type: json`). Do NOT use `prompt_type: image` — use `type: tool, provider: comfyui` for image generation.
7. `dynamic`: requires `name`, `effects` (non-empty array), optional `flow` (default `chain`).
8. `if` / `conditional`: requires `if` (condition object) and `then` (array). `name` is optional. `else` is optional.
9. `loop`: requires `body` (non-empty array) and exactly one of `each` or `while`. `name` is optional.
10. `reflector`: requires `name` and `effects` (non-empty array).
11. `tool`: requires `name` and `provider`. Tool effects are for non-LLM side-effects only — generating images, processing video/audio, file conversion. Never use a tool effect for text summarization, analysis, writing, coding, or data extraction — those are `prompt` effects. Supported providers: `ffmpeg` (requires `params.input` and `params.output`), `comfyui` (requires `prompt` and `model` as top-level fields; `params` for sampler settings). Top-level `prompt` supports Mustache rendering. All string values in `params` also support Mustache rendering.

**Naming:**
12. All `name` values must match `^[A-Za-z_][A-Za-z0-9_]*$` — letters, digits, underscores; must start with letter or underscore; no spaces or dots. The pattern `iter_<N>` (e.g. `iter_0`) is reserved and must not be used as a name.
13. Effect names must be unique among siblings within the same scope.

**State path addressing:**
14. In templates (Mustache): use `{{key}}` for initial state keys; use `{{prime.<name>.value}}` for top-level effect outputs; use `{{prime.<dynamic_name>.<child_name>.value}}` for outputs nested inside a dynamic.
15. In CEL expressions (`if.expr`, `while.expr`): always use the full prefix `state.prime.<name>.value`. Never omit `state.`.
16. Loop `each.in` must point to a `prompt_type: json` effect whose output is a JSON array (e.g. `prime.my_prompt.value`).

**If/else branches:**
17. Use the same inner effect `name` in both `then` and `else` branches of any `if` effect, so downstream state path references resolve regardless of which branch executed.

**Atomic design philosophy:**
18. Prefer many small effects over few large ones — each LLM call should do one focused thing (classify, plan, draft one block, review). If a prompt template asks the model to analyze AND generate AND review, split it into separate effects.
19. Use `prompt_type: json` to produce structured data that downstream effects consume via state interpolation. This is how effects pass typed data to each other.
20. A complex JSON schema is a smell. If a prompt needs more than 3–4 schema properties, split it into multiple smaller prompts that each produce a simpler schema, then interpolate the results downstream. Do not prescribe a fixed number of effects per dynamic — let the problem dictate the decomposition.
21. Use `dynamic(chain)` to sequence dependent atomic steps; `dynamic(tree)` for independent parallel work. Use `loop(each) + collect` to process items individually and aggregate results into an array.
22. Use `if(cel)` to make decisions based on state values from prior effects — route the orchestration dynamically rather than hardcoding assumptions.

**Design patterns:**
23. **Prompt-then-tool pipeline:** Use a prompt effect to generate parameters (e.g. ffmpeg flags, image prompts), then a tool effect to execute with those parameters. The prompt's structured output feeds the tool's params via Mustache interpolation (e.g. `{{prime.plan_flags.value.output_path}}`).
24. **Staged decomposition:** Break complex generation into: plan (json) → per-item generation (loop+collect) → assembly (prompt) → review (prompt). Each stage is a small, focused LLM call.
25. **State-based branching:** Use `if(cel)` with `state.prime.<name>.value.<field>` to branch on structured output from prior effects. Use the same effect name in both `then` and `else` branches for consistent downstream state paths.

**Composition via `use`:**
26. **Prefer `ref:` over `path:` for curation entries** — `ref: utilities/critique` is a stable library lookup; `path:` is for project-local files outside the curation library. Never use the deprecated `orchestration:` field — it emits a deprecation warning and exists only for back-compat.
27. **Declare `outputs:` when the caller needs specific fields** — produces a flat dict at `prime.<use_name>.value`. Omit `outputs:` (and the child `interface:`) to opt into full-namespace mode where every child effect is reachable at `prime.<use_name>.<child_effect>.value`.
28. **Declare an `interface:` block on reusable utilities** — typed `inputs` (with `required: true`) get validated automatically; typed `outputs` with `path:` auto-generate the caller's output mapping so callers don't have to repeat dot-paths. The curation library at `src/circuitry/curation/utilities/` is the canonical exemplar.
29. **Don't form `use:` cycles.** A→B→A is detected at validate time and at runtime. If two utilities legitimately need to call each other, factor out the shared logic into a third utility they both call.
