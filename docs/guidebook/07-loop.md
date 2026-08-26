# Loop

The loop body executes, writes state, and the continuation condition evaluates against that new state — each iteration's output is the next iteration's input. This is feedback in the literal cybernetic sense: the output of the system is fed back as input, and the system decides from what it observes whether to go around again.

Two kinds of loop answer two kinds of question. `each` asks "for every one of these?" and walks a collection. `while` asks "again?" and consults a condition — a CEL expression, or the model tasting the dish. Both are bounded by `max_iterations`, the deliberate floor under runaway feedback.

## The shape

```
Loop ::= { type: 'loop', body: Effect+,
           each: { in: STATE_PATH, as?: NAME }           — in resolves to an array; as defaults to item
           ⊕ while: Condition,                           — checked before each pass; always sequential
           name?: NAME,                                  — named ⇒ iter_<N> / last / collected nodes
           collect?: NAME,                               — a body step; needs a named loop
           flow?: 'chain' | 'tree', max_concurrency?: INT≥1,   — each loops only
           max_iterations?: INT,                         — default 100
           min_iterations?: INT,
           on_error?: 'fail'|'break'|'continue',
           labels?: MAP, description?: STRING }
```

`body` and exactly one of `each` / `while` are required.

## `each` — over a collection

```yaml
- type: loop
  name: courses
  each:
    in: prime.menu.plan_courses.value
    as: course
  collect: cook
  body:
    - type: prompt
      name: cook
      template: "Write the cooking steps for: {{course}}"
```

`each.in` is a root-relative state path that must resolve to an array at run time — `input.menu` for a caller-supplied list, `prime.menu.plan_courses.value` for the output of an `array`/`json` prompt, or a `runtime.` path. `as` names the current element for the body (`item` by default). The body runs once per element, in order.

The path is checked statically for its root — a bare key or a `state.` spelling is a hard error from `cof check`, with the fix named — and dynamically for its type. A path that resolves to nothing, or to something that is not a list, ends the loop with termination reason `collection_unresolved` and zero passes. Never silent success: a text prompt that was supposed to produce a list, and produced prose, is caught here.

```yaml
# ✗ runtime — plan_courses is text; each.in needs an array, so the loop terminates collection_unresolved
- type: prompt
  name: plan_courses
  template: "List three courses for {{input.occasion}}."
- type: loop
  name: courses
  each: {in: prime.plan_courses.value, as: course}
  body:
    - type: prompt
      name: cook
      template: "Write the cooking steps for: {{course}}"
```

The fix is upstream: `prompt_type: array` with a schema, so the producing prompt is held to the shape the loop needs.

**In parallel.** An `each` loop is a chain by default. `flow: tree` runs every pass concurrently — the loop's *iterations* fan out, while the steps inside one pass still run in order — and `max_concurrency` bounds the pool. Results are assembled in the original order whatever order they finished in:

```yaml
- type: loop
  name: courses
  flow: tree
  max_concurrency: 4
  collect: cook
  each: {in: prime.menu.plan_courses.value, as: course}
  body:
    - type: prompt
      name: cook
      template: "Write the cooking steps for: {{course}}"
```

## `while` — until the condition says stop

```yaml
- type: loop
  name: season
  while:
    mode: model
    template: "Taste this. Does the seasoning need adjusting?\n\n{{prime.adjust.value}}"
  min_iterations: 1
  max_iterations: 5
  body:
    - type: prompt
      name: adjust
      template: "Adjust the seasoning of {{prime.check_diet.main_course.value}} and describe the dish now."
```

The condition is evaluated *before* each pass, and it sees the pass that just finished under the same within-iteration names the body uses — `{{prime.adjust.value}}` in the condition is the latest adjustment. Before the first pass there is nothing to see yet, and the name falls through to the enclosing scope (empty, here). `min_iterations` forces that many passes regardless of the answer — the way to say "always taste at least once" — and `max_iterations` (default `100`) stops the loop whatever the model thinks. A `while` loop is always sequential; `flow` does not apply.

Model mode wraps the template the way `if` does — *"… Should the loop continue? Answer (yes/no):"* — so phrase it as a question whose *yes* means "go around again". CEL mode is deterministic, and reads whatever the previous pass left in state:

```yaml
- type: prompt
  name: guest_count
  prompt_type: number
  template: "How many guests are coming to {{input.occasion}}? Reply with a number only."
- type: loop
  name: courses_until_full
  while:
    mode: cel
    expr: "state.prime.guest_count.value > 6"
  max_iterations: 3
  collect: course
  body:
    - type: prompt
      name: course
      template: "Suggest one more course for {{input.occasion}}, pass {{_loop_index}}."
```

One thing a CEL condition cannot do is read the loop's own `collected` or `last` — both are written when the loop *completes*, so from inside the loop the path is missing, the expression errors, and an erroring expression is `false`. A `while` written that way runs zero passes and terminates `condition_false`. *Where* you read loop state matters as much as *what* you read; the next section is the map.

### Refining across passes

The condition sees the previous pass. The *body* of a named loop does not: inside the body, `{{prime.adjust.value}}` means *this pass's* `adjust`, which has not been written yet when the pass begins, so it renders empty. `last` and `iter_<N>` are post-loop spellings. There is no spelling in a named loop for "the pass before this one".

When the body needs to build on its own previous output — taste, adjust, taste again — use an **unnamed** loop and let the body overwrite the same name it reads:

```yaml
- type: prompt
  name: dish
  template: "Describe {{prime.check_diet.main_course.value}} as first plated, seasoning included."
- type: loop
  while:
    mode: model
    template: "Taste this. Does the seasoning still need adjusting?\n\n{{prime.dish.value}}"
  max_iterations: 5
  body:
    - type: prompt
      name: dish
      template: "Adjust the seasoning and describe the dish now:\n{{prime.dish.value}}"
- type: prompt
  name: card
  template: "Write the menu line for: {{prime.dish.value}}"
```

An unnamed loop writes into the enclosing scope, so each pass's `dish` replaces the last: the body reads the previous pass, the condition tastes the latest, and after the loop `prime.dish.value` is the final version with no `last` to spell. The seed prompt before the loop gives the first pass something to read. What you give up is history — no `iter_<N>`, no `collected` — which is the right trade when only the final state matters. Keep the loop named when the record of every pass is the point.

## `collect`

`collect: <step>` gathers one body step's value from every pass into an array at `prime.<loop>.collected.value`, in pass order. It needs a named loop — the array has to live somewhere — and it is the usual way a loop hands its work to the effect after it:

```yaml
- type: prompt
  name: menu_card
  template: "Write the menu card from these courses: {{prime.courses.collected.value}}"
```

A pass that was skipped or broke on error contributes nothing; `collected` reports only values that were actually produced.

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
  name: courses
  each: {in: input.menu, as: course}
  body:
    - type: prompt
      name: prep
      template: "List the prep for: {{course}}"
    - type: prompt
      name: cook
      template: "Cook, given this prep: {{prime.courses.iter_0.prep.value}}"
```

The correct within-pass read is `{{prime.prep.value}}`. [Troubleshooting State Paths](../troubleshooting-state-paths.md) has a symptom table — what `meta.prompt_sent` looks like for each wrong form — for the day one slips through.

## Named or not

A **named** loop records everything above: `iter_<N>` per pass, `last`, `collected`, and a summary at the node itself:

```
prime.courses.value.iterations               # passes completed
prime.courses.value.termination.reason       # why it stopped (below)
prime.courses.value.effects_by_iteration     # what each pass ran
prime.courses.meta.mode                      # "each" / "while"
prime.courses.meta.max_iterations / min_iterations
prime.courses.meta.each_in_path / each_as    # each loops
prime.courses.iter_0.cook.value
prime.courses.last.cook.value
prime.courses.collected.value
```

An **unnamed** loop is transparent: the body writes at stable paths in the enclosing scope, and each pass overwrites the last. No `iter_<N>`, no `last`, no `collected`. It is the right shape when only the final pass matters and nothing downstream needs the history. `collect` on an unnamed loop has nowhere to write and silently aggregates nothing:

```yaml
# ✗ runtime — no name, so no collected node: the array downstream expects is never written
- type: loop
  collect: cook
  each: {in: input.menu, as: course}
  body:
    - type: prompt
      name: cook
      template: "Write the cooking steps for: {{course}}"
```

**Termination reasons** — `collection_exhausted` (every element done), `condition_false` (the `while` said stop), `max_iterations` (the cap), `collection_unresolved` (`each.in` was not an array), `error` (a pass failed under `fail` or `break`). They are CEL-readable — `state.prime.season.value.termination.reason == 'max_iterations'` is a fine thing to branch on after a taste loop that never converged.

## Errors in a loop

`on_error` on a loop governs a failed *pass*: `fail` (default) propagates; `break` ends the loop at the failed pass, keeping what completed before it; `continue` drops the pass and goes on. Under `break` and `continue`, the dropped pass is absent from `last` and `collected`. In a `tree` loop, `continue` lets the other passes finish and drops the failed ones.

## Inside nested containers

`{{course}}` and `{{_loop_index}}` reach every effect in the body however deeply it is wrapped — an `if` branch, a grouping `dynamic`, an inner loop. An inner loop's `as` shadows the outer's if they share a name; give them different names.

```yaml
- type: loop
  name: courses
  each: {in: input.menu, as: course}
  body:
    - type: if
      if: {mode: cel, expr: "state.input.diet == 'vegetarian'"}
      then:
        - type: prompt
          name: cook
          template: "Write vegetarian cooking steps for course {{_loop_index}}: {{course}}"
      else:
        - type: prompt
          name: cook
          template: "Write the cooking steps for course {{_loop_index}}: {{course}}"
```

## Anti-patterns

**Iterating prose.** `each.in` on a `text` prompt. Make the producer an `array` prompt with a schema.

**Reading `collected` or `last` from inside the loop.** Both are written when the loop completes. Inside, read `{{prime.<step>.value}}`.

**`iter_0` inside the body.** Stale data with a warning. Above.

**Unbounded feedback.** A model-mode `while` with `max_iterations: 100` and a question the model tends to answer *yes* to is a hundred model calls. Set the cap to the number of passes you would accept, and make the condition ask for *stop* evidence, not *continue* enthusiasm.

**A plural body.** `summarize_articles` as a single prompt inside the loop is a sign the loop is not doing the iterating. One element, one singular step.

## See also

- [Orchestration Reference → `loop`](../orchestration-reference.md#loop) and [Referencing a sibling within an iteration](../orchestration-reference.md#referencing-a-sibling-within-an-iteration).
- [`learn/loop`](../../src/circuitry/curation/learn/loop.yml) · [`patterns/critique_refine_loop`](../../src/circuitry/curation/patterns/critique_refine_loop.yml).
