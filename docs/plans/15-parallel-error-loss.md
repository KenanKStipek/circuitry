# Plan 15: Surface All Errors from Parallel Tree Execution

## Problem

`dynamic.py:217` raises only `tree_errors[0]` when multiple effects fail concurrently, discarding all other errors. Debugging requires re-running to discover the next error.

## Design Decision

Use a custom `TreeExecutionError(RuntimeError)` instead of `ExceptionGroup` because:
- Python 3.9 compatibility required (`ExceptionGroup` is 3.11+)
- Callers only `except Exception` and `str(e)` -- custom `__str__` is fully compatible
- `.errors` attribute mirrors `ExceptionGroup.exceptions` for future migration

## Implementation

### Step 1: Create `TreeExecutionError` in `dynamic.py`

```python
class TreeExecutionError(RuntimeError):
    def __init__(self, errors: list[Exception]) -> None:
        self.errors = errors
        super().__init__(self._format())

    def _format(self) -> str:
        if len(self.errors) == 1:
            return str(self.errors[0])
        parts = [f"{len(self.errors)} effects failed in parallel:"]
        for i, err in enumerate(self.errors, 1):
            parts.append(f"  [{i}] {type(err).__name__}: {err}")
        return "\n".join(parts)
```

### Step 2: Replace `raise tree_errors[0]` (line 217)

```python
if tree_errors:
    exc = TreeExecutionError(tree_errors)
    exc.__cause__ = tree_errors[0]
    raise exc
```

### Step 3: Export from `core/__init__.py`

### Step 4: No changes to `meta["error"]` handling

`str(e)` now captures the full multi-error string automatically.

## Tests (`tests/core/test_dynamic_topologies.py`)

1. `test_tree_raises_all_errors_when_multiple_effects_fail` -- `.errors` has all failures, `str(e)` contains both messages
2. `test_tree_single_error_message_unchanged` -- backward compatible for single failure
3. `test_tree_meta_error_contains_all_failures` -- store metadata includes all messages

## Files to Change

| File | Change |
|------|--------|
| `src/circuitry/core/dynamic.py` | Add `TreeExecutionError`, replace `raise tree_errors[0]` |
| `src/circuitry/core/__init__.py` | Export `TreeExecutionError` |
| `tests/core/test_dynamic_topologies.py` | Add 3 tests |
