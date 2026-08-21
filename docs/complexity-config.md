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

An unrecognised signal name is an error rather than a silent no-op, so a typo
in `weights` cannot quietly do nothing.

To see what these settings produce for a given orchestration before running it,
use [`cof score`](./score-command.md).

### `routing`

| Field | Type | Default | Description |
| --- | --- | --- | --- |
| `enabled` | boolean | `false` | Turns band routing on. Requires `bands`. |
| `bands` | array | — | Ordered band table (below). |
| `respect_explicit` | boolean | `true` | Keeps explicit model choices (`--model`, a per-effect `model:`, a profile override) winning over the router. |

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

### `decomposition`

| Field | Type | Default | Description |
| --- | --- | --- | --- |
| `enabled` | boolean | `false` | Turns decomposition on. |
| `threshold` | number | `80` | Score above which an effect is decomposed. Must be within 0–100. |
| `max_depth` | integer | `2` | Recursion ceiling for nested decomposition. `0` or greater. |
| `max_chunks` | integer | `8` | Maximum chunks a plan may produce. `1` or greater. |
| `on_failure` | string | `route_up` | `route_up` runs the original prompt on a more capable model; `fail` propagates the error. |

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
