# Troubleshooting Deterministic State Paths

This guide provides a reproducible workflow to isolate orchestration divergence using deterministic state paths and runtime metadata.

## State Namespaces

State has exactly three root namespaces. Outside CEL every path is
root-relative; inside a CEL expression, `state` binds to the root.

| Namespace | Holds | Outside CEL | Inside CEL |
|---|---|---|---|
| `input` | Caller-supplied values — CLI `-e`/`--state`, profile inputs, `use.inputs`, REST/scheduler `state` payloads | `input.<name>` | `state.input.<name>` |
| `prime` | Effect outputs, rooted at the compiled orchestration root | `prime.<name>.value` | `state.prime.<name>.value` |
| `runtime` | Framework metadata (`last_run`, plugins, persistence, ...) | `runtime.<key>` | `state.runtime.<key>` |

There is no sugar layer — a bare key or a `state.`-prefixed path outside CEL
is a hard error from `cof check`/`validate()`, not a deprecation warning. If
you're diagnosing a path that used to resolve and no longer does, start here
before the patterns below.

## Workflow

1. Capture run state with CLI:
   - `python -m circuitry.cli.app run <orch.yml> --out out.json`
2. Open `out.json` and inspect `runtime.last_run` for top-level status and timestamps.
3. Traverse `prime.*` nodes and check each `<node>.meta.error` field.
4. Follow breadcrumbed errors (for nested dynamic/control flow failures) from parent to child.
5. Confirm specific failure location using deterministic node path + effect name.

## Fast Failure Extraction

Use the embedded helper to collect all failure records in sorted path order:

```python
from circuitry import inspect_divergence_paths

records = inspect_divergence_paths(state=state)
for item in records:
    print(item["path"], item["error"])
```

Each record includes:
- `path`: deterministic state path (for example `prime.outer.inner.task`)
- `error`: captured runtime error message
- `created_at` and `completed_at` when available

## Common Debug Patterns

- Prompt failure:
  - Inspect `<prompt>.meta.error` and `<prompt>.meta.fallback_attempts`.
  - Validate adapter/model and fallback sequence.
- Dynamic composition failure:
  - Inspect parent dynamic `meta.error` for nested breadcrumb chains.
  - Follow child path segments in order (`prime.a.b.c`).
- Conditional/loop divergence:
  - Inspect named control-node `value` summary (`branch`, `iterations`, termination reason).
  - Compare with expected execution path in orchestration definition.

## A Template Rendered Empty Inside a Loop

Nothing fails: the effect runs, the adapter returns something, and the run is
green. The tell is in `meta.prompt_sent` — the rendered prompt the adapter
actually received. Read it before anything else; a missing substitution is
visible there and nowhere else.

Run `cof validate <orch.yml>` first. Three spellings are warned about by name:

| Symptom in `meta.prompt_sent` | Cause | Fix |
|---|---|---|
| Empty where a sibling step's output should be | `{{prime.<loop>.<step>.value}}` — `prime.<loop>` holds `iter_<N>`, `last`, `collected` and `meta`, never body step names | `{{prime.<step>.value}}` |
| The *same* value in every iteration, and it is iteration 0's | `{{prime.<loop>.iter_0.<step>.value}}` inside the body — `N` is a constant | `{{prime.<step>.value}}` |
| Empty where a root input or an earlier effect should be | The name is shadowed by a body step with the same name | Rename one of them |
| Empty where a `collect` array should be | `prime.<loop>.collected.value` read from *inside* the loop; it is written when the loop finishes | Read it after the loop |
| Empty (or a previous run's data) where the final pass should be | `{{prime.<loop>.last.<step>.value}}` read from *inside* the loop; `last` is written when the loop completes | Read it after the loop; inside the body, `{{prime.<step>.value}}` is the current pass |

After the loop, when you mean "the final pass", write
`{{prime.<loop>.last.<step>.value}}` rather than pinning an `iter_<N>` you
cannot know — `last` is the final *completed* iteration's node (errored passes
under `on_error: continue`/`break` are skipped; a zero-iteration loop writes no
`last` key).

`{{prime.<step>.value}}` is the within-iteration form and resolves through a
scope chain — current iteration, then enclosing scope, then root state — in
named and unnamed loops, `chain` and `tree` flow, and `while` conditions. See
[Referencing a sibling within an iteration](orchestration-reference.md#referencing-a-sibling-within-an-iteration).

## Reproducibility Notes

- Path ordering from `inspect_divergence_paths` is deterministic.
- Nested runtime failures preserve hierarchical path breadcrumbs in parent errors.
- Keep troubleshooting artifacts (`out.json`, orchestration file, effective settings) together for incident review.
