# API Reference

## Public Python API

Import from top-level package:

```python
from circuitry import (
    CircuitryConfig,
    CircuitryExecutionError,
    RunResult,
    inspect_divergence_paths,
    inspect_orchestration,
    run_orchestration,
    run_shared_orchestration,
    validate_orchestration,
)
```

Unlike the `cof` CLI, the TUI, the MCP server, and the REST trigger service
(`circuitry.service.RestTriggerService`) — which all load the private `.env`
`cof setup` writes (`~/.config/circuitry/.env`) before anything else runs —
this module never loads it automatically: an embedding program owns its own
process environment, and is free to load whatever `.env` (or none) it
chooses before calling in. Call `circuitry.cli.config.load_user_env()`
yourself first if you want the same file.

`load_user_env()` reports a refused or insecure file through the
`circuitry` logger (`logger.warning(...)`), not by printing. The package
attaches only a `NullHandler` to that logger, so an embedding program
(including `RestTriggerService`) that hasn't configured its own logging
handler never sees the warning — configure a handler for the `circuitry`
logger, or call `cof doctor` / check `circuitry.cli.config.last_user_env_result()`
yourself, if you need to surface it.

### `run_orchestration`

Primary embedded entrypoint for local orchestration files.

- Module: `src/circuitry/api.py`
- Returns: `RunResult`
- Raises: `CircuitryExecutionError` when `raise_on_error=True` and runtime fails

Key parameters:
- `orchestration_path`: YAML path
- `state` or `state_path`: initial input state
- `dry_run`: skip model invocation and emit deterministic placeholder outputs
- `validate_only`: run the same checks `cof check`/`validate_orchestration` run by default — structural check, compile, preflight — and stop there: `RunResult.ok` is `False` for any document those reject, with the same errors; it never builds the adapter or dispatches an effect
- `out_path`: write resulting state to disk (`pretty` controls formatting, same as `cof run --pretty`)
- `adapter`: an already-constructed adapter to run against, instead of the one
  the config resolves — the seam a host uses to supply its own model transport
  (and a test uses to script one). Only the adapter-reachability preflight
  check is skipped when it is supplied, since such an adapter need not be
  buildable from config — tool and library-ref preflight still run.
- `trust_document` (default `True`): the file is trusted like `cof run
  ./file.yml` — its whole `runtime:` block and `plugins:` list apply, and
  `RunResult.warnings` carries one `Applied host settings from <file>: ...`
  line naming any host settings among them. Pass `False` for a path you did
  not choose yourself (fetched, generated, picked by a tool or network
  caller): the document is then limited to `runtime.complexity` and
  `runtime.state`, and its other settings are ignored with a warning. See
  [Orchestration reference](./orchestration-reference.md).
- `allow_capabilities` (default `None`): pre-approve capabilities (`shell`,
  `python_eval`, `fs-write`, `network`) a `use: ref:` child reached from this
  document may need — gated regardless of `trust_document` (#275). This
  call's own consent, never persisted to `cof trust`'s store; without it, a
  covered document refuses unless its digest was already consented there.
  See [Capability consent](./guidebook/04-configuration.md#capability-consent-for-a-fetched-or-referenced-document).

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
- `out_path`: write resulting state to disk (`pretty` controls formatting)
- `allow_capabilities` (default `None`): pre-approve capabilities the fetched
  asset (or a `use: ref:` child it reaches) may need — this call's own
  consent, never persisted. Without it, the asset refuses unless its digest
  was already consented via `cof trust`. See
  [Capability consent](./guidebook/04-configuration.md#capability-consent-for-a-fetched-or-referenced-document).

A fetched asset is limited: it may only set `runtime.complexity` and
`runtime.state` (unless config sets `trust_orchestration_runtime`).

### `validate_orchestration`

Compiler-backed structure validation.

- Module: `src/circuitry/api.py`
- Returns: validation report dictionary
- `trust_document` (default `True`) matches `run_orchestration`: the report's
  `warnings` name the host settings the file would apply, or with `False` the
  ones a run would ignore.
- `config` (default `None`): pass the same `CircuitryConfig` the corresponding
  `run_orchestration` call would use so "valid" reliably predicts "runnable"
  — without one, the allowlist and preflight gates are skipped here exactly
  as they are in `run_orchestration(config=None)`.

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

### `RunResult`

The dataclass `run_orchestration` and `run_shared_orchestration` return (`ok`, `state`, `error`, `warnings`, `out_path`).

- Module: `src/circuitry/cli/runtime_shim.py`

### `CircuitryConfig`

The resolved config dataclass `run_orchestration`'s `config=` parameter accepts and `run_shared_orchestration`'s `config=` requires.

Build one with `CircuitryConfig()` (built-in defaults) or `CircuitryConfig.from_dict({...})` (the same keys as `config.json`). Neither reads config files or environment variables: `from_dict` takes exactly the dict you pass, with no global/project layering or env merging.

- Module: `src/circuitry/cli/config.py`

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
