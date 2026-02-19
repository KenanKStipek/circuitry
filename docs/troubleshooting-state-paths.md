# Troubleshooting Deterministic State Paths

This guide provides a reproducible workflow to isolate orchestration divergence using deterministic state paths and runtime metadata.

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

## Reproducibility Notes

- Path ordering from `inspect_divergence_paths` is deterministic.
- Nested runtime failures preserve hierarchical path breadcrumbs in parent errors.
- Keep troubleshooting artifacts (`out.json`, orchestration file, effective settings) together for incident review.
