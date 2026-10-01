# Prompt

A prompt in Circuitry is not a function call — input in, output out. It is a *computation that returns a value alongside a transformed context*. The value is the model's response. The context is everything around it: which adapter answered, how many tokens it cost, whether it errored, what the rendered prompt actually was. In the vocabulary of the state monad, `f(state) → (state', y)` — the prompt is `unit`, the smallest thing that can be lifted into the run.

That framing is the whole design in miniature. Because a prompt returns *state and value together*, two prompts can be chained without any glue: the second reads what the first wrote. Because the value lands at a path derived from the effect's name, the chaining is deterministic. Everything else in the language — loops, conditionals, planners — is built by reusing this one shape at larger scales.

## The shape

```
Prompt ::= { type: 'prompt', name: NAME,
             template: TEMPLATE ⊕ messages: Message+,
             prompt_type?: 'text'|'json'|'object'|'array'|'number'|'boolean',   — default text
             schema: JSONSCHEMA,                        — REQUIRED iff prompt_type ∈ {json, object, array}
             model?: STRING, provider?: STRING, provider_fallbacks?: STRING*,
             params?: MAP, inputs?: MAP,
             assets?: Asset*, retries?: Retry,
             timeout_ms?: INT, deterministic?: BOOL,
             on_error?: 'fail'|'skip'|'continue', description?: STRING }
```

Two fields are required: `name` and exactly one of `template` or `messages`. Everything else is a refinement.

## The minimal prompt

```yaml
- type: prompt
  name: kind
  template: "Is this issue a bug, a feature or a question? Answer with the one word, nothing else. Issue: {{input.issue}}"
```

Run it, and one node appears in state:

```
prime.kind.value    # the model's answer
prime.kind.meta     # adapter, model, model_reason, prompt_type, prompt_sent,
                    # tokens_sent, tokens_received, error, fallback_attempts,
                    # created_at, completed_at
```

`value` is what the prompt is *for*. `meta` is the transformed context — the second half of the monadic return, and the reason a Circuitry run is a decision record rather than a transcript. `prompt_sent` in particular is the rendered text the adapter actually received, after every `{{…}}` was interpolated; when a run goes green but the output looks wrong, read it before anything else.

## Templates and messages

`template` is a [Mustache](https://mustache.github.io/) string. Anything in double braces is looked up in the shadow state: `{{input.issue}}` reads a caller-supplied value, `{{prime.kind.value}}` reads an earlier effect's output. Triple-stache `{{{…}}}` skips HTML escaping, which matters when you interpolate code or markup. [Shadow state](03-state.md) covers the full addressing rules; the short version is that every reference starts with `input.`, `prime.`, or `runtime.`.

`messages` is the role-based alternative for models that expect a conversation:

```yaml
- type: prompt
  name: is_bug
  prompt_type: boolean
  messages:
    - role: system
      content: "You triage issues for a Python library. Reply with only true or false."
    - role: user
      content: "Does this issue report a bug? {{input.issue}}"
```

Each `content` is a template in its own right. The turns reach the model as a real conversation — the system message in the provider's system slot, then the user and assistant turns in order — on ollama, the OpenAI-compatible adapters and anthropic. An adapter that cannot take turns gets them flattened into `role: content` lines and says so in `meta.warnings`. Either way `meta.prompt_sent` shows the flattened form.

The two forms are mutually exclusive — a prompt with both fails validation:

```yaml
# ✗ template and messages are alternatives, never companions
- type: prompt
  name: kind
  template: "Classify this issue."
  messages:
    - role: user
      content: "Classify this issue."
```

## Typed output

`prompt_type` declares the shape of the value the runtime should extract from the model's reply, and the runtime enforces it. Six types:

| `prompt_type` | `value` is | Notes |
| --- | --- | --- |
| `text` | string | The default. The reply, as-is. |
| `boolean` | `true` / `false` | Lenient: `Yes.`, `yes, because …`, `**TRUE**`, `Y` parse as true, `No.`/`false!`/`N` as false — leading/trailing wrapping (quotes, markdown, punctuation) is stripped first. An answer that still doesn't read as yes/no raises rather than becoming `null`. |
| `number` | number | Lenient: `42`, `42.`, `3.5`, `-1`, `1e3` all parse. A reply with extra words attached (`about 42`, `42 degrees`) raises instead of guessing which number was meant. |
| `json` | parsed JSON | `schema` required. |
| `object` | JSON object | `schema` required. |
| `array` | JSON array | `schema` required. |

Structured types require a [JSON Schema](https://json-schema.org/) (Draft-07), and the value is validated against it before it lands in state:

```yaml
- type: prompt
  name: assessment
  prompt_type: json
  schema:
    type: object
    properties:
      severity: {type: string, enum: [low, medium, high]}
      components:
        type: array
        items: {type: string}
    required: [severity]
  template: |
    Rate the severity of this issue and list the components it affects.
    Return ONLY a JSON object with "severity" (low, medium or high) and "components".

    {{input.issue}}
```

Downstream effects then read fields, not text: `{{prime.assessment.value.components}}` in a template, `state.prime.assessment.value.severity == 'high'` in a CEL expression. This is how effects pass *typed* data to each other, and it is what makes a later `loop` over the components or an `if` on the severity possible at all — a loop cannot iterate prose.

A structured type without a schema is rejected:

```yaml
# ✗ json, object and array all require a schema
- type: prompt
  name: assessment
  prompt_type: json
  template: "Return the severity and the affected components as JSON: {{input.issue}}"
```

Keep schemas small. A schema with more than three or four properties is a smell: the prompt is asking the model to do several things at once, and the fix is several prompts with simpler schemas, each one focused. End every template that expects structure with an explicit instruction — "Return ONLY a JSON object with …". The extractor is forgiving about code fences and a sentence of preamble, but a reply that wanders produces a `null` value and a schema failure, and the instruction is what keeps the reply clean.

(A seventh `prompt_type`, `tool`, exists in the schema for tool-call replies and currently passes the reply through untouched. Treat it as reserved.)

## Choosing the model — or not

`model`, `provider`, and `provider_fallbacks` exist on every prompt, and the best-written orchestrations almost never set them. Capability belongs in [configuration](04-configuration.md): a document that hard-wires `model: gpt-4o` cannot run on a local model tomorrow without an edit, and cannot be routed by the [complexity router](10-complexity.md) today. Set `model:` on a prompt only when *that specific step* needs a specific model no matter what the run says — and know that an explicit `model:` outranks even the `--model` flag.

`deterministic: true` asks for temperature zero, plus a fixed seed where the provider takes one (ollama, openai); a `temperature` in `params` outranks it. `params` carries generation parameters: `temperature`, `max_tokens` and `stop` mean the same thing on every adapter, which maps them to its provider's names (`max_tokens` is ollama's `num_predict` and anthropic's `max_tokens`); any other key — `top_p`, ollama's `num_ctx` — goes to the provider as written. Like `model`, set it only where one step needs it.

## Prompt-local inputs and assets

`inputs` supplies template variables scoped to this one call — constants that do not belong in shared state:

```yaml
- type: prompt
  name: reply
  inputs:
    length: "two plain sentences"
  template: "Reply as the maintainer to whoever filed this issue (kind: {{prime.kind.value}}), in {{length}}. Issue: {{input.issue}}"
```

Prefer shared state for anything another effect might want; use `inputs` for genuinely local constants.

`assets` attaches images for a vision model — the screenshot attached to the issue:

```yaml
- type: prompt
  name: screenshot
  assets:
    - {kind: image, ref: "{{{input.screenshot}}}"}
  template: "What error does this screenshot show? Issue: {{input.issue}}"
```

`ref` is a template that renders to a local path (read and sent as the image's bytes) or an `http(s)` URL (passed through where the provider accepts one; ollama does not). Triple braces keep the value unescaped; double braces HTML-escape it, which breaks a URL with `&` in its query string. The bytes never land in state: `meta.assets` records each image's path, size and sha256. ollama, the OpenAI-compatible adapters and anthropic send images; `cof check` warns when the effect's adapter cannot, and other `kind`s are skipped with a warning.

## Errors, retries, timeouts

A prompt can fail: the adapter times out, the model returns something that will not parse as the declared type, the provider is down. `retries`, `provider_fallbacks`, `timeout_ms`, and `on_error` govern what happens next, and they are the subject of [Errors](05-errors.md). A prompt's `timeout_ms` bounds each attempt and can only shorten the adapter's own timeout, never lengthen it; it rounds up to whole seconds. The one-line preview: by default a failed prompt fails the run (`on_error: fail`), every attempt is recorded in `meta`, and nothing ever fails silently.

## What lands in the shadow state

```
prime.<name>.value                       # the typed result (null on skip / dry-run)
prime.<name>.meta.adapter                # which adapter answered
prime.<name>.meta.model                  # which model — after routing, if routing is on
prime.<name>.meta.model_reason           # "default" | "explicit" | "router"
prime.<name>.meta.prompt_type
prime.<name>.meta.prompt_sent            # the rendered prompt, post-interpolation
prime.<name>.meta.tokens_sent / tokens_received
prime.<name>.meta.error                  # null, or the failure
prime.<name>.meta.fallback_attempts      # the provider attempt chain
prime.<name>.meta.finish_reason          # the provider's stop reason, when it reports one
prime.<name>.meta.warnings               # present only when something needs saying: a reply
                                         # cut off at the length limit, options an adapter ignored
prime.<name>.meta.assets                 # present only with image assets: path, size, sha256
prime.<name>.meta.complexity             # present only when scoring is on (chapter 10)
prime.<name>.meta.decomposition          # present only when decomposition fired (chapter 11)
```

Inside a `dynamic` the path gains the container's name (`prime.context.search.value`); inside a loop it gains the pass (`prime.diagnoses.iter_0.diagnose.value`). The rule never changes: the path is derived from the names above it, and nothing else.

## Anti-patterns

**One prompt, three jobs.** A template that asks the model to *analyze and generate and review* is three prompts wearing one name. Split it — `outline` → `expand` → `polish` — and each step becomes observable, retryable, and routable on its own. Prefer many small effects over few large ones.

**Naming an effect after its type.** `name: prompt` validates, but the validator warns, and for good reason: generic names are exactly the ones that collide when two of them end up siblings. Name the effect after the job — `kind`, not `prompt`.

```yaml
# ⚠ validates, but warns: name the job, not the type
- type: prompt
  name: prompt
  template: "Is this issue a bug, a feature or a question? Issue: {{input.issue}}"
```

**A plural name.** `summarize_articles` is a signal that one effect is doing many things; the shape you want is a `loop` over the articles with a singular body effect, `summarize_article`.

**Reserved names.** `iter_<N>` is reserved for loop passes and cannot be an effect name; `last` is reserved under loops. Names match `^[A-Za-z_][A-Za-z0-9_]*$` and must be unique among siblings.

## See also

- [Orchestration Reference → `prompt`](../orchestration-reference.md#prompt) — the full field table.
- [`learn/prompt`](../../src/circuitry/curation/learn/prompt.yml) — text, json, number, and boolean outputs in one runnable file: `cof run learn/prompt`.
