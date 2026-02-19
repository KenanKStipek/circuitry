# API Reference

## Public Python API

Import from top-level package:

```python
from circuitry import (
    CircuitryExecutionError,
    inspect_divergence_paths,
    inspect_orchestration,
    run_orchestration,
    run_shared_orchestration,
    validate_orchestration,
)
```

### `run_orchestration`

Primary embedded entrypoint for local orchestration files.

- Module: `src/circuitry/api.py`
- Returns: `RunResult`
- Raises: `CircuitryExecutionError` when `raise_on_error=True` and runtime fails

Key parameters:
- `orchestration_path`: YAML path
- `state` or `state_path`: initial input state
- `dry_run`: skip model invocation and emit deterministic placeholder outputs
- `out_path`: write resulting state to disk

### `run_shared_orchestration`

Embedded entrypoint for shared-library assets.

- Module: `src/circuitry/api.py`
- Returns: `RunResult`
- Uses: shared-library retrieval (`runtime.library`) and optional `service_profile`

Key parameters:
- `asset_id`, `version`: shared asset selector
- `config`: required `CircuitryConfig`
- `service_profile`: optional runtime override profile name
- `auth_token`: optional library auth token

### `validate_orchestration`

Compiler-backed structure validation.

- Module: `src/circuitry/api.py`
- Returns: validation report dictionary

### `inspect_orchestration`

Static orchestration inspection (effect counts/types, declared runtime hints).

- Module: `src/circuitry/api.py`
- Returns: inspection report dictionary

### `inspect_divergence_paths`

Extracts deterministic divergence/failure-path records from runtime state.

- Module: `src/circuitry/api.py`
- Returns: list of diagnostic records

### `CircuitryExecutionError`

Runtime exception that includes the failed `RunResult` as `.result`.

- Module: `src/circuitry/api.py`

## Adapter Factory API

Public adapter factory:

- `build_adapter` from `circuitry.adapters`
- Module: `src/circuitry/adapters/factory.py`

Use adapter factories for direct adapter wiring only when bypassing orchestration runtime.

## Architecture-to-Code Map

- CLI command surface: `src/circuitry/cli/app.py`
- Runtime entry shim: `src/circuitry/cli/runtime_shim.py`
- Compiler: `src/circuitry/core/compiler.py`
- Effect runtime:
  - prompts: `src/circuitry/core/prompt.py`
  - dynamics: `src/circuitry/core/dynamic.py`
  - conditionals: `src/circuitry/core/conditional.py`
  - loops: `src/circuitry/core/loop.py`
  - reflectors: `src/circuitry/core/reflector.py`
- State store: `src/circuitry/core/store/store.py`
- Adapter boundary: `src/circuitry/adapters/`

## Integration Patterns

Canonical pattern:
1. Validate orchestration once (`validate_orchestration`).
2. Run with deterministic `state` payload (`run_orchestration` or `run_shared_orchestration`).
3. Persist/inspect resulting runtime metadata from returned `RunResult.state`.

Anti-patterns:
- Mutating state externally mid-run.
- Depending on undocumented internal modules instead of public package exports.
- Treating non-public runtime keys as stable API without versioning policy.

## Documentation Versioning and Update Rule

When runtime/public exports change:

1. Update `src/circuitry/__init__.py` exports.
2. Update this file (`docs/api-reference.md`) in the same change.
3. Keep architecture references aligned in `docs/architecture.md`.
4. Run documentation conformance tests:
   - `pytest -q tests/docs/test_documentation_contracts.py`
