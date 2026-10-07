# Errors

Runs degrade deliberately, never mysteriously. A model call can time out, return something that will not parse, or come back from a provider that is down; a tool's binary can be missing; a child orchestration can fail validation. Circuitry's position on all of it is the same: the failure is recorded where it happened, the policy for what happens next is declared on the effect, and the run record says not just *that* something failed but what the framework did about it.

This chapter comes before the cybernetic effects on purpose. A control loop that steers on state has to know what a failed step leaves in state, and the answer is always the same shape.

## The four knobs

```yaml
- type: prompt
  name: reply
  template: "Reply as the maintainer to whoever filed this issue. Thank them and say what happens next. Issue: {{input.issue}}"
  retries: {max_attempts: 3, backoff_ms: 500}
  provider_fallbacks: [ollama, "openai:gpt-4o-mini"]
  timeout_ms: 60000
  on_error: skip        # no reply drafted — the fix goes on
```

**`retries`** — `{max_attempts, backoff_ms}` on a prompt. Each attempt re-renders nothing and re-sends the same prompt. Not every failure is retried: a dispatch failure (the adapter call itself erroring) is classified first — a rate limit, a request timeout, a 5xx, or a connection that never completed is worth trying again; a bad request, an auth failure, a not-found, or a missing key never will be, so that failure ends the attempt loop immediately rather than spending the rest of `max_attempts` on something that cannot succeed. A reply that came back but could not be used — see `provider_fallbacks` below — is always worth retrying, the same as before this classification existed. The wait between a retryable failure and the next attempt is exponential backoff with jitter starting from `backoff_ms`, capped at 60 seconds; a provider's own `Retry-After` response header, when the adapter can read one, overrides the computed wait outright (still capped) — litellm reads it off the SDK exception's response headers, and the curl-based adapters (openai, anthropic, ollama, watsonx, replicate, and the OpenAI-compatible family) capture it the same way `run_curl` keeps everything else off argv and out of temp files, falling back to the computed backoff on curl too old to support the capture. Each attempt is recorded, and `meta.retries_used` says how many it took. `tool` and `use` take the same `retries` key, with their own retryable classification — see below.

**`provider_fallbacks`** — an ordered list of providers to try when the primary errors *or* answers with something this effect can't use: an unreadable boolean/number, or JSON that fails its `schema`. Either kind of failure moves to the next provider in the list before a retry is ever counted — a small local model that keeps answering "maybe" to a yes/no prompt reaches a stronger fallback instead of exhausting `retries` on itself. Each entry is an `adapter[:model]` token: `ollama` means the ollama adapter with the run's default model; `openai:gpt-4o-mini` names both. The attempt chain — every adapter and model tried, in order, with its outcome and token cost — lands at `meta.fallback_attempts`, and `meta.fallback_recovered` is `true` when it took more than one. An attempt that answered but was unusable carries a size-capped `raw_reply` alongside its status (`decode_failed` for an unreadable boolean/number, `schema_invalid` for a schema failure) so you can see what was rejected, not just that it was. You can always see who actually answered. With no `provider_fallbacks` configured, an unreadable reply behaves exactly as it always has — it fails the attempt and `retries` applies. (An effect's own `provider:` sets the *primary* the same way; both are usually run policy, set in config or a profile, rather than something a document author writes.)

**`timeout_ms`** — the effect's budget, on a prompt or a tool: a test run that hangs fails its step when the budget runs out, instead of holding the agent all night. A sub-second value rounds up to one second rather than flooring to zero. Separate from both of config's machine-level timeouts — the socket timeout of whichever adapter an attempt actually dispatches to (`runtime.adapters.<name>.timeout_seconds`, read for *that* adapter, not necessarily the run default — a `provider:`/fallback that sends an attempt elsewhere uses that adapter's own configured timeout) and a tool's own default (`runtime.tools.timeout_seconds`, 300s unset) — which are per machine rather than per step; a large local model needs cold-load headroom there that no single effect should have to carry. Not every tool plugin can actually be killed on overrun — see the `tool` effect's [Timeout](../orchestration-reference.md#tool) section for which ones honor it.

**`on_error`** — what happens when the attempts are exhausted. Three values on every effect except loops:

| `on_error` | The effect | The run |
| --- | --- | --- |
| `fail` (default) | records the error | stops; the failure propagates to the root with a breadcrumb path |
| `skip` | records the error, writes `value: null` | continues with the next effect |
| `continue` | records the error, value stays `null` | continues with the next effect |

For a leaf effect — prompt, tool, use — `skip` and `continue` are the same degradation: a null value, an error in `meta`, and a run that keeps going. Downstream templates render the missing value as empty; a downstream CEL expression that reads it is false as a whole, whatever its operator. The difference shows on an `if`: `skip` runs *neither* branch (`branch: null`), while `continue` takes the `else` branch as a degraded default. Choose by what downstream needs.

Loops have their own vocabulary — `fail` / `break` / `continue` — because a failed *pass* is a different thing from a failed effect. [Loop](07-loop.md) covers it; the short form is that `break` ends the loop at the failed pass and `continue` drops that pass and goes on, and neither a dropped pass nor a broken one counts toward `last`.

A tool effect fails the same way whether the plugin raised or just reported
it: `on_error` reacts identically to an exception and to a result the
plugin marks failed without one (an HTTP-family tool's 4xx/5xx response, by
default). `meta.exit_code` means a process exit code only — never an HTTP
status or a plugin-specific soft flag; see the `tool` effect's [result
contract](../orchestration-reference.md#tool) for the full breakdown,
including `meta.status_code` and `meta.raw`. An HTTP-family tool's failure
message also carries a bounded, redacted excerpt of the response body when
one is available — a provider's validation reason, not just the bare
status line — so `meta.error` alone usually says why, without a separate
lookup into `meta.raw.body`.

## `retries` and `expect` on `tool` and `use`

```yaml
- type: tool
  name: generate_frame
  provider: comfyui
  prompt: "{{input.prompt}}"
  model: flux1-dev-fp8.safetensors
  retries: {max_attempts: 3, backoff_ms: 1000}
  expect: "size(value) > 0"
```

(`comfyui`'s own `value` is the generated image's path/URL/base64 string, not
an object — `size(value) > 0` is the right shape for it; a tool whose
`value` actually is an object, e.g. an `http` JSON response, checks a field
on it the way the second example below does.)

**`retries`** on `tool` and `use` is the same two fields, the same default (unset: one attempt) and the same backoff curve as a prompt's. What counts as retryable differs by what kind of failure it is. An **HTTP-family** tool (`http`, `web_fetch`, `webhook`, `linear`) classifies by status the way an adapter dispatch does: 429, 408 and 5xx retry; any other 4xx does not, for the same reason a prompt's bad request or missing key doesn't — it cannot succeed on a second try. Every other **process** tool — `shell`, `ffmpeg`, `comfyui`, and the rest — retries on any failure: there is no status to classify, and the motivating case is a macOS GPU watchdog killing a FLUX render mid-job, which a second attempt on a cooled-down GPU often clears. A `use` effect's `retries` re-runs the **whole child orchestration** from scratch each attempt, not just the step that failed inside it.

**`expect`** is a check run after an attempt otherwise succeeds — the native replacement for the hand-built "did this actually work" `tool` step a model-generation pipeline tends to grow on its own. Either a CEL expression (a bare string is shorthand for `{mode: cel, expr: <string>}`) over this effect's own `value` and `meta` — bound directly, not under `state.` the way every other CEL site in the framework binds — plus `state` for reaching other effects' output the normal way; or `{mode: model, template: ...}`, which asks yes/no the same way a model-mode `if` does, on the run's own adapter/model, spending real tokens recorded at `meta.expect.tokens_sent`/`tokens_received`. A false or unreadable expectation fails the attempt with `expect failed: <expr or template summary>` — `retries` and `on_error` apply exactly as they would for any other failure, and an expect failure is always worth retrying (it carries no status of its own to classify). The outcome always lands at `meta.expect`, pass or fail:

```yaml
- type: tool
  name: ladder_step
  provider: comfyui
  prompt: "{{input.prompt}}"
  model: "{{input.checkpoint}}"
  params: {width: 1024, height: 1024}
  retries: {max_attempts: 3, backoff_ms: 2000}
  expect:
    mode: cel
    expr: "size(value) > 0 && has(meta.raw.outputs)"
```

(`meta.status_code` is only ever set for an HTTP-family tool — `comfyui` is
a process tool and never has one, so checking it here would always fail,
not skip; `meta.raw` is this tool's own result, ComfyUI's history entry for
the prompt, which carries an `outputs` node once the job actually ran.)

This replaces the `awk`-based `exit 1` guards a video pipeline otherwise writes by hand (`count`/`check8k`-style steps checking a prior tool's output before trusting it): the check is the effect's own `expect:`, not a sibling step, and a failure feeds the same `retries`/`on_error` every other failure does.

## `finally` — cleanup that always runs

```yaml
- type: dynamic
  name: with_ollama_server
  effects:
    - type: tool
      name: start_server
      provider: shell
      params: {command: launchctl, allowed_commands: [launchctl], args: [submit, -l, com.example.ollama, --, /usr/bin/env, ollama, serve]}
    - type: tool
      name: wait_ready
      provider: shell
      params: {command: curl, allowed_commands: [curl], args: [-sf, --retry, "30", --retry-connrefused, --retry-delay, "1", "{{input.ollama_url}}/api/version"]}
    - type: tool
      name: ask
      provider: http
      params: {url: "{{input.ollama_url}}/api/chat", method: POST, parse: json, json: {model: "{{input.model}}", stream: false}}
  finally:
    - type: tool
      name: stop_server
      provider: shell
      params: {command: launchctl, allowed_commands: [launchctl], allow_nonzero: true, args: [remove, com.example.ollama]}
```

`finally:` is a list of cleanup effects, legal on the document root and on a `dynamic` effect, nowhere else. It runs after the main `effects` complete, whatever happened — success, a failure anywhere inside, or a best-effort run on Ctrl-C/SIGTERM/SIGHUP/cancellation before the process exits — always sequentially, regardless of the enclosing `flow`. It sees state exactly as the body left it, so a `finally` step can read a value an earlier step produced (a started server's own output, a lock file's path) the normal way.

A Ctrl-C/SIGTERM/SIGHUP during a `flow: tree` `dynamic` or parallel `loop` stops it promptly, not just eventually: a branch not yet started never starts, a branch already running has its child process (the whole process group, not just the one pid) killed outright, and the step itself is never merged into its parent — `finally:` still runs regardless. A second SIGINT/SIGTERM while that cleanup is still running ends the process at once; a further hangup never does — closing a terminal can send SIGHUP twice in a row, and once a run is stopping, every hangup after the first is simply ignored instead of racing that cleanup, and stays ignored for the rest of the process so a late one can't kill `cof` before it finishes writing its state. `cof run`/`run-library` exit `130` for Ctrl-C/SIGINT, `143` for SIGTERM, or `129` for SIGHUP (a terminal hangup — closing the terminal window, an SSH disconnect) instead of `1`, so a script can tell an interrupted run apart from an ordinary failure; a run already launched with SIGHUP ignored (as `nohup` does) keeps that: no handler is installed for it, so a later hangup does nothing. See [Stopping a parallel step promptly](../orchestration-reference.md#resuming-a-run-cof-run---resume) in the reference.

Before `finally` existed, the only way to guarantee a cleanup step always ran was to mark the step *before* it `on_error: continue` with a comment explaining why — which quietly changes that step's own error policy too, turning "this step's failure doesn't matter" into "run the next thing regardless of what happened here":

```yaml
# ✗ runtime — on_error: continue on `ask` is doing two jobs at once: "a bad
# reply doesn't matter" AND "the server below must still be stopped". It
# validates and runs fine; `finally:` above is the fix, not a rejection.
- type: tool
  name: ask
  provider: http
  on_error: continue  # so the server below is always stopped; check fails loudly instead
  params: {url: "{{input.ollama_url}}/api/chat", method: POST, parse: json, json: {model: "{{input.model}}", stream: false}}
- type: tool
  name: stop_server
  provider: shell
  params: {command: launchctl, allowed_commands: [launchctl], allow_nonzero: true, args: [remove, com.example.ollama]}
```

`finally` separates the two concerns: `ask` keeps `on_error: fail` (the default, so a real failure actually stops the run) and `stop_server` moves to `finally`, where it always runs regardless.

A failure inside `finally` never hides the original error. If the main `effects` failed, that failure is still what's reported — a `finally` failure on top of it is recorded as a second note (`meta.finally_error`), never a replacement. If the main `effects` succeeded, a `finally` failure fails the run too, unless that particular `finally` effect has its own `on_error: continue`/`skip` — the same per-effect opt-out as anywhere else. A `use` child's own root `finally:` runs the same way, when the child orchestration finishes — the parent sees only the child's own `meta.error`/`meta.finally_error` folded into whatever the `use` effect reports, same as any other child failure.

## Where failure lands

```
prime.<name>.value                    # null after a skip
prime.<name>.meta.error               # the message
prime.<name>.meta.fallback_attempts   # [{adapter, model, status, error, tokens_sent,
                                       #   tokens_received, raw_reply?}, …]
prime.<name>.meta.fallback_recovered  # true when a fallback answered
prime.<name>.meta.retries_used        # present when a retry succeeded
prime.<name>.meta.tokens_sent_total   # every attempt's cost this execution made —
prime.<name>.meta.tokens_received_total # failed, retried and fallen-back-from alike
prime.<container>.meta.error          # "investigate.cause: …" — a breadcrumb into the child
```

A container that fails closes its own node with `value: false` and a `meta.error` that names the failing child's path (`context.search: …`), in `chain` and `tree` flow alike. The child's own node keeps its `meta.error` too, so from the root down you can follow the errors to the leaf. `inspect_divergence_paths(state)` in the SDK does the walk for you and returns every errored node in path order.

Two things never happen. A failure never leaves a *partial* value: a `use` child whose run failed discards its scratch state, and nothing lands at the parent's `value`. (With `runtime.state.record_children: true`, the child's partial record is kept beside that `value` for the audit trail. It is a record, not something downstream reads.) And a failure never goes unrecorded: even `on_error: skip` writes the error into `meta`, and the per-effect `on_effect_complete` hook fires for the failed node as it does for a successful one, carrying the error. That includes a failure the child absorbed: when an effect inside a `use` child skipped its own failure, the `use` node lists it in `meta.child_errors` as `{path, error}`, so a composed run that went green still says what it lost.

## Designing for degradation

The agent looks for related issues with `gh`, which needs a login and the network. Related issues help; the fix does not depend on them:

```yaml
- type: tool
  name: related
  provider: gh
  params: {args: [issue, list, --search, "{{input.error}}"], cwd: "{{input.repo}}"}
  on_error: skip

- type: prompt
  name: plan
  template: |
    Plan the fix for this issue in at most three steps.
    Related issues, if any: {{{prime.related.value}}}

    {{{input.issue}}}
```

When `gh` fails, `plan` reads "Related issues, if any: " and carries on. The model gets less; the run gets a plan; the state file says the lookup failed and why. That is the pattern: put `skip` on the effects whose absence downstream can tolerate, leave `fail` on the ones it cannot, and write the downstream templates so that an empty interpolation reads as "none" rather than as a broken sentence.

The other half of the pattern is *not* to reach for `on_error` when what you want is a decision. If the missing related issues should change what happens next — a different planning prompt, say — that is an [`if`](06-if.md). Remember the CEL rule from [Shadow state](03-state.md): an expression that reads a null or missing path is false as a whole. So test positively for the value you need — `size(state.prime.related.value) > 0` — and let the missing case fall to `else`. To branch on the failure itself, test the error: `state.prime.related.meta.error != null` is true when the lookup failed, and false when it succeeded, because a null `error` makes the whole expression false. Do not write `state.prime.related.value == null`: it reads a null path, so it is false even after the skip.

## Validation is the first error handler

Most failures should never reach the runtime. `cof check` validates a document against the schema and the static rules — namespace-rooted paths, unique sibling names, required schemas, well-formed templates, `use` cycles, unfetched library sources — and then runs *preflight*: every adapter and plugin the document references reports whether it can run (`check()`), so a missing API key or an uninstalled binary stops the run before the first model call rather than after the tenth. `cof doctor` runs the same checks against everything compiled in. `--skip-preflight` exists for the moments you know better.

Preflight reads `on_error` too. When every prompt that uses an adapter has its own `on_error: skip` or `continue`, that adapter is a *soft* dependency: a missing credential becomes a warning that names the effects that will skip, `cof check` passes, and the run goes on with those values `null`. One prompt on that adapter without the handling makes it a hard dependency again, and the error names that prompt. Only each prompt's own `on_error` counts, not an enclosing container's. An adapter name that does not exist is always a hard failure.

`cof run` applies the same structural check before its first effect, so a document `cof check` rejects never starts — it cannot fail halfway with the earlier effects already done. A `use` child loaded by `path:` or `ref:` is checked the same way when it loads, as an `inline:` child always was; `validate: false` on the `use` turns that off.

Three slips that YAML and Mustache would otherwise let through are errors. A key written twice in one mapping is rejected with both line numbers, where YAML alone keeps the last one and drops the first without a word. A key an effect does not know is ignored, so one that is a near miss of a key it does know is an error naming the key you meant:

```yaml
# ✗ whlie is not while: as written the loop has no condition, and cof check names the key you meant
- type: loop
  name: fix_until_green
  whlie: {mode: cel, expr: "state.prime.test_run.value.exit_code != 0"}
  max_iterations: 5
  body:
    - type: prompt
      name: patch
      template: "Write a patch that makes the failing tests pass: {{{prime.test_run.value.output}}}"
```

And a malformed Mustache tag — a brace short, a section closed under the wrong name — is an error naming the field:

```yaml
# ✗ {{input.issue} is a brace short: cof check rejects the template rather than send it as written
- type: prompt
  name: plan
  template: "Plan the fix for this issue in at most three steps: {{input.issue}"
```

Warnings are advisory: deprecated spellings, type-keyword names, an unknown key that resembles no known one, and a tool argument YAML read as a number or a boolean (`-0` arrives as `0`, `off` as `False` — quote it) are reported and never change the exit code.

```yaml
# ⚠ owner is no key a tool knows: it is ignored, and cof check says so
- type: tool
  name: related
  provider: shell
  owner: triage
  params: {command: gh, args: [issue, list, --search, "{{input.error}}"]}
```

## Dry runs

`cof run … --dry-run` executes the whole orchestration without calling a model: every prompt writes `value: null` and a `meta` block with the rendered `prompt_sent`; CEL conditions evaluate for real, while model-mode ones take a fixed answer (an `if` takes `then`, a `while` stops after its minimum passes); `each` loops iterate whatever collection is already in `input`; tools are skipped. It is the fastest way to see the state graph a document will produce, and to read every rendered prompt before spending a token. The [complexity scorer](10-complexity.md) runs in a dry run too.

## Anti-patterns

**`skip` everywhere.** A run where every effect is skippable cannot fail, and therefore cannot tell you anything. Skip the optional; fail the essential.

**Retrying a parse failure without changing the ask.** If a `json` prompt fails its schema against the *same* model three times, the fourth attempt will too — the fix is the template ("Return ONLY …", a smaller schema), not `max_attempts`. A stronger fallback model is a different lever: `provider_fallbacks` moves a schema-invalid reply to the next provider before any retry is spent, so a capable model gets a chance the struggling one never will.

**Testing for `null` in CEL.** `state.prime.x.value == null` is false after a skip, because an expression that reads a null path is false as a whole. Test positively for the value, or test `meta.error != null`.

**Timeouts in the wrong clock.** A cold-loading 70B model needs minutes; put that in the adapter's `timeout_seconds`, not on every effect.

## See also

- [Orchestration Reference → `prompt`](../orchestration-reference.md#prompt) — `retries`, `provider_fallbacks`, `timeout_ms`, `on_error`.
- [Orchestration Reference → `tool`](../orchestration-reference.md#tool) — `retries`, `expect`.
- [Orchestration Reference → `use`](../orchestration-reference.md#use) — `retries`, `expect`.
- [Orchestration Reference → `dynamic`](../orchestration-reference.md#dynamic) — `finally`.
- [Troubleshooting Deterministic State Paths](../troubleshooting-state-paths.md).
