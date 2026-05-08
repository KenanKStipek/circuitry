# Plan 01: Replace `eval()` with Safe Expression Evaluator

## Problem

`core/loop.py:458` and `core/conditional.py:339` use `eval()` with `{"__builtins__": {}}` for CEL expression evaluation. This sandboxing is trivially bypassable (e.g., `().__class__.__bases__[0].__subclasses__()`). Any orchestration YAML author can achieve arbitrary code execution.

## Fix: Introduce `simpleeval`

- Zero `eval()` usage -- walks AST directly
- Pure Python, MIT-licensed, 3M+ monthly PyPI downloads
- Supports all CEL-subset patterns used in the project: comparisons, boolean operators, dot-access, subscript

## Files to Change

| File | Action |
|------|--------|
| `src/circuitry/core/cel_eval.py` | **Create** -- `evaluate_cel()` + CEL preprocessing |
| `src/circuitry/core/loop.py` | **Modify** -- replace `_evaluate_cel`/`_cel_to_python` with import from `cel_eval` |
| `src/circuitry/core/conditional.py` | **Modify** -- replace `_evaluate_cel`/`_cel_to_python` with import from `cel_eval` |
| `requirements.txt` | **Modify** -- add `simpleeval>=0.9.13` |
| `pyproject.toml` | **Modify** -- add `simpleeval>=0.9.13` to `dependencies` |
| `tests/core/test_cel_eval.py` | **Create** -- correctness + security + edge case tests |

## Steps

### 1. Add `simpleeval` dependency

Add `simpleeval>=0.9.13` to `requirements.txt` and `pyproject.toml` `dependencies`.

### 2. Create `src/circuitry/core/cel_eval.py`

Centralized safe evaluator module:
- Pre-process CEL syntax to Python (`&&`/`||` to `and`/`or`, dot-to-bracket conversion)
- Create `SimpleEval` instance with `names = {"state": ctx}`
- Register functions: `size` -> `len`, `has`, `int`, `string`
- `DEFAULT_OPERATORS` only (no power, no augmented assignment)
- Catch all exceptions, return `False` (preserve existing error behavior)

### 3. Refactor `loop.py`

Remove `_evaluate_cel` (lines 440-461) and `_cel_to_python` (lines 463-481). Replace with:
```python
from .cel_eval import evaluate_cel
```

### 4. Refactor `conditional.py`

Remove `_evaluate_cel` (lines 320-342) and `_cel_to_python` (lines 344-367). Same replacement.

### 5. Add tests

**Correctness tests** (backward compatibility):
- `state.input.ok == True` with matching/non-matching state
- `state.prime.get_role.value == 'admin'` with nested dict
- `size(state.items) >= 1` with non-empty list
- `&&` and `||` operators
- Empty/whitespace expressions return `False`

**Security tests** (critical):
- `().__class__.__bases__[0].__subclasses__()` returns `False`
- `__import__('os').system('echo pwned')` returns `False`
- `eval('1+1')` returns `False`
- `open('/etc/passwd').read()` returns `False`
- Very long expression returns `False` (DoS protection)

**Edge cases**:
- Boolean literal `True` evaluates to `True`
- `1 == 1` evaluates to `True`
- Deeply nested access `state.a.b.c.d.e.f`
- CEL `true`/`false` literals (add to `names` dict)

### 6. Verify existing tests pass

Tests that exercise CEL and must pass unchanged:
- `tests/core/test_conditional_loop_metadata.py`
- `tests/core/test_state_path_contract.py`
- `tests/core/test_verbose_output.py`
- `tests/core/test_compiler_validation.py`

## Migration

No breaking changes. The function signature and return type are identical. Existing orchestration expressions are fully supported. The only behavioral change: expressions that previously achieved code execution now return `False`.
