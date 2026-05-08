# Plan 02: Fix Undefined `mtag` in `core/tool.py`

## Problem

`ToolRuntime.execute()` references `mtag` in two code paths before it is assigned:

1. **Dry-run path (~line 203):** `mtag` referenced for verbose output, but not assigned until line 227.
2. **Exception handler (~line 286):** If exception fires before line 227, `mtag` is undefined.

Both produce `NameError` at runtime.

## Fix

Initialize `mtag = ""` early and compute it properly in the dry-run block.

## Steps

### 1. Initialize `mtag` early (after line 187)

```python
t0 = time.monotonic()
mtag = ""  # <-- add this
```

### 2. Compute `mtag` in dry-run block (before verbose output)

Inside `if self.dry_run:`, before `if self.verbose:`:
```python
mtag = _model_tag({"model": self.defn.model})
```

### 3. No change needed in exception handler

With `mtag = ""` initialized early, the handler degrades gracefully to showing `self.defn.provider` only.

## Tests to Add (`tests/core/test_tool_effect.py`)

1. **`test_tool_runtime_dry_run_verbose_no_name_error`** -- dry_run=True, verbose=True does not crash
2. **`test_tool_runtime_dry_run_verbose_includes_model_tag`** -- model name appears in verbose output
3. **`test_tool_runtime_exception_before_mtag_verbose_no_name_error`** -- build_plugin raises, on_error="skip", no NameError
4. **`test_tool_runtime_exception_before_mtag_verbose_fail_reraises`** -- on_error="fail" propagates original error, not NameError

## Files to Change

| File | Change |
|------|--------|
| `src/circuitry/core/tool.py` | Initialize `mtag = ""` early; compute in dry-run block |
| `tests/core/test_tool_effect.py` | Add 4 regression tests |
