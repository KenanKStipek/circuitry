# Plan 13: Add Missing JSON Schemas to Orchestration YAMLs

## Problem

Two orchestration files use `prompt_type: json` without a `schema` field, meaning LLM output is parsed as JSON but never validated.

## Affected Files

| File | Prompt | Expected Output |
|------|--------|----------------|
| `orchestrations/_composition.yml:12-15` | `collect_topics` | Array of strings |
| `orchestrations/_loop.yml:3-6` | `topics` | Array of strings |

## Fix

### `_composition.yml`

```yaml
- type: prompt
  name: collect_topics
  prompt_type: json
  schema:
    type: array
    items:
      type: string
  template: 'Return exactly this JSON array: ["reliability","observability","testing"]'
```

### `_loop.yml`

```yaml
- type: prompt
  name: topics
  prompt_type: json
  schema:
    type: array
    items:
      type: string
  template: 'List exactly 3 programming languages as a JSON array...'
```

## Notes

- No runtime changes needed. The compiler already reads `schema` and passes it to `PromptDefinition`.
- `jsonschema` must be installed for validation to run; if missing, validation is silently skipped (existing behavior).
- No `minItems` constraint -- keeps schema robust if templates change.
- `bundled/rules/reflector.yml` also has `prompt_type: json` without schema, but is documentation, not a runnable orchestration.

## Files to Change

| File | Change |
|------|--------|
| `orchestrations/_composition.yml` | Add `schema` field |
| `orchestrations/_loop.yml` | Add `schema` field |
