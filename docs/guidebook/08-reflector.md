# Reflector

> "Control mechanisms that lay their own plans." — Gordon Pask, *An Approach to Cybernetics* (1961)

`if` chooses between plans that were written in advance. `loop` repeats one. The **reflector** writes the plan: it asks a model to *generate* the effects to run next, validates what came back against the same schema every hand-written document passes, executes it, and — if asked — does it again with the results in hand. It is the system planning itself, and it is the effect to reach for only when the steps cannot be known up front.

That last clause is the whole design guidance. A reflector costs a planning call per cycle, produces a topology you did not review, and is bounded only by the limits you set on it. When you *can* write the steps, write them: a chain is cheaper, faster, and auditable before it runs. The reflector is for the issue that arrived this morning — the steps to fix it depend on what is broken now, and no one could have written them yesterday.

## The shape

```
Reflector ::= { type: 'reflector', name: NAME, effects: Effect+,   — the planning dynamic, run each cycle
                plan_from_step?: NAME,                   — default propose_steps
                max_iterations?: INT≥1,                  — planning cycles; default 1
                max_effects?: INT≥1,                     — effects per plan; default 8
                stop_on_done?: BOOL,                     — default true
                generated_key?: STRING,                  — default generated
                prime_template?: STRING, flow?: Flow, description?: STRING }
```

`name` and a non-empty `effects` list are required. Among the inner effects, one prompt — `propose_steps` by default, or whatever `plan_from_step` names — is the planner: its output *is* the plan.

## A reflector plans a fix

```yaml
- type: reflector
  name: plan_fix
  max_effects: 4
  max_iterations: 2
  effects:
    - type: prompt
      name: propose_steps
      template: |
        Issue #42: parse_duration("1h30m") raises ValueError: invalid duration. "90m" works.
        The error is raised in src/durations.py, where one regular expression matches one number and one unit.
        Plan the steps to find the cause and describe the fix. Output must follow the OUTPUT CONTRACT exactly.
```

Each planning cycle runs in five phases:

1. **Render the prime directive.** A planning instruction — Circuitry ships a versioned default, `REFLECTOR_PRIME_V1` — is rendered with `max_effects` and two slots, a **goal** and a **context**. In the current runtime both slots render empty; [what the planner can see](#what-the-planner-can-see) below explains why.
2. **Run the inner dynamic** with the rendered prime prepended to the planning prompt's template. The planner sees the directive, then whatever the template says — which is why the example above writes the situation into the template itself.
3. **Extract the plan** from the planning step's output: a YAML document with a `done` flag and an `effects` list. Code fences are tolerated; markdown is not.
4. **Decide whether to stop.** An empty `effects` list stops. `done: true` stops when `stop_on_done` is set (the default) — *without executing that cycle's effects*. `done` means "there is nothing left to do", so a planner that still has work to run must say `done: false`; the last useful plan is always a `done: false` one, and the cycle after it says `done: true` with nothing to run.
5. **Validate and execute** the plan through `use(inline)` — full schema validation, the same allowlists, the same cycle guards — as a state-isolated child under `prime.<name>.generated.iter_<cycle>`. Then, if cycles remain, plan again.

After `max_iterations` cycles, or an earlier stop, the reflector's own `value` is `true`. Any cycle that fails — the planner errored, the plan would not parse, the generated orchestration failed — fails the reflector (`value: false`, the reason recorded in `meta.iterations`) and with it the run: a reflector has no `on_error` of its own, so put one in its own document and call it through a `use` with `on_error: skip` if a failed plan must be survivable.

## What the plan looks like

The directive fixes the output shape, and it is deliberately narrow:

```yaml
done: false
effects:
  - type: prompt
    name: reproduce
    template: "Write a pytest test that calls parse_duration with the text 1h30m. The test expects 5400."
  - type: dynamic
    name: locate
    flow: chain
    effects:
      - type: prompt
        name: read_pattern
        template: "Explain which texts the regular expression (\\d+)([hms]) matches with re.fullmatch."
      - type: prompt
        name: propose_fix
        template: "Write a regular expression that matches one or more pairs of a number and a unit."
```

Every generated effect is an ordinary effect — the plan is a Circuitry orchestration, and it passes the same gate as one you wrote. `max_effects` caps the number of top-level effects per cycle (`8` by default; the alias `max_steps` is accepted).

The directive also fixes the plan's *language*: [ASD-STE100 Simplified Technical English](https://www.asd-ste100.org/) — one instruction per sentence, active voice, imperative mood, a twenty-word ceiling on procedural sentences, one meaning per word, no vague verbs, articles never dropped — applied to the plan's step descriptions and to every generated `template`. A narrowed output distribution is what makes generated plans predictable to validate and small planner models sufficient to write them. `prime_template:` replaces the directive wholesale, and with it the constraint; it is the author's choice to keep it.

## Isolation, and what the plan can see

A generated plan runs as a child with **isolated state**: like any `use` child, it cannot read the parent's `prime` or `input` namespaces. A generated template that references `{{prime.context.search.value}}` renders empty. This is by design — the plan is a self-contained document, and everything its prompts need must be in their templates — and it is why the planning template should carry the specifics: the planner writes them into the plan.

The parent, on the other hand, sees everything the plan produced:

```
prime.plan_fix.value                                    # true when planning finished
prime.plan_fix.meta.iterations                          # one record per cycle: done, stop, parsed, error, plan_text
prime.plan_fix.inner.propose_steps.value                # the last planner reply
prime.plan_fix.generated.iter_0.reproduce.value         # a generated effect's output
prime.plan_fix.generated.iter_0.locate.propose_fix.value
```

Generated effects are addressable at exactly the paths their names dictate, one level under the cycle they ran in — so a downstream prompt can read a plan's results by name once it knows the plan's vocabulary, and a `use` effect elsewhere can wrap the reflector to map specific outputs.

### What the planner can see

Today, three things: the directive, the literal text of its own template, and the reflector's own node. Two things that look like channels into the rest of the run are not:

- **The directive's goal and context slots.** They are meant to carry the value of a root-level effect named `goal` and the run's effective settings. The directive looks both up in the store the reflector runs in, which holds `prime` rather than the state root, so it finds neither and both slots render empty. A `goal` effect before the reflector is harmless, and the planner never reads it.
- **Root references in the planning prompt.** The inner planning dynamic is rendered against the reflector's own node rather than the run's root, so `{{prime.…}}` and `{{input.…}}` written into `propose_steps` render empty too.

The node itself is readable, with paths relative to it. From the second cycle on, `{{{inner.propose_steps.value}}}` is the previous plan (triple-stache: it is YAML), and `{{generated.iter_0.reproduce.value}}` is what one of its effects produced. That is how a planner works "with the results in hand": name what the last cycle ran, and ask what comes next.

So write into the planning template, as plain text, everything the planner must know about the rest of the run. That keeps a reflector honest about what it is: a planner for a situation you can describe when you write the document, not one that reads the run. When the plan must depend on what the run produced, generate the plan with an ordinary prompt, which can read state, and run it with a `use` effect's `inline:`, as [Composition](09-composition.md) shows.

## Bounding the feedback

Three limits, all worth setting explicitly:

- `max_iterations` — planning cycles. Each cycle is a planning call *plus* everything the plan runs.
- `max_effects` — the plan's width.
- `stop_on_done` — whether the planner's own `done: true` ends the loop. Turn it off only when you want exactly `max_iterations` cycles regardless.

And one switch outside the document: a [profile](04-configuration.md) with `effects.plan_fix.enabled: false` turns agentic planning off for a single run, writing a skip node in its place. It is the effect most worth switching off — for a cost-controlled rerun, a deterministic test, a demo where the plan should not vary.

```yaml
# profiles/no-planning.yml
effects:
  plan_fix:
    enabled: false
```

## Anti-patterns

**A reflector where a chain would do.** If you can name the steps, name them. The reflector's cost is a plan you did not review.

**A planning template with no specifics.** "Plan the next steps" is all the planner gets, so it plans generic steps. Write the situation into `propose_steps`, as plain text.

**A plan that reads parent state.** Rendered empty; the plan must carry its own specifics.

**Unbounded cycles.** `max_iterations: 10` with `stop_on_done: false` is ten plans and everything they run. Set the number you would accept.

**Replacing the prime casually.** `prime_template` opts out of the output contract and the STE constraint at once. Keep the contract if you keep anything.

## See also

- [Orchestration Reference → `reflector`](../orchestration-reference.md#reflector).
- [`learn/reflector`](../../src/circuitry/curation/learn/reflector.yml) · `REFLECTOR_PRIME_V1` in `src/circuitry/core/primes.py`.
- [Decomposition](11-decomposition.md) — the runtime's own reflector: a planner that splits over-complex prompts.
