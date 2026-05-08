# Plan 14: Raise Error on Unknown Flow Values

## Problem

`_normalize_flow()` at `compiler.py:140-147` silently defaults unknown flow values to `"chain"`. A typo like `flow: tee` degrades to sequential execution with no indication.

## Fix

Replace the silent fallback with a `ValueError`.

**Before:**
```python
def _normalize_flow(flow: str) -> Literal["chain", "tree"]:
    flow = (flow or "chain").strip().lower()
    if flow in ("chain", "chain_of_thought", "cot"):
        return "chain"
    if flow in ("tree", "tree_of_thought", "tot"):
        return "tree"
    return "chain"  # silent swallow
```

**After:**
```python
_VALID_FLOWS: dict[str, Literal["chain", "tree"]] = {
    "chain": "chain", "chain_of_thought": "chain", "cot": "chain",
    "tree": "tree", "tree_of_thought": "tree", "tot": "tree",
}

def _normalize_flow(flow: str) -> Literal["chain", "tree"]:
    key = (flow or "chain").strip().lower()
    canonical = _VALID_FLOWS.get(key)
    if canonical is not None:
        return canonical
    valid = ", ".join(sorted(_VALID_FLOWS.keys()))
    raise ValueError(f"Unknown flow value {flow!r}. Valid values are: {valid}.")
```

## Tests (`tests/core/test_compiler_validation.py`)

1. `test_compile_rejects_unknown_flow_on_orchestration` -- `flow: "tee"` -> `ValueError`
2. `test_compile_rejects_unknown_flow_on_dynamic_effect` -- nested dynamic with bad flow
3. `test_compile_rejects_unknown_flow_on_loop_effect` -- loop with bad flow
4. `test_compile_accepts_all_valid_flow_aliases` -- parametrize over all 6 aliases

## Files to Change

| File | Change |
|------|--------|
| `src/circuitry/core/compiler.py` | Replace `_normalize_flow` with dict-lookup + `ValueError` |
| `tests/core/test_compiler_validation.py` | Add 4 tests |
