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
| `flow` | string | no | `chain` | Top-level flow for the implicit root dynamic. `chain` or `tree` |
| `version` | string | no | — | Free-form version string for **this document**, e.g. `"1.2.0"` |
| `interface` | object | no | — | Declared `inputs` / `outputs` — see [Interface](#interface) |

Other top-level keys: `description` (free text for readers of the file) and two that feed the run's configuration, `runtime:` and `plugins:` (below). Any other key is ignored, and `cof check` says so — see [What `cof check` and `cof run` reject](#what-cof-check-and-cof-run-reject). How much of `runtime:` and `plugins:` applies depends on how the document reached `cof`:

- **`runtime:`** — `runtime.complexity` (see [Complexity Configuration](./complexity-config.md)) and `runtime.state` (e.g. `record_children`, see [Complete record](#complete-record-opt-in)) always apply; each replaces the config-level block of the same name. Every other key — `adapters`, `plugins`, `persistence`, `library`, `mcp`, anything else — is a host setting.
- **`plugins:`** — runtime-plugin modules to load. An entry config.json already lists in `plugins` or `enabled_plugins` always loads; any other is a host setting too.

**A file you run by path is trusted**, like a script you run: `cof run ./my.yml`, `cof check` / `cof score` on a path, a local file picked in the TUI Run view, the SDK's `run_orchestration(orchestration_path=...)` and scheduler jobs apply the document's whole `runtime:` block and `plugins:` list. When that includes host settings, `cof run` prints one line on stderr naming them (keys only, never values), and `cof check` shows the same line:

```
Warning: Applied host settings from my.yml: runtime.adapters.openai.base_url, plugins: acme.telemetry
```

`runtime.plugins.<name>` and `runtime.adapters.<name>` merge into the host's config key by key, not whole-block replace: a document setting `runtime.plugins.sqlite.*` does not drop the host's `runtime.plugins.shell.*`, and one setting `runtime.adapters.ollama.timeout_seconds` does not drop another adapter's config. The one exception is `runtime.plugins.shell.allowed_commands`: when the host pins it, the effective list is always the intersection of the host's and the document's, never the document's outright — the only `runtime:` key a trusted document cannot simply override (see [Host settings versus orchestration documents](./threat-model.md#6-host-settings-versus-orchestration-documents)).

**A document that arrives any other way is limited** to `runtime.complexity` and `runtime.state`: `cof run <library name>` (bundled, folder and github sources), `cof run-library` and `run_shared_orchestration`, the MCP `run_orchestration` / `validate_orchestration` tools, the REST trigger, and plans a model generates. The TUI's Library view's "run this entry" is limited the same way whatever the source, and its Chat view's "run it now" is limited too — saving a model-generated draft to disk does not make it a file you named by path. Run that saved file with `cof run f.yml`, or pick it from the Run view's own local-file list, to trust it like any other local file. Every limited document's host settings are ignored, with a warning from `cof run` (on stderr) and `cof check` naming each key, and an unlisted `plugins:` entry is skipped. `use:` children never contribute their own `runtime:` or `plugins:` — they run on the parent's settings.

This is the trust boundary for someone else's orchestration: the document decides what the run does, never where an adapter sends prompts and credentials, which binaries a tool runs, or where state is stored. `cof fetch` followed by `cof run ./fetched.yml` is running the file by path, so read a fetched file before you run it that way. A host that only ever runs its own documents can trust every document with `trust_orchestration_runtime: true` in config.json (or `CIRCUITRY_TRUST_ORCHESTRATION_RUNTIME=1`) — see [Configuration](./guidebook/04-configuration.md) for what that exposes. See the [threat model](./threat-model.md) for the risk.

**About `version`.** It is the author's own version string for the file — a changelog handle, nothing more. It is **not** a schema version and **not** a feature gate: the runtime reads it and ignores it, and no behaviour anywhere keys off its value. Numbers are still accepted for back-compat with files written before this was pinned down, but a string is canonical. Omit the field entirely unless you are actually versioning the document.

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

`cof check` and `cof run` apply one structural check to a document before anything in it runs — `cof run` does not start a document `cof check` rejects, so it cannot fail halfway with earlier effects already done. A `use` child loaded by `path:` or `ref:` gets the same check when it loads, as an `inline:` child always has (`validate: false` on the `use` turns it off). Beyond the schema:

- **A repeated key** in one mapping — two `template:` lines, two `effects:` blocks — is an error naming the key and where it is, in every document the runtime loads, YAML or JSON (a file, a library ref, a `use` child, `inline:` included). YAML and JSON on their own would each keep only the last one. A YAML document names both lines; a JSON document names the key's path, since `json.loads` carries no line numbers of its own.
- **An unknown key** on an effect or at the top level is ignored, so it is reported. One that is a near miss of a key that effect knows — `whlie:`, `max_iteration:`, `temlpate:`, or `adapter:` on a `prompt` where `provider:` belongs — is an **error** naming the key it meant. Any other unknown key is a **warning**. Inside `each:`, `if:`/`while:` conditions, `retries:`, `messages:` and `assets:` entries, every key is known, so any other key is a schema error.
- **An `interface.inputs` default that doesn't match its declared `type`** is an error naming the input, the type and the value — the same rule a caller-supplied value is checked against once it's filled in, but without that check's string coercion: a YAML/JSON default is already a typed value, not a CLI `-e`/`use:` value crossing as text, so a quoted numeral (`default: "3"` for an `integer` input) is flagged with a hint to remove the quotes rather than silently accepted.
- **A loop needs exactly one of `each` or `while`.** One with neither would run zero passes and report a clean termination.
- **A malformed Mustache template** — an unclosed tag (`{{input.topic}`), a section closed under the wrong name — is an error naming the field, wherever a template is rendered: a prompt's `template` or `messages`, a tool's `prompt`, `params` and `params_json`, a model-mode `if`/`while` template, a `use` effect's `inline` and string `inputs`. Were one to reach a run anyway, rendering it fails the effect under its `on_error`; the raw text is never sent on.

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
| `prompt_type` | string | no | `text` | `text`, `json`, `boolean`, `number`, `array`, `object`, `tool`. `boolean`/`number` parse the reply leniently (`Yes.`, `**TRUE**`, `42.`, `1e3` all read correctly; wrapping quotes/markdown/punctuation stripped first) and raise — rather than decoding to `null` — on a reply that still doesn't parse; `provider_fallbacks` (see below), when configured, is tried before `on_error`/`retries` apply. Raw reply kept on `meta.answer` |
| `schema` | object | no | — | JSON Schema for validating structured output |
| `description` | string | no | — | Human-readable description |
| `model` | string | no | — | Per-effect model override |
| `provider` | string | no | — | Per-effect provider override |
| `provider_fallbacks` | array | no | — | Ordered fallback providers. Tried on a dispatch failure *and* on a reply this effect can't use (an unreadable boolean/number, schema-invalid JSON) — either moves to the next provider before a retry is counted. Each attempt lands in `meta.fallback_attempts` with its own `tokens_sent`/`tokens_received` and, for an unusable reply, a size-capped `raw_reply` and a `decode_failed`/`schema_invalid` status |
| `params` | object | no | — | Generation parameters. `temperature`, `max_tokens` and `stop` are mapped to each provider's own names; every other key goes to the provider unchanged (e.g. ollama `num_ctx`, OpenAI `top_p`) — see [Generation options](#generation-options-messages-and-images) |
| `timeout_ms` | integer | no | — | Per-attempt timeout in milliseconds, capped by the timeout of whichever adapter that attempt actually dispatches to (`runtime.adapters.<name>.timeout_seconds` for *that* adapter — not necessarily the run default's, if `provider:`/`provider_fallbacks` sends the attempt elsewhere); rounded up to whole seconds. `0` or absent: the adapter's timeout |
| `deterministic` | boolean | no | `false` | Temperature 0 unless `params.temperature` is set, plus a fixed seed on ollama and openai unless `params.seed` is set |
| `inputs` | object | no | — | Prompt-local key/value pairs for template rendering |
| `assets` | array | no | — | Images for a vision model: `[{kind: "image", ref: "path/to/img"}]`. `ref` is a Mustache template rendering to a local path or an `http(s)` URL. Other kinds are skipped with a warning |
| `retries` | object | no | — | `{max_attempts: N, backoff_ms: M}`. A dispatch failure retries only if classified retryable (429, 408, 5xx, a timeout, a dropped connection); 400/401/403/404/422 and a missing key fail the attempt loop immediately. A reply that came back but failed to decode/validate (see `provider_fallbacks` below) always retries, same as before classification existed. Wait is exponential backoff with jitter from `backoff_ms`, capped at 60s; a provider's `Retry-After` header overrides the computed wait when the adapter can read one |
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
- `meta.tokens_sent_total` / `meta.tokens_received_total` — every attempt this dispatch made, failed/retried/fallen-back-from included, summed across all of them; `meta.tokens_sent`/`meta.tokens_received` stay the winning attempt's own count, unchanged in meaning.

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

A slow model needs a longer adapter timeout, `runtime.adapters.ollama.timeout_seconds: 1800` in config: a prompt's `timeout_ms` is capped by the timeout of whichever adapter the attempt actually dispatches to, so it can only shorten the wait — that's `ollama`'s own configured timeout here even when `ollama` isn't the run's default adapter, as long as this prompt's `provider:` (or a fallback) names it. Write `ref` with triple braces (`{{{...}}}`): double braces HTML-escape the value, which breaks a URL with `&` in its query string.

**`on_error` and preflight — optional adapters:** `cof check`/`cof run` walk every `adapter`/`provider` an orchestration references and probe its credentials before anything runs (`check()`, see the plugins pages). By default that's a **hard** dependency: a missing credential fails preflight for the whole file, even if only one effect needs it. Set `on_error: skip` (or `continue`) on every `prompt` effect that uses a given adapter and preflight reclassifies it as **soft** — a missing credential downgrades to a warning naming the effects that will skip, and the run proceeds, leaving those effects' `value` as `null`. An adapter is soft only when *every* effect referencing it tolerates failure; one effect without `on_error` handling makes the whole adapter a hard dependency again, and preflight's error names that effect specifically. This looks at each `prompt` effect's own `on_error`, not an enclosing `dynamic`/`loop`/`if` container's — a `prompt` effect nested in a container that tolerates failure still needs its own `on_error: skip`/`continue` to be classified as soft. `cof run --skip-preflight` bypasses preflight entirely (hard and soft alike) — unrelated to this classification.

---

### `dynamic`

A named container that executes child effects sequentially (`chain`) or in parallel (`tree`). Groups related effects and records aggregated metadata.

**State output path:** `prime.<name>.<child_name>.value`

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `type` | `"dynamic"` | yes | — | |
| `name` | string | yes | — | Name pattern; must be unique among siblings |
| `flow` | string | no | `chain` | `chain` or `tree`. See [Deprecated spellings](#deprecated-spellings) |
| `effects` | array | yes | — | Non-empty list of child effects |
| `description` | string | no | — | |
| `max_concurrency` | integer | no | unbounded (every child at once) | Max parallel workers when `flow: tree`. No meaning on `flow: chain`, which always runs one child at a time. |
| `stop_on_error` | boolean | no | `false` | `flow: tree` only. `true` cancels every child that has not started yet as soon as one fails; a child already running cannot be cancelled, finishes on its own, and its failure (if any) is not added to the dynamic's `meta.error` — only the triggering failure is. A cancelled child leaves no node and fires no hooks. No effect on `flow: chain`, where a failing child already stops the ones after it. |
| `on_error` | string | no | `fail` | `fail`, `skip`, `continue` — same meaning as on a leaf effect (see [Errors](guidebook/05-errors.md)): governs whether a failure anywhere inside this dynamic propagates to *its own* parent (`fail`) or is recorded on this dynamic's own `meta.error` and swallowed there, letting the parent continue (`skip`/`continue`, the same degradation for both). |
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
| `type` | `"if"` | yes | — | `"conditional"` also parses; see [Deprecated spellings](#deprecated-spellings) |
| `name` | string | no | — | Optional; enables state recording of the decision |
| `if` | object | yes | — | Condition definition |
| `if.mode` | string | no | `model` | `model` or `cel` |
| `if.template` | string | model only | — | LLM evaluates and returns boolean. The runtime wraps it and appends `Answer (yes/no):`, so phrase the ask as yes/no. Reply parsed leniently — `Yes.`, `yes, because …`, `**TRUE**`, `Y` parse as true, `No.`/`false!`/`N` as false (wrapping quotes/markdown/punctuation stripped first); an answer that still doesn't parse raises rather than defaulting to false, so `on_error` applies. Raw reply recorded on `meta.answer` (plus `meta.adapter`/`meta.model`) |
| `if.expr` | string | cel only | — | CEL expression; must use `state.prime.<name>.value` prefix |
| `if.strict` | bool | no | `false` | cel only. When true, an unset `state.` path raises instead of making the expression `False` |
| `then` | array | yes | — | Effects when condition is true |
| `else` | array | no | `[]` | Effects when condition is false |
| `threshold` | number | no | `0.5` | Deprecated, no effect: the built-in evaluator is a categorical yes/no with no confidence to cut. Still recorded on `meta.threshold`; `cof check` warns when set. |
| `on_error` | string | no | `fail` | `fail`, `continue`, `skip` |
| `labels` | object | no | — | Arbitrary metadata annotations, recorded on `meta.labels` (named only — an unnamed `if` has no node to carry it) |

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
    template: "Is this output high quality? Answer yes or no.\n\n{{prime.draft.value}}"
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
- Final pass (after the loop completes): `prime.<name>.last.<body_effect>.value` — the last *completed* iteration's node, same shape as `iter_<N>`. A pass that errored under `on_error: continue`/`break` is skipped in favor of the last one that finished; a zero-iteration loop writes no `last` key. Saved state writes it as a reference, `"last": {"$ref": "iter_<N>"}` — see [Loop Iteration Paths](#loop-iteration-paths).
- Aggregated (when `collect` is set): `prime.<name>.collected.value` — array of every iteration's collected effect value, in pass order. A pass that failed under `on_error: break`/`continue` is left out entirely (not a `null` placeholder), and its index is listed on `prime.<name>.meta.failed_passes`.
- From *inside* the body: `prime.<body_effect>.value` — the current pass. See [Referencing a sibling within an iteration](#referencing-a-sibling-within-an-iteration).
- From *inside* the body, the **previous** completed pass: `prime.<name>.prev.<body_effect>.value` (and `.meta`) — chain flow only (`each` and `while`); absent on the first pass, so a template renders it empty and CEL's `has()` reads false. A `flow: tree` body referencing it is a `cof check` error: tree passes run in parallel, so there is no previous one.
- Termination: `prime.<name>.value.termination.reason` — see [Loop termination](#loop-termination) below.

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `type` | `"loop"` | yes | — | |
| `name` | string | no | — | Optional; enables wrapper metadata recording and `collect` |
| `collect` | string | no | — | Name of a body effect; aggregates its `.value` across all iterations into `prime.<name>.collected.value`. Requires a named loop — `cof check` rejects `collect` on an unnamed loop, naming the fix (give the loop a `name`). |
| `flow` | `"chain"` \| `"tree"` | no | `chain` | `chain` = sequential (default). `tree` = all `each` iterations run in parallel via `ThreadPoolExecutor`. `while` loops always run sequentially. |
| `max_concurrency` | integer | no | `min(32, cpu_count + 4)` | Max parallel workers when `flow: tree` — `ThreadPoolExecutor`'s own default, not "every iteration at once": `each.in` has no default bound on collection size (see `max_iterations` below), unlike a `dynamic` tree's fixed, author-declared effect list, so an unset `max_concurrency` here is deliberately capped rather than spawning one thread per item. |
| `body` | array | yes | — | Non-empty list of effects to execute per iteration |
| `each` | object | one-of | — | Collection iteration; mutually exclusive with `while`. A loop with neither or both is an error |
| `each.in` | string | yes (each) | — | Root-relative state path to a JSON array — `input.`/`prime.`/`runtime.`-rooted, or a binding of an enclosing loop (`each.as`), e.g. `s.crops` inside a loop whose `each.as` is `s`. `input.*` is a first-class source; the array need not come from a `prompt_type: json` effect. `state.`-prefixed spellings and bare keys that name no binding in scope are hard errors here (`state.` is a CEL-only binding). |
| `each.as` | string | no | `item` | Variable name for current element in body templates *and* in `mode: cel` expressions inside this loop's own body — see [CEL Expressions](#cel-expressions) |
| `each.truncate` | bool | no | `false` | `false`: a collection longer than `max_iterations` fails the loop at start (see [Loop termination](#loop-termination)). `true`: process only the first `max_iterations` elements and record `termination: max_iterations_reached` plus `unvisited` instead. |
| `while` | object | one-of | — | Continuation condition; mutually exclusive with `each` |
| `while.mode` | string | no | `model` | `model` or `cel` |
| `while.template` | string | model only | — | LLM returns boolean for continuation decision. The runtime wraps it and appends `Should the loop continue? Answer (yes/no):`, so phrase the ask as yes/no. Parsed the same lenient way as `if.template`; raw reply recorded on `meta.answer` (plus `meta.adapter`/`meta.model`, `meta.tokens_sent`/`meta.tokens_received` for the last check and `meta.tokens_sent_total`/`meta.tokens_received_total` summed across every check this loop made) on each check |
| `while.expr` | string | cel only | — | CEL expression against state |
| `while.strict` | bool | no | `false` | cel only. When true, an unset `state.` path raises instead of making the expression `False` |
| `max_iterations` | integer | no | — (no cap) | Hard cap on iterations. Unset means the loop runs until its collection is exhausted (`each`) or its condition is false (`while`). For `each`, when set, the collection must not be longer than it unless `each.truncate: true` is set — see [Loop termination](#loop-termination). |
| `min_iterations` | integer | no | `0` | Minimum iterations to run before the condition is checked at all. A forced pass does not evaluate the condition and discard the answer — it never evaluates it. `while` loops only; on an `each` loop it has no effect and `cof check` warns. |
| `on_error` | string | no | `fail` | `fail`, `break`, `continue` |
| `labels` | object | no | — | Arbitrary metadata annotations, recorded on `meta.labels` (named only — an unnamed loop has no node to carry it) |

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
`state.iter.index + 1 < 3` caps a loop at 3 passes without a separate
`max_iterations`. `state.iter.count` is the same count with no `-1` offset
(`0` before the first pass, `N` once N have run); `state.iter.count < 3`
reads the same thing without the `+ 1` and is the recommended spelling for
new conditions.

#### Loop termination

Every completed named loop node writes `prime.<name>.value.termination.reason`,
one of:

| Reason | Modes | Meaning |
|---|---|---|
| `condition_false` | `while` | The condition evaluated false (after `min_iterations`) — the loop converged. |
| `collection_exhausted` | `each` | Every element was visited; nothing was cut short. |
| `max_iterations_reached` | `while`, `each` (with `each.truncate: true`) | The cap ended the loop, not the condition or the collection. `while` prints a `--verbose` warning line when this happens. An `each` loop also writes `termination.unvisited` — the count of elements it never got to. |
| `collection_unresolved` | `each` | `each.in` didn't resolve to an array (missing path, wrong type). See `meta.each_in_error`. |
| `condition_error` | `while` | The condition raised under `on_error: break`/`continue` — a broken condition can never become false, so the loop stops rather than spinning to `max_iterations`. |
| `error` | both | The loop (or a body effect under `on_error: fail`) raised, or an `each` loop's bounds check failed under `on_error: break`/`continue`. `termination.detail` and `meta.error` say why in every case. |

**`max_iterations` has no default — a loop runs until its collection or its
condition ends it.** A `while` loop with no `max_iterations` set runs until
its condition is false, however many passes that takes; set `max_iterations`
yourself if you want a floor against runaway feedback. An `each` loop with no
`max_iterations` set runs every item in its collection. When `max_iterations`
*is* set on an `each` loop, its bound is the collection: the length is known
before the first pass runs, so a collection longer than the cap is never a
runaway, it's under-provisioning. This **fails the loop at start** — before
any iteration executes — with a message naming both numbers (`collection has
144 items but max_iterations is 100`). Set `each.truncate: true` to opt into
processing just the first `max_iterations` elements; the node then records
`termination: max_iterations_reached` and `unvisited` instead of erroring.
This bounds check is gated by the loop's own `on_error` exactly like a
failed pass is: `break`/`continue` record `termination.reason: "error"` and
`meta.error` and let the run continue past the loop (0 iterations) instead
of raising; only `on_error: fail` (the default) stops the run.

#### Referencing a sibling within an iteration

A body step reading the step before it — compute → classify → score — is the
most common multi-step loop shape. Five *different* questions get five
*different* paths, and substituting one for another fails silently:

| You want | Write | Legal where |
|---|---|---|
| A step's output in the **current pass** | `{{prime.<step>.value}}` | inside the body, and inside a `while` condition |
| The **previous completed pass** | `{{prime.<loop>.prev.<step>.value}}` | inside the body only — chain flow (`each`/`while`); absent on the first pass |
| One **specific past pass** | `{{prime.<loop>.iter_<N>.<step>.value}}` | **after** the loop only |
| The **final pass** | `{{prime.<loop>.last.<step>.value}}` | **after** the loop only |
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
  the body, and only inside the body. After the loop, the name means the outer
  effect again.
- **Named and unnamed loops behave identically**, in `chain` and in `tree`
  flow. (A `tree` loop parallelises whole iterations, not the steps inside one —
  body steps still run in order and still chain.) Adding or removing a loop's
  `name:` never changes which spelling resolves.
- **An `if` branch is its own link in the same scope chain.** A step inside a
  `then`/`else` branch resolves the branch's own earlier steps first, then
  falls through to whatever this rule already says for the enclosing scope —
  a loop iteration's prior steps, an outer branch's, or root state. This
  holds for a named `if` too, and for one nested inside another.
- **The bare form `{{<step>.value}}` also works** and means the same node. It is
  accepted, not preferred: a bare name can collide with a user-supplied state
  key, and `prime.`-prefixed cannot.
- **`prev` is the previous *completed* pass, body-only.** Chain flow only —
  `each` and `while` both build it, `tree` never does, since tree passes run
  in parallel and there is no previous one; referencing it in a `flow: tree`
  body is a `cof check` error. Absent on the first pass — not an empty node
  — so a template renders it empty and CEL's `has()` reads false; `.meta` is
  there the same as `.value`. A nested loop's own `prev` is independent of
  any enclosing loop's.
- **`{{prime.<loop>.<step>.value}}` does not resolve, by design.** `prime.<loop>`
  is the loop's own node — it holds `iter_<N>`, `last`, `collected` and `meta`,
  never body step names. `cof validate` warns on it.
- **`iter_<N>` inside the body is a trap.** `N` is a constant, so it does not
  render empty — it renders *pass N's* output during every pass, which is
  plausible-looking stale data. `cof validate` warns on this too.
- **`last` is post-loop only, like `iter_<N>`.** It is written when the loop
  completes, so inside the body (or the `while` condition) it resolves to the
  previous pass at best — write `{{prime.<step>.value}}` for the current pass.
  `cof validate` warns on in-body use.
- In CEL the same forms apply with the `state.` prefix and no braces:
  `state.prime.<step>.value`.
- **A loop's `each.as` binding and its iteration index are also legal
  `state.<name>` reads in CEL, but only inside that loop's own body** — see
  [CEL Expressions](#cel-expressions).

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

A reflector is the effect most worth switching off for a single run: a profile
with `effects.goal.enabled: false` turns agentic planning off while leaving the
rest of the orchestration intact — see [Disabling Effects](#disabling-effects).

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
| `params` | object | no | `{}` | Plugin-specific parameters. All string values support Mustache rendering. A leaf anywhere in `params` (any depth, in objects and lists), `{from: <path>}`, passes the value at that path unchanged instead of rendering it (see [Params by reference](#params-by-reference)). Takes precedence over top-level `prompt`/`model`. Quote every `args` entry: an unquoted `0x1`, `off` or `-0` reaches the tool as `1`, `False`, `0` (`cof check` warns) |
| `params_json` | string | no | — | A Mustache template rendered to text and parsed as JSON, producing a real array/object instead of a Mustache-rendered string. Deep-merged over `params` (wins on overlapping keys). See [`params_json`](#params_json) below |
| `timeout_ms` | integer | no | — | Per-effect timeout in milliseconds |
| `on_error` | string | no | `fail` | `fail`, `skip`, `continue` |
| `description` | string | no | — | |

#### Params by reference

A string `params` value is Mustache-rendered, so it always reaches the plugin as
text (unless `params_json` builds it from JSON, see below). To pass a value
as it is — an array, an object, a number, a boolean — anywhere inside
`params` (any depth, in objects and lists), write `{from: <path>}`:

```yaml
- type: tool
  name: get_equity_quotes
  provider: mcp
  params:
    server: robinhood
    tool: get_equity_quotes
    arguments:
      symbols: {from: prime.symbol_list.value}      # a native array, not a string
```

This resolves exactly like a `use` effect's by-reference `inputs` (see
[Inputs by reference](#inputs-by-reference)): the same scope — state
namespaces and the bindings of an enclosing loop (`each.as`, `iter`,
`prime.<loop>.prev`) — and the same compile-time rooting check at `cof check`
time.

- A path that doesn't resolve is an error naming the param's own path
  (`Tool effect '<name>' param '<path>': '{from: ...}' did not resolve to a
  value.`), unless the leaf also carries `default:` — then that value is used
  instead:
  ```yaml
  params:
    symbols: {from: prime.symbol_list.value, default: []}
  ```
  A path that resolves to an explicit `null` counts as unresolved too — there
  is no way to tell "missing" from "stored null" — so it also raises (or
  falls back to `default:`), unlike a `use` effect's input reference, which
  passes `null` through as the value.
- Only a mapping with exactly the key `from` (optionally plus `default`) is a
  reference. Any other mapping — including one with other keys mixed in — is
  passed through literally, same as today. To pass a *literal* one-key
  `{from: ...}` object to a plugin, use `params_json` instead.
- A security-sensitive param a plugin treats as its own allowlist (today:
  the `shell` plugin's `allowed_commands`) must stay a literal list of
  strings written in the document: `{from: ...}` there — as the whole value
  or as a list item — is rejected at `cof check` time and again at run time,
  the same as a templated string entry, so neither runtime state nor
  `params_json` can widen what a document is allowed to run.

Tool providers reference a *tool plugin*, not an *adapter*, so the `prompt`-effect `on_error` reclassification above does not apply here: a missing tool-plugin dependency (e.g. `ffmpeg` not on `PATH`) always hard-fails preflight regardless of this effect's `on_error`.

**Timeout:** `timeout_ms` on the effect wins, rounded up to the next whole second — a sub-second budget (e.g. `250`) never floors to a 0-second timeout, which some plugins would treat as "fail instantly" and others (curl-based ones) as "no limit at all". Unset, the tool's own default comes from `runtime.tools.timeout_seconds` in config (300s if that's unset too) — independent of `runtime.adapters.<name>.timeout_seconds`, the LLM adapter's socket timeout; a tool-only run, or one on the no-op adapter, still gets a real budget. See [Timeouts](./guidebook/04-configuration.md#timeouts).

Not every plugin can actually be bounded by it:

| How a plugin honors it | Plugins |
|---|---|
| Subprocess timeout (`subprocess.run(timeout=...)` or curl `--max-time`) | Every binary-wrapping plugin: `ffmpeg`, `shell`, `git`, `gh`, `ripgrep`, `sed`, `awk`, `pandoc`, `imagemagick`, `exiftool`, `gpg`, `docker`, `kubectl`, and the rest of that family. `comfyui` and `web_search`/`weather` apply it to their own curl calls |
| Own network call | `web_fetch` (its own `params.timeout_ms`, when set, shortens it for that call — never extends past the effect's budget), `rss`, `wikipedia` (retries disabled so it can't multiply the budget), `whois`, `http`, `slack`, `notion`, `jira`, `github`, `gdrive`, `gcalendar`, `s3`, `linear`, `email_smtp`, `webhook`, `dns`, `mcp_client`, `playwright`, `screenshot`, `discord` (via a background thread with a deadline, since discord.py's webhook send has no timeout parameter of its own). These apply the timeout per socket operation (connect, each read, etc.), not to the whole call. |
| Sandboxed child process, killed on overrun | `python_eval` |
| Ignored — pure in-memory, nothing to bound | `json`, `xml`, `csv`, `regex`, `hash`, `hex`, `uuid`, `base64`, `gzip`, `zip`, `tar`, `fs`, `env_vars`, `validate_yaml`, `html_extract`, `pdf_extract`, `math`, `clock`, `system_info`, `process_list` |
| Ignored — has its own, separate bound instead | `port_check` (`params.timeout_ms`, socket-level, default 2s), `surrealdb` (the SDK's own socket timeout), `embed`/`rerank`/`vector_search` (local inference; the first call per model can also trigger an unbounded download) |

**Result contract — one meaning for "this tool failed":** a tool effect
fails (`meta.error` set, `on_error` applies) exactly when the plugin raises,
or when it returns a result with `ok: false` without raising. Both paths
are equivalent; `on_error: fail` (the default) re-raises either way,
`skip`/`continue` record the error and leave `value: null`.

- **`meta.exit_code`** means a process exit code, and only that. For the
  plugins that wrap a binary/subprocess (`ffmpeg`, `shell`, `git`, `gh`,
  `ripgrep`, and the rest of that family) it's that process's exit code,
  and a non-zero exit fails the step unless `allow_nonzero: true`. Every
  other plugin — including the HTTP-family, `mcp_client`, and the
  soft-outcome ones below — always leaves it `None`: it is never an HTTP
  status or a soft 0/1/2 flag.
- **HTTP-family plugins** (`http`, `web_fetch`, `webhook`, `linear`) fail
  (`ok: false`) on a 4xx/5xx response by default, with the status recorded
  on **`meta.status_code`**. The failure message (`meta.error`, and
  `meta.stderr`) includes a bounded, redacted excerpt of the response body
  when one is available — the JSON `error`/`message`/`detail` field, or
  the first ~500 characters otherwise — so a validation reason a provider
  sent back is visible without having to read `meta.raw.body` separately.
  Each has a `fail_on_error: false` param that restores the old
  always-succeeds behaviour — the response still lands on
  `value`/`meta.status_code`, it just doesn't fail the effect.
- **Soft-outcome plugins** (`wikipedia`, `dns`, `port_check`,
  `validate_yaml`) keep `ok: true` regardless of outcome and report it on
  their own fields instead: a missing Wikipedia page or an NXDOMAIN lookup
  is `value: null`/`[]` plus a `raw` field naming why; a closed port is
  `value: false`; an invalid document from `validate_yaml` is
  `value.ok: false` (the document's validity, not the tool call's). None of
  these are tool failures — check the field, not `on_error`.
- **`meta.raw`** is the plugin's own `ToolResult.raw` (the fields each
  plugin's own docs describe, e.g. `http`'s `raw.headers`, `web_fetch`'s
  `content_type`), redacted (`cli.redaction.redact` — credential-like keys,
  JWT/API-key-shaped strings, URL userinfo) and capped at 64 KiB serialized;
  an oversized `raw` is replaced with a `{_truncated, _original_bytes,
  _preview}` marker. Set for every tool effect that returned a result.
  **Prompt effects never set `meta.raw`** — only tool effects do.
- **`meta.params_rendered`** (the effect's rendered `params`, merged with
  any `params_json`) is redacted the same way before storage; the plugin
  itself still receives the real, unredacted values.

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
rather than a plain, written-down value — only a literal list counts. See
[Tools and persistence](guidebook/13-tools-and-persistence.md).

---

### `use`

Runs another orchestration as an isolated sub-step. State is fully isolated: declared `inputs` are passed in as initial state; the parent does not see the child's working state directly. Outputs land at `prime.<name>` according to the namespacing mode (see below).

Isolated state, shared observation: the child's effects are reported to the parent run's observers — `--live-state`, the TUI, and every runtime plugin's `on_effect_start` / `on_effect_complete` — at paths namespaced under the use node (`prime.<name>.<child_effect>`, nesting further for a `use` inside a `use`). Live snapshots mirror the child's in-flight effects under that node for watchers only; what actually lands in parent state is still exactly what the namespacing mode below says.

Config inheritance: the child executes with the exact same resolved `runtime.*` config as the parent run — adapters, tool plugins (including MCP servers), complexity settings — never re-resolved from disk. The child's own `runtime:` and `plugins:` keys are not read at all, so a composed child can change even less than a root document can. See point 30 under "Composition via `use`" below for the one case (a parent-level `runtime:` block of its own) where that can still surprise you.

**State output path:** `prime.<name>.value` (declared-outputs mode) or `prime.<name>.<child_effect>.value` (full-namespace mode)

**`meta.child_errors`:** `null` when nothing was swallowed, otherwise a list of `{path, error}` for every effect anywhere in the child's tree whose own `on_error: skip`/`continue` absorbed a failure — see point 31 below.

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `type` | `"use"` | yes | — | |
| `name` | string | yes | — | Pattern `^[A-Za-z_][A-Za-z0-9_]*$` |
| `ref` | string | * | — | Library lookup across configured sources. Bare (`utilities/critique`) or source-qualified (`hub:utilities/critique`) |
| `path` | string | * | — | Filesystem path (absolute, cwd-relative, or parent-orchestration-relative) |
| `inline` | string | * | — | Mustache template that renders to orchestration YAML at runtime |
| `orchestration` | string | * | — | **DEPRECATED** — use `ref` or `path` instead. Still accepted; emits `DeprecationWarning` |
| `validate` | bool | no | `true` | Check the child — `inline`, `path` or `ref` — the way `cof check` checks a file (schema, near-miss keys) before it runs. `false` skips that; a repeated key is still an error |
| `inputs` | object | no | `{}` | Map of name → value passed to child as initial state. String values are Mustache-rendered; `{from: <path>}` passes the value at that path unchanged (see [Inputs by reference](#inputs-by-reference)) |
| `outputs` | object | no | — | Declared outputs — see [Outputs](#outputs). When present, switches to declared-outputs mode |
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

#### Inputs by reference

A string input is Mustache-rendered, so it always reaches the child as text. To hand the
child a value as it is (an array, an object, a number, a boolean) write `{from: <path>}`:

```yaml
- type: loop
  name: ladder
  each: {in: input.rungs, as: r}
  body:
    - type: use
      name: rung
      path: parts/rung.yml
      inputs:
        rung: {from: r}                              # the loop item, as an object
        methods: {from: prime.render.value.methods}  # an array
        base: "{{input.base}}"                       # a string, rendered as before
```

- The path is rooted at `input.`, `prime.` or `runtime.` (like `each.in` and `outputs.path`), or at
  a binding of an enclosing loop (`each.as`, `iter`), with dotted keys and integer list indices
  after it (`input.rungs.1.name`). Any other root is an error at `cof check` time.
- The child gets a deep copy: nothing it does can reach the parent's state.
- A path that resolves to nothing passes `null`; if the child's interface marks that input
  `required`, the `use` fails with the path in the message.
- Only a mapping with the single key `from` is a reference. Any other mapping is a literal value.

#### Complete record (opt-in)

In declared-outputs mode only the declared values land at `prime.<name>.value`; what the
child did (its commands, answers, decisions) is visible in `--live-state` while it runs and
then dropped. Set `runtime.state.record_children: true` (in config, or in the orchestration's
own `runtime:` block) to keep it: each `use` node keeps its child's effects beside its
`value` and `meta`, in the same shape live state shows them, at every depth of `use`, and a
failed child keeps whatever it got to. The run's `--out` file then holds the whole run.

```yaml
runtime:
  state:
    record_children: true
```

The record is for reading after the run, not for wiring: downstream effects still read a
`use`'s declared outputs.

Every `use` node also records, whether or not the setting is on:

| Field | Meaning |
|-------|---------|
| `meta.inputs` | What the child received: rendered strings, referenced values, literals |
| `meta.orchestration_sha256` | SHA-256 of the child's YAML text (the file's bytes, or the rendered inline YAML) |

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
    score: {path: prime.score.value, type: number}
    notes: {path: prime.notes.value, type: string}
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

## Outputs

`use.outputs` and `interface.outputs` declare the same thing — *expose this state path under this name* — and take the same shape. One canonical form covers both:

```yaml
outputs:
  summary: {path: prime.summarize.value, type: string}
  score:   {path: prime.rate.value, type: number, description: 0–10 quality score}
```

| Key | Required | Meaning |
|-----|----------|---------|
| `path` | **yes** | Dot-delimited state path in the orchestration that produces the value |
| `type` | no | `string`, `number`, `boolean`, `array`, `object` — declaration metadata |
| `description` | no | Prose for humans and for library listings |

**Also accepted:** a bare state-path string is shorthand for `{path: <string>}`, and works in both contexts:

```yaml
outputs:
  summary: prime.summarize.value      # identical to {path: prime.summarize.value}
```

Both spellings compile to the same `name → path` mapping and always have — the shorthand is sugar, not a second syntax. Write the object form; it is what the docs, the rules files, and the curation library use, and it is the one that has somewhere to put `type`.

### Interface

An orchestration declares its contract with a top-level `interface:` block, and it is enforced the same way everywhere the document runs: as a `use` effect's child, and at the top level — `cof run`, the SDK's `run_orchestration`, the REST trigger, MCP's `run_orchestration`, and the scheduler all share one enforcement path.

```yaml
interface:
  inputs:
    article: {type: string, required: true, description: Text to summarize.}
    style: {type: string, default: neutral, description: Tone for the summary.}
  outputs:
    summary: {path: prime.summarize.value, type: string}
```

| Key | Required | Meaning |
|-----|----------|---------|
| `type` | no | `string`, `number`, `integer`, `boolean`, `array`, `object` — checked, and used to coerce a CLI `-e` value or a `use` input rendered through Mustache (both cross as text) to the declared type. `cof check` also checks a `default:` against this — see [What `cof check` and `cof run` reject](#what-cof-check-and-cof-run-reject) |
| `required` | no | Fails the run before anything executes if this input is missing. Default `false` |
| `default` | no | Value used when the caller omits this input. Any JSON type; should match `type` when both are given |
| `description` | no | Prose for humans and for library listings |

An input `interface.inputs` doesn't declare is never rejected — undeclared extra inputs always pass through. A missing `--state` file is a hard error (`state file not found: ...`), not empty input.

An explicit `outputs:` on the `use` effect takes precedence over the child's `interface.outputs`.

---

## Deprecated spellings

Circuitry's parser is deliberately forgiving: every spelling below still parses and runs, and none of them will ever fail validation. They are simply not what the language teaches — one construct, one spelling, everywhere in the docs, the rules files, and the curation library.

| Deprecated | Canonical | Where |
|------------|-----------|-------|
| `type: conditional` | `type: if` | Effect type |
| `flow: chain_of_thought`, `flow: cot` | `flow: chain` | Any `flow` field |
| `flow: tree_of_thought`, `flow: tot` | `flow: tree` | Any `flow` field |
| `orchestration:` on a `use` | `ref:` or `path:` | `use` effect |

`cof validate` (and `cof check`, the TUI Validate view, and the MCP `validate_orchestration` tool) reports these as **warnings**. Warnings never change the exit code — a file full of them is still a valid file.

The same command warns when an effect is **named after an effect type** (`use`, `loop`, `if`, `dynamic`, `prompt`, `tool`, `reflector`). Name effects after the job they do — `summarize_article`, not `prompt`. Type-keyword names are generic, which is exactly why two of them end up siblings in one scope and collide.

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
cof run agent.yml --profile no-planning
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

The per-effect `on_effect_start` / `on_effect_complete` hooks still fire for
the node, so observability sees the skip rather than a gap.

**Rules:**

| Situation | Behavior |
|-----------|----------|
| Disabled container (`dynamic` / `loop` / `if` / `reflector`) | The whole subtree is disabled — the container's node is written as disabled and nothing inside it runs (no child nodes appear at all) |
| Disabled `loop` body effect | Skipped in every iteration; `prime.<loop>.iter_<n>.<name>` is a skip node |
| Disabled `loop` `collect` target | Each iteration's slot is a skip node, and `prime.<loop>.collected.value` omits it — `collected` reports only values that were actually produced |
| Disabled effect inside an `if`/`else` branch | Skipped when that branch is selected; the branch's other effects run normally |
| A container's *condition* (`if:` on an `if` effect, `while:` on a loop) | Not disableable — it is a condition, not an effect, and a container with no condition has no defined branch. Targeting `<name>.if`, `<name>.while`, or `<name>.condition` fails profile validation with an actionable error. Disable the whole container instead |
| Unknown effect path | Fails profile validation, listing the orchestration's valid paths |
| Anonymous (transparent) `if`/`loop` | Contributes no path segment, so it has no address to disable — disable its individual named children instead |

**Downstream coherence.** A disabled node's `value` is `null`, so:

- Mustache templates referencing it render empty:
  `"report on <{{prime.goal.value}}>"` → `"report on <>"`
- CEL conditions that read it evaluate `False` — a `state.` path that is unset
  (missing, or resolving through `null`) makes the whole expression `False` by
  rule, before evaluation, and logs a warning naming the path. That covers
  comparisons (`state.prime.goal.value == "yes"`), sizes
  (`size(state.prime.goal.value) > 0`) and negative tests
  (`state.prime.goal.value != ""`) alike: nothing there is never a satisfied
  condition.

A run with no `--profile` is unaffected: `enabled` is never set from
orchestration YAML, so every effect compiles as enabled.

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
| Loop iteration index | `{{_loop_index}}` | Zero-based index, available in both `each` and `while` loop bodies |

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

#### The language

Expressions are evaluated by [cel-python](https://pypi.org/project/cel-python/),
a real [CEL](https://github.com/google/cel-spec) implementation, so the whole
language is available — not a subset:

| Construct | Example |
|-----------|---------|
| Comparison | `state.prime.score.value >= 0.8` |
| Boolean logic, negation | `state.input.a && !state.input.b`, `state.input.a \|\| state.input.b` |
| Ternary | `state.input.tier == 'pro' ? 10 : 1` |
| Field presence | `has(state.prime.summary.value)` |
| Comprehension macros | `state.input.items.all(i, i.score > 0)`, `.exists(...)`, `.exists_one(...)`, `.map(...)`, `.filter(...)` |
| Standard functions | `size(...)`, `int(...)`, `string(...)`, `double(...)`, `matches(...)` |
| String methods | `state.input.url.startsWith('https://')`, `.contains(...)`, `.endsWith(...)` |
| Membership | `'admin' in state.input.roles` |
| List / map literals | `state.input.role in ['admin', 'owner']` |

Two CEL rules that surprise people coming from Python:

- **Equality is typed.** `1 == true` is `false`, and `'1' == 1` is `false`. Only
  `int`/`uint`/`double` compare across types (`1 == 1.0` is `true`).
- **`&&` / `||` / `!`, not `and` / `or` / `not`.** Python spellings do not parse.

Expressions are capped at 4096 characters and parsed once, then cached — a
loop's `while` expression pays the parser cost on its first iteration only.

Only `state` is in scope. Values are converted into CEL's type system on the
way in, so an expression cannot reach a Python object's attributes, methods or
class; anything with no CEL counterpart reads as `null`.

#### CEL scoping: what `state.<key>` may name, at each nesting level

`state.<key>` is a hard error at compile time (`cof check`) unless `<key>`
resolves to something actually in scope for the expression's exact position
in the effect tree:

| Nesting level | Legal `state.<key>` roots |
|---|---|
| Anywhere | `input`, `prime`, `runtime` |
| Inside a loop's own `body` (`each` or `while`), or a `while` loop's own `expr` | + `iter` — loop metadata, currently `iter.index` (0-based) and, in a `while` loop's own `expr` only, also `iter.count`. In `each.body`/`while.body`, `iter.index` is the current pass's own position. In `while.expr`, `iter.index` is the index of the *last finished* pass instead (-1 before the first pass, N - 1 after N passes have run — a pass failed under `on_error: continue` still counts), and `iter.count` is the same count without the offset (0 before the first pass, N after N have run) |
| Inside an `each` loop's own `body` | + the loop's `each.as` name, bound to the current element |

These loop-scoped names stack with nesting and are visible to *every*
`mode: cel` expression inside that body — a body `if`, a nested loop's
`while`, a conditional several containers deep — the same way Mustache's
`{{<as_name>}}` already resolves through nested `if`/`dynamic` wrappers.
They go out of scope the moment the loop returns: an expression after the
loop, or in a sibling loop, gets the same "not a state namespace" error as
any other undeclared key.

```yaml
- type: loop
  name: filter_scores
  collect: verdict
  each: {in: input.scores, as: score}
  body:
    - type: if
      if:
        mode: cel
        expr: "state.score >= 50"          # the loop's own `as` binding
      then:
        - type: prompt
          name: verdict
          template: "{{score}} at index {{_loop_index}} passes."
      else:
        - type: prompt
          name: verdict
          template: "{{score}} at index {{_loop_index}} fails."
```

**Nested loops shadow by depth, same as templates.** If an inner loop reuses
an outer loop's `as` name, `state.<name>` inside the inner body reads the
inner binding; back in the outer body, after the inner loop returns, the
same spelling reads the outer binding again. `iter.index` shadows the same
way — it always means *this* loop's own counter, never an ancestor's.

A name no enclosing loop declared is still a hard error, and the message
names what actually is in scope:

```
CEL expression at 'prime.effects[0].body[0]': 'state.other' does not name a
state namespace ('state' binds to the state root). Write
'state.input.other' for caller-supplied values or 'state.prime.other' for
effect outputs. Names bound by an enclosing loop here: iter, score.
```

See `learn/cel_showcase.yml`'s `filter_scores` loop for a runnable example.

#### What fails where

`cof check` parses every `mode: cel` expression at compile time and rejects
what is not CEL, naming the effect and the expression:

```
CEL expression at 'prime.effects[0]' (effect 'gate'): expression does not parse
(not state.prime.x.value
 ^). Expression: 'not state.prime.x.value'
```

At run time an expression that cannot be evaluated — a bad `size()` argument,
an unknown function — **errors the effect** (`meta.error`) and fails the run,
the same as a failing tool effect. It never quietly takes the `else` branch.
Set `on_error: continue` on the conditional to opt into the else-branch
fallback explicitly; the error is still recorded on `meta.error`.

Reading *unset* state is not an error: see
[Disabling Effects](#disabling-effects) — an unset path makes
the expression `False` and logs a warning naming the path.

#### `strict: true` — when absent state must not pick a branch

The default is deliberate: a disabled node or an effect that has not run yet
reads as `False`, matching the way a template referencing one renders empty.
For a condition where a missing field silently choosing a branch would be
unsafe — an order-exit rule, a safety gate — set `strict: true` and an
unresolved path raises instead:

```yaml
- type: if
  name: exit_gate
  if:
    mode: cel
    strict: true
    expr: "state.prime.tick.value.price <= state.input.stop_price"
  then:
    - type: tool
      name: close_position
      tool: webhook
```

Without `strict`, a `tick` that failed to produce a `price` would evaluate the
whole condition to `False` and take the *no-exit* branch. With it, the effect
errors and `on_error` decides what happens next.

`strict` applies to `mode: cel` only, on both `if:` and a loop's `while:`.

A path passed to `has()` is exempt from the absent-state rule anywhere in the
expression — that is what `has()` is for:

```yaml
expr: "has(state.prime.score.value) && state.prime.score.value > 0.8"
```

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

When you mean "the final pass" — the usual case after a `while` loop or a
data-dependent `each` loop, where `N` is unknowable — don't guess `N`:

```yaml
template: "Final result: {{prime.explain.last.summary.value}}"
```

`last` is the last *completed* iteration's node, same shape as `iter_<N>` (deep
field paths work). A pass that errored under `on_error: continue`/`break` is
skipped in favor of the last one that finished; a loop that ran zero iterations
writes no `last` key, so the read renders empty exactly like a missing
`iter_<N>`.

`last` is not a second copy of that pass. During a run it *is* the `iter_<N>`
node, and saved state (`--out`, `--print`, the `--live-state` mirror) writes it
as a reference to the sibling key it names, so each loop's final pass is on
disk once:

```json
"explain": {
  "iter_0": { "summary": { "value": "…" } },
  "iter_1": { "summary": { "value": "…" } },
  "last": { "$ref": "iter_1" }
}
```

A tool reading a saved state file follows the reference itself
(`node[node["last"]["$ref"]]`). Circuitry does it for you wherever saved state
comes back in — a previous run's state passed with `--state`, the TUI's Runs
view — so `{{prime.<loop>.last.<step>.value}}` reads through it exactly as it
does in the run that wrote it. State files written before the reference form
hold `last` as a full copy of the pass; they still load as they always did.

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

### Loop `each.in` Must Be Root-Relative and Resolve to a JSON Array

`each.in` must be rooted at one of the three state namespaces —
`input.`/`prime.`/`runtime.` — or at a binding of an enclosing loop
(its `each.as` name), and must resolve to an array at runtime.
`input.*` is a first-class source, so the array does not have to come from a
`prompt_type: json` effect; a caller-supplied array works directly:

```yaml
# Good: caller-supplied array, no prompt needed
- type: loop
  each:
    in: input.topics   # caller passed an array under this input
    as: topic
  body: [...]

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

# Good: an enclosing loop's binding — a nested loop over a field of the outer item
- type: loop
  each: {in: input.sets, as: s}
  body:
    - type: loop
      each: {in: s.crops, as: c}   # s is the enclosing loop's binding
      body: [...]

# Bad: not root-relative — both are hard errors from `cof check`
- type: loop
  each:
    in: topics               # bare key, no enclosing loop binds it — write input.topics or prime.topics.value
  body: [...]

- type: loop
  each:
    in: state.prime.topics.value   # state. is a CEL-only binding, not legal here
  body: [...]
```

---

## LLM Authoring Rules

The following rules are sufficient for generating structurally correct Circuitry orchestration YAML. Apply all of them exactly.

**File structure:**
1. Top-level fields: `adapter` (string), `model` (string), `effects` (array). Only `effects` is required. `description`, `version`, `interface`, `flow`, `runtime` and `plugins` are the other keys the top level knows; anything else is ignored with a warning. A top-level `runtime:` block should set only `complexity` and `state`; never put `adapters`, `plugins`, `persistence`, `library` or credentials in it — those are host settings that belong in config.json. A document run by library name, fetched or generated has its copy ignored with a warning; a file run by path applies it with a notice.
2. `adapter` and `model` are only required when the orchestration contains `prompt` or `reflector` effects. Tool-only orchestrations (`type: tool` effects only) do not need `adapter` or `model`.
3. Valid `adapter` values: any name in the compiled-in adapter registry (`cof list --extensions`) — e.g. `ollama`, `openai`, `anthropic`, `litellm`, `cyberdiner`, `host_claude`. Two need special handling. `cyberdiner` is a job-queue broker: `model:` must be a capability tier (`cheap`, `fast-cheap`, `fast`, `good-cheap`, `good`, `good-fast`, `alpha` — the network owns the list), not a provider model name, and `runtime.adapters.cyberdiner.expo_url` / `token` must be set in config — never in the YAML. `host_claude` is MCP-only (the host Claude session generates each prompt — set via `circuitry-mcp` rather than config.json. By default rejects non-Claude `model:` pins; pass `override_model=True` to `run_orchestration` to ignore the pin and run through Claude regardless).
4. Valid `flow` values: `chain` (sequential) and `tree` (parallel). Write nothing else — `chain_of_thought`/`cot` and `tree_of_thought`/`tot` still parse but are deprecated and warned about.

**Effect types and required fields:**
5. Valid `type` values: `prompt`, `dynamic`, `if`, `loop`, `reflector`, `tool`, `use`. (`conditional` still parses as an alias of `if` but is deprecated and warned about — always write `if`.)
6. `prompt`: requires `name` and exactly one of `template` or `messages`. Optional: `prompt_type` (default `text`), `schema` (required when `prompt_type: json`). Do NOT use `prompt_type: image` — use `type: tool, provider: comfyui` for image generation.
7. `dynamic`: requires `name`, `effects` (non-empty array), optional `flow` (default `chain`).
8. `if`: requires `if` (condition object) and `then` (array). `name` is optional. `else` is optional.
9. `loop`: requires `body` (non-empty array) and exactly one of `each` or `while`. `name` is optional.
10. `reflector`: requires `name` and `effects` (non-empty array).
11. `tool`: requires `name` and `provider`. Tool effects are for non-LLM side-effects only — generating images, processing video/audio, file conversion. Never use a tool effect for text summarization, analysis, writing, coding, or data extraction — those are `prompt` effects. Supported providers: `ffmpeg` (requires `params.input` and `params.output`), `comfyui` (requires `prompt` and `model` as top-level fields; `params` for sampler settings). Top-level `prompt` supports Mustache rendering. All string values in `params` also support Mustache rendering.

**Naming:**
12. All `name` values must match `^[A-Za-z_][A-Za-z0-9_]*$` — letters, digits, underscores; must start with letter or underscore; no spaces or dots. The pattern `iter_<N>` (e.g. `iter_0`) is reserved and must not be used as a name. `value`, `meta`, `input`, `prime` and `runtime` are also reserved — each collides with a structural slot the runtime itself writes.
13. Effect names must be unique among siblings within the same scope.
13a. Never name an effect after an effect type (`use`, `loop`, `if`, `dynamic`, `prompt`, `tool`, `reflector`). Name it after the job it does — `summarize_article`, not `prompt`. Generic names are what make two siblings collide; validation warns on them.
13b. Write each key at most once, and only the keys the effect type lists. A repeated key, or a near miss of a known key (`whlie`, `max_iteration`, `adapter` on a prompt where `provider` belongs), fails validation; any other unknown key is ignored with a warning. Quote every tool `params.args` entry — unquoted, YAML reads `0x1` as `1`, `off` as `false`, `-0` as `0`.

**State path addressing:**
14. In templates (Mustache): use `{{input.<name>}}` for caller-supplied input (never bare `{{key}}` — that is a hard error when `key` matches a declared `interface.inputs` name); use `{{prime.<name>.value}}` for top-level effect outputs; use `{{prime.<dynamic_name>.<child_name>.value}}` for outputs nested inside a dynamic.
15. In CEL expressions (`if.expr`, `while.expr`): always use the full prefix `state.prime.<name>.value`. Never omit `state.`.
16. Loop `each.in` must be a root-relative path to a JSON array — `input.<name>`, `prime.<name>.value`, or a `runtime.` path — or a binding of an enclosing loop (its `each.as` name), e.g. `s.crops` inside a loop whose `each.as` is `s`. `input.*` is a first-class source; it need not point to a `prompt_type: json` effect. Bare keys that name no binding in scope and `state.`-prefixed spellings are hard errors here.

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
27a. **Write outputs as objects** — `summary: {path: prime.summarize.value, type: string}`, in `use.outputs` and `interface.outputs` alike. The bare-string form (`summary: prime.summarize.value`) is accepted in both places and means the same thing, but the object form is the one to write.
28. **Declare an `interface:` block on reusable utilities** — typed `inputs` (with `required: true`) get validated automatically; typed `outputs` with `path:` auto-generate the caller's output mapping so callers don't have to repeat dot-paths. The curation library at `src/circuitry/curation/utilities/` is the canonical exemplar.
29. **Don't form `use:` cycles.** A→B→A is detected at validate time and at runtime. If two utilities legitimately need to call each other, factor out the shared logic into a third utility they both call.
30. **A `use:` child always executes with the exact same resolved runtime config as the parent run** — the merged `runtime.*` block (adapters, tool plugins, MCP servers, complexity settings, ...) is never re-resolved from disk for a composed child, at any nesting depth. A server or plugin declared in the user-level `~/.config/circuitry/config.json` is just as visible to a `use:`-composed child as it is to the same orchestration run standalone. The child's own `runtime:` and `plugins:` keys are never read. The one place this can still surprise you: if the *top-level* orchestration itself declares `runtime.complexity` or `runtime.state`, that block replaces the config-level one wholesale (see [Complexity Configuration](./complexity-config.md)) — restate every field you still need rather than partially overriding it. Any other `runtime:` key in the document is ignored with a warning (see [File Structure](#file-structure)).
31. **A child effect's own `on_error: skip`/`continue` doesn't hide the failure from the parent.** The `use` node's `meta.child_errors` (`null` when the child was fully healthy) lists every effect anywhere in the child's tree — at any nesting depth, including through further `use:` effects — whose own `meta.error` was swallowed by its `on_error`, as `{path, error}` pairs relative to the child's root. Check it after a composed run whenever the mapped output alone ("ok: false") isn't enough to tell a real failure apart from an intentionally degraded result.
