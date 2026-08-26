# Errors

Runs degrade deliberately, never mysteriously. A model call can time out, return something that will not parse, or come back from a provider that is down; a tool's binary can be missing; a child orchestration can fail validation. Circuitry's position on all of it is the same: the failure is recorded where it happened, the policy for what happens next is declared on the effect, and the run record says not just *that* something failed but what the framework did about it.

This chapter comes before the cybernetic effects on purpose. A control loop that steers on state has to know what a failed step leaves in state, and the answer is always the same shape.

## The four knobs

```yaml
- type: prompt
  name: pair_wine
  template: "Pick a wine to pair with: {{prime.suggest_dish.value}}"
  retries: {max_attempts: 3, backoff_ms: 500}
  provider_fallbacks: [ollama, "openai:gpt-4o-mini"]
  timeout_ms: 60000
  on_error: skip        # no sommelier tonight — dinner goes on
```

**`retries`** — `{max_attempts, backoff_ms}` on a prompt. Each attempt re-renders nothing and re-sends the same prompt; `backoff_ms` is the pause between attempts. Each attempt is recorded, and `meta.retries_used` says how many it took. Prompts only — a tool that failed is not usually improved by asking again, and a `use` child carries its own policies.

**`provider_fallbacks`** — an ordered list of providers to try when the primary errors. Each entry is an `adapter[:model]` token: `ollama` means the ollama adapter with the run's default model; `openai:gpt-4o-mini` names both. The attempt chain — every adapter and model tried, in order, with its outcome — lands at `meta.fallback_attempts`, and `meta.fallback_recovered` is `true` when it took more than one. You can always see who actually answered. (An effect's own `provider:` sets the *primary* the same way; both are usually run policy, set in config or a profile, rather than something a document author writes.)

**`timeout_ms`** — the effect's budget. Separate from the adapter's socket timeout in config (`runtime.adapters.<name>.timeout_seconds`), which is per machine rather than per step; a large local model needs cold-load headroom there that no single effect should have to carry.

**`on_error`** — what happens when the attempts are exhausted. Three values on every effect except loops:

| `on_error` | The effect | The run |
| --- | --- | --- |
| `fail` (default) | records the error | stops; the failure propagates to the root with a breadcrumb path |
| `skip` | records the error, writes `value: null` | continues with the next effect |
| `continue` | records the error, value stays `null` | continues with the next effect |

For a leaf effect — prompt, tool, use — `skip` and `continue` are the same degradation: a null value, an error in `meta`, and a run that keeps going. Downstream templates render the missing value as empty; downstream CEL comparisons against it are false. The difference shows on an `if`: `skip` runs *neither* branch (`branch: null`), while `continue` takes the `else` branch as a degraded default. Choose by what downstream needs.

Loops have their own vocabulary — `fail` / `break` / `continue` — because a failed *pass* is a different thing from a failed effect. [Loop](07-loop.md) covers it; the short form is that `break` ends the loop at the failed pass and `continue` drops that pass and goes on, and neither a dropped pass nor a broken one counts toward `last` or `collected`.

## Where failure lands

```
prime.<name>.value                    # null after a skip
prime.<name>.meta.error               # the message
prime.<name>.meta.fallback_attempts   # [{adapter, model, status, error}, …]
prime.<name>.meta.fallback_recovered  # true when a fallback answered
prime.<name>.meta.retries_used        # present when a retry succeeded
prime.<container>.meta.error          # "prime.menu.wine: …" — a breadcrumb into the child
```

A container that fails closes its own node with `value: false` and a `meta.error` that names the child path, so from the root down you can follow the breadcrumbs to the leaf. `inspect_divergence_paths(state)` in the SDK does the walk for you and returns every errored node in path order.

Two things never happen. A failure never leaves a *partial* value: a `use` child whose run failed discards its scratch state whole, and nothing lands at the parent's path. And a failure never goes unrecorded: even `on_error: skip` writes the error into `meta`, and the per-effect `on_effect_complete` hook fires for the failed node as it does for a successful one, carrying the error.

## Designing for degradation

The dinner party has a step that fetches a recipe from the web for inspiration. The web is optional; dinner is not:

```yaml
- type: tool
  name: fetch_recipe
  provider: web_fetch
  params: {url: "{{input.recipe_url}}"}
  on_error: skip

- type: prompt
  name: plan_courses
  template: |
    Plan three courses for {{input.occasion}}.
    Inspiration, if any: {{prime.fetch_recipe.value}}
```

When the fetch fails, `plan_courses` reads "Inspiration, if any: " and carries on. The model gets less; the run gets a menu; the state file says the fetch failed and why. That is the pattern: put `skip` on the effects whose absence downstream can tolerate, leave `fail` on the ones it cannot, and write the downstream templates so that an empty interpolation reads as "none" rather than as a broken sentence.

The other half of the pattern is *not* to reach for `on_error` when what you want is a decision. If the missing recipe should change what happens next — a different planning prompt, say — that is an [`if`](06-if.md) on `state.prime.fetch_recipe.meta.error != null`… except that a negative test against `null` is exactly the CEL shape to avoid. Test positively for the value you need — `size(state.prime.fetch_recipe.value) > 0` — and let the error case fall to `else`.

## Validation is the first error handler

Most failures should never reach the runtime. `cof check` validates a document against the schema and the static rules — namespace-rooted paths, unique sibling names, required schemas, `use` cycles, unfetched library sources — and then runs *preflight*: every adapter and plugin the document references reports whether it can run (`check()`), so a missing API key or an uninstalled binary stops the run before the first model call rather than after the tenth. `cof doctor` runs the same checks against everything compiled in. `--skip-preflight` exists for the moments you know better.

Warnings are advisory: deprecated spellings and type-keyword names are reported and never change the exit code.

## Dry runs

`cof run … --dry-run` executes the whole orchestration without calling a model: every prompt writes `value: null` and a `meta` block with the rendered `prompt_sent`; CEL conditions evaluate for real, while model-mode ones take a fixed answer (an `if` takes `then`, a `while` stops after its minimum passes); `each` loops iterate whatever collection is already in `input`; tools are skipped. It is the fastest way to see the state graph a document will produce, and to read every rendered prompt before spending a token. The [complexity scorer](10-complexity.md) runs in a dry run too.

## Anti-patterns

**`skip` everywhere.** A run where every effect is skippable cannot fail, and therefore cannot tell you anything. Skip the optional; fail the essential.

**Retrying a parse failure without changing the ask.** If a `json` prompt fails its schema three times, the fourth attempt will too. The fix is the template — "Return ONLY …", a smaller schema — not `max_attempts`.

**Catching errors with negative CEL.** `state.prime.x.value != ""` is true against `null`, which is what a skipped effect leaves. Test positively.

**Timeouts in the wrong clock.** A cold-loading 70B model needs minutes; put that in the adapter's `timeout_seconds`, not on every effect.

## See also

- [Orchestration Reference → `prompt`](../orchestration-reference.md#prompt) — `retries`, `provider_fallbacks`, `timeout_ms`, `on_error`.
- [Troubleshooting Deterministic State Paths](../troubleshooting-state-paths.md).
