# Complexity Configuration — `runtime.complexity`

`runtime.complexity` is the single config surface for complexity-aware
execution: scoring each prompt effect, routing a score to a model, and
decomposing an over-complex effect into chunks.

Everything is **off by default**. A config with no `complexity` block behaves
exactly as it did before the block existed.

## Configuration reference

```json
{
  "runtime": {
    "complexity": {
      "scoring": {
        "enabled": false,
        "weights": {"prompt_size": 1.0, "state_references": 1.5},
        "keywords": {"migrate": 8, "refactor": 5}
      },
      "routing": {
        "enabled": false,
        "respect_explicit": true,
        "bands": [
          {"name": "cheap", "max": 40, "model": "small"},
          {"name": "mid", "max": 75, "model": "medium"},
          {"name": "top", "model": "large"}
        ]
      },
      "decomposition": {
        "enabled": false,
        "threshold": 80,
        "max_depth": 2,
        "max_chunks": 8,
        "on_failure": "route_up"
      }
    }
  }
}
```

The same block in an orchestration's `runtime:` mapping, in YAML:

```yaml
runtime:
  complexity:
    scoring:
      enabled: true
    routing:
      enabled: true
      bands:
        - {name: cheap, max: 40, model: small}
        - {name: top, model: large}
```

## The three switches

| Switch | Default | Requires | What it turns on |
| --- | --- | --- | --- |
| `scoring` | `false` | — | The deterministic scorer: every prompt effect gets a complexity score and an explainable breakdown. |
| `routing` | `false` | `scoring` | Band-based model selection from the score. |
| `decomposition` | `false` | `scoring` | Splitting an effect that scores above `threshold` into chunks. |

The switches are independent apart from one ordering constraint: **scoring is
the substrate**. Routing and decomposition each read scores, so enabling either
without `scoring.enabled: true` is a config error naming the missing
prerequisite. Routing and decomposition do *not* require each other — score
only, score + route, score + decompose, and all three are each valid.

## Fields

### `scoring`

| Field | Type | Default | Description |
| --- | --- | --- | --- |
| `enabled` | boolean | `false` | Turns the scorer on. |
| `weights` | object | see below | Relative multiplier per signal. Only the signals you name are overridden; the rest keep their defaults. |
| `keywords` | object | `{}` | Keyword → weight table for deterministic string matching. Keys are free-form; values must be numbers. |

Scores are normalized into a bounded **0–100** range, so band tables and
thresholds are portable between orchestrations.

Recognised signals and their default weights:

| Signal | Default weight | Measures |
| --- | --- | --- |
| `prompt_size` | `1.0` | Token estimate of the template, or of the rendered prompt when available. |
| `state_references` | `1.5` | Distinct state references in the template. |
| `prompt_type` | `0.75` | `text` versus `array`/`json` output. |
| `output_schema` | `1.25` | Depth and breadth of a declared output schema. |
| `output_size` | `1.0` | Expected output size (schema `maxItems`, declared limits). |
| `structural_position` | `0.5` | Nesting depth, loop body, reflector-generated. |
| `keywords` | `1.0` | Contribution of the `keywords` table above. |

**These names are the only names.** They are what you configure here, what the
scorer reports, what the runtime writes under `meta.complexity.signals`, and
what the TUI's breakdown pane prints — one vocabulary end to end
(`circuitry.core.complexity.SIGNAL_NAMES` is the source, and this table is
validated against it), so a weight you set always reaches the signal it names.

An unrecognised signal name is an error rather than a silent no-op, so a typo
in `weights` cannot quietly do nothing:

```
runtime.complexity.scoring.weights: unknown signal 'structural-position'. Valid
signals: keywords, output_schema, output_size, prompt_size, prompt_type,
state_references, structural_position.
```

The error is raised at **config resolution** — before the first effect
dispatches — so a stale weight name stops the run at startup rather than
warning into a log mid-run.

To see what these settings produce for a given orchestration before running it,
use [`cof score`](./score-command.md).

#### What scoring writes to state

With `scoring.enabled: true`, every prompt effect's state node gains a
`meta.complexity` entry:

```json
"prime": {
  "triage": {
    "value": "...",
    "meta": {
      "adapter": "ollama",
      "model": "llama3",
      "model_reason": "default",
      "complexity": {
        "score": 34.7,
        "max_score": 100.0,
        "mode": "rendered",
        "estimated": false,
        "weight_total": 7.0,
        "signals": {
          "prompt_size": {
            "raw": 412.0, "normalized": 0.34, "weight": 1.0,
            "contribution": 4.86, "note": "~412 tokens of rendered prompt"
          },
          "state_references": { "...": "..." }
        },
        "warnings": []
      }
    }
  }
}
```

Three properties are contractual:

- **Written before dispatch.** The entry lands in the same meta block that
  records the resolved adapter and model, before the adapter is called — so it
  is still on the node when the prompt *fails*, which is when you most want to
  know how hard the prompt was. It is likewise present on a `--dry-run` node.
- **Absent, not empty, when disabled.** With scoring off there is no
  `complexity` key at all — not `null`, not `{}`. A run with the switch off
  produces the same state tree as a build without the feature.
- **Addressable from CEL.** `signals` is keyed by signal name rather than being
  a list, so an orchestration can branch on how hard its own step was:

  ```yaml
  - type: conditional
    name: gate
    if:
      mode: cel
      expr: "state.prime.triage.meta.complexity.score > 60.0"
    then:
      - {type: prompt, name: deep_review, template: "..."}
    else:
      - {type: prompt, name: quick_pass, template: "..."}
  ```

`mode` is always `rendered` at runtime — the score measures the prompt actually
sent, interpolated state included, not the template. Per-signal `detail` (the
counted reference names, the normalization tables) is not persisted — it would
multiply the size of every prompt node to explain what each signal's `note`
already summarizes. Re-score the effect with
`circuitry.core.complexity.score()` to get it back.

### `routing`

| Field | Type | Default | Description |
| --- | --- | --- | --- |
| `enabled` | boolean | `false` | Turns band routing on. Requires `bands`. |
| `bands` | array | — | Ordered band table (below). |
| `respect_explicit` | boolean | `true` | Keeps explicit model choices (`--model`, a per-effect `model:`, a profile override) winning over the router. |

With `enabled: true`, each prompt effect's score picks a band and that band's
`model` is what dispatches — recorded on the node as `meta.model` with
`meta.model_reason: router`, and on the run as `sources["model"] == "router"`.
The substitution happens at the single place a model is resolved, so the routed
model behaves in every downstream respect like one the effect had named itself:
it is the default model for the `provider`/`provider_fallbacks` attempt chain,
it is what `--verbose` reports, and it is what a retry re-sends.

A band is `{"name": "...", "max": <number>, "model": "..."}`. `name` is an
optional label. `max` is the band's **inclusive** upper bound: a score matches
the first band whose `max` is greater than or equal to it, so a score of
exactly `40` matches a band with `max: 40`. A band with no `max` is the
**catch-all**; it is required and must be last, so every score resolves to a
model.

Band tables are validated at config resolution — never mid-run. These all fail
with a message naming the offending entry:

- an empty table, or one with no catch-all;
- a catch-all that is not the last entry;
- `max` values that are not strictly ascending (unordered or overlapping bands);
- a non-numeric `max`, a `max` outside 0–100, a missing or empty `model`, or an
  unknown key in a band object.

Bands are validated even when `routing.enabled` is `false`, so a broken table
surfaces before you flip the switch.

Model names pass through untouched: for the `cyberdiner` adapter they are tier
names and expo is the authority; for local adapters they are real model names.
The router resolves a band to a string and does not interpret it.

#### Where the router sits: model precedence

```
profile effect override (model)  >  per-effect model:  >  --model  >
profile effect override (routing pin)  >  ROUTER  >
orchestration model:  >  config default_model
```

Everything above the router is a model a human named on purpose, and the router
never overrules one. A profile's per-effect `routing:` pin (see
[Per-Effect Routing Control](profiles.md#per-effect-routing-control)) sits
just above the router itself: it beats the router's own score-based choice,
but a genuinely explicit model — this effect's own `model:`, `--model`, or a
profile's run-level `model:` — still beats the pin. Everything below the
router is a default the router exists to replace. Consequences worth stating
out loud:

- **A per-effect `model:` beats `--model`.** This predates routing: `--model`
  sets the run *default*, and an effect that names its own model has always
  opted out of the run default whatever supplied it. Use a profile's `effects:`
  block to retarget a specific effect from outside the orchestration.
- **A profile `routing:` pin does not need scoring on.** It names a band's
  model directly rather than deriving it from a score, so it resolves the
  same whether or not `scoring`/`routing.enabled` are switched on elsewhere —
  only the band table (`routing.bands`) has to exist.
- **The band is recorded even when the router defers, or an effect opts out.**
  On an effect that pins its own model, or one a profile opted out of routing,
  you still get `meta.complexity.band`, answering "what would routing have
  picked" — which is the question you ask right before removing the pin.
  `meta.model_reason` is the field that says whether the band was *applied*;
  `band` alone never implies it was.

Turning routing off restores the previous behaviour exactly: with
`routing.enabled: false` the state tree — model, `model_reason`, and every
other key — is byte-identical to a build without the feature.

`respect_explicit: false` is the opt-out, and it means what it says: the router
then decides for **every** scored prompt effect, including ones that name their
own `model:` and runs launched with `--model`. It is the knob for "this run
routes, full stop" — a cost sweep, a bulk re-run — and it is off by default
because overruling a deliberate choice is the surprising behaviour.

A band table is by itself a sufficient model configuration. If routing is on
and no `--model`, orchestration `model:`, or `default_model` is set, the
catch-all band — whose whole job is "the model for anything not otherwise
matched" — becomes the run default, so `cof run` no longer refuses with "no
model resolved". Effects that cannot be scored (and every non-prompt effect)
run on it.

#### Watching it happen: `cof run --explain-routing`

`cof score` previews scores before a run; `--explain-routing` prints them
*during* one, as each prompt effect dispatches:

```
$ cof run pipeline.yml --explain-routing
▸ prime.classify score 7.2/100 · band trivial · model llama3.2:1b · why router
▸ prime.summarize score 20.5/100 · band ordinary · model llama3.2 · why router
▸ prime.audit score 41.7/100 · band hard · model llama3.1:70b · why explicit
```

One line per prompt effect, printed the moment before it dispatches — the
same `on_effect_start` window `meta.complexity` is written in, so the line
never lags or races the call it describes. Needs
`runtime.complexity.scoring.enabled: true`; with scoring off it prints
nothing, on every effect, silently — there is no score to report and no
separate switch to check.

Each line reads straight off the node's pre-dispatch meta, with no
recomputation:

- **score** — `meta.complexity.score` / `max_score`.
- **band** — `meta.complexity.band`, present only when
  `routing.enabled: true`. It names the row of the band table the score falls
  in; with routing off the line says `routing off` in its place rather than
  guessing a name for a table that isn't configured.
- **model** — `meta.model`, the model actually about to be dispatched.
- **why** — `meta.model_reason`: `router` when the band decided, `explicit`
  when the effect names its own `model:` (or a profile override does),
  `default` when it inherits the run's. A band naming a different model than
  the one on the line is not a contradiction — it is the router deferring, and
  `why` says to whom.

`--quiet` and `--json` both suppress it, same as the rest of a run's prose
output. It composes with `--verbose`: the two report different things (token
counts and timing vs. score and model choice), so nothing is printed twice.

#### Try it: one orchestration, three answers

`docs/examples/routing/` has a runnable demo — one orchestration
(`triage.yml`, three prompts of deliberately different weight, the heaviest
pinning its own `model:`) and two band tables over it. `--dry-run` is enough:
routing happens in the pre-dispatch meta block, so no model is ever called.

```bash
cof run docs/examples/routing/triage.yml --dry-run --explain-routing \
    --config docs/examples/routing/bands-frugal.json \
    -e ticket=... -e rules=... -e prior_decisions=...
```

**Deciding** — the frugal table spreads the three effects across three models:

```
▸ prime.classify score 7.2/100 · band trivial · model llama3.2:1b · why router
▸ prime.summarize score 20.5/100 · band ordinary · model llama3.2 · why router
▸ prime.audit score 41.7/100 · band hard · model llama3.1:70b · why explicit
```

Swap in `bands-generous.json` and the same three scores land differently —
`classify` moves up to `ordinary`, `summarize` to `hard`. Same orchestration,
same scores, different policy: that is the whole point of the table being
config rather than code.

**Deferring** — `prime.audit` already shows one half of it above: `why
explicit`, dispatching its own `llama3.1:70b` while still reporting the `hard`
band it scored into. Add `--model llama3.2` for the other half, and every
effect that was routing falls back to the flag:

```
▸ prime.classify score 7.2/100 · band trivial · model llama3.2 · why default
▸ prime.summarize score 20.5/100 · band ordinary · model llama3.2 · why default
▸ prime.audit score 41.7/100 · band hard · model llama3.1:70b · why explicit
```

The bands are still computed and still reported — you can see exactly what you
overrode — but nothing routes.

**Disabled** — drop `--config` and there is no complexity block at all: no
scores, no bands, no lines, and `meta.model_reason` back to
`explicit`/`default` on every node.

**Per-effect override, from a profile** — `docs/examples/routing/profiles/
pin-and-opt-out.yml` pins `summarize` to the top `hard` band and opts
`classify` out of routing entirely, without touching `triage.yml` or either
band table (see [Per-Effect Routing Control](profiles.md#per-effect-routing-control)):

```bash
cof run docs/examples/routing/triage.yml --dry-run --explain-routing \
    --config docs/examples/routing/bands-frugal.json \
    --profile pin-and-opt-out \
    -e ticket=... -e rules=... -e prior_decisions=...
```

```
▸ prime.classify score 7.3/100 · band trivial · model llama3.2 · why default
▸ prime.summarize score 20.6/100 · band hard · model llama3.1:8b · why router
▸ prime.audit score 42.0/100 · band hard · model llama3.1:70b · why explicit
```

`classify` still scores into `trivial`, but the opt-out means routing never
substitutes that band's model — it dispatches on the orchestration's own
default (`why default`), not `why router`. `summarize` scores into `ordinary`
on this table (see the plain `bands-frugal.json` run above) but the pin sends
it to `hard` instead — the band shown is the one that actually applied, not
the one the score alone would have picked. `audit` is unaffected either way:
its own `model:` already outranks a pin the same way it outranks the router.

### `decomposition`

| Field | Type | Default | Description |
| --- | --- | --- | --- |
| `enabled` | boolean | `false` | Turns decomposition on. |
| `threshold` | number | `80` | Score above which an effect is decomposed. Must be within 0–100. |
| `max_depth` | integer | `2` | Recursion ceiling for nested decomposition. `0` or greater. |
| `max_chunks` | integer | `8` | Maximum chunks a plan may produce. `1` or greater. |
| `on_failure` | string | `route_up` | `route_up` runs the original prompt on a more capable model; `fail` propagates the error. |

#### What a decomposition actually does

When a prompt effect's recorded score **strictly exceeds** `threshold` (a score
equal to the threshold does not trigger), the runtime replaces the single model
call with three bounded steps:

1. **Plan.** The bundled planner (`agents/decompose`, see
   [the shared library](shared-library.md)) runs in an isolated store, handed
   the effect's raw template, a listing of the context keys it could read, its
   output shape, and `max_chunks`. The planner run itself has decomposition
   switched off — its own planning prompt would otherwise trigger the feature
   it implements.
2. **Validate.** The emitted YAML must parse, pass the orchestration schema,
   keep the fan-out within `[2, max_chunks]`, and contain a top-level effect
   that actually writes the planner-reported `result_path`
   (`prime.merge.value`). An invalid plan never runs.
3. **Execute and write back.** The emitted orchestration runs as a
   state-isolated child seeded with a *copy* of the effect's render context —
   same inline-identity cycle guard as a `use` child, same namespaced
   observability (child effects announce under the decomposing effect's node,
   and live-state snapshots mirror them there). The value at `result_path` is
   written at the **original effect's own path**, so a downstream
   `{{prime.<name>.value}}` reference resolves unchanged and nothing else in
   the orchestration knows the substitution happened.

The attempt is recorded at `meta.decomposition` on the effect node —
`{decomposed, outcome, reason, score, threshold, depth, max_depth, plan,
chunk_count, yaml, result_path, fallback_model, error}` — whether it
decomposed, routed up, ran as-is, or failed. Like `meta.complexity`, the key is
absent entirely when the feature never triggered.

Chunks are scored too, inside the child, so an over-complex chunk decomposes
again — that is what `max_depth` bounds. At the ceiling the effect **routes
up** instead of decomposing: it runs once on the routing table's catch-all
(most capable) band model, or — with routing off — simply runs as-is, and the
depth it stopped at is recorded. The ceiling is checked before the planner
runs, so a depth-limited effect costs no extra calls.

`on_failure` governs the three real failure paths — the planner failing, an
invalid plan, and the child execution failing:

- `route_up` (the default) falls back exactly like the depth ceiling: the
  original prompt runs on the catch-all band model (or as-is with routing off),
  the failure is recorded under `meta.decomposition.reason`, and the run
  lives. A decomposition attempt can never turn a working run into a failed
  one.
- `fail` propagates like any other effect error (`meta.error` is set, the
  effect's own `on_error` applies).

Either way the child's scratch state is discarded whole on failure — nothing
partial ever lands at the original effect's path. `respect_explicit` from the
routing block covers the fallback too: an effect that pins its own `model:`
keeps it when route-up would otherwise substitute one.

#### Persisting plans to disk: `cof run --decompose-out <dir>`

`meta.decomposition.yaml` already carries the generated orchestration, but a
run that finishes and is never looked at again makes that plan effectively
unreadable. `--decompose-out <dir>` writes it out — one file per effect whose
decomposition attempt reached a planner-emitted document:

```bash
cof run pipeline.yml --decompose-out ./plans
```

```
plans/
  3f2c9e7a-...__prime.audit.yml
  3f2c9e7a-...__prime.audit.merge_step.yml
```

Each filename is `<run_id>__<effect_path>.yml` — the run's own id paired with
the effect's canonical dotted path (the same path `--explain-routing` and
`effect_observer` report), sanitized for the filesystem. Both halves are
unique within and across runs (loop iterations and nested decomposition
children get distinct paths too), so no two plans ever collide.

A written file is the planner's emitted YAML verbatim — already validated
(schema, merge contract) when the plan succeeded — headed by a comment block
naming the run, the effect, and the outcome:

```yaml
# Generated by circuitry decomposition — cof run --decompose-out
# run_id: 3f2c9e7a-...
# effect: prime.audit
# status: succeeded
# reason: none
# result_path: prime.merge.value
# chunk_count: 3
effects:
  - type: prompt
    name: chunk_a
    template: "..."
  ...
```

The header is plain YAML comments, so the file loads and runs exactly as
written — `cof run ./plans/<file>.yml` (supplying `--adapter`/`--model` if the
plan doesn't pin its own, same as any other bare orchestration file).

**Failed plans are written too.** A plan that failed validation
(`invalid_plan`) or whose child execution failed (`execution_failed`) still
produced YAML — that is the one worth reading to see what went wrong — so it
is written with `# status: failed` and an `# error:` line carrying the
recorded `meta.decomposition.error`. The two failure paths that never reach a
planner-emitted document (`planner_failed`, and `max_depth` — which never
calls the planner at all) have no YAML to write and are skipped.

Nothing beyond the plan itself is written: the file is the orchestration
structure the planner returned, not run state, so it carries no secrets or
context values beyond whatever templates the plan's own effects reference —
same as any other orchestration file.

Needs `runtime.complexity.decomposition.enabled: true`; a run that never
triggers decomposition (or runs without `--decompose-out`) writes nothing.

## Precedence

The block rides the normal `runtime.*` precedence — **orchestration over
config** — with no separate plumbing:

```
orchestration runtime.complexity  >  config runtime.complexity  >  defaults
```

The runtime merge is shallow over top-level runtime keys, so an
orchestration-level `complexity` block **replaces** the config-level one
wholesale rather than merging into it. An orchestration that overrides the
block must restate every value it still wants — including sub-blocks it does
not change.

`resolve_effective_settings` records the winning layer under
`sources["complexity"]`, plus `sources["complexity.scoring"]`,
`sources["complexity.routing"]` and `sources["complexity.decomposition"]` for
each sub-block (`orchestration`, `config`, or `default`).

The *model* has its own chain, which routing joins — see [Where the router
sits](#where-the-router-sits-model-precedence). When the router wins it,
`sources["model"]` reads `router`, while `EffectiveSettings.model` keeps the
run default the router falls back to (the per-effect answers are on each node's
`meta.model`). `EffectiveSettings.model_locked` is the companion flag: `true`
when the model was pinned by `--model` or a profile's run-level `model:`, which
is what the runtime reads to know an explicit choice from an inherited default
once `sources["model"]` has been claimed by the router.

## Reading the resolved settings from code

```python
from circuitry.cli.complexity_config import resolve_complexity_settings

settings = resolve_complexity_settings(effective.runtime)
if settings.routing.enabled:
    for band in settings.routing.bands:
        ...  # band.max is None for the catch-all
```

`EffectiveSettings.complexity` carries the same object, already validated at
resolution time. Consumers read these typed dataclasses
(`ComplexitySettings`, `ScoringSettings`, `RoutingSettings`,
`DecompositionSettings`, `ComplexityBand`) rather than re-parsing raw dicts.
`ComplexitySettings.as_dict()` renders the resolved block for machine-readable
output.

## Errors

Every problem in the block raises `ComplexityConfigError` — a `ConfigError`,
and therefore a `ValueError` — at config resolution, with a message naming the
full path of the offending value:

```
runtime.complexity.routing.bands must end with a catch-all band — an entry with
no 'max' — so every score resolves to a model. Drop 'max' from
runtime.complexity.routing.bands[1] or append a new entry.
```

```
runtime.complexity.decomposition.enabled is true but
runtime.complexity.scoring.enabled is false. Decomposition consumes complexity
scores, so it requires the scorer — set runtime.complexity.scoring.enabled to
true or turn decomposition off.
```
