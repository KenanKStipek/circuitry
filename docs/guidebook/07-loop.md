# Loop

The loop body executes, writes state, and the continuation condition evaluates against that new state — each iteration's output is the next iteration's input. This is feedback in the literal cybernetic sense: the output of the system is fed back as input, and the system decides from what it observes whether to go around again.

Two kinds of loop answer two kinds of question. `each` asks "for every one of these?" and walks a collection. `while` asks "again?" and consults a condition — a CEL expression over the latest test run, or the model reading the latest draft. Neither is capped unless you say so. An `each` loop walks a collection resolved before its first pass, so it can run long but never forever. A `while` loop runs until its condition says stop, and one whose condition never does runs indefinitely. `max_iterations` is the ceiling you set over that runaway feedback. It is opt-in, and when you set it, it must be at least 1: `cof check` rejects `max_iterations: 0`.

## The shape

```
Loop ::= { type: 'loop', body: Effect+,
           each: { in: STATE_PATH | BINDING_PATH,        — resolves to an array; may start at an outer loop's as
                   as?: NAME,                            — defaults to item
                   truncate?: BOOL }                     — default false: too long a collection fails the loop
           ⊕ while: Condition,                           — checked before each pass; always sequential
           name?: NAME,                                  — named ⇒ iter_<N> / last / collected nodes
           collect?: NAME,                               — a body step; needs a named loop
           flow?: 'chain' | 'tree', max_concurrency?: INT≥1,   — each loops only
           max_iterations?: INT≥1,                       — no default; an each collection may not exceed it
           min_iterations?: INT,                        — while loops only; each: no effect, cof check warns
           on_error?: 'fail'|'break'|'continue',
           labels?: MAP, description?: STRING }
```

`body` and exactly one of `each` / `while` are required. `cof check` rejects a loop with neither or both, so a misspelled `whlie:` is an error rather than a loop that runs zero passes and reports a clean finish.

## `each` — over a collection

```yaml
- type: loop
  name: diagnoses
  each:
    in: prime.failures.value
    as: test
  collect: diagnose
  body:
    - type: prompt
      name: diagnose
      template: "Explain why this test fails, in two sentences: {{test}}"
```

`each.in` is a root-relative state path that must resolve to an array at run time — `input.failures` for a caller-supplied list, `prime.failures.value` for the output of an `array`/`json` prompt or of a tool that returns a list, or a `runtime.` path. `as` names the current element for the body (`item` by default). The body runs once per element, in order.

The path is checked statically for its root — a bare key or a `state.` spelling is a hard error from `cof check`, with the fix named — and dynamically for its type. A path that resolves to nothing, or to something that is not a list, ends the loop with termination reason `collection_unresolved` and zero passes. Never silent success: a text prompt that was supposed to produce a list, and produced prose, is caught here.

```yaml
# ✗ runtime — failures is text; each.in needs an array, so the loop terminates collection_unresolved
- type: prompt
  name: failures
  template: "List the failing tests in this pytest output: {{{prime.test_run.value}}}"
- type: loop
  name: diagnoses
  each: {in: prime.failures.value, as: test}
  body:
    - type: prompt
      name: diagnose
      template: "Explain why this test fails, in two sentences: {{test}}"
```

The fix is upstream: `prompt_type: array` with a schema, so the producing prompt is held to the shape the loop needs.

**The collection is the bound.** An `each` loop knows its length before the first pass, so hitting `max_iterations` is never a runaway; it means the list is longer than you said it could be. Without `max_iterations`, the loop runs every element, and nothing below applies. With it, a collection with more elements than the cap fails the loop at start, with no pass run. The error names both numbers and the `each.in` path: *each loop 'diagnoses' (prime.failures.value): collection has 12 items but max_iterations is 8 — …*. This bounds check is gated by the loop's own `on_error` exactly like a failed pass is — see [Errors in a loop](#errors-in-a-loop) — so `on_error: break`/`continue` let the run continue past it instead of stopping. Raise the cap, bound the collection upstream, or say that the first *N* are enough:

```yaml
- type: loop
  name: diagnoses
  max_iterations: 8
  each: {in: prime.failures.value, as: test, truncate: true}
  body:
    - type: prompt
      name: diagnose
      template: "Explain why this test fails, in two sentences: {{test}}"
```

With `truncate: true` the loop diagnoses the first eight failing tests and records why it stopped: termination reason `max_iterations_reached`, plus `termination.unvisited`, the count of tests it never reached. A shorter list is not truncated; the list can end before the cap.

**In parallel.** An `each` loop is a chain by default. `flow: tree` runs every pass concurrently — the loop's *iterations* fan out, while the steps inside one pass still run in order — and `max_concurrency` bounds the pool. Left unset, it falls back to `ThreadPoolExecutor`'s own `min(32, cpu_count + 4)` — not "every iteration at once": `each.in` has no default bound on collection size, unlike a `dynamic` tree's fixed, author-declared effect list, so an unset pool size here is deliberately capped. Results are assembled in the original order whatever order they finished in:

```yaml
- type: loop
  name: diagnoses
  flow: tree
  max_concurrency: 4
  collect: diagnose
  each: {in: prime.failures.value, as: test}
  body:
    - type: prompt
      name: diagnose
      template: "Explain why this test fails, in two sentences: {{test}}"
```

Each pass writes into its own isolated state and cannot see its siblings'; their results are merged in order when the last one lands. Watching the run is a different matter. Every step inside a pass reports to observers at its full path, `prime.diagnoses.iter_3.diagnose`, as it starts and as it lands: the `--live-state` mirror shows the finished diagnoses while the others are still running, and the TUI, effect observers and runtime plugins hear about each one from the worker thread that ran it.

## `while` — until the condition says stop

```yaml
- type: loop
  name: polish
  while:
    mode: model
    template: "Read this reply to an issue reporter. Does it still need changes before it is posted?\n\n{{prime.revise.value}}"
  min_iterations: 1
  max_iterations: 5
  body:
    - type: prompt
      name: revise
      template: "Revise this maintainer reply so it is accurate, polite and brief: {{prime.reply.value}}"
```

The condition is evaluated *before* each pass, once the loop has run at least `min_iterations` passes; a pass `min_iterations` still forces runs without the condition being evaluated at all — not evaluated and overridden, never evaluated, so it costs no model call and reads no state. Once evaluated, the condition sees the pass that just finished under the same within-iteration names the body uses — `{{prime.revise.value}}` in the condition is the latest revision. `min_iterations` is the way to say "always revise at least once" regardless of what the condition would have answered — and `max_iterations` stops the loop whatever the model thinks. It has no default: without it, a `while` loop runs until its condition says stop, however many passes that takes. A loop stopped that way completes normally, but it is not recorded as converged: its termination reason is `max_iterations_reached`, and a `--verbose` run prints a warning line. A `while` loop is always sequential; `flow` does not apply.

Model mode wraps the template the way `if` does — *"… Should the loop continue? Answer (yes/no):"* — and parses the reply just as leniently (see [If](06-if.md)): `Yes.`/`yes, because …`/`**TRUE**` continue, `No.`/`false!` stop, and an answer that doesn't parse as either raises instead of silently stopping the loop. Each check's raw reply, adapter and model land on `meta.answer`/`meta.adapter`/`meta.model`. So phrase it as a question whose *yes* means "go around again". CEL mode is deterministic, and reads whatever the previous pass left in state:

```yaml
- type: tool
  name: test_run
  provider: pytest
  params: {args: [-q], cwd: "{{input.repo}}", allow_nonzero: true}
- type: loop
  name: candidates
  while:
    mode: cel
    expr: "state.prime.check.meta.exit_code != 0"
  min_iterations: 1
  max_iterations: 3
  body:
    - type: prompt
      name: patch
      template: "Write a unified diff for this repository that makes the failing tests pass. Output the diff only.\n\n{{{prime.test_run.value}}}"
    - type: tool
      name: check
      provider: git
      params: {args: [apply, --check, "-"], cwd: "{{input.repo}}", stdin: "{{{prime.patch.value}}}\n", allow_nonzero: true}
```

`git apply --check` only asks whether the patch would apply; `check` records the answer as its exit code, and the loop drafts another patch until one applies, three times at most. After the loop, `prime.candidates.last.patch.value` is the patch that passed the check. `min_iterations: 1` is what starts the loop here: without it the condition would be checked before any pass has run, reading `check` before it exists, and the loop would terminate `condition_false` without ever drafting a patch.

A `mode: cel` condition also sees `state.iter.index`: the index of the *last finished* pass — `-1` on the check before the first pass, `N - 1` once N passes have run (a pass that failed under `on_error: continue` still counts here, since the condition only knows a pass ran, not whether it finished). This is the index of the pass that just ran, not the one about to run, so `state.iter.index + 1 < 3` caps a loop at 3 passes. `state.iter.count` is the same count without the offset — `0` before the first pass, `N` once N have run — so `state.iter.count < 3` reads the same thing without the `+ 1`. Either combines with other state as a condition (`state.iter.count < 3 && state.prime.check.meta.exit_code != 0`).

One thing a CEL condition cannot do is read the loop's own `collected` or `last` — both are written when the loop *completes*, so from inside the loop the path is missing, and an expression that reads a missing path is `false`. A `while` written that way runs zero passes and terminates `condition_false`. *Where* you read loop state matters as much as *what* you read; the next section is the map.

### Refining across passes

The condition sees the previous pass. The *body* of a named loop now does too: `prime.<loop>.prev.<step>.value` (and `.meta`) is the previous **completed** pass — absent on the first pass, so a template renders it empty and a CEL `has()` check reads false. It works for both `each` and `while`, in chain flow; a `flow: tree` body referencing it is a `cof check` error, since tree passes run in parallel and there is no previous one to read. Nested loops: each loop's `prev` is its own, independent of any enclosing loop's.

```yaml
- type: prompt
  name: draft
  template: "Write a single paragraph on: {{input.topic}}"
- type: loop
  name: review
  max_iterations: 3
  min_iterations: 1
  while:
    mode: model
    template: "Does this paragraph still need revision for clarity or brevity?\n\n{{prime.refine.value}}"
  body:
    - type: prompt
      name: critique
      template: >-
        Critique this paragraph for clarity and brevity:
        {{#prime.review.prev}}{{prime.review.prev.refine.value}}{{/prime.review.prev}}{{^prime.review.prev}}{{prime.draft.value}}{{/prime.review.prev}}
    - type: prompt
      name: refine
      template: "Rewrite the paragraph addressing this critique: {{prime.critique.value}}"
```

`critique` reads the *previous* pass's `refine` — never *this* pass's own (unwritten) `refine`, and never the first pass's `draft` forever the way a bare `{{prime.draft.value}}` would. On the first pass `prime.review.prev` is absent, so the inverted Mustache section (`{{^prime.review.prev}}`) falls back to the pre-loop `draft`; from the second pass on, the regular section (`{{#prime.review.prev}}`) takes over and reads the growing refinement instead. [`patterns/critique_refine_loop`](../../src/circuitry/curation/patterns/critique_refine_loop.yml) is the complete version of this shape.

Before `prev`, the only way to build on a loop's own previous output was an **unnamed** loop overwriting the same name it reads (a `while` loop's body reusing the name `test_run` for both the seed before the loop and every pass's own rerun) — at the cost of losing `iter_<N>` and `collected` for every pass. `prev` keeps both: the body sees the previous pass, and a named loop still records each one, so prefer it unless a loop genuinely has nothing worth keeping history of.

## `collect`

`collect: <step>` gathers one body step's value from every pass into an array at `prime.<loop>.collected.value`, in pass order. It needs a named loop — the array has to live somewhere, and `cof check` rejects `collect` on an unnamed loop, naming the fix (give the loop a `name`) — and it is the usual way a loop hands its work to the effect after it:

```yaml
- type: prompt
  name: plan
  template: "Plan one fix that accounts for all of these diagnoses: {{prime.diagnoses.collected.value}}"
```

A collect target that a profile switched off contributes no slot, and nothing after a pass that broke the loop is collected. A pass dropped under `on_error: continue` is, today, the exception; [Errors in a loop](#errors-in-a-loop) says what it leaves.

## Five read forms, one per question

This is the table to keep. Five *different* questions get five *different* paths, and substituting one for another does not fail — it renders something plausible and wrong.

| You want | Write | Legal where |
| --- | --- | --- |
| A step's output in the **current pass** | `{{prime.<step>.value}}` | inside the body, and inside the `while` condition |
| The **previous completed pass** | `{{prime.<loop>.prev.<step>.value}}` | inside the body only — chain flow (`each`/`while`); absent on the first pass |
| One **specific past pass** | `{{prime.<loop>.iter_<N>.<step>.value}}` | **after** the loop only |
| The **final completed pass** | `{{prime.<loop>.last.<step>.value}}` | **after** the loop only |
| **Every** pass's output of one step | `{{prime.<loop>.collected.value}}` | after the loop (requires `collect`) |

The rules of the form:

- **Resolution is a scope chain.** Inside the body, `prime.<step>` means the current iteration first, then the enclosing scope, then root. A root input or an effect that ran before the loop keeps resolving; a body step named the same as an outer effect shadows it, inside the body only.
- **`prev` is the previous pass that completed, body-only.** Chain flow only — a `flow: tree` body referencing it is a `cof check` error, since tree passes run in parallel. Absent (not an empty node) before the first pass, so a template renders it empty and CEL's `has()` reads false. See [Refining across passes](#refining-across-passes) above.
- **`last` is the last pass that completed.** A pass that errored under `on_error: continue` or `break` is skipped in favour of the one before it; a loop that ran zero passes writes no `last` key, so a read of it renders empty rather than serving a stale value. Prefer `last` over guessing an `N` — for a `while` loop, and for a data-dependent `each` loop, `N` is unknowable.
- **`iter_<N>` inside the body is a trap.** `N` is a constant, so it does not render empty — it renders *pass N's* output during every pass, which looks right on pass N and is stale on every other. `cof check` warns.
- **`prime.<loop>.<step>.value` does not resolve, by design.** `prime.<loop>` is the loop's own node — `iter_<N>`, `last`, `collected`, `value`, `meta` — and never holds body step names. `cof check` warns.
- **The bare form `{{<step>.value}}` also works** and means the same node; it is accepted, not preferred, because a bare name can collide with a caller-supplied key.
- In CEL the same forms apply with the `state.` prefix: `state.prime.<step>.value`.

```yaml
# ⚠ validates with a warning — iter_0 inside the body pins every pass to the first
- type: loop
  name: diagnoses
  each: {in: input.failures, as: test}
  body:
    - type: prompt
      name: trace
      template: "Name the function this test exercises: {{test}}"
    - type: prompt
      name: diagnose
      template: "Explain why the test fails, given the function it exercises: {{prime.diagnoses.iter_0.trace.value}}"
```

The correct within-pass read is `{{prime.trace.value}}`. [Troubleshooting State Paths](../troubleshooting-state-paths.md) has a symptom table — what `meta.prompt_sent` looks like for each wrong form — for the day one slips through.

## Named or not

A **named** loop records everything above: `iter_<N>` per pass, `last`, `collected`, and a summary at the node itself:

```
prime.diagnoses.value.iterations               # passes completed
prime.diagnoses.value.termination.reason       # why it stopped (below)
prime.diagnoses.value.effects_by_iteration     # what each pass ran
prime.diagnoses.meta.mode                      # "each", or the while condition's "model" / "cel"
prime.diagnoses.meta.max_iterations / min_iterations   # max_iterations only when set
prime.diagnoses.meta.each_in_path / each_as    # each loops
prime.diagnoses.iter_0.diagnose.value
prime.diagnoses.last.diagnose.value
prime.diagnoses.collected.value
```

In a saved shadow state — `--out`, `--json`, the `--live-state` mirror — `last` is not a second copy of the final pass. It is written once as a reference, `"last": {"$ref": "iter_2"}`, and every reader that loads the file links it back. [Shadow state](03-state.md) has the details. Inside the run, `prime.diagnoses.last.diagnose.value` reads the same as always.

An **unnamed** loop is transparent: the body writes at stable paths in the enclosing scope, and each pass overwrites the last. No `iter_<N>`, no `last`, no `collected`. It is the right shape when only the final pass matters and nothing downstream needs the history. `collect` on an unnamed loop has nowhere to write, so `cof check` rejects it:

```yaml
# ✗ — no name, so no collected node: give the loop a name to fix this
- type: loop
  collect: diagnose
  each: {in: input.failures, as: test}
  body:
    - type: prompt
      name: diagnose
      template: "Explain why this test fails, in two sentences: {{test}}"
```

**Termination reasons** — every completed named loop records one: `collection_exhausted` (every element done), `condition_false` (the `while` said stop), `max_iterations_reached` (the cap you set: a `while` that never said stop, or a truncated `each`, which also records `unvisited`), `collection_unresolved` (`each.in` was not an array), `condition_error` (the `while` condition could not be evaluated, under `break` or `continue`), `error` (the loop failed; `termination.detail` and `meta.error` say why). They are CEL-readable — `state.prime.candidates.value.termination.reason == 'max_iterations_reached'` is a fine thing to branch on after a loop whose patches never applied.

## Progress while it runs

A named loop carries `prime.<name>.meta.progress` while it is running, not only once it finishes:

```
prime.render.meta.progress.done       # passes completed so far
prime.render.meta.progress.total      # each: the collection length; while: max_iterations, or null if uncapped
prime.render.meta.progress.elapsed_s  # wall time since the loop started
prime.render.meta.progress.eta_s      # average pass time × passes remaining; null until a pass has
                                       # completed, or whenever total itself is unknown
```

It updates every pass — the same `on_write` publish a chain loop already does per pass (so `--live-state` and any state observer see it move), and the same per-iteration publish a `flow: tree` loop's isolated branches already do. A `--verbose` run watching a real terminal (never under `--quiet`/`--json`, never when stdout isn't a TTY) also shows one updating line for the loop currently running — `shots 7/32, ~4 min left` for a bounded `each`, `attempts 2/3` for a bounded `while`. It costs one `time.monotonic()` call and a few float operations per pass: cheap enough to leave on for a multi-hour ladder or film render.

## Errors in a loop

`on_error` on a loop governs a failed *pass*, and the same too-long-collection bounds check an `each` loop runs before its first pass ("The collection is the bound", above): `fail` (default) propagates; `break` ends the loop at the failed pass, keeping what completed before it; `continue` drops the pass and goes on. In a `tree` loop every pass is already running, so `break` and `continue` both let the other passes finish and drop the failed ones, and `fail` raises the lowest-index failure deterministically — naming that pass's iteration — rather than whichever concurrent pass happened to raise first. Under `break` and `continue` alike, `last` is the last pass that completed, never the failed one.

`collected` holds the values of the passes that produced one, in pass order, and never loses a completed pass: a failed pass is left out entirely — the same contract a disabled collect target already has — even if the collect target itself produced a value before a later body effect in that same pass failed. The loop's `meta.failed_passes` lists the indices of every pass that failed, so failing tests `[test_a, test_b, test_c]` with `test_b` erroring under `on_error: continue` collects `[diagnosis-of-a, diagnosis-of-c]` with `meta.failed_passes: [1]`. The failed pass's own `iter_<N>` node still exists, with whatever body effects ran before the failure and an error in `meta` on the one that failed — reading it directly is how to see what a dropped pass actually did.

## Inside nested containers

`{{test}}` and `{{_loop_index}}` reach every effect in the body however deeply it is wrapped — an `if` branch, a grouping `dynamic`, an inner loop. An inner loop's `as` shadows the outer's if they share a name; give them different names. A CEL condition in the body reads the same two as `state.test` and `state.iter.index`, and a branch step reads the branch's earlier steps, as [If](06-if.md) describes:

```yaml
- type: loop
  name: diagnoses
  each: {in: input.failures, as: test}
  body:
    - type: if
      if: {mode: cel, expr: "state.test.startsWith('tests/integration/')"}
      then:
        - type: prompt
          name: services
          template: "List the external services this integration test needs: {{test}}"
        - type: prompt
          name: diagnose
          template: "Explain why failing test {{_loop_index}} fails: {{test}}. It needs these services: {{prime.services.value}}"
      else:
        - type: prompt
          name: diagnose
          template: "Explain why failing test {{_loop_index}} fails: {{test}}"
```

An inner loop can iterate a field of the outer loop's element. `each.in` may start at an enclosing loop's `as` binding, the same way a CEL path or a `use` input `{from: …}` can:

```yaml
- type: loop
  name: suites
  each: {in: input.suites, as: suite}
  body:
    - type: loop
      name: failing
      each: {in: suite.tests, as: test}
      collect: diagnose
      body:
        - type: prompt
          name: diagnose
          template: "In {{suite.file}}, explain why this test fails: {{test}}"
```

With `input.suites` as `[{file: tests/test_durations.py, tests: [test_hours_and_minutes, test_all_units]}, …]`, each suite's inner loop walks that suite's own failing tests. A root that is neither a namespace nor a binding in scope fails `cof check`, and the error lists the bindings that are in scope.

## Anti-patterns

**Iterating prose.** `each.in` on a `text` prompt. Make the producer an `array` prompt with a schema.

**Reading `collected` or `last` from inside the loop.** Both are written when the loop completes. Inside, read `{{prime.<step>.value}}`.

**`iter_0` inside the body.** Stale data with a warning. Above.

**Unbounded feedback.** A model-mode `while` with no `max_iterations` and a question the model tends to answer *yes* to does not stop; with `max_iterations: 100` it is a hundred model calls. Set the cap to the number of passes you would accept, and make the condition ask for *stop* evidence, not *continue* enthusiasm.

**A plural body.** `summarize_articles` as a single prompt inside the loop is a sign the loop is not doing the iterating. One element, one singular step.

## See also

- [Orchestration Reference → `loop`](../orchestration-reference.md#loop) and [Referencing a sibling within an iteration](../orchestration-reference.md#referencing-a-sibling-within-an-iteration).
- [`learn/loop`](../../src/circuitry/curation/learn/loop.yml) · [`patterns/critique_refine_loop`](../../src/circuitry/curation/patterns/critique_refine_loop.yml).
