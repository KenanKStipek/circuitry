# Plan 04: Add `if/then` Conditional Constraints to JSON Schema

## Problem

The JSON Schema documents conditional requirements in description strings but doesn't enforce them:

1. **PromptEffect:** `schema` described as "Required when prompt_type is json" -- no `if/then` constraint.
2. **ConditionDef:** `mode: model` requires `template`, `mode: cel` requires `expr` -- documented only in descriptions.

Invalid configs pass validation and blow up at runtime.

## Steps

### Step 1: Add `if/then` to `PromptEffect`

**File:** `src/circuitry/schema/orchestration.schema.json`

```json
"if": {
  "properties": {
    "prompt_type": { "enum": ["json", "object", "array"] }
  },
  "required": ["prompt_type"]
},
"then": {
  "required": ["schema"]
}
```

### Step 2: Add `if/then` to `ConditionDef`

Use `allOf` with two independent `if/then` blocks:

```json
"required": ["mode"],
"allOf": [
  {
    "if": { "properties": { "mode": { "const": "model" } }, "required": ["mode"] },
    "then": { "required": ["template"] }
  },
  {
    "if": { "properties": { "mode": { "const": "cel" } }, "required": ["mode"] },
    "then": { "required": ["expr"] }
  }
]
```

### Step 3: Add compiler-level validation

**File:** `src/circuitry/core/compiler.py`

- In `_compile_prompt`: raise `ValueError` if `prompt_type in ("json", "object", "array")` and `schema is None`
- In `_compile_conditional`: raise `ValueError` if `mode == "model"` and no `template`, or `mode == "cel"` and no `expr`
- In `_compile_loop` while block: same validation for while conditions

### Step 4: Add tests

**File:** `tests/cli/test_validate.py`
- `test_validate_rejects_json_prompt_without_schema`
- `test_validate_rejects_model_condition_without_template`
- `test_validate_rejects_cel_condition_without_expr`
- Positive cases for each

**File:** `tests/core/test_compiler_validation.py`
- `test_compile_rejects_json_prompt_without_schema`
- `test_compile_rejects_model_condition_without_template`
- `test_compile_rejects_cel_condition_without_expr`
- `test_compile_rejects_loop_while_model_without_template`
- `test_compile_rejects_loop_while_cel_without_expr`

### Step 5: Update description strings

Clarify that constraints are enforced, not just guidance.

## Backward Compatibility

Existing orchestrations that omit required fields were already silently broken at runtime. This surfaces the error earlier. A migration note should accompany this change.

## Files to Change

| File | Change |
|------|--------|
| `src/circuitry/schema/orchestration.schema.json` | Add `if/then` constraints |
| `src/circuitry/core/compiler.py` | Add defensive validation |
| `tests/cli/test_validate.py` | Add schema validation tests |
| `tests/core/test_compiler_validation.py` | Add compiler validation tests |
