# Plan 10: Fix Incorrect Loop Template Variables in README

## Problem

README.md (lines 237-246) contains a while-loop example referencing two non-existent template variables:
- `{{_iter}}` -- never injected anywhere
- `{{_prev_iter}}` -- never injected anywhere

Also uses `condition:` instead of `template:` (wrong field name), and nested Mustache `iter_{{_iter}}` which Chevron does not support.

The only existing loop variable is `_loop_index`, and it is only injected for `each`-mode loops.

## Decision

1. **Fix the docs** -- replace broken example with correct pattern
2. **Add `_loop_index` to while-loops** -- one-line runtime fix for consistency
3. **Do NOT add `_prev_iter`** -- trivial arithmetic, and nested Mustache is unsupported

## Steps

### Step 1: Fix README while-loop example (lines 237-246)

Replace with correct pattern matching `docs/orchestration-reference.md`:

```yaml
- type: loop
  name: refine
  while:
    mode: model
    template: "Does this draft need more improvement? Reply true or false.\n\n{{prime.draft.value}}"
  max_iterations: 5
  body:
    - type: prompt
      name: draft
      template: "Improve this text:\n{{prime.draft.value}}"
```

### Step 2: Add `_loop_index` to while-loop context

In `src/circuitry/core/loop.py`, before `_execute_body` call in while-loop path (~line 310):

```python
ctx["_loop_index"] = iteration_count
```

### Step 3: Document `_loop_index` in README State Paths section

```
Available inside loop body templates:
  {{_loop_index}}   -- zero-based iteration index (both each and while loops)
  {{<each.as>}}     -- current collection element (each loops only)
```

### Step 4: Add `_loop_index` to orchestration-reference.md

Document in the loop section and Mustache Template Interpolation table.

## Files to Change

| File | Change |
|------|--------|
| `README.md` | Fix while-loop example, document `_loop_index` |
| `src/circuitry/core/loop.py` | Add `ctx["_loop_index"] = iteration_count` in while-loop path |
| `docs/orchestration-reference.md` | Document `_loop_index` variable |
