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
           min_iterations?: INT,
           on_error?: 'fail'|'break'|'continue',
           labels?: MAP, description?: STRING }
```

`body` and exactly one of `each` / `while` are required.

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

**The collection is the bound.** An `each` loop knows its length before the first pass, so hitting `max_iterations` is never a runaway; it means the list is longer than you said it could be. Without `max_iterations`, the loop runs every element, and nothing below applies. With it, a collection with more elements than the cap fails the loop at start, with no pass run. The error names both numbers and the `each.in` path: *each loop 'diagnoses' (prime.failures.value): collection has 12 items but max_iterations is 8 — …*. Raise the cap, bound the collection upstream, or say that the first *N* are enough:

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

**In parallel.** An `each` loop is a chain by default. `flow: tree` runs every pass concurrently — the loop's *iterations* fan out, while the steps inside one pass still run in order — and `max_concurrency` bounds the pool. Results are assembled in the original order whatever order they finished in:

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

The condition is evaluated *before* each pass, and it sees the pass that just finished under the same within-iteration names the body uses — `{{prime.revise.value}}` in the condition is the latest revision. Before the first pass there is nothing to see yet, and the name falls through to the enclosing scope (empty, here). `min_iterations` forces that many passes regardless of the answer — the way to say "always revise at least once" — and `max_iterations` stops the loop whatever the model thinks. It has no default: without it, a `while` loop runs until its condition says stop, however many passes that takes. A loop stopped that way completes normally, but it is not recorded as converged: its termination reason is `max_iterations_reached`, and a `--verbose` run prints a warning line. A `while` loop is always sequential; `flow` does not apply.

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

`git apply --check` only asks whether the patch would apply; `check` records the answer as its exit code, and the loop drafts another patch until one applies, three times at most. After the loop, `prime.candidates.last.patch.value` is the patch that passed the check. Before the first pass there is no `check` yet, which makes the condition false, so `min_iterations: 1` starts the loop.

One thing a CEL condition cannot do is read the loop's own `collected` or `last` — both are written when the loop *completes*, so from inside the loop the path is missing, and an expression that reads a missing path is `false`. A `while` written that way runs zero passes and terminates `condition_false`. *Where* you read loop state matters as much as *what* you read; the next section is the map.

### Refining across passes

The condition sees the previous pass. The *body* of a named loop does not: inside the body, `{{prime.revise.value}}` means *this pass's* `revise`, which has not been written yet when the pass begins, so it renders empty. `last` and `iter_<N>` are post-loop spellings. There is no spelling in a named loop for "the pass before this one".

When the body needs to build on its own previous output — patch, apply, run the tests, patch again — use an **unnamed** loop and let the body overwrite the same name it reads:

```yaml
- type: tool
  name: test_run
  provider: pytest
  params: {args: [-q], cwd: "{{input.repo}}", allow_nonzero: true}
- type: loop
  while:
    mode: cel
    expr: "state.prime.test_run.meta.exit_code != 0"
  max_iterations: 3
  body:
    - type: prompt
      name: patch
      template: "Write a unified diff for this repository that makes the failing tests pass. Output the diff only.\n\n{{{prime.test_run.value}}}"
    - type: tool
      name: apply
      provider: git
      params: {args: [apply, "-"], cwd: "{{input.repo}}", stdin: "{{{prime.patch.value}}}\n"}
    - type: tool
      name: test_run
      provider: pytest
      params: {args: [-q], cwd: "{{input.repo}}", allow_nonzero: true}
- type: prompt
  name: summary
  template: "Write the pull request description for this fix. The final test run:\n\n{{{prime.test_run.value}}}"
```

An unnamed loop writes into the enclosing scope, so each pass's `test_run` replaces the last: `patch` reads the failures the previous pass left, the condition checks the latest exit code, and after the loop `prime.test_run.value` is the final run with no `last` to spell. The test run before the loop gives the first pass something to read. Name this loop and it goes wrong on the second pass: `patch` reads *this* pass's `test_run`, which has not run yet, so the read falls through to the test run before the loop, and the model patches the first failure again on top of its own first patch. What you give up is history — no `iter_<N>`, no `collected` — which is the right trade when only the final state matters. Keep the loop named when the record of every pass is the point.

## `collect`

`collect: <step>` gathers one body step's value from every pass into an array at `prime.<loop>.collected.value`, in pass order. It needs a named loop — the array has to live somewhere — and it is the usual way a loop hands its work to the effect after it:

```yaml
- type: prompt
  name: plan
  template: "Plan one fix that accounts for all of these diagnoses: {{prime.diagnoses.collected.value}}"
```

A collect target that a profile switched off contributes no slot, and nothing after a pass that broke the loop is collected. A pass dropped under `on_error: continue` is, today, the exception; [Errors in a loop](#errors-in-a-loop) says what it leaves.

## Four read forms, one per question

This is the table to keep. Four *different* questions get four *different* paths, and substituting one for another does not fail — it renders something plausible and wrong.

| You want | Write | Legal where |
| --- | --- | --- |
| A step's output in the **current pass** | `{{prime.<step>.value}}` | inside the body, and inside the `while` condition |
| One **specific past pass** | `{{prime.<loop>.iter_<N>.<step>.value}}` | **after** the loop only |
| The **final completed pass** | `{{prime.<loop>.last.<step>.value}}` | **after** the loop only |
| **Every** pass's output of one step | `{{prime.<loop>.collected.value}}` | after the loop (requires `collect`) |

The rules of the form:

- **Resolution is a scope chain.** Inside the body, `prime.<step>` means the current iteration first, then the enclosing scope, then root. A root input or an effect that ran before the loop keeps resolving; a body step named the same as an outer effect shadows it, inside the body only.
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

An **unnamed** loop is transparent: the body writes at stable paths in the enclosing scope, and each pass overwrites the last. No `iter_<N>`, no `last`, no `collected`. It is the right shape when only the final pass matters and nothing downstream needs the history. `collect` on an unnamed loop has nowhere to write and silently aggregates nothing:

```yaml
# ✗ runtime — no name, so no collected node: the array downstream expects is never written
- type: loop
  collect: diagnose
  each: {in: input.failures, as: test}
  body:
    - type: prompt
      name: diagnose
      template: "Explain why this test fails, in two sentences: {{test}}"
```

**Termination reasons** — every completed named loop records one: `collection_exhausted` (every element done), `condition_false` (the `while` said stop), `max_iterations_reached` (the cap you set: a `while` that never said stop, or a truncated `each`, which also records `unvisited`), `collection_unresolved` (`each.in` was not an array), `condition_error` (the `while` condition could not be evaluated, under `break` or `continue`), `error` (the loop failed; `termination.detail` and `meta.error` say why). They are CEL-readable — `state.prime.candidates.value.termination.reason == 'max_iterations_reached'` is a fine thing to branch on after a loop whose patches never applied.

## Errors in a loop

`on_error` on a loop governs a failed *pass*: `fail` (default) propagates; `break` ends the loop at the failed pass, keeping what completed before it; `continue` drops the pass and goes on. In a `tree` loop every pass is already running, so `break` and `continue` both let the other passes finish and drop the failed ones. Under `break` and `continue` alike, `last` is the last pass that completed, never the failed one, and under `break` in a `chain` loop the failed pass is absent from `collected`.

Under `continue`, and under `break` in a `tree` loop, `collected` is not yet that clean. The runtime assembles it from the first *k* passes, where *k* counts the passes that completed. So the failed pass leaves a `null` in the array, and an `each` loop loses one pass from the end for each failure: failing tests `[test_a, test_b, test_c]` with `test_b`'s diagnosis failing collect as `[diagnosis-of-a, null]`. Until that is fixed, read the `iter_<N>` nodes when such a loop has had failures; each failed pass is an `iter_<N>` whose step has a `null` value and an error in `meta`.

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
