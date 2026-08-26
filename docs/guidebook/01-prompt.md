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
  name: suggest_dish
  template: "Suggest one main course for {{input.occasion}}, in one sentence."
```

Run it, and one node appears in state:

```
prime.suggest_dish.value    # the model's answer
prime.suggest_dish.meta     # adapter, model, model_reason, prompt_type, prompt_sent,
                            # tokens_sent, tokens_received, error, fallback_attempts,
                            # created_at, completed_at
```

`value` is what the prompt is *for*. `meta` is the transformed context — the second half of the monadic return, and the reason a Circuitry run is a decision record rather than a transcript. `prompt_sent` in particular is the rendered text the adapter actually received, after every `{{…}}` was interpolated; when a run goes green but the output looks wrong, read it before anything else.

## Templates and messages

`template` is a [Mustache](https://mustache.github.io/) string. Anything in double braces is looked up in state: `{{input.occasion}}` reads a caller-supplied value, `{{prime.suggest_dish.value}}` reads an earlier effect's output. Triple-stache `{{{…}}}` skips HTML escaping, which matters when you interpolate code or markup. [State](03-state.md) covers the full addressing rules; the short version is that every reference starts with `input.`, `prime.`, or `runtime.`.

`messages` is the role-based alternative for models that expect a conversation:

```yaml
- type: prompt
  name: is_vegetarian
  prompt_type: boolean
  messages:
    - role: system
      content: "You are a strict classifier. Reply with only true or false."
    - role: user
      content: "Is this dish vegetarian? {{prime.suggest_dish.value}}"
```

Each `content` is a template in its own right. The two forms are mutually exclusive — a prompt with both fails validation:

```yaml
# ✗ template and messages are alternatives, never companions
- type: prompt
  name: suggest_dish
  template: "Suggest a main course."
  messages:
    - role: user
      content: "Suggest a main course."
```

## Typed output

`prompt_type` declares the shape of the value the runtime should extract from the model's reply, and the runtime enforces it. Six types:

| `prompt_type` | `value` is | Notes |
| --- | --- | --- |
| `text` | string | The default. The reply, as-is. |
| `boolean` | `true` / `false` | `yes`, `true`, `1`, `y` parse as true. |
| `number` | number | |
| `json` | parsed JSON | `schema` required. |
| `object` | JSON object | `schema` required. |
| `array` | JSON array | `schema` required. |

Structured types require a [JSON Schema](https://json-schema.org/) (Draft-07), and the value is validated against it before it lands in state:

```yaml
- type: prompt
  name: parse_recipe
  prompt_type: json
  schema:
    type: object
    properties:
      ingredients:
        type: array
        items: {type: string}
      servings: {type: number}
    required: [ingredients]
  template: |
    Extract the ingredient list and the serving count from this recipe.
    Return ONLY a JSON object with "ingredients" and "servings".

    {{input.recipe_text}}
```

Downstream effects then read fields, not text: `{{prime.parse_recipe.value.ingredients}}` in a template, `state.prime.parse_recipe.value.servings > 6` in a CEL expression. This is how effects pass *typed* data to each other, and it is what makes a later `loop` over the ingredients or an `if` on the serving count possible at all — a loop cannot iterate prose.

A structured type without a schema is rejected:

```yaml
# ✗ json, object and array all require a schema
- type: prompt
  name: parse_recipe
  prompt_type: json
  template: "Return the ingredients as JSON: {{input.recipe_text}}"
```

Keep schemas small. A schema with more than three or four properties is a smell: the prompt is asking the model to do several things at once, and the fix is several prompts with simpler schemas, each one focused. End every template that expects structure with an explicit instruction — "Return ONLY a JSON object with …". The extractor is forgiving about code fences and a sentence of preamble, but a reply that wanders produces a `null` value and a schema failure, and the instruction is what keeps the reply clean.

(A seventh `prompt_type`, `tool`, exists in the schema for tool-call replies and currently passes the reply through untouched. Treat it as reserved.)

## Choosing the model — or not

`model`, `provider`, and `provider_fallbacks` exist on every prompt, and the best-written orchestrations almost never set them. Capability belongs in [configuration](04-configuration.md): a document that hard-wires `model: gpt-4o` cannot run on a local model tomorrow without an edit, and cannot be routed by the [complexity router](10-complexity.md) today. Set `model:` on a prompt only when *that specific step* needs a specific model no matter what the run says — and know that an explicit `model:` outranks even the `--model` flag.

`deterministic: true` asks the adapter for temperature zero (or its equivalent). `params` passes provider-specific generation parameters through untouched — `{temperature: 0.2, top_p: 0.9}` — and, like `model`, is usually left to config.

## Prompt-local inputs and assets

`inputs` supplies template variables scoped to this one call — constants that do not belong in shared state:

```yaml
- type: prompt
  name: pair_wine
  inputs:
    budget: "under forty dollars"
  template: "Pick a wine {{budget}} to pair with: {{prime.suggest_dish.value}}"
```

Prefer shared state for anything another effect might want; use `inputs` for genuinely local constants.

`assets` attaches non-text inputs — `[{kind: image, ref: ./plate.jpg}]` — for multimodal-capable models. `kind` names the asset type; `ref` is a resolvable path or identifier.

## Errors, retries, timeouts

A prompt can fail: the adapter times out, the model returns something that will not parse as the declared type, the provider is down. `retries`, `provider_fallbacks`, `timeout_ms`, and `on_error` govern what happens next, and they are the subject of [Errors](05-errors.md). The one-line preview: by default a failed prompt fails the run (`on_error: fail`), every attempt is recorded in `meta`, and nothing ever fails silently.

## What lands in state

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
prime.<name>.meta.complexity             # present only when scoring is on (chapter 10)
prime.<name>.meta.decomposition          # present only when decomposition fired (chapter 11)
```

Inside a `dynamic` the path gains the container's name (`prime.menu.suggest_dish.value`); inside a loop it gains the pass (`prime.courses.iter_0.cook.value`). The rule never changes: the path is derived from the names above it, and nothing else.

## Anti-patterns

**One prompt, three jobs.** A template that asks the model to *analyze and generate and review* is three prompts wearing one name. Split it — `outline` → `expand` → `polish` — and each step becomes observable, retryable, and routable on its own. Prefer many small effects over few large ones.

**Naming an effect after its type.** `name: prompt` validates, but the validator warns, and for good reason: generic names are exactly the ones that collide when two of them end up siblings. Name the effect after the job — `suggest_dish`, not `prompt`.

```yaml
# ⚠ validates, but warns: name the job, not the type
- type: prompt
  name: prompt
  template: "Suggest one main course for {{input.occasion}}."
```

**A plural name.** `summarize_articles` is a signal that one effect is doing many things; the shape you want is a `loop` over the articles with a singular body effect, `summarize_article`.

**Reserved names.** `iter_<N>` is reserved for loop passes and cannot be an effect name; `last` is reserved under loops. Names match `^[A-Za-z_][A-Za-z0-9_]*$` and must be unique among siblings.

## See also

- [Orchestration Reference → `prompt`](../orchestration-reference.md#prompt) — the full field table.
- [`learn/prompt`](../../src/circuitry/curation/learn/prompt.yml) — text, json, number, and boolean outputs in one runnable file: `cof run learn/prompt`.
