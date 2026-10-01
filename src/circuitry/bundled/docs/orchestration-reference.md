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
| `adapter` | string | no | from config.json | Adapter: `ollama`, `openai`, `anthropic`, `litellm` |
| `model` | string | no | from config.json | Model identifier (e.g. `llama3`, `gpt-4o`, `claude-haiku-20240307`) |
| `flow` | string | no | `chain` | Top-level flow for the implicit root dynamic |
| `version` | string | no | — | Free-form version string for **this document**, e.g. `"1.2.0"`. Not a schema version and not a feature gate — the runtime reads it and ignores it. Omit unless you are versioning the file |

Other top-level keys: `description` (free text for readers of the file) and two that feed the run's configuration, `runtime:` and `plugins:` (below). Any other key is ignored, and `cof check` says so — see [What `cof check` and `cof run` reject](#what-cof-check-and-cof-run-reject). How much of `runtime:` and `plugins:` applies depends on how the document reached `cof`:

- **`runtime:`** — `runtime.complexity` and `runtime.state` (e.g. `record_children`) always apply. Every other key — `adapters`, `plugins`, `persistence`, `library`, `mcp`, anything else — is a host setting.
- **`plugins:`** — runtime-plugin modules to load. An entry config.json already lists in `plugins` or `enabled_plugins` always loads; any other is a host setting too.

A file you run by path (`cof run ./my.yml`, `cof check` / `cof score` on a path, the SDK's `run_orchestration`) is trusted: its whole `runtime:` block and `plugins:` list apply, and `cof run` / `cof check` print one line naming the host settings among them (keys only, never values). A document that arrives any other way — a library name, `cof run-library`, MCP, the REST trigger, a generated plan — is limited to `runtime.complexity` and `runtime.state`; its host settings are ignored with a warning naming each key. `use:` children never contribute their own `runtime:` or `plugins:`.

A host that only ever runs its own documents can trust every document with `trust_orchestration_runtime: true` in config.json (or `CIRCUITRY_TRUST_ORCHESTRATION_RUNTIME=1`).

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

## What `cof check` and `cof run` reject

`cof check` and `cof run` apply one structural check to a document before anything in it runs — `cof run` does not start a document `cof check` rejects. A `use` child loaded by `path:` or `ref:` gets the same check when it loads, as an `inline:` child always has (`validate: false` on the `use` turns it off). Beyond the schema:

- **A repeated key** in one mapping is an error naming the key and where it is, in every document the runtime loads, YAML or JSON. YAML and JSON on their own would each keep only the last one.
- **An unknown key** on an effect or at the top level is ignored, so it is reported. A near miss of a key that effect knows — `whlie:`, `max_iteration:`, `temlpate:`, or `adapter:` on a `prompt` where `provider:` belongs — is an **error** naming the key it meant; any other unknown key is a **warning**. Inside `each:`, `if:`/`while:` conditions, `retries:`, `messages:` and `assets:` entries, any other key is a schema error.
- **An `interface.inputs` default that doesn't match its declared `type`** is an error naming the input, the type and the value — no string coercion, so a quoted numeral default is flagged, not silently accepted.
- **A loop needs exactly one of `each` or `while`.**
- **A malformed Mustache template** — an unclosed tag (`{{input.topic}`), a section closed under the wrong name — is an error naming the field, wherever a template is rendered. Were one to reach a run anyway, rendering it fails the effect under its `on_error`; the raw text is never sent on.

`cof check` also **warns** when a tool's `params.args` holds a value YAML read as a number, boolean or null: the tool receives `str()` of it, so an unquoted `0x1` arrives as `1`, `off` as `False`, `-0` as `0`. Quote the argument.

---

## Effect Types

### `prompt`

The atomic execution unit. Performs exactly one model invocation and writes a typed result to state.

**State output path:** `prime.<name>.value`

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `type` | `"prompt"` | yes | — | |
| `name` | string | yes | — | Pattern `^[A-Za-z_][A-Za-z0-9_]*$`; `iter_<N>` reserved; `value`/`meta`/`input`/`prime`/`runtime` reserved |
| `template` | string | one-of | — | Mustache template; mutually exclusive with `messages` |
| `messages` | array | one-of | — | Role-based messages; mutually exclusive with `template`. Sent as real conversation turns (a system message, then user/assistant turns) to adapters that take them — see [Generation options](#generation-options-messages-and-images) |
| `prompt_type` | string | no | `text` | `text`, `json`, `boolean`, `number`, `array`, `object`, `tool`. `boolean`/`number` parse the reply leniently (`Yes.`, `**TRUE**`, `42.`, `1e3` all read correctly) and raise — rather than decoding to `null` — on a reply that still doesn't parse. Raw reply kept on `meta.answer` |
| `schema` | object | no | — | JSON Schema for validating structured output |
| `description` | string | no | — | Human-readable description |
| `model` | string | no | — | Per-effect model override |
| `provider` | string | no | — | Per-effect provider override |
| `provider_fallbacks` | array | no | — | Ordered fallback providers |
| `params` | object | no | — | Generation parameters. `temperature`, `max_tokens` and `stop` are mapped to each provider's own names; every other key goes to the provider unchanged (e.g. ollama `num_ctx`, OpenAI `top_p`) — see [Generation options](#generation-options-messages-and-images) |
| `timeout_ms` | integer | no | — | Per-attempt timeout in milliseconds, capped by the adapter's `timeout_seconds`; rounded up to whole seconds. `0` or absent: the adapter's timeout |
| `deterministic` | boolean | no | `false` | Temperature 0 unless `params.temperature` is set, plus a fixed seed on ollama and openai unless `params.seed` is set |
| `inputs` | object | no | — | Prompt-local key/value pairs for template rendering |
| `assets` | array | no | — | Images for a vision model: `[{kind: "image", ref: "path/to/img"}]`. `ref` is a Mustache template rendering to a local path or an `http(s)` URL. Other kinds are skipped with a warning |
| `retries` | object | no | — | `{max_attempts: N, backoff_ms: M}` |
| `on_error` | string | no | `fail` | `fail`, `skip`, `continue` |

**Example — text output:**
```yaml
- type: prompt
  name: summarize
  template: "Summarize this article in one sentence: {{input.article_text}}"
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
  template: "Extract a JSON array of key items from: {{input.text}}"
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
      content: "Is this text positive? {{input.text}}"
```

#### Generation options, messages and images

`params`, `deterministic`, `timeout_ms`, `messages` and `assets` reach the adapter on every call:

| | ollama | OpenAI-compatible family, `openai` | `anthropic` |
|---|---|---|---|
| `params.temperature` | `options.temperature` | `temperature` | `temperature` |
| `params.max_tokens` | `options.num_predict` | `max_tokens` (`openai`: `max_completion_tokens`) | `max_tokens` (else the adapter's configured `max_tokens`) |
| `params.stop` (string or list) | `options.stop` | `stop` | `stop_sequences` |
| any other `params` key | `options.<key>`; `format`, `keep_alive` and `think` go top level | request body | request body |
| `deterministic: true` seed | `options.seed: 0` | `openai` only: `seed: 0` | — |
| `messages` | `/api/chat` turns | `messages` turns | `system` field + turns |
| image assets | base64 `images` (local files only) | `image_url` parts (`data:` URL for a local file) | `image` blocks (base64 or `url`) |

A `tool` turn has no standalone form in the OpenAI-compatible or Anthropic APIs, so those adapters send it as a user turn prefixed `tool:`. Images go with the last user turn. Every other adapter (`litellm`, `replicate`, `watsonx`, `cyberdiner`, `host_claude`, and an out-of-tree adapter whose `generate()` has no `options` keyword) runs the prompt as before, with `messages` flattened into one `role: content` string, and records one warning naming what it ignored. `cof check` and run preflight warn when a prompt's image assets go to an adapter that cannot send images.

What lands in `meta` beyond the usual keys, each only when it has something to say:

- `meta.assets` — one `{kind, ref, media_type, size, sha256}` per local image (`{kind, ref}` for a URL). The image bytes never enter state.
- `meta.finish_reason` — the provider's stop reason (`stop`, `length`, `end_turn`, `max_tokens`, ...) when it reports one.
- `meta.warnings` — a reply cut off at the length limit (`finish_reason` `length`/`max_tokens`), options an adapter ignored, an asset kind no adapter sends.

**Example — an image to a local vision model:**
```yaml
- type: prompt
  name: regions
  provider: ollama
  model: qwen3.8:27b
  deterministic: true
  params: {max_tokens: 4096, keep_alive: 0, think: false}
  assets:
    - {kind: image, ref: "{{{input.run}}}/see/view.png"}
  prompt_type: json
  schema:
    type: object
    properties:
      regions: {type: array, items: {type: string}}
    required: [regions]
  template: "List the distinct regions of this image. Return ONLY a JSON object with \"regions\"."
```

A slow model needs a longer adapter timeout, `runtime.adapters.ollama.timeout_seconds: 1800` in config: a prompt's `timeout_ms` is capped by the adapter's `timeout_seconds`, so it can only shorten the wait. Write `ref` with triple braces (`{{{...}}}`): double braces HTML-escape the value, which breaks a URL with `&` in its query string.

---

### `dynamic`

A named container that executes child effects sequentially (`chain`) or in parallel (`tree`). Groups related effects and records aggregated metadata.

**State output path:** `prime.<name>.<child_name>.value`

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `type` | `"dynamic"` | yes | — | |
| `name` | string | yes | — | Name pattern; must be unique among siblings |
| `flow` | string | no | `chain` | `chain` or `tree` |
| `effects` | array | yes | — | Non-empty list of child effects |
| `description` | string | no | — | |
| `max_concurrency` | integer | no | unbounded (every child at once) | Max parallel workers when `flow: tree`. No meaning on `flow: chain`, which always runs one child at a time. |
| `stop_on_error` | boolean | no | `false` | `flow: tree` only. `true` cancels every child that has not started yet as soon as one fails; a child already running cannot be cancelled, finishes on its own, and its failure (if any) is not added to the dynamic's `meta.error` — only the triggering failure is. A cancelled child leaves no node and fires no hooks. No effect on `flow: chain`, where a failing child already stops the ones after it. |
| `on_error` | string | no | `fail` | `fail`, `skip`, `continue` — same meaning as on a leaf effect: governs whether a failure anywhere inside this dynamic propagates to *its own* parent (`fail`) or is recorded on this dynamic's own `meta.error` and swallowed there, letting the parent continue (`skip`/`continue`, the same degradation for both). |
| `labels` | object | no | — | Arbitrary metadata annotations, recorded on `meta.labels` |

**Flow semantics:**
- `chain` — sequential: each effect executes after the previous, and sees all prior outputs in state
- `tree` — parallel: all effects execute concurrently against the same input snapshot; none see each other's outputs. Each branch reports `on_effect_start` / `on_effect_complete` and reaches `--live-state` while it runs, from its own worker thread

**Example — chain (sequential pipeline):**
```yaml
- type: dynamic
  name: pipeline
  flow: chain
  effects:
    - type: prompt
      name: outline
      template: "Outline an essay on: {{input.topic}}"
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
      template: "Summarize: {{input.text}}"
    - type: prompt
      name: sentiment
      prompt_type: boolean
      template: "Is this text positive? {{input.text}}"
```

---

### `if`

Evaluates a condition against state and executes exactly one branch (`then` or `else`). Non-selected branches produce no effects. Name is optional — a named `if` records decision metadata to state.

**State output path (named):** `prime.<name>.<branch_effect_name>.value`

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `type` | `"if"` | yes | — | `"conditional"` also parses as a deprecated alias; always write `"if"` |
| `name` | string | no | — | Optional; enables state recording of the decision |
| `if` | object | yes | — | Condition definition |
| `if.mode` | string | no | `model` | `model` or `cel` |
| `if.template` | string | model only | — | LLM evaluates and returns boolean, parsed leniently (`Yes.`, `**TRUE**`, `Y` all read as true); an unreadable answer raises rather than defaulting to false. Raw reply on `meta.answer` |
| `if.expr` | string | cel only | — | CEL expression; must use `state.prime.<name>.value` prefix |
| `then` | array | yes | — | Effects when condition is true |
| `else` | array | no | `[]` | Effects when condition is false |
| `threshold` | number | no | `0.5` | Deprecated, no effect: the built-in evaluator is a categorical yes/no with no confidence to cut. Still recorded on `meta.threshold`; `cof check` warns when set. |
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
- Final pass (after the loop completes): `prime.<name>.last.<body_effect>.value` — the last *completed* iteration's node, same shape as `iter_<N>`; a zero-iteration loop writes no `last` key. Saved state (`--out`, `--live-state`) writes it as a reference to that pass, `"last": {"$ref": "iter_<N>"}`, not a second copy; `--state` and the TUI resolve it back.
- Aggregated (when `collect` is set): `prime.<name>.collected.value` — array of every iteration's collected effect value, in pass order. A pass that failed under `on_error: break`/`continue` is left out entirely (not a `null` placeholder), and its index is listed on `prime.<name>.meta.failed_passes`.
- From *inside* the body: `prime.<body_effect>.value` — the current pass. See [Referencing a sibling within an iteration](#referencing-a-sibling-within-an-iteration).

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `type` | `"loop"` | yes | — | |
| `name` | string | no | — | Optional; enables wrapper metadata recording and `collect` |
| `collect` | string | no | — | Name of a body effect; aggregates its `.value` across all iterations into `prime.<name>.collected.value`. Requires a named loop — `cof check` rejects `collect` on an unnamed loop, naming the fix (give the loop a `name`). |
| `flow` | `"chain"` \| `"tree"` | no | `chain` | `chain` = sequential (default). `tree` = all `each` iterations run in parallel via `ThreadPoolExecutor`. `while` loops always run sequentially. |
| `max_concurrency` | integer | no | unbounded | Max parallel workers when `flow: tree`. |
| `body` | array | yes | — | Non-empty list of effects to execute per iteration |
| `each` | object | one-of | — | Collection iteration; mutually exclusive with `while`. A loop with neither or both is an error |
| `each.in` | string | yes (each) | — | State path to a JSON array (must be `prompt_type: json` output), or a binding of an enclosing loop (its `each.as` name) |
| `each.as` | string | no | `item` | Variable name for current element in body templates |
| `while` | object | one-of | — | Continuation condition; mutually exclusive with `each` |
| `while.mode` | string | no | `model` | `model` or `cel` |
| `while.template` | string | model only | — | LLM returns boolean for continuation decision, parsed the same lenient way as `if.template`. Raw reply on `meta.answer` on each check |
| `while.expr` | string | cel only | — | CEL expression against state |
| `max_iterations` | integer | no | — (no cap) | Hard cap on iterations. Unset means the loop runs until its collection is exhausted (`each`) or its condition is false (`while`) |
| `min_iterations` | integer | no | `0` | Minimum iterations to run before the condition is checked at all. A forced pass does not evaluate the condition and discard the answer — it never evaluates it. `while` loops only; on an `each` loop it has no effect and `cof check` warns. |
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
  template: "List 3 topics about {{input.subject}} as a JSON array."

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

Each iteration writes into its own isolated state, merged back in index order when the loop finishes, but observers see it while it runs: every effect inside reports `on_effect_start` / `on_effect_complete` at its full path (`prime.draft_effects.iter_3.draft_effect`) from the worker thread running it, and `--live-state` shows each iteration's finished steps before the loop completes.

**While loop example:**
```yaml
- type: loop
  name: refine
  while:
    mode: model
    template: "Does this draft need more improvement? Answer yes or no.\n\n{{prime.polish.value}}"
  max_iterations: 5
  body:
    - type: prompt
      name: polish
      template: "Improve this text:\n{{prime.draft.value}}"
```

The condition is checked between passes, once the loop has run at least
`min_iterations` passes — a pass `min_iterations` forces runs without the
condition being evaluated at all, so it costs no model call and reads no
state on that pass. Once checked, it sees the pass that just finished
under the same within-iteration names the body uses — so `{{prime.polish.value}}`
above is the latest `polish` output, not the first one. Before the first
checked pass there is nothing to see yet and the name falls through to the enclosing scope.
In `mode: cel`, the condition also sees `state.iter.index`: the index of the
*last finished* pass — `-1` on the check before the first pass, `N - 1` once
N passes have run (a pass that failed under `on_error: continue` still
counts, since the condition only knows a pass ran) — so
`state.iter.index + 1 < 3` caps a loop at 3 passes. `state.iter.count` is
the same count with no `-1` offset (`0` before the first pass, `N` once N
have run); `state.iter.count < 3` reads the same thing without the `+ 1`
and is the recommended spelling for new conditions.

#### Referencing a sibling within an iteration

A body step reading the step before it — compute → classify → score — is the
most common multi-step loop shape. Three *different* questions get three
*different* paths, and substituting one for another fails silently:

| You want | Write | Legal where |
|---|---|---|
| A step's output in the **current pass** | `{{prime.<step>.value}}` | inside the body, and inside a `while` condition |
| One **specific past pass** | `{{prime.<loop>.iter_<N>.<step>.value}}` | **after** the loop only |
| **Every** pass's output | `{{prime.<loop>.collected.value}}` | after the loop (requires `collect`) |

```yaml
- type: loop
  name: score_items
  collect: score
  each: {in: prime.items.value, as: item}
  body:
    - type: prompt
      name: classify
      template: "Which category does this fall into? {{item}}"
    - type: prompt
      name: score            # reads classify from THIS pass
      template: "Rate this {{prime.classify.value}} item 1-10: {{item}}"
```

Rules of the form:

- **Resolution is a scope chain**: the current iteration first, then the
  enclosing scope, then root state. A name the iteration has not written falls
  through, so root inputs and effects from before the loop keep resolving.
- **Shadowing**: a body step named the same as an enclosing effect wins inside
  the body, and only inside the body.
- **Named and unnamed loops behave identically**, in `chain` and in `tree` flow.
  (A `tree` loop parallelises whole iterations, not the steps inside one.)
- **An `if` branch is its own link in the same scope chain.** A step inside a
  `then`/`else` branch resolves the branch's own earlier steps first, then
  falls through to the enclosing scope. This holds for a named `if` too, and
  for one nested inside another.
- **The bare form `{{<step>.value}}` also works** and means the same node. It is
  accepted, not preferred: a bare name can collide with a user-supplied state
  key, and `prime.`-prefixed cannot.
- **`{{prime.<loop>.<step>.value}}` does not resolve, by design.** `prime.<loop>`
  is the loop's own node — `iter_<N>`, `collected`, `meta`. `cof validate` warns.
- **`iter_<N>` inside the body is a trap.** `N` is a constant, so it renders
  *pass N's* output during every pass — stale data, not an empty string.
  `cof validate` warns.
- In CEL the same forms apply with the `state.` prefix and no braces:
  `state.prime.<step>.value`.

---

### `reflector`

A planning-time effect. Instead of executing a fixed set of effects, the reflector reads state, generates a Dynamic plan, and executes it. It can run multiple planning cycles (`max_iterations`). Used for adaptive, open-ended tasks where the number of steps is not known ahead of time.

The built-in prime constrains the plan's step descriptions and every generated effect's `template` to ASD-STE100 Simplified Technical English — short, imperative, unambiguous sentences — so generated steps read consistently for the small models and humans that consume them next. A custom `prime_template` opts out of this constraint; it is the author's choice to keep it or not.

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
| `max_effects` | integer | no | `8` | Max top-level effects per planning cycle. Enforced: a generated plan over the cap is an invalid plan (same path as a plan that fails to parse), with an error naming the count and the cap. Never truncated. |
| `max_steps` | integer | no | `8` | Alias for `max_effects` |
| `prime_template` | string | no | built-in | Custom prime template for planning. The built-in prime enforces ASD-STE100 Simplified Technical English on generated plan text; supplying your own opts out |

**Example:**
```yaml
- type: reflector
  name: goal
  max_iterations: 3
  effects:
    - type: prompt
      name: propose_steps
      prompt_type: json
      template: "Given the goal '{{input.user_goal}}', what are the next steps?"
    - type: prompt
      name: execute
      template: "Execute: {{prime.goal.propose_steps.value}}"
```

**The `{goal}` and `{context}` prime slots.** The built-in prime (and any custom
`prime_template` that keeps these placeholders) is rendered with two values
read from the run's root state, not from the reflector's own node:

- `{goal}` — the value of a top-level effect named `goal` (commonly a
  `prompt`), i.e. `prime.goal.value` at the run root. Empty when the
  orchestration has no such effect, or it hasn't produced a value yet.
- `{context}` — a concise, redacted summary of `runtime.effective_settings`:
  the run's `model`, `adapter`, and enabled `plugins`, as JSON. It is passed
  through the same redaction `runtime.effective_settings` itself gets before
  being embedded in state, and capped at 2000 characters, so it stays a
  planning hint rather than a full settings dump and never carries a secret.
  Empty when `runtime.effective_settings` hasn't been recorded yet.

Both are best-effort: a reflector nested under a loop, dynamic, or
conditional still reads the run's true root, not its immediate container —
including a reflector that is a branch of a `flow: tree` dynamic or a
parallel loop, where the branch's own state is otherwise isolated from the
rest of the run.

---

### `tool`

Executes a non-LLM side-effect via a named plugin. The plugin runs synchronously and writes its result to state. Tool effects do not require an `adapter` or `model` at the orchestration level — those fields only need to be set if the orchestration also contains `prompt` effects.

**State output path:** `prime.<name>.value`

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `type` | `"tool"` | yes | — | |
| `name` | string | yes | — | Pattern `^[A-Za-z_][A-Za-z0-9_]*$`; `iter_<N>` reserved; `value`/`meta`/`input`/`prime`/`runtime` reserved |
| `provider` | string | yes | — | Plugin name: `ffmpeg`, `comfyui` |
| `prompt` | string | no | — | Primary input text. Mustache-rendered. For comfyui: the image generation prompt |
| `model` | string | no | — | Model/checkpoint name. For comfyui: checkpoint filename |
| `params` | object | no | `{}` | Plugin-specific parameters. All string values support Mustache rendering. Takes precedence over top-level `prompt`/`model`. Quote every `args` entry: an unquoted `0x1`, `off` or `-0` reaches the tool as `1`, `False`, `0` (`cof check` warns) |
| `timeout_ms` | integer | no | — | Per-effect timeout in milliseconds |
| `on_error` | string | no | `fail` | `fail`, `skip`, `continue` |
| `description` | string | no | — | |

**Timeout:** `timeout_ms` on the effect wins, rounded up to the next whole second — a sub-second budget (e.g. `250`) never floors to a 0-second timeout, which some plugins would treat as "fail instantly" and others (curl-based ones) as "no limit at all". Unset, the tool's own default comes from `runtime.tools.timeout_seconds` in config (300s if that's unset too) — independent of `runtime.adapters.<name>.timeout_seconds`, the LLM adapter's socket timeout; a tool-only run, or one on the no-op adapter, still gets a real budget.

Not every plugin can actually be bounded by it:

| How a plugin honors it | Plugins |
|---|---|
| Subprocess timeout (`subprocess.run(timeout=...)` or curl `--max-time`) | Every binary-wrapping plugin: `ffmpeg`, `shell`, `git`, `gh`, `ripgrep`, `sed`, `awk`, `pandoc`, `imagemagick`, `exiftool`, `gpg`, `docker`, `kubectl`, and the rest of that family. `comfyui` and `web_search`/`weather` apply it to their own curl calls |
| Own network call | `web_fetch` (its own `params.timeout_ms`, when set, shortens it for that call — never extends past the effect's budget), `rss`, `wikipedia` (retries disabled so it can't multiply the budget), `whois`, `http`, `slack`, `notion`, `jira`, `github`, `gdrive`, `gcalendar`, `s3`, `linear`, `email_smtp`, `webhook`, `dns`, `mcp_client`, `playwright`, `screenshot`, `discord` (via a background thread with a deadline, since discord.py's webhook send has no timeout parameter of its own). These apply the timeout per socket operation (connect, each read, etc.), not to the whole call. |
| Sandboxed child process, killed on overrun | `python_eval` |
| Ignored — pure in-memory, nothing to bound | `json`, `xml`, `csv`, `regex`, `hash`, `hex`, `uuid`, `base64`, `gzip`, `zip`, `tar`, `fs`, `env_vars`, `validate_yaml`, `html_extract`, `pdf_extract`, `math`, `clock`, `system_info`, `process_list` |
| Ignored — has its own, separate bound instead | `port_check` (`params.timeout_ms`, socket-level, default 2s), `surrealdb` (the SDK's own socket timeout), `embed`/`rerank`/`vector_search` (local inference; the first call per model can also trigger an unbounded download) |

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

#### `params_json`

`params:` values are Mustache-rendered to **strings** — fine for scalars, but
there is no way to write a static YAML template for an array or object whose
shape depends on a prior step (e.g. a list of ticker symbols an earlier
effect produced). `params_json` closes that gap: its value is a Mustache
template rendered to text and then parsed as JSON, and the resulting
object is deep-merged over `params` (its keys win on conflicts). This keeps
`params` for the parts of the call that are known upfront and reserves
`params_json` for the parts that have to be assembled at runtime.

A prior step's list/object value can be a native Python list/dict already in
state (e.g. from an `array`/`object` prompt, the `json` plugin's parse/
extract, MCP `structuredContent`, a `surrealdb` result, or a loop `each.as`
item) — `params_json` serializes it back to JSON text when splicing it in.
It can also be a string that already holds JSON text; that string is
spliced in verbatim. Either way, use `{{{...}}}` (triple-stache) so the
value is not HTML-escaped, since `params_json` parses the whole rendered
template as JSON. Only splice a *whole* JSON value this way — a bare scalar
like `{{{input.name}}}` inside a JSON string literal breaks if the value
contains a `"`, `\`, or newline; put scalars under `params:` instead and
reserve `params_json` for array/object values.

Building a real array for an MCP tool call, from a list a previous step
computed (`prime.symbol_list.value` holding e.g. `'["AAPL","MSFT","TSLA"]'`):
```yaml
- type: tool
  name: get_equity_quotes
  provider: mcp
  params:
    server: robinhood
    tool: get_equity_quotes
  params_json: '{"arguments": {"symbols": {{{prime.symbol_list.value}}} }}'
```
This sends `arguments.symbols` as a JSON array in one call instead of one
`mcp` effect per symbol.

Building a nested object for a `surrealdb` `create`, from a JSON object a
previous step assembled (`prime.person_json.value`):
```yaml
- type: tool
  name: save_person
  provider: surrealdb
  params:
    mode: create
    table: person
  params_json: '{"data": {{{prime.person_json.value}}} }'
```

A `params_json` that fails to render to valid JSON, or renders to something
other than a JSON object, is a hard error (not silently ignored) — it is
treated like any other tool-effect failure and follows the effect's
`on_error` policy.

A handful of keys are security boundaries a plugin enforces — today, only
`shell`'s `allowed_commands` — and `params_json` may never set one: setting
it there is a hard error even when the rest of the rendered JSON is valid,
because `params_json` is runtime-built and can carry model-generated content
(an LLM's own output feeding back into what commands it is allowed to run).
The same key is also rejected in `params:` itself when any of its string
entries is still a Mustache tag (e.g. `allowed_commands: ["{{cmd}}"]`)
rather than a plain, written-down value — only a literal list counts.

---

## State Path Addressing

A run's **shadow state** has the same keys as the orchestration: every effect writes to the path its name and position dictate, one segment per named container, and later effects read it there.

### Mustache Template Interpolation

Templates use Mustache syntax (`{{...}}`). Two kinds of references:

| Reference type | Syntax | Example |
|---------------|--------|---------|
| Caller-supplied input | `{{input.<name>}}` | `{{input.user_input}}` |
| Top-level effect output | `{{prime.<name>.value}}` | `{{prime.summarize.value}}` |
| Nested effect inside dynamic | `{{prime.<dynamic_name>.<child_name>.value}}` | `{{prime.pipeline.outline.value}}` |
| Loop iteration element | `{{<each.as>}}` or `{{item}}` | `{{topic}}` (when `as: topic`) |

**Rules:**
- Never use `{{<name>.value}}` or `{{<name>}}` alone for effect outputs — always include the `prime.` prefix
- Never use a bare `{{<name>}}` for caller-supplied input — always include the `input.` prefix; a bare reference matching a declared `interface.inputs` name is a hard error from `cof check`
- Nested effects always include their parent dynamic name in the path
- A malformed tag (`{{input.topic}`, a section closed under the wrong name) is an error from `cof check` and `cof run`, never text sent on as written

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

**Outside the loop only.** `N` is a constant, so an `iter_<N>` path used inside
the body reads pass `N` during every pass — stale data rather than an error. To
read a sibling in the pass you are currently in, write `{{prime.<step>.value}}`;
see [Referencing a sibling within an iteration](#referencing-a-sibling-within-an-iteration).

### Iteration Bindings Inside Nested Containers

`{{<each.as>}}` and `{{_loop_index}}` reach every effect in the body, however
deeply it is wrapped — an `if` branch, a grouping `dynamic`, an inner loop, or
any combination of them:

```yaml
- type: loop
  name: outer
  each: {in: items, as: item}
  body:
    - type: dynamic          # grouping wrapper
      name: wrap
      effects:
        - type: prompt
          name: nested
          template: "sees: {{item.name}}"   # renders the current element
```

Inside a nested `dynamic`, its own children stay addressable by the short
sibling path (`{{wrap.nested.value}}`) as well as the absolute one, and a name
collision resolves to the nearer node — the dynamic's own child wins over an
outer binding of the same name.

---

## Patterns & Antipatterns

### Chain vs Tree

| Use `chain` when... | Use `tree` when... |
|--------------------|-------------------|
| Later effects need prior outputs | Effects are independent |
| Sequential processing pipeline | Parallel analysis of same input |
| Order matters | Order doesn't matter |

### Named vs Transparent Control

- **Named** loops and `if` effects record wrapper metadata (iteration count, decision result) to state — use when you need to inspect or reference the control structure itself
- **Unnamed** (transparent) loops and `if` effects execute without wrapper records — simpler, lower overhead

### Staged Prompts Over Single-Shot

For complex outputs, break into stages rather than asking the model to do everything at once:

```yaml
# Good: staged
- type: prompt
  name: outline
  template: "Outline the key points of: {{input.text}}"
- type: prompt
  name: expand
  template: "Expand each point: {{prime.outline.value}}"
- type: prompt
  name: finalize
  template: "Polish and finalize: {{prime.expand.value}}"

# Avoid: single-shot complex generation
- type: prompt
  name: result
  template: "Read, outline, expand, and polish this text in one shot: {{input.text}}"
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

The state path in `each.in` must resolve to an array at runtime. This means it must point to a `prompt_type: json` effect whose output is a JSON array, or to a binding of an enclosing loop (its `each.as` name) that itself holds an array, e.g. `s.crops` inside a loop whose `each.as` is `s`:

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
1. Top-level fields: `adapter` (string), `model` (string), `effects` (array). Only `effects` is required. `description`, `version`, `interface`, `flow`, `runtime` and `plugins` are the other keys the top level knows; anything else is ignored with a warning. A top-level `runtime:` block should set only `complexity` and `state`; never put `adapters`, `plugins`, `persistence`, `library` or credentials in it — those are host settings that belong in config.json. A document run by library name, fetched or generated has its copy ignored with a warning; a file run by path applies it with a notice.
2. `adapter` and `model` are only required when the orchestration contains `prompt` or `reflector` effects. Tool-only orchestrations (`type: tool` effects only) do not need `adapter` or `model`.
3. Valid `adapter` values: `ollama`, `openai`, `anthropic`, `litellm`.
4. Valid `flow` values: `chain` (sequential) and `tree` (parallel). Write nothing else — `chain_of_thought`/`cot` and `tree_of_thought`/`tot` still parse but are deprecated and warned about.

**Effect types and required fields:**
5. Valid `type` values: `prompt`, `dynamic`, `if`, `loop`, `reflector`, `tool`, `use`. (`conditional` still parses as an alias of `if` but is deprecated and warned about — always write `if`.)
6. `prompt`: requires `name` and exactly one of `template` or `messages`. Optional: `prompt_type` (default `text`), `schema` (required when `prompt_type: json`). Do NOT use `prompt_type: image` — use `type: tool, provider: comfyui` for image generation.
7. `dynamic`: requires `name`, `effects` (non-empty array), optional `flow` (default `chain`).
8. `if`: requires `if` (condition object) and `then` (array). `name` is optional. `else` is optional.
9. `loop`: requires `body` (non-empty array) and exactly one of `each` or `while`. `name` is optional.
10. `reflector`: requires `name` and `effects` (non-empty array).
11. `tool`: requires `name` and `provider`. Supported providers: `ffmpeg` (requires `params.input` and `params.output`), `comfyui` (requires `prompt` and `model` as top-level fields; `params` for sampler settings). Top-level `prompt` supports Mustache rendering. All string values in `params` also support Mustache rendering.

**Naming:**
12. All `name` values must match `^[A-Za-z_][A-Za-z0-9_]*$` — letters, digits, underscores; must start with letter or underscore; no spaces or dots. The pattern `iter_<N>` (e.g. `iter_0`) is reserved and must not be used as a name. `value`, `meta`, `input`, `prime` and `runtime` are also reserved — each collides with a structural slot the runtime itself writes.
13. Effect names must be unique among siblings within the same scope.
13a. Never name an effect after an effect type (`use`, `loop`, `if`, `dynamic`, `prompt`, `tool`, `reflector`). Name it after the job it does — `summarize_article`, not `prompt`. Generic names are what make two siblings collide; validation warns on them.
13b. Outputs — in `use.outputs` and `interface.outputs` alike — are objects: `summary: {path: prime.summarize.value, type: string}`. A bare path string is accepted as shorthand in both, but write the object form.
13c. Write each key at most once, and only the keys the effect type lists. A repeated key, or a near miss of a known key (`whlie`, `max_iteration`, `adapter` on a prompt where `provider` belongs), fails validation; any other unknown key is ignored with a warning. Quote every tool `params.args` entry — unquoted, YAML reads `0x1` as `1`, `off` as `false`, `-0` as `0`.

**State path addressing:**
14. In templates (Mustache): use `{{input.<name>}}` for caller-supplied input (never bare `{{key}}` — that is a hard error when `key` matches a declared `interface.inputs` name); use `{{prime.<name>.value}}` for top-level effect outputs; use `{{prime.<dynamic_name>.<child_name>.value}}` for outputs nested inside a dynamic.
15. In CEL expressions (`if.expr`, `while.expr`): always use the full prefix `state.prime.<name>.value`. Never omit `state.`.
16. Loop `each.in` must point to a `prompt_type: json` effect whose output is a JSON array (e.g. `prime.my_prompt.value`), or to a binding of an enclosing loop (its `each.as` name).

**If/else branches:**
17. Use the same inner effect `name` in both `then` and `else` branches of any `if` effect, so downstream state path references resolve regardless of which branch executed.
