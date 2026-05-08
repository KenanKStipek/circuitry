# Plan 06: Eliminate Silent Exception Swallowing

## Problem

23+ locations catch broad exceptions and return defaults with zero logging. Template rendering, CEL evaluation, config parsing, plugin loads, and collection resolution all silently swallow errors. Debugging is a nightmare.

## Logging Strategy

### Logger Convention

Every module gets `logger = logging.getLogger(__name__)`, producing names like `circuitry.core.prompt`.

### Log Levels

| Level | Usage |
|-------|-------|
| `DEBUG` | Best-effort operations expected to sometimes fail (version lookup, last-run save) |
| `WARNING` | Operations that silently degrade behavior (template rendering, config parse, plugin load) |
| `ERROR` | Operations where fallback meaningfully changes semantics (CEL eval changing branch decisions) |

### Default Config

Add to `src/circuitry/__init__.py`:
```python
logging.getLogger("circuitry").addHandler(logging.NullHandler())
```

Wire `--verbose` to `DEBUG` level on the `circuitry` logger in `cli/app.py`.

## Catalog of Changes

### Template Rendering (add `logger.warning`)
- `core/prompt.py:78-84` -- `_render()` chevron failure
- `core/tool.py:69-85` -- `_render_params()` recursive render
- `core/tool.py:217-221` -- top-level prompt render
- `core/conditional.py:296-301` -- condition template render
- `core/loop.py:416-421` -- while template render

### CEL Evaluation (narrow exception type + `logger.error`)
- `core/conditional.py:330-342` -- narrow to `(KeyError, TypeError, ValueError, AttributeError)`
- `core/loop.py:450-461` -- same treatment

### Config Parsing (narrow + `logger.warning`)
- `cli/config.py:177-181` -- global config load
- `cli/config.py:193-197` -- project config load

### Plugin System (add `logger.warning`)
- `core/runtime_plugins.py:42-52` -- plugin load failure
- `core/runtime_plugins.py:67-95` -- plugin hook failure

### Collection Resolution (add `logger.warning`)
- `core/loop.py:375-396` -- path traversal hits non-dict

### Runtime Shim (add `logger.error`)
- `cli/runtime_shim.py:293-295` -- error during error handling
- `cli/runtime_shim.py:187-192` -- timeout parse (narrow + warn)
- `cli/runtime_shim.py:257-296` -- top-level run failure (add `exc_info=True`)

### App Best-Effort (add `logger.debug`/`logger.warning`)
- `cli/app.py:109-113` -- last-run save (debug)
- `cli/app.py:650-655` -- bundled orchestrator (error)
- `cli/app.py:663-673` -- rules load (warning)
- `cli/app.py:676-686` -- plugin docs (debug)
- `cli/app.py:808-811` -- version fallback (debug)

### Reflector (add logging)
- `core/reflector.py:242-254` -- format failure (warning)
- `core/reflector.py:257-270` -- goal extraction (debug)
- `core/reflector.py:273-286` -- context extraction (debug)

## Tests (`tests/core/test_exception_logging.py`, `tests/cli/test_exception_logging.py`)

1. Template rendering logs warning (use `caplog` fixture)
2. CEL evaluation logs error on failure
3. CEL evaluation propagates unexpected exceptions (e.g., `SyntaxError`)
4. Config parsing logs warning on malformed config
5. Config parsing lets `RuntimeError` propagate (narrowed catch)
6. Plugin load failure logs warning
7. Plugin hook failure logs warning
8. Collection resolution logs warning on bad path
9. Runtime_shim error cleanup logs on inner failure
10. `--verbose` enables log output
11. Tool param rendering logs warning
12. Loop CEL evaluation logging

## Files to Change

| File | Change |
|------|--------|
| `src/circuitry/__init__.py` | Add `NullHandler` |
| `src/circuitry/cli/app.py` | Wire `--verbose` to logging; add debug/warning/error |
| `src/circuitry/core/prompt.py` | Add logger + warnings |
| `src/circuitry/core/tool.py` | Add logger + warnings |
| `src/circuitry/core/conditional.py` | Add logger + narrow CEL catch + error logging |
| `src/circuitry/core/loop.py` | Add logger + warnings + narrow CEL catch |
| `src/circuitry/cli/config.py` | Add logger + narrow catches + warnings |
| `src/circuitry/core/runtime_plugins.py` | Add logger + warnings |
| `src/circuitry/cli/runtime_shim.py` | Add logger + error logging |
| `src/circuitry/core/reflector.py` | Add logger + debug/warnings |
| `tests/core/test_exception_logging.py` | **Create** -- 12 tests |
| `tests/cli/test_exception_logging.py` | **Create** -- CLI-layer tests |
