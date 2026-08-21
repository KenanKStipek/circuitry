# `cof score` — per-effect complexity preview

Score a whole orchestration **without running it**.

`compile_orchestration` freezes the entire effect graph before the first model
call, so the tree can be walked and every prompt in it scored statically. That
is all this command does: compile, walk, score, print. No adapter is built, no
store is opened, nothing executes.

```
cof score <orchestration> [--config PATH] [--profile NAME] [--json]
```

## What it prints

```
$ cof score pipeline.yml
                          Circuitry · Score (static preview)
┏━━━━━━━━━━━━━━━━━━━━━━━┳━━━━━━━━━━━━━━━━━━━━━┳━━━━━━━┳━━━━━━━┳━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓
┃ Effect                ┃ Type                ┃ Score ┃ Band  ┃ Dominant signals / reason     ┃
┡━━━━━━━━━━━━━━━━━━━━━━━╇━━━━━━━━━━━━━━━━━━━━━╇━━━━━━━╇━━━━━━━╇━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┩
│ intro                 │ prompt              │  14.4 │ cheap │ state_references 7.1, …       │
│ refine.critique       │ prompt              │  42.5 │ heavy │ keywords 13.6, …              │
│ planner.propose_steps │ prompt              │  10.8 │ cheap │ state_references 4.3, …       │
│ planner.generated     │ reflector_generated │     — │ —     │ not scoreable — planned at …  │
│ helper                │ use                 │     — │ —     │ not scoreable — compiled at … │
└───────────────────────┴─────────────────────┴───────┴───────┴───────────────────────────────┘
3 scored, 2 not scoreable, 5 effect(s) total.
Scores are static estimates measured against prompt templates, not rendered
prompts. …
```

The **Band** column appears only when a band table is configured
(`runtime.complexity.routing.bands` — see
[Complexity Configuration](./complexity-config.md)). Bands are read even when
`routing.enabled` is false, so a table can be sanity-checked against real
scores before the router is switched on.

**Dominant signals** are the three signals that contributed the most score
points, largest first. The full breakdown — every signal's raw measurement,
normalized value, weight and contribution — is one `--json` away.

## What the preview cannot know

Two things, and the command says both out loud rather than printing a table
that merely *looks* complete.

### The scores are template-based estimates

A static score measures the authored template. The runtime scores the
*rendered* prompt, after state has been interpolated. Interpolation only ever
adds text, so a runtime score is at least as high as the preview — and much
higher wherever a template pulls in a large blob. Every run of the command
states this, in the table footer and in the `notice` field of `--json`.

### Some effects do not exist yet

| Reported as | Why there is nothing to score |
| --- | --- |
| `reflector_generated` | A reflector plans its effects at runtime, from the output of its `plan_from_step` prompt. Nothing exists to score before the run reaches it. Reported at `<reflector>.<generated_key>`. |
| `use` | A `use` effect resolves and compiles the orchestration it references only when it executes, so its children are not in the frozen tree. |
| `tool` | A tool effect calls a plugin, not a model. There is no prompt. |
| a disabled effect | `--profile` disabled it, so it will not execute at all. |

These appear as rows with `scoreable: false` and a `reason` — never silently
omitted. A table that quietly dropped them would read as a complete inventory
when it is not, which is worse than having no preview.

Effects a `dynamic` container declares *are* in the frozen tree — the compiler
compiles them alongside everything else — so they are scored like any other
nested effect.

## Addressing

Rows are keyed by the same dotted paths a profile targets: relative to the
`prime` root, with anonymous conditionals and loops contributing no segment of
their own.

| Shape | Path |
| --- | --- |
| top-level prompt `intro` | `intro` |
| prompt `critique` in loop `refine`'s body | `refine.critique` |
| prompt `approve` in conditional `decide`'s `then` | `decide.approve` |
| prompt `reject` in the same conditional's `else` | `decide.reject` |
| a reflector's authored prompt | `planner.propose_steps` |
| what that reflector generates | `planner.generated` |

Both branches of a conditional are previewed: which one runs depends on state
that does not exist yet, and showing only one would be a guess. A loop body's
effects appear **once**, at the path they are addressed by — the iteration
count is not known before the run either — but they are scored as being inside
a loop, which the `structure` signal reflects (`loop_depth` in its breakdown).

## Options

| Option | Effect |
| --- | --- |
| `--config`, `-c` | Config JSON to resolve settings from (or `CIRCUITRY_CONFIG`). |
| `--profile` | Named profile (`profiles/<name>.yml`) to preview under. Applies the same per-effect model/provider overrides and disabled effects a run would, through the same `apply_effect_overrides`. |
| `--json` | Emit the payload below on stdout and nothing else. |

`--config` and `--profile` exist so the preview is of *a specific run*: the
weights, the keyword table and the band table all come from the settings that
run would resolve.

## `--json`

```jsonc
{
  "orchestration": "pipeline.yml",
  "config": "circuitry.config.json",
  "profile": null,
  "mode": "static",
  "estimated": true,
  "notice": "Scores are static estimates measured against prompt templates, …",
  "max_score": 100.0,
  "weights": {"prompt_size": 1.0, "state_references": 1.5, "…": 0},
  "bands": [{"name": "cheap", "max": 20.0, "model": "llama3"}],
  "effects": [
    {
      "path": "intro",
      "type": "prompt",
      "scoreable": true,
      "reason": null,
      "score": 14.4,
      "band": "cheap",
      "band_model": "llama3",
      "dominant_signals": [{"name": "state_references", "contribution": 7.06}],
      "breakdown": { /* the full ComplexityScore.to_dict() */ }
    },
    {
      "path": "planner.generated",
      "type": "reflector_generated",
      "scoreable": false,
      "reason": "planned at runtime: the reflector generates these effects …",
      "score": null,
      "band": null,
      "band_model": null,
      "dominant_signals": [],
      "breakdown": null
    }
  ],
  "summary": {"effects": 5, "scored": 3, "unscoreable": 2,
              "highest": {"path": "refine.critique", "score": 42.5}}
}
```

`weights` is keyed by the *scorer's* signal names, not the config's — the two
vocabularies differ (`prompt_type` → `output_type`, `output_schema` →
`schema_shape`, `structural_position` → `structure`), and the translation lives
in `ScoringSettings.scorer_weights()`.

`breakdown` is the scorer's own serialization, so the contributions still sum
to the score exactly. A surprising number can be argued with by reading it,
without re-running anything.

## Scoring must be enabled

`cof score` previews the scores a run would compute, so it needs the same
switch a run does. With `runtime.complexity.scoring.enabled` false — the
default — it exits `1` with an actionable message rather than inventing
numbers:

```
$ cof score pipeline.yml
Scoring disabled. Complexity scoring is disabled in circuitry.config.json.
`cof score` previews the scores a run would compute, so it needs the same
switch a run does. Enable it with runtime.complexity.scoring.enabled: true …
```

`--json` still emits a parseable `{"ok": false, "scoring_enabled": false,
"error": "…"}` in that case, so a script can tell "off" from "broken".

## Determinism

The scorer is a pure function (see
`circuitry.core.complexity`), the walk is a pure function of the compiled
tree, and neither reads a clock or a random source. The same orchestration and
the same settings always produce byte-identical output — which is what makes
`cof score --json` usable as a lint gate in CI.
