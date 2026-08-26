# Prompt decomposition

Everything above, composed. "Cook Thanksgiving dinner for twelve" is too much for one prompt — a single model call asked to plan, cook, and plate a dozen dishes does all of them badly — and when that prompt scores over the decomposition threshold, the runtime *knows* it. What it does next uses every part of the language so far: the prompt goes to a **planner that is itself an orchestration** (chapter 8's reflector, purpose-built); the planner emits a fan-out-and-merge document (chapter 2's tree and chain); the document is validated and run as an isolated child (chapter 9's `use`); the merged result is written back at the original prompt's own state path (chapter 3's determinism), so the effect after it reads `{{prime.<name>.value}}` and never knows dinner was ever in pieces.

This is the third of the three complexity switches, and it needs the first: decomposition consumes scores, so `scoring.enabled` must be on.

## Turning it on

```json
{
  "runtime": {
    "complexity": {
      "scoring": {"enabled": true},
      "decomposition": {
        "enabled": true,
        "threshold": 45,
        "max_chunks": 5,
        "max_depth": 1,
        "on_failure": "route_up"
      }
    }
  }
}
```

| Field | Default | Meaning |
| --- | --- | --- |
| `threshold` | `80` | A prompt whose score **strictly exceeds** this is decomposed. A score equal to it does not trigger. 0–100. |
| `max_chunks` | `8` | The widest fan-out a plan may propose. |
| `max_depth` | `2` | Recursion ceiling: chunks are scored too, and an over-complex chunk decomposes again, down to this depth. |
| `on_failure` | `route_up` | `route_up` runs the original prompt on a stronger model instead; `fail` propagates the error. |

`cof run … --decompose` forces the switch on for one run (with `--scoring`, or scoring already on); `--no-decompose` forces it off.

## What a decomposition does

When a prompt's recorded score exceeds `threshold`, the runtime replaces the single model call with three bounded steps.

**1. Plan.** The bundled planner — `agents/decompose`, a curation entry you can read and run yourself — runs in an isolated store. It is handed the effect's raw template, a listing of the inputs the template reads, the effect's declared output shape (`prompt_type` and `schema`), and `max_chunks`. Its directive (`DECOMPOSE_PRIME_V1`) is specific: *split on the seam that is already in the prompt* — one chunk per numbered instruction, per input document, per output field, per entity — and *do not invent a pipeline the work does not have*. The planner's own run has decomposition switched off; its planning prompt would otherwise trigger the feature it implements.

**2. Validate.** The emitted YAML must parse, pass the full orchestration schema, keep the fan-out within `[2, max_chunks]` — a "decomposition" into one chunk is the original prompt with scaffolding and is refused — and honour the merge contract: a top-level effect named `merge`, producing the same `prompt_type` and `schema` as the source, with `interface.outputs.result` pointing at `prime.merge.value`, and `interface.inputs` declaring every input the source template read, under the same names. An invalid plan never runs.

**3. Execute and write back.** The plan runs as a state-isolated child seeded with a *copy* of the effect's render context — the same inline-identity cycle guard as a `use` child, the same namespaced observability (chunk effects announce under the decomposing effect's node, and live-state snapshots mirror them there). The value at `prime.merge.value` is written at the **original effect's own path**. Downstream, nothing changes.

Thanksgiving, decomposed, looks like a plan you might have written yourself:

```yaml
interface:
  inputs:
    guests: {type: number, required: true}
    dietary_notes: {type: string, required: false}
  outputs:
    result: {path: prime.merge.value, type: string}

effects:
  - type: dynamic
    name: courses
    flow: tree
    effects:
      - type: prompt
        name: turkey
        template: "Write the plan to roast a turkey for {{input.guests}} guests. Note timing."
      - type: prompt
        name: sides
        template: "Write the plan for three side dishes for {{input.guests}} guests. Respect: {{input.dietary_notes}}"
      - type: prompt
        name: dessert
        template: "Write the plan for two desserts for {{input.guests}} guests."
  - type: prompt
    name: merge
    template: |
      Combine these plans into one timeline for the day, oven by oven.
      Turkey: {{prime.courses.turkey.value}}
      Sides: {{prime.courses.sides.value}}
      Desserts: {{prime.courses.dessert.value}}
```

Courses that cook in parallel, and a merge that plates them.

## Bounded at every edge

**Depth.** Chunks are scored inside the child, so an over-complex chunk decomposes again — and `max_depth` bounds that recursion. At the ceiling the effect **routes up** rather than decomposing: it runs once on the routing table's catch-all (most capable) band model — or, with routing off, simply runs as-is — and the depth it stopped at is recorded. The ceiling is checked before the planner runs, so a depth-limited effect costs no extra calls.

**Failure.** `on_failure` governs the three real failure paths — the planner failing, an invalid plan, the child execution failing. `route_up`, the default, falls back exactly like the depth ceiling: the original prompt runs on the catch-all band model, the failure is recorded under `meta.decomposition.reason`, and the run lives. **A decomposition attempt can never turn a working run into a failed one.** `fail` propagates like any other effect error, under the effect's own `on_error`. Either way the child's scratch state is discarded whole; nothing partial ever lands at the original path. `respect_explicit` from the routing block covers the fallback too: an effect that pins its own `model:` keeps it when route-up would otherwise substitute one.

**Record.** `meta.decomposition` on the effect node carries the full decision — `{decomposed, outcome, reason, score, threshold, depth, max_depth, plan, chunk_count, yaml, result_path, fallback_model, error}` — whether it decomposed, routed up, ran as-is, or failed. Like `meta.complexity`, the key is absent entirely when the feature never triggered.

## Reading the plans: `--decompose-out`

`meta.decomposition.yaml` carries the generated orchestration, but a run that finishes and is never looked at makes that plan effectively unreadable. `--decompose-out <dir>` writes every plan out, one file per decomposed effect:

```bash
cof run thanksgiving.yml --scoring --decompose --decompose-out ./plans -e guests=12
```

```
plans/
  3f2c9e7a-…__prime.plan_dinner.yml
```

Each file is `<run_id>__<effect_path>.yml`, the planner's YAML verbatim, headed by a comment block naming the run, the effect, the outcome, and the merge contract — plain comments, so `cof run ./plans/<file>.yml` runs it as an ordinary document. Failed plans are written too, with `# status: failed` and the error: a plan that failed validation or whose execution failed is the one worth reading, and a planner payload that failed its own envelope is written as `.rejected.yml`. This is the loop that grows the library — a good plan is a candidate `recipes/` entry, promoted by hand.

## Why the seams matter

Decomposition is where the design pays off all at once. The planner can emit a fan-out because the *dynamic* makes parallel composition a first-class thing. The runtime can run the plan safely because *use* isolates it and *validation* gates it. The merged result can be substituted invisibly because every effect's path is *deterministic* — `prime.plan_dinner.value` means the same thing whether one model call or a child orchestration produced it. And the decision is auditable because everything that happened was written to `meta`. None of that was built for decomposition; decomposition is what you get when the pieces fit.

## Anti-patterns

**A threshold at the default with a small local model.** `80` is conservative. If your model struggles at 45, set 45 — `cof score` shows you where your prompts sit.

**Decomposition without routing.** Legal, and the route-up fallback then "runs as-is" — meaning a failed decomposition retries the original prompt on the same model that was too small for it. Pair the two switches.

**Expecting chunks to read the parent's state.** They read the *source effect's inputs*, which the plan declares; the chunks are a self-contained document.

**Never reading the plans.** `--decompose-out` costs nothing. The plans are how you learn what the planner thinks your prompts are made of — and where the next library entry comes from.

## See also

- [Complexity Configuration → `decomposition`](../complexity-config.md#decomposition) — every field, error, and the exact `meta.decomposition` record.
- [`agents/decompose`](../../src/circuitry/curation/agents/decompose.yml) — the planner, as an orchestration you can run.
